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

## Creation

```bash
rwv workweave <project> create <name>
```

Create forks from CWD's active workspace by default. Inside a workweave,
the fork source is the surrounding workweave; from a primary weave, it is
primary. To specify a different source:

```bash
rwv workweave <project> create <name> --from primary
rwv workweave <project> create <name> --from .workweaves/<project>--other
```

`--from` accepts `primary`, an absolute path, or a relative path resolved
against the primary weave root. The source must be a workspace whose
`rwv.lock` is committed and current.

### What create writes

1. One `git worktree` per `owned`/`fork`/`dependency` manifest repo (plus the
   project repo), at `{workweave}/{repo-path}/`, each on a fresh ephemeral
   branch named `{project}--{name}/{source-branch}`. `role: reference` repos
   are instead **symlinked** to the canonical weave-root clone
   (`<primary_root>/{repo-path}`): read-only study material wants the same
   lock-pinned ref in every workweave, so a worktree's per-workweave branch
   isolation is moot while its full working-tree duplication cost is not. The
   symlink targets PRIMARY's canonical (never the source workspace), so a
   nested workweave never chains symlink→symlink. `--worktree-references`
   restores the worktree behavior for reference repos.
2. `workweave:` artifacts from `rwv.yaml` (`copy:` entries deep-copied;
   `link:` entries are absolute symlinks into the source root).
3. A `.rwv-workweave` marker recording `{primary, project, parent}`.
   `parent` is the workspace forked from — it becomes the auto-target for
   bare `rwv sync-to` from inside the workweave.
4. `.rwv-active` set to `project`.
5. Integration activation (context verb: surfaces symlinks, skips install
   hooks).

The `.rwv-workweave` parent field is anchored at
`src/workspace.rs::WorkweaveMarker`.

### Dirty-source check

`create` checks for uncommitted changes in `projects/<project>/` before
proceeding. If the project directory is dirty it refuses with a clear
error:

```
Error: rwv workweave create: refusing to create workweave —
       projects/web-app has uncommitted changes:
  rwv.yaml

To proceed, do one of:
  1. commit the changes: git -C projects/web-app commit
  2. stash the changes: git -C projects/web-app stash
  3. capture them into the workweave: rwv workweave web-app create payments --capture-dirty
```

The check covers only `projects/<project>/`. Manifest-repo worktrees are
forked at HEAD by `git worktree add`, so uncommitted edits in manifest
repos are not captured and not checked.

Pass `--capture-dirty` to opt in to capturing the dirty state. Captured
dirty state becomes an obstacle at retire time — if the same changes are
present in the parent, the project-repo replay will hit a conflict. Commit
or stash them inside the workweave before running `rwv sync-to`.

The dirty-source check is anchored at
`src/workweave.rs::create_workweave`.

### Re-invocation (idempotent path)

If the workweave already exists and is clean, re-invoking `create` without
`--force` validates the `.rwv-workweave` marker (same primary and project)
and returns immediately. Non-git state written between invocations
(`.runtime/`, `.claude/`, etc.) is preserved in place. This is the Gas
City rig's standard "ensure workweave exists" path.

Use `--force` to tear down and recreate from scratch. `--force` refuses if
the existing workweave holds uncommitted changes or unmerged commits — the
operator must discard those explicitly with `rwv workweave <project> delete
<name> --force`.

## Working state

Inside the workweave, edit, test, and commit as usual. The workspace
wiring makes cross-repo imports resolve to the worktrees.

```bash
cd github/chatly/protocol
git commit -am "protocol: add payment fields"

cd ../server
git commit -am "server: add /payments endpoint"

cd ../..
rwv lock
git -C projects/web-app commit -am "lock: payments feature"
```

`rwv lock` is per-workspace: it updates the workweave's `rwv.lock`, not
primary's. Each workspace owns its own lock; see
[lock-as-derived](./lock-as-derived.md).

