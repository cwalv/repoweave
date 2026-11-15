# Stops and abort

Some verbs change many refs at once and can be interrupted in the middle: by a
conflict, by an error in one repo, by a crash, by `Ctrl-C`. rwv makes one promise
about them. At any point the operation is **stopped**, never half-done in a way that
cannot be named: it can be finished (`--continue`) or put back (`rwv abort`), and
neither loses a commit or an edit without keeping it.

## Operations

`rwv sync`, `rwv land`, `rwv cherry-pick`, and `rwv pull` and `rwv update --remote` with
`--merge-upstream` or `--reset-to-upstream` run as **operations**. An operation writes
an **op record** at the workspace it started in, before it moves anything. The record
holds the plan (the verb's parameters, the tips it will write, the consents it was
given: `--allow-member-change`, `--reset-to-target`, `--history`, and for
`cherry-pick` its `<rev>`, `-m` and `--except`) and how far it got. A workspace the
operation also changes holds a lease pointing back at the record, which keeps a second
operation out. During an operation only `--continue`, `rwv abort` and the verbs that read
(`rwv status`, `rwv backup list`, `rwv doctor`) run; every verb that writes,
the setters included, refuses until the operation ends.

Every refusal a verb can judge up front is reported, all together, before the record is
written and before anything changes; a refusal there leaves no trace. Every other verb
that acts on several repos or refs (`rwv branch create`, `rename`, `delete`,
`set-mode`, `set-history`, the `member` verbs, `rwv lock --commit`, `rwv backup drop`,
`rwv workweave create` and `delete`, `rwv activate --update`) orders its writes so that
every crash point is a state the same verb, run again, completes, accepting its own
partial result. Those verbs need no record; they converge.

## Savepoints

Before an operation moves a ref it takes a **savepoint** of it: one per ref the
operation may move, each checkout's HEAD (naming the branch it was on), each branch it
moves as a ref, each dependent it carries. A landing's target's savepoints are filed
apart from its source's, since the checkouts of one repo in both workspaces share its
refs. Savepoints last one operation and exist for `rwv abort`. Each tip the operation
writes is recorded, so abort can tell, by compare-and-swap, whether a ref is still where
the operation left it.

## Kinds of stop

| Stop | What stopped it | Exits |
|---|---|---|
| conflict | a merge, rebase or copy in one checkout conflicted; the stop names the checkout | resolve it, mark it resolved, `--continue`; or `rwv abort` |
| `rwv.toml` conflict | the project repo's `rwv.toml` is unmerged; each entry in conflict is listed with both values | `--continue --keep yours` or `--keep incoming`, which takes that side for every entry in conflict; or edit `rwv.toml` into what you mean, mark it resolved, then `--continue`; or `rwv abort` |
| check stop | the VCS merged `rwv.toml` as text without a conflict, and the result differs from the merge of both sides' values, or does not parse (`unparseable-project` `mid-operation`) | mark an `rwv.toml` that parses resolved, then `--continue`; `--continue --keep text-merge` marks the VCS's text merge resolved as it stands; or `rwv abort` |
| lost race | a landing's target or receiving branch moved since the landing read it (`target` `moved mid-operation`) | `rwv abort` only; `--continue` refuses it |
| crash | the process died anywhere | `--continue` or `rwv abort`; a `--continue` that finds its work done ends the operation |

At a check stop rwv writes nothing to `rwv.toml`: it names what differs and leaves the
file as the VCS made it. `--keep` is accepted only where there is something to choose:
`yours` or `incoming` where regions of `rwv.toml` are in conflict, taking that side for
every entry both sides changed differently, and `--continue --keep text-merge` at the
check stop. At any other stop a `--keep` is refused. To mix the two
sides, keep one, then run the setter for each entry you want otherwise once the
operation has ended (`rwv member set-branch`, `set-url`, `set-role`,
`set-recurse-submodules`, `rwv branch set-history`); an edit of the file at the stop,
marked resolved, does the same in one step.

## Continuing

