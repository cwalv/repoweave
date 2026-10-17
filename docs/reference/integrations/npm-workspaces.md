# npm-workspaces

Merges a managed `workspaces` region into a project-level `package.json` listing every active member repo (excluding `reference` repos) that contains a `package.json`. For member repos that are themselves multi-package monorepos, the array is expanded with prefixed globs rather than a bare repo path — see [Multi-package repos](#multi-package-repos) below.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | member repos with `package.json` |
| Manages | `package.json` (hybrid merge) |
| Install hook | `npm install` (if `npm` is on `PATH`) |

## Managed keys

`rwv` manages only the workspace membership keys inside `projects/<project>/package.json`:

| Key | Ownership | Behavior |
|---|---|---|
| `workspaces` (array form) or `workspaces.packages` (object form) | `Author` | Updated on authoring verbs (`rwv add`, `rwv remove`, `rwv update`, `rwv role`, `rwv doctor --fix`) when `"x-repoweave"` is present or `workspaces` is absent. When `workspaces` is an object on disk, `rwv` owns only `workspaces.packages` and preserves sibling keys (`nohoist`, etc.). |
| `name` (`"<project>"`), `private` (`true`) | `DefaultOnly` | Seeded only when absent on a fresh file; never overwritten or stripped on deactivation. |
| `"x-repoweave": { "managed": true }` | Marker | Top-level JSON marker recording `rwv` ownership of the `workspaces` region. |

All other keys (`scripts`, `dependencies`, `devDependencies`, `engines`, `overrides`, etc.) are user-owned and preserved across authoring and deactivation passes.

## Generated file

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

## Multi-package repos

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

Runs `npm install` at the weave root during `rwv activate`, `rwv materialize`, `rwv fetch`, and authoring verbs (unless `--no-materialize` is passed) to update `node_modules/` and `package-lock.json` ([commit policy](./index.md#committed-files-and-committability)).

## Deactivation

Gated on the `"x-repoweave"` marker: strips `workspaces` (or `workspaces.packages`, pruning `workspaces` if empty) and `"x-repoweave"`, and deletes `package.json` only if no other keys remain. `name`, `private`, and all user-authored keys survive. When `"x-repoweave"` was present and stripped on deactivation, the co-requisite `package-lock.json` ([commit policy](./index.md#committed-files-and-committability)) is also removed; an unmarked user-held `package.json` and its lockfile are left untouched. Does not remove `node_modules/`.

When the last npm member repo is removed from `rwv.toml`, authoring verbs strip the marked `workspaces` key on the same rule while leaving any existing lockfile in place.

## Check

`rwv doctor` reports:

- **`tool-missing`** (warning): member repos with `package.json` are present, but `npm` is not on `PATH`.

## Verify (drift)

`rwv doctor` and context verbs (`rwv activate`, `rwv fetch`, `rwv workweave create`) run the read-only Axis-2 verify pass on `projects/<project>/package.json`:

- **MISSING** (`managed-file-missing`, `safe_to_fix = true`): `package.json` is absent while npm member repos are present (`rwv doctor --fix` creates it).
- **DRIFT** (`managed-file-drift`, `safe_to_fix = true`): `"x-repoweave"` is present, but `workspaces` differs from what `rwv.toml` declares (or no npm member repos remain and the marked `workspaces` key is orphaned).
- **USER-HELD** (`managed-file-drift`, `safe_to_fix = false`): `package.json` has a `workspaces` key without `"x-repoweave"`. `rwv` leaves the file untouched; add `"x-repoweave": { "managed": true }` or remove `workspaces` to hand ownership to `rwv`.

## Related

- [pnpm-workspaces](./pnpm-workspaces.md) — alternative for projects using pnpm
- [add an integration](../../how-to/add-an-integration.md) — switching ecosystems
