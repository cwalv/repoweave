# pnpm-workspaces

Merges a managed `packages:` list into a project-level `pnpm-workspace.yaml` listing every active member repo (excluding `reference` repos) that contains a `package.json`. For member repos that are themselves multi-package monorepos, the `packages:` list is expanded with prefixed globs rather than a bare repo path — see [Multi-package repos](#multi-package-repos) below.

| | |
|---|---|
| Default enabled | no (opt-in) |
| Auto-detects | member repos with `package.json` |
| Manages | `pnpm-workspace.yaml` (hybrid merge) |
| Install hook | `pnpm install` (if `pnpm` is on `PATH`) |

Disabled by default. Enable explicitly in `rwv.toml` for projects using pnpm:

```toml
[integrations.npm-workspaces]
enabled = false

[integrations.pnpm-workspaces]
enabled = true
```

## Managed keys

`rwv` owns only the `# managed by repoweave`-marked `packages:` block inside `projects/<project>/pnpm-workspace.yaml`. User-authored keys (`catalog:`, `catalogs:`, `overrides:`, `peerDependencyRules:`, `hoistPattern:`, `onlyBuiltDependencies:`, etc.) and comments outside the `packages:` block are preserved byte-for-byte across authoring and deactivation passes.

## Generated file

```yaml
# managed by repoweave
packages:
  - github/chatly/protocol
  - github/chatly/server
  - github/chatly/tools/packages/*
```

Written in `projects/<project>/pnpm-workspace.yaml` and surfaced at the weave root via symlink. Committable. The corresponding `pnpm-lock.yaml` and `node_modules/` are produced by `pnpm install` — `node_modules/` is gitignored tool state; `pnpm-lock.yaml` is fully rwv-owned, and its commit-vs-gitignore status is the operator's choice — see [Committed files and committability](./index.md#committed-files-and-committability).

## Multi-package repos

pnpm reads workspace member globs from `pnpm-workspace.yaml`'s `packages:` key — it does **not** use `package.json`'s `workspaces` key. A member repo that is itself a multi-package monorepo declares its own sub-packages in its own `pnpm-workspace.yaml` (a `packages:` sequence). Listing the repo root in the weave-root `pnpm-workspace.yaml` would orphan its sub-packages from pnpm's install and link graph.

repoweave detects this case: if a member repo's root contains a `pnpm-workspace.yaml` with a `packages:` list, the repo root is replaced by one entry per glob, prefixed with the repo path.

**Example** — `github/chatly/tools/pnpm-workspace.yaml` contains:

```yaml
packages:
  - packages/*
  - ./clients/ts
```

The weave-root `pnpm-workspace.yaml` emits:

```
github/chatly/tools/packages/*
github/chatly/tools/clients/ts
```

Leading `./` in member globs is stripped during prefixing.

Member repos without a `pnpm-workspace.yaml`, or whose `pnpm-workspace.yaml` has no `packages:` list, keep the single `<repo-path>` entry.

## Install hook

Runs `pnpm install` at the weave root during `rwv activate`, `rwv materialize`, `rwv fetch`, and authoring verbs (unless `--no-materialize` is passed) to update `node_modules/` and `pnpm-lock.yaml` ([commit policy](./index.md#committed-files-and-committability)).

## Deactivation

Gated on the `# managed by repoweave` marker above `packages:`: strips the `packages:` block and the marker comment while preserving `catalog:`, `overrides:`, and all other user-authored keys and comments. Deletes `pnpm-workspace.yaml` only when nothing user-authored remains. Leaves unmarked `pnpm-workspace.yaml` files, `node_modules/`, and `pnpm-lock.yaml` ([commit policy](./index.md#committed-files-and-committability)) untouched.

## Check

`rwv doctor` reports:

- **`tool-missing`** (warning): member repos with `package.json` are present, but `pnpm` is not on `PATH`.

## Verify (drift)

`rwv doctor` and context verbs (`rwv activate`, `rwv fetch`, `rwv workweave create`) run the read-only Axis-2 verify pass on `projects/<project>/pnpm-workspace.yaml`:

- **MISSING** (`managed-file-missing`, `safe_to_fix = true`): `pnpm-workspace.yaml` is absent while member repos with `package.json` are present (`rwv doctor --fix` creates it).
- **DRIFT** (`managed-file-drift`, `safe_to_fix = true`): `# managed by repoweave` is present, but `packages:` differs from what `rwv.toml` declares (or no matching member repos remain and the marked `packages:` block is orphaned).
- **USER-HELD** (`managed-file-drift`, `safe_to_fix = false`): `pnpm-workspace.yaml` has a `packages:` key without the `# managed by repoweave` marker. `rwv` leaves the file untouched; add `# managed by repoweave` above `packages:` or remove `packages:` to hand ownership to `rwv`.

## Related

- [npm-workspaces](./npm-workspaces.md) — the default npm-ecosystem integration
- [add an integration](../../how-to/add-an-integration.md) — switching ecosystems
