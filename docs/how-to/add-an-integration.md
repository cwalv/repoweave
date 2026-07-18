# Add an integration

Integrations translate between the project manifest and ecosystem workspace files. Most run automatically based on what's in the repos (a `package.json` triggers `npm-workspaces`, a `go.mod` triggers `go-work`). Some are opt-in.

For the per-integration generated-file format and details, see [reference/integrations](../reference/integrations/index.md). Integrations ship with `rwv`; to request a new one, open a [GitHub issue](https://github.com/cwalv/repoweave/issues).

## Enable an opt-in integration

Edit the active project's `rwv.yaml` and add an `integrations` block:

```yaml
integrations:
  pnpm-workspaces:
    enabled: true
  npm-workspaces:
    enabled: false
```

Only overrides need to be listed — integrations not mentioned use their own defaults. After editing, regenerate:

```bash
rwv activate web-app
```

Reactivating runs each integration's deactivate hook (to clean up old files), then its activate hook (to generate new ones).

## Switch ecosystem (npm → pnpm)

```yaml
integrations:
  npm-workspaces:
    enabled: false
  pnpm-workspaces:
    enabled: true
```

Then:

```bash
rwv activate web-app
rm -rf node_modules
pnpm install
```

## Add static files (linter configs, build orchestrators)

The `static-files` integration symlinks declared files from the project directory to the weave directory. Use it for top-level configs that don't belong to any ecosystem integration — `.eslintrc.json`, `turbo.json`, `nx.json`, `.mise.toml`, `.envrc`, `Makefile`:

```yaml
integrations:
  static-files:
    enabled: true
    files: [turbo.json, .eslintrc.json, .prettierrc, .mise.toml]
```

Each listed file must exist in the project directory (e.g., `projects/web-app/turbo.json`). On activation, the integration symlinks each to the weave directory. Missing files print a warning but don't fail activation.

See [reference/integrations/static-files](../reference/integrations/static-files.md) for worked examples (Turborepo, Nx, mise, direnv, justfile).

## Per-integration config

Some integrations accept config beyond `enabled`:

```yaml
integrations:
  static-files:
    enabled: true
    files: [turbo.json, .eslintrc.json]
  gita:
    enabled: true
```

The config keys are integration-specific; see the per-integration reference page for what each one accepts.

## Disable a default-enabled integration

Set `enabled: false`. For example, on a Go-only project:

```yaml
integrations:
  npm-workspaces:
    enabled: false
  uv-workspace:
    enabled: false
  cargo-workspace:
    enabled: false
```

Auto-detection means an unused integration is silently a no-op (no `package.json` repos → `npm-workspaces` generates nothing), so explicit disabling is usually only needed when two competing integrations could both auto-trigger (npm vs. pnpm).

## Related

- [reference/integrations](../reference/integrations/index.md) — full list of built-in integrations
- [GitHub issues](https://github.com/cwalv/repoweave/issues) — request a new integration or report a problem
- [reference/formats](../reference/formats.md) — `rwv.yaml` schema
