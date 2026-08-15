# repoweave architecture

The referenceable module and ownership model for `rwv`: what each module owns,
how one invocation gets from argv to a verb, and where the durable state lives.
This document owns the *cross-module* facts no single file can state. It links
out — rather than restating — for the user-facing contracts, which live in
`docs/` and are the published surface.

Every mechanism claim below carries a `file:line` citation. Line numbers are
against the tree at the time of writing; symbol names are the durable anchor.

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
  (`git_command`, `src/git.rs:32`).

Because there is no process to crash, crash recovery is a *state* question, not
a supervision one: a multi-repo operation that dies mid-flight leaves a record
on disk that a later invocation reads and resumes (§5).

## 2. Module map

One library crate (`repoweave`, `src/lib.rs`) and two binaries. `src/lib.rs` is
a flat list of 34 `pub mod` declarations — there is no internal layering
enforced by the module tree, so the grouping below is by role, not by
visibility.

| Binary | Source | Declared |
|---|---|---|
| `rwv` | `src/main.rs` | `Cargo.toml:12-14` |
| `generate-explain` | `src/bin/generate-explain.rs` | auto-discovered by Cargo (no `[[bin]]` stanza) |

`src/main.rs` is thirteen lines whose entire body is
`repoweave::cli::dispatch::run()`. The emptiness is load-bearing and documented
at `src/main.rs:1-9`: a `[[bin]]` is a separate crate from the `[lib]`, so any
logic placed there could only reach the library through `pub` items. Keeping it
empty is what lets the consent tokens in `src/cli/consent.rs` stay
`pub(in crate::cli)` (§6.1).

### Entry and addressing

| Module | Owns |
|---|---|
| `cli` (`src/cli.rs`, 563) | The clap type tree: `Cli` (`:24`) and `Commands` (`:66`). |
| `cli::consent` (`src/cli/consent.rs`) | The seven consent tokens plus the `DriftConsent` either/or enum, each minted only from its named flag by a `pub(in crate::cli)` `from_flag`. |
| `cli::dispatch` (`src/cli/dispatch.rs`) | `run()` (`:321`) — argv interception, parse, the single resolution point, and the verb match. |
| `workspace` (`src/workspace.rs`, 2521) | `WorkspaceContext` (`:75`) and its `resolve` (`:549`); `Checkout` (`:163`); `Resolution` (`:1017`); the `.rwv-active` / `.rwv-workweave` marker constants (`:247`, `:250`). |
| `registry` (`src/registry.rs`, 707) | Host-to-local-prefix mapping. `Registry` trait (`:63`), `resolve_to_clone_info` (`:235`) — the shared resolution path for `fetch` and `init --adopt`. Unrelated to integrations despite the name. |
| `selector` (`src/selector.rs`, 460) | The shared `--role` / `--repo` filter grammar. |

### Data model

| Module | Owns |
|---|---|
| `naming` (`src/naming.rs`) | The flat-address grammar — the `--` weave separator and the `+` segment escape — and the name types it constrains: `ProjectName`, `WorkweaveName`, `RefNameError`, `validate_ref_name`, and the typed rendering pair `weave_dir_name` / `parse_weave_dir_name`. **No `use crate::` anywhere in the file**; every consumer reaches down to it. `manifest`, `vcs` and `workspace` re-export the names they used to own, so their public paths are unchanged. |
| `manifest` (`src/manifest.rs`, 2666) | `Manifest` (`:937`), `LockFile` (`:1112`), `Project` (`:1352`), and the newtypes `RepoPath`, `RepoEntry`, `Role`, `RepoUrl`. `ProjectName` and `WorkweaveName` are re-exports from `naming`. |
| `lock` (`src/lock.rs`, 427) | Snapshotting member HEADs into `rwv.lock`. |
| `workweave` (`src/workweave.rs`, 3799) | Workweave create / delete / list / log, and `CheckoutKind` classification. |
| `workweave_index` (`src/workweave_index.rs`, 1833) | The primary-side `.rwv-workweave-index` (`:99`) and `RefRegistry` (`:595`) — the ref-ownership receipt store. |
| `op_state` (`src/op_state.rs`) | The in-flight-operation record: `OpVerb`, `OpPhase`, `OwnerRecord`, `PhaseTips`, `TouchedWorkspaces`, `acquire_op`; and `OpId` (`:90`) / `SyncStrategy` (`:139`), the two record fields the engine also reads. |
| `owned_state` (`src/owned_state.rs`, 1025) | The attested-generation ledger `.rwv-owned-digests` (`OWNED_DIGESTS_FILE`, `:48`): what rwv last accepted for each fully-owned generated file, and the inputs a generation read. Consumed by `activate`, `check`, `workweave` and the cargo integration. |
| `durable_file` (`src/durable_file.rs`) | The one whole-file publish path: `replace` (overwrite) and `create_new` (refuse an occupied target), both temp-then-fsync-then-publish. Used by `workweave_index` and `op_state`. |

