# Integrations reference

Integrations translate between repoweave's multi-repo model (repos, projects, roles) and one ecosystem's workspace format (`package.json`, `go.work`, `Cargo.toml`, etc.).

For the conceptual frame, see the [workspace lens](../../explanation/lenses/workspace.md). For enabling/disabling integrations in `rwv.yaml`, see [add an integration](../../how-to/add-an-integration.md). For implementing a brand-new integration in rwv's source, see [contributing/writing-integrations](../../contributing/writing-integrations.md).

## Built-in integrations

| Integration | Default enabled | Auto-detects | Generates | Install hook (during `rwv activate`) |
|---|---|---|---|---|
| [`npm-workspaces`](./npm-workspaces.md) | yes | repos with `package.json` | `package.json` | `npm install` |
| [`pnpm-workspaces`](./pnpm-workspaces.md) | no | repos with `package.json` | `pnpm-workspace.yaml` | `pnpm install` |
| [`go-work`](./go-work.md) | yes | repos with `go.mod` | `go.work` | — |
| [`uv-workspace`](./uv-workspace.md) | yes | repos with `pyproject.toml` | `pyproject.toml` | `uv sync` |
| [`cargo-workspace`](./cargo-workspace.md) | yes | repos with `Cargo.toml` | `Cargo.toml` | `cargo generate-lockfile` |
| [`gita`](./gita.md) | no (opt-in) | all repos | `gita/` directory | — |
| [`vscode-workspace`](./vscode-workspace.md) | yes | all repos | `{project}.code-workspace` | — |
| [`static-files`](./static-files.md) | no | n/a (configured explicitly) | symlinks declared files to weave directory | — |

## Hook points

Each integration participates in three hook points:

- **Activation hooks** (run during `rwv activate`, workweave creation, `rwv sync`, `rwv add`, `rwv remove`) — generate config files and symlinks. This is the *write path*.
- **Install hooks** (run during `rwv activate`, after config generation) — run install commands (`npm install`, `uv sync`, `cargo generate-lockfile`, etc.) to keep ecosystem lock files current with the active membership. Suppressed with `rwv activate --no-install`.
- **Check hooks** (run during `rwv doctor`) — read-only inspection. Verify the environment is healthy, report missing tools or stale config.

When `rwv activate` runs, integrations perform Axis-1 surfacing: they create the symlinks that make committed project-repo files visible at the weave root. `activate` never re-authors a hybrid file's managed region; it only verifies for drift and warns. Authoring happens on intent verbs (`add`, `remove`, `update`, `lock`). `deactivate` is a separate verb — it strips managed keys and removes the file only when nothing user-authored remains. Each integration auto-detects relevant repos — if none are found, it is a no-op.

For the normative ownership contract (Axis 1 surfacing, Axis 2 content ownership, the hybrid-merge invariants, and the marker-as-generate-vs-verify-switch), see [file-ownership](../../explanation/joints/file-ownership.md).

## Committed files and committability

Ecosystem files live in the project directory (symlinked to the weave or workweave directory). Committability works differently depending on content ownership:

- **Fully rwv-owned** files (gita CSVs, ecosystem lockfiles) are regenerated in full on each
  authoring pass. They are safe to gitignore or whole-delete.
- **Hybrid** files (`Cargo.toml`, `pyproject.toml`, `pnpm-workspace.yaml`, `go.work`,
  `package.json`, `*.code-workspace`) are user-authored source that rwv manages a declared region
  within. They **must be committed** — user content (profiles, lints, overrides, replace
  directives) survives only because the file lives in the project repo. Gitignoring a hybrid file
  would destroy user content on re-activate. rwv merges its managed keys into the existing file;
  it never whole-writes or whole-deletes it.

Ecosystem lock files (`package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.sum`, `Cargo.lock`) are produced by the ecosystem tools during the install step. These pin exact external-dependency versions and should be committed alongside the workspace configs.

Tool state directories (`node_modules/`, `.venv/`, `target/`) are gitignored and managed by the ecosystem tool, not by `rwv`.

## Configuration in `rwv.yaml`

Integration config lives under an `integrations` key. Only overrides need to be listed — integrations not mentioned use their own defaults:

```yaml
integrations:
  npm-workspaces:
    enabled: false                 # this project uses pnpm instead
  pnpm-workspaces:
    enabled: true
  go-work:
    enabled: false                 # this project doesn't use Go
  static-files:
    enabled: true
    files: [turbo.json, .eslintrc.json]
```

## `IntegrationContext` — what hooks receive

Each integration receives an `IntegrationContext` with:

| Field | Description |
|---|---|
| `output_dir` | Where generated files are written (project directory for activate; workweave project directory for workweaves) |
| `workspace_root` | Where repos live on disk (used for manifest detection like finding `package.json`) |
| `project` | Active project name (may be multi-segment, e.g., `chatly/web-app`) |
| `repos` | Repo entries from the project's `rwv.yaml` |
| `config` | Per-integration config from the `integrations` key in `rwv.yaml` |
| `all_repos_on_disk` | All git repos found under registry directories. Computed once, shared across integrations |
| `all_project_paths` | All project paths (e.g., `['web-app', 'mobile-app']`). Computed once, shared |

The `active_repos()` method filters out `reference` repos (excluded from build graphs). `detect_repos_with_manifest()` finds active repos containing a given file (e.g., `package.json`), using `workspace_root`.

For the trait shape, see [contributing/writing-integrations](../../contributing/writing-integrations.md).

## Build orchestration

Build orchestration tools (Nx, Turborepo) add three capabilities on top of ecosystem workspace files:

| Capability | What it does | When you need it |
|---|---|---|
| Dependency-aware task ordering | Builds `protocol` before `web` because `web` imports from `protocol` | Multiple packages with interdependent build steps |
| Caching | Skips re-running tasks when inputs haven't changed | Slow builds, CI optimization |
| Affected analysis | Runs only packages changed since a base ref | Large workspaces |

These tools consume the same workspace structure that activation hooks generate. Adding `nx.json` or `turbo.json` requires zero restructuring — they discover packages from `package.json` workspaces, `go.work`, etc. Use the [`static-files`](./static-files.md) integration to place `turbo.json` or `nx.json` at the weave directory.

For most projects, ecosystem workspace commands are sufficient without an orchestrator:

| Ecosystem | Cross-package command | Dependency ordering | Filtering |
|---|---|---|---|
| **npm** | `npm run test --workspaces` | No | `npm run test -w pkg-name` |
| **pnpm** | `pnpm -r run test` | Yes (topological) | `pnpm --filter @scope/*` |
| **Go** | `go test ./...` (with `go.work`) | Native | N/A |
| **Cargo** | `cargo test --workspace` | Yes | `cargo test -p my-crate` |
| **uv** | `uv run --all-packages pytest` | Yes | `uv run --package my-pkg pytest` |
