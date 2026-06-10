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
//! strategy: rebase                 # "ff" | "rebase" | "merge"
//! source: /abs/path/src
//! target: /abs/path/tgt
//! retire: false
//! phase: replay                    # replay | relock | advance-target | retire
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
// OwnerRecord — the full op record at the initiating workspace
// ---------------------------------------------------------------------------

/// The owner op record written to `.rwv-op` at the initiating workspace.
///
/// All path fields are absolute. `started_at` is RFC3339 UTC.
/// `converged_tips` is populated at relock completion; empty before.
/// `overrides` records named overrides supplied at invocation for audit
/// fidelity on `--continue`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Per-repo converged tips written at relock completion.
    /// Key: repo path (relative to workspace root, e.g. `github/foo/bar`).
    /// Value: SHA string. Consumed by advance-target and abort's HEAD check.
    #[serde(default)]
    pub converged_tips: BTreeMap<String, String>,
    /// Named overrides supplied at invocation (e.g. `allow-stale-lock`).
    #[serde(default)]
    pub overrides: Vec<String>,
    /// RFC3339 UTC timestamp when the op started.
    pub started_at: String,
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
            converged_tips: BTreeMap::new(),
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
            converged_tips: BTreeMap::new(),
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
// Backward-compat shim for callers that use the old `read` / `clear` API
// ---------------------------------------------------------------------------

/// Read the owner record from `workspace_dir`. Alias for `read_owner`.
///
/// Used by `check.rs` (doctor hygiene scan) and other callers that only
/// need to inspect whether an owner record exists.
pub fn read(workspace_dir: &Path) -> anyhow::Result<Option<OwnerRecord>> {
    read_owner(workspace_dir)
}

/// Remove the owner record from `workspace_dir`. Alias for `clear_owner`.
///
/// Legacy call sites that only clear the owner workspace call this.
/// For clearing a lease workspace, call `clear_lease` or `clear_all_at`.
pub fn clear(workspace_dir: &Path) {
    clear_owner(workspace_dir);
}

/// Remove owner records from each listed workspace. No-op for absent files.
///
/// Legacy call sites that managed only owner workspaces call this. Does not
/// clear leases; call `clear_all_at` for workspaces that may hold leases.
pub fn clear_all(workspace_dirs: &[&Path]) {
    for &dir in workspace_dirs {
        clear_owner(dir);
    }
}

// ---------------------------------------------------------------------------
// Concurrency guard
// ---------------------------------------------------------------------------

/// Check that no op-state file (owner record or lease) exists in any of the
/// given workspace directories.
///
/// Returns an error if any workspace carries an owner record or a lease,
/// naming the workspace and the in-progress op's details so the operator
/// knows what they interrupted.
pub fn check_no_op_in_progress(workspace_dirs: &[&Path]) -> anyhow::Result<()> {
    for &dir in workspace_dirs {
        // Check for owner record.
        if let Some(record) = read_owner(dir)? {
            let elapsed = elapsed_since(&record.started_at);
            anyhow::bail!(
                "{} in progress (started {elapsed} ago, mid `{phase}`) at {dir}.\n\
                 Resolve and rerun with `--continue`, or `rwv abort` to discard.",
                record.verb,
                phase = record.phase,
                dir = dir.display(),
            );
        }
        // Check for lease.
        if let Some(lease) = read_lease(dir)? {
            anyhow::bail!(
                "op {} in progress (lease at {dir}; owner workspace: {owner}).\n\
                 Resolve and rerun with `--continue`, or `rwv abort` to discard.",
                lease.id,
                dir = dir.display(),
                owner = lease.owner.display(),
            );
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
        assert!(read_back.converged_tips.is_empty());
        assert!(read_back.overrides.is_empty());
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
