# Add an integration

Enable, configure, or disable an ecosystem integration with `rwv integration`, which writes `projects/<project>/rwv.toml` and commits its own change. Turning an integration on authors the managed files it declares in the same commit; turning one off deletes nothing, and `rwv materialize` cleans up:

```bash
rwv integration set-enabled pnpm-workspaces true
rwv integration set-enabled npm-workspaces false
rwv materialize       # strip disabled-integration artifacts and run install hooks
```

`cargo-workspace`, `npm-workspaces`, `go-work`, and `uv-workspace` are enabled by default and act only when a member repo carries their manifest; `vscode-workspace` is enabled by default and authors its file for every project. `pnpm-workspaces`, `gita`, and `static-files` are opt-in. A misspelled integration name or key is refused by the verb (`unknown-integration-setting`), which lists the accepted names or keys. Another key of an integration's table is set with `rwv integration set <name> <key> <value>`, the value a TOML value: `rwv integration set cargo-workspace rustc-wrapper '"sccache"'`. See [Integrations reference](../reference/integrations/index.md) for the full list and per-integration settings. A hand edit of `rwv.toml` is recoverable: `rwv doctor --fix` authors the files of an integration whose `enabled` was edited by hand, and the verbs above converge whatever a hand edit left.

## Switch ecosystems (`npm` → `pnpm`)

Disable `npm-workspaces` and enable `pnpm-workspaces`:

```bash
rwv integration set-enabled npm-workspaces false
rwv integration set-enabled pnpm-workspaces true
```

Enabling authors `pnpm-workspace.yaml`. Then strip the `rwv`-managed `workspaces` block from `package.json` (removing `package-lock.json`), and run `pnpm install`:

```bash
rm -rf node_modules
rwv materialize
```

If `pnpm` is not on `PATH`, `rwv materialize` finishes with a `tool-missing` warning instead of running `pnpm install`; install `pnpm` and run `rwv materialize` again.

## Surface static files at the weave root

Use the `static-files` integration to symlink project-owned configuration files (`.eslintrc.json`, `turbo.json`, `nx.json`, `.mise.toml`, `.envrc`, `Makefile`, `justfile`) from `projects/<project>/` to the weave root:

```bash
rwv integration set-enabled static-files true
rwv integration set static-files files '["turbo.json", ".eslintrc.json", ".prettierrc", ".mise.toml"]'
```

Each listed file must exist in `projects/<project>/`; `rwv doctor` names any that do not. If you later remove a filename from `files`, unlink the leftover weave-root symlink with `rwv materialize --remove-undeclared-links`. See [Static files integration reference](../reference/integrations/static-files.md) for worked examples.

## Disable a default-enabled integration

Turn it off, then run `rwv materialize` to strip any managed regions or generated files that `rwv` previously authored for that integration:

```bash
rwv integration set-enabled npm-workspaces false
rwv doctor            # disabled-integration-artifact lists what materialize will remove
rwv materialize
```

`rwv materialize` prints one `[stripped] <integration>: disabled, removed <files>` line per integration. It strips `rwv`'s marked region out of each hybrid file and keeps your content, removes any lock file the integration generated, and removes the weave-root links; tool state such as `node_modules/` stays. `rwv doctor --fix` never does this, and neither does the setter.

Explicit disabling is only needed for `vscode-workspace` or for an integration whose ecosystem files are present: the other default-enabled integrations do nothing for a project with no matching member (for example, `npm-workspaces` is a no-op when no member has a `package.json`).

## Related

- [Regenerate ecosystem workspace files](./regenerate-ecosystem-files.md) — when to use `rwv doctor --fix`, `rwv materialize`, and `rwv activate`
- [Integrations reference](../reference/integrations/index.md) — built-in integrations and hybrid file ownership
- [Static files integration reference](../reference/integrations/static-files.md) — surfacing top-level tool configs
- [File formats reference](../reference/formats.md) — `rwv.toml` `[integrations]` schema
