# rwv integration

## Purpose

Change one key of an `[integrations.<name>]` table in the project's `rwv.toml`
without editing the file: turn an integration on or off, or set one of its
other keys. Each form commits its own change and nothing else the project repo
holds uncommitted, as the `rwv member` setters do.

## Invocation

```
rwv integration set-enabled <name> <true|false> [--project <name>] [--no-materialize]
rwv integration set <name> <key> <value> [--project <name>] [--no-materialize]
```

- `<name>` is an integration as `[integrations.<name>]` names it: `npm-workspaces`,
  `pnpm-workspaces`, `go-work`, `uv-workspace`, `cargo-workspace`, `gita`,
  `vscode-workspace` or `static-files`. A name no built-in integration has is
  refused (`unknown-integration-setting`), listing the accepted ones.
- `set-enabled` writes `enabled`. Turning an integration on authors the managed
  files it declares and commits them with the key. Turning one off deletes
  nothing: the files it authored stay, and the verb names `rwv materialize` as
  the step that strips them.
- `set` writes any other key of the table. `<value>` is a TOML value, so a
  string is quoted and a list is bracketed (`'"sccache"'`, `'["turbo.json"]'`).
  A key the integration does not read, and a value that is not TOML, are
  refused (`unknown-integration-setting`); the refusal for a key lists the keys
  the integration takes. `enabled` is refused, naming `set-enabled`.
- A value `rwv.toml` already holds is not rewritten, but the generated files
  are regenerated, so a hand edit of the key is finished by re-running the
  verb.
- `--project <name>` operates on that project instead of the active one.
  `--no-materialize` skips the integration install hooks of the regeneration;
  `rwv materialize` runs them later.

Every other uncommitted edit to `rwv.toml` stays uncommitted: the commit holds
the key this verb changed.

## Examples

Turn the gita integration on and author its files:

```
rwv integration set-enabled gita true
```

Name the files static-files surfaces at the weave root:

```
rwv integration set static-files files '["turbo.json", ".eslintrc.json"]'
```

Turn an integration off, then strip what it authored:

```
rwv integration set-enabled npm-workspaces false
rwv materialize
```
