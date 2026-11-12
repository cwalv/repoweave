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

Every commit `rwv abort` resets off is one the operation made, so the commits it discards are the operation's own output. Uncommitted edits made during the stop are kept: before any reset that would discard them, `rwv abort` keeps them as a backup and prints its name (`rwv backup list` shows it). Canceling the operation's own stopped rebase or merge is the one step it takes as the VCS's own abort would. A refusal moves and writes nothing.

### Handling a `foreign-tip` refusal

If a commit was made in a repository after the sync paused, `rwv abort` refuses to overwrite that unattributed tip and retains op-state:

- **To discard the post-crash commits and roll back to the savepoint**, pass `--abandon-foreign-tip <repo>` (repeatable per repository; `projects/<name>` names the project repo):
  ```bash
  rwv abort --abandon-foreign-tip github/foo/bar
  ```
  The abandoned commits are first kept as a backup marked `abandoned`, named for the branch they were on, and abort prints its name. `rwv backup list` shows it with the commits only it holds, and `rwv backup drop github/foo/bar <name>` drops it once they are not wanted. If the foreign agent kept committing between two runs, the rerun keeps the tip as it then stands.

Once `rwv abort` has begun, the operation can only be aborted: `--continue` refuses it, so rerun `rwv abort` after clearing what it refused.

A repository refused because it is stopped in an operation abort did not start prints its own choices: finish or abandon that operation there, then rerun `rwv abort`; or `rwv abort --abandon-foreign-tip <repo>`, which keeps its edits and commits as backups before canceling it.

### When no operation is in progress

If no op-state record is present in the workspace, `rwv abort` exits with status `1` and prints `Error: no operation in progress`.

## Related

- [Recover from a sync conflict](./recover-from-sync-conflict.md) — resolving conflicts before running `--continue`
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave work with `rwv land`
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, savepoints, and op-state

For the full contracts, run `rwv explain abort` (verified restore and foreign-tip recovery), `rwv explain sync`, and `rwv explain land` (op-state and `--continue`).
