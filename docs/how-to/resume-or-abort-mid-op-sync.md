# Resume or abort a mid-op sync

When `rwv sync` or `rwv land` is interrupted by a conflict, crash, or `Ctrl-C`, inspect the in-flight operation with `rwv status`, then either finish it with `--continue` or roll back to the pre-op savepoints with `rwv abort`:

```bash
rwv status                # the operation, and each repository it stopped in
rwv sync --continue       # or: rwv land --continue
rwv abort                 # roll back all involved workspaces to pre-op savepoints
```

## Inspect what is in flight

`rwv status` is read-only and marks any repository that is mid-rebase, mid-merge, mid-cherry-pick, mid-revert or mid-bisect (in `--json` output, `"mid_op": "<op>"`).

While an operation is recorded, `rwv status` opens with a header naming it: the verb, its id, the phase it stopped in, its age and the workspace that holds it, then the two commands that end it (`rwv sync --continue` or `rwv land --continue`, and `rwv abort`). Each repository the operation stopped in carries its mark (`[mid-rebase]`, for example) and is reported as `edits` `mid-operation`, which prints no choice of its own: the header's commands are the way on. A pin or alignment state the stop causes can still be reported by `rwv status` while the operation is parked (a member checkout one commit beyond its pin as `pin` `unpinned`, a hand-finished rebase as `pin` `diverged`), without choices; the stop's own commands are the way on. `rwv doctor` reports the recorded operation as `stale-op-state`, naming the same two commands, and never repairs it: another terminal may be resolving the conflict.

For `rwv land`, the initiating workweave holds the owner record and the target workspace holds a lease pointing back to it. Run `--continue` from the initiating workspace. `rwv abort` can be run from either workspace and rolls back both with the same verdict per repository; its lines for the other workspace's repositories carry a `[target]` prefix (run from the initiating workweave) or a `[source]` prefix (run from the target).

## Resume with `--continue`

If the operation paused on a conflict, mark the resolved files resolved in the conflicted repository (for git, `git add <files>`) and run `rwv sync --continue` or `rwv land --continue` from the weave root. See [Recover from a sync conflict](./recover-from-sync-conflict.md) for the conflict resolution walkthrough.

If the operation stopped as `unparseable-project` `mid-operation`, the project repo's `rwv.toml` that its merge or rebase produced does not parse. Edit `rwv.toml` in the project repo until it parses, stage it with the `git -C <checkout> add -- rwv.toml` line the stop prints, and run the printed `--continue`. A merge commits the staged file in the merge; a rebase, which has already finished, makes it part of the replayed tip, a commit the rebase wrote and nothing has published, in the same rewrite that re-pins the replayed commits. The staged file goes only into that commit, on the branch the rebase wrote: with another branch checked out, HEAD detached, or a commit of yours on top of the replayed tip, `--continue` stops the same way and says which; so does an uncommitted edit to `rwv.lock`, which that rewrite replaces. Anything else you staged stays staged, out of the commit. An edit that is not staged leaves the stop as it was, and one that leaves the working `rwv.toml` unparseable over a staged fix that parses stops the same way, saying so, before anything is committed. `rwv abort` keeps an uncommitted fix, staged or not, in the checkout's backup slot, then cancels the merge or rebase.

If another workspace moved a repository's branch while the operation was stopped, `--continue` reads that move before it takes the repository in, as the start of an operation does: it refreshes the files the move changed and says `refreshed drift`, or, where it cannot read the move (no record of where the checkout stood, or a branch moved to a commit that does not contain it), refuses as `edits` `drift left in place` with the choice that sets the difference aside, after which `--continue` runs again. A repository stopped in a merge or copy whose branch moved cannot be refreshed, and concluding the merge would record the move's reverse, so `--continue` refuses it the same way, whatever `--keep` it was given, with `rwv abort` as its one choice; abort keeps the moved tip in the branch's slot.

