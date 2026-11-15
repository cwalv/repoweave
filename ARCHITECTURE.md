# repoweave architecture

The referenceable module and ownership model for `rwv`: what each module owns,
how one invocation gets from argv to a verb, and where the durable state lives.
This document owns the *cross-module* facts no single file can state. It links
out — rather than restating — for the user-facing contracts, which live in
`docs/` and are the published surface.

Every mechanism claim below names the symbol it is about and the file that
holds it. It does not carry line numbers, and that is a decision rather than an
omission: this document used to cite `file:line` against "the tree at the time
of writing", a base no reader could check, and the numbers rotted. Measured
before they came out, 18 of the 21 coordinates that could be checked
mechanically were wrong — some by more than a thousand lines — while the symbol
named beside each one was right in every case. A name survives the edit that
moves it; a number does not, and a number nobody can date cannot even be shown
to be stale.

**Counts in this document are verified at commit `8e727620`.** Those are the
claims a symbol cannot carry — how many modules there are, how many of a thing
exist — so they are dated instead. A count here means "true at `8e727620`";
re-derive before trusting one against a later tree. File lengths are not
stated: they moved by thousands of lines within one release.

The residue, stated where the rule is: a symbol that is renamed leaves this
document wrong in a way that greps clean, because the old name simply stops
occurring. No gate reads this file for symbol names — `check_doc_symbol_refs`
scans comments in `.rs` sources, not markdown — so that check is a human one.

## 1. What rwv is

rwv is a single-shot CLI that manages a **weave**: a directory tree holding many
independently-versioned repositories, plus one or more *projects* that declare
which of those repositories they use and at which revisions. A project is a
manifest (`rwv.toml`) plus a lock (`rwv.lock`), both committed to a *project
repo*; the members are ordinary clones on disk. The verbs converge disk to that
committed pair, move it forward, or fan work out into parallel checkouts
(*workweaves*).

Three properties shape everything below:

- **No daemon, no persistent process.** Every invocation resolves its own
  context from the filesystem and exits. There is no server, no IPC, and no
  in-memory state that outlives the process.
- **The filesystem is the only persistence layer.** Durable state is a small
  set of marker and record files (§4) plus the version control system's own
  refs. Nothing rwv knows is held anywhere else.
- **The VCS is a subprocess, not a library.** `Cargo.toml` declares no `git2`
  and no `gix`; every VCS operation shells out through one command builder
  (`git_command`, `src/git.rs`).

Because there is no process to crash, crash recovery is a *state* question, not
a supervision one: a multi-repo operation that dies mid-flight leaves a record
on disk that a later invocation reads and resumes (§5).

## 2. Module map

One library crate (`repoweave`, `src/lib.rs`) and three programs. `src/lib.rs` is
a flat list of 81 `mod` declarations at `8e727620` — there is no internal layering
enforced by the module tree, so the grouping below is by role, not by
visibility.

| Program | Source | Declared |
|---|---|---|
| `rwv` | `src/main.rs` | `[[bin]]` in `Cargo.toml` |
| `rwv-each` | `src/bin/rwv-each/main.rs` | `[[bin]]` in `Cargo.toml` |
| `generate-explain` | `src/bin/generate-explain.rs` | `[[example]]` in `Cargo.toml`, so `cargo install` never installs it |

`src/main.rs` is a shim: its whole body runs
`repoweave::cli::dispatch::run_and_report` on a thread with one stack size on
every platform. It holds no logic, and that is load-bearing and documented at
`src/main.rs`: a `[[bin]]` is a separate crate from the `[lib]`, so any logic
placed there could only reach the library through `pub` items. Keeping it empty
is what lets the consent tokens in `src/cli/consent.rs` stay
`pub(in crate::cli)` (§6.1).

### Entry and addressing

| Module | Owns |
|---|---|
| `cli` (`src/cli.rs`) | The clap type tree: `Cli` and `Commands`. |
| `cli::consent` (`src/cli/consent.rs`) | The consent tokens and the `DriftConsent` either/or enum, each token minted only from its named flag by a `pub(in crate::cli)` `from_flag`. |
| `cli::operand` (`src/cli/operand.rs`) | `Operand`, a path argument as argv spelled it, and `InvocationDir`, the resolved directory it is read from; `Operand::locate` and the `InvocationDir` mint are `pub(in crate::cli)`. |
| `cli::dispatch` (`src/cli/dispatch.rs`) | `run()` — argv interception, parse, the single resolution point, and the verb match. |
| `workspace` (`src/workspace.rs`) | `WorkspaceContext` and its `resolve`; `Checkout`; `Resolution`; the `.rwv-active` / `.rwv-workweave` marker constants. `WorkweaveMarker` (`src/workspace/marker.rs`) and `RootObservation::require_exclusive` with the identity payloads it produces (`src/workspace/root_identity.rs`) each sit in a child module holding nothing else that sets their private fields, so the compiler admits one literal. |
| `registry` (`src/registry.rs`) | Host-to-local-prefix mapping. `Registry` trait; `CloneUrl`, the only URL a `Vcs` method hands git, constructed only here and compared only by `names_same_remote`; `resolve_to_clone_info`, which turns a manifest `url` or CLI source, shorthand included, into one; `recorded_remotes`, which reads a clone's configured remotes as ones. Unrelated to integrations despite the name. |
| `selector` (`src/selector.rs`) | The shared `--role` / `--repo` filter grammar. |
| `member_selection` (`src/member_selection.rs`) | `MemberSelection`: the members `--role` and `--repo` name, less the `reference` members under `--no-reference`. Built once where the flags parse, and asked by every step of a verb, so no step covers a member another step left out. |
| `exit_status` (`src/exit_status.rs`) | `CompletedWithRemainder`, the error that ends a verb with exit status `3`: it finished, and part of the work is left to the operator. `run_and_report` in `src/cli/dispatch.rs` maps it to the status after the verb has printed what it left. |

### Data model

| Module | Owns |
|---|---|
| `naming` (`src/naming.rs`) | The flat-address grammar — the `--` weave separator and the `+` segment escape — and the name types it constrains: `ProjectName`, `WorkweaveName`, `RefNameError`, `validate_ref_name`, the one-segment renderings `ProjectName::flat` and `WorkweaveId::flat` (typed `FlatProjectName` and `FlatAddress`, which nothing else can construct), and the two seams that recover an identity from what it renders — `workweave_name_in` for a caller holding the project, `resolve_flat_address` for one holding only what an operator typed. **No `use crate::` anywhere in the file**; every consumer reaches down to it. `manifest`, `vcs` and `workspace` re-export the names they used to own, so their public paths are unchanged. |
| `manifest` (`src/manifest.rs`) | `Manifest`, `LockFile`, `Project`, and the newtypes `RepoPath`, `RepoEntry`, `Role`, `RepoUrl`. `ProjectName` and `WorkweaveName` are re-exports from `naming`. |
| `lock` (`src/lock.rs`) | Snapshotting member HEADs into `rwv.lock`. |
| `repin` (`src/repin.rs`) | The `Rwv-Repin` record of a re-pin by name: `Naming`, `Repin`'s one-line format, and `commit` / `commit_merge`, the only callers of the seam's commit operations, which derive each record from the lock change itself. `unmerged` reads it back with `recorded`. [`docs/internals/repin-record.md`](docs/internals/repin-record.md). |
| `workweave` (`src/workweave.rs`) | Workweave create / delete / list / log, and member birth (`birth_ephemeral_worktree`, `materialize_member`). |
| `workweave_index` (`src/workweave_index.rs`) | The workweave index a project clone keeps in the directory its worktrees share: the basedir setting and, per workweave, its path and the workspace it was forked from. Converts an index an earlier release kept at the old path. |
| `op_state` (`src/op_state.rs`) | The in-flight-operation record: `OpVerb`, `OpPhase`, `OwnerRecord`, `TouchedWorkspaces`, `acquire_op`; and `OpId` and `SavepointKey`, the key a savepoint is filed under (`Op`, `LandTarget`, `LandInPlace`). The record carries a schema number, `OP_RECORD_SCHEMA`, whose wire shape a test pins by digest. [`docs/internals/op-state.md`](docs/internals/op-state.md). |
| `owned_state` (`src/owned_state.rs`) | The attested-generation ledger `.rwv-owned-digests` (`OWNED_DIGESTS_FILE`,): what rwv last accepted for each fully-owned generated file, and the inputs a generation read. Consumed by `activate`, `check`, `workweave` and the cargo integration. |
| `durable_file` (`src/durable_file.rs`) | The one whole-file publish path: `replace` (overwrite) and `create_new` (refuse an occupied target), both temp-then-fsync-then-publish. A state file reaches it only through `StatePath`. |
| `liveness` (`src/liveness.rs`) | The one answer to whether a process still holds a file: an advisory lock the kernel drops with the process, and where the filesystem has none a pid and host the next process on that host can check. Temps, short claims and the git-dir scratch owner (`Scratch`, `src/git/scratch.rs`) all ask it. |
| `state_file` (`src/state_file.rs`) | `StateFile`, the closed list of files rwv keeps as its own record, and `StatePath`, where one lives. A `StatePath` is not `AsRef<Path>` and hands out no path, so no path-taking write accepts it; its `publish`, `create` and `claim` are the writes, and each goes through `durable_file`. |

