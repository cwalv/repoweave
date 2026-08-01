# Switch projects

A weave holds multiple projects under `projects/`; one is *active* at a time. The active project's ecosystem files are symlinked at the weave directory so build tools see a coherent workspace.

## Switch with `rwv activate`

```bash
rwv activate mobile-app
```

This:

1. Updates `.rwv-active` to `mobile-app`.
2. Removes ecosystem-file symlinks at the weave directory.
3. Regenerates ecosystem files in `projects/mobile-app/` from its `rwv.toml`.
4. Symlinks them to the weave directory.

`.rwv-active` is the single source of truth for which project is active within a workspace. There is no CWD-based override: cd-ing into `projects/<name>/` does not switch the active project. Action verbs (`rwv lock`, `rwv add`, `rwv sync`) read `.rwv-active` and operate on the active project regardless of CWD.

When CWD is under `projects/<name>/` and that name differs from `.rwv-active`, action verbs that ambiguously apply emit a helpful error suggesting both fixes (`rwv activate <name>` or `--project <name>` for a one-shot).

## Reconcile tool state after switching

Ecosystem tool state (`node_modules/`, `.venv/`, `target/`) is shared across projects in the same workspace. After `rwv activate`, run the ecosystem's install command to reconcile dependencies for the new project's package set:

```bash
rwv activate mobile-app
npm install            # or: uv sync, cargo build, etc.
```

This is incremental — only the dep diff is installed/removed.

## When switching is too slow

If the dep diff between projects is large, or you need both projects active simultaneously, use a workweave instead. A workweave has its own `node_modules/`, `.venv/`, `target/`, ecosystem files, and active-project marker — no reconciliation needed:

```bash
rwv workweave mobile-app create dev
cd ../.workweaves/mobile-app--dev
```

Workweaves live at `<parent>/.workweaves/<project>--<name>/` — by default a sibling of the weave root.

See [create a feature workweave](./create-feature-workweave.md) for the workweave primitive.

Workweaves are also the answer when you want parallel work on the *same* project without disturbing the primary weave — see the [monorepo lens](../explanation/lenses/monorepo.md) for the recommended patterns.

## One-shot project override

For a single command without changing the active project:

```bash
rwv lock --project mobile-app
```

This applies the verb to `mobile-app` without modifying `.rwv-active`. Useful in scripts and CI.

## Related

- [create a feature workweave](./create-feature-workweave.md) — for parallel work without switching
- [monorepo lens](../explanation/lenses/monorepo.md) — when to switch vs. when to workweave
- [reference/formats](../reference/formats.md) — `.rwv-active` shape
