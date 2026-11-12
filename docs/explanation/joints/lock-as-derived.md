# Lock as derived state

`rwv.lock` is derived state. Every pin in it is read from a member repo
on disk: by `rwv lock`, which records where each checkout is, and by the
verbs that name a commit, `rwv member add` for the commit it cloned,
`rwv member pin` for the commit you choose and `rwv update --remote` for
the upstream tip. That single property has outsized consequences for the
rest of the design, and this joint pulls them together.

## What "derived" means here

A piece of state is derived when there is a function from other state
to it and the function is the source of truth. The lock entries' values
are not opinions; they are reads of `git rev-parse HEAD` (one per
member repo) baked into a file. Re-running `rwv lock` on the same
checkouts and the same previous lock yields the same lock.

Useful frame for what this implies:

| Question | Single-repo analog | repoweave analog |
|---|---|---|
| Where does new state come from? | `git commit` on a working tree | `git commit` in each member repo |
| What captures cross-repo state? | n/a — single repo | `rwv lock` records the member repo tips |
| Is the snapshot meaningful before commit? | No — uncommitted edits don't have SHAs | No — uncommitted edits in member repos don't have SHAs |
| Can two operators produce identical snapshots? | Yes, given the same commits | Yes, given the same commit set per repo |

The lock is to a project what `git commit` is to a working tree: a
cheap, content-addressed checkpoint of state that exists elsewhere on
disk. It is not a coordinator, not a config, not a constraint.

## `rwv lock` records where the checkouts are

`rwv lock` reads each member repo's current HEAD and writes it into
`rwv.lock` as that member's pin. It does not:

- Fetch anything from the network.
- Run integration installs.
- Mutate any member repo's working tree.
- Give a member a pin it does not have.

It does read the previous lock, for one purpose: a pin moves forward
only. A member whose checkout is ahead of its pin takes the checkout's
commit. A member with no pin keeps none, because the commit a first pin
names is a choice a verb makes by name (`rwv member add`, `rwv member
pin`, `rwv update --remote`). A member whose checkout is behind its pin
or has diverged from it, or whose pin no ref here reaches, is refused,
`--repo` included, which only narrows: moving a pin backward is something
you say, with `rwv member pin`, a setter, or a hand edit of the entry.

Integration install hooks belong to `rwv activate` and `rwv materialize`,
which prepare the workspace on disk. `rwv lock` records commits and
nothing else.

The other "advancement" verbs — `rwv update --remote` and `rwv sync` — emit
fresh locks internally as part of their own work, but the locks they
emit are still derived: each one records the manifest tips at the moment
of capture. `rwv update --remote` records the members it advanced, a
member with no pin taking its upstream tip, and refuses before any member
moves a tip that would move a pin backward.

## `rwv.lock` is never a merge input

The consequence of "fully determined" is that the lock is never
something to merge. Two workspaces whose project repos both have lock
edits cannot have a meaningful merge conflict on the lock — there is
exactly one correct post-merge lock, and it is the snapshot of the
post-merge manifest tips. Asking an operator to resolve a lock conflict
by hand is asking them to compute a function whose inputs are already
known.

[`rwv land`](./sync-semantics.md) encodes this directly. The lock is
excluded from the project-repo merge phase (Phase 1') and regenerated
in Phase 3 from the now-aligned manifest tips. `rwv sync` records its lock
once every member has taken in: each pin merged three ways against the
merge base's lock, with every member it moved at its new tip. Any lock-only commit on
the source side becomes an empty patch on the target side and is
dropped automatically. The phase model is described in
[sync-semantics](./sync-semantics.md).

A merge rwv does not make — a `git merge` or `git pull` run by hand — merges
the lock through the lock merge driver, which `rwv init` and `rwv clone` set
up: pins merge three-way, and a merge whose pins it cannot settle is left
conflicted with both locks marked, so no side is taken silently. The VCS
layer owns the mechanism; [vcs-as-seam](./vcs-as-seam.md) documents it.

## Implications for everyday workflow

The "lock as derived" framing collapses a class of questions:

- *What happens if I hand-edit `rwv.lock`?* The next sync regenerates
  from manifest tips. Your edit either matches reality (no effect) or
  doesn't (gets overwritten). The file is not a place to author state.
- *What happens if I commit lock changes from two workweaves
  independently?* Each commit's lock-content is determined by that
  workweave's manifest tips at the time. When the second workweave
  syncs, the lock is recomputed; no conflict arises.
- *Why isn't there a "lock-only" diff to review?* The diff between two
  locks is fully implied by the diff between the manifest tips they
  pin. Reviewing the lock is reviewing a hash of the input set.
- *How do I roll the lock back to a prior state?* By checking out a
  prior project-repo commit. The lock at that commit is the lock that
  was derived from those manifest tips.

The lock is committed alongside the project-repo state for one purpose:
reproducibility. A collaborator who checks out the project repo at any
commit and runs `rwv update` gets exactly the manifest-tip set the lock
records. The lock is the *artifact*; manifest commits are the *source*.

## What the lock contains

The persisted shape is small: one entry per member repo, pinning it to
its 40-character commit SHA (never a tag or branch name, even when a tag
points at `HEAD`). Detailed schema lives in
[reference/formats](../../reference/formats.md); the conceptual point
here is that *every* field in the file is a function of state recorded
elsewhere.

A hand edit does not compound: the next `rwv lock` reads it as the
previous pin and moves it forward to where the checkout is, or refuses to
move it backward.

## Related joints

- [sync-semantics](./sync-semantics.md) — the phase model that depends
  on "lock is never a merge input."
- [shared-refs-drift](./shared-refs-drift.md) — the lock is one
  ingredient; on-disk drift between worktrees is another, and they
  interact.
- [verb-vs-composition](./verb-vs-composition.md) — why "rwv
  resolve-lock-conflicts" isn't a verb (the function has a closed-form
  answer).
