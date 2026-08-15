//! Op-state files: multi-workspace operation tracking for `rwv sync` and `rwv sync-to`.
//!
//! # Schema v2 (no back-compat with v1)
//!
//! An in-flight v1 `.rwv-op` across an upgrade is resolved by the operator
//! with `abort`. Do not add v1-compat parsing.
//!
//! ## Owner record (`.rwv-op`)
//!
//! Written at the **initiating workspace** (the owner). Holds all op parameters
//! plus the current phase. It is the sole copy of mutable op state.
//!
//! ```json
//! {
//!   "id": "1779769917405921588",
//!   "verb": "sync",
//!   "strategy": "rebase",
//!   "project": "web-app",
//!   "source": "/abs/path/src",
//!   "target": "/abs/path/tgt",
//!   "retire": false,
//!   "phase": "replay",
//!   "advanced_tips": {},
//!   "converged_tips": {},
//!   "overrides": [],
//!   "started_at": "2026-06-10T21:14:03Z"
//! }
//! ```
//!
//! `id` is the op id, shared with savepoint refs. `project` is the project
//! both `source` and `target` are resolved under. `verb` is `sync` or
//! `sync-to`; `strategy` is `ff` or `rebase`; `phase` is `replay`, `relock`,
//! `advance-target`, or `retire`. `advanced_tips` is replay-phase intent (repo
//! → planned/actual tip): empty before replay entry, cleared at relock in the
//! same write that populates `converged_tips`, which is written at relock
//! completion and empty before.
//!
//! ## Thin lease (`.rwv-op-lease`)
//!
//! Written at every **other workspace the op mutates** (never at the owner).
//! Immutable once written; a mutex plus a redirect, nothing else.
//!
//! ```json
//! {
//!   "id": "1779769917405921588",
//!   "owner": "/abs/path/to/owner/workspace",
//!   "created_at": "2026-06-10T21:14:03Z"
//! }
//! ```
//!
//! ## Read-only workspaces
//!
//! Not marked. Safe because source reads are snapshots: a read resolves its
//! content at a revision, never from the working tree, so a concurrent write
//! to that workspace cannot tear it.
//!
//! ## Acquisition atomicity
//!
//! For `sync` / `sync-to`, the owner record and every touched-workspace lease
//! are acquired **atomically at guard time** via [`acquire_op`]: each file is
//! published with `crate::durable_file::create_new`, and on `AlreadyExists`
//! anywhere the acquisition unwinds any partial state it created and returns
//! the standard in-flight refusal. This closes the guard→mark TOCTOU window
//! that a plain [`check_no_op_in_progress`] would leave: two concurrent ops
//! could otherwise both pass a `check_no_op_in_progress` guard and only collide
//! later at the git layer. The acquired handle carries the touched-workspace
//! set so a precondition refusal after acquisition can call
//! [`release_acquired`] and clear its records per the cleanup table below.
//!
//! ## Cleanup ownership
//!
//! | Exit path | Record + leases |
//! |---|---|
//! | Success / precondition refusal | Cleared everywhere |
//! | Phase failure / crash | Kept everywhere (`--continue` and `abort` remain) |
//! | `abort` | Cleared after restore |
//!
//! ## File locations
//!
//! Both files live at the workspace root (same directory as `.rwv-active`).

use crate::durable_file::CreateNewError;
use crate::manifest::ProjectName;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Name of the owner op-state file at the initiating workspace.
const OP_STATE_FILE: &str = ".rwv-op";

/// Name of the thin-lease file at every other mutated workspace.
const OP_LEASE_FILE: &str = ".rwv-op-lease";

// ---------------------------------------------------------------------------
// OpId — newtype for sync operation identifiers
// ---------------------------------------------------------------------------

/// A nanosecond-resolution identifier for one in-flight sync operation.
///
/// Used to namespace pre-op savepoint refs so concurrent or interleaved
/// sync attempts don't collide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpId(String);

impl OpId {
    /// Generate a fresh `OpId` from the current wall-clock time.
    ///
    /// Panics if the system clock is before UNIX_EPOCH. The previous
    /// fallback to a literal "0" sentinel masked a clock invariant: every
    /// pre-epoch run would collide on a single `OpId`, and the savepoint
    /// ref scheme this id keys depends on uniqueness. Per FP-in-Rust:
    /// don't silently default away an invariant.
    pub fn new_now() -> Self {
        let s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before UNIX_EPOCH")
            .as_nanos()
            .to_string();
        Self(s)
    }

    /// Reconstruct an `OpId` from its string form (e.g. when reading the sync
    /// op marker file). `pub(crate)` to keep the constructor inside the
    /// crate — `OpId::new_now` is the only externally legitimate way to mint
    /// a fresh id.
    pub(crate) fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The operator-facing resume command for a mid-op verb.
///
/// A failed op is resumed with `--continue` from the OWNING workspace, which
/// reads every parameter (source, strategy, target, retire, overrides) back
/// out of op-state — so the resume text is `rwv {verb} --continue`, NEVER a
/// hardcoded `rwv sync {source}`. Hardcoding `rwv sync` was wrong two ways:
/// it named the PULL verb for a `sync-to` (landing) op, and it re-supplied
/// arguments the record already holds. The verb comes from the op's `verb`
/// field ([`OpVerb`]) so the string is derived from op-state, not from a guess
/// made where the message is written.
pub fn resume_command(verb: OpVerb) -> String {
    format!("rwv {verb} --continue")
}

// ---------------------------------------------------------------------------
// SyncStrategy — typed sync strategy
// ---------------------------------------------------------------------------

/// How `rwv sync` advances each repo to its lock target.
///
/// `merge` is intentionally not offered (state-space shrink). See
/// `docs/explanation/joints/sync-semantics.md` §"Why no `merge` strategy" for
/// the justification test and the origin-less weave-to-weave escape hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum SyncStrategy {
    /// Fast-forward only; bail if not possible.
    Ff,
    /// Rebase the local branch onto the lock target.
    Rebase,
}

impl fmt::Display for SyncStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ff => "ff",
            Self::Rebase => "rebase",
        })
    }
}

impl FromStr for SyncStrategy {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ff" => Ok(Self::Ff),
            "rebase" => Ok(Self::Rebase),
            // `merge` was removed (state-space shrink). A pre-removal in-flight
            // op recorded with strategy=merge resolves here as an invalid
            // op-state strategy; per the alpha no-back-compat convention the
            // operator aborts (`rwv abort`) and re-invokes. No migration path.
            other => {
                anyhow::bail!("unknown sync strategy `{other}` in op-state; expected ff or rebase")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// OpVerb — which top-level verb started this op
// ---------------------------------------------------------------------------

/// Which top-level verb started this op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OpVerb {
    /// Single-step sync (existing `rwv sync`).
    Sync,
    /// Two-step sync-to.
    SyncTo,
}

impl std::fmt::Display for OpVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sync => f.write_str("sync"),
            Self::SyncTo => f.write_str("sync-to"),
        }
    }
}

// ---------------------------------------------------------------------------
// OpPhase — current phase in the operation lifecycle (v2)
// ---------------------------------------------------------------------------

/// Current phase of the in-flight operation (schema v2).
///
/// Phases are listed in execution order.
///
/// ```text
/// guard → mark → savepoint → replay → relock → advance-target → retire → cleanup
///                                               (sync-to only)   (--retire only)
/// ```
///
/// The persisted phase is always the phase in progress. Every phase is
/// idempotent and re-runnable from the record alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OpPhase {
    /// Manifest repos + project repo strategy phase (today's Phase 2 + 1').
    Replay,
    /// Regenerate and commit `rwv.lock` (today's Phase 3). On completion,
    /// converged tips are written into the owner record.
    Relock,
    /// FF-advance every target repo to its converged tip (sync-to only).
    AdvanceTarget,
    /// Merged-check then workweave removal (`--retire` only).
    Retire,
}

impl std::fmt::Display for OpPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Replay => f.write_str("replay"),
            Self::Relock => f.write_str("relock"),
            Self::AdvanceTarget => f.write_str("advance-target"),
            Self::Retire => f.write_str("retire"),
        }
    }
}

// ---------------------------------------------------------------------------
// PhaseTips — phase-scoped tip table
// ---------------------------------------------------------------------------

/// The op's per-repo tip table, scoped to the lifecycle half that owns it.
///
/// Background: the abort-intent journal records two disjoint tip tables —
/// `advanced_tips` (replay-phase intent) and `converged_tips` (written at
/// relock completion). The original schema carried both as flat `BTreeMap`
/// fields on [`OwnerRecord`], governed only by a comment-enforced temporal
/// invariant ("advanced_tips valid only during replay; cleared in the same
/// write that populates converged_tips"). That left the illegal
/// *both-populated* state representable, ruled out only by convention.
///
/// `PhaseTips` makes that state **structurally unrepresentable**: a record
/// holds exactly one table at a time, and the only transition from the replay
/// table to the converged table is the atomic [`PhaseTips::converge`] swap.
///
/// Wire format: this ADT is *in-memory only*. The persisted `.rwv-op` JSON
/// keeps the historical flat shape (two independent top-level keys
/// `advanced_tips` / `converged_tips`) via the [`OwnerRecord`] serde shim,
/// so persisted op-state round-trips byte-for-byte across this change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhaseTips {
    /// Replay-phase intent table (`advanced_tips`). Empty before replay entry;
    /// extended/overwritten as repos advance; consumed by `abort`.
    Replay(BTreeMap<String, String>),
    /// Converged table (`converged_tips`) written at relock completion.
    /// Consumed by advance-target and abort's HEAD check.
    Converged(BTreeMap<String, String>),
}

impl Default for PhaseTips {
    /// A fresh op starts in the replay half with no recorded tips.
    fn default() -> Self {
        Self::Replay(BTreeMap::new())
    }
}

impl PhaseTips {
    /// The replay-phase intent table, or `None` once converged.
    ///
    /// Readers that want the journal entry for a repo during replay use this;
    /// it yields `None` after [`PhaseTips::converge`], by which point the
    /// replay table no longer exists.
    pub fn advanced(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            Self::Replay(m) => Some(m),
            Self::Converged(_) => None,
        }
    }

    /// The converged table, or `None` while still in the replay half.
    pub fn converged(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            Self::Converged(m) => Some(m),
            Self::Replay(_) => None,
        }
    }

    /// Mutable access to the replay-phase intent table during replay.
    ///
    /// Returns `None` if the record has already converged — replay-phase
    /// writes after convergence are a logic error and the type refuses them.
    pub fn advanced_mut(&mut self) -> Option<&mut BTreeMap<String, String>> {
        match self {
            Self::Replay(m) => Some(m),
            Self::Converged(_) => None,
        }
    }

    /// Atomically swap the replay table out and the converged table in.
    ///
    /// This is the *single guarded place* the temporal invariant lives now:
    /// converging discards the replay table and installs `converged` in one
    /// move, so a record can never hold both. Idempotent re-convergence (a
    /// `--continue` re-running relock) simply replaces the converged table.
    pub fn converge(&mut self, converged: BTreeMap<String, String>) {
        *self = Self::Converged(converged);
    }
}

