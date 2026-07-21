# How-to: Write a plugin

A plugin is an executable named `rwv-<verb>` on `$PATH`. When `rwv <verb>` is
invoked and `<verb>` is not a built-in command, `rwv` locates the first matching
executable in `$PATH` and execs it. This page covers how to read the context
`rwv` hands your plugin, how to call back into `rwv`, and how to stay compatible
as `rwv` evolves.

The conceptual framing — what belongs in a plugin versus core, what a plugin may
and may not write — is in the [plugin-boundary](../explanation/joints/plugin-boundary.md)
joint.

## What `rwv` provides at exec time

`rwv` sets a context envelope on every plugin spawn before `exec`. The variables
and their semantics:

| Variable | Value | Unset when |
|---|---|---|
| `RWV_VERSION` | `rwv` semver | never |
| `RWV_WORKSPACE` | primary workspace root (absolute path) | no workspace resolved |
| `RWV_WORKWEAVE` | `<project>--<name>` | not in / not addressing a workweave |
| `RWV_PROJECT` | resolved project name | no workspace or project resolved |

(This is the same table as in the [CLI reference](../reference/cli.md#context-envelope);
it is reproduced here for writing-plugin context. If the two ever diverge, the
reference table is authoritative.)

`rwv` never reads any of these variables itself. They are outputs set at spawn for
the child; direction is one way.

### Checking whether you are inside a workspace

Test `$RWV_WORKSPACE`:

```sh
if [ -z "${RWV_WORKSPACE:-}" ]; then
  echo "rwv-myverb: not inside a workspace (use rwv -C <path> myverb to address one)" >&2
  exit 1
fi
```

### Checking whether you are inside a workweave

Test `$RWV_WORKWEAVE`. Its presence is the signal; there is no separate kind
variable.

```sh
if [ -n "${RWV_WORKWEAVE:-}" ]; then
  echo "operating in workweave: $RWV_WORKWEAVE"
fi
```

### Running outside a workspace

Some plugins legitimately run outside any workspace — `--help`, generators, or
commands that set up a workspace from scratch. `rwv` execs the plugin even when no
workspace was resolved (the envelope variables are simply absent). Your plugin
decides whether it requires a workspace.

## Addressing back into `rwv`

Use the envelope values as explicit addressing flags when calling back into `rwv`:

```sh
rwv -C "$RWV_WORKSPACE" --project "$RWV_PROJECT" status --json
```

Do not rely on `rwv` re-discovering the workspace from the cwd of your plugin
process. The `-C` flag is the robust form: the plugin may change directory, may be
invoked with an unusual cwd, or may need to address a specific workspace explicitly.

When you are inside a workweave (`$RWV_WORKWEAVE` is set), `-C "$RWV_WORKSPACE"`
already addresses the workweave directory. No extra flag is needed; `rwv` resolves
through the `.rwv-workweave` marker inside that directory.

If you need to address the primary weave from inside a workweave plugin, you cannot
derive the primary path from the envelope directly. Address it explicitly or use
`rwv resolve --primary` to discover it.

## Consuming `rwv` JSON output

`rwv status --json`, `rwv doctor --json`, and other `--json`-capable verbs emit a
self-describing envelope with a `$schema` field. Schemas are committed at
`docs/reference/schemas/<verb>.json` inside the `rwv` repo and are embedded in
`rwv explain <verb>` bundles.

### Schema probing over version arithmetic

Do not test `$RWV_VERSION` to gate on the presence of a field. Test the field
directly:

```sh
# Probe: does this rwv version expose the field we need?
has_lock_summary=$(rwv status --json | jq 'has("lock_summary")')
if [ "$has_lock_summary" = "false" ]; then
  echo "rwv-myverb: requires rwv with lock_summary in status --json output" >&2
  exit 1
fi
```

Structural probing degrades gracefully when fields are added (the check keeps
passing), and gives an actionable error when the field is absent. A version ceiling
fails silently when a field is backported to an older branch and triggers a false
negative on every compatible version above the ceiling.

Use `$RWV_VERSION` only when you need a floor for a behavioral change that has no
structural signal — for example, a change to how `rwv` handles a flag. Those cases
are rare; most plugin needs are field-shaped.

### Additive-schema guarantee

Within a major `rwv` version, `--json` schemas only gain fields; they never remove
or re-type existing fields. A plugin that reads `repos[].absolute_path` from
`rwv status --json` will keep working across patch and minor releases. Major-version
breaks are flagged in the migration guide.

Pre-1.0 (`0.x`): the standard pre-V1 rules apply — schema breaks are permitted
with changelog notice. Plugin authors targeting `0.x` accept that.

## Naming and visibility

- Name your executable `rwv-<verb>` where `<verb>` describes the operation.
  One-word verbs are conventional; hyphens are allowed (`rwv-check-deps`).
- Do not name it after an existing `rwv` core verb — core always wins at dispatch
  time, making the plugin unreachable.
- `rwv --help` lists plugins it discovers on `$PATH` under an "External commands"
  section, names only. Shadowed duplicates (a later `$PATH` entry with the same
  name) are excluded from this list — they appear in `rwv doctor --json` for audit.

## What your plugin must not write

Do not write rwv-owned files:

- `rwv.yaml`, `rwv.lock`
- `.rwv-active`, `.rwv-workweave`, `.rwv-workweave-index`
- Ecosystem workspace files managed by an integration (`Cargo.toml`, `go.work`,
  `package.json`, and so on)
- Savepoint refs

The full list is in the [plugin-boundary](../explanation/joints/plugin-boundary.md)
joint. Writes to these files corrupt `rwv`'s composition state; `rwv doctor` will
surface the violation after the fact.

## Exit codes and output

`rwv` propagates your exit code verbatim. Signal death is mapped to `128 + N` with
a note on stderr. `rwv` does not wrap or capture your stdout or stderr; your plugin
owns its I/O entirely. This means JSON output, terminal-control sequences, and
streaming progress all work without translation.
