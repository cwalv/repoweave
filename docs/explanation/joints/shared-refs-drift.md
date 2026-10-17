# Shared-refs drift

repoweave's workweave model is built on top of git worktrees. Worktrees
sharing a single repo's object store and refs DAG is what makes them
cheap: no re-clone, no re-fetch, branch creation is just a ref write.
It also means a branch ref checked out in one workspace can be moved from
outside it, leaving that workspace's index and working tree describing the
old commit. This joint defines the two flavors of that drift, the
safe-to-fix classification rwv applies to them, and the invariant that
makes auto-fix safe.

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
workweave its own flat ephemeral branch (`<project>--<workweave>`,
Invariant I3 in [clone-topology](./clone-topology.md)), so no rwv operation
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

rwv treats drift as a first-class problem and offers `rwv doctor` /
`rwv doctor --fix` for it (and applies the same fix automatically at
the end of `rwv sync`). The interesting question is *what counts as
safe to fix*.

Auto-fixing index drift by stomping the index, or auto-fixing
working-tree drift by checking out HEAD's tree, is fine *unless* the
content rwv would overwrite is actually live work — staged changes the
user authored, or edits to working-tree files the user is in the
middle of writing.

rwv distinguishes the two cases at the blob level. For every file or
index entry rwv would touch as part of fixing drift:

- **Safe class.** The content matches an *already-committed blob*
  reachable in the object DAG. By construction, no information is
  lost by replacing it with HEAD's blob — every byte already exists in
  some commit. This is drift in the strict sense: stale stable
  content, not live work.
- **Live class.** The content does not match any committed blob.
  Could be a staged change the user typed, a mid-edit save in the
  working tree, an untracked file rwv shouldn't touch. Auto-fixing
  here could silently discard user work.

`rwv doctor` reports both classes; `rwv doctor --fix` and the
post-sync auto-refresh only touch the *safe* class. The live class is
surfaced as a warning so the operator can resolve it intentionally —
no silent overwrite, ever.

## The shared invariant

The safety property is worth stating directly because everything else
in this joint hangs off it:

> `rwv doctor --fix` and sync's post-refresh **only** replace content
> that exactly matches an already-committed blob in the object DAG. No
> user work is ever silently discarded.

This is what makes auto-fix safe to apply mechanically. The classifier
runs per-file (or per-index-entry); each item makes its own
safe/live decision; the safe ones are remediated and the live ones are
left alone.

The invariant is checked at the blob-content level, not at file-path
or timestamp level. That matters: a file the user touched but didn't
actually change (a no-op edit and save) classifies as safe because its
blob already exists. A file modified in any meaningful way classifies
as live and is left alone.

## What this means for everyday workflow

Most users will rarely think about drift explicitly:

- If a checked-out branch ref moves underneath a workspace and `git status`
  shows phantom staged changes, run `rwv doctor --fix` (or let the next
  `rwv sync` refresh it).
- Build tools that read files seeing stale content is a *real* symptom
  of working-tree drift, not a build-tool bug. The fix is the same.
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

## Related joints

- [sync-semantics](./sync-semantics.md) — sync's Phase 3 disk
  reconciliation runs the same auto-fix as `rwv doctor --fix` post
  Phase-2.
- [workweave-hierarchy](./workweave-hierarchy.md) — the tree of
  workspaces that share each repo's refs, one branch per workspace.
- [clone-topology](./clone-topology.md) — the safe/live classifier
  asks "is this blob in the object DAG?" The tier-0 invariants are what
  make "the" object DAG unambiguous.
