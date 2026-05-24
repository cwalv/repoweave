# The agent lens

> Project-first, session-last.

This lens is for harnesses, automation, and LLM-driven agents that need to drive a multi-repo workspace without the ambient session state a human relies on. It's also for the operator who wants to *delegate* work to an agent and bring the result back home cleanly.

Pick it up if you've ever had to explain your repository topology to an LLM every time you started a new session, or watched an agent accidentally refactor a third-party dependency it had no business touching.

## Humans use sessions; agents use missions

The [workspace lens](./workspace.md) describes how a human activates a project to make their *entire terminal* feel like the project — `cd ~/work`, then `cargo test --workspace`, then any tool sees a coherent workspace. Activation is a session-level operation.

Agents don't want sessions. They want a *path* — a workspace directory where the project is already coherent, where `rwv prime` reports who's who, and where the role annotations tell them what they're allowed to mutate. Agent interactions are project-anchored rather than session-anchored: they take a path and a task, and they don't depend on ambient shell state.

This is why most of the agent surface in `rwv` is *reflection over the project* rather than *commands that mutate session state*.

## The landed agent surface

This lens describes the recommended workflow pattern; the parts that are *current tool behavior today* (vs. recommended pattern) are these:

### Discovery — `rwv prime`, `rwv setup`

`rwv prime` emits a structured "this is a repoweave workspace and here is what you can do" advert: active project, project repos, roles, lock state. Agent harnesses pick this up at session start as a high-signal map of the workspace that fits in an LLM's context window.

```bash
rwv prime                          # emit the advert
rwv prime --no-suppress            # always emit (default suppresses outside a weave)
rwv setup claude                   # register prime as a Claude Code SessionStart hook
rwv setup agents-md                # generate AGENTS.md for Cursor/Copilot/etc.
```

`rwv setup claude` and `rwv setup agents-md` are per-harness setup verbs. They're not deeply scalable to a dozen harnesses, but the shape is right for the current population.

### JIT reflection — `rwv explain`

Agent harnesses should *not* scrape `rwv --help`. The reflection endpoint is `rwv explain`:

```bash
rwv explain                        # list every verb
rwv explain <verb>                 # human-readable
rwv explain <verb> --json          # machine-readable verb spec
rwv explain <verb> --json-schema   # JSON Schema for the verb's --json output
```

`rwv explain` is reflection over the same clap-derive that powers the CLI, so the output is always in sync with the binary. An agent that wants to know "does `rwv push` support `--force`?" asks `rwv explain push --json`, not its training data.

The rendered output is committed at `docs/reference/explain/` for offline browsing — those files are build artifacts, not hand-authored. CI fails if they diverge from the source.

### Structured output — the `--json` envelope

Every JSON-capable verb emits a self-describing envelope:

```json
{ "$schema": "<url>", "<key>": [...] }
```

The key is verb-specific (`repos` for status, `violations` for doctor, `outcomes` for sync). Schemas live at `docs/reference/schemas/<verb>.json` and are reachable via `rwv explain <verb> --json-schema`. Agents resolve `$schema` once and cache the schema; they don't assume any shape.

Under parallel mode (`rwv sync -j N > 1`), output switches to NDJSON: one JSON record per line as workers finish, no envelope, each line carrying its own `$schema`. The branch-on-shape pattern lets consumers handle both modes uniformly: read the first record; if subsequent lines also have `$schema`, parse as NDJSON.

### Roles as a safety boundary

For an agent, the role on each repo is the difference between a successful refactor and an accidental upstream mutation:

- `owned` — the work surface; agent may edit.
- `fork` — agent may edit, but knows changes ideally flow upstream.
- `dependency` — agent should read but not edit.
- `reference` — agent should treat as read-only.

