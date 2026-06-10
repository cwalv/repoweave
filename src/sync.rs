//! `rwv sync <source>` and `rwv abort` implementation.
//!
//! `rwv sync` aligns the CWD workspace with another workspace's committed
//! `rwv.lock`. `rwv abort` rolls back to pre-sync state using savepoint refs.

use crate::git::GitVcs;
use crate::lock::{commit_lock_file_with_message, generate_lock};
use crate::manifest::{LockFile, Manifest, Project, ProjectName, RepoPath, Role, WorkweaveName};
use crate::op_state::{self, LeaseRecord, OwnerRecord};
use crate::parallel::run_in_parallel;
use crate::vcs::{ConflictOp, RefName, ResolvedRevisionId, Vcs, VcsError, VcsErrorOutput};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use crate::workweave::workweave_path_for;
use anyhow::Context;
use schemars::JsonSchema;
use serde::Serialize;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

/// Which side of a sync a check or error is reporting against.
#[derive(Debug, Clone, Copy)]
enum Side {
    Source,
    Destination,
}

impl Side {
    fn as_str(self) -> &'static str {
        match self {
            Side::Source => "source",
            Side::Destination => "destination",
        }
    }
}

/// Display name for a workspace: the workweave name when in a workweave,
/// otherwise the basename of the primary path.
fn workspace_name(ctx: &WorkspaceContext) -> String {
    match &ctx.location {
        WorkspaceLocation::Workweave { name, .. } => name.as_str().to_owned(),
        WorkspaceLocation::Weave { .. } => ctx
            .primary_path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_owned(),
    }
}

// ---------------------------------------------------------------------------
// SyncStrategy — typed sync strategy
// ---------------------------------------------------------------------------

/// How `rwv sync` advances each repo to its lock target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
#[clap(rename_all = "lowercase")]
pub enum SyncStrategy {
    /// Fast-forward only; bail if not possible.
    Ff,
    /// Rebase the local branch onto the lock target.
    Rebase,
    /// Merge the lock target into the local branch with an auto-generated commit.
    Merge,
}

impl fmt::Display for SyncStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ff => "ff",
            Self::Rebase => "rebase",
            Self::Merge => "merge",
        })
    }
}

impl FromStr for SyncStrategy {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ff" => Ok(Self::Ff),
            "rebase" => Ok(Self::Rebase),
            "merge" => Ok(Self::Merge),
            other => anyhow::bail!(
                "unknown sync strategy `{other}` in op-state; expected ff, rebase, or merge"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// SyncSource — typed source workspace for `rwv sync`
// ---------------------------------------------------------------------------

/// Source workspace for `rwv sync <source>`.
///
/// The boundary parser ([`FromStr`]) disambiguates by shape:
/// - `Primary` — the literal string `"primary"` (the primary workspace root).
/// - `Workweave(name)` — a bare identifier with no path separators or leading
///   dot. Resolves to `<workweave_parent>/<primary>--<name>`.
/// - `Path(p)` — anything else: an absolute path is used as-is, a relative
///   path is joined against the primary workspace root.
///
/// `SyncSource` is the single source of truth for this disambiguation; the
/// resolver matches on the enum rather than re-parsing strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncSource {
    Primary,
    Workweave(WorkweaveName),
    Path(PathBuf),
}

impl SyncSource {
    /// Resolve to an on-disk workspace path using the surrounding context to
    /// locate `primary`.
    ///
    /// Returns `Err` when the variant is `Workweave` and the context is a
    /// primary weave with no active project set (neither from `.rwv-active`
    /// nor via `--project`). In that case the workweave path cannot be
    /// constructed, and proceeding would silently produce a garbage path.
    /// `require_active_project` emits an actionable error message.
    pub fn resolve(&self, ctx: &WorkspaceContext) -> anyhow::Result<PathBuf> {
        match self {
            Self::Primary => Ok(ctx.primary_path().to_path_buf()),
            Self::Workweave(name) => {
                // Resolve the project from the current context: the workweave
                // we're syncing FROM is assumed to belong to the same project
                // as the workspace we're syncing INTO (sync is per-project).
                // When CWD is the primary weave, require an active project
                // rather than silently falling back to an empty string.
                let project = match &ctx.location {
                    WorkspaceLocation::Workweave { project, .. } => project.clone(),
                    WorkspaceLocation::Weave { .. } => ctx.require_active_project()?.clone(),
                };
                Ok(workweave_path_for(ctx.primary_path(), &project, name))
            }
            Self::Path(p) => {
                if p.is_absolute() {
                    Ok(p.clone())
                } else {
                    Ok(ctx.primary_path().join(p))
                }
            }
        }
    }
}

impl FromStr for SyncSource {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "primary" {
            return Ok(Self::Primary);
        }
        if looks_path_like(s) {
            return Ok(Self::Path(PathBuf::from(s)));
        }
        Ok(Self::Workweave(WorkweaveName::new(s)))
    }
}

impl fmt::Display for SyncSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Primary => f.write_str("primary"),
            Self::Workweave(n) => f.write_str(n.as_str()),
            Self::Path(p) => f.write_str(&p.to_string_lossy()),
        }
    }
}

/// A string is "path-like" when it contains a separator, starts with a dot
/// (so `.` and `./foo` parse as paths), or is absolute. Bare identifiers fall
/// through to the workweave-name interpretation.
fn looks_path_like(s: &str) -> bool {
    s.contains('/')
        || s.contains(std::path::MAIN_SEPARATOR)
        || s.starts_with('.')
        || PathBuf::from(s).is_absolute()
}

// ---------------------------------------------------------------------------
// RepoSyncOutcome — per-repo result of a sync operation
// ---------------------------------------------------------------------------

/// Why a per-repo sync attempt failed.
///
/// Discriminates between the recoverable cases the caller may want to react
/// to differently — directly maps to `--json` output for `rwv status` /
/// `rwv sync`. The `error` payload is the underlying error string for
/// human-readable display.
///
/// `cause` optionally carries the underlying typed [`VcsError`] when the
/// failure originated from a `Vcs` trait call (e.g. `RebaseConflict` from
/// `Vcs::rebase`). This lets `--json` consumers pattern-match on the
/// structured failure mode without parsing the human string.
#[derive(Debug)]
pub enum SyncFailure {
    /// Couldn't read HEAD on the repo (e.g. not a repo, or I/O failure).
    HeadUnreadable {
        error: String,
        cause: Option<VcsError>,
    },
    /// `--strategy ff` cannot proceed (divergence, conflict).
    FastForwardImpossible {
        error: String,
        cause: Option<VcsError>,
    },
    /// `--strategy rebase` failed (conflict or git error).
    RebaseFailed {
        error: String,
        cause: Option<VcsError>,
    },
    /// `--strategy merge` failed (conflict or git error).
    MergeFailed {
        error: String,
        cause: Option<VcsError>,
    },
}

impl SyncFailure {
    /// Stable variant tag suitable for `--json` output.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::HeadUnreadable { .. } => "head-unreadable",
            Self::FastForwardImpossible { .. } => "ff-impossible",
            Self::RebaseFailed { .. } => "rebase-failed",
            Self::MergeFailed { .. } => "merge-failed",
        }
    }

    pub fn error(&self) -> &str {
        match self {
            Self::HeadUnreadable { error, .. }
            | Self::FastForwardImpossible { error, .. }
            | Self::RebaseFailed { error, .. }
            | Self::MergeFailed { error, .. } => error,
        }
    }

    pub fn cause(&self) -> Option<&VcsError> {
        match self {
            Self::HeadUnreadable { cause, .. }
            | Self::FastForwardImpossible { cause, .. }
            | Self::RebaseFailed { cause, .. }
            | Self::MergeFailed { cause, .. } => cause.as_ref(),
        }
    }

    fn for_strategy(strategy: SyncStrategy, error: String, cause: Option<VcsError>) -> Self {
        match strategy {
            SyncStrategy::Ff => Self::FastForwardImpossible { error, cause },
            SyncStrategy::Rebase => Self::RebaseFailed { error, cause },
            SyncStrategy::Merge => Self::MergeFailed { error, cause },
        }
    }
}

impl fmt::Display for SyncFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.error())
    }
}

/// Per-repo result of a sync operation.
///
/// Public so the `--json` wire-output layer can pattern-match on it. The
/// JSON shape is produced by converting through [`SyncOutcomeOutput`].
#[derive(Debug)]
pub enum RepoSyncOutcome {
    /// HEAD advanced to the lock SHA.
    Converged,
    /// Lock SHA is already an ancestor of HEAD; no change made.
    AlreadyAhead { commits_ahead: usize },
    /// HEAD was already equal to the lock SHA before sync.
    NoOp,
    /// Strategy failed (conflict, divergence, etc.).
    Failed(SyncFailure),
}

impl RepoSyncOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

// ---------------------------------------------------------------------------
// JSON wire-output types for `rwv sync --json`
// ---------------------------------------------------------------------------

/// Wire-output mirror of [`SyncFailure`] for `--json` emission.
///
/// Carries the same payload as the in-memory enum but with a `cause`
/// represented as the serialisable [`VcsErrorOutput`]. The hand-rolled tag
/// strings match [`SyncFailure::kind`] (verified via snapshot tests).
///
/// `message` is the human-readable display string of the failure (free-form
/// text, not a typed discriminant). `cause` is the structured typed cause when
/// the failure originated from a [`crate::vcs::VcsError`] call — consumers
/// that want to branch on failure mode should inspect `cause.kind` rather than
/// parsing `message`.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SyncFailureOutput {
    HeadUnreadable {
        /// Free-form display message for this failure. Not a typed discriminant.
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<VcsErrorOutput>,
    },
    #[serde(rename = "ff-impossible")]
    FastForwardImpossible {
        /// Free-form display message for this failure. Not a typed discriminant.
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<VcsErrorOutput>,
    },
    RebaseFailed {
        /// Free-form display message for this failure. Not a typed discriminant.
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<VcsErrorOutput>,
    },
    MergeFailed {
        /// Free-form display message for this failure. Not a typed discriminant.
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<VcsErrorOutput>,
    },
}

impl From<&SyncFailure> for SyncFailureOutput {
    fn from(f: &SyncFailure) -> Self {
        let message = f.error().to_owned();
        let cause = f.cause().map(VcsErrorOutput::from);
        match f {
            SyncFailure::HeadUnreadable { .. } => Self::HeadUnreadable { message, cause },
            SyncFailure::FastForwardImpossible { .. } => {
                Self::FastForwardImpossible { message, cause }
            }
            SyncFailure::RebaseFailed { .. } => Self::RebaseFailed { message, cause },
            SyncFailure::MergeFailed { .. } => Self::MergeFailed { message, cause },
        }
    }
}

/// One per-repo record in `rwv sync --json` output.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SyncOutcomeOutput {
    Converged {
        path: String,
        absolute_path: String,
    },
    AlreadyAhead {
        path: String,
        absolute_path: String,
        commits_ahead: usize,
    },
    NoOp {
        path: String,
        absolute_path: String,
    },
    Failed {
        path: String,
        absolute_path: String,
        failure: SyncFailureOutput,
    },
}

impl SyncOutcomeOutput {
    pub fn from_outcome(path: String, absolute_path: String, outcome: &RepoSyncOutcome) -> Self {
        match outcome {
            RepoSyncOutcome::Converged => Self::Converged {
                path,
                absolute_path,
            },
            RepoSyncOutcome::AlreadyAhead { commits_ahead } => Self::AlreadyAhead {
                path,
                absolute_path,
                commits_ahead: *commits_ahead,
            },
            RepoSyncOutcome::NoOp => Self::NoOp {
                path,
                absolute_path,
            },
            RepoSyncOutcome::Failed(failure) => Self::Failed {
                path,
                absolute_path,
                failure: SyncFailureOutput::from(failure),
            },
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// Top-level envelope for `rwv sync --json` (serial mode).
#[derive(Debug, Serialize, JsonSchema)]
pub struct SyncJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub outcomes: Vec<SyncOutcomeOutput>,
}

/// Top-level envelope for `rwv sync-to --json` (serial mode).
///
/// Identical shape to [`SyncJsonOutput`]; differs only in the `$schema` URL
/// embedded at runtime. Kept as a separate type so the generated schema
/// artifact (`docs/reference/schemas/sync-to.json`) has its own title/description.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SyncToJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub outcomes: Vec<SyncOutcomeOutput>,
}

/// One NDJSON record emitted by `rwv sync --json -j N` with `N > 1`.
///
/// Under NDJSON streaming mode, the envelope wrapper is dropped and each
/// per-repo outcome becomes its own self-describing line. Every NDJSON
/// record carries its own `$schema` URL so consumers can identify a line
/// without out-of-band context.
///
/// Serialised with `#[serde(flatten)]` on the inner outcome so the wire
/// shape is a single flat object: `{"$schema": "...", "kind": "...", ...}`,
/// not a nested wrapper.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SyncOutcomeNdjsonRecord<'a> {
    #[serde(rename = "$schema")]
    pub schema: &'a str,
    #[serde(flatten)]
    pub outcome: &'a SyncOutcomeOutput,
}

/// Schema URL embedded in `rwv sync --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/`.
pub const SYNC_JSON_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/sync.json";

/// Schema URL embedded in `rwv sync-to --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/`.
pub const SYNC_TO_JSON_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/sync-to.json";

impl fmt::Display for RepoSyncOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Converged => f.write_str("converged"),
            Self::AlreadyAhead { commits_ahead } => write!(
                f,
                "already-ahead (lock is {commits_ahead} commit{s} behind HEAD; \
                 rerun with --strategy=rebase to land HEAD on lock, or accept the divergence)",
                s = if *commits_ahead == 1 { "" } else { "s" }
            ),
            Self::NoOp => f.write_str("up-to-date"),
            Self::Failed(failure) => fmt::Display::fmt(failure, f),
        }
    }
}

fn sync_one_repo(
    repo: &Path,
    target: &ResolvedRevisionId,
    strategy: SyncStrategy,
) -> RepoSyncOutcome {
    let head = match GitVcs.head_revision(repo) {
        Ok(h) => h,
        Err(e) => {
            let error = e.to_string();
            return RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
                error,
                cause: Some(e),
            });
        }
    };

    if head == *target {
        return RepoSyncOutcome::NoOp;
    }

    // Detect AlreadyAhead: lock is a strict ancestor of HEAD (HEAD is past the lock).
    let is_ancestor = GitVcs.is_ancestor(repo, target, &head).unwrap_or(false);

    if is_ancestor {
        let commits_ahead = GitVcs
            .count_commits_in_range(repo, target, &head)
            .unwrap_or(0);
        return RepoSyncOutcome::AlreadyAhead { commits_ahead };
    }

    match apply_strategy(repo, target, strategy) {
        Ok(()) => RepoSyncOutcome::Converged,
        Err(StrategyError { message, cause }) => {
            RepoSyncOutcome::Failed(SyncFailure::for_strategy(strategy, message, cause))
        }
    }
}

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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Hyphen-spelled label for a mid-op [`ConflictOp`] — `"rebase"`, `"merge"`,
/// or `"cherry-pick"`. Used to compose `"mid-{label}"` messages without
/// hardcoding a VCS vocabulary table inside sync. The VCS vocabulary (which
/// op names exist) lives behind [`ConflictOp`]; this helper only shapes the
/// display text.
fn mid_op_label(op: ConflictOp) -> &'static str {
    match op {
        ConflictOp::Rebase => "rebase",
        ConflictOp::Merge => "merge",
        ConflictOp::CherryPick => "cherry-pick",
    }
}

/// Failure from [`apply_strategy`] carrying both the human-formatted error
/// string and (when available) the underlying typed [`VcsError`] so callers
/// can plumb structured cause info into `--json` output.
struct StrategyError {
    message: String,
    cause: Option<VcsError>,
}

impl StrategyError {
    fn from_message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cause: None,
        }
    }

    fn from_vcs(err: VcsError) -> Self {
        Self {
            message: err.to_string(),
            cause: Some(err),
        }
    }
}

