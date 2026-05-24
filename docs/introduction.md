# Introduction

**repoweave** coordinates work across multiple repositories that share a project. It gives you:

- **Monorepo ergonomics** — cross-repo imports resolve locally; no version-bump dance during development.
- **Polyrepo sovereignty** — repos stay independent, separately ownable, with normal git history.
- **Reproducibility** — a single `rwv.lock` pins every repo to an exact revision; `rwv fetch` reproduces the whole world from one URL.
- **Isolation on demand** — workweaves give you parallel, sandboxed copies of an entire workspace without disturbing the primary weave.

The fundamental unit is the **project**: a small repo at `projects/<name>/` carrying a manifest (`rwv.yaml`) of which repos belong to the project, a lock (`rwv.lock`) pinning their revisions, and any cross-cutting docs. `rwv fetch <project-url>` clones the project repo and every repo it lists; one command, complete environment.

## Who is this for?

You'll get the most out of repoweave if any of these describe your setup:

- A product that spans **two or more repositories** that have to work together.
- A **shared internal library** consumed by other repos in your project.
- An ambition to do **agent-driven refactoring** across repos with safe blast radius.
- An existing **polyrepo setup** suffering from "wrong-version clone" ambient confusion.

You can use repoweave with a single repo, but most of the value shows up at N ≥ 2.

## Where to start

Three doors in, depending on what shape "the problem" has for you.

### "I have a folder full of clones and no idea which ones are right"

Start with the [workspace lens](./explanation/lenses/workspace.md). It explains how repoweave moves the source of truth for a development environment out of human memory and into a versioned project repo — and what that gets you.

Then walk through the [tutorial](./tutorial.md): fetch a project, get the latest, make a change, lock the state.

### "I want monorepo speed without a monorepo"

Start with the [monorepo lens](./explanation/lenses/monorepo.md). It pitches the zero-version-change workflow, workweaves as isolation-without-silos, `rwv sync` as bring-it-home, and the pyramid of stability for canonical cross-repo tips.

Then read [create a feature workweave](./how-to/create-feature-workweave.md) and [bring workweave work home](./how-to/bring-workweave-work-home.md) for the operational patterns.

### "I want agents to drive my multi-repo project safely"

Start with the [agent lens](./explanation/lenses/agent.md). It covers the landed agent surface (`rwv prime`, `rwv explain`, the `--json` envelope, NDJSON, roles as an allow-list) and the recommended workflow pattern: give the agent its own workweave, sync the result home with verification.

Then read [hand task to agent](./how-to/hand-task-to-agent.md) for the harness recipe.

## How the docs are organized

The docs follow the [Diátaxis](https://diataxis.fr) framework — four quadrants, each with one job:

| | What you want | Where to go |
|---|---|---|
| **Tutorial** | A first walkthrough | [tutorial.md](./tutorial.md) — one path, no choice points |
| **How-to** | A focused recipe | [how-to/](./how-to/index.md) — task-shaped, terse |
| **Explanation** | The "why" and the model | [explanation/lenses/](./explanation/lenses/) (motivational) + [explanation/joints/](./explanation/joints/) (cross-cutting) |
| **Reference** | Lookup-shaped facts | [reference/](./reference/cli.md) — verbs, file formats, roles, glossary, integrations |

The **lenses** are three ways of looking at repoweave, pitched to different audiences. The **joints** are cross-cutting concepts (lock-as-derived, sync semantics, workweave hierarchy, ...) that all lenses reference — they define the vocabulary.

If you're building or maintaining `rwv` itself, see [contributing/](./contributing/developing.md).

## A note on terminology

A **weave** is a repoweave workspace — a directory containing repos and projects. A **workweave** is a worktree-derived sandbox of a weave. A **project** is the small coordination repo under `projects/`. A **manifest repo** is a repo listed in a project's `rwv.yaml`.

For the full vocabulary, see [reference/glossary](./reference/glossary.md).
