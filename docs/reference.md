# Reference

Authoritative reference for repoweave's terminology, directory layout, file formats, and commands.

### Terminology

| Term | Meaning |
|------|---------|
| **weave** | A repoweave workspace — a directory containing repos, project directories, and ecosystem wiring generated from the active project |
| **workweave** | A worktree-based derivative of a weave, created on demand for isolation (agents, features, PR review) |
| **project** | A directory under `projects/` containing `rwv.yaml`, `rwv.lock`, and project-scoped docs. Itself a git repo |
| **manifest** (`rwv.yaml`) | Declares which repos belong to a project, their roles, and integration config |
| **lock file** (`rwv.lock`) | Pins repos to exact revisions for reproducibility |
| **activation** | Generating ecosystem workspace files from a project's manifest and symlinking them to the weave directory |
| **role** | A repo's relationship to a project: `primary` (your code), `fork`, `dependency`, `reference` |
| **workspace surface** | The directory where ecosystem tools find workspace config files |

## Directory layout

repoweave uses a flat, provenance-based layout for repos: `{registry}/{owner}/{repo}/`. This keeps discovery simple, avoids path collisions when multiple projects share repos, and means every repo is a regular clone — `cd github/chatly/server && git status` just works. No bare repos, no worktree indirection for the default case.

Hierarchy and grouping happen at the project level (via `rwv.yaml`, which pulls in any subset of repos) and inside individual repos (organize your code however you like). Generated editor workspaces (VS Code `.code-workspace`) further let you focus on just the repos in a given project.

The first path segment is a **registry** — a short name for where the repo lives. `rwv` ships with built-in defaults for well-known hosts (`github.com` -> `github`, `gitlab.com` -> `gitlab`, `bitbucket.org` -> `bitbucket`); custom registries are configured in `rwv`'s own config. A registry can be domain-based (e.g., `git.mycompany.com` -> `internal`, handles `https://` and `git@` URLs) or directory-based (e.g., `/srv/repos` -> `local`, handles `file://` URLs). This follows Go's GOPATH precedent (`$GOPATH/src/github.com/owner/repo`), shortened for ergonomics.

Two kinds of directories:

| Kind | Path | Purpose |
|------|------|---------|
| **Normal** | `{registry}/{owner}/{repo}/` | Code. Build tools look here. Other repos import from here. Listed in root `package.json` workspaces, `go.work`, etc. |
| **Project** | `projects/{name}/` | Coordination. `rwv.yaml`, lock files, docs. Build tools never see these. No importable code. |

The path determines the directory's role — you can tell what a directory is for from its location. Build tools (npm/pnpm, Go, Cargo, uv) are configured to look inside registry directories (`github/`, `gitlab/`, etc.), not `projects/`. Project repos have GitHub URLs (for fetchability) but their local path reflects their *role*, not their provenance.

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
│   │   ├── rwv.lock                      # pinned revisions (committed)
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
- **Ecosystem lock files are committable** — `package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.sum`, `Cargo.lock` live in the project directory alongside their workspace configs, symlinked to the weave directory.
- **Projects are directories** with an `rwv.yaml` file, an `rwv.lock` file, and `docs/`. They don't contain code — build tools are unaware of them.
- **Overlap is natural** — `server` and `protocol` appear in both projects' `rwv.yaml` files, but there's one clone on disk.
- **Repos without a project stay on disk** — clone something for a quick look; it's an inert directory until you add it to a project.

## Repos files

YAML format with a `repositories` root key. Each entry is keyed by local path and has `type`, `url`, `version`, and `role` fields. Based on vcstool's `.repos` format, extended with `role` and an optional `integrations` key for integration configuration (see [Integrations](./integrations.md)). Each project directory contains an `rwv.yaml` (the declaration) and optionally an `rwv.lock` (pinned revisions).

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

Generated by `rwv lock`, same format but with resolved revisions instead of branch names. When a tag exists at HEAD, the tag name is recorded; otherwise, the revision ID. Optionally records which workweave it was generated from:

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
    version: e1f2a3b4c5d6e7f8a9b0c1d2e3f4a5b6c7d8e9f0  # untagged — revision ID
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

