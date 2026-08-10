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
`rwv` emits a corrective error pointing at `-w/--workweave`. To address a
workweave by path, pass the full path to the workweave directory.

### `-w <project>--<name>` / `--workweave <project>--<name>` — name addressing

The agent-native addressing form. A workweave's identity is the
`<project>--<name>` string that appears in every path, branch name, and
tooling artifact an agent touches. The CLI already addresses workweaves by
name inside the `workweave` verb family; this flag makes that addressing
global:

```
rwv -w myproj--hotfix sync-to --retire   # from anywhere inside the ecosystem
rwv -w myproj--hotfix status
```

- **Container-location-independent.** The name survives placement changes
  (`--dir` overrides, container migration) that would break a path-based
  address. The registry records the actual path; `-w` resolves through it.
- **Composes with `-C`.** `-C` establishes the workspace (locates the
  primary); `-w` selects the checkout within it. Use `-C` when your process
  is outside the ecosystem entirely.
- **Full form only.** The argument must be `<project>--<name>`. Both
  components must be non-empty; no path separators. The split follows the
  first `--` (consistent with the directory-name convention).
- **Repetition is an error.** Passing `-w` twice is rejected for the same
  reason as `-C`: two addresses is a confused invocation.
- **Path-shaped argument.** If the argument contains a path separator or
  exists on disk as a path, `rwv` emits a corrective error pointing at `-C`.

Resolution: find the workspace root (from `-C` or cwd walk), then look up
`<name>` in the registry for `<project>` with `.rwv-workweave` marker
round-trip validation. A stale or unregistered name produces an actionable
error listing the project's known workweaves.

## Verbs

### `rwv` (bare)

Show current context: weave directory, active project, workweave (if any), repos.

### `rwv fetch [<source>] [...]`

Read `rwv.lock` and align clones to it. Bootstrap when lock is absent.

Two modes, keyed on whether `<source>` is given:

- **With `<source>`** — a URL (`https://…`, `git@…`, `owner/repo`, or the project name for a `--provider`-configured registry): the *bootstrap* mode. Clones the project repo from `<source>` into the current directory, reads its committed `rwv.lock`, and clones every listed manifest repo to its canonical slot.
- **No `<source>`** — the *in-place repair* mode: re-materialize missing manifest members in the current workspace. Uses the existing `rwv.toml` and `rwv.lock`; clones any repo whose canonical directory is absent (the `MissingCanonicalClone` / `DanglingReference` findings from `rwv doctor` point here). Run from the workspace root.

An already-present clone is **realigned, not skipped**: when the lock covers the repo, `fetch` resolves the locked revision in that clone's own object store and moves the checkout onto it *without changing what HEAD is attached to* — fast-forwarding the local counterpart of the branch `version:` declares, and leaving the checkout on it. No network fetch happens for a present clone, so a locked revision missing from the local object store is an error, not a re-fetch. When the lock has no entry for the repo, or there is no lock at all, the clone is left as it is and the lock records its current HEAD. A clone that is materialized by this run is *born* attached at the lock revision, not at the remote tip.

Realignment refuses when it cannot do that: when the pin is not a fast-forward of the branch (an older lock, or a branch carrying commits `origin` has not seen), and when the checkout is on a branch the manifest does not declare. `--detach-checkouts` waives both by materializing the pin on a detached HEAD — it discards nothing and moves no branch. Because `rwv sync-to` refuses to land onto a detached target, `git checkout <branch>` in the member is what puts it back on its branch.

