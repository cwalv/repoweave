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

rwv repairs drift in four places: `rwv doctor --fix`, which reports it
and refreshes it when asked; `rwv land`, which refreshes it in each
checkout, in either workspace, whose edits it judges, before judging
them; every other verb that refuses on a checkout's edits or writes to it
(`rwv lock`, `rwv update --remote`, `rwv switch`, `rwv sync`,
`rwv workweave delete` and the rest), which refreshes that checkout's drift
before judging what is left; and the end of `rwv sync`, for each checkout
the sync took in. `rwv doctor` without `--fix` only reports it, as drift. The interesting question is *what counts as drift*.

The project repo is judged the same way. Every verb that writes or commits
there (`rwv lock`, `rwv member` and its setters, `rwv branch set-history`,
`rwv update --remote`, `rwv sync`, `rwv land`, `rwv pull`,
`rwv cherry-pick`) refreshes the project repo's drift before it reads
`rwv.toml`, so its commit never carries the move's reverse of `rwv.toml` or
`rwv.lock`. Where the move changed a file you edited there too, the verb is
refused as `moved and edited` and commits nothing. `rwv doctor` reports the
project repo's drift where its branch moved under it, and both
`rwv doctor --fix` and its two flags act on it as on a member.

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
  backward or sideways, HEAD is detached, no reflog exists, the
  index lists no path at all, or the checkout is stopped in a merge,
  rebase, cherry-pick, revert or bisect, nothing is shown to have moved
  under the checkout and every difference is an edit. A checkout left behind also holds the moved-from tip's index
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
unchanged. A file git does not compare with its index entry (marked
skip-worktree with a file on disk, or assume-unchanged) is compared by
content instead: holding its entry's content it is read as any other file,
and otherwise it is an edit. Something on disk where the index lists
nothing — at the path itself, or a file or link where one of its leading
directories would go, ignored or not — is an edit and never overwritten,
unless it is a file the same move removes and that still holds the
moved-from tip's content: a move that turns a file into a directory, or a
directory into a file, is refreshed whole.

