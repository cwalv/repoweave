# uv-workspace

Merges `[tool.uv.workspace]` and `[tool.uv.sources]` entries into a project-level `pyproject.toml` listing every active member repo (excluding `reference` repos) that contains a `pyproject.toml`.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | member repos with `pyproject.toml` |
| Manages | `pyproject.toml` (hybrid merge) |
| Generates | `uv.lock`, written by `uv sync` ([commit policy](./index.md#committed-files-and-committability)) |
| Install hook | `uv sync` |

## Configuration

Enabled by default. The table takes only `enabled`:

```toml
[integrations.uv-workspace]
enabled = false
```

## Managed and generated files

`rwv` owns only these keys inside `projects/<project>/pyproject.toml`:

| Key | Ownership | Behavior |
|---|---|---|
| `[tool.uv.workspace].members` | `Author` | Decorated with `# managed by rwv`. Sorted list of active member repo paths containing `pyproject.toml`. |
| `[tool.uv.sources].<pkg>` (`{ workspace = true }`) | `Author` | One entry per member repo, keyed by that member's own `[project].name` in `<member>/pyproject.toml` (members without `[project].name` are skipped with a warning). User-authored non-workspace sources (`git`, `url`, `index`, `path`) inside `[tool.uv.sources]` are preserved and never overwritten. |
| `[tool.uv].package` (`false`) | `DefaultOnly` | Written only when absent so `uv sync` accepts a virtual workspace root; never overwrites an existing value and is not stripped on deactivation. |

All other tables (`[project]`, `[build-system]`, `[tool.ruff]`, `[tool.pytest]`, etc.) and comments are user-owned and survive authoring and deactivation unmodified.

```toml
[tool.uv]
package = false

[tool.uv.workspace]
# managed by rwv
members = ["github/chatly/protocol", "github/chatly/server"]

[tool.uv.sources]
chatly-protocol = { workspace = true }
chatly-server = { workspace = true }
```

Written in `projects/<project>/pyproject.toml` and surfaced at the weave root via symlink. Committable. The corresponding `uv.lock` and `.venv/` are produced by `uv sync` — `.venv/` is gitignored tool state; `uv.lock` is fully rwv-owned, and its commit-vs-gitignore status is the operator's choice — see [Committed files and committability](./index.md#committed-files-and-committability).

### Version constraints inside the workspace

uv's `workspace = true` silently overrides version constraints, so version mismatches only surface *outside* the workspace. This is different from Cargo/Go/npm, which catch constraint mismatches in-workspace. Test the published artifact in a separate environment before announcing a release.

## Install hook

Runs `uv sync` at the weave root to update `.venv/` and `uv.lock` ([commit policy](./index.md#committed-files-and-committability)). Which verbs run it, and how `--no-materialize` skips it, is the same for every integration: see [Hook points](./index.md#hook-points). With `uv` absent from `PATH` the hook is skipped with a `tool-missing` warning and the verb continues; a `uv sync` that runs and fails is an error.

### Polyglot cargo+uv: a compiled extension can go stale

A repo carrying both `Cargo.toml` and `pyproject.toml` — a maturin/pyo3 bindings crate — is claimed by `cargo-workspace` and `uv-workspace` at once, with no conflict between the generated files. `uv sync` drives maturin, which drives cargo, and the result installs as an editable Python package backed by a compiled extension module.

This install hook reacts to a **resolution event** — a dependency added, removed, or re-pinned — not to Rust source edited inside an already-resolved package (see [install hooks](./index.md#hook-points): the hook's mandate is current membership plus already-recorded pins, nothing else). Editing the crate's `.rs` file and leaving the Python package's own version untouched is not a resolution event, so the compiled extension is not rebuilt: Python keeps running whatever was compiled at the last reinstall, and both `rwv materialize` and a plain `uv sync` report success anyway. Only `uv sync --reinstall-package <name>` forces the rebuild.

The source can advance this way through `rwv sync-to`, a `git pull` inside the member, or hand-editing the file directly — the install hook's contract is identical regardless of how source arrived, so this is not specific to rwv.

The layer that does see every source advance is **import time**, not install time: [`maturin-import-hook`](https://github.com/PyO3/maturin-import-hook) rebuilds an editable-installed maturin package on `import` when its Rust source (or a path dependency, at any depth) has changed since the last build. Enable it once per `uv`-managed environment:

```bash
uv add --dev maturin_import_hook
uv run -m maturin_import_hook site install --detect-uv
```

`site install` writes a `sitecustomize.py`, so the staleness check runs at every interpreter start in that environment with nothing to add per script; `--detect-uv` has the rebuild itself invoked through `uv`. Trade-off: every import of an editable-installed maturin package now pays that check — negligible once the build is current, a rebuild when it is not — so treat it as a development-environment convenience and keep it out of anything shipped: `MATURIN_IMPORT_HOOK_ENABLED=0` disables it without removing the `install()` call or the site hook, for a production image built from a dev virtualenv.

rwv does not run or configure this hook. The install hook's contract is scoped to membership and pins; tracking source edits inside an already-resolved package is import/build-time work the ecosystem's own tooling already does.

## Deactivation

Gated on the `# managed by rwv` marker on `[tool.uv.workspace].members`: strips `members` and `{ workspace = true }` entries in `[tool.uv.sources]`, prunes empty `[tool.uv.workspace]`, `[tool.uv.sources]`, `[tool.uv]`, and `[tool]` tables, and deletes `pyproject.toml` only when the document is otherwise empty. Preserves `[project]`, `[tool.ruff]`, `[tool.uv].package`, and user-authored source entries (`git`, `url`, `index`, `path`), so a file `rwv` seeded keeps `[tool.uv] package = false`. Unmarked `pyproject.toml` files are left untouched, and `.venv/` is never removed.

- **Disabled** (`enabled = false`): `rwv materialize` strips the marked region, removes `uv.lock`, and removes both weave-root links.
- **Last Python member removed**: the authoring verb strips the marked region on the same rule and leaves `uv.lock` in place. The weave-root links stay; `rwv doctor` reports them as `surfacing` warnings, and `rwv materialize --remove-undeclared-links` removes them.

## Diagnostics

Reported by `rwv doctor`. Verify rows also print as warnings on `rwv activate`, `rwv materialize`, `rwv workweave create` and the activation a `rwv fetch` performs; see [Hook points](./index.md#hook-points).

| Kind | Severity | Condition | Repair |
|---|---|---|---|
| [`tool-missing`](../doctor-findings.md#tool-missing) | warning | Member repos with `pyproject.toml` are present, but `uv` is not on `PATH`. | Install `uv`. |
| [`managed-file-missing`](../doctor-findings.md#managed-file-missing) | warning | `pyproject.toml` is absent while Python member repos are present. | `rwv doctor --fix` |
| [`managed-file-drift`](../doctor-findings.md#managed-file-drift) | warning | `# managed by rwv` is present on `members`, but `[tool.uv.workspace].members` differs from what `rwv.toml` declares, or no Python member repos remain and the marked region is still there. | `rwv doctor --fix` |
| [`managed-file-user-held`](../doctor-findings.md#managed-file-user-held) | warning | `[tool.uv.workspace].members` is present without `# managed by rwv`. `rwv` leaves the file untouched. | Add `# managed by rwv` above `members`, or remove `[tool.uv.workspace]` to hand ownership to `rwv`. |
| [`disabled-integration-artifact`](../doctor-findings.md#disabled-integration-artifact) | warning | The integration is disabled, and the marked region or `uv.lock` is still on disk. | `rwv materialize` |

## Related

- [cargo-workspace](./cargo-workspace.md) — the other half of a maturin/PyO3 bindings crate
- [add an integration](../../how-to/add-an-integration.md) — enabling and disabling integrations