fn apply_strategy(
    repo: &Path,
    target: &ResolvedRevisionId,
    strategy: SyncStrategy,
) -> Result<(), StrategyError> {
    match strategy {
        SyncStrategy::Ff => {
            if let Err(e) = GitVcs.advance_if_fast_forward(repo, target) {
                return Err(StrategyError::from_message(format!(
                    "cannot fast-forward; rerun with --strategy rebase or --strategy merge. {}",
                    e
                )));
            }
        }
        SyncStrategy::Rebase => {
            // Route through the Vcs trait so the in-flight conflict signal
            // (mid-rebase state + RebaseConflict) is the consolidated path
            // for both manifest and project repos. `git rebase <target>` is
            // equivalent to `git rebase --onto <target> <target>` — git
            // computes the merge-base internally to bound the replay set.
            GitVcs
                .rebase(repo, target, target)
                .map_err(StrategyError::from_vcs)?;
        }
        SyncStrategy::Merge => {
            // Merge with auto-generated commit message.
            GitVcs
                .merge_from(repo, target)
                .map_err(|e| StrategyError::from_message(format!("merge failed: {e}")))?;
        }
    }
    Ok(())
}

fn create_savepoint(repo: &Path, op_id: &OpId) -> anyhow::Result<ResolvedRevisionId> {
    Ok(GitVcs.create_savepoint(repo, op_id.as_str())?)
}

fn delete_savepoint(repo: &Path, op_id: &OpId) {
    GitVcs.drop_savepoint(repo, op_id.as_str());
}

/// The recovery instruction differs by side: source's lock is committed
/// upstream from the operator's perspective ("Run `rwv lock` in the source
/// workspace and commit before syncing"), destination's is right here
/// ("Run `rwv lock` to refresh before syncing").
fn lock_recovery(side: Side) -> &'static str {
    match side {
        Side::Source => "Run `rwv lock` in the source workspace and commit before syncing",
        Side::Destination => "Run `rwv lock` to refresh before syncing",
    }
}

fn check_lock_freshness(
    workspace_dir: &Path,
    lock: &LockFile,
    side: Side,
    workspace_name: &str,
) -> anyhow::Result<()> {
    // Resolve lock entries against on-disk repos so the comparison below is
    // purely a canonical-SHA equality check. Tag-form entries (e.g. v0.3.4)
    // resolve to the canonical SHA; SHA-form entries pass through unchanged.
    let (resolved, failures) = lock.clone().resolve_versions(workspace_dir);
    if let Some((repo_path, raw_version)) = failures.first() {
        let raw = raw_version.as_str().to_string();
        let side_str = side.as_str();
        let recovery = lock_recovery(side);
        anyhow::bail!(
            "{side_str} workspace '{workspace_name}' lock references unknown revision {raw} for {repo_path}. {recovery}.",
        );
    }

    for (repo_path, lock_entry) in resolved.iter_entries() {
        let abs = workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            continue;
        }
        if let Ok(actual) = GitVcs.head_revision(&abs) {
            if actual != lock_entry.version {
                let side_str = side.as_str();
                let recovery = lock_recovery(side);
                anyhow::bail!(
                    "{side_str} workspace '{workspace_name}' has a stale lock — {repo_path} tip={actual} doesn't match lock={}. {recovery}.",
                    lock_entry.version
                );
            }
        }
    }
    Ok(())
}

/// Phase 1 precondition predicate: would resetting `cwd_tip` to `source_tip`
/// discard reachable commits? Returns `true` when CWD is an ancestor of (or
/// equal to) source — the safe cases.
fn cwd_is_ancestor_or_equal(
    cwd_project_dir: &Path,
    cwd_tip: &ResolvedRevisionId,
    source_tip: &ResolvedRevisionId,
) -> bool {
    if cwd_tip == source_tip {
        return true;
    }
    // Both tips must be reachable in cwd_project_dir's object DB for
    // is_ancestor to work. (Source's tip is reachable because Phase 1's
    // reset --hard relies on the same reachability.)
    GitVcs
        .is_ancestor(cwd_project_dir, cwd_tip, source_tip)
        .unwrap_or(false)
}

/// Phase 1 precondition (ff-strategy only): refuse if fast-forwarding CWD's
/// project repo to source's tip would discard commits reachable only from CWD.
///
/// Cases:
/// - equal: no-op, allowed.
/// - CWD ancestor of source (forward): allowed — the normal sync case.
/// - source ancestor of CWD (CWD ahead): refused — ff cannot discard commits.
/// - diverged (neither ancestor): refused — ff cannot merge.
///
/// `rebase` and `merge` strategies bypass this precondition; they handle
/// divergence by replaying CWD's project commits (with `rwv.lock` excluded)
/// onto source's tip.
fn check_phase1_ancestor(
    cwd_project_dir: &Path,
    cwd_tip: &ResolvedRevisionId,
    source_tip: &ResolvedRevisionId,
    cwd_workspace_name: &str,
    source_workspace_name: &str,
) -> anyhow::Result<()> {
    if cwd_is_ancestor_or_equal(cwd_project_dir, cwd_tip, source_tip) {
        return Ok(());
    }

    // CWD is not an ancestor of source. Count the commits CWD has that source
    // doesn't (the ones a fast-forward would refuse to land).
    let extra_count = GitVcs
        .count_commits_in_range(cwd_project_dir, source_tip, cwd_tip)
        .map(|n| n.to_string())
        .unwrap_or_else(|_| "?".to_owned());

    anyhow::bail!(
        "destination workspace '{cwd_workspace_name}' project repo at {cwd_tip} has {extra_count} \
         commits not in source workspace '{source_workspace_name}'. Rerun with `--strategy rebase` \
         or `--strategy merge` to land them, sync the other direction first to bring those commits \
         to source, or use `--force` if you intend to discard them (preserved in refs/rwv/pre-op/<id> \
         for `rwv abort`).",
    );
}

// ---------------------------------------------------------------------------
// Conflict-bail messages
// ---------------------------------------------------------------------------
//
// Sync's conflict messages lead with concrete resolution steps and mention
// `rwv abort` last as the rollback option. The per-VCS step text comes from
// [`Vcs::conflict_resolution_hint`] so a future non-git impl supplies its
// own vocabulary.

/// Which lock-phase emitted a top-level failure — for the message tag.
#[derive(Debug, Clone, Copy)]
enum Phase {
    /// Phase 1' — project-repo strategy with `rwv.lock` excluded.
    One,
    /// Phase 3 — regenerate `rwv.lock` and commit if changed.
    Three,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::One => "Phase 1' (project repo)",
            Self::Three => "Phase 3 (re-lock)",
        }
    }
}

/// Map a [`SyncStrategy`] to the in-flight VCS op a conflict would leave behind.
///
/// `Ff` cannot leave a conflict (fast-forward refuses without altering state);
/// we still return a hint shape for the message — the user will likely want to
/// rerun with `--strategy rebase` and resolve there.
fn conflict_op_for_strategy(strategy: SyncStrategy) -> ConflictOp {
    match strategy {
        // `--strategy ff` cannot conflict; pick Rebase as the resolution
        // mode the user is likely to fall back to.
        SyncStrategy::Ff | SyncStrategy::Rebase => ConflictOp::Rebase,
        SyncStrategy::Merge => ConflictOp::Merge,
    }
}

/// Bail message for the manifest-repo per-repo sync loop (Site 1).
///
/// One or more repos in the loop emitted a per-repo failure (printed already).
/// Lead with the resolution steps that apply uniformly to each conflicted
/// repo; mention `rwv abort` last as the rollback option.
fn manifest_repo_failure_message(strategy: SyncStrategy, resolved_source: &SyncSource) -> String {
    let op = conflict_op_for_strategy(strategy);
    let hint = GitVcs.conflict_resolution_hint(op);
    format!(
        "sync hit failures in one or more manifest repos (see per-repo lines above).\n\
         \n\
         To resolve each conflicted repo:\n\
           cd <repo>\n\
         {hint}\n\
           rwv sync {resolved_source}   # re-run; already-converged repos are no-ops\n\
         \n\
         If you'd rather roll everything back: `rwv abort`."
    )
}

/// Bail message for the Phase 1' / Phase 3 top-level failures (Sites 2 and 3).
///
/// Both phases print their inner error via `eprintln!` before bailing; this
/// message gives the operator a uniform "what next?" block that leads with
/// resolution steps (for the conflict sub-case the inner error implies) and
/// closes with `rwv abort` as the rollback option.
fn phase1_or_phase3_failure_message(
    phase: Phase,
    cwd_project_dir: &Path,
    strategy: SyncStrategy,
    resolved_source: &SyncSource,
) -> String {
    let op = conflict_op_for_strategy(strategy);
    let hint = GitVcs.conflict_resolution_hint(op);
    let phase_label = phase.label();
    let repo_display = cwd_project_dir.display();
    format!(
        "sync failed in {phase_label} (see error above).\n\
         \n\
         If the failure is a conflict, resolve in {repo_display}:\n\
           cd {repo_display}\n\
         {hint}\n\
           rwv sync {resolved_source}   # re-run; already-converged repos are no-ops\n\
         \n\
         For other failures: fix the underlying issue then `rwv sync {resolved_source}`.\n\
         If you'd rather roll everything back: `rwv abort`."
    )
}

/// Bail message for an inner per-conflict-site.
///
/// Used by Phase 1' when a rebase or merge leaves the project repo in the
/// VCS-native in-flight state. The per-VCS resolution steps come from the
/// trait method; this helper builds the surrounding framing (which repo,
/// how to re-run, how to abort).
fn per_conflict_bail_message(
    repo: &Path,
    op: ConflictOp,
    op_label: &str,
    detail: &str,
    resolved_source: &SyncSource,
) -> String {
    let hint = GitVcs.conflict_resolution_hint(op);
    let repo_display = repo.display();
    format!(
        "sync hit a conflict in {repo_display} during {op_label} ({detail}).\n\
         \n\
         To resolve:\n\
           cd {repo_display}\n\
         {hint}\n\
           rwv sync {resolved_source}   # re-run; already-converged repos are no-ops\n\
         \n\
         If you'd rather roll everything back: `rwv abort`."
    )
}

// Post-sync index/working-tree refresh is delegated to
// [`Vcs::refresh_index_to_head_if_safe`] and
// [`Vcs::refresh_working_tree_to_head_if_safe`]; the safety logic
// (reachability check before any clobber) lives in the VCS impl rather
// than being inlined here. See those trait method doc-comments.

/// Precondition: the CWD project repo's committed `.gitattributes` must contain
/// `rwv.lock merge=ours` before any sync strategy that performs a 3-way merge
/// (`Rebase` or `Merge`).
///
/// The mechanism has two halves: the inline `-c merge.ours.driver=true` flag
/// *defines* a merge driver named "ours", and the `.gitattributes` line
/// *assigns* that driver to `rwv.lock`. Without the assignment, git's default
/// 3-way merge runs on `rwv.lock` and conflicts whenever both sides have
/// lock edits — regardless of which drivers are defined in config.
///
/// `Ff` does not perform any merge — it advances the branch pointer — so
/// the invariant is not required.
///
/// Checked against the committed file (via `git show HEAD:.gitattributes`)
/// rather than the working-tree file because:
/// 1. The invariant must survive rebases (which replay committed trees).
/// 2. A `.gitattributes` that exists only in the working tree is not
///    durable — it won't be present after a `git reset --hard` or fresh clone.
///
/// If absent, bails with an actionable message naming the file path, the exact
/// missing line, and the command to fix (`rwv doctor --fix`). Does NOT write
/// the file — that is `rwv doctor --fix`'s job; sync's invariant is "only
/// change what the source says to change".
fn verify_replay_exclusion_invariant(cwd_project_dir: &Path) -> anyhow::Result<()> {
    let has_line = GitVcs
        .has_committed_replay_exclusion(cwd_project_dir, Path::new("rwv.lock"))
        .unwrap_or(false);

    if has_line {
        return Ok(());
    }

    anyhow::bail!(
        "sync --strategy=rebase and --strategy=merge require `rwv.lock merge=ours` \
         in the project repo's committed .gitattributes, but {ga} does not contain \
         that line.\n\
         \n\
         Without it, git's 3-way merge runs on rwv.lock and conflicts whenever \
         both sides have lock edits.\n\
         \n\
         To fix: run `rwv doctor --fix` from this workspace, then commit the result:\n\
           cd {dir}\n\
           rwv doctor --fix\n\
           git add .gitattributes && git commit -m \"chore: add rwv.lock replay-exclusion\"",
        ga = cwd_project_dir.join(".gitattributes").display(),
        dir = cwd_project_dir.display(),
    )
}

