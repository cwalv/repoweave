# Recover from a sync conflict

When `rwv sync` or `rwv land` pauses on a merge or rebase conflict, resolve the conflicted files in the named repository, stage them, and resume from the weave root with `--continue` (or roll back with `rwv abort`):

```bash
# 1. Edit conflicted files and stage them in the named repository
cd github/chatly/server
# edit conflicted files ...
git add <files>

# 2. Resume from the weave root where the sync started
cd ../../..
rwv sync --continue      # or: rwv land --continue
```

To abandon the sync instead and restore every repository in the involved workspaces to its pre-op savepoint:

```bash
rwv abort
```

## Reading the conflict output

When a project-repo replay or member-repo rebase hits a textual conflict, `rwv` prints a per-repo line naming the conflicted checkout followed by a summary with resolution instructions:

```text
  github/chatly/server: Rebase in <weave-root>/github/chatly/server hit a conflict; resolve and continue, or abort to roll back
Error: sync hit conflicts in one or more member repos (see per-repo lines above).

To resolve each conflicted repo:
cd <repo>
  # edit conflicted files
  git add <files>
then resume (repos already composed are left as they are):
rwv sync --continue

If you'd rather roll everything back: `rwv abort`.

rwv explain op-parked
```

`rwv sync` stops on a conflict with one message naming the repo, the checkout and the branch it was taking in, the backend's hint up to staging, and `rwv sync --continue` or `rwv abort`; the output above is `rwv land`'s.

`--continue` reads all original parameters — the branch taken in or the target, `--history`, `--reset-to-target`, `--retire`, and any consent flags — from the saved op-state record. Do not pass the original flags again alongside `--continue`; any other flag is rejected as a usage error (exit 2), except `--project`, which must name the project the op started for, and `--json` on `rwv land`.

If you step through a multi-commit rebase manually with `git rebase --continue`, the lock merge driver merges `rwv.lock` in each remaining pick; a pick whose pins it cannot settle stops with `rwv.lock` conflicted, which you resolve to either side, since `rwv` relocks afterwards. Once the manual rebase finishes, run `rwv sync --continue` (or `rwv land --continue`) so `rwv` regenerates `rwv.lock` and clears the op-state record.

## Common conflict cases

- **Project repo (`projects/<name>/`) conflict:** Two workspaces edited the same non-lock file in the project repo. (`rwv.lock` itself is recorded again once every repo has taken in.) Resolve the file in `projects/<name>/`, run `git add`, and resume with `--continue`. Until the operation ends, no verb that commits the project repo runs there — the `rwv member` and `rwv branch` setters, `rwv lock --commit`, and a new `rwv sync`, `rwv land`, `rwv pull` or `rwv cherry-pick` refuse with `edits` `commit mid-operation`, even once every path is staged, since their commit would become the merge's. The operation's own `--continue` writes that commit from what you staged; make the setting change after it, or `rwv abort`. The one exception is a setter for an entry of a conflicted `rwv.toml`, which settles that entry instead (next item).
- **`rwv.toml` conflict:** Both sides changed the same entry of `rwv.toml` differently — one member's table, one integration's table, or one other top-level key such as `history` or `[workweave]`. The stop of `rwv sync`, `rwv land` or `rwv pull` lists each entry in conflict with the value on each side and the setter that keeps each. `yours` is the side of the branch checked out in the project repo and `incoming` the side it takes in, under `merge` and `linear` history alike — where git's own `ours` and `theirs` swap during a replay, these do not:

  ```text
  rwv.toml entry github/chatly/server is in conflict:
    branch: "feat-a" in yours, "feat-b" incoming
      keep yours: rwv member set-branch github/chatly/server feat-a
      keep incoming: rwv member set-branch github/chatly/server feat-b
  ```

  Run the setter you choose from the weave root. At this stop it settles that entry on both sides and commits nothing: while another entry is still in conflict, `rwv.toml` stays conflicted and is rewritten from both sides with only what is left between conflict markers; once none is, the merged `rwv.toml` is staged. An entry only one side changed comes out with that side's value, as git merged it. Like every setter, a setter here writes `rwv.toml` whole from its values, so comments and hand layout in it are not kept. `--continue` refuses while `rwv.toml` is in conflict and lists what is left. An entry no verb sets (`[workweave]`, an integration's table) is named with the hand edit that settles it: edit `rwv.toml`, then `git add -- rwv.toml`. A hand edit staged with `git add` settles the whole file at any point. A setter rewrites the conflicted `rwv.toml`; before it does, it keeps the project repo's edits as they stand in a backup (`rwv backup list` names it) and prints the command that reads `rwv.toml` from it, so an edit you had not staged is not lost. A setter interrupted after it recorded the settled sides but before it rewrote the file leaves `rwv.toml` showing the markers it had before; the stop's list, `--continue` and the next setter read what was recorded, so running the setters that list names, or `rwv abort`, finishes from there. Under `linear` history, keeping the incoming value of every entry your commit changed leaves that commit with nothing of its own, and the replay drops it, as `git rebase` does.

  `rwv cherry-pick` does not take this route: at its stop a setter refuses with `edits` `commit mid-operation` as for any project conflict, and `rwv.toml` is resolved by hand and staged with `git add -- rwv.toml`.
- **Member repo conflict:** A member repo's branch has a textual conflict against the incoming target revision. Resolve the conflicted files in that member checkout, run `git add`, and resume with `--continue`.
- **`rwv land --retire` conflict:** When `--retire` hits a conflict, the workweave is preserved so you can resolve it. After staging fixes, run bare `rwv land --continue` — `--retire` is remembered in op-state and runs automatically once the sync finishes.

For inspecting in-flight op-state, handling `foreign-tip` refusals during `rwv abort`, or finding the commits an abandoned foreign tip left in an `abandoned` backup (`rwv backup list`), see [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md).

## Related

- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspecting mid-op state, `--continue` rules, and `rwv abort` recovery
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave changes with `rwv land`
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, lock exclusion, and savepoint protocol
- [VCS as seam](../explanation/joints/vcs-as-seam.md) — how VCS conflict state is surfaced