The role is machine-readable in `rwv.yaml` and surfaces in `rwv status --json` so the harness can use it directly as an allow-list. See [workspace lens — Roles](./workspace.md#roles-change-resistance-made-explicit) and [reference/roles](../../reference/roles.md).

## The recommended pattern: agent workweave as gravity well

The hero workflow for agent delegation is to give the agent its own workweave. *This is a recommended pattern, not a tool primitive* — but the primitives (workweaves, sync, `--retire`, parent tracking) compose into the pattern naturally.

The shape:

```bash
# Human, from the primary weave:
rwv workweave web-app create agent-refactor
# Hand the path .workweaves/web-app--agent-refactor/ to the agent.

# Agent works in that workweave:
cd .workweaves/web-app--agent-refactor
# ... edits, tests, commits across manifest repos ...
rwv lock
git -C projects/web-app commit -am "lock: refactor X"

# Bring it home, with one verb:
rwv sync --retire
```

Three properties make this pattern work:

1. **Isolation from human state.** The agent's workweave has its own `node_modules/`, `.venv/`, `target/`. The human's in-progress edits in the primary weave can't disturb the agent's build, and vice versa.
2. **Project context preserved.** Unlike "clone a repo into a tempdir," the agent sees the *full* workspace — every repo at the project's lock, with `package.json` workspaces / `go.work` / `Cargo.toml [workspace]` wired up. Cross-repo imports work, integration tests work, the agent's refactors can span repos.
3. **Verification, then merge.** The human inspects the workweave's state before running `rwv sync --retire`. The sync is direction-neutral — the human pulls the work in rather than the agent pushing it. Asymmetric in cost: the agent's workweave is modified by sync (mostly nothing, on the happy path); the human's workspace absorbs the change.

### Dedicated long-lived agent workweaves

The pattern generalizes to a *semi-persistent* agent workweave: a workspace that lives across many agent sessions, acting as the gravity well where agent work consolidates before the human decides to bring it all the way home to primary.

The human works in the primary weave. The agent works in `.workweaves/web-app--agent`. Periodically the human reviews and `rwv sync agent`s the work in. Less periodically, the agent `rwv sync primary`s to absorb upstream changes the human has made.

This recommended pattern composes from existing primitives — there is no special "agent weave" type. Parent tracking via `.rwv-workweave` makes `rwv sync` from feature workweaves target their parent (the agent workweave) by default, which is exactly the discipline you want.

The cleanly-composed-from-primitives nature is the elegance. Each piece (workweaves, sync, `--retire`, parent tracking) is general-purpose; the gravity-well pattern is just one application.

## What this lens is *not* about

This lens is the *operator's* view of agent delegation. It does not propose:

- A "rwv agent" subcommand or runtime. Agents drive the existing verbs via `rwv explain` reflection and `--json` consumption.
- A new "agent" role separate from `owned`/`fork`/`dependency`/`reference`. The existing role taxonomy is the safety boundary.
- A managed agent harness. Existing harnesses (Claude Code, Cursor, Copilot, AGENTS.md-aware tools) drive `rwv` directly.

Where the tool already has agent-shaped surface (`rwv prime`, `rwv explain`, the `--json` envelope), it's described as current behavior. Where the recommended workflow goes beyond the tool (dedicated agent workweave, gravity-well pattern), it's marked as a recommended pattern composed from existing primitives.

## The shape, in one paragraph

Agents take a path, not a session. The landed tool surface — `rwv prime` for discovery, `rwv explain` for JIT reflection, the `--json` envelope (with NDJSON under parallel mode) for structured output, roles as a machine-readable safety boundary — gives a harness everything it needs to drive a multi-repo workspace without scraping help text. The recommended workflow pattern is to give the agent its own workweave: isolation from human state, full project context preserved, verification before merge via `rwv sync --retire`. The pattern composes from existing primitives — there's no special agent runtime, and the elegance is exactly that.

## Related

- [Workspace lens](./workspace.md) — the project-as-coordination-entity model
- [Monorepo lens](./monorepo.md) — workweaves as parallel-work primitive
- [Hand task to agent](../../how-to/hand-task-to-agent.md) — the operational recipe
- [Workweave hierarchy](../joints/workweave-hierarchy.md) — parent tracking, one-hop sync
- [Sync semantics](../joints/sync-semantics.md) — phase model, `--retire`, NDJSON
- [reference/cli — Scripting helpers](../../reference/cli.md#scripting-helpers) — `prime`, `resolve`, `explain` side by side
