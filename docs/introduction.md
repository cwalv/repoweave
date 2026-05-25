# Introduction

**repoweave** coordinates work across multiple repos that share a project. It gives you:

- **A committable manifest and lock.** `rwv.yaml` says which repos belong; `rwv.lock` pins every revision. `sha256sum rwv.lock` is the project fingerprint — the multi-repo equivalent of `git rev-parse HEAD`.
- **One-command reproduction.** `rwv fetch <url>` clones the project and every repo it lists, generates ecosystem workspace files where they apply, and runs install commands. New machine to working environment in one step.
- **A home for cross-cutting artifacts.** Operational scripts, k8s manifests, demos, design notes, decision records — anything that doesn't belong to a single library — lives in the project repo without contaminating any library's commit history.
- **Isolated parallel work via workweaves.** Worktree-derived sandboxed copies of the whole workspace. Use them for feature branches, PR review, or agent sandboxes; the primary weave stays undisturbed.
- **Structured agent context.** `rwv prime` and `rwv explain --json` give AI harnesses a machine-readable view of the workspace, with roles (`owned` / `fork` / `dependency` / `reference`) acting as a read-only allow-list.
- **Local cross-repo imports where languages line up.** Generated workspace files (`Cargo.toml [workspace]`, `go.work`, `package.json` workspaces, `pyproject.toml [tool.uv.workspace]`, ...) mean a change in a shared library is immediately visible to its consumer — no publish step during development.

The repos themselves stay independent — separately ownable, with normal git history. repoweave is a coordination layer, not a monorepo migration. You still commit and push per repo; the project lock and ecosystem wiring make that feel less expensive than it usually does.

The fundamental unit is the **project**: a small repo at `projects/<name>/` carrying a manifest (`rwv.yaml`) of which repos belong to the project, a lock (`rwv.lock`) pinning their revisions, and any cross-cutting docs. `rwv fetch <project-url>` clones the project repo and every repo it lists; one command, complete environment.

## Who is this for?

You'll get the most out of repoweave if any of these describe your setup:

- A product that spans **two or more repos** that have to work together.
- A **single product whose source build needs sibling clones** — your README says "clone these seven repos next to this one" but nothing pins the revisions.
- A **shared internal library** consumed by other repos in your project.
- An ambition to do **agent-driven refactoring** across repos with safe blast radius.
- An existing **polyrepo setup** suffering from "wrong-version clone" ambient confusion.
- **Cross-cutting artifacts** (scripts, manifests, demos, decision records) that have nowhere obvious to live.

The unit is *repos your dev environment depends on*, not *repos under your project directory*. A project that looks like one repo from the outside can still be a strong fit if its build pulls in several siblings.

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

| | What you want | Where to go |
|---|---|---|
| **Tutorial** | A first walkthrough | [tutorial.md](./tutorial.md) — one path, no choice points |
| **How-to** | A focused recipe | [how-to/](./how-to/index.md) — task-shaped, terse |
| **Explanation** | The "why" and the model | [explanation/](./explanation/lenses/workspace.md) — motivational pages plus cross-cutting concepts |
| **Reference** | Lookup-shaped facts | [reference/](./reference/cli.md) — verbs, file formats, roles, glossary, integrations |

If you're building or maintaining `rwv` itself, see [contributing/](./contributing/developing.md).

## A note on terminology

A **weave** is a repoweave workspace — a directory containing repos and projects. A **workweave** is a worktree-derived sandbox of a weave. A **project** is the small coordination repo under `projects/`. A **manifest repo** is a repo listed in a project's `rwv.yaml`.

For the full vocabulary, see [reference/glossary](./reference/glossary.md).
