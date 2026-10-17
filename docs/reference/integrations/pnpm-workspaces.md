# pnpm-workspaces

Merges a managed `packages:` list into a project-level `pnpm-workspace.yaml` listing every active member repo (excluding `reference` repos) that contains a `package.json`. For member repos that are themselves multi-package monorepos, the `packages:` list is expanded with prefixed globs rather than a bare repo path — see [Multi-package repos](#multi-package-repos) below.

| | |
|---|---|
| Default enabled | no (opt-in) |
| Auto-detects | member repos with `package.json` |
| Manages | `pnpm-workspace.yaml` (hybrid merge) |
| Generates | `pnpm-lock.yaml`, written by `pnpm install` ([commit policy](./index.md#committed-files-and-committability)) |
| Install hook | `pnpm install` |

## Configuration

Disabled by default. Enable it in `rwv.toml` for projects using pnpm, and turn off `npm-workspaces`, which claims the same member repos:

```toml
[integrations.npm-workspaces]
enabled = false

[integrations.pnpm-workspaces]
enabled = true
```

The table takes only `enabled`. Then run `rwv doctor --fix` to author `pnpm-workspace.yaml` and `rwv materialize` to strip what `npm-workspaces` left — see [add an integration](../../how-to/add-an-integration.md#switch-ecosystems-npm--pnpm).

## Managed and generated files

`rwv` owns only the `# managed by repoweave`-marked `packages:` block inside `projects/<project>/pnpm-workspace.yaml`. User-authored keys (`catalog:`, `catalogs:`, `overrides:`, `peerDependencyRules:`, `hoistPattern:`, `onlyBuiltDependencies:`, etc.) and comments outside the `packages:` block are preserved byte-for-byte across authoring and deactivation passes.

```yaml
# managed by repoweave
packages:
  - github/chatly/protocol
  - github/chatly/server
  - github/chatly/tools/packages/*
```

Written in `projects/<project>/pnpm-workspace.yaml` and surfaced at the weave root via symlink. Committable. The corresponding `pnpm-lock.yaml` and `node_modules/` are produced by `pnpm install` — `node_modules/` is gitignored tool state; `pnpm-lock.yaml` is fully rwv-owned, and its commit-vs-gitignore status is the operator's choice — see [Committed files and committability](./index.md#committed-files-and-committability).

### Multi-package repos

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

Runs `pnpm install` at the weave root to update `node_modules/` and `pnpm-lock.yaml` ([commit policy](./index.md#committed-files-and-committability)). Which verbs run it, and how `--no-materialize` skips it, is the same for every integration: see [Hook points](./index.md#hook-points). With `pnpm` absent from `PATH` the hook is skipped with a `tool-missing` warning and the verb continues; a `pnpm install` that runs and fails is an error.

## Deactivation

Gated on the `# managed by repoweave` marker above `packages:`: strips the `packages:` block and the marker comment while preserving `catalog:`, `overrides:`, and all other user-authored keys and comments. Deletes `pnpm-workspace.yaml` only when nothing user-authored remains. Unmarked `pnpm-workspace.yaml` files are left untouched, and `node_modules/` is never removed.

- **Disabled** (`enabled = false`): `rwv materialize` strips the marked block, removes `pnpm-lock.yaml`, and removes both weave-root links.
- **Last matching member removed**: the authoring verb strips the marked block on the same rule and leaves `pnpm-lock.yaml` in place. The weave-root links stay; `rwv doctor` reports them as `surfacing` warnings, and `rwv materialize --remove-undeclared-links` removes them.

## Diagnostics

Reported by `rwv doctor`. Verify rows also print on the context verbs listed under [Hook points](./index.md#hook-points).

| Kind | Severity | Condition | Repair |
|---|---|---|---|
| [`tool-missing`](../doctor-findings.md#tool-missing) | warning | Member repos with `package.json` are present, but `pnpm` is not on `PATH`. | Install `pnpm`. |
| [`managed-file-missing`](../doctor-findings.md#managed-file-missing) | warning | `pnpm-workspace.yaml` is absent while member repos with `package.json` are present. | `rwv doctor --fix` |
| [`managed-file-drift`](../doctor-findings.md#managed-file-drift) | warning | `# managed by repoweave` is present, but `packages:` differs from what `rwv.toml` declares, or no matching member repos remain and the marked block is still there. | `rwv doctor --fix` |
| [`managed-file-user-held`](../doctor-findings.md#managed-file-user-held) | warning | `pnpm-workspace.yaml` has a `packages:` key without the `# managed by repoweave` marker. `rwv` leaves the file untouched. | Add `# managed by repoweave` above `packages:`, or remove `packages:` to hand ownership to `rwv`. |
| [`disabled-integration-artifact`](../doctor-findings.md#disabled-integration-artifact) | warning | The integration is disabled, and the marked `packages:` block or `pnpm-lock.yaml` is still on disk. | `rwv materialize` |

## Related

- [npm-workspaces](./npm-workspaces.md) — the default npm-ecosystem integration
- [add an integration](../../how-to/add-an-integration.md) — switching ecosystems