// ---------------------------------------------------------------------------
// Override — a named consent the operator supplied at invocation
// ---------------------------------------------------------------------------

/// A named consent supplied at invocation and recorded on the owner record.
///
/// Each variant is one CLI flag, and serialises to that flag's name without
/// the leading dashes — the spelling already on disk in every `.rwv-op`
/// written so far. `--continue` re-derives the op's consent from this list
/// and `cleanup` reads it to decide whether the project savepoint survives as
/// the only remaining pointer to discarded commits, so mint and read must be
/// the same value rather than the same text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Override {
    /// The lock-freshness precondition was waived on both sides.
    AllowStaleLock,
    /// Phase 1' may hard-reset the project repo past commits the source does
    /// not carry.
    DiscardLocalCommits,
}

// ---------------------------------------------------------------------------
// OwnerRecord — the full op record at the initiating workspace
// ---------------------------------------------------------------------------

/// The owner op record written to `.rwv-op` at the initiating workspace.
///
/// All path fields are absolute. `started_at` is RFC3339 UTC.
///
/// The replay-intent (`advanced_tips`) and converged (`converged_tips`) tip
/// tables are carried by the [`PhaseTips`] ADT in `tips`, which makes the
/// illegal *both-populated* state unrepresentable (see [`PhaseTips`]). The
/// persisted `.rwv-op` JSON keeps the historical flat shape — two independent
/// top-level keys `advanced_tips` / `converged_tips` — via a serde shim
/// (`WireOwnerRecord`), so on-disk op-state round-trips unchanged.
/// `overrides` records named overrides supplied at invocation for audit
/// fidelity on `--continue`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "WireOwnerRecord", into = "WireOwnerRecord")]
pub struct OwnerRecord {
    /// Unique operation identifier (nanosecond wall-clock string). Shared
    /// with savepoint refs and lease files.
    pub id: String,
    /// Which verb started this op.
    pub verb: OpVerb,
    /// Strategy supplied to the op.
    pub strategy: String,
    /// The project this op operates on. Both [`Self::source`] and
    /// [`Self::target`] resolve under it and neither one's `.rwv-active` is
    /// consulted: the pointer is ambient state an operator may retarget while
    /// the op is parked, and a workspace pointing elsewhere still holds this
    /// op's repos.
    pub project: ProjectName,
    /// Absolute path of the source workspace.
    pub source: PathBuf,
    /// Absolute path of the target workspace (for `sync`: same as the owner
    /// workspace; for `sync-to`: the named target workspace).
    pub target: PathBuf,
    /// Whether `--retire` was passed.
    pub retire: bool,
    /// Current phase.
    pub phase: OpPhase,
    /// The op's per-repo tip table, scoped to exactly one lifecycle half at a
    /// time (see [`PhaseTips`]). Replay-phase intent during replay; converged
    /// tips after the atomic [`PhaseTips::converge`] swap at relock completion.
    pub tips: PhaseTips,
    /// Named overrides supplied at invocation.
    pub overrides: Vec<Override>,
    /// RFC3339 UTC timestamp when the op started.
    pub started_at: String,
}

// ---------------------------------------------------------------------------
// WireOwnerRecord — flat persisted shape for OwnerRecord (serde shim)
// ---------------------------------------------------------------------------

/// On-disk representation of [`OwnerRecord`]: the historical flat JSON with
/// two independent tip maps. Exists solely to keep the persisted `.rwv-op`
/// shape stable while the in-memory model uses the [`PhaseTips`] ADT.
///
/// The flat↔ADT mapping is driven by which table is populated, preserving the
/// temporal invariant: a converged record (non-empty `converged_tips`) maps to
/// [`PhaseTips::Converged`]; otherwise the record is still in the replay half
/// and maps to [`PhaseTips::Replay`], carrying `advanced_tips`. A both-empty
/// record canonicalises to the empty replay half and serialises identically.
///
/// Schema v2 carries no default for any field here: every write goes through
/// the full struct, so a record missing a key is malformed, not old, and
/// [`read_owner`] surfaces that as a parse error rather than silently filling
/// it in.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireOwnerRecord {
    id: String,
    verb: OpVerb,
    strategy: String,
    project: ProjectName,
    source: PathBuf,
    target: PathBuf,
    retire: bool,
    phase: OpPhase,
    advanced_tips: BTreeMap<String, String>,
    converged_tips: BTreeMap<String, String>,
    overrides: Vec<Override>,
    started_at: String,
}

impl From<WireOwnerRecord> for OwnerRecord {
    fn from(w: WireOwnerRecord) -> Self {
        // Reconstruct the phase-scoped ADT from the two flat maps. A populated
        // converged table is the unambiguous signal that the op has crossed the
        // relock boundary (the swap clears advanced_tips in the same write), so
        // it wins; otherwise the record is still in the replay half.
        let tips = if !w.converged_tips.is_empty() {
            PhaseTips::Converged(w.converged_tips)
        } else {
            PhaseTips::Replay(w.advanced_tips)
        };
        Self {
            id: w.id,
            verb: w.verb,
            strategy: w.strategy,
            project: w.project,
            source: w.source,
            target: w.target,
            retire: w.retire,
            phase: w.phase,
            tips,
            overrides: w.overrides,
            started_at: w.started_at,
        }
    }
}

impl From<OwnerRecord> for WireOwnerRecord {
    fn from(r: OwnerRecord) -> Self {
        // Flatten the ADT back to two maps; the inactive half serialises empty.
        let (advanced_tips, converged_tips) = match r.tips {
            PhaseTips::Replay(m) => (m, BTreeMap::new()),
            PhaseTips::Converged(m) => (BTreeMap::new(), m),
        };
        Self {
            id: r.id,
            verb: r.verb,
            strategy: r.strategy,
            project: r.project,
            source: r.source,
            target: r.target,
            retire: r.retire,
            phase: r.phase,
            advanced_tips,
            converged_tips,
            overrides: r.overrides,
            started_at: r.started_at,
        }
    }
}

impl OwnerRecord {
    /// Build a new [`OwnerRecord`] for a `rwv sync` invocation.
    ///
    /// Initial phase is `Replay` — the first phase the driver will enter.
    pub fn new_sync(
        op_id: &OpId,
        strategy: SyncStrategy,
        project: ProjectName,
        source_workspace: PathBuf,
        cwd_workspace: PathBuf,
    ) -> Self {
        Self {
            id: op_id.as_str().to_owned(),
            verb: OpVerb::Sync,
            strategy: strategy.to_string(),
            project,
            source: source_workspace,
            target: cwd_workspace,
            retire: false,
            phase: OpPhase::Replay,
            tips: PhaseTips::default(),
            overrides: Vec::new(),
            started_at: utc_now_rfc3339(),
        }
    }

    /// Build a new [`OwnerRecord`] for a `rwv sync-to` invocation.
    ///
    /// Initial phase is `Replay` — the first phase the driver will enter.
    pub fn new_sync_to(
        op_id: &OpId,
        strategy: SyncStrategy,
        project: ProjectName,
        cwd_workspace: PathBuf,
        target_workspace: PathBuf,
        retire: bool,
    ) -> Self {
        Self {
            id: op_id.as_str().to_owned(),
            verb: OpVerb::SyncTo,
            strategy: strategy.to_string(),
            project,
            source: cwd_workspace,
            target: target_workspace,
            retire,
            phase: OpPhase::Replay,
            tips: PhaseTips::default(),
            overrides: Vec::new(),
            started_at: utc_now_rfc3339(),
        }
    }

    /// Path to the owner op-state file within `workspace_dir`.
    pub fn path_in(workspace_dir: &Path) -> PathBuf {
        workspace_dir.join(OP_STATE_FILE)
    }
}

// ---------------------------------------------------------------------------
// LeaseRecord — the thin lease at non-owner mutated workspaces
// ---------------------------------------------------------------------------

/// The thin lease written to `.rwv-op-lease` at every workspace the op
/// mutates other than the owner workspace.
///
/// Immutable once written. Provides mutex semantics and a pointer back
/// to the owner record for `--continue` and `abort`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRecord {
    /// Unique operation identifier (nanosecond wall-clock string). Same as
    /// the owner record's `id`.
    pub id: String,
    /// Absolute path to the owner workspace (the workspace holding the full
    /// owner record). Follow this pointer to load op state.
    pub owner: PathBuf,
    /// RFC3339 UTC timestamp at which this lease was written.
    ///
    /// Surfaced in `rwv doctor` dead-lease reports as observability-only
    /// context (RFC3339 raw + humanized elapsed). **Never used as a
    /// decision input** — the classification is structural (owner record
    /// absent or op-id mismatch), not elapsed-time based.
    ///
    /// `write_lease` always populates this. `Option` stays regardless: a
    /// syntactic `Option<T>` field deserializes a missing key as `None`
    /// unconditionally — `#[serde(default)]` has no bearing on it — so this
    /// key is optional on the wire whether or not that reads as intentional,
    /// and `rwv doctor --json`'s dead-lease finding keeps its existing
    /// nullable shape either way.
    pub created_at: Option<String>,
}

impl LeaseRecord {
    /// Path to the lease file within `workspace_dir`.
    pub fn path_in(workspace_dir: &Path) -> PathBuf {
        workspace_dir.join(OP_LEASE_FILE)
    }
}

// ---------------------------------------------------------------------------
// Resolution: owner record or lease → canonical owner record
// ---------------------------------------------------------------------------

/// The result of resolving a workspace's op-state to the canonical owner.
pub struct ResolvedOwner {
    /// The full owner record.
    pub record: OwnerRecord,
    /// The absolute path to the owner workspace (where `.rwv-op` lives).
    pub owner_workspace: PathBuf,
}