### Verbs

`activate`, `add_remove`, `check` (`rwv doctor`, the largest
module), `init`, `push`, `status`, `sync`, `prime`, `setup`, `explain`. Each
owns one verb's orchestration and its `--json` envelope type (§7). `sync`
additionally owns the phase machine (§5).

The verbs that take a project in, or move it forward, are split across modules
by what they may change. Of these only `update_remote` writes `rwv.lock`, and
the rest read it.

| Module | Owns |
|---|---|
| `clone` (`src/clone.rs`) | `rwv clone`: clones the project repo, runs `bring_members` over its members, then activates the project when none is active. A member the lock gives no commit to be at is judged `pin` `none` or `pin` `unreachable` and listed, and the verb returns `CompletedWithRemainder`. |
| `fetch` (`src/fetch.rs`) | `rwv fetch`, and `pruning_fetch`, the one function every pruning fetch goes through — `rwv pull` and `rwv update --remote` call it too. It moves no local branch, checkout or lock, and keeps the old tip of a rewritten remote-tracking branch as a `superseded` backup. |
| `fetch_claim` (`src/fetch_claim.rs`) | `FetchClaim::hold`, the claim on an object store that lets one fetch into it run at a time. [`docs/internals/concurrency.md`](docs/internals/concurrency.md). |
| `pull` (`src/pull.rs`) | `rwv pull`: the fetch, the fast-forward of the project branch (`take_in_project`), then `update_members`. |
| `update` (`src/update.rs`) | `rwv update`, and `bring_members`, the clone-missing-and-align engine `rwv clone` shares: it clones each member not on disk, aligns each present one to its lock entry, and reports what became of every pin as a `PinOutcome`. The lock is read and never written. |
| `update_remote` (`src/update_remote.rs`) | `rwv update --remote`: the pruning fetch, the advance of each repo to its branch HEAD, and the re-snapshot of `rwv.lock` through `write_project_lock` (`src/lock.rs`, which `rwv lock` also runs). An authoring verb, so it also regenerates integration files. |
| `branch` (`src/branch.rs`) | `rwv branch set-mode`, `rwv branch list`, `rwv branch rename` and `rwv branch delete`, and `require_recorded`, the gate the verbs that align, land or publish call before they change anything (`require_record`, without the Names rule, for `rwv lock`); `record_trunk` for `init` and `clone`; `workweave_parent` and `record_workweave` for `workweave create`; and the record judgments `switch` shares (`judge_lookup`, `judge_differs`, `feature_record`, `judge_name_taken`). |
| `deletion` (`src/deletion.rs`) | The deletion rule: `Rule::open` reads what belongs to a branch and `Rule::apply` judges each, producing an `Outcome` of `Deleted` and `Survivor` values with their `Reason`. `branch delete`, `workweave delete` and `land --retire` all run it; none decides a deletion itself. |
| `records` (`src/records.rs`) | The branch records: the entry format (`Entry`, `Record`), the entry-by-entry merge of the records remotes (`merge_snapshots`), `View` (what a branch's record reads as, the cycle walk, dependents) and `Store`, whose `write` merges, then writes, then moves the local records branch by compare-and-swap, re-reading and retrying when it loses; `write_dropping` also drops the entries the drop rule names. `entry_path` and `entry_name` are the one codec between a branch name and its entry's file, which escapes `%` and `/` so no entry's path is below another's. The git spelling stays in `src/git/records.rs`. |
| `switch` (`src/switch.rs`) | `rwv switch` and `rwv branch create`: each judges every repo it would move, the project repo's own move included, before it writes a record or moves a checkout, then puts the project repo on the branch and aligns the members through `update`. |
| `alignment` (`src/alignment.rs`) | The alignment engine: `Aligner` reads a workspace's checked-out branch, its record and its `WorkspaceState` (the aligned branch, and per branch the mode and member entries it was aligned by), plans each member's expected state (`plan`), says what holds it back (`blocked`) and moves it (`apply`). `Accounting` is what the weave's live weave branches declare and record. |
| `names` (`src/names.rs`) | The Names rule: `judge_feature_names`, the one judgment that a feature's `owned` and `fork` member branches are no live branch's declared branch and no other project's feature's, run where a feature is written, opened, aligned or published. |
| `backup` (`src/backup.rs`) | `rwv backup list`, `drop` and `prune`, and the pin rule that refuses a drop leaving a live pin of any project reached by nothing. The git spelling stays in `src/git/backup.rs`. |

The verbs that land work, take a target in or settle a member's entry are the
branching half of the tree. Each owns its own judgment and shares its
machinery with the others through the modules below.

| Module | Owns |
|---|---|
| `land` (`src/land.rs`) | `rwv land`'s resolution and judgment: `resolve` and `read_target` find the feature's record, its target, `T`'s tip and the workspace that has `T`; `judge` plans each member's receiving branch and every refusal the landing owes, before the phase machine composes anything. [`docs/internals/landing.md`](docs/internals/landing.md). |
| `feature_sync` (`src/feature_sync.rs`) | `rwv sync`: a feature takes in the branch its record lands into, or any weave branch merges another's history with `--from`; each refusal is judged before anything moves. |
| `lineage` (`src/lineage.rs`) | The lineage rule: `judge_by_history` returns a `Verdict`, passing when the receiving branch contains the work's base, the newest commit of its first-parent chain a lock in the target's history pinned. `PinHistory` is every pin each member had through every parent. |
| `lock_merge` (`src/lock_merge.rs`) | `merge_pins`, the pure three-way merge of a project's pins over the base's lock, each parent's lock and the merged `rwv.toml`, which every caller that joins two project histories reads. |
| `lock_driver` (`src/lock_driver.rs`) | The lock merge driver the VCS runs for `rwv.lock` in a merge rwv does not make, its setup, and the refusals that hold every verb off a project whose setup or lock it leaves unsettled. |
| `project_merge` (`src/project_merge.rs`) | The project merge: computed as a tree first (`Vcs::tree_merges`), the member set read from it, members merged, dropped checkouts pruned after every member merged, the project commit written last with the relock. |
| `stack`, `remap` (`src/stack.rs`, `src/remap.rs`) | Dependents carried along a rewrite of their target's branches, parents first and all composed before any branch moves; and the pins of a rebased project branch mapped through the member rebases. |
| `landing_claim` (`src/landing_claim.rs`) | The claim a landing holds on its target for the life of its operation, in the project clone's store. |
| `merge_upstream` (`src/merge_upstream.rs`) | `--merge-upstream` and `--reset-to-upstream` for `pull` and `update --remote`, run under the op record. |
| `cherry_pick` (`src/cherry_pick.rs`) | `rwv cherry-pick`: one project commit ported onto the checked-out feature, each `owned` or `fork` member's range copied, then the project commit with the relock. |
| `set_history`, `member_branch`, `member_pin`, `member_settings`, `member_url`, `lock_commit`, `manifest_commit` | The setters: `branch set-history`, `member set-branch`, `pin`, `set-role`, `set-recurse-submodules`, `set-url`, and `lock --commit`'s commit of a hand-edited `rwv.toml`. Each commits only its own change, so every other uncommitted edit to the project repo stays where it stands. `manifest::edit` is the one writer of `rwv.toml` text. |
| `declarations`, `entry_report`, `pins`, `unmerged` | What the live weave branches declare (read by Protection, Names and alignment alike); the member-entry changes a workspace stands in; where a checkout stands against its pin and the live locks pinning a member; and the merges in a pushed range that joined pins without joining the member. |
| `commit_guard`, `cli::during_op` | No commit of the project repo while an operation is in flight there; and the table of the verbs that run while an operation is recorded in a workspace, every other verb refusing there. |

### Seams and shared machinery

| Module | Owns |
|---|---|
| `vcs` (`src/vcs.rs`) | The `Vcs` trait, its witness and destroy-proof types, and `VcsError`. §6.1. |
| `git` (`src/git.rs`) | The one implementor, `GitVcs`, plus `git_command()`. |
| `integration` (`src/integration.rs`) | The `Integration` trait, `IntegrationContextBase` and `IntegrationContext`; the finding vocabulary `Issue` / `IssueKind` / `MemberIncompatibility` the trait returns. §6.2. |
| `integrations/` | Eight built-in implementors plus `merge.rs`, the managed-file merge engine they share. |
| `integration_runner` (`src/integration_runner.rs`) | The lifecycle driver: enablement, error containment, and the six entry points that call the trait. |
| `plugins` (`src/plugins.rs`) | External-subcommand discovery and dispatch. §6.4. |
| `tool_program` (`src/tool_program.rs`) | `ToolProgram`: one `PATH` lookup, absolutized, that the install hooks, go-work and plugin dispatch spawn from. `rwv each` has its own, because that binary never names the library. |
| `parallel` (`src/parallel.rs`) | Bounded per-repo fan-out for the network-bound verbs. |
| `share` (`src/share.rs`) | Placing a member's tracked files from the repository's other workspaces by reflink or hardlink, with byte-exact verification and repair. [`docs/internals/member-population.md`](docs/internals/member-population.md). |
| `settle` (`src/settle.rs`) | The permissions a member's tracked files hold for its role, and re-sharing after a member moves. Same document. |
| `diagnosis` (`src/diagnosis.rs`) | The state-to-choices table — axes, their states, every choice and each spelling of it with its Where and whether its verb has shipped — and `Report`, the refusal of the repos a verb judged, rendered from that table to the terminal, to `--json` and to the refusals page. [`docs/internals/refusal-kinds.md`](docs/internals/refusal-kinds.md). |
| `refusal` (`src/refusal.rs`) | `RefusalKind`, the token a refusal naming a condition carries, and the funnel that prints every refusal. Same document. |

## 3. Process model

One process, one verb, then exit. `run()` (`src/cli/dispatch.rs`) proceeds
in five ordered steps.

**1. Pre-parse argv interception** (`dispatch.rs`). A raw
`std::env::args()` scan, before clap sees anything, that turns retired flag
spellings into `exit(2)` migration errors. It also catches a mistyped
`rwv workweave <PROJECT> <WORD>` subcommand with a Levenshtein guard
(`SUBCOMMAND_TYPO_THRESHOLD = 2`, `dispatch.rs`).

**2. Command build** (`dispatch.rs`). `Cli::command()` plus a
dynamically appended "External commands" `after_help` section listing the
plugins found on `PATH` (§6.4).

**3. Parse** (`dispatch.rs`).

**4. The single resolution point** (`dispatch.rs`). Two steps, in
order:

- *Workspace origin.* With no `-C`, `workspace::acquire_origin_dir()`
  (`src/workspace.rs`) — the only cwd read that feeds resolution; the one
  other reader is `workweave delete`'s step-out probe, which treats the cwd
  as an open handle to release, never as an origin. With `-C`, the
  override read from that cwd. Either way the origin is an
  `cli::operand::InvocationDir`, resolved, which only `crate::cli` can mint.
  **Resolution never `chdir`s** (`dispatch.rs`); every path is absolute from
  here on: a path argument is an `Operand` in the parsed CLI struct, and
  dispatch locates each one against the `InvocationDir` before it calls a
  handler. The one `chdir` in the tree is that
  same step-out, long after resolution, releasing the deleting process's own
  handle on a workweave it is about to remove.
- *Workweave selection.* `-w <project>--<name>` is resolved against the
  primary-side index and validated against the target's `.rwv-workweave`
  marker; the resulting path replaces the origin.

`--project` is deliberately *not* global — it is a per-verb flag on the nine
verbs that act on a project, and feeds `WorkspaceContext::resolve` as
`project_override`.

**5. The verb match** (`dispatch.rs`), which calls one module's entry
point. The unmatched-verb arm falls through to plugin dispatch (§6.4). The exit
status is decided at one funnel, `run_and_report` (`dispatch.rs`): `0` when the
verb returns, `3` when it returns `CompletedWithRemainder` (`src/exit_status.rs`)
after printing what it left, and `1` for any other error, after the refusal
funnel prints it. Arguments that do not parse exit `2` before any verb runs.

### Where parallelism lives

There are no threads outside `parallel::run_in_parallel`
(`src/parallel.rs`), which runs a closure over items in a bounded
`std::thread::scope` pool and gathers results in input order. `clone`, `fetch`,
`pull`, `update` and `update --remote` default to
`min(available_parallelism, 8)`; `sync`, `land` and
`push` default to serial. A push authenticates and runs the repo's pre-push
hook, which concurrent pushes would run side by side against one terminal's
credential prompts. `-j` never changes `--json` output: every verb prints one
envelope, its records in input order (§7).

## 4. State & ownership

All durable rwv state is files. There is no database and no cache directory.

| File | Constant | Location | Owns |
|---|---|---|---|
| `rwv.toml` | — | `projects/<name>/` | Committed intent: membership, roles, per-integration config. |
| `rwv.lock` | — | `projects/<name>/` | Committed intent: the revision each member is pinned to. |
| `.rwv-active` | `ACTIVE_PROJECT_FILE`, `src/workspace.rs` | weave root | Which project the primary root currently presents. Ambient default. Read only by `observe_root`, through the private `read_active_pointer`; a file naming no project refuses where a verb would select by it (`unreadable-active-project`). |
| `.rwv-workweave` | `WORKWEAVE_MARKER_FILE`, `src/workspace.rs` | each workweave root | A workweave's only identity file: `{primary, project, name, parent}`; the name is the marker's own, so an index rebuild finds every workweave a linked checkout of the project clone sits in. Restored from the index and the worktree relation when missing or disagreeing (`src/workspace/reconcile.rs`). |
| `rwv-workweave-index` | `StateFile::WorkweaveIndex`, `src/state_file.rs` | the project clone's common dir (`Vcs::store_of`) | The primary's inverse view: the basedir setting and, per workweave, `{path, parent}`, the record of `parent` a marker is restored from. A file of the old name at `projects/<project>/` (`StateFile::OldPathWorkweaveIndex`) is read by nothing as the index: verbs refuse it as `unreadable-workweave-index` and `rwv doctor --fix` rebuilds the index from the markers and removes it. |
| `rwv-workweave-index.lock` | `StateFile::IndexClaim`, `src/workweave_index.rs` | beside the index | The claim every edit of the index holds (`IndexClaim::hold`). |
| `.rwv-landing.lock` | `StateFile::LandingClaim`, `src/landing_claim.rs` | `rwv-landings/` in the project clone's store (`Vcs::store_of`) | The claim a landing holds on its target branch from before it reads the target's tips until it releases it; it names the op and the workspace holding the op's record. [`docs/internals/landing.md`](docs/internals/landing.md). |
| `.rwv-fetch.lock` | `StateFile::FetchClaim`, `src/fetch_claim.rs` | a member's or the project clone's store (`Vcs::store_of`) | The claim every pruning fetch into that store holds (`FetchClaim::hold`). |
| `.rwv-op` | `OP_STATE_FILE`, `src/op_state.rs` | a weave root | The owner record of an in-flight multi-repo operation. §5. |
| `.rwv-op-lease` | `OP_LEASE_FILE`, `src/op_state.rs` | every other workspace the operation mutates | A mutex plus a redirect to the owner. Immutable once written. |
| `.rwv-owned-digests` | `OWNED_DIGESTS_FILE`, `src/owned_state.rs` | beside a generated file | SHA-256 of fully-owned generated content, so drift is detectable. |
| `.rwv-owned-digests.lock` | `StateFile::OwnedDigestsClaim`, `src/owned_state.rs` | beside the ledger | The claim every edit of the ledger holds. |
| `rwv-workspace-state` | `StateFile::WorkspaceState`, `src/state_file.rs` | the project checkout's private dir (`Vcs::private_dir`) | `WorkspaceState` (`src/alignment.rs`): the aligned branch, and per branch the member entries and the record (`SeenRecord`) the workspace last aligned by. A change to the checked-out branch's record that this workspace did not write is `target` `record changed` (`record_change_line`); `unseen_record_change` is the refusing gate. Written only when it changes; unparseable reads as absent. |

Two ownership rules are structural rather than conventional:

**The marker, the index and the worktree relation are three records of one fact.**
The marker tells a workweave where its primary is; the index tells a primary
where its workweaves are and what each was forked from; the relation is the
link from the workweave's project checkout to the primary's project clone. Every
entry consumed from the index is validated by round-tripping through the
target's marker (`marker.primary` canonicalizes to this primary, `marker.project`
matches). A stale or foreign entry degrades to `None` plus a `rwv doctor`
finding. Where two of the three agree, the third is rewritten from them by every
verb that does not destroy the workweave (`reconcile_invocation`, run once in
`cli::dispatch` before the verb resolves its root); `workweave delete` and
`land --retire`, a resumed one included, refuse on any disagreement instead
(`src/workspace/reconcile.rs`). The relation says nothing of `parent`, so for
that field the index is the authority a marker is restored from, and a change
of parent (`workspace::reparent`) writes the index first: a stop between the
two writes is carried forward to the new parent by the next verb.

