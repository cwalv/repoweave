# Resume or abort a mid-op sync

When `rwv sync` or `rwv land` is interrupted by a conflict, crash, or `Ctrl-C`, inspect the in-flight operation with `rwv status`, then either finish it with `--continue` or roll back to the pre-op savepoints with `rwv abort`:

```bash
rwv status                # inspect per-repo mid_op state
rwv sync --continue       # or: rwv land --continue
rwv abort                 # roll back all involved workspaces to pre-op savepoints
```

## Inspect what is in flight

`rwv status` is read-only and marks any repository that is mid-rebase, mid-merge, mid-cherry-pick, mid-revert or mid-bisect (in `--json` output, `"mid_op": "<op>"`).

For `rwv land`, the initiating workweave holds the owner record and the target workspace holds a lease pointing back to it. Run `--continue` from the initiating workspace. `rwv abort` can be run from either workspace and rolls back both with the same verdict per repository; its lines for the other workspace's repositories carry a `[target]` prefix (run from the initiating workweave) or a `[source]` prefix (run from the target).

## Resume with `--continue`

If the operation paused on a conflict, stage the resolved files in the conflicted repository (`git add <files>`) and run `rwv sync --continue` or `rwv land --continue` from the weave root. See [Recover from a sync conflict](./recover-from-sync-conflict.md) for the conflict resolution walkthrough.

If the operation stopped as `unparseable-project` `mid-operation`, the project repo's `rwv.toml` that its merge or rebase produced does not parse. Edit `rwv.toml` in the project repo until it parses, stage it with the `git -C <checkout> add -- rwv.toml` line the stop prints, and run the printed `--continue`. A merge commits the staged file in the merge; a rebase, which has already finished, makes it part of the replayed tip, a commit the rebase wrote and nothing has published, in the same rewrite that re-pins the replayed commits. The staged file goes only into that commit, on the branch the rebase wrote: with another branch checked out, HEAD detached, or a commit of yours on top of the replayed tip, `--continue` stops the same way and says which; so does an uncommitted edit to `rwv.lock`, which that rewrite replaces. An edit that is not staged leaves the stop as it was, and one that leaves the working `rwv.toml` unparseable over a staged fix that parses stops the same way, saying so, before anything is committed. Under merge history `rwv abort` cancels the merge as `git merge --abort` does: a staged fix is discarded with the merge, and an unstaged edit to the staged `rwv.toml` refuses the abort until it is staged or set aside; under linear history `rwv abort` keeps an uncommitted fix as a backup.