### Verbs

`activate`, `add_remove`, `check` (`rwv doctor`, 8712 lines — the largest
module), `fetch`, `init`, `push`, `status`, `sync`, `update`, `prime`, `setup`,
`explain`. Each owns one verb's orchestration and its `--json` envelope type
(§7). `sync` (6779) additionally owns the phase machine (§5).

### Seams and shared machinery

| Module | Owns |
|---|---|
| `vcs` (`src/vcs.rs`, 3087) | The `Vcs` trait (`:1762`), its witness and warrant types, and `VcsError` (`:296`). §6.1. |
| `git` (`src/git.rs`, 3088) | The one implementor, `GitVcs` (`:330`, `impl` at `:731`), plus `git_command()` (`:32`). |
| `integration` (`src/integration.rs`, 618) | The `Integration` trait (`:416`) and `IntegrationContext`; the finding vocabulary `Issue` / `IssueKind` / `MemberIncompatibility` (`:302`) the trait returns. §6.2. |
| `integrations/` | Eight built-in implementors plus `merge.rs`, the managed-file merge engine they share. |
| `integration_runner` (`src/integration_runner.rs`, 786) | The lifecycle driver: enablement, error containment, and the six entry points that call the trait. |
| `plugins` (`src/plugins.rs`, 726) | External-subcommand discovery and dispatch. §6.4. |
| `parallel` (`src/parallel.rs`, 579) | Bounded per-repo fan-out for the network-bound verbs. |

## 3. Process model

One process, one verb, then exit. `run()` (`src/cli/dispatch.rs:321`) proceeds
in five ordered steps.

**1. Pre-parse argv interception** (`dispatch.rs:325-461`). A raw
`std::env::args()` scan, before clap sees anything, that turns retired flag
spellings into `exit(2)` migration errors. It also catches a mistyped
`rwv workweave <PROJECT> <WORD>` subcommand with a Levenshtein guard
(`SUBCOMMAND_TYPO_THRESHOLD = 2`, `dispatch.rs:447`).

**2. Command build** (`dispatch.rs:473-479`). `Cli::command()` plus a
dynamically appended "External commands" `after_help` section listing the
plugins found on `PATH` (§6.4).

**3. Parse** (`dispatch.rs:487-502`).

**4. The single resolution point** (`dispatch.rs:504-561`). Two steps, in
order:

- *Workspace origin.* With no `-C`, `workspace::acquire_origin_dir()`
  (`src/workspace.rs:52`) — the only cwd read that feeds resolution; the one
  other reader is `workweave delete`'s step-out probe, which treats the cwd
  as an open handle to release, never as an origin. With `-C`, the
  canonicalized override. **Resolution never `chdir`s** (`dispatch.rs:517-520`);
  every path is absolute from here on. The one `chdir` in the tree is that
  same step-out, long after resolution, releasing the deleting process's own
  handle on a workweave it is about to remove.
