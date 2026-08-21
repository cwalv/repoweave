# Bring workweave work home

Land work from a workweave into its parent (typically primary). The one-liner handles the common case; the manual ceremony underneath is the same operation broken into steps.

## The one-liner

```bash
cd ../.workweaves/web-app--payments
rwv sync-to --retire
```

Workweaves live at `<parent>/.workweaves/<project>--<name>/` — by default a sibling of the weave root, not a child. From the weave root, that path is `../.workweaves/<project>--<name>/`.

`rwv sync-to` is the landing verb: CWD's committed state lands in the target workspace. Bare `rwv sync-to` auto-targets the recorded parent from `.rwv-workweave` (one hop toward primary). `--retire` adds a post-landing cleanup step so the workweave is deleted on success.

The full orchestration is three steps, not one:

1. **Replay CWD's commits onto the parent's tip.** CWD's unique project commits are replayed onto the parent's project tip, with `rwv.lock` excluded from each commit's diff. Manifest repos in CWD are aligned to the parent's lock targets. CWD advances to the new tip, sitting linearly on top of the parent's prior state.
2. **Auto-relock CWD if manifest tips moved.** If step 1's manifest-repo advances changed any lock targets, `rwv.lock` is regenerated and committed into CWD automatically.
3. **Parent fast-forwards to CWD's new tip.** The parent's project repo fast-forwards to CWD's tip. The parent now has CWD's commits linearly above its prior state.

Then, if all three steps succeed and `--retire` was given: verify no worktree is dirty, then delete the workweave (worktrees + ephemeral branches + directory).

See [sync-semantics](../explanation/joints/sync-semantics.md) for the full phase model and the auto-relock detail.

## Preview what will land

Before running `rwv sync-to`, inspect what the workweave has that its recorded parent does not:

```bash
rwv workweave <project> log            # commit listing per repo, versus recorded parent
rwv workweave <project> log --diff     # unique diff versus the recorded parent
```

Must be run from inside a workweave. Anchored at the common ancestor, so commits the parent gained since the workweave was created are not shown as reversals — the output is what will actually land, not a symmetric diff. Add `--json` for a machine-readable envelope.

## Preconditions

Before `rwv sync-to` (or `--retire`) will run, both workspaces must satisfy `rwv doctor --locked`: each repo's tip must match its `rwv.lock`. Concretely:

```bash
# in the workweave
git -C github/chatly/server status   # clean
git -C github/chatly/web    status   # clean
rwv lock                              # snapshot post-edit state
git -C projects/web-app commit -am "lock: payments feature"
```

The lock-precondition check is what makes sync deterministic — the lock is the target, not the working tree.

## Explicit target

To land into a workspace other than the recorded parent, name it explicitly:

```bash
rwv sync-to primary
rwv sync-to /path/to/other-weave
rwv sync-to other-workweave-name
```

With an explicit target, `--retire` still applies: the workweave is deleted on success if the flag is given.

## What `--retire` actually does

`--retire` does not skip or alter any of the three sync steps. It runs after a successful step 3:

1. Verify every worktree in the workweave is clean (no uncommitted edits).
2. Verify the workweave's manifest tips equal the parent's (confirming convergence).
3. Delete the workweave: worktrees, ephemeral branches, and the directory.

