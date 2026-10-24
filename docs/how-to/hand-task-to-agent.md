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

## Consume `--json` output

Every `--json`-capable verb prints one JSON object, `{ "$schema": "<url>", "<key>": [...] }`, once it is done. `-j` sets how many repos run at once and never changes that shape, so a harness parses stdout as one document whatever worker count it passes or the host defaults to:

| Verb | Envelope key | Default `-j` worker count |
|---|---|---|
| `rwv status --json` | `repos` | — |
| `rwv doctor --json` | `violations` | — |
| `rwv fetch --json` | `outcomes` (+ `placements`) | `min(nproc, 8)` |
| `rwv update --json` | `repos` (+ `placements`) | `min(nproc, 8)` |
| `rwv push --json` | `outcomes` | `1` |
| `rwv sync --json` | `outcomes` (+ `placements`) | `1` |
| `rwv sync-to --json` | `outcomes` (+ `placements`) | `1` |

Per-repo records are sorted by repo path whatever order the workers finished in. To follow repos as they finish, run the command through `rwv each` with `--joblog` or `--results`.

## Isolate agent work in a workweave

When creating a workweave for an agent (`rwv workweave web-app create agent-task-99`), the agent's directory at `../.workweaves/web-app--agent-task-99/` gets:

- a git worktree on an ephemeral branch for every member repo (`reference` repos have read-only tracked files), populated via copy-on-write reflinks or read-only hardlinks where supported
- isolated tool state (`node_modules/`, `.venv/`, `target/`), installed by the integration install hooks `create` runs, and its own `rwv.lock`
- a `.rwv-workweave` marker pointing back to the parent workspace

A workweave created by `claude --worktree`, through the `WorktreeCreate` hook `rwv setup claude` registers, is handed over even when an install hook fails: the failure is a warning on stderr naming `rwv materialize`, and the agent's tool state is incomplete until someone runs `rwv materialize` inside the workweave. A manual `rwv workweave create` exits 1 in the same case, and keeps the workweave.

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
