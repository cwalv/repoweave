# pnpm-workspaces

Generates a `pnpm-workspace.yaml` listing every project repo (excluding `reference` repos) that contains a `package.json`. For repos that are themselves multi-package monorepos, the `packages:` list is expanded with prefixed globs rather than a bare repo path — see [Multi-package repos](#multi-package-repos) below.

| | |
|---|---|
| Default enabled | no (opt-in) |
| Auto-detects | repos with `package.json` |
| Generates | `pnpm-workspace.yaml` |
| Install hook | `pnpm install` (if `pnpm` is on PATH) |

Disabled by default. Enable explicitly in `rwv.toml` for projects using pnpm:

```toml
[integrations.npm-workspaces]
enabled = false

[integrations.pnpm-workspaces]
enabled = true
```

## Generated file

```yaml
packages:
  - github/chatly/protocol
  - github/chatly/server
  - github/chatly/tools/packages/*
```

Generated in the project directory, symlinked to the weave directory. Committable. The corresponding `pnpm-lock.yaml` and `node_modules/` are produced by `pnpm install` — `pnpm-lock.yaml` is committable persistent state, `node_modules/` is gitignored tool state.

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

Repos without a `pnpm-workspace.yaml`, or whose `pnpm-workspace.yaml` has no `packages:` list, keep the single `<repo-path>` entry (existing behavior).

## Install hook

Runs `pnpm install` during `rwv activate` to update `pnpm-lock.yaml` and `node_modules/`. Suppress with `rwv activate --no-install`.

## Deactivation

Removes `pnpm-workspace.yaml`. Does not remove `node_modules/` or `pnpm-lock.yaml`.

## Check

Warns if repos with `package.json` exist but `pnpm` is not on PATH.

## Related

- [npm-workspaces](./npm-workspaces.md) — the default npm-ecosystem integration
- [add an integration](../../how-to/add-an-integration.md) — switching ecosystems
