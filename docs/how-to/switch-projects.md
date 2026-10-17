# Switch projects

A weave holds multiple projects under `projects/`, with one active at the weave root at a time. Switch the active project with `rwv activate`:

```bash
rwv activate mobile-app
```

`rwv activate` does four things:

1. Verifies managed integration files in `projects/mobile-app/` and warns if any have drifted from `rwv.toml` (`activate` never authors integration files; run `rwv doctor --fix` to regenerate them).
2. Removes the previous project's weave-root symlinks and symlinks `projects/mobile-app/`'s ecosystem workspace files to the weave root.
3. Runs integration install hooks (`npm install`, `uv sync`, `cargo fetch`, etc.) so shared tool state (`node_modules/`, `.venv/`, `target/`) matches the newly active project.
4. Updates `.rwv-active` to `mobile-app`.

To switch the active project and weave-root symlinks without running install hooks:

```bash
rwv activate mobile-app --no-materialize
```

Run `rwv materialize` later when you want the install hooks to run.

`.rwv-active` is the single source of truth for which project is active at the weave root. Changing directories into `projects/<name>/` does not switch the active project; action verbs (`rwv lock`, `rwv add`, `rwv sync`) read `.rwv-active`. When CWD is under `projects/<name>/` and that name differs from `.rwv-active`, verbs that write (`add`, `remove`, `role`, `lock`, `update`, `push`, `sync`, `sync-to`, `fetch` with no source, `doctor --fix`) refuse with `cwd-in-inactive-project`, naming `--project <name>`, `--project <active>`, and `rwv activate <name>`. `rwv status` warns and reports on the active project; `rwv doctor` reports on the active project without a warning.

## One-shot project override

To run a single command against another project without changing `.rwv-active`:

```bash
rwv lock --project mobile-app
```

## When switching is too slow

If the dependency diff between projects is large, or you need both projects open simultaneously, create a workweave instead. Each workweave has its own `node_modules/`, `.venv/`, `target/`, and surfaced ecosystem files:

```bash
rwv workweave mobile-app create dev
cd ../.workweaves/mobile-app--dev
```

## Related

- [Create a feature workweave](./create-feature-workweave.md) — isolated workspaces for parallel work without switching
- [Monorepo lens](../explanation/lenses/monorepo.md) — when to switch vs. when to use a workweave
- [File formats reference](../reference/formats.md) — `.rwv-active` format