- *Workweave selection.* `-w <project>--<name>` is resolved against the
  primary-side index and validated against the target's `.rwv-workweave`
  marker; the resulting path replaces the origin.

`--project` is deliberately *not* global — it is a per-verb flag on the nine
verbs that act on a project, and feeds `WorkspaceContext::resolve` as
`project_override`.

**5. The verb match** (`dispatch.rs:562-1116`), which calls one module's entry
point. The unmatched-verb arm falls through to plugin dispatch (§6.4).

### Where parallelism lives

There are no threads outside `parallel::run_in_parallel`
(`src/parallel.rs:175`), which runs a closure over items in a bounded
`std::thread::scope` pool and gathers results in input order. `fetch` and
`update` default to `min(available_parallelism, 8)`; `sync`, `sync-to` and
`push` default to serial, because their `--json` envelope is a single document
and `-j N > 1` switches them to NDJSON (§7).

## 4. State & ownership

All durable rwv state is files. There is no database and no cache directory.

| File | Constant | Location | Owns |
|---|---|---|---|
| `rwv.toml` | — | `projects/<name>/` | Committed intent: membership, roles, per-integration config. |
| `rwv.lock` | — | `projects/<name>/` | Committed intent: the revision each member is pinned to. |
| `.rwv-active` | `ACTIVE_PROJECT_FILE`, `src/workspace.rs:247` | weave root | Which project the primary root currently presents. Ambient default. |
| `.rwv-workweave` | `WORKWEAVE_MARKER_FILE`, `src/workspace.rs:250` | each workweave root | A workweave's only identity file: `{primary, project, parent}`. Self-describing without the index. |
| `.rwv-workweave-index` | `INDEX_FILENAME`, `src/workweave_index.rs:99` | `projects/<project>/` | The primary's inverse view: container path, name→path map, and `RefRegistry` receipts. |
| `.rwv-op` | `OP_STATE_FILE`, `src/op_state.rs:73` | a workspace root | The owner record of an in-flight multi-repo operation. §5. |
| `.rwv-op-lease` | `OP_LEASE_FILE`, `src/op_state.rs:76` | every other workspace the operation mutates | A mutex plus a redirect to the owner. Immutable once written. |
| `.rwv-owned-digests` | `OWNED_DIGESTS_FILE`, `src/owned_state.rs:48` | beside a generated file | SHA-256 of fully-owned generated content, so drift is detectable. |

Two ownership rules are structural rather than conventional:

**The marker and the index point opposite ways, and the marker wins.** The
marker tells a workweave where its primary is; the index tells a primary where
its workweaves are. The index is **advisory** — every entry consumed from it is
validated by round-tripping through the target's marker (`marker.primary`
canonicalizes to this primary, `marker.project` matches). A stale or foreign
entry degrades to `None` plus a `rwv doctor` finding; destructive operations
hard-require the round-trip (`src/workweave_index.rs:63-79`).

**`RefRegistry` is a receipt store, not a cache.** It records that rwv created a
particular ref name in a particular store, and it is the reason rwv can destroy
a ref: without a receipt, a ref is somebody else's. It is homed in the
*primary's* project checkout rather than in the workweave's marker because
deleting a workweave is a `remove_dir_all`, which would take a marker-homed
receipt with it (`src/workweave_index.rs:48-53`).

`.rwv-active` and `.rwv-workweave` are **mutually exclusive**; `rwv doctor`
reports the conflict.

## 5. The sync phase machine

`sync` and `sync-to` are one machine. `src/sync.rs` exposes four public entry
points — `run_sync` (`:1906`), `run_sync_json` (`:5175`), `run_sync_to`
(`:5300`), `run_sync_to_json` (`:5314`) — that all delegate to one private
driver, `run_machine` (`:1932`).

### Shape

The phase type is `op_state::OpPhase` (`src/op_state.rs:119`), a four-variant
enum that serializes kebab-case:

