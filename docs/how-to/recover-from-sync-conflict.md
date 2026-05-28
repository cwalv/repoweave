# Recover from a sync conflict

Both `rwv sync` and `rwv sync-to` use the same multi-phase engine. A conflict mid-way leaves you in a state that requires understanding *which* phase failed. The error message tells you — and includes a Vcs-trait-derived hint with the concrete next steps.

The procedure is nearly identical for both verbs because the conflict surface is shared: both run Phase 2 (manifest repos) and Phase 1' (project repo replay) through the same engine. What differs is which command you use to resume.

## The error you'll see

When a Phase 1' replay or Phase 2 strategy hits a real conflict, the operation bails with a message that names the affected repo and embeds the resolution hint:

```text
error: sync stopped — conflict in github/chatly/server during rebase

To resolve:
  edit the conflicted files
  cd github/chatly/server
  git add <files>
  git rebase --continue

Then re-run with `--continue` to resume.
To give up entirely: `rwv abort`.
```

The hint text is owned by the VCS layer (`Vcs::conflict_resolution_hint` — see [vcs-as-seam](../explanation/joints/vcs-as-seam.md)). The sync engine embeds it verbatim so the operator sees concrete next steps without trial-and-error.

## Fix and resume — `rwv sync`

```bash
cd github/chatly/server
# edit conflicted files
git add <files>
git rebase --continue
```

Then re-run the same sync command with `--continue`:

```bash
cd <workspace-root>
rwv sync <source> --strategy rebase --continue
```

Already-advanced repos are no-ops. The fixed repo finishes, then any remaining repos proceed. Phase 3 regenerates `rwv.lock` at the end.

## Fix and resume — `rwv sync-to`

The procedure is the same: resolve the conflict in the named repo, complete the VCS operation, then re-run with `--continue`:

```bash
cd github/chatly/server
# edit conflicted files
git add <files>
git rebase --continue
```

Then resume from the workweave root:

```bash
cd <workweave-root>
rwv sync-to --continue
# or, if the original invocation named an explicit target:
rwv sync-to primary --continue
```

`--continue` validates that your invocation parameters match what op-state recorded (target, strategy, `--retire` flag). Repeat the same flags you used originally — a mismatch errors out so you don't accidentally resume with different intent. If the original invocation had `--retire`, pass it again on the `--continue` resume; the cleanup step runs after the resumed operation completes successfully.

### Step 3 failure (FF-advance failure)

If steps 1 and 2 succeed but step 3 (the parent's fast-forward advance) fails — this requires a concurrent operation on the target between steps 2 and 3, so it is rare — op-state is preserved in the same way. Re-run `rwv sync-to --continue` (or `rwv sync-to <target> --continue`) to retry step 3. No conflict resolution is needed; step 3 is always a fast-forward and will succeed once the concurrent op-state clears.

## Give up entirely

```bash
rwv abort
```

`rwv abort` reads the in-progress op marker, restores every repo to its savepoint ref, runs any VCS-native abort (`git rebase --abort`, `git merge --abort`, `git cherry-pick --abort`) for in-progress operations, then removes the marker and savepoint refs.

For `rwv sync-to`, `rwv abort` rolls back savepoints in **both** CWD and the target — the target's state is restored to its pre-op tip as well.

After abort, both workspaces are in their exact pre-op state. Discarded commits remain reachable from the savepoint until git's normal unreferenced-object collection runs — you can recover by hand even after abort.

## Common cases

**Phase 1' (project repo) conflict.** Two workweaves edited the same file in `projects/<name>/docs/` and both committed. The `rwv.lock` line itself never conflicts — it is excluded from Phase 1' inputs and regenerated in Phase 3. Resolve the non-lock conflict, `git rebase --continue`, re-run with `--continue`.

**Phase 2 (manifest repo) conflict.** A manifest repo's branch needs `rebase` or `merge` to advance, and the cross-history has a genuine textual conflict. Same procedure: resolve in the repo, complete the in-progress VCS op, re-run with `--continue`.

**Lock precondition failure.** The operation refused before any repo was touched because `rwv doctor --locked` failed. Fix the unlocked repos (commit, then `rwv lock`, then commit the lock) and re-run. This is not a conflict — there's no savepoint to abort because nothing was mutated.

**`--retire` conflict.** When `rwv sync-to --retire` hits a conflict in steps 1–3, the workweave is preserved (the `--retire` cleanup is not reached until all three steps succeed). Resolve the conflict, then resume with `rwv sync-to --retire --continue` — pass `--retire` again so the parameter-match check passes. The cleanup runs after the resumed steps complete successfully.

## Related

- [sync-semantics](../explanation/joints/sync-semantics.md) — the three phases and where conflicts arise; abort and savepoint protocol
- [vcs-as-seam](../explanation/joints/vcs-as-seam.md) — why the hint text comes from the VCS impl
- [bring workweave work home](./bring-workweave-work-home.md) — the normal-path sync-to workflow
