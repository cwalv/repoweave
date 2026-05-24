# npm-workspaces

Generates a `package.json` with a `workspaces` array listing every project repo (excluding `reference` repos) that contains a `package.json`.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | repos with `package.json` |
| Generates | `package.json` |
| Lock hook | `npm install` (if `npm` is on PATH) |

## Generated file

```json
{
  "name": "repoweave",
  "private": true,
  "workspaces": [
    "github/chatly/protocol",
    "github/chatly/server",
    "github/chatly/web"
  ]
}
```

Generated in the project directory, symlinked to the weave directory. Committable. The corresponding `package-lock.json` and `node_modules/` are produced by `npm install` — `package-lock.json` is committable persistent state, `node_modules/` is gitignored tool state.

## Lock hook

Runs `npm install` during `rwv lock` to update `package-lock.json` and `node_modules/`. To install immediately after activation, run `npm install` manually.

## Deactivation

Removes the generated `package.json`. Does not remove `node_modules/` or `package-lock.json`.

## Check

Warns if repos with `package.json` exist but `npm` is not on PATH.

## Related

- [pnpm-workspaces](./pnpm-workspaces.md) — alternative for projects using pnpm
- [add an integration](../../how-to/add-an-integration.md) — switching ecosystems
