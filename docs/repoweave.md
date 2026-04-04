# repoweave

repoweave provides the scaffolding for building projects from components tracked in separate repositories. It removes two major sources of friction:

- The edit → bump → publish → update → reinstall cycle for internal dependencies
- The difficulty of creating ephemeral multi-repo workspaces for agents, PRs, or parallel work

### Workspaces

Many major package ecosystems have converged on the concept of a workspace that groups multiple packages under one root for cross-package imports, shared dependency resolution, and coordinated development (`go.work`, Cargo `[workspace]`, pnpm workspaces, uv workspaces, etc.). They deal with packages, not repositories, but these are often 1:1.

### Monorepos

Full monorepos eliminate the version dance and solve other problems that workspaces help, but require vendoring or forking everything. Revision logs are polluted, provenance is obscured, and collaboration on or distribution of logical subsets of the monorepo is much more painful than it is with repositories that are scoped appropriately.

### The weave metaphor

The goal is to weave independent **threads** (your repositories) into a single, coherent **fabric** — a unified workspace. The threads keep their identity and history; they simply work better together.

A `weave` is a workspace in the same sense as a `go.work` workspace or a Cargo `[workspace]`, but with superpowers. Often, the workspace configurations can be generated from the repoweave manifest alone. In addition to simple cross-package imports and shared dependency resolution that workspace management tools bring, you get monorepo ergonomics.

repoweave provides a `lock` mechanism analogous to package manager locks, with a similar feel to the atomic commit you get from working in a monorepo (so no more edit → bump version → publish → update dependents → reinstall dance for repos in the same weave). Want to reproduce a weave on another machine, or CI? The `lock` makes it easy. It also makes it easy to create ephemeral workweaves for isolated work or review, like a multi-repo `git worktree`. Also, all your code lives in one directory tree, so every tool that touches the filesystem — editors, grep, agents, debuggers, build tools — works across all of it, just like a monorepo.

### Terminology

| Term | Meaning |
|------|---------|
| **weave** | A repoweave workspace — a directory containing repos, project directories, and ecosystem wiring generated from the active project |
| **workweave** | A worktree-based derivative of a weave, created on demand for isolation (agents, features, PR review) |
| **project** | A directory under `projects/` containing `rwv.yaml`, `rwv.lock`, and project-scoped docs. Itself a git repo |
| **manifest** (`rwv.yaml`) | Declares which repos belong to a project, their roles, and integration config |
| **lock file** (`rwv.lock`) | Pins repos to exact SHAs for reproducibility |
| **activation** | Generating ecosystem workspace files from a project's manifest and symlinking them to the weave root |
| **role** | A repo's relationship to a project: `primary` (your code), `fork`, `dependency`, `reference` |
| **workspace surface** | The root directory where ecosystem tools find workspace config files |

## Core idea

The weave has three layers:

1. **The directory tree** — repos under one root. Every tool benefits: search, navigation, agents, editors. This is the convention alone — no tooling required.
2. **Ecosystem wiring** — the weave root (or workweave root) is the **workspace surface**: the directory that ecosystem tools see. Integrations generate workspace files (`package.json`, `go.work`, `Cargo.toml`, `pnpm-workspace.yaml`) at this surface so cross-package imports resolve locally. Ecosystem tools don't know repos exist — they see a workspace directory with packages. `import { thing } from '@myorg/shared'` just works.
3. **Reproducibility** — a committed `rwv.yaml` file and its `rwv.lock` pin each repo to an exact SHA, making the project state reproducible from a single project repo.

Committing a project gets a coherent cross-repo "revision", similar to a monorepo commit. You commit in individual repos first, then regenerate and commit the lock file. By default, it's not atomic as a monorepo is, but it could be scripted to be so. It's two-phase commit — the lock update is detectable and reversible:

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

`sha256sum rwv.lock` gives a single fingerprint for the project state — the multi-repo equivalent of `git rev-parse HEAD` on a monorepo.

