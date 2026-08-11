# Integrations reference

Integrations translate between repoweave's multi-repo model (repos, projects, roles) and one ecosystem's workspace format (`package.json`, `go.work`, `Cargo.toml`, etc.).

For the conceptual frame, see the [workspace lens](../../explanation/lenses/workspace.md). For enabling/disabling integrations in `rwv.toml`, see [add an integration](../../how-to/add-an-integration.md). Integrations ship with `rwv`; to request a new one, open a [GitHub issue](https://github.com/cwalv/repoweave/issues).

## Built-in integrations

| Integration | Default enabled | Auto-detects | Generates | Install hook (during `rwv activate`) |
|---|---|---|---|---|
| [`npm-workspaces`](./npm-workspaces.md) | yes | repos with `package.json` | `package.json` | `npm install` |
| [`pnpm-workspaces`](./pnpm-workspaces.md) | no | repos with `package.json` | `pnpm-workspace.yaml` | `pnpm install` |
| [`go-work`](./go-work.md) | yes | repos with `go.mod` | `go.work` | — |
| [`uv-workspace`](./uv-workspace.md) | yes | repos with `pyproject.toml` | `pyproject.toml` | `uv sync` |
| [`cargo-workspace`](./cargo-workspace.md) | yes | repos with `Cargo.toml` | `Cargo.toml` | `cargo fetch` (`cargo generate-lockfile` when there is no lock yet) |
| [`gita`](./gita.md) | no (opt-in) | all repos | `gita/` directory | — |
| [`vscode-workspace`](./vscode-workspace.md) | yes | all repos | `{project}.code-workspace` | — |
| [`static-files`](./static-files.md) | no | n/a (configured explicitly) | symlinks declared files to weave directory | — |

## Hook points

Each integration participates in three hook points:

- **Activation hooks** (run during `rwv add`, `rwv remove`, `rwv update`, and `rwv doctor --fix`) — generate config files. This is the *write path*. Symlink surfacing is separate and runs on every activation, including `rwv activate`, `rwv fetch` and workweave creation.
- **Install hooks** (run during `rwv activate`, after config generation) — run install commands (`npm install`, `uv sync`, `cargo fetch`, etc.) to make the ecosystem state that current membership and the already-recorded pins imply real on disk. A lock file gains what new membership requires; a version it already pins does not move. Advancing a dependency is something you ask for with the ecosystem's own update command, never something activation does for you. Suppressed with `rwv activate --no-materialize`.
- **Check hooks** (run during `rwv doctor`) — read-only inspection. Verify the environment is healthy, report missing tools or stale config. Per-integration `check()` covers environment/config preconditions (Axis-2 content drift, missing tools, etc.). Surfacing-symlink integrity — whether a declared file's symlink exists and resolves correctly at the weave root — is verified **framework-wide** by a separate Axis-1 surfacing check that runs alongside per-integration checks in `rwv doctor`; it is not a per-integration `check()` responsibility.

When `rwv activate` runs, integrations perform Axis-1 surfacing: they create the symlinks that make committed project-repo files visible at the weave root. `activate` never re-authors a hybrid file's managed region; it only verifies for drift and warns. Authoring happens on the verbs that change what the files are generated from — `add`, `remove`, `update` — and on `rwv doctor --fix`. Each integration also implements a deactivation hook, invoked internally when a checkout is destroyed — deleting a workweave runs it against that workweave's own copy of the project directory. It strips managed keys and removes the file only when nothing user-authored remains; this is not a CLI verb. Switching the active project does **not** run it. A project that is no longer selected is not a project that is going away, and its ecosystem files are content committed in its own project repo, where they stay. What a switch removes is the outgoing project's *surfacing* — the weave-root symlinks onto those files — and that is the activation framework's job, not an integration's. Each integration auto-detects relevant repos. If none are found it has nothing to contribute, and the managed region follows: on an authoring verb it is stripped (same marker gate and same delete-if-nothing-user-authored rule as the deactivation hook), and `rwv doctor` reports it as drift until one runs. So removing a project's last Rust member leaves no `members` entry behind, and an integration that never had a repo to detect stays a no-op. An emptied membership reaches the hybrid region only — an ecosystem lockfile is a build product rather than a claim about who owns the workspace list, so it survives. Nothing removes it at a weave root: the deactivation hook is the only code path that does, and it runs only against the checkout of a workweave being deleted. A lockfile you no longer want is one to delete yourself.

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

Ecosystem lock files (`package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.sum`, `Cargo.lock`) are produced by the ecosystem tools during the install step and pin exact external-dependency versions. Like other fully rwv-owned files, rwv does not mandate either policy: commit them for a checkout that's reproducible straight from the project repo, or gitignore them to regenerate on the next `rwv activate`.

Tool state directories (`node_modules/`, `.venv/`, `target/`) are gitignored and managed by the ecosystem tool, not by `rwv`.

## Configuration in `rwv.toml`

Integration config lives under an `integrations` key. Only overrides need to be listed — integrations not mentioned use their own defaults:

```toml
[integrations.npm-workspaces]
enabled = false

[integrations.pnpm-workspaces]
enabled = true

[integrations.go-work]
enabled = false

[integrations.static-files]
enabled = true
files = ["turbo.json", ".eslintrc.json"]
```

## `IntegrationContext` — what hooks receive

Each integration receives an `IntegrationContext` with:

| Field | Description |
|---|---|
| `output_dir` | Where generated files are written (project directory for activate; workweave project directory for workweaves) |
| `workspace_root` | Where repos live on disk (used for manifest detection like finding `package.json`) |
| `project` | Active project name (may be multi-segment, e.g., `chatly/web-app`) |
| `repos` | Repo entries from the project's `rwv.toml` |
| `config` | Per-integration config from the `integrations` key in `rwv.toml` |
| `all_repos_on_disk` | All git repos found under registry directories. Computed once, shared across integrations |
| `all_project_paths` | All project paths (e.g., `['web-app', 'mobile-app']`). Computed once, shared |

The `active_repos()` method filters out `reference` repos (excluded from build graphs). `detect_repos_with_manifest()` finds active repos containing a given file (e.g., `package.json`), using `workspace_root`.

To request a new integration or contribute one, open a [GitHub issue](https://github.com/cwalv/repoweave/issues).

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
