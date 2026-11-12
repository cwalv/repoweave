# Shared-refs drift

repoweave's workweave model is built on top of git worktrees. Worktrees
sharing a single repo's object store and refs DAG is what makes them
cheap: no re-clone, no re-fetch, branch creation is just a ref write.
It also means a branch ref checked out in one workspace can be moved from
outside it, leaving that workspace's index and working tree describing the
old commit. This joint defines the two flavors of that drift, the
safe-to-fix classification rwv applies to them, and the invariant that
makes refreshing it safe.

## How worktrees share state (and what they don't)

Git's worktree model:

- **Shared:** the object store (`.git/objects/`) and branch refs
  (`.git/refs/heads/`). A commit visible in one worktree is visible in
  any sibling.
- **Per-worktree:** the index (`HEAD`/`index`), the working tree files
  on disk, and any worktree-private bookkeeping.

The asymmetry is the whole reason worktrees are cheap. It is also the
source of drift whenever a branch ref checked out in one workspace is
moved without updating that workspace's index and working tree.

Git's porcelain refuses to do that: `git push`, `git fetch` and
`git branch -f` each refuse to move a branch that another worktree has
checked out. Plumbing does not refuse — `git update-ref` moves it — and
neither does a branch two worktrees share after a forced attach
(`git worktree add -f`, `git checkout --ignore-other-worktrees`), where a
commit in one moves the ref under the other. rwv takes none of these paths.
What else produces this drift has not been traced, so this joint names no
cause; it defines what rwv detects and repairs, whatever moved the ref.

Picture a workspace whose checked-out branch `main` is moved underneath
it, by a `git update-ref` run from another worktree of the same repo:

```
workspace B before: HEAD → main → commit X    (index + WT on disk = X)
ref main advances to commit Y outside B's working tree:
workspace B after:  HEAD → main → commit Y    (index + WT on disk still = X)
```

B's `HEAD` symbolic ref resolves to Y now (it followed `main`), but B's
*index file* and B's *working tree files* still reflect X. Nothing
about git's ref store is corrupt, yet B's index and working tree are now
drifted from its own `HEAD`.

`rwv` keeps workweaves from competing for a branch name by giving each
workweave's `owned` and `fork` members its own flat ephemeral branch
(`<project>--<workweave>`, Invariant I3 in
[clone-topology](./clone-topology.md)), and by leaving a member whose branch
another workspace has checked out detached at its pin (`rwv update` does this,
and `rwv status` names where the branch is checked out), so no rwv operation
moves a branch another workspace has checked out. The drift classes below
are about what happens when something outside rwv does.

## Two drift classes

The drift produces two distinct symptoms; rwv distinguishes them.

### Index drift

The on-disk *index* no longer matches HEAD. `git status` shows
"phantom" staged changes — entries that aren't actually anything the
user staged, but stale index entries left over from when HEAD was at a
prior commit.

Index drift is harmless functionally — file content on disk is
unchanged — but noisy. It leaks into every `git status`, every commit
prompt, every editor's git integration. The operator either learns to
ignore it (bad — masks real staged changes) or compulsively re-runs
`git reset` (bad — interrupts flow). Neither is acceptable as a
permanent state.

### Working-tree drift

On-disk *files* still reflect a prior commit. The index may agree or
disagree, but the user-visible file content is stale relative to the
worktree's effective HEAD.

This is worse than index drift: workers reading the files (test
runners, build tools, agents) see content that doesn't match the
commit they think they're on. Build outputs are computed from the wrong
inputs; tests run against the wrong sources; agents reason from stale
documentation.

In both cases the *cause* is the same — the checked-out branch ref moved
without updating the workspace's index and working tree. The symptom is
what differs.

## Safe-to-fix classification

rwv repairs drift in three places: `rwv doctor --fix`, which reports it
and refreshes it when asked; `rwv land`, which refreshes it in each
checkout, in either workspace, whose edits it judges, before judging
them; and the end of `rwv sync`, for each checkout the sync took in. The
interesting question is *what counts as drift*.

A checkout that differs from its HEAD is not evidence that its branch
moved. A deleted file, a file returned to an earlier version, a staged
`git revert --no-commit`, one file copied over another and a mode change
each leave only content some commit already holds, and a staged revert
leaves the index holding exactly the tree drift from the parent commit
would. Content alone cannot tell those edits from drift, and replacing
them would discard the operator's work. So rwv asks two questions, and
refreshes only what both answer:

- **Did the branch move under this checkout?** Every move a checkout
  makes itself — a commit, a reset, a rebase, a revert, a stash, an
  `update-ref` run in it — is recorded in its own `HEAD` reflog. A move
  made from another worktree is not. So a checkout whose newest reflog
  entry is a commit other than HEAD, and an ancestor of it, had its
  branch moved forward from that commit (the *moved-from tip*) by
  something else. Where the newest entry is HEAD, the branch moved
  backward or sideways, HEAD is detached, no reflog exists, or the
  checkout is stopped in a merge, rebase, cherry-pick, revert or bisect,
  nothing is shown to have moved under the checkout and every difference
  is an edit. A checkout left behind also holds the moved-from tip's index
  entry at every path the move changed and it did not edit; one whose index
  already holds HEAD's entry at such a path has taken the move, or part of
  it, in a way its reflog does not record (a hand repair, a lost entry), and
  every difference there is an edit too.
