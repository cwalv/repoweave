# static-files

Symlinks declared files from the project directory (`projects/<project>/`) to the weave root. Escape hatch for top-level config files that don't belong to any ecosystem integration — build orchestrator configs (`turbo.json`, `nx.json`), linter configs (`.eslintrc.json`, `.prettierrc`), or anything else tools expect at the weave root.

| | |
|---|---|
| Default enabled | no |
| Auto-detects | n/a (configured explicitly) |
| Surfaces | declared files from `projects/<project>/` at the weave root |
| Install hook | — |

## Configuration

Disabled by default. Enable explicitly in `rwv.toml`; the table takes `enabled` and `files`, and any other key is refused when `rwv.toml` is parsed:

```toml
[integrations.static-files]
enabled = true
files = ["turbo.json", "nx.json", ".eslintrc.json", ".prettierrc"]
```

Each file listed in `files` must exist in the project directory (e.g., `projects/web-app/turbo.json`). `rwv doctor --fix`, `rwv activate`, `rwv materialize`, and the authoring verbs symlink each file to the weave root so tools find them where they expect.

### Entry forms

| Entry | The weave-root link reads |
|---|---|
| `"turbo.json"` | this weave's `projects/<project>/turbo.json` |
| `{ path = "turbo.json" }` | the same as the plain string |
| `{ path = ".beads", from = "primary" }` | the primary's `projects/<project>/.beads`, from every workweave of the project |

A table takes `path` (required) and `from` (optional). `"primary"` is the only value `from` accepts. A table with any other key or value is reported as `malformed-settings` rather than read as a plain entry.

### Branch content and shared state

A plain entry is branch content. Each workweave surfaces its own checkout of `projects/<project>/`, so an edit made in a workweave is visible there and lands with that workweave's branch. Use it for files that change with the code: `CLAUDE.md`, `turbo.json`, linter configs.

`from = "primary"` is for shared state that every workweave must see one copy of: an issue tracker's directory, a local database, a cache that workweaves coordinate through. In a workweave, the link is an absolute path into the primary's project directory, taken from the primary the workweave records:

- a workweave created from another workweave still links the primary, not the workweave it was created from;
- the link targets the project's own directory, `projects/<project>/<path>`, never the primary's weave-root entry, so it does not depend on which project the primary currently presents;
- in the primary itself, the entry is surfaced exactly like a plain one.

The primary's copy is the one that has to exist. While it is absent, the link is not created.

A name may not appear both in `files` (in either form) and in `[workweave] link`. `rwv doctor` reports the combination as a `config-rejected` error and exits 1; `rwv activate` prints the same message as a warning and continues; the authoring verbs (`rwv add`, `remove`, `update`, `role`) refuse and put `rwv.toml` back.

### Examples

#### Turborepo with npm workspaces

A project using Turborepo for build caching alongside npm workspaces:

```toml
# projects/web-app/rwv.toml

[repositories."github/chatly/protocol"]
type = "git"
url = "git@github.com:chatly/protocol.git"
version = "main"
role = "owned"

[repositories."github/chatly/server"]
type = "git"
url = "git@github.com:chatly/server.git"
version = "main"
role = "owned"

[repositories."github/chatly/web"]
type = "git"
url = "git@github.com:chatly/web.git"
version = "main"
role = "owned"

[integrations.static-files]
enabled = true
files = ["turbo.json"]
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

After activation, the weave root contains `package.json` (from `npm-workspaces`) and `turbo.json` (symlinked). Turborepo discovers packages from `package.json` workspaces and reads its pipeline config from `turbo.json` — both at the weave root where it expects them.

#### Linter and formatter configs

ESLint and Prettier expect their config at the weave root to apply across all packages:

```toml
[integrations.static-files]
enabled = true
files = [".eslintrc.json", ".prettierrc"]
```

```json
// projects/web-app/.eslintrc.json
{
  "root": true,
  "extends": ["eslint:recommended", "plugin:@typescript-eslint/recommended"],
  "parser": "@typescript-eslint/parser"
}
```

The `"root": true` setting tells ESLint to stop walking up the directory tree, so it doesn't pick up a config from a parent directory outside the weave.

#### Nx build orchestrator

```toml
[integrations.pnpm-workspaces]
enabled = true

[integrations.npm-workspaces]
enabled = false

[integrations.static-files]
enabled = true
files = ["nx.json"]
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

#### Shared issue-tracker state

A tracker that keeps its database in a directory beside the project's files, such as beads' `.beads`, needs every workweave to reach the primary's copy. A workweave's own checkout carries only the tracked configuration:

```toml
[integrations.static-files]
enabled = true
files = ["CLAUDE.md", { path = ".beads", from = "primary" }]
```

