# Integrations reference

Integrations translate between repoweave's multi-repo model (repos, projects, roles) and one ecosystem's workspace format (`package.json`, `go.work`, `Cargo.toml`, etc.).

For the conceptual frame, see the [workspace lens](../../explanation/lenses/workspace.md). For enabling/disabling integrations in `rwv.toml`, see [add an integration](../../how-to/add-an-integration.md). Integrations ship with `rwv`; to request a new one, open a [GitHub issue](https://github.com/cwalv/repoweave/issues).

## Built-in integrations

| Integration | Default enabled | Auto-detects | Manages / Generates | Install hook (`rwv activate`, `rwv materialize`) |
|---|---|---|---|---|
| [`npm-workspaces`](./npm-workspaces.md) | yes | member repos with `package.json` | `package.json` (hybrid), `package-lock.json` | `npm install` |
| [`pnpm-workspaces`](./pnpm-workspaces.md) | no | member repos with `package.json` | `pnpm-workspace.yaml` (hybrid), `pnpm-lock.yaml` | `pnpm install` |
| [`go-work`](./go-work.md) | yes | member repos with `go.mod` | `go.work` (hybrid), `go.work.sum` | — |
| [`uv-workspace`](./uv-workspace.md) | yes | member repos with `pyproject.toml` | `pyproject.toml` (hybrid), `uv.lock` | `uv sync` |
| [`cargo-workspace`](./cargo-workspace.md) | yes | member repos with `Cargo.toml` | `Cargo.toml` (hybrid), `Cargo.lock` | `cargo fetch` (`cargo generate-lockfile` when no lock exists yet) |
| [`gita`](./gita.md) | no (opt-in) | all active member repos | `gita/repos.csv`, `gita/groups.csv` | — |
| [`vscode-workspace`](./vscode-workspace.md) | yes | all member repos | `{project}.code-workspace` (hybrid) | — |
| [`static-files`](./static-files.md) | no | n/a (configured explicitly) | symlinks declared files to weave root | — |

## Hook points

Integration work is split across authoring, surfacing, materialization, and inspection:

- **Authoring hooks** (run during `rwv add`, `rwv remove`, `rwv update`, `rwv role`, and `rwv doctor --fix`) — merge managed keys into hybrid config files or write fully-owned config files in `projects/<project>/`. This is the *write path*.
- **Surfacing & materialization** (`rwv activate` and `rwv materialize`) — `rwv activate <project>` (primary only) switches `.rwv-active`, verifies Axis-2 content drift without re-authoring managed files, creates Axis-1 weave-root symlinks (`<file> -> projects/<project>/<file>`), and runs install hooks. `rwv materialize` (primary or workweave) strips disabled-integration artifacts, settles attested-file drift (`--regenerate-drifted` / `--adopt-drifted`), repairs missing symlinks, and runs install hooks.
- **Install hooks** (run during `rwv activate`, `rwv materialize`, `rwv fetch`, and authoring verbs unless `--no-materialize` is passed) — run ecosystem install commands (`npm install`, `pnpm install`, `uv sync`, `cargo fetch`) to materialize the state implied by current membership and already-recorded pins. A lock file gains what new membership requires; an existing pinned version does not move. Advancing a dependency is done with the ecosystem's own update command, never by activation.
- **Check & verify hooks** (run during `rwv doctor` and context verbs) — read-only inspection. Per-integration `check()` and `verify()` cover tool availability, member compatibility, and Axis-2 content drift. Axis-1 surfacing-symlink integrity at the weave root is verified framework-wide by `rwv doctor`.

| Lifecycle event | Trigger | Axis-1 (weave-root symlinks) | Axis-2 (project-dir config) | Ecosystem lockfiles |
|---|---|---|---|---|
| **Authoring** | `rwv add`, `remove`, `update`, `role`, `doctor --fix` | Refreshed for active project | Merges managed keys into hybrid files; writes fully-owned configs | Created/updated by install hook (unless `--no-materialize`) |
| **Emptied membership** | Last matching member repo removed (or made `reference`) | Removed when managed file is deleted | Strips marker-gated managed keys; deletes file only if no user content remains (`doctor` reports orphaned region as drift until stripped) | Preserved (lockfiles are build products; delete manually if unwanted) |
| **Project switch** | `rwv activate <project>` | Replaces outgoing project's weave-root symlinks with incoming project's | Read-only `verify()` on incoming project; outgoing project's committed files in `projects/<old>/` are untouched | Updated by install hook for incoming project (unless `--no-materialize`) |
| **Materialization** | `rwv materialize` | Repairs missing symlinks; `--remove-undeclared-links` prunes stale links | Strips disabled-integration regions; settles attested-file drift (`--regenerate-drifted` / `--adopt-drifted`) | Updated by install hook; owned lockfiles of disabled integrations removed |
| **Workweave teardown** | `rwv workweave <project> delete <name>` | Removed with workweave directory | Runs internal `deactivate()` hook on the workweave's project checkout (strips marked regions, preserves user keys) | Owned lockfiles removed in the workweave checkout |

For the normative ownership contract (Axis 1 surfacing, Axis 2 content ownership, hybrid-merge invariants, and marker-gated generate-vs-verify dispatch), see [file-ownership](../../explanation/joints/file-ownership.md).

## Committed files and committability

Ecosystem files live in the project directory (symlinked to the weave or workweave root). Committability depends on content ownership:

- **Fully rwv-owned** files (gita CSVs, ecosystem lockfiles) are regenerated in full on each
  authoring pass. They are safe to gitignore or whole-delete.
- **Hybrid** files (`Cargo.toml`, `pyproject.toml`, `pnpm-workspace.yaml`, `go.work`,
  `package.json`, `*.code-workspace`) are user-authored source that rwv manages a declared region
  within. They **must be committed** — user content (profiles, lints, overrides, replace
  directives) survives only because the file lives in the weave repo. Gitignoring a hybrid file
  would destroy user content on re-activate. rwv merges its managed keys into the existing file;
  it never whole-writes or whole-deletes it.

Ecosystem lock files (`package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.work.sum`, `Cargo.lock`) are produced by the ecosystem tools and pin exact external-dependency versions — at activation's install step where the integration has an install hook, or whenever the tool itself runs where it has none (go-work has no hook; `go.work.sum` appears when Go tooling runs). Like other fully rwv-owned files, rwv does not mandate either policy: commit them for a checkout that's reproducible straight from the weave repo, or gitignore them to regenerate on the next `rwv activate` or `rwv materialize`.

Each lock file is surfaced as a link that exists before the file does, so the tool's write lands in the project directory. A tool that renames its first lock over that link (pnpm), or any tool run where the link is missing (after an `rm package-lock.json`, say), leaves a real file at the weave root instead. `add`, `remove`, `update`, `role`, `materialize` and `rwv doctor --fix` take that file into the project directory and restore the link, when the project's own copy is absent, identical, committed with no edit since (so version control can give it back), or, for a lock rwv records (`Cargo.lock`), still the generation it recorded; otherwise both are named and neither is touched. A gitignored lock is never committed, so one that differs from the root's is named unless rwv recorded it. `rwv activate` only names it.

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

## Build orchestration

Build orchestration tools (Nx, Turborepo) add three capabilities on top of ecosystem workspace files:

| Capability | What it does | When you need it |
|---|---|---|
| Dependency-aware task ordering | Builds `protocol` before `web` because `web` imports from `protocol` | Multiple packages with interdependent build steps |
| Caching | Skips re-running tasks when inputs haven't changed | Slow builds, CI optimization |
| Affected analysis | Runs only packages changed since a base ref | Large workspaces |

These tools consume the same workspace structure that rwv integrations generate and surface. Adding `nx.json` or `turbo.json` requires zero restructuring — they discover packages from `package.json` workspaces, `go.work`, etc. Use the [`static-files`](./static-files.md) integration to place `turbo.json` or `nx.json` at the weave root.

For most projects, ecosystem workspace commands are sufficient without an orchestrator:

| Ecosystem | Cross-package command | Dependency ordering | Filtering |
|---|---|---|---|
| **npm** | `npm run test --workspaces` | No | `npm run test -w pkg-name` |
| **pnpm** | `pnpm -r run test` | Yes (topological) | `pnpm --filter @scope/*` |
| **Go** | `go test ./...` (with `go.work`) | Native | N/A |
| **Cargo** | `cargo test --workspace` | Yes | `cargo test -p my-crate` |
| **uv** | `uv run --all-packages pytest` | Yes | `uv run --package my-pkg pytest` |
