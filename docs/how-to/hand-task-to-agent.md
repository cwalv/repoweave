# Hand a task to an agent

How a harness drives `rwv` today: discover the workspace, discover the verbs, work in an isolated workweave, bring the result home.

This page covers only the *landed* agent surface. For the recommended workflow pattern (dedicated agent workweaves as gravity wells), see the [agent lens](../explanation/lenses/agent.md).

## Discover the workspace

Drop the harness into the weave or workweave root. The agent prompt picks up `rwv prime`'s output for context:

```bash
rwv prime
```

`rwv prime` emits a structured workspace context (active project, roles, repo paths, lock state) suitable for an LLM system prompt. It is suppressed when CWD is not inside a weave or workweave (pass `--no-suppress` to always emit). To register `rwv prime` as a Claude Code hook so it runs automatically on session start:

```bash
rwv setup claude
```

For Cursor / Copilot / AGENTS.md-aware tools:

```bash
rwv setup agents-md
```

## Discover the verbs (JIT reflection)

The agent's harness should not scrape `rwv --help`. The reflection endpoint is `rwv explain`:

```bash
rwv explain                        # list every explainable verb
rwv explain <verb>                 # markdown bundle for that verb
```

The bundle has a fixed shape — *Purpose*, *Invocation* (flags, types, defaults), *Output* (a description plus, for `--json`-capable verbs, the JSON Schema as a fenced code block), *Exit codes*, *Examples*, *Common errors*. Build artifact of `cargo run --bin generate-explain`; always in sync with the binary. Use it to discover available flags, JSON-output shapes, and selector grammar without round-tripping through documentation.

The rendered output is committed at `docs/reference/explain/` for offline browsing. Those files are build artifacts — do not hand-edit them. CI fails when the rendered output diverges from the source.

## Consume `--json` output

Every JSON-capable verb emits a self-describing envelope:

```json
{ "$schema": "<url>", "<key>": [...] }
```

The key is verb-specific:

| Verb | Key |
|---|---|
| `rwv status --json` | `repos` |
| `rwv doctor --json` | `violations` |
| `rwv fetch --json` | `outcomes` |
| `rwv update --json` | `repos` |
| `rwv sync --json` | `outcomes` |
| `rwv sync-to --json` | `outcomes` |
| `rwv push --json` | `outcomes` |

Schemas live at `docs/reference/schemas/<verb>.json` and are also embedded inside the corresponding `rwv explain <verb>` bundle. Agents should resolve `$schema` once and cache, not assume any shape.

### NDJSON under `-j N > 1`

`rwv fetch`, `rwv update`, `rwv push`, `rwv sync`, and `rwv sync-to` all switch to NDJSON when run with `-j N > 1` and `--json`:

- **Serial / envelope mode** (`-j 1`, the default): one envelope document on stdout at the end of the run.
- **Parallel / NDJSON mode** (`-j N > 1`): each per-repo outcome streamed as one JSON line as its worker finishes. The envelope wrapper is dropped; every line carries its own `$schema` field so a consumer can identify it without context.

Branch on shape, not on the verb: peek at stdout's first record. If it's a single envelope, parse as one document; if subsequent lines have their own `$schema`, parse as NDJSON.

## Recommended workflow: agent workweave

Give the agent its own workweave so its work is isolated from human-driven changes:

```bash
# from the primary weave
rwv workweave web-app create agent-task-99
cd ../.workweaves/web-app--agent-task-99
```

Workweaves live at `<parent>/.workweaves/<project>--<name>/` — by default a sibling of the weave root, so from the weave root the path is `../.workweaves/<project>--<name>/`.

The agent runs in this directory:

- has its own worktrees on ephemeral branches for `owned`, `fork`, and
  `dependency` repos; `reference` repos are symlinks to the canonical
  weave-root clone shared with every other workweave (read-only)
- has its own `node_modules/`, `.venv/`, `target/`
- can `rwv lock` without touching primary's lock
- has `.rwv-workweave` recording primary as its parent

When the agent is done, bring the work home with one verb:

```bash
rwv sync-to --retire
```

This lands the workweave's commits into its recorded parent (primary in this case) and deletes the workweave on success. Bare `rwv sync-to` auto-targets the parent recorded in `.rwv-workweave`; `--retire` deletes the workweave after the landing succeeds. If step 1 hits a conflict, the workweave is preserved for the operator to fix and re-run with `rwv sync-to --continue`; see [recover from sync conflict](./recover-from-sync-conflict.md).

## Selector grammar (filtering for agent tasks)

Action verbs that scan repos accept `--role` and `--repo` filters:

```bash
rwv fetch chatly/web-app --role owned       # only repos with role: owned
rwv push --repo glob:'github/chatly/*'      # owned + manifest-path glob
rwv update --role owned --repo re:'^proto'
```

Patterns accept `Exact` (no prefix), `re:` (regex), and `glob:` (glob). Repeated flags are union. `--role` is case-insensitive. See [reference/cli — Selector grammar](../reference/cli.md#selector-grammar).

## Related

- [agent lens](../explanation/lenses/agent.md) — the motivation and recommended workweave-as-gravity-well pattern
- [bring workweave work home](./bring-workweave-work-home.md) — sync semantics, `--retire`, manual ceremony
- [recover from sync conflict](./recover-from-sync-conflict.md) — conflict resolution path
- [reference/cli](../reference/cli.md) — full verb surface, including the Scripting helpers section
