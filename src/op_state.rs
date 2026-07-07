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
//! ```yaml
//! id: "1779769917405921588"       # op id, shared with savepoint refs
//! verb: sync                       # "sync" | "sync-to"
//! strategy: rebase                 # "ff" | "rebase"
//! source: /abs/path/src
//! target: /abs/path/tgt
//! retire: false
//! phase: replay                    # replay | relock | advance-target | retire
//! advanced_tips: {}                # replay-phase intent: repo → planned/actual tip; empty before replay entry; cleared at relock (same write as converged_tips)
//! converged_tips: {}               # written at relock completion; empty before
//! overrides: []                    # named overrides supplied at invocation
//! started_at: 2026-06-10T21:14:03Z
//! ```
//!
//! ## Thin lease (`.rwv-op-lease`)
//!
//! Written at every **other workspace the op mutates** (never at the owner).
//! Immutable once written; a mutex plus a redirect, nothing else.
//!
//! ```yaml
//! id: "1779769917405921588"
//! owner: /abs/path/to/owner/workspace
//! ```
//!
//! ## Read-only workspaces
//!
//! Not marked. Safe because source reads are snapshots (§6 of the design).
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

use crate::sync::{OpId, SyncStrategy};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Name of the owner op-state file at the initiating workspace.
pub const OP_STATE_FILE: &str = ".rwv-op";

/// Name of the thin-lease file at every other mutated workspace.
pub const OP_LEASE_FILE: &str = ".rwv-op-lease";

// ---------------------------------------------------------------------------
// OpVerb — which top-level verb started this op
// ---------------------------------------------------------------------------

/// Which top-level verb started this op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
/// Phases are listed in execution order; the driver loop persists the phase
/// before entering it so a crash re-enters the same phase on resume.
///
/// ```text
/// guard → mark → savepoint → replay → relock → advance-target → retire → cleanup
///                                               (sync-to only)   (--retire only)
/// ```
///
/// The persisted phase is always the phase in progress. Every phase is
/// idempotent and re-runnable from the record alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
// PhaseTips — phase-scoped tip table (fo-wbbqof.5)
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
/// Wire format: this ADT is *in-memory only*. The persisted `.rwv-op` YAML
/// keeps the historical flat shape (two independent top-level keys
/// `advanced_tips:` / `converged_tips:`) via the [`OwnerRecord`] serde shim,
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
// OwnerRecord — the full op record at the initiating workspace
// ---------------------------------------------------------------------------

/// The owner op record written to `.rwv-op` at the initiating workspace.
///
/// All path fields are absolute. `started_at` is RFC3339 UTC.
///
/// The replay-intent (`advanced_tips`) and converged (`converged_tips`) tip
/// tables are carried by the [`PhaseTips`] ADT in `tips`, which makes the
/// illegal *both-populated* state unrepresentable (see [`PhaseTips`]). The
/// persisted `.rwv-op` YAML keeps the historical flat shape — two independent
/// top-level keys `advanced_tips:` / `converged_tips:` — via a serde shim
/// ([`WireOwnerRecord`]), so on-disk op-state round-trips unchanged.
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
    /// Absolute path of the source workspace.
    pub source: PathBuf,
    /// Absolute path of the target workspace (for `sync`: same as the owner
    /// workspace; for `sync-to`: the named target workspace).
    pub target: PathBuf,
    /// Whether `--retire` was passed.
    pub retire: bool,
    /// Current phase. The driver persists this before entering each phase.
    pub phase: OpPhase,
    /// The op's per-repo tip table, scoped to exactly one lifecycle half at a
    /// time (see [`PhaseTips`]). Replay-phase intent during replay; converged
    /// tips after the atomic [`PhaseTips::converge`] swap at relock completion.
    pub tips: PhaseTips,
    /// Named overrides supplied at invocation (e.g. `allow-stale-lock`).
    pub overrides: Vec<String>,
    /// RFC3339 UTC timestamp when the op started.
    pub started_at: String,
}

// ---------------------------------------------------------------------------
// WireOwnerRecord — flat persisted shape for OwnerRecord (serde shim)
// ---------------------------------------------------------------------------

