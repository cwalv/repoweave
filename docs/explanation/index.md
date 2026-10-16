# Explanation

**Perspectives** introduce repoweave through three common workflows — multi-repo workspaces, monorepo-style cross-repo editing, and parallel agent isolation. **Concepts** and **Design notes** explain the invariants and architectural boundaries that hold the model together.

## Perspectives

- [Multi-repo workspaces](./lenses/workspace.md) — the version-controlled project repository as the answer to "what do I need to work on X?"
- [Monorepo speed without migration](./lenses/monorepo.md) — cross-repo builds, workweaves, and lock pins without collapsing repositories
- [Parallel agent orchestration](./lenses/agent.md) — isolated workweaves, `rwv prime`, `rwv explain`, and deterministic landing for AI coding sessions

## Concepts

- [Pyramid of stability](./joints/pyramid-of-stability.md) — how workweaves, branch tracking (`rwv.toml`), and pinned channels (`rwv.lock`) layer stability
- [Clone topology](./joints/clone-topology.md) — Tier-0 invariants: one canonical store per member repo, linked worktrees, and disjoint branches
- [Branch model](./joints/branches.md) — what a branch is for in the project repo, the canonical member store, and a workweave checkout
- [Weave root as presentation](./joints/weave-root.md) — why the weave root is a derived view of the active project rather than a git repository
- [Symlinks as structure](./joints/symlinks-as-structure.md) — how weave-root symlinks surface ecosystem workspace files and shared directories
- [Workweave hierarchy](./joints/workweave-hierarchy.md) — parent-child relationships between workweaves and flat `<project>--<workweave>` branch naming
- [Workweave lifecycle](./joints/workweave-lifecycle.md) — creating, syncing, landing (`sync-to --retire`), and deleting workweaves
- [Lock-as-derived](./joints/lock-as-derived.md) — why `rwv.lock` is computed from member HEADs on mutation and authoritative only at `fetch`
- [Sync semantics](./joints/sync-semantics.md) — the three-phase replay, savepoint, and op-state recovery model behind `rwv sync` and `rwv sync-to`
- [Shared-refs drift](./joints/shared-refs-drift.md) — what happens when a checked-out branch ref moves underneath a worktree and how `rwv` classifies drift
- [File ownership](./joints/file-ownership.md) — Axis-1 (surfaced symlink) vs. Axis-2 (owned or hybrid content) and per-key ownership modes
- [Destructive operations](./destructive-operations.md) — why every state-discarding path requires a narrow, single-check override flag

## Design notes

- [Verb vs composition](./joints/verb-vs-composition.md) — why `rwv` provides workspace verbs and leaves per-repo git porcelain to `git` and `gita`
- [Verb vs vocabulary](./joints/verb-vs-vocabulary.md) — why `sync` and `sync-to` are separate directional verbs rather than flags on one command
- [VCS as seam](./joints/vcs-as-seam.md) — the internal `Vcs` trait boundary that isolates git command mechanics from workspace logic
- [Plugin boundary](./joints/plugin-boundary.md) — when a feature belongs in `rwv` core vs. an external `rwv-<name>` plugin executable
