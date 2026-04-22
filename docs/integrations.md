# Integrations

## What ecosystem tools see

The weave directory (or workweave directory) is what ecosystem tools see. npm sees a directory with a `package.json` listing workspace packages. Go sees a directory with a `go.work` listing modules. Cargo sees a directory with a `Cargo.toml` listing workspace members. These tools have no idea that the packages come from different git repos. They see a workspace directory with packages in it — nothing more.

Integrations are the translation layer between repoweave's multi-repo world (repos, projects, roles) and the ecosystem's workspace world (`package.json`, `go.work`, `Cargo.toml`). They read the project's `rwv.yaml` — which describes repos — and produce the ecosystem workspace files in the weave directory. The result: ecosystem tools work exactly as they would in a monorepo, because from their perspective, it *is* a workspace directory with packages.

## How integrations work

Rather than hardcoding knowledge of each ecosystem and tool, `rwv` uses **integrations** — pluggable units that each know how to derive config for one tool from the repo list. Each integration participates in two hook points:

- **Activation hooks** (run during `rwv activate`, workweave creation, `rwv sync`, `rwv add`, `rwv remove`) — generate config files and symlinks, or do nothing. This is the write path.
- **Lock hooks** (run during `rwv lock`) — run install commands (`npm install`, `uv sync`, `cargo generate-lockfile`, etc.) to ensure ecosystem lock files are up to date. This is where package installation happens.
- **Check hooks** (`rwv doctor`) — read-only inspection. Verify the environment is healthy, report missing tools, stale config, etc.

Each integration provides:

1. **A name** — unique identifier (e.g., `npm-workspaces`).
2. **A default enabled state** — whether the integration runs without explicit opt-in.
3. **Activation logic** — receives the resolved repo list (paths, URLs, roles) and its config; generates workspace config files, or does nothing.
3a. **Lock logic** — receives the same inputs; runs install commands to update ecosystem lock files (`npm install`, `uv sync`, `cargo generate-lockfile`, etc.).
4. **Deactivation logic** — removes generated files. Called during workweave deletion.
5. **Check logic** — receives the same inputs; returns issues and warnings without changing state.

When a project is activated, a workweave is created, or `rwv sync` materializes repo changes, integrations are run: the deactivation hook cleans up first, then the activation hook generates fresh config. Each integration auto-detects relevant repos — if none are found, it does nothing.

`rwv doctor` runs check hooks across all enabled integrations as part of its broader convention audit.

Ecosystem integrations auto-detect repos with the relevant manifest file. If none are found, they do nothing — no config file is generated, no error is raised.

### Generated files are persistent

Generated ecosystem files live in the project directory (symlinked to the weave directory, or the workweave directory for workweaves). They are **committable, not ephemeral** — they are regenerated on workweave creation, `rwv sync`, or `rwv add`, but they persist between runs and can be committed to version control.

Ecosystem lock files (`package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.sum`, `Cargo.lock`) are produced by the ecosystem tools during the install step. These are important persistent state that pins exact dependency versions within each ecosystem. They should be committed alongside the ecosystem workspace configs.

Some integrations have lock hooks that run external tools (`npm install`, `pnpm install`, `uv sync`) during `rwv lock`. These commands create their own tool state (`node_modules/`, `.venv/`). Tool state directories are gitignored and managed by the ecosystem tool, not by `rwv`. You can also run install commands manually after `rwv activate`. If tool state gets corrupted in a workweave, delete and recreate the workweave (`rwv workweave {project} delete {name}` then `rwv workweave {project} create {name}`) to regenerate config and re-run activation hooks.

### Hook configuration in `rwv.yaml`

Integration config lives in the project's `rwv.yaml` under an `integrations` key. Only overrides need to be listed — integrations not mentioned use their own defaults:

```yaml
# projects/web-app/rwv.yaml
repositories:
  # ...

integrations:
  npm-workspaces:
    enabled: false                 # this project uses pnpm instead
  pnpm-workspaces:
    enabled: true
  go-work:
    enabled: false                 # this project doesn't use Go
```

### Integration context

Each integration receives an `IntegrationContext` with:

- **output_dir** — where generated files should be written (project directory for activate, workweave project directory for workweaves).
- **workspace_root** — where repos live on disk (used for manifest detection like finding `package.json`).
- **project** — the active project name (may be multi-segment, e.g., `chatly/web-app`).
- **repos** — repo entries from the project's `rwv.yaml`: `{local_path: {type, url, version, role, ...}}`.
- **config** — per-integration config from the `integrations` key in `rwv.yaml`.
- **all_repos_on_disk** — all git repos found on disk under registry directories (relative paths). Computed once, shared across integrations.
- **all_project_paths** — all project paths (e.g., `['web-app', 'mobile-app']`). Computed once, shared across integrations.

The `active_repos()` method filters out `reference` repos, which should not be included in ecosystem workspace configs (they are read-only and not part of the build graph). The `detect_repos_with_manifest()` helper finds active repos containing a given file (e.g., `package.json`), using `workspace_root` for detection.

## Built-in integrations

| Integration | Default enabled | Auto-detects | Generates | Lock hook (runs during `rwv lock`) |
|---|---|---|---|---|
| `npm-workspaces` | yes | repos with `package.json` | `package.json` | `npm install` |
| `pnpm-workspaces` | no | repos with `package.json` | `pnpm-workspace.yaml` | `pnpm install` |
| `go-work` | yes | repos with `go.mod` | `go.work` | -- |
| `uv-workspace` | yes | repos with `pyproject.toml` | `pyproject.toml` | `uv sync` |
| `cargo-workspace` | yes | repos with `Cargo.toml` | `Cargo.toml` | `cargo generate-lockfile` |
| `gita` | yes | all repos | `gita/` directory | -- |
| `vscode-workspace` | yes | all repos | `{project}.code-workspace` | -- |
| `static-files` | no | n/a (configured explicitly) | symlinks declared files to weave directory | -- |

## npm-workspaces

Generates a `package.json` with a `workspaces` array listing every project repo (excluding `reference` repos) that contains a `package.json`. The lock hook (run during `rwv lock`) runs `npm install` if `npm` is on PATH to update `package-lock.json` and `node_modules/`. To install immediately after activation, run `npm install` manually.

### Generated file

```json
{
  "name": "repoweave",
  "private": true,
  "workspaces": [
    "github/chatly/protocol",
    "github/chatly/server",
    "github/chatly/web"
  ]
}
```

This file is generated in the project directory and symlinked to the weave directory (or workweave directory). It is committable. The corresponding `package-lock.json` and `node_modules/` are produced by `npm install` — `package-lock.json` is committable persistent state, `node_modules/` is gitignored tool state.

### Deactivation

Removes the generated `package.json`. Does not remove `node_modules/` or `package-lock.json`.

### Check

Warns if repos with `package.json` exist but `npm` is not on PATH.

## pnpm-workspaces

Generates a `pnpm-workspace.yaml` file listing every project repo (excluding `reference` repos) that contains a `package.json`. The lock hook (run during `rwv lock`) runs `pnpm install` if `pnpm` is on PATH to update `pnpm-lock.yaml` and `node_modules/`. To install immediately after activation, run `pnpm install` manually.

Disabled by default. Enable explicitly in `rwv.yaml` for projects that use pnpm instead of npm:

```yaml
integrations:
  npm-workspaces:
    enabled: false
  pnpm-workspaces:
    enabled: true
```

### Generated file

```yaml
packages:
  - github/chatly/protocol
  - github/chatly/server
  - github/chatly/web
```

This file is generated in the project directory and symlinked to the weave directory (or workweave directory). It is committable. The corresponding `pnpm-lock.yaml` and `node_modules/` are produced by `pnpm install` — `pnpm-lock.yaml` is committable persistent state, `node_modules/` is gitignored tool state.

### Deactivation

Removes `pnpm-workspace.yaml`. Does not remove `node_modules/` or `pnpm-lock.yaml`.

### Check

Warns if repos with `package.json` exist but `pnpm` is not on PATH.

## go-work

Generates a `go.work` file listing every project repo (excluding `reference` repos) that contains a `go.mod`.

### Generated file

```
go 1.21

use (
    ./github/chatly/protocol
    ./github/chatly/server
)
```

