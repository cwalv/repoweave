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

### `-V` / `--version` — release version

Prints `rwv <version>`, where `<version>` is the release version in semver form
(`0.24.0`). A binary built inside a git checkout adds the checkout's `git
describe` in parentheses as build information (`0.24.0 (v0.24.0-46-gae23167)`);
a binary built from a source archive has none. `RWV_VERSION` in the
[plugin envelope](./plugin-protocol.md) and the version a clean `rwv doctor
--all` records carry the release version alone.

## Verbs

### `rwv` (bare)

Show current context: weave root, active project, workweave (if any), repos.

### `rwv clone <source> [...]`

Clone a project and each of its members at the commit its `rwv.lock` pins.

`<source>` is a URL (`https://…`, `git@…`, `file://` for a local remote) or `owner/repo` shorthand. `clone` clones the project repo from `<source>` into the current directory, reads its committed `rwv.lock`, clones every listed member repo to its canonical slot, and activates the project when none is active (`.rwv-active`, weave-root links, install hooks). A plain path refuses with `unresolvable-repo-source`, naming the `file://` spelling. It writes no lock.

Each member is born attached to the local counterpart of the branch its entry declares, at the commit the lock pins. A member the lock cannot place is cloned on that branch at the tip of the remote's branch, listed, and left for the operator to settle; the clone itself completes:

| Member | State | Printed as |
|---|---|---|
| The lock has no entry for it, or there is no `rwv.lock` | `pin` `none` | `rwv clone: <repo>: pin none` |
| The lock pins a commit no ref in the clone reaches | `pin` `unreachable` | `rwv clone: <repo>: pin unreachable` |

