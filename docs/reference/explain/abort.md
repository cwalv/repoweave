# rwv abort

## Purpose

Roll the operation in progress back to the savepoint refs it wrote at its
start, in every workspace it touched: `rwv sync`, `rwv land`, `rwv pull` or
`rwv update --remote` with `--merge-upstream` or `--reset-to-upstream`, and
`rwv cherry-pick`, each of which leaves an operation record when it stops.
Call `abort` when one has left the workspace in a state you would rather
undo than continue, and you want to
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
puts the checkout back at its savepoint, and keeps first whatever that would
otherwise lose. The outcome by what the checkout holds:

| Current state | Outcome |
|---|---|
| At the savepoint | `untouched`; HEAD not touched |
| At a tip the op recorded writing (`advanced_tips` or `converged_tips`) | `restored (from recorded intent tip)` or `restored (from recorded converged tip)`; reset to the savepoint |
| At any other tip the savepoint reaches | reset to the savepoint; nothing is lost |
| At any other tip, holding a commit the savepoint does not reach (a commit you made at a stop) | `its tip ... is kept as backup <name>`, then `restored`; the line after it is the command that shows the backup's commits |
| Stopped in a rebase, merge or copy | canceled as the VCS's own abort would, its working copy kept as a backup first (`its uncommitted edits are kept as backup <name>`), from whichever workspace abort runs in; where the op did not start it, the commits its HEAD holds are kept too; then the tip it returns to is judged as above |
| Stopped in an operation begun at the savepoint, in a repo the op never integrates (or with no savepoint) | `left stopped in a <operation>, which this operation never starts there`; left as it is |
| In the target workspace of a `land`, in a repo whose branch the landing never fast-forwarded | `left at <tip>, where something other than this landing moved it`; not the op's, so not restored |

Abort never discards a commit or an edit without keeping it first, and
prints each backup on one line with its name, followed by the command that
shows what it holds. Before any reset that would discard uncommitted edits
in the workspace abort runs in, it keeps them as a backup too.

Where the op record parses no further than its header, abort reads no tip
it recorded: every tip it leaves that holds commits the savepoint does not
reach is kept as a backup.

The op-state file (`.rwv-op`) is removed from all involved workspaces once
every repo is restored.

### What is **not** restored

- **Uncommitted working-tree changes.** The savepoint records commits, not
  edits. Edits a reset or cancel would discard are kept as a backup, not
  restored into the working tree.
- **Side effects outside the git history**: integration-generated files,
  `node_modules/`, `.venv/`, etc. are not snapshotted and are not rolled back.

### Abort is planned whole, then executed

Before it writes anything, abort plans every step on both sides: each checkout's restore, each branch moved as a ref, each branch moved in place in a workspace since gone or in a checkout since moved off it onto a tip the landing did not write (put back as a ref, the checkout left as it stands), each created branch it would delete, each ref an update moved. What it cannot read blocks the plan, naming what could not be read. A cancel the VCS would refuse (a file the stopped operation staged and the working tree changed since) blocks it too. Any blocked step, or a landing's target judged moved as a unit, refuses the whole abort with one report: nothing of either side is restored, the op record is kept and `--continue` stays open.

Once the plan is clear abort takes the claims it needs, plans again under them, and only then begins. From that point `--continue` is closed. The target's project branch is written first. Each write checks, just before it writes, the state the plan judged; a repo that moved in between makes that one write refuse, and abort stops there: what was restored stays, nothing after it is done, and `rwv abort` run again completes the rest.

### When `abort` refuses

`abort` returns an error with `no operation in progress` when no `.rwv-op`
owner record or `.rwv-op-lease` thin lease is found in CWD's workspace.

This can happen if:
- No sync or land was ever started.
- The operation already completed successfully and the op-state was cleaned up.
- The op-state was removed manually.
- A prior `rwv abort` completed cleanly and cleared op-state.

`abort` also refuses, as `foreign-tip`, when its plan is blocked: a branch is
gone or checked out elsewhere, a landing's target moved since,
uncommitted edits would be discarded in a workspace abort was not run in, or
something could not be read. The refusal says nothing is restored (for a landing,
"nothing of either side is restored"), keeps op-state and leaves `--continue`
open, and prints the choices below. After abort has begun, the refusal says
"nothing more is restored" and `--continue` is closed.

### A landing's target is restored whole or not at all

A landing's target is one unit: its project commit's lock pins its members'
tips. Before abort moves anything of a landing, it reads every branch the
landing moved in the target, in the target's checkouts, in place in another
workspace, or as refs. Where one stands somewhere other than where the landing
left it (another landing built on it, a commit, a deletion), or is checked out
where abort may not move it, abort refuses as `foreign-tip` before either side
moves, naming each and the commits on it since, and prints the choices. A
member branch of the target that only moved on since, the project branch
where the landing left it, is put back with the rest, its tip kept as a
backup first. Otherwise:

- free the checkout, or put the deleted branch back where the landing left it,
  then rerun `rwv abort`, which restores the whole target;
- `rwv land --continue`, from the landing's workspace, where the landing
  stopped in its retire: the landing stands and the retire finishes;
- `rwv abort --leave-target`: the source is restored and the landing ends,
  its whole target left as it stands. Each tip the landing wrote there is
  kept as a backup first where no branch reaches it;
  the target's savepoints are dropped and the landing's claim and leases end.
  It is refused, before anything moves, where abort can restore the target,
  and for an operation that is not a landing.

