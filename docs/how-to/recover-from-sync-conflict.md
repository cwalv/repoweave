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
then run `rwv sync --continue`, or `rwv abort` to put everything back as it was, keeping first as a backup each edit and commit it would discard

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
- **`rwv.toml` conflict:** git left regions of `rwv.toml` in conflict, and both sides changed an entry in them differently — one field of a member both sides hold, one key of an integration's table or of `[workweave]` both sides hold, a member or table one side added or removed, or one other top-level key such as `history`. What git merged outside those regions stands as git merged it, and an entry inside them only one side changed comes out with that side's value. The stop of `rwv sync`, `rwv land` or `rwv pull` lists each entry in conflict with the value on each side. `yours` is the side of the branch checked out in the project repo and `incoming` the side it takes in, under `merge` and `linear` history alike — where git's own `ours` and `theirs` swap during a replay, these do not:

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

  `--continue --keep yours` or `--continue --keep incoming` takes that side's text in each region in conflict of the working file as git left it at the stop, sets each entry there only the other side changed to that side's value, and writes the result into `rwv.toml`, keeping the comments and layout outside the regions as git merged them. It then stages the file and goes on. Where no entry is in conflict — each side added a different member at the same place, say, so git conflicts only on where the lines go — the stop says so, and a bare `--continue` writes and stages the merge the same way; a `--keep` given there takes nothing and says so. A bare `--continue` with an entry in conflict refuses, listing it. Neither writes over an edit: once you have edited `rwv.toml` since the stop, both refuse, and the ways on are to finish it by hand and mark it resolved once it holds what you mean, then `--continue`, or `rwv abort`; run the operation again and it stops here afresh, where `--keep` takes a side again. A dependent branch the operation carries along stops on `rwv.toml` the same way, `yours` being the dependent's side. To mix the two sides, keep one, then run the setters for the entries you want otherwise once the operation has ended. Every command the stop prints carries `-C <workspace>` where you started the operation from outside its workspace, with `-w` or `-C`, so it acts on the stopped workspace from where you typed it.

  It writes only over `rwv.toml` as the stop left it, so it keeps no backup of what it writes over, conflict markers and all: where you resolved `rwv.toml` by hand and did not stage it, `--continue` refuses and prints the `git add -- rwv.toml` that keeps your resolution, and a file that already declares the merge is staged as it stands. A hand edit staged with `git add` settles the whole file and is committed as staged. Verbs that write the project repo refuse at the stop: `rwv update` as `op-in-progress`, and `rwv member set-role` and `rwv lock --commit` as `edits` `commit mid-operation`, which prints the stop's own choices; the stop's own lines name the `--keep` continues.

  Under `linear` history each replayed commit that conflicts in `rwv.toml` stops on its own, and each stop takes its own `--continue`; an entry settled at one stop is asked again only where a later commit changes it again. Keeping incoming for every entry your commit changed leaves that commit with nothing of its own, and the replay drops it, as `git rebase` does.

  `rwv sync`, `rwv land` and `rwv pull` also check `rwv.toml` where the VCS merged its text with no conflict: a line merge of a file whose tables repeat the same lines can succeed with a wrong result, such as a setting moved to another member or a removed member back. Under `merge` history the merge is checked before it is committed: where the merged text parses and declares something other than the merge of both sides' values, the operation stops with `rwv.toml` unmerged, listing each entry that differs; the working file holds the VCS's text merge, untouched. Under `linear` history the replay runs as a plain rebase runs it, and once it has finished, the replayed tip's `rwv.toml` is compared with the merge of the values of the merge base, the commit replayed onto and your branch's tip before the replay; where they differ the operation stops as `unparseable-project` `mid-operation`, listing each entry. The replayed tip is compared however the replay's own conflicts were resolved, by hand or by `--keep`. A dependent branch `rwv sync` or `rwv land` carries along gets the same comparison of its replayed tip: where it differs the dependent is not moved, and a line names it and the `rwv workweave create` that opens it, where `rwv sync` stops on its own tip.

  The comparison judges only entries at most one side changed. An entry both sides changed is the VCS's to merge or to conflict on, as with any file: where the VCS merged it with no conflict (each side added an element to one list, or your branch changed again an entry whose first change is already upstream), its text stands. Where the comparison stops for another entry, it also lists each entry both sides changed whose value the text merge holds as one side has it, with both sides' values, since a moved line may have carried the other side's value elsewhere.

  What is not checked: the commits a replay writes before its tip keep the VCS's text, as a plain rebase leaves them; a text that does not parse stops as `unparseable-project` `mid-operation` with no comparison; where your branch and the one it takes in have more than one merge base (each merged the other before), no one base is common to both and the VCS's merge stands unchecked; and a merge made by hand, or on a forge, is not checked. A landing judges the `rwv.toml` its replay left against the target whatever settled it, so a target entry a later replayed commit changed lands only once named with `--allow-member-change`.

  At either check stop rwv writes nothing to `rwv.toml`, and there are two ways on besides `rwv abort`:

  ```text
    keep the text merge as the VCS made it
      rwv sync --continue --keep text-merge
    edit rwv.toml into what you mean and mark it resolved, then continue
      git -C … add -- rwv.toml
      rwv sync --continue
  ```

  A bare `--continue` with nothing marked resolved refuses and prints the stop again. `--keep text-merge` marks the VCS's text resolved as it stands: at the merge check it refuses once you have edited the working file, and points at `git add` to take your edit instead; at the replayed tip it keeps the tip's text as committed, leaving an edit not marked resolved in the checkout, uncommitted, and refuses where another `rwv.toml` is marked resolved, printing the `git restore --staged` that withdraws it. Typed at a stop the check did not make, it refuses, as `--keep yours` and `--keep incoming` refuse at a check stop. A file you mark resolved is taken whole and not judged again, where it parses; one that does not stops the bare `--continue` again with the parser's error, as any `rwv.toml` that does not parse does; marking the replayed tip's own `rwv.toml` resolved unchanged marks nothing. Once settled, the replayed tip is not judged again in that operation, whatever later stop it meets.

  `rwv cherry-pick` makes the same comparison for what it ports. It ports no `rwv.toml` entry change, reporting each one `not-ported` with the setter that makes it; make the setting change once the cherry-pick ends. It does port the `[integrations]` and `[workweave]` tables, and where both sides changed one and git merged its text with no conflict, that text is compared with the merge of both sides' values of the picked commit's parent, your branch's tip and the picked commit. Where it does not parse or differs, the pick stops with `rwv.toml` unmerged and the VCS's text merge in place, untouched, listing what differs; where git itself conflicted on `rwv.toml`, the stop holds your branch's tables instead. This stop has two ways on, with no `--keep`: edit `rwv.toml` into what you mean, stage it with the printed `git add`, then `rwv cherry-pick --continue`; or `rwv abort`. Entries and the top-level `history` are never ported, so a wrong text merge of them is not compared; a table only one side changed is taken by value.
- **Member repo conflict:** A member repo's branch has a textual conflict against the incoming target revision. Resolve the conflicted files in that member checkout, run `git add`, and resume with `--continue`.
- **`rwv land --retire` conflict:** When `--retire` hits a conflict, the workweave is preserved so you can resolve it. After marking your fixes resolved, run bare `rwv land --continue` — `--retire` is remembered in the op record and runs automatically once the sync finishes.

For inspecting the in-flight op record, handling `foreign-tip` refusals during `rwv abort`, or finding the commits `rwv abort` kept as a backup (`rwv backup list`), see [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md).

## Related

- [Resume or abort a mid-op sync](./resume-or-abort-mid-op-sync.md) — inspecting mid-op state, `--continue` rules, and `rwv abort` recovery
- [Bring workweave work home](./bring-workweave-work-home.md) — landing workweave changes with `rwv land`
- [Sync semantics](../explanation/joints/sync-semantics.md) — phase ordering, lock exclusion, and savepoint protocol
- [VCS as seam](../explanation/joints/vcs-as-seam.md) — how VCS conflict state is surfaced
