# The plugin boundary

A plugin is an external `rwv-<verb>` executable on `$PATH` that `rwv` discovers
and dispatches when a verb is not built into `rwv` itself. This joint defines the
boundary between what belongs in core and what belongs in a plugin, what a plugin
may and may not do, and how `rwv` and plugins exchange context.

The sibling joint [verb-vs-composition](./verb-vs-composition.md) covers whether a
proposed operation earns a core verb at all. This joint covers what happens once
the answer is "it belongs outside core."

## The in-scope test for core

A verb belongs in `rwv` core when it needs **write access to composition state** —
`rwv.yaml`, `rwv.lock`, `.rwv-active`, `.rwv-workweave`, ecosystem workspace files,
or savepoint refs. Everything `rwv` owns is listed in the
[file-ownership](./file-ownership.md) joint. An operation that does not read or
write those files is not composition-aware in any way that requires core ownership;
it is a candidate for a plugin.

The negative form is equally load-bearing: "needs to run across all repos" is not a
criterion for core. Parallel fan-out, output collection, and per-repo command
dispatch are unix composition; the plugin space is where they live. `rwv` provides
the addressing surface and hands it off.

## Write prohibition

A plugin must not write rwv-owned files:

- `rwv.yaml`, `rwv.lock`
- `.rwv-active`, `.rwv-workweave`, `.rwv-workweave-index`
- Ecosystem workspace files (`Cargo.toml`, `go.work`, `package.json`, … when
  managed by an integration)
- Savepoint refs (`.rwv-savepoint-*`)

A plugin that writes these files corrupts `rwv`'s composition state. `rwv doctor`
can notice that rwv-owned files changed outside a core-verb write — that audit
surface is where the violation surfaces, not a dispatch-time check.

A plugin may maintain its **own** state anywhere else — its own dotfiles, a
side-database, per-repo metadata files it owns. That is the plugin's own concern
under one rule: claim a namespace that does not overlap with rwv's.

## Security posture

The trust boundary is identical to `git`, `cargo`, and every `PATH`-dispatch tool:
if an attacker can write to your `$PATH`, plugins are not your first problem. `rwv`
does not add an allowlist or per-plugin approval gate, for the same reason those
tools don't: a confirmation prompt breaks non-interactive use and trains click-
through; an allowlist is semantics config (same invocation, different behavior per
machine).

What `rwv` adds that most PATH-dispatch tools lack: **`rwv doctor` inventories the
plugin surface**. `rwv doctor --json` carries a `plugins` array listing every
`rwv-*` executable discovered on `$PATH`, with each binary's absolute path and a
`shadowed` flag when a duplicate name earlier in `$PATH` would win at exec time.
The inventory is reporting only — plugin presence never fails the doctor check or
affects the exit code. The audit surface is where scrutiny belongs; silent dispatch
is correct at invocation time.

The inventory is machine-readable (`rwv doctor --json`) and is the right place to
script periodic audits of what plugins are on a machine.

## Context envelope

`rwv` projects the resolved workspace context into a set of environment variables
set on every plugin spawn. The canonical table is in the CLI reference at
[`docs/reference/cli.md` — Context envelope](../../reference/cli.md#context-envelope).

Key properties, stated here for reasoning about the boundary:

- **Outputs only.** `rwv` sets these variables; it never reads them back. The
  direction discipline is intentional: these are handoff values for the child, not
  ambient state consulted by `rwv`.
- **Unset encodes absence.** `RWV_WORKSPACE` being absent means no workspace
  resolved. `RWV_WORKWEAVE` being absent means the checkout is a primary weave, not
  a workweave. No separate kind variable is needed — presence is the signal.
- **`RWV_VERSION` is always set.** Use it to gate on a minimum `rwv` version if
  needed. Prefer structural field probing over version arithmetic: testing whether
  a JSON field exists is more precise than testing a version floor.
- **Consistent with `--json` output.** The envelope and the `resolution` block in
  `--json` output are projections of the same resolved value. A plugin that reads
  both gets the same coordinates.

## Addressing back into `rwv`

A plugin that needs to invoke `rwv` (to read status, consume `--json` output, or
trigger a core verb) addresses it explicitly using the envelope values:

```sh
rwv -C "$RWV_WORKSPACE" --project "$RWV_PROJECT" status --json
```

The global flags `-C` and `--project` are the addressing surface; `$RWV_WORKSPACE`
and `$RWV_PROJECT` carry exactly the right values. When the plugin runs inside a
workweave (indicated by `$RWV_WORKWEAVE` being set), the `-C` path already
addresses the workweave directory — no extra flag is needed.

## Additive-schema guarantee

Within a major version, `rwv`'s committed `--json` schemas only gain fields; they
never remove or re-type existing fields. This is the promotion of current practice
to a stated guarantee, **hardening at 1.0**. Pre-1.0 the standard pre-V1 rules
apply: schema breaks are permitted with changelog notice, and plugin authors
targeting 0.x accept that.

The practical consequence: a plugin that reads `rwv status --json` and checks for
`repos[].absolute_path` does not need to gate on a version ceiling. It fails only
if `rwv` removes the field in a major-version break, which would be called out in
the migration guide for that major version.

Schema probing is the right forward-compat technique: test for the field you depend
on (`if (.repos[0] | has("absolute_path"))`), not a version ceiling. A plugin that
probes structurally degrades gracefully when fields are added, upgraded, or not yet
present on an older install.

## What `rwv doctor` does not do for plugins

Plugins do not register doctor checks. Check registration is the integrations axis
— in-tree, reviewed, lifecycle-owned by `rwv`. Giving third-party executables a
hook into doctor's verdict would let arbitrary `$PATH` content shape `rwv`'s own
health report. Doctor's only plugin surface is the inventory described above:
`rwv` observing `$PATH`, not plugins extending `rwv`.

## Related joints

- [verb-vs-composition](./verb-vs-composition.md) — whether a proposed operation
  earns a core verb at all; the plugin space is the home for operations that don't.
- [file-ownership](./file-ownership.md) — the canonical list of rwv-owned files
  that plugins must not write.
- [verb-vs-vocabulary](./verb-vs-vocabulary.md) — naming discipline for verbs that
  do earn a place in core.