**The index records no ownership.** What rwv may delete is decided by the
deletion rule (`src/deletion.rs`) from the branch records and the refs
themselves, never from a record that rwv created a ref; the index carries no
`receipts` at all, and a file at the old path is an unreadable index that
`rwv doctor --fix` rebuilds from the markers and removes (`src/workweave_index.rs`).

`.rwv-active` and `.rwv-workweave` are **mutually exclusive**; `rwv doctor`
reports the conflict.

**Surfacing persists nothing.** Which file at the weave root belongs to which
project is read off the disk each time: a link names its project, and a real
file a tool wrote past its link (`src/root_write.rs`) is judged against the
project `.rwv-active` names. So a switch that would leave such a file behind is
refused rather than recorded: `surface_with_notices` (`src/activate.rs`) takes
the outgoing project's root writes home first, and refuses with
`conflicting-root-write` when one conflicts with that project's copy. Recording
an owner for the file instead would be the one persisted piece of surfacing
state, with its own staleness and repair to carry.

### What is guarded against a second writer, and what is not

Git is the reference budget, in both directions: a surface where rwv is
sloppier than git's analog is a defect, and a guard git declines to build on
the analogous surface is out of scope here too. That splits the state above
into two tiers with different guarantees, and the difference is not a matter of
degree.