At the primary, `.beads` links `projects/<project>/.beads`. In every workweave of the project, `.beads` links the primary's `projects/<project>/.beads` directly, so a tool that finds `.beads` by searching upward from its working directory reaches the same database from anywhere in the workweave. `CLAUDE.md` stays branch content: each workweave sees its own edits.

#### Toolchain versions (`.mise.toml`)

[mise](https://mise.jdx.dev/) reads `.mise.toml` from the directory you `cd` into:

```toml
[integrations.static-files]
enabled = true
files = [".mise.toml"]
```

```toml
# projects/web-app/.mise.toml
[tools]
node = "22"
go = "1.22"
rust = "1.78"
python = "3.12"
```

After activation, `mise install` at the weave root installs the declared versions. Combine with direnv (`use mise` in `.envrc`) for automatic activation on `cd`.

#### Environment activation (`.envrc`)

[direnv](https://direnv.net/) reads `.envrc` on `cd`:

```toml
[integrations.static-files]
enabled = true
files = [".envrc"]
```

```bash
# projects/web-app/.envrc
use mise                                      # activate toolchain versions from .mise.toml
export GITA_PROJECT_HOME="$PWD"               # gita reads $GITA_PROJECT_HOME/gita/
export DATABASE_URL="postgres://localhost/web_app_dev"
export NODE_ENV="development"
```

Run `direnv allow` once after creating or modifying `.envrc`.

`.envrc` files often contain developer-local paths or credentials. Consider what belongs in the committed `.envrc` versus a `.envrc.local` that each developer maintains separately.

#### Makefile or justfile

A `Makefile` or [`justfile`](https://github.com/casey/just) at the weave root provides a consistent command interface across the multi-repo workspace:

```toml
[integrations.static-files]
enabled = true
files = ["justfile"]
```

```makefile
# projects/web-app/justfile
default:
    @just --list

install:
    rwv materialize

test:
    npm run test --workspaces

lint:
    eslint .
    prettier --check .
```

A `justfile` (or `Makefile`) is useful for documenting multi-step project commands — such as running `rwv materialize` to refresh workspace dependencies, or running lint after a format pass.

## Managed and generated files

Unlike ecosystem integrations that auto-detect repos and generate config, `static-files` does no generation and no detection. The files are hand-written and committed in the project directory. The integration simply makes them visible at the weave root via symlinks.

If a declared file is missing from the project directory, `rwv doctor` reports it as a `config-rejected` warning, and the verbs that surface files skip it with a warning rather than failing.

## Install hook

None.

## Deactivation

`static-files` authors no content, so there is nothing to strip. A link counts as surfacing only when it reads the declared path in a project directory: `projects/<project>/<path>` relative to the weave root, or, for a primary-sourced entry in a workweave, the same path under the primary. Any other link at a declared name is left alone. The original files in the project directory, including the primary's copy behind a primary-sourced link, are untouched.

- **Disabled** (`enabled = false`): `rwv materialize` removes the weave-root links the integration surfaced.
- **Name dropped from `files`**: the link it left is reported like any other undeclared surfacing link and removed by `rwv materialize --remove-undeclared-links`.

## Diagnostics

Reported by `rwv doctor`.

| Kind | Severity | Condition | Repair |
|---|---|---|---|
| [`malformed-settings`](../doctor-findings.md#malformed-settings) | error | A `files` entry that is neither a string nor a `{ path, from }` table — a table with another key, or a `from` value other than `"primary"`. | Correct the entry the message names. |
| [`config-rejected`](../doctor-findings.md#config-rejected) | error | A path appears in both `integrations.static-files.files` and `[workweave] link`. | Remove the name from one of the two. |
| [`config-rejected`](../doctor-findings.md#config-rejected) | warning | A declared file is missing from `projects/<project>/`. In a workweave, primary-sourced entries (`from = "primary"`) are not checked against the workweave's own project directory, since that is not the copy they surface. | Create the file, or drop it from `files`. |
| [`surfacing`](../doctor-findings.md#surfacing) | warning | The weave-root link for a declared file is missing or mis-resolved — for example, after a manual `rm` of a symlink, after enabling the integration in an existing workweave, or after a file is added to `files` after a workweave was created. A real file occupying the path is reported but never auto-clobbered. | `rwv doctor --fix` |

For a primary-sourced entry in a workweave, the expected link is the absolute
path into the primary's project directory. `rwv doctor` reports a missing link,
or a link that reads the workweave's own copy, as safe to fix, and
`rwv doctor --fix` replaces it in the existing workweave. A link that resolves
somewhere `rwv`'s surfacing never points (for example, into a different tree's
`projects/`) is reported as not auto-fixed and left in place; remove it, and
`rwv doctor --fix` creates the expected one.

## Related

- [add an integration](../../how-to/add-an-integration.md#surface-static-files-at-the-weave-root) — the enable-and-surface recipe
- [Integrations reference](./index.md#build-orchestration) — build orchestrators that read the surfaced files