A submodule is refreshed in the index alone: where the move changed the
commit it records, its entry takes HEAD's, and its checkout stays at the
commit it stood at, as git's own checkout and merge leave it; `git submodule
update` brings it along. A populated submodule the move turns into a file or
link is an edit while anything is in its directory: git's merge would delete
that directory whole, files no commit holds included, as `git merge
--ff-only` does in a single repository. rwv refuses that instead.

**The write.** A refresh is git's own two-way merge from the moved-from tip
to that tip with the stale paths taken from HEAD. Every other path is one both
sides of the merge agree on, so git leaves its index entry and file as they
are. Immediately before writing, rwv reads the checkout again, and writes
nothing unless both readings find the same stale paths and the branch has not
moved again. Git then checks each stale path as it writes it and refuses the
merge where a path's index entry or tracked file no longer holds the
moved-from tip's content or something untracked is in the way. Two things git's
merge does not refuse over, an ignored file or directory where the move adds a
path and a tracked file deleted from disk, rwv checks itself just before git
starts. The files are written first, checked against a copy of the index,
and every index entry after them in one write, so a refresh stopped or refused
between the two leaves each path's index entry at the moved-from tip beside a
file holding HEAD's content, which reads as drift again and is finished by the
next verb. Where the branch moves again before that, a file both moves
changed holds the first move's content, which reads as `moved and edited`;
its merge brings it onto the newer tip. Only a refresh that leaves nothing stale is recorded in the checkout's
own reflog as standing at HEAD, so the refreshed checkout no longer reads as
moved under: a staged deletion, return to the old content or mode, or revert
made after the refresh is an edit like any other. Each refreshed file and each
reset index entry is reported.

A write rwv does not make leaves the stale paths as they stood, still drift,
with nothing recorded, and names them as *drift left in place* with the
reason: a path changed after it was read or something on disk is in the way,
the branch moved again, the index holds unresolved conflicts, or the write
failed (with git's own account). Running the verb again reads the checkout
afresh: a path you changed is then an edit, and the rest is refreshed. Where
the index holds unresolved conflicts, resolve them first (`git status` lists
them). Committing drift left in place would record the move's reverse.

What no reading can see is a change made in the instant between rwv's last
check and git's own write: an ignored file put where the move adds a path, a
stale tracked file deleted, or the branch moved again. Git's merge has no
option that refuses over an ignored file, and it reads a missing tracked file
as up to date. A write that fails partway, on a full disk for one, can leave
some files written while their index entries still hold the old tip's: those
read as drift in the index alone, and the next run finishes them.

Git's own rules apply to what the merge writes. In a sparse checkout a path
outside its patterns is written to the index alone, and a file outside them
that holds its index entry's content is removed, as any git checkout does;
nothing is lost, since a commit holds that content. A refreshed index entry
loses an assume-unchanged mark, and with `core.fileMode=false` an exec bit git
does not record is set as the commit says: git reads neither as a change, and
neither does rwv.

`rwv doctor` reports the stale paths as `index-drift` and
`working-tree-drift` `safe-to-fix`, and everything else as `live-staged`
and `live-edits`; a checkout can carry both. `--fix` refreshes the
stale paths and leaves the rest; drift left in place is named with its
reason and the route, and the fix reports an error for it. The end of
`rwv sync` warns the same way, naming `rwv doctor --fix`.

`rwv land` judges each checkout's edits on what the refresh leaves, so
drift is never refused as an edit and never listed under the choice that
commits edits. A landing that goes ahead refreshes every such checkout. A
landing that refuses changes nothing, except in a checkout whose own edits it
refused: that checkout's drift is refreshed, so the commit or stash the
refusal offers there cannot commit the old tip's content back over the move.
Drift left in place refuses the landing as `edits` `drift left in place`,
whose one choice is to land again; that checkout's other edits are listed
beside it with no choice of their own until the drift is refreshed.

Every other verb that judges a checkout's edits does the same: it refreshes
the checkout's drift, says what it replaced, and judges only what is left, so
where the branch moved forward the `git commit` and `git stash push` it offers
there carry your edits and not the move's reverse. A branch rewritten under
the checkout shows no move rwv can read (below). Drift it leaves in place refuses it as `edits`
`drift left in place`, whose one choice is `rwv doctor --fix`; the checkout's
other edits are judged once that has refreshed it.

A path is a byte string to git, and rwv reads every listing that way, so a
tracked name that is not UTF-8 does not stop the reading. Where such a name is
itself one of the paths that differ from HEAD, the drift cannot be read: every
verb refuses the checkout as `edits` `drift left in place`, naming why, and
offers `git stash push`, which keeps every difference, the move's reverse with
your edits, and leaves the checkout at HEAD; take back only your own files
from it. `rwv doctor` names the same and offers no commit.

### Drift rwv cannot show

Where nothing shows that the branch moved under the checkout — the move went
unrecorded, the checkout already took part of it, it moved backward or
sideways — a checkout holding an old commit's content reads as edits, and
rwv refreshes none of it: `rwv land` refuses it as `edits` `dirty`, and
`rwv doctor` reports it `live-staged` and `live-edits`. If those differences
are not your work, stash them (`git stash push` in the checkout, the second
command the `edits` refusal prints): the checkout returns to HEAD, nothing is
committed, and the stash keeps every difference, your own edits included.
Taking it back with `git stash pop` brings the move's reverse back too, as
unstaged changes beside your own; to keep only yours, take back just their
files (`git checkout stash@{0} -- <file>`) and drop the stash. Stash only what
rwv cannot show: drift `rwv doctor` reports `safe-to-fix` is refreshed by
`rwv doctor --fix`, and a stash made over it turns it into drift rwv cannot
show once popped. Committing them instead records the move's reverse on the
branch.

One case reads as drift although it is not. After a refresh by hand that
writes no reflog entry (`git checkout HEAD -- .`, `git restore --staged
--worktree .`, `git read-tree -m -u HEAD`), a staged return of every path the
move changed (a `git rm` of the one file it added, `git checkout <old> --` the
one file it changed, a staged revert of the move) leaves the index, files and
reflog exactly as a checkout never refreshed. rwv cannot tell it from drift,
and refreshes it: HEAD's content goes back over the staged return, and the
refresh names each file it replaced. What it replaces is the moved-from tip's
content, which that tip holds, so nothing is lost that history does not hold.
To keep such a return, commit it before rwv next reads the checkout. A path holding
content other than the old tip's is an edit and is never replaced.

A file the move changed that you edited too is neither. rwv does not
refresh that checkout at all, because a partial refresh would erase what
shows the move: it lists such files as `moved and edited`. So does
something standing where the move puts a path of its own: a directory or
an untracked file at a file's name, or a link or file where its directory
goes. Its first choice is to move that out of the way.

`rwv doctor --fix --merge-edited-drift` hands the edits to git. It keeps
the working copy and index as a backup, points the checkout's HEAD at the
tip its files hold, so that only your edits differ from HEAD, and runs
`git checkout -m` onto the branch. Git merges each edit onto the moved tip
as it merges local changes on any checkout, and refreshes the move's other
files with it. A file it does not resolve is left conflicted, both versions
in it (`ours` is the moved tip, `theirs` your edit), for you to resolve and
mark resolved. Git honors the moved tip's attributes: line endings,
filters, `-merge`, `binary`, `merge=union`. A file declared derived takes
the moved tip's content, and your edit to it stays in the backup. Links,
modes, directories that replace files, and files the move renames are all
git's to merge.

`rwv doctor --fix --set-aside-edited-drift` is the other way out. It points
HEAD at the tip the files hold in the same way, has git stash the edits to
the listed files, then checks the branch out with a plain `git checkout`.
The stash is based on the tip your files held, so it holds your edits and
nothing of the move, and its message names the branch it was set aside from.
The `git stash apply <stash>` that merges them back onto the moved tip is
printed the moment the stash is made, before the checkout is tried, and again
in the report; where an earlier run's stash is kept too, the report prints
each, oldest first. A stash set aside from another branch, or against another
tip, is never named here.

Where git refuses a step, such as staged changes, an untracked file in the
way, an intent-to-add entry under the stash, or an index lock a killed git
process left, rwv prints git's own words and leaves the checkout `set back`:
HEAD at the tip its files hold. A refusal git makes before it writes leaves
the files as they stood; one during its write, a permission denied part way
for one, can leave some files moved, and the backup taken before the merge
holds the edits either way. Every verb names that state. Its choices are the
step git asks for (unstage, remove the lock, free the branch from the other
checkout that holds it, create a deleted branch again, or clear what git
quotes), then the merge or the set-aside again, which carries the edits onto
wherever the branch is by then. A rerun first repeats the first run's hold
over what no backup keeps, against the branch as it is then: an ignored file
where the branch now puts a file of its own is to be moved out of the way
first. `rwv update`, `rwv switch` and `rwv status` name the state alone. Since HEAD names what the files hold there, a commit or stash you make
in that state records your edits alone.

Neither flag acts while a listed file is one git does not compare with
what it records: an ignored file, or one marked assume-unchanged or
skip-worktree. No backup would keep it, so such a checkout is refused with
the command that clears each mark, or tracks the ignored file, as its
choices. Each flag acts only on checkouts of the workspace it runs in. For
another workspace's checkout it prints that workspace's `rwv -C` line.

## The shared invariant

> rwv replaces a path only where its branch is shown to have moved under the
> checkout, two readings agree that the path still holds exactly what the
> moved-from tip held, a commit reachable from HEAD, and git's own write
> refuses unless it still does. Where the evidence is not conclusive, every
> difference is an edit. No content that the checkout's history does not
> hold is replaced or deleted, save a change made in the instant before git
> writes. A staged return of a whole move after a refresh by hand reads as
> drift (above): it is refreshed, and what that replaces is the moved-from
> tip's content, which history holds.

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
