# npm-workspaces

Generates a `package.json` with a `workspaces` array listing every project repo (excluding `reference` repos) that contains a `package.json`. For repos that are themselves multi-package monorepos, the array is expanded with prefixed globs rather than a bare repo path — see [Multi-package repos](#multi-package-repos) below.

| | |
|---|---|
| Default enabled | yes |
| Auto-detects | repos with `package.json` |
| Generates | `package.json` |
| Install hook | `npm install` (if `npm` is on PATH) |

## Generated file

```json
{
  "name": "repoweave",
  "private": true,
  "workspaces": [
    "github/chatly/protocol",
    "github/chatly/server",
    "github/chatly/tools/packages/*"
  ]
}
```

Generated in the project directory, symlinked to the weave directory. Committable. The corresponding `package-lock.json` and `node_modules/` are produced by `npm install` — `package-lock.json` is committable persistent state, `node_modules/` is gitignored tool state.

## Multi-package repos

npm does not support nested workspaces. If a project repo's root `package.json` declares its own `workspaces` key, listing the repo root in the weave-root `package.json` would orphan its sub-packages from the install and link graph.

repoweave detects this case and expands each such repo into one entry per member glob, prefixed with the repo path:

- **Array form** — `"workspaces": ["packages/*", "./clients/ts"]` in `github/chatly/tools/package.json` produces:
  ```
  github/chatly/tools/packages/*
  github/chatly/tools/clients/ts
  ```
  Leading `./` in member globs is stripped during prefixing.

- **Object form** — `"workspaces": {"packages": ["packages/*"], "nohoist": [...]}` reads from `.packages`; only the `packages` array is used, and the same prefixing applies.

Repos whose root `package.json` does **not** declare a `workspaces` key keep the single `<repo-path>` entry (existing behavior).

## Install hook

Runs `npm install` during `rwv activate` to update `package-lock.json` and `node_modules/`. Suppress with `rwv activate --no-install`.

## Deactivation

Removes the generated `package.json`. Does not remove `node_modules/` or `package-lock.json`.

## Check

Warns if repos with `package.json` exist but `npm` is not on PATH.

## Related

- [pnpm-workspaces](./pnpm-workspaces.md) — alternative for projects using pnpm
- [add an integration](../../how-to/add-an-integration.md) — switching ecosystems
