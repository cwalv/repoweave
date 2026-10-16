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

`rwv init web-app` creates `projects/web-app/` as a git repository containing `rwv.toml`, `.gitignore`, and `.gitattributes` (with `rwv.lock merge=rwv-ours`), configures the local `merge.rwv-ours` git driver, and activates `web-app` by writing `.rwv-active`.

## Enable ecosystem integrations

By default, `rwv init` scaffolds `rwv.toml` with `[repositories]` and leaves `[integrations]` opt-in. Enable the integrations that match your stack in `projects/web-app/rwv.toml`:

```toml
[integrations.npm-workspaces]
enabled = true

[integrations.vscode-workspace]
enabled = true
```

Then author the newly enabled integration files, surface their weave-root symlinks, and run their install hooks:

```bash
rwv doctor --fix
rwv activate web-app
```

(`rwv add`, `rwv remove`, `rwv update`, `rwv role`, and `rwv doctor --fix` author integration files inside `projects/web-app/`; `rwv activate` surfaces weave-root symlinks and runs install hooks.)

## Commit and publish the project repository

`projects/web-app/` is an ordinary git repository. Commit `rwv.toml`, `rwv.lock`, and the generated workspace files, then push to a remote repository so teammates and CI can materialize the weave with `rwv fetch`:

```bash
git -C projects/web-app add -A
git -C projects/web-app commit -m "Initialize web-app project"
git -C projects/web-app remote add origin https://github.com/chatly/web-app.git
git -C projects/web-app push -u origin main
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
rwv lock --commit
```

Because `github/chatly/shared-types/` is already cloned in the weave, `rwv add` reuses the existing canonical clone without re-downloading it. Switch between projects at the weave root with [`rwv activate`](./switch-projects.md) or open both simultaneously in separate [workweaves](./create-feature-workweave.md).

## Related

- [Add a repo to a project](./add-a-repo.md) — URL clones, `--new`, brownfield adoption, and `rwv init --adopt`
- [Add an integration](./add-an-integration.md) — enabling and configuring ecosystem workspace files
- [Switch projects](./switch-projects.md) — switching `.rwv-active` vs. using workweaves
- [File formats reference](../reference/formats.md) — `rwv.toml` and `rwv.lock` schema