**rwv's own metadata is guarded.** Every record only rwv writes publishes
atomically — write-then-rename, so no reader sees a half-written file and a
crash cannot tear one. Where two processes could otherwise interleave a
read-modify-write, a `link(2)` claim serializes it, which is git's `index.lock`
in a different spelling. Two shapes, and they are not the same guarantee:

- **The owned-digest ledger and the workweave index take a claim per
  read-modify-write**, released when the edit returns; a release that fails is
  an error naming the file left behind. That one is a type rather than a
  discipline: `read_owned_digests_for_edit` and `write_owned_digests` take a
  `&LedgerClaim`, and `workweave_index::read_for_edit` and
  `workweave_index::write` take an `&IndexClaim`, so an edit path that reads or
  writes outside the exclusion does not compile.
- **The workspace state takes no claim.** Each workspace keeps its own in its
  private dir and only a verb acting on that workspace writes it, published
  whole; two mutating verbs racing in one workspace are not a case rwv guards,
  as git does not guard two commands racing in one worktree. One that does not
  parse reads as absent and is reported, and the next aligning verb rewrites
  it; losing it loses no record, since the branch records hold every fact it
  caches.
- **The op record and its lease are claimed once, by creation.** `acquire_op`
  creates them with `link(2)` and a loser gets `EEXIST`, so for the operation's
  lifetime there is exactly one writer and its later phase writes need no
  further claim.

The cost is git's cost, paid in full view: a process killed while holding a
claim leaves it behind, and every later writer refuses — naming the file to
remove — until a human removes it.

**The working tree is not guarded, and that is the decision.** Member
checkouts and manifests are shared, unlocked space: an editor, a build, or a
second rwv may write one at any moment, including while a verb is running, and
rwv neither locks them nor assumes exclusive access — exactly as git cannot
stop an editor writing a file during `git add`. Interference here is a
detection problem, never an exclusion one.

**What makes detection enough is the attestation invariant: rwv never records
a claim it did not observe.** A verb that derives state from working-tree
inputs re-reads those inputs at the moment it records what produced the result,
and refuses to record — naming the input that moved and the rerun that fixes
it — if they no longer match what the derivation consumed. That converts a
silent false record into a loud retry, which is why the unguarded tier costs
less than it looks like it should.

**Where a verb refuses because another operation is in flight, that refusal is
advice, not exclusion.** `materialize` and `activate` read the op-state marker
once at startup and refuse while an operation involves the workspace — the
analog of git refusing to act mid-rebase. They hold nothing: an operation that
starts after that read proceeds unnoticed. The window is affordable precisely
because the attestation invariant stands behind it, so removing that backstop
would silently weaken these refusals too. Checking a marker and acquiring a
lease are different acts, and this is the first.

Two things follow that rwv deliberately does not do. It does not serialize its
verbs against each other — the exclusive lease is `sync`'s tool, because a
half-applied multi-repo operation is the one case worth a stale-lock risk, and
spreading that risk across the whole verb surface buys little the two rules
above do not already give. And it does not adjudicate content: a change rwv
recorded honestly raises nothing here, however unwise it was.

The full calibration, including how to judge a new concern against it, is
[`docs/internals/concurrency.md`](docs/internals/concurrency.md); the boundaries
above are pinned by `tests/process/concurrency_boundaries_test.rs`.

## 5. The sync phase machine

`land` runs the phase machine. `src/sync.rs` exposes its entry points,
`run_land` and `land_json_run`, each handed the `OutputMode` the dispatcher
resolved: text runs the machine under a text handler, and JSON under the
envelope one. Both delegate to one private driver, `run_machine`. `rwv sync`
does not run it: `feature_sync::run_sync` takes a branch's target in by
judging first and then acting, and `pull` and `update --remote` with
`--merge-upstream` or `--reset-to-upstream` (`src/merge_upstream.rs`) and
`cherry-pick` run their own steps. All of them file the same op record and
savepoints, so `--continue` and `rwv abort` read the record whichever verb
wrote it.

