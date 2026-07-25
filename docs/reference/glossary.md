# Glossary

Terminology lookup. For deeper material on each concept, follow the cross-links.

| Term | Meaning |
|---|---|
| **Weave** | A repoweave workspace — a directory containing repos, project directories, and ecosystem wiring generated from the active project. |
| **Primary weave** | The "main" weave at the workspace root (as opposed to a workweave under `.workweaves/`). Target of `rwv sync primary`. |
| **Workweave** | A worktree-based derivative of a weave, created on demand for isolation (agents, features, PR review). Lives at `<parent>/.workweaves/<project>--<name>/`. Created, duplicated, and destroyed with `rwv workweave <project> create` (`--from <source>` forks from an existing workweave) and `rwv workweave <project> delete` — not by copying or removing the directory. |
| **Project** | A directory under `projects/` containing `rwv.yaml`, `rwv.lock`, and project-scoped docs. Itself a git repo with normal history. |
| **Project repo** | The git repo at `projects/<name>/`. Carries the manifest, lock, and cross-cutting docs. Does not contain importable code. |
| **Manifest repo** | A repo listed in a project's `rwv.yaml`. Lives at `<registry>/<owner>/<repo>/` as a regular clone. The work surfaces. |
| **Manifest** (`rwv.yaml`) | Declares which repos belong to a project, their roles, and integration config. See [formats](./formats.md). |
| **Lock file** (`rwv.lock`) | Pins repos to exact revisions for reproducibility. Derived state — output of `rwv lock`. See [lock-as-derived](../explanation/joints/lock-as-derived.md). |
| **Stale lock** | Freshness condition: the lock covers every manifest repo but pins a revision behind current HEAD. Doctor finding kind `stale-lock`; bypassed with `--allow-stale-lock`. Fixed by `rwv lock`. Contrast **Incomplete lock**. |
| **Incomplete lock** | Coverage condition: the lock has no entry for some manifest repo. Doctor finding kind `incomplete-lock`; also what `rwv fetch --frozen` refuses on. Fixed by `rwv lock` (adds the missing entry). Contrast **Stale lock**. |
| **Activation** | Generating ecosystem workspace files from a project's manifest and symlinking them to the weave directory. What `rwv activate <project>` does; it is the verb that writes `.rwv-active`. Inside a workweave, where `activate` is refused, `rwv doctor --fix` re-surfaces missing symlinks. |
| **Active project** | The project named in `.rwv-active`. Single source of truth — no CWD override. |
| **Role** | A repo's relationship to a project: `owned`, `fork`, `dependency`, `reference`. Encodes change resistance. See [roles](./roles.md). |
| **Integration** | A built-in unit, shipped with rwv, that translates between repoweave's multi-repo model and one ecosystem's workspace format. Bound by a file-ownership contract (surfacing + content ownership). See [integrations](./integrations/index.md) and [file-ownership](../explanation/joints/file-ownership.md). |
| **Plugin** | An external `rwv-<verb>` executable on `$PATH`, dispatched when a verb isn't built into rwv itself — the low-friction, self-serve extension point for adding a verb without a PR against core. Must not write rwv-owned files; unlike an integration's enforced file-ownership contract, this is a documented rule, not a dispatch-time guard — `rwv doctor` can only flag a violation after the fact. Contrast with **Integration**. See [plugin-protocol](./plugin-protocol.md) for the contract, [plugin-boundary](../explanation/joints/plugin-boundary.md) for the rationale. |
| **Registry** | The first segment of a repo's canonical path; a short name for where the repo lives. Built-in: `github`, `gitlab`, `bitbucket`. |
| **Pyramid of stability** | Cross-repo canonical-tip concept: a project's branches each carry their own lock, defining "stable" / "rc" / "main" channels for the whole system. See [joint](../explanation/joints/pyramid-of-stability.md). |
| **Canonical tip** | The vetted cross-repo state of a project on a given branch (encoded by that branch's `rwv.lock`). |
| **Lens** | A motivational way of looking at rwv targeted at one audience. Three lenses: [workspace](../explanation/lenses/workspace.md), [monorepo](../explanation/lenses/monorepo.md), [agent](../explanation/lenses/agent.md). |
| **Joint** | A cross-cutting concept that all lenses reference (Diátaxis-explanation, audience-neutral). See [joints/](../explanation/joints/). |
| **Phase 2** | First runtime phase of `rwv sync`: advance manifest repos to the source's lock targets. |
| **Phase 1'** | Second runtime phase of `rwv sync`: replay CWD's unique project commits onto source's project tip, with `rwv.lock` excluded. |
| **Phase 3** | Third runtime phase of `rwv sync`: regenerate `rwv.lock` from post-Phase-2 manifest tips. |
| **Strategy** | Sync strategy: `ff` (fast-forward, default) or `rebase`. Applies uniformly to project and manifest repos. (`merge` is not offered — see [sync semantics](../explanation/joints/sync-semantics.md#why-no-merge-strategy).) |
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
| **Drift (index / working-tree)** | A repo's index or on-disk files don't match its HEAD tree — usually a shared-refs side effect from sibling worktrees. See [shared-refs-drift](../explanation/joints/shared-refs-drift.md). |
| **Replay exclusion** | The mechanism that drops `rwv.lock` from Phase 1' commit diffs. Implemented per-VCS via `Vcs::set_replay_exclusion`. See [vcs-as-seam](../explanation/joints/vcs-as-seam.md). |

## Related

- [reference/cli](./cli.md) — verb-by-verb reference
- [reference/formats](./formats.md) — file shapes
- [reference/roles](./roles.md) — role definitions
- [explanation/joints](../explanation/joints/) — conceptual material
- [explanation/lenses](../explanation/lenses/) — motivational entry points
