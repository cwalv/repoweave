# Bring workweave work home

Land a workweave's feature into the branch its record lands into (typically
`main`, checked out in the primary weave) and retire the workweave:

```bash
cd ../.workweaves/web-app--payments
rwv land --retire
```

`rwv land` reads the feature's record for its target, finds the workspace
that has the target checked out, and judges every refusal before anything
moves. Then:

1. **Relock and compose here.** Members with commits beyond the workweave's
   `rwv.lock` are relocked forward. Each member holding work its receiving
   branch lacks is rebased onto that branch's tip, and the project branch onto
   the target's tip — all in the workweave.
2. **Record the lock.** `rwv.lock` is merged three ways and committed, each
   composed member at its new tip.
3. **Fast-forward the target.** Where the target is checked out, each
   receiving branch and the target's project branch fast-forward to what was
   composed. That workspace then aligns, and the workweave's members follow
   the target's new pins.
4. **Retire on `--retire`.** The workweave's worktrees, the branches that
   belong to it by the deletion rule, and its directory are removed.

To land into another branch for one run, name it: `rwv land --target stable`.
The record is unchanged.

## Preview what will land

From inside the workweave, list the commits that will land relative to the weave branch they land onto (here `main`), or the diff from where each repo's pin meets your HEAD:

```bash
rwv log --against main            # commit listing per repo beyond main's pins
rwv log --against main --diff     # diff per repo from the merge-base of that pin and HEAD
```

Add `--json` for a machine-readable envelope. To read a workweave from outside it, address it with `-w`: `rwv -w web-app--payments log --against main`.

## Preconditions

- A target checked out nowhere lands as refs: it and each receiving branch
  move without a checkout, and a branch it declares beyond its pin refuses
  (`pin` `unpinned`), naming the `rwv workweave create` that opens a
  workspace on the target to record it in.
- A receiving branch checked out in a third workspace, such as the primary
  detached on the project with its members still on `main`, fast-forwards
  in place there; that checkout must be clean, and the workspace is leased
  to the landing while it runs.
- Every repo the landing moves, here and in the target's workspace, must be
  clean; under `--retire`, every checkout of the workweave, untracked files
  included.
- In the target's workspace, a receiving branch holding commits its lock does
  not record, and no other lock or remote holds, refuses (`pin` `unpinned`).
  The refusal names the target's workspace and prints its choices addressed
  to it (`rwv -C <target workspace> lock --commit`), which run from where
  they are printed.
- The landing changes the target's member entries only where named: an
  entry the feature changed refuses (`target` `member changes`) until
  `rwv land --allow-member-change <repo>` names it.

## Keep the workweave or land in two steps

When you want to keep the workweave for follow-up work or inspect the parent before deleting:

```bash
cd ../.workweaves/web-app--payments
rwv land
rwv workweave delete payments   # when ready to clean up
```

## Land multiple workweaves serially

When two workweaves both have commits to land, land them one at a time. The second landing needs more than a fast-forward, so the target must say how it accepts landings: [record that first](#saying-how-the-target-accepts-landings), or pass `--history <setting>` to the `rwv sync` and `rwv land` below.

```bash
# 1. Land and retire ww1
cd ../.workweaves/web-app--ww1
rwv land --retire

# 2. Optionally take the target's new work into ww2 to test against it, then land and retire ww2
cd ../web-app--ww2
rwv sync
rwv land --retire
```

`rwv land` composes ww2's work onto the target's new tip by itself, so the `rwv sync` step is only for building and testing ww2 against ww1's landed work before it lands. It takes in the branch ww2's record lands into, by the target's `history` setting: under `linear` ww2's work is rebased onto the target's tip, under `merge` the target's work is merged in.

`rwv.lock` is recomputed rather than merged; see [Sync semantics](../explanation/joints/sync-semantics.md) for how `rwv.lock` is excluded during replay.

## Saying how the target accepts landings

How a landing composes ww2's work onto a target that moved is the target's `history` setting, recorded in its own `rwv.toml`. Record it once, in a workspace on the target:

```bash
rwv branch set-history linear
```

| Setting | Step 1 behavior |
|---|---|
| `linear` | Replay CWD's unique commits onto the target's tip, producing linear history |
| `merge` | A merge commit whose first parent is the target's tip |
| `ff-only` | Refuse unless the target's tip is already in the feature; `rwv sync` rebases it there first |

A target that records nothing refuses a landing that needs more than a fast-forward (`history` `unset`); `rwv land --history linear` lands once without recording a setting. The refusal prints the setter addressed to the target's workspace (`rwv -C <target workspace> branch set-history <setting>`), so it runs from the workweave it is printed in.

## If `land` hits a conflict

If a replay or rebase hits a conflict during steps 1–3, `rwv land` stops before reaching `--retire`, keeps the workweave, and keeps its op record. Resolve the conflicted files in the named repo, mark them resolved (for git, `git add`), and resume from the workweave root — or roll both workspaces back to their pre-op state:

```bash
git -C <conflicted-repo> add <files>
rwv land --continue    # or: rwv abort
```

Do not re-pass `--retire` or strategy flags with `--continue`; they are restored from the saved op record. See [Recover from a sync conflict](./recover-from-sync-conflict.md) for the step-by-step conflict resolution walkthrough.

## If `--retire` refuses to delete the workweave

`--retire` removes the workweave only when no checkout under it, a manifest member or not, holds uncommitted changes (`edits`) or a detached HEAD with commits no branch reaches (`alignment`). Both are judged before anything moves, so a refused `land --retire` has landed nothing. Commit or discard what the refusal names, and run it again:

```bash
git -C github/chatly/extra commit -am "keep this"   # the checkout the refusal names
rwv land --retire
```

## If `--retire` leaves a branch behind

A branch of the workweave that has not landed on the branch the workweave's record lands into survives the retire. The workweave is gone, `land` exits with status `3`, and the output lists each branch and why (`unlanded`, `pinned`, `open-head`, `members`, `unconfirmed`). Land it, or delete it with `rwv branch delete <branch> --discard-unlanded`, which keeps its tip as a backup first.

## Related

- [Recover from a sync conflict](./recover-from-sync-conflict.md) — resolving project or member-repo conflicts during `sync` or `land`
- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspecting the op record, `--continue` rules, and `rwv abort` recovery
- [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md) — creation, landing, and deletion contracts
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, lock exclusion, and savepoints
- [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — parent tracking and one-hop landing
- [Lock as derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is regenerated rather than merged
