# Integrations

Rather than hardcoding knowledge of each ecosystem and tool, `rwv` uses **integrations** — pluggable units that each know how to derive config for one tool from the repo list. Each integration participates in two hook points:

- **Activation hooks** (run during weave creation, sync, add, remove) — generate config files, run install commands, or do nothing. This is the write path.
- **Check hooks** (`rwv check`) — read-only inspection. Verify the environment is healthy, report missing tools, stale config, etc.

## How integrations work

Each integration provides:

1. **A name** — unique identifier (e.g., `npm-workspaces`).
2. **A default enabled state** — whether the integration runs without explicit opt-in.
3. **Activation logic** — receives the resolved repo list (paths, URLs, roles) and its config; generates files, runs commands, or does nothing.
4. **Deactivation logic** — removes generated files. Called during weave deletion.
5. **Check logic** — receives the same inputs; returns issues and warnings without changing state.

When a weave is created or synced, integrations are run: the deactivation hook cleans up first, then the activation hook generates fresh config. Each integration auto-detects relevant repos — if none are found, it does nothing.

`rwv check` runs check hooks across all enabled integrations as part of its broader convention audit.

Ecosystem integrations auto-detect repos with the relevant manifest file. If none are found, they do nothing — no config file is generated, no error is raised.

### Generated files are persistent

Generated ecosystem files live in the primary directory (or the weave directory for weaves). They are **committable, not ephemeral** — they are regenerated on weave creation, sync, or `rwv add`, but they persist between runs and can be committed to version control.

Ecosystem lock files (`package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.sum`, `Cargo.lock`) are produced by the ecosystem tools during the install step. These are important persistent state that pins exact dependency versions within each ecosystem. They should be committed alongside the ecosystem workspace configs.

Some integrations also run external tools (`npm install`, `pnpm install`, `uv sync`) that create their own tool state (`node_modules/`, `.venv/`). Tool state directories are gitignored and managed by the ecosystem tool, not by `rwv`. If tool state gets corrupted or out of sync, `rwv weave {project} --sync` regenerates config and re-runs install commands.

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

- **root** — the primary directory or weave directory (the directory where generated files should be written).
- **project** — the active project name (may be multi-segment, e.g., `chatly/web-app`).
- **repos** — repo entries from the project's `rwv.yaml`: `{local_path: {type, url, version, role, ...}}`.
- **config** — per-integration config from the `integrations` key in `rwv.yaml`.
- **all_repos_on_disk** — all git repos found on disk under registry directories (relative paths). Computed once, shared across integrations.
- **all_project_paths** — all project paths (e.g., `['web-app', 'mobile-app']`). Computed once, shared across integrations.

The `active_repos()` method filters out `reference` repos, which should not be included in ecosystem workspace configs (they are read-only and not part of the build graph).

## Built-in integrations

| Integration | Default enabled | Auto-detects | Generates | Post-generate command |
|---|---|---|---|---|
| `npm-workspaces` | yes | repos with `package.json` | root `package.json` | `npm install` |
| `pnpm-workspaces` | no | repos with `package.json` | `pnpm-workspace.yaml` | `pnpm install` |
| `go-work` | yes | repos with `go.mod` | `go.work` | -- |
| `uv-workspace` | yes | repos with `pyproject.toml` | root `pyproject.toml` | `uv sync` |
| `cargo-workspace` | yes | repos with `Cargo.toml` | root `Cargo.toml` | -- |
| `gita` | yes | all repos | `gita/` directory | -- |
| `vscode-workspace` | yes | all repos | `{project}.code-workspace` | -- |

## npm-workspaces

Generates a root `package.json` with a `workspaces` array listing every project repo (excluding `reference` repos) that contains a `package.json`. After writing the file, runs `npm install` if `npm` is on PATH.

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

This file lives at the root of the primary directory (or weave directory). It is committable. The corresponding `package-lock.json` and `node_modules/` are produced by `npm install` — `package-lock.json` is committable persistent state, `node_modules/` is gitignored tool state.

### Deactivation

Removes the generated `package.json`. Does not remove `node_modules/` or `package-lock.json`.

### Check

Warns if repos with `package.json` exist but `npm` is not on PATH.

## pnpm-workspaces

Generates a `pnpm-workspace.yaml` file listing every project repo (excluding `reference` repos) that contains a `package.json`. After writing the file, runs `pnpm install` if `pnpm` is on PATH.

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

This file lives at the root of the primary directory (or weave directory). It is committable. The corresponding `pnpm-lock.yaml` and `node_modules/` are produced by `pnpm install` — `pnpm-lock.yaml` is committable persistent state, `node_modules/` is gitignored tool state.

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

This file lives at the root of the primary directory (or weave directory). It is committable. The corresponding `go.sum` is produced by the Go toolchain and is also committable persistent state.

### Deactivation

Removes `go.work`. Does not remove `go.sum`.

### Check

