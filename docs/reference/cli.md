# CLI reference

Lookup-shaped reference for every `rwv` verb. For conceptual material see the [joints](../explanation/joints/); for task-shaped recipes see the [how-to guides](../how-to/index.md).

> **CI-enforced artifacts.** `docs/reference/explain/` and `docs/reference/schemas/` are build artifacts generated from rust source (schemars + clap-derive) via `rwv explain`. Do not hand-edit those files — CI fails when they diverge from the source. The source-of-truth templates live at `docs/reference/explain/templates/<verb>.md.tmpl`; to add or correct a verb's reflection output, edit the template or the underlying clap-derive struct.

> **Flag table drift gate.** The `--flag` names in each verb's flag table below are drift-gated by `tests/doc_claims_cli_md_test.rs`: every flag listed here must exist in `rwv <verb> --help`. CI fails if a removed flag is not also removed from the table. Effect descriptions are intentionally hand-maintained and not drift-checked.

## Global options

These flags precede the subcommand and apply to all verbs.

### `-C <path>` / `--cwd <path>` — path addressing

Resolve the workspace as if `rwv` were invoked from `<path>`. Any path
*inside* a checkout works; the normal containment walk (marker, root, `$HOME`
ceiling) runs from there. Relative path arguments elsewhere on the command line
resolve against this directory.