`rwv.lock` records whatever revision each repo is at — a snapshot of current state. When a tag exists at HEAD, the lock records the tag name (readable, auditable); otherwise the revision ID. The lock file format itself encodes release state: tag = released, revision ID = unreleased.

Ecosystem lock files (`package-lock.json`, `Cargo.lock`, etc.) complement `rwv.lock`. Together they capture both layers: `rwv.lock` pins which commit of each repo, and ecosystem lock files pin which versions of external dependencies were resolved. Full reproducibility requires both.

See [Concepts](./concepts.md#locking-and-the-version-dance) for how this changes the development workflow, and [Releasing](./releasing.md) for the release-time workflow.

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

For repos that don't publish packages (internal services, scripts), the version matters less — `rwv.lock` captures the exact revision regardless. The `+revision` convention is most valuable for packages that are consumed as dependencies, where "which version of protocol is server using?" is a common question.

## Projects

A project is a directory under `projects/` with an `rwv.yaml` file, a lock file, and a `docs/` directory. (We use "project" rather than "workspace" to avoid overloading the term — every ecosystem already uses "workspace" for its own build wiring: npm workspaces, `go.work`, Cargo workspaces, pnpm workspaces. A project is also more than build wiring — it includes docs, roles, and lock files. We reserve "workspace" for ecosystem workspaces exclusively.)

```
projects/web-app/
├── rwv.yaml                      # Which repos, with what roles
├── rwv.lock                      # Pinned revisions (rwv lock)
└── docs/                         # Cross-repo architecture docs, roadmap, coordination
```

### What projects do

Projects answer the question: "I'm working on the web app — which repos matter?"

Without projects, you have a flat list of 20 repos and need tribal knowledge to know which ones are relevant. With a project, every `rwv` command — `rwv lock`, `rwv check` — and every workweave is scoped to the repos that matter for that work.

Projects also provide a home for documentation that doesn't belong to any single repo. An architecture decision that spans `server` and `protocol` shouldn't live in either repo — it lives in `projects/web-app/docs/`.

### Fetching a project

On a new machine, you don't clone repos one by one:

```bash
mkdir ~/weaveroot && cd ~/weaveroot
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
rwv workweave web-app create experiment   # new workweave reads the branch's rwv.yaml
```

This avoids inventing inheritance or "derived project" machinery. A branch is already a variant with full version history.

### Ecosystem files and multiple projects

Each project has its own ecosystem files in its project directory (`projects/web-app/package.json`, `projects/mobile-app/package.json`). The symlinks at the weave directory point to the active project's files. Switching projects with `rwv activate` swaps the symlinks; to reconcile tool state run `npm install` (or `rwv lock`, which triggers integration lock hooks) afterward.

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

## Workweaves

A **workweave** is a worktree-based derivative of the weave, created on demand for isolation. Each repo gets a git worktree (not a clone) on an ephemeral branch, with its own ecosystem files and tool state. Workweaves live in `.workweaves/{name}/` under the weave directory.

`node_modules/`, `.venv/`, branches, and generated files are all per-workweave. One workweave can be on `feature-A` while another is on `main`, while the weave stays undisturbed.

See the [Tutorial](./tutorial.md#workweaves) for use-case walkthroughs (feature branches, PR review, agent isolation, parallel projects).

### Workspace context

Commands like `add`, `remove`, `lock`, and `check` infer the project and workspace from your CWD:

- **In a workweave directory** — uses that workweave directly.
- **In the weave directory** — resolves to the weave.
- **In a project directory** — resolves to the weave.
- **Override** — use `--project` flag.

If you edit `rwv.yaml` in a workweave, sync it with `rwv workweave web-app sync {name}`. `rwv add` and `rwv remove` handle this automatically.

## Commands

`rwv` is a standalone Rust CLI that manages repos following repoweave conventions using direct git commands. Installed out of band — not part of any project. Nothing about the underlying `rwv.yaml` files changes; `rwv` is a convenience layer on top.

| Command | What it does |
|---|---|
| `rwv` | Show current context (root, project, workweave, repos). |
| `rwv workweave {project} create [name]` | Create a workweave (isolated working copy with worktrees on ephemeral branches). |
| `rwv workweave {project} delete {name}` | Delete a workweave (remove worktrees, clean up ephemeral branches). |
| `rwv workweave {project} sync {name}` | Sync workweave worktrees and ecosystem files with manifest. |
| `rwv workweave {project} list` | List workweaves for a project. |
| `rwv init {project}` | Create a new project directory with empty `rwv.yaml`. Optional `--provider {registry}/{owner}` sets up the remote. |
| `rwv activate {project}` | Set the active project — generate ecosystem files in the project directory, symlink to the weave directory. |
| `rwv fetch {source}` | Clone a project repo and all its listed repos, activate, update `rwv.lock`. `--locked` for exact reproduction, `--frozen` for CI (errors if lock is stale). |
| `rwv add {url}` | Clone a repo, register in `rwv.yaml`, re-run integration hooks. With `--role`, sets the role. With `--new`, initializes a new repo at the canonical path (infers URL). |
| `rwv remove {path}` | Remove from `rwv.yaml`, re-run integration hooks. With `--delete`, also removes the clone (confirms unless `--force`). |
| `rwv lock` | Snapshot repo versions into the project's `rwv.lock`. Errors on uncommitted changes (`--dirty` to bypass). Runs integration lock hooks. |
| `rwv check` | Convention enforcement: orphaned clones, dangling references, missing roles, stale locks, workweave drift, integration checks. |
| `rwv resolve` | Print the weave directory (workweave or weave). Useful for scripting: `cd $(rwv resolve)`. |

### `rwv check` and multi-project awareness

`rwv check` scans all `projects/*/rwv.yaml` files to build a complete inventory of known repos. This prevents false orphan warnings — a repo from another project is not an orphan.

| Check | Description |
|---|---|
| **Orphaned clones** | Directories under registry paths not listed in ANY project `rwv.yaml` |
| **Dangling references** | Entries in an `rwv.yaml` pointing to paths not on disk |
| **Missing role** | `rwv.yaml` entries without a `role` field |
| **Stale lock** | Project's `rwv.lock` doesn't match current HEAD revisions |
| **Workweave drift** | Worktrees missing from a workweave or extra worktrees not in manifest |
| **Integration checks** | Each integration's check hook reports tool availability, stale config, etc. (see [Integrations](./integrations.md)) |

### `rwv lock`

Lock snapshots the active project's repo versions. It reads HEAD from each repo (regular clones in the weave, worktrees in a workweave) and writes `rwv.lock` to the project directory. If a tag exists at HEAD, the tag name is recorded; otherwise the revision ID.

**Uncommitted changes are an error.** If any repo in the project has uncommitted changes, `rwv lock` refuses to proceed — the lock file would record a revision that doesn't reflect the actual state on disk. Use `--dirty` to bypass:

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

## Integrations

Integrations translate between repoweave's multi-repo world and ecosystem workspace formats. They generate workspace config files (`package.json`, `go.work`, `Cargo.toml`, etc.) at the workspace surface during activation, run install commands during `rwv lock`, and perform read-only checks during `rwv check`.

| Integration | Default enabled | Auto-detects | Generates (activation) | Lock hook (`rwv lock`) |
|---|---|---|---|---|
| `npm-workspaces` | yes | repos with `package.json` | `package.json` | `npm install` |
| `pnpm-workspaces` | no | repos with `package.json` | `pnpm-workspace.yaml` | `pnpm install` |
| `go-work` | yes | repos with `go.mod` | `go.work` | -- |
| `uv-workspace` | yes | repos with `pyproject.toml` | `pyproject.toml` | `uv sync` |
| `cargo-workspace` | yes | repos with `Cargo.toml` | `Cargo.toml` | `cargo generate-lockfile` |
| `gita` | yes | all repos | `gita/` config directory | -- |
| `vscode-workspace` | yes | all repos | `{project}.code-workspace` | -- |
| `static-files` | no | n/a (configured explicitly) | symlinks declared files to weave directory | -- |

See [Integrations](./integrations.md) for the workspace surface concept, generated file formats, configuration, and details on each integration.
