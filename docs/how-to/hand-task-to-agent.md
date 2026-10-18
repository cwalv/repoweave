# Hand a task to an agent

Give an AI coding agent workspace context via `rwv prime`, verb reflection via `rwv explain`, and an isolated multi-repo sandbox via `rwv workweave`:

```bash
# 1. Register Claude Code session and worktree hooks (once per machine)
rwv setup claude

# 2. Or create a dedicated agent workweave manually and land when done
rwv workweave web-app create agent-task-99
cd ../.workweaves/web-app--agent-task-99
# ... agent edits, tests, and commits across repos ...
rwv sync-to --retire
```

## Discover the workspace (`rwv prime`)

Run `rwv prime` from the weave or workweave root to emit structured workspace context (active project, member roles, repository paths, and lock status) for an LLM system prompt:

```bash
rwv prime
```

`rwv prime` produces no output when CWD is outside a weave or workweave (pass `--no-suppress` to always emit). Running `rwv setup claude` registers `rwv prime` on `SessionStart` and `PreCompact` hooks and wires `WorktreeCreate` / `WorktreeRemove` so `claude --worktree` creates and removes full multi-repo workweaves automatically. It edits `~/.claude/settings.json` and refuses (`claude-settings-missing`) until Claude Code has run once and created that file.

## Discover verbs and schemas (`rwv explain`)

Instead of scraping `rwv --help`, agents can query the built-in reflection surface:

```bash
rwv explain              # list all explainable verbs and topics
rwv explain <verb>       # markdown bundle: purpose, flags, exit codes, and JSON Schema
```

Every `rwv explain <verb>` bundle is compiled into the binary from `docs/reference/explain/` and embeds the exact JSON Schema for `--json`-capable verbs.

## Consume `--json` and NDJSON output

Every `--json`-capable verb emits a self-describing document carrying a `$schema` URL:

| Verb | Envelope key (`-j 1`) | Default `-j` worker count |
|---|---|---|
| `rwv status --json` | `repos` | `1` (envelope) |
| `rwv doctor --json` | `violations` | `1` (envelope) |
| `rwv fetch --json` | `outcomes` (+ `placements`) | `min(nproc, 8)` (NDJSON on multi-core; pass `-j 1` for envelope) |
| `rwv update --json` | `repos` (+ `placements`) | `min(nproc, 8)` (NDJSON on multi-core; pass `-j 1` for envelope) |
| `rwv push --json` | `outcomes` | `1` (envelope; pass `-j N > 1` for NDJSON) |
| `rwv sync --json` | `outcomes` (+ `placements`) | `1` (envelope; pass `-j N > 1` for NDJSON) |
| `rwv sync-to --json` | `outcomes` (+ `placements`) | `1` (envelope; pass `-j N > 1` for NDJSON) |

- **Serial / envelope mode (`-j 1`):** One JSON object `{ "$schema": "<url>", "<key>": [...] }` written at completion.
- **Parallel / NDJSON mode (`-j N` where `N > 1`):** One self-describing JSON line streamed per completed unit of work, each carrying its own `$schema` field. Because `rwv fetch` and `rwv update` default to `min(nproc, 8)` workers, pass `-j 1` when your harness wants a single JSON envelope from those two verbs, or branch on whether the first line is a complete JSON object.

## Isolate agent work in a workweave

When creating a workweave for an agent (`rwv workweave web-app create agent-task-99`), the agent's directory at `../.workweaves/web-app--agent-task-99/` gets:

- a git worktree on an ephemeral branch for every member repo (`reference` repos have read-only tracked files), populated via copy-on-write reflinks or read-only hardlinks where supported
- isolated tool state (`node_modules/`, `.venv/`, `target/`), installed by the integration install hooks `create` runs, and its own `rwv.lock`
- a `.rwv-workweave` marker pointing back to the parent workspace

When the agent finishes committing across member repos, `rwv sync-to --retire` auto-relocks CWD, replays the work onto the parent workspace, fast-forwards the parent, and deletes the workweave. If a conflict occurs, the workweave is preserved so the agent or operator can resolve it and run `rwv sync-to --continue`.

## Filter repos with selectors

Verbs that scan repositories (`fetch`, `update`, `push`) accept `--role` and `--repo` filters (combined as a union; regexes match against the full `github/org/repo` manifest path):

```bash
rwv fetch chatly/web-app --role owned
rwv push --repo glob:'github/chatly/*'
rwv update --role owned --repo re:'proto'
```

A narrowed `rwv update` regenerates no integration files and runs no install hooks, and says so:

```text
[skipped] core: the integration files were not regenerated (the update was narrowed by --role or --repo); `rwv doctor --fix` regenerates them
[skipped] core: the install hooks did not run (the update was narrowed by --role or --repo); `rwv materialize` runs them
```

## Related

- [Agent lens](../explanation/lenses/agent.md) — parallel agent workflow patterns
- [Create a feature workweave](./create-feature-workweave.md) — workweave creation and landing
- [Bring workweave work home](./bring-workweave-work-home.md) — `rwv sync-to` and multi-workweave landing
- [Recover from a sync conflict](./recover-from-sync-conflict.md) — resolving conflicts during `sync` or `sync-to`
- [CLI reference](../reference/cli.md) — complete flag and selector grammar reference