### Shape

The phase type is `op_state::OpPhase` (`src/op_state.rs`), a four-variant
enum that serializes kebab-case:

```rust
pub enum OpPhase { Replay, Relock, AdvanceTarget, Retire }
```

The full lifecycle is eight stages —
`guard → mark → savepoint → replay → relock → advance-target → retire → cleanup`
(`src/op_state.rs`) — but only the middle four are *phases*. Guard,
mark and savepoint run once before the loop, inside `guard_and_mark`
(`src/sync.rs`); cleanup runs once after it (`src/sync.rs`).

Control flow is a loop over persisted state, not a sequence of calls:

```rust
fn drive(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    loop {
        let phase = ctx.current_phase()?;          // re-read from disk
        let next = run_phase(ctx, phase)?;
        match next {
            Some(p) => op_state::set_phase(&ctx.owner_workspace_dir, p)?,
            None => { cleanup(ctx)?; return Ok(()) }
        }
    }
}
```

(`src/sync.rs`.) `run_phase` is a four-arm match; each arm calls
one phase function and returns the next phase, with `None` terminal.
`next_after_relock` ends the operation for every verb but `land`, which
continues to advance-target.

Per phase: `run_replay`, `run_relock`,
`run_advance_target`, `run_retire`.

### Replay's member set

`run_replay` (`src/sync.rs`) runs Phase 1', the project repo, before it
touches any member checkout. It then loads the manifest that replay
produced and decides the member set against it. Replay makes no checkout: a
member that manifest names and CWD lacks replays nothing and keeps its pin
through the relock, and gets its checkout from the alignment of CWD that runs
once the record and savepoints are gone, so `rwv abort`, which restores refs
and nothing else, never meets a checkout the op made. After the Phase 2
fan-out over the member repos, `prune_dropped_repo` removes a member the
pre-op manifest named and the replayed one does not, behind the store-removal
judgment ([`docs/internals/store-removal.md`](docs/internals/store-removal.md)).
The pre-op manifest is read at the project repo's savepoint, so a `--continue`
weighs the same set the interrupted run did. In a workweave the prune takes
the checkout out of the primary's clone through `release_checkout`
(`src/workweave.rs`), the same release `delete_workweave` runs for each
member: the worktree is unregistered and the workweave's recorded branch
retired there. Relock regenerates the lock
from the same replayed manifest, so the checkouts and the lock agree by
construction.

The member set is read, never predicted. A rule over the two locks and the
lock at their common ancestor misreads a member CWD dropped and added back,
and has no answer when the histories share no lock; only the replay's own
result covers both.

The trade-off is when a prune lands. It follows the rebase and the whole
fan-out, so an op that parks defers it to `--continue`, and an aborted op
has pruned nothing. That matters because savepoints are refs: `rwv abort`
can move a ref back but cannot bring back a deleted checkout. The cost is
that a prune waits on conflicts in repos unrelated to the pruned member,
and a member-repo park finds the project repo already replayed;
abort's verified restore covers it through the project tip recorded right
after Phase 1'.

### Why the loop reads from disk

`op_state::set_phase` (`src/op_state.rs`) is the **single persistence
point** of the whole machine. The driver loop's call (`sync.rs`) is a
*post*-transition write; resume entry (`sync.rs`) writes through the same
function before re-entering a phase, so the write itself is not ordered with
respect to the transition — the loop's use of it is. The invariant, stated at
`src/sync.rs`, is that the persisted phase is the phase in progress
and every phase is re-runnable from the record alone. That gives three crash
positions and one rule:

- inside `run_phase` — the record still names the running phase; resume
  re-enters it;
- after `run_phase` returned but before `set_phase` committed — the record
  still names the just-completed phase; resume re-runs it (idempotently), then
  transitions;
- after `set_phase` committed — resume enters the next phase directly.

`--continue` is therefore not a separate code path: `run_machine` chooses
`load_continuing_context` over `guard_and_mark` and enters the same
loop. There are no resume flags threaded through call stacks
(`src/sync.rs`).

### What the record holds, and what it deliberately does not

`OwnerRecord` (`src/op_state.rs`) is JSON, written only at the initiating
workspace; other mutated workspaces get a `LeaseRecord` that is an id
plus a pointer. `resolve_to_owner` follows the pointer, so `--continue`
and `abort` behave identically from either side.

Per-repo *progress* is not in the record — savepoint refs and the VCS's own
mid-operation state already are that. What the record holds is the plan the
VCS cannot: what a `--continue` re-applies, and for a landing the tips it
composed (`converged_tips`, written at relock) and wrote in its target.

`abort` does not read the record to decide what to keep. `run_abort`
(`src/sync.rs`) performs a **verified restore** per repo: it resets to the
savepoint, keeping first, whoever made it, a tip holding a commit nothing
else reaches in the branch's one slot and a working copy it discards in the
checkout's one slot (`Vcs::keep_in_slot` for the first); each
slot's earlier values stay in the VCS's history of the slot ref. The
record decides only which stopped operation is the op's to cancel and, for a
landing, whether its target moved since, toward refusing.
`VerifiedRestoreOutcome` (`src/vcs.rs`) enumerates the answers, including
`ForeignTip`, a repo moved between plan and write, which is reported rather
than reset and keeps the record.

A merge or replay of the project repo can stop with `rwv.toml` unmerged, as a
conflict stop (the VCS left regions in conflict) or a check stop (the VCS merged
the text with no conflict, and the merge of both sides' values differs from
it). `src/manifest_conflict.rs` reads the sides and resolves a stop on
`--continue` with `--keep`; the stop is journaled in `take_in.manifest_stop`.
[`docs/internals/sync-phase-machine.md`](docs/internals/sync-phase-machine.md)
describes both stops.

Savepoints are git refs under `refs/rwv/pre-op/<key>`; backups, which outlive
any operation, are refs under `refs/rwv/backup/<branch>/<id>` (see
`src/git/backup.rs`; `src/backup.rs` is `rwv backup`, with the pin rule).
The records branch (`rwv-records`) is the other state rwv keeps in the project
repo's refs: one TOML file per weave branch, committed with object writes and
no working tree (`Vcs::write_commit`), advanced by `Vcs::advance_records`, a
compare-and-swap on the local branch, and read from the remote-tracking tips of
the records remotes (`Vcs::records_tip`). A read-only verb folds those tips in
memory (`Store::view`); a verb that writes a record, aligns, locks, lands or
publishes commits the fold first (`Store::write`).

Every `Vcs` savepoint method takes a `SavepointKey`
(`src/op_state.rs`): `Op(OpId)` for the workspace the op replayed in,
`LandTarget(OpId)` for a `land` target's repos, whose ref leaf carries a
`-target` suffix because a worktree pair shares one ref store and the first
restore would otherwise drop the ref the second needs, and `LandInPlace(OpId)`
for a checkout in a third workspace that holds a receiving branch the landing
fast-forwards in place. Under a key there is one savepoint per ref the
operation may move, a checkout's or a branch's, which `abort` restores by
compare-and-swap. `OpId` is nanoseconds
since epoch, minted by `OpId::new_now()` and read back only through its
validated parse, which refuses an id ending in that suffix.

## 6. The seams

### 6.1 `Vcs`

`pub trait Vcs` (`src/vcs.rs`) is the whole VCS surface: every VCS
operation is a method on it, wide and flat — no supertraits, no associated
types, object-safe. One implementor exists —
`GitVcs` (`src/git.rs`), a unit struct, `impl` at `src/git.rs`. Callers
reach it two ways: `vcs_for(VcsType)` (`src/vcs.rs`) returning
`Box<dyn Vcs>` for the manifest-driven paths, and direct use of the unit value
`GitVcs` elsewhere. `manifest::VcsType` has one variant today; `jj`, `sl` and
`hg` appear only in the trait's doc comment.

