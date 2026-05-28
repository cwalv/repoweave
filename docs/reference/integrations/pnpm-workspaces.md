# pnpm-workspaces

Generates a `pnpm-workspace.yaml` listing every project repo (excluding `reference` repos) that contains a `package.json`.

| | |
|---|---|
| Default enabled | no (opt-in) |
| Auto-detects | repos with `package.json` |
| Generates | `pnpm-workspace.yaml` |
| Install hook | `pnpm install` (if `pnpm` is on PATH) |

Disabled by default. Enable explicitly in `rwv.yaml` for projects using pnpm:

```yaml
integrations:
  npm-workspaces:
    enabled: false
  pnpm-workspaces:
    enabled: true
```

## Generated file

```yaml
packages:
  - github/chatly/protocol
  - github/chatly/server
  - github/chatly/web
```

Generated in the project directory, symlinked to the weave directory. Committable. The corresponding `pnpm-lock.yaml` and `node_modules/` are produced by `pnpm install` — `pnpm-lock.yaml` is committable persistent state, `node_modules/` is gitignored tool state.

## Install hook

Runs `pnpm install` during `rwv activate` to update `pnpm-lock.yaml` and `node_modules/`. Suppress with `rwv activate --no-install`.

## Deactivation

Removes `pnpm-workspace.yaml`. Does not remove `node_modules/` or `pnpm-lock.yaml`.

## Check

Warns if repos with `package.json` exist but `pnpm` is not on PATH.

## Related

- [npm-workspaces](./npm-workspaces.md) — the default npm-ecosystem integration
- [add an integration](../../how-to/add-an-integration.md) — switching ecosystems