A clone that left any member exits with status `3` ([Exit status](#exit-status)); `rwv explain pin` serves the state's page. This is the check a CI job runs for a reproducible build: a clone that exits `0` placed every member at its pin.

| Flag | Effect |
|---|---|
| `--allow-non-empty-dir` | Clone into a non-empty directory that is not a workspace |
| `--no-reference` | Skip cloning repositories with `role = "reference"`; they are not listed |
| `-j N` | Parallel per-repo workers (default: `min(nproc, 8)`; `-j 1` for serial execution) |
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `--no-materialize` | Skip the install hooks of the activation a clone performs; `.rwv-active` and the weave-root links are still written. With a project already active, a clone activates nothing and the flag has no effect |

`--json` emits the envelope `{ "$schema": "...", "outcomes": [...], "left": [...] }` once every repo is done, with `outcomes` sorted by repo path, whatever `-j` is, and `left` the members listed above (empty exactly when the exit status is `0`). See [JSON envelope convention](#--json-envelope-convention).

### `rwv fetch [...]`

The pruning fetch of every remote of every repo in the project: the project repo and each member present on disk. It brings branches and tags into each object store, drops the remote-tracking branches a remote no longer has, and reports what moved. It moves no local branch, no checkout and no lock.

A member's clone that lacks its url remote (the remote that has the member's `url`) gets one before the fetch, named by the url's owner segment. When that name already holds another url, the run refuses before it adds or fetches anything: `name` `taken`, naming the remote and both urls, with the choice to configure where the repo pushes and pulls.

A repo's fetch holds the claim on its object store, so fetches into one store run one at a time and parallel seats wait on each other instead of failing on a ref lock. A remote-tracking branch that moved to a tip not descending from the old one is `rewritten`, and its old tip is kept as a superseded tip under `refs/rwv/superseded/`. A branch the remote deleted is `pruned`, which supersedes nothing. A member not on disk is reported `skipped`; `rwv update` clones it. A repo whose remote could not be completed is `unconfirmed`, the cause named, and the run exits 1 with `partial-run-aborted` after trying every repo.

| Flag | Effect |
|---|---|
| `--no-reference` | Skip repositories with `role = "reference"` |
| `--role` / `--repo` | Selector filters (see [Selector grammar](#selector-grammar)). A selector narrows the fetch to the members it names and leaves the project repo out |
| `-j N` | Parallel per-repo workers (default: `min(nproc, 8)`; `-j 1` for serial execution) |
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `--project <name>` | Fetch this project instead of the active one (does not change `.rwv-active`) |

`--json` emits the envelope `{ "$schema": "...", "repos": [...] }` once every repo is done, with `repos` listing the project repo first and then the members by path, each with `status` (`fetched` / `unconfirmed` / `skipped`) and `moved`. See `rwv explain fetch`.

### `rwv pull [...]`

Fetch, fast-forward the project branch to its upstream, then update the members to the lock it holds. Three steps, in order:

1. The pruning fetch of every repo, as [`rwv fetch`](#rwv-fetch-). A repo it could not complete refuses the pull before anything is taken in.
2. The project branch fast-forwards when it is behind its upstream, and reports what the commits it took in changed: `re-pinned` for a member whose pin the pulled lock moved to a commit that does not descend from the old one, and `member-changed` for each member entry the pulled `rwv.toml` added, removed or changed. A branch that is at its upstream, ahead of it, or has none has nothing to take in. One that has diverged refuses as `push-branch` `diverged`, with nothing changed. Uncommitted changes to tracked files in the project repo refuse as `edits` `dirty` before the branch moves.
3. [`rwv update`](#rwv-update-), which `--role` and `--repo` narrow.

| Flag | Effect |
|---|---|
| `--role` / `--repo` | Selector filters; they narrow step 3 only |
| `-j N` | Parallel per-repo workers (default: `min(nproc, 8)`) |
| `--json` | Structured output: one JSON envelope with the fetch's records (`fetched`), the project branch's (`project`) and the update's (`outcomes`) |
| `--no-materialize` | As for `rwv update` |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

### `rwv update [...]`

Clone the members missing on disk, then align the rest with the branch the project repo has checked out. Each member goes where that branch's record, its member entries and its lock say it sits (the expected state), and the workspace records the branch it aligned for. It never advances a pin and never writes the lock; [`rwv update --remote`](#rwv-update---remote-) is the form that does. Run it from the weave root or a workweave.

This is the repair after members drift or go missing: a manifest entry pointing at a missing clone (the `missing-canonical-clone` and `dangling-reference` findings from `rwv doctor` point here), a raw checkout in a member, a workweave lacking a member's worktree, a project repo switched with the VCS. With no project active and exactly one project in the weave, it updates that project and then activates it, printing `rwv update: activating project '<name>', the only project in this weave (none was active)`; with several, it refuses with `no-active-project` unless `--project` names one.

A member's expected state:

| Workspace and role | Expected state |
|---|---|
| A declared branch, or a `dependency` or `reference` member of a feature | On the branch its entry declares when that branch or its upstream reaches the pinned commit and the branch is not checked out in another workspace; else detached at the pin |
| A feature: an `owned` or `fork` member | On the feature's own branch, created at the pin where it is missing |
| A detached workspace | Detached at the pin |
| No lock entry | On the declared branch as found; where it is found nowhere, left where it is |
| A pin no ref in the clone reaches | Left where it is, reported as `pin` `unreachable` |

A member on its branch behind the pin is fast-forwarded to it; one ahead of it stays where it is. A member detached because its branch is checked out in another workspace carries the fact `checkout`, naming where, and nothing moves that branch. A member on a branch other than the expected one moves off it only when every commit it holds stays on some other ref: its branch is a declared branch or named after a recorded weave branch, or no commit it holds is left on no other branch, remote-tracking branch or backup. A member that moves off another branch is reported as `moved from <branch>`. A member that cannot move is reported on the `alignment` axis with the commits that would be left, and so is one with uncommitted changes or an operation in progress; `update` exits 0 for these and leaves them for the choices the report prints. A member whose branch takes in from a branch of another name refuses (`push-branch` `foreign upstream`).

| Flag | Effect |
|---|---|
| `--role` / `--repo` | Selector filters (see [Selector grammar](#selector-grammar)). A run with a filter does not record the branch the workspace aligned for |
| `-j N` | Parallel per-repo workers (default: `min(nproc, 8)`; `-j 1` for serial execution) |
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `--project <name>` | Update this project instead of the active one (does not change `.rwv-active`). A project the weave root does not present moves none of the members it shares with the one it does, as set out below |
| `--no-materialize` | Skip the install hooks of the activation an update performs when it selects the weave's only project; `.rwv-active` and the weave-root links are still written. With a project already active, an update activates nothing and the flag has no effect |

A member the project and the project the weave root presents both list has one checkout. Given `--project` for a project the weave root does not present, `update` refuses the run, before anything moves, over a shared member that is not aligned for that project: as `alignment`, offering `rwv activate <project> --update`, or as `edits` when the member holds uncommitted changes or an operation in progress. Members only the named project lists align as usual, and so does a shared member already aligned for it. `rwv pull --project` and `rwv update --remote --project` refuse the same way.

`--json` emits the envelope `{ "$schema": "...", "outcomes": [...], "judged": [...] }` once every repo is done, with `outcomes` sorted by repo path, whatever `-j` is; each outcome carries `moved_from` and `facts` where they apply, and `judged` carries the members left where they were. See [JSON envelope convention](#--json-envelope-convention) and `rwv explain update`.

### `rwv update --remote [...]`

**The form that gets the latest.** Advance each member repo to its branch HEAD on the remote, then re-snapshot `rwv.lock`. Maps semantically to `cargo update` / `npm update`. The pruning fetch it runs first is the one `rwv fetch` runs.

The advance is a **fast-forward of the branch the checkout is on**, not a checkout of the tip: on the canonical, that branch must be the local counterpart of the one `branch` declares, and `update --remote` refuses when the checkout is on any other branch, naming `rwv update`. It also refuses when the tip is not a fast-forward — reconcile the branch with its tracking tip yourself (ordinary `git rebase` / `git merge`) and re-run. Inside a workweave the same fast-forward rule applies to the workweave's own branch, and a divergence points at `rwv sync` rather than at a flag. An already-detached member stays detached, unless the repo is stopped mid-rebase / mid-merge / mid-bisect, which refuses.

`advanced N repo(s)` counts repos whose SHA actually changed.

| Flag | Effect |
|---|---|
| `--remote` | Selects this form |
| `--role` / `--repo` | Selector filters |
| `-j N` | Parallel per-repo workers (default: min(nproc, 8)) |
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `--dirty` | Allow update with uncommitted changes in repos when relocking. Needs `--remote` |
| `--commit` | Commit `rwv.lock` together with the integration files regenerated against the new tips, and with `rwv.toml` when it has uncommitted changes. Other uncommitted changes in the project repo refuse (`edits`) before anything moves. Needs `--remote` |
| `-m` / `--message <msg>` | The commit message for `--commit`, in place of the generated summary. Without `--commit` it is a usage error (exit 2) |
| `--no-materialize` | Skip the install hooks of the regeneration `update --remote` runs at primary; `rwv materialize` runs them later. Inside a workweave `update --remote` runs no hooks either way |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`). A selected member it shares with the active project that is not aligned for it refuses the run, as for [`rwv update`](#rwv-update-) |

A repo that fails to advance fails the run: the repos that did advance stay where they moved, the lock is not written, and the run exits 1 with `partial-run-aborted`.

`--json` emits one record per repo with `path`, `absolute_path`, `branch`, `kind` (`updated` / `up-to-date` / `failed`), `old_sha`, `new_sha`, and `error`. See `rwv explain update`.

### `rwv lock`

Checkpoint current local state into `rwv.lock`. No network access.

Reads HEAD from each repo and records its resolved 40-character hex commit SHA in `rwv.lock` (never a tag or branch name, even when a tag points at HEAD). Errors on uncommitted changes to tracked files; untracked files are fine (use `--dirty` to bypass).

| Flag | Effect |
|---|---|
| `--dirty` | Allow locking repos with uncommitted changes to tracked files |
| `--commit` | Commit `rwv.lock` after writing it, together with `rwv.toml` when it is modified or has never been committed. Other uncommitted changes in the project repo refuse (`edits`) |
| `-m` / `--message <msg>` | The commit message for `--commit`, in place of the generated summary. Without `--commit` it is a usage error (exit 2) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

The generated commit message names what the commit holds: `lock: refresh N repo(s)` (followed by the per-repo list) when pins moved, `lock: drop <repo>` (or `drop N repos`) when repos left the manifest, `lock: record rwv.toml` when only `rwv.toml` changed, each joined when several apply, and `lock: no changes` otherwise. `--commit` prints `Committed rwv.lock` or `Committed rwv.lock, rwv.toml`, or `Lock unchanged, nothing to commit.`

Pure git SHA snapshot — no integration hooks fire. To refresh ecosystem lockfiles (`node_modules`, `.venv`, etc.) after membership changes, run `rwv materialize` (or `rwv activate <project>` when switching the active project).

### `rwv activate <project>`

Switch the active project in the primary weave. `rwv activate <project>` is project selection plus materialization: it updates `.rwv-active`, then does what [`rwv materialize`](#rwv-materialize-) does for the project the weave root now presents — surfaces weave-root symlinks for the project's existing integration and static files and runs integration install hooks (`npm install`, `pnpm install`, `uv sync`, `cargo fetch`). It does not rewrite integration workspace files to pick up `rwv.toml` changes: after the hooks it reports a missing or drifted one as a warning naming `rwv doctor --fix`, as `rwv materialize` and `rwv workweave create` do.

| Flag | Effect |
|---|---|
| `--update` | Align the project's members as `rwv update` does, then activate. A shared member with uncommitted changes still refuses, before anything moves; one the alignment leaves where it is refuses the activation afterward |
| `--no-materialize` | Skip integration install hooks for a fast context-switch; `rwv materialize` runs them later |

`rwv activate` moves no checkout. A member the project and the project the weave root presents both list has one checkout, and the incoming project's install hooks run against it, so `rwv activate` refuses, before changing anything, while such a member holds uncommitted changes or an operation in progress (`edits`) or is not where the incoming project's checked-out branch puts it (`alignment`). The `edits` refusal offers committing the changes or setting them aside; the `alignment` refusal offers `rwv activate <project> --update`. A project already presented is never refused over its own members.

An install hook whose tool is not on `PATH` does not fail the verb: it prints ``[warning] <integration>: <tool> is not on PATH, so this integration's install hook did not run; install <tool>, then run `rwv materialize` `` (finding kind `tool-missing`) and the other hooks still run. This holds for every verb that runs install hooks. Every line a verb prints for a side effect it skipped is listed under [Skipped side effects](#skipped-side-effects). A hook whose tool runs and fails is an error: the verb exits 1, and `activate` leaves `.rwv-active` unwritten.

`.rwv-active` is the single source of truth for the active project; CWD does not override.

The install hooks are withheld while a generated file rwv attests holds content it never accepted — see `rwv materialize` below, which is where that is settled.

Refuses if an operation elsewhere holds op-state covering this workspace (`op-in-progress`).

### `rwv materialize [...]`

Reconcile integration-managed workspace files, weave-root symlinks, and install hooks for the project this checkout already presents: the materialization step of `rwv activate`, without the project selection. Valid in both a primary weave (for `.rwv-active`) and a workweave (for `.rwv-workweave`). Takes no project name; at a primary with no active project it refuses and points to `rwv activate <project>`. Refuses if an operation elsewhere holds op-state covering this workspace (`op-in-progress`).

`rwv materialize` runs these steps in order:

| Step | Contract |
|---|---|
| 1. Undeclared links | Only with `--remove-undeclared-links`: unlinks weave-root symlinks at names the project no longer declares, naming each one |
| 2. Disabled-integration cleanup | Strips marked regions from co-owned files and deletes whole files authored by integrations now disabled in `rwv.toml`, along with their weave-root symlinks (`disabled-integration-artifact` in `rwv doctor`, which has no auto-fix arm so deletion stays explicit here). Drops attestations for files no enabled integration generates |
| 3. Surfacing repair | Re-creates missing or broken weave-root symlinks for the project |
| 4. Drift check | If an attested generated file's bytes differ from the accepted generation, refuses (`unaccepted-generated-content`) unless `--regenerate-drifted` or `--adopt-drifted` is passed |
| 5. Host settings | Fits host-dependent settings, such as the cargo `rustc-wrapper` key, to this machine |
| 6. Install hooks | Runs each enabled integration's non-advancing install command (`npm install`, `pnpm install`, `uv sync`, `cargo fetch`) without moving existing dependency pins. A generated lock file whose inputs changed (`derived-state-stale`) is re-derived here, by the tool that writes it |

`rwv materialize` does not rewrite integration workspace files to pick up `rwv.toml` changes: after the install hooks it reports a missing or drifted `package.json`, `go.work` or `Cargo.toml` region as a warning naming `rwv doctor --fix`, which regenerates it.

Every step reads what the project's integrations declare, so an integration that cannot read its own `[integrations.<name>]` block can stop the verb before its first step. For example, an unknown key in a `cargo-workspace` `members` entry makes `rwv materialize` refuse, changing nothing (`materialize: nothing here was changed`), and list the block with the field to correct. Over the same block `rwv member add`, `rwv member remove`, `rwv member set-role`, `rwv member set-recurse-submodules` and `rwv update` refuse and leave `rwv.toml` unchanged, and `rwv activate` withholds its install hooks.

| Flag | Effect |
|---|---|
| `--regenerate-drifted` | Discard the drifted content and regenerate it from the current inputs |
| `--adopt-drifted` | Record the drifted content as the accepted generation |
| `--remove-undeclared-links` | Unlink weave-root symlinks at names the active project no longer declares (the files they point at are untouched) |

Passing both `--regenerate-drifted` and `--adopt-drifted` is refused. The bare run lists every path it would act on, and what `--regenerate-drifted` discards is not recoverable through `rwv`. `rwv activate`, `rwv member add`, `rwv member remove`, `rwv member set-role`, `rwv member set-recurse-submodules`, `rwv update`, `rwv workweave create` and `rwv doctor --fix` run the same hooks and withhold them (naming both flags) while drift stands. `rwv activate --no-materialize` selects a project without running the hooks.

### `rwv init <name-or-source> [--provider <registry>/<owner>] [--adopt]`

Create a new project repo at `projects/<name>/` with an empty `rwv.toml`, configure `rwv.lock merge=rwv-ours` in `.gitattributes` and the `merge.rwv-ours` git driver in the project repo's config (checked by `rwv doctor` as `missing-replay-exclusion` and `missing-merge-driver-config`), author the integration files an empty manifest implies (`<name>.code-workspace` with the default integrations), and activate the project. A new project starts with nothing for `rwv doctor` to report. With `--provider`, adds a remote with the project repo's URL and records it as the project repo's url remote.

When invoked in an **empty directory**, `init` bootstraps that directory as a weave root (no pre-existing `rwv.toml` required) and creates the project inside it. Running in a non-empty directory without an existing workspace refuses.

With `--adopt`, `<name-or-source>` is a URL or `owner/repo` shorthand: `init` clones the project repo from that source instead of `git init`-ing a new one (brownfield adoption of an existing project repo), records the remote the clone made as the project repo's url remote, and runs the adopted project's install hooks as it activates it. It authors no integration file: the adopted project's members are not on disk until `rwv fetch`, and `rwv doctor --fix` regenerates once they are. `--no-materialize` skips those hooks; without `--adopt` it is a usage error (exit 2), since a new project has no members for a hook to act on.

### `rwv member`

Add a repo to the active project, remove one, or change how the manifest lists one. Each subcommand below is an authoring verb: it edits `rwv.toml`, regenerates the integration-managed workspace files, and runs the integration install hooks (at primary; inside a workweave a `member` verb runs no hooks). Each takes `--project <name>` to operate on that project instead of the active one (does not change `.rwv-active`) and `--no-materialize` to skip the install hooks (regeneration still runs, and `rwv materialize` runs the hooks later).

### `rwv member add <url> [...]`

Clone a repo (if not present), register it in the target project's `rwv.toml`, settle write permissions for the role, regenerate integration-managed workspace files, and run integration install hooks. If the repo entry was already added to `rwv.toml` by hand, `rwv member add` converges the on-disk clone, permissions, and integration files to match.

The positional argument has one spelling per meaning:

- **A URL** (`https://…`, `git@…`, `file://` for a local remote) or **registry shorthand** (`github/owner/repo`) clones from that remote.
- **`--adopt <path>`** registers the clone already at `<path>` under the weave root, reading its URL from its remote. A clone with several remotes refuses with `no-remote-url`, since the URL to record is a choice among them: remove the others, or name the URL (`rwv member add <url>`) when the clone sits at that URL's placement. A relative `<path>` resolves against the directory `rwv` runs in (or the `-C`/`-w` directory), symlinks in it are followed, and the member path recorded is the position of the location it leads to under the weave root containing it; from a workweave, a path inside the workweave names the primary's clone at the same position. No clone there (a bare repo counts as none) refuses with `no-clone-to-adopt`; a location under no weave root refuses with `adopt-path-outside-weave`; a workweave path holding a clone that is not a worktree of the primary's refuses with `foreign-clone-in-workweave`.
- **`--new <address>`** creates a repo; see below.
- **Anything else** — a plain path, relative or absolute — refuses with `unresolvable-repo-source`, naming both the `file://` and the `--adopt` spelling. A URL-shaped argument no registry can place (`https://example.com`) refuses with `no-matching-registry`.

| Flag | Effect |
|---|---|
| `--role <role>` | Sets the role (`owned` / `fork` / `dependency` / `reference`). Defaults to `owned`. |
| `--adopt` | Register an existing clone; the positional argument is its path (relative to the invocation directory) under the weave root. Conflicts with `--new` |
| `--new` | Create a new repo instead of cloning; the positional argument becomes a creation address (a bare registry name, or `registry/owner/repo`) rather than a URL or path. See `rwv explain member`. |
| `--no-recurse-submodules` | Record `recurse-submodules = false` for the new member, so new checkouts of it leave its submodules uninitialized. A member the manifest already lists keeps its entry; `rwv member set-recurse-submodules` changes it |
| `--param <name>=<value>` | A creation parameter for `--new` (repeatable). Requires `--new` |
| `--params-json <json>` | The same parameters as one JSON object of string values. Requires `--new` |
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

`rwv member add` writes to CWD's workspace's manifest (the active workspace's `rwv.toml`), not always primary's.

**A clone already at the placement.** When the URL's placement already holds a git clone, `member add` registers it only if one of its remotes has a URL equivalent to the one given (`Directory already exists at '<path>', skipping clone`). A clone of any other remote, or one with no remote, refuses with `foreign-clone-at-placement`, naming `rwv member add --adopt <path>`; nothing is written. Anything at the placement that is not a clone (a directory, empty or not, or a file) refuses with `non-clone-at-placement`, naming it; so does one at the workweave's own placement when `member add` or `member add --adopt` runs from a workweave.

**Canonical path.** Every URL lands at `<registry>/<owner>/<repo>/`. `<registry>` is the matched built-in registry's name (`github`, `gitlab`, `bitbucket`) when the host is one of those; for a host none of them recognize, it's the URL's own host (e.g. `git.corp.example/team/repo/`); for `file://`, which has no host, it's `local`.

**Shared-clone warning.** If the target clone directory is already registered by another project in the same weave, `rwv member add` proceeds (the manifest entry is added to the active project as usual) and emits a warning to stderr naming the other project(s). Sharing a clone across projects is legal — the same repo can be a `dependency` in one project and `owned` in another — but is worth flagging so accidental double-registration is visible.

### `rwv member remove <path> [...]`

Remove the repository from `rwv.toml`, regenerate integration-managed workspace files, and re-run integration install hooks. If the entry was already removed from `rwv.toml` by hand, `rwv member remove` converges the integration files and optional checkout deletion (`--delete`) to match.

| Flag | Effect |
|---|---|
| `--delete` | Also delete this workspace's checkout of the repo: the canonical clone at primary, the workweave's worktree inside a workweave, along with the branch the workweave holds in the canonical clone. Refuses over uncommitted changes or commits nothing else reaches, a [backup](#rwv-backup) holding such commits included (`dropped-repo-has-unique-commits`) |
| `--delete-shared-clone` | Requires `--delete`. At primary, removes the clone even if other projects still reference it. Without it, a clone another project lists refuses (`shared-clone-referenced`); the check runs only at primary, since a workweave's worktree is that workweave's own |
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

### `rwv member set-role <path> <role>`

Change the repo's `role` in `rwv.toml`, regenerate integration-managed workspace files (a `reference` repo leaves every ecosystem's build graph), run integration install hooks, and settle the repo's files: a `reference` repo's tracked regular files lose their write bits; any other role restores owner write permission. If `role` was already edited in `rwv.toml` by hand, `rwv member set-role` converges the on-disk permissions and integration files to match. Refuses a change to `reference` while the repo has uncommitted changes to tracked files.

| Flag | Effect |
|---|---|
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

### `rwv member set-recurse-submodules <path> <true|false>`

Set the repo's `recurse-submodules` in `rwv.toml` and regenerate integration-managed workspace files. The setting says whether a new checkout of the member populates the submodules it records; a checkout that already exists keeps the submodules it has. `true` is the default and is not written out. If the key was already edited in `rwv.toml` by hand, `rwv member set-recurse-submodules` converges the integration files to match.

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
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `-j N` | Parallel per-repo syncs (default: 1) |
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
| `--retire` | Delete the workweave on success. Requires a workweave context; warning + no-op in a primary weave. Refuses, after the landing has completed and with the op parked at the retire phase, while the workweave holds divergent work (`retire-not-converged`), uncommitted changes in any checkout (`edits`), or a checkout replay could not prune (`retire-unpruned-checkout`); resolve it and `--continue`, or `rwv abort` |
| `--allow-stale-lock` | Consent: skip the lock-freshness precondition on both source and destination |
| `--discard-local-commits` | Consent: discard CWD's project commits not reachable from target, hard-resetting the project repo to target's tip. Pre-sync state preserved in `refs/rwv/pre-op/<id>` |
| `--continue` | Resume after resolving a mid-op conflict. All parameters are read from the in-progress op-state file |
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `-j N` | Parallel per-repo step-1 syncs (default: 1) |
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
- **Project repo branch** — `projects/<project>` must be on the branch that the remote it publishes to declares as default, whatever that branch is called. With that remote's `HEAD` unset push refuses rather than guess (`no-remote-default-branch`, naming the `git remote set-head` that records it); a detached or unborn HEAD refuses with `head-not-on-branch`, and any other branch with `project-repo-off-canonical-branch`.
- **Committed project state** — the project repo has no staged or unstaged change to a tracked file, and `rwv.toml` and `rwv.lock` are tracked (`edits`). Push publishes the project repo's last commit, so commit first: `rwv lock --commit` when only those two files are uncommitted. Other untracked files pass.
- **Lock agreement** — every member repo's HEAD must match the committed `rwv.lock` entry (`lock-state-mismatch`).
- **Pushable member branches** — every repo in the plan must be on a branch (`unpushable-repo-branch`).
- **Where each branch publishes** — every branch goes to its push target, found in git's order: the branch's `pushRemote`, `remote.pushDefault`, the remote the branch takes in from, and last the repo's url remote (a member's, the remote that has its `url`; the project repo's, the one it was created with, else its only remote). A branch with none has no push target, and one that takes in from a branch of another name has a foreign upstream; either refuses as `push-branch` (`no push target`, `foreign upstream`) with the choice to configure where it pushes, and so does a branch that has diverged from its push branch while holding commits of its own (`push-branch` `diverged`, the commits listed). Every refusal is reported before anything is pushed. `--force` waives the divergence refusal and the `behind` skip below, as a force-push always has.

Two states complete instead of refusing, and `push` prints each under its repo and leaves the repo unpushed. A branch behind its push branch is `push-branch` `behind`, with the choice to take it in (`rwv update --remote --repo <m>`). A branch whose upstream the remote deleted is `push-branch` `gone`, listing the commits only it holds, and is pushed only when `--repo` or `--role` names the repo.

| Flag | Effect |
|---|---|
| `--role` / `--repo` | Selector filters |
| `--force` | Force-push every repo in the operation (member repos and the project repo). Default deny. The lock-precondition check still runs unconditionally |
| `-j N` | Parallel push (up to N concurrent for member repos; project repo always last and serial). Default: 1, because every push authenticates and runs the repo's pre-push hook: concurrent pushes would contend for one terminal's credential prompts and run those hooks side by side. The default push set is already narrowed to writable roles, so there is less to parallelize than in a fetch |
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `--dry-run` | Print the push plan without executing |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

`--json` emits the envelope `{"$schema": "...", "outcomes": [...], "refused": [...], "reported": [...]}`, `reported` holding the repos judged and left unpushed. Member-repo records use `kind` `pushed`, `skipped`, or `failed`; the project-repo record (always last) uses `kind` `project-repo-pushed` or `project-repo-failed`. When a member's push is rejected, the project repo is withheld: `refused` lists each such member as `publish` `withheld`. See [JSON envelope convention](#--json-envelope-convention).

See [push a cross-repo feature](../how-to/push-cross-repo-feature.md).

### `rwv abort [--abandon-foreign-tip <repo>]`

Roll an in-flight `sync` or `sync-to` back to its savepoints in every workspace the op touched: `refs/rwv/pre-op/<op-id>`, and `refs/rwv/pre-op/<op-id>-target` for the repos of a `sync-to`'s target, whose checkouts share a ref store with the workweave's. Run it from either workspace of the op: the verdict for each repo is the same, and op-state is cleared in both. Runs VCS-native abort for in-progress operations (`git rebase --abort`, `git merge --abort`). Lines for the workspace abort runs in carry no prefix; lines for the op's other workspace are labeled with its side, `[target] <repo>` or `[source] <repo>`. A repo is named as the flag takes it: its workspace-relative path, or `projects/<name>` for the project repo.

| Flag | Effect |
|---|---|
| `--abandon-foreign-tip <repo>` | Consent to restoring this repo over a tip abort cannot attribute to the op. Repeatable, one repo per occurrence; **no all-repos form**. Spell `<repo>` as abort's per-repo output does — the workspace-relative path, or `projects/<name>` for the project repo (the project of the op in progress; another project's repo is refused). The abandoned commits are first kept as a backup marked `abandoned`, which [`rwv backup list`](#rwv-backup-list---json) shows and `rwv backup drop` drops. |

Errors if no sync operation is in progress. Without the flag, a repo whose tip is unattributable is refused, not reset, and the refusal writes nothing. A restore over the op's own tips writes no backup either: what abort throws away is what the op made.

An op record this rwv cannot fully parse is still aborted when its op id, verb, project and workspaces parse. Abort says so, reads none of the tips the record holds, and attributes only a tip at its savepoint or mid-op; every other tip is refused as `foreign-tip`, for `--abandon-foreign-tip <repo>` to settle. A record where even those fields do not parse names no op, and abort refuses it; the error names what removing the file leaves behind and the command that clears each part.

**Exit codes:**
- `0` — all repositories restored cleanly to their savepoints and op-state removed.
- `1` — a refusal (`no-op-recorded` with nothing in progress, or `foreign-tip` without `--abandon-foreign-tip <repo>`), or a partial restore failure (`abort completed with failures`: at least one repository failed to reset, and op-state is retained so `rwv abort` can be retried).

### `rwv backup`

List, drop or prune the backups rwv keeps. A **backup** is a ref an rwv verb makes before any step that would leave a commit or an uncommitted edit unpreserved: `rwv workweave delete --discard-unmerged-commits` and `--discard-uncommitted` make one first, `rwv abort --abandon-foreign-tip` keeps the tip it gives up as one marked `abandoned`, and `rwv doctor --fix` turns a savepoint no operation owns any more into one. A backup is named `<branch>/<id>`, `<branch>` being the branch it backs up (`HEAD` for a detached HEAD), and stays until it is dropped. Each subcommand acts on the repos of the active project, its project repo (`projects/<name>`) and its members, and takes `--project <name>` to act on another project's (does not change `.rwv-active`). `drop` and `prune` refuse while an operation is in progress; `list` does not.

### `rwv backup list [--json]`

List each repo's backups, one line each: the name, the commit it holds, `[abandoned]` or `[rebased]` where the backup says so, and `[redundant]` where a branch or remote-tracking branch reaches everything it holds. Under each, the commits no branch reaches, newest first (20 at most in text; `--json` carries them all). A repo with no backup is left out. Reads refs as last fetched and contacts no remote.

`--json` emits the envelope `{"$schema": "...", "repos": [...]}`. Each repo record has `path` (as `rwv backup drop` names the repo) and `backups`; each backup has `name`, `tip`, `mark` (`rebased` or `abandoned`, absent otherwise), `redundant` and `held` (`sha`, `short`, `subject`). See [JSON envelope convention](#--json-envelope-convention).

### `rwv backup drop <repo> [<name>...]`

Drop the named backups of one repo, or all of that repo's when no name is given, printing each tip as it goes. `<repo>` is the workspace-relative path of a member, or `projects/<name>` for the project repo; no form drops across repos. A name `rwv backup list` does not show is reported and skipped, so a drop interrupted partway is finished by running it again.

Refused under the **pin rule** (`pin` `would be unreachable`) when it would leave a commit some project's live lock pins reached by no branch, remote-tracking branch or backup. A pin is live in every project of the weave, not only the active one: it is the commit a member's pin names in the `rwv.lock` at the tip of any local or remote-tracking branch of any project's repo. The refusal names the commits and the locks, and nothing is dropped. Dropping a backup that holds commits nothing else reaches loses them; the drop prints what it dropped, and that is the whole record.

### `rwv backup prune`

Fetch every remote of every repo of the project with pruning, then drop each backup a branch or remote-tracking branch reaches in full (`[redundant]` in the list). A remote that cannot be fetched is named and its remote-tracking branches are not counted, so a backup is dropped only on what a confirmed fetch or a local branch vouches for. Obeys the pin rule as `drop` does. Prints `nothing to prune` when no backup is redundant. A backup holding commits no branch reaches is never pruned; only `drop` removes it.

### `rwv branch`

Record how a weave branch is used, or list the weave branches with their records. A weave branch is a branch of the project repo that carries `rwv.toml` and `rwv.lock`. Its **record** is `feature` (work onto a target branch, with `as` the target's own mode when the record was written) or `declared` (members on the branches its `rwv.toml` entries declare). Records are kept on a branch of the project repo of their own, written without a checkout, and every verb that aligns members, locks, lands or publishes (`update`, `pull`, `update --remote`, `lock`, `sync`, `sync-to`, `push`) refuses a checked-out branch that has none, naming `rwv fetch` and `rwv branch set-mode`. `rwv init`, `rwv clone` and `rwv workweave create` record the branch they leave checked out. See [Formats](./formats.md#branch-records).

### `rwv branch set-mode [<W>] <feature|declared> [--target <T>] [--project <name>]`

Write the record of `<W>`, the project repo's checked-out branch when none is named, and nothing else: no branch moves and no checkout changes. `<W>` must exist locally or on a remote as last fetched (`name` `no-such-branch` otherwise). The mode is always the last word, so a branch named `declared` is still a branch when two words are given.

| Mode | Takes |
|---|---|
| `feature` | `--target <T>`, required: the branch the feature lands into, which must have a record that reads as one value. Its mode is stored as `as`. A `--target` whose chain of targets returns to `<W>`, `<W>` itself included, is refused (`target` `cycle`) |
| `declared` | no `--target` |

A `--target` with no record is refused as `target` `unrecorded`, naming `rwv branch set-mode <T>`. Recording a feature prints where it lands (`target-recorded`): into `<T>`'s member branches when `<T>` is a feature, else into each branch `<T>` declares, listing the members whose entry in `<W>` differs from `<T>`'s. A change to `<W>`'s mode lists each feature that targets `<W>` and recorded its mode differently, which refuses to land until rebound. A branch whose entry two writers set differently (`target` `conflict`) or whose entry does not read (`target` `unreadable`) is settled by this verb, which writes the one value. The records branch's own name and every name below it is refused as `name` `reserved`.

### `rwv branch list [--json] [--project <name>]`

List each weave branch, local or only on a remote as last fetched, with its record: `declared`, `feature onto <T> (as <mode>)`, `unrecorded`, `conflict`, `unreadable` or `cycle`. `*` marks the checked-out branch; `[unpublished]` marks an entry no remote's records, as last fetched, hold; `[target gone]` a target that exists nowhere; `[remote only]` a branch with no local copy. It reads refs as last fetched and writes nothing: the records of the remotes are merged in memory, not into the local records branch.

`--json` emits the envelope `{"$schema": "...", "branches": [...]}`. Each branch has `name`, `checked_out`, `local`, `record` (an object whose `state` is `declared`, `feature` with `target_branch` and `as`, `unrecorded`, `conflict` with `values`, `unreadable` with `reason`, or `cycle` with `chain`), `published` (absent for an unrecorded branch) and `target_gone`. See [JSON envelope convention](#--json-envelope-convention).

### `rwv status [--json] [...]`

Show per-repo state for the CWD workspace: each repo's branch, tip and lock relation, and below the table what stands apart on the axes `status` reads.

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
| mid-op | `mid-rebase` / `mid-merge` / `mid-cherry-pick` / `mid-bisect` when the repo is stopped in that operation; absent otherwise |

`--json` emits the envelope `{"$schema": "...", "repos": [...]}`. Each record includes `lock_pinned_by_name` (`true` when an older or hand-edited `rwv.lock` entry holds a tag or branch name rather than a 40-character hex commit SHA; `rwv lock` rewrites it to a commit SHA). See [JSON envelope convention](#--json-envelope-convention).

Below the table, `status` prints:

- the branch each project's workspace last aligned its members for (`<project> aligned for <branch>`, `aligned detached`, or `aligned for no branch`);
- one `alignment` line for each member that is not in its expected state, with the commits that a move off its branch would leave, and one for a member carrying the fact `checkout` (its branch is checked out in another workspace, so it sits detached at its pin); for the project repo, `project moved`, `project unaligned` or `project detached` against the aligned branch;
- one `push-branch` line for each member whose checked-out branch needs attention: `behind` (take it in with `rwv update --remote --repo <m>`), `diverged`, `gone`, `foreign upstream` and `no push target`. A branch that is `ahead` of its push branch is the ordinary state of work in progress and has no line. A `gone` line appears only while the branch holds commits no remote-tracking branch reaches, and lists them: a branch whose upstream was deleted after its work landed is not marked;
- one `operator branch` line for each local branch of a member that holds unpushed commits nothing else accounts for: it is checked out nowhere, is named after no recorded weave branch of any project here, and is no declared branch of that member. The line gives the member, the branch and its count of unpushed commits, and notes a gone upstream; it prints no choice, because such a branch is legal;
- the backups each repo holds (`backup <repo> <name> <tip>`), the listing `rwv backup list` gives.

The lines about a member follow the selector. The lines about the project (`aligned for`, `project moved`, `project unaligned`, `project detached`) print only when no `--role` or `--repo` is given. It reads refs as last fetched, so `rwv fetch` first. `--json` carries the same as `aligned`, `judged` (each line with its `axis`, `state`, `facts` and `choices`; empty when none stands apart), `operator_branches` (with the unpushed `commits`) and `backups`.

A `missing` relation names a clone directory that has vanished; the repair is [`rwv update`](#rwv-update-), which clones the member again, run from the weave root (see `rwv doctor` for the paired detection: `missing-canonical-clone`, `dangling-reference`). An `unreachable` relation names a clone that is present but whose object store lacks the locked SHA; `rwv update` does not repair it (with the directory present it performs a local checkout without a network fetch). If the remote still has the revision, [`rwv fetch`](#rwv-fetch-) or `git fetch` in the affected repo re-pulls it; if the remote's history was rewritten, re-lock to a reachable state — see [reconcile repos](../how-to/reconcile-repos.md).

### `rwv log [<range>] [--against <W>] [--diff] [--json] [--project <name>]`

Show the VCS's log in each repo of the project the invocation resolves to: every member, then the project repo (`projects/<project>`). It runs in a primary weave or a workweave alike; `rwv -w <project>--<name> log ...` addresses a workweave from elsewhere. Nothing is written.

| Flag | Effect |
|---|---|
| `<range>` | A range in the VCS's own syntax (for git, `main..feat-x`, `feat-x..`, `v1.2...main`), read in every repo. Each endpoint resolves in each repo as the VCS would resolve it. Refused together with `--against` |
| `--against <W>` | A weave branch's pins have their own spelling. Per member, list the commits HEAD reaches that `<W>`'s pin for that member does not; in the project repo, those `<W>` lacks. `<W>` is a branch of the project repo, local or remote-tracking as last fetched, whose tip carries `rwv.toml`. A member `<W>` does not pin is listed as `not in <W>`. Run it in a feature to list what the feature has not landed on its target |
| `--diff` | With `--against` only: show each repo's diff from the common ancestor of its pin and HEAD to HEAD, in place of the commit listing. Anchored at the common ancestor, so commits the pin gained after HEAD forked are not shown as reversals |
| `--json` | Emit the envelope below |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

With neither `<range>` nor `--against`, each repo lists the VCS's log of its HEAD; there is no default weave branch, so a bare `rwv log` never reads pins.

A repo where a `<range>` endpoint names nothing prints `name no-such-branch` naming the endpoint in its own section, the other repos are still listed, and the exit is non-zero; the refusal that follows on stderr prints the choices and the `rwv explain name` route. An `--against` whose `<W>` is no weave branch refuses `name no-such-branch` before listing anything. A repo that could not be read in full for another reason (a missing checkout, a pin the checkout does not hold) is listed with a `note` and leaves the exit at `0`.

Text output is one `=== <path> ===` section per member followed by `=== (project) ===`, each listing `<12-character sha> <subject>`. `--json` emits `{"$schema": "...", "range"?, "against"?, "diff", "repos": [...]}`. Each element of `repos` carries `path`, `head`, and `commits[]` (each `{ "sha", "short", "subject" }`); with `--against`, `pin` (the commit `<W>` pins for the repo, `<W>`'s own tip for the project repo, absent for a member it does not pin); with a range, `endpoints[]` (each `{ "spelled", "id" }`, the commit each endpoint names in that repo); under `--diff`, `diff_base` and `diff`; `note` when the repo could not be fully read; and `refused[]` (the records [below](#refused-repos-under---json)) in a repo where an endpoint named nothing. See [JSON envelope convention](#--json-envelope-convention).

### `rwv doctor [...]`

Convention and health audit across clones, `rwv.toml`, `rwv.lock`, workweaves, op-state leases, git attributes/merge drivers, and enabled integrations.

| Flag | Effect |
|---|---|
| `--locked` | Zero exit iff every repo tip matches its lock entry (precondition for `rwv sync`) |
| `--fix` | Repair every finding marked **Auto-fixable** in [Doctor findings](./doctor-findings.md), which carries that mark on each finding it documents. Never touches live staged content or live edits. Idempotent. |
| `--json` | Emits envelope `{"$schema": "...", "violations": [...], "issues": [...], "advisories": [...], "plugins": [...]}`: rwv's own findings on `violations`, integration findings on `issues` |
| `--all` | Scan all projects and run weave-wide checks (orphan detection, cross-project stale locks, etc.). By default only the active project is checked |
| `--abandon-lost-pins` | With `--fix`, relink a workweave checkout whose link dangles and whose pinned revision and branch are in no clone onto the workweave's branch created at the canonical clone's HEAD, keeping every file: the pinned commits are given up, and their changes stand as edits. Without this flag, `--fix` reports the checkout |
| `--kind <kind>` | Filter the report to the named finding kind (repeatable; text and `--json` both). Kind names are the wire spellings `--json` emits, for `violations` and `issues` alike (e.g. `branch-discipline`, `orphaned-savepoint`, `tool-missing`); a filter naming only `violations` kinds excludes every integration issue. An unknown name refuses (`unknown-finding-kind`), listing the valid set. Kinds whose classes normally collapse to a per-class count line render itemized under the filter — this is the drill-down. The exit code reflects the filtered view, text and `--json` alike: non-zero on any finding in the view, warnings included, so `rwv doctor --kind tool-missing` (or `--json --kind tool-missing`) fails a CI job on a missing tool |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

Without `--kind`, the exit code is non-zero when any finding is an error, and a warning leaves it at `0` — text and `--json` alike. `--fix` exits non-zero when a repair it attempted failed or refused, or an error finding remains.

See [Doctor findings](./doctor-findings.md) for the complete catalog of wire `--kind` / `sub_kind` tokens, their severities (`error` vs `warning`), and their `--fix` dispositions.

### `rwv workweave create <name>`

Create an isolated workweave for the project the invocation resolves to (see [Project resolution](#project-resolution)); `--project <name>` names another:

1. **Manifest worktrees** — creates a git worktree on an ephemeral `<project>--<name>` branch for every member repo (`owned`, `fork`, `dependency`, and `reference`), with `reference` repos' tracked regular files set read-only (`chmod -w`, preserving symlinks).
2. **Project worktree & marker** — creates the project worktree at `projects/<project>/` on `<project>--<name>` and writes `.rwv-workweave` at the workweave root.
3. **Activation** — surfaces workweave-root symlinks, fits host settings such as the cargo `rustc-wrapper` key, runs the integration install hooks (`npm install`, `uv sync`, `cargo fetch`, …) in the new workweave, then verifies the integration files in `projects/<project>/`: a missing or drifted one is reported as a warning naming `rwv doctor --fix`, and `create` authors none. `--no-materialize` skips the hooks; `rwv materialize` inside the workweave runs them later.

A failure before the install hooks rolls the workweave back. An install hook that fails after the workweave is created and registered does not: the workweave is kept, and `create` exits 1 with `workweave created at <path>; only its install hooks did not complete. Fix the cause, then run rwv materialize inside it`. Under `--hook-mode` and `--claude-hook` the same failure is a warning on stderr and `create` exits 0, so the agent runtime that asked for the workweave still gets its path. A missing tool is a `tool-missing` warning in every mode.

| Flag | Effect |
|---|---|
| `--from <source>` | Fork from a specific source (default: CWD's active workspace). `<source>` reads as `rwv sync <source>` reads it: `primary`, a bare workweave name of this project, or a path (absolute, or relative to the invocation directory). A directory with a bare name is written `./<dir>`; a bare word that names no workweave refuses with `no-such-workweave`. Forking from an existing workweave is how you **duplicate** one; copying a workweave with `cp` aliases the original's git state rather than duplicating it |
| `--replace-existing` | Destroy an existing workweave at this path before recreating. Without this flag, re-invoking `create` against an existing workweave is the idempotent path. Refuses if the existing workweave has uncommitted or unmerged work |
| `--capture-dirty` | Allow creation when the source project directory has uncommitted changes. Uncommitted edits to `rwv.toml` and `rwv.lock` are **moved**: copied into the new workweave's project worktree, then returned to their committed version in the source, so a later `rwv sync-to` from the workweave is not refused over the source's copy. Other uncommitted files stay in the source only, and `create` lists them. Without this flag, `create` refuses (`dirty-project-dir`) |
| `--dir <path>` | The exact path to place the workweave at; there is no default (recorded in the index as the absolute path it resolves to). Absolute paths are used as-is; relative paths resolve against the invocation directory. Refused together with `--basedir` |
| `--basedir <path>` | The directory to place the workweave under for this invocation, in place of the recorded default: the workweave lands at `<path>/<project>--<name>`. Relative paths resolve against the invocation directory. Refused together with `--dir` |
| `--project <name>` | Create the workweave for this project instead of the active project (does not change `.rwv-active`) |
| `--no-materialize` | Skip the install hooks in the new workweave; `rwv materialize` inside it runs them later |

Workweaves live at `<basedir>/<project>--<name>/` where `<basedir>` is recorded per-project in `projects/<project>/.rwv-workweave-index` (machine-local JSON, excluded from git through the project repo's `.git/info/exclude`, never `.gitignore`). The default basedir is `<parent-of-primary>/.workweaves`. Set it explicitly with `rwv workweave set-default-create-basedir <path>`; `create` records new entries into the index; every `find`-direction verb (list, delete, sync targets by bare name) resolves via the recorded `name → absolute path` entries with `.rwv-workweave` marker round-trip validation. Doctor reconciles the index against on-disk state — stale entries are pruned, orphan workweaves are adopted, a tracked index is flagged as a hygiene finding.

### `rwv workweave set-default-create-basedir <path>`

Record the default basedir for the project: the directory `rwv workweave create` places new workweaves under. Writes the `container` field of `projects/<project>/.rwv-workweave-index`. Absolute paths are used as-is; relative paths resolve against the invocation directory. Existing registry entries are preserved. `--dir` and `--basedir` on `create` are unaffected. An explicit, recorded, audit-visible act, not ambient process state. `--project <name>` records it for that project.

### `rwv workweave delete <name> [--discard-uncommitted] [--discard-unmerged-commits]`

Delete a workweave. Default refuses if any checkout under it is dirty (`edits`), or holds commits contained in neither the workweave's recorded parent nor the primary weave (`unmerged-commits`; work in a nested workweave counts as merged once its parent has it). `--discard-uncommitted` waives the first refusal, `--discard-unmerged-commits` the second. The `edits` refusal prints `rwv workweave delete <name> --discard-uncommitted` among its choices. `--project <name>` selects the project whose workweave it deletes.

Neither flag discards for good: each keeps what it consents to losing as a [backup](#rwv-backup) before anything is removed, and a backup that cannot be written stops the delete with nothing changed. `--discard-uncommitted` keeps each checkout's uncommitted edits as a commit on its HEAD (untracked files included, ignored ones not). `--discard-unmerged-commits` keeps each workweave branch it deletes at its tip, and the commits a detached HEAD holds that no branch reaches. The delete prints each backup it made; `rwv backup list` shows them afterwards.

Both checks cover every checkout under the workweave, not only the members the project's manifest names: a member replay could not prune, one the workweave added that has not landed, or a clone put there by hand. Such a checkout is listed with the suffix `(not a manifest member)`, and one whose commits no baseline holds at all is reported rather than skipped. `create --replace-existing` weighs the same set.

### `rwv workweave list`

List workweaves for a project. `--project <name>` names the project.

### `rwv workweave [--hook-mode] [--claude-hook]`

Two flags on `workweave` itself, preceding the subcommand, for driving workweave creation and teardown from Claude Code's own hook events instead of the shell.

| Flag | Effect |
|---|---|
| `--hook-mode` | With `create`, print only the new workweave's path to stdout instead of the usual create output; everything else, install-tool output included, goes to stderr. An install hook that fails in the created workweave is a warning, and `create` exits 0 |
| `--claude-hook` | Read a Claude Code hook payload as JSON from stdin and dispatch on it directly, bypassing the subcommands entirely. `hook_event_name: "WorktreeCreate"` runs a bare `create` — project inferred from the hook's `cwd` (the current workweave's project, or the primary weave's active project), name derived from `branch_name` (falling back to a timestamp) — and prints its path to stdout; an install hook that fails there is a warning, as under `--hook-mode`. `"WorktreeRemove"` deletes the workweave named by `worktree_path`; fire-and-forget, warnings go to stderr and it always exits `0`. Conflicts with `--hook-mode` |

`rwv setup claude` registers `rwv workweave --claude-hook` for both the `WorktreeCreate` and `WorktreeRemove` Claude Code hook events; `--hook-mode` is for a hook command you write yourself.

### `rwv setup claude [--uninstall]`

Register Claude Code hooks in `~/.claude/settings.json` (`SessionStart` and `PreCompact` running `rwv prime`; `WorktreeCreate` and `WorktreeRemove` running `rwv workweave --claude-hook`). Requires `~/.claude/settings.json` to exist already (refuses if absent). `--uninstall` removes all `rwv` hooks from `~/.claude/settings.json`.

### `rwv completions <shell>`

Generate shell completions (bash, zsh, fish, etc.). Source the output in your shell rc file.

## Exit status

| Status | Meaning |
|---|---|
| `0` | The verb completed everything it set out to do. |
| `1` | The verb stopped: a refusal, a failed repo, a conflict parked for `--continue` or `rwv abort`. |
| `2` | The arguments did not parse. |
| `3` | The verb completed and left part of the work to you. [`rwv clone`](#rwv-clone-source-) ends so when a member is left with no pin or at a pin no ref reaches; its report and `--json` envelope list each member. |

A status of `3` is a finished run, not a failure: everything the verb could do was done, and the output names what remains.

## Skipped side effects

A verb that leaves out a side effect it would otherwise perform says so on
stderr, in a line naming the verb that performs it. A run that skipped
nothing prints none of these lines, and none of them changes the exit code.

| When | Line |
|---|---|
| `--no-materialize` on `activate`, `member add`, `member remove`, `member set-role`, `update`, `pull`, `workweave create`, `clone` or `init --adopt` | ``[skipped] core: the install hooks did not run (--no-materialize); `rwv materialize` runs them`` |
| `member add`, `member remove`, `member set-role` or `update --remote` inside a workweave, which never run hooks there | ``[skipped] core: the install hooks did not run (an authoring verb runs none inside a workweave); `rwv materialize` runs them`` |
| `member add`, `member remove`, `member set-role` or `update --remote` with `--project <name>` naming a project the weave root does not present: the files are regenerated and the verb stops there | ``[skipped] core: the weave-root links were not refreshed (`<name>` is not the project this weave root presents); `rwv activate <name>` refreshes them`` and ``[skipped] core: the install hooks did not run (`<name>` is not the project this weave root presents); `rwv activate <name>` runs them`` |
| `update --remote` with `--role` or `--repo`, which regenerates nothing and runs no hooks | ``[skipped] core: the integration files were not regenerated (the update was narrowed by --role or --repo); `rwv doctor --fix` regenerates them`` and ``[skipped] core: the install hooks did not run (the update was narrowed by --role or --repo); `rwv materialize` runs them`` |
| `clone <source>` while another project is active (on stdout in text mode, stderr under `--json`) | ``rwv clone: skipping auto-activate (project '<active>' already active); `rwv activate <cloned>` selects it and runs its install hooks`` |
| `update`, its project already active, when it cloned a missing member or realigned one to its pin | ``[skipped] core: the install hooks did not run (project `<project>` was already active, so this update activated nothing); `rwv materialize` runs them`` (with `--project` naming a project the root does not present, the `rwv activate <project>` form above) |
| An install hook whose tool is not on `PATH`, on every verb that runs hooks | ``[warning] <integration>: <tool> is not on PATH, so this integration's install hook did not run; install <tool>, then run `rwv materialize` `` |

Under `--json` in serial mode (`-j 1`), the `rwv clone`, `rwv update` and
`rwv update --remote` envelopes, and `rwv pull`'s, carry a `skipped` array, present and empty when nothing was
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
| `rwv explain <token>` | The entry for a refusal token, a `rwv doctor` finding kind or sub-kind, or a VCS error kind, as [Refusals](./refusals.md), [Doctor findings](./doctor-findings.md) and [VCS errors](./vcs-errors.md) print it. An unknown name refuses (`no-explain-entry`), suggesting a near match. A sub-kind that more than one finding kind carries refuses (`ambiguous-explain-token`), naming the `<kind>/<sub-kind>` spelling for each |
| `rwv explain <kind>/<sub-kind>` | The entry for that kind's sub-kind, or the kind's entry where one entry covers all its sub-kinds |

Every page it serves opens with a `rwv <version>` line — the same string `rwv --version` prints — and a blank line, so a saved copy says which build it describes.

Use case: agent harness asks "what flags does `rwv push` take, and what does it print?" — the bundle is authoritative. For `--json`-capable verbs (`status`, `doctor`, `clone`, `fetch`, `pull`, `update`, `sync`, `sync-to`, `push`), the JSON Schema is embedded as a fenced code block inside the bundle.

The rendered bundles are checked in at `docs/reference/explain/` for offline browsing.

## Project resolution

Project-scoped verbs (`member add`, `member remove`, `member set-role`, `member set-recurse-submodules`, `workweave create`, `workweave delete`, `workweave list`, `workweave set-default-create-basedir`, `lock`, `update`, `pull`, `fetch`, `push`, `sync`, `sync-to`, `status`, `log`, `doctor`) pick their target project via a fixed chain, highest priority first:

1. `--project <name>` — the explicit override on the invocation.
2. `-w/--workweave <project>--<name>` — the `<project>--` prefix of the global workweave-selector flag names the project.
3. The weave root's own identity file — **one tier**, whose spelling follows the kind of root CWD resolved into:
   - `.rwv-workweave` marker, when CWD resolves inside a workweave. The marker names the project structurally.
   - `.rwv-active` pointer at a primary root — the file `rwv activate` maintains.

Step 3 is one tier rather than two ranked ones because the two files are **mutually exclusive**: a primary root carries the pointer, a workweave root carries the marker, never both. No invocation ever sees both answers, so there is no precedence between them to get wrong. `rwv doctor` enforces the exclusivity, reporting a root carrying both as a `weave-root-identity-conflict`.

The pointer is total at primary by construction: every path that creates a project leaves one active. `rwv init` activates the project it creates, and `rwv clone <source>` activates the project it clones when none is active. A missing pointer therefore arises from hand surgery, and is repaired without a guess only where nothing is left to guess: `rwv update` in a weave holding exactly one project activates it. Anywhere else a missing or stale pointer produces a corrective error, not silent structure inference. `rwv workweave create <name>` writes only the marker: a workweave's project is fixed at creation and cannot be switched, so there is no selection for a pointer to record.

### Target line — visibility when the pointer decides

When resolution falls through to step 3, the verb prints a target line to **stderr** before acting:

```
target: workspace /home/cwa/weaveroot/foundations · project tmuxcc (.rwv-active)
```

Explicitly (`--project` or `-w`) or structurally (workweave marker) resolved invocations stay silent — the operator already named the target, or the workweave did.

Stderr, not stdout, so the line never contaminates a `--json` verb's output. The line is prose, not a parse surface — no schema, no version, no consumer contract. The `resolution` block in `--json` output carries the resolved coordinate as machine data; provenance stays out of the JSON so agents assert on results, not on how they were reached.

### CWD inside another project

Changing into `projects/<name>/` does not select that project. When `.rwv-active` selects a different one and no `--project` is given, a verb that writes into its project — `member add`, `member remove`, `member set-role`, `member set-recurse-submodules`, `workweave create`, `workweave delete`, `workweave list`, `workweave set-default-create-basedir`, `lock`, `update`, `pull`, `fetch`, `push`, `sync`, `sync-to`, `doctor --fix` — refuses (`cwd-in-inactive-project`) rather than write into the active project from the other's directory:

```
Error: CWD is inside projects/mobile/, but `.rwv-active` selects web, so this would act on web. Pass `--project mobile` to act on mobile, or `--project web` to act on web from here; `rwv activate mobile` switches the active project.
```

Reporting verbs do not refuse: `rwv status` and `rwv log` warn, naming `--project <name>` and `rwv activate <name>`, and report on the active project, and `rwv doctor` without `--fix` reports on the active project.

### Corrective errors

Each of these is a refusal and carries a token; the message ends with `rwv
explain <token>`, which serves that token's entry. [Refusals](./refusals.md)
lists every token, and a condition `rwv doctor` also reports keeps its entry on
[Doctor findings](./doctor-findings.md). A refusal that judged repo states
instead prints `rwv <verb>: <repo>: <kind> <state>` for each, its choices,
and one `rwv explain <kind>` per kind; the same page lists every kind, its
states and the choices each prints.

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

- **Unreadable pointer** (`unreadable-active-project`) — `.rwv-active` is empty, cannot be read, or holds text that is not a project name:

  ```
  Error: /work/weave/.rwv-active names no project rwv can select (the file is empty). Run `rwv activate <project>` to select a project, or `rwv doctor --fix` to remove the file.
  ```

  The refusal is not read as "no project selected". `rwv doctor` reports the same condition under that token and, under `--fix`, removes the file; `rwv activate <project>` replaces it.

### Which verbs are project-scoped

| Category | Verbs | Resolution behavior |
|---|---|---|
| Project-scoped | `member add`, `member remove`, `member set-role`, `member set-recurse-submodules`, `workweave create`, `workweave delete`, `workweave list`, `workweave set-default-create-basedir`, `lock`, `update`, `pull`, `fetch`, `push`, `sync`, `sync-to`, `status`, `log`, `doctor` (project-scoped checks) | Full chain; target line when the pointer decides |
| Project named positionally | `activate <project>` | Explicit already; no chain consulted |
| Workspace-scoped, no project | `init`, `abort`, `resolve`, `prime`, `explain`, `doctor` (cross-project scan) | No project selection involved |

## Selector grammar

`rwv fetch`, `rwv pull`, `rwv update`, `rwv push`, and `rwv status` share a selector surface for picking which repos to operate on or report. The [`rwv each`](./rwv-each.md) plugin takes the same flags and hands them to `rwv status`.

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
| `rwv log --json` | `repos` |
| `rwv doctor --json` | `violations` (and `issues` for integration findings) |
| `rwv clone --json` | `outcomes` (and `left`) |
| `rwv fetch --json` | `repos` |
| `rwv pull --json` | `outcomes` (and `fetched`, `project`) |
| `rwv update --json` | `outcomes` |
| `rwv update --remote --json` | `repos` |
| `rwv sync --json` | `outcomes` |
| `rwv sync-to --json` | `outcomes` |
| `rwv push --json` | `outcomes` |

The `sync` and `sync-to` envelopes also carry `complete`: `true` when the op ran its last phase and cleared op-state, `false` when a phase parked it.

### Refused repos under `--json`

A refusal that judged repo states carries them as records, one per repo and axis:

```json
{ "path": "github/acme/lib", "axis": "edits", "state": "dirty", "listed": ["src/lib.rs"], "facts": [], "choices": [ { "id": "C13", "phrase": "commit your edits, or set them aside", "where": null, "run": ["git -C /work/github/acme/lib commit", "git -C /work/github/acme/lib stash push"] } ] }
```

- `axis` is the kind `rwv explain <axis>` serves; `state` is named as that page names it, and `choices` are the ones it lists for the state, in its order, with the commands spelled out under `run`. `where` is the directory a choice runs in when that is not where the verb ran, else `null`. `path` is the repo's manifest path, `projects/<name>` for the project repo; `workspace`, present only when the repo was judged in another workspace than the verb's (a landing's target), is that workspace; `subject`, absent when the state names nothing, is what it names (the operation in progress, the name taken).
- `rwv push --json` and `rwv sync`'s and `rwv sync-to`'s envelopes carry them in `refused`, empty when nothing was refused on a repo's state; a `prune-refused` member record carries the checkout's in its own `refused`.
- A verb that refused on repo states before printing anything prints the refusal envelope instead, `{ "$schema": "<url>", "refused": [...] }`, schema at `docs/reference/schemas/refusal.json`.

Schemas live at `docs/reference/schemas/<verb>.json` and are also embedded as fenced code blocks inside the corresponding `rwv explain <verb>` bundle.

### The envelope and `-j`

`-j` changes how a verb runs, never what `--json` prints: the envelope is the same under `-j 1` and `-j N`, printed once the verb is done, with its per-repo records sorted by repo path (`push`'s project-repo record last). Nothing streams on stdout. For per-repo progress as each repo finishes, use [`rwv each`](./rwv-each.md)'s `--joblog` and `--results`.

Exit semantics under `--json` match the text mode: non-zero when at least one per-repo outcome is `failed`, when a refusal stopped the verb, and for `sync` and `sync-to` when a checkout replay could not prune is left on disk. The envelope still prints in that last case. A refusal's text always goes to stderr, under `--json` too.

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
