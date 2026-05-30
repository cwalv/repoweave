# CLI reference

Lookup-shaped reference for every `rwv` verb. For conceptual material see the [joints](../explanation/joints/); for task-shaped recipes see the [how-to guides](../how-to/index.md).

> **CI-enforced artifacts.** `docs/reference/explain/` and `docs/reference/schemas/` are build artifacts generated from rust source (schemars + clap-derive) via `rwv explain`. Do not hand-edit those files — CI fails when they diverge from the source. The source-of-truth templates live at `docs/reference/explain/templates/<verb>.md.tmpl`; to add or correct a verb's reflection output, edit the template or the underlying clap-derive struct.

## Verbs

### `rwv` (bare)

Show current context: weave directory, active project, workweave (if any), repos.

### `rwv fetch <source> [...]`

Read `rwv.lock` and align clones to it. Bootstrap when lock is absent.

| Flag | Effect |
|---|---|
| `--frozen` | Error if lock is stale; never advance. Suitable for CI |
| `--role` / `--repo` | Selector filters (see [Selector grammar](#selector-grammar)) |
| `-j N` | Parallel per-repo workers (default: min(nproc, 8)) |
| `--json` | Structured output / NDJSON when `-j N` with `N > 1` |

The lock is **read-only**. To advance tips and re-snapshot, use `rwv update`.

`--json` emits `{ "$schema": "...", "outcomes": [...] }` envelope in serial mode
(`-j 1` or no `-j`). Under `-j N` with `N > 1`, switches to NDJSON (one
self-describing record per repo as workers finish). See [JSON envelope
convention](#--json-envelope-convention).

Anchored by `tests/doc_claims_fetch_test.rs`.

### `rwv update [...]`

**The verb that gets the latest.** Advance each manifest repo to its branch HEAD on the remote, then re-snapshot `rwv.lock`. Maps semantically to `cargo update` / `npm update`.

| Flag | Effect |
|---|---|
| `--role` / `--repo` | Selector filters |
| `-j N` | Parallel per-repo workers (default: min(nproc, 8)) |

Anchored by `tests/doc_claims_update_test.rs`.

### `rwv lock`

Checkpoint current local state into `rwv.lock`. No network access.

Reads HEAD from each repo; records the tag name if HEAD is tagged, otherwise the revision ID. Errors on uncommitted changes (use `--dirty` to bypass).

| Flag | Effect |
|---|---|
| `--dirty` | Lock anyway when working trees have uncommitted changes |

Pure git SHA snapshot — no integration hooks fire. To refresh ecosystem lockfiles (`node_modules`, `.venv`, etc.) after membership changes, run `rwv activate`.

### `rwv activate <project>`

Set the active project. Updates `.rwv-active`, regenerates ecosystem workspace files in the project directory, symlinks them to the weave directory.

`.rwv-active` is the single source of truth for the active project; CWD does not override.

Anchored by `tests/doc_claims_activate_test.rs`.

### `rwv init <name> [--provider <registry>/<owner>] [--adopt]`

Create a new project repo at `projects/<name>/` with empty `rwv.yaml`. With `--provider`, configures the project repo's remote URL. With `--adopt`, scans the working tree and builds an initial `rwv.yaml` from clones already on disk (brownfield migration).

### `rwv add <url> [--role <role>] [--new]`

Clone a repo (if not present), register it in the *active workspace*'s `rwv.yaml`, run integration hooks.

| Flag | Effect |
|---|---|
| `--role <role>` | Sets the role (`owned` / `fork` / `dependency` / `reference`). Defaults to `owned`. |
| `--new` | Init a new local repo at canonical path; infer URL from path convention |

`rwv add` writes to CWD's workspace's manifest (the active workspace's `rwv.yaml`), not always primary's.

### `rwv remove <path> [--delete] [--force]`

Remove from `rwv.yaml`, re-run activation (regenerates ecosystem workspace files).

| Flag | Effect |
|---|---|
| `--delete` | Also remove the clone (errors if another project references it) |
| `--force` | Bypass the cross-project safety check on `--delete` |

### `rwv sync <source> [...]`

Absorb `<source>`'s committed state into CWD. `<source>` is required: a workspace name (`primary`, a workweave name) or a path. Bare `rwv sync` (no `<source>`) is an error with a hint pointing at `rwv sync-to`.

| Flag | Effect |
|---|---|
| `--strategy ff\|rebase\|merge` | Default `ff`. Applies uniformly to project and manifest repos; `rwv.lock` is excluded from project-repo merge inputs and regenerated in Phase 3 |
| `--force` | Bypass lock-freshness precondition; hard-reset project repo to source tip |
| `--json` / `-j N` | Structured output / parallel sync (NDJSON when N > 1) |

See [sync semantics](../explanation/joints/sync-semantics.md) for the three-phase model and the direction-explicit pair with `rwv sync-to`.

Anchored by `tests/doc_claims_sync_test.rs`.

### `rwv sync-to [<target>] [...]`

Advance `<target>` to CWD's tip via a three-step orchestration: (1) rebase/merge CWD against target; (2) auto-relock CWD if manifest tips moved; (3) FF-advance target to CWD's new tip. All rewriting happens in CWD; target is only ever advanced via fast-forward.

`<target>` is a workspace name (`primary`, a workweave name) or a path. Omit inside a workweave to auto-target the parent recorded in `.rwv-workweave`. Required in a primary weave.

| Flag | Effect |
|---|---|
| `--strategy ff\|rebase\|merge` | Default `rebase` (unlike `rwv sync`). Step 3 is always FF regardless |
| `--retire` | Delete the workweave on success. Requires a workweave context; warning + no-op in a primary weave |
| `--force` | Bypass lock-freshness precondition |
| `--continue` | Resume after resolving a mid-op conflict |
| `--json` / `-j N` | Structured output / parallel step-1 sync (NDJSON when N > 1) |

See [sync semantics](../explanation/joints/sync-semantics.md) for the full three-step model, strategy semantics, and the `--retire` contract.

Anchored by `tests/doc_claims_sync_test.rs` (shared schema; `$schema` URL differs).

### Direction conventions: `sync` vs. `sync-to`

`rwv sync` and `rwv sync-to` are a direction-explicit pair. The verb names where CWD's state goes — *to* CWD, or *from* CWD *to* a named target.

| Invocation | CWD context | State source | What changes |
|---|---|---|---|
| `rwv sync <source>` | any workspace | `<source>` | CWD absorbs source's state; CWD's commits land on top |
| `rwv sync-to <target>` | any workspace | CWD | both: CWD aligns with target's state first (CWD's commits on top); target then FF-advances to CWD's new tip |
| `rwv sync-to` (bare) | workweave | CWD | same as above; target = parent recorded in `.rwv-workweave` |
| `rwv sync-to --retire` | workweave | CWD | same as above, plus delete the workweave on success |

The `<source>` argument in `rwv sync` is always required — there is no auto-target. The `<target>` argument in `rwv sync-to` is optional inside a workweave (auto-targets parent); required in a primary weave.

### `rwv push [...]`

Coordinated cross-repo push. Walks the manifest, applies per-role push policy: `owned` repos pushed (with lock-precondition check), `fork` repos skipped, `dependency`/`reference` repos skipped. Project repo is pushed last.

| Flag | Effect |
|---|---|
| `--role` / `--repo` | Selector filters |
| `--force` | Bypass lock-precondition check |
| `-j N` | Parallel push (up to N concurrent) |

Anchored by `tests/doc_claims_push_test.rs`. See [push a cross-repo feature](../how-to/push-cross-repo-feature.md).

### `rwv abort`

Restore CWD's workspace to its pre-sync state using savepoint refs at `refs/rwv/pre-op/<op-id>`. Runs VCS-native abort for in-progress operations (`git rebase --abort`, `git merge --abort`).

Errors if no sync operation is in progress.

### `rwv status [--json] [...]`

Show per-repo state for the CWD workspace.

| Column | Values |
|---|---|
| path | Repo path relative to workspace root |
| branch | Current branch name, `-` if detached |
| tip | Current HEAD SHA (first 12 chars) |
| lock SHA | SHA from `rwv.lock` (first 12 chars), `-` if no lock |
| relation | `ok` / `ahead` / `behind` / `diverged` / `no-lock` / `unknown` |
| mid-op | Present if mid-rebase, mid-merge, etc. |

`--json` emits the envelope `{"$schema": "...", "repos": [...]}`. See [JSON envelope convention](#--json-envelope-convention).

Anchored by `tests/doc_claims_status_test.rs`.

### `rwv doctor [...]`

Convention audit. Reports orphaned clones, dangling references, missing roles, stale locks, workweave drift, index drift, working-tree drift, and integration health.

| Flag | Effect |
|---|---|
| `--locked` | Zero exit iff every repo tip matches its lock entry (precondition for `rwv sync`) |
| `--fix` | Auto-remediate safely-fixable findings: index drift, working-tree drift, missing `rwv.lock merge=ours` replay-exclusion, and legacy `role: primary` manifest spellings. Never touches live staged content or live edits. Idempotent. |
| `--json` | Emits envelope `{"$schema": "...", "violations": [...]}` |

| Check | Description |
|---|---|
| Orphaned clones | Directories under registry paths not listed in any project's `rwv.yaml` |
| Dangling references | Entries in an `rwv.yaml` pointing to paths not on disk |
| Missing role | `rwv.yaml` entries without a `role` field |
| Stale lock | Project's `rwv.lock` doesn't match current HEAD revisions |
| Workweave drift | Worktrees missing from a workweave or extra worktrees not in manifest |
| Index drift | A repo's index doesn't match HEAD tree (shared-refs side effect) |
| Working-tree drift | A repo's on-disk files don't match HEAD tree (shared-refs side effect) |
| Missing replay-exclusion | A project repo's `.gitattributes` lacks `rwv.lock merge=ours` (`--fix` appends it) |
| Legacy `role: primary` | A project `rwv.yaml` uses the pre-rename spelling; `--fix` rewrites each `role: primary` line to `role: owned`, preserving comments and key order |
| Integration checks | Per-integration check hooks (tool availability, stale config) |

### `rwv workweave <project> create <name>`

Create a workweave: worktrees on ephemeral branches for each repo, generated ecosystem files, per-workweave tool state.

| Flag | Effect |
|---|---|
| `--from <source>` | Fork from a specific source (default: CWD's active workspace) |

Workweaves live at `<parent>/.workweaves/<project>--<name>/`.

### `rwv workweave <project> delete <name> [--force]`

Delete a workweave. Default refuses if any worktree is dirty; `--force` bypasses.

### `rwv workweave <project> list`

List workweaves for a project.

### Scripting helpers

Three verbs designed for shell-script and agent-harness consumption. Side-by-side coverage so the shapes are obvious.

#### `rwv prime [--no-suppress]`

Emit structured workspace context for agent system prompts: active project, repos, roles, lock state. Suppressed when CWD is not inside a weave or workweave (use `--no-suppress` to always emit).

Use case: agent harness reads this on session start to bootstrap context.

#### `rwv resolve`

Print the weave directory (workweave dir if in a workweave, otherwise weave root). Useful in scripts:

```bash
cd $(rwv resolve)
```

Use case: one-shot path resolution from anywhere inside a workspace.

#### `rwv explain [<verb>]`

Per-verb reflection — *replaces hand-maintained `--help` scraping* in agent harnesses.

| Invocation | Returns |
|---|---|
| `rwv explain` | List of explainable verbs |
| `rwv explain <verb>` | Markdown bundle: purpose, invocation, output description, JSON Schema (for `--json`-capable verbs), exit codes, examples, common errors |

Use case: agent harness asks "what flags does `rwv push` take, and what does it print?" — the bundle is authoritative. For `--json`-capable verbs (`status`, `doctor`, `sync`), the JSON Schema is embedded as a fenced code block inside the bundle.

The rendered bundles are checked in at `docs/reference/explain/` for offline browsing. CI fails if they diverge from the generator output.

### `rwv setup claude [--uninstall]`

Register `rwv prime` as a Claude Code hook (`SessionStart` + `PreCompact`). `--uninstall` removes all rwv hooks.

### `rwv setup agents-md`

Generate `AGENTS.md` at the workspace root for Cursor, Copilot, and other AGENTS.md-aware tools.

### `rwv completions <shell>`

Generate shell completions (bash, zsh, fish, etc.). Source the output in your shell rc file.

## Selector grammar

`rwv fetch`, `rwv update`, and `rwv push` share a selector surface for picking which repos to operate on.

### Flags

| Flag | Selects on |
|---|---|
| `--role <kind>` | Repo role (`owned`, `fork`, `dependency`, `reference`) |
| `--repo <pattern>` | Repo's manifest path |

`--role` parsing is **case-insensitive** (`--role Owned`, `--role OWNED` both work).

`--repo` patterns match against the *manifest-repo path* (e.g., `github/chatly/server`), not against project repo paths.

### Pattern prefixes

`--repo` accepts three pattern forms:

| Prefix | Form | Example |
|---|---|---|
| (none) | Exact match | `--repo github/chatly/server` |
| `re:` | Regex | `--repo re:'^github/chatly/(server|web)$'` |
| `glob:` | Glob | `--repo glob:'github/chatly/*'` |

### Union semantics

Repeated flags are **union**:

```bash
rwv fetch --role owned --role fork                  # owned ∪ fork
rwv push --repo github/chatly/server --repo github/chatly/web   # server ∪ web
```

There is no intersection syntax. To compose intersections, pipe `rwv status --json` through `jq` (see [run a command across repos](../how-to/run-a-command-across-repos.md)).

`--role` and `--repo` together act as **union** as well: `--role owned --repo glob:'github/chatly/*'` matches owned repos *or* repos under `github/chatly/`. To filter both, use `--role` alone or `jq` post-filtering.

Anchored by the selector tests in `tests/`.

## `--json` envelope convention

Every JSON-capable verb emits a self-describing envelope:

```json
{ "$schema": "<schema-url>", "<key>": [...] }
```

- `"$schema"` is a stable URL per release.
- `"<key>"` is verb-specific:

| Verb | Envelope key |
|---|---|
| `rwv status --json` | `repos` |
| `rwv doctor --json` | `violations` |
| `rwv fetch --json` | `outcomes` |
| `rwv sync --json` | `outcomes` |
| `rwv sync-to --json` | `outcomes` |

Schemas live at `docs/reference/schemas/<verb>.json` and are also embedded as fenced code blocks inside the corresponding `rwv explain <verb>` bundle.

### NDJSON under parallel mode

When a verb runs with `-j N` and `N > 1`, the output switches to NDJSON: one JSON record per line as workers finish, **no envelope wrapper**, and each line carries its own `"$schema"` field so a consumer can identify any single record without out-of-band context.

The branch-on-shape pattern lets consumers handle both modes uniformly: peek at stdout; if subsequent lines carry their own `"$schema"`, parse as NDJSON; otherwise parse as one envelope document.

Exit semantics under `--json` are the same in both modes: non-zero iff at least one per-repo outcome is `failed`.

Anchored by `tests/doc_claims_sync_test.rs`, `tests/doc_claims_fetch_test.rs`, and the per-verb `sync_json_test.rs` / `fetch_json_test.rs` families.

## Related

- [reference/formats](./formats.md) — `rwv.yaml`, `rwv.lock`, `.rwv-active`, `.rwv-workweave`
- [reference/roles](./roles.md) — role definitions and change-resistance semantics
- [reference/glossary](./glossary.md) — terminology lookup
- [reference/integrations](./integrations/index.md) — per-integration generated files and config
- [explanation/joints](../explanation/joints/) — conceptual material referenced by these verbs