```rust
pub enum OpPhase { Replay, Relock, AdvanceTarget, Retire }
```

The full lifecycle is eight stages —
`guard → mark → savepoint → replay → relock → advance-target → retire → cleanup`
(`src/op_state.rs:112-114`) — but only the middle four are *phases*. Guard,
mark and savepoint run once before the loop, inside `guard_and_mark`
(`src/sync.rs:2365`); cleanup runs once after it (`src/sync.rs:4265`).

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

(`src/sync.rs:1964`.) `run_phase` (`:1986`) is a four-arm match; each arm calls
one phase function and returns the next phase, with `None` terminal.
`next_after_relock` (`:2012`) is where plain `sync` and `sync-to` diverge —
`sync` terminates after relock, `sync-to` continues to advance-target.

Per phase: `run_replay` (`:3576`), `run_relock` (`:4006`),
`run_advance_target` (`:4097`), `run_retire` (`:4234`).

### Why the loop reads from disk

`op_state::set_phase` (`src/op_state.rs:696`) is the **single persistence
point** of the whole machine. The driver loop's call (`sync.rs:1957`) is a
*post*-transition write; resume entry (`sync.rs:2806`) writes through the same
function before re-entering a phase, so the write itself is not ordered with
respect to the transition — the loop's use of it is. The invariant, stated at
`src/sync.rs:1953-1962`, is that the persisted phase is the phase in progress
and every phase is re-runnable from the record alone. That gives three crash
positions and one rule:

- inside `run_phase` — the record still names the running phase; resume
  re-enters it;
- after `run_phase` returned but before `set_phase` committed — the record
  still names the just-completed phase; resume re-runs it (idempotently), then
  transitions;
- after `set_phase` committed — resume enters the next phase directly.

`--continue` is therefore not a separate code path: `run_machine` chooses
`load_continuing_context` (`:2727`) over `guard_and_mark` and enters the same
loop. There are no resume flags threaded through call stacks
(`src/sync.rs:1740`).

### What the record holds, and what it deliberately does not

`OwnerRecord` (`src/op_state.rs:242`) is YAML, written only at the initiating
workspace; other mutated workspaces get a `LeaseRecord` (`:414`) that is an id
plus a pointer. `resolve_to_owner` (`:464`) follows the pointer, so `--continue`
and `abort` behave identically from either side.

Per-repo *progress* is not in the record — savepoint refs and the VCS's own
mid-operation state already are that. What the record holds is per-repo
*intent*, which the VCS cannot: `PhaseTips` (`:165`) is a two-state enum,
`Replay(map)` before relock and `Converged(map)` after, swapped at a single
point by `PhaseTips::converge` (`:219`). It has no serde impls of its own; the
`WireOwnerRecord` shim (`:283`) flattens it into two independent YAML keys,
`advanced_tips` and `converged_tips`.

That distinction is what `abort` consumes. `run_abort` (`src/sync.rs:4688`) runs
two rails per repo: it first writes a pre-abort ref at the current tip, then
performs a **verified restore** — resetting to the savepoint only when the
observed tip is attributable to the operation. `VerifiedRestoreOutcome`
(`src/vcs.rs:553`) enumerates the answers, including `ForeignTip`, which is
reported rather than reset and blocks the record from being cleared
(`src/sync.rs:4935-4945`).

Savepoints are git refs under `refs/rwv/pre-op/<op-id>`; pre-abort refs under
`refs/rwv/pre-abort/<op-id>`. `OpId` (`src/op_state.rs:90`) is nanoseconds since
epoch, minted by `OpId::new_now()` (`:100`).

## 6. The seams

### 6.1 `Vcs`