`--continue` loads all parameters (source or target, the landing's plan, `--history`, `--retire`) from the saved op-state record. Do not re-supply the original flags. Already-converged repositories are skipped, so `--continue` is idempotent.

## Roll back with `rwv abort`

`rwv abort` restores every ref the operation moved to its pre-op savepoint (under `refs/rwv/pre-op/<op-id>`, or `refs/rwv/pre-op/<op-id>-target` in the target of a `land`): each checkout, onto the branch it was on, and each branch a landing moved without a checkout. It cancels a rebase, merge or cherry-pick the operation left in progress in the source, and clears the op-state record:

| Current repository tip | `rwv abort` outcome |
|---|---|
| Equal to the savepoint | `untouched` — the operation never moved this repository |
| Equal to the recorded intent or converged tip | `restored` — reset to the savepoint |
| On another branch than when the operation began, at a tip the operation accounts for | `switched back` onto that branch, the branch it was on left where it stands, then judged by this table |
| Stopped in a rebase, merge or cherry-pick the operation started, begun on a tip the operation wrote | `restored` — the VCS operation canceled, then judged by this table |
| Stopped in an operation begun where the operation found it, in a repository the operation never merges or rebases, or in a `land`'s target a repository the landing never moved | `left` where it stands, the operation in progress there included |
| Stopped in any other operation: begun on a commit made after the stop, or in a `land`'s target on a tip the landing moved | refused, the operation in progress left as it is; op-state retained |
| Anything else | `foreign-tip violation` — restore refused, the tip left in place and nothing written; op-state retained |

A branch the landing moved without a checkout is put back once every checkout is restored, only from the tip the landing wrote and only while nothing has it checked out; one the landing created is deleted.

Every commit `rwv abort` resets off is one the operation made, so the commits it discards are the operation's own output. Uncommitted edits made during the stop are kept: before any reset that would discard them in the workspace you run it in, `rwv abort` keeps them as a backup and prints its name (`rwv backup list` shows it). Edits in a checkout of the other workspace of a landing refuse the abort instead: it names the command that sets them aside there, or run `rwv abort` in that workspace, which keeps them as a backup. Canceling the operation's own stopped rebase or merge in the workspace you run it in is the one step it takes as the VCS's own abort would; in the other workspace of a landing, `rwv abort` first keeps that checkout's working copy as a backup and prints its name. A refusal moves and writes nothing: abort plans every step on both sides before it makes any, so one obstacle anywhere (a foreign tip, a branch that is gone or checked out elsewhere, edits in the other workspace of a landing, another lock pinning what a landing wrote in its target, a manifest it cannot read) refuses the whole abort, says `nothing is restored` (for a landing, `nothing of either side is restored`) and leaves `--continue` open.

### Handling a `foreign-tip` refusal

If a commit was made in a repository after the sync paused, `rwv abort` refuses to overwrite that unattributed tip and retains op-state:

- **To discard the post-crash commits and roll back to the savepoint**, pass `--abandon-foreign-tip <repo>` (repeatable per repository; `projects/<name>` names the project repo):
  ```bash
  rwv abort --abandon-foreign-tip github/foo/bar
  ```
  The abandoned commits are first kept as a backup marked `abandoned`, named for the branch they were on, and abort prints its name. `rwv backup list` shows it with the commits only it holds, and `rwv backup drop github/foo/bar <name>` drops it once they are not wanted. If the foreign agent kept committing between two runs, the rerun keeps the tip as it then stands.

The refusal ends with the commands that end the operation, one per way it can end, each already carrying every `--abandon-foreign-tip` you gave, so run a printed command as printed:

- `rwv abort` again, once the fixes it printed are made;
- `rwv abort --abandon-foreign-tip <repo> ...`, naming each repository that can be given up;
- `rwv land --continue`, where a landing stopped in its retire and can finish;
- `rwv abort --leave-target`, for a landing whose target something else moved, or holds a commit the landing wrote that another lock now pins: the source is restored, the target is left as it stands, and each tip the landing wrote there is kept as a backup first where no branch reaches it.

Edits in the other workspace of a landing are set aside there with the stash the refusal prints (`git -C <checkout> stash push --include-untracked`, or `--all` where an ignored file is in the way), or the abort is run in that workspace (`rwv -C <workspace> abort`) where the refusal prints that line.

Once `rwv abort` has begun, the operation can only be aborted: `--continue` refuses it. A write that finds a repository moved since abort judged it stops there, saying `abort stopped there`; what was restored stays, nothing after it is done, and `rwv abort` run again completes the rest.

A repository refused because it is stopped in an operation abort did not start prints its own choices: finish or abandon that operation there, then rerun `rwv abort`; or `rwv abort --abandon-foreign-tip <repo>`, which keeps its edits and commits as backups before canceling it.

A branch the operation moved with no checkout holding it is put back only where it still sits at the tip the operation wrote. One that moved since is refused with `rwv abort --abandon-foreign-tip <repo>`, which keeps its new tip as an `abandoned` backup and puts it back; one a checkout now holds names that checkout to free, then rerun `rwv abort`.

### When no operation is in progress

If no op-state record is present in the workspace, `rwv abort` exits with status `1` and prints `Error: no operation in progress`.

## Related

- [Recover from a sync conflict](./recover-from-sync-conflict.md) — resolving conflicts before running `--continue`
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave work with `rwv land`
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, savepoints, and op-state

For the full contracts, run `rwv explain abort` (verified restore and foreign-tip recovery), `rwv explain sync`, and `rwv explain land` (op-state and `--continue`).
