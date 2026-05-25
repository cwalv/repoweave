# rwv prime

## Purpose

Emit structured workspace context as markdown for agent consumption.
`prime` is the workspace-context-recovery command — it is registered by
`rwv setup claude` as both a **SessionStart** and **PreCompact** hook so
agents always receive current workspace state at session start and before
context compaction.

The markdown output (`render_context`) covers: workspace location (weave or
workweave), active project, repository table with roles and branches,
enabled integrations, key commands, directory layout, and agent integration
surfaces (structured-output verbs, per-verb reflection via
`rwv explain <verb>`, and schema paths). Agents consume this as a
system-reminder-style artifact injected at the start of their context.

`prime` is intentionally **silent (exit 0)** when CWD is not inside any
weave or workweave — absence of output is not an error. Pass
`--no-suppress` to always emit output: outside a workspace this produces
`render_overview`, a fuller orientation document covering repoweave
concepts (weave / workweave / lock-and-sync), common pitfalls, a typical
multi-repo flow, and the full command reference. The `--no-suppress` path
is used by SessionStart hooks that may fire before the agent has cd'd into
a workspace.

There is no `--json` mode; prime is the markdown-orientation channel by
design.

The same `render_context` output is written to `AGENTS.md` by
`rwv setup agents-md` for non-Claude agents (Cursor, Copilot, etc.) that
read AGENTS.md.

## Invocation

```
rwv prime [--no-suppress]
```

Run `rwv --help prime` for the full clap surface.

## Output

**Inside a weave or workweave:** a markdown document with sections:

- Workspace location (weave or workweave path) and active project.
- Repository table (path, role, branch, URL) from the active project's
  `rwv.yaml`.
- Enabled integrations.
- Key commands reference table.
- Directory layout tree (registry dirs, projects with active marker).
- Agent integration surfaces: structured-output verbs, per-verb reflection
  via `rwv explain <verb>`, committed schema paths.

**Outside any workspace without `--no-suppress`:** no output (silent).

**Outside any workspace with `--no-suppress`:** an orientation document
(`render_overview`) covering concepts, common pitfalls, a typical
multi-repo flow, essential commands, and sync-family guidance.

## Exit codes

- `0` — always. `prime` does not fail on workspace-resolution errors; it
  is either silent or emits orientation context depending on `--no-suppress`.

## Examples

Prime the workspace (typically invoked by a SessionStart hook):

```
rwv prime
```

Always emit context even outside a workspace (SessionStart hook outside a
weave):

```
rwv prime --no-suppress
```

Register `rwv prime` as Claude Code SessionStart + PreCompact hooks:

```
rwv setup claude
```

Generate AGENTS.md with the same content for non-Claude agents:

```
rwv setup agents-md
```

## Common errors

- *project not active* — no `.rwv-active` is set; prime emits weave-level
  context but omits the repository table and per-project sections.
- *manifest parse failure* — if the active project's `rwv.yaml` cannot be
  parsed, the repository table section is silently omitted (the rest of the
  output still emits).
