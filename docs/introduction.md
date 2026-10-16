# Introduction

**repoweave** coordinates work across multiple repos that share a project. It gives you:

- **A committable manifest and lock.** `rwv.toml` says which repos belong; `rwv.lock` pins every revision. `sha256sum rwv.lock` is the project fingerprint — the multi-repo equivalent of `git rev-parse HEAD`.
- **One command reproduces the workspace.** `rwv fetch <url>` clones the project and every repo it lists, generates ecosystem workspace files where they apply, and runs install commands. For toolchain pins, env activation, or full OS-level reproduction, drop a `.mise.toml` / `.envrc` / `devcontainer.json` into the project repo (they're cross-cutting artifacts) — `rwv fetch` carries them with everything else. See [adjacent-tools](./adjacent-tools.md).
- **A natural home for cross-cutting artifacts that don't quite fit anywhere else.** Operational scripts, k8s manifests, ADRs, demos, release notes, devcontainer configs, `.mise.toml` toolchain pins, Nix flakes — they live in the project repo without contaminating any single library's history, and they come along on every `rwv fetch`.
- **Isolated parallel work via workweaves.** Worktree-derived sandboxed copies of the whole workspace, with automatic copy-on-write file sharing where the filesystem supports reflinks. Use them for feature branches, PR review, or agent sandboxes; the primary weave stays undisturbed.
- **A bounded surface for automation.** `rwv prime` advertises the workspace; `rwv explain <verb>` returns a markdown bundle with the verb's JSON Schema embedded; `rwv setup claude` registers both `SessionStart`/`PreCompact` context hooks and `WorktreeCreate`/`WorktreeRemove` lifecycle hooks so `claude --worktree` creates a full multi-repo workweave; roles (`owned` / `fork` / `dependency` / `reference`) surface repo change-resistance (`reference` repos have read-only tracked files on disk; `dependency` repos are writable but excluded from `rwv push` by default); workweaves isolate the blast radius. Together they give an agent harness everything it needs to drive the workspace without scraping help text.
- **Local cross-repo imports where languages line up.** Generated workspace files (`Cargo.toml [workspace]`, `go.work`, `package.json` workspaces, `pyproject.toml [tool.uv.workspace]`, ...) mean a change in a shared library is immediately visible to its consumer — no publish step during development.
- **Built-in failure recovery and drift repair.** Every `rwv sync` and `rwv sync-to` records savepoints under `refs/rwv/pre-op/<id>/` so `rwv abort` can restore every touched repo to its exact pre-operation state; `rwv doctor` detects and repairs workspace drift (`--fix`); destructive overrides require explicit, narrowly scoped flags (`--discard-local-commits`, `--allow-stale-lock`).
- **Near-zero exit cost.** Member repos stay ordinary git clones at `<registry>/<owner>/<repo>/` with untouched history, while `rwv.lock` and ecosystem workspace files live in `projects/<name>/` and surface at the weave root via symlinks. Deleting `projects/`, `.rwv-active`, and the root symlinks leaves plain standalone git clones behind — you can walk away from `rwv` at any time without migrating your repositories back out.

The repos themselves stay independent — separately ownable, with normal git history. repoweave is a coordination layer, not a monorepo migration. You still commit and push per repo; the project lock and ecosystem wiring make that feel less expensive than it usually does.

The fundamental unit is the **project**: a small repo at `projects/<name>/` carrying a manifest (`rwv.toml`) of which repos belong to the project, a lock (`rwv.lock`) pinning their revisions, and any cross-cutting docs. `rwv fetch <project-url>` clones the project repo and every repo it lists; one command, reproducible workspace.

## Install

**Prerequisites:** `git` on `$PATH`. On Windows, enable **Developer Mode** (or run from an elevated shell) so `rwv` can create workspace symlinks.

- **Quick install (Linux/macOS):** `curl -fsSL https://cwalv.github.io/repoweave/install.sh | sh`
- **Pre-built binaries:** download from [GitHub Releases](https://github.com/cwalv/repoweave/releases) (Linux x86_64/aarch64, macOS x86_64/aarch64, Windows x86_64)
- **Cargo:** `cargo install repoweave`
- **pip / uvx:** `pip install repoweave` (or `uvx repoweave`)

## Core vocabulary

Five terms recur throughout the docs:

- **weave** — a repoweave workspace; a directory containing `projects/` and cloned member repos.
- **workweave** — a worktree-derived sandbox of a weave (isolated parallel cross-repo work).
- **project** — the small coordination repo under `projects/<name>/`, carrying `rwv.toml` and `rwv.lock`.
- **manifest repo** (or **member repo**) — a repo listed in a project's `rwv.toml`. The repos you actually work in; regular clones at `<registry>/<owner>/<repo>/`.
- **lock** (`rwv.lock`) — pins each manifest repo to a specific 40-character commit SHA. Derived from manifest-repo tips; never edited by hand.

For the full vocabulary, see [reference/glossary](./reference/glossary.md).

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
| **Explanation** | The "why" and the model | [explanation/](./explanation/index.md) — motivational lenses plus cross-cutting joints |
| **Reference** | Lookup-shaped facts | [reference/](./reference/cli.md) — verbs, file formats, roles, glossary, integrations |

To report a bug, request an integration, or ask a design question, open a [GitHub issue](https://github.com/cwalv/repoweave/issues).