/// Resolve a workspace's op-state to the canonical owner record.
///
/// - If `workspace_dir` holds an owner record (`.rwv-op`), returns it directly.
/// - If `workspace_dir` holds a thin lease (`.rwv-op-lease`), follows the
///   pointer and reads the owner record from the recorded owner workspace.
/// - Returns `None` if neither file is present.
/// - Returns an error if a present file cannot be parsed, or if the lease
///   pointer leads to a missing owner record.
///
/// This is the entry point for `--continue` and `abort` invoked from any
/// workspace (owner or leased).
pub fn resolve_to_owner(workspace_dir: &Path) -> anyhow::Result<Option<ResolvedOwner>> {
    let owner_path = OwnerRecord::path_in(workspace_dir);
    if owner_path.exists() {
        let record = read_owner(workspace_dir)?.ok_or_else(|| {
            anyhow::anyhow!(
                "internal: owner record disappeared at {}",
                workspace_dir.display()
            )
        })?;
        return Ok(Some(ResolvedOwner {
            record,
            owner_workspace: workspace_dir.to_path_buf(),
        }));
    }

    let lease_path = LeaseRecord::path_in(workspace_dir);
    if lease_path.exists() {
        let lease = read_lease(workspace_dir)?.ok_or_else(|| {
            anyhow::anyhow!("internal: lease disappeared at {}", workspace_dir.display())
        })?;
        // Follow the pointer.
        let record = read_owner(&lease.owner)?.ok_or_else(|| {
            anyhow::anyhow!(
                "lease at {} points to {}, but no owner record found there; \
                 the owner workspace may have been moved or its op-state manually \
                 removed. Run `rwv abort` in the owner workspace to clean up, or \
                 manually remove {} and {}.",
                workspace_dir.display(),
                lease.owner.display(),
                workspace_dir.join(OP_LEASE_FILE).display(),
                lease.owner.join(OP_STATE_FILE).display(),
            )
        })?;
        return Ok(Some(ResolvedOwner {
            record,
            owner_workspace: lease.owner,
        }));
    }

    Ok(None)
}

// ---------------------------------------------------------------------------
// Owner record I/O
// ---------------------------------------------------------------------------

/// Write `record` to the `.rwv-op` file in `workspace_dir`.
///
/// This is the **phase-persistence write path**: [`set_phase`] and any
/// caller updating fields on an *already-acquired* owner record (overrides,
/// converged tips, `--continue` restarts) call it. It overwrites any existing
/// file.
///
/// **The first write of a fresh op must go through [`acquire_op`]**, not this
/// function — a bare `write_owner` would silently overwrite a peer op's
/// in-flight record. Callers that just need a check without a claim use
/// [`check_no_op_in_progress`].
pub fn write_owner(workspace_dir: &Path, record: &OwnerRecord) -> anyhow::Result<()> {
    let path = OwnerRecord::path_in(workspace_dir);
    let json = serde_json::to_string_pretty(record).context("failed to serialize owner record")?;
    crate::durable_file::replace(&path, json.as_bytes())
}

/// Read the `.rwv-op` file from `workspace_dir`, returning `None` if absent.
///
/// Returns an error if the file exists but cannot be parsed.
pub fn read_owner(workspace_dir: &Path) -> anyhow::Result<Option<OwnerRecord>> {
    let path = OwnerRecord::path_in(workspace_dir);
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read owner record from {}", path.display()))?;
    let record: OwnerRecord = serde_json::from_str(&json)
        .with_context(|| format!("failed to parse owner record at {}", path.display()))?;
    Ok(Some(record))
}

/// Record `new_phase` as the phase in progress in the owner record at
/// `workspace_dir`.
///
/// Reads the existing file, updates the phase, writes back. The driver loop's
/// post-transition write is one caller; a resume that re-enters an earlier
/// phase is the other, so this is not ordered.
pub fn set_phase(workspace_dir: &Path, new_phase: OpPhase) -> anyhow::Result<()> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no owner record found at {} to advance phase",
            workspace_dir.display()
        )
    })?;
    record.phase = new_phase;
    write_owner(workspace_dir, &record)
}

/// Remove the `.rwv-op` owner record from `workspace_dir`. No-op if absent.
pub fn clear_owner(workspace_dir: &Path) {
    let _ = std::fs::remove_file(OwnerRecord::path_in(workspace_dir));
}

// ---------------------------------------------------------------------------
// Lease I/O
// ---------------------------------------------------------------------------

/// Write `lease` to the `.rwv-op-lease` file in `workspace_dir`.
///
/// A lease is immutable once written; this function should be called exactly
/// once per lease workspace at op start.
pub fn write_lease(workspace_dir: &Path, lease: &LeaseRecord) -> anyhow::Result<()> {
    let path = LeaseRecord::path_in(workspace_dir);
    let json = serde_json::to_string_pretty(lease).context("failed to serialize lease record")?;
    std::fs::write(&path, json)
        .with_context(|| format!("failed to write lease to {}", path.display()))
}

/// Read the `.rwv-op-lease` file from `workspace_dir`, returning `None` if absent.
///
/// Returns an error if the file exists but cannot be parsed.
pub fn read_lease(workspace_dir: &Path) -> anyhow::Result<Option<LeaseRecord>> {
    let path = LeaseRecord::path_in(workspace_dir);
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read lease from {}", path.display()))?;
    let lease: LeaseRecord = serde_json::from_str(&json)
        .with_context(|| format!("failed to parse lease at {}", path.display()))?;
    Ok(Some(lease))
}

/// Remove the `.rwv-op-lease` lease file from `workspace_dir`. No-op if absent.
pub fn clear_lease(workspace_dir: &Path) {
    let _ = std::fs::remove_file(LeaseRecord::path_in(workspace_dir));
}

/// Remove both op-state files (owner record + lease) from `workspace_dir`.
/// No-op for files that are absent.
pub fn clear_all_at(workspace_dir: &Path) {
    clear_owner(workspace_dir);
    clear_lease(workspace_dir);
}

// ---------------------------------------------------------------------------
// Concurrency guard
// ---------------------------------------------------------------------------

/// Check that no op-state file (owner record or lease) exists in any of the
/// given workspace directories.
///
/// This is the cross-verb mutex: any verb that mutates repo state in an
/// involved workspace (sync / sync-to / update / `lock --commit` / workweave
/// delete / retire) calls it and refuses while an op involves that workspace.
///
/// Returns an error if any workspace carries an owner record or a lease. Both
/// branches name the op (verb), its age, the phase it stalled in, and the two
/// exits — `--continue` from the OWNING workspace, or `rwv abort` — so the
/// operator knows exactly what they interrupted and how to clear it. When the
/// workspace only holds a thin lease, the pointer is followed to the owner
/// record so the message carries the same detail (naming the owner workspace
/// where `--continue` must run). A crashed op leaves a stale record on purpose:
/// there is no auto-expiry; `rwv abort`'s verified restore is the way out.
///
/// For non-mutating callers this is enough — they don't write op state, so
/// there is no TOCTOU exposure. For `sync` / `sync-to`, which do write op
/// state, use [`acquire_op`] instead: it performs the same refusal shape but
/// via an atomic create so two concurrent ops cannot both pass and only
/// collide later at the git layer.
pub fn check_no_op_in_progress(workspace_dirs: &[&Path]) -> anyhow::Result<()> {
    for &dir in workspace_dirs {
        if let Some(err) = in_flight_refusal_for(dir)? {
            return Err(err);
        }
    }
    Ok(())
}

