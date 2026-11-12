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
4. **Retire on `--retire`.** After step 3 succeeds, `--retire` verifies that every worktree in the workweave is clean and converged with the parent, then deletes the workweave's worktrees, the branches that belong to it by the deletion rule, and its directory.

To land into a workspace other than the recorded parent, pass the target explicitly:

```bash
rwv sync-to primary
rwv sync-to /path/to/other-weave
rwv sync-to other-workweave-name
```

## Preview what will land

From inside the workweave, list the commits that will land relative to the weave branch they land onto (here `main`), or the diff from where each repo's pin meets your HEAD:

```bash
rwv log --against main            # commit listing per repo beyond main's pins
rwv log --against main --diff     # diff per repo from the merge-base of that pin and HEAD
```

Add `--json` for a machine-readable envelope. To read a workweave from outside it, address it with `-w`: `rwv -w web-app--payments log --against main`.

## Preconditions

- Every worktree in both the workweave and the target workspace must be clean (no uncommitted changes to tracked files).
- In the workweave (CWD), member repos with new commits (`ahead` of `rwv.lock`) are auto-relocked by `rwv sync-to` at operation start.
- In the **target** workspace, every repo must already match its `rwv.lock`. If you made commits directly in the target workspace without locking them, run `rwv lock --commit` in the target workspace before running `rwv sync-to`.

## Keep the workweave or land in two steps

When you want to keep the workweave for follow-up work or inspect the parent before deleting:

```bash
cd ../.workweaves/web-app--payments
rwv sync-to
rwv workweave delete payments   # when ready to clean up
```

## Land multiple workweaves serially

When two workweaves both have commits to land, land them one at a time:

```bash
# 1. Land and retire ww1
cd ../.workweaves/web-app--ww1
rwv sync-to --retire

# 2. Optionally take the target's new work into ww2 to test against it, then land and retire ww2
cd ../web-app--ww2
rwv sync --strategy rebase
rwv sync-to --retire
```

`rwv sync-to` replays ww2's commits onto the parent's new tip by itself, so the `rwv sync` step is only for building and testing ww2 against ww1's landed work before it lands. It takes in the branch ww2's record lands into. Plain `rwv sync` uses `--strategy ff`, which refuses when ww2 and its target have both moved, as they have here; `--strategy rebase` rebases ww2's work onto the target's tip instead.

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

`--retire` removes the workweave only when no checkout under it, a manifest member or not, holds uncommitted changes (`edits`) or a detached HEAD with commits no branch reaches (`alignment`). The landing has already completed; the op stays parked at its retire step in both workspaces. Commit or discard what the refusal names, and re-run the retire step:

```bash
git -C github/chatly/extra commit -am "keep this"   # the checkout the refusal names
rwv sync-to --continue
```

`rwv abort` rolls the whole op back instead.

## If `--retire` leaves a branch behind

A branch of the workweave that has not landed on the branch the workweave's record lands into survives the retire. The workweave is gone, `sync-to` exits with status `3`, and the output lists each branch and why (`unlanded`, `pinned`, `open-head`, `members`, `unconfirmed`). Land it, or delete it with `rwv branch delete <branch> --discard-unlanded`, which keeps its tip as a backup first.

## Related

- [Recover from a sync conflict](./recover-from-sync-conflict.md) — resolving project or member-repo conflicts during `sync` or `sync-to`
- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspecting op-state, `--continue` rules, and `rwv abort` recovery
- [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md) — creation, landing, and deletion contracts
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, lock exclusion, and savepoints
- [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — parent tracking and one-hop landing
- [Lock as derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is regenerated rather than merged