This file is generated in the project directory and symlinked to the weave directory (or workweave directory). It is committable. The corresponding `go.sum` is produced by the Go toolchain and is also committable persistent state.

### Deactivation

Removes `go.work`. Does not remove `go.sum`.

### Check

No checks currently. Could warn if `go` is not on PATH when Go repos are present.

## uv-workspace

Generates a `pyproject.toml` with a `[tool.uv.workspace]` section listing every project repo (excluding `reference` repos) that contains a `pyproject.toml`. The lock hook (run during `rwv lock`) runs `uv sync` if `uv` is on PATH to update `uv.lock` and `.venv/`. To install immediately after activation, run `uv sync` manually.

### Generated file

```toml
# Generated by rwv — do not edit
[tool.uv.workspace]
members = [
    "github/chatly/protocol",
    "github/chatly/server",
]
```

This file is generated in the project directory and symlinked to the weave directory (or workweave directory). It is committable. The corresponding `uv.lock` and `.venv/` are produced by `uv sync` — `uv.lock` is committable persistent state, `.venv/` is gitignored tool state.

### Deactivation

Removes the generated `pyproject.toml`. Does not remove `.venv/` or `uv.lock`.

### Check

Warns if repos with `pyproject.toml` exist but `uv` is not on PATH.

## cargo-workspace

Generates a `Cargo.toml` with a `[workspace]` section listing every project repo (excluding `reference` repos) that contains a `Cargo.toml`. Uses resolver version 2.

### Generated file

```toml
# Generated by rwv — do not edit

[workspace]
members = ["github/chatly/protocol", "github/chatly/server"]
resolver = "2"
```

This file is generated in the project directory and symlinked to the weave directory (or workweave directory). It is committable. The corresponding `Cargo.lock` and `target/` are produced by Cargo — `Cargo.lock` is committable persistent state, `target/` is gitignored tool state.

### Deactivation

Removes the generated `Cargo.toml` only if it starts with the generated-file header (to avoid deleting a hand-written `Cargo.toml`).

### Check

No checks currently. Could warn if `cargo` is not on PATH when Rust repos are present.

## gita