`pub trait Vcs` (`src/vcs.rs:1762`) is the whole VCS surface: every VCS
operation is a method on it, wide and flat — no supertraits, no associated
types, object-safe. One implementor exists —
`GitVcs` (`src/git.rs:330`), a unit struct, `impl` at `src/git.rs:731`. Callers
reach it two ways: `vcs_for(VcsType)` (`src/vcs.rs:280`) returning
`Box<dyn Vcs>` for the manifest-driven paths, and direct use of the unit value
`GitVcs` elsewhere. `manifest::VcsType` has one variant today; `jj`, `sl` and
`hg` appear only in the trait's doc comment.

The trait divides at a banner (`src/vcs.rs:2458-2477`) into a pre-branch-model
half and the branch model proper. The second half is the part that carries
architecture rather than mechanism: **every ref write is one of four kinds —
MOVE, ATTACH, DESTROY, DESTROY-STORE — and each method takes the proof its kind
requires.** The proof is a value that cannot be forged:

- **Observation witnesses.** `AttachedRef` (`:1187`), `UnbornRef` (`:1248`) and
  `DetachedHead` (`:1275`) have no public constructor. The only code that
  builds them is the default body of `Vcs::head_attachment` (`:2499`), which
  wraps the implementable half, `observe_head` (`:2490`) returning
  `HeadObservation` (`:1458`). Splitting the observable enum from the witness
  enum is what makes the witnesses unforgeable. `verify_attachment` (`:2523`)
  re-observes and fails with `VcsError::StaleRefWitness` if the world moved.
- **Ownership receipts.** `OwnedRef` (`:1066`) means rwv holds a persisted
  receipt for exactly this name in exactly this store.
  `OwnedRef::from_receipt` is `pub(crate)` with two callers, both in
  `src/workweave_index.rs` — the registry read and write paths (§4). A ref rwv
  did not record creating cannot be spoken of as owned.
- **Warrants.** `DeletionWarrant` (`:1667`) wraps a private enum with four
  constructors, so "may I delete this" is answered by construction rather than
  by a boolean argument. `DiscardWarrant` (`:1630`) pairs a `SavepointRef`
  (`:1597`) with an operator consent token: a destructive reset is
  representable only when a savepoint provably exists.
- **Consent tokens.** Four of the five live in `pub mod consent`
  (`src/cli/consent.rs`), not in `vcs`, with `from_flag` constructors that are
  `pub(in crate::cli)`. That privacy is the reason `src/main.rs` must stay
  empty (§2): the tokens are mintable only from a flag, inside the module that
  parses flags.

Several witness types deliberately have **no `as_str()`** — `TrackingRef`,
`OwnedRef`, `AttachedRef` — so a caller cannot launder a proof back into a
bare string. `compile_fail` doctests pin this.

`VcsError` (`:296`) carries a `kind()` (`:355`) of stable kebab-case tags and a
serializable mirror, `VcsErrorOutput` (`:380`), for `--json`.

**What the seam holds by compiler, and what it does not.** No production frame
outside `git.rs` spawns git or assembles its argv: `git_command` is private to
the module, and the only `Command::new("git")` elsewhere in `src/` are two
`#[cfg(test)]` fixtures in `sync.rs`. The remote name is held the same way — the
constant is private and no trait method accepts one — so core can neither spell
it nor be handed a parameter to spell it into. What is not mechanised is git
vocabulary in *operator text*: four message sites in core still write `origin`
as a literal, three of them inside recovery advice that names a git command as
well. The published contract for this seam, and that residue in full, is
[`docs/explanation/joints/vcs-as-seam.md`](docs/explanation/joints/vcs-as-seam.md).

### 6.2 `Integration`

`pub trait Integration` (`src/integration.rs:416`) is ten methods: five
required (`name`, `default_enabled`, `activate`, `deactivate`, `check`) and
five defaulted (`activate_hook`, `generated_files`, `managed_files`, `verify`,
`member_incompatibility`).

Eight built-in implementors, all unit structs, returned in fixed order by
`builtin_integrations()` (`src/integrations/mod.rs:23`): `npm-workspaces`,
`pnpm-workspaces`, `go-work`, `uv-workspace`, `cargo-workspace`, `gita`,
`vscode-workspace`, `static-files`.