A lock other than the two abort puts back (the target's project branch and
the source's) can pin a commit the landing wrote in the target: another
project that lists the same member, or another branch of this one, locked it
after the landing moved the member's branch. Where restoring the target, with
the source put back, would leave such a pin on a commit no branch or backup
reaches, abort refuses before anything moves, naming each pin and the lock
that holds it (`restoring '<branch>' leaves <commit>, which <project>
<branch> pins, reached by no branch or backup`). `rwv land --continue` where the retire stopped, or
`rwv abort --leave-target`, ends the landing with the target and the pin as
they stand.

### A record this rwv cannot read

A `.rwv-op` whose op id does not parse, or that another release wrote in a
record schema this one does not read, is refused by every verb that reads it
as `unreadable-op-record`, plain `abort` included; `rwv status` reports it.
`rwv abort --discard-unreadable-record`, run in the workspace holding the
record or in one the operation leases, ends the operation: it removes the
record, the leases it holds on other workspaces and its landing claims, and
moves nothing back, since only the record says which tips are the
operation's. It lists each repo left stopped in a VCS operation and each
savepoint and backup the repos hold. Once no record claims a savepoint,
`rwv doctor --fix` keeps each tip it holds that no branch reaches as a backup
and drops it. Where the record reads, the flag is refused.

### A savepoint left after its operation ended

A savepoint an earlier release kept after its operation completed (a
**tombstone**) is no recovery point `rwv abort` reads: the operation's record
is gone, so `rwv abort` refuses. `rwv doctor --fix` turns it into a backup (a
live `orphaned-savepoint`), which `rwv backup list` shows and `rwv backup drop`
drops; nothing reads a tombstone as a backup before then.

## Invocation

```
rwv abort
rwv abort --leave-target
rwv abort --discard-unreadable-record
```

`abort` reads everything else it needs from `.rwv-op`. `--leave-target` ends a landing whose
target something else moved: the source is restored and the target is left as it
stands, each tip the landing wrote there kept as a backup first where no branch
reaches it. It is refused, before anything moves, where abort can restore the
target, for an operation that is not a landing, and where the source's own
restore is blocked. The target is given up only after the source was restored.

Run `rwv abort --help` for the full clap surface.

## Output

Per-repo restoration lines to stdout. Each line names the outcome:

```
  <repo-path>: restored (from recorded intent tip)
  <repo-path>: restored (from recorded converged tip)
  <repo-path>: restored (from mid-op state)
  <repo-path>: its uncommitted edits are kept as backup <name>; this shows it:
  	<the VCS command that shows the backup>
  <repo-path>: its tip, holding commits this operation did not make, is kept as backup <name>; this shows it:
  	<the VCS command that shows the backup>
  <repo-path>: restored
  <repo-path>: left stopped in a merge, which this operation never starts there
```

Each backup line is followed by the command that shows what the backup holds,
so what abort moved off is recoverable from the transcript alone.

For a `land`, lines for the workspace `abort` was invoked from carry no
prefix, and lines for the op's other workspace carry its side: `[target]`
when `abort` runs in the initiating workspace, `[source]` when it runs in
the target.

Non-actionable outcomes (`untouched` and `no savepoint`) are demoted to a
single aggregate summary line printed at the end:

```
  summary: N repo(s) skipped (no savepoint), N untouched (tip == savepoint)
```

The recovery-options block is printed exactly once at the end (to stderr)
when the plan was blocked. It lists each refused repo with the choices
that clear it, each command on a line of its own, then the command that ends the
operation for each way it can end (rerun `rwv abort` once the fixes are made;
`rwv land --continue` where a landing's retire stopped; `rwv abort
--leave-target` where the target is not restored). The choices:

- a checkout off the branch its savepoint names, where that branch is gone
  or checked out elsewhere: the VCS command that puts the branch back at the
  savepoint, or freeing the other checkout, then `rwv abort`.
- uncommitted edits in a workspace abort was not run in, or an operation
  stopped there that the VCS would refuse to cancel as it stands: set them
  aside there with the printed command, then `rwv abort`; or run
  `rwv -C <workspace> abort`, printed where that workspace takes the abort up,
  which keeps them as a backup first.
- a plan that could not be read (a broken manifest, a branch savepoint): make it
  readable and rerun; for a landing, `rwv abort --leave-target` ends it without
  reading the target.

A refused abort writes nothing. An abort that began and found a write no longer
doing what the plan named stops at that write and exits non-zero.

Once `abort` begins it marks the operation's record, so an abort that refuses
or is interrupted leaves an operation only `rwv abort` takes up: `--continue`
refuses it (`resume-contradicts-record`).

## Exit codes

- `0` — all repos restored successfully (or had no savepoint, i.e., nothing
  to restore); op-state cleared.
- non-zero — no operation in progress, a blocked plan (`foreign-tip`, op-state
  retained), or one or more repos failed to restore.

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

Abort after a commit made at a stop: the commit is kept, and the printed
command shows it:

```
rwv abort
# ... github/foo/bar: its tip, holding commits this operation did not make, is kept as backup main/<id>; this shows it:
# ...     git -C github/foo/bar log --stat <savepoint>..refs/rwv/backup/main/<id>
```

## Common errors

- *no operation in progress* — no `.rwv-op` file found. Either no sync is
  in flight, or the op already completed and was cleaned up. Nothing to abort.
- *`foreign-tip`* — the plan is blocked: a branch is gone or checked out
  elsewhere, a landing's target moved since, or edits stand in the other
  workspace. The refusal moves and writes nothing, names what clears each, and
  op-state is retained so `rwv abort` reruns.
- *abort completed with failures* — a write the plan named could not be made
  (the repo moved after abort judged it, or a backup could not be written, in
  which case abort did not reset: information-preservation is the first
  obligation). Abort stopped at that
  step: what it restored stays, nothing after it was done, op-state is
  retained, and `rwv abort` can be retried.
