# repoweave

You know `go.work`? It lets you group Go modules under one root so cross-module imports resolve locally. pnpm workspaces do the same for Node packages. Cargo workspaces for Rust crates. uv workspaces for Python packages.

repoweave does the same thing at the repo layer.

Every major package ecosystem has converged on the same concept: a workspace that groups multiple packages under one root for cross-package imports, shared dependency resolution, and coordinated development. None of these tools care whether the packages are in one repo or twenty — `go.work` lists directories, not repos. A pnpm workspace can already span repo boundaries. The workspace concept is repo-agnostic.

What ecosystem workspaces don't handle: getting the code on disk, tracking which repos belong together and why, pinning versions across repos, or generating the ecosystem workspace configs. repoweave handles all of that. An `rwv.yaml` manifest describes the same set of directories that `package.json` workspaces or `go.work` describes — for a different purpose (repo lifecycle vs. dependency resolution). The integrations translate one workspace manifest into another.

This also gives you monorepo ergonomics without merging repos. All your code lives in one directory tree, so every tool that touches the filesystem — editors, grep, agents, debuggers, build tools — works across all of it. Your code can talk to your other code without ceremony. But repos stay sovereign: normal git, normal branches, normal push/pull.

Monorepos succeed because they provide a single, well-understood convention — directory layout, cross-package imports, atomic versioning, workspace-wide tooling all just work. Multi-repo setups have no equivalent standard, so every team reinvents the glue (or doesn't, and lives with the friction). repoweave provides that convention: a standard layout, ecosystem wiring, version pinning, and reproducibility across repos.

One example of reduced friction: **no version bump cycle for cross-repo changes.** Normally, changing `protocol` before `server` can use it means: bump protocol's version, publish, update server's dependency, install, test. With repoweave, the ecosystem workspace wiring means `server` already imports from the local `protocol` checkout. You commit in both repos, lock, done. The version bump can happen later — or never, for internal code.

## Core idea

The workspace has three layers:

1. **The directory tree** — repos under one root. Every tool benefits: search, navigation, agents, editors. This is the convention alone — no tooling required.
2. **Ecosystem wiring** — integrations generate workspace files (`package.json`, `go.work`, `Cargo.toml`, `pnpm-workspace.yaml`) so cross-package imports resolve locally. `import { thing } from '@myorg/shared'` just works.
3. **Reproducibility** — a committed `rwv.yaml` file and its `rwv.lock` pin each repo to an exact SHA, making the project state reproducible from a single project repo.

The only difference from a monorepo commit: updating the lock file is two steps instead of one. You commit in individual repos first, then regenerate and commit the lock file. It's two-phase commit — the lock update is detectable and reversible:

```bash
# 1. Commit in individual repos (already done)
# 2. Update and commit the lock file
rwv lock
cd projects/web-app
git add rwv.lock && git commit -m "lock: add payment endpoint"
```

To reproduce a project from scratch:

```bash
cargo install repoweave
mkdir my-workspace && cd my-workspace
rwv fetch chatly/web-app
```

`sha256sum rwv.lock` gives a single fingerprint for the project state — the multi-repo equivalent of `git rev-parse HEAD`.

## Directory layout

Repos are **regular clones** at canonical paths: `{registry}/{owner}/{repo}/`. No bare repos, no worktree indirection for the default case. The primary directory has ecosystem files at its root — committable, not gitignored.

Normal repos are organized by provenance: `{registry}/{owner}/{repo}/`. The first path segment is a **registry** — a short name for where the repo lives. `rwv` ships with built-in defaults for well-known hosts (`github.com` -> `github`, `gitlab.com` -> `gitlab`, `bitbucket.org` -> `bitbucket`); custom registries are configured in `rwv`'s own config. A registry can be domain-based (e.g., `git.mycompany.com` -> `internal`, handles `https://` and `git@` URLs) or directory-based (e.g., `/srv/repos` -> `local`, handles `file://` URLs). This follows Go's GOPATH precedent (`$GOPATH/src/github.com/owner/repo`), shortened for ergonomics.

Two kinds of directories:

| Kind | Path | Purpose |
|------|------|---------|
| **Normal** | `{registry}/{owner}/{repo}/` | Code. Build tools look here. Other repos import from here. Listed in root `package.json` workspaces, `go.work`, etc. |
| **Project** | `projects/{name}/` | Coordination. `rwv.yaml`, lock files, docs. Build tools never see these. No importable code. |

Path encodes kind — you can tell what a directory is for from its location. Build tools (npm/pnpm, Go, Cargo, uv) are configured to look inside registry directories (`github/`, `gitlab/`, etc.), not `projects/`. Project repos have GitHub URLs (for fetchability) but their local path reflects their *role*, not their provenance.

Project paths default to `projects/{name}/` for ergonomics. If names collide (two owners with a project called `web-app`), `rwv fetch` errors and suggests a scoped path: `projects/{owner}/{name}/` or `projects/{registry}/{owner}/{name}/`. `rwv` commands that take a project path require the path as created — if the project lives at `projects/chatly/web-app/`, you must use `chatly/web-app`, not just `web-app`. Errors if no matching directory with an `rwv.yaml` file exists.

Example — a team building a chat product with a web app and mobile app:

```
web-app/                                  # primary
├── github/                               # regular clones
│   ├── chatly/
│   │   ├── server/                       # regular clone, on main
│   │   ├── web/                          # regular clone, on feature-A
│   │   ├── mobile/                       # regular clone, on main
│   │   └── protocol/                     # regular clone, on main
│   │
│   ├── socketio/
│   │   └── engine.io/                    # regular clone (fork)
│   │
│   └── nickel-io/
│       └── push-sdk/                     # regular clone (dependency)
│
├── projects/
│   ├── web-app/
│   │   ├── rwv.yaml                      # source of truth: which repos, what roles
│   │   ├── rwv.lock                      # pinned SHAs (committed)
│   │   └── docs/
│   └── mobile-app/
│       ├── rwv.yaml
│       ├── rwv.lock
│       └── docs/
│
├── package.json                          # ecosystem workspace — committable
├── package-lock.json                     # ecosystem lock — committable
├── go.work                               # ecosystem workspace — committable
├── go.sum                                # ecosystem lock — committable
├── node_modules/                         # tool state — gitignored
├── .venv/                                # tool state — gitignored
└── .gitignore
```

- **Repos are regular clones** — `cd github/chatly/server && git status` works. No bare repos, no `.git` file indirection, universal tool compatibility.
- **Ecosystem files at the root** — `package.json`, `go.work`, `Cargo.toml` live at the primary's root. They're committable, not ephemeral.
- **Ecosystem lock files are committable** — `package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.sum`, `Cargo.lock` live at the root alongside their workspace configs.
- **Projects are directories** with an `rwv.yaml` file, an `rwv.lock` file, and `docs/`. They don't contain code — build tools are unaware of them.
- **Overlap is natural** — `server` and `protocol` appear in both projects' `rwv.yaml` files, but there's one clone on disk.
- **Repos without a project stay on disk** — clone something for a quick look; it's an inert directory until you add it to a project.

## Weaves

A weave is an isolated working copy of a project — its own git worktrees, ecosystem files, and tool state. The primary directory is a regular directory of clones; weaves are created on demand when isolation is needed (agents, PR review, parallel features).

```bash
rwv weave web-app agent-42       # isolated working copy for an agent
rwv weave web-app hotfix         # parallel working copy for a hotfix
rwv weave web-app review-pr-99   # isolated working copy for PR review
```

Each weave:

1. **Creates worktrees** from the regular clones for every repo in `rwv.yaml`, each on an ephemeral branch.
2. **Runs integrations** — generates ecosystem files (`package.json`, `go.work`, etc.) and runs install commands inside the weave directory.

Weaves are fully isolated. `node_modules/`, `.venv/`, branches, and generated files are per-weave. One weave can be on `feature-A` while another is on `main`, while the primary stays undisturbed.

### Sibling model

Weaves are **siblings** of the primary directory, not nested inside it. The naming convention is `{primary}--{weave-name}`:

```
web-app/                              # primary — regular clones
├── github/chatly/server/             # clone, on main
├── package.json
├── package-lock.json
└── ...

web-app--agent-42/                    # weave — worktrees
├── github/chatly/server/             # worktree, on agent-42/main
├── github/chatly/web/                # worktree, on agent-42/feature-A
├── github/chatly/protocol/           # worktree, on agent-42/main
├── package.json                      # generated for this weave
├── package-lock.json                 # this weave's resolution
├── node_modules/                     # isolated
└── .venv/                            # isolated
```

Siblings. Same level of permanence. The weave is a first-class working copy, not a buried artifact.

### How weave creation works

```bash
rwv weave web-app agent-42
```

For each repo in `projects/web-app/rwv.yaml`, this runs:

```bash
git -C github/chatly/server worktree add \
  ../web-app--agent-42/github/chatly/server \
  -b agent-42/main    # ephemeral branch off current HEAD
```

Then runs integration hooks to generate ecosystem files and install dependencies in the new weave.

### Ephemeral branches

Git won't let two worktrees check out the same branch. Weaves handle this by creating ephemeral branches:

```bash
# If the clone's server/ is on "main":
git worktree add ../web-app--agent-42/github/chatly/server -b agent-42/main
# Creates branch "agent-42/main" at the same commit as "main"
```

The agent (or developer) works on `agent-42/main`. When done, the branch gets merged or cherry-picked back, then deleted with the weave. The primary clone stays on `main` undisturbed.

This is how agents naturally work anyway — they should be on their own branches. The ephemeral branch isn't a workaround; it's correct behavior.

### Weave lifecycle

**Creation:**

```bash
rwv weave web-app agent-42
# Creates web-app--agent-42/ as a sibling
# Fans out git worktree add for each repo
# Generates ecosystem files
# Runs npm install / uv sync / etc.
```

**Working:**

```bash
cd web-app--agent-42
npm test --workspaces
cd github/chatly/server && git commit -m "fix"
```

Standard git, standard ecosystem tools. The weave is just a directory with worktrees.

**Cleanup:**

```bash
rwv weave web-app agent-42 --delete
# Runs git worktree remove for each repo
# Removes the directory
# Optionally deletes ephemeral branches
```

Or, if the ephemeral branches have been merged: `rm -rf web-app--agent-42` plus `git worktree prune` in each repo to clean up stale metadata.

### WEAVEROOT

By default, weaves are siblings of the primary directory. Override with the `WEAVEROOT` environment variable:

```bash
WEAVEROOT=~/weaves rwv weave web-app agent-42
# Creates ~/weaves/web-app--agent-42/
```

Or per-invocation:

```bash
rwv weave web-app agent-42 --path ~/weaves/agent-42
```

This is useful when sibling weaves would clutter a parent directory, or when weaves should live on a different filesystem.

### Weave context

Commands like `add`, `remove`, `lock`, and `check` infer the project and weave from your CWD:

- **In a weave directory** — uses that weave directly.
- **In the primary directory** — resolves to the primary.
- **In a project directory** — resolves to the primary.
- **Override** — use `--project` flag.

### Syncing after manifest changes

If you edit `rwv.yaml` (add/remove repos), sync the weave:

```bash
rwv weave web-app --sync
```

`rwv add` and `rwv remove` handle this automatically — they update `rwv.yaml` and re-run integration hooks in one step.

### Agent isolation

Creating an isolated weave for an agent:

```bash
rwv weave web-app agent-42
# Agent CWD: web-app--agent-42/
```

The agent gets its own worktrees on ephemeral branches, its own `node_modules/`, its own everything. It commits, tests, and iterates without affecting the developer's primary directory.

When done:

```bash
rwv weave web-app agent-42 --delete
```

For the Claude Code SDK, the orchestrator creates the weave before launching the agent. The agent doesn't need to know about repoweave internals — it sees a directory with repos in it.

**CWD constraint**: Claude Code's `isolation: "worktree"` requires CWD inside a git repo. The weave directory itself isn't a git repo — it contains git worktrees. The agent's CWD should be set to a specific repo within the weave (e.g., the primary repo).

## Repos files

YAML format with a `repositories` root key. Each entry is keyed by local path and has `type`, `url`, `version`, and `role` fields. Based on vcstool's `.repos` format, extended with `role` and an optional `integrations` key for integration configuration (see [Integrations](#integrations)). Each project directory contains an `rwv.yaml` (the declaration) and optionally an `rwv.lock` (pinned SHAs).

### Project `rwv.yaml` files

The source of truth for which repos belong to a project. Committed in the project repo, with version history:

```yaml
# projects/web-app/rwv.yaml
repositories:
  github/chatly/server:
    type: git
    url: https://github.com/chatly/server.git
    version: main
    role: primary
  github/chatly/web:
    type: git
    url: https://github.com/chatly/web.git
    version: main
    role: primary
  github/chatly/protocol:
    type: git
    url: https://github.com/chatly/protocol.git
    version: main
    role: primary                # shared message types
  github/socketio/engine.io:
    type: git
    url: https://github.com/chatly/engine.io.git
    version: main
    role: fork                   # added reconnection logic
```

### Lock files

Generated by `rwv lock`, same format but with resolved revisions instead of branch names. When a tag exists at HEAD, the tag name is recorded; otherwise, the raw SHA. Optionally records which weave it was generated from:

```yaml
# projects/web-app/rwv.lock — generated, committed
weave: agent-42    # or omitted for the primary
repositories:
  github/chatly/server:
    type: git
    url: https://github.com/chatly/server.git
    version: v2.1.0              # tagged — human readable
  github/chatly/web:
    type: git
    url: https://github.com/chatly/web.git
    version: e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0  # untagged — raw SHA
  github/chatly/protocol:
    type: git
    url: https://github.com/chatly/protocol.git
    version: 7a3b2c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b  # untagged
  # ...
```

The `weave` field is metadata — it records provenance without mixing responsibilities. It is omitted when the lock was generated from the primary.

Lock files live alongside `rwv.yaml` in the project directory, committed in the project repo. Each project owns its own lock state.

`sha256sum rwv.lock` is the project fingerprint. Two developers with the same lock file checksum have identical source for every repo in the project.

### Locking and versioning

Ecosystem lock files (`package-lock.json`, `Cargo.lock`) resolve version *ranges* to specific versions — `^1.2.0` becomes `1.2.3`. The input is a constraint; the output is a resolution. `rwv.lock` is different: it records whatever revision each repo is at. There's no range to resolve — it's a snapshot of current state.

But the purpose is the same: **reproducibility**. Both kinds of lock file answer "given this description of what I want, what exact code do I get?"

The practical difference matters for workflow. In a traditional multi-repo setup, changing `protocol` before `server` can use it requires: bump protocol's version, publish, update server's dependency, install, test. With repoweave, the ecosystem workspace wiring means `server` already imports from the local `protocol` checkout. You commit in both repos, run `rwv lock`, done. The version bump can happen later — or never, for internal code that doesn't need a published version.

This is monorepo-level iteration speed without a monorepo. The lock file captures your exact state whether or not you've done a formal version bump. When a tag exists at HEAD, the lock records it (readable, auditable). When it doesn't, the SHA is equally pinned — just less pretty.

The ecosystem lock files complement `rwv.lock`. Together they capture both layers: `rwv.lock` pins which commit of each repo, and `package-lock.json` / `Cargo.lock` / etc. pin which versions of external dependencies were resolved within that ecosystem. Full reproducibility requires both.

### Versioning guidelines

repoweave doesn't require any particular versioning scheme, but one practice makes the multi-repo workflow smoother: **include the git revision in your package version.**

Semver supports build metadata via the `+` suffix: `2.1.0+7a3b2c1`. The revision after `+` doesn't affect version precedence but carries provenance. Whatever mechanism a package uses to report its version — `package.json` `version` field, `pyproject.toml`, `Cargo.toml`, a `--version` flag — it should include the revision when possible.

This makes the version output useful for debugging across repos:

```
$ my-server --version
my-server 2.1.0+7a3b2c1

$ my-protocol --version
my-protocol 1.4.0+e1f2a3b
```

You can immediately tell which commits are running. In a monorepo, `git rev-parse HEAD` tells you everything. In a multi-repo setup, the `+revision` suffix gives you the same traceability per package.

Most ecosystems support this:
- **npm**: `version` in `package.json` supports semver build metadata
- **Cargo**: `version` in `Cargo.toml` supports `+` metadata
- **Python**: PEP 440 uses `+local` for local version identifiers (e.g., `2.1.0+7a3b2c1`)
- **Go**: `runtime/debug.BuildInfo` can embed VCS revision via build flags

For repos that don't publish packages (internal services, scripts), the version matters less — `rwv.lock` captures the exact SHA regardless. The `+revision` convention is most valuable for packages that are consumed as dependencies, where "which version of protocol is server using?" is a common question.

## Projects

A project is a directory under `projects/` with an `rwv.yaml` file, a lock file, and a `docs/` directory. (We use "project" rather than "workspace" to avoid overloading the term — every ecosystem already uses "workspace" for its own build wiring: npm workspaces, `go.work`, Cargo workspaces, pnpm workspaces. A project is also more than build wiring — it includes docs, roles, and lock files. We reserve "workspace" for ecosystem workspaces exclusively.)

```
projects/web-app/
├── rwv.yaml                      # Which repos, with what roles
├── rwv.lock                      # Pinned SHAs (rwv lock)
└── docs/                         # Cross-repo architecture docs, roadmap, coordination
```

### What projects do

Projects answer the question: "I'm working on the web app — which repos matter?"

Without projects, you have a flat list of 20 repos and need tribal knowledge to know which ones are relevant. With a project, every `rwv` command — `rwv lock`, `rwv check` — and every weave is scoped to the repos that matter for that work.

Projects also provide a home for documentation that doesn't belong to any single repo. An architecture decision that spans `server` and `protocol` shouldn't live in either repo — it lives in `projects/web-app/docs/`.

### Fetching a project

On a new machine, you don't clone repos one by one:

```bash
mkdir ~/workspace && cd ~/workspace
rwv fetch chatly/web-app
```

`rwv fetch` clones the project repo to `projects/web-app/`, reads its `rwv.yaml`, creates regular clones for every listed repo at their canonical paths, and generates ecosystem files at the root. One command, and you have the complete working environment.

### Overlap between projects

Same repo, different projects — natural and expected:

```
projects/web-app/rwv.yaml:
  github/chatly/server           role: primary
  github/chatly/protocol         role: primary

projects/mobile-app/rwv.yaml:
  github/chatly/server           role: primary
  github/chatly/protocol         role: primary
```

There's one clone of `server` on disk. The role annotations may differ between projects — `server` could be `primary` in one and `dependency` in another. Each project's `rwv.yaml` determines which role applies.

### Project variants via branches

Need a variant of a project — same repos but with an extra dependency, or a different role for one repo? Use a branch in the project repo rather than a separate project:

```bash
cd projects/web-app
git checkout -b experiment
# edit rwv.yaml (add a repo, change a role)
rwv weave web-app experiment   # new weave reads the branch's rwv.yaml
```

This avoids inventing inheritance or "derived project" machinery. A branch is already a variant with full version history.

### Ecosystem files and multiple projects

The primary directory serves multiple projects, but ecosystem files (`package.json`, `go.work`) at the root reflect the union of all repos on disk. Every repo appears in the ecosystem workspace configs. This is the simplest approach and matches how the directory actually works — all repos are on disk, all ecosystem tools can see them.

If a project needs scoped ecosystem files (only its own repos), it creates a weave. Weaves generate ecosystem files scoped to the project's `rwv.yaml`.

## Roles

Roles signal **change resistance** — how freely you (or an agent) should modify the code:

| Role | Change resistance | Meaning |
|------|-------------------|---------|
| `primary` | None | Your code. Change it if it's an improvement. |
| `fork` | Low | Forked upstream. Ideally changes accepted upstream, but expediency is fine. |
| `dependency` | Medium | Code you build against. Changes need upstream acceptance, or convert to a fork. |
| `reference` | High | Cloned for reading/study during design. No local changes. Could be removed when done. |

**Roles are per-project, not per-repo.** The same repo can have different roles in different projects. `engine.io` is a `fork` in web-app (patched for reconnection) but could be a `dependency` in another project (using it unmodified). The active project's `rwv.yaml` determines the current role.

Roles are a first-class field in `rwv.yaml`:

```yaml
  github/socketio/engine.io:
    type: git
    url: https://github.com/chatly/engine.io.git
    version: main
    role: fork                   # added reconnection logic
```

**Directory owner as heuristic** — `github/chatly/` is likely primary, `github/{other}/` is likely reference or dependency. But this is a default, not a rule — projects override it.

## Commands

`rwv` is a standalone Rust CLI that manages repos following repoweave conventions using direct git commands. Installed out of band — not part of any project. Nothing about the underlying `rwv.yaml` files changes; `rwv` is a convenience layer on top.

| Command | What it does |
|---|---|
| `rwv` | Show current context (root, project, weave, repos). |
| `rwv weave {project} [name]` | Create a weave (isolated working copy with worktrees on ephemeral branches). |
| `rwv weave {project} --delete` | Delete a weave (remove worktrees, clean up ephemeral branches). |
| `rwv weave {project} --sync` | Sync weave worktrees and ecosystem files with manifest. |
| `rwv weave {project} --list` | List weaves for a project. |
| `rwv fetch {source}` | Clone a project repo and all its listed repos, generate ecosystem files. |
| `rwv add {url\|path}` | Clone a repo, register in `rwv.yaml`, re-run integration hooks. With `--role`, sets the role annotation. |
| `rwv remove {path}` | Remove from `rwv.yaml`, re-run integration hooks. With `--delete`, also removes the clone (confirms unless `--force`). |
| `rwv lock` | Snapshot repo versions into the project's `rwv.lock`. Errors on uncommitted changes (`--dirty` to bypass). Runs integration lock hooks. |
| `rwv check` | Convention enforcement: orphaned clones, dangling references, missing roles, stale locks, weave drift, integration checks. |
| `rwv resolve` | Print the weave root (if in a weave) or primary root. Useful for scripting: `cd $(rwv resolve)`. |

### `rwv check` and multi-project awareness

`rwv check` scans all `projects/*/rwv.yaml` files to build a complete inventory of known repos. This prevents false orphan warnings — a repo from another project is not an orphan.

| Check | Description |
|---|---|
| **Orphaned clones** | Directories under registry paths not listed in ANY project `rwv.yaml` |
| **Dangling references** | Entries in an `rwv.yaml` pointing to paths not on disk |
| **Missing role** | `rwv.yaml` entries without a `role` field |
| **Stale lock** | Project's `rwv.lock` doesn't match current HEAD SHAs |
| **Weave drift** | Worktrees missing from a weave or extra worktrees not in manifest |
| **Integration checks** | Each integration's check hook reports tool availability, stale config, etc. (see [Integrations](#integrations)) |

### `rwv lock`

Lock snapshots the active project's repo versions. It reads HEAD from each repo (regular clones in the primary, worktrees in a weave) and writes `rwv.lock` to the project directory. If a tag exists at HEAD, the tag name is recorded; otherwise the raw SHA.

**Uncommitted changes are an error.** If any repo in the project has uncommitted changes, `rwv lock` refuses to proceed — the lock file would record a SHA that doesn't reflect the actual state on disk. Use `--dirty` to bypass:

```bash
$ rwv lock
error: github/chatly/server has 2 uncommitted changes
error: lock would record HEAD (abc1234), not working tree state
hint: commit your changes first, or use --dirty to lock anyway

$ rwv lock --dirty
⚠ github/chatly/server: 2 uncommitted changes (locking HEAD, not working tree)
wrote projects/web-app/rwv.lock (4 repos)
```

**Integration lock hooks.** After writing `rwv.lock`, each integration's lock hook runs to ensure ecosystem lock files are up to date (`npm install --package-lock-only`, `uv lock`, `cargo generate-lockfile`, etc.). This way `rwv lock` means "pin everything" — both repo versions and ecosystem dependency versions.

### Bootstrap on a new machine

```bash
cargo install repoweave
mkdir ~/workspace && cd ~/workspace
rwv fetch chatly/web-app
```

## Why not git submodules?

Git submodules aim to solve a similar problem — coordinating code across repos — but take a different approach. The feature mapping is close:

| repoweave | Git submodules |
|---|---|
| project `rwv.yaml` | `.gitmodules` |
| project `rwv.lock` | SHA stored in parent tree (inherent) |
| `rwv fetch` | `git submodule update --init --recursive` |
| `rwv lock` | `git add <submodule>` (records current SHA) |

Submodules are better at one thing: **atomic locking**. The SHA is part of the parent's git tree — there's no two-phase commit. When you commit the parent, the lock updates atomically. repoweave's explicit lock file is the price of not using submodules.

But submodules take ownership in ways that conflict with multi-repo development:

- **Detached HEAD** — submodules check out a SHA, not a branch. You `cd` into one and you're in detached HEAD. You have to `git checkout main` before working. Every repo, every time.
- **Can't adopt existing clones** — submodules want to own the clone. You can't take 16 repos already on disk and retroactively make them submodules.
- **Parent owns the relationship** — updating a submodule means: commit in child, `cd` to parent, `git add child`, commit parent. The parent is always in the loop. For reference repos you don't control, this is backwards.
- **No partial fetch** — submodules are all-or-nothing per parent. No "fetch only the web-app project's repos." No project-scoped views.
- **No roles** — submodules are a flat list. No way to distinguish primary from reference, no per-project role assignments.
- **Flat nesting only** — if a dependency uses submodules, you get recursive submodule hell. repoweave's flat `{registry}/{owner}/{repo}` layout avoids nesting.

The design trade-off: submodules get atomic locking for free by taking ownership. repoweave gives up atomic locking to preserve sovereignty — repos stay on normal branches, you work in them normally, and the lock file is an explicit (two-step) operation.

## Integrations

Integrations are pluggable units that each derive config for one tool from the repo list. Each participates in activation hooks (run when creating/syncing weaves or after `rwv add`/`rwv remove`) and check hooks (`rwv check` — read-only inspection). Integration config lives in the project's `rwv.yaml` under an `integrations` key; only overrides need to be listed.

| Integration | Default enabled | Auto-detects | Generates |
|---|---|---|---|
| `npm-workspaces` | yes | repos with `package.json` | `package.json` + `npm install` |
| `pnpm-workspaces` | no | repos with `package.json` | `pnpm-workspace.yaml` + `pnpm install` |
| `go-work` | yes | repos with `go.mod` | `go.work` |
| `uv-workspace` | yes | repos with `pyproject.toml` | `pyproject.toml` + `uv sync` |
| `cargo-workspace` | yes | repos with `Cargo.toml` | `Cargo.toml` |
| `gita` | yes | all repos | `gita/` config directory |
| `vscode-workspace` | yes | all repos | `{project}.code-workspace` |

All generated files live in the primary directory (or the weave directory for weaves). Ecosystem integrations generate workspace config files that are committable — they are persistent state, not ephemeral artifacts. Integrations merge into existing files where possible — for example, the vscode-workspace integration preserves user-added settings and extensions while updating managed keys.

Ecosystem lock files (`package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.sum`, `Cargo.lock`) are produced by the ecosystem tools themselves (npm, pnpm, uv, go, cargo) during the install step. These are important persistent state — they pin exact dependency versions within each ecosystem and should be committed alongside the ecosystem workspace configs.

See [integrations.md](integrations.md) for generated file formats, configuration, and details on each integration.

## Adjacent tools

repoweave solves "which repos, at what versions, in what structure." Several adjacent tools solve other layers — toolchain versions, environment activation, containerized dev environments, CI checkout. They're complementary, and the active project's state makes many of them easier to configure.

### What's derivable from project state

A project's `rwv.yaml` + the files generated by integration hooks already imply most of the dev environment:

| Layer | Derivable? | How |
|---|---|---|
| **Repos on disk** | Yes | `rwv fetch` — the whole point |
| **Toolchains needed** | Yes | `package.json` exists -> Node, `go.work` exists -> Go |
| **Toolchain versions** | Partially | Ecosystem files often pin versions (`.nvmrc`, `go.mod`'s go directive). `.mise.toml` at root fills the gap. |
| **Workspace deps** | Yes | `npm install`, `go work sync` — deterministic once repos + workspace files exist |
| **Editor workspace** | Yes | `.code-workspace` folders directly derivable from project `rwv.yaml` |
| **Base image / OS packages** | No | System-level, not inferrable from repo structure |
| **Services** | No | Databases, message queues — runtime deps, not repo deps |
| **Secrets / env vars** | No | Out of scope |

### mise / asdf — toolchain versions

[mise](https://mise.jdx.dev/) (formerly rtx) manages language runtime versions with a single `.mise.toml` at the root:

```toml
# .mise.toml at root
[tools]
node = "22"
go = "1.22"
rust = "1.78"
```

### direnv — environment activation

[direnv](https://direnv.net/) auto-activates environments when you `cd` into a directory:

```bash
# .envrc at root
use mise                                      # activate toolchain versions
export GITA_PROJECT_HOME="$PWD/gita"          # point gita at derived config
```

### Devcontainers / Codespaces

`rwv fetch` replaces a wall of `git clone` commands in `postCreateCommand`:

```jsonc
// .devcontainer/devcontainer.json
{
  "features": {
    "ghcr.io/devcontainers/features/node:1": {},
    "ghcr.io/devcontainers/features/go:1": {},
    "ghcr.io/devcontainers/features/rust:1": {}
  },
  "postCreateCommand": "cargo install repoweave && rwv fetch chatly/web-app",
  "forwardPorts": [5432]
}
```

### Nix flakes — structural parallel

[Nix flakes](https://wiki.nixos.org/wiki/Flakes) are the deepest structural parallel. `flake.nix` inputs = project `rwv.yaml`, `flake.lock` = project `rwv.lock`, `devShell` = toolchain+deps setup. The difference: Nix owns the entire build graph and is all-or-nothing. repoweave is deliberately lighter — just repos and conventions, composable with whatever build/env tools you prefer.

### CI multi-repo checkout

`rwv.yaml` can drive a reusable checkout action — same pattern as `rwv fetch` but in CI:

```yaml
# .github/workflows/ci.yml
- uses: actions/checkout@v4          # this repo (projects/web-app)
- run: cargo install repoweave && rwv fetch   # reads rwv.yaml, clones code repos
- run: npm install && npm test
```

## Prior art: multi-repo coordination

There is no universal standard for multi-repo development. "Polyrepo" names the strategy (the counterpart to "monorepo") but prescribes no conventions. Each ecosystem that needed multi-repo coordination invented its own:

| Tool | Ecosystem | Manifest format | Lock/pin mechanism |
|------|-----------|----------------|--------------------|
| Google `repo` | Android/embedded | XML (`default.xml`) | `repo manifest -r` (revision-locked manifest) |
| West | Zephyr RTOS | YAML | `west manifest --freeze` |
| vcstool | ROS | YAML (`.repos`) | `vcs export --exact` |
| git submodules | General | `.gitmodules` | SHA in parent tree (inherent) |

These tools are well-established within their ecosystems but none crossed over to become a general standard. Each makes trade-offs specific to its community — `repo` assumes Gerrit, West requires a manifest repo, submodules take ownership of clones.

Meanwhile, ecosystem workspace tools have converged on a shared *pattern* — a manifest listing directories for cross-package resolution — without coordinating on format:

| Tool | Manifest | Purpose |
|------|----------|---------|
| npm/pnpm | `package.json` workspaces / `pnpm-workspace.yaml` | Cross-package imports, shared deps |
| Go | `go.work` | Cross-module resolution |
| Cargo | `Cargo.toml` `[workspace]` | Shared lock, shared target |
| uv | `pyproject.toml` `[tool.uv.workspace]` | Cross-package deps |

These don't care about repo boundaries — they list directories. They handle dependency resolution but not repo lifecycle (cloning, pinning, reproducing).

repoweave sits at the intersection: it provides the repo lifecycle layer (like `repo`/West/vcstool) and generates the ecosystem workspace configs (like `go.work`/pnpm). The manifest format is YAML, based on vcstool's `.repos` format — the most portable of the existing formats.
