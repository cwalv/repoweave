# Create a project

Initialize a new project repository under `projects/<name>/`, add member repositories, enable ecosystem integrations, and commit the initial manifest and lock:

```bash
mkdir -p ~/weave && cd ~/weave
rwv init web-app
rwv add https://github.com/chatly/server.git
rwv add https://github.com/chatly/client.git
rwv add https://github.com/chatly/shared-types.git --role dependency
rwv lock --commit
```

`rwv init web-app` creates `projects/web-app/` as a git repository containing `rwv.toml` and `.gitattributes` (with `rwv.lock merge=rwv-ours`), configures the local `merge.rwv-ours` git driver, and activates `web-app` by writing `.rwv-active`. Each `rwv add` clones the repo, records it in `rwv.toml`, and regenerates the integration files; `rwv lock --commit` then writes `rwv.lock` and commits it together with `rwv.toml`.

## Enable ecosystem integrations

By default, `cargo-workspace`, `npm-workspaces`, `go-work`, `uv-workspace`, and `vscode-workspace` are enabled. The first four act only when a member repo carries their marker (`Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`); `vscode-workspace` covers every member repo. `pnpm-workspaces`, `gita`, and `static-files` are opt-in in `projects/web-app/rwv.toml`, and you can set `enabled = true` or `enabled = false` on any integration:

```toml
[integrations.npm-workspaces]
enabled = false

[integrations.pnpm-workspaces]
enabled = true

[integrations.static-files]
enabled = true
files = ["justfile"]
```

After editing `rwv.toml`, author the newly enabled integrations' files, then remove what the disabled ones left behind, surface the weave-root symlinks, and run the install hooks:

```bash
rwv doctor --fix
rwv materialize
```

`rwv doctor --fix` authors files for an integration you just enabled; it does not remove a disabled integration's files, and says so. `rwv materialize` strips them, repairs the weave-root symlinks, and runs install hooks for the active project. An install hook whose tool is missing (`pnpm` here) prints a warning naming the tool and does not fail the command. `rwv add`, `rwv remove`, `rwv update`, and `rwv role` regenerate integration files as they change membership, and in the primary weave they also run install hooks.

## Commit and publish the project repository

`projects/web-app/` is an ordinary git repository. `rwv lock --commit` commits `rwv.lock`, plus `rwv.toml` when it has uncommitted changes, and nothing else, so commit the rest with git: `.gitattributes` and the generated workspace files. Teammates get `rwv.lock merge=rwv-ours` only from a committed `.gitattributes`, and `rwv push` does not refuse while `.gitattributes` is untracked. Then push to a remote and set `origin/HEAD`, which `rwv push` requires before it publishes the project repo:

```bash
git -C projects/web-app add -A
git -C projects/web-app commit -m "Initialize web-app project"
git -C projects/web-app remote add origin https://github.com/chatly/web-app.git
git -C projects/web-app push -u origin main
git -C projects/web-app remote set-head origin main
```

Teammates and CI can now clone and materialize the entire workspace with one command:

```bash
mkdir -p ~/weave && cd ~/weave
rwv fetch https://github.com/chatly/web-app.git
```

A missing ecosystem tool does not fail the fetch: the install hook that needs it is skipped with a `tool-missing` warning, and `rwv fetch` exits 0. A CI job that needs every install hook to have run gates on it next:

```bash
rwv doctor --kind tool-missing
```

It exits non-zero while any enabled integration's tool is not on `PATH`, and 0 once each one is installed.

## Create a second project in the same weave

A single weave can host multiple projects under `projects/` that share canonical member clones under `github/` (`gitlab/`, `local/`):

```bash
rwv init billing-service
rwv add https://github.com/chatly/shared-types.git --role dependency
rwv add https://github.com/chatly/billing.git
git -C projects/billing-service add -A
git -C projects/billing-service commit -m "Initialize billing-service project"
rwv lock --commit
```

`rwv init billing-service` makes `billing-service` the active project, so the `rwv add` commands that follow write to its `rwv.toml`. Because `github/chatly/shared-types/` is already cloned in the weave, `rwv add` reuses the existing canonical clone without re-downloading it, and warns that the clone is now shared between two projects. Switch between projects at the weave root with [`rwv activate`](./switch-projects.md) or open both simultaneously in separate [workweaves](./create-feature-workweave.md).

## Related

- [Add a repo to a project](./add-a-repo.md) — URL clones, `--new`, brownfield adoption, and `rwv init --adopt`
- [Add an integration](./add-an-integration.md) — enabling and configuring ecosystem workspace files
- [Switch projects](./switch-projects.md) — switching `.rwv-active` vs. using workweaves
- [File formats reference](../reference/formats.md) — `rwv.toml` and `rwv.lock` schema
