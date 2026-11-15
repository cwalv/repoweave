# Create a project

Initialize a new project repository under `projects/<name>/`, add member repositories, enable ecosystem integrations, and commit the initial manifest and lock:

```bash
mkdir -p ~/weave && cd ~/weave
rwv init web-app
rwv member add https://github.com/chatly/server.git
rwv member add https://github.com/chatly/client.git
rwv member add https://github.com/chatly/shared-types.git --role dependency
```

`rwv init web-app` creates `projects/web-app/` as a git repository containing `rwv.toml`, `.gitattributes` (with `rwv.lock merge=rwv-lock`) and `web-app.code-workspace`, commits exactly those three files as its first commit, configures the lock merge driver in the repo's git config, and activates `web-app` by writing `.rwv-active`. Each `rwv member add` clones the repo, records it in `rwv.toml`, regenerates the integration files, and commits them together with `rwv.lock`, which pins the commit it cloned.

## Enable ecosystem integrations

By default, `cargo-workspace`, `npm-workspaces`, `go-work`, `uv-workspace`, and `vscode-workspace` are enabled. The first four act only when a member repo carries their marker (`Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`); `vscode-workspace` covers every member repo. `pnpm-workspaces`, `gita`, and `static-files` are opt-in in `projects/web-app/rwv.toml`, and you can set `enabled` to `true` or `false` on any integration with `rwv integration set-enabled`:

```bash
rwv integration set-enabled npm-workspaces false
rwv integration set-enabled pnpm-workspaces true
rwv integration set-enabled static-files true
rwv integration set static-files files '["justfile"]'
```

Each `rwv integration` setter commits its own change to `rwv.toml`, and turning an integration on authors the managed files it declares in the same commit. Turning one off deletes nothing; remove what the disabled integrations left behind, surface the weave-root symlinks, and run the install hooks with:

```bash
rwv materialize
```

`rwv doctor --fix` also authors files for an integration whose `enabled` was edited by hand; it does not remove a disabled integration's files, and says so. `rwv materialize` strips them, repairs the weave-root symlinks, and runs install hooks for the active project. An install hook whose tool is missing (`pnpm` here) prints a warning naming the tool and does not fail the command. `rwv member add`, `rwv member remove`, `rwv update --remote`, and `rwv member set-role` regenerate integration files as they change membership, and in the primary weave they also run install hooks.

## Commit and publish the project repository

`projects/web-app/` is an ordinary git repository, and everything above is already committed: `rwv init` committed `rwv.toml`, `.gitattributes` and the workspace file, and each `rwv member add` committed `rwv.lock`, `rwv.toml` and the integration files it regenerated. Record where it publishes and publish:

```bash
rwv member set-url projects/web-app https://github.com/chatly/web-app.git
rwv push
```

`rwv push` publishes the project repo to its url remote: the remote `rwv member set-url projects/<name>`, `rwv init --provider` or `rwv clone` recorded, else its only remote. With several remotes and none recorded, a branch that has no push remote of its own has no push target, and `rwv push` refuses it (`push-branch` `no push target`) before pushing anything; `rwv member set-url projects/<name> <url>` records one.

Teammates and CI can now clone and materialize the entire workspace with one command:

```bash
mkdir -p ~/weave && cd ~/weave
rwv clone https://github.com/chatly/web-app.git
```

A missing ecosystem tool does not fail the clone: the install hook that needs it is skipped with a `tool-missing` warning, and `rwv clone` exits 0 when every member sits at its pin. A member the lock does not pin, or pins at an unreachable commit, makes it exit 3 instead. A CI job that needs every install hook to have run gates on it next:

```bash
rwv doctor --kind tool-missing
```

It exits non-zero while any enabled integration's tool is not on `PATH`, and 0 once each one is installed.

## Create a second project in the same weave

A single weave can host multiple projects under `projects/` that share canonical member clones under `github/` (`gitlab/`, `local/`):

```bash
rwv init billing-service
rwv member add https://github.com/chatly/shared-types.git --role dependency
rwv member add https://github.com/chatly/billing.git
```

`rwv init billing-service` makes `billing-service` the active project, so the `rwv member add` commands that follow write to its `rwv.toml`. Because `github/chatly/shared-types/` is already cloned in the weave, `rwv member add` reuses the existing canonical clone without re-downloading it, and warns that the clone is now shared between two projects. Switch between projects at the weave root with [`rwv activate`](./switch-projects.md) or open both simultaneously in separate [workweaves](./create-feature-workweave.md).

## Related

- [Add a repo to a project](./add-a-repo.md) — URL clones, `--new`, and brownfield adoption
- [Add an integration](./add-an-integration.md) — enabling and configuring ecosystem workspace files
- [Switch projects](./switch-projects.md) — switching `.rwv-active` vs. using workweaves
- [File formats reference](../reference/formats.md) — `rwv.toml` and `rwv.lock` schema
