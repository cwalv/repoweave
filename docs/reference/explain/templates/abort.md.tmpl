# rwv abort

## Purpose

Roll an in-flight `rwv sync` or `rwv land` back to the savepoint refs it
wrote at the start of the operation, in every workspace it touched. Call `abort` when a
sync or land has left the workspace in an unresolvable state and you want to
get back to the last known-good position rather than continue forward.

`abort` reads op-state to find the in-flight operation. The **owner record**
(`.rwv-op`) lives at the initiating workspace (CWD for `rwv sync`; CWD for
`rwv land`). Every other workspace the op mutates holds a **thin lease**
(`.rwv-op-lease`) pointing back at the owner. `abort` can be invoked from
any involved workspace — from a leased workspace it follows the pointer to
the owner record automatically. For `land` ops, both the initiating
workspace and the recorded target are rolled back, whichever of the two
`abort` is invoked from, and each repo gets the same verdict from either.

### What is restored

For each member repo and the project repo in every involved workspace, abort
applies a verified restore. The destructive reset to the savepoint happens only
when the repo's current tip is attributable to the op. The classification:

| Current tip | Outcome |
|---|---|
| Equal to the savepoint | `untouched` — op never moved this repo; HEAD not touched |
| Equal to the recorded intent tip (`advanced_tips[repo]`) | `restored (from recorded intent tip)` — op advanced this repo during replay before crashing; reset to savepoint |
| Equal to the recorded converged tip (`converged_tips[repo]`) | `restored (from recorded converged tip)` — op converged repo before crash; reset to savepoint |
| Stopped in a rebase, merge or cherry-pick the op started there: a repo the op integrates, where canceling returns to the savepoint or a tip the op wrote | `restored (from mid-op state)` — canceled, then restored as the tip it returns to is |
| Stopped in an operation begun at the savepoint, in a repo the op never integrates (or with no savepoint) | `left stopped in a <operation>, which this operation never starts there` — left as it is |
| Stopped in any other operation: begun on a tip the op never wrote, or in a repo the op never integrates on a tip it moved (every operation in a `land`'s target) | `stopped in a <operation> begun at <tip>` — refused, nothing touched |
| Anything else, in the target workspace of a `land`, in a repo whose branch the landing never fast-forwarded | `left at <tip>, where something other than this landing moved it` — not the op's, so not restored and not a violation |
| Anything else | `foreign-tip violation` — restore refused, the tip left where it is, nothing written |
| Anything else, in a repo named on `--abandon-foreign-tip` | `restored (abandoned foreign tip)` — an operation in progress is canceled and the tip kept as a backup marked `abandoned`, then the repo is reset to the savepoint |

Every commit abort resets off is one the op made, so the commits a clean
restore discards are the op's own output. Uncommitted edits are another
matter: before any reset or cancel that would discard edits (a tracked
change, or an untracked file at a path the savepoint tracks), abort keeps
them as a backup and prints its name. Canceling the op's own stopped
operation is the one step it takes as the VCS's own abort would, edits
included.

Where the op record parses no further than its header, abort reads no tip
it recorded: a repo is restored only where it sits at its savepoint, and one
stopped in an operation begun there is canceled once its edits are kept.

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

- **Uncommitted working-tree changes.** The savepoint records commits, not
  edits. Edits a reset would discard are kept as a backup (`rwv backup
  list`), not restored into the working tree.
- **Side effects outside the git history** — integration-generated files,
  `node_modules/`, `.venv/`, etc. are not snapshotted and are not rolled back.

### When `abort` refuses

`abort` returns an error with `no operation in progress` when no `.rwv-op`
owner record or `.rwv-op-lease` thin lease is found in CWD's workspace.

This can happen if:
- No sync or land was ever started.
- The operation already completed successfully and the op-state was cleaned up.
- The op-state was removed manually.
- A prior `rwv abort` completed cleanly and cleared op-state.

`abort` also refuses per-repo (foreign-tip violation) when a repo's current tip
cannot be attributed to the op. In that case op-state is retained so the
operator can re-run after reconciling.

### Abandoning a foreign tip on purpose

`--abandon-foreign-tip <repo>` is the operator's answer to "these foreign
commits are not wanted". It waives the attribution rule for the repo it names —
abort resets that repo to the savepoint even though the tip is unattributable —
and waives nothing anywhere else. Repeat it per repo. **There is no all-repos
form**, by design: whether abandoning a repo's foreign commits is acceptable is
a judgment about those specific commits, and a blanket spelling would answer it
for repos the operator never looked at. `abort` with no flag still refuses.

The flag is named for the consequence it permits, and the name is accurate
because the tip is kept first: abort writes the foreign tip as a **backup**
marked `abandoned`, named for the branch it moved off, and resets only once that
backup exists. The commits remain reachable through it after the branch moves
off them. Abort prints the backup's name. `rwv backup list` shows it with the
commits only it holds, and `rwv backup drop <repo> <name>` drops it when they
are not wanted; getting them back is putting a branch on the backup's commit.

Spell `<repo>` as abort's own per-repo output does — the workspace-relative
path (`github/foo/bar`), or `projects/<name>` for the project repo. A `land`
op consults the consent once per side, so naming a repo covers that repo in both
the source and the target workspace; each side's backup is named for that
side's own branch. Naming a repo that is not refused does nothing. Naming
another project's repo refuses, naming the one the operation belongs to.

The consent applies to the tip abort finds when it runs. If a foreign agent
committed on top of a tip an earlier abort refused, or reset the branch and
built something else, the rerun keeps the tip as it now stands.

### A savepoint left after its operation ended

A savepoint an earlier release kept after its operation completed (a
**tombstone**) is no recovery point `rwv abort` reads: the operation's record
is gone, so `rwv abort` refuses. `rwv doctor --fix` turns it into a backup (a
live `orphaned-savepoint`), which `rwv backup list` shows and `rwv backup drop`
drops; nothing reads a tombstone as a backup before then.

## Invocation

```
rwv abort
rwv abort --abandon-foreign-tip <repo> [--abandon-foreign-tip <repo> ...]
```

`abort` reads everything else it needs from `.rwv-op`. The one flag is the
per-repo waiver described above; without it `abort` never moves a branch off
commits it cannot attribute to the op.

Run `rwv abort --help` for the full clap surface.

## Output

Per-repo restoration lines to stdout. Each line names the outcome:

```
  <repo-path>: restored (from recorded intent tip)
  <repo-path>: restored (from recorded converged tip)
  <repo-path>: restored (from mid-op state)
  <repo-path>: restored (abandoned foreign tip, per --abandon-foreign-tip)
  <repo-path>: its uncommitted edits are kept as backup <name> (`rwv backup list`)
  <repo-path>: left stopped in a merge, which this operation never starts there
```

The abandoned-tip line is followed by the tip that was left behind and the
backup it was kept as, so the abandonment is recoverable from the transcript
alone.

For a `land`, lines for the workspace `abort` was invoked from carry no
prefix, and lines for the op's other workspace carry its side: `[target]`
when `abort` runs in the initiating workspace, `[source]` when it runs in
the target.

Non-actionable outcomes (`untouched` and `no savepoint`) are demoted to a
single aggregate summary line printed at the end:

```
  summary: N repo(s) skipped (no savepoint), N untouched (tip == savepoint)
```

Foreign-tip violations are printed to stderr with the observed tip (left in
place), the expected savepoint, the recorded converged tip (if any), and a list
of blocking commits between the savepoint and the observed tip (capped at 5
with a count of any remainder). Each foreign-tip block also notes whether the
tip is strictly ahead of or diverged from the savepoint.

The recovery-options block is printed exactly once at the end (to stderr)
when at least one repo refused. It lists each refused repo with the choices
that clear it, each command on a line of its own:

- a foreign tip: `rwv abort --abandon-foreign-tip <repo>`; the commits are
  kept as an `abandoned` backup.
- an operation in progress abort may not cancel: finish or abandon it there
  (the VCS's status command names how), then `rwv abort`; or `rwv abort
  --abandon-foreign-tip <repo>`, which keeps its edits and commits as
  backups before canceling it.

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
rwv sync
# ... rebase conflict that cannot be resolved ...
rwv abort
```

Roll back a land (rolls back both CWD and the target workspace):

```
rwv land
# ... conflict mid-op ...
rwv abort
```

Recover from a foreign-tip violation (another agent advanced the branch)
when the foreign commits are not wanted:

```
rwv abort
# ... foreign-tip violation for github/foo/bar ...
rwv abort --abandon-foreign-tip github/foo/bar   # op-state was retained
# ... github/foo/bar: restored (abandoned foreign tip) ...
# the abandoned commits are the backup abort printed:
rwv backup list
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
  recovery options. The refusal moves and writes nothing. Op-state is retained
  so you can re-run `rwv abort` after reconciling, or with
  `--abandon-foreign-tip <repo>` if the foreign commits are unwanted.
- *`--abandon-foreign-tip` names the repo of project `<other>`* — the flag took
  `projects/<other>`, but the operation belongs to another project. Name its
  project repo as the message says.
- *verified restore failed* — abort could not complete one repo's restore; the
  cause follows. When it is the backup under `--abandon-foreign-tip` that could
  not be written, abort did not reset: information-preservation is the first
  obligation, and the repo is left where it was. The other repos are still
  attempted, op-state is retained, and `rwv abort` can be retried.
