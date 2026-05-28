//! Op-state file: multi-workspace operation tracking for `rwv sync` and `rwv sync-to`.
//!
//! # File format
//!
//! The op-state file (`.rwv-op`) is YAML. Example:
//!
//! ```yaml
//! id: 1779769917405921588   # wall-clock nanoseconds (same as savepoint-ref id)
//! verb: sync                # "sync" | "sync-to"
//! strategy: rebase          # "ff" | "rebase" | "merge"
//! source: /abs/path/to/src  # CWD at invocation (for sync: the named source; for sync-to: CWD)
//! target: /abs/path/to/tgt  # CWD at invocation (for sync: CWD; for sync-to: the named target)
//! retire: false             # true only for sync-to --retire
//! phase: running            # see OpPhase
//! started_at: 2026-05-27T15:50:25Z  # RFC3339 UTC
//! ```
//!
//! # File naming
//!
//! The file is named `.rwv-op` and lives at the workspace root (the active
//! path, i.e. the same directory that holds `.rwv-active`).
//!
//! The old `.rwv-sync-op` file (which only stored a raw op-id string) is
//! superseded. Existing `.rwv-sync-op` files are left untouched — they are
//! short-lived state and callers that wrote them before this upgrade will
//! read back `None` from [`read`] and should treat that as no op in progress.
//! The abort path still reads `.rwv-sync-op` as a fallback (see
//! [`read_legacy`]).
//!
//! # Multi-workspace writes
//!
//! For `sync` (single-step): the file is written only to CWD. Phase is always
//! `running`; on success it is removed.
//!
//! For `sync-to` (multi-step): the same op-state (same `id`) is written to
//! BOTH the CWD workspace and the target workspace. Phase advances:
//! `step1-rebase` → `step1-complete` → `step3-ff` → removed from both.
//!
//! # Concurrency safety
//!
//! Before any mutation, callers check all involved workspaces for an existing
//! op-state file. If any workspace already has one, the new op is refused.
//! This closes the hole where the old single-file marker only protected one
//! workspace at a time.

use crate::sync::{OpId, SyncStrategy};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Name of the op-state file written at each involved workspace root.
pub const OP_STATE_FILE: &str = ".rwv-op";

/// Name of the legacy single-workspace marker written by older `rwv sync` builds.
/// Still recognised by `run_abort` as a fallback for backward compatibility with
/// workspaces that have a `.rwv-sync-op` file from a pre-upgrade run.
pub const LEGACY_OP_MARKER: &str = ".rwv-sync-op";

// ---------------------------------------------------------------------------
// OpVerb — which top-level verb started this op
// ---------------------------------------------------------------------------

/// Which top-level verb started this op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpVerb {
    /// Single-step sync (existing `rwv sync`).
    Sync,
    /// Two-step sync-to (future `rwv sync-to`).
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
// OpPhase — current phase in the operation lifecycle
// ---------------------------------------------------------------------------

/// Current phase of the in-flight operation.
///
/// For `sync` (single-step): only `Running` is used; the file is removed on
/// success so `Running` is the only observable state.
///
/// For `sync-to` (multi-step): phases advance from `Step1Rebase` through
/// `Step1Complete` to `Step3Ff`; the file is removed from both workspaces
/// on success. Auto-relock (step 2) occurs between `Step1Complete` and
/// `Step3Ff` as bead 1's orchestration drives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpPhase {
    /// `sync`: the operation is running (single-step; always this phase).
    Running,
    /// `sync-to` step 1: rebasing CWD project commits onto source tip.
    Step1Rebase,
    /// `sync-to` step 1 complete: rebase finished; auto-relock next.
    Step1Complete,
    /// `sync-to` step 3: fast-forwarding target to the rebased tip.
    Step3Ff,
}

impl std::fmt::Display for OpPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => f.write_str("running"),
            Self::Step1Rebase => f.write_str("step1-rebase"),
            Self::Step1Complete => f.write_str("step1-complete"),
            Self::Step3Ff => f.write_str("step3-ff"),
        }
    }
}

