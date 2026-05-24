# Recover from a sync conflict

`rwv sync` is multi-phase. A conflict mid-way leaves you in a state that requires understanding *which* phase failed. The error message tells you — and includes a Vcs-trait-derived hint with the concrete next steps.

## The error you'll see

When a Phase 1' replay or Phase 2 strategy hits a real conflict, sync bails with a message that names the affected repo and embeds the resolution hint:

```text
error: sync stopped — conflict in github/chatly/server during rebase

To resolve:
  edit the conflicted files
  cd github/chatly/server
  git add <files>
  git rebase --continue

Then re-run `rwv sync ... --strategy rebase` to resume.
To give up entirely: `rwv abort`.
```

The hint text is owned by the VCS layer (`Vcs::conflict_resolution_hint` — see [vcs-as-seam](../explanation/joints/vcs-as-seam.md)). Sync embeds it verbatim so the operator sees concrete next steps without trial-and-error.

## Fix and resume

```bash
cd github/chatly/server
# edit conflicted files
git add <files>
git rebase --continue
```

Then re-run the same sync command:

```bash
cd <workspace-root>
rwv sync <source> --strategy rebase
```

Already-advanced repos are no-ops. The fixed repo finishes, then any remaining repos proceed. Phase 3 regenerates `rwv.lock` at the end.

The savepoint protocol (`refs/rwv/pre-op/<id>`) is still in place — every repo's pre-sync tip is preserved until a successful run cleans up. You can always back out.

## Give up entirely

```bash
rwv abort
```

`rwv abort` reads the in-progress op marker, restores every repo to its savepoint ref, runs any VCS-native abort (`git rebase --abort`, `git merge --abort`, `git cherry-pick --abort`) for in-progress operations, then removes the marker and savepoint refs.

After abort, the workspace is in its exact pre-sync state. Discarded commits remain reachable from the savepoint until git's normal unreferenced-object collection runs — you can recover by hand even after abort.

## Common cases

**Phase 1' (project repo) conflict.** Two workweaves edited the same file in `projects/<name>/docs/` and both committed. The `rwv.lock` line itself never conflicts — it is excluded from Phase 1' inputs and regenerated in Phase 3. Resolve the non-lock conflict, `git rebase --continue`, re-run sync.

**Phase 2 (manifest repo) conflict.** A manifest repo's branch needs `rebase` or `merge` to advance, and the cross-history has a genuine textual conflict. Same procedure: resolve in the repo, complete the in-progress VCS op, re-run sync.

**Lock precondition failure.** Sync refused before any repo was touched because `rwv check --locked` failed. Fix the unlocked repos (commit, then `rwv lock`, then commit the lock) and re-run. This is not a conflict — there's no savepoint to abort because nothing was mutated.

## Related

- [sync-semantics](../explanation/joints/sync-semantics.md) — the three phases and where conflicts arise
- [vcs-as-seam](../explanation/joints/vcs-as-seam.md) — why the hint text comes from the VCS impl
- [bring workweave work home](./bring-workweave-work-home.md) — the normal-path sync workflow
