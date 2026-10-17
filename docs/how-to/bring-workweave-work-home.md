# Bring workweave work home

Land committed work from a workweave into its parent workspace (typically the primary weave) and retire the workweave:

```bash
cd ../.workweaves/web-app--payments
rwv sync-to --retire
```

Bare `rwv sync-to` reads `.rwv-workweave` and targets the recorded parent workspace:

1. **Auto-relock CWD and replay onto the parent's tip.** If any member repo in the workweave has commits ahead of the workweave's `rwv.lock`, `sync-to` snapshots and commits `rwv.lock` automatically. It then replays the workweave's unique project commits onto the parent's project tip and aligns member repos.
2. **Re-derive `rwv.lock` if member tips moved.** If step 1's alignment changed any member revisions, `rwv.lock` is regenerated and committed in the workweave.
3. **Fast-forward the parent.** The parent's project repo and member checkouts advance to the workweave's new tips.
4. **Retire on `--retire`.** After step 3 succeeds, `--retire` verifies that every worktree in the workweave is clean and converged with the parent, then deletes the workweave's worktrees, ephemeral branches, and directory.

To land into a workspace other than the recorded parent, pass the target explicitly:

```bash
rwv sync-to primary
rwv sync-to /path/to/other-weave
rwv sync-to other-workweave-name
```

## Preview what will land

From inside the workweave, inspect the commits or diff that will land relative to the recorded parent's common ancestor:

```bash
rwv workweave <project> log            # commit listing per repo vs. recorded parent
rwv workweave <project> log --diff     # unique diff vs. recorded parent
```

Add `--json` for a machine-readable envelope.

## Preconditions

- Every worktree in both the workweave and the target workspace must be clean (no uncommitted changes to tracked files).
- In the workweave (CWD), member repos with new commits (`ahead` of `rwv.lock`) are auto-relocked by `rwv sync-to` at operation start.
- In the **target** workspace, every repo must already match its `rwv.lock`. If you made commits directly in the target workspace without locking them, run `rwv lock --commit` in the target workspace before running `rwv sync-to`.

## Keep the workweave or land in two steps

When you want to keep the workweave for follow-up work or inspect the parent before deleting:

```bash
cd ../.workweaves/web-app--payments
rwv sync-to
rwv workweave web-app delete payments   # when ready to clean up
```

## Land multiple workweaves serially

When two workweaves both have commits to land, land them one at a time:

```bash
# 1. Land and retire ww1
cd ../.workweaves/web-app--ww1
rwv sync-to --retire

# 2. Optionally absorb primary's new state into ww2 to test against it, then land and retire ww2
cd ../web-app--ww2
rwv sync primary --strategy rebase
rwv sync-to --retire
```

`rwv sync-to` replays ww2's commits onto the parent's new tip by itself, so the `rwv sync primary` step is only for building and testing ww2 against ww1's landed work before it lands. Plain `rwv sync primary` uses `--strategy ff`, which stops when ww2 and the parent have both moved, as they have here; `--strategy rebase` replays ww2's commits onto the parent's tip instead.

`rwv.lock` is recomputed rather than merged; see [Sync semantics](../explanation/joints/sync-semantics.md) for how `rwv.lock` is excluded during replay.

## Choosing a strategy

`rwv sync-to` defaults to `--strategy rebase` when aligning CWD against a target that has advanced since the workweave was created:

| Strategy | Step 1 behavior |
|---|---|
| `rebase` (default) | Replay CWD's unique commits onto the target's tip, producing linear history |
| `ff` | Refuse if CWD and the target have diverged; succeeds only when CWD is strictly ahead |

## If `sync-to` hits a conflict

If a replay or rebase hits a conflict during steps 1–3, `rwv sync-to` pauses before reaching `--retire`, preserves the workweave, and writes op-state. Resolve the conflicted files in the named repo, stage them with `git add`, and resume from the workweave root — or roll both workspaces back to their pre-op state:

```bash
git -C <conflicted-repo> add <files>
rwv sync-to --continue    # or: rwv abort
```

Do not re-pass `--retire` or strategy flags with `--continue`; they are restored from the saved op-state. See [Recover from a sync conflict](./recover-from-sync-conflict.md) for the step-by-step conflict resolution walkthrough.

## If `--retire` refuses to delete the workweave

When the parent dropped a member repo that the workweave still has a checkout of, `sync-to` lands the work and then tries to prune that checkout. If the checkout holds uncommitted work, unpushed commits, or the only copy of its repository, the prune is refused and `--retire` refuses too (`retire-unpruned-checkout`, exit 1), naming the checkout. The landing has already completed; the op stays parked at its retire step in both workspaces. Save what you need from the checkout, remove it by hand, and re-run the retire step:

```bash
rm -rf github/chatly/extra       # the checkout the refusal names, after saving its work
rwv sync-to --continue
```

`--continue` refuses again while the checkout is still on disk. `rwv abort` rolls the whole op back instead.

## Related

- [Recover from a sync conflict](./recover-from-sync-conflict.md) — resolving project or member-repo conflicts during `sync` or `sync-to`
- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspecting op-state, `--continue` rules, and `rwv abort` recovery
- [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md) — creation, landing, and deletion contracts
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, lock exclusion, and savepoints
- [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — parent tracking and one-hop landing
- [Lock as derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is regenerated rather than merged