// ---------------------------------------------------------------------------
// OpState — the serialisable record
// ---------------------------------------------------------------------------

/// The in-flight operation record written to `.rwv-op` at each involved
/// workspace root.
///
/// All path fields are absolute. The `started_at` timestamp is RFC3339 UTC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpState {
    /// Unique operation identifier (nanosecond wall-clock string). Shared
    /// across all workspace copies of this op-state so `rwv abort` can find
    /// the matching savepoint refs in every workspace.
    pub id: String,
    /// Which verb started this op.
    pub verb: OpVerb,
    /// Strategy supplied to the op.
    pub strategy: String,
    /// Absolute path of the source workspace (for `sync`: the named source;
    /// for `sync-to`: CWD at invocation).
    pub source: PathBuf,
    /// Absolute path of the target workspace (for `sync`: CWD at invocation;
    /// for `sync-to`: the named target).
    pub target: PathBuf,
    /// Whether `--retire` was passed (meaningful for `sync-to` only; always
    /// false for bare `sync`).
    pub retire: bool,
    /// Current phase of the op.
    pub phase: OpPhase,
    /// RFC3339 UTC timestamp when the op started.
    pub started_at: String,
}

impl OpState {
    /// Build a new [`OpState`] for a `rwv sync` invocation.
    ///
    /// `cwd_workspace` is the CWD workspace (target); `source_workspace` is
    /// the named source. Phase is initialised to `Running`.
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
            phase: OpPhase::Running,
            started_at: utc_now_rfc3339(),
        }
    }

    /// Build a new [`OpState`] for a future `rwv sync-to` invocation.
    ///
    /// `cwd_workspace` is CWD (source); `target_workspace` is the named
    /// target. Phase is initialised to `Step1Rebase`.
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
            phase: OpPhase::Step1Rebase,
            started_at: utc_now_rfc3339(),
        }
    }

    /// Path to the op-state file within `workspace_dir`.
    pub fn path_in(workspace_dir: &Path) -> PathBuf {
        workspace_dir.join(OP_STATE_FILE)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Write `state` to the `.rwv-op` file in `workspace_dir`.
///
/// Overwrites any existing file. Callers must call [`check_no_op_in_progress`]
/// on all involved workspaces before calling this.
pub fn write(workspace_dir: &Path, state: &OpState) -> anyhow::Result<()> {
    let path = OpState::path_in(workspace_dir);
    let yaml = serde_yaml::to_string(state)
        .map_err(|e| anyhow::anyhow!("failed to serialize op-state: {e}"))?;
    std::fs::write(&path, yaml)
        .map_err(|e| anyhow::anyhow!("failed to write op-state to {}: {e}", path.display()))
}

/// Read the `.rwv-op` file from `workspace_dir`, returning `None` if absent.
///
/// Returns an error if the file exists but cannot be parsed — a corrupted
/// op-state file is treated as an error (the operator must inspect manually
/// or use `rwv abort`).
pub fn read(workspace_dir: &Path) -> anyhow::Result<Option<OpState>> {
    let path = OpState::path_in(workspace_dir);
    if !path.exists() {
        return Ok(None);
    }
    let yaml = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read op-state from {}: {e}", path.display()))?;
    let state: OpState = serde_yaml::from_str(&yaml)
        .map_err(|e| anyhow::anyhow!("failed to parse op-state at {}: {e}", path.display()))?;
    Ok(Some(state))
}

/// Read the legacy `.rwv-sync-op` marker from `workspace_dir`, returning
/// `None` if absent. Used by `rwv abort` to support workspaces that still
/// have the old marker from a pre-upgrade run.
pub fn read_legacy(workspace_dir: &Path) -> Option<String> {
    let path = workspace_dir.join(LEGACY_OP_MARKER);
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// Advance the `phase` field in the op-state file at `workspace_dir`.
///
/// Reads the existing file, updates the phase, writes back. Returns an error
/// if the file is absent (caller logic error) or cannot be parsed.
pub fn advance_phase(workspace_dir: &Path, new_phase: OpPhase) -> anyhow::Result<()> {
    let mut state = read(workspace_dir)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no op-state file found at {} to advance phase",
            workspace_dir.display()
        )
    })?;
    state.phase = new_phase;
    write(workspace_dir, &state)
}