/// Build the standard in-flight-op refusal for `dir` if it carries an op-state
/// file, or `Ok(None)` if the directory is clean.
///
/// Split out of [`check_no_op_in_progress`] so [`acquire_op`] can emit the
/// identical refusal shape on an atomic-create `AlreadyExists`.
fn in_flight_refusal_for(dir: &Path) -> anyhow::Result<Option<anyhow::Error>> {
    // Owner record: full detail is right here.
    if let Some(record) = read_owner(dir)? {
        let elapsed = elapsed_since(&record.started_at);
        return Ok(Some(anyhow::anyhow!(
            "{verb} in progress (started {elapsed} ago, mid `{phase}`) at {dir}.\n\
             Rerun with `{resume}` from that workspace after resolving, \
             or `rwv abort` to discard.",
            verb = record.verb,
            phase = record.phase,
            dir = crate::path_spelling::operator_path(dir),
            resume = resume_command(record.verb),
        )));
    }
    // Lease: follow the pointer to the owner record so the refusal carries
    // the same op / age / phase detail and names where `--continue` runs.
    if let Some(lease) = read_lease(dir)? {
        // A resolvable owner record gives the rich message; if the pointer
        // is dangling (owner workspace moved / op-state hand-removed), fall
        // back to the lease's own fields rather than failing the guard —
        // the operator still learns an op involves this workspace.
        match read_owner(&lease.owner) {
            Ok(Some(record)) => {
                let elapsed = elapsed_since(&record.started_at);
                return Ok(Some(anyhow::anyhow!(
                    "{verb} in progress (started {elapsed} ago, mid `{phase}`); this \
                     workspace ({dir}) is leased to it. Owner workspace: {owner}.\n\
                     Rerun with `{resume}` from the owner workspace after \
                     resolving, or `rwv abort` to discard.",
                    verb = record.verb,
                    phase = record.phase,
                    dir = crate::path_spelling::operator_path(dir),
                    owner = crate::path_spelling::operator_path(&lease.owner),
                    resume = resume_command(record.verb),
                )));
            }
            _ => {
                return Ok(Some(anyhow::anyhow!(
                    "op {id} in progress (lease at {dir}; owner workspace: {owner}).\n\
                     Rerun the owning verb with `--continue` from the owner workspace, or \
                     `rwv abort` to discard.",
                    id = lease.id,
                    dir = crate::path_spelling::operator_path(dir),
                    owner = crate::path_spelling::operator_path(&lease.owner),
                )));
            }
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Atomic acquisition (guard→mark TOCTOU fix)
// ---------------------------------------------------------------------------

/// Handle returned by a successful [`acquire_op`] — records which files were
/// atomically created so a subsequent precondition refusal can undo them.
///
/// The plain "concurrency guard" (`check_no_op_in_progress`) is a check, not a
/// claim: two ops can both pass it and collide later at the git layer (R7 root
/// cause). Atomic acquisition writes each file with `O_CREAT|O_EXCL`
/// so the OS refuses the second creator; the handle carries the created set so
/// the caller can [`release_acquired`] on a downstream precondition refusal
/// (per the cleanup table's "refusal → cleared everywhere" row).
///
/// Cleanup ownership after success:
///
/// - Op runs to success / precondition-refusal AFTER acquisition → caller calls
///   [`release_acquired`] to clear the acquired records.
/// - Op crashes after acquisition → records remain on disk; `--continue` /
///   `rwv abort` are the exits (structural cleanup by the dead-lease doctor
///   check when leases become provably-dead is a hygiene backstop, not a
///   policy).
#[must_use = "an acquired op handle must either be committed to the running op \
              or released via release_acquired on refusal"]
#[derive(Debug)]
pub struct AcquiredOp {
    touched: TouchedWorkspaces,
}

impl AcquiredOp {
    /// The owner workspace this acquisition writes into.
    pub fn owner_workspace(&self) -> &Path {
        self.touched.owner()
    }
}

/// The workspaces one op writes op-state into: the owner, which holds
/// `.rwv-op`, and every workspace that holds a `.rwv-op-lease` pointing back
/// at it.
///
/// `sync` mutates only the workspace it runs in, so it has no leases;
/// `sync-to` also mutates the target and leases it. Acquisition and cleanup
/// both take the set from here. Spelled at each site instead, the two drift
/// apart the moment a verb starts touching another workspace, and the leases
/// cleanup then stops clearing surface only as dead-lease doctor findings.
#[derive(Debug, Clone)]
pub struct TouchedWorkspaces {
    owner: PathBuf,
    leases: Vec<PathBuf>,
}

impl TouchedWorkspaces {
    pub fn of(verb: OpVerb, owner_workspace_dir: &Path, dest_workspace_dir: &Path) -> Self {
        let leases = match verb {
            OpVerb::Sync => Vec::new(),
            OpVerb::SyncTo => vec![dest_workspace_dir.to_path_buf()],
        };
        Self {
            owner: owner_workspace_dir.to_path_buf(),
            leases,
        }
    }

    pub fn owner(&self) -> &Path {
        &self.owner
    }

    /// Remove every op-state file this op wrote.
    ///
    /// Leases go first so an interruption part-way leaves an owner record with
    /// no leases — a valid resume target — rather than a lease pointing at a
    /// record that is already gone.
    pub fn clear(&self) {
        for ws in &self.leases {
            clear_lease(ws);
        }
        clear_owner(&self.owner);
    }
}

/// Atomically acquire an op across `touched` — the owner workspace plus every
/// additional workspace the op will mutate.
///
/// Semantics:
///
/// - The owner workspace gets `.rwv-op` written with the full `owner_record`.
/// - Every lease workspace gets `.rwv-op-lease` written pointing back at the
///   owner.
/// - Every file is created with `O_CREAT|O_EXCL`. On `AlreadyExists` anywhere,
///   any files already created by *this* call are removed and the returned
///   error is the standard [`check_no_op_in_progress`]-shape refusal reading
///   the *pre-existing* holder (name, age, phase, `--continue` / `abort`
///   exits). This preserves the cleanup-table row that says a refusal leaves
///   no trace.
/// - A crash between successful acquisition and the caller's next persistent
///   write (Mark's overrides update, savepoints) leaves records with no
///   savepoints. That partial state is what the dead-lease doctor check
///   diagnoses (a lease whose recorded owner workspace has no matching
///   `.rwv-op` with the same op id is structurally dead).
///
/// Ordering: the owner record is written first, then leases. This makes the
/// crash-partial case symmetric to the abort/rollback path — an owner record
/// with no leases is a valid resume target; a lease with no owner record is
/// the dead-lease case doctor auto-fixes.
pub fn acquire_op(
    touched: &TouchedWorkspaces,
    owner_record: &OwnerRecord,
) -> anyhow::Result<AcquiredOp> {
    let owner_workspace_dir = touched.owner();
    let lease_workspaces = &touched.leases;
    // Acquisition dominates every other refusal, so if any touched workspace
    // already carries op-state we must emit the rich in-flight refusal from IT
    // rather than a raw AlreadyExists context. We check first
    // (cheap) and then atomic-create; a losing racer whose file lands between
    // check and create still gets the correct shape via the AlreadyExists
    // branch below (which re-reads and re-derives the same message).
    if let Some(err) = in_flight_refusal_for(owner_workspace_dir)? {
        return Err(err);
    }
    for ws in lease_workspaces {
        if let Some(err) = in_flight_refusal_for(ws)? {
            return Err(err);
        }
    }

    // Owner first, then leases.
    let owner_path = OwnerRecord::path_in(owner_workspace_dir);
    let owner_json = serde_json::to_string_pretty(owner_record)
        .context("failed to serialize owner record for acquisition")?;
    match crate::durable_file::create_new(&owner_path, owner_json.as_bytes()) {
        Ok(()) => {}
        Err(CreateNewError::AlreadyExists) => {
            // Race: another op landed here. Read it back for the standard
            // refusal shape. If it disappeared between EEXIST and re-read
            // (the winner completed successfully already), fall through with a
            // generic refusal — the operator can retry.
            return Err(
                in_flight_refusal_for(owner_workspace_dir)?.unwrap_or_else(|| {
                    anyhow::anyhow!(
                        "raced with a concurrent op at {}; retry",
                        owner_workspace_dir.display()
                    )
                }),
            );
        }
        Err(CreateNewError::Io(e)) => {
            return Err(anyhow::Error::new(e).context(format!(
                "failed to acquire owner record at {}",
                owner_path.display()
            )));
        }
    }

    let mut acquired_leases: Vec<PathBuf> = Vec::with_capacity(lease_workspaces.len());
    for ws in lease_workspaces {
        let lease_path = LeaseRecord::path_in(ws);
        let lease = LeaseRecord {
            id: owner_record.id.clone(),
            owner: owner_workspace_dir.to_path_buf(),
            created_at: Some(utc_now_rfc3339()),
        };
        let lease_json = match serde_json::to_string_pretty(&lease) {
            Ok(s) => s,
            Err(e) => {
                // Serialize failure is not I/O contention but still needs to
                // roll back the owner + any leases we already wrote so refusal
                // leaves no trace.
                rollback_acquired(owner_workspace_dir, &acquired_leases);
                return Err(anyhow::Error::new(e).context("failed to serialize lease record"));
            }
        };
        match crate::durable_file::create_new(&lease_path, lease_json.as_bytes()) {
            Ok(()) => acquired_leases.push(ws.clone()),
            Err(CreateNewError::AlreadyExists) => {
                // Race on a lease: undo owner + any leases we already wrote,
                // then emit the in-flight refusal reading the *existing*
                // lease (following its pointer for the rich message shape,
                // matching what a plain check_no_op_in_progress would emit).
                rollback_acquired(owner_workspace_dir, &acquired_leases);
                return Err(in_flight_refusal_for(ws)?.unwrap_or_else(|| {
                    anyhow::anyhow!("raced with a concurrent op at {}; retry", ws.display())
                }));
            }
            Err(CreateNewError::Io(e)) => {
                rollback_acquired(owner_workspace_dir, &acquired_leases);
                return Err(anyhow::Error::new(e).context(format!(
                    "failed to acquire lease at {}",
                    lease_path.display()
                )));
            }
        }
    }

    Ok(AcquiredOp {
        touched: touched.clone(),
    })
}

/// Undo an acquisition performed by [`acquire_op`] — clear the owner record and
/// every acquired lease. Idempotent; missing files are ignored.
///
/// Called when a precondition refuses AFTER a successful acquisition (the
/// cleanup table's "precondition refusal → cleared everywhere" row), and by
/// [`acquire_op`] itself when a partial acquisition hits `AlreadyExists` on a
/// later file.
pub fn release_acquired(acquired: &AcquiredOp) {
    acquired.touched.clear();
}

fn rollback_acquired(owner_workspace_dir: &Path, acquired_leases: &[PathBuf]) {
    for ws in acquired_leases {
        clear_lease(ws);
    }
    clear_owner(owner_workspace_dir);
}

// ---------------------------------------------------------------------------
// Dead-lease detection (structural, no wall-clock policy)
// ---------------------------------------------------------------------------

/// A lease whose recorded owner workspace has no matching `.rwv-op` with the
/// same op id — the structural dead-lease case.
///
/// Structural because it is derived from ancestry-of-state, not from elapsed
/// time: the operator's precedent (matching the stale-op-state
/// doctor check) is that time may be *surfaced* to the operator, never
/// consumed as policy. A dead lease arises from:
///
/// - crash between owner-record acquisition and the caller writing the lease
///   (this concrete window is why we detect it — the crash pattern the
///   acquire→mark ordering leaves behind);
/// - owner workspace deleted / moved out from under the lease;
/// - manual `rm .rwv-op` without also clearing the lease.
///
/// The reverse pattern (owner record with no matching lease anywhere) is
/// caught by the existing `StaleOpState` doctor finding — that scan already
/// walks every workspace and reports every `.rwv-op` it sees. So the *only*
/// gap this new check covers is the lease-side dangling-pointer case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLease {
    /// Absolute path to the workspace directory holding the dangling lease.
    pub workspace_dir: PathBuf,
    /// Op id recorded in the lease.
    pub op_id: String,
    /// Owner workspace the lease pointed at.
    pub recorded_owner: PathBuf,
    /// Why the lease is dead (owner missing vs owner op id mismatch), for the
    /// human-facing message.
    pub reason: DeadLeaseReason,
    /// RFC3339 UTC timestamp at which the lease was written, carried through
    /// from [`LeaseRecord::created_at`]. Observability-only: surfaced in
    /// doctor reports, never a decision input.
    pub created_at: Option<String>,
}

/// Discriminator for why a lease is structurally dead. Reported to the
/// operator so `doctor` can name the specific shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeadLeaseReason {
    /// The recorded owner workspace has no `.rwv-op` at all.
    OwnerRecordAbsent,
    /// The recorded owner workspace has an `.rwv-op` but with a *different*
    /// op id — the lease references an op the owner has since moved past
    /// (owner cleared and a new op started, but this stale lease survived).
    OwnerOpIdMismatch { owner_op_id: String },
}

/// Inspect the lease at `workspace_dir` and classify it as dead if its owner
/// pointer is dangling. Returns `Ok(None)` when there is no lease, when the
/// lease reads as invalid (surfaced separately by the parse error), or when
/// the lease resolves to an owner record with the matching op id (the live
/// case).
///
/// The classification is purely structural: existence of the owner file and
/// id equality. No elapsed-time input; no filesystem timestamps.
pub fn detect_dead_lease(workspace_dir: &Path) -> anyhow::Result<Option<DeadLease>> {
    let Some(lease) = read_lease(workspace_dir)? else {
        return Ok(None);
    };
    let created_at = lease.created_at.clone();
    let owner_path = OwnerRecord::path_in(&lease.owner);
    if !owner_path.exists() {
        return Ok(Some(DeadLease {
            workspace_dir: workspace_dir.to_path_buf(),
            op_id: lease.id,
            recorded_owner: lease.owner,
            reason: DeadLeaseReason::OwnerRecordAbsent,
            created_at,
        }));
    }
    // Owner record exists — check op-id match. If the owner file is present
    // but unparseable, treat that as a live/undecided case: the stale-op-state
    // check will surface the parse issue via its own path.
    if let Ok(Some(owner_record)) = read_owner(&lease.owner) {
        if owner_record.id != lease.id {
            let owner_op_id = owner_record.id.clone();
            return Ok(Some(DeadLease {
                workspace_dir: workspace_dir.to_path_buf(),
                op_id: lease.id,
                recorded_owner: lease.owner,
                reason: DeadLeaseReason::OwnerOpIdMismatch { owner_op_id },
                created_at,
            }));
        }
    }
    Ok(None)
}

/// Remove the lease file at `workspace_dir`. Used by `doctor --fix` on a
/// dead-lease finding — safe because the classification proved the lease is
/// no longer paired with a live owner record.
pub fn fix_dead_lease(workspace_dir: &Path) {
    clear_lease(workspace_dir);
}

// ---------------------------------------------------------------------------
// --continue: read op-state for resume
// ---------------------------------------------------------------------------

/// Resume a `--continue` attempt by resolving op-state at `workspace_dir`.
///
/// Follows a lease pointer if the workspace holds a lease. Returns the
/// resolved owner record and the owner workspace path.
///
/// Returns an error if no op-state is present.
pub fn resume(workspace_dir: &Path) -> anyhow::Result<(OwnerRecord, PathBuf)> {
    match resolve_to_owner(workspace_dir)? {
        Some(resolved) => Ok((resolved.record, resolved.owner_workspace)),
        None => {
            anyhow::bail!(
                "no sync/sync-to op in progress to continue at {}. \
                 If you meant to start a new op, omit `--continue`.",
                workspace_dir.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// RFC3339 UTC timestamp for "right now". Shared with the health-floor
/// record's `recorded_at` — display-only in both homes, never policy.
pub(crate) fn utc_now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let (year, month, day, hour, min, sec) = unix_secs_to_ymd_hms(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Decompose Unix seconds into (year, month, day, hour, min, sec) UTC.
///
/// Uses the algorithm from Richards (2013) — exact for all dates in the
/// range 1970–2399. No overflow for u64 timestamps within that range.
#[allow(clippy::many_single_char_names)]
fn unix_secs_to_ymd_hms(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let hour = (secs / 3600) % 24;
    let min = (secs / 60) % 60;
    let sec = secs % 60;
    let days = secs / 86400;

    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, hour, min, sec)
}

/// Human-readable elapsed time since `started_at` (RFC3339).
///
/// Used by doctor reports for both `StaleOpState` and `DeadOpLease` findings.
/// Observability only — never a decision input.
pub(crate) fn elapsed_since(started_at: &str) -> String {
    if let Some(elapsed_secs) = parse_rfc3339_to_unix(started_at).map(|start| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(start)
    }) {
        if elapsed_secs < 60 {
            return format!("{elapsed_secs}s");
        } else if elapsed_secs < 3600 {
            return format!("{}m", elapsed_secs / 60);
        } else {
            return format!("{}h", elapsed_secs / 3600);
        }
    }
    started_at.to_owned()
}

/// Parse a `YYYY-MM-DDTHH:MM:SSZ` string to Unix seconds. Returns `None` on
/// any parse failure.
fn parse_rfc3339_to_unix(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: u64 = date_parts.next()?.parse().ok()?;
    let month: u64 = date_parts.next()?.parse().ok()?;
    let day: u64 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let min: u64 = time_parts.next()?.parse().ok()?;
    let sec: u64 = time_parts.next()?.parse().ok()?;

    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y / 400;
    let yoe = y % 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let z = era * 146097 + doe;
    let days = z.checked_sub(719468)?;

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_project() -> ProjectName {
        ProjectName::new("web-app").expect("valid project name")
    }

    /// The resume verb is derived from the op's `verb` field, never hardcoded
    /// `rwv sync`: naming the pull verb for a `sync-to` op sends the operator
    /// into the verb-mismatch refusal.
    #[test]
    fn resume_command_derives_from_op_verb() {
        assert_eq!(resume_command(OpVerb::Sync), "rwv sync --continue");
        assert_eq!(resume_command(OpVerb::SyncTo), "rwv sync-to --continue");
    }

    #[test]
    fn utc_roundtrip_is_plausible() {
        let ts = utc_now_rfc3339();
        assert!(
            ts.len() == 20 && ts.ends_with('Z') && ts.contains('T'),
            "unexpected timestamp format: {ts}"
        );
        assert!(
            parse_rfc3339_to_unix(&ts).is_some(),
            "timestamp {ts} failed to round-trip"
        );
    }

    #[test]
    fn elapsed_since_returns_secs_for_recent() {
        let ts = utc_now_rfc3339();
        let elapsed = elapsed_since(&ts);
        assert!(
            elapsed.ends_with('s'),
            "expected seconds-suffix for fresh timestamp; got {elapsed}"
        );
    }

    // -----------------------------------------------------------------------
    // Owner record round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn owner_write_read_roundtrip_sync() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/src/ws"),
            PathBuf::from("/cwd/ws"),
        );
        write_owner(dir, &record).unwrap();
        let read_back = read_owner(dir).unwrap().unwrap();
        assert_eq!(read_back.id, record.id);
        assert_eq!(read_back.verb, OpVerb::Sync);
        assert_eq!(read_back.strategy, "rebase");
        assert_eq!(read_back.project, test_project());
        assert_eq!(read_back.phase, OpPhase::Replay);
        assert!(!read_back.retire);
        // Fresh record is in the replay half with an empty intent table.
        assert_eq!(read_back.tips, PhaseTips::Replay(BTreeMap::new()));
        assert!(read_back.tips.advanced().unwrap().is_empty());
        assert!(read_back.tips.converged().is_none());
        assert!(read_back.overrides.is_empty());
    }

    // -----------------------------------------------------------------------
    // advanced_tips field round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn advanced_tips_populated_roundtrip() {
        // Write a record with advanced_tips populated, read it back, verify the
        // map survives serialisation/deserialisation unchanged.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let mut record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/src/ws"),
            PathBuf::from("/cwd/ws"),
        );
        let advanced = record.tips.advanced_mut().unwrap();
        advanced.insert("github/foo/bar".to_owned(), "aabbccdd".to_owned());
        advanced.insert("(project)".to_owned(), "deadbeef".to_owned());
        write_owner(dir, &record).unwrap();
        let read_back = read_owner(dir).unwrap().unwrap();
        let advanced = read_back.tips.advanced().expect("still in replay half");
        assert_eq!(advanced.len(), 2);
        assert_eq!(
            advanced.get("github/foo/bar").map(String::as_str),
            Some("aabbccdd"),
        );
        assert_eq!(
            advanced.get("(project)").map(String::as_str),
            Some("deadbeef"),
        );
        // No converged table while in the replay half.
        assert!(read_back.tips.converged().is_none());
    }

    #[test]
    fn wire_record_missing_advanced_tips_key_is_rejected() {
        // Schema v2 carries no default for any WireOwnerRecord field — every
        // write goes through the full struct, so a record missing a key is
        // malformed, not old, and must fail to parse rather than silently
        // filling it in.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{
  "id": "9999999999999999999",
  "verb": "sync",
  "strategy": "ff",
  "project": "web-app",
  "source": "/src/ws",
  "target": "/cwd/ws",
  "retire": false,
  "phase": "replay",
  "converged_tips": {},
  "overrides": [],
  "started_at": "2026-06-01T00:00:00Z"
}
"#;
        std::fs::write(dir.join(OP_STATE_FILE), json).unwrap();
        let err = read_owner(dir).unwrap_err();
        assert!(
            err.to_string().contains("failed to parse owner record"),
            "a record missing advanced_tips must fail to parse; got: {err}"
        );
    }

    #[test]
    fn wire_record_missing_project_key_is_rejected() {
        // The op's project is what abort resolves both of the op's workspaces
        // under. A record without it has no answer that is not a guess at
        // ambient state, so it is malformed rather than defaulted.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{
  "id": "9999999999999999999",
  "verb": "sync",
  "strategy": "ff",
  "source": "/src/ws",
  "target": "/cwd/ws",
  "retire": false,
  "phase": "replay",
  "advanced_tips": {},
  "converged_tips": {},
  "overrides": [],
  "started_at": "2026-06-01T00:00:00Z"
}
"#;
        std::fs::write(dir.join(OP_STATE_FILE), json).unwrap();
        let err = read_owner(dir).unwrap_err();
        assert!(
            err.to_string().contains("failed to parse owner record"),
            "a record missing project must fail to parse; got: {err}"
        );
    }

    #[test]
    fn wire_record_rejects_a_project_name_the_type_refuses() {
        // The wire field is the validated newtype, not a String the reader
        // widens back into one: a name no `ProjectName` could be built from
        // is a parse error at the boundary rather than a path component
        // assembled later.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{
  "id": "9999999999999999999",
  "verb": "sync",
  "strategy": "ff",
  "project": "web--app",
  "source": "/src/ws",
  "target": "/cwd/ws",
  "retire": false,
  "phase": "replay",
  "advanced_tips": {},
  "converged_tips": {},
  "overrides": [],
  "started_at": "2026-06-01T00:00:00Z"
}
"#;
        std::fs::write(dir.join(OP_STATE_FILE), json).unwrap();
        let err = read_owner(dir).unwrap_err();
        assert!(
            err.to_string().contains("failed to parse owner record"),
            "a record whose project name fails validation must fail to parse; got: {err}"
        );
    }

    #[test]
    fn wire_record_missing_converged_tips_key_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{
  "id": "9999999999999999999",
  "verb": "sync",
  "strategy": "ff",
  "project": "web-app",
  "source": "/src/ws",
  "target": "/cwd/ws",
  "retire": false,
  "phase": "replay",
  "advanced_tips": {},
  "overrides": [],
  "started_at": "2026-06-01T00:00:00Z"
}
"#;
        std::fs::write(dir.join(OP_STATE_FILE), json).unwrap();
        let err = read_owner(dir).unwrap_err();
        assert!(
            err.to_string().contains("failed to parse owner record"),
            "a record missing converged_tips must fail to parse; got: {err}"
        );
    }

    #[test]
    fn wire_record_missing_overrides_key_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{
  "id": "9999999999999999999",
  "verb": "sync",
  "strategy": "ff",
  "project": "web-app",
  "source": "/src/ws",
  "target": "/cwd/ws",
  "retire": false,
  "phase": "replay",
  "advanced_tips": {},
  "converged_tips": {},
  "started_at": "2026-06-01T00:00:00Z"
}
"#;
        std::fs::write(dir.join(OP_STATE_FILE), json).unwrap();
        let err = read_owner(dir).unwrap_err();
        assert!(
            err.to_string().contains("failed to parse owner record"),
            "a record missing overrides must fail to parse; got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // PhaseTips ADT — phase-scoped tip table
    // -----------------------------------------------------------------------

    #[test]
    fn phase_tips_converge_is_atomic_swap() {
        // The illegal "both populated" state is unrepresentable: converging
        // discards the replay table and installs the converged one in a single
        // move, so no value of `PhaseTips` ever holds both.
        let mut tips = PhaseTips::default();
        tips.advanced_mut()
            .unwrap()
            .insert("github/foo/bar".to_owned(), "aabb".to_owned());
        assert!(tips.advanced().is_some());
        assert!(tips.converged().is_none());

        let mut converged = BTreeMap::new();
        converged.insert("github/foo/bar".to_owned(), "ccdd".to_owned());
        tips.converge(converged);

        // After the swap the replay table is gone — replay-phase reads/writes
        // now yield None, and only the converged table is visible.
        assert!(tips.advanced().is_none());
        assert!(tips.advanced_mut().is_none());
        assert_eq!(
            tips.converged()
                .and_then(|m| m.get("github/foo/bar"))
                .map(String::as_str),
            Some("ccdd"),
        );

        // Re-convergence (idempotent --continue replay) just replaces the table.
        let mut again = BTreeMap::new();
        again.insert("(project)".to_owned(), "eeff".to_owned());
        tips.converge(again);
        assert!(tips.advanced().is_none());
        assert_eq!(tips.converged().map(BTreeMap::len), Some(1));
    }

    #[test]
    fn converged_tips_wire_roundtrip_and_clears_advanced() {
        // A converged record serialises with a populated `converged_tips:` and
        // an empty `advanced_tips:` (the swap cleared it) — and reads back as
        // the Converged half. This mirrors the J(relock) crash-matrix state.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let mut record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/src/ws"),
            PathBuf::from("/cwd/ws"),
        );
        // Populate the replay half, then converge (the atomic swap).
        record
            .tips
            .advanced_mut()
            .unwrap()
            .insert("github/foo/bar".to_owned(), "aabb".to_owned());
        let mut converged = BTreeMap::new();
        converged.insert("github/foo/bar".to_owned(), "ccdd".to_owned());
        converged.insert("(project)".to_owned(), "deadbeef".to_owned());
        record.tips.converge(converged);
        record.phase = OpPhase::Relock;

        write_owner(dir, &record).unwrap();
        // The persisted JSON keeps the flat shape with advanced_tips emptied.
        let raw = std::fs::read_to_string(dir.join(OP_STATE_FILE)).unwrap();
        assert!(
            raw.contains("\"advanced_tips\": {}"),
            "expected emptied flat advanced_tips key, got:\n{raw}"
        );
        assert!(
            raw.contains("\"converged_tips\""),
            "expected converged_tips key, got:\n{raw}"
        );

        let read_back = read_owner(dir).unwrap().unwrap();
        assert_eq!(read_back, record);
        assert!(read_back.tips.advanced().is_none());
        let converged = read_back.tips.converged().expect("converged half");
        assert_eq!(converged.len(), 2);
        assert_eq!(
            converged.get("github/foo/bar").map(String::as_str),
            Some("ccdd"),
        );
    }

    #[test]
    fn both_empty_tip_maps_canonicalise_to_replay() {
        // Both maps empty is ambiguous on the wire: it's the common at-entry
        // state (a fresh op, still in replay), but it is *also* what
        // Converged(empty) — relock with zero repos — flattens to, since the
        // From<OwnerRecord> impl always empties the inactive half. The
        // canonicalisation always resolves that ambiguity to the replay half,
        // regardless of the recorded `phase`.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{
  "id": "1",
  "verb": "sync",
  "strategy": "rebase",
  "project": "web-app",
  "source": "/src/ws",
  "target": "/cwd/ws",
  "retire": false,
  "phase": "relock",
  "advanced_tips": {},
  "converged_tips": {},
  "overrides": [],
  "started_at": "2026-06-01T00:00:00Z"
}
"#;
        std::fs::write(dir.join(OP_STATE_FILE), json).unwrap();
        let record = read_owner(dir).unwrap().unwrap();
        assert_eq!(record.tips, PhaseTips::Replay(BTreeMap::new()));
    }

    #[test]
    fn owner_write_read_roundtrip_sync_to() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync_to(
            &op_id,
            SyncStrategy::Ff,
            test_project(),
            PathBuf::from("/cwd/ws"),
            PathBuf::from("/tgt/ws"),
            true,
        );
        write_owner(dir, &record).unwrap();
        let read_back = read_owner(dir).unwrap().unwrap();
        assert_eq!(read_back.verb, OpVerb::SyncTo);
        assert_eq!(read_back.strategy, "ff");
        assert_eq!(read_back.phase, OpPhase::Replay);
        assert!(read_back.retire);
    }

    #[test]
    fn read_owner_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_owner(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn clear_owner_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Ff,
            test_project(),
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        assert!(read_owner(dir).unwrap().is_some());
        clear_owner(dir);
        assert!(read_owner(dir).unwrap().is_none());
    }

    #[test]
    fn set_phase_updates_owner_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        set_phase(dir, OpPhase::Relock).unwrap();
        let updated = read_owner(dir).unwrap().unwrap();
        assert_eq!(updated.phase, OpPhase::Relock);
    }

    // -----------------------------------------------------------------------
    // write_owner durability — crash recovery via the durable_file rename
    // -----------------------------------------------------------------------

    /// Atomicity, pinned by the mechanism rather than by hoping: an overwrite
    /// replaces `.rwv-op` by `rename(2)`, so the target's inode changes and no
    /// reader can ever catch a partial write. An in-place rewrite (the pre-fix
    /// `std::fs::write`) would keep the inode — and would be observable
    /// half-written.
    ///
    /// Not gated because its subject is Unix — replace-by-rename is the invariant
    /// the whole owned-file discipline rests on, and it holds on any platform.
    /// Gated because the INSTRUMENT is unverified: this proves the replacement by
    /// watching the inode change, and whether NTFS gives a renamed-over file a new
    /// file index is not something this repository can check. Ported on the
    /// assumption that it does, it would go red against correct code.
    #[test]
    #[cfg(unix)]
    fn write_owner_replaces_by_rename_not_in_place() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let mut record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        let path = OwnerRecord::path_in(dir);
        let first_inode = std::fs::metadata(&path).unwrap().ino();

        record.phase = OpPhase::Relock;
        write_owner(dir, &record).unwrap();
        assert_ne!(
            std::fs::metadata(&path).unwrap().ino(),
            first_inode,
            "the owner record must be replaced by rename, not rewritten in place"
        );

        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    /// A write that fails partway must not touch `.rwv-op` at all: the prior
    /// record has to survive exactly as it was. A read-only directory forces
    /// the failure at the earliest point `durable_file::replace` can hit one
    /// — its temp file needs `O_CREAT` on a fresh name, which the directory
    /// refuses — while leaving the existing `.rwv-op` itself writable. That's
    /// the same asymmetry a crash mid-write exploits: truncating an
    /// already-existing, already-permitted file needs no directory
    /// permission at all, so the pre-fix `std::fs::write` would sail through
    /// this and clobber the record instead of failing.
    ///
    /// Not gated because its subject is Unix — a failed write must not damage
    /// the prior record on any platform. Gated on the obstruction: a Windows
    /// read-only directory attribute does not stop a file being created inside
    /// it, so the write would succeed, and this would go red against correct
    /// code rather than pass vacuously. Denying that creation takes an ACL deny
    /// entry, which is a different fixture, not a different spelling.
    #[test]
    #[cfg(unix)]
    fn write_owner_failure_leaves_prior_record_recoverable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let original = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(dir, &original).unwrap();

        let writable_perms = std::fs::metadata(dir).unwrap().permissions();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let mut next = original.clone();
        next.phase = OpPhase::Relock;
        let result = write_owner(dir, &next);

        std::fs::set_permissions(dir, writable_perms).unwrap();

        assert!(
            result.is_err(),
            "a write into a read-only directory must fail, not silently succeed"
        );
        assert_eq!(
            read_owner(dir).unwrap().unwrap(),
            original,
            "a failed write must leave the prior record exactly as it was"
        );
    }

    /// `read_owner` must never mis-parse a torn record as something else, and
    /// must never panic on one: no prefix of a valid record's bytes shorter
    /// than the whole thing parses, since a truncated JSON object is always
    /// missing its closing brace. This pins the read side's half of crash
    /// safety; the write side — `write_owner_replaces_by_rename_not_in_place`
    /// and `write_owner_failure_leaves_prior_record_recoverable` — is what
    /// keeps a torn file from ever landing at `.rwv-op` through `write_owner`
    /// itself. This test only covers a torn file that got there some other
    /// way (a full disk mid-rename, a hand-edited file).
    #[test]
    fn read_owner_rejects_every_truncated_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/src/ws"),
            PathBuf::from("/cwd/ws"),
        );
        let json = serde_json::to_string_pretty(&record).unwrap();
        let path = OwnerRecord::path_in(dir);

        for len in 0..json.len() {
            std::fs::write(&path, &json.as_bytes()[..len]).unwrap();
            let result = read_owner(dir);
            assert!(
                result.is_err(),
                "a {len}-byte prefix of a valid record must not parse; got {result:?}"
            );
        }

        std::fs::write(&path, &json).unwrap();
        assert_eq!(read_owner(dir).unwrap().unwrap(), record);
    }

    // -----------------------------------------------------------------------
    // Lease round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn lease_write_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let lease = LeaseRecord {
            id: "1234567890".to_owned(),
            owner: PathBuf::from("/owner/ws"),
            created_at: None,
        };
        write_lease(dir, &lease).unwrap();
        let read_back = read_lease(dir).unwrap().unwrap();
        assert_eq!(read_back.id, "1234567890");
        assert_eq!(read_back.owner, PathBuf::from("/owner/ws"));
    }

    #[test]
    fn read_lease_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_lease(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn lease_missing_created_at_key_parses_as_none() {
        // Unlike WireOwnerRecord's fields, `created_at`'s missing-key
        // tolerance isn't something `#[serde(default)]` controls: serde's
        // derive treats a syntactic `Option<T>` field as optional-on-the-wire
        // unconditionally, so this parses whether or not the attribute is
        // present.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{"id": "1234567890", "owner": "/owner/ws"}"#;
        std::fs::write(dir.join(OP_LEASE_FILE), json).unwrap();
        let lease = read_lease(dir).unwrap().unwrap();
        assert_eq!(lease.created_at, None);
    }

    #[test]
    fn clear_lease_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let lease = LeaseRecord {
            id: "abc".to_owned(),
            owner: PathBuf::from("/owner"),
            created_at: None,
        };
        write_lease(dir, &lease).unwrap();
        assert!(read_lease(dir).unwrap().is_some());
        clear_lease(dir);
        assert!(read_lease(dir).unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // resolve_to_owner (pointer-follow)
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_owner_from_owner_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        std::fs::create_dir_all(&owner_dir).unwrap();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(&owner_dir, &record).unwrap();

        let resolved = resolve_to_owner(&owner_dir).unwrap().unwrap();
        assert_eq!(resolved.owner_workspace, owner_dir);
        assert_eq!(resolved.record.id, record.id);
    }

    #[test]
    fn resolve_owner_from_lease_workspace_follows_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();

        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync_to(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/cwd"),
            PathBuf::from("/tgt"),
            false,
        );
        write_owner(&owner_dir, &record).unwrap();

        let lease = LeaseRecord {
            id: op_id.as_str().to_owned(),
            owner: owner_dir.clone(),
            created_at: None,
        };
        write_lease(&lease_dir, &lease).unwrap();

        let resolved = resolve_to_owner(&lease_dir).unwrap().unwrap();
        assert_eq!(resolved.owner_workspace, owner_dir);
        assert_eq!(resolved.record.id, record.id);
        assert_eq!(resolved.record.verb, OpVerb::SyncTo);
    }

    #[test]
    fn resolve_to_owner_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(resolve_to_owner(tmp.path()).unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // check_no_op_in_progress
    // -----------------------------------------------------------------------

    #[test]
    fn check_no_op_passes_when_clean() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(check_no_op_in_progress(&[tmp.path()]).is_ok());
    }

    #[test]
    fn check_no_op_fails_when_owner_record_present() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Ff,
            test_project(),
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        let err = check_no_op_in_progress(&[dir]).unwrap_err().to_string();
        assert!(
            err.contains("in progress"),
            "expected 'in progress' in error: {err}"
        );
        assert!(
            err.contains("--continue"),
            "expected '--continue' hint in error: {err}"
        );
    }

    #[test]
    fn check_no_op_fails_when_lease_present() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let lease = LeaseRecord {
            id: "some-op-id".to_owned(),
            owner: PathBuf::from("/owner/ws"),
            created_at: None,
        };
        write_lease(dir, &lease).unwrap();
        let err = check_no_op_in_progress(&[dir]).unwrap_err().to_string();
        assert!(
            err.contains("in progress"),
            "expected 'in progress' in error: {err}"
        );
    }

    #[test]
    fn check_no_op_owner_refusal_names_verb_and_both_exits() {
        // The cross-verb mutex refusal names the
        // op's verb, its age, and BOTH exits: `rwv <verb> --continue` and
        // `rwv abort`.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync_to(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/cwd"),
            PathBuf::from("/tgt"),
            false,
        );
        write_owner(dir, &record).unwrap();
        let err = check_no_op_in_progress(&[dir]).unwrap_err().to_string();
        assert!(
            err.contains("sync-to in progress"),
            "refusal must name the op's verb: {err}"
        );
        assert!(
            err.contains("started") && err.contains("ago"),
            "refusal must name the op's age: {err}"
        );
        assert!(
            err.contains("rwv sync-to --continue"),
            "refusal must offer verb-derived `--continue`: {err}"
        );
        assert!(
            err.contains("rwv abort"),
            "refusal must offer `rwv abort`: {err}"
        );
    }

    #[test]
    fn check_no_op_lease_refusal_follows_pointer_for_rich_message() {
        // A workspace holding only a thin lease still gets the full op / age /
        // owner detail — the guard follows the lease pointer to the owner
        // record.
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();

        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync_to(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            owner_dir.clone(),
            PathBuf::from("/tgt"),
            false,
        );
        write_owner(&owner_dir, &record).unwrap();
        let lease = LeaseRecord {
            id: op_id.as_str().to_owned(),
            owner: owner_dir.clone(),
            created_at: None,
        };
        write_lease(&lease_dir, &lease).unwrap();

        let err = check_no_op_in_progress(&[lease_dir.as_path()])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("sync-to in progress"),
            "lease refusal must name the op's verb via the pointer: {err}"
        );
        assert!(
            err.contains(&owner_dir.display().to_string()),
            "lease refusal must name the owner workspace: {err}"
        );
        assert!(
            err.contains("rwv sync-to --continue") && err.contains("rwv abort"),
            "lease refusal must offer both exits: {err}"
        );
    }

    #[test]
    fn check_no_op_lease_refusal_falls_back_on_dangling_pointer() {
        // If the lease pointer is dangling (owner record gone), the guard still
        // refuses — falling back to the lease's own fields rather than passing.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let lease = LeaseRecord {
            id: "dangling-op".to_owned(),
            owner: tmp.path().join("nonexistent-owner"),
            created_at: None,
        };
        write_lease(dir, &lease).unwrap();
        let err = check_no_op_in_progress(&[dir]).unwrap_err().to_string();
        assert!(
            err.contains("dangling-op") && err.contains("in progress"),
            "dangling-pointer refusal must still name the op id: {err}"
        );
        assert!(
            err.contains("--continue") && err.contains("rwv abort"),
            "dangling-pointer refusal must still offer both exits: {err}"
        );
    }

    #[test]
    fn check_no_op_fails_across_multiple_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let empty_dir = tmp.path().join("empty");
        let dirty_dir = tmp.path().join("dirty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        std::fs::create_dir_all(&dirty_dir).unwrap();

        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Ff,
            test_project(),
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(&dirty_dir, &record).unwrap();

        let err = check_no_op_in_progress(&[empty_dir.as_path(), dirty_dir.as_path()])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("in progress"),
            "expected in-progress error: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // resume (--continue)
    // -----------------------------------------------------------------------

    #[test]
    fn resume_returns_record_for_owner_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Rebase,
            test_project(),
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        let (resumed, owner_ws) = resume(dir).unwrap();
        assert_eq!(resumed.id, record.id);
        assert_eq!(owner_ws, dir);
    }

    #[test]
    fn resume_follows_lease_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();

        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync_to(
            &op_id,
            SyncStrategy::Ff,
            test_project(),
            PathBuf::from("/cwd"),
            PathBuf::from("/tgt"),
            false,
        );
        write_owner(&owner_dir, &record).unwrap();
        let lease = LeaseRecord {
            id: op_id.as_str().to_owned(),
            owner: owner_dir.clone(),
            created_at: None,
        };
        write_lease(&lease_dir, &lease).unwrap();

        let (resumed, owner_ws) = resume(&lease_dir).unwrap();
        assert_eq!(resumed.id, record.id);
        assert_eq!(owner_ws, owner_dir);
    }

    #[test]
    fn resume_errors_when_no_op_present() {
        let tmp = tempfile::tempdir().unwrap();
        let err = resume(tmp.path()).unwrap_err().to_string();
        assert!(
            err.contains("no sync"),
            "expected 'no sync/sync-to op in progress' error; got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Cleanup paths
    // -----------------------------------------------------------------------

    #[test]
    fn clear_all_at_removes_both_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            SyncStrategy::Ff,
            test_project(),
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        let lease = LeaseRecord {
            id: op_id.as_str().to_owned(),
            owner: PathBuf::from("/owner"),
            created_at: None,
        };
        write_lease(dir, &lease).unwrap();
        assert!(OwnerRecord::path_in(dir).exists());
        assert!(LeaseRecord::path_in(dir).exists());
        clear_all_at(dir);
        assert!(!OwnerRecord::path_in(dir).exists());
        assert!(!LeaseRecord::path_in(dir).exists());
    }

    // -----------------------------------------------------------------------
    // Phase enum serialization
    // -----------------------------------------------------------------------

    #[test]
    fn phase_serializes_to_kebab_case() {
        let cases = [
            (OpPhase::Replay, "replay"),
            (OpPhase::Relock, "relock"),
            (OpPhase::AdvanceTarget, "advance-target"),
            (OpPhase::Retire, "retire"),
        ];
        for (phase, expected) in cases {
            let display = phase.to_string();
            assert_eq!(display, expected, "Display mismatch for {phase:?}");
        }
    }

    // -----------------------------------------------------------------------
    // Atomic acquisition (guard→mark TOCTOU fix)
    // -----------------------------------------------------------------------

    fn sync_touched(owner: &Path) -> TouchedWorkspaces {
        TouchedWorkspaces::of(OpVerb::Sync, owner, owner)
    }

    fn sync_to_touched(owner: &Path, target: &Path) -> TouchedWorkspaces {
        TouchedWorkspaces::of(OpVerb::SyncTo, owner, target)
    }

    fn make_sync_record(op_id: &OpId, owner: &Path, source: &Path) -> OwnerRecord {
        OwnerRecord::new_sync(
            op_id,
            SyncStrategy::Rebase,
            test_project(),
            source.to_path_buf(),
            owner.to_path_buf(),
        )
    }

    fn make_sync_to_record(op_id: &OpId, owner: &Path, target: &Path) -> OwnerRecord {
        OwnerRecord::new_sync_to(
            op_id,
            SyncStrategy::Rebase,
            test_project(),
            owner.to_path_buf(),
            target.to_path_buf(),
            false,
        )
    }

    #[test]
    fn acquire_op_writes_owner_only_for_plain_sync() {
        // Plain `sync` has no lease workspaces — only the owner record is
        // written. This is the minimal-acquisition case.
        let tmp = tempfile::tempdir().unwrap();
        let owner = tmp.path();
        let op_id = OpId::new_now();
        let record = make_sync_record(&op_id, owner, &PathBuf::from("/src"));

        let acquired = acquire_op(&sync_touched(owner), &record).unwrap();

        assert!(OwnerRecord::path_in(owner).exists());
        assert_eq!(acquired.owner_workspace(), owner);
        // Round-trip preserves the record.
        let read_back = read_owner(owner).unwrap().unwrap();
        assert_eq!(read_back.id, record.id);
    }

    #[test]
    fn acquire_op_writes_owner_plus_lease_for_sync_to() {
        // `sync-to` writes an owner at CWD and a lease at the target.
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let target_dir = tmp.path().join("target");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        let op_id = OpId::new_now();
        let record = make_sync_to_record(&op_id, &owner_dir, &target_dir);

        let acquired = acquire_op(&sync_to_touched(&owner_dir, &target_dir), &record).unwrap();

        // Owner record + lease both present.
        assert!(OwnerRecord::path_in(&owner_dir).exists());
        assert!(LeaseRecord::path_in(&target_dir).exists());
        // Lease points back at the owner.
        let lease = read_lease(&target_dir).unwrap().unwrap();
        assert_eq!(lease.id, op_id.as_str());
        assert_eq!(lease.owner, owner_dir);
        assert_eq!(acquired.owner_workspace(), owner_dir);
    }

    /// The handle is `#[must_use]`, and once the preconditions have run the
    /// sync engine discards it rather than releasing it: from that point the
    /// records belong to the phase driver and `--continue` / `abort` are the
    /// exits. Clearing on drop would delete the running op's own claim.
    #[test]
    fn dropping_an_acquired_handle_leaves_the_records_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let target_dir = tmp.path().join("target");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        let op_id = OpId::new_now();
        let record = make_sync_to_record(&op_id, &owner_dir, &target_dir);

        drop(acquire_op(&sync_to_touched(&owner_dir, &target_dir), &record).unwrap());

        assert!(
            OwnerRecord::path_in(&owner_dir).exists(),
            "the owner record must outlive the handle"
        );
        assert!(
            LeaseRecord::path_in(&target_dir).exists(),
            "the lease must outlive the handle"
        );
    }

    #[test]
    fn acquire_op_refuses_when_owner_workspace_already_has_op() {
        // If a prior op left an owner record, acquisition must refuse with the
        // standard in-flight-op refusal (verb / age / phase / both exits) —
        // NOT a raw "AlreadyExists" I/O error.
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path();
        // Plant a prior op.
        let prior_id = OpId::new_now();
        let prior = make_sync_to_record(&prior_id, owner_dir, &PathBuf::from("/prior/target"));
        write_owner(owner_dir, &prior).unwrap();

        // Second acquisition attempt.
        let new_id = OpId::new_now();
        let new_record = make_sync_record(&new_id, owner_dir, &PathBuf::from("/new/src"));
        let err = acquire_op(&sync_touched(owner_dir), &new_record)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("in progress"),
            "refusal must name in-flight state; got: {err}"
        );
        assert!(
            err.contains("sync-to in progress"),
            "refusal must name the prior op's verb: {err}"
        );
        assert!(
            err.contains("--continue") && err.contains("rwv abort"),
            "refusal must offer both exits: {err}"
        );
        // The prior op's record must survive (no clobbering).
        let after = read_owner(owner_dir).unwrap().unwrap();
        assert_eq!(after.id, prior_id.as_str());
    }

    #[test]
    fn acquire_op_rolls_back_owner_when_lease_workspace_taken() {
        // sync-to acquires owner then lease. If the lease workspace already
        // holds an op-state file, the just-written owner MUST be rolled back
        // so a refusal leaves no trace (cleanup table).
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let target_dir = tmp.path().join("target");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        // Plant a lease at the target (as if a prior sync-to already claimed it).
        let prior_lease = LeaseRecord {
            id: "prior-op".to_owned(),
            owner: PathBuf::from("/some/prior/owner"),
            created_at: None,
        };
        write_lease(&target_dir, &prior_lease).unwrap();

        let op_id = OpId::new_now();
        let record = make_sync_to_record(&op_id, &owner_dir, &target_dir);
        let err = acquire_op(&sync_to_touched(&owner_dir, &target_dir), &record)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("in progress"),
            "lease-side refusal must name in-flight state; got: {err}"
        );
        // The owner file we would have written must NOT persist — refusal
        // leaves no trace.
        assert!(
            !OwnerRecord::path_in(&owner_dir).exists(),
            "owner record must be rolled back on lease-side EEXIST; still present"
        );
        // The prior lease must survive untouched.
        let after = read_lease(&target_dir).unwrap().unwrap();
        assert_eq!(after.id, "prior-op");
    }

    #[test]
    fn release_acquired_clears_owner_and_leases() {
        // After a successful acquisition, `release_acquired` must clear every
        // file it created — this is the cleanup-table row for a precondition
        // refusal AFTER acquisition.
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let target_dir = tmp.path().join("target");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        let op_id = OpId::new_now();
        let record = make_sync_to_record(&op_id, &owner_dir, &target_dir);
        let acquired = acquire_op(&sync_to_touched(&owner_dir, &target_dir), &record).unwrap();
        assert!(OwnerRecord::path_in(&owner_dir).exists());
        assert!(LeaseRecord::path_in(&target_dir).exists());

        release_acquired(&acquired);

        assert!(
            !OwnerRecord::path_in(&owner_dir).exists(),
            "owner cleared on release"
        );
        assert!(
            !LeaseRecord::path_in(&target_dir).exists(),
            "lease cleared on release"
        );
    }

    #[test]
    fn acquire_op_is_atomic_under_concurrent_racers() {
        // Two threads race to acquire the same owner workspace: exactly one
        // succeeds, the other gets an in-flight refusal.
        //
        // This is the core TOCTOU regression test — a check-then-write guard
        // would let both racers pass; atomic O_CREAT|O_EXCL forces a serial
        // winner. Repeated across trials to expose ordering flakiness.
        for _ in 0..20 {
            let tmp = tempfile::tempdir().unwrap();
            let owner_dir = tmp.path().to_path_buf();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

            let owner_dir_a = owner_dir.clone();
            let barrier_a = barrier.clone();
            let h1 = std::thread::spawn(move || {
                let op = OpId::new_now();
                let rec = make_sync_record(&op, &owner_dir_a, &PathBuf::from("/src"));
                barrier_a.wait();
                acquire_op(&sync_touched(&owner_dir_a), &rec)
            });

            let owner_dir_b = owner_dir.clone();
            let barrier_b = barrier.clone();
            let h2 = std::thread::spawn(move || {
                let op = OpId::new_now();
                let rec = make_sync_record(&op, &owner_dir_b, &PathBuf::from("/src2"));
                barrier_b.wait();
                acquire_op(&sync_touched(&owner_dir_b), &rec)
            });

            let r1 = h1.join().unwrap();
            let r2 = h2.join().unwrap();

            let (ok, err) = match (r1, r2) {
                (Ok(a), Err(e)) => (a, e),
                (Err(e), Ok(a)) => (a, e),
                (Ok(_), Ok(_)) => panic!("both racers acquired — TOCTOU regression"),
                (Err(e1), Err(e2)) => panic!("both racers failed: {e1} / {e2}"),
            };

            assert_eq!(ok.owner_workspace(), owner_dir);
            let err_s = err.to_string();
            assert!(
                err_s.contains("in progress"),
                "loser must see in-flight refusal; got: {err_s}"
            );
            assert!(
                err_s.contains("--continue") && err_s.contains("rwv abort"),
                "loser must see both exits: {err_s}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Dead-lease detection
    // -----------------------------------------------------------------------

    #[test]
    fn detect_dead_lease_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(detect_dead_lease(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn detect_dead_lease_returns_none_when_owner_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();
        let op_id = OpId::new_now();
        let record = make_sync_to_record(&op_id, &owner_dir, &lease_dir);
        write_owner(&owner_dir, &record).unwrap();
        write_lease(
            &lease_dir,
            &LeaseRecord {
                id: op_id.as_str().to_owned(),
                owner: owner_dir.clone(),
                created_at: None,
            },
        )
        .unwrap();

        assert!(detect_dead_lease(&lease_dir).unwrap().is_none());
    }

    #[test]
    fn detect_dead_lease_flags_missing_owner_record() {
        // The concrete crash pattern: a lease whose owner workspace has no
        // `.rwv-op` — either the owner file was hand-removed or the workspace
        // was deleted out-of-band. Doctor auto-fixes this.
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();
        write_lease(
            &lease_dir,
            &LeaseRecord {
                id: "dangling-op".to_owned(),
                owner: owner_dir.clone(),
                created_at: None,
            },
        )
        .unwrap();

        let dead = detect_dead_lease(&lease_dir).unwrap().unwrap();
        assert_eq!(dead.workspace_dir, lease_dir);
        assert_eq!(dead.op_id, "dangling-op");
        assert_eq!(dead.recorded_owner, owner_dir);
        assert_eq!(dead.reason, DeadLeaseReason::OwnerRecordAbsent);
    }

    #[test]
    fn detect_dead_lease_flags_owner_op_id_mismatch() {
        // A stale lease survived past its op — the owner is now on a fresh op
        // with a different id. The lease is dead by structural comparison of
        // op ids, not by any time input.
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();
        let fresh_op_id = OpId::new_now();
        let owner_record = make_sync_to_record(&fresh_op_id, &owner_dir, &lease_dir);
        write_owner(&owner_dir, &owner_record).unwrap();
        // Lease references an OLDER op id — stale carry-over.
        write_lease(
            &lease_dir,
            &LeaseRecord {
                id: "old-op-id".to_owned(),
                owner: owner_dir.clone(),
                created_at: None,
            },
        )
        .unwrap();

        let dead = detect_dead_lease(&lease_dir).unwrap().unwrap();
        assert_eq!(dead.op_id, "old-op-id");
        assert!(matches!(
            dead.reason,
            DeadLeaseReason::OwnerOpIdMismatch { .. }
        ));
        if let DeadLeaseReason::OwnerOpIdMismatch { owner_op_id } = dead.reason {
            assert_eq!(owner_op_id, fresh_op_id.as_str());
        }
    }

    #[test]
    fn fix_dead_lease_removes_lease_file() {
        let tmp = tempfile::tempdir().unwrap();
        let lease_dir = tmp.path();
        write_lease(
            lease_dir,
            &LeaseRecord {
                id: "dangling-op".to_owned(),
                owner: PathBuf::from("/gone"),
                created_at: None,
            },
        )
        .unwrap();
        assert!(LeaseRecord::path_in(lease_dir).exists());

        fix_dead_lease(lease_dir);

        assert!(
            !LeaseRecord::path_in(lease_dir).exists(),
            "fix_dead_lease removes the lease file"
        );
    }
}
