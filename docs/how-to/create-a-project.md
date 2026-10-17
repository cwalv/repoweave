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

`rwv init web-app` creates `projects/web-app/` as a git repository containing `rwv.toml` and `.gitattributes` (with `rwv.lock merge=rwv-ours`), configures the local `merge.rwv-ours` git driver, and activates `web-app` by writing `.rwv-active`.

## Enable ecosystem integrations

By default, `cargo-workspace`, `npm-workspaces`, `go-work`, `uv-workspace`, and `vscode-workspace` are enabled and auto-detect when matching member markers (`Cargo.toml`, `package.json`, `go.mod`, `pyproject.toml`, or 2+ member repos) are present. Integrations like `pnpm-workspaces`, `gita`, and `static-files` are opt-in in `projects/web-app/rwv.toml` (and you can explicitly set `enabled = true` or `enabled = false` on any integration):

```toml
[integrations.npm-workspaces]
enabled = false

[integrations.pnpm-workspaces]
enabled = true

[integrations.static-files]
enabled = true
files = ["justfile"]
```

After editing `rwv.toml`, author the newly enabled integration files, surface their weave-root symlinks, and run their install hooks:

```bash
rwv doctor --fix
rwv activate web-app
```

(`rwv add`, `rwv remove`, `rwv update`, `rwv role`, and `rwv doctor --fix` author integration files inside `projects/web-app/`; `rwv activate` surfaces weave-root symlinks and runs install hooks.)

## Commit and publish the project repository

`projects/web-app/` is an ordinary git repository. While `rwv lock --commit` stages and commits `rwv.lock`, run `git commit` in `projects/web-app/` to commit `rwv.toml`, `.gitattributes`, and generated workspace files, then push to a remote repository and set `origin/HEAD` so teammates and CI can materialize the weave with `rwv fetch`:

```bash
git -C projects/web-app add -A
git -C projects/web-app commit -m "Initialize web-app project"
git -C projects/web-app remote add origin https://github.com/chatly/web-app.git
git -C projects/web-app push -u origin main
git -C projects/web-app remote set-head origin main
```

Teammates can now clone and materialize the entire workspace with one command:

```bash
mkdir -p ~/weave && cd ~/weave
rwv fetch https://github.com/chatly/web-app.git
```

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

Because `github/chatly/shared-types/` is already cloned in the weave, `rwv add` reuses the existing canonical clone without re-downloading it. Switch between projects at the weave root with [`rwv activate`](./switch-projects.md) or open both simultaneously in separate [workweaves](./create-feature-workweave.md).

## Related

- [Add a repo to a project](./add-a-repo.md) — URL clones, `--new`, brownfield adoption, and `rwv init --adopt`
- [Add an integration](./add-an-integration.md) — enabling and configuring ecosystem workspace files
- [Switch projects](./switch-projects.md) — switching `.rwv-active` vs. using workweaves
- [File formats reference](../reference/formats.md) — `rwv.toml` and `rwv.lock` schema