No checks currently. Could warn if `go` is not on PATH when Go repos are present.

## uv-workspace

Generates a root `pyproject.toml` with a `[tool.uv.workspace]` section listing every project repo (excluding `reference` repos) that contains a `pyproject.toml`. After writing the file, runs `uv sync` if `uv` is on PATH.

### Generated file

```toml
# Generated by rwv — do not edit
[tool.uv.workspace]
members = [
    "github/chatly/protocol",
    "github/chatly/server",
]
```

This file lives at the root of the primary directory (or weave directory). It is committable. The corresponding `uv.lock` and `.venv/` are produced by `uv sync` — `uv.lock` is committable persistent state, `.venv/` is gitignored tool state.

### Deactivation

Removes the generated `pyproject.toml`. Does not remove `.venv/` or `uv.lock`.

### Check

Warns if repos with `pyproject.toml` exist but `uv` is not on PATH.

## cargo-workspace

Generates a root `Cargo.toml` with a `[workspace]` section listing every project repo (excluding `reference` repos) that contains a `Cargo.toml`. Uses resolver version 2.

### Generated file

```toml
# Generated by rwv — do not edit

[workspace]
members = ["github/chatly/protocol", "github/chatly/server"]
resolver = "2"
```

This file lives at the root of the primary directory (or weave directory). It is committable. The corresponding `Cargo.lock` and `target/` are produced by Cargo — `Cargo.lock` is committable persistent state, `target/` is gitignored tool state.

### Deactivation

Removes the generated `Cargo.toml` only if it starts with the generated-file header (to avoid deleting a hand-written `Cargo.toml`).

### Check

No checks currently. Could warn if `cargo` is not on PATH when Rust repos are present.

## gita

[gita](https://github.com/nosarthur/gita) provides the multi-repo dashboard (`gita ll`), cross-repo git delegation (`gita super`), cross-repo shell commands (`gita shell`), groups, and context scoping. Rather than reimplement these in `rwv`, we use gita directly via this integration.

The activation hook generates gita's config files into a `gita/` directory inside the primary (or weave) directory, scoped to the project's repos. Point gita at this directory via `GITA_PROJECT_HOME`:

```bash
# .envrc in primary or weave dir
export GITA_PROJECT_HOME="$PWD/gita"
```

`GITA_PROJECT_HOME` replaces (not supplements) gita's default config directory. Each weave gets its own gita config — gita commands are always scoped to the current context's repos.

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

- **path**: absolute path to the repo (or worktree in a weave)
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

Generates a `{project}.code-workspace` file in the primary (or weave) directory. Uses a single-root workspace (the directory itself) with git settings configured for the multi-repo layout.

The file is named after the project (e.g., `web-app.code-workspace`), making the project visible in the VS Code title bar.

### Generated file

```json
{
  "folders": [
    { "path": ".", "name": "web-app (primary)" }
  ],
  "settings": {
    "git.autoRepositoryDetection": "subFolders",
    "git.repositoryScanMaxDepth": 3
  }
}
```

- **Single root folder** at `"."` — the primary or weave directory.
- **Folder name** includes the context (e.g., `"web-app (primary)"`, `"web-app (agent-42)"`).
- **`git.autoRepositoryDetection: subFolders`** prevents VS Code from walking up to a parent repo.
- **`git.repositoryScanMaxDepth: 3`** ensures VS Code discovers repos at the `registry/owner/repo` depth.
- **Merge on activate** — only `folders` and managed `settings` keys are replaced. Other keys (extensions, launch configs, other settings) are preserved, so user customizations survive re-activation. This merge behavior is meaningful because the generated file persists in the directory and can be customized.

### Deactivation

Removes any `.code-workspace` files from the directory.

### Check

Validates that the `.code-workspace` file exists as a regular file (not a symlink) in the directory.

## Build orchestration

Build orchestration tools (Nx, Turborepo) add three capabilities on top of the workspace files that activation hooks generate:

| Capability | What it does | When you need it |
|---|---|---|
| **Dependency-aware task ordering** | Builds `protocol` before `web` because `web` imports from `protocol` | Multiple packages with build steps that depend on each other |
| **Caching** | Skips re-running tasks when inputs haven't changed | Slow builds, CI optimization |
| **Affected analysis** | Determines which packages changed since a base ref, runs only those | Large workspaces where running everything is too slow |

These tools consume the same workspace structure that activation hooks generate. Adding `nx.json` or `turbo.json` at root requires zero restructuring — they discover packages from `package.json` workspaces, `go.work`, etc.

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
}
```

`IntegrationContext` provides the primary or weave directory (as `root`), project name, resolved repo entries (with paths, URLs, roles), per-integration config from `rwv.yaml`, all repos found on disk across registries (`all_repos_on_disk`), and all project paths (`all_project_paths`). The disk-scan fields are computed once and shared across integrations.

New integrations are registered in `registry.rs`.