**There is no id-to-implementation lookup.** No map, no registry, no dynamic
loading. The set is the fixed `Vec`, and `name()` is consulted for exactly one
purpose: fetching that integration's config out of `manifest.integrations`
inside `for_each_enabled` (`src/integration_runner.rs:131`). Integrations are
compiled in; the plugin mechanism (§6.4) is unrelated and does not reach this
trait.

`integration_runner` owns the lifecycle, and the division is worth stating
because the two halves are easy to confuse:

- **`integration.rs`** defines the contract and the per-integration input,
  `IntegrationContext`.
- **`integration_runner.rs`** assembles that input once per cycle
  (`IntegrationContextBase`, `:17`), precomputes the on-disk detection cache
  (`build_detection_cache`, `:52`), filters by enablement, and drives six
  entry points — `run_activations` (`:170`), `run_checks` (`:188`),
  `run_verifications` (`:209`), `run_member_incompatibilities` (`:232`),
  `run_activate_hooks` (`:257`), `run_deactivations` (`:276`). It does **not**
  own the list: callers pass `&[&dyn Integration]`.
- Errors are contained per integration: one integration returning `Err` becomes
  an `Issue` tagged with its name, and iteration continues.

`src/integrations/merge.rs` is the shared managed-file engine the hybrid
implementors call — the `ManagedDoc` trait (`:210`) and its `JsonDoc` /
`TomlDoc` / `YamlDoc` / `GoWorkDoc` implementations, `merge_activate` (`:282`)
/ `strip_deactivate` (`:358`), and the ownership-marker machinery — so the
hybrid-file invariants live once rather than per integration. It imports
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

### 6.3 Type-level enforcement, in one place

Three of the mechanisms above are the same technique applied at different
scales, and they explain several otherwise-odd shapes in the tree:

| Shape | Enforces |
|---|---|
| `ResolvedRevisionId` (`src/vcs.rs:40`) serializes but does not deserialize; `RawRevisionId` (`:166`) does both | A revision read from a lock file cannot be mistaken for one resolved against a repo. |
| `ResolvedLockFile` (`src/manifest.rs:1153`) has no `Deserialize` | Same rule at the file level: parsing yields `LockFile`; resolution is a separate, explicit step (`resolve_versions`, `:1246`). |
| Witnesses, receipts, warrants and consent tokens (§6.1) | A destructive VCS operation is unrepresentable without the proof its class requires. |

### 6.4 Plugin dispatch

A plugin is an executable named `rwv-<verb>` on `PATH`. There is no protocol,
no envelope message, and no stdio contract.

Discovery is `which`-based: `find_plugin` (`src/plugins.rs:303`) for a single
lookup, `discover_plugins` (`:146`) for the `--help` listing and for
`rwv doctor`. First occurrence in `PATH` order wins; later copies are reported
as `shadowed` rather than silently dropped. Core verbs always win — clap parses
them before the external-subcommand fallthrough fires, so a `rwv-status` on
`PATH` can never shadow the built-in (`src/plugins.rs:7-9`).

Invocation is a plain subprocess: `build_command` (`:264`) is the single seam
that constructs it, `dispatch_external` (`:319`) spawns and waits, and the call
site is `src/cli/dispatch.rs:1112`. Stdio is fully inherited — the child owns
the terminal and rwv wraps none of its output. Exit status propagates, with
signals mapped to `128 + sig`.

**The envelope is environment variables.** `envelope_vars` (`:92`) projects
`workspace::Resolution` (§7) into four variables: `RWV_VERSION`,
`RWV_WORKSPACE`, `RWV_WORKWEAVE`, `RWV_PROJECT`. Presence of `RWV_WORKWEAVE`
encodes the checkout kind; there is no `kind` field. rwv never reads these back
— they are outputs only, and a plugin that wants to call rwv passes them as
arguments (`src/plugins.rs:62-68`). The addressing flags rwv consumed (`-C`,
`-w`, `--project`) never appear in the child's argv.