- **Does this path still hold what the moved-from tip held?** The
  classifier runs per path. A path the move changed whose index entry and
  file both still hold the moved-from tip's content is stale: both take
  HEAD's content, a file HEAD lacks removed. A path whose file already
  holds HEAD's content (compared by content, after git's line-ending and
  filter conversions) and whose index entry is the moved-from tip's is
  stale in the index alone, and its entry is reset. A path the move did
  not change is an edit. A path the move changed that something else
  changed too is left as it stands: neither side can be dropped without
  losing one.

A file whose stat data changed but whose content did not reads as
unchanged. A file git does not compare with its index entry
(marked skip-worktree with a file on disk, or assume-unchanged) is an edit.
Something on disk where the index lists nothing — at the path itself, or
a file or link where one of its leading directories would go, ignored or
not — is an edit and never overwritten.

**The write.** A refresh is git's own two-way merge from the moved-from tip
to that tip with the stale paths taken from HEAD. Every other path is one both
sides of the merge agree on, so git leaves its index entry and file as they
are. Git checks each stale path as it writes it and refuses the whole merge,
writing nothing, where a path no longer holds the moved-from tip's content or
something untracked is in the way: a file saved, or an entry staged, between
the reading and the write is never overwritten. The paths a refused merge
covered are left as they stand and treated as edits. Once a refresh has
written anything, rwv records in the checkout's own reflog that it now stands
at HEAD, so the refreshed checkout no longer reads as moved under: a staged
deletion, return to the old content or mode, or revert made after the refresh
is an edit like any other. Each refreshed file and each reset index entry is
reported.

`rwv doctor` reports the stale paths as `index-drift` and
`working-tree-drift` `safe-to-fix`, and everything else as `live-staged`
and `live-edits`; a checkout can carry both. `--fix` refreshes the
stale paths and leaves the rest; stale paths whose write git refused are
named as left in place, and the fix reports an error for them.

`rwv land` judges each checkout's edits on what the refresh leaves, so
drift is never refused as an edit and never listed under the choice that
commits edits. A landing that goes ahead refreshes every such checkout; a
stale path whose write git refused makes it refuse that checkout's edits,
the path included. A landing that refuses changes nothing, except in a
checkout whose own edits it refused: that checkout's drift is refreshed,
so the commit or stash the refusal offers there cannot commit the old tip's
content back over the move.

### Drift rwv cannot show

Where nothing shows that the branch moved under the checkout — the move went
unrecorded, the checkout already took part of it, it moved backward or
sideways — a checkout holding an old commit's content reads as edits, and
rwv refreshes none of it: `rwv land` refuses it as `edits` `dirty`, and
`rwv doctor` reports it `live-staged` and `live-edits`. If those differences
are not your work, stash them (`git stash push` in the checkout, the second
command the `edits` refusal prints): the checkout returns to HEAD, nothing is
committed, and the stash keeps every difference, your own edits included, to
take back or drop. Committing them instead records the move's reverse on the
branch.

## The shared invariant

> rwv replaces a path only where its branch is shown to have moved under the
> checkout, and only through a write git itself refuses unless the path still
> holds exactly what the moved-from tip held, a commit reachable from HEAD.
> Where the evidence is not conclusive, every difference is an edit. No
> uncommitted edit is ever replaced or deleted.

The classifier is one function that `rwv doctor --fix`, `rwv land` and
`rwv sync` share. Each path makes its own decision; the stale ones are
refreshed and the rest are left alone. A file the user touched but did
not change reads as unchanged; a file changed in any way reads as live.

## What this means for everyday workflow

Most users will rarely think about drift explicitly:

- If a checked-out branch ref moves underneath a workspace and `git status`
  shows phantom staged changes, run `rwv doctor --fix`, or let the next
  `rwv land` or `rwv sync` refresh it.
- Build tools that read files seeing stale content is a *real* symptom
  of working-tree drift, not a build-tool bug. The fix is the same.
- Running `git reset` in a drifted checkout records the move as the
  checkout's own. What it leaves — files at the old tip, index at HEAD —
  reads as an edit returning files to earlier content, and rwv leaves it
  for you; stash it if it is not your work (see
  [Drift rwv cannot show](#drift-rwv-cannot-show)).
- Tools that aggressively track file timestamps (some IDEs, some
  watchers) may notice ref movement and lag a moment catching up. That is
  unavoidable in the worktree model.

Drift is not a sign that your repository objects are corrupt: every
commit is intact, and only a workspace's index and files lag its ref.

## What rwv does *not* do

- rwv assigns each workweave its own `<project>--<workweave>` ephemeral
  branch (Invariant I3 in [clone-topology](./clone-topology.md)), but it
  cannot prevent external git commands or plumbing from moving a branch
  ref checked out in a workspace.
- rwv does not rewrite git's worktree model. When a ref moves underneath
  a checkout, this joint governs how `rwv` classifies and remediates the
  resulting drift.
- rwv does not silently discard live content. Ever. Live work
  classifies as live; the operator decides.
- rwv does not decide a path both the move and an edit changed. It is
  left as it stands, with the edits.

## Related joints

- [sync-semantics](./sync-semantics.md) — sync's Phase 3 disk
  reconciliation runs the same classifier as `rwv doctor --fix` post
  Phase-2.
- [workweave-hierarchy](./workweave-hierarchy.md) — the tree of
  workspaces that share each repo's refs, one branch per workspace.
- [clone-topology](./clone-topology.md) — the moved-from tip the
  classifier compares against is a commit in the shared object store.
  The tier-0 invariants are what make that store unambiguous.
