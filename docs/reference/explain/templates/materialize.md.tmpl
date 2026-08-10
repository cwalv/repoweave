# rwv materialize

## Purpose

Run the integration install hooks (`npm install`, `uv sync`, `cargo fetch`,
etc.) for the project this checkout already presents, and nothing else.

**Hooks materialize; they never move a pin.** A hook's mandate is to make the
ecosystem state implied by *current membership plus the versions already
recorded* real on disk: a lock file gains what a new member requires, tool
state directories are brought up to what the lock says, and a version the lock
already pins stays where it is. Advancing a dependency is something you ask for
with the ecosystem's own update command (`cargo update`, `npm update`,
`uv lock --upgrade`) — never a side effect of a repoweave verb.

That guarantee is what makes this verb safe to run at any time, and safe to
name as the remedy after a `rwv sync` delivers changes.

### Why this is a separate verb

`rwv activate` does two things: it **selects** a project (writes
`.rwv-active`, moves the weave root's shared names) and it **materializes**
that project's ecosystem state. Only a primary can express selection, and only
for one project at a time — which is why `rwv activate` is refused inside a
workweave, where the project is fixed at creation.

Materialization has no such restriction. It is meaningful wherever the project
identity is already settled: in a workweave always, at a primary for the
project it currently presents. `rwv materialize` is that half on its own.

It takes **no project argument**. Naming a project would be a selection, and
selection is the one thing this verb does not do — `.rwv-active` is never read
as an instruction and never written.

`rwv activate --no-materialize` is the mirror image: select without
materializing. One word names the operation on both sides.

### What it touches

1. **Surfacing repair.** The weave root's symlinks onto the project's owned
   files are re-created if missing, scoped to this project's own files. The
   root's shared names are not moved — the root already presents this project,
   so there is nothing to move.
2. **Install hooks.** Each enabled integration's hook runs against the
   now-in-place symlinks.

It does **not** author managed content. If an integration's managed file is
missing, its hook refuses and names `rwv doctor --fix`, which is the verb that
authors.

## Invocation

```
rwv materialize
```

No flags, no arguments.

Run `rwv --help materialize` for the full clap surface.

## Output

Install hook output goes to stderr. On success there is no confirmation
message.

## Exit codes

- `0` — hooks ran successfully.
- non-zero — no project is presented by this checkout, the workspace could not
  be resolved, the manifest failed to parse, or an integration hook returned an
  error.

## Examples

Refresh a workweave's ecosystem state after syncing source in:

```
rwv sync ../other-weave
rwv materialize
```

Select a project fast, materialize later:

```
rwv activate web-app --no-materialize
rwv materialize
```

## Common errors

- *nothing is materialized at `<path>`: no project is active here* — a primary
  weave with no `.rwv-active`. There is no project to materialize until one is
  selected; run `rwv activate <name>`.
- *managed file missing … run `rwv doctor --fix` to regenerate* — the project's
  ecosystem config was never authored (or was deleted). `materialize` never
  authors; `rwv doctor --fix` does.
- *integration activate-hook error* — an install command returned non-zero. Fix
  the underlying ecosystem problem and rerun.