## Directory layout

repoweave uses a flat, provenance-based layout for repos: `{registry}/{owner}/{repo}/`. This keeps discovery simple, avoids path collisions when multiple projects share repos, and means every repo is a regular clone — `cd github/chatly/server && git status` just works. No bare repos, no worktree indirection for the default case.

Hierarchy and grouping happen at the project level (via `rwv.yaml`, which pulls in any subset of repos) and inside individual repos (organize your code however you like). Generated editor workspaces (VS Code `.code-workspace`) further let you focus on just the repos in a given project.

The first path segment is a **registry** — a short name for where the repo lives. `rwv` ships with built-in defaults for well-known hosts (`github.com` -> `github`, `gitlab.com` -> `gitlab`, `bitbucket.org` -> `bitbucket`); custom registries are configured in `rwv`'s own config. A registry can be domain-based (e.g., `git.mycompany.com` -> `internal`, handles `https://` and `git@` URLs) or directory-based (e.g., `/srv/repos` -> `local`, handles `file://` URLs). This follows Go's GOPATH precedent (`$GOPATH/src/github.com/owner/repo`), shortened for ergonomics.

Two kinds of directories:

| Kind | Path | Purpose |
|------|------|---------|
| **Normal** | `{registry}/{owner}/{repo}/` | Code. Build tools look here. Other repos import from here. Listed in root `package.json` workspaces, `go.work`, etc. |
| **Project** | `projects/{name}/` | Coordination. `rwv.yaml`, lock files, docs. Build tools never see these. No importable code. |

Path encodes kind — you can tell what a directory is for from its location. Build tools (npm/pnpm, Go, Cargo, uv) are configured to look inside registry directories (`github/`, `gitlab/`, etc.), not `projects/`. Project repos have GitHub URLs (for fetchability) but their local path reflects their *role*, not their provenance.

Project paths default to `projects/{name}/` for ergonomics. If names collide (two owners with a project called `web-app`), `rwv fetch` errors and suggests a scoped path: `projects/{owner}/{name}/` or `projects/{registry}/{owner}/{name}/`. `rwv` commands that take a project path require the path as created — if the project lives at `projects/chatly/web-app/`, you must use `chatly/web-app`, not just `web-app`. Errors if no matching directory with an `rwv.yaml` file exists.

Example — a team building a chat product with a web app and mobile app:

```
web-app/                                  # weave
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
├── package.json -> projects/web-app/package.json       # symlink to active project
├── package-lock.json -> projects/web-app/package-lock.json
├── go.work -> projects/web-app/go.work
├── go.sum -> projects/web-app/go.sum
├── node_modules/                         # tool state — gitignored
├── .venv/                                # tool state — gitignored
├── .rwv-active                           # "web-app" — tracks active project
└── .gitignore
```

- **Repos are regular clones** — `cd github/chatly/server && git status` works. No bare repos, no `.git` file indirection, universal tool compatibility.
- **Ecosystem files are symlinked** — `package.json`, `go.work`, `Cargo.toml` at root are symlinks to the active project's directory. The real files live in `projects/web-app/` and are committable in the project repo.
- **Ecosystem lock files are committable** — `package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.sum`, `Cargo.lock` live in the project directory alongside their workspace configs, symlinked to root.
- **Projects are directories** with an `rwv.yaml` file, an `rwv.lock` file, and `docs/`. They don't contain code — build tools are unaware of them.
- **Overlap is natural** — `server` and `protocol` appear in both projects' `rwv.yaml` files, but there's one clone on disk.
- **Repos without a project stay on disk** — clone something for a quick look; it's an inert directory until you add it to a project.

## Workweaves

The weave is where you do most of your work — regular git clones, project directories, and ecosystem wiring. But sometimes you need isolation: an agent task, a PR review, a parallel feature branch. That's what workweaves are for.

