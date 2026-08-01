# The plugin boundary

A plugin is an external `rwv-<verb>` executable on `$PATH` that `rwv` discovers
and dispatches when a verb is not built into `rwv` itself. This joint is the
rationale: why the boundary sits where it does, and why `rwv` doesn't sandbox
plugins. For the wire contract a plugin author codes against — discovery,
the environment envelope, addressing, exit codes, the write prohibition, the
compatibility guarantee — see the
[plugin-protocol](../../reference/plugin-protocol.md) reference.

The sibling joint [verb-vs-composition](./verb-vs-composition.md) covers whether a
proposed operation earns a core verb at all. This joint covers what happens once
the answer is "it belongs outside core."

## The in-scope test for core

A verb belongs in `rwv` core when it needs **write access to composition state** —
`rwv.toml`, `rwv.lock`, `.rwv-active`, `.rwv-workweave`, ecosystem workspace files,
or the `refs/rwv/*` savepoint-ref namespace. The concrete list a plugin must not
write is in the [plugin-protocol](../../reference/plugin-protocol.md#write-prohibition)
reference; the [file-ownership](./file-ownership.md) joint covers the surfacing and
content-ownership *mechanics* behind those files, not a flat inventory. An operation
that does not read or write those files is not composition-aware in any way that
requires core ownership; it is a candidate for a plugin.

The negative form is equally load-bearing: "needs to run across all repos" is not a
criterion for core. Parallel fan-out, output collection, and per-repo command
dispatch are unix composition; the plugin space is where they live. `rwv` provides
the addressing surface and hands it off.

## Write prohibition

A plugin must not write rwv-owned files — the full list, and why each one is
rwv's to write, is in the [plugin-protocol](../../reference/plugin-protocol.md#write-prohibition)
reference. The rule exists because a plugin that writes these files corrupts
`rwv`'s composition state, and `rwv` has no dispatch-time way to stop it —
`rwv doctor` is the audit surface: it can notice that an rwv-owned file changed
outside a core-verb write, after the fact. That after-the-fact posture is a
deliberate consequence of the security posture below, not an oversight.

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

## What `rwv doctor` does not do for plugins

Plugins do not register doctor checks. Check registration is the integrations axis
— in-tree, reviewed, lifecycle-owned by `rwv`. Giving third-party executables a
hook into doctor's verdict would let arbitrary `$PATH` content shape `rwv`'s own
health report. Doctor's only plugin surface is the inventory described above:
`rwv` observing `$PATH`, not plugins extending `rwv`.

## Related joints

- [plugin-protocol](../../reference/plugin-protocol.md) — the wire contract: discovery,
  the environment envelope, addressing, exit codes, the write prohibition, the
  compatibility guarantee.
- [verb-vs-composition](./verb-vs-composition.md) — whether a proposed operation
  earns a core verb at all; the plugin space is the home for operations that don't.
- [file-ownership](./file-ownership.md) — the surfacing and content-ownership
  mechanics behind the files plugins must not write.
- [verb-vs-vocabulary](./verb-vs-vocabulary.md) — naming discipline for verbs that
  do earn a place in core.