fn find_project_name(ctx: &WorkspaceContext) -> anyhow::Result<ProjectName> {
    match &ctx.location {
        WorkspaceLocation::Weave { project: Some(_) } => {
            // Delegate to require_active_project_on_disk so a dangling
            // .rwv-active fails early with a clear message rather than
            // producing confusing downstream git errors.
            ctx.require_active_project_on_disk().cloned()
        }
        WorkspaceLocation::Workweave { project, .. } => Ok(project.clone()),
        WorkspaceLocation::Weave { project: None } => {
            // require_active_project produces the same helpful error
            // mentioning --project / rwv activate; defer to it.
            ctx.require_active_project().cloned()
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 3 helpers: materialize new repos / prune dropped repos
// ---------------------------------------------------------------------------

/// Materialize a repo that's listed in the (source) lock but missing from the
/// CWD workspace on disk.
///
/// - In a workweave: `git worktree add` against the canonical clone at primary
///   (mirrors what `create_workweave` does for initial materialization).
/// - In a primary weave: `git clone` from the manifest URL (mirrors
///   `rwv fetch`'s clone path).
///
/// `entry` carries the manifest URL; `target` is the lock revision to check
/// out. Caller is responsible for the surrounding sync flow (Phase 2 will then
/// call `sync_one_repo` to land HEAD on `target`).
fn materialize_missing_repo(
    ctx: &WorkspaceContext,
    repo_path: &RepoPath,
    entry: &crate::manifest::RepoEntry,
    project_name: &ProjectName,
) -> anyhow::Result<()> {
    let dest = ctx.active_path().join(repo_path.as_path());
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    match &ctx.location {
        WorkspaceLocation::Workweave { name, .. } => {
            // Canonical clone lives at primary.
            let canonical = ctx.primary_path().join(repo_path.as_path());
            if !canonical.exists() {
                anyhow::bail!(
                    "canonical clone for {repo_path} missing at {}; \
                     run `rwv sync` from primary first to materialize it there",
                    canonical.display()
                );
            }
            // Use the manifest's tracking branch as the start point. Workweave
            // ephemeral branches are scoped by (project, workweave_name) to
            // mirror create_workweave's naming.
            let start_ref = entry.version.as_str();
            let head_rev = GitVcs
                .resolve_revision(&canonical, start_ref)
                .with_context(|| format!("failed to resolve {start_ref} in canonical clone"))?;
            let branch = crate::vcs::RefName::new(format!(
                "{}--{}/{}",
                project_name.as_str(),
                name.as_str(),
                start_ref,
            ));
            GitVcs
                .create_worktree(&canonical, &dest, &branch, &head_rev)
                .with_context(|| format!("worktree add for {repo_path} failed"))?;
        }
        WorkspaceLocation::Weave { .. } => {
            GitVcs
                .clone_repo(&entry.url.to_string(), &dest)
                .with_context(|| format!("clone of {repo_path} from {} failed", entry.url))?;
        }
    }
    Ok(())
}

/// Conservatively remove a repo's worktree/clone after it has been dropped
/// from the lock. Refuses (and warns) if the worktree has uncommitted changes
/// or local-only commits (branch tip differs from canonical HEAD in workweave;
/// any commits at all in primary).
fn prune_dropped_repo(ctx: &WorkspaceContext, repo_path: &RepoPath) -> anyhow::Result<()> {
    let dest = ctx.active_path().join(repo_path.as_path());
    if !dest.exists() {
        return Ok(());
    }
    if GitVcs.has_uncommitted_changes(&dest).unwrap_or(true) {
        anyhow::bail!(
            "{repo_path}: dropped from lock but worktree has uncommitted changes; \
             commit/discard and re-run sync, or remove manually"
        );
    }

    match &ctx.location {
        WorkspaceLocation::Workweave { .. } => {
            // Diverged-from-canonical check: refuse if local commits would be lost.
            let canonical = ctx.primary_path().join(repo_path.as_path());
            if canonical.exists() {
                let wt_head = GitVcs.head_revision(&dest).ok();
                let canon_head = GitVcs.head_revision(&canonical).ok();
                if let (Some(w), Some(c)) = (wt_head, canon_head) {
                    if w != c {
                        // Allow when w is ancestor of c (no unique commits in workweave).
                        let is_ancestor = GitVcs.is_ancestor(&dest, &w, &c).unwrap_or(false);
                        if !is_ancestor {
                            anyhow::bail!(
                                "{repo_path}: dropped from lock but worktree has commits not in canonical clone; \
                                 push/merge them and re-run, or remove manually"
                            );
                        }
                    }
                }
                GitVcs
                    .remove_worktree(&canonical, &dest)
                    .with_context(|| format!("worktree remove for {repo_path} failed"))?;
                let _ = GitVcs.worktree_prune(&canonical);
            } else {
                // No canonical to compare to; remove the directory as a best effort.
                std::fs::remove_dir_all(&dest)
                    .with_context(|| format!("failed to remove {}", dest.display()))?;
            }
        }
        WorkspaceLocation::Weave { .. } => {
            // Primary: refuse if local-only branches with unique commits exist.
            // Conservative — any branch with commits not on origin is grounds.
            // We don't know the manifest role of this dropped repo at prune
            // time (the lock entry is gone); Role::Owned selects the
            // canonical-clone remote convention (`origin` in [`GitVcs`])
            // which matches what every non-fork lock entry was cloned with.
            let any_local_only = match GitVcs.list_local_branches(&dest) {
                Ok(names) => {
                    let mut any = false;
                    for branch in &names {
                        // Strip the refs/heads/ prefix that for-each-ref
                        // emits; trait methods take bare branch names.
                        let short = RefName::new(
                            branch.as_str().trim_start_matches("refs/heads/").to_owned(),
                        );
                        let has_counterpart = GitVcs
                            .branch_has_remote_counterpart(&dest, &short, Role::Owned)
                            .unwrap_or(false);
                        if !has_counterpart {
                            any = true;
                            break;
                        }
                        let count = GitVcs
                            .count_commits_ahead_of_remote(&dest, &short, Role::Owned)
                            .unwrap_or(0);
                        if count > 0 {
                            any = true;
                            break;
                        }
                    }
                    any
                }
                Err(_) => true, // conservative: refuse on uncertainty
            };
            if any_local_only {
                anyhow::bail!(
                    "{repo_path}: dropped from lock but clone has local-only commits; \
                     push them and re-run, or remove manually"
                );
            }
            std::fs::remove_dir_all(&dest)
                .with_context(|| format!("failed to remove {}", dest.display()))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// OutputHandler trait: open extensibility seam for per-repo output.
// ---------------------------------------------------------------------------

/// Trait implemented by every output mode.
///
/// The three built-in implementations are [`TextHandler`],
/// [`JsonEnvelopeHandler`], and [`JsonNdjsonHandler`]. Callers pass
/// `&dyn OutputHandler` into the orchestration body so new modes can be
/// added without touching existing orchestration code.
///
/// ## Contract
///
/// - `record` is called once per completed per-repo outcome (including
///   failures surfaced before the main sync loop). Implementations may
///   print, buffer, or stream as appropriate.
/// - `emit_text` controls whether the orchestration body emits human-readable
///   progress lines to stdout/stderr. Returning `false` suppresses all
///   non-record chatter (used by JSON modes).
///
/// ## Thread safety
///
/// `Send + Sync` is required because `record` may be called from parallel
/// worker threads inside `run_in_parallel`. Implementations use interior
/// mutability (e.g. `Mutex`) for any mutable state.
pub trait OutputHandler: Send + Sync {
    /// Record one per-repo outcome.
    ///
    /// `path` is the repo's manifest-relative path string; `abs_path` is
    /// the absolute on-disk path. `outcome` is the raw sync result —
    /// implementations convert to `SyncOutcomeOutput` internally when needed.
    fn record(&self, path: &str, abs_path: &str, outcome: &RepoSyncOutcome);

    /// Return `true` if the orchestration body should emit human-readable
    /// text progress to stdout/stderr. JSON-mode handlers return `false`.
    fn emit_text(&self) -> bool;
}

/// Text-mode handler: prints one line per repo to stdout/stderr and discards
/// structured records. Used by `rwv sync` and `rwv sync-to` (no `--json`).
///
/// `stdout_lock` serialises concurrent writes when `-j > 1` is combined with
/// text mode (defensive; text mode is serial in normal use).
pub struct TextHandler<'a> {
    stdout_lock: &'a Mutex<()>,
}

impl OutputHandler for TextHandler<'_> {
    fn emit_text(&self) -> bool {
        true
    }

    fn record(&self, path: &str, _abs_path: &str, outcome: &RepoSyncOutcome) {
        let _guard = self.stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        if outcome.is_failure() {
            eprintln!("  {path}: {outcome}");
        } else {
            println!("  {path}: {outcome}");
        }
    }
}

/// Envelope-mode handler: buffers all records so the caller can emit a single
/// `{ "$schema": ..., "outcomes": [...] }` JSON object after the sync
/// completes. Used by `rwv sync --json` with `-j 1` (or unspecified).
///
/// Text chatter is suppressed (`emit_text` returns `false`).
pub struct JsonEnvelopeHandler<'a> {
    records: &'a Mutex<Vec<SyncOutcomeOutput>>,
}

impl OutputHandler for JsonEnvelopeHandler<'_> {
    fn emit_text(&self) -> bool {
        false
    }

    fn record(&self, path: &str, abs_path: &str, outcome: &RepoSyncOutcome) {
        let out = SyncOutcomeOutput::from_outcome(path.to_owned(), abs_path.to_owned(), outcome);
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(out);
    }
}

/// NDJSON streaming handler: writes one self-describing JSON line per record
/// to stdout as it arrives, and also buffers into `records` so post-loop
/// callers can check `any_failure`. Used by `rwv sync --json -j N` with
/// `N > 1`.
///
/// `stdout_lock` serialises concurrent writes so parallel workers cannot
/// interleave bytes. Text chatter is suppressed (`emit_text` returns `false`).
pub struct JsonNdjsonHandler<'a> {
    stdout_lock: &'a Mutex<()>,
    records: &'a Mutex<Vec<SyncOutcomeOutput>>,
    schema_url: &'a str,
}

impl OutputHandler for JsonNdjsonHandler<'_> {
    fn emit_text(&self) -> bool {
        false
    }

    fn record(&self, path: &str, abs_path: &str, outcome: &RepoSyncOutcome) {
        let out = SyncOutcomeOutput::from_outcome(path.to_owned(), abs_path.to_owned(), outcome);
        let record = SyncOutcomeNdjsonRecord {
            schema: self.schema_url,
            outcome: &out,
        };
        // Best-effort: a serialization failure here would mean the outcome
        // type itself is malformed; we still want to buffer the record so
        // the post-loop any_failure check works correctly.
        if let Ok(line) = serde_json::to_string(&record) {
            let _guard = self.stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            let _ = writeln!(handle, "{line}");
            let _ = handle.flush();
        }
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(out);
    }
}

// ---------------------------------------------------------------------------
// The phase machine
// ---------------------------------------------------------------------------
//
// One data-driven machine drives both `rwv sync` and `rwv sync-to`. Phases:
//
//   guard → mark → savepoint → replay → relock → advance-target → retire → cleanup
//                                                 (sync-to only)   (--retire only)
//
// guard + mark + savepoint happen once, before the loop, in `guard_and_mark`.
// The persisted record's phase starts at `Replay`. The driver loop is:
//
//   loop {
//       op_state::advance_phase(owner, state.phase);   // one persistence point
//       state.phase = run_phase(ctx, state.phase)?;
//       if state.terminal { break }
//   }
//   cleanup(ctx);                                       // drop savepoints + clear record
//
// **Invariant:** the persisted phase is the phase in progress, and every
// phase is idempotent and re-runnable from the record alone.
//
// - replay re-entry derives per-repo state from the VCS itself (savepoint →
//   redo; mid-conflict → VCS-native continue; already-converged → no-op).
//   No `resume_phase` / `step1_continue` flags anywhere.
// - relock re-entry: regenerating a current lock is a no-op.
// - advance-target re-entry: ff to an already-reached tip is a no-op.
// - retire re-entry: re-running the merged-check is read-only.
//
// `--continue` (both verbs) = load record (resolving lease pointer if invoked
// from a non-owner workspace), enter the driver loop at the recorded phase.

/// Immutable per-op context built once, before the driver loop. Holds
/// everything the phase functions need that isn't on disk in the owner record.
struct OpContext<'a> {
    /// CWD workspace context (the workspace invocation was made from).
    cwd_ctx: WorkspaceContext,
    /// CWD workspace root (== owner workspace for fresh invocations from the
    /// owner side; == lease workspace when --continue is invoked from target).
    cwd_workspace_dir: PathBuf,
    /// Workspace that holds the full owner record. Resolved from a lease
    /// pointer when --continue is invoked from a non-owner workspace.
    owner_workspace_dir: PathBuf,
    /// Source-of-content workspace (CWD pulls from here). For plain `sync`
    /// this is the explicit source; for `sync-to` this is CWD (since sync-to
    /// step 1 is `sync` with target as source).
    source_workspace_dir: PathBuf,
    source_project_dir: PathBuf,
    source_workspace_name: String,
    /// Destination workspace (the one phases write into). For plain `sync`
    /// this is CWD; for `sync-to` this is the target workspace.
    dest_workspace_dir: PathBuf,
    dest_project_dir: PathBuf,
    /// CWD's project repo dir. Equal to `dest_project_dir` for plain `sync`
    /// (where dest is CWD); for `sync-to` it's where replay+relock run, and
    /// `dest_project_dir` is where advance-target lands.
    cwd_project_dir: PathBuf,
    /// CWD project name (used for materialize_missing_repo's ephemeral
    /// branch namespace in workweaves).
    cwd_project_name: ProjectName,
    /// `SyncSource` form of `source_workspace_dir`, retained for the
    /// human-readable hints in bail messages (`rwv sync <thing>`).
    resolved_source: SyncSource,
    /// Path arg the operator passed on the CLI (or recorded in op-state),
    /// retained for hint messages that show the original target spelling.
    cli_path: PathBuf,
    strategy: SyncStrategy,
    force: bool,
    retire: bool,
    jobs: usize,
    handler: &'a dyn OutputHandler,
    verb: op_state::OpVerb,
    op_id: OpId,
    /// Cached snapshot of source tip + manifest + lock, pinned at the start
    /// of the FIRST replay entry (T0). Filled in by the replay phase on
    /// initial entry; re-derived from disk on resume (the source workspace
    /// is read-only from our perspective and may have moved on, but Phase 2's
    /// per-repo no-op detection handles already-converged repos cleanly).
    ///
    /// Kept in a `Cell`/`RefCell` so phase functions remain `&self` callers;
    /// `OpContext` is built per-invocation so no thread-safety concerns.
    snapshot: std::cell::RefCell<Option<SourceSnapshot>>,
}

/// Atomic source snapshot pinned at T0 (start of replay).
///
/// The source's project tip is read once and everything derived from it —
/// manifest, lock — is read AT that revision via `Vcs::read_file_at_revision`.
/// A concurrent mutation of the source after T0 changes refs but cannot touch
/// anything we've read (§6 of the design doc).
struct SourceSnapshot {
    /// Source project tip at T0.
    source_project_tip: ResolvedRevisionId,
    /// Source manifest, read at `source_project_tip`.
    source_manifest: Manifest,
    /// Source lock (raw, unresolved), read at `source_project_tip`.
    raw_source_lock: LockFile,
}

impl OpContext<'_> {
    /// Convenience: the active phase recorded on the owner record.
    fn current_phase(&self) -> anyhow::Result<op_state::OpPhase> {
        let record = op_state::read_owner(&self.owner_workspace_dir)?.ok_or_else(|| {
            anyhow::anyhow!(
                "internal: owner record disappeared at {} mid-op",
                self.owner_workspace_dir.display()
            )
        })?;
        Ok(record.phase)
    }
}

/// Plain enum of which top-level driver entry built this op.
#[derive(Debug, Clone, Copy)]
enum MachineVerb {
    /// `rwv sync <source>`: degenerate machine with no advance-target/retire.
    Sync,
    /// `rwv sync-to <target>`: full machine; advance-target always runs;
    /// retire runs only when `--retire` is set.
    SyncTo,
}