/// Remove the `.rwv-op` file from `workspace_dir`. No-op if absent.
pub fn clear(workspace_dir: &Path) {
    let _ = std::fs::remove_file(OpState::path_in(workspace_dir));
}

/// Remove the `.rwv-op` file from each listed workspace. No-op for absent files.
pub fn clear_all(workspace_dirs: &[&Path]) {
    for &dir in workspace_dirs {
        clear(dir);
    }
}

/// Check that no op-state file exists in any of the given workspace directories.
///
/// Returns an error if any workspace has an op-state file, naming the
/// workspace and the in-progress op's phase and start time so the operator
/// knows what they interrupted. The error message instructs the operator to
/// resolve and rerun with `--continue`, or use `rwv abort` to discard.
pub fn check_no_op_in_progress(workspace_dirs: &[&Path]) -> anyhow::Result<()> {
    for &dir in workspace_dirs {
        if let Some(state) = read(dir)? {
            let elapsed = elapsed_since(&state.started_at);
            anyhow::bail!(
                "{} in progress (started {elapsed} ago, mid `{phase}`) at {dir}.\n\
                 Resolve and rerun with `--continue`, or `rwv abort` to discard.",
                state.verb,
                phase = state.phase,
                dir = dir.display(),
            );
        }
        // Also check legacy marker for pre-upgrade workspaces.
        if let Some(id) = read_legacy(dir) {
            anyhow::bail!(
                "sync in progress (legacy marker, op-id={id}) at {dir}.\n\
                 Resolve and rerun with `--continue`, or `rwv abort` to discard.",
                dir = dir.display(),
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// --continue: read op-state for resume
// ---------------------------------------------------------------------------

/// Resume a `--continue` attempt by reading the in-progress op-state.
///
/// Returns the recorded [`OpState`] if an op-state file exists in
/// `workspace_dir`.
///
/// Returns an error if:
/// - No op-state file is present ("no sync/sync-to op in progress to continue").
/// - A legacy `.rwv-sync-op` marker is present (instructs operator to abort first).
pub fn resume(workspace_dir: &Path) -> anyhow::Result<OpState> {
    match read(workspace_dir)? {
        Some(s) => Ok(s),
        None => {
            // Check for legacy marker.
            if read_legacy(workspace_dir).is_some() {
                anyhow::bail!(
                    "a legacy sync marker (`.rwv-sync-op`) is present at {}. \
                     Run `rwv abort` to discard the old state, then rerun without `--continue`.",
                    workspace_dir.display()
                );
            }
            anyhow::bail!(
                "no sync/sync-to op in progress to continue at {}. \
                 If you meant to start a new op, omit `--continue`.",
                workspace_dir.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// --continue compatibility check (kept for unit tests; not called by sync.rs)
// ---------------------------------------------------------------------------

/// Parameters extracted from a `rwv sync` invocation for comparison with an
/// existing op-state file.
///
/// Retained for unit-test coverage; the sync engine no longer calls
/// [`check_continue`] — it uses [`resume`] instead.
#[derive(Debug, Clone)]
pub struct SyncParams {
    pub verb: OpVerb,
    pub strategy: String,
    /// Absolute path of the source workspace.
    pub source: PathBuf,
    /// Absolute path of the target workspace (CWD).
    pub target: PathBuf,
    pub retire: bool,
}

/// Validate a `--continue` attempt against a set of expected parameters.
///
/// Kept for unit-test coverage. The sync engine uses [`resume`] instead and
/// reads all parameters from the op-state file directly.
pub fn check_continue(workspace_dir: &Path, params: &SyncParams) -> anyhow::Result<OpState> {
    let state = match read(workspace_dir)? {
        Some(s) => s,
        None => {
            // Also check the legacy marker.
            if read_legacy(workspace_dir).is_some() {
                anyhow::bail!(
                    "a legacy sync marker (`.rwv-sync-op`) is present at {}. \
                     Run `rwv abort` to discard the old state, then rerun without `--continue`.",
                    workspace_dir.display()
                );
            }
            anyhow::bail!(
                "no sync/sync-to in progress to continue at {}",
                workspace_dir.display()
            );
        }
    };

    // Collect all parameter mismatches.
    let mut mismatches: Vec<String> = Vec::new();
    if state.verb != params.verb {
        mismatches.push(format!("verb: recorded={} got={}", state.verb, params.verb));
    }
    if state.strategy != params.strategy {
        mismatches.push(format!(
            "strategy: recorded={} got={}",
            state.strategy, params.strategy
        ));
    }
    if state.source != params.source {
        mismatches.push(format!(
            "source: recorded={} got={}",
            state.source.display(),
            params.source.display()
        ));
    }
    if state.target != params.target {
        mismatches.push(format!(
            "target: recorded={} got={}",
            state.target.display(),
            params.target.display()
        ));
    }
    if state.retire != params.retire {
        mismatches.push(format!(
            "retire: recorded={} got={}",
            state.retire, params.retire
        ));
    }

    if !mismatches.is_empty() {
        let recorded_summary = format!(
            "--strategy={} --retire={} source={} target={}",
            state.strategy,
            state.retire,
            state.source.display(),
            state.target.display()
        );
        let got_summary = format!(
            "--strategy={} --retire={} source={} target={}",
            params.strategy,
            params.retire,
            params.source.display(),
            params.target.display()
        );
        anyhow::bail!(
            "in-progress op parameters do not match:\n  recorded: {recorded_summary}\n  got: {got_summary}\n  differences: {}\n\
             Use `rwv abort` to discard, or re-run with the original parameters.",
            mismatches.join(", ")
        );
    }

    Ok(state)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// RFC3339 UTC timestamp for "right now".
fn utc_now_rfc3339() -> String {
    // Use `std::time::SystemTime` → format manually. We avoid pulling in a
    // full time crate; RFC3339 truncated to seconds is sufficient for
    // human-readable display. Format: `YYYY-MM-DDTHH:MM:SSZ`.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Decompose into calendar fields via a simple algorithm (valid for dates
    // within the range we care about: 2024–2100).
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
    let days = secs / 86400; // days since 1970-01-01

    // Shift epoch to 1 Mar 0000 (the Richards algorithm anchor).
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month prime [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, hour, min, sec)
}

/// Human-readable elapsed time since `started_at` (RFC3339).
///
/// Falls back to the raw timestamp string on any parse failure — the message
/// is still useful even if the elapsed computation fails.
fn elapsed_since(started_at: &str) -> String {
    // Parse the RFC3339 string we wrote ourselves.
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
    // We only emit `YYYY-MM-DDTHH:MM:SSZ` — parse that exact shape.
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

    // Days since 1970-01-01 via the Richards inverse algorithm.
    // Adjust for months ≤ 2 belonging to the previous "year" in the algorithm.
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
        // Must look like "2026-05-27T15:50:25Z".
        assert!(
            ts.len() == 20 && ts.ends_with('Z') && ts.contains('T'),
            "unexpected timestamp format: {ts}"
        );
        // Must parse back.
        assert!(
            parse_rfc3339_to_unix(&ts).is_some(),
            "timestamp {ts} failed to round-trip"
        );
    }

    #[test]
    fn elapsed_since_returns_secs_for_recent() {
        let ts = utc_now_rfc3339();
        let elapsed = elapsed_since(&ts);
        // "0s" or small number of seconds.
        assert!(
            elapsed.ends_with('s'),
            "expected seconds-suffix for fresh timestamp; got {elapsed}"
        );
    }

    #[test]
    fn write_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let state = OpState::new_sync(
            &op_id,
            crate::sync::SyncStrategy::Rebase,
            PathBuf::from("/src/ws"),
            PathBuf::from("/cwd/ws"),
        );
        write(dir, &state).unwrap();
        let read_back = read(dir).unwrap().unwrap();
        assert_eq!(read_back.id, state.id);
        assert_eq!(read_back.verb, OpVerb::Sync);
        assert_eq!(read_back.strategy, "rebase");
        assert_eq!(read_back.phase, OpPhase::Running);
        assert!(!read_back.retire);
    }

    #[test]
    fn read_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn clear_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let state = OpState::new_sync(
            &op_id,
            crate::sync::SyncStrategy::Ff,
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write(dir, &state).unwrap();
        assert!(read(dir).unwrap().is_some());
        clear(dir);
        assert!(read(dir).unwrap().is_none());
    }

    #[test]
    fn check_no_op_passes_when_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let result = check_no_op_in_progress(&[tmp.path()]);
        assert!(result.is_ok());
    }

    #[test]
    fn check_no_op_fails_when_file_present() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let state = OpState::new_sync(
            &op_id,
            crate::sync::SyncStrategy::Ff,
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
        );
        write(dir, &state).unwrap();
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
    fn advance_phase_updates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let state = OpState::new_sync_to(
            &op_id,
            crate::sync::SyncStrategy::Rebase,
            PathBuf::from("/src"),
            PathBuf::from("/tgt"),
            false,
        );
        write(dir, &state).unwrap();
        advance_phase(dir, OpPhase::Step1Complete).unwrap();
        let updated = read(dir).unwrap().unwrap();
        assert_eq!(updated.phase, OpPhase::Step1Complete);
    }

    #[test]
    fn check_continue_ok_when_params_match() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let src = PathBuf::from("/abs/src");
        let tgt = PathBuf::from("/abs/tgt");
        let state = OpState::new_sync(
            &op_id,
            crate::sync::SyncStrategy::Rebase,
            src.clone(),
            tgt.clone(),
        );
        write(dir, &state).unwrap();

        let params = SyncParams {
            verb: OpVerb::Sync,
            strategy: "rebase".to_owned(),
            source: src,
            target: tgt,
            retire: false,
        };
        let result = check_continue(dir, &params);
        assert!(result.is_ok(), "expected Ok; got {result:?}");
    }

    #[test]
    fn check_continue_errors_on_strategy_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let src = PathBuf::from("/abs/src");
        let tgt = PathBuf::from("/abs/tgt");
        let state = OpState::new_sync(
            &op_id,
            crate::sync::SyncStrategy::Rebase,
            src.clone(),
            tgt.clone(),
        );
        write(dir, &state).unwrap();

        let params = SyncParams {
            verb: OpVerb::Sync,
            strategy: "merge".to_owned(), // mismatch
            source: src,
            target: tgt,
            retire: false,
        };
        let err = check_continue(dir, &params).unwrap_err().to_string();
        assert!(
            err.contains("strategy"),
            "expected strategy mismatch detail; got {err}"
        );
        assert!(
            err.contains("rwv abort"),
            "expected abort suggestion; got {err}"
        );
    }

    #[test]
    fn check_continue_errors_when_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let params = SyncParams {
            verb: OpVerb::Sync,
            strategy: "ff".to_owned(),
            source: PathBuf::from("/src"),
            target: PathBuf::from("/tgt"),
            retire: false,
        };
        let err = check_continue(tmp.path(), &params).unwrap_err().to_string();
        assert!(
            err.contains("no sync"),
            "expected 'no sync' message; got {err}"
        );
    }
}
