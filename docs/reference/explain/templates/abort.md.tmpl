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
| Anything else, in a repo named on `--abandon-foreign-tip` | `restored (abandoned foreign tip)` — reset to savepoint; the foreign tip stays reachable at the rail-1 reference |

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

### Abandoning a foreign tip on purpose

`--abandon-foreign-tip=<repo>` is the operator's answer to "these foreign
commits are not wanted". It waives rail 2 for the repo it names — abort resets
that repo to the savepoint even though the tip is unattributable — and waives
nothing anywhere else. Repeat it per repo. **There is no all-repos form**, by
design: whether abandoning a repo's foreign commits is acceptable is a
judgement about those specific commits, and a blanket spelling would answer it
for repos the operator never looked at. `abort` with no flag still refuses.

The flag is named for the consequence it permits, and the name is accurate
because rail 1 runs first: the pre-abort reference already holds the tip being
left behind, so the commits remain reachable at `refs/rwv/pre-abort/<op-id>`
after the branch moves off them. Recovering them is `git reset --hard` onto
that ref.

Spell `<repo>` as abort's own per-repo output does — the workspace-relative
path (`github/foo/bar`) or `(project)` for the project repo. A `sync-to` op
consults the consent once per side, so naming a repo covers that repo in both
the source and the target workspace. Naming a repo that is not refused does
nothing.

**Abort refuses even with the flag** when the pre-abort reference does not
already hold the observed tip. This happens when the branch advanced between
two abort runs: the reference is first-write-wins, so it still holds the
earlier run's capture, and resetting would leave the commits made since that
capture reachable from nothing. The flag consents to abandoning commits, not
to destroying them, so the refusal stands and names this as its reason.
Re-point or copy off the newer commits before re-running.

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
rwv abort --abandon-foreign-tip=<repo> [--abandon-foreign-tip=<repo> ...]
```

`abort` reads everything else it needs from `.rwv-op`. The one flag is the
per-repo waiver described above; without it `abort` never moves a branch off
commits it cannot attribute to the op.

Run `rwv --help abort` for the full clap surface.

## Output

Per-repo restoration lines to stdout. Each line names the outcome:

```
  <repo-path>: restored (from recorded intent tip)
  <repo-path>: restored (from recorded converged tip)
  <repo-path>: restored (from mid-op state)
  <repo-path>: restored (abandoned foreign tip, per --abandon-foreign-tip)
```

The abandoned-tip line is followed by the tip that was left behind and the
reference it stays reachable at, so the abandonment is recoverable from the
transcript alone.

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

- if the foreign commits are wanted: move the branch back to them and re-run
  `rwv abort`.
- if they are not: re-run as `rwv abort --abandon-foreign-tip=<repo>`.
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

Recover from a foreign-tip violation (another agent advanced the branch)
when the foreign commits are not wanted:

```
rwv abort
# ... foreign-tip violation for github/foo/bar ...
rwv abort --abandon-foreign-tip=github/foo/bar   # op-state was retained
# ... github/foo/bar: restored (abandoned foreign tip) ...
# the abandoned commits remain at refs/rwv/pre-abort/<op-id>
```

When they ARE wanted, put the branch back on them yourself and re-run with no
flag, so the tip abort sees is one it can attribute to the op.

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
  Op-state is retained so you can re-run `rwv abort` after reconciling, or
  with `--abandon-foreign-tip=<repo>` if the foreign commits are unwanted.
- *foreign-tip violation on a repo named by `--abandon-foreign-tip`* — the
  pre-abort reference holds an earlier tip than the observed one, so the
  consent could not be honoured without destroying the commits in between.
  The message names the captured tip. Re-point or copy off those commits,
  then re-run.
- *create pre-abort ref failed* — abort could not write the pre-abort reference
  before attempting restore. The restore is not attempted when this fails, since
  information-preservation is the first obligation.
