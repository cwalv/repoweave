# Explanation

**Perspectives** introduce repoweave through three common workflows — multi-repo workspaces, monorepo-style cross-repo editing, and parallel agent isolation. **Concepts** and **Design notes** explain the invariants and architectural boundaries that hold the model together.

## Perspectives

- [Multi-repo workspaces](./lenses/workspace.md) — the version-controlled project repository as the answer to "what do I need to work on X?"
- [Monorepo speed without migration](./lenses/monorepo.md) — cross-repo builds, workweaves, and lock pins without collapsing repositories
- [Parallel agent orchestration](./lenses/agent.md) — isolated workweaves, `rwv prime`, `rwv explain`, and deterministic landing for AI coding sessions

## Concepts

### Layout & files

- [Clone topology](./joints/clone-topology.md) — Tier-0 invariants: one canonical store per member repo, linked worktrees, and disjoint branches
- [Weave root as presentation](./joints/weave-root.md) — why the weave root is a derived view of the active project rather than a git repository
- [Symlinks as structure](./joints/symlinks-as-structure.md) — how weave-root symlinks surface ecosystem workspace files and shared directories
- [File ownership](./joints/file-ownership.md) — Axis-1 (surfaced symlink) vs. Axis-2 (owned or hybrid content) and per-key ownership modes
- [Destructive operations](./destructive-operations.md) — why every state-discarding path requires a narrow, single-check override flag

### Workweaves & branches

- [Workweave hierarchy](./joints/workweave-hierarchy.md) — the tree of provenance between workweaves, flat `<project>--<workweave>` branch naming, and the records that say where each branch lands
- [Workweave lifecycle](./joints/workweave-lifecycle.md) — creating, syncing, landing (`land --retire`), and deleting workweaves
- [Records and modes](./joints/branch-records.md) — how a weave branch is used, written once as its record and published with it
- [Alignment](./joints/alignment.md) — where members sit for the checked-out branch, and what is reported when they do not
- [The deletion rule](./joints/deletion-rule.md) — what deleting a branch, a workweave or a store keeps, refuses and asks first
- [Shared-refs drift](./joints/shared-refs-drift.md) — what happens when a checked-out branch ref moves underneath a worktree and how `rwv` classifies drift

### Lock & sync

- [Lock-as-derived](./joints/lock-as-derived.md) — why `rwv.lock` records the commit each member is pinned at on its branch, is written by verbs from member HEADs and computed across a merge, and is never edited or conflict-resolved by hand
- [Sync semantics](./joints/sync-semantics.md) — taking a target's work into a branch with `rwv sync`
- [History and lineage](./joints/history-and-lineage.md) — the history setting, rebasing and stacks, and the lineage rule
- [Landing](./joints/landing.md) — `rwv land`: compose in the source, then move the target
- [Stops and abort](./joints/stops-and-abort.md) — savepoints, `--continue`, and what `rwv abort` keeps

## Design notes

- [Verb vs composition](./joints/verb-vs-composition.md) — why `rwv` provides workspace verbs and leaves per-repo git porcelain to `git` and `gita`
- [Verb vs vocabulary](./joints/verb-vs-vocabulary.md) — why each verb takes its name from git's vocabulary (`clone`, `fetch`, `pull`, `push`) or a package manager's (`update`, `lock`), chosen by what the verb does
- [VCS as seam](./joints/vcs-as-seam.md) — the internal `Vcs` trait boundary that isolates git command mechanics from workspace logic
- [Plugin boundary](./joints/plugin-boundary.md) — when a feature belongs in `rwv` core vs. an external `rwv-<name>` plugin executable