The trait divides at a banner (`src/vcs.rs`) into the mechanism half and the
branch model proper. The second half is the part that carries architecture
rather than mechanism: **every ref write is one of four kinds —
MOVE, ATTACH, DESTROY, DESTROY-STORE — and each method takes the proof its kind
requires.** The proof is a value that cannot be forged:

- **Observation witnesses.** `AttachedRef`, `UnbornRef` and
  `DetachedHead` have no public constructor. The only code that
  builds them is the default body of `Vcs::head_attachment`, which
  wraps the implementable half, `observe_head` returning
  `HeadObservation`. Splitting the observable enum from the witness
  enum is what makes the witnesses unforgeable. `verify_attachment`
  re-observes and fails with `VcsError::StaleRefWitness` if the world moved.
- **Destroy proofs.** `DestroyProof` is an opaque struct over a private enum:
  a branch deletion is representable only with a proof that every commit on the
  branch is reached by a ref that outlives it (`DestroyProof::landed`, which
  runs the ancestry check) or that a backup of it was made first
  (`DestroyProof::backed_up`, which makes the backup and holds the witness).
  The proof names the store, the branch and the tip it was proved about, and
  `Vcs::delete_branch` deletes only at that tip, so a branch that moved since is
  left. `Vcs::reset_attached_ref_keeping` takes a `BackupWitness`: a reset that
  leaves a tip is representable only when a backup of that tip provably exists.
- **Found branches.** `FoundBranch` is the branch the alignment engine found
  in a store, with the revision a birth would start it at; a worktree is born
  only on one, so a birth derives its branch from the store rather than from an
  independent name. `Departure` (`src/alignment.rs`) is the matching proof for a
  checkout leaving its attachment: minted only after the stray set of the move
  was read.
- **The fetch claim.** `Vcs::fetch_pruning` takes a `&FetchClaim`
  (`src/fetch_claim.rs`), which only `FetchClaim::hold` constructs, so a
  pruning fetch into an object store without the claim on it is not
  expressible.
- **Consent tokens.** They live in `pub mod consent` (`src/cli/consent.rs`),
  not in `vcs`, with constructors that are `pub(in crate::cli)` and called
  only from `cli::dispatch::run`, from argv — or, for a `--continue`, from
  the op record `run` resolved. That privacy is the reason `src/main.rs` must
  stay empty (§2).

Several witness types deliberately have **no `as_str()`** — `TrackingRef`,
`FoundBranch`, `AttachedRef` — so a caller cannot launder a proof back into a
bare string. `compile_fail` doctests pin this.

A commit carries key/value **annotations** beside its message:
`commit_only` and `commit_merge` take `&[Annotation]`, and
`annotation_values` reads one commit's values under a key (git: trailers).

**The capabilities the branching work added to the seam** (relative to
`477979a2`, as of `8e727620`; the full list is the trait, and
[`docs/internals/vcs-backend-contract.md`](docs/internals/vcs-backend-contract.md)
states what each capability requires of a second backend):

| Capability | Methods |
|---|---|
| Roles of remotes, as opaque handles | `url_remote`, `add_url_remote`, `project_url_remote`, `record_project_url_remote`, `home_remote`, `push_target`, `push_target_of`, `own_push_remote`, `push_branch`, `branch_push_targets`, `upstream`, `upstream_of`, `remote_copies`, `recorded_urls` |
| Facts computed from ancestry | `relation`, `reached_by_remote`, `reached_by_no_branch`, `unpushed_commits`, `unpushed_beyond`, `pushed_range`, `held_by_no_remote`, `patch_equivalents`, `merge_bases`, `merge_base_against`, `first_parent_chain`, `on_first_parents`, `ancestry`, `commits_beyond`, `commits_excluding`, `commits_changing`, `stray_commits`, `log` |
| Commits and refs with no working tree | `write_commit`, `files_at`, `file_versions`, `advance_branch_ref`, `advance_records`, `records_branch`, `records_tip`, `publish_records`, `record_conflict` |
| History-changing operations | `rebase`, `replayed`, `record_replayed`, `copy_replayed`, `take_rewritten`, `forget_replays`, `merge_as_refs`, `commit_merge`, `start_merge`, `end_merge`, `copy_commit`, `continue_copy`, `copied_from`, `copied_into`, `conflict_picks`, `conflict_sides`, `tree_merges` |
| Operations in progress | `mid_operation`, `branch_under_rebase`, `rebase_in_flight`, `merge_in_flight`, `copy_in_flight`, `merging_commits`, `cancel_in_flight_op`, `ending_stopped` |
| Savepoints and backups per ref | `create_ref_savepoint`, `ref_savepoints`, `drop_ref_savepoint`, `plan_ref_restore`, `execute_ref_restore`, `plan_checkout_restore`, `execute_checkout_restore`, `create_backup`, `write_backup`, `list_backups`, `drop_backup`, `keep_in_slot`, `backup_edits`, `backup_unpreserved_head`, `show_kept` |
| Checkouts and stores | `store_of`, `private_dir`, `main_checkout`, `checkouts_of_branch`, `checkout_recorded_elsewhere`, `record_checkout_moved`, `nested_stores`, `stores_kept_for`, `registered_branch`, `holds_only_what_it_fetched`, `materialize_worktree_detached` |
| Working copy | `carry_edits_onto`, `check_out_branch`, `set_edits_aside`, `set_back`, `snapshot_edits`, `copy_uncommitted`, `uncompared_paths`, `unmerged_paths` |
| Lock driver | `lock_driver_setup`, `declare_lock_driver_in`, `define_lock_driver`, `lock_driver_declaration_file` |
| Operator text | `hint`, printed as "VCS action (git: ...)" |
| Tool readiness | `minimum_release`, `tool_readiness` |

`VcsError` carries a `kind()` of stable kebab-case tags and a
serializable mirror, `VcsErrorOutput`, for `--json`.

**The tool a backend drives, and what it can do.** A backend states the
oldest release of its tool it runs on (`Vcs::minimum_release`) and whether the
installed one meets it (`Vcs::tool_readiness`). `cli::dispatch::run` asks
every backend `vcs_for` can resolve, through `vcs::refuse_unready_tools`, once
per invocation and before any verb that touches a repository reads anything,
and refuses with `unsupported-vcs-tool` or `missing-vcs-tool`; `explain`,
`completions`, `setup` and plugin dispatch are exempt by an exhaustive match, so a new verb is
classified when it is added. For git the floor is `MINIMUM_RELEASE`
(`src/git.rs`), the oldest release every command it runs outside a detected
capability exists in, and it is read from one `git version` that every
capability also answers from. A command newer than the floor is a capability
the trait exposes as an `Option` (`Vcs::tree_merges`), which a caller takes
its fallback on when it is `None`; the list is in
[`docs/internals/git-floor.md`](docs/internals/git-floor.md).