Prior art for the semantics: `make -C`, `tar -C` (Unix-wide), `git -C`,
`hg`/`sl --cwd` — resolve as if invoked from the given directory. The long
spelling `--cwd` follows `hg`/`sl` (names the semantics precisely: "resolve
as if cwd were this").

```
rwv -C /path/to/weave status
rwv -C /path/to/workweave resolve
rwv -C /tmp/empty-dir init my-project
```

**Repetition is an error.** Passing `-C` twice (or mixing `-C` and `--cwd`)
is rejected: a command line carrying two addresses is a confused invocation
and `rwv` refuses rather than picking one.

**Workweave-name shaped arguments.** If the argument matches the
`<project>--<name>` workweave-name shape and no path exists at that location,
`rwv` emits a corrective error pointing at `-w/--workweave` (the flag for
name-based workweave addressing, available in a future release). To address a
workweave by path, pass the full path to the workweave directory.

## Verbs

### `rwv` (bare)

Show current context: weave directory, active project, workweave (if any), repos.

### `rwv fetch [<source>] [...]`

Read `rwv.lock` and align clones to it. Bootstrap when lock is absent.

Two modes, keyed on whether `<source>` is given:

- **With `<source>`** — a URL (`https://…`, `git@…`, `owner/repo`, or the project name for a `--provider`-configured registry): the *bootstrap* mode. Clones the project repo from `<source>` into the current directory, reads its committed `rwv.lock`, and clones every listed manifest repo to its canonical slot.
- **No `<source>`** — the *in-place repair* mode: re-materialize missing manifest members in the current workspace. Uses the existing `rwv.yaml` and `rwv.lock`; clones any repo whose canonical directory is absent (the `MissingCanonicalClone` / `DanglingReference` findings from `rwv doctor` point here); leaves already-materialized repos alone. Run from the workspace root.

| Flag | Effect |
|---|---|
| `--frozen` | Error if lock is stale; never advance. Suitable for CI |
| `--force` | Bypass safety checks; re-clone even if a canonical clone directory is already present |
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
| `--json` | Structured output: envelope under `-j 1`, NDJSON under `-j > 1` |
| `--dirty` | Allow update with uncommitted changes in repos when relocking |
| `--commit` | Commit `rwv.lock` after writing it |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

`--json` emits one record per repo with `path`, `absolute_path`, `branch`, `kind` (`updated` / `up-to-date` / `failed`), `old_sha`, `new_sha`, and `error`. See `rwv explain update`.

Anchored by `tests/doc_claims_update_test.rs`.

### `rwv lock`

Checkpoint current local state into `rwv.lock`. No network access.

Reads HEAD from each repo; records the tag name if HEAD is tagged, otherwise the revision ID. Errors on uncommitted changes (use `--dirty` to bypass).

| Flag | Effect |
|---|---|
| `--dirty` | Allow locking repos with uncommitted changes |
| `--commit` | Commit `rwv.lock` after writing it |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

Pure git SHA snapshot — no integration hooks fire. To refresh ecosystem lockfiles (`node_modules`, `.venv`, etc.) after membership changes, run `rwv activate`.

### `rwv activate <project>`

Set the active project. Updates `.rwv-active`, regenerates ecosystem workspace files in the project directory, symlinks them to the weave directory.

`.rwv-active` is the single source of truth for the active project; CWD does not override.

Anchored by `tests/doc_claims_activate_test.rs`.

### `rwv init <name-or-source> [--provider <registry>/<owner>] [--adopt]`

Create a new project repo at `projects/<name>/` with an empty `rwv.yaml`. With `--provider`, configures the project repo's remote URL.

When invoked in an **empty directory**, `init` bootstraps that directory as a workspace root (no pre-existing `rwv.yaml` required) and creates the project inside it. Running in a non-empty directory without an existing workspace refuses.

With `--adopt`, `<name-or-source>` is a URL or `owner/repo` shorthand: `init` clones the project repo from that source instead of `git init`-ing a new one (brownfield adoption of an existing project repo).

### `rwv add <url> [--role <role>] [--new]`

Clone a repo (if not present), register it in the *active workspace*'s `rwv.yaml`, run integration hooks.

| Flag | Effect |
|---|---|
| `--role <role>` | Sets the role (`owned` / `fork` / `dependency` / `reference`). Defaults to `owned`. |
| `--new` | Init a new local repo at canonical path; infer URL from path convention |

`rwv add` writes to CWD's workspace's manifest (the active workspace's `rwv.yaml`), not always primary's.

**Canonical path.** For URLs matching the `<host>/<owner>/<repo>` shape (github, gitlab, etc.), the clone lands at `<host>/<owner>/<repo>/`. Other URL shapes (bare `file://` remotes, self-hosted git servers with non-`<owner>/<repo>` paths) land at the URL's tail path segments — the canonical-path convention only applies where the URL exposes an unambiguous owner/repo split.

**Shared-clone warning.** If the target clone directory is already registered by another project in the same weave, `rwv add` proceeds (the manifest entry is added to the active project as usual) and emits a warning to stderr naming the other project(s). Sharing a clone across projects is legal — the same repo can be a `dependency` in one project and `owned` in another — but is worth flagging so accidental double-registration is visible.

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
| `--strategy ff\|rebase` | Default `ff`. Applies uniformly to project and manifest repos; `rwv.lock` is excluded from the per-commit merge inputs git uses during rebase replay and regenerated in Phase 3. (`merge` is not offered — see [sync semantics](../explanation/joints/sync-semantics.md#why-no-merge-strategy).) |
| `--allow-stale-lock` | Consent: skip the lock-freshness precondition on both source and destination |
| `--discard-local-commits` | Consent: discard CWD's project commits not reachable from source, hard-resetting the project repo to source's tip. Pre-sync state preserved in `refs/rwv/pre-op/<id>` |
| `--continue` | Resume a sync interrupted mid-op. All parameters are read from the in-progress op-state file; no other flags may be passed alongside `--continue` (except `--project`) |
| `--json` / `-j N` | Structured output / parallel sync (NDJSON when N > 1) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

See [sync semantics](../explanation/joints/sync-semantics.md) for the three-phase model and the direction-explicit pair with `rwv sync-to`.

**Destination cleanliness preflight.** Before any repo is touched, `rwv sync` scans the destination workspace for uncommitted tracked changes; a dirty path refuses the operation with the offending files listed. `rwv sync-to` runs the same scan on its source (CWD). Drift attributable to rwv (regenerated ecosystem files, activation-owned symlinks) is excluded. Commit or stash the listed paths and retry.

Anchored by `tests/doc_claims_sync_test.rs`. Flag table drift-gated by `tests/doc_claims_cli_md_test.rs`.

### `rwv sync-to [<target>] [...]`

Advance `<target>` to CWD's tip via a three-step orchestration: (1) rebase CWD against target; (2) auto-relock CWD if manifest tips moved; (3) FF-advance target to CWD's new tip. All rewriting happens in CWD; target is only ever advanced via fast-forward.

`<target>` is a workspace name (`primary`, a workweave name) or a path. Omit inside a workweave to auto-target the parent recorded in `.rwv-workweave`. Required in a primary weave.

| Flag | Effect |
|---|---|
| `--strategy ff\|rebase` | Default `rebase` (unlike `rwv sync`). Step 3 is always FF regardless. (`merge` is not offered — see [sync semantics](../explanation/joints/sync-semantics.md#why-no-merge-strategy).) |
| `--retire` | Delete the workweave on success. Requires a workweave context; warning + no-op in a primary weave |
| `--allow-stale-lock` | Consent: skip the lock-freshness precondition on both source and destination |
| `--discard-local-commits` | Consent: discard CWD's project commits not reachable from target, hard-resetting the project repo to target's tip. Pre-sync state preserved in `refs/rwv/pre-op/<id>` |
| `--continue` | Resume after resolving a mid-op conflict. All parameters are read from the in-progress op-state file |
| `--json` / `-j N` | Structured output / parallel step-1 sync (NDJSON when N > 1) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

See [sync semantics](../explanation/joints/sync-semantics.md) for the full three-step model, strategy semantics, and the `--retire` contract.

Anchored by `tests/doc_claims_sync_to_test.rs` (shared schema; `$schema` URL differs). Flag table drift-gated by `tests/doc_claims_cli_md_test.rs`.

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

Coordinated cross-repo push. Default scope: `owned` and `fork` repos (the roles the operator controls). `dependency` and `reference` repos are skipped unless `--role` or `--repo` selectors override the default. Project repo is pushed last.

| Flag | Effect |
|---|---|
| `--role` / `--repo` | Selector filters |
| `--force` | Force-push every repo in the operation (manifest repos and the project repo). Default deny. The lock-precondition check still runs unconditionally |
| `-j N` | Parallel push (up to N concurrent for manifest repos; project repo always last and serial) |
| `--json` / `-j N` | Structured output / parallel push (NDJSON when N > 1) |
| `--dry-run` | Print the push plan without executing |

`--json` emits the envelope `{"$schema": "...", "outcomes": [...]}`. Manifest-repo records use `kind` `pushed`, `skipped`, or `failed`; the project-repo record (always last) uses `kind` `project-repo-pushed` or `project-repo-failed`. See [JSON envelope convention](#--json-envelope-convention).

Anchored by `tests/doc_claims_push_test.rs` and `tests/push_json_test.rs`. See [push a cross-repo feature](../how-to/push-cross-repo-feature.md).

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
| relation | `ok` / `ahead` / `behind` / `diverged` / `no-lock` / `unknown` / `missing` / `unreachable` |
| mid-op | Present if mid-rebase, mid-merge, etc. |

`--json` emits the envelope `{"$schema": "...", "repos": [...]}`. See [JSON envelope convention](#--json-envelope-convention).

A `missing` relation names a clone directory that has vanished; the repair is `rwv fetch` with no source — the in-place re-materialize mode — run from the workspace root (see [`rwv fetch`](#rwv-fetch--source--) above and `rwv doctor` for the paired detection: `MissingCanonicalClone`, `DanglingReference`). An `unreachable` relation names a clone that is present but whose object store lacks the locked SHA; in-place `rwv fetch` does not repair it (with the directory present it performs a local checkout without a network fetch). If the remote still has the revision, `git fetch` in the affected repo re-pulls it; if the remote's history was rewritten, re-lock to a reachable state — see [reconcile repos](../how-to/reconcile-repos.md).

Anchored by `tests/doc_claims_status_test.rs`.

### `rwv doctor [...]`

Convention audit. Reports orphaned clones, dangling references, missing roles, stale locks, workweave drift, index drift, working-tree drift, dead op-leases, cargo version-skew across workspace members, cargo patch shadowing, missing canonical clones (workweave worktrees whose primary clone has vanished), uninitialized submodules, and integration health.

| Flag | Effect |
|---|---|
| `--locked` | Zero exit iff every repo tip matches its lock entry (precondition for `rwv sync`) |
| `--fix` | Auto-remediate safely-fixable findings: index drift, working-tree drift, missing `rwv.lock merge=rwv-ours` replay-exclusion (including migration from the legacy `merge=ours` spelling — auto-commits when the repo has no other staged changes) and its paired durable `merge.rwv-ours.driver` config, and legacy `role: primary` manifest spellings. Never touches live staged content or live edits. Idempotent. |
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
| Missing replay-exclusion | A project repo's `.gitattributes` lacks `rwv.lock merge=rwv-ours` or still carries the legacy `merge=ours` spelling (`--fix` adds/migrates the line and, on migration, commits it) |
| Legacy `role: primary` | A project `rwv.yaml` uses the pre-rename spelling; `--fix` rewrites each `role: primary` line to `role: owned`, preserving comments and key order |
| Dead op-lease | A `.rwv-op-lease` file whose recorded owner has no matching `.rwv-op` for the same op id — structurally impossible to belong to any in-flight operation. `--fix` removes the lease. |
| Cargo version skew | The same crate is required at different version-req strings across workspace members (post `workspace = true` indirection); warning-severity observatory, report-only |
| Cargo patch shadowing | A member's `.cargo/config.toml` declares a `[patch.<registry>].<crate>` key that shadows a weave-level entry for the same key (cargo's closest-config-wins per-key). Warning-severity; report-only |
| Missing canonical clone | A workweave worktree whose canonical clone (the primary-weave clone it was linked from) is no longer on disk. Repair: `rwv fetch` (in-place) to re-materialize |
| Uninitialized submodule | A repo checkout has a `.gitmodules` entry whose submodule path is absent or empty on disk (submodule init never ran there). Warning-severity; the finding message names the exact `git submodule update` invocation to run |
| Integration checks | Per-integration check hooks (tool availability, stale config) |

### `rwv workweave <project> create <name>`

Create a workweave: worktrees on ephemeral branches for each `owned`/`fork`/`dependency` repo, symlinks to the canonical weave-root clone for each `role: reference` repo, generated ecosystem files, per-workweave tool state.

| Flag | Effect |
|---|---|
| `--from <source>` | Fork from a specific source (default: CWD's active workspace). Accepts `primary`, an absolute or relative path, or omitted to fork from CWD's active workspace |
| `--force` | Destroy an existing workweave at this path before recreating. Without this flag, re-invoking `create` against an existing workweave is the idempotent path. Refuses if the existing workweave has uncommitted changes |
| `--capture-dirty` | Allow creation when the source project directory has uncommitted changes. The dirty state is captured into the new workweave's project worktree |
| `--worktree-references` | Cut a real `git worktree` for `role: reference` repos instead of the default symlink to the canonical weave-root clone. Restores the legacy behavior (per-workweave reference refs) at the cost of duplicating each reference repo's working tree into the workweave |
| `--dir <path>` | Per-invocation placement override. Places the workweave at exactly this path (recorded verbatim in the index). Absolute paths are used as-is; relative paths resolve against the primary root. Overrides the recorded container for this invocation only |

Workweaves live at `<container>/<project>--<name>/` where `<container>` is recorded per-project in `projects/<project>/.rwv-workweave-index` (machine-local JSON, one line in the project's `.gitignore`). The default container is `<parent-of-primary>/.workweaves`. Set the container explicitly with `rwv workweave <project> set-container <path>`; `create` records new entries into the index; every `find`-direction verb (list, delete, sync targets by bare name) resolves via the recorded `name → absolute path` entries with `.rwv-workweave` marker round-trip validation. Doctor reconciles the index against on-disk state — stale entries are pruned, orphan workweaves are adopted, a tracked index is flagged as a hygiene finding.

`RWV_WORKWEAVE_DIR` is deprecated: when set, `create` still seeds the initial container from it and fires a loud deprecation warning; use the `set-container` verb to record the container explicitly. Removal of the env-var fallback ships in a follow-up release.

### `rwv workweave <project> set-container <path>`

Record the workweave container for `project`. Writes the `container` field of `projects/<project>/.rwv-workweave-index`. Absolute paths are used as-is; relative paths resolve against the primary root. Existing registry entries are preserved. This is the replacement for `RWV_WORKWEAVE_DIR`: an explicit, recorded, audit-visible act, not ambient process state.

### `rwv workweave <project> delete <name> [--force]`

Delete a workweave. Default refuses if any worktree is dirty, or holds commits contained in neither the workweave's recorded parent nor the primary weave (work in a nested workweave counts as merged once its parent has it); `--force` bypasses both checks.

### `rwv workweave <project> list`

List workweaves for a project.

### `rwv workweave <project> log [--diff] [--json]`

Show this workweave's unique commits versus its recorded parent, per repo, including the project repo (`projects/<project>`). Must be run from inside a workweave.

| Flag | Effect |
|---|---|
| `--diff` | Show the unique diff versus the parent instead of the commit listing. Anchored at the common ancestor, so commits the parent gained after the fork are not shown as reversals |
| `--json` | Emit machine-readable JSON |

Text output includes one `=== <path> ===` section per manifest repo followed by `=== (project) ===` for the project repo. JSON output adds a `project_repo` field at the top level (same shape as each element of `repos[]`, `path` set to `"(project)"`).

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

Use case: agent harness asks "what flags does `rwv push` take, and what does it print?" — the bundle is authoritative. For `--json`-capable verbs (`status`, `doctor`, `fetch`, `update`, `sync`, `sync-to`, `push`), the JSON Schema is embedded as a fenced code block inside the bundle.

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
| `rwv update --json` | `repos` |
| `rwv sync --json` | `outcomes` |
| `rwv sync-to --json` | `outcomes` |
| `rwv push --json` | `outcomes` |

Schemas live at `docs/reference/schemas/<verb>.json` and are also embedded as fenced code blocks inside the corresponding `rwv explain <verb>` bundle.

### NDJSON under parallel mode

When a verb runs with `-j N` and `N > 1`, the output switches to NDJSON: one JSON record per line as workers finish, **no envelope wrapper**, and each line carries its own `"$schema"` field so a consumer can identify any single record without out-of-band context.

The branch-on-shape pattern lets consumers handle both modes uniformly: peek at stdout; if subsequent lines carry their own `"$schema"`, parse as NDJSON; otherwise parse as one envelope document.

Exit semantics under `--json` are the same in both modes: non-zero iff at least one per-repo outcome is `failed`.

Anchored by `tests/doc_claims_sync_test.rs`, `tests/doc_claims_fetch_test.rs`, `tests/doc_claims_update_test.rs`, `tests/doc_claims_push_test.rs`, and the per-verb `*_json_test.rs` families (`sync`, `sync_to`, `fetch`, `update`, `push`).

## Related

- [reference/formats](./formats.md) — `rwv.yaml`, `rwv.lock`, `.rwv-active`, `.rwv-workweave`
- [reference/roles](./roles.md) — role definitions and change-resistance semantics
- [reference/glossary](./glossary.md) — terminology lookup
- [reference/integrations](./integrations/index.md) — per-integration generated files and config
- [explanation/joints](../explanation/joints/) — conceptual material referenced by these verbs
