# Workweave hierarchy

A workweave is a worktree-derived copy of a workspace: its own
constituent-repo branches (for worktree-materialized repos), its own
ecosystem files, its own tool state.
Workweaves are how repoweave gives operators (and agents) parallel,
isolated cross-repo work without disturbing the primary weave.

Workweaves are not flat. A workweave can be created from inside another
workweave; the result is a tree. This joint explains the tree model, how
flow direction works, and what the tool tracks versus what the operator
must.

## The tree model

The root of the tree is always the primary weave. From there:

```
primary
├── workweave A                ← created from primary
│   └── workweave A-child      ← created from inside A
└── workweave B                ← created from primary
```

Each non-root node was *forked from* a single parent — the workspace
that was CWD when `rwv workweave create` ran. That parent is recorded;
see "Parent tracking" below.

The model is intentionally a tree, not a DAG. A workweave has exactly
one parent. There is no "merge two workweaves into one third one" at the
tool level — that would be a graph operation, and rwv does not own it.
Two sibling workweaves coordinate by syncing through their common
ancestor.

## Flow direction

Work flows along edges of the tree in a specific direction. Creation
runs down the edges (solid); landing runs back up them (dotted):

```mermaid
flowchart TD
    P[primary weave]
    A[workweave A]
    AC[workweave A-child]
    B[workweave B]

    P -->|create| A
    P -->|create| B
    A -->|create| AC

    A -.->|rwv sync-to| P
    B -.->|rwv sync-to| P
    AC -.->|rwv sync-to| A
```

- **Creation direction** (primary → workweave). `rwv workweave create`
  forks from the current workspace. The child starts pinned to the
  parent's lock.
- **Landing direction** (workweave → parent). `rwv sync-to` pushes the
  child's committed work *up* toward the root. Each `rwv sync-to`
  advances exactly one edge (one hop, see below). `rwv sync` goes the
  other direction: CWD absorbs the named source.

The two directions are not symmetric — see
[sync-semantics](./sync-semantics.md) for the full direction-pair
contract. The flow-direction discipline is *don't skip edges and don't
cross edges*: a child pushes to its parent, not to a sibling or to a
grandparent.

## Parent tracking is tool behavior

The `.rwv-workweave` marker file at every workweave root carries a
`parent` field that records the workspace the workweave was created
from. The shape:

```yaml
primary: /home/user/work
project: web-app
parent: /home/user/work/.workweaves/web-app--feat   # parent workweave
```

`parent` is `primary` when the workweave was created from the primary
weave, and the parent-workweave path when the workweave was created from
inside another workweave. Legacy markers written before the field
existed parse cleanly — the read path backfills `parent` to `primary`
so callers always see a value.

Tool-tracked parentage is what makes two operations safe to run without
an explicit target:

1. **Bare `rwv sync-to` has a target.** Running `rwv sync-to` with no
   argument pushes to the recorded parent. From a child workweave that
   means the parent workweave, not the primary. `rwv sync` (CWD absorbs
   a source) always requires an explicit argument — it has no auto-target.
2. **`rwv sync-to --retire` knows what to retire to.** The retire flag
   pushes to the recorded parent, verifies convergence, and deletes the
   workweave on success.

Anchored by the `.rwv-workweave` parent field plumbed through
`src/workspace.rs::WorkweaveMarker` and consumed by
`src/sync.rs::retire_workweave_after_sync`.

## One hop, not transitive

Bare `rwv sync-to` follows the parent edge — one hop. It does not chase
the tree up to the primary on its own. From a child of a workweave,
reaching primary takes two sync-to invocations:

```bash
cd .workweaves/web-app--feat-child
rwv sync-to               # → parent workweave (web-app--feat)
cd ../web-app--feat
rwv sync-to               # → primary
```

Or one explicit sync-to that names the target:

```bash
cd .workweaves/web-app--feat-child
rwv sync-to primary
```

The one-hop default is intentional. Transitive sync-to would silently
land the child's work in a workspace it was never forked from, bypassing
the intermediate review step. Explicit-target sync-to is always available
when the operator means it.

## What rwv tracks vs. what is discipline

Even with parent tracking, parts of the workweave model remain
operator-managed. Worth being explicit:

| Behavior | Tool support | Discipline |
|---|---|---|
| Recording the parent edge | Yes (`.rwv-workweave`) | — |
| Bare `rwv sync-to` auto-targets the parent | Yes | — |
| `rwv sync-to --retire` lands one hop and deletes on success | Yes | — |
| Sibling-to-sibling coordination | No | Sync-to via shared parent |
| Cross-picking commits across branches | No | Use `git cherry-pick` directly |
| Promotion between branches in the project repo | No | Ordinary git workflow |
| Detecting unusual flow (skip-rebase, sibling sync) | Limited | Operator awareness |

The boundary is consistent with the principle in
[verb-vs-composition](./verb-vs-composition.md): rwv encodes
coordination the manifest knows and the VCS doesn't. The parent edge is
manifest-adjacent (recorded in workspace marker state); siblings and
cherry-picks are pure git operations on branches the operator can name.

## Naming and paths

Workweaves are co-located under `.workweaves/` at the parent workspace
root, named `<project>--<workweave-name>`:

```
primary/
├── .workweaves/
│   ├── web-app--feat/
│   │   └── .workweaves/
│   │       └── web-app--feat-child/
│   └── web-app--review-pr-42/
```

The `<project>` prefix is part of the directory name (not just metadata)
so multiple projects can host workweaves named identically without
collision.

A workweave's `.workweaves/` directory is its own children's parent;
the tree is naturally reflected in the filesystem path. This means
`find` and editor file pickers see the tree without any special tooling.

## Ephemeral branch names and the git worktree constraint

Each workweave runs on a per-workweave ephemeral branch name — e.g.
`foundations--fo-pte54.5/main` rather than `main`. This is what lets
`rwv sync-to` work cleanly as a local-to-local primitive.

Git imposes a constraint: only one worktree can have a given branch
checked out at a time. If two workweaves both tracked `main`, checking
out the second would require the first to detach its HEAD, and the
sequence `sync-to primary` would be trying to push into a branch another
worktree owns. The analogous single-repo operation — `git push
<local-path> feature:main` — is similarly awkward when `main` is already
checked out at the destination.

rwv sidesteps this entirely. Because primary's `main` and a workweave's
`foundations--fo-pte54.5/main` are different branch names, no two
workweaves compete for the same named branch. `rwv sync-to` can push
directly into primary's `main` without any detach-or-stash dance. The
ephemeral naming scheme is not just bookkeeping — it is what makes
`sync-to` a clean primitive where the single-repo git analog is not.

## Related joints

- [workweave-lifecycle](./workweave-lifecycle.md) — what happens at each
  stage of a workweave's existence: creation flags, working state, the
  retire contract, and deletion semantics.
- [sync-semantics](./sync-semantics.md) — what happens when a sync runs
  between two workspaces on the tree; the full direction-pair contract.
- [pyramid-of-stability](./pyramid-of-stability.md) — the project-repo
  side of the canonical-tip story; orthogonal to workweave hierarchy.
- [clone-topology](./clone-topology.md) — the ephemeral-branch
  convention is invariant I3 in the tier-0 topology spec; the
  hierarchy's worktree-sharing model is the physical artifact those
  invariants govern. Note the I2/I3 carve-out for `role: reference`
  repos, which are materialized as symlinks to the canonical store
  rather than linked worktrees and therefore carry no ephemeral branch.
- [verb-vs-composition](./verb-vs-composition.md) — why "sync through
  sibling" / "cross-pick across workweaves" aren't rwv verbs.
