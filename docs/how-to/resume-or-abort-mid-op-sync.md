# Resume or abort a mid-op sync

When `rwv sync` or `rwv sync-to` is interrupted — by a conflict, a crash, or
a Ctrl-C — op-state is left in place so you can either pick up where you left
off or roll everything back to the pre-op position. This guide covers both
paths.

## Step 1 — inspect what is in flight

```bash
rwv status
```

`rwv status` is read-only and never mutates anything. It reports each repo's
branch tip, lock relation, and mid-op state. A repo that is mid-rebase,
mid-merge, or mid-cherry-pick will show a non-null `mid_op` field (text output:
bracketed annotation on the row; JSON: `"mid_op": "<op>"`).

To see the raw op-state records, check the workspace root for the `.rwv-op`
file (owner record) or `.rwv-op-lease` file (thin lease pointer written in the
target workspace of a `sync-to`):

```bash
# Is an operation in progress in this workspace?
ls -la .rwv-op .rwv-op-lease 2>/dev/null

# Inspect the owner record (present in CWD for sync; in CWD for sync-to)
cat .rwv-op

# Inspect the lease (present in the target workspace for sync-to)
cat .rwv-op-lease
```

The owner record names the operation id, the phase the machine was in when the
crash occurred, the recorded target (for `sync-to`), and any overrides
(`--allow-stale-lock`, `--discard-local-commits`) that were consented to at
invocation time. These are the parameters `--continue` uses — you do not need
to re-supply them.

For `rwv sync-to`, if you are in the **target** workspace (not CWD), the lease
file points back to the owner workspace. Run the commands below from the owner
workspace (the one that has `.rwv-op`, not `.rwv-op-lease`).

## Step 2a — resume with `--continue`

If you want to finish the operation, resolve any conflict first (the error
message names the repo and the exact next steps), then re-run with bare
`--continue`:

**After a `rwv sync` rebase conflict:**

```bash
# 1. Resolve the conflict in the named repo
cd github/chatly/server
# edit conflicted files
git add <files>

# 2. Resume the sync from the workspace root — rwv drives the rest
cd <workspace-root>
rwv sync --continue
```

**After a `rwv sync-to` rebase conflict:**

```bash
# 1. Resolve the conflict in the named repo
cd github/chatly/server
# edit conflicted files
git add <files>

# 2. Resume the sync-to from the workweave root — rwv drives the rest
cd <workweave-root>
rwv sync-to --continue
```

For merge or cherry-pick conflicts (rare, non-rebase strategies), the error
message includes `git merge --continue` or `git cherry-pick --continue` as the
intermediate VCS step before re-running `rwv <verb> --continue`.

**Fallback: bare `git rebase --continue`.** If you need to resume a stopped
rebase outside rwv, bare `git rebase --continue` is safe — the durable
`merge.rwv-ours.driver` repo-local config keeps the lock exclusion active. After
the bare-git resume finishes, re-run `rwv sync --continue` (or
`rwv sync-to --continue`) so rwv can complete relocking and clean up op-state.
This is a fallback path; prefer the `resolve → git add → rwv --continue` loop.

### What `--continue` does (and does not) need

`--continue` reads all parameters — source/target, strategy,
`--allow-stale-lock`, `--discard-local-commits`, `--retire` — from the
persisted op-state owner record. **Do not re-supply the original flags.** A
mismatch between the command line and the recorded op-state is an error, so
bare `--continue` is the safe default.

The driver re-enters whichever phase was in progress at the time of the
interruption and continues from there. Already-converged repos are no-ops, so
re-running is cheap and idempotent.

## Step 2b — give up with `rwv abort`

If you want to discard the in-flight operation and return both workspaces to
their pre-op state:

```bash
rwv abort
```

`rwv abort` reads the `.rwv-op` file to identify the operation and the
involved workspaces. No flags are required or accepted.

Before restoring any repo, `rwv abort` writes a durable pre-abort reference at
`refs/rwv/pre-abort/<op-id>` in that repo (first-write-wins across abort
re-runs — a prior capture is preserved). This means **abort is itself
undoable**: even after `rwv abort` completes, the pre-abort ref is there and
you can recover the tip manually.

