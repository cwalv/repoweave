# Alignment

A project's members are repos with branches of their own, and the project
repo's checked-out branch has an opinion about where each one sits. **Alignment**
is the agreement between that opinion and the member checkouts. It is judged,
never guessed: where a member sits is a function of the checked-out branch, its
[record](./branch-records.md), its `rwv.toml` entries, its `rwv.lock`, the
branch the workspace last aligned for, and the refs. Dirt only refuses; it never
changes the answer.

## Where members sit

> Members sit as the checked-out project branch's record says. In a `feature`,
> its `owned` and `fork` members are on branches named after it, which rwv creates,
> and its `dependency` and `reference` members are as declared. On a `declared`
> branch, members are on the branches its entries declare. In a detached workspace
> every repo is detached at the revision and at its pins.

A member's **expected state** is the first row that matches:

| Situation | Expected state |
|---|---|
| the lock has no pin for it | on its expected branch where one exists (aligned, `pin` `none`), in a detached workspace too, as at a clone; where none exists, unaligned |
| detached workspace | detached at the pin |
| a feature's `owned` or `fork` member | on the member branch named after the feature |
| the branch its entry declares reaches the pin (itself or by its upstream), and no other workspace has it checked out | on that declared branch |
| otherwise | detached at the pin, as `pin` `not on declared branch` or with the fact `checkout` |

Where a member's branch is behind the pin it fast-forwards; ahead of it, the
member stays, reported as `pin` `ahead`. A branch that exists nowhere is
created only where the verb has a reason: a feature's own branch is created at the
pin, and a declared branch is created only by `rwv member set-branch --new`,
`--set-new-branch`, or a landing.

Alignment reads no other branch's content and no kind of change. A merge, a rename,
a cherry-pick or a conflict edit changes it only by changing an entry (reported, see
below) or by leaving a branch without a record (refused).

## What the verbs do with it

`rwv update`, `rwv switch`, `rwv branch create`, `rwv branch set-mode`,
`rwv workweave create`, `rwv member set-branch`, `rwv member pin` and
`rwv activate --update` align members. They share one rule for whether a member can
go to its expected state, and one for which members move:

- a member moves with the VCS's own branch switch, keeping the commits of the
  branch it leaves, when it is on a declared branch, on a branch named after a
  recorded weave branch, or on any other branch whose commits all sit on some other
  branch, remote-tracking branch or backup (reported `moved from <branch>`);
- a member holding commits only its branch holds, uncommitted changes, or an
  operation in progress does not move. `rwv update` reports it and the verbs that
  need it aligned refuse it, each with the choices that clear that state;
- `rwv switch` and `rwv branch create` carry uncommitted edits as the VCS's own
  switch does, and refuse when an edit would be overwritten.

`rwv lock`, `rwv push`, `rwv sync`, `rwv land` and `rwv cherry-pick` refuse an
acted-on member that is not aligned while it could be (`alignment`, choices
printed). Each successful alignment records the branch the workspace aligned for
and the member entries it aligned them by. That is the pointer state each workspace
keeps for itself, written only when something in it changes.

## States that are reported, not errors

| State | Meaning | What it does |
|---|---|---|
| unrecorded | the checked-out project branch has no record | the verbs that align, lock, land or publish refuse; `fetch`, `status`, `switch`, `branch create`, `branch set-mode`, `branch list`, `log`, `backup` and `abort` run |
| unaligned | the branch the workspace last aligned for is gone (a raw rename) or none is recorded | reported on `alignment` as `project unaligned`; `rwv update` aligns |
| moved | the project repo is on another branch than the one the workspace aligned for, which still exists (a raw switch) | a fact, with `rwv switch <aligned>` and `rwv update` as choices; members not aligned for the checked-out branch are refused per member |
| detached | a detached workspace, or a raw detach | only `fetch`, `status`, `switch`, `branch create`, `branch set-mode`, `branch list`, `log`, `backup` and `abort` run; `update` too in a detached workspace |
| checkout | a member's branch is checked out in another workspace | the member sits detached at its pin, naming the checkout; the project repo is never detached this way: a project branch checked out elsewhere refuses `rwv switch` and opening it |

A workspace's marker is checked against the relation it is derived from on every
verb: the project repo must be a linked worktree of the named primary's project
clone, and that clone's index must list the root. A missing or disagreeing marker
is restored from the index and reported; the verbs that destroy a workweave
(`rwv workweave delete`, `rwv land --retire`) refuse instead, naming
`rwv doctor --fix`.

## Finding the branch

For a member whose expected branch needs finding, the first match wins:

1. a local branch with a same-name upstream is used;
2. a local branch with no upstream whose name is on a remote is given that
   upstream first;
3. a local branch whose upstream has another name refuses as
   `push-branch` `foreign upstream`: no verb that aligns, lands or pushes goes on;
4. any other local branch is used;
5. a branch only on a remote is taken from the only remote that has it, else the
   home remote, created at the pin where the upstream reaches the pin, else at
   the upstream's tip;
6. a branch nowhere: a feature's is created at the pin, or with no pin the member
   stays unaligned (`pin` `none`).

A member that stays unaligned keeps its pin through `rwv lock`, is pushed by
nothing, and is still judged at the pin gate. A member another project lists but the
checked-out branch does not is not aligned at all, and reported as such.

## Names

One member branch must not serve two features, or a feature and a declared branch.
A feature `W` is refused, written or opened, while a member's branch `W` is declared
(`name` `declared`) or is the member branch of another project's feature `W` that
lists the same member (`name` `feature`). `rwv member set-branch` is refused when the
branch it names is a feature of a project listing the member. A weave branch named as
the records branch or below it is refused (`name` `reserved`). A collision seen
later, for instance after a merge brought in a declared branch of that name, is
refused by the verbs that align, land or push that feature, with
`rwv branch rename` and `rwv branch set-mode <W> declared` as the choices; nothing
is lost.

## Entries change what alignment follows

An entry, such as a member's declared branch, is content: a merge, a fast-forward or
a cherry-pick carries it like any line of a config file. The aligning verbs, `rwv lock`,
`rwv push` and `rwv status` compare the checked-out branch's entries with the
ones stored at the last alignment; `rwv pull` and `rwv sync` report the changes
they take in. Each difference is reported as `member-changed` with the previous and
the new value and the old branch's unpushed commits. On a declared branch it stays
reported, verb after verb, until the push branch carries the change or it is
restored. Alignment follows the new branch; a pin it does not reach detaches the
member. On a feature it is a fact: the landing decides, and
[names the change](./landing.md#member-changes-are-named).

## Related

- [Records and modes](./branch-records.md): what the checked-out branch's record is.
- [Shared-refs drift](./shared-refs-drift.md): files that lag a branch moved under a checkout.
- [Workweave lifecycle](./workweave-lifecycle.md): how a workweave comes to sit where it does.
- The `alignment` and `checkout` kinds in [Refusals](../../reference/refusals.md).