/// On-disk representation of [`OwnerRecord`]: the historical flat YAML with
/// two independent tip maps. Exists solely to keep the persisted `.rwv-op`
/// shape stable while the in-memory model uses the [`PhaseTips`] ADT.
///
/// The flat↔ADT mapping is driven by which table is populated, preserving the
/// temporal invariant: a converged record (non-empty `converged_tips`) maps to
/// [`PhaseTips::Converged`]; otherwise the record is still in the replay half
/// and maps to [`PhaseTips::Replay`], carrying `advanced_tips`. A both-empty
/// record canonicalises to the empty replay half and serialises identically.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WireOwnerRecord {
    id: String,
    verb: OpVerb,
    strategy: String,
    source: PathBuf,
    target: PathBuf,
    retire: bool,
    phase: OpPhase,
    /// Empty on records predating this field (`#[serde(default)]`).
    #[serde(default)]
    advanced_tips: BTreeMap<String, String>,
    #[serde(default)]
    converged_tips: BTreeMap<String, String>,
    #[serde(default)]
    overrides: Vec<String>,
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
        source_workspace: PathBuf,
        cwd_workspace: PathBuf,
    ) -> Self {
        Self {
            id: op_id.as_str().to_owned(),
            verb: OpVerb::Sync,
            strategy: strategy.to_string(),
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
        cwd_workspace: PathBuf,
        target_workspace: PathBuf,
        retire: bool,
    ) -> Self {
        Self {
            id: op_id.as_str().to_owned(),
            verb: OpVerb::SyncTo,
            strategy: strategy.to_string(),
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
/// This is the **only write path for op state** — all phase persistence
/// calls go through here to guarantee the "one write, one file" invariant.
///
/// Overwrites any existing file. Callers must call
/// [`check_no_op_in_progress`] on all touched workspaces before the first
/// write.
pub fn write_owner(workspace_dir: &Path, record: &OwnerRecord) -> anyhow::Result<()> {
    let path = OwnerRecord::path_in(workspace_dir);
    let yaml = serde_yaml::to_string(record).context("failed to serialize owner record")?;
    std::fs::write(&path, yaml)
        .with_context(|| format!("failed to write owner record to {}", path.display()))
}

/// Read the `.rwv-op` file from `workspace_dir`, returning `None` if absent.
///
/// Returns an error if the file exists but cannot be parsed.
pub fn read_owner(workspace_dir: &Path) -> anyhow::Result<Option<OwnerRecord>> {
    let path = OwnerRecord::path_in(workspace_dir);
    if !path.exists() {
        return Ok(None);
    }
    let yaml = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read owner record from {}", path.display()))?;
    let record: OwnerRecord = serde_yaml::from_str(&yaml)
        .with_context(|| format!("failed to parse owner record at {}", path.display()))?;
    Ok(Some(record))
}

/// Advance the `phase` field in the owner record at `workspace_dir`.
///
/// Reads the existing file, updates the phase, writes back. This is the
/// one persistence write in the driver loop.
pub fn advance_phase(workspace_dir: &Path, new_phase: OpPhase) -> anyhow::Result<()> {
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
    let yaml = serde_yaml::to_string(lease).context("failed to serialize lease record")?;
    std::fs::write(&path, yaml)
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
    let yaml = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read lease from {}", path.display()))?;
    let lease: LeaseRecord = serde_yaml::from_str(&yaml)
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
pub fn check_no_op_in_progress(workspace_dirs: &[&Path]) -> anyhow::Result<()> {
    for &dir in workspace_dirs {
        // Owner record: full detail is right here.
        if let Some(record) = read_owner(dir)? {
            let elapsed = elapsed_since(&record.started_at);
            anyhow::bail!(
                "{verb} in progress (started {elapsed} ago, mid `{phase}`) at {dir}.\n\
                 Rerun with `rwv {verb} --continue` from that workspace after resolving, \
                 or `rwv abort` to discard.",
                verb = record.verb,
                phase = record.phase,
                dir = dir.display(),
            );
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
                    anyhow::bail!(
                        "{verb} in progress (started {elapsed} ago, mid `{phase}`); this \
                         workspace ({dir}) is leased to it. Owner workspace: {owner}.\n\
                         Rerun with `rwv {verb} --continue` from the owner workspace after \
                         resolving, or `rwv abort` to discard.",
                        verb = record.verb,
                        phase = record.phase,
                        dir = dir.display(),
                        owner = lease.owner.display(),
                    );
                }
                _ => anyhow::bail!(
                    "op {id} in progress (lease at {dir}; owner workspace: {owner}).\n\
                     Rerun the owning verb with `--continue` from the owner workspace, or \
                     `rwv abort` to discard.",
                    id = lease.id,
                    dir = dir.display(),
                    owner = lease.owner.display(),
                ),
            }
        }
    }
    Ok(())
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

/// RFC3339 UTC timestamp for "right now".
fn utc_now_rfc3339() -> String {
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
fn elapsed_since(started_at: &str) -> String {
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
    use crate::sync::OpId;

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
            crate::sync::SyncStrategy::Rebase,
            PathBuf::from("/src/ws"),
            PathBuf::from("/cwd/ws"),
        );
        write_owner(dir, &record).unwrap();
        let read_back = read_owner(dir).unwrap().unwrap();
        assert_eq!(read_back.id, record.id);
        assert_eq!(read_back.verb, OpVerb::Sync);
        assert_eq!(read_back.strategy, "rebase");
        assert_eq!(read_back.phase, OpPhase::Replay);
        assert!(!read_back.retire);
        // Fresh record is in the replay half with an empty intent table.
        assert_eq!(read_back.tips, PhaseTips::Replay(BTreeMap::new()));
        assert!(read_back.tips.advanced().unwrap().is_empty());
        assert!(read_back.tips.converged().is_none());
        assert!(read_back.overrides.is_empty());
    }

    // -----------------------------------------------------------------------
    // advanced_tips field round-trip tests (fo-6rysot.1)
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
            crate::sync::SyncStrategy::Rebase,
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
    fn advanced_tips_default_empty_when_field_absent() {
        // A serialised record that pre-dates the advanced_tips field (i.e. the
        // YAML has no advanced_tips key) must parse to an empty map.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Craft a minimal YAML record without the advanced_tips key, exactly as
        // an in-flight record from before this change would look.
        let yaml = r#"id: "9999999999999999999"
verb: sync
strategy: ff
source: /src/ws
target: /cwd/ws
retire: false
phase: replay
converged_tips: {}
overrides: []
started_at: 2026-06-01T00:00:00Z
"#;
        let path = dir.join(OP_STATE_FILE);
        std::fs::write(&path, yaml).unwrap();
        let record = read_owner(dir).unwrap().unwrap();
        // A legacy record (no advanced_tips key, empty converged_tips) maps to
        // the empty replay half.
        assert_eq!(record.tips, PhaseTips::Replay(BTreeMap::new()));
        // Verify the rest of the record parsed correctly too.
        assert_eq!(record.id, "9999999999999999999");
        assert_eq!(record.verb, OpVerb::Sync);
    }

    // -----------------------------------------------------------------------
    // PhaseTips ADT — phase-scoped tip table (fo-wbbqof.5)
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
            crate::sync::SyncStrategy::Rebase,
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
        // The persisted YAML keeps the flat shape with advanced_tips emptied.
        let raw = std::fs::read_to_string(dir.join(OP_STATE_FILE)).unwrap();
        assert!(
            raw.contains("advanced_tips: {}"),
            "expected emptied flat advanced_tips key, got:\n{raw}"
        );
        assert!(
            raw.contains("converged_tips:"),
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
    fn legacy_both_empty_record_canonicalises_to_replay() {
        // A record with both maps empty (the common at-entry state) round-trips
        // to the empty replay half and serialises identically — the both-empty
        // case has a single canonical ADT representation.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let yaml = r#"id: "1"
verb: sync
strategy: rebase
source: /src/ws
target: /cwd/ws
retire: false
phase: relock
advanced_tips: {}
converged_tips: {}
overrides: []
started_at: 2026-06-01T00:00:00Z
"#;
        std::fs::write(dir.join(OP_STATE_FILE), yaml).unwrap();
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
            crate::sync::SyncStrategy::Ff,
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
            crate::sync::SyncStrategy::Ff,
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        assert!(read_owner(dir).unwrap().is_some());
        clear_owner(dir);
        assert!(read_owner(dir).unwrap().is_none());
    }

    #[test]
    fn advance_phase_updates_owner_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            crate::sync::SyncStrategy::Rebase,
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        advance_phase(dir, OpPhase::Relock).unwrap();
        let updated = read_owner(dir).unwrap().unwrap();
        assert_eq!(updated.phase, OpPhase::Relock);
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
    fn clear_lease_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let lease = LeaseRecord {
            id: "abc".to_owned(),
            owner: PathBuf::from("/owner"),
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
            crate::sync::SyncStrategy::Rebase,
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
            crate::sync::SyncStrategy::Rebase,
            PathBuf::from("/cwd"),
            PathBuf::from("/tgt"),
            false,
        );
        write_owner(&owner_dir, &record).unwrap();

        let lease = LeaseRecord {
            id: op_id.as_str().to_owned(),
            owner: owner_dir.clone(),
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
            crate::sync::SyncStrategy::Ff,
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
        // The cross-verb mutex refusal (fo-4rpnkm.2, Correction 1) names the
        // op's verb, its age, and BOTH exits: `rwv <verb> --continue` and
        // `rwv abort`.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync_to(
            &op_id,
            crate::sync::SyncStrategy::Rebase,
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
        // record (fo-4rpnkm.2 wired the existing lease into the mutex).
        let tmp = tempfile::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();

        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync_to(
            &op_id,
            crate::sync::SyncStrategy::Rebase,
            owner_dir.clone(),
            PathBuf::from("/tgt"),
            false,
        );
        write_owner(&owner_dir, &record).unwrap();
        let lease = LeaseRecord {
            id: op_id.as_str().to_owned(),
            owner: owner_dir.clone(),
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
            crate::sync::SyncStrategy::Ff,
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
            crate::sync::SyncStrategy::Rebase,
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
            crate::sync::SyncStrategy::Ff,
            PathBuf::from("/cwd"),
            PathBuf::from("/tgt"),
            false,
        );
        write_owner(&owner_dir, &record).unwrap();
        let lease = LeaseRecord {
            id: op_id.as_str().to_owned(),
            owner: owner_dir.clone(),
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
            crate::sync::SyncStrategy::Ff,
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        let lease = LeaseRecord {
            id: op_id.as_str().to_owned(),
            owner: PathBuf::from("/owner"),
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
}
