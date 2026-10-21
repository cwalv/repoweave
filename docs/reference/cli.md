# CLI reference

Lookup-shaped reference for every `rwv` verb. For conceptual material see the [explanation overview](../explanation/index.md); for task-shaped recipes see the [how-to guides](../how-to/index.md).

## Global options

These flags precede the subcommand and apply to all verbs.

### `-C <path>` / `--cwd <path>` — path addressing

Resolve the workspace as if `rwv` were invoked from `<path>`. Any path
*inside* a checkout works; the normal containment walk (marker, root, `$HOME`
ceiling) runs from there. Relative path arguments elsewhere on the command line
resolve against this directory, and an [external command](#external-commands)
starts in it.

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

Address a registered workweave by its `<project>--<name>` identifier from
anywhere in the workspace:

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
  components must be non-empty; no path separators. The address is never
  parsed apart: each recorded (project, name) pair is rendered and matched
  against the argument, so the address means whatever the registry records.
- **Repetition is an error.** Passing `-w` twice is rejected for the same
  reason as `-C`: two addresses is a confused invocation.
- **Path-shaped argument.** If the argument contains a path separator or
  exists on disk as a path, `rwv` emits a corrective error pointing at `-C`.

Resolution: find the weave root (from `-C` or cwd walk), then match the
address against every recorded workweave, validating the match by
`.rwv-workweave` marker round-trip. An unknown address produces an
actionable error listing the recorded workweaves across the weave. Two
further refusals are possible: an address that more than one recorded pair
renders to is refused as ambiguous, and an address whose registry entry
fails the marker round-trip is refused with the mismatch named —
`rwv doctor --fix` repairs the registry side.

## Verbs

### `rwv` (bare)

Show current context: weave root, active project, workweave (if any), repos.

### `rwv fetch [<source>] [...]`

Read `rwv.lock` and align clones to it. Bootstrap when lock is absent.

Two modes, keyed on whether `<source>` is given:

- **With `<source>`** — a URL (`https://…`, `git@…`, `file://` for a local remote) or `owner/repo` shorthand: the *bootstrap* mode. Clones the project repo from `<source>` into the current directory, reads its committed `rwv.lock`, clones every listed member repo to its canonical slot, and activates the project when none is active (`.rwv-active`, weave-root links, install hooks), `--frozen` and filtered runs included. A plain path refuses with `unresolvable-repo-source`, naming the `file://` spelling.
- **No `<source>`** — the *in-place repair* mode: re-materialize missing manifest members in the current workspace. Uses the existing `rwv.toml` and `rwv.lock`; clones any repo whose canonical directory is absent (the `missing-canonical-clone` and `dangling-reference` findings from `rwv doctor` point here). Run from the weave root. With no project active and exactly one project in the weave, it fetches that project and then activates it, printing `rwv fetch: activating project '<name>', the only project in this weave (none was active)`; with several, it refuses with `no-active-project` unless `--project` names one.

How `rwv fetch` handles each manifest repository:

| Clone & lock state | `rwv fetch` behavior |
|---|---|
| Absent on disk | Clones from remote and attaches the declared `version` branch at the locked SHA (or remote tip if unlocked) |
| Present, locked SHA is a fast-forward of the declared `version` branch | Fast-forwards the local branch to the locked SHA and leaves HEAD attached (local object store only; no network fetch) |
| Present, locked SHA is missing from the local object store | Errors (`fetch` does not contact the remote for an already-present clone; run `git -C <repo> fetch` first) |
| Present, locked SHA is not a fast-forward or checkout is on another branch | Fails that repo, unless `--detach-checkouts` is passed to materialize the pin on a detached HEAD (use `git checkout <branch>` later before running `rwv sync-to`). The other repos still align, and the run exits 1 with `partial-run-aborted` |
| Present, no lock entry (or no `rwv.lock`) | Leaves the clone at its current HEAD and records that SHA in `rwv.lock` (unless `--frozen` is passed) |

| Flag | Effect |
|---|---|
| `--frozen` | Error if lock is incomplete; never advance. Suitable for CI |
| `--allow-non-empty-dir` | Bootstrap into a non-empty directory that is not a workspace |
| `--no-reference` | Skip cloning/fetching repositories with `role = "reference"` |
| `--detach-checkouts` | Realign a present clone even where that changes what HEAD is attached to: materialize the pin on a detached HEAD instead of refusing |
| `--role` / `--repo` | Selector filters (see [Selector grammar](#selector-grammar)). A filtered run skips the lock write |
| `-j N` | Parallel per-repo workers (default: `min(nproc, 8)`; `-j 1` for serial execution) |
| `--json` | Structured output: single JSON envelope when `-j 1`, self-describing NDJSON lines when `-j > 1` (default on multi-core hosts) |
| `--project <name>` | In-place mode only: fetch this project instead of the active one (does not change `.rwv-active`). With `<source>` it refuses with `inapplicable-flag` |
| `--no-materialize` | Skip the install hooks of the activation a fetch performs (a bootstrap fetch's, or an in-place fetch selecting the weave's only project); `.rwv-active` and the weave-root links are still written. With a project already active, an in-place fetch activates nothing and the flag has no effect |

The lock is **read-only** for covered entries. To advance tips and re-snapshot, use `rwv update`.

`--json` emits `{ "$schema": "...", "outcomes": [...] }` envelope in serial mode
(`-j 1`). Under `-j N` with `N > 1`, switches to NDJSON (one
self-describing record per repo as workers finish). Without `-j` the worker
count is `min(nproc, 8)`, so multi-core hosts get NDJSON. See [JSON envelope
convention](#--json-envelope-convention).

### `rwv update [...]`

**The verb that gets the latest.** Advance each member repo to its branch HEAD on the remote, then re-snapshot `rwv.lock`. Maps semantically to `cargo update` / `npm update`.

The advance is a **fast-forward of the branch the checkout is on**, not a checkout of the tip: on the canonical, that branch must be the local counterpart of the one `version` declares, and `update` refuses when the checkout is on any other branch. It also refuses when the tip is not a fast-forward — reconcile the branch with its tracking tip yourself (ordinary `git rebase` / `git merge`) and re-run, or pass `--detach-checkouts` to materialize the tip on a detached HEAD without moving your branch. Inside a workweave the same fast-forward rule applies to the workweave's own branch, and a divergence points at `rwv sync` rather than at a flag. An already-detached member stays detached, unless the repo is stopped mid-rebase / mid-merge / mid-bisect, which refuses.

`advanced N repo(s)` counts repos whose SHA actually changed.

| Flag | Effect |
|---|---|
| `--detach-checkouts` | Advance a repo even where that changes what HEAD is attached to: materialize the tip on a detached HEAD instead of refusing |
| `--role` / `--repo` | Selector filters |
| `-j N` | Parallel per-repo workers (default: min(nproc, 8)) |
| `--json` | Structured output: envelope under `-j 1`, NDJSON under `-j > 1` |
| `--dirty` | Allow update with uncommitted changes in repos when relocking |
| `--commit` | Commit `rwv.lock` together with the integration files regenerated against the new tips, and with `rwv.toml` when it has uncommitted changes. Other uncommitted changes in the project repo refuse (`dirty-checkout`) |
| `-m` / `--message <msg>` | The commit message for `--commit`, in place of the generated summary. Without `--commit` it is a usage error (exit 2) |
| `--no-materialize` | Skip the install hooks of the regeneration `update` runs at primary; `rwv materialize` runs them later. Inside a workweave `update` runs no hooks either way |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

A repo that fails to advance fails the run: the repos that did advance stay where they moved, the lock is not written, and the run exits 1 with `partial-run-aborted`.

`--json` emits one record per repo with `path`, `absolute_path`, `branch`, `kind` (`updated` / `up-to-date` / `failed`), `old_sha`, `new_sha`, and `error`. See `rwv explain update`.

### `rwv lock`

Checkpoint current local state into `rwv.lock`. No network access.

Reads HEAD from each repo and records its resolved 40-character hex commit SHA in `rwv.lock` (never a tag or branch name, even when a tag points at HEAD). Errors on uncommitted changes (use `--dirty` to bypass).

| Flag | Effect |
|---|---|
| `--dirty` | Allow locking repos with uncommitted changes |
| `--commit` | Commit `rwv.lock` after writing it, together with `rwv.toml` when it is modified or has never been committed. Other uncommitted changes in the project repo refuse (`dirty-checkout`) |
| `-m` / `--message <msg>` | The commit message for `--commit`, in place of the generated summary. Without `--commit` it is a usage error (exit 2) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

The generated commit message names what the commit holds: `lock: refresh N repo(s)` (followed by the per-repo list) when pins moved, `lock: drop <repo>` (or `drop N repos`) when repos left the manifest, `lock: record rwv.toml` when only `rwv.toml` changed, each joined when several apply, and `lock: no changes` otherwise. `--commit` prints `Committed rwv.lock` or `Committed rwv.lock, rwv.toml`, or `Lock unchanged, nothing to commit.`

Pure git SHA snapshot — no integration hooks fire. To refresh ecosystem lockfiles (`node_modules`, `.venv`, etc.) after membership changes, run `rwv materialize` (or `rwv activate <project>` when switching the active project).

### `rwv activate <project>`

Switch the active project in the primary weave: updates `.rwv-active`, surfaces weave-root symlinks for the project's existing integration and static files, and runs integration install hooks (`npm install`, `pnpm install`, `uv sync`, `cargo fetch`). Does not author or regenerate integration workspace files in the project directory; it reports a missing or drifted one as a warning and names `rwv doctor --fix`.

| Flag | Effect |
|---|---|
| `--no-materialize` | Skip integration install hooks for a fast context-switch; `rwv materialize` runs them later |

An install hook whose tool is not on `PATH` does not fail the verb: it prints ``[warning] <integration>: <tool> is not on PATH, so this integration's install hook did not run; install <tool>, then run `rwv materialize` `` (finding kind `tool-missing`) and the other hooks still run. This holds for every verb that runs install hooks. Every line a verb prints for a side effect it skipped is listed under [Skipped side effects](#skipped-side-effects). A hook whose tool runs and fails is an error: the verb exits 1, and `activate` leaves `.rwv-active` unwritten.

`.rwv-active` is the single source of truth for the active project; CWD does not override.

The install hooks are withheld while a generated file rwv attests holds content it never accepted — see `rwv materialize` below, which is where that is settled.

Refuses if an operation elsewhere holds op-state covering this workspace (`op-in-progress`).

### `rwv materialize [...]`

Reconcile integration-managed workspace files, weave-root symlinks, and install hooks for the project this checkout already presents. Valid in both a primary weave (for `.rwv-active`) and a workweave (for `.rwv-workweave`). Takes no project name; at a primary with no active project it refuses and points to `rwv activate <project>`. Refuses if an operation elsewhere holds op-state covering this workspace (`op-in-progress`).

`rwv materialize` runs these steps in order:

| Step | Contract |
|---|---|
| 1. Undeclared links | Only with `--remove-undeclared-links`: unlinks weave-root symlinks at names the project no longer declares, naming each one |
| 2. Disabled-integration cleanup | Strips marked regions from co-owned files and deletes whole files authored by integrations now disabled in `rwv.toml`, along with their weave-root symlinks (`disabled-integration-artifact` in `rwv doctor`, which has no auto-fix arm so deletion stays explicit here). Drops attestations for files no enabled integration generates |
| 3. Surfacing repair | Re-creates missing or broken weave-root symlinks for the project |
| 4. Drift check | If an attested generated file's bytes differ from the accepted generation, refuses (`unaccepted-generated-content`) unless `--regenerate-drifted` or `--adopt-drifted` is passed |
| 5. Host settings | Fits host-dependent settings, such as the cargo `rustc-wrapper` key, to this machine |
| 6. Install hooks | Runs each enabled integration's non-advancing install command (`npm install`, `pnpm install`, `uv sync`, `cargo fetch`) without moving existing dependency pins. A generated lock file whose inputs changed (`derived-state-stale`) is re-derived here, by the tool that writes it |

`rwv materialize` authors no integration workspace file: a missing or drifted `package.json`, `go.work` or `Cargo.toml` region is `rwv doctor --fix`'s to regenerate.

Every step reads what the project's integrations declare, so an integration that cannot read its own `[integrations.<name>]` block can stop the verb before its first step. For example, an unknown key in a `cargo-workspace` `members` entry makes `rwv materialize` refuse, changing nothing (`materialize: nothing here was changed`), and list the block with the field to correct. Over the same block the intent verbs (`add`, `remove`, `role`, `update`) refuse and leave `rwv.toml` unchanged, and `rwv activate` withholds its install hooks.

| Flag | Effect |
|---|---|
| `--regenerate-drifted` | Discard the drifted content and regenerate it from the current inputs |
| `--adopt-drifted` | Record the drifted content as the accepted generation |
| `--remove-undeclared-links` | Unlink weave-root symlinks at names the active project no longer declares (the files they point at are untouched) |

Passing both `--regenerate-drifted` and `--adopt-drifted` is refused. The bare run lists every path it would act on, and what `--regenerate-drifted` discards is not recoverable through `rwv`. `rwv activate`, `rwv add`, `rwv remove`, `rwv role`, `rwv update`, `rwv workweave create` and `rwv doctor --fix` run the same hooks and withhold them (naming both flags) while drift stands. `rwv activate --no-materialize` selects a project without running the hooks.

### `rwv init <name-or-source> [--provider <registry>/<owner>] [--adopt]`

Create a new project repo at `projects/<name>/` with an empty `rwv.toml`, configure `rwv.lock merge=rwv-ours` in `.gitattributes` and the `merge.rwv-ours` git driver in the project repo's config (checked by `rwv doctor` as `missing-replay-exclusion` and `missing-merge-driver-config`), and activate the project. With `--provider`, configures the project repo's remote URL.

When invoked in an **empty directory**, `init` bootstraps that directory as a weave root (no pre-existing `rwv.toml` required) and creates the project inside it. Running in a non-empty directory without an existing workspace refuses.

With `--adopt`, `<name-or-source>` is a URL or `owner/repo` shorthand: `init` clones the project repo from that source instead of `git init`-ing a new one (brownfield adoption of an existing project repo), and runs the adopted project's install hooks as it activates it. `--no-materialize` skips those hooks; without `--adopt` it is a usage error (exit 2), since a new project has no members for a hook to act on.

### `rwv add <url> [...]`

Clone a repo (if not present), register it in the target project's `rwv.toml`, settle write permissions for the role, regenerate integration-managed workspace files, and run integration install hooks (at primary; inside a workweave `add` runs no hooks). If the repo entry was already added to `rwv.toml` by hand, `rwv add` converges the on-disk clone, permissions, and integration files to match.

The positional argument has one spelling per meaning:

- **A URL** (`https://…`, `git@…`, `file://` for a local remote) or **registry shorthand** (`github/owner/repo`) clones from that remote.
- **`--adopt <path>`** registers the clone already at `<path>` under the weave root, reading its URL from its `origin` remote. A relative `<path>` resolves against the directory `rwv` runs in (or the `-C`/`-w` directory), and the member path recorded is its position under the weave root containing it; from a workweave, a path inside the workweave names the primary's clone at the same position. No clone there (a bare repo counts as none) refuses with `no-clone-to-adopt`; a location under no weave root refuses with `adopt-path-outside-weave`; a workweave path holding a clone that is not a worktree of the primary's refuses with `foreign-clone-in-workweave`.
- **`--new <address>`** creates a repo; see below.
- **Anything else** — a plain path, relative or absolute — refuses with `unresolvable-repo-source`, naming both the `file://` and the `--adopt` spelling. A URL-shaped argument no registry can place (`https://example.com`) refuses with `no-matching-registry`.

| Flag | Effect |
|---|---|
| `--role <role>` | Sets the role (`owned` / `fork` / `dependency` / `reference`). Defaults to `owned`. |
| `--adopt` | Register an existing clone; the positional argument is its path (relative to the invocation directory) under the weave root. Conflicts with `--new` |
| `--new` | Create a new repo instead of cloning; the positional argument becomes a creation address (a bare registry name, or `registry/owner/repo`) rather than a URL or path. See `rwv explain add`. |
| `--param <name>=<value>` | A creation parameter for `--new` (repeatable). Requires `--new` |
| `--params-json <json>` | The same parameters as one JSON object of string values. Requires `--new` |
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

`rwv add` writes to CWD's workspace's manifest (the active workspace's `rwv.toml`), not always primary's.

**A clone already at the placement.** When the URL's placement already holds a git clone, `add` registers it only if its `origin` URL is equivalent to the one given (`Directory already exists at '<path>', skipping clone`). A clone of any other remote, or one with no `origin`, refuses with `foreign-clone-at-placement`, naming `rwv add --adopt <path>`; nothing is written. Anything at the placement that is not a clone (a directory, empty or not, or a file) refuses with `non-clone-at-placement`, naming it; so does one at the workweave's own placement when `add` or `add --adopt` runs from a workweave.

**Canonical path.** Every URL lands at `<registry>/<owner>/<repo>/`. `<registry>` is the matched built-in registry's name (`github`, `gitlab`, `bitbucket`) when the host is one of those; for a host none of them recognize, it's the URL's own host (e.g. `git.corp.example/team/repo/`); for `file://`, which has no host, it's `local`.

**Shared-clone warning.** If the target clone directory is already registered by another project in the same weave, `rwv add` proceeds (the manifest entry is added to the active project as usual) and emits a warning to stderr naming the other project(s). Sharing a clone across projects is legal — the same repo can be a `dependency` in one project and `owned` in another — but is worth flagging so accidental double-registration is visible.

### `rwv remove <path> [...]`

Remove the repository from `rwv.toml`, regenerate integration-managed workspace files, and re-run integration install hooks (at primary; inside a workweave `remove` runs no hooks). If the entry was already removed from `rwv.toml` by hand, `rwv remove` converges the integration files and optional checkout deletion (`--delete`) to match.

| Flag | Effect |
|---|---|
| `--delete` | Also delete this workspace's checkout of the repo: the canonical clone at primary, the workweave's worktree inside a workweave, along with the branch the workweave holds in the canonical clone. Refuses over uncommitted changes or commits nothing else reaches |
| `--delete-shared-clone` | With `--delete` at primary, remove the clone even if other projects still reference it. Without it, a clone another project lists refuses (`shared-clone-referenced`); the check runs only at primary, since a workweave's worktree is that workweave's own |
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

### `rwv role <path> <role>`

Change the repo's `role` in `rwv.toml`, regenerate integration-managed workspace files (a `reference` repo leaves every ecosystem's build graph), run integration install hooks (at primary; inside a workweave `role` runs no hooks), and settle the repo's files: a `reference` repo's tracked regular files lose their write bits; any other role restores owner write permission. If `role` was already edited in `rwv.toml` by hand, `rwv role` converges the on-disk permissions and integration files to match. Refuses a change to `reference` while the repo has uncommitted changes to tracked files.

| Flag | Effect |
|---|---|
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

### `rwv sync <source> [...]`

Absorb `<source>`'s committed state into CWD. `<source>` is required: a workspace name (`primary`, a workweave name) or a path, relative paths resolving against the directory `rwv` runs in (or the `-C`/`-w` directory). Bare `rwv sync` (no `<source>`) is an error with a hint pointing at `rwv sync-to`.

| Flag | Effect |
|---|---|
| `--strategy ff\|rebase` | Default `ff`. Applies uniformly to project and member repos; `rwv.lock` is excluded from the per-commit merge inputs git uses during rebase replay and regenerated in Phase 3. (`merge` is not offered — see [sync semantics](../explanation/joints/sync-semantics.md#why-no-merge-strategy).) |
| `--allow-stale-lock` | Consent: skip the lock-freshness precondition on both source and destination |
| `--discard-local-commits` | Consent: discard CWD's project commits not reachable from source, hard-resetting the project repo to source's tip. Pre-sync state preserved in `refs/rwv/pre-op/<id>` |
| `--continue` | Resume a sync interrupted mid-op. All parameters, the project included, are read from the in-progress op-state file; no other flags may be passed alongside `--continue` (except `--project`, which asserts rather than rebinds — naming a project other than the recorded one is refused) |
| `--json` / `-j N` | Structured output / parallel sync (NDJSON when N > 1) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

See [sync semantics](../explanation/joints/sync-semantics.md) for the three-phase model and the direction-explicit pair with `rwv sync-to`.

**Destination cleanliness preflight.** Before any repo is touched, `rwv sync` scans the destination workspace for uncommitted tracked changes; a dirty path refuses the operation with the offending files listed. `rwv sync-to` runs the same scan on its source (CWD). Drift attributable to rwv (regenerated ecosystem files, activation-owned symlinks) is excluded. Commit or stash the listed paths and retry.

**A checkout replay could not prune.** Replay removes the checkout of a member the replayed manifest no longer names, and refuses to remove one holding uncommitted changes, commits nothing else reaches, or the weave's only copy of its store. The refusal does not stop the op: replay and relock finish and op-state is cleared. The verb then exits 1, naming each checkout still on disk:

```
Error: sync completed: replay and relock finished and op-state is cleared, so `rwv abort` does not apply. Pruning was refused for 1 member checkout the manifest no longer names, and it is still on disk:
  local/owner/extra
Re-running sync will not prune it. Once nothing in it is needed, remove it by hand.
```

`rwv sync-to` behaves the same way, except under `--retire`, which refuses first (below).

**Exit codes:** `0` when the op completes; `1` when it parks on a refusal or conflict (op-state held; `--continue` or `rwv abort`), and when it completes with a checkout it could not prune (op-state cleared). Under `--json` the envelope's `complete` field tells the two `1`s apart: `true` when the op ran its last phase and cleared op-state, `false` when a phase parked it.

### `rwv sync-to [<target>] [...]`

Advance `<target>` to CWD's tip via a three-step orchestration: (1) rebase CWD against target; (2) auto-relock CWD if manifest tips moved; (3) FF-advance target to CWD's new tip. All rewriting happens in CWD; target is only ever advanced via fast-forward.

`<target>` is a workspace name (`primary`, a workweave name) or a path, relative paths resolving against the directory `rwv` runs in (or the `-C`/`-w` directory). Omit inside a workweave to auto-target the parent recorded in `.rwv-workweave`. Required in a primary weave.

| Flag | Effect |
|---|---|
| `--strategy ff\|rebase` | Default `rebase` (unlike `rwv sync`). Step 3 is always FF regardless. (`merge` is not offered — see [sync semantics](../explanation/joints/sync-semantics.md#why-no-merge-strategy).) |
| `--retire` | Delete the workweave on success. Requires a workweave context; warning + no-op in a primary weave. Refuses, after the landing has completed and with the op parked at the retire phase, while the workweave holds divergent work (`retire-not-converged`), uncommitted changes in any checkout (`dirty-checkout`), or a checkout replay could not prune (`retire-unpruned-checkout`); resolve it and `--continue`, or `rwv abort` |
| `--allow-stale-lock` | Consent: skip the lock-freshness precondition on both source and destination |
| `--discard-local-commits` | Consent: discard CWD's project commits not reachable from target, hard-resetting the project repo to target's tip. Pre-sync state preserved in `refs/rwv/pre-op/<id>` |
| `--continue` | Resume after resolving a mid-op conflict. All parameters are read from the in-progress op-state file |
| `--json` / `-j N` | Structured output / parallel step-1 sync (NDJSON when N > 1) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

See [sync semantics](../explanation/joints/sync-semantics.md) for the full three-step model, strategy semantics, and the `--retire` contract. Exit codes and the unpruned-checkout case are as for `rwv sync` above.

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

Coordinated cross-repo push from the primary weave. Default scope: `owned` and `fork` repos (the roles the operator controls). `dependency` and `reference` repos are skipped unless `--role` or `--repo` selectors override the default. Project repo is pushed last.

**Preconditions** (evaluated in this order, before any network call, under `--dry-run` too):
- **Primary weave only** — refused inside a workweave (`push-from-workweave`); land work into primary with `rwv sync-to` first.
- **Project repo branch** — `projects/<project>` must be on the branch its remote declares as default (`origin/HEAD`), whatever that branch is called. With `origin/HEAD` unset push refuses rather than guess (`no-remote-default-branch`, naming `git remote set-head origin -a`); a detached or unborn HEAD refuses with `head-not-on-branch`, and any other branch with `project-repo-off-canonical-branch`.
- **Committed project state** — the project repo has no staged or unstaged change to a tracked file, and `rwv.toml` and `rwv.lock` are tracked (`dirty-checkout`). Push publishes the project repo's last commit, so commit first with `rwv lock --commit` or `rwv update --commit`. Other untracked files pass.
- **Lock agreement** — every member repo's HEAD must match the committed `rwv.lock` entry (`lock-state-mismatch`).
- **Pushable member branches** — every repo in the plan must be on a branch (`unpushable-repo-branch`).

| Flag | Effect |
|---|---|
| `--role` / `--repo` | Selector filters |
| `--force` | Force-push every repo in the operation (member repos and the project repo). Default deny. The lock-precondition check still runs unconditionally |
| `-j N` | Parallel push (up to N concurrent for member repos; project repo always last and serial) |
| `--json` / `-j N` | Structured output / parallel push (NDJSON when N > 1) |
| `--dry-run` | Print the push plan without executing |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

`--json` emits the envelope `{"$schema": "...", "outcomes": [...]}`. Member-repo records use `kind` `pushed`, `skipped`, or `failed`; the project-repo record (always last) uses `kind` `project-repo-pushed` or `project-repo-failed`. See [JSON envelope convention](#--json-envelope-convention).

See [push a cross-repo feature](../how-to/push-cross-repo-feature.md).

### `rwv abort [--abandon-foreign-tip <repo>]`

Roll an in-flight `sync` or `sync-to` back to its savepoints in every workspace the op touched: `refs/rwv/pre-op/<op-id>`, and `refs/rwv/pre-op/<op-id>-target` for the repos of a `sync-to`'s target, whose checkouts share a ref store with the workweave's. Run it from either workspace of the op: the verdict for each repo is the same, and op-state is cleared in both. Runs VCS-native abort for in-progress operations (`git rebase --abort`, `git merge --abort`). Lines for the workspace abort runs in carry no prefix; lines for the op's other workspace are labeled with its side, `[target] <repo>` or `[source] <repo>`.

| Flag | Effect |
|---|---|
| `--abandon-foreign-tip <repo>` | Consent to restoring this repo over a tip abort cannot attribute to the op. Repeatable, one repo per occurrence; **no all-repos form**. Spell `<repo>` as abort's per-repo output does — the workspace-relative path, or `(project)`. The abandoned commits stay reachable at the pre-abort reference abort prints — `refs/rwv/pre-abort/<op-id>`, or `refs/rwv/pre-abort/<op-id>-target` for a repo on a `sync-to`'s target side; where that reference does not already hold the tip, abort refuses even with the flag. |

Errors if no sync operation is in progress. Without the flag, a repo whose tip is unattributable is refused, not reset.

**Exit codes:**
- `0` — all repositories restored cleanly to their savepoints and op-state removed.
- `1` — a refusal (`no-op-recorded` with nothing in progress, or `foreign-tip` without `--abandon-foreign-tip <repo>`), or a partial restore failure (`abort completed with failures`: at least one repository failed to reset, and op-state is retained so `rwv abort` can be retried).

### `rwv status [--json] [...]`

Show per-repo state for the CWD workspace.

| Flag | Effect |
|---|---|
| `--json` | Output as JSON (see envelope below) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |
| `--role` / `--repo` | [Selector filters](#selector-grammar); narrow the table and the `--json` `repos` array. A filter matching no repo prints no rows and exits 0 |

| Column | Values |
|---|---|
| path | Repo path relative to weave root |
| branch | Current branch name, `-` if detached |
| tip | Current HEAD SHA (first 12 chars) |
| lock SHA | SHA from `rwv.lock` (first 12 chars), `-` if no lock |
| relation | `ok` / `ahead` / `behind` / `diverged` / `no-lock` / `unknown` / `missing` / `unreachable` |
| mid-op | Present if mid-rebase, mid-merge, etc. |

`--json` emits the envelope `{"$schema": "...", "repos": [...]}`. Each record includes `lock_pinned_by_name` (`true` when an older or hand-edited `rwv.lock` entry holds a tag or branch name rather than a 40-character hex commit SHA; `rwv lock` rewrites it to a commit SHA). See [JSON envelope convention](#--json-envelope-convention).

A `missing` relation names a clone directory that has vanished; the repair is `rwv fetch` with no source — the in-place re-materialize mode — run from the weave root (see [`rwv fetch`](#rwv-fetch-source-) above and `rwv doctor` for the paired detection: `missing-canonical-clone`, `dangling-reference`). An `unreachable` relation names a clone that is present but whose object store lacks the locked SHA; in-place `rwv fetch` does not repair it (with the directory present it performs a local checkout without a network fetch). If the remote still has the revision, `git fetch` in the affected repo re-pulls it; if the remote's history was rewritten, re-lock to a reachable state — see [reconcile repos](../how-to/reconcile-repos.md).

### `rwv doctor [...]`

Convention and health audit across clones, `rwv.toml`, `rwv.lock`, workweaves, op-state leases, git attributes/merge drivers, and enabled integrations.

| Flag | Effect |
|---|---|
| `--locked` | Zero exit iff every repo tip matches its lock entry (precondition for `rwv sync`) |
| `--fix` | Repair every finding marked **Auto-fixable** in [Doctor findings](./doctor-findings.md), which carries that mark on each finding it documents. Never touches live staged content or live edits. Idempotent. |
| `--json` | Emits envelope `{"$schema": "...", "violations": [...], "issues": [...], "advisories": [...], "plugins": [...]}`: rwv's own findings on `violations`, integration findings on `issues` |
| `--all` | Scan all projects and run weave-wide checks (orphan detection, cross-project stale locks, etc.). By default only the active project is checked |
| `--reattach-checkouts` | With `--fix`, reattach a canonical store's detached HEAD to its tracking counterpart when that counterpart exists and its tip equals HEAD. Without this flag, `--fix` only reports a detached canonical, naming the `git switch` that would reattach it |
| `--adopt-detached-checkouts` | With `--fix`, let the branch-model migration mint a workweave's ephemeral branch at a detached checkout's HEAD (the lock SHA), giving up a pre-flat branch holding that name if one exists (warns if doing so strands commits HEAD does not carry). Without this flag, `--fix` reports both tips and leaves the checkout alone |
| `--kind <kind>` | Filter the report to the named finding kind (repeatable; text and `--json` both). Kind names are the wire spellings `--json` emits, for `violations` and `issues` alike (e.g. `branch-discipline`, `orphaned-savepoint`, `tool-missing`); a filter naming only `violations` kinds excludes every integration issue. An unknown name refuses (`unknown-finding-kind`), listing the valid set. Kinds whose classes normally collapse to a per-class count line render itemized under the filter — this is the drill-down. The exit code reflects the filtered view, text and `--json` alike: non-zero on any finding in the view, warnings included, so `rwv doctor --kind tool-missing` (or `--json --kind tool-missing`) fails a CI job on a missing tool |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

Without `--kind`, the exit code is non-zero when any finding is an error, and a warning leaves it at `0` — text and `--json` alike. `--fix` exits non-zero when a repair it attempted failed or refused, or an error finding remains.

See [Doctor findings](./doctor-findings.md) for the complete catalog of wire `--kind` / `sub_kind` tokens, their severities (`error` vs `warning`), and their `--fix` dispositions.

### `rwv workweave <project> create <name>`

Create an isolated workweave for `<project>`:

1. **Manifest worktrees** — creates a git worktree on an ephemeral `<project>--<name>` branch for every member repo (`owned`, `fork`, `dependency`, and `reference`), with `reference` repos' tracked regular files set read-only (`chmod -w`, preserving symlinks).
2. **Project worktree & marker** — creates the project worktree at `projects/<project>/` on `<project>--<name>` and writes `.rwv-workweave` at the workweave root.
3. **Activation** — verifies the existing integration files in `projects/<project>/` (a missing or drifted one is reported as a warning naming `rwv doctor --fix`; `create` authors none), surfaces workweave-root symlinks, fits host settings such as the cargo `rustc-wrapper` key, and runs the integration install hooks (`npm install`, `uv sync`, `cargo fetch`, …) in the new workweave. `--no-materialize` skips the hooks; `rwv materialize` inside the workweave runs them later.

A failure before the install hooks rolls the workweave back. An install hook that fails after the workweave is created and registered does not: the workweave is kept, and `create` exits 1 with `workweave created at <path>; only its install hooks did not complete. Fix the cause, then run rwv materialize inside it`. Under `--hook-mode` and `--claude-hook` the same failure is a warning on stderr and `create` exits 0, so the agent runtime that asked for the workweave still gets its path. A missing tool is a `tool-missing` warning in every mode.

| Flag | Effect |
|---|---|
| `--from <source>` | Fork from a specific source (default: CWD's active workspace). Accepts `primary`, an absolute path, a path relative to the invocation directory, or omitted to fork from CWD's active workspace. Forking from an existing workweave is how you **duplicate** one; copying a workweave with `cp` aliases the original's git state rather than duplicating it |
| `--replace-existing` | Destroy an existing workweave at this path before recreating. Without this flag, re-invoking `create` against an existing workweave is the idempotent path. Refuses if the existing workweave has uncommitted or unmerged work |
| `--capture-dirty` | Allow creation when the source project directory has uncommitted changes. Uncommitted edits to `rwv.toml` and `rwv.lock` are **moved**: copied into the new workweave's project worktree, then returned to their committed version in the source, so a later `rwv sync-to` from the workweave is not refused over the source's copy. Other uncommitted files stay in the source only, and `create` lists them. Without this flag, `create` refuses (`dirty-project-dir`) |
| `--dir <path>` | Per-invocation placement override. Places the workweave at exactly this path (recorded verbatim in the index). Absolute paths are used as-is; relative paths resolve against the invocation directory. Overrides the recorded container for this invocation only |
| `--no-materialize` | Skip the install hooks in the new workweave; `rwv materialize` inside it runs them later |

Workweaves live at `<container>/<project>--<name>/` where `<container>` is recorded per-project in `projects/<project>/.rwv-workweave-index` (machine-local JSON, excluded from git through the project repo's `.git/info/exclude`, never `.gitignore`). The default container is `<parent-of-primary>/.workweaves`. Set the container explicitly with `rwv workweave <project> set-container <path>`; `create` records new entries into the index; every `find`-direction verb (list, delete, sync targets by bare name) resolves via the recorded `name → absolute path` entries with `.rwv-workweave` marker round-trip validation. Doctor reconciles the index against on-disk state — stale entries are pruned, orphan workweaves are adopted, a tracked index is flagged as a hygiene finding.

### `rwv workweave <project> set-container <path>`

Record the workweave container for `project`. Writes the `container` field of `projects/<project>/.rwv-workweave-index`. Absolute paths are used as-is; relative paths resolve against the invocation directory. Existing registry entries are preserved. An explicit, recorded, audit-visible act, not ambient process state.

### `rwv workweave <project> delete <name> [--discard-uncommitted] [--discard-unmerged-commits]`

Delete a workweave. Default refuses if any checkout under it is dirty (`uncommitted-work`), or holds commits contained in neither the workweave's recorded parent nor the primary weave (`unmerged-commits`; work in a nested workweave counts as merged once its parent has it). `--discard-uncommitted` waives the first refusal, `--discard-unmerged-commits` the second.

Both checks cover every checkout under the workweave, not only the members the project's manifest names: a member replay could not prune, one the workweave added that has not landed, or a clone put there by hand. Such a checkout is listed with the suffix `(not a manifest member)`, and one whose commits no baseline holds at all is reported rather than skipped. `create --replace-existing` weighs the same set.

### `rwv workweave <project> list`

List workweaves for a project.

### `rwv workweave <project> log [--diff] [--json]`

Show this workweave's unique commits versus its recorded parent, per repo, including the project repo (`projects/<project>`). Must be run from inside a workweave.

| Flag | Effect |
|---|---|
| `--diff` | Show the unique diff versus the parent instead of the commit listing. Anchored at the common ancestor, so commits the parent gained after the fork are not shown as reversals |
| `--json` | Emit machine-readable JSON |

Text output includes one `=== <path> ===` section per member repo followed by `=== (project) ===` for the project repo. `--json` emits an object with `workweave`, `parent`, `diff` (boolean), `repos[]`, and `project_repo` (same shape as each element of `repos[]`, with `path` set to `"(project)"`). Each `repos[]` element carries `path`, `head`, `parent_tip` and `unique_commits[]` (each `{ "sha", "short", "subject" }`); under `--diff` also `diff_base` and `diff`; and `note` when the repo could not be fully read, such as a checkout that is missing.

### `rwv workweave [--hook-mode] [--claude-hook] <project> <action>`

Two flags on `workweave` itself, preceding `<project>` — not part of any subaction — for driving workweave creation and teardown from Claude Code's own hook events instead of the shell.

| Flag | Effect |
|---|---|
| `--hook-mode` | With `create`, print only the new workweave's path to stdout instead of the usual create output; everything else, install-tool output included, goes to stderr. An install hook that fails in the created workweave is a warning, and `create` exits 0 |
| `--claude-hook` | Read a Claude Code hook payload as JSON from stdin and dispatch on it directly, bypassing `<project>` and `<action>` entirely. `hook_event_name: "WorktreeCreate"` creates a workweave — project inferred from the hook's `cwd` (the current workweave's project, or the primary weave's active project), name derived from `branch_name` (falling back to a timestamp) — and prints its path to stdout; an install hook that fails there is a warning, as under `--hook-mode`. `"WorktreeRemove"` deletes the workweave named by `worktree_path`; fire-and-forget, warnings go to stderr and it always exits `0`. `<project>` is not required when this flag is set. Conflicts with `--hook-mode` |

`rwv setup claude` registers `rwv workweave --claude-hook` for both the `WorktreeCreate` and `WorktreeRemove` Claude Code hook events; `--hook-mode` is for a hook command you write yourself.

### `rwv setup claude [--uninstall]`

Register Claude Code hooks in `~/.claude/settings.json` (`SessionStart` and `PreCompact` running `rwv prime`; `WorktreeCreate` and `WorktreeRemove` running `rwv workweave --claude-hook`). Requires `~/.claude/settings.json` to exist already (refuses if absent). `--uninstall` removes all `rwv` hooks from `~/.claude/settings.json`.

### `rwv completions <shell>`

Generate shell completions (bash, zsh, fish, etc.). Source the output in your shell rc file.

## Skipped side effects

A verb that leaves out a side effect it would otherwise perform says so on
stderr, in a line naming the verb that performs it. A run that skipped
nothing prints none of these lines, and none of them changes the exit code.

| When | Line |
|---|---|
| `--no-materialize` on `activate`, `add`, `remove`, `role`, `update`, `workweave create`, `fetch` or `init --adopt` | ``[skipped] core: the install hooks did not run (--no-materialize); `rwv materialize` runs them`` |
| `add`, `remove`, `role` or `update` inside a workweave, which never run hooks there | ``[skipped] core: the install hooks did not run (an authoring verb runs none inside a workweave); `rwv materialize` runs them`` |
| `add`, `remove`, `role` or `update` with `--project <name>` naming a project the weave root does not present: the files are regenerated and the verb stops there | ``[skipped] core: the weave-root links were not refreshed (`<name>` is not the project this weave root presents); `rwv activate <name>` refreshes them`` and ``[skipped] core: the install hooks did not run (`<name>` is not the project this weave root presents); `rwv activate <name>` runs them`` |
| `update --role` or `--repo`, which regenerates nothing and runs no hooks | ``[skipped] core: the integration files were not regenerated (the update was narrowed by --role or --repo); `rwv doctor --fix` regenerates them`` and ``[skipped] core: the install hooks did not run (the update was narrowed by --role or --repo); `rwv materialize` runs them`` |
| `fetch <source>` while another project is active (on stdout in text mode, stderr under `--json`) | ``rwv fetch: skipping auto-activate (project '<active>' already active); `rwv activate <fetched>` selects it and runs its install hooks`` |
| `fetch` with no source, its project already active, when it cloned a missing member or realigned one to its pin | ``[skipped] core: the install hooks did not run (project `<project>` was already active, so this fetch activated nothing); `rwv materialize` runs them`` (with `--project` naming a project the root does not present, the `rwv activate <project>` form above) |
| An install hook whose tool is not on `PATH`, on every verb that runs hooks | ``[warning] <integration>: <tool> is not on PATH, so this integration's install hook did not run; install <tool>, then run `rwv materialize` `` |

Under `--json` in serial mode (`-j 1`), the `rwv update` and `rwv fetch`
envelopes carry a `skipped` array, present and empty when nothing was
skipped. Each record has `effect` (`install-hooks`, `integration-files`,
`weave-root-links` or `activation`), `reason` (`no-materialize`,
`inside-workweave`, `narrowed`, or an object: `tool-missing` with the tool,
`not-presented` or `project-already-active` with the project), `remedy` (the
command), and `integration` for a single integration's hook. For example:

```json
"skipped": [
  { "effect": "integration-files", "reason": "narrowed", "remedy": "rwv doctor --fix" },
  { "effect": "install-hooks", "reason": "narrowed", "remedy": "rwv materialize" }
]
```

NDJSON (`-j N` with `N > 1`) carries no skipped records; the stderr lines still
print.

## Scripting helpers

Three verbs designed for shell-script and agent-harness consumption. Side-by-side coverage so the shapes are obvious.

### `rwv prime [--no-suppress]`

Emit structured workspace context for agent system prompts: active project, repos, roles, the integrations that run, lock state. Suppressed when CWD is not inside a weave or workweave (use `--no-suppress` to always emit).

Use case: agent harness reads this on session start to bootstrap context.

### `rwv resolve`

Print the weave root (workweave root if in a workweave, otherwise primary weave root). Useful in scripts:

```bash
cd $(rwv resolve)
```

Use case: one-shot path resolution from anywhere inside a workspace.

### `rwv explain [<verb>]`

Per-verb reflection — *replaces hand-maintained `--help` scraping* in agent harnesses.

| Invocation | Returns |
|---|---|
| `rwv explain` | List of explainable verbs |
| `rwv explain <verb>` | Markdown bundle: purpose, invocation, output description, JSON Schema (for `--json`-capable verbs), exit codes, examples, common errors |
| `rwv explain <token>` | The entry for a refusal token, a `rwv doctor` finding kind or sub-kind, or a VCS error kind, as [Refusals](./refusals.md), [Doctor findings](./doctor-findings.md) and [VCS errors](./vcs-errors.md) print it. An unknown name refuses (`no-explain-entry`), suggesting a near match |

Every page it serves opens with a `rwv <version>` line — the same string `rwv --version` prints — and a blank line, so a saved copy says which build it describes.

Use case: agent harness asks "what flags does `rwv push` take, and what does it print?" — the bundle is authoritative. For `--json`-capable verbs (`status`, `doctor`, `fetch`, `update`, `sync`, `sync-to`, `push`), the JSON Schema is embedded as a fenced code block inside the bundle.

The rendered bundles are checked in at `docs/reference/explain/` for offline browsing.

## Project resolution

Project-scoped verbs (`add`, `remove`, `role`, `lock`, `update`, `push`, `sync`, `sync-to`, `status`, `doctor`, `fetch` in-place) pick their target project via a fixed chain, highest priority first:

1. `--project <name>` — the explicit override on the invocation.
2. `-w/--workweave <project>--<name>` — the `<project>--` prefix of the global workweave-selector flag names the project.
3. The weave root's own identity file — **one tier**, whose spelling follows the kind of root CWD resolved into:
   - `.rwv-workweave` marker, when CWD resolves inside a workweave. The marker names the project structurally.
   - `.rwv-active` pointer at a primary root — the file `rwv activate` maintains.

Step 3 is one tier rather than two ranked ones because the two files are **mutually exclusive**: a primary root carries the pointer, a workweave root carries the marker, never both. No invocation ever sees both answers, so there is no precedence between them to get wrong. `rwv doctor` enforces the exclusivity, reporting a root carrying both as a `weave-root-identity-conflict`.

The pointer is total at primary by construction: every path that creates a project leaves one active. `rwv init` activates the project it creates, and `rwv fetch <source>` activates the project it clones when none is active. A missing pointer therefore arises from hand surgery, and is repaired without a guess only where nothing is left to guess: an in-place `rwv fetch` in a weave holding exactly one project activates it. Anywhere else a missing or stale pointer produces a corrective error, not silent structure inference. `rwv workweave <project> create <name>` writes only the marker: a workweave's project is fixed at creation and cannot be switched, so there is no selection for a pointer to record.

### Target line — visibility when the pointer decides

When resolution falls through to step 3, the verb prints a target line to **stderr** before acting:

```
target: workspace /home/cwa/weaveroot/foundations · project tmuxcc (.rwv-active)
```

Explicitly (`--project` or `-w`) or structurally (workweave marker) resolved invocations stay silent — the operator already named the target, or the workweave did.

Stderr, not stdout, so the line never contaminates a `--json` verb's output. The line is prose, not a parse surface — no schema, no version, no consumer contract. The `resolution` block in `--json` output carries the resolved coordinate as machine data; provenance stays out of the JSON so agents assert on results, not on how they were reached.

### CWD inside another project

Changing into `projects/<name>/` does not select that project. When `.rwv-active` selects a different one and no `--project` is given, a verb that writes into its project — `add`, `remove`, `role`, `lock`, `update`, `push`, `sync`, `sync-to`, in-place `fetch`, `doctor --fix` — refuses (`cwd-in-inactive-project`) rather than write into the active project from the other's directory:

```
Error: CWD is inside projects/mobile/, but `.rwv-active` selects web, so this would act on web. Pass `--project mobile` to act on mobile, or `--project web` to act on web from here; `rwv activate mobile` switches the active project.
```

Reporting verbs do not refuse: `rwv status` warns, naming `--project <name>` and `rwv activate <name>`, and reports on the active project, and `rwv doctor` without `--fix` reports on the active project.

### Corrective errors

Each of these is a refusal and carries a token; the message ends with `rwv
explain <token>`, which serves that token's entry. [Refusals](./refusals.md)
lists every token, and a condition `rwv doctor` also reports keeps its entry on
[Doctor findings](./doctor-findings.md).

- **No active project** (`no-active-project`) — CWD is at a workspace primary with neither `--project` nor `.rwv-active` set. The error names the fix commands and, if projects exist under `projects/`, lists them as a menu:

  ```
  Error: no active project; run `rwv activate <name>` or pass `--project <name>`. Existing projects: foundations, tmuxcc
  ```

  In a workspace with no projects yet, the error suggests `rwv init` instead.

- **Stale pointer** (`dangling-active-project`) — `.rwv-active` names a project whose `projects/<name>/` directory does not exist:

  ```
  Error: active project `ghost` is set in `.rwv-active` but `projects/ghost/` does not exist; run `rwv activate <existing-project>` or remove `.rwv-active`.
  ```

  `rwv doctor` reports the same condition under that same token and, under `--fix`, clears the file — one token, one entry, whichever surface you met it on.

### Which verbs are project-scoped

| Category | Verbs | Resolution behavior |
|---|---|---|
| Project-scoped | `add`, `remove`, `role`, `lock`, `update`, `push`, `sync`, `sync-to`, `status`, `doctor` (project-scoped checks), `fetch` (in-place) | Full chain; target line when the pointer decides |
| Project named positionally | `activate <project>`, `workweave <project> …` | Explicit already; no chain consulted |
| Workspace-scoped, no project | `init`, `abort`, `resolve`, `prime`, `explain`, `doctor` (cross-project scan) | No project selection involved |

## Selector grammar

`rwv fetch`, `rwv update`, `rwv push`, and `rwv status` share a selector surface for picking which repos to operate on or report. The [`rwv each`](./rwv-each.md) plugin takes the same flags and hands them to `rwv status`.

### Flags

| Flag | Selects on |
|---|---|
| `--role <kind>` | Repo role (`owned`, `fork`, `dependency`, `reference`) |
| `--repo <pattern>` | Repo's manifest path |

`--role` parsing is **case-insensitive** (`--role Owned`, `--role OWNED` both work).

`--repo` patterns match against the *member-repo path* (e.g., `github/chatly/server`), not against project repo paths.

### Pattern prefixes

`--repo` accepts three pattern forms:

| Prefix | Form | Example |
|---|---|---|
| (none) | Exact match | `--repo github/chatly/server` |
| `re:` | Regex | `--repo re:'^github/chatly/(server\|web)$'` |
| `glob:` | Glob | `--repo glob:'github/chatly/*'` |

### Union semantics

Repeated flags are **union**:

```bash
rwv fetch --role owned --role fork                  # owned ∪ fork
rwv push --repo github/chatly/server --repo github/chatly/web   # server ∪ web
```

There is no intersection syntax. To compose intersections, pipe `rwv status --json` through `jq`.

`--role` and `--repo` together act as **union** as well: `--role owned --repo glob:'github/chatly/*'` matches owned repos *or* repos under `github/chatly/`. To filter both, use `--role` alone or `jq` post-filtering.

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
| `rwv doctor --json` | `violations` (and `issues` for integration findings) |
| `rwv fetch --json` | `outcomes` |
| `rwv update --json` | `repos` |
| `rwv sync --json` | `outcomes` |
| `rwv sync-to --json` | `outcomes` |
| `rwv push --json` | `outcomes` |

The `sync` and `sync-to` envelopes also carry `complete`: `true` when the op ran its last phase and cleared op-state, `false` when a phase parked it.

Schemas live at `docs/reference/schemas/<verb>.json` and are also embedded as fenced code blocks inside the corresponding `rwv explain <verb>` bundle.

### NDJSON under parallel mode

When a verb runs with `-j N` and `N > 1`, the output switches to NDJSON: one JSON record per line as workers finish, **no envelope wrapper**, and each line carries its own `"$schema"` field so a consumer can identify any single record without out-of-band context.

The branch-on-shape pattern lets consumers handle both modes uniformly: peek at stdout; if subsequent lines carry their own `"$schema"`, parse as NDJSON; otherwise parse as one envelope document.

Exit semantics under `--json` are the same in both modes and match the text mode: non-zero when at least one per-repo outcome is `failed`, when a refusal stopped the verb, and for `sync` and `sync-to` when a checkout replay could not prune is left on disk. The envelope, or the record lines, still print in that last case. NDJSON records carry no `complete`; `rwv status` answers whether an op is in progress.

## External commands

`rwv <verb>` where `<verb>` is not a core verb resolves to a `rwv-<verb>` executable on `$PATH` and execs it. This is the same convention `git` and `cargo` use for their external commands. Core verbs always win: clap matches them before external fallthrough runs, so a `rwv-status` on `$PATH` can never shadow the builtin, and naming a plugin after a future core verb makes it unreachable once that verb ships.

`rwv each` is such a plugin, shipped with `rwv`; see [rwv each](./rwv-each.md).

`rwv` projects the resolved workspace context into an environment envelope on every spawn — `RWV_VERSION`, `RWV_WORKSPACE`, `RWV_WORKWEAVE`, `RWV_WORKWEAVE_UNREGISTERED`, `RWV_PROJECT` — and propagates the child's exit status back verbatim. The child starts in the invocation directory — the `-C` directory, the `-w` workweave, or else the directory `rwv` was run from — so its relative arguments resolve the way a core verb's do. For the full contract — the envelope table (value and unset-condition for each variable), addressing back into `rwv`, exit-code semantics, discovery and naming, the write prohibition, and the `--json` compatibility guarantee — see the [plugin-protocol](./plugin-protocol.md) reference.

## Related

- [reference/plugin-protocol](./plugin-protocol.md) — the external-command wire contract in full
- [reference/rwv-each](./rwv-each.md) — the plugin that ships with `rwv`
- [reference/formats](./formats.md) — `rwv.toml`, `rwv.lock`, `.rwv-active`, `.rwv-workweave`
- [reference/roles](./roles.md) — role definitions and change-resistance semantics
- [reference/glossary](./glossary.md) — terminology lookup
- [reference/integrations](./integrations/index.md) — per-integration generated files and config
- [explanation](../explanation/index.md) — conceptual material referenced by these verbs