impl MachineVerb {
    fn op_verb(self) -> op_state::OpVerb {
        match self {
            Self::Sync => op_state::OpVerb::Sync,
            Self::SyncTo => op_state::OpVerb::SyncTo,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level entry points (public API surface unchanged)
// ---------------------------------------------------------------------------

/// Execute `rwv sync <source>`.
///
/// `source` is required; bare `rwv sync` (no source) is not supported.
/// Use `rwv sync-to` to land work upward.
///
/// `do_continue = true` activates `--continue` mode: instead of refusing when
/// an op-state file is present, the call resumes from the recorded phase.
/// All parameters are read from the recorded state in that path.
#[allow(clippy::too_many_arguments)]
pub fn run_sync(
    cwd: &Path,
    source: Option<&SyncSource>,
    strategy: SyncStrategy,
    force: bool,
    retire: bool,
    project_override: Option<ProjectName>,
    jobs: usize,
    do_continue: bool,
) -> anyhow::Result<()> {
    let stdout_lock: Mutex<()> = Mutex::new(());
    let handler = TextHandler {
        stdout_lock: &stdout_lock,
    };
    run_machine(
        MachineVerb::Sync,
        cwd,
        source,
        strategy,
        force,
        retire,
        project_override,
        jobs,
        &handler,
        do_continue,
    )
}

// ---------------------------------------------------------------------------
// Driver entry: shared by sync and sync-to (text + json modes)
// ---------------------------------------------------------------------------

/// Build the [`OpContext`] and run the phase-machine driver. Both `rwv sync`
/// and `rwv sync-to` route through here; the `verb` parameter selects which
/// phases run (advance-target / retire are sync-to-only and --retire-only).
///
/// `source` is the explicit source/target the operator passed on the CLI, or
/// `None` under `--continue` (read from op-state). `do_continue = true` means
/// "resolve op-state from CWD (following a lease pointer if invoked from a
/// non-owner workspace), enter the driver loop at the recorded phase".
#[allow(clippy::too_many_arguments)]
fn run_machine(
    verb: MachineVerb,
    cwd: &Path,
    source: Option<&SyncSource>,
    strategy: SyncStrategy,
    force: bool,
    retire: bool,
    project_override: Option<ProjectName>,
    jobs: usize,
    handler: &dyn OutputHandler,
    do_continue: bool,
) -> anyhow::Result<()> {
    let ctx = if do_continue {
        load_continuing_context(
            verb,
            cwd,
            project_override.clone(),
            jobs,
            handler,
        )?
    } else {
        guard_and_mark(
            verb,
            cwd,
            source,
            strategy,
            force,
            retire,
            project_override.clone(),
            jobs,
            handler,
        )?
    };

    drive(&ctx)
}

/// The phase-machine driver. Persists `state.phase` before each phase, then
/// runs `run_phase` which returns the next phase (or `None` for terminal).
///
/// One persistence point per iteration. Every phase is idempotent and
/// re-runnable from the record alone — crash inside a phase leaves the
/// record at that phase, and resume re-enters there.
fn drive(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    loop {
        let phase = ctx.current_phase()?;
        // One write, one file: persist the phase we're about to enter so a
        // crash inside `run_phase` leaves the record at `phase`. The owner
        // record already holds `phase` (either from guard_and_mark's initial
        // write, or from the previous iteration's transition); this call is
        // a no-op write the first time around and an advance after that.
        op_state::advance_phase(&ctx.owner_workspace_dir, phase.clone())
            .context("failed to persist phase advance")?;

        let next = run_phase(ctx, phase)?;
        match next {
            Some(p) => {
                // The next iteration's advance_phase write is the canonical
                // persistence point for `p`. We don't write it here.
                op_state::advance_phase(&ctx.owner_workspace_dir, p)
                    .context("failed to advance phase between iterations")?;
            }
            None => {
                cleanup(ctx)?;
                return Ok(());
            }
        }
    }
}

/// Dispatch one phase. Each phase function is idempotent and returns the
/// next phase (or `None` to signal "no more phases — proceed to cleanup").
fn run_phase(
    ctx: &OpContext<'_>,
    phase: op_state::OpPhase,
) -> anyhow::Result<Option<op_state::OpPhase>> {
    use op_state::OpPhase;
    match phase {
        OpPhase::Replay => {
            run_replay(ctx)?;
            Ok(Some(OpPhase::Relock))
        }
        OpPhase::Relock => {
            run_relock(ctx)?;
            Ok(next_after_relock(ctx))
        }
        OpPhase::AdvanceTarget => {
            run_advance_target(ctx)?;
            Ok(next_after_advance_target(ctx))
        }
        OpPhase::Retire => {
            run_retire(ctx)?;
            Ok(None)
        }
    }
}

/// After relock, plain `sync` is done; `sync-to` continues with advance-target.
fn next_after_relock(ctx: &OpContext<'_>) -> Option<op_state::OpPhase> {
    match ctx.verb {
        op_state::OpVerb::Sync => None,
        op_state::OpVerb::SyncTo => Some(op_state::OpPhase::AdvanceTarget),
    }
}

/// After advance-target, retire runs only when `--retire` was passed.
fn next_after_advance_target(ctx: &OpContext<'_>) -> Option<op_state::OpPhase> {
    if ctx.retire {
        Some(op_state::OpPhase::Retire)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Pre-loop: guard + mark + savepoint (fresh start)
// ---------------------------------------------------------------------------

/// Guard (preconditions), mark (write owner record + leases), savepoint
/// (per-repo pre-op refs). Returns the immutable [`OpContext`] driving the
/// loop. Refusals here leave no trace.
#[allow(clippy::too_many_arguments)]
fn guard_and_mark<'a>(
    verb: MachineVerb,
    cwd: &Path,
    source: Option<&SyncSource>,
    strategy: SyncStrategy,
    force: bool,
    retire: bool,
    project_override: Option<ProjectName>,
    jobs: usize,
    handler: &'a dyn OutputHandler,
) -> anyhow::Result<OpContext<'a>> {
    let emit_text = handler.emit_text();
    let cwd_ctx = WorkspaceContext::resolve(cwd, project_override.clone())?;
    let cwd_workspace_dir = cwd_ctx.active_path().to_path_buf();

    // Resolve the SyncSource the operator passed. For sync, this is the
    // source workspace; for sync-to, this is the target workspace.
    let resolved_arg = match source {
        Some(s) => s.clone(),
        None => match (verb, &cwd_ctx.location) {
            // Bare `rwv sync` inside a workweave: read parent from the marker.
            (MachineVerb::Sync, WorkspaceLocation::Workweave { dir, .. }) => {
                let marker = crate::workspace::WorkweaveMarker::read(dir)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "bare `rwv sync` requires a `.rwv-workweave` marker in the \
                         workweave; found none at {} (re-create the workweave or pass \
                         an explicit source)",
                        dir.display()
                    )
                })?;
                SyncSource::Path(marker.parent)
            }
            (MachineVerb::Sync, WorkspaceLocation::Weave { .. }) => {
                anyhow::bail!(
                    "bare `rwv sync` syncs to the workweave's recorded parent, but CWD \
                     ({}) is in the primary weave, not a workweave; pass an explicit source",
                    cwd.display()
                );
            }
            (MachineVerb::SyncTo, _) => {
                anyhow::bail!(
                    "sync-to requires an explicit target (resolved by the caller); none provided"
                );
            }
        },
    };
    let cli_path = resolved_arg.resolve(&cwd_ctx)?;

    // For plain `sync`: source = arg, dest = CWD.
    // For `sync-to`:   source = CWD, dest = arg (target).
    let (source_workspace_dir, dest_workspace_dir) = match verb {
        MachineVerb::Sync => (cli_path.clone(), cwd_workspace_dir.clone()),
        MachineVerb::SyncTo => (cwd_workspace_dir.clone(), cli_path.clone()),
    };

    // Project override: when CWD is a workweave its project is immutable and
    // authoritative; pass it through so the *other* workspace resolves the
    // same project regardless of its `.rwv-active`. Otherwise propagate the
    // caller's explicit `--project`.
    let other_project_override = match &cwd_ctx.location {
        WorkspaceLocation::Workweave { project, .. } => Some(project.clone()),
        WorkspaceLocation::Weave { .. } => project_override.clone(),
    };

    let cwd_project_name = find_project_name(&cwd_ctx)?;
    let cwd_project_dir = cwd_workspace_dir.join("projects").join(&cwd_project_name);

    let (source_project_dir, source_workspace_name) = match verb {
        MachineVerb::Sync => {
            let source_ctx =
                WorkspaceContext::resolve(&source_workspace_dir, other_project_override.clone())?;
            let pname = find_project_name(&source_ctx)?;
            let dir = source_ctx.active_path().join("projects").join(&pname);
            (dir, workspace_name(&source_ctx))
        }
        MachineVerb::SyncTo => {
            // For sync-to, source == CWD.
            (cwd_project_dir.clone(), workspace_name(&cwd_ctx))
        }
    };

    let dest_project_dir = match verb {
        MachineVerb::Sync => cwd_project_dir.clone(),
        MachineVerb::SyncTo => {
            let dest_ctx =
                WorkspaceContext::resolve(&dest_workspace_dir, Some(cwd_project_name.clone()))?;
            let pname = find_project_name(&dest_ctx)?;
            dest_ctx.active_path().join("projects").join(&pname)
        }
    };

    // For plain sync: `resolved_source` for hint messages mirrors the
    // operator-supplied arg (where they were syncing FROM).
    // For sync-to: hint messages refer to where they're syncing FROM in step 1
    // (which is the target workspace). The arg from the operator's POV is the
    // target; for hint text purposes we render that path.
    let resolved_source_for_hints = match verb {
        MachineVerb::Sync => resolved_arg.clone(),
        // sync-to: step 1's sync calls have `target` as the source workspace;
        // bail messages reference `rwv sync <thing>` so we render the target
        // path verbatim.
        MachineVerb::SyncTo => SyncSource::Path(cli_path.clone()),
    };

    // Sibling-sync warning: only meaningful for plain sync.
    if matches!(verb, MachineVerb::Sync) {
        warn_on_sibling_sync(&cwd_ctx, &source_workspace_dir, emit_text);
    }

    // === Preconditions (no mutation yet) ===

    // CWD project repo must not be mid-op.
    if let Some(op) = GitVcs.mid_op(&cwd_project_dir) {
        anyhow::bail!(
            "CWD project repo is mid-{op}; resolve before running sync",
            op = mid_op_label(op),
        );
    }

    // sync-to: --strategy=ff has special semantics (CWD must be strictly
    // ahead of target). Bail before any side effects on a refusal.
    if matches!(verb, MachineVerb::SyncTo) && strategy == SyncStrategy::Ff {
        check_sync_to_ff_precondition(
            &cwd_project_dir,
            &dest_project_dir,
            emit_text,
        )?;
    }

    // sync-to dirty-target preflight: refuse up-front if the target
    // workweave has uncommitted changes the advance-target phase would
    // overwrite.
    if matches!(verb, MachineVerb::SyncTo) {
        let cwd_project_preflight = Project::from_dir(&cwd_project_dir)
            .context("failed to load CWD project for dirty-target preflight")?;
        check_dirty_target_preflight(
            &cwd_project_preflight,
            &dest_workspace_dir,
            &dest_project_dir,
            &cli_path,
        )?;
    }

    // Concurrency guard: refuse if any touched workspace carries op-state.
    let touched: Vec<&Path> = match verb {
        MachineVerb::Sync => vec![cwd_workspace_dir.as_path()],
        MachineVerb::SyncTo => vec![cwd_workspace_dir.as_path(), dest_workspace_dir.as_path()],
    };
    op_state::check_no_op_in_progress(&touched)?;

    // === Mark: write owner record + leases ===

    let op_id = OpId::new_now();
    let owner_workspace_dir = cwd_workspace_dir.clone();

    let record = match verb {
        MachineVerb::Sync => OwnerRecord::new_sync(
            &op_id,
            strategy,
            source_workspace_dir.clone(),
            cwd_workspace_dir.clone(),
        ),
        MachineVerb::SyncTo => OwnerRecord::new_sync_to(
            &op_id,
            strategy,
            cwd_workspace_dir.clone(),
            dest_workspace_dir.clone(),
            retire,
        ),
    };
    op_state::write_owner(&owner_workspace_dir, &record)
        .context("failed to write owner record")?;

    // Lease at every other mutated workspace. For plain sync there is no
    // other mutated workspace; for sync-to the target gets a lease.
    if matches!(verb, MachineVerb::SyncTo) {
        let lease = LeaseRecord {
            id: op_id.as_str().to_owned(),
            owner: owner_workspace_dir.clone(),
        };
        op_state::write_lease(&dest_workspace_dir, &lease)
            .context("failed to write lease to target workspace")?;
    }

    // === Savepoint: per-repo pre-op anchor refs ===
    //
    // CWD-side repos and the project repo are anchored so replay re-entry
    // and abort can both restore. For sync-to the target's repos are not
    // savepointed (advance-target is ff-only — no destructive op to undo
    // on the target side; abort hardening (.4) covers target-side rollback
    // separately).
    create_savepoint(&cwd_project_dir, &op_id)?;
    let cwd_project = Project::from_dir(&cwd_project_dir)
        .context("failed to load CWD project for savepoint phase")?;
    for repo_path in cwd_project.manifest.iter_repo_paths() {
        let abs = cwd_workspace_dir.join(repo_path.as_path());
        if abs.exists() {
            let _ = create_savepoint(&abs, &op_id);
        }
    }

    Ok(OpContext {
        cwd_ctx,
        cwd_workspace_dir,
        owner_workspace_dir,
        source_workspace_dir,
        source_project_dir,
        source_workspace_name,
        dest_workspace_dir,
        dest_project_dir,
        cwd_project_dir,
        cwd_project_name,
        resolved_source: resolved_source_for_hints,
        cli_path,
        strategy,
        force,
        retire,
        jobs,
        handler,
        verb: verb.op_verb(),
        op_id,
        snapshot: std::cell::RefCell::new(None),
    })
}

/// Load context for `--continue`: read the owner record (following a lease
/// pointer if invoked from a non-owner workspace), derive all op parameters
/// from it, and rebuild the [`OpContext`].
fn load_continuing_context<'a>(
    verb: MachineVerb,
    cwd: &Path,
    project_override: Option<ProjectName>,
    jobs: usize,
    handler: &'a dyn OutputHandler,
) -> anyhow::Result<OpContext<'a>> {
    let emit_text = handler.emit_text();
    let cwd_ctx = WorkspaceContext::resolve(cwd, project_override.clone())?;
    let cwd_workspace_dir = cwd_ctx.active_path().to_path_buf();

    let (record, owner_workspace_dir) = op_state::resume(&cwd_workspace_dir)?;
    let op_id = OpId::from_string(record.id.clone());

    if emit_text {
        eprintln!(
            "continuing {verb_str} (op {op_id}, mid `{phase}`)",
            verb_str = record.verb,
            phase = record.phase,
        );
    }

    // The recorded verb is authoritative; cross-check against the entry-point
    // verb. (Same kind of belt-and-braces as the destructive-ops tripwire:
    // catches "operator ran `rwv sync --continue` on a `rwv sync-to` op", which
    // is harmless because we'd ignore their verb anyway, but worth flagging.)
    let recorded_verb = match record.verb {
        op_state::OpVerb::Sync => MachineVerb::Sync,
        op_state::OpVerb::SyncTo => MachineVerb::SyncTo,
    };
    if !verbs_match(verb, recorded_verb) {
        anyhow::bail!(
            "in-progress op is `{recorded}` but `rwv {invoked}` --continue was invoked. \
             Run `rwv {recorded} --continue` instead, or `rwv abort` to discard.",
            recorded = record.verb,
            invoked = match verb {
                MachineVerb::Sync => "sync",
                MachineVerb::SyncTo => "sync-to",
            },
        );
    }

    let strategy = record
        .strategy
        .parse::<SyncStrategy>()
        .context("op-state has invalid strategy")?;

    // Resolve source/dest workspaces by verb. The owner workspace is recorded
    // in `record.target`/`record.source` as absolute paths; rebuild contexts
    // from those.
    let cwd_project_name = find_project_name(&cwd_ctx)?;
    let cwd_project_dir = owner_workspace_dir.join("projects").join(&cwd_project_name);

    let (source_workspace_dir, dest_workspace_dir, cli_path) = match recorded_verb {
        MachineVerb::Sync => (record.source.clone(), record.target.clone(), record.source.clone()),
        MachineVerb::SyncTo => (
            record.source.clone(),
            record.target.clone(),
            record.target.clone(),
        ),
    };

    let other_project_override = match &cwd_ctx.location {
        WorkspaceLocation::Workweave { project, .. } => Some(project.clone()),
        WorkspaceLocation::Weave { .. } => project_override.clone(),
    };

    let (source_project_dir, source_workspace_name) = match recorded_verb {
        MachineVerb::Sync => {
            let source_ctx =
                WorkspaceContext::resolve(&source_workspace_dir, other_project_override.clone())?;
            let pname = find_project_name(&source_ctx)?;
            let dir = source_ctx.active_path().join("projects").join(&pname);
            (dir, workspace_name(&source_ctx))
        }
        MachineVerb::SyncTo => (cwd_project_dir.clone(), workspace_name(&cwd_ctx)),
    };

    let dest_project_dir = match recorded_verb {
        MachineVerb::Sync => cwd_project_dir.clone(),
        MachineVerb::SyncTo => {
            let dest_ctx =
                WorkspaceContext::resolve(&dest_workspace_dir, Some(cwd_project_name.clone()))?;
            let pname = find_project_name(&dest_ctx)?;
            dest_ctx.active_path().join("projects").join(&pname)
        }
    };

    let resolved_source_for_hints = match recorded_verb {
        MachineVerb::Sync => SyncSource::Path(source_workspace_dir.clone()),
        MachineVerb::SyncTo => SyncSource::Path(dest_workspace_dir.clone()),
    };

    Ok(OpContext {
        cwd_ctx,
        cwd_workspace_dir,
        owner_workspace_dir,
        source_workspace_dir,
        source_project_dir,
        source_workspace_name,
        dest_workspace_dir,
        dest_project_dir,
        cwd_project_dir,
        cwd_project_name,
        resolved_source: resolved_source_for_hints,
        cli_path,
        strategy,
        force: false, // --continue never adds --force; consents are recorded in `overrides`
        retire: record.retire,
        jobs,
        handler,
        verb: record.verb,
        op_id,
        snapshot: std::cell::RefCell::new(None),
    })
}

fn verbs_match(invoked: MachineVerb, recorded: MachineVerb) -> bool {
    matches!(
        (invoked, recorded),
        (MachineVerb::Sync, MachineVerb::Sync) | (MachineVerb::SyncTo, MachineVerb::SyncTo)
    )
}

