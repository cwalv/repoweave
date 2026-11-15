# The deletion rule and removing stores

Removing a branch, a workweave or a member is the one place a multi-repo tool can lose
work that single-repo git would also lose, and the one place it can lose it across
repos at once. rwv holds a single bar there: **a verb never deletes what only the
deleted thing holds, unless the operator names that loss, and a named loss is kept as a
backup where a backup can exist.** This page states what each deleting verb removes,
what keeps a branch alive, and what removing a store asks first.

## What belongs to a branch

`rwv branch delete <W>`, `rwv workweave delete <name>` and `rwv land --retire` delete a
weave branch `W` and what belongs to it: the project branch `W`, which may exist only
on a remote as last fetched, and in each member `W`'s `rwv.toml` lists, the branch named
`W` that no live weave branch declares. A declared branch never belongs, so no cleanup
touches the branches a line's members sit on. A weave branch is **live** when its tip,
local or remote-tracking as of the last fetch, exists in any project of this machine's
workspaces.

## The deletion rule

A branch that belongs is deleted when all of these hold; otherwise it survives, with its
record, reported with its reason:

1. **It has landed.** Every commit on it is reached by the branch `W` lands into: the
   target of its record, or `--target <T>`. Members are judged first. This is ancestry,
   the same test a landing uses; nothing else proves it.
2. **It is not pinned.** No live lock outside `W`'s own pins a commit only this branch
   reaches.
3. **For the project branch,** it is not an open pull request's head (published, its push
   branch landed on neither the target nor the target's remote-tracking branch), and no
   member branch that belongs survives.
4. **`W` has no dependents:** no other branch's record lands into it.

| Survivor reason | Meaning |
|---|---|
| `unlanded` | commits the target lacks; with the fact `patch-equivalent` where the target holds commits carrying the same changes |
| `pinned` | a live lock pins a commit only it reaches |
| `open-head` | its published copy is a pull request head that has landed nowhere |
| `members` | the project branch, while a member branch that belongs survives |
| `unconfirmed` | its remote could not be fetched, so nothing on it is deleted |

A delete that leaves a survivor exits with status `3`; the survivors are listed, and
under `--json` named with their reasons. A branch never published is judged on local
refs alone, offline too; the rule fetches only the remotes the branches have an upstream
or a push branch on. A record is dropped in the same write only when its branch is gone
locally and from every remote and no workspace is aligned for it.

## Named consents

Each flag lifts one clause and names what it gives up:

| Flag | Lifts | Keeps first |
|---|---|---|
| `--discard-unlanded` | clause 1, the landed condition | the tip of each branch it deletes, as a backup; a detached HEAD's commits |
| `--discard-uncommitted` | the dirt refusal | each checkout's uncommitted edits as a commit on its HEAD, then the backup |
| `--delete-remote` | nothing: it also deletes each push branch of what belongs that has landed on the target's remote-tracking branch | its tip, locally |
| `--delete-remote-unlanded <repo>` | the landed condition on that repo's push branch, an open pull request head included; needs `--discard-unlanded`; never the push branch of a declared-mode project branch | its tip, as a backup; leased to the fetched tip |
| `--discard-unpublished` | the stores' refusal below | nothing: it goes with the store, and lists what went |

No flag lifts clause 2 or 4. A pinned branch survives `--discard-unlanded`, and a branch
with dependents is refused outright, since its dependents would be left targeting
nothing. Remote deletes never run unless named, and never on a declared branch.

## A workweave's removal

`rwv workweave delete` refuses, before changing anything, a checkout with uncommitted
changes, a detached HEAD holding commits no branch reaches, and a branch with dependents.
It also refuses when the workweave's marker, the project clone's index and its project
checkout's link to that clone disagree, restoring nothing and naming `rwv doctor --fix`.
Then it removes the workweave and applies the rule to its branch when that is a feature.
Both refusals cover every checkout under the workweave, a clone put there by hand
included. A workweave whose recorded directory is gone is deleted the same way: its
registrations and index entry go, and nothing is judged of checkouts that are no longer
there. `rwv land --retire` does the same after a landing, and a refusal then leaves the
landing done and the workweave in place; `rwv land --continue` retires it once nothing
refuses.

## Removing a store

A member's checkout is a worktree of a store, and so is each submodule inside a
checkout. Removing the checkout in the primary weave destroys the store, since the clone
is the repo's only copy. In a workweave, the worktree goes and the shared store stays,
but a submodule store a workweave's checkout keeps goes with it. In both cases nothing
else holds what the store holds that no remote has.

So before it removes a store, rwv asks **every store it removes**, a submodule store
checked out or not included, what it holds that no remote-tracking branch of a remote the
store has reaches: a branch, a tag, a backup, an operation's savepoint, edits the VCS set
aside, a checked-out commit on no branch. It first fetches those remotes, so the answer
is what the remotes hold now. The removal refuses before it changes anything in two
cases:

- **`dropped-repo-has-unique-commits`**: the store holds work no remote has. The refusal
  names each item, with the submodule's path where it is inside one. Publish or merge the
  work, take back set-aside edits, or drop a backup you do not want with
  `rwv backup drop`.
- **`remotes-unconfirmed`**: a remote did not answer, so the comparison cannot be made.
  Run the removal again when it answers, or remove the member and keep its clone with
  `rwv member remove <repo>` without `--delete`.

`--discard-unpublished` lets the removal go on with the work no remote holds, judged
against the remotes as last fetched, and lists what went. No backup is kept for it,
because the backup would be inside the store being removed. A removal that touches no
submodule store asks no remote at all.

A sync or landing that would prune a member the new entries drop, and meets either
refusal, still completes. It keeps the checkout, says why, and names the removal that
takes it later, `rwv member remove <repo> --delete`; a sync then exits 1, since a
checkout it was asked to prune is still on disk.

## Related

- [Destructive operations](../destructive-operations.md): the policy of named consents and kept backups.
- [Workweave lifecycle](./workweave-lifecycle.md): where deletion sits in a workweave's life.
- [Landing](./landing.md): `--retire` and the target the rule compares against.
- [`rwv branch delete`](../../reference/cli.md#rwv-branch-delete-w---target-t---discard-unlanded---delete-remote---delete-remote-unlanded-repo---json---project-name) and [`rwv backup`](../../reference/cli.md#rwv-backup).