| Flag | Effect |
|---|---|
| `--frozen` | Error if lock is incomplete; never advance. Suitable for CI |
| `--allow-non-empty-dir` | Bootstrap into a non-empty directory that is not a workspace |
| `--no-reference` | Skip cloning/fetching repositories with `role: reference` |
| `--detach-checkouts` | Realign a present clone even where that changes what HEAD is attached to: materialize the pin on a detached HEAD instead of refusing |
| `--role` / `--repo` | Selector filters (see [Selector grammar](#selector-grammar)). A filtered run skips the lock write |
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

The advance is a **fast-forward of the branch the checkout is on**, not a checkout of the tip: on the canonical, that branch must be the local counterpart of the one `version:` declares, and `update` refuses when the checkout is on any other branch. It also refuses when the tip is not a fast-forward — reconcile the branch with its tracking tip yourself (ordinary `git rebase` / `git merge`) and re-run, or pass `--detach-checkouts` to materialize the tip on a detached HEAD without moving your branch. Inside a workweave the same fast-forward rule applies to the workweave's own branch, and a divergence points at `rwv sync` rather than at a flag. An already-detached member stays detached, unless the repo is stopped mid-rebase / mid-merge / mid-bisect, which refuses.

`advanced N repo(s)` counts repos whose SHA actually changed.

| Flag | Effect |
|---|---|
| `--detach-checkouts` | Advance a repo even where that changes what HEAD is attached to: materialize the tip on a detached HEAD instead of refusing |
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

Set the active project. Updates `.rwv-active`, regenerates ecosystem workspace files in the project directory, symlinks them to the weave directory, then runs integration install hooks (`npm install`, `uv sync`, `cargo fetch`, etc.).

| Flag | Effect |
|---|---|
| `--no-install` | Skip integration install hooks for a fast context-switch |

`.rwv-active` is the single source of truth for the active project; CWD does not override.

Anchored by `tests/doc_claims_activate_test.rs`.

### `rwv init <name-or-source> [--provider <registry>/<owner>] [--adopt]`

Create a new project repo at `projects/<name>/` with an empty `rwv.toml`. With `--provider`, configures the project repo's remote URL.

When invoked in an **empty directory**, `init` bootstraps that directory as a workspace root (no pre-existing `rwv.toml` required) and creates the project inside it. Running in a non-empty directory without an existing workspace refuses.

With `--adopt`, `<name-or-source>` is a URL or `owner/repo` shorthand: `init` clones the project repo from that source instead of `git init`-ing a new one (brownfield adoption of an existing project repo).

### `rwv add <url> [...]`

Clone a repo (if not present), register it in the *active workspace*'s `rwv.toml`, run integration hooks.

| Flag | Effect |
|---|---|
| `--role <role>` | Sets the role (`owned` / `fork` / `dependency` / `reference`). Defaults to `owned`. |
| `--new` | Init a new local repo at canonical path; infer URL from path convention |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

`rwv add` writes to CWD's workspace's manifest (the active workspace's `rwv.toml`), not always primary's.

**Canonical path.** Every URL lands at `<registry>/<owner>/<repo>/`. `<registry>` is the matched built-in registry's name (`github`, `gitlab`, `bitbucket`) when the host is one of those; for a host none of them recognise, it's the URL's own host (e.g. `git.corp.example/team/repo/`); for `file://`, which has no host, it's `local`.

**Shared-clone warning.** If the target clone directory is already registered by another project in the same weave, `rwv add` proceeds (the manifest entry is added to the active project as usual) and emits a warning to stderr naming the other project(s). Sharing a clone across projects is legal — the same repo can be a `dependency` in one project and `owned` in another — but is worth flagging so accidental double-registration is visible.

### `rwv remove <path> [...]`

Remove from `rwv.toml`, re-run activation (regenerates ecosystem workspace files).

| Flag | Effect |
|---|---|
| `--delete` | Also remove the clone (errors if another project references it) |
| `--delete-shared-clone` | With `--delete`, remove the clone even if other projects still reference it |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

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
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

`--json` emits the envelope `{"$schema": "...", "outcomes": [...]}`. Manifest-repo records use `kind` `pushed`, `skipped`, or `failed`; the project-repo record (always last) uses `kind` `project-repo-pushed` or `project-repo-failed`. See [JSON envelope convention](#--json-envelope-convention).

Anchored by `tests/doc_claims_push_test.rs` and `tests/push_json_test.rs`. See [push a cross-repo feature](../how-to/push-cross-repo-feature.md).

### `rwv abort`

Restore CWD's workspace to its pre-sync state using savepoint refs at `refs/rwv/pre-op/<op-id>`. Runs VCS-native abort for in-progress operations (`git rebase --abort`, `git merge --abort`).

Errors if no sync operation is in progress.

### `rwv status [--json] [...]`

Show per-repo state for the CWD workspace.

| Flag | Effect |
|---|---|
| `--json` | Output as JSON (see envelope below) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

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
| `--fix` | Repair every finding marked **Auto-fixable** in [Doctor findings](./doctor-findings.md), which carries that mark on each finding it documents. Never touches live staged content or live edits. Idempotent. |
| `--json` | Emits envelope `{"$schema": "...", "violations": [...]}` |
| `--all` | Scan all projects and run weave-wide checks (orphan detection, cross-project stale locks, etc.). By default only the active project is checked |
| `--reattach-checkouts` | With `--fix`, reattach a canonical store's detached HEAD to its tracking counterpart when that counterpart exists and its tip equals HEAD. Without this flag, `--fix` only reports a detached canonical, naming the `git switch` that would reattach it |
| `--adopt-detached-checkouts` | With `--fix`, let the branch-model migration mint a workweave's ephemeral branch at a detached checkout's HEAD (the lock SHA), giving up a pre-flat branch holding that name if one exists (warns if doing so strands commits HEAD does not carry). Without this flag, `--fix` reports both tips and leaves the checkout alone |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

| Check | Description |
|---|---|
| Orphaned clones | Directories under registry paths not listed in any project's `rwv.toml` |
| Dangling references | Entries in an `rwv.toml` pointing to paths not on disk |
| Missing role | `rwv.toml` entries without a `role` field |
| Stale lock | Project's `rwv.lock` doesn't match current HEAD revisions |
| Workweave drift | Worktrees missing from a workweave or extra worktrees not in manifest |
| Index drift | A repo's index doesn't match HEAD tree (shared-refs side effect) |
| Working-tree drift | A repo's on-disk files don't match HEAD tree (shared-refs side effect) |
| Missing replay-exclusion | A project repo's `.gitattributes` lacks `rwv.lock merge=rwv-ours` or still carries the legacy `merge=ours` spelling (`--fix` adds/migrates the line and, on migration, commits it) |
| Legacy `role: primary` | A project `rwv.toml` uses the pre-rename spelling; `--fix` rewrites each `role: primary` line to `role: owned`, preserving comments and key order |
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
| `--from <source>` | Fork from a specific source (default: CWD's active workspace). Accepts `primary`, an absolute or relative path, or omitted to fork from CWD's active workspace. Forking from an existing workweave is how you **duplicate** one; copying a workweave with `cp` aliases the original's git state rather than duplicating it |
| `--replace-existing` | Destroy an existing workweave at this path before recreating. Without this flag, re-invoking `create` against an existing workweave is the idempotent path. Refuses if the existing workweave has uncommitted or unmerged work |
| `--capture-dirty` | Allow creation when the source project directory has uncommitted changes. The dirty state is captured into the new workweave's project worktree |
| `--worktree-references` | Cut a real `git worktree` for `role: reference` repos instead of the default symlink to the canonical weave-root clone. Restores the legacy behavior (per-workweave reference refs) at the cost of duplicating each reference repo's working tree into the workweave |
| `--dir <path>` | Per-invocation placement override. Places the workweave at exactly this path (recorded verbatim in the index). Absolute paths are used as-is; relative paths resolve against the primary root. Overrides the recorded container for this invocation only |

Workweaves live at `<container>/<project>--<name>/` where `<container>` is recorded per-project in `projects/<project>/.rwv-workweave-index` (machine-local JSON, one line in the project's `.gitignore`). The default container is `<parent-of-primary>/.workweaves`. Set the container explicitly with `rwv workweave <project> set-container <path>`; `create` records new entries into the index; every `find`-direction verb (list, delete, sync targets by bare name) resolves via the recorded `name → absolute path` entries with `.rwv-workweave` marker round-trip validation. Doctor reconciles the index against on-disk state — stale entries are pruned, orphan workweaves are adopted, a tracked index is flagged as a hygiene finding.

### `rwv workweave <project> set-container <path>`

Record the workweave container for `project`. Writes the `container` field of `projects/<project>/.rwv-workweave-index`. Absolute paths are used as-is; relative paths resolve against the primary root. Existing registry entries are preserved. An explicit, recorded, audit-visible act, not ambient process state.

### `rwv workweave <project> delete <name> [--discard-uncommitted] [--discard-unmerged-commits]`

Delete a workweave. Default refuses if any worktree is dirty, or holds commits contained in neither the workweave's recorded parent nor the primary weave (work in a nested workweave counts as merged once its parent has it). `--discard-uncommitted` waives the first refusal, `--discard-unmerged-commits` the second.

### `rwv workweave <project> list`

List workweaves for a project.

### `rwv workweave <project> log [--diff] [--json]`

Show this workweave's unique commits versus its recorded parent, per repo, including the project repo (`projects/<project>`). Must be run from inside a workweave.

| Flag | Effect |
|---|---|
| `--diff` | Show the unique diff versus the parent instead of the commit listing. Anchored at the common ancestor, so commits the parent gained after the fork are not shown as reversals |
| `--json` | Emit machine-readable JSON |

Text output includes one `=== <path> ===` section per manifest repo followed by `=== (project) ===` for the project repo. JSON output adds a `project_repo` field at the top level (same shape as each element of `repos[]`, `path` set to `"(project)"`).

### `rwv workweave [--hook-mode] [--claude-hook] <project> <action>`

Two flags on `workweave` itself, preceding `<project>` — not part of any subaction — for driving workweave creation and teardown from Claude Code's own hook events instead of the shell.

| Flag | Effect |
|---|---|
| `--hook-mode` | With `create`, print only the new workweave's path to stdout instead of the usual create output. Registered by `rwv setup claude` as the Claude Code `WorktreeCreate` hook command |
| `--claude-hook` | Read a Claude Code hook payload as JSON from stdin and dispatch on it directly, bypassing `<project>` and `<action>` entirely. `hook_event_name: "WorktreeCreate"` creates a workweave — project inferred from the hook's `cwd` (the current workweave's project, or the primary weave's active project), name derived from `branch_name` (falling back to a timestamp) — and prints its path to stdout. `"WorktreeRemove"` deletes the workweave named by `worktree_path`; fire-and-forget, warnings go to stderr and it always exits `0`. `<project>` is not required when this flag is set. Conflicts with `--hook-mode` |

`rwv setup claude` registers `rwv workweave --claude-hook` for both the `WorktreeCreate` and `WorktreeRemove` Claude Code hook events.

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

## Project resolution

Project-scoped verbs (`add`, `remove`, `lock`, `update`, `push`, `sync`, `sync-to`, `status`, `doctor`, `fetch` in-place) pick their target project via a fixed chain, highest priority first:

1. `--project <name>` — the explicit override on the invocation.
2. `-w/--workweave <project>--<name>` — the `<project>--` prefix of the global workweave-selector flag names the project.
3. The weave root's own identity file — **one tier**, whose spelling follows the kind of root CWD resolved into:
   - `.rwv-workweave` marker, when CWD resolves inside a workweave. The marker names the project structurally.
   - `.rwv-active` pointer at a primary root — the file `rwv activate` maintains.

Step 3 is one tier rather than two ranked ones because the two files are **mutually exclusive**: a primary root carries the pointer, a workweave root carries the marker, never both. No invocation ever sees both answers, so there is no precedence between them to get wrong. `rwv doctor` enforces the exclusivity, reporting a root carrying both as a `weave-root-identity-conflict`.

The pointer is total at primary by construction: every path that creates a project also activates it (`rwv init` and `rwv fetch <source>` each write `.rwv-active` as part of their normal execution). A missing or stale pointer therefore only arises from hand surgery — and produces a corrective error, not silent structure inference. `rwv workweave <project> create <name>` writes only the marker: a workweave's project is fixed at creation and cannot be switched, so there is no selection for a pointer to record.

### Target line — visibility when the pointer decides

When resolution falls through to step 3, the verb prints a target line to **stderr** before acting:

```
target: workspace /home/cwa/weaveroot/foundations · project tmuxcc (.rwv-active)
```

Explicitly (`--project` or `-w`) or structurally (workweave marker) resolved invocations stay silent — the operator already named the target, or the workweave did.

Stderr, not stdout, so the line never contaminates a `--json` verb's output. The line is prose, not a parse surface — no schema, no version, no consumer contract. The `resolution` block in `--json` output *(later change)* carries the resolved coordinate as machine data; provenance stays out of the JSON deliberately so agents assert on results, not on how they were reached.

### Corrective errors

- **No active project** — CWD is at a workspace primary with neither `--project` nor `.rwv-active` set. The error names the fix commands and, if projects exist under `projects/`, lists them as a menu:

  ```
  Error: no active project; run `rwv activate <name>` or pass `--project <name>`. Existing projects: foundations, tmuxcc
  ```

  In a workspace with no projects yet, the error suggests `rwv init` instead.

- **Stale pointer** — `.rwv-active` names a project whose `projects/<name>/` directory does not exist:

  ```
  Error: active project `ghost` is set in `.rwv-active` but `projects/ghost/` does not exist; run `rwv activate <existing-project>` or remove `.rwv-active`.
  ```

  `rwv doctor` also reports the stale pointer as a `dangling-active-project` finding and, under `--fix`, clears the file.

### Which verbs are project-scoped

| Category | Verbs | Resolution behaviour |
|---|---|---|
| Project-scoped | `add`, `remove`, `lock`, `update`, `push`, `sync`, `sync-to`, `status`, `doctor` (project-scoped checks), `fetch` (in-place) | Full chain; target line when the pointer decides |
| Project named positionally | `activate <project>`, `workweave <project> …` | Explicit already; no chain consulted |
| Workspace-scoped, no project | `init`, `abort`, `resolve`, `prime`, `explain`, `doctor` (cross-project scan) | No project selection involved |

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

## External commands

`rwv <verb>` where `<verb>` is not a core verb resolves to a `rwv-<verb>` executable on `$PATH` and execs it. This is the same convention `git` and `cargo` use for their external commands. Core verbs always win: clap matches them before external fallthrough runs, so a `rwv-status` on `$PATH` can never shadow the builtin, and naming a plugin after a future core verb makes it unreachable once that verb ships.

`rwv` projects the resolved workspace context into an environment envelope on every spawn — `RWV_VERSION`, `RWV_WORKSPACE`, `RWV_WORKWEAVE`, `RWV_PROJECT` — and propagates the child's exit status back verbatim. For the full contract — the envelope table (value and unset-condition for each variable), addressing back into `rwv`, exit-code semantics, discovery and naming, the write prohibition, and the `--json` compatibility guarantee — see the [plugin-protocol](./plugin-protocol.md) reference.

## Related

- [reference/plugin-protocol](./plugin-protocol.md) — the external-command wire contract in full
- [reference/formats](./formats.md) — `rwv.toml`, `rwv.lock`, `.rwv-active`, `.rwv-workweave`
- [reference/roles](./roles.md) — role definitions and change-resistance semantics
- [reference/glossary](./glossary.md) — terminology lookup
- [reference/integrations](./integrations/index.md) — per-integration generated files and config
- [explanation/joints](../explanation/joints/) — conceptual material referenced by these verbs