[gita](https://github.com/nosarthur/gita) provides the multi-repo dashboard (`gita ll`), cross-repo git delegation (`gita super`), cross-repo shell commands (`gita shell`), groups, and context scoping. Rather than reimplement these in `rwv`, we use gita directly via this integration.

The activation hook generates gita's config files into a `gita/` directory inside the weave (or workweave) directory, scoped to the project's repos. Point gita at this directory via `GITA_PROJECT_HOME`:

```bash
# .envrc in weave or workweave dir
export GITA_PROJECT_HOME="$PWD/gita"
```

`GITA_PROJECT_HOME` replaces (not supplements) gita's default config directory. Each workweave gets its own gita config — gita commands are always scoped to the current context's repos.

For build/test/lint across packages, prefer the ecosystem's own workspace commands (`npm run --workspaces test`, `pnpm -r run test`, `cargo test --workspace`) — they understand package dependency ordering. gita's value is at the **git layer**: status, bulk fetch/pull, seeing which repos have uncommitted work.

### Generated files

The hook writes two CSV files into `gita/`:

**`repos.csv`** — header row, then one line per repo:

```csv
path,name,flags
/home/dev/workspace/github/chatly/server,server,
/home/dev/workspace/github/chatly/web,web,
/home/dev/workspace/github/chatly/protocol,protocol,
```

- **path**: absolute path to the repo (or worktree in a workweave)
- **name**: display name used in gita commands (basename of the repo path)
- **flags**: extra args inserted after `git` in delegated commands (currently unused)

**`groups.csv`** — header row, groups derived from role annotations:

```csv
group,repos
fork,engine-io
primary,server web protocol
```

This enables role-scoped gita commands:

```bash
gita ll primary          # dashboard for primary repos only
gita super primary pull  # pull only primary repos
```

### Deactivation

Removes the entire `gita/` directory.

### Check

Warns if `gita` is not on PATH.

### Why not `gita freeze` / `gita clone`?

gita has its own serialization format (`gita freeze` outputs CSV with URL, name, path, branch) and can bootstrap from it via `gita clone`. This overlaps with `rwv lock` / `rwv fetch`, but `gita freeze` records branch names rather than pinned SHAs, so it's less precise for reproducibility. repoweave's `rwv.lock` also carries role annotations and YAML structure. The two mechanisms would overlap awkwardly, so the gita integration only generates the config files that gita needs at runtime — it doesn't use gita's own freeze/clone flow.

## vscode-workspace

Generates a `{project}.code-workspace` file in the weave (or workweave) directory. Uses a single-root workspace (the directory itself) with git settings configured for the multi-repo layout.

The file is named after the project (e.g., `web-app.code-workspace`), making the project visible in the VS Code title bar.

### Generated file

```json
{
  "folders": [
    { "path": ".", "name": "web-app (weave)" }
  ],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3
  }
}
```

- **Single root folder** at `"."` — the weave or workweave directory.
- **Folder name** includes the context (e.g., `"web-app (weave)"`, `"web-app (agent-42)"`).
- **`git.autoRepositoryDetection: subFolders`** prevents VS Code from walking up to a parent repo.
- **`git.repositoryScanMaxDepth: 3`** ensures VS Code discovers repos at the `registry/owner/repo` depth.
- **Merge on activate** — only `folders` and managed `settings` keys are replaced. Other keys (extensions, launch configs, other settings) are preserved, so user customizations survive re-activation. This merge behavior is meaningful because the generated file persists in the directory and can be customized.

### Deactivation

Removes any `.code-workspace` files from the directory.

### Check

Validates that the `.code-workspace` file exists as a regular file (not a symlink) in the directory.

## static-files

Symlinks declared files from the project directory to the weave directory on activation. This is the escape hatch for top-level config files that don't belong to any ecosystem integration — build orchestrator configs (`turbo.json`, `nx.json`), linter configs (`.eslintrc.json`, `.prettierrc`), or anything else that tools expect at the weave directory.

Disabled by default. Enable explicitly in `rwv.yaml` with a list of files:

```yaml
integrations:
  static-files:
    enabled: true
    files: [turbo.json, nx.json, .eslintrc.json, .prettierrc]
```

Each file listed in `files` must exist in the project directory (e.g., `projects/web-app/turbo.json`). On activation, the integration symlinks each file to the weave directory so that tools like Turborepo or Nx find them where they expect.

### How it works

Unlike ecosystem integrations that auto-detect repos and generate config, static-files does no generation and no detection. The files are hand-written and committed in the project directory. The integration simply makes them visible at the weave directory via symlinks.

If a declared file is missing from the project directory, the integration prints a warning but activation still succeeds — the missing file is skipped.

### Deactivation

Symlinks are removed by the activation framework (any symlink at the weave directory pointing into `projects/` is cleaned up). The original files in the project directory are untouched.

### Check

Warns if any declared file is missing from the project directory.

### Examples

#### Turborepo with npm workspaces

A project using Turborepo for build caching alongside npm workspaces:

```yaml
# projects/web-app/rwv.yaml
repositories:
  github/chatly/protocol:
    url: git@github.com:chatly/protocol.git
  github/chatly/server:
    url: git@github.com:chatly/server.git
  github/chatly/web:
    url: git@github.com:chatly/web.git

integrations:
  static-files:
    enabled: true
    files: [turbo.json]
```

The `turbo.json` file lives alongside `rwv.yaml` in the project directory:

```json
{
  "$schema": "https://turbo.build/schema.json",
  "tasks": {
    "build": {
      "dependsOn": ["^build"],
      "outputs": ["dist/**"]
    },
    "test": {
      "dependsOn": ["build"]
    },
    "lint": {}
  }
}
```

After activation, the weave directory contains:
- `package.json` — generated by the `npm-workspaces` integration
- `turbo.json` — symlinked by the `static-files` integration

Turborepo discovers packages from `package.json` workspaces and reads its pipeline config from `turbo.json` — both at the weave directory, exactly where it expects them.

#### Linter and formatter configs (`.eslintrc.json`, `.prettierrc`)

ESLint and Prettier expect their config at the weave directory to apply across all packages. With npm or pnpm workspaces, linting commands run from the weave directory — the config must be there too.

```yaml
# projects/web-app/rwv.yaml
integrations:
  static-files:
    enabled: true
    files: [.eslintrc.json, .prettierrc]
```

```json
// projects/web-app/.eslintrc.json
{
  "root": true,
  "extends": ["eslint:recommended", "plugin:@typescript-eslint/recommended"],
  "parser": "@typescript-eslint/parser"
}
```

```json
// projects/web-app/.prettierrc
{
  "semi": false,
  "singleQuote": true,
  "tabWidth": 2
}
```

The `"root": true` in `.eslintrc.json` is important — it tells ESLint to stop walking up the directory tree, so it doesn't accidentally pick up a config from a parent directory outside the weave.

After activation, `eslint .` and `prettier --check .` run from the weave directory and apply consistently across every package in the workspace.

#### Nx build orchestrator (`nx.json`)

[Nx](https://nx.dev/) is a build orchestrator that adds dependency-aware task ordering, caching, and affected-build analysis on top of npm/pnpm workspaces. Like Turborepo, it reads a config file from the weave directory.

```yaml
# projects/web-app/rwv.yaml
integrations:
  pnpm-workspaces:
    enabled: true
  npm-workspaces:
    enabled: false
  static-files:
    enabled: true
    files: [nx.json]
```

```json
// projects/web-app/nx.json
{
  "$schema": "./node_modules/nx/schemas/nx-schema.json",
  "targetDefaults": {
    "build": {
      "dependsOn": ["^build"],
      "cache": true
    },
    "test": {
      "cache": true
    }
  },
  "defaultBase": "main"
}
```

After activation:
- `pnpm-workspace.yaml` — generated by the `pnpm-workspaces` integration (Nx works best with pnpm)
- `nx.json` — symlinked by the `static-files` integration

Nx discovers packages from `pnpm-workspace.yaml` and reads task configuration from `nx.json`. Run `pnpm exec nx run-many -t build` to build all affected packages in dependency order.

#### Toolchain versions (`.mise.toml`)

[mise](https://mise.jdx.dev/) reads `.mise.toml` from the directory you `cd` into, activating the declared toolchain versions. Placing it at the weave directory ensures everyone working on the project uses the same Node, Go, Rust, or Python version — without per-repo `.nvmrc` or `.tool-versions` files scattered across repos.

```yaml
# projects/web-app/rwv.yaml
integrations:
  static-files:
    enabled: true
    files: [.mise.toml]
```

```toml
# projects/web-app/.mise.toml
[tools]
node = "22"
go = "1.22"
rust = "1.78"
python = "3.12"
```

After activation, `mise install` at the weave directory installs the declared versions. Combine with direnv (`use mise` in `.envrc`) for automatic activation on `cd`.

#### Environment activation (`.envrc`)

[direnv](https://direnv.net/) reads `.envrc` from the directory you enter, activating environment variables and shell configuration automatically. At the weave directory, `.envrc` sets up the full development environment in one step.

```yaml
# projects/web-app/rwv.yaml
integrations:
  static-files:
    enabled: true
    files: [.envrc]
```

```bash
# projects/web-app/.envrc
use mise                                      # activate toolchain versions from .mise.toml
export GITA_PROJECT_HOME="$PWD/gita"         # point gita at the generated config
export DATABASE_URL="postgres://localhost/web_app_dev"
export NODE_ENV="development"
```

After activation, entering the weave directory automatically activates toolchains, sets `GITA_PROJECT_HOME` so gita commands are scoped to the project, and exports any other environment variables developers need. Run `direnv allow` once after creating or modifying `.envrc`.

Note: `.envrc` files often contain developer-local paths or credentials. Consider what belongs in the committed `.envrc` versus a `.envrc.local` that each developer maintains separately.

#### Makefile or justfile

A `Makefile` or [`justfile`](https://github.com/casey/just) at the weave directory provides a consistent command interface across the multi-repo workspace — shortcuts for common sequences that span repos or require a specific order.

```yaml
# projects/web-app/rwv.yaml
integrations:
  static-files:
    enabled: true
    files: [justfile]
```

```makefile
# projects/web-app/justfile
default:
    @just --list

# Install all dependencies
install:
    rwv lock
    npm install

# Run tests across all packages
test:
    npm run test --workspaces

# Lint and format check
lint:
    eslint .
    prettier --check .

# Lock repos and ecosystem deps, then commit the lock file
lock:
    rwv lock
    cd projects/web-app && git add rwv.lock && git commit -m "chore: update lock"
```

After activation, `just install`, `just test`, and `just lint` work from the weave directory. A justfile (or Makefile) is particularly useful for documenting the commands that require multi-step sequences — like running `rwv lock` before `npm install`, or running lint after a format pass.

## Build orchestration

Build orchestration tools (Nx, Turborepo) add three capabilities on top of the workspace files that activation hooks generate:

| Capability | What it does | When you need it |
|---|---|---|
| **Dependency-aware task ordering** | Builds `protocol` before `web` because `web` imports from `protocol` | Multiple packages with build steps that depend on each other |
| **Caching** | Skips re-running tasks when inputs haven't changed | Slow builds, CI optimization |
| **Affected analysis** | Determines which packages changed since a base ref, runs only those | Large workspaces where running everything is too slow |

These tools consume the same workspace structure that activation hooks generate. Adding `nx.json` or `turbo.json` requires zero restructuring — they discover packages from `package.json` workspaces, `go.work`, etc. Use the `static-files` integration to place `turbo.json` or `nx.json` at the weave directory (see [static-files](#static-files) above).

For most projects, ecosystem workspace commands are sufficient without a build orchestrator:

| Ecosystem | Cross-package command | Dependency ordering | Filtering |
|---|---|---|---|
| **npm** | `npm run test --workspaces` | No | `npm run test -w pkg-name` |
| **pnpm** | `pnpm -r run test` | Yes (topological) | `pnpm --filter @scope/*` |
| **Go** | `go test ./...` (with go.work) | Native | N/A |
| **Cargo** | `cargo test --workspace` | Yes | `cargo test -p my-crate` |
| **uv** | `uv run --all-packages pytest` | Yes | `uv run --package my-pkg pytest` |

### gita vs. build orchestration

gita operates at the **git/repo layer** — it doesn't know about packages or build graphs. Ecosystem tools and build orchestrators operate at the **package layer** — they don't know about git status or multi-repo state. The two are complementary:

| Task | Better tool | Why |
|---|---|---|
| Run build/test across packages | **pnpm/npm/cargo/uv** (or Nx/Turbo) | Understands package dependency ordering |
| Git status across repos | **gita** | Ecosystem tools don't know about git |
| Git operations across repos | **gita** | `gita super fetch`, `gita super pull` |
| Arbitrary shell across repos | **gita** | When it's not ecosystem-specific |

## Writing custom integrations

Integrations ship with `rwv` as Rust modules in `src/integrations/`. Each implements the `Integration` trait:

```rust
pub trait Integration {
    fn name(&self) -> &str;
    fn default_enabled(&self) -> bool;

    fn activate(&self, ctx: &IntegrationContext) -> Result<()>;
    fn deactivate(&self, root: &Path) -> Result<()>;
    fn check(&self, ctx: &IntegrationContext) -> Result<Vec<Issue>>;
    fn lock(&self, ctx: &IntegrationContext) -> Result<()> { Ok(()) }
    fn generated_files(&self, ctx: &IntegrationContext) -> Vec<String> { Vec::new() }
}
```

`IntegrationContext` provides:
- **`output_dir`** — the directory where generated files should be written (project directory for activate, workweave project directory for workweaves).
- **`workspace_root`** — the directory where repos live on disk (for manifest detection via `detect_repos_with_manifest`).
- **`project`** — the active project name.
- **`repos`** — repo entries from the project's `rwv.yaml`.
- **`config`** — per-integration config from the `integrations` key in `rwv.yaml`.
- **`all_repos_on_disk`** — all git repos found on disk under registry directories. Computed once, shared across integrations.
- **`all_project_paths`** — all project paths. Computed once, shared across integrations.

The `active_repos()` method filters out `reference` repos. The `detect_repos_with_manifest()` method finds active repos containing a given manifest file (e.g., `package.json`), using `workspace_root` for detection.

New integrations are registered in `registry.rs`.
