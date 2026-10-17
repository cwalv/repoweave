# Workweave lifecycle

A workweave has four stages: creation, working state, landing, and
deletion. The daily bring-work-home flow touches all four. This joint
covers what each stage means, what the tool enforces, and what the
operator must ensure.

The joint does not re-document the phase machine that `rwv sync-to`
runs internally — that lives in [sync-semantics](./sync-semantics.md).
It does not re-document the tree model or one-hop semantics — those live
in [workweave-hierarchy](./workweave-hierarchy.md). The focus here is
the observable lifecycle: what exists, when, and how to move it forward.

Feature work happens in workweaves. Canonical clones in the primary weave
stay on the branch `rwv.toml` declares for each member (`version`, which
`rwv add` records from the remote's default branch); a workweave puts every
member on a branch of its own and lands back with `rwv sync-to --retire`.

## Creation

```bash
rwv workweave <project> create <name>
```

Create forks from CWD's active workspace by default. Inside a workweave,
the fork source is the surrounding workweave; from a primary weave, it is
primary. To specify a different source:

```bash
rwv workweave <project> create <name> --from primary
rwv workweave <project> create <name> --from ../.workweaves/<project>--other
```

`--from` accepts `primary`, an absolute path, or a relative path resolved
against the primary weave root. The source must be a workspace whose
`rwv.lock` is committed and current.

### What create writes

1. One `git worktree` per member repo, whatever its role (plus the project
   repo), at `{workweave}/{repo-path}/`, each on a fresh flat ephemeral branch
   named `{project}--{name}` (with `/` in scoped project names encoded as `+`).
   Each worktree belongs to the canonical weave-root clone, so a nested
   workweave's worktrees belong to the same store as its parent's. A
   `role = "reference"` repo's tracked files are read-only. Where the
   filesystem allows, a member's files share storage with another workspace's
   copy rather than being written afresh.
2. `[workweave]` artifacts from `rwv.toml` (`copy = [...]` entries deep-copied;
   `link = [...]` entries are absolute symlinks into the source root).
3. A `.rwv-workweave` marker recording `{primary, project, parent}`.
   `parent` is the workspace forked from — it becomes the auto-target for
   bare `rwv sync-to` from inside the workweave. This is the workweave
   root's **only** identity file: no `.rwv-active` is written beside it.
   The pointer records project *selection*, which is primary-only — a
   workweave's project is fixed here at creation and cannot be switched —
   and the two files are mutually exclusive, one tier of the resolution
   chain rather than two. `rwv doctor` reports a root carrying both as a
   `weave-root-identity-conflict`.
4. Integration surfacing (surfaces symlinks to the workweave root without
   running install hooks; run `rwv materialize` inside the workweave when you
   want install hooks to execute).

### Dirty-source check

`create` checks for uncommitted changes in `projects/<project>/` before
proceeding. If the project directory is dirty it refuses with a clear
error:

```
Error: rwv workweave create: refusing to create workweave —
       projects/web-app has uncommitted changes:
  rwv.toml

To proceed, do one of:
  1. commit the changes: git -C projects/web-app commit
  2. stash the changes: git -C projects/web-app stash
  3. capture them into the workweave: rwv workweave web-app create payments --capture-dirty
```

The check covers only `projects/<project>/`. Member-repo worktrees are
forked at HEAD by `git worktree add`, so uncommitted edits in member
repos are not captured and not checked.

Pass `--capture-dirty` to opt in to capturing the dirty state. Captured
dirty state becomes an obstacle at retire time — if the same changes are
present in the parent, the project-repo replay will hit a conflict. Commit
or stash them inside the workweave before running `rwv sync-to`.

### Re-invocation (idempotent path)

If the workweave already exists and is clean, re-invoking `create` without
`--replace-existing` validates the `.rwv-workweave` marker (same primary and
project) and returns immediately. Non-git state written between invocations
(`.runtime/`, `.claude/`, etc.) is preserved in place. This is the standard
"ensure workweave exists" path.

Use `--replace-existing` to tear down and recreate from scratch. It refuses if
the existing workweave holds uncommitted changes or unmerged commits — the
operator must discard those explicitly with `rwv workweave <project> delete
<name> --discard-uncommitted --discard-unmerged-commits`.

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
`rwv sync-to`, a manual `rwv lock` is optional because `rwv sync-to`
auto-relocks CWD at op-start.

The workweave's project and member repos run on a flat ephemeral branch
(`{project}--{name}`). This branch name is what allows `rwv sync-to`
to push cleanly into primary's `main` without a detach-or-stash dance —
two worktrees never compete for the same branch name. See
[workweave-hierarchy](./workweave-hierarchy.md#ephemeral-branch-names-and-the-git-worktree-constraint).

### Absorbing upstream changes

If the parent workspace has advanced since the workweave was created
(another workweave landed, or work was committed directly in primary), use
`rwv sync` before landing:

```bash
rwv sync primary --strategy rebase
```

`rwv sync` absorbs the named workspace into CWD. The workweave's unique
commits are replayed on top of primary's new tip; `rwv.lock` is
regenerated. See [sync-semantics](./sync-semantics.md) for the full
direction-pair contract.

## Landing: `rwv sync-to --retire`

The one-liner to land work and close out the workweave:

```bash
cd ../.workweaves/web-app--payments
rwv sync-to --retire
```

Workweaves live at `<parent>/.workweaves/<project>--<name>/` — by default a sibling of the weave root, so from the weave root the path is `../.workweaves/<project>--<name>/`. Bare `rwv sync-to --retire` follows the single recorded `parent` edge in `.rwv-workweave` (see [workweave-hierarchy — One hop, not transitive](./workweave-hierarchy.md#one-hop-not-transitive)).

`rwv sync-to --retire` runs the `sync-to` phase machine ([sync-semantics](./sync-semantics.md)) — auto-relocking CWD if its member repos moved ahead of `rwv.lock`, rebasing CWD onto the target (whose lock must be fresh per `rwv doctor --locked`), regenerating `rwv.lock`, and fast-forwarding the target — and then runs the **retire** phase to delete the workweave on success.

### The retire contract

Retire is a phase in the sync-to machine, not a separate command. It runs
only after `advance-target` has completed successfully. It does two checks:

**Merged-check** — every member repo in CWD must have the same HEAD as
the corresponding repo in the target workspace. After a successful
`advance-target`, both sides have been fast-forwarded to CWD's converged
tips, so this should hold. Retire refuses (with the diverged repos listed)
if it does not.

The check compares **member repo tips**, not project repo tips. The
project repo's post-sync state can diverge from the target by the
auto-relock commit Phase 3 writes. That commit is purely derived
and will be regenerated on the parent's next sync, so comparing project
tips would refuse every retire, including the happy path. Manifest tip
equality is the accurate convergence signal.

**Dirty-check** — no worktree in the workweave may have uncommitted
changes. Any dirty path blocks retire.

### What retire deletes

On success, retire calls `delete_workweave` with `force: false`:

- Every member-repo worktree is removed (`git worktree remove`), whatever
  its role.
- Stale `.git/worktrees/` entries are pruned.
- Each repo's ephemeral branch (`{project}--{name}`) is deleted if its
  creation receipt is recorded in `.rwv-workweave-index`.
- The project-repo worktree and its ephemeral branch are removed.
- The workweave directory is removed.

Ephemeral branch deletion is what makes the `git branch -D` analogy
exact: once retire runs, the only refs to the workweave's commits are the
ones that landed in the parent during `advance-target`. Any commit not
reachable from the parent is permanently lost. The pre-op savepoints
(`refs/rwv/pre-op/<op-id>`) remain during the op but are dropped in cleanup
after a successful retire.

### Why `--retire` is on `sync-to` and not on `sync`

The dominant "land my work" direction is workweave → parent, which is
`sync-to`. The retiring workweave is the CWD; the parent is the target.
Putting `--retire` on `sync` would require the operator to name the
workweave to delete from the parent's CWD — a less natural invocation
shape, and one that would silently delete something outside CWD.

### Why not `--land`

"Land" overloads PR-merge vocabulary and misleads when the parent is not
primary. A nested workweave lands into its parent workweave, not into the
project's main branch. "Retire" states directly what the flag does —
close out this workweave — regardless of where in the workweave tree it
sits.

### If retire fails

A merged-check or dirty-check failure keeps the op record at phase
`retire`. The workweave is preserved. The repair loop:

```bash
# fix the divergence (commit, sync, etc.), then:
rwv sync-to --continue    # re-runs the retire check

# or roll back the entire op:
rwv abort
```

`rwv abort` restores both CWD and the target to their pre-op savepoints.
See the abort contract in [sync-semantics](./sync-semantics.md#abort-verified-restore-contract).

## Deletion

To delete without landing:

```bash
rwv workweave <project> delete <name>
```

Refuses if:

- Any worktree has **uncommitted changes** (staged, unstaged, or untracked
  files outside `.gitignore`).
- Any worktree HEAD holds **commits not reachable from the recorded parent
  or the primary weave** — work that would be permanently destroyed when
  the ephemeral branches are force-deleted.

The refusal lists the dirty or diverged paths. Options:

- Commit the work and land it: `rwv sync-to --retire`
- Stash or discard the uncommitted changes, then delete
- Consent to losing everything: `rwv workweave <project> delete <name>
  --discard-uncommitted --discard-unmerged-commits`

**`--discard-uncommitted`** waives the first refusal, **`--discard-unmerged-commits`**
the second. Passing both is the `git branch -D` contract: it consents to
destroying whatever is in the workweave. The same sequence runs as retire's
delete step (worktrees removed, branches force-deleted, directory removed),
but without the waived check.

The diverged-commit check uses **both** the recorded parent and the primary
weave as baselines: work counts as merged when it is reachable from either.
This means a nested workweave's commits that have landed in the parent
workweave (but not yet in primary) are not flagged as unmerged.

## Lifecycle in one view

```
create ──→ work ──→ sync-to --retire ──→ (gone)
             │            │
             │      (fail: preserve, fix, --continue or abort)
             │
             └──→ sync-to (without --retire) ──→ work ──→ delete
                                                            │
                                        (--discard-* if dirty)
```

The tool enforces every transition that involves rwv-owned state or
commit reachability. The working state in the middle is plain git — the
workweave is a git worktree, and commits are ordinary git commits.

## Related joints

- [workweave-hierarchy](./workweave-hierarchy.md) — the tree model, parent
  tracking, one-hop semantics, and ephemeral branch naming.
- [sync-semantics](./sync-semantics.md) — the full phase machine,
  retire-as-phase detail, abort contract, and strategy choices.
- [lock-as-derived](./lock-as-derived.md) — why `rwv.lock` is per-workspace
  and why it is regenerated (not merged) at each sync boundary.
