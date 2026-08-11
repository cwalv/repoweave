# Explanation — lenses and joints

The explanation section is divided into two complementary kinds of page.

**Lenses** are motivational entry points organised by reader persona. Each lens asks "what is the most useful frame for someone arriving with *this* particular need?" A lens is not exhaustive — it is a curated path into the system that highlights the concepts most relevant to one kind of work. The three lenses (workspace, monorepo, agent) cover the three most common reasons someone reaches for repoweave, but they share the same underlying model; reading one lens does not exclude the others.

**Joints** are cross-cutting design seams: the decisions and invariants that hold the system together regardless of which lens you arrived through. A joint answers "why is the system shaped this way?" and "what would break if this invariant were violated?" Joints are written for readers who need to understand the model deeply — integrations authors, contributors, or anyone debugging a surprising behaviour. Unlike lenses, joints are not organised by persona; they are organised by the seam they describe.

If you are new, start with the lens that matches your situation (linked below); move to the joints when you hit a behaviour you want to understand at a deeper level.

## Lenses

- [Workspace lens](./lenses/workspace.md) — the version-controlled project repo as the answer to "what do I need to work on X?"
- [Monorepo lens](./lenses/monorepo.md) — monorepo speed without a monorepo migration
- [Agent lens](./lenses/agent.md) — agent-safe multi-repo orchestration via `rwv prime`, `rwv explain`, and workweave isolation

## Joints

- [Pyramid of stability](./joints/pyramid-of-stability.md)
- [Clone topology](./joints/clone-topology.md)
- [Symlinks as structure](./joints/symlinks-as-structure.md)
- [Workweave hierarchy](./joints/workweave-hierarchy.md)
- [Lock-as-derived](./joints/lock-as-derived.md)
- [Sync semantics](./joints/sync-semantics.md)
- [Shared-refs drift](./joints/shared-refs-drift.md)
- [Verb vs composition](./joints/verb-vs-composition.md)
- [Verb vs vocabulary](./joints/verb-vs-vocabulary.md)
- [VCS as seam](./joints/vcs-as-seam.md)
- [File ownership](./joints/file-ownership.md)
- [Plugin boundary](./joints/plugin-boundary.md)
