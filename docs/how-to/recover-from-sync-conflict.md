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

- **Project repo (`projects/<name>/`) conflict:** Two workspaces edited the same non-lock file in the project repo. (`rwv.lock` itself is recorded again once every repo has taken in.) Resolve the file in `projects/<name>/`, run `git add`, and resume with `--continue`. Until the operation ends, no verb that commits the project repo runs there — the `rwv member` and `rwv branch` setters, `rwv lock --commit`, and a new `rwv sync`, `rwv land`, `rwv pull` or `rwv cherry-pick` refuse with `edits` `commit mid-operation`, even once every path is staged, since their commit would become the merge's. The operation's own `--continue` writes that commit from what you staged; make the setting change after it, or `rwv abort`.
- **`rwv.toml` conflict:** Both sides changed the same entry of `rwv.toml` differently — one field of a member both sides hold, one key of an integration's table or of `[workweave]` both sides hold, a member or table one side added or removed, or one other top-level key such as `history`. A field only one side changed is no conflict, even beside a field of the same table that is: it comes out with that side's value. The stop of `rwv sync`, `rwv land` or `rwv pull` lists each entry in conflict with the value on each side. `yours` is the side of the branch checked out in the project repo and `incoming` the side it takes in, under `merge` and `linear` history alike — where git's own `ours` and `theirs` swap during a replay, these do not:

  ```text
  rwv.toml entry github/chatly/server is in conflict:
    branch: "feat-a" in yours, "feat-b" incoming
  (yours: the side of the branch checked out in …; incoming: the side it takes in)
  to take one side for every entry in conflict, and write and stage the merge of both sides:
    rwv sync --continue --keep yours
    rwv sync --continue --keep incoming
  or edit rwv.toml into what you mean, then stage it:
    git -C … add -- rwv.toml
  ```

  `--continue --keep yours` or `--continue --keep incoming` takes that side's value for every entry in conflict and every other entry from the side that changed it, writes that merge into `rwv.toml` as an edit of the checked-out side's file (its comments and layout stay), stages it, and goes on. Where no entry is in conflict — each side added a different member, say, and git conflicts only on where the lines go — the stop says so, and a bare `--continue` writes and stages the merge the same way; a `--keep` given there takes nothing and says so. A bare `--continue` with an entry in conflict refuses, listing it. To mix the two sides, keep one, then run the setters for the entries you want otherwise once the operation has ended. Every command the stop prints carries `-C <workspace>` where you started the operation from outside its workspace, with `-w` or `-C`, so it acts on the stopped workspace from where you typed it.

  Before it writes, `--continue` keeps the project repo's edits as they stand in a backup (`rwv backup list` names it) and prints the command that reads `rwv.toml` from it. It writes only over a file that does not read as a manifest, the conflict markers git left: where you resolved `rwv.toml` by hand and did not stage it, `--continue` refuses and prints the `git add -- rwv.toml` that keeps your resolution, and a file that already declares the merge is staged as it stands. A hand edit staged with `git add` settles the whole file and is committed as staged. The setters, and every other verb that commits the project repo, refuse at the stop with `edits` `commit mid-operation`, naming the `--keep` continues.

  Under `linear` history each replayed commit that conflicts in `rwv.toml` stops on its own, and each stop takes its own `--continue`; an entry settled at one stop is asked again only where a later commit changes it again. Keeping incoming for every entry your commit changed leaves that commit with nothing of its own, and the replay drops it, as `git rebase` does.

  `rwv cherry-pick` does not take this route: it ports no `rwv.toml` entry change, reporting each one `not-ported` with the setter that makes it; make the setting change once the cherry-pick ends.
- **Member repo conflict:** A member repo's branch has a textual conflict against the incoming target revision. Resolve the conflicted files in that member checkout, run `git add`, and resume with `--continue`.
- **`rwv land --retire` conflict:** When `--retire` hits a conflict, the workweave is preserved so you can resolve it. After staging fixes, run bare `rwv land --continue` — `--retire` is remembered in op-state and runs automatically once the sync finishes.

For inspecting in-flight op-state, handling `foreign-tip` refusals during `rwv abort`, or finding the commits an abandoned foreign tip left in an `abandoned` backup (`rwv backup list`), see [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md).

## Related

- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspecting mid-op state, `--continue` rules, and `rwv abort` recovery
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave changes with `rwv land`
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, lock exclusion, and savepoint protocol
- [VCS as seam](../explanation/joints/vcs-as-seam.md) — how VCS conflict state is surfaced
