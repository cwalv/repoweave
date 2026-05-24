# rwv prime

## Purpose

Print agent-oriented orientation context for the current workspace: weave
or workweave layout, project list, active project, conventions, and
pointers to deeper documentation. `prime` is the bootstrap verb for agents
— it answers "where am I and what can I do here?" in markdown that an LLM
can read directly without parsing JSON.

Prime advertises (but does not dump) the JSON surface: it tells you which
verbs accept `--json`, points at `rwv explain <verb>` for per-verb JIT
reflection, and where committed schemas live. Schemas themselves are not
inlined — that would defeat the context economy that motivated reflection.

## Invocation

```
rwv prime
```

Run `rwv --help prime` for the full clap surface. `prime` emits markdown to
stdout; there is no `--json` mode (and there won't be — prime is the
markdown-orientation channel by design).

## Output

A single markdown document describing the workspace. Sections typically
include:

- Workspace location (weave or workweave) and active project.
- Project list and per-project repo summary.
- Conventions (manifest layout, lock file, registry paths).
- Pointers to `rwv explain <verb>` for JIT reflection and to
  `docs/reference/schemas/` for committed JSON Schema artifacts.

## Exit codes

- `0` — prime emitted successfully.
- non-zero — workspace could not be resolved or a manifest failed to parse.

## Examples

Prime the workspace and feed it into an agent's context:

```
rwv prime
```

Save prime output for later reference:

```
rwv prime > /tmp/workspace-prime.md
```

## Common errors

- *workspace could not be resolved* — `cwd` is not inside a weave or
  workweave. Run from inside the workspace tree.
- *project not active* — no `.rwv-active` is set; prime still emits the
  weave-level context but the per-project section is omitted.
