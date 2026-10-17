# Add an integration

Enable, configure, or disable an ecosystem integration in `projects/<project>/rwv.toml`, then author newly enabled files with `rwv doctor --fix` and clean up disabled integrations with `rwv materialize`:

```toml
# projects/web-app/rwv.toml
[integrations.pnpm-workspaces]
enabled = true

[integrations.npm-workspaces]
enabled = false
```

```bash
rwv doctor --fix      # author managed files for newly enabled integrations
rwv materialize       # strip disabled-integration artifacts and run install hooks
```

`cargo-workspace`, `npm-workspaces`, `go-work`, and `uv-workspace` are enabled by default and act only when a member repo carries their manifest; `vscode-workspace` is enabled by default and authors its file for every project. `pnpm-workspaces`, `gita`, and `static-files` are opt-in. A misspelled integration name or key (`[integrations.cargo]`, `hide-dotfile`) is refused when `rwv.toml` is parsed, with the nearest valid name. See [Integrations reference](../reference/integrations/index.md) for the full list and per-integration settings.

## Switch ecosystems (`npm` → `pnpm`)

Disable `npm-workspaces` and enable `pnpm-workspaces` in `rwv.toml`:

```toml
[integrations.npm-workspaces]
enabled = false

[integrations.pnpm-workspaces]
enabled = true
```

Then author `pnpm-workspace.yaml`, strip the `rwv`-managed `workspaces` block from `package.json` (removing `package-lock.json`), and run `pnpm install`:

```bash
rwv doctor --fix
rm -rf node_modules
rwv materialize
```

If `pnpm` is not on `PATH`, both commands finish with a `tool-missing` warning instead of running `pnpm install`; install `pnpm` and run `rwv materialize` again.

## Surface static files at the weave root

Use the `static-files` integration to symlink project-owned configuration files (`.eslintrc.json`, `turbo.json`, `nx.json`, `.mise.toml`, `.envrc`, `Makefile`, `justfile`) from `projects/<project>/` to the weave root:

```toml
[integrations.static-files]
enabled = true
files = ["turbo.json", ".eslintrc.json", ".prettierrc", ".mise.toml"]
```

```bash
rwv doctor --fix
```

Each listed file must exist in `projects/<project>/`; `rwv doctor` names any that do not. If you later remove a filename from `files`, unlink the leftover weave-root symlink with `rwv materialize --remove-undeclared-links`. See [Static files integration reference](../reference/integrations/static-files.md) for worked examples.

## Disable a default-enabled integration

Set `enabled = false` in `rwv.toml`, then run `rwv materialize` to strip any managed regions or generated files that `rwv` previously authored for that integration:

```toml
[integrations.npm-workspaces]
enabled = false
```

```bash
rwv doctor            # disabled-integration-artifact lists what materialize will remove
rwv materialize
```

`rwv materialize` prints one `[stripped] <integration>: disabled, removed <files>` line per integration. It strips `rwv`'s marked region out of each hybrid file and keeps your content, removes any lock file the integration generated, and removes the weave-root links; tool state such as `node_modules/` stays. `rwv doctor --fix` never does this.

Explicit disabling is only needed for `vscode-workspace` or for an integration whose ecosystem files are present: the other default-enabled integrations do nothing for a project with no matching member (for example, `npm-workspaces` is a no-op when no member has a `package.json`).

## Related

- [Regenerate ecosystem workspace files](./regenerate-ecosystem-files.md) — when to use `rwv doctor --fix`, `rwv materialize`, and `rwv activate`
- [Integrations reference](../reference/integrations/index.md) — built-in integrations and hybrid file ownership
- [Static files integration reference](../reference/integrations/static-files.md) — surfacing top-level tool configs
- [File formats reference](../reference/formats.md) — `rwv.toml` `[integrations]` schema