Compatibility handling is exactly one mechanism: `RWV_VERSION`. There is no
negotiation, no minimum-version check, and no capability handshake.

## 7. Output and generated artifacts

**There is no shared output envelope.** Each `--json`-capable verb owns its own
top-level struct — `StatusJsonOutput` (`src/status.rs:22`), `FetchJsonOutput`
(`src/fetch.rs:60`), `UpdateJsonOutput` (`src/update.rs:75`), `PushJsonOutput`
(`src/push.rs:91`), `SyncJsonOutput` (`src/sync.rs:484`), `SyncToJsonOutput`
(`:509`) — each pinning its own schema URL. `rwv doctor` has no library struct
at all; it builds a `serde_json::Value` in `build_doctor_json`
(`src/check.rs:8185`).

What *is* shared is `Resolution` (`src/workspace.rs:1017`): every envelope
carries it, and it is the same value the plugin envelope projects (§6.4). Under
`-j N > 1` the envelope is dropped and each per-repo record is emitted as a
self-describing NDJSON line via `#[serde(flatten)]` wrappers.

Stdout belongs to the JSON-capable verbs; operator prose goes to stderr
(`src/workspace.rs:741-742`).

### Generated docs are checked into the tree

`generate-explain` is a second binary, not a build script, and it does two jobs.

It **assembles**: each of the 16 `docs/reference/explain/templates/<verb>.md.tmpl`
becomes `docs/reference/explain/<verb>.md`, and for the seven verbs with a JSON
envelope the schemars-derived schema is spliced in and also written out to
`docs/reference/schemas/<verb>.json`. `src/explain.rs:14-30` then `include_str!`s
the assembled markdown into the binary, which is how `rwv explain` works with no
filesystem access at runtime.

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

- **Unit tests in `src/`** — 21 of the 29 files directly under `src/` carry
  `#[cfg(test)] mod tests`. The exceptions are `check.rs`, `integration.rs`,
  `lock.rs`, `explain.rs`, `init.rs` and `update.rs`, plus `lib.rs` and
  `main.rs`, which hold no logic.
- **Integration tests** — `tests/*.rs`, each its own binary, driving the `rwv`
  executable across the process boundary via `assert_cmd`. `tests/common/mod.rs`
  supplies a `git()` builder that strips every inherited `GIT_*` variable, so a
  subprocess can never reach the outer repository.
- **Contract helpers** — `tests/common/contract.rs` encodes the hybrid
  file-ownership regressions once, as free functions taking activate/deactivate
  closures, so every integration is held to the same four shapes.

Two families are worth naming because they pin *documents* rather than code:
`doc_claims_*_test.rs` asserts that published claims match behaviour, and
`branch_model_compile_fail_test.rs` pins the consent-token privacy by rustc
error code — the type-level rules of §6.3 fail the suite if they are relaxed.

`scripts/ci-local.sh` is the single gate: `cargo check`, `cargo test --release`,
`cargo clippy -D warnings`, `cargo fmt --check`, then the explain-artifact drift
gate. `.github/workflows/ci.yml` runs that same script; a separate Windows job
runs `cargo check` only, because the suite assumes symlinks, permission bits and
signals.

## 9. Where the contract lives

This document describes implementation. The **contracts** — what rwv promises,
in language that survives a refactor — live in `docs/`, and are the published
surface:

- [`docs/explanation/joints/`](docs/explanation/joints/) — the normative
  contracts, one per seam. `sync-semantics.md` owns the phase machine, record
  schema, and abort's verified-restore rules (§5); `vcs-as-seam.md` owns the
  VCS boundary (§6.1); `file-ownership.md` owns the two ownership axes, the
  hybrid-merge contract, and the regeneration trigger model (§6.2);
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
