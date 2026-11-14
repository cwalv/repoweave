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
rwv -w myproj--hotfix land --retire   # from anywhere inside the ecosystem
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

## The git it needs

Every verb that reads or writes a repository first asks the `git` on `PATH`
for its version, once, before doing anything else. Below git 2.36 it refuses
with [`unsupported-vcs-tool`](./refusals.md#unsupported-vcs-tool), naming the
release it found; with no `git` it can start, it refuses with
[`missing-vcs-tool`](./refusals.md#missing-vcs-tool). `rwv explain`, `rwv
completions`, `rwv setup claude`, `--help`, `--version` and an external verb
(a plugin, which rwv starts without reading a repository) run without git. A command newer than 2.36 is used where the installed git
has it and replaced by an older way to do the same work where it does not.

## Verbs

### `rwv` (bare)

Show current context: weave root, active project, workweave (if any), repos.

### `rwv clone <source> [...]`

Clone a project and each of its members at the commit its `rwv.lock` pins.

`<source>` is a URL (`https://…`, `git@…`, `file://` for a local remote) or a shorthand naming its registry, `<registry>/<owner>/<repo>` (`github/chatly/web-app`). `clone` clones the project repo from `<source>` into the current directory, defines the lock merge driver in its config (config does not travel with a clone), reads its committed `rwv.lock`, clones every listed member repo to its canonical slot, and activates the project when none is active (`.rwv-active`, weave-root links, install hooks). A plain path, or a two-segment `<owner>/<repo>` that names no registry, refuses with `unresolvable-repo-source`, naming the three-segment shorthand with the built-in registries and the `file://` spelling. It writes no lock.

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

A repo's fetch holds the claim on its object store, so fetches into one store run one at a time and parallel seats wait on each other instead of failing on a ref lock. A remote-tracking branch that moved to a tip not descending from the old one is `rewritten`, and its old tip is kept as a backup marked `superseded`, which `rwv backup list` shows and `rwv backup drop` drops. A branch the remote deleted is `pruned`, which supersedes nothing. A member not on disk is reported `skipped`; `rwv update` clones it. A repo whose remote could not be completed is `unconfirmed`, the cause named, and the run exits 1 with `partial-run-aborted` after trying every repo.

A member with a remote also reports each commit that some project's live lock pins, on any local or remote-tracking branch of the project repo, that no remote-tracking branch of the member reaches after the fetch: `pin` `off remote`, listing the locks that pin it. Choices print only in a workspace whose checked-out branch's lock holds the pin: publishing it for a feature's, pinning another commit for a declared branch's. A pin held elsewhere is reported with the choice to wait. The report changes nothing and does not change the exit status.

| Flag | Effect |
|---|---|
| `--no-reference` | Skip repositories with `role = "reference"` |
| `--role` / `--repo` | Selector filters (see [Selector grammar](#selector-grammar)). A selector narrows the fetch to the members it names and leaves the project repo out |
| `-j N` | Parallel per-repo workers (default: `min(nproc, 8)`; `-j 1` for serial execution) |
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `--project <name>` | Fetch this project instead of the active one (does not change `.rwv-active`) |

`--json` emits the envelope `{ "$schema": "...", "repos": [...] }` once every repo is done, with `repos` listing the project repo first and then the members by path, each with `status` (`fetched` / `unconfirmed` / `skipped`), `moved` and `pins_off_remote`. See `rwv explain fetch`.

### `rwv pull [...]`

Fetch, fast-forward the project branch to its upstream, then update the members to the lock it holds. Three steps, in order:

1. The pruning fetch of every repo, as [`rwv fetch`](#rwv-fetch-). A repo it could not complete refuses the pull before anything is taken in.
2. The project branch fast-forwards when it is behind its upstream, and reports what the commits it took in changed: `re-pinned` for a member whose pin the pulled lock moved to a commit that does not descend from the old one, and `member-changed` for each member entry the pulled `rwv.toml` added, removed or changed. A branch that is at its upstream, ahead of it, or has none has nothing to take in. One that has diverged refuses as `push-branch` `diverged`, with nothing changed, unless `--merge-upstream` merges it or `--reset-to-upstream projects/<name>` moves it to its upstream. Before the branch moves, an uncommitted change to a file the fast-forward would overwrite refuses as `edits` `dirty`, listing the files, and an uncommitted `rwv.toml` or `rwv.lock` refuses as `edits` `rwv.toml or rwv.lock uncommitted`; other uncommitted changes stay as they are, named in one line.
3. [`rwv update`](#rwv-update-), which `--role` and `--repo` narrow.

| Flag | Effect |
|---|---|
| `--role` / `--repo` | Selector filters; they narrow step 3 only |
| `--merge-upstream` | Merge a diverged project branch with its upstream, first parent the upstream. Each member whose pin both sides changed under one declared branch first merges the other side's pin into its branch (a branch no checkout has attached merges as refs); the merged `rwv.toml` decides the member set, a member it drops is pruned once every member has merged, and one it adds is cloned. The project commit is written last and records each merged member, and for every other member the pin the side that changed it holds (three-way against the merge base: both sides changing it alike keeps that pin; both changing it under one declared branch keeps the pin containing the other, and diverged pins print `pin` `unmerged`; sides declaring the member differently keep the pin of the side whose branch the merge keeps). A path a repo declares derived (`<path> merge=rwv-ours`) that both sides changed takes the upstream's version, in the project repo and in each member, as a rebase onto the upstream would; any other path both sides changed conflicts. Runs under an operation record: a conflict stops, listing each `rwv.toml` entry in conflict with the value on each side, `rwv pull --continue` resumes it, and `rwv abort` puts everything back. A merged `rwv.toml` that does not parse stops as `unparseable-project` `mid-operation`, the merge in progress with that file staged; `rwv pull --continue` commits a staged `rwv.toml` that parses in the merge |
| `--reset-to-upstream <repo>` | Move the project branch to its upstream, keeping the tip it leaves in a backup (`rwv backup list`). The repo is `projects/<name>`. Refused where the upstream is gone, a record lands into the branch, or the checkout has edits. Runs under the operation record |
| `--keep <yours\|incoming\|text-merge>` | With `--continue`, where the VCS left regions of `rwv.toml` in conflict: `yours` or `incoming` takes that side's value for every entry in them both sides changed differently, and writes and stages the merge of both sides, what the VCS merged outside the regions standing as it merged it; refused where `rwv.toml` was edited since the stop. Where the check stopped a text merge of `rwv.toml` that differs from the merge of the values, `text-merge` marks that text merge resolved as the VCS made it and writes nothing, and `yours` or `incoming` refuses; typed at any other stop `text-merge` refuses. See [Recover from a sync conflict](../how-to/recover-from-sync-conflict.md) |
| `--continue` | Resume the pull a conflict stopped, from the workspace it runs in |
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

A member on its branch behind the pin is fast-forwarded to it; one ahead of it stays where it is. A member detached because its branch is checked out in another workspace carries the fact `checkout`, naming where, and nothing moves that branch. A member on a branch other than the expected one moves off it only when every commit it holds stays on some other ref: its branch is a declared branch or named after a recorded weave branch, or no commit it holds is left on no other branch, remote-tracking branch or backup. A member that moves off another branch is reported as `moved from <branch>`. A member that cannot move is reported on the `alignment` axis with the commits that would be left, and so is one with uncommitted changes or an operation in progress; `update` exits 0 for these and leaves them for the choices the report prints. A member whose branch takes in from a branch of another name refuses (`push-branch` `foreign upstream`). A change to the checked-out branch's record made elsewhere is reported as `target` `record changed`, and an update that aligned every member stores the new record, which accepts it (see `rwv branch set-mode`).

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

**The form that gets the latest.** Advance each member repo to its branch HEAD on the remote, then record the advanced tips in `rwv.lock`. Maps semantically to `cargo update` / `npm update`. The pruning fetch it runs first is the one `rwv fetch` runs.

The advance is a **fast-forward of the branch the checkout is on**, not a checkout of the tip: in the primary weave, that branch must be the local counterpart of the one `branch` declares, and `update --remote` refuses when the checkout is on any other branch, naming `rwv update`. On a feature branch, an `owned` or `fork` member advances its feature branch from that branch's own upstream (the same-named branch on a remote), never from its declared branch: taking the target's work in is `rwv sync`'s. A feature branch with no upstream has nothing to take in, and one that takes in from a branch of another name refuses as `push-branch` `foreign upstream`. A `dependency` or `reference` member advances on its declared branch; where that branch is checked out in another workspace it advances detached to the upstream's tip, moving no branch, and the lock records that tip. A branch that is ahead of its upstream has nothing to take in. One that has diverged refuses as `push-branch` `diverged`, with nothing changed, unless `--merge-upstream` merges it with its upstream (first parent the upstream; a branch no checkout has attached merges as refs and touches no working tree; a conflict there is refused, naming that a workspace with the branch checked out is where to run it) or `--reset-to-upstream <repo>` moves it to its upstream, keeping the tip it leaves in a backup. A member named bare, `<m>`, means the branch this workspace expects for it and no other. Outside a feature, a member branch other than that one which a commit of the project branch's unpushed range declares, which `rwv push` publishes, is named `<m>:<branch>`: `--repo <m>:<branch>` with `--merge-upstream` merges it, `--reset-to-upstream <m>:<branch>` resets it, each as refs where it is off its upstream (where its merge as refs would conflict, the merge refuses `push-branch` `diverged` and offers the routes that need no merge: the reset alone where the expected branch already holds each of its unpushed commits, by ancestry or by a copy that records its source; else copying them onto the expected branch, recording their source, then `rwv lock --commit` and the reset, or publishing the branch's tip under a name you choose, then the reset; each project commit of the unpushed range whose pin the reset leaves only a backup holding is listed), and the lock records nothing of it; a member named only this way keeps its expected branch where it is, and a diverged one named with neither flag refuses as `push-branch` `diverged`. `<m>:<branch>` refuses (`inapplicable-flag`) where the branch is the one this workspace expects (name it `<m>`), where no commit of the unpushed range declares it, where it does not exist here, where a checkout holds it, or where it takes in from no upstream; a `glob:` or `re:` pattern takes no `:<branch>` and refuses one. For a feature's `owned` or `fork` member, `--merge-upstream` first applies the lineage rule: an upstream that lacks the base the member's work is built on refuses as `pin` `off lineage`, before anything moves. Both run under an operation record: a conflict stops, `rwv update --remote --continue` resumes, and `rwv abort` puts everything back. Inside a workweave the same fast-forward rule applies to the workweave's own branch, and a divergence points at `rwv sync` rather than at a flag. An already-detached member stays detached, unless the repo is stopped mid-rebase / mid-merge / mid-bisect, which refuses.

The lock records the members the run advanced and no others, every other pin as it was. A member with no pin takes its upstream's tip as its first pin. The pin a member would take is its branch's own tip where the branch holds its upstream's tip already or `--merge-upstream` merges the two, else the upstream's tip. One that does not descend from the pin the lock holds, or a pin no ref here reaches, would move the pin backward: the run refuses (`pin` `backwards`) after the fetch and before any member moves, whatever `--repo` names; `rwv member pin` is the verb that moves a pin backward by name, and so is `--reset-to-upstream <repo>` for the member it names, reported as `re-pinned <old> to <new>, by name`. A declared branch that exists nowhere after the fetch, locally or on a remote, refuses at the same point as `name` `no-such-branch`, as `rwv lock --commit` refuses it.

`advanced N repo(s)` counts repos whose SHA actually changed.

| Flag | Effect |
|---|---|
| `--remote` | Selects this form |
| `--role` / `--repo` | Selector filters; they select the members advanced and recorded |
| `-j N` | Parallel per-repo workers (default: min(nproc, 8)) |
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `--merge-upstream` | Merge each acted-on member branch that has diverged from its upstream, and each branch a commit of the unpushed range declares, as refs. A path the member declares derived (`<path> merge=rwv-ours`) that both sides changed takes the upstream's version, as a rebase onto the upstream would. Needs `--remote` |
| `--reset-to-upstream <repo>` | Move this member's branch to its upstream, keeping the tip it leaves in a backup; repeat per repo. Refused where the upstream is gone, a record lands into the branch, or the checkout has edits. Needs `--remote` |
| `--continue` | Resume the update a conflict stopped. Needs `--remote` |
| `--dirty` | Record each member's committed HEAD although it has uncommitted changes to tracked files, leaving those edits out of the lock. Needs `--remote` |
| `--commit` | Commit `rwv.lock` together with the integration files regenerated against the new tips, and with `rwv.toml` when it has uncommitted changes. Other uncommitted changes in the project repo stay out of the commit and as they are, named in one line. Needs `--remote` |
| `-m` / `--message <msg>` | The commit message for `--commit`, in place of the generated summary. Without `--commit` it is a usage error (exit 2) |
| `--no-materialize` | Skip the install hooks of the regeneration `update --remote` runs at primary; `rwv materialize` runs them later. Inside a workweave `update --remote` runs no hooks either way |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

A repo that fails to advance fails the run: the repos that did advance stay where they moved, the lock is not written, and the run exits 1 with `partial-run-aborted`.

`--json` emits one JSON envelope whose `repos` lists one record per repo with `path`, `absolute_path`, `branch`, `kind` (`updated` / `up-to-date` / `failed`), `old_sha`, `new_sha`, and `error`. See `rwv explain update`.

### `rwv lock`

Record where the member checkouts are as `rwv.lock`'s pins. No network access.

Reads HEAD from each member the run acts on and records its resolved 40-character hex commit SHA in `rwv.lock` (never a tag or branch name, even when a tag points at HEAD), moving a pin **forward only**. Every other pin stays as it was.

- A member with no pin **keeps none**, reported as `pin` `none` with the verbs that give a first pin: `rwv member pin`, `rwv update --remote` (and `rwv member add`, which pins what it clones). An unborn member and a lock that does not parse (it pins nothing, and is written afresh) are the same.
- A member whose HEAD is behind its pin, has diverged from it, or whose pin no ref here reaches is refused as `pin` `backwards`, with or without `--repo`, before anything is written. A pin moves backward only by name: `rwv member pin <m> <rev> [--commit]`, which the refusal prints with `HEAD` as the revision where that would succeed, a setter, or a hand edit of the member's entry that `--commit` commits (reported `re-pinned`).
- A member not where the checked-out branch's record puts it is refused on the `alignment` axis, `rwv update` being the choice that aligns it. A member the record leaves where it was found, with no pin to place it, keeps its pin.
- A detached workspace, or a detached project repo, refuses (`alignment` `project detached`): there is no branch to record the pins on.
- Uncommitted changes to tracked files in a member refuse (`edits` `dirty`); use `--dirty` to bypass. Untracked files are fine.
- An operation in progress in this workspace refuses (`op-in-progress`), with or without `--commit`, as every verb that writes does; the operation's `--continue` or `rwv abort` ends it first.

Every member is judged before `rwv.lock` is written, and all refusals are reported together. Under `--commit`, the project repo's other uncommitted changes stay out of the commit and as they are, named in one line.

A forward move over commits no remote-tracking branch reaches reports them when another project's lock for the member does not pin them: the other project's own `rwv status` then lists the member as `pin` `ahead`, with the fact `pinned-by` naming this project's lock.

| Flag | Effect |
|---|---|
| `--dirty` | Record each member's committed HEAD although it has uncommitted changes to tracked files, leaving those edits out of the lock |
| `--repo <selector>` | Record only the members matching the selector (exact path, `glob:<pat>` or `re:<pat>`; repeat for union); it only narrows, and moves no pin backward. A selector matching no member refuses (`repo-not-in-manifest`) |
| `--commit` | Commit `rwv.lock` after writing it, together with `rwv.toml` when it is modified or has never been committed. Other uncommitted changes in the project repo stay out of the commit and as they are |
| `-m` / `--message <msg>` | The commit message for `--commit`, in place of the generated summary. Without `--commit` it is a usage error (exit 2) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |
| `--merge-driver <base> <ours> <theirs>` | Plumbing: the command the lock merge driver's config names. git runs it from the project repo with the three files it wrote; no operator runs it. See [The lock merge driver](#the-lock-merge-driver) |

The generated commit message names what the commit holds: `lock: refresh N repo(s)` (followed by the per-repo list) when pins moved, `lock: drop <repo>` (or `drop N repos`) when repos left the manifest, `lock: record rwv.toml` when only `rwv.toml` changed, each joined when several apply, and `lock: no changes` otherwise. `--commit` over a `rwv.toml` that differs from the committed one commits the whole file: it first runs the step of each setter for the entries that changed (a changed declared branch is pinned at its tip and the member switched onto it, a pin that moves backward reported as `re-pinned <old> to <new>, by name` as `rwv member pin` reports it, a branch that exists nowhere refusing with `name` `no-such-branch`; a changed url relinks, or is reported waiting for a fetch as `rwv member set-url` reports it; a hand-added member is given its first pin; a hand-removed member's checkout is kept), then commits every other change as it stands, `[integrations]` and `[workweave]` included, with the files its regeneration rewrites. Every refusal is judged before anything is written. `--commit` prints `Committed rwv.lock` or `Committed rwv.lock, rwv.toml`, or `Lock unchanged, nothing to commit.`

No integration hooks fire. To refresh ecosystem lockfiles (`node_modules`, `.venv`, etc.) after membership changes, run `rwv materialize` (or `rwv activate <project>` when switching the active project).

#### The lock merge driver

`rwv lock --merge-driver <base> <ours> <theirs>` merges `rwv.lock` in a merge rwv does not make: a `git merge`, `git pull`, `git rebase`, `git cherry-pick` or `git revert` run by hand. `rwv init` and `rwv clone` set it up: `rwv.lock merge=rwv-lock` in the project repo's committed `.gitattributes`, and `merge.rwv-lock.driver = rwv lock --merge-driver %O %A %B` in the clone's config, which every workweave of the clone shares. git finds `rwv` on `PATH` by name, as it finds git-lfs for its filters.

Pins merge three-way against the merge base's lock: a side that moved a pin wins; both moved alike, that pin; a member one side added takes that side's pin. Both moved under one declared branch, the pin containing the other. The two sides declaring a member differently, the pin of the side whose declared branch the merge keeps, reported on stderr. The declared branches are read from each side's `rwv.toml` at its commit, which git names to the driver only in a merge of two heads (`git merge`, `git pull`); in a rebase, cherry-pick or revert a member the sides moved differently is not decided.

A merge the driver does not settle — a pin diverged under one branch, or a member whose kept branch it cannot name — writes both locks between conflict markers over `<ours>` and exits 1, so git leaves `rwv.lock` conflicted. If git cannot run the driver at all (`rwv` is not on `PATH`), git leaves `rwv.lock` conflicted with this side's lock unmarked in the working file. Either way every rwv verb resolved for the project refuses with `edits` `lock unresolved` until you resolve `rwv.lock` to the pins you mean and stage it.

rwv's own merges and replays (`rwv pull`, `rwv sync`) do not run the driver: they settle `rwv.lock` themselves and commit their own relock.

A project repo whose attribute line is absent or names a driver other than the lock merge driver (`merge=rwv-ours`, `merge=ours`), or whose config does not name the command or still defines `merge.rwv-ours.driver`, is refused with `missing-lock-merge-driver` by every verb that writes the project repo or its lock, before it writes anything; `rwv doctor --fix` migrates it, committing only the attribute line.

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

Refuses if an operation elsewhere holds the op record covering this workspace (`op-in-progress`).

### `rwv materialize [...]`

Reconcile integration-managed workspace files, weave-root symlinks, and install hooks for the project this checkout already presents: the materialization step of `rwv activate`, without the project selection. Valid in both a primary weave (for `.rwv-active`) and a workweave (for `.rwv-workweave`). Takes no project name; at a primary with no active project it refuses and points to `rwv activate <project>`. Refuses if an operation elsewhere holds the op record covering this workspace (`op-in-progress`).

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

Create a new project repo at `projects/<name>/` with an empty `rwv.toml`, set up the lock merge driver — `rwv.lock merge=rwv-lock` in `.gitattributes` and `merge.rwv-lock.driver` naming `rwv lock --merge-driver %O %A %B` in the project repo's config (checked by `rwv doctor` as `missing-lock-merge-driver`) — author the integration files an empty manifest implies (`<name>.code-workspace` with the default integrations), commit exactly those files (`rwv.toml`, `.gitattributes` and the integration files) as the project repo's first commit, and activate the project. A new project starts with nothing for `rwv doctor` to report and nothing to commit by hand; `init` pushes nothing. It refuses before writing anything when no commit author is configured, and a commit a hook refuses after the files are written is reported with the files left uncommitted. Re-running `init` for an existing project refuses (`project-dir-occupied`) and commits nothing. With `--provider`, adds a remote with the project repo's URL and records it as the project repo's url remote.

When invoked in an **empty directory**, `init` bootstraps that directory as a weave root (no pre-existing `rwv.toml` required) and creates the project inside it. Running in a non-empty directory without an existing workspace refuses.

With `--adopt`, `<name-or-source>` is a URL or a `<registry>/<owner>/<repo>` shorthand; a two-segment `<owner>/<repo>` refuses with `unresolvable-repo-source`. `init` clones the project repo from that source instead of `git init`-ing a new one (brownfield adoption of an existing project repo), records the remote the clone made as the project repo's url remote, writes `rwv.toml` when the clone has none and the `rwv.lock merge=rwv-lock` line when `.gitattributes` lacks it or names an earlier release's driver, defines the lock merge driver in its config, commits whichever of the two it changed and nothing else (the repo's first commit when it had none), and runs the adopted project's install hooks as it activates it. It authors no integration file: the adopted project's members are not on disk until `rwv fetch`, and `rwv doctor --fix` regenerates once they are. `--no-materialize` skips those hooks; without `--adopt` it is a usage error (exit 2), since a new project has no members for a hook to act on.

### `rwv member`

Add a repo to the active project, remove one, or change how the manifest lists one. Each subcommand below is an authoring verb: it edits `rwv.toml`, regenerates the integration-managed workspace files, and runs the integration install hooks (at primary; inside a workweave a `member` verb runs no hooks). Each takes `--project <name>` to operate on that project instead of the active one (does not change `.rwv-active`) and `--no-materialize` to skip the install hooks (regeneration still runs, and `rwv materialize` runs the hooks later).

### `rwv member add <url> [...]`

Clone a repo (if not present), register it in the target project's `rwv.toml`, settle write permissions for the role, regenerate integration-managed workspace files, and run integration install hooks. It then commits the entry, the regenerated files and `rwv.lock` in one commit, with the member's **first pin** in the lock: the commit it cloned. With `--adopt`, the pin is the clone's HEAD where HEAD sits on the declared branch or that branch's upstream reaches it; a clone holding commits only it has, and a repo `--new` made with no commits, have none, reported as `pin` `none` with the verbs that give one. If the repo entry was already added to `rwv.toml` by hand, `rwv member add` converges the on-disk clone, permissions, and integration files to match, and gives the member the pin it lacks in a commit that carries the entry. The add refuses before it changes anything when the project directory is not a repository. Uncommitted changes to other tracked files of the project repo, staged or not, stay out of the commit and as they are, named in one line.

The positional argument has one spelling per meaning:

- **A URL** (`https://…`, `git@…`, `file://` for a local remote) or **registry shorthand** (`github/owner/repo`) clones from that remote.
- **`--adopt <path>`** registers the clone already at `<path>` under the weave root, reading its URL from its remote. A clone with several remotes refuses with `no-remote-url`, since the URL to record is a choice among them: remove the others, or name the URL (`rwv member add <url>`) when the clone sits at that URL's placement. A relative `<path>` resolves against the directory `rwv` runs in (or the `-C`/`-w` directory), symlinks in it are followed, and the member path recorded is the position of the location it leads to under the weave root containing it; from a workweave, a path inside the workweave names the primary's clone at the same position. No clone there (a bare repo counts as none) refuses with `no-clone-to-adopt`; a location under no weave root refuses with `adopt-path-outside-weave`; a workweave path holding a clone that is not a worktree of the primary's refuses with `foreign-clone-in-workweave`.
- **`--new <address>`** creates a repo; see below.
- **Anything else** — a plain path, relative or absolute — refuses with `unresolvable-repo-source`, naming both the `file://` and the `--adopt` spelling. A URL-shaped argument no registry can place (`https://example.com`) refuses with `no-matching-registry`.

| Flag | Effect |
|---|---|
| `--role <role>` | Sets the role (`owned` / `fork` / `dependency` / `reference`). Defaults to `owned`. For a member the manifest already lists, a role other than its own refuses (`inapplicable-flag`), naming `rwv member set-role` |
| `--adopt` | Register an existing clone; the positional argument is its path (relative to the invocation directory) under the weave root. Conflicts with `--new` |
| `--new` | Create a new repo instead of cloning; the positional argument becomes a creation address (a bare registry name, or `registry/owner/repo`) rather than a URL or path. See `rwv explain member`. |
| `--no-recurse-submodules` | Record `recurse-submodules = false` for the new member, so new checkouts of it leave its submodules uninitialized. For a member the manifest already lists that recurses, the flag refuses (`inapplicable-flag`), naming `rwv member set-recurse-submodules` |
| `--param <name>=<value>` | A creation parameter for `--new` (repeatable). Requires `--new` |
| `--params-json <json>` | The same parameters as one JSON object of string values. Requires `--new` |
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

`rwv member add` writes to CWD's workspace's manifest (the active workspace's `rwv.toml`), not always primary's.

**A clone already at the placement.** When the URL's placement already holds a git clone, `member add` registers it only if one of its remotes has a URL equivalent to the one given (`Directory already exists at '<path>', skipping clone`). A clone of any other remote, or one with no remote, refuses with `foreign-clone-at-placement`, naming `rwv member add --adopt <path>`; nothing is written. Anything at the placement that is not a clone (a directory, empty or not, or a file) refuses with `non-clone-at-placement`, naming it; so does one at the workweave's own placement when `member add` or `member add --adopt` runs from a workweave.

**Canonical path.** Every URL lands at `<registry>/<owner>/<repo>/`. `<registry>` is the matched built-in registry's name (`github`, `gitlab`, `bitbucket`) when the host is one of those; for a host none of them recognize, it's the URL's own host (e.g. `git.corp.example/team/repo/`); for `file://`, which has no host, it's `local`.

**Shared-clone warning.** If the target clone directory is already registered by another project in the same weave, `rwv member add` proceeds (the manifest entry is added to the active project as usual) and emits a warning to stderr naming the other project(s). Sharing a clone across projects is legal — the same repo can be a `dependency` in one project and `owned` in another — but is worth flagging so accidental double-registration is visible.

### `rwv member remove <path> [...]`

Remove the repository from `rwv.toml`, regenerate integration-managed workspace files, re-run integration install hooks, and commit the removal with the member's pin dropped from `rwv.lock` and the files it regenerated, leaving every other uncommitted edit where it is. Uncommitted changes to other tracked files of the project repo, staged or not, stay out of the commit and as they are, named in one line. If the entry was already removed from `rwv.toml` by hand, `rwv member remove` converges the integration files and optional checkout deletion (`--delete`) to match.

| Flag | Effect |
|---|---|
| `--delete` | Also delete this workspace's checkout of the repo: the canonical clone at primary, the workweave's worktree inside a workweave, along with the branch the workweave holds in the canonical clone. Refuses over uncommitted changes or commits nothing else reaches, a [backup](#rwv-backup) holding such commits included (`dropped-repo-has-unique-commits`) |
| `--delete-shared-clone` | Requires `--delete`. At primary, removes the clone even if other projects still reference it. Without it, a clone another project lists refuses (`shared-clone-referenced`); the check runs only at primary, since a workweave's worktree is that workweave's own |
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

### `rwv member pin <path> <rev> [--commit]`

Pin a member at `<rev>`, a commit id or a name that resolves to one in the member's clone, in either direction, and align the member to the pin. This is the verb that gives a member its first pin when `member add` could not, and the one that moves a pin backward; it reports `re-pinned` when it does.

Where the member follows a declared branch (every member of a declared workspace, and a `dependency` or `reference` member of a feature), `<rev>` must be a commit that branch or its upstream reaches, or the verb refuses (`pin` `not on declared branch`). A `<rev>` the clone does not resolve refuses (`name` `no-such-branch`). A member left detached with commits no branch holds is backed up first. A `<rev>` behind the branch's tip leaves the checkout at the tip, and the verb reports where it stands against the pin (`pin` `ahead` or `unpinned`); the next plain `rwv lock` records the tip again. A workspace that holds no checkout of the member gets the pin only, and `rwv update` creates the checkout.

Every refusal is reported before the lock is written or the member moves. A detached workspace refuses (`alignment` `project detached`).

| Flag | Effect |
|---|---|
| `--commit` | Commit `rwv.lock` after writing it, together with `rwv.toml` when it has uncommitted changes, recording the re-pin in an `Rwv-Repin` trailer ([formats](./formats.md#re-pins-by-name-in-lock-commits)). Other uncommitted changes in the project repo stay out of the commit and as they are. Without it the lock is left written and uncommitted, and the `rwv lock --commit` that later commits it records no re-pin by name |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

### `rwv member set-branch <path> <branch> [--new] [--at <rev>] [--url <url>]`

Declare the branch the member follows. The two meanings have two spellings. Without `--new`, `<branch>` must exist, locally or on a remote as last fetched; one that exists nowhere refuses (`name` `no-such-branch`), naming `rwv fetch` (the branch may sit on a remote not yet fetched) and the same command with `--new`. With `--new`, `<branch>` must exist nowhere (else `name` `exists`, naming the spelling without it) and is created.

In a declared workspace, and for a `dependency` or `reference` member of a feature, the member is re-pinned to the branch's tip, or to `--at <rev>` on the branch or its upstream (else `pin` `not on declared branch`), in either direction and by name (`re-pinned` is reported when it moves backward). With `--new` the branch is created at the member's pin, or at `--at`, and pinned there. The member then switches onto the branch, and the entry and the pin are committed together; a detached HEAD holding commits no branch has is backed up first. An existing branch ahead of `--at` is reported: a landing merges into its tip, so a pin that must stay put needs a branch of its own (`--new --at <rev>`). A branch another live weave branch also declares is reported as the fact `shared`, listing them.

For an `owned` or `fork` member of a feature only the entry changes: `--at` is refused (`inapplicable-flag`), the member and its pin stay, and with `--new` the branch is created at the branch the feature lands into's pin of the member.

`--url <url>` sets the member's url in the same commit, as `member set-url` does. A branch name a feature of that name uses refuses (`name` `feature`). Every refusal is reported before anything changes, and the commit holds the entry and the pin and nothing else the project repo holds uncommitted. The aligning verbs, `lock`, `push` and `status` report each entry change as `member-changed`, with the previous and the new value and the old branch's unpushed commits, until the push branch carries it or it is restored.

| Flag | Effect |
|---|---|
| `--new` | Create the branch |
| `--at <rev>` | Pin at this commit of the branch or its upstream; with `--new`, create the branch there |
| `--url <url>` | Also set the member's url |
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

### `rwv member set-url <path> <url>`

Set the member's url. The branch the member sits on in this workspace, when its upstream is on the old url's remote and it has no push remote of its own, takes the new url's remote's branch of the same name as its upstream; a branch that has diverged from it refuses (`push-branch` `diverged`) before anything changes. A new url whose remote name another url holds refuses (`name` `taken`). The verb adds no remote and fetches nothing: where no remote of the clone has the new url, or that remote holds no copy of the branch as last fetched, it reports that the branch relinks to the new url once a fetch finds it there, naming `rwv fetch` (which adds the url's remote first), and the aligning verbs report the same until one relinks it. A url change that arrives by a merge relinks the same way at the next aligning verb, reported with the entry changes. The entry and the lock's url are committed alone.

### `rwv member set-role <path> <role>`

Change the repo's `role` in `rwv.toml`, regenerate integration-managed workspace files (a `reference` repo leaves every ecosystem's build graph), run integration install hooks, and settle the repo's files: a `reference` repo's tracked regular files lose their write bits; any other role restores owner write permission. If `role` was already edited in `rwv.toml` by hand, `rwv member set-role` converges the on-disk permissions and integration files to match. Refuses a change to `reference` while the repo has uncommitted changes to tracked files. The change is committed alone: any other uncommitted edit to `rwv.toml` stays uncommitted, and so do uncommitted changes to other tracked files of the project repo, named in one line.

| Flag | Effect |
|---|---|
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

### `rwv member set-recurse-submodules <path> <true|false>`

Set the repo's `recurse-submodules` in `rwv.toml` and regenerate integration-managed workspace files. The setting says whether a new checkout of the member populates the submodules it records; a checkout that already exists keeps the submodules it has. `true` is the default and is not written out. If the key was already edited in `rwv.toml` by hand, `rwv member set-recurse-submodules` converges the integration files to match. The change is committed alone, as `member set-role`'s is.

| Flag | Effect |
|---|---|
| `--no-materialize` | Skip the install hooks; regeneration still runs, and `rwv materialize` runs the hooks later |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

### `rwv sync [...]`

Take new work into the checked-out weave branch. In a feature, `rwv sync` takes in the branch its record lands into (the target); on any weave branch, `--from <branch>` merges that branch's history in. A declared branch takes nothing in without `--from`: it refuses as `target` `no target`. There is no positional argument; `rwv land` is the verb that lands a feature into its target.

**From the target.** Each `owned` or `fork` member the target's lock pins takes in its merge source: the tip of the branch the member lands on (its branch named after the target when the target is itself a feature, else the branch the entries declare), the newer of that branch and its upstream, or the target's pin where that branch exists nowhere. Commits that branch holds beyond the target's pin are listed, since the landing will pin them. A member with no new work follows its merge source: by fast-forward, or, where the merge source lacks the member's tip, by moving to it after a backup. A member holding new work — commits its merge source lacks — is integrated as the target's `history` setting says, read from the target's own `rwv.toml` (the member's entry, else the top level): under `linear` and `ff-only` its work is rebased onto the merge source after a `rebased` backup, and under `merge` the merge source is merged in, the member's branch as first parent. A range holding a merge commit is never rebased: under a rebasing setting it is integrated by a merge commit instead, reported `merged`. Where the target records no setting for a repo that needs one, the sync refuses as `history` `unset` unless `--history` gives one for this run; a `--history` differing from a recorded setting refuses as `history` `contradicted`. Then the project branch takes in the target's tip the same way, by the target's top-level `history`, and the lock is recorded and committed: each member's pin merged three ways against the merge base's lock, every member the sync moved at its new tip. `dependency` and `reference` members are not moved by the take-in; they are aligned afterwards to the lock the sync committed.

**The lineage rule.** A member holding new work, and one that would follow while another weave branch lands into this one, moves only when the branch it lands on contains its base: the newest commit of its first-parent chain that a lock in the target's history pins. Otherwise the sync refuses as `pin` `off lineage`, naming the base and the branch, with the fact `patch-equivalent` where commits on that branch carry the same changes. The rule judges after the sync's fetch of every repo, so a pin published on a branch or tag of a member's remotes is judged like any other. A pin this clone still lacks then does not decide the base; where a lock commit of the target's history that the source's project branch does not reach holds it (a lock written since the two branches parted), the sync reports it once as `history pin absent`, naming the pin and its lock commit, and an older one is not reported. `rwv land` fetches nothing: it judges on the pins this clone holds and reports a lacked pin from since the branches parted the same way, saying this clone does not hold it and that `rwv sync` fetches and judges it. A landing made without a sync, on a clone that lacks a pin, can therefore pass work that a clone holding the pin refuses as `pin` `off lineage`: with that note where the pin is from since the branches parted, and silently where it is older, as for work carried onto a published branch the target moved off before then. `rwv sync` first catches either case wherever a branch or tag of the member's remotes still holds the pin; nothing catches it where none does. What lands brings back commits the target dropped, as merging them would in single-repo git. A clone that holds a pin judges by it wherever it stands in the history.

**`--from <branch>`.** Merges the branch's history in, whatever the setting: each member that lacks the branch's pin merges it as a merge commit with this branch's tip as first parent (a `--from` of the record's own target may fast-forward instead), then the project branch takes the branch in, with the relock. On a declared branch, a member the named branch declares on another branch is refused (`pin` `not on declared branch`) until that branch is declared here.

**`--reset-to-target <repo>`.** Moves the named repo's branch to its merge source instead, skipping the lineage rule and the push-branch judgment for it: a member to the tip of the branch it lands on, `projects/<name>` to the target's tip with the target's lock. The tip it leaves is kept as a backup, and the new work it drops is listed with the commands that copy it back or merge it in.

After the take-in, members a new entry set drops are pruned, missing ones are cloned, and every member is aligned as [`rwv update`](#rwv-update-) aligns it. Where the take-in changed an input of the project the weave root presents — `rwv.toml`, `rwv.lock`, or a member's manifest an enabled integration reads — `rwv sync` says so and names `rwv materialize`; sync runs no install hooks.

Every refusal is judged before anything moves, and all of them are reported together:

| Axis | State | When |
|---|---|---|
| `target` | `no target` | The branch is declared and `--from` names nothing to merge |
| `target` | `gone` | The target exists neither here nor on a remote |
| `target` | `record changed` | The branch's record changed since this workspace last saw it; `rwv update` accepts the new one |
| `target` | `dependents` | Another weave branch lands into this one, and the sync would rewrite a branch it is built on |
| `name` | `no-such-branch` | `--from` names a branch that exists nowhere |
| `history` | `unset` | A repo needs more than a fast-forward, the target records no `history` for it, and no `--history` gives one |
| `history` | `contradicted` | `--history` differs from the setting the target records for a repo |
| `pin` | `off lineage` | A member's base is not on the branch it lands on (above) |
| `pin` | `backwards` | A member's checkout does not contain the pin this branch's lock holds for it (behind it, diverged from it, or the pin in no ref here), as `rwv land` refuses it: the sync would move the pin off it with no name. The `rwv member pin` the refusal prints moves it by name; a member `--reset-to-target` names is not judged |
| `pin` | `none` | This branch's lock has no entry for a member, as `rwv land` refuses it |
| `pin` | `unreachable` | The target pins a commit this clone lacks |
| `pin` | `not on declared branch` | `--from` on a declared branch, for a member the named branch declares elsewhere |
| `push-branch` | `behind` | A branch the sync would move is behind its push branch: take that in first |
| `push-branch` | `diverged` | A branch the sync would move, or the target, has diverged from its upstream. Where everything it holds beyond its push branch is already on the branch it lands on, the divergence is reported as the fact `landed` and the sync goes on; where only a superseded tip of the upstream reaches it (the fact `rewritten`), or it holds commits never pushed, it refuses |
| `protected` | `declared` | A rebase, follow or reset would rewrite a member branch some weave branch declares |
| `edits` | `dirty` | A repo the sync would move holds uncommitted changes to tracked files |
| `checkout` | `in another workspace` | A member branch the sync would move is checked out in another workspace |
| `alignment` | (any) | A member is not where the branch expects it; `rwv update` puts it there |

`inapplicable-flag` refuses `--target` with `--from`, and a `--reset-to-target` naming a repo the sync takes nothing into. A fetch that could not complete refuses before anything is judged (`partial-run-aborted`).

A conflict stops the sync with its op record kept (`op-parked`), naming the checkout: resolve it, then `rwv sync --continue`, or `rwv abort` to put every repo back at its savepoint. Where `rwv.toml` is unmerged the stop lists each entry in conflict in the regions the VCS left, and `rwv sync --continue --keep yours` or `--keep incoming` writes and stages the merge of both sides, taking that side for each; what the VCS merged outside the regions stands.

Where the VCS merges the project repo's `rwv.toml` as text with no conflict, the sync checks the result against the merge of both sides' values. Under `merge` the merge is checked before it is committed; a text that parses and differs stops the sync with `rwv.toml` conflicted and its working file as the VCS merged it, listing each entry that differs. Under `linear` the replay runs as `git rebase` runs it, and once it has finished the replayed tip's `rwv.toml` is compared with the merge of the values of the merge base, the commit replayed onto and the branch's tip before the replay; where it parses and differs the sync stops as `unparseable-project` `mid-operation`, listing each entry. Only entries at most one side changed are compared; an entry both sides changed keeps the VCS's text, as it merged it, and where the sync stops for another entry it is listed too where the text holds one side's value. The commits the replay wrote before the tip are not checked. The replayed tip is checked however the replay's own conflicts were resolved. Where the two sides have more than one merge base, the text merge is not checked. An `rwv.toml` the operator marks resolved at the tip stop that does not parse is not taken: the bare continue stops again with the parser's error. A dependent branch the sync carries along gets the same comparison of its replayed tip; where it differs the dependent is not moved, and a line names it and the `rwv workweave create` that opens it, where `rwv sync` stops on its own tip. At either stop rwv writes nothing to `rwv.toml`: the bare `rwv sync --continue` refuses until an `rwv.toml` is marked resolved, which is taken whole, and `--keep text-merge` marks the text merge resolved as it stands, refused where the working file was edited at the merge check or another file is marked resolved at the tip; once settled, the replayed tip is not judged again in that operation. A merge made by `git merge` by hand, or on a forge, is not checked.

A merge or rebase of the project repo that merges `rwv.toml` as text without a conflict, into a file that does not parse, stops the sync as `unparseable-project` `mid-operation`, its op record kept, naming the file and the parser's error. Under `merge` the merge is left in progress with the merged file staged; under `linear` the rebase has finished and the replayed tip holds it. Edit `rwv.toml` until it parses and stage it, then `rwv sync --continue`: a merge commits the staged file in the merge, and a rebase makes it part of the replayed tip, which the rebase wrote and nothing has published. While the staged file does not parse, `--continue` stops the same way. `rwv abort` puts every repo back. Only the result is read: a commit of the rebased range whose `rwv.toml` does not parse, followed by one whose does, is replayed as it is, as git replays any commit, and `rwv cherry-pick` refuses it as `unparseable-project` `commit`. A staged fix goes into the replayed tip alone; where earlier replayed commits keep an `rwv.toml` that does not parse, the stop says how many, and they land as the replay wrote them.

| Flag | Effect |
|---|---|
| `--from <branch>` | Merge this weave branch's history in for one run, as a merge commit, whatever the setting |
| `--target <T>` | Take in `T` instead of the record's target, for one run; the record is unchanged |
| `--reset-to-target <repo>` | Move this repo's branch to its merge source after a backup, listing the new work it drops; repeat per repo. `projects/<name>` names the project branch |
| `--history linear\|merge\|ff-only` | How a repo holding new work takes in, for this run, where the target records no `history` for it; a value differing from a recorded one is refused |
| `--keep <yours\|incoming\|text-merge>` | With `--continue`, where the VCS left regions of `rwv.toml` in conflict: `yours` or `incoming` takes that side's value for every entry in them both sides changed differently, and writes and stages the merge of both sides, what the VCS merged outside the regions standing as it merged it; refused where `rwv.toml` was edited since the stop. Where the check stopped a text merge of `rwv.toml` that differs from the merge of the values, `text-merge` marks that text merge resolved as the VCS made it and writes nothing, and `yours` or `incoming` refuses; typed at any other stop `text-merge` refuses. See [Recover from a sync conflict](../how-to/recover-from-sync-conflict.md) |
| `--continue` | Resume a sync a conflict stopped, after resolving it; every parameter comes from the op record |
| `--json` | Structured output: one JSON envelope |
| `-j N` | Parallel per-repo fetches (default: 1) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

`--json` prints `{"$schema": "...", "outcomes": [], "placements": [...], "members": [...], "complete": ..., "refused": [...], "advisories": [...]}`: `placements` says how the files of each checkout the sync moved were placed, `members` lists the checkouts a dropped entry pruned or failed to prune, `refused` carries each judged line with its choices, and `advisories` carries `derived-state-stale` when the take-in changed a materialized input. See `rwv explain sync`.

**A checkout the sync could not prune.** A member the new entries drop is pruned once every repo has taken in, and the prune is refused for a checkout holding uncommitted changes, commits nothing else reaches, or the weave's only copy of its store. The refusal does not stop the sync: it finishes and clears its op record, then exits 1, naming each checkout still on disk:

```
Error: sync completed: replay and relock finished and the op record is cleared, so `rwv abort` does not apply. Pruning was refused for 1 member checkout the manifest no longer names, and it is still on disk:
  local/owner/extra
Re-running sync will not prune it. Once nothing in it is needed, `rwv member remove <path> --delete` removes it.
```

**Exit codes:** `0` when the sync completes or has nothing to take in; `1` when it refuses, stops on a conflict (op record kept; `--continue` or `rwv abort`), or completes with a checkout it could not prune (op record cleared). Under `--json` the envelope's `complete` field tells a stopped sync from a completed one.

### `rwv land [--target <T>] [...]`

Land the checked-out feature into the branch its record lands into (its target), or into `--target <T>` for one run; the record is unchanged. The tip a receiving branch or the target lands on is the newer of it and its upstream as last fetched, so a target behind its upstream takes the upstream in, fast-forward only, and reports what it took in: the upstream's commits, each pin they changed (`re-pinned` where backward) and each member entry they changed (`member-changed`); a pin the upstream's lock holds that this clone lacks refuses (`pin` `unreachable`, naming `rwv fetch`). The work is composed in this workspace, as the target's `history` setting says for each repo: each `owned` or `fork` member holding work its receiving branch lacks is rebased onto that branch's tip under `linear`, joined with it by a merge commit whose first parent is the receiving tip under `merge`, and refused under `ff-only` (`history` `not fast-forward`, naming `rwv sync`); the project branch likewise with the target's tip, by the top-level setting; then the lock is recorded. Any other member is not moved, and its pin is merged three ways against the merge base's lock: the feature's pin where only the feature changed it, else the target's. Commits a moved member's receiving branch holds beyond the target's pin, and commits an unmoved member's new pin holds beyond it, are listed, since the landing pins them; under `--json` each is in `reports` as `beyond-pin`, and an unmoved member's outcome is `no-op`. A repo whose branch holds the receiving tip on its first-parent chain fast-forwards and reads no setting; one holding it only through a merge's second parent lands by a merge commit, first parent the receiving tip, under every setting. A range a rebase would replay that holds a merge commit is never rebased: it lands by a merge commit, reported `merged`. A repo that needs a setting the target does not record refuses (`history` `unset`) unless `--history` gives one for this run. Then, where the target is checked out, each receiving branch and the target fast-forward in their checkouts, each on the branch the landing planned and to exactly what it composed; the target is reported `push-branch` `ahead` where its push branch lacks the landing, naming `rwv push --branch <T>` (under `--json`, `push-branch-ahead` in `reports`); a resumed landing repeats these reports; that workspace aligns, and this workspace's members follow the target's new pins. A receiving branch checked out in a third workspace (the primary or another workweave, whichever project it presents) fast-forwards in place in that checkout, which must be clean (`edits`); the landing leases that workspace while it runs, and `rwv abort`, run in the landing's own workspace or in a workspace it leases, puts the checkout back with the rest (where that checkout has since been moved off the branch onto a tip the landing did not write, it puts the branch back as a ref and leaves the checkout as it stands), rolling the landing back whole or refusing before anything moves, naming what is in the way and the command that ends it (once the landing has released the target's workspace, or the third workspace, `rwv abort` typed there takes up only a landing whose retire is recorded as stopped, and otherwise refuses, printing `rwv -C <workspace> abort` for the landing's own workspace); a receiving branch checked out anywhere else refuses (`checkout`). For a project the target's workspace does not present (an inactive project), a member the presented project shares is that project's checkout: the landing judges neither its pin nor its alignment, fast-forwards it only where it is on the receiving branch, else moves the receiving branch as a ref or in place; the alignment that follows names `rwv activate <p> --update` for a shared member it may not move, and each `rwv` choice printed for the target carries `--project <p>`. Where the target is checked out nowhere, it and each receiving branch move as refs instead, each by compare-and-swap against the tip the landing read: one another verb moved since stops the landing (`target` `moved mid-operation`), and `rwv abort` restores what it moved; a branch the target declares for a member the landing does not move, beyond the target's pin by commits no other lock or remote holds, refuses (`pin` `unpinned`), naming `rwv workweave create <T> --branch <T>` and `rwv lock --commit` there; from the primary, the primary then switches to the target. A declared branch refuses as `target` `no target`. A target that exists nowhere, or one named by a record published on a remote where the local branch of that name takes in from a branch of another name, or neither takes in that remote's branch of the name nor publishes there, refuses as `target` `gone`, listing each branch the feature could be rebound to (never the gone target, nor a branch landing into the feature); the rebind names the sole candidate, and where none qualifies the choice is to declare the branch. Every refusal is reported together, before anything moves, the source's own edits, alignment and lock among them even when there is no target to land into, and with `--retire` what retiring it refuses: the record changed since this workspace last saw it, the target's mode changed since the record was written (`target` `mode changed`), work built on another lineage (`pin` `off lineage`), any change to the target's member entries that `--allow-member-change` does not name (`target` `member changes`), and, where there is a target, any branch whose record lands into this one (`target` `dependents`). Where the target is checked out, its edits are judged in each repo the landing moves: the project repo, each receiving branch's checkout, and each member whose pin the landing changes, which aligning moves; a resumed landing judges them again before it relocks. See `rwv explain land` for every state.

**Member changes.** A landing changes the target's member entries, and its top-level `history`, only where `--allow-member-change` names them. A change is any field of the entry of a member the target lists (`type`, `url`, `role`, `branch`, `history`, `recurse-submodules`) that the feature changed since the merge base to a value the target does not hold, the member's removal, or the pin of a member the landing does not compose that it moves to a commit not containing the target's; a field only the target changed stays the target's and is no change. Where the merge base's `rwv.toml` cannot be read, every difference counts as the feature's, a removal and an unset top-level `history` included. Adding a member is no change, and the other top-level tables (`[integrations]`, `[workweave]`) land as content, unnamed. The refusal lists each changed field with the target's value and the feature's, then the rebind onto each live weave branch whose entries match the feature's and whose pin of a re-pinned member the feature's pin contains, the setter that keeps the target's value here, and last the consent, naming every repo the landing changes. Each named change is reported, with the target's value and the landed one, at the start and again on each `--continue`, and under `--json` in `reports` as `entry-changed` or `entry-removed`. A named backward pin is reported `re-pinned`, and the landing's lock commit records each named re-pin by name. The pin of a member the landing does not compose, a `dependency` or `reference` member's always, is merged three-way, before anything moves: the feature's pin where only the feature changed it, which for a `dependency` or `reference` member the branch it sits on in the target must reach (`pin` `not on declared branch`, offering a pin at that branch's tip); where both changed it under one declared branch, the pin containing the other (neither: `pin` `unmerged`, offering a pin at the receiving branch's tip where it holds both); where the two sides declare it differently, the pin of the side whose branch the landing keeps, listing the commits the other side's pin added that the kept branch lacks, with the copy. A checkout in the target's workspace of a member the landing drops is pruned once every member has merged.

**A text merge of `rwv.toml` that differs.** The landing checks git's clean text merge of `rwv.toml` as `rwv sync` does: under `merge` before the merge commit, under `linear` on the replayed tip once the replay has finished, and on each dependent it carries along; the stops and their `--keep text-merge` and staged edit are the same, and the bare `rwv land --continue` refuses there until an `rwv.toml` is marked resolved.

**An `rwv.toml` that does not parse.** A merge or rebase of the project repo that merges `rwv.toml` as text without a conflict, into a file that does not parse, stops the landing as `unparseable-project` `mid-operation` before the target moves, its op record kept: stage an `rwv.toml` that parses, then `rwv land --continue`, which commits it in the merge, or makes it part of the replayed tip under `linear`; or `rwv abort`. The target never moves to a tip whose `rwv.toml` does not parse.

**A stop in `rwv.toml`.** `--continue` judges `rwv.toml` as the resolution leaves it against the target's tip the landing read, under the names the landing started with: a change none of them covers refuses as `target` `member changes`, the op still stopped, offering `rwv land --continue --allow-member-change <m>`, which adds the name, and `rwv abort`. Once the replay has finished, the `rwv.toml` it left is judged the same way, whatever settled it, so a target entry a later replayed commit changed lands only once named.

**Receiving branches.** Each `owned` or `fork` member lands on its receiving branch: its branch named after the target when the target is a feature, else the branch the merged entries declare. One that exists nowhere is created at the target's pin (for a member the target does not pin, at the feature's), reported `created`. Where the landing changes a member's declared branch, the new branch moves as a branch no checkout holds (one checked out anywhere refuses as `checkout`), the target's workspace switches to it, and the commits the target's pin reaches that the new branch lacks are listed with the copy that ports them, again by `rwv land --continue`, and under `--json` as `left_behind`. A member whose declared branch changes and that holds no new work takes the new branch's tip as its pin, merging nothing; its branch here moves there, the tip it leaves kept in a backup. `rwv abort` deletes a branch the landing created, where it still sits where it was created and another branch reaches it.

**Drift.** A checkout, in this workspace or the target's, whose branch something outside it moved forward still holds the old tip's files and what the VCS records for them (for git, its index entries). Before its edits are judged the landing reads it file by file. A file the move changed whose content and record still hold the old tip's is drift: it is refreshed to the branch's tip and reported, naming each file (`refreshed drift`, and under `--json` as a `reports` entry of kind `refreshed`). What a refresh replaces is the old tip's content, which that tip holds; a return to the old tip recorded for the next commit (for git, staged) made after a refresh by hand cannot be told from drift and is refreshed the same way, so commit such a return first to keep it. Any other difference is an uncommitted edit, refused as `edits` `dirty` and left as it is: a deletion, a file returned to earlier content, a revert recorded for the next commit, a copy and a mode change included, whatever commit already holds their content. A file the move changed that an edit here changed too holds the whole checkout back: nothing in it is refreshed, and the landing refuses as `edits` `moved and edited`, listing such files apart from the checkout's other edits, which get no choice of their own, since a commit there would record the move's reverse; every other verb that judges the checkout's edits names it the same way, a conflicted file is judged `unresolved` first, and where the checkout is in the target's workspace the merge choice is printed with `rwv -C <workspace>`. Something standing where the move puts a path of its own (a directory or an untracked file at a file's name, a link or file where its directory goes) holds it the same way, its one choice the move-aside. Its choices are `rwv doctor --fix --merge-edited-drift`, which keeps the working copy and the VCS's record of it as a backup, points HEAD at the tip the files hold and has the VCS merge every edit onto the branch (for git, `git checkout -m`), refreshing the move's other files and leaving a file it cannot resolve conflicted (`edits` `unresolved`, until it is resolved and marked resolved); and `rwv doctor --fix --set-aside-edited-drift`, which points HEAD the same way, has the VCS set the edits to those files aside against the tip they held and checks the branch out, printing the VCS action that merges them back (for git, `git stash apply <set-aside>`). A file the VCS does not compare with what it records (ignored where the move adds it; for git, also marked assume-unchanged or skip-worktree) holds the checkout too; both flags refuse it, since no backup would keep it, and print the command that has the VCS compare it. Where the VCS refuses either flag, its words are printed and the checkout is left `edits` `set back`, which every verb names and the flag run again finishes. A landing refused before it knows its target reads drift without refreshing it: drift is not listed as an edit, and an edit beside it is listed with `rwv doctor --fix` as its one choice. The landing reads each such checkout again just before writing, and the VCS itself writes the refresh and refuses where a file changed after it was read; then nothing is written there, and the landing refuses as `edits` `drift left in place`, naming why, with landing again as its one choice and the checkout's other edits listed with no choice of their own. A landing that proceeds refreshes the drift of every checkout whose edits it judges; one that refuses refreshes only a checkout whose own edits it refused, so the commit or set-aside offered there cannot carry the old tip's content back. `rwv doctor --fix` reads drift the same way; see [Shared refs and drift](../explanation/joints/shared-refs-drift.md).

| Flag | Effect |
|---|---|
| `--target <T>` | Land into `T` instead of the record's target, for one run |
| `--allow-member-change <repo>` | Let the landing change the target's entry for this member: any field, its removal, or a backward pin; `projects/<name>` names the target's top-level `history`. Repeat per repo. A name the landing changes nothing for refuses (`inapplicable-flag`). With `--continue`, adds to the names the landing started with |
| `--history linear\|merge\|ff-only` | How a repo holding work its receiving branch lacks lands, for this run, where the target records no `history` for it; a value differing from a recorded one is refused. The target's own move is a fast-forward whatever the setting |
| `--retire` | Once the landing is done, remove the workweave and apply the deletion rule to the feature's branches (see `rwv workweave delete`): one that has not landed survives, listed with its reason, and the exit status is `3`. Its dirt, untracked files included, is refused with the other refusals before anything moves. Refuses at the start when the workweave's marker, the project clone's index and its project checkout's link to that clone disagree. A no-op with a warning in the primary |
| `--keep <yours\|incoming\|text-merge>` | With `--continue`, where the VCS left regions of `rwv.toml` in conflict: `yours` or `incoming` takes that side's value for every entry in them both sides changed differently, and writes and stages the merge of both sides, what the VCS merged outside the regions standing as it merged it; refused where `rwv.toml` was edited since the stop. Where the check stopped a text merge of `rwv.toml` that differs from the merge of the values, `text-merge` marks that text merge resolved as the VCS made it and writes nothing, and `yours` or `incoming` refuses; typed at any other stop `text-merge` refuses. See [Recover from a sync conflict](../how-to/recover-from-sync-conflict.md) |
| `--continue` | Resume after resolving a conflict. Every parameter is read from the op record; `--allow-member-change` adds to its names |
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `-j N` | Parallel per-repo steps (default: 1) |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

**A target that moved.** A target or receiving branch another verb moved after the landing read it, or a checkout there switched to another branch, stops the landing as `target` `moved mid-operation`, with its op record kept; `rwv abort` restores every ref the landing moved and leaves the others where they stand, then `rwv land` judges the new tips. `--continue` refuses it.

**Exit codes:** `0` when the landing completes; `1` when it refuses or stops (op record kept; `--continue` or `rwv abort`); `3` for a retire that left a branch of the workweave.

### `rwv cherry-pick <project-rev> [-m <n>] [--except <repo>]... [--json] [--project <name>]`

Port one project commit onto the checked-out feature: what `<project-rev>` introduced, applied as new commits in each repo it moves. To port a fix into a line, run it in a feature whose target is that line, then land the feature. `<project-rev>` is any revision the project repo resolves as last fetched; one naming nothing refuses as `name` `no-such-branch`. A declared branch refuses as `target` `no target`, naming `rwv branch create <W>` and `rwv workweave create <ww>`.

The commit's `rwv.lock` is read against its parent's; for a merge commit, against the parent `-m <n>` names (from 1). A merge commit without `-m`, an `-m` naming a parent the commit lacks, and an `-m` on a commit that is no merge refuse as `port` `mainline`, the parents listed.

- **`owned` and `fork` members.** For each member whose pin the commit moves, the commits the new pin reaches and the old one does not, less those this branch already holds, are copied onto the member's branch here oldest first, each recording the commit it copies (git: `cherry-pick -x`). A copy that comes out empty, its change already here, is dropped and reported. Where the new pin is a merge whose parent `-m` names is the old pin, that merge is copied against the same parent instead, so a landing made by merge ports whole. Any other merge in the commits to copy refuses (`port` `merge in range`). The member must sit at this branch's pin (`pin` `ahead` or `unpinned` refuse, naming `rwv lock --commit`).
- **`dependency` and `reference` members** are never copied: the new pin is carried by name, as `rwv member pin` would set it, and must be on the member's declared branch or its upstream (`pin` `not on declared branch`). A pin here that already reaches it is kept. The project commit records each carried pin in its `Rwv-Repin` annotation, verb `cherry-pick`.
- **Refused per member:** a pin the commit moves backward, a diverged change included (`port` `backward`, listing the commits it drops), a member this branch does not list (`port` `not listed`), and a pin this clone lacks (`pin` `unreachable`). A member the commit adds or removes, or one its parent does not pin, has no range: its pin here stays, reported.
- **Entries are never ported.** Each change the commit made to a member entry (`branch`, `url`, `role`, `history`, `recurse-submodules`; the member added or removed) or to the top-level `history` is reported `not-ported`, with the parent's value, the commit's and this branch's, and the setter that would port it. This branch's entries and `history` stay as they are. The other top-level tables (`[integrations]`, `[workweave]`) port as content: a table only the commit changed is taken, and one both sides changed that git conflicts on stops the port with `rwv.toml` unmerged, holding this branch's tables, for you to set as wanted and stage. Where the VCS merged such a table's text with no conflict, the text is first compared with the merge of both sides' values of the commit's parent, this branch's tip and the commit, as `rwv sync` compares its text merges; where it does not parse or differs, the port stops with `rwv.toml` unmerged and the VCS's text merge in place, untouched, listing what differs. `rwv cherry-pick --continue` takes the staged `rwv.toml`; there is no `--keep` here.

**The project commit**, last, applies the commit's other project files, writes the relock (each copied member's new tip, each carried pin, each `--except` member's tip; the commit's own `rwv.lock` is never picked), and keeps the commit's message and author, recording the commit it mirrors in its `Rwv-Picked-From` annotation. A commit leaving nothing to port makes no commit and says so, so running it again after a completed port changes nothing.

Every refusal is reported before anything moves. The copies run under an op record: a conflict in a member's copy or a project file stops the port with the backend's hint up to staging; resolve and stage, then `rwv cherry-pick --continue`, or `rwv abort` to put every repo back. `--continue` rechecks each repo first: a member switched to another branch refuses (`head-not-on-branch`), and a commit the port did not make, such as one made by hand during the stop, refuses as a foreign tip (`foreign-tip`); it is never taken for a copy. A member a refusal names can be ported by hand from the commits the refusal lists, then left out of the rerun with `--except`, which records it as it stands.

| Flag | Effect |
|---|---|
| `-m <n>` / `--mainline <n>` | Pick a merge commit against its parent `n` (from 1) |
| `--except <repo>` | Leave this member out of the port, lifting its refusals: its checked-out tip is recorded as `rwv lock` records it, forward only (`pin` `backwards` otherwise); a member this branch does not list records nothing. Repeat per member. One the commit does not move refuses (`inapplicable-flag`) |
| `--continue` | Resume after resolving a conflict. Every parameter is read from the op record |
| `--json` | One JSON envelope: per member the commits copied (old and new ids), dropped and left out; the pins carried; each `--except` member's recorded tip; each `not-ported` change; the project commit |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

**Exit codes:** `0` when the port completes or there is nothing to port; `1` when it refuses or stops on a conflict (op record kept; `--continue` or `rwv abort`).

### `rwv push [...]`

Publish a weave branch from any workspace: its members' branches, then its record, then the project branch. Default scope: `owned` and `fork` repos (the roles the operator controls), the branch's record and the project branch. `dependency` and `reference` repos are pushed only when `--role` or `--repo` names them, and a selector narrows the push to the repos it names: the project branch and the record go only when `--repo projects/<name>` names the project repo. `push` never writes `rwv.lock`.

**Preconditions** (reported together before anything is pushed):
- **A recorded, named branch** — the weave branch to push (the project repo's checked-out one, or `--branch`) has a record that reads as one value (`target` `unrecorded`, `conflict`, `unreadable`: `rwv branch set-mode`), is not the minted `<project>--<workweave>` name of a workweave made without a branch (`name` `minted`), and is not a detached workspace (`alignment` `project detached`).
- **Committed project state** — `rwv.toml` and `rwv.lock` are tracked and have no staged or unstaged change (`edits` `rwv.toml or rwv.lock uncommitted`, naming `rwv lock --commit`). Push publishes the project repo's last commit and plans from the `rwv.toml` and `rwv.lock` it holds. Other uncommitted files are not part of the push: they are named on one line (`uncommitted files left out of the push`) and the push goes on.
- **Remotes confirmed** — `push` fetches every remote of the project repo and each member first, pruning. A remote that fetch could not complete, when it is a push target or the url remote a pin is judged by, refuses the push (`push-branch` `unconfirmed`, the cause listed).
- **Pins** — each member the push acts on must be on the branch the checked-out branch's record expects (`alignment`), and is judged against the committed `rwv.lock` entry. A member with no pin (`pin` `none`), a pin no ref reaches (`unreachable`), a HEAD behind the pin (`behind`) or diverged from it (`diverged`), or commits beyond the pin that no remote-tracking branch and no live lock reaches (`unpinned`, listing them) refuses, with the choice that clears it. Commits beyond the pin that are pushed or pinned by another project's live lock (`ahead`, the fact `pinned-by` naming the lock) are reported and the push goes on.
- **Merges** — for each merge in the range the project branch publishes, a member whose pins both parents changed to different commits under one declared branch must be pinned at the pushed tip at a commit containing both (`pin` `unmerged`, naming the merge), unless the tip declares the member elsewhere or a commit in the range at or after the merge, the merge's own included, records re-pinning it backward by name to a commit that does not reach the pin the merge left behind (`pin` `waived`, listing the merge, the re-pin and the verb that made it). The record is the `Rwv-Repin` trailer rwv's by-name verbs write on their lock commits ([formats](./formats.md#re-pins-by-name-in-lock-commits)): a commit made with git, a raw merge whose lock conflict was settled by hand included, carries none and waives nothing, and a trailer whose pins disagree with what its commit's lock changed waives nothing and is listed. The refusal offers the merge for that member and `rwv member pin <m> <rev> --commit`, the by-name re-pin that waives it. A member the parents declare differently is listed, not refused.
- **Where each branch publishes** — every branch goes to its push target, found in git's order: the branch's `pushRemote`, `remote.pushDefault`, the remote the branch takes in from, and last the repo's url remote (a member's, the remote that has its `url`; the project repo's, the one it was created with, else its only remote). A branch with none has no push target, and one that takes in from a branch of another name has a foreign upstream; either refuses as `push-branch` (`no push target`, `foreign upstream`) with the choice to configure where it pushes, and so does a branch that has diverged from its push branch while holding commits of its own (`push-branch` `diverged`, the commits listed), unless `--overwrite` names its repo.

Two states complete instead of refusing, and `push` prints each under its repo and leaves the repo unpushed. A branch behind its push branch is `push-branch` `behind`, with the choice to take it in (`rwv update --remote --repo <m>`). A branch whose upstream the remote deleted is `push-branch` `gone`, listing the commits only it holds, and is pushed only when `--repo` names the repo. A branch pushed with no upstream takes in from its push branch afterwards. A feature's member branch is pushed when it holds commits the target's pin for the member lacks (the pin in the target's lock at its tip, the newer of the target and its own upstream; a target diverged from its upstream refuses as `push-branch` `diverged`); otherwise the push publishes the branches the tip declares and every branch a commit in the pushed range declares. Such a branch diverged from its push branch while holding unpushed commits refuses as `push-branch` `diverged`, naming the branch.

**Order.** The members' branches, then the record, then the project branch. Each branch publishes the commit the push judged: a branch that moves while the push runs keeps its new commits for the next push. After the members are pushed the project branch is withheld (`publish` `withheld`) while a pin its lock holds is not reached as it must be: by a remote-tracking branch of the url remote or the push target for a feature's `owned` and `fork` members (where such a member's branch holds nothing beyond the target's pin, the push leaves it alone and the refusal offers `rwv push --branch <target>`), by the push branch of the declared branch for a declared branch's, and by a remote-tracking branch of the url remote for a `dependency` or `reference` member. A pin whose member is not materialized here is `pin` `unreachable`. A lock with no entry for a member the push does not act on is `publish` `no pin`. A project push the remote rejects is `publish` `withheld`; the members' branches that were pushed are published. Only the pushed tip's pins must be reached; a project commit of a declared branch's pushed range that pins an `owned` or `fork` member at a commit no branch or remote-tracking branch reaches is reported, not refused, as `pin` `held by no branch`, naming each project commit pinning it, the backups holding it, and the pushed tip's pin where that records a copy of it, where the refusal's choice publishes it under a name.

The record is published as one commit on the push target's records branch as last fetched: the branch's own entry, and the entries this machine has dropped; no other local entry, and never a conflicted or unreadable one. A remote that moved meanwhile is fetched and the commit rebuilt. A remote that refuses its records branch does not stop the push: the project branch is published without its record there, and the rejection is reported as a fact (`publish` `records rejected`, the remote's answer listed), saying that another machine opening the branch is asked once how to treat it (`target` `unrecorded`: `rwv fetch`, or `rwv branch set-mode`). The push exits 0.

| Flag | Effect |
|---|---|
| `--role` / `--repo` | Select the repos the push acts on; `projects/<name>` names the project repo. A `dependency` or `reference` repo is pushed only when one names it |
| `--branch <W>` | Push weave branch `W` instead of the checked-out one, judging each member by `W`'s record, lock and branches |
| `--overwrite <repo>` | Replace this repo's diverged push branch: its remote tip is kept as a backup (where a `rebased` backup of rwv's own rebase already holds it, that one serves, and it is dropped once the push lands), the replacement is leased to that tip, and it is refused (`pin` `would be unreachable`) while a live lock pins a commit that, once replaced, no remote-tracking branch of the repo reaches (for a published project branch's lock) or nothing reaches (for a local one). The lock of the project branch this push replaces, by a fast-forward or by its own `--overwrite`, is exempt. Never a branch some weave branch declares, nor a declared-mode project branch (`protected`). Repeat per repo; the project repo is `projects/<name>` |
| `-j N` | Parallel fetch and push (up to N concurrent for member repos; project repo always last and serial). Default: 1, because every push authenticates and runs the repo's pre-push hook: concurrent pushes would contend for one terminal's credential prompts and run those hooks side by side. The default push set is already narrowed to writable roles, so there is less to parallelize than in a fetch |
| `--json` | Structured output: one JSON envelope, whatever `-j` is |
| `--dry-run` | Print the push plan without pushing. It runs the pruning fetch the push runs, so its verdict is the push's: a refusal the push would make is refused, and a remote the fetch cannot complete refuses as `push-branch` `unconfirmed`. Nothing is written but what a fetch writes |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

`--json` emits the envelope `{"$schema": "...", "outcomes": [...], "refused": [...], "reported": [...]}`, `reported` holding the repos judged and left unpushed and the merges a re-pin by name waived. Member-repo records use `kind` `pushed`, `skipped`, or `failed`; the branch's record follows as `records-published` or `records-rejected`; the project-repo record (always last) uses `kind` `project-repo-pushed` or `project-repo-failed`. When a member's push is rejected, the project repo is withheld: `refused` lists each such member as `publish` `withheld`; a rejected project push is listed the same way. See [JSON envelope convention](#--json-envelope-convention).

See [push a cross-repo feature](../how-to/push-cross-repo-feature.md).

### `rwv abort [--abandon-foreign-tip <repo>] [--leave-target] [--discard-unreadable-record]`

Roll the operation in progress back to its savepoints in every workspace the op touched (`sync`, `land`, `pull` and `update --remote` with `--merge-upstream` or `--reset-to-upstream`, and `cherry-pick`, each of which leaves an operation record when it stops): one per ref the op may move, under `refs/rwv/pre-op/<op-id>`, and under `refs/rwv/pre-op/<op-id>-target` for a landing's target, whose checkouts share a ref store with the workweave's. A checkout's savepoint names the branch it was on, so a checkout the op switched is switched back before anything is reset, and no other branch is reset; a branch a landing moved without a checkout is put back afterwards, by compare-and-swap against the tip the landing wrote. Run it from either workspace of the op: the verdict for each repo is the same, and op record is cleared in both. Runs VCS-native abort for operations the op left in progress in the source (`git rebase --abort`, `git merge --abort`), where abort was typed in the other workspace keeping that checkout's working copy as a backup first and printing its name; a repo of the target the landing never moved is left as it stands, an operation in progress there included. Lines for the workspace abort runs in carry no prefix; lines for the op's other workspace are labeled with its side, `[target] <repo>` or `[source] <repo>`. A repo is named as the flag takes it: its workspace-relative path, or `projects/<name>` for the project repo.

Abort reads the members it restores from each workspace's working `rwv.toml`. Where one does not parse, it reads the one at the checked-out commit while that project repo is stopped in a VCS operation, else the one at the operation's savepoint in that workspace, the same for the source and a landing's target and from whichever workspace abort runs in, so an operation stopped on an `rwv.toml` it produced is always abortable. An `rwv.toml` that does not parse and is not committed is an uncommitted edit like any other: kept as a backup in the workspace abort runs in, and refusing the abort in the other workspace of a landing. Only where neither the working file nor the one at the savepoint parses does abort refuse it as something it cannot read. Every member the operation recorded moving is restored whichever file lists it.

Abort plans every write it would make, on both sides, before it makes any. Where anything blocks the plan (a tip it cannot attribute to the op, a branch gone or checked out elsewhere, uncommitted edits in a workspace abort was not run in, an operation stopped in a repo that abort did not start, a commit the landing wrote in its target that a lock other than the two abort puts back pins and that restoring the target would leave reached by no branch or backup, something it cannot read) it refuses whole as `foreign-tip`: nothing of either side is restored, the op record is kept and `--continue` stays open. The refusal names each obstacle with what clears it, then, for each way the operation can end, the command that ends it, spelled with every `--abandon-foreign-tip` the invocation already named, so the printed command run as printed ends the operation. A fix that must be made in the other workspace of a landing is printed with the workspace named (`rwv -C <workspace> abort`) only where that workspace takes the abort up. Once abort has begun, `--continue` refuses the op; a write that no longer finds what the plan judged (something moved between the plan and the write) stops there, what was already restored stays, nothing after it is done, and `rwv abort` run again completes the rest.

| Flag | Effect |
|---|---|
| `--abandon-foreign-tip <repo>` | Consent to restoring this repo over a tip abort cannot attribute to the op. Repeatable, one repo per occurrence; **no all-repos form**. Spell `<repo>` as abort's per-repo output does — the workspace-relative path, or `projects/<name>` for the project repo (the project of the op in progress; another project's repo is refused). The abandoned commits are first kept as a backup marked `abandoned`, which [`rwv backup list`](#rwv-backup-list---json) shows and `rwv backup drop` drops. Naming a repo that is not refused does nothing; consent already given is repeated by every closing command abort prints. |
| `--leave-target` | End a landing whose target something else moved since (another landing built on it, a commit, a deletion, a checkout abort may not move): the source is restored and the whole target left as it stands, each tip the landing wrote there first kept as a backup where no branch reaches it, the target's savepoints dropped. Refused, before anything moves, where abort can restore the target, for an operation that is not a landing, and where the source's own restore is blocked (the refusal names it). The target is given up (its savepoints dropped, the landing's lease and claim released) only after every step on the source side ran as planned; where one did not, nothing of the target is given up and the printed `rwv abort --leave-target` still ends the landing. |
| `--discard-unreadable-record` | End the operation whose record this rwv cannot read ([`unreadable-op-record`](./doctor-findings.md#unreadable-op-record)), run in the workspace holding the record or in one the operation leases: remove the record, the leases it holds on other workspaces and its landing claims. Nothing is moved back and no ref is reset, since only the record says which tips are the operation's. It lists each repo left stopped in a VCS operation and each savepoint and backup the repos hold. Refused where the record reads (`inapplicable-flag`), and with either other flag. |

A dependent's branch the op carried along and moved, that has since moved again, is gone, or is checked out outside the op's workspaces, refuses the whole abort before anything is put back (`foreign-tip`): a gone or checked-out branch names what to do first; a moved one offers `--abandon-foreign-tip` for each repo it moved in, and where each was only built on, the op's own `--continue`, which keeps those commits.

Refuses as `no-op-recorded` if no operation is in progress. Without the flag, a repo whose tip is unattributable is refused, not reset, and the refusal writes nothing. A restore over the op's own tips writes no backup either: what abort throws away is what the op made.

An op record this rwv cannot fully parse is still aborted when its op id, verb, project and workspaces parse. Abort says so, reads none of the tips the record holds, and attributes only a tip at its savepoint or mid-op; every other tip is refused as `foreign-tip`, for `--abandon-foreign-tip <repo>` to settle. A record where even those fields do not parse, or that another release's record schema wrote, is refused by every verb as `unreadable-op-record`, naming `rwv abort --discard-unreadable-record`.

**Exit codes:**
- `0` — all repositories restored cleanly to their savepoints and the op record removed.
- `1` — a refusal (`no-op-recorded` with nothing in progress; `unreadable-op-record`; `abort-in-its-workspace`; or `foreign-tip`, whenever the plan is blocked, `--abandon-foreign-tip <repo>` or `--leave-target` given or not), or a restore that stopped at a write that failed (`abort completed with failures`: what was restored stays, the op record is retained so `rwv abort` can be retried).

### `rwv backup`

List, drop or prune the backups rwv keeps. A **backup** is a ref an rwv verb makes before any step that would leave a commit or an uncommitted edit unpreserved: `rwv workweave delete --discard-unlanded` and `--discard-uncommitted`, and `rwv branch delete --discard-unlanded`, make one first, `rwv abort --abandon-foreign-tip` keeps the tip it gives up as one marked `abandoned`, and `rwv doctor --fix` turns a savepoint no operation owns any more into one. A backup is named `<branch>/<id>`, `<branch>` being the branch it backs up (`HEAD` for a detached HEAD), and stays until it is dropped. Each subcommand acts on the repos of the active project, its project repo (`projects/<name>`) and its members, and takes `--project <name>` to act on another project's (does not change `.rwv-active`). `drop` and `prune` refuse while an operation is in progress; `list` does not.

### `rwv backup list [--json]`

List each repo's backups, one line each: the name, the commit it holds, `[abandoned]` or `[rebased]` where the backup says so, and `[redundant]` where a branch or remote-tracking branch reaches everything it holds. Under each, the commits no branch reaches, newest first (20 at most in text; `--json` carries them all). A repo with no backup is left out. The repos are the project repo and the members `rwv.toml` declares; while a merge or replay has left `rwv.toml` unmerged, the members its checked-out commit declares. Reads refs as last fetched and contacts no remote.

`--json` emits the envelope `{"$schema": "...", "repos": [...]}`. Each repo record has `path` (as `rwv backup drop` names the repo) and `backups`; each backup has `name`, `tip`, `mark` (`rebased` or `abandoned`, absent otherwise), `redundant` and `held` (`sha`, `short`, `subject`). See [JSON envelope convention](#--json-envelope-convention).

### `rwv backup drop <repo> <name>...`
### `rwv backup drop <repo> --all`

Drop the named backups of one repo, or with `--all` every backup of that repo, printing each tip as it goes. A bare `rwv backup drop <repo>` names neither and is a usage error that drops nothing; `--all` with names is one too. `<repo>` is the workspace-relative path of a member, or `projects/<name>` for the project repo; no form drops across repos. A name `rwv backup list` does not show is reported and skipped, so a drop interrupted partway is finished by running it again.

Refused under the **pin rule** (`pin` `would be unreachable`) when it would leave a commit some project's live lock pins reached by no branch, remote-tracking branch or backup. A pin is live in every project of the weave, not only the active one: it is the commit a member's pin names in the `rwv.lock` at the tip of any local or remote-tracking branch of any project's repo. The refusal names the commits and the locks, and nothing is dropped. Dropping a backup that holds commits nothing else reaches loses them; the drop prints what it dropped, and that is the whole record.

### `rwv backup prune`

Fetch every remote of every repo of the project with pruning, then drop each backup a branch or remote-tracking branch reaches in full (`[redundant]` in the list). A remote that cannot be fetched is named and its remote-tracking branches are not counted, so a backup is dropped only on what a confirmed fetch or a local branch vouches for. Obeys the pin rule as `drop` does. Prints `nothing to prune` when no backup is redundant. A backup holding commits no branch reaches is never pruned; only `drop` removes it.

### `rwv branch`

Create a weave branch, record how one is used or how it accepts landings, list the weave branches with their records, rename one, or delete one. A weave branch is a branch of the project repo that carries `rwv.toml` and `rwv.lock`. Its **record** is `feature` (work onto a target branch, with `as` the target's own mode when the record was written) or `declared` (members on the branches its `rwv.toml` entries declare). Records are kept on a branch of the project repo of their own, written without a checkout, and every verb that aligns members, locks, lands or publishes (`update`, `pull`, `update --remote`, `lock`, `sync`, `land`, `push`) refuses a checked-out branch that has none, naming `rwv fetch` and `rwv branch set-mode`. `rwv init`, `rwv clone` and `rwv workweave create` record the branch they leave checked out. See [Formats](./formats.md#branch-records).

### `rwv branch create <W> [--target <T> | --declared] [--set-branch <m>=<b>]... [--set-new-branch <m>=<b>]... [--pin <m>=<rev>]... [--at <rev>] [--project <name>]`

Create `<W>`, write its record, and switch the workspace to it, as the VCS's own branch creation switches: uncommitted edits are carried, an uncommitted `rwv.toml` included. A bare create is a `feature` onto the checked-out branch, which must have a record that reads (`target` `unrecorded` otherwise), starting at that branch's tip as `--target` does; a feature's `owned` and `fork` members get branches `<W>` at their pins. A `declared` branch's members keep following the branches its entries declare. Every repo is judged first and nothing is written while any is refused.

| Flag | Effect |
|---|---|
| `--target <T>` | A `feature` onto `<T>`, which must have a record that reads. Starts at `<T>`'s tip: the newer of `<T>` and its upstream as last fetched |
| `--declared` | A `declared` branch, starting at the checked-out commit |
| `--set-branch <m>=<b>` | Declare the existing branch `<b>` for member `<m>`, in `<W>`'s first commit, as `rwv member set-branch` does; repeat per member |
| `--set-new-branch <m>=<b>` | Create `<b>` and declare it for member `<m>`, in `<W>`'s first commit, as `rwv member set-branch --new` does; repeat per member |
| `--pin <m>=<rev>` | Start `<m>`'s branch at `<rev>`, as `rwv member set-branch --at` does; needs a `--set-branch` or `--set-new-branch` naming `<m>`, and may be given once per member |
| `--at <rev>` | Start at this revision. An uncommitted `rwv.lock` is carried only where the branch starts at the checked-out commit; elsewhere it is refused (`edits`) |
| `--project <name>` | Operate on this project instead of the active project |

From a detached project repo a spelling is required and `<W>` starts at its HEAD. A create that starts at `<T>`'s tip is refused while `<T>` and its upstream have diverged (`push-branch` `diverged`), naming `--at <T>`. Refused with nothing created: a name any repo has, locally or on a remote as last fetched (`name` `exists`); the records branch's name or one below it (`name` `reserved`); a feature name a live weave branch declares for a member (`name` `declared`) or another project's feature uses for a member both list (`name` `feature`); an operation in progress in a repo it would move (`edits`). See [`rwv explain branch`](./explain/branch.md).

### `rwv branch set-mode [<W>] <feature|declared> [--target <T>] [--project <name>]`

Write the record of `<W>`, the project repo's checked-out branch when none is named. Where this workspace has `<W>` checked out, its members then re-align to the new record as `rwv update` aligns them; another workspace on `<W>` is not touched. `<W>` must exist locally or on a remote as last fetched (`name` `no-such-branch` otherwise). The mode is always the last word, so a branch named `declared` is still a branch when two words are given.

| Mode | Takes |
|---|---|
| `feature` | `--target <T>`, required: the branch the feature lands into, which must have a record that reads as one value. Its mode is stored as `as`. A `--target` whose chain of targets returns to `<W>`, `<W>` itself included, is refused (`target` `cycle`) |
| `declared` | no `--target` |

A `--target` with no record is refused as `target` `unrecorded`, naming `rwv branch set-mode <T>`. Recording a feature prints where it lands (`target-recorded`): into `<T>`'s member branches when `<T>` is a feature, else into each branch `<T>` declares, listing the members whose entry in `<W>` differs from `<T>`'s. A change to `<W>`'s mode lists each feature that targets `<W>` and recorded its mode differently, which refuses to land until rebound. A branch whose entry two writers set differently (`target` `conflict`) or whose entry does not read (`target` `unreadable`) is settled by this verb, which writes the one value. The records branch's own name and every name below it is refused as `name` `reserved`. Recording a feature is refused where an `owned` or `fork` member's branch of its name is one a live weave branch declares (`name` `declared`) or one another project's feature of that name uses for a member both list (`name` `feature`).

Each workspace keeps the record it last saw for each branch. When the checked-out branch's record differs from it and this workspace did not write the change, because it arrived by fetch or another workspace of this clone wrote it, `status`, `update`, `pull`, `switch`, `push` and `land` report it as `target` `record changed`, with the old and the new value and where it came from, and `rwv sync` refuses on it before anything is fetched or moved. `rwv update` accepts the change, storing the new record; `rwv branch set-mode <W> feature --target <T>`, or `declared`, restores the old value. A record this workspace writes (`branch set-mode`, `branch create`, `branch rename`, `workweave create`) is stored as it is written and never reported to it, and a workspace that has stored no record for the branch stores the one it reads without a report.

### `rwv branch set-history <linear|merge|ff-only|--unset> [--repo <path>] [--project <name>]`

Record how the checked-out weave branch accepts landings: its top-level `history`, or with `--repo` one member's, which overrides the top-level one for that member. `--unset` removes the key; a member whose own key is removed follows the top-level one again. No value is the default and no role implies one, so a project whose `rwv.toml` names none has the setting unset, and no verb writes one but this. A value the file already holds is not rewritten, and is committed when the commit lacks it, so a hand edit is finished by running this.

| Value | Taking the target's work in | Landing |
|---|---|---|
| `linear` | fast-forward, else rebase onto it | fast-forward, else rebase onto the target, then fast-forward |
| `merge` | fast-forward, else a merge commit | fast-forward, else a merge commit |
| `ff-only` | as `linear` | fast-forward only |

| Flag | Effect |
|---|---|
| `--unset` | Remove the setting instead of writing one |
| `--repo <path>` | The member whose own setting to write; `projects/<name>`, this project's repo, names the top-level setting, as no `--repo` does. Another project's repo, and a member the manifest does not list, are refused |
| `--project <name>` | Operate on this project instead of the active project |

The change is committed alone, as `member set-role`'s is: any other uncommitted edit to `rwv.toml` stays uncommitted, and so do uncommitted changes to other tracked files of the project repo, named in one line. Refused before anything is written: a branch with no record that reads (`target`), a detached project repo (`alignment` `project detached`), and an operation in progress. A value other than the three is refused by the argument parser, and in a hand-edited `rwv.toml` by every verb that reads the file. A `history` one project's live weave branches record for a member another project records differently is reported by `rwv doctor` as [`history-disagreement`](./doctor-findings.md#history-disagreement).

### `rwv branch list [--json] [--project <name>]`

List each weave branch, local or only on a remote as last fetched, with its record: `declared`, `feature onto <T> (as <mode>)`, `unrecorded`, `conflict`, `unreadable` or `cycle`. `*` marks the checked-out branch; `[unpublished]` marks an entry no remote's records, as last fetched, hold; `[target gone]` a target that exists nowhere; `[remote only]` a branch with no local copy. It reads refs as last fetched and writes nothing: the records of the remotes are merged in memory, not into the local records branch.

`--json` emits the envelope `{"$schema": "...", "branches": [...]}`. Each branch has `name`, `checked_out`, `local`, `record` (an object whose `state` is `declared`, `feature` with `target_branch` and `as`, `unrecorded`, `conflict` with `values`, `unreadable` with `reason`, or `cycle` with `chain`), `published` (absent for an unrecorded branch) and `target_gone`. See [JSON envelope convention](#--json-envelope-convention).

### `rwv branch rename <new> [--adopt] [--project <name>]`

Rename the project repo's checked-out branch to `<new>`. A declared-mode project branch is renamed like any other: protection bars only its rewrites. In a feature, the same-named branch of each `owned` and `fork` member that has one is renamed too, except one a live weave branch declares: that branch is not the feature's and stays where it stands, and the member's branch `<new>` starts at its tip, the member's checkout here moving onto it (reported as `branched <repo>: <new> starts at <old>`, naming the declaring branches). A declared-mode branch's members sit on the branches `rwv.toml` declares and are not touched. Each renamed branch stops taking in from the remote branch it did, and each old remote branch is reported on stderr as `left: <repo> <old>: <remote> still has it`, with the `rwv branch delete <old> --target <new> --delete-remote` that deletes it once landed. The record, its stored entries, every record that lands into `<old>` (retargeted to `<new>`, and published with a push of that branch) and the workspace's aligned branch move with it in one commit; nothing is left under the old name. A run that stopped partway is completed by running it again.

| Flag | Effect |
|---|---|
| `--adopt` | Take up a branch a remote already has under `<new>`, a rename made on the forge or on another machine: each renamed branch whose push target has `<new>` takes in from it, and the project branch must have one. Refused where the two have diverged (`push-branch` `diverged`, the unpushed commits listed) |
| `--project <name>` | Operate on this project instead of the active project |

Refused, before anything changes: a branch with no record that reads (`target` `unrecorded`, `conflict`, `unreadable`); a `<new>` that exists as a local branch of any repo the rename moves, or on a remote as last fetched without `--adopt` (`name` `exists`), or that holds a record of a value other than the one moved (`name` `exists` without `--adopt`; with it, `target` `differs` listing both values); a feature `<new>` that a live weave branch declares for a member (`name` `declared`) or that another project's feature uses for a member both list (`name` `feature`); the records branch's name or a name below it (`name` `reserved`); a branch checked out in another workspace (`checkout`); a record that lands into `<old>` and does not read (`target`); a detached project repo. See [`rwv explain branch`](./explain/branch.md).

### `rwv branch delete <W> [--target <T>] [--discard-unlanded] [--delete-remote] [--delete-remote-unlanded <repo>]... [--json] [--project <name>]`

Delete `<W>` and the branches that belong to it, by the deletion rule: the project branch `<W>` (which may exist only on a remote, as last fetched) and, in each member `<W>`'s `rwv.toml` lists, the branch of the same name that no live weave branch declares. A declared branch never belongs. A branch that belongs is deleted when everything on it has landed on the branch `<W>` lands into, no live lock pins a commit that only it reaches, and, for the project branch, its published copy has landed and no member branch that belongs survives. Members are judged first. A branch the rule keeps survives with its record and is reported with its reason (`unlanded`, `pinned`, `open-head`, `members`, `unconfirmed`); the exit status is then `3`.

| Flag | Effect |
|---|---|
| `--target <T>` | The branch whose commits count as landed; the record's target when omitted, and required for a branch that is not a feature (`target` `no target`). `<T>` needs a record that reads |
| `--discard-unlanded` | Also delete a branch that has not landed, after keeping its tip as a backup. Lifts the landed clause only: a pinned branch survives |
| `--delete-remote` | Also delete each push branch of what belongs that has landed on the target's remote-tracking branch: its upstream, never another remote's branch of that name |
| `--delete-remote-unlanded <repo>` | Also delete that repo's push branch, landed or not, an open pull request's head included, after keeping its tip as a backup, and kept (`pinned`) while a published lock pins a commit no other remote-tracking branch reaches. One repo per occurrence, as `rwv backup drop` names it; needs `--discard-unlanded`; never the push branch of a declared-mode project branch (`protected` `declared mode`) |
| `--json` | Emit one envelope with `branch`, `target`, `deleted`, `survivors` and `records_dropped` |
| `--project <name>` | Operate on this project instead of the active project |

Refused before anything changes: a `<W>` checked out in any workspace (`checkout`) and a `<W>` other branches' records land into (`target` `dependents`, one line each). The rule fetches only the remotes the branches have an upstream or a push branch on; a branch on a remote that could not be fetched is kept (`unconfirmed`) and nothing on it is deleted remotely, and a branch never published is judged on local refs. Remote deletes are leased to the tip the last fetch saw; a remote that does not take one keeps the branch (`unconfirmed`, its answer listed) and the rule goes on. A branch is never landed on a branch of its own name. A record whose branch is gone locally and from every remote, with no workspace of this machine aligned for it, is dropped in the same write.

`--json` emits the envelope `{"$schema": "...", "branch": "...", "landed_on": "...", "deleted": [...], "survivors": [...], "records_dropped": [...]}`. Each `deleted` record has `path`, `branch`, `site` (`local` or `remote <name>`), `because` and `backup` when one was kept first; each survivor has `path`, `branch`, `reason` and `detail`. See [JSON envelope convention](#--json-envelope-convention).

### `rwv switch <W> [--project <name>]`
### `rwv switch --detach <rev> [--project <name>]`

Put the project repo on `<W>` and align the members to it, or detach the workspace at `<rev>`. A recorded branch reopens bare. An unrecorded branch is refused (`target` `unrecorded`, naming `rwv fetch` and `rwv branch set-mode`): record it, then open it. `<W>` may exist only on a remote as last fetched. Opening a feature is refused where an `owned` or `fork` member's branch of its name is one a live weave branch declares (`name` `declared`) or another project's feature of that name uses (`name` `feature`); a collision that arrives after the feature was opened refuses the verbs that align or publish it the same way, naming `rwv branch rename` and `rwv branch set-mode <W> declared`. `rwv switch` with no branch is refused: aligning members for the checked-out branch is `rwv update`.

| Flag | Effect |
|---|---|
| `--detach <rev>` | Detach the project repo at `<rev>` and every member at the pin the lock holds there; no record is written |
| `--project <name>` | Operate on this project instead of the active project |

The project repo moves as the VCS moves it, carrying edits the move does not overwrite. Every repo it would move is judged before anything moves: refused are a branch checked out in another workspace (`checkout`), an operation in progress or edits the move would overwrite (`edits`), an uncommitted `rwv.toml` or `rwv.lock` (`edits`, naming `rwv lock --commit`), and a member that cannot move (`alignment`). From a detached project repo, what its HEAD holds that no branch reaches is kept as a backup first. A branch whose `rwv.toml` an earlier release wrote is switched onto and the run stops before the members (`unparseable-project` `here`). See [`rwv explain switch`](./explain/switch.md).

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
| mid-op | `mid-rebase` / `mid-merge` / `mid-cherry-pick` / `mid-revert` / `mid-bisect` when the repo is stopped in that operation; absent otherwise |

`--json` emits the envelope `{"$schema": "...", "repos": [...]}`. Each record includes `lock_pinned_by_name` (`true` when an older or hand-edited `rwv.lock` entry holds a tag or branch name rather than a 40-character hex commit SHA; `rwv lock` rewrites it to a commit SHA). See [JSON envelope convention](#--json-envelope-convention).

Below the table, `status` prints:

- the branch each project's workspace last aligned its members for (`<project> aligned for <branch>`, `aligned detached`, or `aligned for no branch`);
- one `alignment` line for each member that is not in its expected state, with the commits that a move off its branch would leave, and one for a member carrying the fact `checkout` (its branch is checked out in another workspace, so it sits detached at its pin); for the project repo, `project moved`, `project unaligned` or `project detached` against the aligned branch;
- `target` `record changed` for the project repo's checked-out branch when its record changed since this workspace last saw it, and not by this workspace (see `rwv branch set-mode`);
- one `pin` line for each member whose HEAD is not at the commit the lock pins for it: `none` (the lock has no entry), `unreachable`, `behind`, `diverged`, `unpinned` (commits beyond the pin that nothing else holds, listed) or `ahead` (commits beyond it that are pushed or pinned by another project's live lock, with the fact `pinned-by`);
- one `push-branch` line for each member whose checked-out branch needs attention: `behind` (take it in with `rwv update --remote --repo <m>`), `diverged`, `gone`, `foreign upstream` and `no push target`. A branch that is `ahead` of its push branch is the ordinary state of work in progress and has no line. A `gone` line appears only while the branch holds commits no remote-tracking branch reaches, and lists them: a branch whose upstream was deleted after its work landed is not marked;
- one `operator branch` line for each local branch of a member that holds unpushed commits nothing else accounts for: it is checked out nowhere, is named after no recorded weave branch of any project here, and is no declared branch of that member. The line gives the member, the branch and its count of unpushed commits, and notes a gone upstream; it prints no choice, because such a branch is legal;
- the backups each repo holds (`backup <repo> <name> <tip>`), the listing `rwv backup list` gives.

The lines about a member follow the selector. The lines about the project (`aligned for`, `project moved`, `project unaligned`, `project detached`, `record changed`) print only when no `--role` or `--repo` is given. It reads refs as last fetched, so `rwv fetch` first. `--json` carries the same as `aligned`, `judged` (each line with its `axis`, `state`, `facts` and `choices`; empty when none stands apart), `operator_branches` (with the unpushed `commits`) and `backups`.

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

Convention and health audit across clones, `rwv.toml`, `rwv.lock`, workweaves, op record leases, git attributes/merge drivers, and enabled integrations.

A repo whose alignment or pin `rwv doctor` cannot read is an error of the run: it is printed as an error naming the checkout and the cause, after every finding the run did establish, and the run exits non-zero. It is never read as no finding.

| Flag | Effect |
|---|---|
| `--locked` | Zero exit iff every repo tip matches its lock entry |
| `--fix` | Repair every finding marked **Auto-fixable** in [Doctor findings](./doctor-findings.md), which carries that mark on each finding it documents. Never touches live changes marked for commit or live edits. Idempotent. |
| `--json` | Emits envelope `{"$schema": "...", "violations": [...], "issues": [...], "advisories": [...], "plugins": [...]}`: rwv's own findings on `violations`, integration findings on `issues` |
| `--all` | Scan all projects and run weave-wide checks (orphan detection, cross-project pins, etc.). By default only the active project is checked |
| `--abandon-lost-pins` | Refused without `--fix`. With it, relink a workweave checkout whose link dangles and whose pinned revision and branch are in no clone onto the workweave's branch created at the canonical clone's HEAD, keeping every file: the pinned commits are given up, and their changes stand as edits. Without this flag, `--fix` reports the checkout |
| `--merge-edited-drift` | Refused without `--fix`. With it, in each checkout of this workspace whose branch moved under it and that holds files both that move and an edit here changed, or something standing where the move puts a path (`edits` `moved and edited`), keep the working copy and the VCS's record of it as a backup and print its id, point HEAD at the tip the files hold, and have the VCS merge every edit onto the branch (for git, `git checkout -m <branch>`), refreshing the move's other files. A file the merge does not resolve is left conflicted, refused as `edits` `unresolved` until resolved; a file declared derived takes the moved tip's content, its edit kept in the backup. A file the VCS does not compare (ignored, assume-unchanged, skip-worktree) or something in the way of the move refuses the flag with the command or move-aside that clears it. Where the VCS refuses, its words are printed and the checkout is left `edits` `set back`, which the flag run again finishes. Another workspace's checkout is refused with its `rwv -C <workspace>` line. Without this flag, `--fix` writes nothing to such a checkout and refuses it; where no checkout holds such a file, the flag refuses as `inapplicable-flag` |
| `--set-aside-edited-drift` | Refused without `--fix`. With it, in each checkout `--merge-edited-drift` would merge, point HEAD at the tip the files hold, have the VCS set the listed files' edits aside (for git, `git stash push` of those files, a stash based on that tip), and check the branch out, so each file holds the moved tip's content and the move's other files are refreshed. The report prints the command that merges the edits back onto the moved tip (for git, `git stash apply <set-aside>`). Refused where `--merge-edited-drift` is, and an entry only intended for adding that the VCS's set-aside refuses leaves the checkout `edits` `set back` |
| `--release-unreadable-claims` | Refused without `--fix`. With it, release every landing claim and takeover guard this run reports as [`claim-unreadable`](./doctor-findings.md#claim-unreadable) and that still holds what it held when reported: a landing of another rwv release that holds one loses its claim. Without this flag, `--fix` reports them |
| `--release-unreachable-claims` | Refused without `--fix`. With it, release every fetch, workweave-index and owned-digest claim this run reports as [`claim-holder-unreachable`](./doctor-findings.md#claim-holder-unreachable): a claim on a filesystem with no file locks whose holder is on a host this one cannot reach, and that loses its claim if it still runs. Without this flag, `--fix` reports them. The refusal a fetch, pull or workweave edit prints for such a claim names this command |
| `--release-unreadable-claims-on <TARGET>` | Refused without `--fix`, and with `--all`. With it, release the landing claim and takeover guards on this target branch of the project (`--project`, else the active project) that this run reports as [`claim-unreadable`](./doctor-findings.md#claim-unreadable) and that still hold what they held when reported, and no other claim: a landing of another rwv release that holds one loses its claim. The refusal a landing prints names this command. Mutually exclusive with `--release-unreadable-claims` |
| `--kind <kind>` | Filter the report to the named finding kind (repeatable; text and `--json` both). Kind names are the wire spellings `--json` emits, for `violations` and `issues` alike (e.g. `alignment`, `orphaned-savepoint`, `tool-missing`); a filter naming only `violations` kinds excludes every integration issue. An unknown name refuses (`unknown-finding-kind`), listing the valid set. Kinds whose classes normally collapse to a per-class count line render itemized under the filter — this is the drill-down. The exit code reflects the filtered view, text and `--json` alike: non-zero on any finding in the view, warnings included, so `rwv doctor --kind tool-missing` (or `--json --kind tool-missing`) fails a CI job on a missing tool |
| `--project <name>` | Operate on this project instead of the active project (does not change `.rwv-active`) |

Without `--kind`, the exit code is non-zero when any finding is an error, and a warning leaves it at `0` — text and `--json` alike. `--fix` exits non-zero when a repair it attempted failed or refused, or an error finding remains.

See [Doctor findings](./doctor-findings.md) for the complete catalog of wire `--kind` / `sub_kind` tokens, their severities (`error` vs `warning`), and their `--fix` dispositions.

### `rwv workweave create <name> [--new-branch <W> | --branch <W> | --detach <rev>] [--target <T> | --declared] [--set-branch <m>=<b>]... [--set-new-branch <m>=<b>]... [--pin <m>=<rev>]... [--at <rev>] [--dir <path> | --basedir <path>]`

Create an isolated workweave for the project the invocation resolves to (see [Project resolution](#project-resolution)); `--project <name>` names another. The workspace the verb runs in is the new workweave's parent and provenance only: it does not switch, and what it holds uncommitted, `rwv.toml` and `rwv.lock` included, stays there as it is, reported. The workweave starts from commits: the parent's checked-out commit, or `--at`, or the tip of the `--target` branch, with each member at the pin of that commit's `rwv.lock`. To fork a workweave from the one you are in, run `rwv workweave create <name>` inside it.

With no branch flag the project repo is on a feature under the minted name `<project>--<name>`, onto the branch the parent has checked out; the minted name is never published. `--new-branch <W>` creates `W` as `rwv branch create` does (bare: a feature onto the parent's branch; `--target <T>`: onto `T`; `--declared`), refusing a name any repo already has (`name` exists). `--branch <W>` opens `W` by its record; a branch with no record needs `--target` or `--declared`, and a spelling that differs from the record is refused (`target` conflict). A branch checked out in another workspace is refused (`checkout`); `--detach <rev>` makes a detached workspace at any revision, such a branch included, and writes no record. Each `--set-branch` and `--set-new-branch` sets a member's declared branch in the new branch's first commit, as `rwv member set-branch` does; every one is judged before anything is written.

Re-running an identical spelling on a clean workweave whose marker matches returns it, reported; any difference is refused (`name` taken) and changes nothing.

1. **Manifest worktrees**

1. **Manifest worktrees** — creates a git worktree for every member repo, on the project's branch name for an `owned` or `fork` member of a feature and detached at its pin otherwise (`owned`, `fork`, `dependency`, and `reference`), with `reference` repos' tracked regular files set read-only (`chmod -w`, preserving symlinks).
2. **Project worktree & marker** — creates the project worktree at `projects/<project>/` on the workweave's branch (or detached) and writes `.rwv-workweave` at the workweave root.
3. **Activation** — surfaces workweave-root symlinks, fits host settings such as the cargo `rustc-wrapper` key, runs the integration install hooks (`npm install`, `uv sync`, `cargo fetch`, …) in the new workweave, then verifies the integration files in `projects/<project>/`: a missing or drifted one is reported as a warning naming `rwv doctor --fix`, and `create` authors none. `--no-materialize` skips the hooks; `rwv materialize` inside the workweave runs them later.

A failure before the install hooks rolls the workweave back. An install hook that fails after the workweave is created and registered does not: the workweave is kept, and `create` exits 1 with `workweave created at <path>; only its install hooks did not complete. Fix the cause, then run rwv materialize inside it`. Under `--hook-mode` and `--claude-hook` the same failure is a warning on stderr and `create` exits 0, so the agent runtime that asked for the workweave still gets its path. A missing tool is a `tool-missing` warning in every mode.

| Flag | Effect |
|---|---|
| `--new-branch <W>` | Create `<W>` for the workweave, as `rwv branch create` does |
| `--branch <W>` | Open `<W>` by its record |
| `--detach <rev>` | A detached workspace at `<rev>`; no record is written |
| `--target <T>`, `--declared` | How a created branch is used, as for `rwv branch create`; refused with `--branch` |
| `--set-branch <m>=<b>`, `--set-new-branch <m>=<b>`, `--pin <m>=<rev>` | As for `rwv branch create`; with `--new-branch` or the minted name only |
| `--at <rev>` | Start the created branch at this revision |
| `--dir <path>` | The exact path to place the workweave at; there is no default (recorded in the index as the absolute path it resolves to). Absolute paths are used as-is; relative paths resolve against the invocation directory. Refused together with `--basedir` |
| `--basedir <path>` | The directory to place the workweave under for this invocation, in place of the recorded default: the workweave lands at `<path>/<project>--<name>`. Relative paths resolve against the invocation directory. Refused together with `--dir` |
| `--project <name>` | Create the workweave for this project instead of the active project (does not change `.rwv-active`) |
| `--no-materialize` | Skip the install hooks in the new workweave; `rwv materialize` inside it runs them later |

Workweaves live at `<basedir>/<project>--<name>/` where `<basedir>` is recorded per-project in the workweave index the project clone keeps (machine-local JSON, in the directory the clone's worktrees share — never in the project's working tree). The default basedir is `<parent-of-primary>/.workweaves`. Set it explicitly with `rwv workweave set-default-create-basedir <path>`; `create` records new entries into the index; every `find`-direction verb (list, delete, sync targets by bare name) resolves via the recorded `name → path` entries with `.rwv-workweave` marker round-trip validation. Each entry also keeps the workspace the workweave was forked from, so a lost or edited marker is restored from the index. Doctor reconciles the index against on-disk state — stale entries are pruned or their marker restored, orphan workweaves are adopted. An index at `projects/<project>/.rwv-workweave-index`, where earlier releases kept it, refuses these verbs as `unreadable-workweave-index` until `rwv doctor --fix` rebuilds it.

### `rwv workweave set-default-create-basedir <path>`

Record the default basedir for the project: the directory `rwv workweave create` places new workweaves under. Writes the `basedir` of the project's workweave index. Absolute paths are used as-is; relative paths resolve against the invocation directory. Existing registry entries are preserved. `--dir` and `--basedir` on `create` are unaffected. An explicit, recorded, audit-visible act, not ambient process state. `--project <name>` records it for that project.

### `rwv workweave delete <name> [--discard-uncommitted] [--discard-unlanded] [--delete-remote] [--delete-remote-unlanded <repo>]... [--json]`

Delete a workweave, and apply the deletion rule (see `rwv branch delete`) to its aligned branch when that is a feature. Refuses, before it changes anything, if any checkout under it is dirty (`edits`), if a detached HEAD holds commits no branch reaches (`alignment` `detached with unpushed commits`), or if other branches' records land into its branch (`target` `dependents`). It also refuses, restoring nothing, when the workweave's marker, the project clone's index and its project checkout's link to that clone disagree: the refusal names `rwv doctor --fix`, which restores the marker or the entry. `--discard-uncommitted` waives the first refusal and `--discard-unlanded` the second; `--discard-unlanded` also deletes a branch of the workweave that has not landed. The `edits` refusal prints `rwv workweave delete <name> --discard-uncommitted` among its choices. `--project <name>` selects the project whose workweave it deletes.

A branch that has not landed is not a refusal: the workweave is removed, the rule keeps the branch, and the exit status is `3`. The output lists each survivor with its reason, and `--json` carries them in `survivors`.

Neither discard flag discards for good: each keeps what it consents to losing as a [backup](#rwv-backup) before anything is removed, and a backup that cannot be written stops the delete with nothing changed. `--discard-uncommitted` keeps each checkout's uncommitted edits as a commit on its HEAD (untracked files included, ignored ones not). `--discard-unlanded` keeps the commits a detached HEAD holds that no branch reaches and the tip of each branch of the workweave it deletes. The report lists what was discarded by kind (edits, detached commits, unlanded branches); `rwv backup list` shows the backups afterwards.

`--delete-remote` also deletes each push branch of what belongs that has landed on the target's remote-tracking branch, and `--delete-remote-unlanded <repo>`, needing `--discard-unlanded`, also deletes the named repo's push branch whether or not it landed, after keeping its tip as a backup. A push branch of a repo no flag names that has not landed is kept.

Both refusals cover every checkout under the workweave, not only the members the project's manifest names: a member replay could not prune, one the workweave added that has not landed, or a clone put there by hand. Such a checkout is listed with the suffix `(not a manifest member)`.

`--json` emits the envelope `{"$schema": "...", "project": "...", "discarded": {"edits": [...], "detached_commits": [...], "unlanded_branches": [...]}, "deleted": [...], "survivors": [...], "records_dropped": [...]}`; the last three are the deletion rule's, as `rwv branch delete` emits them, and are empty when the workweave's aligned branch is not a feature.

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
| `3` | The verb completed and left part of the work to you. [`rwv clone`](#rwv-clone-source-) ends so when a member is left with no pin or at a pin no ref reaches; its report and `--json` envelope list each member. [`rwv branch delete`](#rwv-branch-delete-w---target-t---discard-unlanded---delete-remote---delete-remote-unlanded-repo---json---project-name), `rwv workweave delete` and `rwv land --retire` end so when the deletion rule keeps a branch; the output and `--json` `survivors` list each with its reason. |

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
| `update`, its project already active, when it cloned a missing member or realigned one to its pin | ``[skipped] core: the install hooks did not run (project `<project>` is the one presented here, and aligning its members runs none); `rwv materialize` runs them`` (with `--project` naming a project the root does not present, the `rwv activate <project>` form above) |
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

Use case: agent harness asks "what flags does `rwv push` take, and what does it print?" — the bundle is authoritative. For `--json`-capable verbs (`status`, `doctor`, `clone`, `fetch`, `pull`, `update`, `sync`, `land`, `push`), the JSON Schema is embedded as a fenced code block inside the bundle.

The rendered bundles are checked in at `docs/reference/explain/` for offline browsing.

## Project resolution

Project-scoped verbs (`member add`, `member remove`, `member set-role`, `member set-recurse-submodules`, `workweave create`, `workweave delete`, `workweave list`, `workweave set-default-create-basedir`, `lock`, `update`, `pull`, `fetch`, `push`, `sync`, `land`, `status`, `log`, `doctor`) pick their target project via a fixed chain, highest priority first:

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

Changing into `projects/<name>/` does not select that project. When `.rwv-active` selects a different one and no `--project` is given, a verb that writes into its project — `member add`, `member remove`, `member set-role`, `member set-recurse-submodules`, `workweave create`, `workweave delete`, `workweave list`, `workweave set-default-create-basedir`, `lock`, `update`, `pull`, `fetch`, `push`, `sync`, `land`, `doctor --fix` — refuses (`cwd-in-inactive-project`) rather than write into the active project from the other's directory:

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
| Project-scoped | `member add`, `member remove`, `member set-role`, `member set-recurse-submodules`, `workweave create`, `workweave delete`, `workweave list`, `workweave set-default-create-basedir`, `lock`, `update`, `pull`, `fetch`, `push`, `sync`, `land`, `status`, `log`, `doctor` (project-scoped checks) | Full chain; target line when the pointer decides |
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
| `rwv land --json` | `outcomes` |
| `rwv push --json` | `outcomes` |

The `sync` and `land` envelopes also carry `complete`: `true` when the op ran its last phase and cleared the op record, `false` when a phase parked it.

### Refused repos under `--json`

A refusal that judged repo states carries them as records, one per repo and axis:

```json
{ "path": "github/acme/lib", "axis": "edits", "state": "dirty", "listed": ["src/lib.rs"], "facts": [], "choices": [ { "id": "C13", "phrase": "commit your edits, or set them aside", "where": null, "run": ["git -C /work/github/acme/lib commit", "git -C /work/github/acme/lib stash push"] } ] }
```

- `axis` is the kind `rwv explain <axis>` serves; `state` is named as that page names it, and `choices` are the ones it lists for the state, in its order, with the commands spelled out under `run`. `where` is the directory a choice runs in when that is not where the verb ran, else `null`. `path` is the repo's manifest path, `projects/<name>` for the project repo; `workspace`, present only when the repo was judged in another workspace than the verb's (a landing's target), is that workspace; `subject`, absent when the state names nothing, is what it names (the operation in progress, the name taken).
- `rwv push --json` and `rwv sync`'s and `rwv land`'s envelopes carry them in `refused`, empty when nothing was refused on a repo's state; a `prune-refused` member record carries the checkout's in its own `refused`.
- A verb that refused on repo states before printing anything prints the refusal envelope instead, `{ "$schema": "<url>", "refused": [...] }`, schema at `docs/reference/schemas/refusal.json`.

Schemas live at `docs/reference/schemas/<verb>.json` and are also embedded as fenced code blocks inside the corresponding `rwv explain <verb>` bundle.

### The envelope and `-j`

`-j` changes how a verb runs, never what `--json` prints: the envelope is the same under `-j 1` and `-j N`, printed once the verb is done, with its per-repo records sorted by repo path (`push`'s project-repo record last). Nothing streams on stdout. For per-repo progress as each repo finishes, use [`rwv each`](./rwv-each.md)'s `--joblog` and `--results`.

Exit semantics under `--json` match the text mode: non-zero when at least one per-repo outcome is `failed`, when a refusal stopped the verb, and for `sync` and `land` when a checkout replay could not prune is left on disk. The envelope still prints in that last case. A refusal's text always goes to stderr, under `--json` too.

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