For each repo, abort then performs a HEAD-verified restore:

| Current tip | Outcome |
|---|---|
| Equal to the savepoint | `untouched` — op never moved this repo |
| Equal to the recorded converged tip | `restored` — reset to savepoint |
| Repo mid-rebase / mid-merge / mid-cherry-pick | `restored` — native abort cancelled; reset to savepoint |
| Anything else | `foreign-tip violation` — restore refused; op-state retained |

For `rwv sync-to`, **abort rolls back both CWD and the recorded target
workspace** — run it from either side of the pair; it reads the lease to find
the other workspace.

Savepoints live at `refs/rwv/pre-op/<op-id>`. After a clean abort, both
workspaces are at their exact pre-op tips and the savepoint refs are gone —
abort removes them along with the op-state. Commits it discarded survive only
as unreferenced objects until git's collection runs, and retrieving them is
ordinary per-repo git work: no rwv verb undoes an abort.

### If `rwv abort` reports a foreign-tip violation

A foreign-tip violation means a repo's HEAD does not match any state the op
recorded (savepoint, converged tip, or a VCS-native mid-op marker). This
happens when another agent or manual git operation advanced the branch after
the crash. Abort reports the violation, preserves op-state, and exits non-zero.

The question the refusal is asking you is whether those foreign commits are
wanted. Only you can answer it, which is why abort will not guess.

**If they are not wanted**, re-run naming the repo:

```bash
rwv abort
# ... foreign-tip violation for github/foo/bar ...
rwv abort --abandon-foreign-tip=github/foo/bar   # op-state was retained
```

The branch returns to the savepoint and the abandoned commits stay reachable
at the pre-abort ref the first run wrote (`refs/rwv/pre-abort/<op-id>`, named
in the refusal). Repeat the flag per repo — it consents for the repo it names
and no other, and there is no all-repos form. Spell the repo as abort's own
output does: the workspace-relative path, or `(project)` for the project repo.

**If they are wanted**, move the branch back to them yourself and re-run
`rwv abort` with no flag, so the tip abort sees is one it can attribute to
the op.

Abort refuses even with `--abandon-foreign-tip` when the pre-abort ref does
not already hold the tip being left behind — which happens when the branch
advanced again between two abort runs, since the ref is written first-write-
wins and still holds the earlier run's capture. Abandoning then would leave
the commits since that capture reachable from nothing, and the flag consents
to abandoning commits, not to destroying them. The refusal says so; re-point
or copy off the newer commits yourself first.

### If `rwv abort` reports "no operation in progress"

When no `.rwv-op` file is present, `rwv abort` prints `Error: no operation in progress` to stderr and exits with status 0 (the `Error:` prefix is shared with real failures; the zero exit distinguishes the "nothing to do" case). Reasons this can happen:

- No sync or sync-to was started.
- The operation already completed successfully and cleaned up op-state.
- Op-state was removed manually.
- A prior `rwv abort` ran cleanly and cleared it.

There is nothing to abort — the exit code is the load-bearing signal.

## What abort does not restore

- **Uncommitted working-tree changes** that existed before the sync started.
  The savepoint is a git commit pointer — staged or unstaged edits are not
  snapshotted.
- **Side effects outside git history** — generated files, `node_modules/`,
  `.venv/`, and so on are not rolled back.

## Related

- [Recover from a sync conflict](./recover-from-sync-conflict.md) — conflict
  resolution steps and the `--continue` resume flow
- [Bring workweave work home](./bring-workweave-work-home.md) — the normal
  `sync-to` landing workflow and what to do when it hits a conflict
- `rwv explain abort` (rendered at `docs/reference/explain/abort.md`) — full
  abort semantics, the two-rail verified-restore contract, and foreign-tip
  violation recovery
- `rwv explain sync` (rendered at `docs/reference/explain/sync.md`) —
  `--continue` flag, phase machine, and op-state overrides
- `rwv explain sync-to` (rendered at `docs/reference/explain/sync-to.md`) —
  multi-workspace op-state, `--continue`, and step-3 FF-advance failure
