# npm-workspaces

Merges a managed `workspaces` region into a project-level `package.json` listing every active member repo (excluding `reference` repos) that contains a `package.json`. For member repos that are themselves multi-package monorepos, the array is expanded with prefixed globs rather than a bare repo path — see [Multi-package repos](#multi-package-repos) below.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | member repos with `package.json` |
| Manages | `package.json` (hybrid merge) |
| Generates | `package-lock.json`, written by `npm install` ([commit policy](./index.md#committed-files-and-committability)) |
| Install hook | `npm install` |

## Configuration

Enabled by default. The table takes only `enabled`:

```toml
[integrations.npm-workspaces]
enabled = false
```

## Managed and generated files

`rwv` manages only the workspace membership keys inside `projects/<project>/package.json`:

| Key | Ownership | Behavior |
|---|---|---|
| `workspaces` (array form) or `workspaces.packages` (object form) | `Author` | Updated on authoring verbs (`rwv add`, `rwv remove`, `rwv update`, `rwv role`, `rwv doctor --fix`) when `"x-repoweave"` is present or `workspaces` is absent. When `workspaces` is an object on disk, `rwv` owns only `workspaces.packages` and preserves sibling keys (`nohoist`, etc.). |
| `name` (`"<project>"`), `private` (`true`) | `DefaultOnly` | Seeded only when absent on a fresh file; never overwritten or stripped on deactivation. |
| `"x-repoweave": { "managed": true }` | Marker | Top-level JSON marker recording `rwv` ownership of the `workspaces` region. |

All other keys (`scripts`, `dependencies`, `devDependencies`, `engines`, `overrides`, etc.) are user-owned and preserved across authoring and deactivation passes.

```json
{
  "name": "web-app",
  "private": true,
  "workspaces": [
    "github/chatly/protocol",
    "github/chatly/server",
    "github/chatly/tools/packages/*"
  ],
  "x-repoweave": {
    "managed": true
  }
}
```

Written in `projects/<project>/package.json` and surfaced at the weave root via symlink. Committable. The corresponding `package-lock.json` and `node_modules/` are produced by `npm install` — `node_modules/` is gitignored tool state; `package-lock.json` is fully rwv-owned, and its commit-vs-gitignore status is the operator's choice — see [Committed files and committability](./index.md#committed-files-and-committability).

### Multi-package repos

npm does not support nested workspaces. If a member repo's root `package.json` declares its own `workspaces` key, listing the repo root in the weave-root `package.json` would orphan its sub-packages from the install and link graph.

repoweave detects this case and expands each such member repo into one entry per member glob, prefixed with the repo path:

- **Array form** — `"workspaces": ["packages/*", "./clients/ts"]` in `github/chatly/tools/package.json` produces:
  ```
  github/chatly/tools/packages/*
  github/chatly/tools/clients/ts
  ```
  Leading `./` in member globs is stripped during prefixing.

- **Object form** — `"workspaces": {"packages": ["packages/*"], "nohoist": [...]}` reads from `.packages`; only the `packages` array is used, and the same prefixing applies.

Member repos whose root `package.json` does **not** declare a `workspaces` key keep the single `<repo-path>` entry.

## Install hook

Runs `npm install` at the weave root to update `node_modules/` and `package-lock.json` ([commit policy](./index.md#committed-files-and-committability)). Which verbs run it, and how `--no-materialize` skips it, is the same for every integration: see [Hook points](./index.md#hook-points). With `npm` absent from `PATH` the hook is skipped with a `tool-missing` warning and the verb continues; an `npm install` that runs and fails is an error.

## Deactivation

Gated on the `"x-repoweave"` marker: strips `workspaces` (or `workspaces.packages`, pruning `workspaces` if empty) and `"x-repoweave"`, and deletes `package.json` only if no other keys remain. `name`, `private`, and all user-authored keys survive, so a file `rwv` seeded keeps `name` and `private`. An unmarked user-held `package.json` and its lockfile are left untouched. `node_modules/` is never removed.

- **Disabled** (`enabled = false`): `rwv materialize` strips the marked region, removes `package-lock.json`, and removes both weave-root links.
- **Last npm member removed**: the authoring verb strips the marked `workspaces` key on the same rule and leaves `package-lock.json` in place. The weave-root links stay; `rwv doctor` reports them as `surfacing` warnings, and `rwv materialize --remove-undeclared-links` removes them.

## Diagnostics

Reported by `rwv doctor`. Verify rows also print as warnings on `rwv activate`, `rwv materialize`, `rwv workweave create` and the activation a `rwv fetch` performs; see [Hook points](./index.md#hook-points).

| Kind | Severity | Condition | Repair |
|---|---|---|---|
| [`tool-missing`](../doctor-findings.md#tool-missing) | warning | Member repos with `package.json` are present, but `npm` is not on `PATH`. | Install `npm`. |
| [`managed-file-missing`](../doctor-findings.md#managed-file-missing) | warning | `package.json` is absent while npm member repos are present. | `rwv doctor --fix` |
| [`managed-file-drift`](../doctor-findings.md#managed-file-drift) | warning | `"x-repoweave"` is present, but `workspaces` differs from what `rwv.toml` declares, or no npm member repos remain and the marked `workspaces` key is still there. | `rwv doctor --fix` |
| [`managed-file-user-held`](../doctor-findings.md#managed-file-user-held) | warning | `package.json` has a `workspaces` key without `"x-repoweave"`. `rwv` leaves the file untouched. | Add `"x-repoweave": { "managed": true }`, or remove `workspaces` to hand ownership to `rwv`. |
| [`disabled-integration-artifact`](../doctor-findings.md#disabled-integration-artifact) | warning | The integration is disabled, and the marked `package.json` region or `package-lock.json` is still on disk. | `rwv materialize` |

## Related

- [pnpm-workspaces](./pnpm-workspaces.md) — alternative for projects using pnpm
- [add an integration](../../how-to/add-an-integration.md) — switching ecosystems
