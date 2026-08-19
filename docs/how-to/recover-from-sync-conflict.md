# Recover from a sync conflict

Both `rwv sync` and `rwv sync-to` use the same multi-phase engine. A conflict mid-way leaves you in a state that requires understanding *which* phase failed. The error message tells you — and includes a Vcs-trait-derived hint with the concrete next steps.

The procedure is nearly identical for both verbs because the conflict surface is shared: both run Phase 2 (manifest repos) and Phase 1' (project repo replay) through the same engine. What differs is which command you use to resume.

## The error you'll see

When a Phase 1' replay or Phase 2 strategy hits a real conflict, rwv prints a per-repo line naming the conflicted repo, then bails with a summary that embeds the resolution hint. For a Phase 2 (manifest-repo) conflict, shown here, the summary's own `cd` step is a generic `<repo>` placeholder — the per-repo line above it is what names the repo:

```text
  github/chatly/server: Rebase in <workspace-root>/github/chatly/server hit a conflict; resolve and continue, or abort to roll back
Error: sync hit conflicts in one or more manifest repos (see per-repo lines above).

To resolve each conflicted repo:
cd <repo>
  # edit conflicted files
  git add <files>
rwv sync --continue   # resume; already-converged repos are no-ops

If you'd rather roll everything back: `rwv abort`.
```

The hint text is owned by the VCS layer (`Vcs::conflict_resolution_hint` — see [vcs-as-seam](../explanation/joints/vcs-as-seam.md)). For rebase ops, the hint stops at staging; rwv core appends the appropriate `rwv <verb> --continue` line. The sync engine composes and emits the full message so the operator sees concrete next steps without trial-and-error.

## Fix and resume — `rwv sync`

```bash
cd github/chatly/server
# edit conflicted files
git add <files>
```

Then resume by running `rwv sync` with `--continue` from the workspace root. All parameters (source, strategy, overrides) are read from the in-progress op-state file — do not pass them again:

```bash
cd <workspace-root>
rwv sync --continue
```

`--continue` drives the remaining rebase picks, including any lock-only commits (resolved via the inline `rwv-ours` driver flags), then relocks and clears op-state. Already-advanced repos are no-ops. Phase 3 regenerates `rwv.lock` at the end.

**Fallback: bare `git rebase --continue`.** If you need to resume outside rwv — for example, to inspect git's own conflict markers one pick at a time — bare `git rebase --continue` works safely because the durable `merge.rwv-ours.driver` repo-local config keeps the lock exclusion active. After the bare-git resume finishes, re-run `rwv sync --continue` so rwv can complete relocking and clean up op-state. This is a fallback, not the primary path.

## Fix and resume — `rwv sync-to`

The procedure is the same: resolve the conflict in the named repo, stage the fixes, then re-run with `--continue`:

```bash
cd github/chatly/server
# edit conflicted files
git add <files>
```

Then resume from the workweave root:

```bash
cd <workweave-root>
rwv sync-to --continue
```

`--continue` reads all parameters — target, strategy, `--retire` — from the in-progress op-state file. Do not re-supply the original flags (including `--retire`); any flag other than `--project` alongside `--continue` is rejected. See [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) for op-state inspection and the full `--continue` contract.

### Step 3 failure (FF-advance failure)

If steps 1 and 2 succeed but step 3 (the parent's fast-forward advance) fails — this requires a concurrent operation on the target between steps 2 and 3, so it is rare — op-state is preserved in the same way. Re-run `rwv sync-to --continue` to retry step 3. No conflict resolution is needed; step 3 is always a fast-forward and will succeed once the concurrent op-state clears.

## Give up entirely

```bash
rwv abort
```

`rwv abort` reads the in-progress op marker, restores every repo to its savepoint ref, runs any VCS-native abort (`git rebase --abort`, `git merge --abort`, `git cherry-pick --abort`) for in-progress operations, then removes the marker and savepoint refs.

For `rwv sync-to`, `rwv abort` rolls back savepoints in **both** CWD and the target — the target's state is restored to its pre-op tip as well.

After abort, both workspaces are in their exact pre-op state. Abort removes the savepoint refs along with the marker, so the commits it discarded survive only as unreferenced objects until git's normal collection runs. Retrieving them is ordinary per-repo git recovery, outside rwv's verbs — no rwv verb undoes an abort. If you may want that work, land it with `rwv sync-to` or push the branch before aborting.

## Common cases

**Phase 1' (project repo) conflict.** Two workweaves edited the same file in `projects/<name>/docs/` and both committed. The `rwv.lock` line itself never conflicts — it is excluded from Phase 1' inputs and regenerated in Phase 3. Resolve the non-lock conflict, stage with `git add <files>`, then re-run with `rwv sync --continue` (or `rwv sync-to --continue`).

**Phase 2 (manifest repo) conflict.** A manifest repo's branch needs `rebase` to advance, and the cross-history has a genuine textual conflict. Same procedure: resolve in the repo, complete the in-progress VCS op, re-run with `--continue`.

**Lock precondition failure.** The operation refused before any repo was touched because `rwv doctor --locked` failed. Fix the unlocked repos (commit, then `rwv lock`, then commit the lock) and re-run. This is not a conflict — there's no savepoint to abort because nothing was mutated.

**`--retire` conflict.** When `rwv sync-to --retire` hits a conflict in steps 1–3, the workweave is preserved (the `--retire` cleanup is not reached until all three steps succeed). Resolve the conflict, then resume with bare `rwv sync-to --continue` — `--retire` is already recorded in op-state and is restored automatically; do not pass it again.

## Related

- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspect op-state records, `--continue` semantics, `rwv abort` detail
- [sync-semantics](../explanation/joints/sync-semantics.md) — the three phases and where conflicts arise; abort and savepoint protocol
- [vcs-as-seam](../explanation/joints/vcs-as-seam.md) — why the hint text comes from the VCS impl
- [bring workweave work home](./bring-workweave-work-home.md) — the normal-path sync-to workflow
