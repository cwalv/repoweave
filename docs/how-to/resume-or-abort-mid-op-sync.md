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

`--continue` loads all parameters (source or target, the landing's plan, `--strategy`, `--retire`) from the saved op-state record. Do not re-supply the original flags. Already-converged repositories are skipped, so `--continue` is idempotent.

## Roll back with `rwv abort`

`rwv abort` restores every repository in the involved workspaces to its pre-op savepoint (`refs/rwv/pre-op/<op-id>`, or `refs/rwv/pre-op/<op-id>-target` for a repository in the target of a `land`), cancels any in-progress VCS rebase/merge/cherry-pick, and clears the op-state record:

| Current repository tip | `rwv abort` outcome |
|---|---|
| Equal to the savepoint | `untouched` — the operation never moved this repository |
| Equal to the recorded intent or converged tip | `restored` — reset to the savepoint |
| Mid-rebase / mid-merge / mid-cherry-pick / mid-revert | `restored` — VCS operation canceled and reset to the savepoint |
| Anything else | `foreign-tip violation` — restore refused, the tip left in place and nothing written; op-state retained |

A clean restore writes no ref: every tip `rwv abort` resets is a commit the operation made, so what it discards is the operation's own output. A refusal moves and writes nothing either.

Uncommitted working-tree edits that existed before the sync started and untracked tool directories (`node_modules/`, `.venv/`, `target/`) are not snapshotted by savepoints and are not modified by `rwv abort`.

### Handling a `foreign-tip` refusal

If a commit was made in a repository after the sync paused, `rwv abort` refuses to overwrite that unattributed tip and retains op-state:

- **To discard the post-crash commits and roll back to the savepoint**, pass `--abandon-foreign-tip <repo>` (repeatable per repository; `projects/<name>` names the project repo):
  ```bash
  rwv abort --abandon-foreign-tip github/foo/bar
  ```
  The abandoned commits are first kept as a backup marked `abandoned`, named for the branch they were on, and abort prints its name. `rwv backup list` shows it with the commits only it holds, and `rwv backup drop github/foo/bar <name>` drops it once they are not wanted. If the foreign agent kept committing between two runs, the rerun keeps the tip as it then stands.
- **To keep the post-crash commits**, move the branch back to the tip recorded by the op (or finish resolving manually) and re-run `rwv abort`.

### When no operation is in progress

If no op-state record is present in the workspace, `rwv abort` exits with status `1` and prints `Error: no operation in progress`.

## Related

- [Recover from a sync conflict](./recover-from-sync-conflict.md) — resolving conflicts before running `--continue`
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave work with `rwv land`
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, savepoints, and op-state

For the full contracts, run `rwv explain abort` (verified restore and foreign-tip recovery), `rwv explain sync`, and `rwv explain land` (op-state and `--continue`).
