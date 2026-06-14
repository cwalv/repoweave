# static-files

Symlinks declared files from the project directory to the weave directory on activation. Escape hatch for top-level config files that don't belong to any ecosystem integration — build orchestrator configs (`turbo.json`, `nx.json`), linter configs (`.eslintrc.json`, `.prettierrc`), or anything else tools expect at the weave directory.

| | |
|---|---|
| Default enabled | no |
| Auto-detects | n/a (configured explicitly) |
| Generates | symlinks declared files to weave directory |
| Lock hook | — |

## Configuration

Disabled by default. Enable explicitly in `rwv.yaml`:

```yaml
integrations:
  static-files:
    enabled: true
    files: [turbo.json, nx.json, .eslintrc.json, .prettierrc]
```

Each file listed in `files` must exist in the project directory (e.g., `projects/web-app/turbo.json`). On activation, the integration symlinks each file to the weave directory so tools find them where they expect.

## How it works

Unlike ecosystem integrations that auto-detect repos and generate config, static-files does no generation and no detection. The files are hand-written and committed in the project directory. The integration simply makes them visible at the weave directory via symlinks.

If a declared file is missing from the project directory, the integration prints a warning but activation still succeeds — the missing file is skipped.

## Deactivation

Symlinks are removed by the activation framework (any symlink at the weave directory pointing into `projects/` is cleaned up). The original files in the project directory are untouched.

## Check

Warns if any declared file is missing from the project directory.

The framework-level Axis-1 surfacing check (run by `rwv doctor`) additionally
warns when a surfacing symlink for a declared file is missing or mis-resolved
at the weave directory — for example, after a manual `rm` of a symlink, after
enabling the integration in an existing workweave, or after a file is added to
the `files:` list after a workweave was created. These surfacing warnings are
emitted framework-side (not by this integration's `check()`) and are safe to
fix with `rwv doctor --fix`. A real file occupying the surfacing path is
reported but never auto-clobbered.

## Examples

### Turborepo with npm workspaces

A project using Turborepo for build caching alongside npm workspaces:

```yaml
# projects/web-app/rwv.yaml
repositories:
  github/chatly/protocol:
    url: git@github.com:chatly/protocol.git
  github/chatly/server:
    url: git@github.com:chatly/server.git
  github/chatly/web:
    url: git@github.com:chatly/web.git

integrations:
  static-files:
    enabled: true
    files: [turbo.json]
```

```json
{
  "$schema": "https://turbo.build/schema.json",
  "tasks": {
    "build": {
      "dependsOn": ["^build"],
      "outputs": ["dist/**"]
    },
    "test": { "dependsOn": ["build"] },
    "lint": {}
  }
}
```

After activation, the weave directory contains `package.json` (from `npm-workspaces`) and `turbo.json` (symlinked). Turborepo discovers packages from `package.json` workspaces and reads its pipeline config from `turbo.json` — both at the weave directory where it expects them.

### Linter and formatter configs

ESLint and Prettier expect their config at the weave directory to apply across all packages:

```yaml
integrations:
  static-files:
    enabled: true
    files: [.eslintrc.json, .prettierrc]
```

```json
// projects/web-app/.eslintrc.json
{
  "root": true,
  "extends": ["eslint:recommended", "plugin:@typescript-eslint/recommended"],
  "parser": "@typescript-eslint/parser"
}
```

The `"root": true` is important — it tells ESLint to stop walking up the directory tree, so it doesn't pick up a config from a parent directory outside the weave.

### Nx build orchestrator

```yaml
integrations:
  pnpm-workspaces:
    enabled: true
  npm-workspaces:
    enabled: false
  static-files:
    enabled: true
    files: [nx.json]
```

```json
// projects/web-app/nx.json
{
  "$schema": "./node_modules/nx/schemas/nx-schema.json",
  "targetDefaults": {
    "build": { "dependsOn": ["^build"], "cache": true },
    "test": { "cache": true }
  },
  "defaultBase": "main"
}
```

Nx discovers packages from `pnpm-workspace.yaml` and reads task configuration from `nx.json`. Run `pnpm exec nx run-many -t build` to build all affected packages in dependency order.

### Toolchain versions (`.mise.toml`)

[mise](https://mise.jdx.dev/) reads `.mise.toml` from the directory you `cd` into:

```yaml
integrations:
  static-files:
    enabled: true
    files: [.mise.toml]
```

```toml
# projects/web-app/.mise.toml
[tools]
node = "22"
go = "1.22"
rust = "1.78"
python = "3.12"
```

After activation, `mise install` at the weave directory installs the declared versions. Combine with direnv (`use mise` in `.envrc`) for automatic activation on `cd`.

### Environment activation (`.envrc`)

[direnv](https://direnv.net/) reads `.envrc` on `cd`:

```yaml
integrations:
  static-files:
    enabled: true
    files: [.envrc]
```

```bash
# projects/web-app/.envrc
use mise                                      # activate toolchain versions from .mise.toml
export GITA_PROJECT_HOME="$PWD/gita"          # point gita at the generated config
export DATABASE_URL="postgres://localhost/web_app_dev"
export NODE_ENV="development"
```

Run `direnv allow` once after creating or modifying `.envrc`.

`.envrc` files often contain developer-local paths or credentials. Consider what belongs in the committed `.envrc` versus a `.envrc.local` that each developer maintains separately.

### Makefile or justfile

A `Makefile` or [`justfile`](https://github.com/casey/just) at the weave directory provides a consistent command interface across the multi-repo workspace:

```yaml
integrations:
  static-files:
    enabled: true
    files: [justfile]
```

```makefile
# projects/web-app/justfile
default:
    @just --list

install:
    rwv lock
    npm install

test:
    npm run test --workspaces

lint:
    eslint .
    prettier --check .
```

A justfile (or Makefile) is particularly useful for documenting commands that require multi-step sequences — like running `rwv lock` before `npm install`, or running lint after a format pass.