If any step 1–3 of the sync itself hits a conflict, the workweave is preserved and the `--retire` cleanup is not reached. Resolve the conflict and re-run; see [If sync-to hits a conflict](#if-sync-to-hits-a-conflict) below.

## If sync-to hits a conflict

When step 1 hits a real conflict (a manifest-repo rebase or the project-repo replay encounters a textual conflict), `rwv sync-to` bails with a message naming the affected repo and the concrete resolution steps. Op-state is written so the operation can be resumed.

After resolving conflicts in the indicated repo:

```bash
# resolve conflicts in the repo named in the error message
cd github/chatly/server
# edit conflicted files
git add <files>

# then resume from the workweave root — rwv drives the remaining picks and relock
cd <workweave-root>
rwv sync-to --continue
```

`--continue` resumes from where the operation paused. All parameters — target, strategy, `--retire` — are read from the in-progress op-state file. Do not re-supply the original flags (including `--retire`); any flag other than `--project` alongside `--continue` is rejected. See [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) for the full `--continue` semantics and how to inspect op-state records.

If step 3 (the parent's FF-advance) fails — rare; requires a concurrent op on the parent between steps 2 and 3 — op-state is similarly preserved. `rwv sync-to --continue` retries step 3.

To give up entirely:

```bash
rwv abort
```

`rwv abort` reads the in-progress op-state, restores every repo in both CWD and the target to its savepoint ref, runs any VCS-native abort (`git rebase --abort`, etc.) for in-progress operations, then removes the marker and savepoint refs. After abort, both workspaces are in their exact pre-op state.

## Manual ceremony

When `--retire` doesn't fit (you want to keep the workweave for follow-up work, the parent isn't primary, you want to inspect the result before deleting), do it in two steps:

```bash
# step 1 — land the work (without retire)
cd ../.workweaves/web-app--payments
rwv sync-to

# step 2 — delete manually when satisfied
rwv workweave web-app delete payments
```

Or, if you want to absorb new work from primary into the workweave before landing (e.g., a sibling workweave already landed):

```bash
# absorb primary's new commits into the workweave first
rwv sync primary --strategy rebase

# then land
rwv sync-to --retire
```

If `rwv sync` stops mid-way with a conflict or crash, see
[Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) for how to
inspect op-state, resume with `--continue`, or roll back with `rwv abort`.

## n-way landing (two workweaves)

When two workweaves both have project commits, land them serially. The first lands with a clean fast-forward (the default `rebase` strategy in `sync-to` step 1 becomes a no-op when CWD is already strictly ahead). The second must first absorb the primary's new state:

Each `cd` here starts from the weave root; workweaves live at `../.workweaves/<project>--<name>/` relative to it.

```bash
# ww1 lands first (clean)
cd ../.workweaves/web-app--ww1
rwv sync-to --retire

# back to the weave root (the ww1 dir was retired), then absorb primary into ww2
cd -
cd ../.workweaves/web-app--ww2
rwv sync primary --strategy rebase

# then land ww2
rwv sync-to --retire
```

`rwv.lock` is never merged — it is recomputed in Phase 3 each time. Lock-file conflicts are prevented by the `rwv.lock merge=rwv-ours` invariant that `rwv doctor --fix` installs in each project repo's committed `.gitattributes`; sync refuses (with the fix as remediation) rather than conflicting on repos that don't yet carry it. See [sync-semantics](../explanation/joints/sync-semantics.md) for the worked example.

## Choosing a strategy

`rwv sync-to` defaults to `--strategy rebase` for step 1 (aligning CWD against the target before the FF-advance). When the workweave is strictly ahead of the target (clean landing path, no divergence), step 1 is a fast-forward no-op. The strategy only matters when the target has advanced since the workweave was created:

| Strategy | Step 1 behavior |
|---|---|
| `rebase` (default) | Replay CWD's unique commits onto target's tip; produces linear history |
| `ff` | Refuse if CWD and target have diverged; only works if CWD is strictly ahead |

`merge` is not offered — see [sync semantics](../explanation/joints/sync-semantics.md#why-no-merge-strategy). Step 3 (FF-advance the target) is always fast-forward regardless of this flag.

## Related

- [workweave lifecycle](../explanation/joints/workweave-lifecycle.md) — the full create → work → land → delete lifecycle; retire contract and deletion semantics
- [sync-semantics](../explanation/joints/sync-semantics.md) — phase ordering, strategy choices, `--retire`, parallel/NDJSON output
- [lock-as-derived](../explanation/joints/lock-as-derived.md) — why `rwv.lock` is treated specially in Phase 1' and regenerated in Phase 3
- [workweave hierarchy](../explanation/joints/workweave-hierarchy.md) — one-hop semantics, parent tracking
- [recover from sync conflict](./recover-from-sync-conflict.md) — what to do when Phase 1' or Phase 2 hits a real conflict
- [resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspect op-state, `--continue`, `rwv abort` detail
