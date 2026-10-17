# Integrations reference

Integrations translate between repoweave's multi-repo model (repos, projects, roles) and one ecosystem's workspace format (`package.json`, `go.work`, `Cargo.toml`, etc.).

For the conceptual frame, see the [workspace lens](../../explanation/lenses/workspace.md). For enabling/disabling integrations in `rwv.toml`, see [add an integration](../../how-to/add-an-integration.md). Integrations ship with `rwv`; to request a new one, open a [GitHub issue](https://github.com/cwalv/repoweave/issues).

## Built-in integrations

| Integration | Default enabled | Auto-detects | Manages / Generates | Install hook |
|---|---|---|---|---|
| [`npm-workspaces`](./npm-workspaces.md) | yes | member repos with `package.json` | `package.json` (hybrid), `package-lock.json` | `npm install` |
| [`pnpm-workspaces`](./pnpm-workspaces.md) | no | member repos with `package.json` | `pnpm-workspace.yaml` (hybrid), `pnpm-lock.yaml` | `pnpm install` |
| [`go-work`](./go-work.md) | yes | member repos with `go.mod` | `go.work` (hybrid); surfaces `go.work.sum` | — |
| [`uv-workspace`](./uv-workspace.md) | yes | member repos with `pyproject.toml` | `pyproject.toml` (hybrid), `uv.lock` | `uv sync` |
| [`cargo-workspace`](./cargo-workspace.md) | yes | member repos with `Cargo.toml` | `Cargo.toml` (hybrid), `Cargo.lock` | `cargo fetch` (`cargo generate-lockfile` when no lock exists yet) |
| [`gita`](./gita.md) | no (opt-in) | all active member repos | `gita/repos.csv`, `gita/groups.csv` | — |
| [`vscode-workspace`](./vscode-workspace.md) | yes | — (every project) | `{project}.code-workspace` (hybrid) | — |
| [`static-files`](./static-files.md) | no | n/a (configured explicitly) | surfaces declared files at the weave root | — |

## Hook points

Integration work is split across authoring, surfacing, install hooks, and inspection. The verbs below are the same for every integration.

- **Authoring** — merges managed keys into hybrid config files and writes fully-owned files in `projects/<project>/`. This is the *write path*. Run by the authoring verbs `rwv add`, `rwv remove`, `rwv update`, and `rwv role`, at the primary or in a workweave, and by `rwv doctor --fix` when doctor reports something it can fix.
- **Surfacing** — creates the weave-root symlinks (`<file> -> projects/<project>/<file>`) for the files the project's integrations declare. Run by `rwv activate <project>` (primary only; it also switches `.rwv-active`), `rwv materialize` (primary or workweave; repairs missing links), `rwv workweave create`, the activation a `rwv fetch` performs, and the authoring verbs.
- **Install hooks** — run the ecosystem install command (`npm install`, `pnpm install`, `uv sync`, `cargo fetch`) to materialize the state implied by current membership and already-recorded pins. A lock file gains what new membership requires; an existing pinned version does not move. Advancing a dependency is done with the ecosystem's own update command, never by activation.
  - Run by `rwv materialize`, `rwv activate`, `rwv workweave create`, the activation a `rwv fetch` performs, `rwv init --adopt`, and the authoring verbs at the primary.
  - Run by `rwv doctor --fix` only when it regenerates.
  - Inside a workweave the authoring verbs run no hooks; run `rwv materialize` there.
  - `--no-materialize` skips them on every verb that runs them except `rwv materialize` and `rwv doctor --fix`, where running them is the point.
  - A hook whose tool is not on `PATH` is skipped with a `tool-missing` warning naming `rwv materialize`, and the other hooks still run. A tool that runs and fails is an error: the verb exits 1.
- **Check & verify** — read-only inspection. Per-integration checks cover tool availability, settings, and member compatibility; verify covers content drift in the files the integration manages. `rwv doctor` runs both. `rwv activate`, `rwv workweave create`, and the activation a `rwv fetch` performs run verify and print its findings as `[warning (drift)]` lines; an in-place `rwv fetch` that activates nothing does not. Surfacing-link integrity at the weave root is checked by `rwv doctor` for every integration alike.

| Lifecycle event | Trigger | Weave-root symlinks | Project-dir config | Ecosystem lockfiles |
|---|---|---|---|---|
| **Authoring** | `rwv add`, `remove`, `update`, `role`; `doctor --fix` when it regenerates | Refreshed for active project | Merges managed keys into hybrid files; writes fully-owned configs | Created/updated by install hook at the primary (unless `--no-materialize`); not inside a workweave |
| **Emptied membership** | Last matching member repo removed (or made `reference`) | Left in place; `rwv doctor` reports them as `surfacing` until `rwv materialize --remove-undeclared-links` removes them | Strips marker-gated managed keys; deletes the file only if no user content remains. After a hand edit of `rwv.toml`, `rwv doctor` reports the leftover marked region as `managed-file-drift` until `rwv doctor --fix` strips it | Preserved (lockfiles are build products; delete manually if unwanted) |
| **Project switch** | `rwv activate <project>` | Replaces outgoing project's weave-root symlinks with incoming project's | Verify only on the incoming project; outgoing project's committed files in `projects/<old>/` are untouched | Updated by install hook for incoming project (unless `--no-materialize`) |
| **Workweave creation** | `rwv workweave <project> create <name>` | Surfaced for the project | Verify only; nothing is authored | Updated by install hook in the new workweave (unless `--no-materialize`) |
| **Materialization** | `rwv materialize` | Repairs missing symlinks; `--remove-undeclared-links` prunes stale links | Strips disabled-integration regions; settles attested-file drift (`--regenerate-drifted` / `--adopt-drifted`) | Updated by install hook; owned lockfiles of disabled integrations removed |
| **Workweave teardown** | `rwv workweave <project> delete <name>` | Removed with workweave directory | Each enabled integration strips its marked regions from the workweave's project checkout before the directory is removed | Removed with workweave directory |

For the normative ownership contract (Axis 1 surfacing, Axis 2 content ownership, hybrid-merge invariants, and marker-gated generate-vs-verify dispatch), see [file-ownership](../../explanation/joints/file-ownership.md).

## Committed files and committability

Ecosystem files live in the project directory (symlinked to the weave or workweave root). Committability depends on content ownership:

- **Fully rwv-owned** files (gita CSVs, ecosystem lockfiles) are rewritten in full: the gita
  CSVs by each authoring pass, the lockfiles by the ecosystem tool an install hook runs. They are
  safe to gitignore or whole-delete.
- **Hybrid** files (`Cargo.toml`, `pyproject.toml`, `pnpm-workspace.yaml`, `go.work`,
  `package.json`, `*.code-workspace`) are user-authored source that rwv manages a declared region
  within. They **must be committed** — user content (profiles, lints, overrides, replace
  directives) reaches another weave or workweave only because the file is committed. rwv merges its
  managed keys into the existing file; it never whole-writes it, and deletes it only when
  stripping leaves no user content.

Ecosystem lock files (`package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.work.sum`, `Cargo.lock`) are produced by the ecosystem tools and pin exact external-dependency versions — at the install step where the integration has an install hook, or whenever the tool itself runs where it has none (go-work has no hook; `go.work.sum` appears when Go tooling runs). Like other fully rwv-owned files, rwv does not mandate either policy: commit them for a checkout that's reproducible straight from the weave repo, or gitignore them to regenerate on the next `rwv activate` or `rwv materialize`.

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

Each table accepts `enabled` plus the keys its integration reads:

| Integration | Keys besides `enabled` |
|---|---|
| `npm-workspaces`, `pnpm-workspaces`, `uv-workspace`, `gita` | — |
| `go-work` | `go-version` |
| `cargo-workspace` | `exclude`, `members`, `patch`, `patch-surface`, `workspace-package`, `rustc-wrapper` |
| `vscode-workspace` | `hide-dotfiles` |
| `static-files` | `files` |

An unknown key in one of these tables, or a table naming an integration that does not exist, is refused when `rwv.toml` is parsed: every verb that reads the manifest exits 1 with the TOML line and column, the accepted names, and the nearest match (`rwv status` skips the project with a warning instead). A key whose value has the wrong type is reported by `rwv doctor` as `malformed-settings`. In a `cargo-workspace` or `static-files` table it also stops every verb that runs install hooks from running them until it is corrected.

Changing `enabled` takes effect at the next authoring pass or `rwv doctor --fix` for a newly enabled integration, and at `rwv materialize` for a newly disabled one — see [add an integration](../../how-to/add-an-integration.md).

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
