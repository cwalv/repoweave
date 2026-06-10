# rwv abort

## Purpose

Restore CWD's workspace to its pre-sync state using savepoint refs written by
`rwv sync` or `rwv sync-to` at the start of the operation. Call `abort` when a
sync or sync-to has left the workspace in an unresolvable state and you want to
get back to the last known-good position rather than continue forward.

`abort` reads the `.rwv-op` op-state file to find the in-flight operation's
id and the list of involved workspaces. For `sync-to` ops, both CWD and the
recorded target workspace are rolled back.

### What is restored

For each manifest repo and the project repo in every involved workspace:

1. If the repo is in a mid-operation VCS state (`mid-rebase`, `mid-merge`,
   `mid-cherry-pick`), the native VCS abort is run first (`git rebase --abort`,
   `git merge --abort`, `git cherry-pick --abort`).
2. The repo is then reset to the savepoint ref at
   `refs/rwv/pre-op/<op-id>` via `git reset --hard <savepoint-sha>`. The
   savepoint ref is deleted after the reset.

The op-state file (`.rwv-op`) is removed from all involved workspaces on
completion.

### What is **not** restored

- **Uncommitted working-tree changes** that existed before the sync started.
  The savepoint is a git commit pointer; it does not record staged or
  unstaged edits. Savepoints are created after the dirty-check phase, so any
  uncommitted changes that survived the dirty check are outside the savepoint's
  scope.
- **Side effects outside the git history** — integration-generated files,
  `node_modules/`, `.venv/`, etc. are not snapshotted and are not rolled back.

### When `abort` refuses

`abort` returns an error with `no operation in progress` when no `.rwv-op`
file is found in CWD's workspace. There is nothing to roll back.

This can happen if:
- No sync or sync-to was ever started.
- The operation already completed successfully and the op-state was cleaned up.
- The op-state was removed manually.

### After `--force` sync

When `rwv sync --force` discards a project repo's committed divergence, the
savepoint for the project repo is kept as a **tombstone** even after the
successful sync completes. The op-state file is still removed (the op
succeeded), so `rwv abort` will refuse — but the tombstone ref at
`refs/rwv/pre-op/<op-id>` remains and can be recovered manually:

```
git reset --hard refs/rwv/pre-op/<op-id>
```

Manifest-repo savepoints are deleted on success regardless; only the project
repo tombstone is preserved.

## Invocation

```
rwv abort
```

No flags. `abort` reads everything it needs from `.rwv-op`.

Run `rwv --help abort` for the full clap surface.

## Output

Per-repo restoration lines to stdout:

```
  <repo-path>: restored
```

On failure for any repo, the error is printed to stderr and `abort` continues
to the next repo before exiting non-zero.

## Exit codes

- `0` — all repos restored successfully (or had no savepoint, i.e., nothing
  to restore).
- non-zero — no operation in progress, or one or more repos failed to restore.

## Examples

Roll back a sync that left repos in a bad state:

```
rwv sync primary --strategy rebase
# ... rebase conflict that cannot be resolved ...
rwv abort
```

Roll back a sync-to (rolls back both CWD and the target workspace):

```
rwv sync-to primary
# ... conflict mid-op ...
rwv abort
```

## Common errors

- *no operation in progress* — no `.rwv-op` file found. Either no sync is
  in flight, or the op already completed and was cleaned up. Nothing to abort.
- *reset --hard failed* — the git operation failed for some repo. Inspect
  the repo directly; the savepoint ref at `refs/rwv/pre-op/<op-id>` is still
  present and can be used for manual recovery.
