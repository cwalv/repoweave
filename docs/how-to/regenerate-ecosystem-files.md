# Regenerate ecosystem workspace files

Bring ecosystem workspace files (`Cargo.toml`, `package.json`, `pnpm-workspace.yaml`, `go.work`, `pyproject.toml`, `gita/`), weave-root symlinks, and tool install state (`node_modules/`, `.venv/`, `target/`) in line with `rwv.toml`:

```bash
rwv doctor --fix      # author or repair managed workspace files and symlinks
rwv materialize       # strip disabled integrations and run install hooks
```

## Which command does what

Repoweave separates authoring workspace config files, selecting the active project, and materializing tool state into distinct verbs:

| Task | Command | Works in |
|---|---|---|
| Mutate membership and automatically re-author managed files | `rwv add`, `rwv remove`, `rwv update`, `rwv role` | Primary weave or workweave |
| Re-author managed files after hand-editing `rwv.toml`, or repair missing weave-root symlinks | `rwv doctor --fix` | Primary weave or workweave |
| Strip artifacts from newly disabled integrations and run install hooks (`npm install`, `uv sync`, `cargo fetch`) | `rwv materialize` | Primary weave or workweave |
| Switch `.rwv-active`, re-point weave-root symlinks to `<project>`, and run install hooks (never authors files) | `rwv activate <project>` | Primary weave only |

## After editing `rwv.toml` by hand

Intent verbs (`rwv add`, `rwv remove`, `rwv update`, `rwv role`) update managed workspace files automatically. When you edit `rwv.toml` directly — for example, changing an integration setting or enabling a new integration — run:

```bash
rwv doctor --fix
```

`rwv doctor --fix` updates the `rwv`-managed sections of hybrid files (`Cargo.toml`, `package.json`, `pnpm-workspace.yaml`, `go.work`, `pyproject.toml`) while preserving user-authored keys, regenerates owned files (`gita/`, `.code-workspace`), and repairs any missing surfacing symlinks in the current workspace.

If your `rwv.toml` edit disabled an integration (`enabled = false`), follow up with `rwv materialize` to strip the disabled integration's managed blocks or files and run install hooks:

```bash
rwv materialize
```

## After `rwv sync` or inside a workweave

`rwv activate` is refused inside a workweave because a workweave's project is fixed at creation. To run integration install hooks inside a workweave (or at the primary weave without switching projects) after `rwv sync` brings in new membership or lock changes:

```bash
rwv materialize
```

Install hooks materialize current membership without advancing versions already pinned in ecosystem lockfiles.

## Resolve unaccepted drift on generated files

When an attested generated file holds edits that `rwv` never wrote or accepted, verbs that run install hooks withhold the hooks and list the drifted paths. Choose whether to overwrite the file from current inputs or accept the on-disk content:

```bash
rwv materialize --regenerate-drifted   # discard unaccepted edits and re-derive from inputs
rwv materialize --adopt-drifted        # record current on-disk content as accepted
```

What `--regenerate-drifted` discards is not saved by `rwv`; copy anything you want to keep before running it.

## Remove weave-root symlinks for undeclared filenames

Dropping a filename from `integrations.static-files.files` in `rwv.toml` leaves the earlier weave-root symlink in place (`rwv doctor` reports it as a `surfacing` warning). Unlink those undeclared symlinks without touching the target files they point to:

```bash
rwv materialize --remove-undeclared-links
```

## Files that `rwv` never overwrites on its own

- **User-owned keys in hybrid files:** `rwv doctor --fix` and intent verbs only touch keys marked by `rwv`'s sentinel comments or metadata keys, leaving your custom `[profile.*]`, `scripts`, `replace`, or `[tool.ruff]` settings intact.
- **Ecosystem lockfiles (`Cargo.lock`, `package-lock.json`, `pnpm-lock.yaml`, `uv.lock`, `go.work.sum`):** Updated by the ecosystem's own tool during install hooks (or when `go` commands run against `go.work`).
- **Compiled native extensions (`maturin` / `PyO3`):** `uv sync` reacts to dependency resolution changes, not edits to `.rs` source files inside an already-installed package. See [uv-workspace: a compiled extension can go stale](../reference/integrations/uv-workspace.md#polyglot-cargouv-a-compiled-extension-can-go-stale) for the `maturin-import-hook` setup.

## Verify the result

Check that weave-root symlinks resolve and `rwv doctor` reports no drift:

```bash
ls -la | grep '\->'
rwv doctor
```

## Related

- [Add an integration](./add-an-integration.md) — enabling, configuring, or disabling integrations in `rwv.toml`
- [Switch projects](./switch-projects.md) — switching the active project with `rwv activate`
- [File ownership](../explanation/joints/file-ownership.md) — how authoring, surfacing, and materialization divide across verbs
- [Integrations reference](../reference/integrations/index.md) — per-integration managed files and install hooks
- [CLI reference — `rwv materialize`](../reference/cli.md#rwv-materialize-) — drift-consent and symlink-cleanup flags