A **workweave** is a worktree-based derivative of the weave, created on demand. Each repo gets a git worktree (not a clone) on an ephemeral branch, with its own ecosystem files and tool state. Workweaves are ephemeral — created around a unit of work and destroyed when done.

```bash
rwv workweave web-app agent-42       # create
cd .workweaves/agent-42
npm test --workspaces                # isolated deps, isolated branches
cd github/chatly/server && git commit -m "fix"
# ... done ...
rwv workweave web-app agent-42 --delete   # clean up
```

Workweaves are fully isolated. `node_modules/`, `.venv/`, branches, and generated files are per-workweave. One workweave can be on `feature-A` while another is on `main`, while the weave stays undisturbed. Workweaves live in `.workweaves/{name}/` under the weaveroot.

This is also the natural escalation when switching projects with `rwv activate` is too slow (large dependency diff) — create a workweave for the second project and it gets its own `node_modules/` with no reconciliation needed.

For agents, the orchestrator creates the workweave before launching the agent. The agent doesn't need to know about repoweave — it sees a directory with repos in it. For agent frameworks that offer their own worktree isolation (e.g., Claude Code's `isolation: "worktree"`), use `rwv workweave` instead — it provides the same isolation but across all repos in the project.

See [workweaves.md](../../../projects/project-repoweave/docs/workweaves.md) for the full design document covering structure, artifact categories, and the relationship to Gas Town/Gas City. See [workflows.md](workflows.md) for detailed walkthrough examples.

### Workspace context

Commands like `add`, `remove`, `lock`, and `check` infer the project and workspace from your CWD:

- **In a workweave directory** — uses that workweave directly.
- **In the weave directory** — resolves to the weave.
- **In a project directory** — resolves to the weave.
- **Override** — use `--project` flag.

If you edit `rwv.yaml` in a workweave, sync it with `rwv workweave web-app --sync`. `rwv add` and `rwv remove` handle this automatically.

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

Generated by `rwv lock`, same format but with resolved revisions instead of branch names. When a tag exists at HEAD, the tag name is recorded; otherwise, the raw SHA. Optionally records which workweave it was generated from:

```yaml
# projects/web-app/rwv.lock — generated, committed
workweave: agent-42    # or omitted for the weave
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

The `workweave` field is metadata — it records provenance without mixing responsibilities. It is omitted when the lock was generated from the weave.

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

Without projects, you have a flat list of 20 repos and need tribal knowledge to know which ones are relevant. With a project, every `rwv` command — `rwv lock`, `rwv check` — and every workweave is scoped to the repos that matter for that work.

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
rwv workweave web-app experiment   # new workweave reads the branch's rwv.yaml
```

This avoids inventing inheritance or "derived project" machinery. A branch is already a variant with full version history.

### Ecosystem files and multiple projects

Each project has its own ecosystem files in its project directory (`projects/web-app/package.json`, `projects/mobile-app/package.json`). The root symlinks point to the active project's files. Switching projects with `rwv activate` swaps the symlinks and runs install commands to reconcile tool state.

If switching is too slow (large dependency diff), create a workweave for the second project — it gets its own `node_modules/`, `.venv/`, and ecosystem files with no reconciliation needed.

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
| `rwv` | Show current context (root, project, workweave, repos). |
| `rwv workweave {project} [name]` | Create a workweave (isolated working copy with worktrees on ephemeral branches). |
| `rwv workweave {project} --delete` | Delete a workweave (remove worktrees, clean up ephemeral branches). |
| `rwv workweave {project} --sync` | Sync workweave worktrees and ecosystem files with manifest. |
| `rwv workweave {project} --list` | List workweaves for a project. |
| `rwv init {project}` | Create a new project directory with empty `rwv.yaml`. Optional `--provider {registry}/{owner}` sets up the remote. |
| `rwv activate {project}` | Set the active project — generate ecosystem files in the project directory, symlink at weave root, run install commands. |
| `rwv fetch {source}` | Clone a project repo and all its listed repos, activate, update `rwv.lock`. `--locked` for exact reproduction, `--frozen` for CI (errors if lock is stale). |
| `rwv add {url}` | Clone a repo, register in `rwv.yaml`, re-run integration hooks. With `--role`, sets the role. With `--new`, initializes a new repo at the canonical path (infers URL). |
| `rwv remove {path}` | Remove from `rwv.yaml`, re-run integration hooks. With `--delete`, also removes the clone (confirms unless `--force`). |
| `rwv lock` | Snapshot repo versions into the project's `rwv.lock`. Errors on uncommitted changes (`--dirty` to bypass). Runs integration lock hooks. |
| `rwv check` | Convention enforcement: orphaned clones, dangling references, missing roles, stale locks, workweave drift, integration checks. |
| `rwv resolve` | Print the weave root (workweave or weave). Useful for scripting: `cd $(rwv resolve)`. |