**What the seam holds by compiler, and what it does not.** No production frame
outside `git.rs` spawns git or assembles its argv: `git_command` is private to
the module, and every git a production frame spawns is built by
`GitCommand::new` (`src/git/argv.rs`), which removes the `GIT_*` variables that
redirect it and pins `LC_ALL=C`, so a failure rwv carries is in the words an
operator can search for. No decision rests on git's wording, which is
translated and changes between releases: the backend tells outcomes apart by
exit status, by formats git documents for machines (`push --porcelain`,
`rev-parse --verify --quiet`), or by the repository's state, and
`tests/source/git_backend_reads_no_wording_test.rs` fails on any stderr text it
matches. A `#[cfg(test)]` fixture that spawns git outside
that constructor inherits `GIT_DIR` when the suite runs under the pre-push
hook; `git_command_in_test` builds the same command for one. A remote is held the same way — it crosses
the seam as a `RemoteHandle`, which only a backend constructs and which
displays but does not give its name back, and no trait method accepts text for
one — so core can neither spell a remote nor be handed a parameter to spell it
into. It reaches one by the role that resolves to it: `Vcs::url_remote`,
`project_url_remote`, `push_target`, `home_remote` and `upstream`
(`tests/source/clone_url_compile_fail_test.rs` pins the absence of a way in). Inside
`git.rs`, a path reaches argv only as a `GitArg`: `git_command` returns a
`GitCommand` (`src/git/argv.rs`) that keeps its `Command` private and takes
arguments as an `Argv`, whose text slot refuses a `&Path` and whose path slot
takes only a `GitArg`, which `GitArg::from_path` alone makes. What is not
mechanized is a path turned into a `&str` by hand and given to a text slot, and
git vocabulary in *operator text*: one message site in core still writes
`origin` as a literal, the doctor's rendering of the `origin-url-mismatch`
finding, whose wire `kind` carries the word too. The published contract for
this seam, and that residue in full, is
[`docs/explanation/joints/vcs-as-seam.md`](docs/explanation/joints/vcs-as-seam.md).

### 6.2 `Integration`

`pub trait Integration` (`src/integration.rs`) is fifteen methods at
`8e727620`: five required (`name`, `default_enabled`, `activate`,
`deactivate`, `check`) and ten defaulted (`tool`, `detection_manifests`,
`settings_keys`, `activate_hook`, `fit_to_host`, `generated_files`,
`managed_files`, `verify`, `owned_paths_on_disk`, `member_incompatibility`).
`tool` is the one CLI an integration shells out to, declared by `DrivesTool`.

Two of them are the materialization path, not authoring. `activate_hook`
runs the ecosystem's install tool and returns a `HookOutcome`: `Done`, or
`ToolMissing(MissingTool)` when that tool, declared once by the integration's
`DrivesTool`, is not on `PATH`, which the runner reports
as a `tool-missing` warning while the other integrations' hooks still run; a
hook that fails while running is an error. Every hook is refused while any
enabled integration's `[integrations.<name>]` block fails to deserialize,
and `rwv materialize` refuses up front in that state, before anything else
it does. `fit_to_host` brings content whose right value is a fact about the
machine rather than about `rwv.toml` — `cargo-workspace`'s `rustc-wrapper`
key — to this host's answer at every materialization that runs the hooks,
ahead of surfacing.

Eight built-in implementors, all unit structs, returned in fixed order by
`builtin_integrations()` (`src/integrations/mod.rs`): `npm-workspaces`,
`pnpm-workspaces`, `go-work`, `uv-workspace`, `cargo-workspace`, `gita`,
`vscode-workspace`, `static-files`.

**There is no registry.** No map, no dynamic loading. The set is the fixed
`Vec`, and `name()` is consulted for two purposes: fetching that integration's
config out of `manifest.integrations` inside `for_each_enabled`
(`src/integration_runner.rs`), and, while `rwv.toml` is parsed, refusing an
`[integrations.<name>]` table no implementor is named by or a key outside that
implementor's `settings_keys` (`deserialize_integrations`, `src/manifest.rs`).
Integrations are compiled in; the plugin mechanism (§6.4) is unrelated and does
not reach this trait.

`integration_runner` owns the lifecycle, and the division is worth stating
because the two halves are easy to confuse:

- **`integration.rs`** defines the contract and its input:
  `IntegrationContextBase`, a `WorkspaceSession` narrowed to one project with
  the project directory derived from the session's root, and the
  `IntegrationContext` each integration reads through accessors. Both are
  built only there (`IntegrationContextBase::new`, `build_context`).
- **`integration_runner.rs`** precomputes the on-disk detection cache
  (`build_detection_cache`,), filters by enablement, and drives seven
  entry points at `8e727620` — `run_activations`, `run_checks`,
  `run_verifications`, `run_member_incompatibilities`,
  `run_activate_hooks`, `run_host_fits`, `run_deactivations`. It does
  **not** own the list: callers pass `&[&dyn Integration]`.
- Errors are contained per integration: one integration returning `Err` becomes
  an `Issue` tagged with its name, and iteration continues.

`src/integrations/merge.rs` is the shared managed-file engine the hybrid
implementors call — the `ManagedDoc` trait and its `JsonDoc` /
`TomlDoc` / `YamlDoc` / `GoWorkDoc` implementations, `merge_activate`
/ `strip_deactivate`, and the ownership-marker machinery — so the
hybrid-file invariants live once rather than per integration. Its writes go
through `write_parseable`, which refuses to write a managed file whose
generated text does not parse back as that document type and leaves the
file as it was; `cargo-workspace`'s own direct writes to `Cargo.toml` and
`.cargo/config.toml` take the same guard. It imports
`Issue`, `IssueKind` and `Severity` and nothing else from core. Its contract is
published at
[`docs/explanation/joints/file-ownership.md`](docs/explanation/joints/file-ownership.md).

The other ownership axis is not here. Attesting a **fully-owned** generated
file — one rwv cannot recompute, so drift can only be detected against a
recorded digest — is `owned_state` (§2). The two axes are independent:
`cargo-workspace` is on both, `vscode-workspace` merges a hybrid file with no
generated one to attest, and `gita` writes its CSVs whole without attesting
them, because rwv derives their content and can simply rewrite it. That is why
`owned_state` is a core service `activate`, `check` and `workweave` consume
directly rather than a second helper under `integrations/`.

