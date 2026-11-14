# Recover from a sync conflict

When `rwv sync` or `rwv land` pauses on a merge or rebase conflict, resolve the conflicted files in the named repository, mark them resolved (for git, `git add`), and resume from the weave root with `--continue` (or roll back with `rwv abort`):

```bash
# 1. Edit conflicted files and stage them in the named repository
cd github/chatly/server
# edit conflicted files ...
git add <files>

# 2. Resume from the weave root where the sync started
cd ../../..
rwv sync --continue      # or: rwv land --continue
```

`rwv status` names the operation and marks each repository it stopped in (`[mid-rebase]`, for example), with the two commands that end it; see [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md#inspect-what-is-in-flight).

To abandon the sync instead and restore every repository in the involved workspaces to its pre-op savepoint:

```bash
rwv abort
```

## Reading the conflict output

When a member-repo rebase or merge hits a textual conflict, `rwv sync` stops with one message naming the repo, the checkout and what it was taking in, the backend's hint up to marking the files resolved, and the two ways on:

```text
Error: rwv sync: the rebase of github/chatly/server onto main stopped on a conflict (src/lib.rs) at <workweave-root>/github/chatly/server.
  # edit conflicted files
  git add <files>
then run `rwv sync --continue`, or `rwv abort` to put everything back as it was

rwv explain op-parked
```

`rwv land` stops the same way and prints a per-repo line, then a longer summary that names `rwv land --continue` and `rwv abort`:

```text
  github/chatly/server: Rebase in <workweave-root>/github/chatly/server hit a conflict; resolve and continue, or abort to roll back
Error: sync hit conflicts in one or more member repos (see per-repo lines above).

To resolve each conflicted repo:
cd <repo>
  # edit conflicted files
  git add <files>
then resume (repos already composed are left as they are):
rwv land --continue

If you'd rather roll everything back: `rwv abort`.

rwv explain op-parked
```

`--continue` reads all original parameters — the branch taken in or the target, `--history`, `--reset-to-target`, `--retire`, and any consent flags — from the saved op record. Do not pass the original flags again alongside `--continue`; any other flag is rejected as a usage error (exit 2), except `--project`, which must name the project the op started for, and `--json` on `rwv land`.

If you step through a multi-commit rebase manually (for git, `git rebase --continue`), the lock merge driver merges `rwv.lock` in each remaining pick; a pick whose pins it cannot settle stops with `rwv.lock` conflicted, which you resolve to either side, since `rwv` relocks afterwards. Once the manual rebase finishes, run `rwv sync --continue` (or `rwv land --continue`) so `rwv` regenerates `rwv.lock` and clears the op record.

## Common conflict cases

- **Project repo (`projects/<name>/`) conflict:** Two workspaces edited the same non-lock file in the project repo. (`rwv.lock` itself is recorded again once every repo has taken in.) Resolve the file in `projects/<name>/`, run `git add`, and resume with `--continue`. Until the operation ends, no verb that writes runs in its workspace or in a workspace it leases — the `rwv member` and `rwv branch` setters, `rwv lock`, `rwv update`, and a new `rwv sync`, `rwv land`, `rwv pull` or `rwv cherry-pick` refuse as `op-in-progress`. Only the verbs that read, `rwv doctor --fix`, the operation's own `--continue` and `rwv abort` run. The operation's own `--continue` writes the commit from what you staged; make the setting change after it, or `rwv abort`.
- **`rwv.toml` conflict:** Both sides changed the same entry of `rwv.toml` differently — one field of a member both sides hold, one key of an integration's table or of `[workweave]` both sides hold, a member or table one side added or removed, or one other top-level key such as `history`. A field only one side changed is no conflict, even beside a field of the same table that is: it comes out with that side's value. The stop of `rwv sync`, `rwv land` or `rwv pull` lists each entry in conflict with the value on each side. `yours` is the side of the branch checked out in the project repo and `incoming` the side it takes in, under `merge` and `linear` history alike — where git's own `ours` and `theirs` swap during a replay, these do not:

  ```text
  rwv.toml entry github/chatly/server is in conflict:
    branch: "feat-a" in yours, "feat-b" incoming
  (yours: the side of the branch checked out in …; incoming: the side it takes in)
  to take one side for every entry in conflict, and write the merge of both sides, marked resolved:
    rwv sync --continue --keep yours
    rwv sync --continue --keep incoming
  or edit rwv.toml into what you mean, then mark it resolved:
    git -C … add -- rwv.toml
  ```

  `--continue --keep yours` or `--continue --keep incoming` takes that side's value for every entry in conflict and every other entry from the side that changed it, writes that merge into `rwv.toml` as an edit of the side git checked out, yours in a merge and incoming in a replay, whose comments and layout stay; comments and layout the other side changed are not carried, and the continue says so and names the backup that holds the file as git left it. It then stages the file and goes on. Where no entry is in conflict — each side added a different member, say, and git conflicts only on where the lines go — the stop says so, and a bare `--continue` writes and stages the merge the same way; a `--keep` given there takes nothing and says so. A bare `--continue` with an entry in conflict refuses, listing it. To mix the two sides, keep one, then run the setters for the entries you want otherwise once the operation has ended. Every command the stop prints carries `-C <workspace>` where you started the operation from outside its workspace, with `-w` or `-C`, so it acts on the stopped workspace from where you typed it.

  Before it writes, `--continue` keeps the project repo's edits as they stand in a backup (`rwv backup list` names it) and prints the command that reads `rwv.toml` from it. It writes only over a file that does not read as a manifest, the conflict markers git left: where you resolved `rwv.toml` by hand and did not stage it, `--continue` refuses and prints the `git add -- rwv.toml` that keeps your resolution, and a file that already declares the merge is staged as it stands. A hand edit staged with `git add` settles the whole file and is committed as staged. The setters, and every other verb that writes, refuse at the stop as `op-in-progress`; the stop's own lines name the `--keep` continues.

  Under `linear` history each replayed commit that conflicts in `rwv.toml` stops on its own, and each stop takes its own `--continue`; an entry settled at one stop is asked again only where a later commit changes it again. Keeping incoming for every entry your commit changed leaves that commit with nothing of its own, and the replay drops it, as `git rebase` does.

  `rwv sync`, `rwv land` and `rwv pull` also check `rwv.toml` where the VCS merged its text with no conflict: a line merge of a file whose tables repeat the same lines can succeed with a wrong result, such as a setting moved to another member or a removed member back. Under `merge` history the merge is checked before it is committed: where the merged text parses and declares something other than the merge of both sides' values, the operation stops with `rwv.toml` unmerged, listing each entry that differs, and the working file shows the VCS's text above the merge of the values between conflict markers. Under `linear` history the replay runs as a plain rebase runs it, and once it has finished, the replayed tip's `rwv.toml` is compared with the merge of the values of the merge base, the commit replayed onto and your branch's tip before the replay; where they differ the operation stops as `unparseable-project` `mid-operation`, listing each entry. A dependent branch `rwv sync` or `rwv land` carries along gets the same comparison of its replayed tip: where it differs the dependent is not moved, and a line names it and the `rwv workweave create` that opens it, where `rwv sync` stops on its own tip.

  The comparison judges only entries at most one side changed. An entry both sides changed is the VCS's to merge or to conflict on, as with any file: where the VCS merged it with no conflict (each side added an element to one list, or your branch changed again an entry whose first change is already upstream), its text stands, both in the comparison and in what the bare `--continue` writes; where the VCS conflicted, the stop above lists it and you choose.

  What is not checked: the commits a replay writes before its tip keep the VCS's text, as a plain rebase leaves them; once a replay has stopped with `rwv.toml` conflicted and you resolved it yourself, marking your own file resolved, the replay's tip is yours and neither it nor a later commit's clean text merge in that replay is checked (where `--continue` or `--keep` wrote the resolution, the tip is still checked); a text that does not parse stops as `unparseable-project` `mid-operation` with no merge of the values offered; where your branch and the one it takes in have more than one merge base (each merged the other before), no one base is common to both and the VCS's merge stands unchecked; and a merge made by hand, or on a forge, is not checked. A landing judges the `rwv.toml` its replay left against the target whatever settled it, so a target entry a later replayed commit changed lands only once named with `--allow-member-change`.

  At either check stop there are three ways on:

  ```text
    write the merge of both sides' values in place of the text merge
      rwv sync --continue
    keep the text merge as the VCS made it
      rwv sync --continue --keep text-merge
    edit rwv.toml into what you mean and mark it resolved, then continue
      git -C … add -- rwv.toml
      rwv sync --continue
  ```

  The bare `--continue` writes the merge of the values as an edit of the VCS's text merge, so the comments and layout both sides gave the file stay; under `linear` history it becomes part of the replayed tip. `--keep text-merge` keeps the VCS's text as it stands; typed at a stop the check did not make, it refuses. A file you stage is taken whole and not judged again, where it parses; one that does not stops the bare `--continue` again with the parser's error, as any `rwv.toml` that does not parse does, and `--keep text-merge` puts the tip's own text back over it, keeping it in a backup; staging the replayed tip's own `rwv.toml` unchanged stages nothing, so to keep it use `--keep text-merge`. Once settled, the replayed tip is not judged again in that operation, whatever later stop it meets.

  `rwv cherry-pick` makes the same comparison for what it ports. It ports no `rwv.toml` entry change, reporting each one `not-ported` with the setter that makes it; make the setting change once the cherry-pick ends. It does port the `[integrations]` and `[workweave]` tables, and where both sides changed one and git merged its text with no conflict, that text is compared with the merge of both sides' values of the picked commit's parent, your branch's tip and the picked commit. Where it does not parse or differs, the pick stops with `rwv.toml` unmerged and your branch's tables in place, listing what differs. This stop has two ways on, with no `--keep`: edit `rwv.toml` into what you mean, stage it with the printed `git add`, then `rwv cherry-pick --continue`; or `rwv abort`. Entries and the top-level `history` are never ported, so a wrong text merge of them is not compared; a table only one side changed is taken by value.
- **Member repo conflict:** A member repo's branch has a textual conflict against the incoming target revision. Resolve the conflicted files in that member checkout, run `git add`, and resume with `--continue`.
- **`rwv land --retire` conflict:** When `--retire` hits a conflict, the workweave is preserved so you can resolve it. After marking your fixes resolved, run bare `rwv land --continue` — `--retire` is remembered in the op record and runs automatically once the sync finishes.

For inspecting the in-flight op record, handling `foreign-tip` refusals during `rwv abort`, or finding the commits an abandoned foreign tip left in an `abandoned` backup (`rwv backup list`), see [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md).

## Related

- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspecting mid-op state, `--continue` rules, and `rwv abort` recovery
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave changes with `rwv land`
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, lock exclusion, and savepoint protocol
- [VCS as seam](../explanation/joints/vcs-as-seam.md) — how VCS conflict state is surfaced