`--continue` loads all parameters (source or target, the landing's plan, `--history`, `--retire`) from the saved op record. Do not re-supply the original flags. Already-converged repositories are skipped, so `--continue` is idempotent.

## Roll back with `rwv abort`

`rwv abort` restores every ref the operation moved to the savepoint it took before moving anything: each checkout, onto the branch it was on, and each branch a landing moved without a checkout. It cancels a rebase, merge or cherry-pick left in progress, and clears the op record:

| Current repository tip | `rwv abort` outcome |
|---|---|
| Equal to the savepoint | `untouched`: the operation never moved this repository |
| Any other tip | `restored`: reset to the savepoint; where the tip holds a commit that neither the savepoint nor another branch reaches, it is kept first in the branch's backup slot, whoever made it |
| On another branch than when the operation began | `switched back` onto that branch, the branch it was on left where it stands, then judged by this table |
| Stopped in a rebase, merge or cherry-pick | the working copy, your conflict resolutions included, kept in the checkout's backup slot; for a rebase, the tip of the branch it rebases, where something other than the rebase moved it during the stop, kept in the branch's slot; then the VCS operation canceled and the repository judged by this table |
| Stopped in an operation begun where the operation found it, in a repository the operation never merges or rebases, or in a `land`'s target a repository the landing never moved | `left` where it stands, the operation in progress there included |

A branch the landing moved without a checkout is put back once every checkout is restored, from the tip it holds, while nothing has it checked out; one the landing created is deleted.

`rwv abort` never discards a commit or an edit without keeping it first, from whichever workspace you run it in, and it does not ask whose it is: a commit you made at a stop, whether or not `--continue` ran after it, a conflict resolution you staged or `--continue` committed, and the operation's own replayed commits are kept alike. It keeps them in slots: one per branch, `<branch>/aborted`, for the tip it reset the branch from, and one per checkout, `<branch>/aborted-working-copy`, for the working copy it discarded there. Each repository it keeps something for is one line, naming the slots and the command that shows what they hold:

```
  local/remotes/app: what abort discarded is kept as backup feat/aborted-working-copy, feat/aborted (canceled the rebase in progress and restored); this shows it: git -C <checkout> log -p --diff-merges=first-parent refs/rwv/backup/feat/aborted-working-copy refs/rwv/backup/feat/aborted --not <savepoint> --branches --remotes
```

The printed command shows what the slots hold that neither the savepoint nor any branch or remote-tracking branch reaches.

A slot is one deep: the next abort that keeps something in it overwrites it. What it held before stays in the VCS's own history of that slot until that expires, as a branch's earlier tips do after any single-repository reset. For git, this lists each value a slot has held, newest first, with `<slot>` spelled as the printed command spells it (`refs/rwv/backup/<branch>/aborted` or `refs/rwv/backup/<branch>/aborted-working-copy`):

```bash
git -C <checkout> log -g -p <slot>
```

`rwv backup list` shows the slots, and `rwv backup drop <repo> <name>` drops one with its history. `rwv backup prune` takes a slot only once a branch reaches what it holds; until then the slot stays until the next abort overwrites it or you drop it, the slots of a retired workweave's branch included. A tip another branch or remote-tracking branch reaches is reset without a slot, since nothing would be lost.

Abort does not tell your work from the operation's, so an abort nobody disturbed still keeps the operation's own replayed commits and conflict markers: each repository the operation moved onto commits nothing else reaches, or left stopped, gets a slot and one line, worded the same whichever it holds. A commit you made at a stop that `--continue` then rebased is also in the backup the operation made before rebasing, which abort names on a line of its own with the command that shows it. A commit that two branches reset by one abort both hold is kept once, in the slot of the branch reset last, and named on that branch's line, which can be another repository's or, under a `land`, the other workspace's.

A refusal moves and writes nothing: abort plans every step on both sides before it makes any, so one obstacle anywhere (a branch that is gone or checked out elsewhere, a branch of a landing's target, project or member, that something else moved, another lock pinning what a landing wrote in its target, a manifest it cannot read) refuses the whole abort as `foreign-tip`, says `nothing is restored` (for a landing, `nothing of either side is restored`) and leaves `--continue` open.

### Ending an operation whose record cannot be read

If every verb refuses with `unreadable-op-record`, the operation record cannot be read: its op id does not parse, or another release wrote it. If another release wrote it, that release resumes or aborts the operation. Otherwise run the command the refusal prints:

```bash
rwv -C <workspace> abort --discard-unreadable-record
```

It removes the record, the leases the operation holds on other workspaces and its landing claims, and moves nothing back: only the record says which tips are the operation's. It lists each repository left stopped in a rebase or merge, and each savepoint and backup the repositories hold. `rwv doctor --fix` then keeps each savepoint tip no branch reaches as a backup and drops the savepoint.

### Handling a `foreign-tip` refusal

The refusal ends with the commands that end the operation, so run a printed command as printed:

- `rwv abort` again, once the fixes it printed are made;
- `rwv land --continue`, where a landing stopped in its retire and can finish;
- `rwv abort --leave-target`, for a landing whose target something else moved, or holds a commit the landing wrote that another lock now pins: the source is restored, the target is left as it stands, and each tip the landing wrote there is kept as a backup first where no branch reaches it.

Once `rwv abort` has begun, the operation can only be aborted: `--continue` refuses it. A write that finds a repository moved since abort judged it stops there, saying `abort stopped there`; what was restored stays, nothing after it is done, and `rwv abort` run again completes the rest.

A branch the operation moved with no checkout holding it that a checkout now holds names that checkout to free, then rerun `rwv abort`.

### When no operation is in progress

If no op record is present in the workspace, `rwv abort` exits with status `1` and prints `Error: no operation in progress`.

## Related

- [Recover from a sync conflict](./recover-from-sync-conflict.md) — resolving conflicts before running `--continue`
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave work with `rwv land`
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, savepoints, and op record

For the full contracts, run `rwv explain abort` (verified restore and foreign-tip recovery), `rwv explain sync`, and `rwv explain land` (op record and `--continue`).