Surfacing is the delivery layer in front of both axes. Each path an
integration declares is a `SurfacedFile` (`src/integration.rs`) answering two
questions: where the write that produces it lands (`SurfacedSource`), and which
project checkout the weave-root link reads (`SurfacedCopy` — the weave's own,
or the primary's, which `static-files` asks for with `from = "primary"`).
`SurfacingSites::link` (`src/activate.rs`) turns a declaration into the one
expected target that `surface_symlinks` creates and `verify_surfacing`
checks: relative into the root's own `projects/`, or, for the primary's copy
in a workweave, absolute into the marker's primary. The owner-scoped predicate
that authorizes removing or replacing a link accepts that absolute shape only
under the root's own primary and never at a `workweave.link` name, since that
section canonicalizes its source into the same shape.

### 6.3 Type-level enforcement, in one place

Three of the mechanisms above are the same technique applied at different
scales, and they explain several otherwise-odd shapes in the tree:

| Shape | Enforces |
|---|---|
| `ResolvedRevisionId` (`src/vcs.rs`) serializes but does not deserialize; `RawRevisionId` does both | A revision read from a lock file cannot be mistaken for one resolved against a repo. |
| `ResolvedLockFile` (`src/manifest.rs`) has no `Deserialize` | Same rule at the file level: parsing yields `LockFile`; resolution is a separate, explicit step (`resolve_versions`,). |
| Witnesses, destroy proofs and consent tokens (§6.1) | A destructive VCS operation is unrepresentable without the proof its class requires. |

### 6.4 Plugin dispatch

A plugin is an executable named `rwv-<verb>` on `PATH`. There is no protocol,
no envelope message, and no stdio contract.

Discovery is `which`-based: `find_plugin` (`src/plugins.rs`) for a single
lookup, `discover_plugins` for the `--help` listing and for
`rwv doctor`. First occurrence in `PATH` order wins; later copies are reported
as `shadowed` rather than silently dropped. Core verbs always win — clap parses
them before the external-subcommand fallthrough fires, so a `rwv-status` on
`PATH` can never shadow the built-in (`src/plugins.rs`).

Invocation is a plain subprocess: `build_command` is the single seam
that constructs it, `dispatch_external` spawns and waits, and the call
site is `src/cli/dispatch.rs`. The child starts in the invocation dir that
`src/cli/dispatch.rs` resolved (`-C`, `-w`, or process cwd), the same
`InvocationDir` core verbs' relative path arguments are located against.
Stdio is fully inherited — the child owns the terminal and rwv wraps none of
its output. Exit status propagates, with
signals mapped to `128 + sig`.

**The envelope is environment variables.** `envelope_vars` projects
`workspace::Resolution` (§7) into four variables: `RWV_VERSION`,
`RWV_WORKSPACE`, `RWV_WORKWEAVE`, `RWV_PROJECT`. Presence of `RWV_WORKWEAVE`
encodes the checkout kind; there is no `kind` field. rwv never reads these back
— they are outputs only, and a plugin that wants to call rwv passes them as
arguments (`src/plugins.rs`). The addressing flags rwv consumed (`-C`,
`-w`, `--project`) never appear in the child's argv.

Compatibility handling is exactly one mechanism: `RWV_VERSION`. There is no
negotiation, no minimum-version check, and no capability handshake.

## 7. Output and generated artifacts

**There is no shared output envelope.** Each `--json`-capable verb owns its own
top-level struct — `StatusJsonOutput` (`src/status.rs`), `CloneJsonOutput`
(`src/clone.rs`), `FetchJsonOutput` (`src/fetch.rs`), `PullJsonOutput`
(`src/pull.rs`), `UpdateJsonOutput` (`src/update.rs`),
`UpdateRemoteJsonOutput` (`src/update_remote.rs`), `PushJsonOutput`
(`src/push.rs`), `SyncJsonOutput` and `LandJsonOutput` (`src/sync.rs`), and
`DoctorJsonOutput` (`src/check.rs`) — each pinning its own schema URL. Both
of `rwv doctor`'s report renderers, the text report and `build_doctor_json`,
take only the `DoctorFindings` its findings pipeline returns.

What *is* shared is `Resolution` (`src/workspace.rs`): every envelope
carries it, and it is the same value the plugin envelope projects (§6.4). The
envelope is the same whatever `-j` is.

Stdout belongs to the JSON-capable verbs; operator prose goes to stderr
(`src/workspace.rs`).

### Generated docs are checked into the tree

`generate-explain` is a program of its own, not a build script: a Cargo example, run with `cargo run --example generate-explain`. It does two jobs.

It **assembles**: each of the 16 `docs/reference/explain/templates/<verb>.md.tmpl`
becomes `docs/reference/explain/<verb>.md`, and for the seven verbs with a JSON
envelope the schemars-derived schema is spliced in and also written out to
`docs/reference/schemas/<verb>.json`. It renders `docs/reference/refusals.md`
from its template, the kinds' entries from `src/diagnosis.rs`'s table, so the
page a refusal routes to cannot say what the refusal does not print. A kind
that `rwv doctor` also reports has its one entry on
`docs/reference/doctor-findings.md` instead, and the generator writes it there,
in place, from the same table and the doctor register.
`src/explain.rs` then `include_str!`s the assembled markdown into the binary,
which is how `rwv explain` works with no filesystem access at runtime.

It also **enforces** generation-time invariants that no compiler can: relative
markdown links across `docs/` resolve on disk; `docs/reference/cli.md` covers
every clap subcommand path; every environment variable read from `src/` is on
an allowlist; every envelope output is documented; and `src/` and `docs/`
contain no tracker IDs and no consumer or deployment-specific vocabulary.

Because the artifacts are committed, `scripts/ci-local.sh` regenerates them and
then `git diff --exit-code`s the result — a drift gate that compares the working
tree against the index, so it cannot pass with an uncommitted regeneration.

## 8. Testing shape

Three tiers.

- **Unit tests in `src/`** — most modules carry `#[cfg(test)]` tests beside
  the code they pin.
- **Integration tests** — `tests/<area>/*.rs`, each a module of its area's
  binary (`tests/<area>/main.rs`), driving the `rwv` executable across the
  process boundary via `assert_cmd`. `tests/common/mod.rs`
  supplies a `git()` builder that strips every inherited `GIT_*` variable, so a
  subprocess can never reach the outer repository.
- **Contract helpers** — `tests/common/contract.rs` encodes the hybrid
  file-ownership regressions once, as free functions taking activate/deactivate
  closures, so every integration is held to the same four shapes.

Two harnesses drive whole operations rather than single verbs: the scenario
harness (`tests/scenario`, steps built in `tests/common/scenario_steps.rs`),
whose steps assert a refusal by the state it names, and the kill matrix
(`tests/kill_matrix`), which kills each verb that records an operation at every
pause point it passes (`src/test_pause.rs`, compiled only under the `test-pause`
feature) and checks that `--continue` and `rwv abort` each end in the state an
unkilled run or the pre-operation state holds. [`docs/internals/testing.md`](docs/internals/testing.md).

Two families are worth naming because they pin *documents* rather than code:
`doc_claims_*_test.rs` asserts that published claims match behavior, and
`branch_model_compile_fail_test.rs` pins the consent-token privacy by rustc
error code — the type-level rules of §6.3 fail the suite if they are relaxed.

`scripts/ci-local.sh` is the single gate: `cargo check`, `cargo test --release`,
`cargo clippy -D warnings`, `cargo fmt --check`, then the explain-artifact drift
gate. With no argument it runs every default stage; `--tier=seat` and
`--tier=landing` split those stages between iterating on a change and landing
it (`docs/internals/testing.md`); `--stages=kill-matrix`, which `--tier=release` adds,
runs the kill matrix, to be run before a release and when a change touches an
operation's steps, pause points or abort. `.github/workflows/ci.yml` runs that same
script, with no argument; a separate Windows job
runs `cargo check` and `cargo clippy` only, because the suite assumes symlinks,
permission bits and signals.

`clippy.toml` bans every way to canonicalize a path but one:
`path_spelling::resolve` (`src/path_spelling.rs`) is the only producer of a
resolved path, and it never yields the Windows verbatim `\\?\` spelling where
a plain one names the same path. A location that is compared or recorded is a
`path_spelling::CanonicalPath`, its identity: equal by resolved spelling, with
`contains` for containment and `same_directory` for the filesystem's own
verdict, and comparable with nothing but another identity. `clippy.toml` bans
`Path::starts_with` so containment goes through `contains`. Host clippy cannot
see a `cfg(windows)` item, so the gate's windows stage also lints for the
Windows target, and its apple stage for `aarch64-apple-darwin`.

## 9. Where the contract lives

This document describes implementation. The **contracts** — what rwv promises,
in language that survives a refactor — live in `docs/`, and are the published
surface:

- [`docs/explanation/joints/`](docs/explanation/joints/) — the normative
  contracts, one per seam. `sync-semantics.md` owns the direction pair, the
  phases as an operator sees them, and abort's verified-restore rules (§5),
  with the per-phase contract in
  [`docs/internals/sync-phase-machine.md`](docs/internals/sync-phase-machine.md)
  and the record schema in
  [`docs/internals/op-state.md`](docs/internals/op-state.md);
  `vcs-as-seam.md` owns the VCS boundary (§6.1); `file-ownership.md` owns the
  two ownership axes, the hybrid-merge contract, and the regeneration trigger
  model (§6.2);
  `plugin-boundary.md` owns the plugin contract (§6.4); `clone-topology.md`,
  `workweave-hierarchy.md` and `workweave-lifecycle.md` own the on-disk model
  (§4).
- [`docs/reference/`](docs/reference/) — the CLI surface, file formats, roles,
  and the generated `explain/` pages and `schemas/`.
- [`docs/internals/`](docs/internals/) — maintainer-facing material that is
  deliberately kept out of `docs/SUMMARY.md`, and therefore off the published
  site. Start there for the `Integration` implementor's contract and the source
  conventions this tree is written to.

The rule that keeps the two apart: a statement belongs in `docs/` if a user or a
plugin author can observe it, and here if only someone reading `src/` can.