The workweave's project repo runs on an ephemeral branch
(`{project}--{name}/main`). This branch name is what allows `rwv sync-to`
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
cd .workweaves/web-app--payments
rwv sync-to --retire
```

`rwv sync-to --retire` is the centerpiece of the bring-work-home flow. It
runs the full sync-to machine then deletes the workweave on success. The
sequence:

```
guard → mark → savepoint → replay → relock → advance-target → retire → cleanup
```

1. **replay** — CWD's unique project commits are replayed onto the parent's
   project tip (with `rwv.lock` excluded from each commit's diff). Manifest
   repos in CWD are aligned to the parent's lock targets. CWD advances to
   the new tip, linearly on top of the parent's prior state.
2. **relock** — `rwv.lock` is regenerated from post-replay manifest tips
   and committed into CWD automatically if it changed.
3. **advance-target** — the parent's project repo (and manifest repos)
   fast-forward to CWD's new tip.
4. **retire** — merged-check and dirty-check; then delete the workweave.

The retire phase is the only place the workweave is deleted. The first
three steps are identical to a plain `rwv sync-to`; `--retire` adds step 4.

### Preconditions

Before step 1 runs, `rwv sync-to --retire` verifies both workspaces
satisfy `rwv doctor --locked`: every repo's tip must match its `rwv.lock`.
Concretely, in the workweave:

```bash
rwv lock
git -C projects/web-app commit -am "lock: payments feature"
```

The lock-precondition check is the guard phase. Precondition failures
leave no trace.

### The retire contract

Retire is a phase in the sync-to machine, not a separate command. It runs
only after `advance-target` has completed successfully. It does two checks:

**Merged-check** — every manifest repo in CWD must have the same HEAD as
the corresponding repo in the target workspace. After a successful
`advance-target`, both sides have been fast-forwarded to CWD's converged
tips, so this should hold. Retire refuses (with the diverged repos listed)
if it does not.

The check compares **manifest repo tips**, not project repo tips. The
project repo's post-sync state normally diverges from the target by exactly
the auto-relock commit — Phase 3 always writes the workweave's `workweave:`
field into the lock, which the parent's lock lacks. That commit is purely
derived and will be regenerated on the parent's next sync, so comparing
project tips would refuse every retire, including the happy path. Manifest
tip equality is the honest convergence signal.

Anchored at `src/sync.rs::retire_workweave_after_sync_to`.

**Dirty-check** — no worktree in the workweave may have uncommitted
changes. Any dirty path blocks retire.

### What retire deletes

On success, retire calls `delete_workweave` with `force: false`:

- Every manifest-repo worktree is removed (`git worktree remove`).
- Each `role: reference` symlink is unlinked (`remove_file`, never followed),
  so the shared canonical clone it aliases is left untouched — no worktree
  remove, no branch delete, no mutation of that store.
- Stale `.git/worktrees/` entries are pruned.
- All ephemeral branches (`{project}--{name}/*`) are force-deleted.
- The project-repo worktree and its ephemeral branches are removed.
- The workweave directory is removed.

Ephemeral branch force-deletion is what makes the `git branch -D` analogy
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
project's main branch. "Retire" is honest about what the flag does —
close out this workweave — regardless of where in the workweave tree it
sits.

### One hop, not transitive

Bare `rwv sync-to --retire` follows the single recorded parent edge. From
a nested workweave, landing into the grandparent (primary) takes two
invocations:

```bash
cd .workweaves/web-app--feat-child
rwv sync-to --retire               # → parent workweave; deletes child

cd ../web-app--feat
rwv sync-to --retire               # → primary; deletes parent
```

Or one invocation with an explicit target:

```bash
cd .workweaves/web-app--feat-child
rwv sync-to primary --retire
```

Explicit-target `sync-to` always works; the one-hop default is a
discipline guard against accidental skip-edge landings.

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

Without `--force`, refuses if:

- Any worktree has **uncommitted changes** (staged, unstaged, or untracked
  files outside `.gitignore`).
- Any worktree HEAD holds **commits not reachable from the recorded parent
  or the primary weave** — work that would be permanently destroyed when
  the ephemeral branches are force-deleted.

The refusal lists the dirty or diverged paths. Options:

- Commit the work and land it: `rwv sync-to --retire`
- Stash or discard the uncommitted changes, then delete
- Consent to losing everything: `rwv workweave <project> delete <name> --force`

**`--force`** matches the `git branch -D` contract: it consents to
destroying whatever is in the workweave. The same sequence runs as retire's
delete step (worktrees removed, branches force-deleted, directory removed),
but without any merged-check or dirty-check.

The diverged-commit check uses **both** the recorded parent and the primary
weave as baselines: work counts as merged when it is reachable from either.
This means a nested workweave's commits that have landed in the parent
workweave (but not yet in primary) are not flagged as unmerged.

Anchored at `src/workweave.rs::delete_workweave`.

## Lifecycle in one view

```
create ──→ work ──→ sync-to --retire ──→ (gone)
             │            │
             │      (fail: preserve, fix, --continue or abort)
             │
             └──→ sync-to (without --retire) ──→ work ──→ delete
                                                            │
                                              (--force if dirty)
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
- [pyramid-of-stability](./pyramid-of-stability.md) — the project-repo tier
  the workweave's commits eventually land in.
