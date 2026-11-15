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

## When projects share a member

A member two projects list has one checkout. `rwv activate` leaves it where it is, and the incoming project's install hooks run against it, so the switch refuses while that member holds uncommitted changes or sits where the incoming project's branch does not put it. For a member on another branch:

```text
rwv activate: github/acme/lib: alignment on branch (main)
choices:
  align members for the checked-out branch
    rwv activate mobile-app --update
```

`--update` aligns `mobile-app`'s members as `rwv update` does, moving the ones whose commits stay on some branch, and then activates:

```bash
rwv activate mobile-app --update
```

For a member with uncommitted changes the refusal offers committing them or setting them aside, and `--update` refuses it too, before anything moves:

```text
rwv activate: github/acme/lib: edits dirty
    src/lib.rs
choices:
  commit your edits, or set them aside
    git -C /work/weave/github/acme/lib commit
    git -C /work/weave/github/acme/lib stash push
```

A member `--update` leaves in place, because it holds commits no other branch reaches, refuses the switch after the others have moved; the refusal lists the commits and the choices that keep them.

To switch the active project and weave-root symlinks without running install hooks:

```bash
rwv activate mobile-app --no-materialize
```

It says what it left out:

```text
[skipped] core: the install hooks did not run (--no-materialize); `rwv materialize` runs them
```

Run `rwv materialize` later when you want the install hooks to run.

`.rwv-active` is the single source of truth for which project is active at the weave root. Changing directories into `projects/<name>/` does not switch the active project; action verbs (`rwv lock`, `rwv member add`, `rwv sync`) read `.rwv-active`. When CWD is under `projects/<name>/` and that name differs from `.rwv-active`, verbs that write (`member add`, `member remove`, `member set-role`, `lock`, `update`, `pull`, `fetch`, `push`, `sync`, `land`, `doctor --fix`) refuse with `cwd-in-inactive-project`, naming `--project <name>`, `--project <active>`, and `rwv activate <name>`. `rwv status` warns and reports on the active project; `rwv doctor` reports on the active project without a warning.

## One-shot project override

To run a single command against another project without changing `.rwv-active`:

```bash
rwv lock --project mobile-app
```

`rwv update`, `rwv pull` and `rwv update --remote` given `--project` for a project the weave root does not present move no member the two projects share: one that is not aligned for that project refuses the run and names `rwv activate <name> --update`.

An authoring verb (`member add`, `member remove`, `member set-role`, `member set-recurse-submodules`, `update --remote`) given `--project` for a project the weave root does not present regenerates that project's files and stops there, saying so:

```text
[skipped] core: the weave-root links were not refreshed (`mobile-app` is not the project this weave root presents); `rwv activate mobile-app` refreshes them
[skipped] core: the install hooks did not run (`mobile-app` is not the project this weave root presents); `rwv activate mobile-app` runs them
```

## When switching is too slow

If the dependency diff between projects is large, or you need both projects open simultaneously, create a workweave instead. Each workweave has its own `node_modules/`, `.venv/`, `target/`, and surfaced ecosystem files:

```bash
rwv workweave create dev --project mobile-app
cd ../.workweaves/mobile-app--dev
```

## Related

- [Create a feature workweave](./create-feature-workweave.md) — isolated workspaces for parallel work without switching
- [Monorepo lens](../explanation/lenses/monorepo.md) — when to switch vs. when to use a workweave
- [File formats reference](../reference/formats.md) — `.rwv-active` format
