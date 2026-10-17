# Recover from a sync conflict

When `rwv sync` or `rwv sync-to` pauses on a merge or rebase conflict, resolve the conflicted files in the named repository, stage them, and resume from the weave root with `--continue` (or roll back with `rwv abort`):

```bash
# 1. Edit conflicted files and stage them in the named repository
cd github/chatly/server
# edit conflicted files ...
git add <files>

# 2. Resume from the weave root where the sync started
cd ../..
rwv sync --continue      # or: rwv sync-to --continue
```

To abandon the sync instead and restore every repository in the involved workspaces to its pre-op savepoint:

```bash
rwv abort
```

## Reading the conflict output

When a project-repo replay or member-repo rebase hits a textual conflict, `rwv` prints a per-repo line naming the conflicted checkout followed by a summary with resolution instructions:

```text
  github/chatly/server: Rebase in <weave-root>/github/chatly/server hit a conflict; resolve and continue, or abort to roll back
Error: sync hit conflicts in one or more member repos (see per-repo lines above).

To resolve each conflicted repo:
cd <repo>
  # edit conflicted files
  git add <files>
rwv sync --continue   # resume; already-converged repos are no-ops

If you'd rather roll everything back: `rwv abort`.
```

`--continue` reads all original parameters — source or target, `--strategy`, `--retire`, and any consent flags — from the saved op-state record. Do not pass the original flags again alongside `--continue`; any flag other than `--project` is rejected.

If you step through a multi-commit rebase manually with `git rebase --continue`, the repository's `merge.rwv-ours.driver` configuration keeps `rwv.lock` conflict-free across picks. Once the manual rebase finishes, run `rwv sync --continue` (or `rwv sync-to --continue`) so `rwv` regenerates `rwv.lock` and clears the op-state record.

## Common conflict cases

- **Project repo (`projects/<name>/`) conflict:** Two workspaces edited the same non-lock file in the project repo. (`rwv.lock` itself is excluded during replay and regenerated at the end.) Resolve the file in `projects/<name>/`, run `git add`, and resume with `--continue`.
- **Member repo conflict:** A member repo's branch has a textual conflict against the incoming target revision. Resolve the conflicted files in that member checkout, run `git add`, and resume with `--continue`.
- **`rwv sync-to --retire` conflict:** When `--retire` hits a conflict, the workweave is preserved so you can resolve it. After staging fixes, run bare `rwv sync-to --continue` — `--retire` is remembered in op-state and runs automatically once the sync finishes.

For inspecting in-flight op-state, handling `foreign-tip` refusals during `rwv abort`, or recovering pre-abort commits from `refs/rwv/pre-abort/<op-id>`, see [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md).

## Related

- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspecting mid-op state, `--continue` rules, and `rwv abort` recovery
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave changes with `rwv sync-to`
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, lock exclusion, and savepoint protocol
- [VCS as seam](../explanation/joints/vcs-as-seam.md) — how VCS conflict state is surfaced
