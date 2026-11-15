# Workweave lifecycle

A workweave has four stages: creation, working state, landing, and
deletion. The daily bring-work-home flow touches all four. This joint
covers what each stage means, what the tool enforces, and what the
operator must ensure.

The joint does not re-document what `rwv land` composes and moves — that lives
in [landing](./landing.md) — or how a stopped operation is finished or put back,
which is [stops and abort](./stops-and-abort.md).
It does not re-document the tree model or one-hop semantics — those live
in [workweave-hierarchy](./workweave-hierarchy.md). The focus here is
the observable lifecycle: what exists, when, and how to move it forward.

Feature work happens in workweaves. Canonical clones in the primary weave
stay on the branch `rwv.toml` declares for each member (`branch`, which
`rwv member add` records from the remote's default branch); a workweave puts its
`owned` and `fork` members on a branch of its own, leaves each `dependency` and
`reference` member detached at its pin, and lands back with
`rwv land --retire`.

## Creation

```bash
rwv workweave create <name>
```

The workspace the command runs in is the new workweave's parent, and only
its provenance: it does not switch, and the workweave starts from commits. Inside
a workweave, the parent is that workweave; from a primary weave, it is primary.
To fork from another workspace, run `create` there. A parent whose `rwv.lock`
is committed and current gives each member its intended pin.

The branch the project repo ends on is chosen by flag:

```bash
rwv workweave create <name>                      # a feature named {project}--{name}
rwv workweave create <name> --new-branch <W>     # a feature named W
rwv workweave create <name> --branch <W>         # open W by its record
rwv workweave create <name> --detach <rev>       # a detached workspace
```

`--target <T>`, `--declared`, `--at <rev>`, `--set-branch`,
`--set-new-branch` and `--pin` go with a created branch and are spelled as for
`rwv branch create`; an opened branch takes none of them.

### What create writes

1. One `git worktree` per member repo, whatever its role (plus the project
   repo), at `{workweave}/{repo-path}/`. The project repo is on the workweave's
   branch (by default `{project}--{name}`, with `/` in scoped project names
   encoded as `+`) or detached; each `owned` or `fork` member of a feature is
   on a branch of the same name at its pin; every other member is detached at
   the commit the lock pins.
   Each worktree belongs to the canonical weave-root clone, so a nested
   workweave's worktrees belong to the same store as its parent's. A
   `role = "reference"` repo's tracked files are read-only. Where the
   filesystem allows, a member's files share storage with another workspace's
   copy rather than being written afresh.
2. `[workweave]` artifacts from `rwv.toml` (`copy = [...]` entries deep-copied;
   `link = [...]` entries are absolute symlinks into the parent's root).
3. A `.rwv-workweave` marker recording `{primary, project, parent}`.
   `parent` is the workspace the command ran in, recorded as provenance only:
   where the workweave lands is its branch's record. This is the workweave
   root's **only** identity file: no `.rwv-active` is written beside it.
   The pointer records project *selection*, which is primary-only — a
   workweave's project is fixed here at creation and cannot be switched —
   and the two files are mutually exclusive, one tier of the resolution
   chain rather than two. `rwv doctor` reports a root carrying both as a
   `weave-root-identity-conflict`.
4. Activation: surfacing symlinks at the workweave root, verifying the
   integration files, then running the integration install hooks, so the
   workweave gets its own `node_modules/`, `.venv/` or `target/`. A hook that
   fails leaves the workweave created and registered: `create` exits 1
   naming `rwv materialize` as the step to re-run inside it. Under
   `--hook-mode` and `--claude-hook`, the path harness integrations use, the
   same failure is a warning and `create` exits 0. `--no-materialize` skips
   the hooks and says so:

   ```text
   [skipped] core: the install hooks did not run (--no-materialize); `rwv materialize` runs them
   ```

### Uncommitted files in the parent

`create` starts from commits. Uncommitted files in the parent, `rwv.toml` and
`rwv.lock` included, stay there as they are, and `create` names them. Commit
an edit the workweave should carry before creating it.

### Re-invocation (idempotent path)

Running an identical spelling on a clean workweave whose `.rwv-workweave`
marker matches (same primary, project and parent) returns it, reported.
Non-git state written between invocations (`.runtime/`, `.claude/`, etc.) is
preserved in place. This is the standard "ensure workweave exists" path. Any
difference is refused (`name` taken), listing what differs, and changes
nothing; replacing a workweave is `rwv workweave delete` followed by `create`.

## Working state

Inside the workweave, edit, test, and commit as usual. The workspace
wiring makes cross-repo imports resolve to the worktrees.

```bash
cd github/chatly/protocol
git commit -am "protocol: add payment fields"

cd ../server
git commit -am "server: add /payments endpoint"
```

If you want to checkpoint the cross-repo state while working, `rwv lock`
updates the workweave's `rwv.lock` (not primary's); each workspace owns its
own lock (see [lock-as-derived](./lock-as-derived.md)). Before landing with
`rwv land`, a manual `rwv lock` is optional because `rwv land`
records CWD's member tips in the landing's own project commit.

Unless a branch was named at creation, the workweave's project repo and its `owned` and
`fork` members run on a feature branch named `{project}--{name}`, so two worktrees never
compete for one branch name. See
[workweave-hierarchy](./workweave-hierarchy.md#ephemeral-branch-names-and-the-git-worktree-constraint).

### Absorbing upstream changes

If the branch the workweave's feature lands into has advanced since the
workweave was created (another workweave landed, or work was committed
directly in primary), use `rwv sync` before landing:

```bash
rwv sync
```

`rwv sync` takes the target's new work into the feature. Each member takes in
the branch it lands on, and the workweave's unique commits are rebased on top
of it or merged with it, as the target's `history` setting says; then the project branch takes in the target's tip and `rwv.lock` is
recorded again. See [sync-semantics](./sync-semantics.md) for the full
direction-pair contract.

## Landing: `rwv land --retire`

The one-liner to land work and close out the workweave:

```bash
cd ../.workweaves/web-app--payments
rwv land --retire
```

Workweaves live at `<parent>/.workweaves/<project>--<name>/` — by default a sibling of the weave root, so from the weave root the path is `../.workweaves/<project>--<name>/`. `rwv land --retire` lands into the target the feature's record names, which a workweave created from another workspace records as that workspace's checked-out branch.

`rwv land --retire` lands as [landing](./landing.md) describes — composing CWD's work in this workspace, recording in one project commit the lock with its member tips, and fast-forwarding the target — and then runs the **retire** step to delete the workweave on success.

### The retire contract

Retire is a step of the landing, not a separate command. It runs
only after the target has moved. It does two things:

**Dirty-check** — no checkout in the workweave may have uncommitted
changes, whether a manifest member names it or not (a clone put there by
hand is listed with the suffix `(not a manifest member)`). Any dirty path
blocks retire. A detached HEAD holding commits no branch reaches blocks it
too (`alignment`).

**The deletion rule** — retire removes the workweave as `rwv workweave delete`
does without its `--discard-*` overrides, then applies the deletion rule to the
workweave's branches: a branch that landed on the branch the workweave's record
lands into is deleted, with its record, and one that did not survives with its
reason. A workweave retired with a branch the rule kept is gone and reports it;
`land` then exits with status `3`. Landing the work into a target other than
the recorded one leaves the branches unlanded as far as the rule can see.

### What retire deletes

On success, retire deletes the workweave as `rwv workweave delete`
does without its `--discard-*` overrides:

- Every member-repo worktree is removed (`git worktree remove`), whatever
  its role.
- Stale `.git/worktrees/` entries are pruned.
- Each branch of the workweave (`{project}--{name}`) that the deletion rule
  finds landed is deleted, in the project repo and in each member that holds
  one.
- The workweave directory is removed.

What the rule keeps stays reachable: a branch that has not landed holds its
commits, and the pre-op savepoints (`refs/rwv/pre-op/<op-id>`) remain during
the op and are dropped in cleanup after a successful retire.

### Why `--retire` is on `land` and not on `sync`

The dominant "land my work" direction is workweave → target, which is
`land`. The retiring workweave is the CWD.
Putting `--retire` on `sync` would require the operator to name the
workweave to delete from the parent's CWD — a less natural invocation
shape, and one that would silently delete something outside CWD.

### Why `--retire`

"Retire" states directly what the flag does —
close out this workweave — regardless of where in the workweave tree it
sits.

### If retire fails

A failed retire check keeps the op record, stopped at retire. The landing
into the target has already completed, and the workweave is preserved. The
repair loop:

```bash
# fix what the refusal names (commit or discard the edits), then:
rwv land --continue    # re-runs the retire check

# or roll back the entire op:
rwv abort
```

`rwv abort` restores both CWD and the target to their pre-op savepoints.
See [stops and abort](./stops-and-abort.md).

## Deletion

To delete without landing:

```bash
rwv workweave delete <name>
```

Refuses if:

- Any checkout has **uncommitted changes** (staged, unstaged, or untracked
  files outside `.gitignore`).
- Any checkout's HEAD is detached holding **commits no branch or
  remote-tracking branch reaches** — work that would be lost with the
  directory.
- The workweave's branch is one other branches' records land into.

Both checks cover every checkout under the workweave, not only the members
the project's manifest names: a member replay dropped but could not prune, a
member the workweave added that has not landed, or a clone put there by
hand. The refusal lists the dirty or diverged paths, marking those outside
the manifest `(not a manifest member)`. Options:

- Commit the work and land it: `rwv land --retire`
- Set the uncommitted changes aside or discard them, then delete
- Consent to losing everything: `rwv workweave delete <name>
  --discard-uncommitted --discard-unlanded`

**`--discard-uncommitted`** waives the first refusal, **`--discard-unlanded`**
the second, and also deletes a branch of the workweave that has not landed.
Passing both consents to losing the workweave's branches and checkouts, and
keeps their contents: each flag makes a backup of what it waives before
anything is removed (`rwv backup list` shows them, `rwv backup drop` drops
them).

A branch that has not landed is not a refusal. The workweave is removed and the
deletion rule keeps the branch, listed with its reason, and the exit status is
`3`. Whether a branch landed is judged against the branch the workweave's
record lands into, by ancestry: the project branch of that name, and in each
member its branch for that target. `--delete-remote` and
`--delete-remote-unlanded <repo>` extend the rule to the push branches. A member
whose checkout keeps a submodule store is asked, before the store goes, whether it
holds work no remote has (`dropped-repo-has-unique-commits`, `remotes-unconfirmed`,
`--discard-unpublished`). A workweave whose directory is already gone is deleted the same way.
[The deletion rule](./deletion-rule.md) states all of it; see also
[`rwv branch delete`](../../reference/cli.md#rwv-branch).

## Lifecycle in one view

```
create ──→ work ──→ land --retire ──→ (gone)
             │            │
             │      (fail: preserve, fix, --continue or abort)
             │
             └──→ land (without --retire) ──→ work ──→ delete
                                                            │
                                        (--discard-* if dirty)
```

The tool enforces every transition that involves rwv-owned state or
commit reachability. The working state in the middle is plain git — the
workweave is a git worktree, and commits are ordinary git commits.

## Related joints

- [workweave-hierarchy](./workweave-hierarchy.md) — the tree model, parent
  tracking, one-hop semantics, and ephemeral branch naming.
- [landing](./landing.md) — what `land` composes and moves, and what `--retire` adds.
- [stops and abort](./stops-and-abort.md) — finishing or putting back a stopped landing.
- [deletion rule](./deletion-rule.md) — what deleting a workweave or branch keeps and refuses.
- [sync-semantics](./sync-semantics.md) — taking the target's work in before landing.
- [lock-as-derived](./lock-as-derived.md) — why `rwv.lock` is per-workspace
  and why it is regenerated (not merged) at each sync boundary.