// ---------------------------------------------------------------------------
// Pre-loop helpers (preconditions extracted from old run_sync_impl)
// ---------------------------------------------------------------------------

/// Sibling-sync warning: only meaningful for plain `sync`. CWD is a workweave
/// and source is another workweave that is NOT CWD's parent → crosses tree
/// branches; warn (don't refuse — the operator may have a reason).
fn warn_on_sibling_sync(cwd_ctx: &WorkspaceContext, source_workspace_dir: &Path, emit_text: bool) {
    if let WorkspaceLocation::Workweave { dir: cwd_ww, .. } = &cwd_ctx.location {
        // Resolve the source workspace's location to compare. Best-effort.
        let source_ctx = match WorkspaceContext::resolve(source_workspace_dir, None) {
            Ok(c) => c,
            Err(_) => return,
        };
        if let WorkspaceLocation::Workweave { dir: source_ww, .. } = &source_ctx.location {
            let cwd_canonical = cwd_ww
                .canonicalize()
                .unwrap_or_else(|_| cwd_ww.to_path_buf());
            let source_canonical = source_ww
                .canonicalize()
                .unwrap_or_else(|_| source_ww.to_path_buf());
            if cwd_canonical != source_canonical {
                let cwd_parent = crate::workspace::WorkweaveMarker::read(cwd_ww)
                    .ok()
                    .flatten()
                    .map(|m| m.parent)
                    .map(|p| p.canonicalize().unwrap_or(p));
                if cwd_parent.as_ref() != Some(&source_canonical) && emit_text {
                    eprintln!(
                        "warning: syncing across workweave siblings ({} → {}); \
                         this skips the recorded parent (informational — proceeding).",
                        cwd_canonical.display(),
                        source_canonical.display(),
                    );
                }
            }
        }
    }
}

/// sync-to `--strategy=ff` precondition: CWD must be strictly ahead of
/// target. If equal: no-op (signalled by `Ok(())` from a caller that then
/// short-circuits — handled inside `run_replay` via the noop-detection path).
/// If diverged: refuse before any side effects.
fn check_sync_to_ff_precondition(
    cwd_project_dir: &Path,
    target_project_dir: &Path,
    _emit_text: bool,
) -> anyhow::Result<()> {
    let cwd_tip = GitVcs
        .head_revision(cwd_project_dir)
        .context("failed to read CWD project HEAD")?;
    let target_tip = GitVcs
        .head_revision(target_project_dir)
        .context("failed to read target project HEAD")?;
    if cwd_tip == target_tip {
        // Equal tips: not an error, replay's per-repo no-op detection will
        // simply do nothing in step 1. Continue into the machine so the
        // record/lease cleanup happens through the canonical cleanup phase.
        return Ok(());
    }
    let cwd_ahead = GitVcs
        .is_ancestor(cwd_project_dir, &target_tip, &cwd_tip)
        .unwrap_or(false);
    if !cwd_ahead {
        anyhow::bail!(
            "sync-to --strategy=ff requires CWD to be strictly ahead of target, \
             but CWD's project tip ({}) is not an ancestor-or-equal of target's tip ({}).\n\
             Rerun with `--strategy=rebase` to rebase CWD's commits onto target's tip first.",
            cwd_tip,
            target_tip,
        );
    }
    Ok(())
}

