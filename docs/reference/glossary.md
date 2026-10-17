# Glossary

Terminology lookup. For deeper material on each concept, follow the cross-links.

| Term | Meaning |
|---|---|
| **Weave** | A repoweave workspace: any directory containing `projects/` (and typically cloned member repos, `.rwv-active`, and symlinked ecosystem workspace files). |
| **Weave root** | The top-level directory of a weave (or workweave), where `.rwv-active` (or `.rwv-workweave`), `projects/`, `<registry>/<owner>/<repo>/`, and surfaced ecosystem symlinks live. Earlier docs sometimes called this the *weave directory* or *workspace root*. |
| **Primary weave** | The main weave at the weave root (as opposed to a workweave under `.workweaves/`). Target of `rwv sync primary`. |
| **Workweave** | A worktree-based derivative of a weave, created on demand for isolation (agents, features, PR review). Lives at `<parent>/.workweaves/<project>--<name>/` by default. Created, duplicated, and destroyed with `rwv workweave <project> create` (`--from <source>` forks from an existing workweave) and `rwv workweave <project> delete`. |
| **Project** | A directory under `projects/` containing `rwv.toml`, `rwv.lock`, and project-scoped docs. Itself a git repo with normal history. |
| **Project repo** | The git repo at `projects/<name>/`. Carries the manifest, lock, and cross-cutting docs. Does not contain importable code. |
| **Manifest repo** / **Member repo** | A repo listed in a project's `rwv.toml`. Lives at `<registry>/<owner>/<repo>/` as a regular clone in the primary weave and as a git worktree in each workweave. Both terms are used interchangeably across the docs (`manifest repo` emphasizes its `rwv.toml` declaration; `member repo` emphasizes its membership in the workspace). |
| **Canonical clone** (or **Canonical store**) | The full git clone at `<primary>/<registry>/<owner>/<repo>/` (or `<primary>/projects/<name>/`) whose `.git` object store backs both the primary weave checkout and every workweave's git worktree. See [clone-topology](../explanation/joints/clone-topology.md). |
| **Ephemeral branch** | The flat per-workweave git branch (`<project>--<workweave>`, with `/` in scoped project names encoded as `+`) created in each member and project repo when a workweave is created, and deleted when the workweave is deleted or retired. See [workweave-hierarchy](../explanation/joints/workweave-hierarchy.md). |
| **Manifest** (`rwv.toml`) | Declares which repos belong to a project, their roles, and integration config. See [formats](./formats.md). |
| **Lock file** (`rwv.lock`) | Pins repos to exact 40-character commit SHAs for reproducibility. Derived state — output of `rwv lock`, `rwv update`, and `rwv sync` Phase 3. See [lock-as-derived](../explanation/joints/lock-as-derived.md). |
| **Stale lock** | Freshness condition: the lock covers every manifest repo but pins a revision behind current HEAD. Doctor finding kind `stale-lock`; bypassed with `--allow-stale-lock`. Fixed by `rwv lock`. Contrast **Incomplete lock**. |
| **Incomplete lock** | Coverage condition: the lock has no entry for some manifest repo. Doctor finding kind `incomplete-lock`; also what `rwv fetch --frozen` refuses on. Fixed by `rwv lock` (adds the missing entry). Contrast **Stale lock**. |
| **Activation** | Selecting the active project (writing `.rwv-active`), verifying integration files in `projects/<name>/`, surfacing weave-root symlinks, and running integration install hooks (`--no-materialize` skips hooks). Performed by `rwv activate <project>`; it does not author or rewrite ecosystem workspace files. Inside a workweave, where `activate` is refused, `rwv materialize` runs hooks and `rwv doctor --fix` re-surfaces missing symlinks. |
| **Materialization** | Running integration install hooks (`cargo fetch`, `npm install`, `go work sync`, ...), cleaning up artifacts from disabled integrations, and settling drifted attested generated files (`--regenerate-drifted`, `--adopt-drifted`). Performed by `rwv materialize` (and as the final step of `rwv activate`, `rwv fetch`, and `rwv sync`). |
| **Active project** | The project named in `.rwv-active`. Single source of truth in the primary weave — no CWD override. |
| **Intent verb** / **Context verb** | The two classes of verbs in the [file-ownership](../explanation/joints/file-ownership.md) trigger model. **Intent verbs** (`init`, `add`, `remove`, `update`, `role`, plus `doctor --fix`) change project composition and author/merge ecosystem workspace files in `projects/<name>/`. **Context verbs** (`activate`, `materialize`, `fetch`, `sync`, `workweave create`) surface symlinks and run install hooks without rewriting declared workspace membership. |
| **Hybrid file** | An ecosystem config file in `projects/<name>/` (such as `Cargo.toml`, `package.json`, `pyproject.toml`, `.gitignore`) where `rwv` owns specific keys or sections (`[workspace]`, `workspaces`) while the user owns the rest of the document. Contrasts with **Pure** (wholly rwv-owned, e.g. `go.work`) and **Generated** (written by external tools during install hooks and tracked in `.rwv-owned-digests`, e.g. `Cargo.lock`). See [file-ownership](../explanation/joints/file-ownership.md). |
| **Role** | A repo's relationship to a project: `owned`, `fork`, `dependency`, `reference`. Encodes change resistance. See [roles](./roles.md). |
| **Integration** | A built-in unit, shipped with rwv, that translates between repoweave's multi-repo model and one ecosystem's workspace format. Bound by a file-ownership contract (surfacing + content ownership). See [integrations](./integrations/index.md) and [file-ownership](../explanation/joints/file-ownership.md). |
| **Plugin** | An external `rwv-<verb>` executable on `$PATH`, dispatched when a verb isn't built into rwv itself. Must not write rwv-owned files; violations are flagged by `rwv doctor`. Contrast with **Integration**. See [plugin-protocol](./plugin-protocol.md) and [plugin-boundary](../explanation/joints/plugin-boundary.md). |
| **Registry** | The first segment of a repo's canonical path; a short name for where the repo lives. Built-in: `github`, `gitlab`, `bitbucket`, plus `local`, reachable only as an `rwv add --new` creation address rather than by matching a URL. See [formats — Directory layout](./formats.md#directory-layout). |
| **Pyramid of stability** | Cross-repo canonical-tip concept: a project's branches each carry their own lock, defining "stable" / "rc" / "main" channels for the whole system. See [pyramid-of-stability](../explanation/joints/pyramid-of-stability.md). |
| **Canonical tip** | The vetted cross-repo state of a project on a given branch (encoded by that branch's `rwv.lock`). |
| **Lens** | A motivational way of looking at rwv targeted at one audience. Three lenses: [workspace](../explanation/lenses/workspace.md), [monorepo](../explanation/lenses/monorepo.md), [agent](../explanation/lenses/agent.md). |
| **Joint** | A cross-cutting concept that all lenses reference (Diátaxis-explanation, audience-neutral). See [explanation/index.md](../explanation/index.md). |
| **Phase 1'** | First runtime phase of `rwv sync`: replay CWD's unique project commits onto source's project tip, with `rwv.lock` excluded. Its manifest decides which member checkouts replay adds and prunes. |
| **Phase 2** | Second runtime phase of `rwv sync`: advance manifest repos to the source's lock targets. |
| **Phase 3** | Third runtime phase of `rwv sync`: regenerate `rwv.lock` from post-Phase-2 manifest tips. |
| **Strategy** | Sync strategy (`ff` or `rebase`), applied uniformly across project and manifest repos. Defaults to `ff` on `rwv sync` (absorbing state into CWD) and `rebase` on `rwv sync-to` (landing a workweave into its parent). (`merge` is not offered — see [sync semantics](../explanation/joints/sync-semantics.md#why-no-merge-strategy).) |
| **Retire** | `rwv sync-to --retire` — post-sync-to cleanup that deletes the workweave on success. The `--retire` flag lives on `rwv sync-to` (the landing direction), not `rwv sync`. |
| **Parent (of a workweave)** | The workspace the workweave was forked from. Recorded in `.rwv-workweave`'s `parent` field. Bare `rwv sync-to` (no target) auto-targets the parent. |
| **sync-to** | `rwv sync-to [<target>]` — the landing verb. Advances a named target to CWD's tip via a three-step orchestration: (1) rebase CWD against target, (2) auto-relock CWD, (3) FF-advance target. Bare invocation inside a workweave auto-targets the recorded parent. Contrast with `rwv sync <source>`, which absorbs state into CWD. See [sync semantics](../explanation/joints/sync-semantics.md). |
| **Selector** | The shared `--role` / `--repo` flag surface on `fetch`, `update`, `push`. See [reference/cli — Selector grammar](./cli.md#selector-grammar). |
| **Savepoint** | A git ref under `refs/rwv/pre-op/<op-id>/` snapshotting a repo's pre-op tip. Used by `rwv abort` to roll back. |
| **Op-state** | The on-disk record of an in-flight `rwv sync` or `rwv sync-to` operation. Comprises an owner record (`.rwv-op`) at the initiating workspace and zero or more thin leases (`.rwv-op-lease`) at other mutated workspaces. Preserved on phase failure; cleared on success or after `rwv abort`. See [formats](./formats.md#rwv-op--owner-op-state-record). |
| **Owner record** | The full op-state record (`.rwv-op`) written at the initiating workspace. Holds all op parameters, the current phase, converged tips, and named overrides. The sole copy of mutable op state — leases at other workspaces point back to it. See [formats](./formats.md#rwv-op--owner-op-state-record). |
| **Owner workspace** | The workspace that holds the owner record for a given op — the workspace from which `rwv sync` or `rwv sync-to` was invoked. `rwv abort` invoked from a non-owner workspace follows the lease pointer to find the owner workspace. |
| **Lease** | A thin immutable file (`.rwv-op-lease`) written at every workspace an op mutates other than the owner. Provides mutex semantics (prevents concurrent ops) and a pointer to the owner workspace. See [formats](./formats.md#rwv-op-lease--thin-lease-pointer). |
| **`--json` envelope** | The shape `{"$schema": "...", "<key>": [...]}` emitted by JSON-capable verbs. Key is verb-specific. See [reference/cli — JSON envelope convention](./cli.md#--json-envelope-convention). |
| **NDJSON mode** | Streaming output mode when a verb runs with `-j N > 1` and `--json`: one record per line, each carrying its own `$schema`, no envelope. |
| **Drift (index / working-tree)** | A repo's index or on-disk files don't match its HEAD tree — e.g. when a branch ref checked out in a workspace is moved externally without updating that workspace's index and working tree. See [shared-refs-drift](../explanation/joints/shared-refs-drift.md). |
| **Replay exclusion** | The mechanism that drops `rwv.lock` from Phase 1' commit diffs. Implemented per-VCS via `Vcs::set_replay_exclusion`. See [vcs-as-seam](../explanation/joints/vcs-as-seam.md). |

## Related

- [reference/cli](./cli.md) — verb-by-verb reference
- [reference/formats](./formats.md) — file shapes
- [reference/roles](./roles.md) — role definitions
- [explanation/index.md](../explanation/index.md) — lenses and cross-cutting joints
- [explanation/lenses/workspace.md](../explanation/lenses/workspace.md) — motivational entry points
