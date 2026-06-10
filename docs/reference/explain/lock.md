# rwv lock

## Purpose

Snapshot each manifest repo's current HEAD into `rwv.lock`. No network
access, no integration hooks, no manifest mutations. `lock` is a pure
local operation: it reads each repo's HEAD SHA from disk and writes the
resulting snapshot into the project's `rwv.lock`.

### Lock as derived state

`rwv.lock` is derived state — its contents are fully determined by the
current HEAD SHAs of the manifest repos. Running `lock` on the same
inputs always produces the same lock. This has two practical consequences:

1. Hand-editing `rwv.lock` has no lasting effect: the next `lock` run
   overwrites with whatever HEAD says.
2. Lock conflicts across workweaves are never meaningful: after any merge,
   the correct lock is the snapshot of the post-merge tips. `rwv sync`
   encodes this by excluding the lock from the merge phase and regenerating
   it in Phase 3.

See [lock-as-derived](../explanation/joints/lock-as-derived.md) for the
full conceptual treatment.

### When to run `lock`

Run `rwv lock` after committing changes across manifest repos (or in a
single repo) to record the new cross-repo state. Common patterns:

- Agent work sessions: run `rwv lock` before landing via `rwv sync-to --retire`
  so the workweave carries a coherent lock.
- Manual cross-repo feature: commit in each affected repo, then `rwv lock`
  to capture the joint state.
- After `rwv add`/`rwv remove`: those verbs regenerate integration files
  (intent-verb path) but do not auto-lock; run `rwv lock --commit` to
  checkpoint the new membership.

Do **not** run `rwv lock` in place of `rwv update`. `lock` snapshots
*current local HEADs*; `update` fetches from the network and advances tips.
If you want the latest upstream commits, use `rwv update`.

### Stale-lock relationship

A "stale lock" (`rwv doctor` violation `stale-lock`) means the current repo
HEADs no longer match what `rwv.lock` records. Running `rwv lock` clears
stale-lock violations by re-snapshotting to match current HEADs.

`rwv doctor --locked` is the scriptable precondition check: zero exit iff
every repo tip matches the lock.

## Invocation

```
rwv lock [--dirty] [--commit] [--project <name>]
```

- `--dirty` — skip the uncommitted-changes check. By default, `lock` refuses
  to snapshot any repo with uncommitted changes (staged, unstaged) — their
  content has no SHA and cannot be reproducibly recovered. `--dirty` bypasses
  this check; use only when you deliberately want to snapshot an in-flight state
  (e.g., a workweave that hasn't been cleaned up yet).
- `--commit` — after writing `rwv.lock`, stage and commit it from the project
  directory with an auto-generated message listing which repos advanced. Refuses
  if the project repo has uncommitted changes outside `rwv.lock` — the
  auto-commit must not bundle unrelated work-in-progress.
- `--project <name>` — operate on this project rather than the active project.
  Does not change `.rwv-active`.

Run `rwv --help lock` for the full clap surface.

## Output

`Wrote <path>` to stderr on success (one line naming the lock file written).

When `--commit` is set, also prints `Committed rwv.lock` (or `Lock unchanged,
nothing to commit.` if no repos advanced).

## Exit codes

- `0` — lock written successfully.
- non-zero — workspace could not be resolved, manifest parse failure, a repo
  has uncommitted changes (without `--dirty`), or a git operation failed.

## Examples

Snapshot current HEADs:

```
rwv lock
```

Snapshot and commit in one step:

```
rwv lock --commit
```

Snapshot even when repos have uncommitted changes:

```
rwv lock --dirty
```

## Common errors

- *repo has uncommitted changes; commit or use --dirty to override* — one or
  more repos have staged or unstaged edits. Commit the changes, or pass
  `--dirty` to snapshot the committed portion of HEAD anyway.
- *project repo has uncommitted changes outside rwv.lock* — `--commit` was
  passed but the project repo has work-in-progress beyond the lock file.
  Commit or stash the other changes before using `--commit`.
- *workspace could not be resolved* — CWD is not inside a weave or workweave;
  run from within the workspace.