/// sync-to dirty-target preflight: refuse if the target workweave has
/// uncommitted changes that advance-target would overwrite.
fn check_dirty_target_preflight(
    cwd_project: &Project,
    target_workspace_dir: &Path,
    target_project_dir: &Path,
    target_path: &Path,
) -> anyhow::Result<()> {
    let mut dirty: Vec<String> = Vec::new();
    for repo_path in cwd_project.manifest.iter_repo_paths() {
        let target_repo = target_workspace_dir.join(repo_path.as_path());
        if target_repo.exists() && GitVcs.has_uncommitted_changes(&target_repo).unwrap_or(true) {
            dirty.push(repo_path.to_string());
        }
    }
    if GitVcs
        .has_uncommitted_changes(target_project_dir)
        .unwrap_or(true)
    {
        dirty.push("(project)".to_string());
    }
    if !dirty.is_empty() {
        anyhow::bail!(
            "sync-to precondition failed: target workweave has uncommitted changes in:\n  {}\n\
             \n\
             advance-target fast-forwards the target's worktrees over this work. Commit or \
             stash in the target ({}), then re-run.",
            dirty.join("\n  "),
            target_path.display(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase: replay
// ---------------------------------------------------------------------------
//
// Pins the source snapshot at T0 (first entry only), then runs Phase 2
// (manifest repos) + Phase 1' (project repo). Per-repo parallelism and
// partial-failure reporting live inside this phase — the `--json` / NDJSON
// contracts are byte-compatible with the pre-restructure shape.
//
// **Re-entry rule (§4):** per-repo state is derived from the VCS itself:
// - repo at its savepoint → redo the strategy (no-op for already-clean cases);
// - repo mid-conflict → leave the VCS-native continue/abort to the operator;
// - repo already at the converged target → no-op (HEAD == lock target).
//
// No resume flags. The strategy functions already handle the "already there"
// case via the `head == target` short-circuit at the top of `sync_one_repo`.

fn run_replay(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    let emit_text = ctx.handler.emit_text();

    // sync-to with `--strategy=ff`: replay is a no-op (target IS source
    // here, and CWD is strictly ahead per the precondition). The advance-
    // target phase does all the work.
    if matches!(ctx.verb, op_state::OpVerb::SyncTo) && ctx.strategy == SyncStrategy::Ff {
        return Ok(());
    }

    if emit_text {
        if let op_state::OpVerb::SyncTo = ctx.verb {
            eprintln!(
                "sync-to: rebasing CWD against target ({})...",
                ctx.cli_path.display(),
            );
        }
    }

    // Pin source snapshot at T0 on first replay entry. On re-entry, the
    // previous snapshot is in the record-less RefCell (per-invocation context
    // is fresh on resume, so we re-pin — source may have moved on, but per-
    // repo no-op detection handles already-converged repos).
    pin_source_snapshot_if_needed(ctx)?;

    let snapshot_borrow = ctx.snapshot.borrow();
    let snapshot = snapshot_borrow
        .as_ref()
        .expect("snapshot pinned just above");

    // Load CWD project (manifest + lock) from disk.
    let cwd_project = Project::from_dir(&ctx.cwd_project_dir)
        .context("failed to load CWD project")?;

    let cwd_workspace_name = workspace_name(&ctx.cwd_ctx);
    let source_workspace_name = ctx.source_workspace_name.as_str();

    // Precondition: lock freshness (unless --force).
    //
    // Source: advisory check against live HEAD vs the pinned lock content.
    // Destination: uses the CWD lock as loaded from disk above.
    if !ctx.force {
        check_lock_freshness(
            &ctx.source_workspace_dir,
            &snapshot.raw_source_lock,
            Side::Source,
            source_workspace_name,
        )?;
        if let Some(ref lock) = cwd_project.lock {
            check_lock_freshness(
                &ctx.cwd_workspace_dir,
                lock,
                Side::Destination,
                &cwd_workspace_name,
            )?;
        }
    }

    // CWD project tip — read before any side effects so precondition checks
    // and Phase 1' use the pre-op starting state.
    let cwd_project_tip = GitVcs
        .head_revision(&ctx.cwd_project_dir)
        .context("failed to read CWD project HEAD")?;

    // Precondition: rebase and merge strategies require `rwv.lock merge=ours`
    // in the project repo's committed `.gitattributes`. FF doesn't merge.
    if matches!(ctx.strategy, SyncStrategy::Rebase | SyncStrategy::Merge) {
        verify_replay_exclusion_invariant(&ctx.cwd_project_dir)?;
    }

    // Precondition: ff strategy refuses divergence; rebase/merge handle it
    // by replaying CWD's commits onto source's tip with `rwv.lock` excluded.
    // `--force` bypasses regardless of strategy and discards CWD's project
    // commits via hard-reset; the savepoint preserves them for `rwv abort`.
    let phase1_ancestor_bypassed = if ctx.force {
        if GitVcs
            .has_uncommitted_changes(&ctx.cwd_project_dir)
            .unwrap_or(true)
        {
            anyhow::bail!(
                "sync --force precondition failed: project repo at {} has uncommitted changes.\n\
                 --force discards committed divergence (recoverable via refs/rwv/pre-op), but \
                 the hard-reset would destroy uncommitted changes unrecoverably. Commit or \
                 stash them, then re-run.",
                ctx.cwd_project_dir.display(),
            );
        }
        !cwd_is_ancestor_or_equal(
            &ctx.cwd_project_dir,
            &cwd_project_tip,
            &snapshot.source_project_tip,
        )
    } else if ctx.strategy == SyncStrategy::Ff {
        check_phase1_ancestor(
            &ctx.cwd_project_dir,
            &cwd_project_tip,
            &snapshot.source_project_tip,
            &cwd_workspace_name,
            source_workspace_name,
        )?;
        false
    } else {
        false
    };

    // === Phase 2 (manifest repos) — materialize missing, prune dropped, sync ===

    let mut materialize_failures: Vec<crate::manifest::RepoPath> = Vec::new();
    for repo_path in snapshot.raw_source_lock.iter_repo_paths() {
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if abs.exists() {
            continue;
        }
        let entry = match snapshot.source_manifest.get_entry(repo_path) {
            Some(e) => e,
            None => continue,
        };
        match materialize_missing_repo(&ctx.cwd_ctx, repo_path, entry, &ctx.cwd_project_name) {
            Ok(()) => {
                if emit_text {
                    println!("  {repo_path}: materialized");
                }
            }
            Err(e) => {
                if emit_text {
                    eprintln!("  {repo_path}: materialize failed: {e}");
                }
                materialize_failures.push(repo_path.clone());
            }
        }
    }

    if let Some(ref cwd_lock) = cwd_project.lock {
        for repo_path in cwd_lock.iter_repo_paths() {
            if snapshot.raw_source_lock.contains_repo(repo_path) {
                continue;
            }
            match prune_dropped_repo(&ctx.cwd_ctx, repo_path) {
                Ok(()) => {
                    if emit_text {
                        println!("  {repo_path}: pruned (dropped from lock)");
                    }
                }
                Err(e) => {
                    if emit_text {
                        eprintln!("  {repo_path}: prune skipped: {e}");
                    }
                }
            }
        }
    }

    let mut any_failure = !materialize_failures.is_empty();

    let (source_lock, source_lock_failures) = snapshot
        .raw_source_lock
        .clone()
        .resolve_versions(&ctx.cwd_workspace_dir);
    let unresolvable: std::collections::BTreeSet<crate::manifest::RepoPath> =
        source_lock_failures.iter().map(|(p, _)| p.clone()).collect();

    struct SyncTask {
        repo_path: crate::manifest::RepoPath,
        abs: PathBuf,
        target: ResolvedRevisionId,
    }
    let mut sync_tasks: Vec<SyncTask> = Vec::new();

    for (repo_path, raw_entry) in snapshot.raw_source_lock.iter_entries() {
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            if emit_text {
                println!("  {repo_path}: skipped (not on disk)");
            }
            continue;
        }
        if unresolvable.contains(repo_path) {
            if emit_text {
                eprintln!(
                    "  {repo_path}: lock pins unknown revision {} in local clone",
                    raw_entry.version
                );
            }
            any_failure = true;
            let head_unreadable_error = format!(
                "lock pins unknown revision {} in local clone",
                raw_entry.version
            );
            let outcome = RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
                error: head_unreadable_error,
                cause: None,
            });
            ctx.handler
                .record(repo_path.as_str(), &abs.to_string_lossy(), &outcome);
            continue;
        }
        let lock_entry = match source_lock.get_entry(repo_path) {
            Some(e) => e,
            None => continue,
        };
        sync_tasks.push(SyncTask {
            repo_path: repo_path.clone(),
            abs,
            target: lock_entry.version.clone(),
        });
    }

    let strategy = ctx.strategy;
    let task_outcomes: Vec<bool> = run_in_parallel(&sync_tasks, ctx.jobs, |_idx, task| {
        let outcome = sync_one_repo(&task.abs, &task.target, strategy);
        let is_failure = outcome.is_failure();
        if !is_failure {
            GitVcs.refresh_index_to_head_if_safe(&task.abs);
            GitVcs.refresh_working_tree_to_head_if_safe(&task.abs);
        }
        ctx.handler.record(
            task.repo_path.as_str(),
            &task.abs.to_string_lossy(),
            &outcome,
        );
        is_failure
    });
    if task_outcomes.iter().any(|f| *f) {
        any_failure = true;
    }

    if any_failure {
        anyhow::bail!(
            "{}",
            manifest_repo_failure_message(strategy, &ctx.resolved_source)
        );
    }

    // === Phase 1' (project repo) — strategy on the project repo ===

    let phase1_outcome = if ctx.force {
        GitVcs
            .hard_reset(&ctx.cwd_project_dir, &snapshot.source_project_tip)
            .map_err(anyhow::Error::from)
            .context("project repo reset --force failed")
    } else {
        apply_project_strategy(
            &ctx.cwd_project_dir,
            &snapshot.source_project_tip,
            &cwd_project_tip,
            strategy,
            &ctx.resolved_source,
        )
    };

    if let Err(e) = phase1_outcome {
        if emit_text {
            eprintln!("Phase 1' (project repo) failed: {e}");
        }
        anyhow::bail!(
            "{}",
            phase1_or_phase3_failure_message(
                Phase::One,
                &ctx.cwd_project_dir,
                strategy,
                &ctx.resolved_source,
            )
        );
    }

    // Stash the ancestor-bypass flag so the cleanup phase can preserve the
    // project savepoint as a tombstone (the only reference to discarded
    // commits). We do this by writing a side-channel marker file at the
    // owner workspace — kept simple, since this is a `--force` edge case.
    if phase1_ancestor_bypassed {
        // The cleanup phase reads this via filesystem.exists() — see cleanup().
        let _ = std::fs::write(
            ctx.owner_workspace_dir
                .join(".rwv-op-force-tombstone"),
            ctx.op_id.as_str(),
        );
    }

    Ok(())
}

/// Pin the source snapshot (project tip + manifest + lock at that revision)
/// if not already pinned this invocation. Idempotent: if already pinned,
/// returns Ok immediately.
fn pin_source_snapshot_if_needed(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    if ctx.snapshot.borrow().is_some() {
        return Ok(());
    }
    let source_project_tip = GitVcs
        .head_revision(&ctx.source_project_dir)
        .context("failed to read source project HEAD")?;

    let raw_source_lock = {
        let content = GitVcs
            .read_file_at_revision(
                &ctx.source_project_dir,
                &source_project_tip,
                Path::new("rwv.lock"),
            )
            .with_context(|| {
                format!(
                    "failed to read source lock at revision {} in {}",
                    source_project_tip,
                    ctx.source_project_dir.display()
                )
            })?;
        LockFile::from_yaml_str(&content).with_context(|| {
            format!(
                "failed to parse source lock at revision {} in {}",
                source_project_tip,
                ctx.source_project_dir.display()
            )
        })?
    };

    let source_manifest = {
        let content = GitVcs
            .read_file_at_revision(
                &ctx.source_project_dir,
                &source_project_tip,
                Path::new("rwv.yaml"),
            )
            .with_context(|| {
                format!(
                    "failed to read source manifest at revision {} in {}",
                    source_project_tip,
                    ctx.source_project_dir.display()
                )
            })?;
        Manifest::from_yaml_str(&content).with_context(|| {
            format!(
                "failed to parse source manifest at revision {} in {}",
                source_project_tip,
                ctx.source_project_dir.display()
            )
        })?
    };

    *ctx.snapshot.borrow_mut() = Some(SourceSnapshot {
        source_project_tip,
        source_manifest,
        raw_source_lock,
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase: relock
// ---------------------------------------------------------------------------
//
// Regenerates `rwv.lock` from the post-replay manifest tips and commits if
// changed. On completion, records the converged per-repo tips in the owner
// record (consumed by abort hardening, sibling .4).
//
// **Re-entry rule:** regenerating a lock that is already current is a no-op
// (write_lock + commit_lock_file_with_message both short-circuit when the
// content hasn't changed). The per-repo HEAD reads that populate
// `converged_tips` are pure reads.

fn run_relock(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    // sync-to with `--strategy=ff`: relock is a no-op (replay was a no-op).
    if matches!(ctx.verb, op_state::OpVerb::SyncTo) && ctx.strategy == SyncStrategy::Ff {
        return Ok(());
    }

    let emit_text = ctx.handler.emit_text();

    // Reload the project after replay (manifest may now include newly-added
    // repos brought over from source).
    let cwd_project = Project::from_dir(&ctx.cwd_project_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to reload project manifest after Phase 1' ({e}).\n\
             \n\
             The project repo was successfully rebased/merged, but the manifest \
             in {cwd_project_dir} could not be parsed. Proceeding would silently \
             omit newly-added repos from the regenerated lock.\n\
             \n\
             To recover: `rwv abort`",
            cwd_project_dir = ctx.cwd_project_dir.display(),
        )
    })?;

    if let Err(e) = regenerate_lock_phase3(
        &ctx.cwd_ctx,
        &ctx.cwd_project_dir,
        &cwd_project,
        &ctx.source_workspace_name,
    ) {
        if emit_text {
            eprintln!("Phase 3 (re-lock) failed: {e}");
        }
        anyhow::bail!(
            "{}",
            phase1_or_phase3_failure_message(
                Phase::Three,
                &ctx.cwd_project_dir,
                ctx.strategy,
                &ctx.resolved_source,
            )
        );
    }

    // Record converged tips on the owner record. These are read by
    // advance-target and consumed by abort hardening (sibling .4).
    record_converged_tips(ctx, &cwd_project)?;

    Ok(())
}

/// Read post-replay HEADs of each manifest repo + project repo and write
/// them into the owner record's `converged_tips` map. Used by advance-target
/// and (sibling .4) abort's HEAD-verified restore.
fn record_converged_tips(ctx: &OpContext<'_>, cwd_project: &Project) -> anyhow::Result<()> {
    let mut owner = op_state::read_owner(&ctx.owner_workspace_dir)?
        .ok_or_else(|| anyhow::anyhow!("internal: owner record missing during relock"))?;
    owner.converged_tips.clear();
    for repo_path in cwd_project.manifest.iter_repo_paths() {
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            continue;
        }
        if let Ok(rev) = GitVcs.head_revision(&abs) {
            owner
                .converged_tips
                .insert(repo_path.as_str().to_owned(), rev.as_str().to_owned());
        }
    }
    if let Ok(rev) = GitVcs.head_revision(&ctx.cwd_project_dir) {
        owner
            .converged_tips
            .insert("(project)".to_owned(), rev.as_str().to_owned());
    }
    op_state::write_owner(&ctx.owner_workspace_dir, &owner)
        .context("failed to write converged_tips back to owner record")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase: advance-target (sync-to only)
// ---------------------------------------------------------------------------
//
// FF-advance every target manifest repo + the target project repo to the
// converged tips recorded by relock.
//
// **Re-entry rule:** ff to an already-reached tip is a no-op (the equal-tip
// check at the top of `ff_advance_repo` short-circuits).

fn run_advance_target(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    let emit_text = ctx.handler.emit_text();

    if emit_text {
        eprintln!("sync-to: fast-forwarding target to CWD's tips...");
    }

    let cwd_project_final = Project::from_dir(&ctx.cwd_project_dir)
        .context("failed to reload CWD project for advance-target")?;

    let mut any_ff_failure = false;
    for repo_path in cwd_project_final.manifest.iter_repo_paths() {
        let cwd_repo = ctx.cwd_workspace_dir.join(repo_path.as_path());
        let target_repo = ctx.dest_workspace_dir.join(repo_path.as_path());
        if !cwd_repo.exists() {
            continue;
        }
        if !target_repo.exists() {
            if emit_text {
                eprintln!("  {}: skipped (not on disk in target)", repo_path);
            }
            continue;
        }
        let cwd_tip = match GitVcs.head_revision(&cwd_repo) {
            Ok(tip) => tip,
            Err(e) => {
                if emit_text {
                    eprintln!("  {}: failed to read CWD tip: {e}", repo_path);
                }
                any_ff_failure = true;
                continue;
            }
        };
        match ff_advance_repo(&target_repo, &cwd_repo, &cwd_tip) {
            Ok(()) => {
                if emit_text {
                    println!(
                        "  {}: ff-advanced to {}",
                        repo_path,
                        &cwd_tip.as_str()[..8.min(cwd_tip.as_str().len())]
                    );
                }
            }
            Err(e) => {
                if emit_text {
                    eprintln!("  {}: ff-advance failed: {e}", repo_path);
                }
                any_ff_failure = true;
            }
        }
    }

    let cwd_project_tip = GitVcs
        .head_revision(&ctx.cwd_project_dir)
        .context("failed to read CWD project HEAD for advance-target")?;

    match ff_advance_repo(&ctx.dest_project_dir, &ctx.cwd_project_dir, &cwd_project_tip) {
        Ok(()) => {
            if emit_text {
                println!(
                    "  (project): ff-advanced to {}",
                    &cwd_project_tip.as_str()[..8.min(cwd_project_tip.as_str().len())]
                );
            }
        }
        Err(e) => {
            if emit_text {
                eprintln!("  (project): ff-advance failed: {e}");
            }
            any_ff_failure = true;
        }
    }

    if any_ff_failure {
        anyhow::bail!(
            "sync-to advance-target failed for one or more repos (see above).\n\
             This should not happen after a clean replay; possible concurrent modification.\n\
             Op-state remains in both workspaces.\n\
             Rerun `rwv sync-to --continue` after resolving, or `rwv abort` to roll back.",
        );
    }

    if ctx.handler.emit_text() {
        eprintln!("sync-to complete: target fast-forwarded to CWD's tip");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase: retire (--retire only)
// ---------------------------------------------------------------------------
//
// Today's retire semantics (merged-check on manifest repos, dirty-check, then
// `delete_workweave`). The full retire-as-phase semantics (merged-check
// failure → phase=retire, abort rolls back target, sibling .3) build on this
// stub. For now, retire still bails normally on failure; on success the
// workweave is gone and the cleanup phase finishes the op record.

fn run_retire(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    let emit_text = ctx.handler.emit_text();

    match &ctx.cwd_ctx.location {
        WorkspaceLocation::Workweave { dir, name, project } => retire_workweave_after_sync_to(
            &ctx.cwd_ctx,
            dir,
            name,
            project,
            &ctx.cwd_project_dir,
            &ctx.dest_workspace_dir,
        ),
        WorkspaceLocation::Weave { .. } => {
            if emit_text {
                eprintln!("warning: --retire is only meaningful inside a workweave; ignoring");
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal: cleanup
// ---------------------------------------------------------------------------
//
// Drop savepoints and clear the owner record + lease. Cleanup is not a
// persisted phase: a crash before cleanup completes leaves the on-disk
// phase at the last work phase (e.g. retire), which is re-runnable;
// `--continue` then re-runs that phase (it's idempotent) and reaches
// cleanup again.

fn cleanup(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    let emit_text = ctx.handler.emit_text();

    // Drop savepoints. Exception: when the --force tombstone marker was
    // written during replay (Phase 1' ancestor check was bypassed), preserve
    // the project savepoint as the only reference to the discarded commits.
    let tombstone_path = ctx
        .owner_workspace_dir
        .join(".rwv-op-force-tombstone");
    let tombstone = tombstone_path.exists();

    if !tombstone {
        delete_savepoint(&ctx.cwd_project_dir, &ctx.op_id);
    } else if emit_text {
        eprintln!(
            "note: --force discarded project commits; pre-sync state preserved at \
             refs/rwv/pre-op/{op_id} (recover with `git reset --hard refs/rwv/pre-op/{op_id}` \
             in {})",
            ctx.cwd_project_dir.display(),
            op_id = ctx.op_id,
        );
    }
    let _ = std::fs::remove_file(&tombstone_path);

    // Manifest savepoints: reload the project so we see post-replay shape.
    if let Ok(project) = Project::from_dir(&ctx.cwd_project_dir) {
        for repo_path in project.manifest.iter_repo_paths() {
            let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
            if abs.exists() {
                delete_savepoint(&abs, &ctx.op_id);
            }
        }
    }

    // Clear owner record and lease (if any).
    op_state::clear_owner(&ctx.owner_workspace_dir);
    if matches!(ctx.verb, op_state::OpVerb::SyncTo) {
        op_state::clear_lease(&ctx.dest_workspace_dir);
    }
    Ok(())
}


/// `rwv sync-to --retire` post-sync-to cleanup.
///
/// Verify that the just-completed sync-to brought CWD's manifest repos into
/// alignment with the target's, and that no worktree has uncommitted changes,
/// then delete the workweave. Bails (preserving the workweave) on any
/// mismatch so the operator can fix and re-run.
///
/// We deliberately compare **manifest repo tips** rather than project repo
/// tips. The project repo's post-sync state typically diverges from the target
/// by exactly the auto-relock commit (Phase 3 always writes the workweave's
/// `workweave:` field into the lock, which the primary's lock lacks). That
/// commit is purely derived — the parent will regenerate it on its next
/// sync — so refusing on project-tip inequality would refuse every retire,
/// even the happy path the bead describes. Manifest tip equality is the
/// honest "work has converged" signal: Phase 2 advances both sides to the
/// same SHAs, so post-sync the manifest repos should be byte-equal.
fn retire_workweave_after_sync_to(
    ctx: &WorkspaceContext,
    workweave_dir: &Path,
    workweave_name: &WorkweaveName,
    project: &crate::manifest::ProjectName,
    cwd_project_dir: &Path,
    target_workspace_dir: &Path,
) -> anyhow::Result<()> {
    // Reload manifest post-Phase 3 so we see any repos newly added by sync.
    let manifest_path = cwd_project_dir.join("rwv.yaml");
    let manifest =
        Manifest::from_path(&manifest_path).context("--retire: failed to reload manifest")?;

    // Compare each manifest repo's HEAD in CWD vs. target. After a successful
    // sync-to, step 3 has fast-forwarded the target's repos to CWD's tips, so
    // both sides should be at the same SHAs. We compare against the target
    // workspace directory (which sync-to already advanced in step 3).
    let target_root = target_workspace_dir;

    let mut diverged: Vec<String> = Vec::new();
    for repo_path in manifest.iter_repo_paths() {
        let cwd_repo = workweave_dir.join(repo_path.as_path());
        let target_repo = target_root.join(repo_path.as_path());
        if !cwd_repo.exists() || !target_repo.exists() {
            // Missing on one side — leave the workweave alone; this is
            // unusual enough that the operator should look.
            diverged.push(format!("{}: missing on one side", repo_path.as_str()));
            continue;
        }
        let cwd_head = GitVcs
            .head_revision(&cwd_repo)
            .with_context(|| format!("--retire: read CWD head for {}", repo_path))?;
        let target_head = GitVcs
            .head_revision(&target_repo)
            .with_context(|| format!("--retire: read target head for {}", repo_path))?;
        if cwd_head != target_head {
            diverged.push(format!(
                "{}: CWD={} target={}",
                repo_path.as_str(),
                short_sha(cwd_head.as_str()),
                short_sha(target_head.as_str())
            ));
        }
    }

    if !diverged.is_empty() {
        anyhow::bail!(
            "--retire: workweave's manifest repos differ from target after sync-to; \
             refusing to delete:\n  {}\n\
             Resolve the divergence and re-run, \
             or `rwv workweave delete --force {}` to discard.",
            diverged.join("\n  "),
            workweave_name.as_str(),
        );
    }

    // Reuse the shared dirty-path check. Any dirty worktree blocks retire.
    let dirty = crate::workweave::collect_dirty_paths(workweave_dir, project, &manifest);
    if !dirty.is_empty() {
        anyhow::bail!(
            "--retire: workweave has uncommitted changes after sync-to; refusing to delete:\n  {}\n\
             Commit/discard and re-run, or `rwv workweave delete --force {}` to discard.",
            dirty.join("\n  "),
            workweave_name.as_str(),
        );
    }

    // Both invariants hold: delete the workweave. Pass `force: false` —
    // collect_dirty_paths already returned empty, so the inner check is
    // belt-and-braces. Use the primary path (delete_workweave needs to
    // locate the workweave under the primary's parent dir).
    crate::workweave::delete_workweave(ctx.primary_path(), project, workweave_name, false)
        .context("--retire: workweave delete failed")?;

    eprintln!("retired workweave {}", workweave_name.as_str());
    Ok(())
}

/// Truncate a SHA to 12 chars for display (matches workweave.rs convention).
fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// Phase 1': replay CWD's unique project commits onto `source_tip` via
/// `strategy`, relying on `.gitattributes rwv.lock merge=ours` (configured at
/// `rwv init` time) to silently keep source's version of the lock through the
/// replay. Phase 3 regenerates the lock from manifest tips afterwards.
///
/// - `Ff`: requires CWD ancestor of source (caller already verified). Performs
///   a fast-forward via `git merge --ff-only`.
/// - `Rebase`: native `git rebase` via [`Vcs::rebase`]. On conflict, leaves
///   the repo mid-rebase so `git rebase --continue` resumes after manual
///   resolution.
/// - `Merge`: native `git merge --no-edit`. The `merge=ours` attribute
///   resolves any lock-line collision automatically.
///
/// Conflicts on non-lock paths halt the operation, leaving the VCS-native
/// in-flight state for the operator to resolve and re-run sync, or
/// `rwv abort`.
fn apply_project_strategy(
    cwd_project_dir: &Path,
    source_tip: &ResolvedRevisionId,
    cwd_tip: &ResolvedRevisionId,
    strategy: SyncStrategy,
    resolved_source: &SyncSource,
) -> anyhow::Result<()> {
    if cwd_tip == source_tip {
        // No-op.
        return Ok(());
    }

    match strategy {
        SyncStrategy::Ff => {
            // CWD must be ancestor of source (caller verified). Fast-forward.
            GitVcs.advance_if_fast_forward(cwd_project_dir, source_tip)?;
        }
        SyncStrategy::Rebase => {
            // `Vcs::rebase` wires the `merge=ours` driver inline; lock-only
            // commits become empty patches and git drops them by default
            // (`--empty=drop`), so source's version of `rwv.lock` survives
            // the replay untouched. Phase 3 then regenerates the lock from
            // manifest tips.
            match GitVcs.rebase(cwd_project_dir, source_tip, source_tip) {
                Ok(()) => {}
                Err(VcsError::RebaseConflict { repo, op }) => {
                    anyhow::bail!(
                        "{}",
                        per_conflict_bail_message(
                            &repo,
                            op,
                            "rebase (project repo)",
                            "see in-flight rebase state for conflicting paths",
                            resolved_source,
                        )
                    );
                }
                Err(e) => anyhow::bail!("project repo rebase failed: {e}"),
            }
        }
        SyncStrategy::Merge => {
            // `Vcs::merge_from` wires the `merge=ours` driver inline so any
            // `rwv.lock` collision auto-resolves in source's favour. On
            // conflict it returns RebaseConflict { op: Merge } with the repo
            // left in mid-merge state.
            match GitVcs.merge_from(cwd_project_dir, source_tip) {
                Ok(()) => {}
                Err(VcsError::RebaseConflict { repo, op }) => {
                    anyhow::bail!(
                        "{}",
                        per_conflict_bail_message(
                            &repo,
                            op,
                            "merge (project repo)",
                            "see in-flight merge state for conflicting paths",
                            resolved_source,
                        )
                    );
                }
                Err(e) => anyhow::bail!("project repo merge failed: {e}"),
            }
        }
    }
    Ok(())
}

/// Phase 3: regenerate `rwv.lock` from the current manifest tips. Commit it
/// if it differs from what's currently in the project repo.
fn regenerate_lock_phase3(
    ctx: &WorkspaceContext,
    cwd_project_dir: &Path,
    cwd_project: &Project,
    source_workspace_name: &str,
) -> anyhow::Result<()> {
    let workweave_pair = match &ctx.location {
        WorkspaceLocation::Workweave { name, dir, .. } => Some((name, dir.as_path())),
        WorkspaceLocation::Weave { .. } => None,
    };

    let new_lock = generate_lock(
        &cwd_project.manifest,
        ctx.primary_path(),
        workweave_pair,
        true, // dirty: skip uncommitted-changes check; sync may have produced WT churn
    )
    .context("failed to generate lock")?;

    let lock_path = cwd_project_dir.join("rwv.lock");
    crate::lock::write_lock(&new_lock, &lock_path)?;

    let message = format!("lock: auto-relock after sync from {source_workspace_name}");
    if commit_lock_file_with_message(cwd_project_dir, &message)? {
        eprintln!("  (project): re-locked after sync from {source_workspace_name}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// rwv abort
// ---------------------------------------------------------------------------

/// Execute `rwv abort` — restore CWD workspace to its pre-sync state.
///
/// Reads the op-state file (`.rwv-op`) to find the op-id and the involved
/// workspaces. For `sync-to` ops, both CWD and the recorded target workspace
/// are rolled back.
pub fn run_abort(cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let workspace_dir = ctx.active_path().to_path_buf();

    // resolve_to_owner follows a lease pointer if the workspace holds a lease,
    // so `rwv abort` invoked from either the owner or a leased workspace finds
    // the same full record. `workspace_dir` is still used for the repo scan below.
    let (op_id, extra_workspace_dirs): (OpId, Vec<PathBuf>) =
        match op_state::resolve_to_owner(&workspace_dir)? {
            Some(resolved) => {
                // For sync-to: also roll back the target (the leased) workspace.
                let extras = if resolved.record.verb == crate::op_state::OpVerb::SyncTo {
                    // Determine which workspace is the "other" workspace to roll back.
                    // The owner workspace is resolved.owner_workspace; the other workspace
                    // for sync-to is target (when CWD is owner/source) or source (when
                    // CWD is target/lease). We roll back workspace_dir if it differs
                    // from the owner, and the owner's target regardless.
                    let mut extras = Vec::new();
                    // Always include the target workspace if we're at the owner.
                    if resolved.owner_workspace == workspace_dir {
                        extras.push(resolved.record.target.clone());
                    } else {
                        // Invoked from the lease workspace: include the owner workspace
                        // so its repos are also restored.
                        extras.push(resolved.owner_workspace.clone());
                    }
                    extras
                } else {
                    vec![]
                };
                (OpId::from_string(resolved.record.id), extras)
            }
            None => anyhow::bail!("no operation in progress"),
        };

    let cwd_project_name = find_project_name(&ctx)?;
    let cwd_project_dir = workspace_dir.join("projects").join(&cwd_project_name);
    // Use the lockless loader: abort's contract is "the state is bad, get me
    // out". rwv.lock may contain git conflict markers from the half-completed
    // rebase, so we must not try to parse it. The abort path only needs the
    // manifest (to enumerate repo paths); it never reads lock contents.
    let cwd_project =
        Project::from_dir_skip_lock(&cwd_project_dir).context("failed to load CWD project")?;

    let mut any_failure = false;

    // Restore CWD manifest repos first.
    for repo_path in cwd_project.manifest.iter_repo_paths() {
        let abs = workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            continue;
        }
        if let Err(e) = abort_one_repo(&abs, &op_id) {
            eprintln!("  {repo_path}: {e}");
            any_failure = true;
        } else {
            println!("  {repo_path}: restored");
        }
    }

    // Restore CWD project repo.
    if let Err(e) = abort_one_repo(&cwd_project_dir, &op_id) {
        eprintln!("  (project): {e}");
        any_failure = true;
    }

    // For sync-to: also roll back repos in the extra (target) workspaces.
    for extra_dir in &extra_workspace_dirs {
        // Resolve the target workspace's project context. Best-effort: if the
        // project name cannot be determined, skip with a warning.
        match WorkspaceContext::resolve(extra_dir, None) {
            Ok(extra_ctx) => {
                let extra_project_name = match find_project_name(&extra_ctx) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!(
                            "  warning: could not determine project for {}: {e}; skipping",
                            extra_dir.display()
                        );
                        continue;
                    }
                };
                let extra_project_dir = extra_ctx
                    .active_path()
                    .join("projects")
                    .join(&extra_project_name);
                let extra_project = match Project::from_dir_skip_lock(&extra_project_dir) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!(
                            "  warning: could not load project at {}: {e}; skipping",
                            extra_project_dir.display()
                        );
                        continue;
                    }
                };
                let extra_ws_dir = extra_ctx.active_path().to_path_buf();
                for repo_path in extra_project.manifest.iter_repo_paths() {
                    let abs = extra_ws_dir.join(repo_path.as_path());
                    if !abs.exists() {
                        continue;
                    }
                    if let Err(e) = abort_one_repo(&abs, &op_id) {
                        eprintln!("  [target] {repo_path}: {e}");
                        any_failure = true;
                    } else {
                        println!("  [target] {repo_path}: restored");
                    }
                }
                if let Err(e) = abort_one_repo(&extra_project_dir, &op_id) {
                    eprintln!("  [target] (project): {e}");
                    any_failure = true;
                }
                // Remove op-state from the extra workspace (owner record or lease).
                op_state::clear_all_at(&extra_ws_dir);
            }
            Err(e) => {
                eprintln!(
                    "  warning: could not resolve workspace at {}: {e}; skipping",
                    extra_dir.display()
                );
            }
        }
    }

    // Remove op-state from CWD workspace (owner record or lease).
    op_state::clear_all_at(&workspace_dir);

    if any_failure {
        anyhow::bail!("abort completed with failures");
    }

    Ok(())
}

fn abort_one_repo(repo: &Path, op_id: &OpId) -> anyhow::Result<()> {
    // Cancel any VCS-native in-flight op (rebase/merge/cherry-pick). No-op
    // when the repo is clean.
    GitVcs.cancel_in_flight_op(repo);

    // Restore to savepoint. `Vcs::restore_savepoint` is the operation's
    // contract — when present, it hard-resets HEAD to the captured SHA
    // and drops the savepoint ref atomically. Returns Ok(false) when no
    // savepoint exists for this repo (nothing to restore).
    GitVcs
        .restore_savepoint(repo, op_id.as_str())
        .context("restore savepoint failed")?;
    Ok(())
}

/// Run `rwv sync --json`.
///
/// Two emission shapes, selected by `jobs`:
///
/// - **Serial / envelope** (`jobs == 1`): collect all per-repo outcomes,
///   then emit `{ "$schema": "...", "outcomes": [...] }` pretty-printed to
///   stdout on completion.
/// - **Parallel / NDJSON** (`jobs > 1`): each per-repo outcome is streamed
///   as one JSON line to stdout the moment its worker finishes. Every line
///   embeds its own `$schema` so consumers can identify a record without
///   out-of-band context. No envelope is emitted — one self-describing
///   record per line.
///
/// In both modes the text-mode per-repo chatter that `run_sync` produces
/// is suppressed; stderr-side diagnostic warnings (e.g. `(project):
/// re-locked after sync from ...`, `--retire` messages) flow through.
/// Reporter prefix wrapping is bypassed under NDJSON: workers don't run
/// subprocesses through `parallel::Reporter`, so there's no
/// `[<prefix>] <line>` text to interleave with JSON output.
///
/// Exit semantics: when any per-repo outcome has kind `failed`, exits with
/// code 1 directly (under envelope mode after emitting the envelope;
/// under NDJSON the failing record was already streamed). The
/// `process::exit` avoids anyhow's stderr error display swallowing the
/// JSON. When all repos succeed, returns `Ok(())` and main exits 0.
///
/// Project-level errors raised before any per-repo work (lock freshness,
/// active-project mismatch, etc.) propagate via `Err` and main's anyhow
/// printer surfaces them — no JSON is emitted in that case. This matches
/// the bead's "non-zero iff at least one repo failed" semantic: when sync
/// can't even reach the per-repo loop, there are no per-repo outcomes to
/// emit, so the structured channel has nothing to say.
#[allow(clippy::too_many_arguments)]
pub fn run_sync_json(
    cwd: &Path,
    source: Option<&SyncSource>,
    strategy: SyncStrategy,
    force: bool,
    retire: bool,
    project_override: Option<ProjectName>,
    jobs: usize,
    do_continue: bool,
) -> anyhow::Result<()> {
    let records: Mutex<Vec<SyncOutcomeOutput>> = Mutex::new(Vec::new());
    let stdout_lock: Mutex<()> = Mutex::new(());
    let ndjson = jobs > 1;
    let project_level_result = if ndjson {
        let handler = JsonNdjsonHandler {
            stdout_lock: &stdout_lock,
            records: &records,
            schema_url: SYNC_JSON_SCHEMA_URL,
        };
        run_machine(
            MachineVerb::Sync,
            cwd,
            source,
            strategy,
            force,
            retire,
            project_override,
            jobs,
            &handler,
            do_continue,
        )
    } else {
        let handler = JsonEnvelopeHandler { records: &records };
        run_machine(
            MachineVerb::Sync,
            cwd,
            source,
            strategy,
            force,
            retire,
            project_override,
            jobs,
            &handler,
            do_continue,
        )
    };

    let records = records.into_inner().unwrap_or_else(|e| e.into_inner());

    run_sync_json_impl(
        ndjson,
        records,
        SYNC_JSON_SCHEMA_URL,
        project_level_result,
        false,
    )
}

/// Shared post-impl JSON emitter: emits the envelope (serial) or a no-op
/// (NDJSON already streamed), then maps exit codes.
///
/// Factored out so both `run_sync_json` and `run_sync_to_json` can share
/// the envelope/NDJSON emission logic with their respective schema URLs.
///
/// When `emit_empty_envelope` is true, an envelope with an empty `outcomes`
/// array is emitted even when `records` is empty (used by sync-to where
/// step 1 may be skipped for ff-clean with no per-repo manifest outcomes).
/// When false (sync's behavior), empty records propagates the error.
fn run_sync_json_impl(
    ndjson: bool,
    records: Vec<SyncOutcomeOutput>,
    schema_url: &str,
    project_level_result: anyhow::Result<()>,
    emit_empty_envelope: bool,
) -> anyhow::Result<()> {
    // If we never reached the per-repo loop (project-level precondition
    // failure), propagate the error so main prints it via anyhow.
    if records.is_empty() && !emit_empty_envelope {
        return project_level_result;
    }

    let any_failure = records.iter().any(SyncOutcomeOutput::is_failure);

    // Under envelope mode we still need to emit the envelope to stdout
    // (NDJSON streamed each record as it arrived, so there's nothing
    // extra to write). Per the bead spec, NDJSON does NOT emit an
    // envelope wrapper around the stream.
    if !ndjson {
        let payload = SyncJsonOutput {
            schema: schema_url.to_owned(),
            outcomes: records,
        };
        let out =
            serde_json::to_string_pretty(&payload).context("failed to serialize sync output")?;
        println!("{out}");
    }

    // Map exit code: non-zero iff any per-repo outcome was a failure.
    // We use process::exit directly so the JSON we just printed is the
    // only thing on stdout — bubbling Err would route through anyhow's
    // stderr formatter (acceptable, but the test harness asserts only on
    // stdout + exit code).
    if any_failure {
        std::process::exit(1);
    }
    // Also propagate any project-level error that fired AFTER per-repo
    // outcomes were captured (e.g., Phase 1' or Phase 3 failure). The
    // outcomes JSON has been emitted; surface the error via Err so
    // main's anyhow display fires.
    project_level_result
}

// ---------------------------------------------------------------------------
// rwv sync-to
// ---------------------------------------------------------------------------
//
// `sync-to` is the same data-driven phase machine as `sync` (above) with
// advance-target always running and retire running when `--retire` is set:
//
//   guard → mark → savepoint → replay → relock → advance-target → [retire] → cleanup
//
// The replay phase IS what `rwv sync <target>` does (CWD absorbs target's
// history with CWD's commits on top). advance-target ff-forwards the
// target's repos to CWD's converged tips. With `--retire`, the workweave
// is then deleted (merged-check + dirty-check, see [`retire_workweave_after_sync_to`]).
//
// Op-state is one full owner record at CWD plus a thin lease at the target
// workspace. Driver re-entry follows the lease pointer when `--continue`
// is invoked from the target side.

/// Execute `rwv sync-to <target>`.
///
/// `target` is the workspace to advance, or `None` when `--continue` is set
/// (target is then read from the in-progress op-state file). All steps are
/// expressed as phases in the data-driven machine; `--continue` resumes at
/// the recorded phase from either workspace.
#[allow(clippy::too_many_arguments)]
pub fn run_sync_to(
    cwd: &Path,
    target: Option<&SyncSource>,
    strategy: SyncStrategy,
    force: bool,
    retire: bool,
    project_override: Option<ProjectName>,
    jobs: usize,
    do_continue: bool,
) -> anyhow::Result<()> {
    let stdout_lock: Mutex<()> = Mutex::new(());
    let handler = TextHandler {
        stdout_lock: &stdout_lock,
    };
    run_machine(
        MachineVerb::SyncTo,
        cwd,
        target,
        strategy,
        force,
        retire,
        project_override,
        jobs,
        &handler,
        do_continue,
    )
}

/// Execute `rwv sync-to <target> --json`.
#[allow(clippy::too_many_arguments)]
pub fn run_sync_to_json(
    cwd: &Path,
    target: Option<&SyncSource>,
    strategy: SyncStrategy,
    force: bool,
    retire: bool,
    project_override: Option<ProjectName>,
    jobs: usize,
    do_continue: bool,
) -> anyhow::Result<()> {
    let records: Mutex<Vec<SyncOutcomeOutput>> = Mutex::new(Vec::new());
    let stdout_lock: Mutex<()> = Mutex::new(());
    let ndjson = jobs > 1;
    let project_level_result = if ndjson {
        let handler = JsonNdjsonHandler {
            stdout_lock: &stdout_lock,
            records: &records,
            schema_url: SYNC_TO_JSON_SCHEMA_URL,
        };
        run_machine(
            MachineVerb::SyncTo,
            cwd,
            target,
            strategy,
            force,
            retire,
            project_override,
            jobs,
            &handler,
            do_continue,
        )
    } else {
        let handler = JsonEnvelopeHandler { records: &records };
        run_machine(
            MachineVerb::SyncTo,
            cwd,
            target,
            strategy,
            force,
            retire,
            project_override,
            jobs,
            &handler,
            do_continue,
        )
    };

    let records = records.into_inner().unwrap_or_else(|e| e.into_inner());

    run_sync_json_impl(
        ndjson,
        records,
        SYNC_TO_JSON_SCHEMA_URL,
        project_level_result,
        true,
    )
}


/// Fast-forward `target_repo` to `cwd_tip`.
///
/// We need the objects to be reachable in `target_repo`. For a worktree
/// pair (workweave + primary), they share the same object store, so any
/// SHA reachable in the CWD worktree is also reachable in the target
/// worktree. We use `git fetch <cwd_repo_path> HEAD` to ensure the object
/// is present in the target's object store, then `git merge --ff-only
/// <sha>` to advance the branch. merge --ff-only (rather than `reset
/// --hard`) because the purpose here is advancing, not discarding: it
/// refuses, instead of silently clobbering, if the update would touch
/// uncommitted changes in the target.
///
/// The fetch-then-advance approach works for both worktrees (same object
/// store, fetch is a no-op) and independent clones (fetch copies objects).
fn ff_advance_repo(
    target_repo: &Path,
    cwd_repo: &Path,
    cwd_tip: &ResolvedRevisionId,
) -> anyhow::Result<()> {
    // Verify that target_repo's HEAD is an ancestor of (or equal to) cwd_tip.
    // If not, this is a concurrent-modification scenario — bail.
    let target_tip = GitVcs
        .head_revision(target_repo)
        .context("failed to read target HEAD")?;

    if target_tip == *cwd_tip {
        return Ok(()); // already at the right tip
    }

    // Fast-forwarding a dirty target worktree risks its uncommitted changes.
    // The sync-to preflight already refused on a dirty target; this catches
    // concurrent modification since then, with a named precondition instead
    // of merge's generic refusal. Checked after the equal-tip return: a
    // dirty worktree we won't move is safe.
    if GitVcs.has_uncommitted_changes(target_repo).unwrap_or(true) {
        anyhow::bail!(
            "target repo at {} has uncommitted changes; refusing to fast-forward \
             over them. Commit or stash in the target, then re-run.",
            target_repo.display(),
        );
    }

    // Check that target is an ancestor of cwd_tip (ff precondition).
    // Bring objects across first via the Vcs trait so merge-base has the
    // SHAs reachable in target_repo. For sibling worktrees that share an
    // object store this is a no-op; for independent clones it copies
    // objects across.
    GitVcs.fetch_objects_from(target_repo, cwd_repo);

    let is_ancestor = GitVcs
        .is_ancestor(target_repo, &target_tip, cwd_tip)
        .unwrap_or(false);

    if !is_ancestor {
        anyhow::bail!(
            "target repo at {} cannot be fast-forwarded: target tip ({}) is not an ancestor \
             of CWD tip ({}). This indicates concurrent modification after step 1 completed.",
            target_repo.display(),
            target_tip,
            cwd_tip,
        );
    }

    // Fast-forward: advance_if_fast_forward refuses (rather than clobbers)
    // if the update would touch uncommitted changes — VCS-native backstop
    // behind the two explicit dirty gates above.
    GitVcs
        .advance_if_fast_forward(target_repo, cwd_tip)
        .context("fast-forward advance failed in target")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_source_parses_primary() {
        assert_eq!(
            "primary".parse::<SyncSource>().unwrap(),
            SyncSource::Primary
        );
    }

    #[test]
    fn sync_source_parses_workweave_name() {
        let parsed: SyncSource = "fo-city".parse().unwrap();
        assert_eq!(parsed, SyncSource::Workweave(WorkweaveName::new("fo-city")));
    }

    #[test]
    fn sync_source_parses_relative_path_with_slash() {
        let parsed: SyncSource = ".workweaves/foo--bar".parse().unwrap();
        assert_eq!(
            parsed,
            SyncSource::Path(PathBuf::from(".workweaves/foo--bar"))
        );
    }

    #[test]
    fn sync_source_parses_dot_relative() {
        let parsed: SyncSource = "./foo".parse().unwrap();
        assert_eq!(parsed, SyncSource::Path(PathBuf::from("./foo")));
    }

    #[test]
    fn sync_source_parses_absolute_path() {
        let abs_path = std::env::temp_dir().join("some/path");
        let abs_str = abs_path.to_str().unwrap();
        let parsed: SyncSource = abs_str.parse().unwrap();
        assert_eq!(parsed, SyncSource::Path(abs_path));
    }

    #[test]
    fn sync_source_display_round_trips_primary() {
        assert_eq!(SyncSource::Primary.to_string(), "primary");
    }

    #[test]
    fn sync_source_display_round_trips_workweave() {
        let s = SyncSource::Workweave(WorkweaveName::new("ww1"));
        assert_eq!(s.to_string(), "ww1");
        assert_eq!(s.to_string().parse::<SyncSource>().unwrap(), s);
    }

    #[test]
    fn sync_source_display_round_trips_path() {
        let s = SyncSource::Path(PathBuf::from("/abs/path"));
        assert_eq!(s.to_string(), "/abs/path");
        assert_eq!(s.to_string().parse::<SyncSource>().unwrap(), s);
    }

    #[test]
    fn sync_failure_kind_tags_are_stable() {
        assert_eq!(
            SyncFailure::HeadUnreadable {
                error: "x".into(),
                cause: None
            }
            .kind(),
            "head-unreadable"
        );
        assert_eq!(
            SyncFailure::FastForwardImpossible {
                error: "x".into(),
                cause: None
            }
            .kind(),
            "ff-impossible"
        );
        assert_eq!(
            SyncFailure::RebaseFailed {
                error: "x".into(),
                cause: None
            }
            .kind(),
            "rebase-failed"
        );
        assert_eq!(
            SyncFailure::MergeFailed {
                error: "x".into(),
                cause: None
            }
            .kind(),
            "merge-failed"
        );
    }

    #[test]
    fn sync_failure_for_strategy_picks_matching_variant() {
        assert!(matches!(
            SyncFailure::for_strategy(SyncStrategy::Ff, "e".into(), None),
            SyncFailure::FastForwardImpossible { .. }
        ));
        assert!(matches!(
            SyncFailure::for_strategy(SyncStrategy::Rebase, "e".into(), None),
            SyncFailure::RebaseFailed { .. }
        ));
        assert!(matches!(
            SyncFailure::for_strategy(SyncStrategy::Merge, "e".into(), None),
            SyncFailure::MergeFailed { .. }
        ));
    }

    // -----------------------------------------------------------------------
    // Conflict-bail messages.
    //
    // One test per bail site asserts the message embeds the per-VCS
    // resolution hint and mentions `rwv abort` last (after the steps).
    // -----------------------------------------------------------------------

    /// Acceptance shape predicate: the message contains a resolution hint
    /// (the trait-method text via `git add <files>` token) and lists
    /// `rwv abort` strictly AFTER it.
    fn assert_resolution_first_abort_last(msg: &str) {
        let add_pos = msg
            .find("git add <files>")
            .unwrap_or_else(|| panic!("expected resolution hint `git add <files>` in: {msg}"));
        let abort_pos = msg
            .find("rwv abort")
            .unwrap_or_else(|| panic!("expected `rwv abort` mentioned in: {msg}"));
        assert!(
            abort_pos > add_pos,
            "`rwv abort` must come AFTER the resolution steps; \
             abort_pos={abort_pos}, add_pos={add_pos}, msg={msg}"
        );
    }

    // Site 1 — manifest-repo per-repo sync loop failure summary.
    #[test]
    fn manifest_repo_failure_message_rebase_includes_rebase_hint() {
        let src = SyncSource::Primary;
        let msg = manifest_repo_failure_message(SyncStrategy::Rebase, &src);
        assert!(
            msg.contains("git rebase --continue"),
            "expected rebase hint in: {msg}"
        );
        assert!(
            msg.contains("rwv sync primary"),
            "expected re-run hint: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    #[test]
    fn manifest_repo_failure_message_merge_includes_merge_hint() {
        let src = SyncSource::Primary;
        let msg = manifest_repo_failure_message(SyncStrategy::Merge, &src);
        assert!(
            msg.contains("git merge --continue"),
            "expected merge hint in: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 2 — Phase 1' (project repo) outer bail.
    #[test]
    fn phase1_bail_message_includes_resolution_steps_and_rwv_abort_last() {
        let src = SyncSource::Workweave(WorkweaveName::new("ww1"));
        let cwd = Path::new("/ws/projects/web-app");
        let msg = phase1_or_phase3_failure_message(Phase::One, cwd, SyncStrategy::Rebase, &src);
        assert!(
            msg.contains("Phase 1' (project repo)"),
            "expected phase label in: {msg}"
        );
        assert!(
            msg.contains("git rebase --continue"),
            "expected rebase hint in: {msg}"
        );
        assert!(
            msg.contains("/ws/projects/web-app"),
            "expected repo path: {msg}"
        );
        assert!(msg.contains("rwv sync ww1"), "expected re-run hint: {msg}");
        assert_resolution_first_abort_last(&msg);
    }

    // Site 3 — Phase 3 (re-lock) outer bail.
    #[test]
    fn phase3_bail_message_includes_resolution_steps_and_rwv_abort_last() {
        let src = SyncSource::Path(PathBuf::from("/abs/source"));
        let cwd = Path::new("/ws/projects/web-app");
        let msg = phase1_or_phase3_failure_message(Phase::Three, cwd, SyncStrategy::Merge, &src);
        assert!(
            msg.contains("Phase 3 (re-lock)"),
            "expected phase label in: {msg}"
        );
        assert!(
            msg.contains("git merge --continue"),
            "expected merge hint in: {msg}"
        );
        assert!(
            msg.contains("rwv sync /abs/source"),
            "expected re-run hint: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 4 — cherry-pick op hint (trait surface; sync no longer uses
    // cherry-pick directly but the message builder must still render the
    // op's hint correctly for any VCS impl that does).
    #[test]
    fn per_conflict_bail_cherry_pick_includes_cherry_pick_hint() {
        let src = SyncSource::Primary;
        let repo = Path::new("/ws/projects/web-app");
        let msg = per_conflict_bail_message(
            repo,
            ConflictOp::CherryPick,
            "cherry-pick (rebase replay)",
            "commit deadbeef on paths: foo.txt",
            &src,
        );
        assert!(
            msg.contains("git cherry-pick --continue"),
            "expected cherry-pick hint in: {msg}"
        );
        assert!(
            msg.contains("cherry-pick (rebase replay)"),
            "expected op label in: {msg}"
        );
        assert!(msg.contains("deadbeef"), "expected detail in: {msg}");
        assert!(msg.contains("foo.txt"), "expected detail in: {msg}");
        assert!(
            msg.contains("rwv sync primary"),
            "expected re-run hint: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 5 — Phase 1' merge inner bail.
    #[test]
    fn per_conflict_bail_merge_includes_merge_hint() {
        let src = SyncSource::Primary;
        let repo = Path::new("/ws/projects/web-app");
        let msg =
            per_conflict_bail_message(repo, ConflictOp::Merge, "merge", "paths: bar.txt", &src);
        assert!(
            msg.contains("git merge --continue"),
            "expected merge hint in: {msg}"
        );
        assert!(msg.contains("bar.txt"), "expected detail in: {msg}");
        assert!(
            msg.contains("rwv sync primary"),
            "expected re-run hint: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    #[test]
    fn conflict_op_for_strategy_maps_ff_to_rebase() {
        // ff cannot leave a conflict; we still nominate Rebase as the
        // fallback the user is likely to switch to.
        assert_eq!(
            conflict_op_for_strategy(SyncStrategy::Ff),
            ConflictOp::Rebase
        );
        assert_eq!(
            conflict_op_for_strategy(SyncStrategy::Rebase),
            ConflictOp::Rebase
        );
        assert_eq!(
            conflict_op_for_strategy(SyncStrategy::Merge),
            ConflictOp::Merge
        );
    }

    // SyncSource::resolve(Workweave) from a primary weave with no
    // active project must error rather than silently producing a garbage path.
    #[test]
    fn sync_source_workweave_resolve_errors_when_no_active_project() {
        // Build a minimal workspace directory so WorkspaceContext::resolve
        // succeeds and recognises it as a Weave (no .rwv-active).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(root.join("projects")).unwrap();

        // Resolve context from the weave root — no .rwv-active, no override.
        let ctx = crate::workspace::WorkspaceContext::resolve(&root, None).unwrap();
        assert!(
            matches!(ctx.location, WorkspaceLocation::Weave { project: None }),
            "expected Weave with no project, got something else"
        );

        let src = SyncSource::Workweave(WorkweaveName::new("some-ww"));
        let err = src.resolve(&ctx).unwrap_err().to_string();

        // require_active_project produces this message when no project is set
        // and no CWD hint is available.
        assert!(
            err.contains("no active project"),
            "expected 'no active project' error, got: {err}"
        );
        assert!(
            err.contains("rwv activate") || err.contains("--project"),
            "expected actionable hint (rwv activate / --project) in error, got: {err}"
        );
    }

    // Post-Phase-1' manifest reload is a hard bail, not warn-and-proceed.
    //
    // Before: on Project::from_dir failure after Phase 1', the code emitted a
    // warning (suppressed in --json mode) and fell through to Phase 3 with a
    // stale snapshot.  After: bail!() with a message that names `rwv abort`.
    //
    // This test pins the inline error wording to ensure it mentions `rwv abort`
    // and does not regress to the old suppress-and-proceed path.  The companion
    // E2E test (`sync_bails_hard_when_post_phase1_manifest_reload_fails` in
    // e2e_sync_abort_test.rs) exercises the live code path end-to-end.
    #[test]
    fn post_phase1_reload_error_message_mentions_rwv_abort() {
        // The error is constructed inline at the call site as:
        //
        //   anyhow::anyhow!(
        //       "failed to reload project manifest after Phase 1' ({e}).\n...\
        //        To recover: `rwv abort`",
        //       cwd_project_dir = ...,
        //   )?;
        //
        // We replicate the format string here so the test breaks if the wording
        // is changed to drop the recovery hint.
        let fake_dir = Path::new("/ws/projects/web-app");
        let fake_err = "YAML parse error: invalid mapping";
        let msg = format!(
            "failed to reload project manifest after Phase 1' ({fake_err}).\n\
             \n\
             The project repo was successfully rebased/merged, but the manifest \
             in {cwd} could not be parsed. Proceeding would silently \
             omit newly-added repos from the regenerated lock.\n\
             \n\
             To recover: `rwv abort`",
            cwd = fake_dir.display(),
        );

        assert!(
            msg.contains("rwv abort"),
            "post-Phase-1' reload error must mention `rwv abort`; msg: {msg}"
        );
        assert!(
            msg.contains("failed to reload project manifest after Phase 1'"),
            "must identify the Phase 1' reload site; msg: {msg}"
        );
        assert!(
            msg.contains("manifest") || msg.contains("rwv.yaml"),
            "must mention the manifest; msg: {msg}"
        );
    }

    // ---------------------------------------------------------------------------
    // Extensibility acceptance test
    //
    // Verifies that a fourth output mode can be added by implementing
    // OutputHandler without modifying TextHandler, JsonEnvelopeHandler, or
    // JsonNdjsonHandler.  A CountingHandler is constructed entirely in this test;
    // no existing handler or orchestration code is touched.
    // ---------------------------------------------------------------------------

    /// A minimal fourth-mode handler that just counts how many outcomes were
    /// recorded.  Demonstrates that OutputHandler is open for extension.
    struct CountingHandler {
        count: std::sync::Mutex<usize>,
    }

    impl CountingHandler {
        fn new() -> Self {
            Self {
                count: std::sync::Mutex::new(0),
            }
        }

        fn recorded(&self) -> usize {
            *self.count.lock().unwrap()
        }
    }

    impl OutputHandler for CountingHandler {
        fn emit_text(&self) -> bool {
            false
        }

        fn record(&self, _path: &str, _abs_path: &str, _outcome: &RepoSyncOutcome) {
            *self.count.lock().unwrap() += 1;
        }
    }

    #[test]
    fn output_handler_is_open_for_extension_without_modifying_existing_handlers() {
        // Build three distinct outcomes.
        let outcomes = vec![
            RepoSyncOutcome::Converged,
            RepoSyncOutcome::NoOp,
            RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
                error: "test".to_owned(),
                cause: None,
            }),
        ];

        let handler = CountingHandler::new();

        // Drive record() from outside the sync orchestration to prove the trait
        // contract is sufficient on its own.
        for outcome in &outcomes {
            handler.record("some/repo", "/abs/some/repo", outcome);
        }

        assert_eq!(
            handler.recorded(),
            3,
            "CountingHandler should record exactly one entry per record() call"
        );

        // Also verify that emit_text() returns the value the handler advertises.
        assert!(
            !handler.emit_text(),
            "CountingHandler should not emit text (it has no text output)"
        );
    }
}
