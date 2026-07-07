# rwv abort

## Purpose

Restore CWD's workspace to its pre-sync state using savepoint refs written by
`rwv sync` or `rwv sync-to` at the start of the operation. Call `abort` when a
sync or sync-to has left the workspace in an unresolvable state and you want to
get back to the last known-good position rather than continue forward.

`abort` reads op-state to find the in-flight operation. The **owner record**
(`.rwv-op`) lives at the initiating workspace (CWD for `rwv sync`; CWD for
`rwv sync-to`). Every other workspace the op mutates holds a **thin lease**
(`.rwv-op-lease`) pointing back at the owner. `abort` can be invoked from
any involved workspace — from a leased workspace it follows the pointer to
the owner record automatically. For `sync-to` ops, both CWD and the recorded
target workspace are rolled back.

### What is restored

For each manifest repo and the project repo in every involved workspace, abort
applies a two-rail verified-restore:

**Rail 1 — pre-abort reference.** Before restoring any repo, a durable
reference at the repo's current tip is written at `refs/rwv/pre-abort/<op-id>`
(first-write-wins — if the ref already exists from a prior abort attempt, the
earlier capture is preserved). This reference is never deleted by abort's
cleanup; abort is itself undoable via that ref.

**Rail 2 — HEAD-verified restore.** The destructive reset to the savepoint
happens only when the repo's current tip is attributable to the op. The
classification:

| Current tip | Outcome |
|---|---|
| Equal to the savepoint | `untouched` — op never moved this repo; HEAD not touched |
| Equal to the recorded intent tip (`advanced_tips[repo]`) | `restored (from recorded intent tip)` — op advanced this repo during replay before crashing; reset to savepoint |
| Equal to the recorded converged tip (`converged_tips[repo]`) | `restored (from recorded converged tip)` — op converged repo before crash; reset to savepoint |
| Repo in a VCS-native mid-op state (rebase / merge / cherry-pick) | `restored (from mid-op state)` — mid-op cancelled; reset to savepoint |
| Anything else | `foreign-tip violation` — restore refused; violation reported |

The `advanced_tips` map is the op's **advancement-intent journal**: written
during the replay phase, it records the planned target tip for genuine
fast-forward advances (before the advance), and the actual post-rebase tip
for rebased advances (right after the rebase succeeds). It is cleared
atomically when `converged_tips` is written at relock completion. This means
a mid-replay crash where the op cleanly advanced repos no longer produces
foreign-tip refusals on those repos — they auto-restore as `restored (from
recorded intent tip)`.

The residual foreign-tip case fires only for genuinely-foreign tips (e.g. an
operator commit made after the crash), plus the irreducible one-write window
between a rebase completing and its tip being persisted into `advanced_tips`
(a documented floor — the tip cannot be recorded before it exists; degrades
to today's behavior for that instant only).

The foreign-tip case means commits landed in the repo that abort cannot
attribute to the op. Abort reports the violation, retains op-state (so the
operator can re-run `rwv abort` after manually reconciling), and exits
non-zero.

The op-state file (`.rwv-op`) is removed from all involved workspaces only
when all repos restored without a foreign-tip violation.

### What is **not** restored

- **Uncommitted working-tree changes** that existed before the sync started.
  The savepoint is a git commit pointer; it does not record staged or
  unstaged edits.
- **Side effects outside the git history** — integration-generated files,
  `node_modules/`, `.venv/`, etc. are not snapshotted and are not rolled back.

### When `abort` refuses

`abort` returns an error with `no operation in progress` when no `.rwv-op`
owner record or `.rwv-op-lease` thin lease is found in CWD's workspace.

This can happen if:
- No sync or sync-to was ever started.
- The operation already completed successfully and the op-state was cleaned up.
- The op-state was removed manually.
- A prior `rwv abort` completed cleanly and cleared op-state.

`abort` also refuses per-repo (foreign-tip violation) when a repo's current tip
cannot be attributed to the op. In that case op-state is retained so the
operator can re-run after reconciling.

### After `--discard-local-commits` sync

When `rwv sync --discard-local-commits` discards a project repo's committed
divergence, the savepoint for the project repo is kept as a **tombstone**
even after the successful sync completes. The op-state file is still removed
(the op succeeded), so `rwv abort` will refuse — but the tombstone ref at
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

Per-repo restoration lines to stdout. Each line names the outcome:

```
  <repo-path>: restored (from recorded intent tip)
  <repo-path>: restored (from recorded converged tip)
  <repo-path>: restored (from mid-op state)
```

Non-actionable outcomes (`untouched` and `no savepoint`) are demoted to a
single aggregate summary line printed at the end:

```
  summary: N repo(s) skipped (no savepoint), N untouched (tip == savepoint)
```

Foreign-tip violations are printed to stderr with the observed tip, the
expected savepoint, the recorded converged tip (if any), the pre-abort ref
label, and a list of blocking commits between the savepoint and the observed
tip (`git log savepoint..tip`, capped at 5 with a count of any remainder).
Each foreign-tip block also notes whether the tip is strictly ahead of or
diverged from the savepoint.

The recovery-options block is printed exactly once at the end (to stderr)
when at least one repo refused, with only the operator-facing choices:

- if a foreign agent advanced the branch after the crash: move the branch
  back and re-run `rwv abort`.
- if you want to keep the foreign tip and discard the op: move the branch
  off the pre-abort ref and delete the savepoint manually.

On failure for any repo, `abort` continues to the next repo before exiting
non-zero.

## Exit codes

- `0` — all repos restored successfully (or had no savepoint, i.e., nothing
  to restore); op-state cleared.
- non-zero — no operation in progress, foreign-tip violation on at least one
  repo (op-state retained), or one or more repos failed to restore.

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

Recover from a foreign-tip violation (another agent advanced the branch):

```
rwv abort
# ... foreign-tip violation for github/foo/bar ...
# Manually move the branch back to the savepoint SHA shown in the message:
cd github/foo/bar
git update-ref refs/heads/<branch> <savepoint-sha>
cd ...
rwv abort   # re-run; op-state was retained
```

## Common errors

- *no operation in progress* — no `.rwv-op` file found. Either no sync is
  in flight, or the op already completed and was cleaned up. Nothing to abort.
- *foreign-tip violation* — a repo's HEAD does not match the savepoint, the
  recorded intent tip (`advanced_tips`), the recorded converged tip
  (`converged_tips`), or a VCS-native mid-op state. Likely causes: a foreign
  agent advanced the branch after the op crashed, or the op crashed in the
  sub-second window between a rebase completing and its tip being persisted
  (the documented one-write-window floor). See the violation message for
  recovery options; the pre-abort ref captures the tip for later recovery.
  Op-state is retained so you can re-run `rwv abort` after reconciling.
- *create pre-abort ref failed* — abort could not write the pre-abort reference
  before attempting restore. The restore is not attempted when this fails, since
  information-preservation is the first obligation.