### `rwv check` and multi-project awareness

`rwv check` scans all `projects/*/rwv.yaml` files to build a complete inventory of known repos. This prevents false orphan warnings — a repo from another project is not an orphan.

| Check | Description |
|---|---|
| **Orphaned clones** | Directories under registry paths not listed in ANY project `rwv.yaml` |
| **Dangling references** | Entries in an `rwv.yaml` pointing to paths not on disk |
| **Missing role** | `rwv.yaml` entries without a `role` field |
| **Stale lock** | Project's `rwv.lock` doesn't match current HEAD SHAs |
| **Workweave drift** | Worktrees missing from a workweave or extra worktrees not in manifest |
| **Integration checks** | Each integration's check hook reports tool availability, stale config, etc. (see [Integrations](#integrations)) |

### `rwv lock`

Lock snapshots the active project's repo versions. It reads HEAD from each repo (regular clones in the weave, worktrees in a workweave) and writes `rwv.lock` to the project directory. If a tag exists at HEAD, the tag name is recorded; otherwise the raw SHA.

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

The weave root (or workweave root) is the **workspace surface** — the directory that ecosystem tools see. npm, Go, Cargo, uv — none of them know that repos exist. They see a workspace directory with packages in it. `package.json` lists workspace packages. `go.work` lists modules. `Cargo.toml` lists crate members. The packages happen to come from different git repos, but the ecosystem tools neither know nor care.

Integrations are the translation layer between repoweave's multi-repo world (repos, projects, roles) and the ecosystem's workspace world (`package.json`, `go.work`, `Cargo.toml`, `pnpm-workspace.yaml`). They read the project's `rwv.yaml` and produce the ecosystem workspace files at the workspace surface. The result is that ecosystem tools work exactly as they would in a monorepo.

Each integration is a pluggable unit that derives config for one tool from the repo list. Each participates in activation hooks (run when creating/syncing workweaves or after `rwv add`/`rwv remove`) and check hooks (`rwv check` — read-only inspection). Integration config lives in the project's `rwv.yaml` under an `integrations` key; only overrides need to be listed.

| Integration | Default enabled | Auto-detects | Generates |
|---|---|---|---|
| `npm-workspaces` | yes | repos with `package.json` | `package.json` + `npm install` |
| `pnpm-workspaces` | no | repos with `package.json` | `pnpm-workspace.yaml` + `pnpm install` |
| `go-work` | yes | repos with `go.mod` | `go.work` |
| `uv-workspace` | yes | repos with `pyproject.toml` | `pyproject.toml` + `uv sync` |
| `cargo-workspace` | yes | repos with `Cargo.toml` | `Cargo.toml` |
| `gita` | yes | all repos | `gita/` config directory |
| `vscode-workspace` | yes | all repos | `{project}.code-workspace` |

All generated files live in the project directory (symlinked to the weave root, or the workweave root for workweaves). Ecosystem integrations generate workspace config files that are committable — they are persistent state, not ephemeral artifacts. Integrations merge into existing files where possible — for example, the vscode-workspace integration preserves user-added settings and extensions while updating managed keys.

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