`--continue` reads every parameter from the op record, so a flag given at the start is
not given again; `--allow-member-change` is the one that adds to the names the landing
started with. Before it does anything else it reads the world again: a ref another
workspace moved while the operation was stopped, the drift in checkouts whose branch
moved under them (it refreshes drift and says so), the setters and consents the plan
depends on. It rechecks every attachment it planned against, and refuses where the plan
no longer describes the state (a member switched to another branch, a lost race). A
commit the operator made by hand during the stop is taken in, and the remaining work
runs on top of it. Repos the earlier run finished are skipped, so `--continue` is
idempotent.

## Abort

`rwv abort` puts every ref the operation moved back at its savepoint, in every
workspace the operation touched, from either one; the verdict for each repo is the
same. A checkout goes back onto the branch it was on before anything is reset, a
branch a landing moved without a checkout is restored once no checkout is left on it,
and a branch the operation created is deleted where it still sits where it was created.

**What it keeps.** Abort never discards a commit or an edit without keeping it first,
and it does not ask whose it is. Before it resets a branch, the tip it resets from is
kept, where that tip holds a commit that neither the savepoint nor another branch or
remote-tracking branch reaches, in that branch's one slot (`<branch>/aborted`). The
working copy of a checkout it cancels a stopped rebase, merge or copy in, or discards
edits in, is kept in that checkout's one slot (`<branch>/aborted-working-copy`), from
whichever workspace abort runs in. The next abort that keeps something in a slot
overwrites it, and what it held stays in the VCS's own history of that slot until that
expires. This is the bar a single-repository reset sets: the VCS keeps the last tip a
reset moved off, one deep. An operation's own commits and a commit made at a stop are
kept alike, so nothing has to decide whose a tip is. Abort prints one line per repo it
keeps something for, with the command that shows it; `rwv backup list` shows the slots
and `rwv backup drop` drops one.

**Planned whole.** Abort plans every write on both sides before making any. One blocked
step (a branch gone or checked out elsewhere, a branch the landing moved in its
target that something else has moved since, something unreadable) refuses the whole abort as `foreign-tip`:
nothing of either side is restored and `--continue` stays open. The refusal names each
obstacle with what clears it. After abort has begun `--continue` is closed, and a write
that finds a repo moved since the plan stops there; running `rwv abort` again completes
the rest.

**Giving up a target.** `rwv abort --leave-target` ends a landing whose target something
else has moved since: the source is restored first, the whole target is left as it stands
with each tip the landing wrote there kept as a backup where no branch reaches it, and
the target's savepoints are dropped only after every step on the source side ran.

**A record rwv cannot read.** A record another release wrote, or one that was edited, is
refused by every verb that reads it as `unreadable-op-record`, and reported by
`rwv status` and `rwv doctor`. `rwv abort --discard-unreadable-record`
ends the operation: it removes the record, its leases and claims, and moves nothing back,
since only the record says which tips are the operation's. It lists each repo left stopped
and each savepoint and backup the repos hold.

## What survives an operation

Savepoints last one operation. A **backup** (`refs/rwv/backup/...` for git) outlives it:
rwv makes one before any step that would leave a commit or an uncommitted edit
unpreserved, and only `rwv backup drop` or, when a branch reaches everything it holds,
`rwv backup prune` removes it. A `rebased` backup is the exception that goes with its
operation or its republish. A savepoint an operation left behind after it ended is
converted to a backup by `rwv doctor --fix`.

## Related

- [Resume or abort a mid-op sync](../../how-to/resume-or-abort-mid-op-sync.md) and [Recover from a sync conflict](../../how-to/recover-from-sync-conflict.md): the operator recipes.
- [Destructive operations](../destructive-operations.md): the policy behind named consents and kept backups.
- [Landing](./landing.md) and [Sync semantics](./sync-semantics.md): the operations that stop.
- [Formats: `.rwv-op`](../../reference/formats.md#rwv-op--owner-op-record).
- `op-parked`, `foreign-tip`, `unreadable-op-record` in [Refusals](../../reference/refusals.md).
