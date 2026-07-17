//! `rwv sync <source>` and `rwv abort` implementation.
//!
//! `rwv sync` aligns the CWD workspace with another workspace's committed
//! `rwv.lock`. `rwv abort` rolls back to pre-sync state using savepoint refs.

use crate::git::GitVcs;
use crate::lock::{commit_lock_file_with_message, generate_lock};
use crate::manifest::{LockFile, Manifest, Project, ProjectName, RepoPath, Role, WorkweaveName};
use crate::op_state::{self, LeaseRecord, OpVerb, OwnerRecord};
use crate::parallel::run_in_parallel;
use crate::status::{compute_relation, LockRelation};
use crate::vcs::{
    ConflictOp, RefName, ResolvedRevisionId, Vcs, VcsError, VcsErrorOutput, VerifiedRestoreOutcome,
};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use crate::workweave::{classify_checkout, workweave_path_for, CheckoutKind};
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

/// The commit message for the auto-relock the sync engine writes into the CWD
/// project repo when it regenerates `rwv.lock` from the converged manifest tips.
///
/// This is the ONE home of the literal (`fo-4rpnkm.1` §3): both the op-start
/// benign-staleness relock and the post-replay Phase-3 relock format their
/// commit message here, and the explain generator (`fo-m26yws`) splices the
/// same string into `rwv explain sync-to` via a `{{MSG:auto_relock}}`
/// placeholder so the docs cannot drift from the code. `<source>` is the
/// display name of the workspace the sync pulled from. Text is unchanged from
/// the pre-extraction literal.
pub fn auto_relock_commit_message(source: &str) -> String {
    format!("lock: auto-relock after sync from {source}")
}

// ---------------------------------------------------------------------------
// SyncStrategy — typed sync strategy
// ---------------------------------------------------------------------------

/// How `rwv sync` advances each repo to its lock target.
///
/// `merge` is intentionally not offered (state-space shrink). See the
/// "documented absence" note in `docs/explanation/joints/sync-semantics.md`
/// for the justification test and the origin-less weave-to-weave escape hatch.
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

/// Refuse a bare `rwv sync` / `rwv sync-to` when the workweave's recorded
/// `parent:` no longer exists on disk (retired or deleted out-of-band).
///
/// Without this guard the parent path flows into `WorkspaceContext::resolve`,
/// which calls `.canonicalize()` and dies with a raw `failed to canonicalize
/// … (os error 2)` that names no remedy. Replace that with the doctor
/// remediation the operator can act on directly. `primary_root` is named so
/// the operator knows where `--fix` will re-point the parent.
pub fn check_parent_not_dangling(parent: &Path, primary_root: &Path) -> anyhow::Result<()> {
    if parent.exists() {
        return Ok(());
    }
    anyhow::bail!(
        "recorded parent workspace does not exist on disk:\n  {}\n\
         \n\
         The parent was retired or deleted out-of-band, leaving this workweave's \
         `.rwv-workweave` `parent:` dangling. Re-point it and retry:\n\
         \n  rwv doctor --fix   # re-points the dangling parent to primary ({})\n\
         \n\
         Then re-run the sync. (Normal `sync-to --retire` / `workweave delete` adopts \
         children automatically; a dangling parent means the parent went away another way.)",
        parent.display(),
        primary_root.display(),
    )
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
}

impl SyncFailure {
    /// Stable variant tag suitable for `--json` output.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::HeadUnreadable { .. } => "head-unreadable",
            Self::FastForwardImpossible { .. } => "ff-impossible",
            Self::RebaseFailed { .. } => "rebase-failed",
        }
    }

    pub fn error(&self) -> &str {
        match self {
            Self::HeadUnreadable { error, .. }
            | Self::FastForwardImpossible { error, .. }
            | Self::RebaseFailed { error, .. } => error,
        }
    }

    pub fn cause(&self) -> Option<&VcsError> {
        match self {
            Self::HeadUnreadable { cause, .. }
            | Self::FastForwardImpossible { cause, .. }
            | Self::RebaseFailed { cause, .. } => cause.as_ref(),
        }
    }

    fn for_strategy(strategy: SyncStrategy, error: String, cause: Option<VcsError>) -> Self {
        match strategy {
            SyncStrategy::Ff => Self::FastForwardImpossible { error, cause },
            SyncStrategy::Rebase => Self::RebaseFailed { error, cause },
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
        }
    }
}

/// Step-3 fast-forward advance record for one repo in `rwv sync-to --json` output.
///
/// Present in a per-repo outcome iff step 3 (advance-target) actually advanced
/// that repo's branch pointer. Omitted (`skip_serializing_if = "Option::is_none"`)
/// in two cases: (a) no-op advance — target was already at CWD's tip; or (b) the
/// pre-advance HEAD read failed (`head_revision` returned `Err`) — in that case
/// `target_tip_before` is `None` and no record is emitted even if the ff succeeded.
#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct Step3AdvanceOutput {
    /// Target repo's HEAD SHA before the fast-forward.
    pub from_sha: String,
    /// Target repo's HEAD SHA after the fast-forward (== CWD's tip).
    pub to_sha: String,
}

/// One per-repo record in `rwv sync --json` output.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SyncOutcomeOutput {
    Converged {
        path: String,
        absolute_path: String,
        /// Step-3 fast-forward advance for this repo; present only in
        /// `rwv sync-to --json` output when step 3 advanced this repo.
        #[serde(skip_serializing_if = "Option::is_none")]
        step3_advance: Option<Step3AdvanceOutput>,
    },
    AlreadyAhead {
        path: String,
        absolute_path: String,
        commits_ahead: usize,
        /// Step-3 fast-forward advance for this repo; present only in
        /// `rwv sync-to --json` output when step 3 advanced this repo.
        #[serde(skip_serializing_if = "Option::is_none")]
        step3_advance: Option<Step3AdvanceOutput>,
    },
    NoOp {
        path: String,
        absolute_path: String,
        /// Step-3 fast-forward advance for this repo; present only in
        /// `rwv sync-to --json` output when step 3 advanced this repo.
        #[serde(skip_serializing_if = "Option::is_none")]
        step3_advance: Option<Step3AdvanceOutput>,
    },
    Failed {
        path: String,
        absolute_path: String,
        failure: SyncFailureOutput,
        /// Step-3 fast-forward advance for this repo; present only in
        /// `rwv sync-to --json` output when step 3 advanced this repo.
        /// Typically absent when the repo failed in step 1.
        #[serde(skip_serializing_if = "Option::is_none")]
        step3_advance: Option<Step3AdvanceOutput>,
    },
}

impl SyncOutcomeOutput {
    pub fn from_outcome(path: String, absolute_path: String, outcome: &RepoSyncOutcome) -> Self {
        match outcome {
            RepoSyncOutcome::Converged => Self::Converged {
                path,
                absolute_path,
                step3_advance: None,
            },
            RepoSyncOutcome::AlreadyAhead { commits_ahead } => Self::AlreadyAhead {
                path,
                absolute_path,
                commits_ahead: *commits_ahead,
                step3_advance: None,
            },
            RepoSyncOutcome::NoOp => Self::NoOp {
                path,
                absolute_path,
                step3_advance: None,
            },
            RepoSyncOutcome::Failed(failure) => Self::Failed {
                path,
                absolute_path,
                failure: SyncFailureOutput::from(failure),
                step3_advance: None,
            },
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// Return a mutable reference to the `step3_advance` field regardless of variant.
    fn step3_advance_mut(&mut self) -> &mut Option<Step3AdvanceOutput> {
        match self {
            Self::Converged { step3_advance, .. }
            | Self::AlreadyAhead { step3_advance, .. }
            | Self::NoOp { step3_advance, .. }
            | Self::Failed { step3_advance, .. } => step3_advance,
        }
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
/// Extends [`SyncJsonOutput`] with sync-to-specific observability fields:
/// - `source_workweave` — the workweave the command was invoked from (null
///   when invoked from the primary weave).
/// - `target` — the absolute path of the target workspace that was advanced.
/// - `retired` — true iff `--retire` was passed AND the workweave was deleted.
/// - `project_repo_advance` — step-3 advance of `projects/<project>/.git`;
///   omitted when the project repo was already at CWD's tip (no-op advance).
/// - per-outcome `step3_advance` — step-3 advance SHA pair for each manifest
///   repo; omitted on a no-op advance.
///
/// Kept as a separate type so the generated schema artifact
/// (`docs/reference/schemas/sync-to.json`) has its own title/description.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SyncToJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    /// The workweave name the command was invoked from; null when invoked from
    /// the primary weave.
    pub source_workweave: Option<String>,
    /// Absolute path of the target workspace that step-3 fast-forwarded.
    pub target: String,
    /// True iff `--retire` was passed AND retire actually fired (the workweave
    /// was deleted). False when `--retire` was not passed, or when retire was
    /// skipped (e.g. invoked from the primary weave).
    pub retired: bool,
    pub outcomes: Vec<SyncOutcomeOutput>,
    /// Step-3 advance of the project repo (`projects/<project>/.git`). Omitted
    /// when the project repo was already at CWD's tip (no-op fast-forward).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_repo_advance: Option<Step3AdvanceOutput>,
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
    // Replay re-entry (`rwv sync --continue`) can find a manifest repo
    // mid-rebase from a previous phase that stopped on a conflict. HEAD in
    // that state points at the last-applied pick (descended from `target`),
    // which would falsely match the AlreadyAhead branch below and skip the
    // remaining picks entirely. Route mid-rebase repos straight to
    // `apply_strategy` so the `rebase_continue` path drives the rebase
    // forward; the head-equality / AlreadyAhead short-circuits assume a
    // repo not in a mid-op state.
    if matches!(GitVcs.mid_op(repo), Some(ConflictOp::Rebase)) && strategy == SyncStrategy::Rebase {
        return match apply_strategy(repo, target, strategy) {
            Ok(()) => RepoSyncOutcome::Converged,
            Err(StrategyError { message, cause }) => {
                RepoSyncOutcome::Failed(SyncFailure::for_strategy(strategy, message, cause))
            }
        };
    }

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
                    "cannot fast-forward; rerun with --strategy rebase. {}",
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
            //
            // Replay re-entry (`rwv sync --continue`): if the repo is
            // already mid-rebase from a previous phase that stopped on a
            // conflict, `Vcs::rebase` would fail immediately ("cannot
            // rebase: you have unstaged changes" / "another rebase is in
            // progress"). Route through `Vcs::rebase_continue` instead so
            // the operator's resolve+stage-then-`rwv sync --continue` loop
            // actually drives the rebase forward through remaining picks
            // (including lock-only picks that must merge to ours via the
            // inline driver flags). A mid-op state that is NOT rebase
            // (mid-merge, mid-cherry-pick) is not rwv-initiated for this
            // path; fall through to `Vcs::rebase` and let it fail loudly.
            match GitVcs.mid_op(repo) {
                Some(ConflictOp::Rebase) => {
                    GitVcs
                        .rebase_continue(repo)
                        .map_err(StrategyError::from_vcs)?;
                }
                _ => {
                    GitVcs
                        .rebase(repo, target, target)
                        .map_err(StrategyError::from_vcs)?;
                }
            }
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

/// Derive the savepoint op-id string used for target-workspace repos.
///
/// Target repos in a sync-to op may share a git object store with CWD repos
/// when both are worktrees of the same canonical clone. In that case, using
/// the same `op_id` for both sides would give them the same ref name
/// (`refs/rwv/pre-op/<op_id>`) — the first restore during `rwv abort` would
/// drop the ref, leaving the second restore unable to find it.
///
/// We use `<op_id>-target` to give target repos their own savepoint ref
/// namespace, guaranteeing abort can restore both sides independently even in
/// a shared-object-store topology.
fn target_savepoint_id(op_id: &OpId) -> String {
    format!("{}-target", op_id.as_str())
}

// NOTE (`fo-4rpnkm.1`, §2): the old `check_lock_freshness` / `lock_recovery`
// pair — which refused on ANY lock↔HEAD mismatch — is replaced by the
// benign-staleness classification (`classify_lock_relations` +
// `anomalous_relation_refusal` + the tips-as-truth / op-start-relock handling
// in `guard_and_mark`). `behind` is no longer an error; only genuinely
// anomalous relations (ahead / diverged / no-lock / unknown) refuse, naming the
// relation. `--allow-stale-lock` still bypasses the whole gate.

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
         commits not in source workspace '{source_workspace_name}'.\n\
         \n\
         To land them: rerun with `--strategy rebase`.\n\
         To bring source in sync first: sync the other direction.\n\
         To discard them (recoverable via `rwv abort`): rerun with `--discard-local-commits` \
         (pre-sync state preserved in refs/rwv/pre-op/<id>).",
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

/// The operator-facing resume command for a mid-op verb (Correction 4).
///
/// A failed op is resumed with `--continue` from the OWNING workspace, which
/// reads every parameter (source, strategy, target, retire, overrides) back
/// out of op-state — so the resume text is `rwv {verb} --continue`, NEVER a
/// hardcoded `rwv sync {source}`. Hardcoding `rwv sync` was wrong two ways:
/// it named the PULL verb for a `sync-to` (landing) op, and it re-supplied
/// arguments the record already holds. The verb comes from the op's `verb`
/// field ([`OpVerb`]) so the string is derived from op-state, not the engine's
/// guess.
fn resume_command(verb: OpVerb) -> String {
    format!("rwv {verb} --continue")
}

/// Bail message for the manifest-repo per-repo sync loop (Site 1).
///
/// One or more repos in the loop emitted a per-repo failure (printed already).
/// Lead with the resolution steps that apply uniformly to each conflicted
/// repo; mention `rwv abort` last as the rollback option.
///
/// `live_conflict` is `Some(op)` only when a manifest repo is genuinely left
/// in a VCS-native in-flight state (a live [`ConflictOp`]); the VCS-native
/// resolution steps (`cd <repo>` / stage the resolution) are emitted ONLY then
/// (Correction 4). A non-conflict batch of failures (e.g. a fetch error) omits
/// the VCS block and points straight at the resume command. For
/// [`ConflictOp::Rebase`] the VCS hint stops at staging — the rwv-native
/// `{resume}` line IS the continue step (fo-w2ajvn.2); merge/cherry-pick
/// hints still carry their VCS-native continue since rwv cannot resume those.
fn manifest_repo_failure_message(verb: OpVerb, live_conflict: Option<ConflictOp>) -> String {
    let resume = resume_command(verb);
    match live_conflict {
        Some(op) => {
            let hint = GitVcs.conflict_resolution_hint(op);
            format!(
                "sync hit conflicts in one or more manifest repos (see per-repo lines above).\n\
                 \n\
                 To resolve each conflicted repo:\n\
                   cd <repo>\n\
                 {hint}\n\
                   {resume}   # resume; already-converged repos are no-ops\n\
                 \n\
                 If you'd rather roll everything back: `rwv abort`."
            )
        }
        None => format!(
            "sync hit failures in one or more manifest repos (see per-repo lines above).\n\
             \n\
             Fix the underlying issue in each failed repo, then:\n\
               {resume}   # resume; already-converged repos are no-ops\n\
             \n\
             If you'd rather roll everything back: `rwv abort`."
        ),
    }
}

/// Bail message for the Phase 1' / Phase 3 top-level failures (Sites 2 and 3).
///
/// Both phases print their inner error via `eprintln!` before bailing; this
/// message gives the operator a uniform "what next?" block that closes with
/// `rwv abort` as the rollback option.
///
/// `live_conflict` is `Some(op)` only when the project repo is actually left
/// mid-op (a rebase/merge/cherry-pick conflict). The VCS-native resolution
/// steps are emitted ONLY then (Correction 4) — Phase 3 (relock) is never a
/// VCS conflict, so it always passes `None` and never teaches a spurious
/// `git rebase --continue` (which would print "No rebase in progress").
fn phase1_or_phase3_failure_message(
    phase: Phase,
    cwd_project_dir: &Path,
    verb: OpVerb,
    live_conflict: Option<ConflictOp>,
) -> String {
    let resume = resume_command(verb);
    let phase_label = phase.label();
    let repo_display = cwd_project_dir.display();
    match live_conflict {
        Some(op) => {
            let hint = GitVcs.conflict_resolution_hint(op);
            format!(
                "sync failed in {phase_label} (see error above).\n\
                 \n\
                 Resolve the conflict in {repo_display}:\n\
                   cd {repo_display}\n\
                 {hint}\n\
                   {resume}   # resume; already-converged repos are no-ops\n\
                 \n\
                 If you'd rather roll everything back: `rwv abort`."
            )
        }
        None => format!(
            "sync failed in {phase_label} (see error above).\n\
             \n\
             Fix the underlying issue, then: {resume}\n\
             If you'd rather roll everything back: `rwv abort`."
        ),
    }
}

/// Bail message for an inner per-conflict-site.
///
/// Used by Phase 1' when a rebase or merge leaves the project repo in the
/// VCS-native in-flight state. This site ALWAYS has a live [`ConflictOp`] (`op`
/// comes from the `RebaseConflict` variant), so the VCS-native steps are
/// unconditional here. The per-VCS resolution steps come from the trait method;
/// this helper builds the surrounding framing (which repo, how to resume, how
/// to abort).
///
/// For [`ConflictOp::Rebase`] the VCS hint stops at staging and the appended
/// `{resume}` line is the continue step (rwv resumes the rebase natively,
/// fo-w2ajvn.2). For [`ConflictOp::Merge`] / [`ConflictOp::CherryPick`] the
/// VCS hint still carries its own `git … --continue` — rwv has no native
/// resume for those ops; the operator finishes them in git, then `{resume}`
/// picks the op back up.
fn per_conflict_bail_message(
    repo: &Path,
    op: ConflictOp,
    op_label: &str,
    detail: &str,
    verb: OpVerb,
) -> String {
    let hint = GitVcs.conflict_resolution_hint(op);
    let resume = resume_command(verb);
    let repo_display = repo.display();
    format!(
        "sync hit a conflict in {repo_display} during {op_label} ({detail}).\n\
         \n\
         To resolve:\n\
           cd {repo_display}\n\
         {hint}\n\
           {resume}   # resume; already-converged repos are no-ops\n\
         \n\
         If you'd rather roll everything back: `rwv abort`."
    )
}

// Post-sync index/working-tree refresh is delegated to
// [`Vcs::refresh_index_to_head_if_safe`] and
// [`Vcs::refresh_working_tree_to_head_if_safe`]; the safety logic
// (reachability check before any clobber) lives in the VCS impl rather
// than being inlined here. See those trait method doc-comments.

/// Precondition + self-heal: the CWD project repo's committed `.gitattributes`
/// must contain `rwv.lock merge=rwv-ours`, and the repo-local `merge.rwv-ours.*`
/// config must be planted, before the `Rebase` strategy runs.
///
/// `Rebase` is still gated even though `merge` (the strategy) was removed: git
/// rebase replays each commit as a 3-way merge against the new base, so the
/// `merge=rwv-ours` driver is required to keep lock-only commits from
/// conflicting on `rwv.lock`. The requirement is about git's *per-commit
/// merge* during replay, not the removed merge *strategy*.
///
/// The mechanism has two halves that MUST both be present:
/// 1. The `.gitattributes` line `rwv.lock merge=rwv-ours` *assigns* the
///    driver to `rwv.lock` — this half lives in the committed tree.
/// 2. A `merge.rwv-ours.driver` config entry *defines* the driver's shell
///    command. This half must be visible to whatever git process runs the
///    replay — rwv's own rebase passes it inline with `-c` (belt-and-braces),
///    but bare `git rebase --continue` (the resume path git itself
///    advertises in conflict stderr) inherits neither the inline flag nor
///    the environment; only durable config keeps the driver defined on
///    that resume. This function plants that config as its first act — it
///    is derived, local, idempotent state, so writing (not just checking)
///    is the right primitive.
///
/// Worktrees share `.git/config` with the canonical repo, so one plant
/// covers every workweave checkout.
///
/// Without half (1), git's default 3-way merge runs on `rwv.lock` and
/// conflicts whenever both sides have lock edits — regardless of what
/// drivers are defined in config.
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
/// If the committed `.gitattributes` still carries the LEGACY `merge=ours`
/// spelling (pre-fo-yk0rlj), the invariant bails with a migration-specific
/// message directing the operator at `rwv doctor --fix` — which rewrites
/// AND commits the .gitattributes migration. If neither the new nor the
/// legacy line is present, the invariant bails with the classic
/// "add-the-line" message.
///
/// The .gitattributes assignment itself is NOT written by this function
/// — that's `rwv doctor --fix`'s job, and requires a commit which sync
/// must not silently make on the operator's behalf.
fn verify_replay_exclusion_invariant(cwd_project_dir: &Path) -> anyhow::Result<()> {
    // Plant the durable config first — regardless of whether the
    // .gitattributes assignment is present, having the driver defined
    // makes any downstream `git rebase --continue` safe against the
    // lock-only pick conflict. This is idempotent; if the config is
    // already planted, `git config` writes the same value.
    crate::git::plant_rwv_merge_driver_config(cwd_project_dir).with_context(|| {
        format!(
            "failed to plant `{}` config in {}",
            crate::git::RWV_MERGE_DRIVER_CONFIG_KEY,
            cwd_project_dir.display()
        )
    })?;

    let has_new = GitVcs
        .has_committed_replay_exclusion(cwd_project_dir, Path::new("rwv.lock"))
        .unwrap_or(false);
    if has_new {
        return Ok(());
    }

    let has_legacy =
        crate::git::has_committed_legacy_replay_exclusion(cwd_project_dir, Path::new("rwv.lock"))
            .unwrap_or(false);

    if has_legacy {
        anyhow::bail!(
            "sync --strategy=rebase requires `rwv.lock merge=rwv-ours` \
             in the project repo's committed .gitattributes, but {ga} still \
             carries the legacy `rwv.lock merge=ours` spelling. The rename \
             (fo-yk0rlj) closes an accidental-collision hazard where an \
             unrelated `merge.ours.driver` in the operator's global git \
             config would silently activate on rwv.lock during a bare \
             `git rebase --continue`.\n\
             \n\
             To migrate: run `rwv doctor --fix` from this workspace. It \
             rewrites the `.gitattributes` line to the new spelling AND \
             commits the change:\n\
               cd {dir}\n\
               rwv doctor --fix",
            ga = cwd_project_dir.join(".gitattributes").display(),
            dir = cwd_project_dir.display(),
        )
    }

    anyhow::bail!(
        "sync --strategy=rebase requires `rwv.lock merge=rwv-ours` \
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
// Sync's reference-alias chokepoint
// ---------------------------------------------------------------------------
//
// The sync/abort phase machine has no single shared repo-set object: each phase
// legitimately iterates a *different* manifest or lock (CWD manifest, source
// lock, target manifest, …). What every mutating phase *does* share is one
// shape — iterate `repo_path`s, join each against a workspace dir to form the
// on-disk checkout `abs`, gate on `abs.exists()`, then operate on `abs`. That
// per-checkout existence gate is the narrowest chokepoint, so the reference
// exclusion lives there, in a single predicate every site routes through.
//
// A `reference` repo materialized as a symlink (`CheckoutKind::ReferenceAlias`)
// aliases the single canonical weave-root clone shared by *every* workweave.
// Operating on it through the symlink — savepoint `refs/rwv/pre-op/*`, rebase /
// ff its branch, `reset --hard` on abort, `worktree add/remove` — mutates that
// shared store (cross-workweave ref collisions, a branch other workspaces
// read). A reference symlink is also read-only, lock-pinned, and byte-identical
// across workweaves, so there is *nothing to sync*. Excluding it here makes the
// canonical store **unreachable by construction** from every mutating phase: an
// absent element cannot be operated on, where a per-call-site guard could be
// forgotten at the next site.
//
// The predicate keys on `CheckoutKind::ReferenceAlias` (⇔ the checkout path is
// a symlink), **never on `role`**. A `reference` repo created with
// `--worktree-references` is a real worktree on its own ephemeral branch:
// `classify_checkout` returns `Worktree` for it, so it passes this gate and
// syncs exactly like any owned/fork worktree. Keying on `role == Reference`
// would silently break that escape hatch. `rwv lock` is unaffected — it reads
// HEAD through the symlink (resolving to the canonical), which correctly pins
// the shared SHA; reference repos stay in the lock for reproducibility and
// `rwv fetch`. Only sync's *advancement / mutation* skips them.

/// Whether a sync/abort phase may operate on the checkout at `abs`.
///
/// True iff `abs` is an on-disk **worktree** checkout. This combines the two
/// conditions every mutating site needs:
/// - it must exist (an absent checkout has nothing to mutate / restore), and
/// - it must not be a [`CheckoutKind::ReferenceAlias`] — a symlink aliasing the
///   shared canonical store, which is read-only and must never be mutated.
///
/// This is the single chokepoint for the reference-repo exclusion: every
/// savepoint / replay / advance-target / abort / materialize / prune loop gates
/// the checkout it is about to touch through this predicate, so the canonical
/// store is unreachable from all of them. Keyed on alias-ness, never on role,
/// so `--worktree-references` reference repos (real worktrees) sync normally.
///
/// Note `classify_checkout` returns [`CheckoutKind::Worktree`] for a
/// non-existent path, so the `abs.exists()` term is load-bearing and cannot be
/// folded away.
fn checkout_is_syncable(abs: &Path) -> bool {
    abs.exists() && classify_checkout(abs) == CheckoutKind::Worktree
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
/// - `record_step3_advance` is called by the advance-target phase for each
///   repo whose target branch pointer was actually moved (no-ops are not
///   reported). The default no-op implementation is suitable for text-mode
///   handlers and plain-sync JSON handlers that do not surface step-3 SHAs.
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

    /// Record a step-3 (advance-target) fast-forward for one repo or the
    /// project repo. Called iff the branch pointer actually moved.
    ///
    /// `path` matches the key used in `record` (manifest-relative repo path,
    /// or the sentinel `"(project)"` for the project repo).
    /// `from_sha` is the target's tip before the FF; `to_sha` is after.
    ///
    /// Default implementation is a no-op — suitable for text-mode and
    /// plain-sync JSON handlers that do not need step-3 SHAs.
    fn record_step3_advance(&self, _path: &str, _from_sha: &str, _to_sha: &str) {}
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

/// Envelope-mode handler for `rwv sync-to --json` (serial mode).
///
/// Extends [`JsonEnvelopeHandler`]'s buffering with a second accumulator for
/// step-3 advance records (one per repo that was actually fast-forwarded in
/// the advance-target phase). After the machine completes, the caller reads
/// both `records` and `step3_advances` to assemble the full
/// [`SyncToJsonOutput`] envelope.
///
/// Text chatter is suppressed (`emit_text` returns `false`).
pub struct JsonEnvelopeSyncToHandler<'a> {
    records: &'a Mutex<Vec<SyncOutcomeOutput>>,
    /// Per-repo step-3 advance records keyed by the repo path string (same
    /// key as `record`'s `path` argument, or `"(project)"` for the project
    /// repo). Only populated when the target's branch pointer actually moved.
    step3_advances: &'a Mutex<std::collections::HashMap<String, Step3AdvanceOutput>>,
}

impl OutputHandler for JsonEnvelopeSyncToHandler<'_> {
    fn emit_text(&self) -> bool {
        false
    }

    fn record(&self, path: &str, abs_path: &str, outcome: &RepoSyncOutcome) {
        let out = SyncOutcomeOutput::from_outcome(path.to_owned(), abs_path.to_owned(), outcome);
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(out);
    }

    fn record_step3_advance(&self, path: &str, from_sha: &str, to_sha: &str) {
        let advance = Step3AdvanceOutput {
            from_sha: from_sha.to_owned(),
            to_sha: to_sha.to_owned(),
        };
        let mut guard = self
            .step3_advances
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.insert(path.to_owned(), advance);
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
    /// Display name of the source workspace, used in human messages
    /// (e.g. the auto-relock commit message and replay's lock-freshness hint).
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
    /// Path arg the operator passed on the CLI (or recorded in op-state),
    /// retained for hint messages that show the original target spelling.
    cli_path: PathBuf,
    strategy: SyncStrategy,
    /// Consent: discard CWD's project commits that are not in source
    /// (hard-reset Phase 1'; today's `--force` divergence semantics).
    /// Recorded as `discard-local-commits` in `OwnerRecord.overrides`.
    discard_local_commits: bool,
    retire: bool,
    jobs: usize,
    handler: &'a dyn OutputHandler,
    verb: op_state::OpVerb,
    op_id: OpId,
    /// Atomic source snapshot pinned at guard time (T0): one ref read of
    /// the source project tip, then manifest + lock read AT that revision.
    /// On `--continue`, T0 is re-established at the start of the resumed
    /// session — per-repo no-op detection handles repos that already
    /// converged in the previous (now-aborted) replay run.
    snapshot: SourceSnapshot,
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
/// Bundled arguments for a `sync` or `sync-to` invocation.
///
/// Replaces the positional argument lists that were duplicated across the
/// `run_sync*` / `run_sync_to*` entry points and threaded into `run_machine`,
/// `guard_and_mark`, and `load_continuing_context`. Callers build one
/// `SyncRequest` and pass it by value to an entry point.
#[derive(Debug, Clone)]
pub struct SyncRequest {
    /// Source (sync) or target (sync-to) workspace. `None` under `--continue`
    /// (read from op-state) or for a bare `rwv sync` inside a workweave
    /// (read from the `.rwv-workweave` parent marker).
    pub source: Option<SyncSource>,
    /// How to advance each repo to its lock target. Defaults to `ff`.
    pub strategy: SyncStrategy,
    /// Bypass the lock-freshness precondition (`--allow-stale-lock`).
    pub allow_stale_lock: bool,
    /// Hard-reset the project repo when CWD is not an ancestor of the source
    /// tip (`--discard-local-commits`).
    pub discard_local_commits: bool,
    /// Delete the workweave after a successful `sync-to` (`--retire`).
    pub retire: bool,
    /// Override the active project (`--project`); `None` uses `.rwv-active`.
    pub project_override: Option<ProjectName>,
    /// Parallel per-repo worker count (resolved `-j N`); `1` runs serially.
    pub jobs: usize,
    /// Resume an in-flight op from op-state (`--continue`).
    pub do_continue: bool,
}

impl Default for SyncRequest {
    fn default() -> Self {
        Self {
            source: None,
            strategy: SyncStrategy::Ff,
            allow_stale_lock: false,
            discard_local_commits: false,
            retire: false,
            project_override: None,
            // 0 is never a valid worker count; default to serial (1) so that
            // SyncRequest::default() is immediately safe to pass to run_sync.
            jobs: 1,
            do_continue: false,
        }
    }
}

pub fn run_sync(cwd: &Path, request: SyncRequest) -> anyhow::Result<()> {
    let stdout_lock: Mutex<()> = Mutex::new(());
    let handler = TextHandler {
        stdout_lock: &stdout_lock,
    };
    run_machine(MachineVerb::Sync, cwd, &request, &handler)
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
fn run_machine(
    verb: MachineVerb,
    cwd: &Path,
    request: &SyncRequest,
    handler: &dyn OutputHandler,
) -> anyhow::Result<()> {
    let ctx = if request.do_continue {
        load_continuing_context(verb, cwd, request, handler)?
    } else {
        guard_and_mark(verb, cwd, request, handler)?
    };

    drive(&ctx)
}

/// The phase-machine driver. Reads the persisted phase, runs it, persists the
/// transition to the next phase, loops.
///
/// Invariant: the persisted phase is the phase in progress. The owner record's
/// `phase` field is the SINGLE source of truth and the persistence point is
/// the post-transition `advance_phase` write — entry into the loop relies on
/// either `guard_and_mark`'s initial write (fresh start: phase=replay) or the
/// prior iteration's post-transition write (resume: phase=whatever crashed).
///
/// Crash semantics:
///   - Inside `run_phase`: record stays at the phase that was running →
///     `--continue` re-enters that phase (idempotent by construction).
///   - After `run_phase` returned but before `advance_phase` of the next phase
///     committed: record still says current → `--continue` re-runs the just-
///     completed phase (idempotent), then transitions.
///   - After `advance_phase` of the next phase committed: record says next →
///     `--continue` enters next directly.
fn drive(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    loop {
        let phase = ctx.current_phase()?;
        let next = run_phase(ctx, phase)?;
        match next {
            Some(p) => {
                // Post-transition write: the canonical (and only) persistence
                // point. Until this commits, a crash leaves the record at the
                // just-completed phase, which re-runs idempotently on resume.
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
fn guard_and_mark<'a>(
    verb: MachineVerb,
    cwd: &Path,
    request: &SyncRequest,
    handler: &'a dyn OutputHandler,
) -> anyhow::Result<OpContext<'a>> {
    let source = request.source.as_ref();
    let strategy = request.strategy;
    let allow_stale_lock = request.allow_stale_lock;
    let discard_local_commits = request.discard_local_commits;
    let retire = request.retire;
    let project_override = request.project_override.clone();
    let jobs = request.jobs;

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
                // A dangling parent (retired/deleted out-of-band) would
                // otherwise die on a raw `failed to canonicalize … (os error
                // 2)` inside WorkspaceContext::resolve. Detect it here and
                // emit the friendly doctor-remediation text instead.
                check_parent_not_dangling(&marker.parent, cwd_ctx.primary_path())?;
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

    // For plain `sync <src>`: replay rebases CWD onto src, then relocks; no advance-target.
    //     source = src (replay pulls from here), dest = CWD (relock writes here).
    // For `sync-to <tgt>`: replay rebases CWD onto tgt, then relocks, then ff-advances tgt.
    //     source = tgt (replay pulls from here), dest = tgt (advance-target writes here).
    //     CWD itself is where replay+relock run; tracked via `cwd_project_dir`.
    let (source_workspace_dir, dest_workspace_dir) = match verb {
        MachineVerb::Sync => (cli_path.clone(), cwd_workspace_dir.clone()),
        MachineVerb::SyncTo => (cli_path.clone(), cli_path.clone()),
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

    // source_workspace_dir is the operator's arg for both verbs (sync's <src>
    // and sync-to's <tgt> — replay pulls from there in either case).
    // `source_is_workweave` scopes tips-as-truth (§2): a `behind` source lock is
    // pulled-as-tips only when the source is workweave-typed; a primary-weave
    // source keeps the refusal (a reproducibility-sensitive locked-snapshot pull
    // from the primary must not silently take live tips over its committed lock).
    let (source_project_dir, source_workspace_name, source_project_name, source_is_workweave) = {
        let override_arg = match verb {
            MachineVerb::Sync => other_project_override.clone(),
            // For sync-to, the target workspace must resolve to CWD's project.
            MachineVerb::SyncTo => Some(cwd_project_name.clone()),
        };
        let source_ctx = WorkspaceContext::resolve(&source_workspace_dir, override_arg)?;
        let pname = find_project_name(&source_ctx)?;
        let dir = source_ctx.active_path().join("projects").join(&pname);
        let is_workweave = matches!(source_ctx.location, WorkspaceLocation::Workweave { .. });
        (dir, workspace_name(&source_ctx), pname, is_workweave)
    };

    // dest_project_dir is where the terminal write lands.
    //   plain sync: CWD (relock writes a new lock commit there).
    //   sync-to: target (advance-target ff-forwards target's repos to CWD's tips).
    let dest_project_dir = match verb {
        MachineVerb::Sync => cwd_project_dir.clone(),
        MachineVerb::SyncTo => source_project_dir.clone(),
    };

    // Sibling-sync warning: only meaningful for plain sync.
    if matches!(verb, MachineVerb::Sync) {
        warn_on_sibling_sync(&cwd_ctx, &source_workspace_dir, emit_text);
    }

    // === Preconditions (no mutation yet) ===

    // Concurrency guard FIRST (Correction 1, ORDERING). An `.rwv-op` /
    // `.rwv-op-lease` involving any touched workspace means a prior op is still
    // in flight; that fact dominates every other precondition. Running it ahead
    // of the lock-relation classification (and the dirty preflights) means a
    // verb hitting a mid-op workspace reports the in-flight op — its name, age,
    // and the two exits (`--continue` / `rwv abort`) — instead of a stale-lock /
    // relation refusal computed against a workspace another op is mutating. The
    // touched set is the same set the mark phase writes op-state into: CWD for
    // sync; CWD + target for sync-to.
    let touched: Vec<&Path> = match verb {
        MachineVerb::Sync => vec![cwd_workspace_dir.as_path()],
        MachineVerb::SyncTo => vec![cwd_workspace_dir.as_path(), dest_workspace_dir.as_path()],
    };
    op_state::check_no_op_in_progress(&touched)?;

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
        check_sync_to_ff_precondition(&cwd_project_dir, &dest_project_dir, emit_text)?;
    }

    // sync-to preflights (both refuse before op-state / any mutation):
    //   - dirty-source (§1): CWD-side tracked dirt would go stale mid-rebase.
    //   - dirty-target: the target's uncommitted work advance-target overwrites.
    // Source before target: the operator's own workspace is the first thing they
    // can fix, and a dirty source is the state we most want to define away.
    if matches!(verb, MachineVerb::SyncTo) {
        let cwd_project_preflight = Project::from_dir(&cwd_project_dir)
            .context("failed to load CWD project for sync-to preflights")?;
        check_dirty_source_preflight(
            &cwd_project_preflight,
            &cwd_workspace_dir,
            &cwd_project_dir,
            cwd,
        )?;
        check_dirty_target_preflight(
            &cwd_project_preflight,
            &dest_workspace_dir,
            &dest_project_dir,
            &cli_path,
        )?;
    }

    // Pin the source snapshot now so the remaining replay preconditions are
    // all reads against a coherent T0. This is the §6 "snapshot reads"
    // mechanism: one atomic ref read pins source; manifest + lock are read
    // at that revision; everything downstream is content-addressed.
    let snapshot = pin_source_snapshot(&source_project_dir)?;

    // Replay preconditions (pure reads; refusals leave no trace).
    let cwd_project = Project::from_dir(&cwd_project_dir)
        .context("failed to load CWD project for guard preconditions")?;
    let cwd_workspace_name_str = workspace_name(&cwd_ctx);

    // === Benign-staleness classification (§2, Correction 2) ===
    //
    // Classify each side's committed lock↔HEAD relation with the SAME per-repo
    // vocabulary `rwv status` uses ([`LockRelation`]). Recall the terminology
    // inversion: the spec's benign "lock behind HEAD" is `LockRelation::Ahead`
    // (tip ahead of lock). `--allow-stale-lock` bypasses the whole gate.
    //
    // Scope of the benign relaxation (kept to the spec's EXPLICIT §2 bullets;
    // see the FLAG in the summary re: the pull destination):
    //   - sync-to (landing): CWD is the landing set. A lock-behind-HEAD (`Ahead`)
    //     CWD repo auto-relocks at op start (below), LOUD line per repo with
    //     commit count. Every other non-`ok` CWD relation refuses.
    //   - sync (pull) SOURCE: a lock-behind-HEAD (`Ahead`) source is tips-as-truth
    //     for a WORKWEAVE source (note + pull committed tips, leave source lock
    //     alone); a PRIMARY-weave source keeps the refusal (reproducibility scope
    //     decision). Every other non-`ok` source relation refuses.
    //   - sync (pull) DESTINATION (CWD): kept at the pre-Design-B behavior — any
    //     non-`ok` relation (INCLUDING `Ahead`) refuses. The spec relaxed only the
    //     pull SOURCE; relaxing the pull destination too would be cleaner but is
    //     NOT in the explicit scope, so it is held and flagged rather than
    //     silently widened.
    //
    // Unresolvable lock entries (a pinned tag/branch that no longer exists) are a
    // corrupt-lock error distinct from any relation and refuse first, naming the
    // unknown revision.
    let source_class = if allow_stale_lock {
        None
    } else {
        Some(classify_lock_relations(
            &source_workspace_dir,
            &snapshot.source_manifest,
            Some(&snapshot.raw_source_lock),
        ))
    };
    let cwd_class = if allow_stale_lock {
        None
    } else {
        Some(classify_lock_relations(
            &cwd_workspace_dir,
            &cwd_project.manifest,
            cwd_project.lock.as_ref(),
        ))
    };

    // CWD repos whose lock is behind HEAD (relation `Ahead`) that a sync-to
    // landing auto-relocks at op start.
    let mut cwd_lock_behind: Vec<RepoRelation> = Vec::new();

    if let (Some(source_class), Some(cwd_class)) = (source_class, cwd_class) {
        // Corrupt (unresolvable) lock entries refuse first, naming the revision.
        if let Some((rp, raw)) = source_class.unresolvable.first() {
            anyhow::bail!(
                "{}",
                unresolvable_lock_refusal(
                    Side::Source,
                    &source_workspace_name,
                    source_project_name.as_str(),
                    rp,
                    raw,
                )
            );
        }
        if let Some((rp, raw)) = cwd_class.unresolvable.first() {
            anyhow::bail!(
                "{}",
                unresolvable_lock_refusal(
                    Side::Destination,
                    &cwd_workspace_name_str,
                    cwd_project_name.as_str(),
                    rp,
                    raw,
                )
            );
        }

        // Source side: refuse on any non-`ok`, non-`Ahead` relation. (`Ahead` is
        // handled per verb below.)
        let source_anomalous: Vec<&RepoRelation> = source_class
            .relations
            .iter()
            .filter(|r| !matches!(r.relation, LockRelation::Ok | LockRelation::Ahead))
            .collect();
        if let Some(msg) = lock_relation_refusal(
            Side::Source,
            &source_workspace_name,
            source_project_name.as_str(),
            &source_anomalous,
        ) {
            anyhow::bail!("{msg}");
        }

        match verb {
            MachineVerb::Sync => {
                // Pull SOURCE `Ahead` = lock behind HEAD (source advanced but has
                // not relocked). Workweave source: tips-as-truth (note, pull
                // committed tips, leave the source lock alone). Primary source:
                // keep the refusal (scope decision).
                let source_lock_behind: Vec<&RepoRelation> = source_class
                    .relations
                    .iter()
                    .filter(|r| r.relation == LockRelation::Ahead)
                    .collect();
                if !source_lock_behind.is_empty() {
                    if source_is_workweave {
                        if emit_text {
                            for r in &source_lock_behind {
                                let n = r
                                    .ahead_count
                                    .map(|c| c.to_string())
                                    .unwrap_or_else(|| "?".to_string());
                                eprintln!(
                                    "note: {source_workspace_name}/{}: source lock behind HEAD \
                                     by {n} commits — pulling committed tips (source lock left \
                                     alone; its next op heals it)",
                                    r.repo_path,
                                );
                            }
                        }
                    } else if let Some(msg) = lock_relation_refusal(
                        Side::Source,
                        &source_workspace_name,
                        source_project_name.as_str(),
                        &source_lock_behind,
                    ) {
                        anyhow::bail!("{msg}");
                    }
                }

                // Pull DESTINATION (CWD): held at pre-Design-B behavior — refuse
                // on ANY non-`ok` relation (including `Ahead`).
                let cwd_stale: Vec<&RepoRelation> = cwd_class
                    .relations
                    .iter()
                    .filter(|r| r.relation != LockRelation::Ok)
                    .collect();
                if let Some(msg) = lock_relation_refusal(
                    Side::Destination,
                    &cwd_workspace_name_str,
                    cwd_project_name.as_str(),
                    &cwd_stale,
                ) {
                    anyhow::bail!("{msg}");
                }
            }
            MachineVerb::SyncTo => {
                // Landing DESTINATION (CWD): refuse on any non-`ok`, non-`Ahead`
                // relation; a lock-behind-HEAD (`Ahead`) CWD auto-relocks at op
                // start (LOUD line + commit count below).
                let cwd_anomalous: Vec<&RepoRelation> = cwd_class
                    .relations
                    .iter()
                    .filter(|r| !matches!(r.relation, LockRelation::Ok | LockRelation::Ahead))
                    .collect();
                if let Some(msg) = lock_relation_refusal(
                    Side::Destination,
                    &cwd_workspace_name_str,
                    cwd_project_name.as_str(),
                    &cwd_anomalous,
                ) {
                    anyhow::bail!("{msg}");
                }
                cwd_lock_behind = cwd_class
                    .relations
                    .into_iter()
                    .filter(|r| r.relation == LockRelation::Ahead)
                    .collect();
            }
        }
    }
    if matches!(strategy, SyncStrategy::Rebase) {
        verify_replay_exclusion_invariant(&cwd_project_dir)?;
    }
    let cwd_project_tip = GitVcs
        .head_revision(&cwd_project_dir)
        .context("failed to read CWD project HEAD")?;
    let phase1_ancestor_bypassed = if discard_local_commits {
        if GitVcs
            .has_uncommitted_changes(&cwd_project_dir)
            .unwrap_or(true)
        {
            anyhow::bail!(
                "--discard-local-commits precondition failed: project repo at {} has uncommitted \
                 changes.\n\
                 --discard-local-commits discards committed divergence (recoverable via \
                 refs/rwv/pre-op), but the hard-reset would destroy uncommitted changes \
                 unrecoverably. Commit or stash them, then re-run.",
                cwd_project_dir.display(),
            );
        }
        !cwd_is_ancestor_or_equal(
            &cwd_project_dir,
            &cwd_project_tip,
            &snapshot.source_project_tip,
        )
    } else if strategy == SyncStrategy::Ff && matches!(verb, MachineVerb::Sync) {
        // Plain sync + ff: CWD must be ancestor-or-equal of source.
        // sync-to's ff precondition was checked separately above
        // (CWD must be strictly AHEAD of target).
        check_phase1_ancestor(
            &cwd_project_dir,
            &cwd_project_tip,
            &snapshot.source_project_tip,
            &cwd_workspace_name_str,
            &source_workspace_name,
        )?;
        false
    } else {
        false
    };

    // NOTE: the concurrency guard (`check_no_op_in_progress`) now runs as the
    // FIRST precondition above, before lock-relation classification and the
    // dirty preflights (Correction 1, ORDERING). It is intentionally not
    // repeated here.

    // === Mark: write owner record + leases ===

    let op_id = OpId::new_now();
    let owner_workspace_dir = cwd_workspace_dir.clone();

    let mut record = match verb {
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
    if allow_stale_lock {
        // Record that the lock-freshness precondition was bypassed so audit
        // trails and --continue resumptions carry the same consent.
        record.overrides.push("allow-stale-lock".to_owned());
    }
    if phase1_ancestor_bypassed {
        // §7-style named consent: --discard-local-commits will discard
        // reachable project commits in Phase 1'. Recorded in the audit-trail
        // `overrides` field so cleanup preserves the project savepoint as a
        // tombstone and --continue resumes with the same consent.
        record.overrides.push("discard-local-commits".to_owned());
    }
    op_state::write_owner(&owner_workspace_dir, &record).context("failed to write owner record")?;

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
    // and abort can both restore.
    //
    // For sync-to we ALSO savepoint the target's repos. Advance-target
    // ff-advances every target repo to CWD's converged tips, which is a
    // destructive move from the target's perspective (the pre-op tip is
    // overwritten). Abort from phase=retire (or any phase after advance-target)
    // must restore the target's repos to their pre-op state, and the only
    // way to do that symmetrically is via the same savepoint mechanism used
    // on the CWD side. Sibling .4 (abort hardening) verifies these refs
    // against HEAD; we create them here so the anchor exists.
    create_savepoint(&cwd_project_dir, &op_id)?;
    for repo_path in cwd_project.manifest.iter_repo_paths() {
        let abs = cwd_workspace_dir.join(repo_path.as_path());
        // Skip reference symlinks: a savepoint here would write
        // `refs/rwv/pre-op/*` into the shared canonical store.
        if checkout_is_syncable(&abs) {
            let _ = create_savepoint(&abs, &op_id);
        }
    }

    // Sync-to: also savepoint the target's repos so abort can restore them.
    //
    // Target repos may share a git object store with CWD repos (worktree
    // topology). Using the same op_id would create the same ref on both sides;
    // the CWD restore would drop it, leaving the target restore unable to find
    // it. `target_savepoint_id` appends "-target" to the op_id to give target
    // repos their own ref namespace.
    if matches!(verb, MachineVerb::SyncTo) {
        let tsp_id = OpId::from_string(target_savepoint_id(&op_id));
        // Load the target's project to enumerate its repos. Best-effort:
        // if we cannot load it (e.g. project not yet materialised) skip
        // — abort will just leave those repos unchanged.
        let target_project_name = {
            let override_arg = Some(cwd_project_name.clone());
            let tc = WorkspaceContext::resolve(&dest_workspace_dir, override_arg);
            tc.ok().and_then(|c| find_project_name(&c).ok())
        };
        if let Some(tpname) = target_project_name {
            let target_project_dir = dest_workspace_dir.join("projects").join(&tpname);
            let _ = create_savepoint(&target_project_dir, &tsp_id);
            if let Ok(tp) = crate::manifest::Project::from_dir_skip_lock(&target_project_dir) {
                for repo_path in tp.manifest.iter_repo_paths() {
                    let abs = dest_workspace_dir.join(repo_path.as_path());
                    // Skip reference symlinks (shared canonical store).
                    if checkout_is_syncable(&abs) {
                        let _ = create_savepoint(&abs, &tsp_id);
                    }
                }
            }
        }
    }

    // === Op-start auto-relock (§2, sync-to landing) ===
    //
    // Runs AFTER savepoints so abort can roll the relock commit back with the
    // rest of the op. When CWD's manifest repos have a lock behind HEAD (relation
    // `Ahead` — new commits since the last relock, the benign landing shape),
    // emit one LOUD line per repo INCLUDING the commit count, then
    // regenerate+commit CWD's `rwv.lock` so the landing never propagates a lock
    // that mismatches the tips it lands. Phase 3 re-runs relock at op end
    // idempotently; doing it here surfaces the surprising number at the moment it
    // matters (the ancestry-gate guardrail). Best-effort: a relock-commit failure
    // here does not abort the op — Phase 3 will still regenerate the lock
    // post-replay.
    if !cwd_lock_behind.is_empty() {
        if emit_text {
            for r in &cwd_lock_behind {
                let n = r
                    .ahead_count
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".to_string());
                eprintln!(
                    "{cwd_workspace_name_str}/{}: lock behind HEAD by {n} commits — auto-relocked",
                    r.repo_path,
                );
            }
        }
        if let Err(e) = regenerate_lock_phase3(
            &cwd_ctx,
            &cwd_project_dir,
            &cwd_project,
            &source_workspace_name,
        ) {
            if emit_text {
                eprintln!(
                    "warning: op-start relock could not commit ({e}); Phase 3 will retry \
                     post-replay"
                );
            }
        }
    }

    Ok(OpContext {
        cwd_ctx,
        cwd_workspace_dir,
        owner_workspace_dir,
        source_workspace_name,
        dest_workspace_dir,
        dest_project_dir,
        cwd_project_dir,
        cwd_project_name,
        cli_path,
        strategy,
        discard_local_commits: phase1_ancestor_bypassed,
        retire,
        jobs,
        handler,
        verb: verb.op_verb(),
        op_id,
        snapshot,
    })
}

/// Load context for `--continue`: read the owner record (following a lease
/// pointer if invoked from a non-owner workspace), derive all op parameters
/// from it, and rebuild the [`OpContext`].
fn load_continuing_context<'a>(
    verb: MachineVerb,
    cwd: &Path,
    request: &SyncRequest,
    handler: &'a dyn OutputHandler,
) -> anyhow::Result<OpContext<'a>> {
    let project_override = request.project_override.clone();
    let jobs = request.jobs;

    let emit_text = handler.emit_text();

    // The literal invocation CWD is only used to locate op-state; resolving it
    // as a WorkspaceContext here lets `op_state::resume` follow a lease pointer
    // (when `--continue` was invoked from the leased target). Everything the
    // engine consumes is rooted at the OWNER below — design §3: "`--continue`
    // / `abort` invoked from a leased workspace follow the pointer to the
    // owner record and operate identically to owner-side invocation."
    let invocation_ctx = WorkspaceContext::resolve(cwd, project_override.clone())?;
    let invocation_workspace_dir = invocation_ctx.active_path().to_path_buf();

    let (record, owner_workspace_dir) = op_state::resume(&invocation_workspace_dir)?;
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

    // Re-root the engine context at the OWNER workspace. When `--continue` is
    // invoked from the leased target, `invocation_workspace_dir` points at the
    // target — but every phase (replay's per-repo enumeration, materialize,
    // record_converged_tips, cleanup's savepoint drop, retire's workweave
    // identity check) must operate on the owner's workspace, not the target's.
    // When CWD == owner, `cwd_ctx == invocation_ctx` and this is a no-op.
    let cwd_ctx = if owner_workspace_dir == invocation_workspace_dir {
        invocation_ctx
    } else {
        WorkspaceContext::resolve(&owner_workspace_dir, project_override.clone())?
    };
    let cwd_workspace_dir = owner_workspace_dir.clone();

    // Resolve source/dest workspaces by verb. OwnerRecord's `source`/`target`
    // are operator-semantic ("where work came from / where it's going").
    // The engine semantics:
    //   plain sync:     engine.source = record.source, engine.dest = record.target (== owner CWD)
    //   sync-to:        engine.source = record.target (replay pulls from target),
    //                   engine.dest   = record.target (advance-target writes target).
    //                   record.source (owner CWD) is tracked separately via cwd_project_dir.
    let cwd_project_name = find_project_name(&cwd_ctx)?;
    let cwd_project_dir = owner_workspace_dir.join("projects").join(&cwd_project_name);

    let (source_workspace_dir, dest_workspace_dir, cli_path) = match recorded_verb {
        MachineVerb::Sync => (
            record.source.clone(),
            record.target.clone(),
            record.source.clone(),
        ),
        MachineVerb::SyncTo => (
            record.target.clone(),
            record.target.clone(),
            record.target.clone(),
        ),
    };

    let other_project_override = match (recorded_verb, &cwd_ctx.location) {
        // sync-to: target must resolve to CWD's (== owner's) project.
        (MachineVerb::SyncTo, _) => Some(cwd_project_name.clone()),
        (_, WorkspaceLocation::Workweave { project, .. }) => Some(project.clone()),
        (_, WorkspaceLocation::Weave { .. }) => project_override.clone(),
    };

    let (source_project_dir, source_workspace_name) = {
        let source_ctx = WorkspaceContext::resolve(&source_workspace_dir, other_project_override)?;
        let pname = find_project_name(&source_ctx)?;
        let dir = source_ctx.active_path().join("projects").join(&pname);
        (dir, workspace_name(&source_ctx))
    };

    let dest_project_dir = match recorded_verb {
        MachineVerb::Sync => cwd_project_dir.clone(),
        MachineVerb::SyncTo => source_project_dir.clone(),
    };

    // Re-pin the source snapshot for this --continue session. The source's
    // T0 is "the start of the (resumed) replay" — re-pinning here gives
    // replay's re-entry rule a coherent set of inputs. Per-repo no-op
    // detection handles already-converged repos cleanly.
    let snapshot = pin_source_snapshot(&source_project_dir)?;

    // --continue resumes with the same consents recorded at fresh-start
    // time: read `overrides` and re-derive each named override from the
    // persisted record so the resumed session behaves identically to the
    // original without requiring the operator to re-supply flags.
    // Note: `allow-stale-lock` was checked only in guard (not needed in
    // resumption phases); only `discard-local-commits` gates Phase 1'.
    let discard_local_commits_resumed = record
        .overrides
        .iter()
        .any(|o| o == "discard-local-commits");

    Ok(OpContext {
        cwd_ctx,
        cwd_workspace_dir,
        owner_workspace_dir,
        source_workspace_name,
        dest_workspace_dir,
        dest_project_dir,
        cwd_project_dir,
        cwd_project_name,
        cli_path,
        strategy,
        discard_local_commits: discard_local_commits_resumed,
        retire: record.retire,
        jobs,
        handler,
        verb: record.verb,
        op_id,
        snapshot,
    })
}

fn verbs_match(invoked: MachineVerb, recorded: MachineVerb) -> bool {
    matches!(
        (invoked, recorded),
        (MachineVerb::Sync, MachineVerb::Sync) | (MachineVerb::SyncTo, MachineVerb::SyncTo)
    )
}

// ---------------------------------------------------------------------------
// Pre-loop helpers (precondition checks used by guard_and_mark)
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
        // Skip reference symlinks: a dirty shared canonical must not block a
        // sync-to that never touches it (advance-target excludes it too).
        if checkout_is_syncable(&target_repo)
            && GitVcs.has_uncommitted_changes(&target_repo).unwrap_or(true)
        {
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

/// The lock file path relative to a project directory. Its tracked-dirty state
/// is carved out of the source preflight (it is the auto-relock's own input).
const RWV_LOCK_FILE: &str = "rwv.lock";

/// sync-to source-side cleanliness preflight (`fo-4rpnkm.1` §1, Correction 3).
///
/// Before writing op-state or touching any repo, refuse if the CWD side (the
/// operator's own workspace — where replay runs) carries uncommitted **tracked**
/// changes in any manifest repo or the project repo. This defines the
/// "half-rebased op with a pre-rebase lock" state out of existence for the
/// dirty-tree class: today the blast radius depends on repo iteration order —
/// repos before the dirty one get rebased, the lock goes stale, then the op dies
/// mid-replay. One refusal naming every dirty path, before anything rebases.
///
/// - **Untracked files are fine** — they survive the replay untouched, matching
///   the intent recorded in the spec (a scratch file must not block a landing).
///   Only tracked modifications (staged or unstaged) refuse.
/// - **Carve-out:** a dirty `projects/<p>/rwv.lock` *alone* is NOT dirt — it is
///   the auto-relock's own input (§2) and the op commits it. A project repo that
///   is dirty *only* in `rwv.lock` passes; any other tracked project-repo change
///   still refuses (and the project entry names the specific files so the lock
///   carve-out is auditable).
fn check_dirty_source_preflight(
    cwd_project: &Project,
    cwd_workspace_dir: &Path,
    cwd_project_dir: &Path,
    cwd_path: &Path,
) -> anyhow::Result<()> {
    let mut dirty: Vec<String> = Vec::new();

    for repo_path in cwd_project.manifest.iter_repo_paths() {
        let repo = cwd_workspace_dir.join(repo_path.as_path());
        // Skip reference symlinks: a dirty shared canonical must not block a
        // sync-to that never rebases it (replay excludes it too).
        if checkout_is_syncable(&repo) {
            let tracked = GitVcs::tracked_dirty_file_names(&repo).unwrap_or_else(|_| {
                // Treat an unreadable repo as dirty so we fail closed rather
                // than silently rebasing over an unknown state.
                vec!["(status unreadable)".to_string()]
            });
            if !tracked.is_empty() {
                dirty.push(repo_path.to_string());
            }
        }
    }

    // Project repo: apply the rwv.lock carve-out. A project repo dirty ONLY in
    // rwv.lock is the auto-relock's expected input, not dirt.
    let project_tracked = GitVcs::tracked_dirty_file_names(cwd_project_dir)
        .unwrap_or_else(|_| vec!["(status unreadable)".to_string()]);
    let non_lock: Vec<&String> = project_tracked
        .iter()
        .filter(|p| p.as_str() != RWV_LOCK_FILE)
        .collect();
    if !non_lock.is_empty() {
        // Name the specific tracked files so the operator sees exactly what
        // refuses (and can confirm the rwv.lock carve-out was applied).
        let files = non_lock
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        dirty.push(format!("(project): {files}"));
    }

    if !dirty.is_empty() {
        anyhow::bail!(
            "sync-to precondition failed: source workspace has uncommitted tracked changes in:\n  {}\n\
             \n\
             replay rebases these repos onto the target and regenerates the lock; committing \
             mid-op would leave a half-rebased op with a stale lock. Commit or stash the tracked \
             changes in the source ({}), then re-run. (Untracked files are fine; a dirty rwv.lock \
             alone is committed by the op.)",
            dirty.join("\n  "),
            cwd_path.display(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Benign-staleness classification (`fo-4rpnkm.1` §2, Correction 2)
// ---------------------------------------------------------------------------
//
// At op start each manifest repo's committed lock SHA is classified against its
// on-disk HEAD via the SAME per-repo relation vocabulary `rwv status` surfaces
// ([`crate::status::LockRelation`] — no parallel enum).
//
// TERMINOLOGY (load-bearing — the spec and the enum name this from opposite
// vantage points): the spec's "lock behind HEAD" (lock is a strict ancestor of
// HEAD — new commits since the last lock, the normal shape of in-progress work)
// is [`LockRelation::Ahead`] — the *tip* is ahead of the lock. That relation is
// the BENIGN case: a landing auto-relocks, a pull takes the source's committed
// tips. The spec's "ahead" case (HEAD is a strict ancestor of the lock — a reset
// or an `update` without FF) is [`LockRelation::Behind`] — the *tip* is behind
// the lock; that is anomalous and refuses. Every non-`ok` relation other than
// `Ahead` (i.e. `Behind` / `Diverged` / `NoLock` / `Unknown` / `Missing` /
// `Unreachable`) hard-refuses, naming the relation. The ancestry gate is the
// whole answer — a benign `Ahead` cannot conceal divergence, because the
// "missing" lock entries are exactly the commits the op will replay/FF.
//
// `Missing` (clone dir absent from disk) and `Unreachable` (clone present but
// locked SHA not in the local object store) are clone-health states; sync skips
// repos that don't exist on disk (`if !repo_abs.exists() { continue }`) so
// these relations are only emitted by `rwv status` — the sync lock-freshness
// gate never sees them.

/// One manifest repo's lock↔HEAD relation, plus the commit count for the benign
/// `Ahead` case (the tip is ahead of the lock — "lock behind HEAD" in the spec's
/// phrasing). The count makes the surprising number visible at the moment it
/// matters — the LOUD auto-relock line.
struct RepoRelation {
    repo_path: RepoPath,
    relation: LockRelation,
    /// `Some(n)` only when `relation == Ahead`: how many commits HEAD is ahead
    /// of the lock (`lock..HEAD`). `None` for every other relation.
    ahead_count: Option<usize>,
}

/// Result of classifying a workspace's committed lock against its repos: the
/// per-repo relations, plus any lock entries whose pinned revision could not be
/// resolved on disk (a tag/branch that no longer exists). Unresolvable entries
/// are a corrupt-lock error distinct from any relation and are reported first
/// (naming the unknown revision), preserving the old lock-freshness diagnostic.
struct LockClassification {
    relations: Vec<RepoRelation>,
    unresolvable: Vec<(RepoPath, crate::vcs::RawRevisionId)>,
}

/// Classify every manifest repo's committed lock SHA against its on-disk HEAD.
///
/// Reference-symlink checkouts are skipped (they alias the shared canonical and
/// are never rebased). Repos missing on disk are skipped (nothing to classify).
/// The lock is resolved against the workspace so tag/branch/SHA lock forms all
/// compare as canonical SHAs, exactly like `rwv status`. `lock` is `None` when
/// the project carries no committed lock at all (every entry then classifies as
/// `no-lock`). Lock entries whose revision does not resolve on disk are returned
/// in `unresolvable` rather than silently dropped.
fn classify_lock_relations(
    workspace_dir: &Path,
    manifest: &Manifest,
    lock: Option<&LockFile>,
) -> LockClassification {
    let (resolved_lock, unresolvable) = match lock {
        Some(raw) => {
            let (resolved, failures) = raw.clone().resolve_versions(workspace_dir);
            (Some(resolved), failures)
        }
        None => (None, Vec::new()),
    };

    let mut out = Vec::new();
    for repo_path in manifest.iter_repo_paths() {
        let repo_abs = workspace_dir.join(repo_path.as_path());
        // Reference aliases are read-only and never rebased; do not classify.
        if !checkout_is_syncable(&repo_abs) {
            continue;
        }
        if !repo_abs.exists() {
            continue;
        }
        let tip = GitVcs.head_revision(&repo_abs).ok();
        // A repo whose lock entry failed to resolve is reported via
        // `unresolvable` (a corrupt-lock error), not as a `no-lock` relation —
        // skip it here to avoid a double-report.
        if unresolvable.iter().any(|(p, _)| p == repo_path) {
            continue;
        }
        let lock_sha = resolved_lock
            .as_ref()
            .and_then(|l| l.get_entry(repo_path))
            .map(|e| e.version.clone());
        let relation = compute_relation(&repo_abs, &tip, &lock_sha);
        let ahead_count = if relation == LockRelation::Ahead {
            // `Ahead` ⟺ lock is a strict ancestor of tip (both present, per
            // compute_relation). Count lock..HEAD — the commits the landing
            // replays / the auto-relock pins.
            match (lock_sha.as_ref(), tip.as_ref()) {
                (Some(lock), Some(tip)) => GitVcs.count_commits_in_range(&repo_abs, lock, tip).ok(),
                _ => None,
            }
        } else {
            None
        };
        out.push(RepoRelation {
            repo_path: repo_path.clone(),
            relation,
            ahead_count,
        });
    }
    LockClassification {
        relations: out,
        unresolvable,
    }
}

/// Refusal naming the first unresolvable lock entry (a tag/branch the lock pins
/// that no longer exists on disk). Distinct from a relation — the lock itself is
/// corrupt. Preserves the old lock-freshness "unknown revision" diagnostic,
/// including the `--project <p>`-qualified recovery hint (fo-mxw3ew) and the
/// `--allow-stale-lock` escape hatch.
fn unresolvable_lock_refusal(
    side: Side,
    workspace_name: &str,
    project_name: &str,
    repo_path: &RepoPath,
    raw_version: &crate::vcs::RawRevisionId,
) -> String {
    let side_str = side.as_str();
    let raw = raw_version.as_str();
    // Atomic `--commit` form (Correction 4): the two-step `rwv lock` +
    // "commit before syncing" teaches the broken pattern where a
    // written-but-unstaged `rwv.lock` then kills the re-run mid-op. `rwv lock
    // --commit` writes AND commits in one step.
    let recovery = match side {
        Side::Source => format!(
            "Run `rwv lock --commit --project {project_name}` in the source workspace before \
             syncing"
        ),
        Side::Destination => {
            format!("Run `rwv lock --commit --project {project_name}` to refresh before syncing")
        }
    };
    format!(
        "lock-freshness precondition failed: {side_str} workspace '{workspace_name}' lock \
         references unknown revision {raw} for {repo_path}.\n\
         \n\
         Usual fix: {recovery}.\n\
         To skip this check: pass `--allow-stale-lock` (use when you know the lock is \
         intentionally ahead of HEAD).",
    )
}

/// Build a lock-freshness refusal naming each offending repo, its relation, and
/// the recovery path — the single refusal used for every non-benign lock-relation
/// gate (anomalous relations on either side; a primary-weave source or a pull
/// destination whose lock is behind HEAD). Returns `None` when `offending` is
/// empty (nothing to refuse).
///
/// Preserves the documented `lock-freshness precondition` phrase (cli.md §sync
/// `--allow-stale-lock` row), the "stale lock" wording, the `--project <p>`-
/// qualified recovery hint (fo-mxw3ew), and the `--allow-stale-lock` escape
/// hatch, while additionally NAMING each repo's relation (§2 guardrail). A
/// `diverged` repo also earns the `rwv lock --commit` bless-HEAD hint.
fn lock_relation_refusal(
    side: Side,
    workspace_name: &str,
    project_name: &str,
    offending: &[&RepoRelation],
) -> Option<String> {
    if offending.is_empty() {
        return None;
    }
    let mut any_diverged = false;
    let mut lines = String::new();
    for r in offending {
        if r.relation == LockRelation::Diverged {
            any_diverged = true;
        }
        // Phrase the relation from a single fixed vantage point (HEAD relative to
        // lock) to match `LockRelation`'s tip-relative naming: `HEAD ahead of
        // lock` is "lock behind HEAD" (a primary source / pull destination that
        // hasn't relocked), `HEAD behind lock` is the reset case, etc.
        lines.push_str(&format!("\n  {}: HEAD {} lock", r.repo_path, r.relation));
    }
    let side_str = side.as_str();
    // Atomic `--commit` form (Correction 4): teach `rwv lock --commit`, not the
    // two-step `rwv lock` + "commit before syncing" whose written-but-unstaged
    // `rwv.lock` kills a re-run mid-op.
    let recovery = match side {
        Side::Source => format!(
            "Run `rwv lock --commit --project {project_name}` in the source workspace before \
             syncing"
        ),
        Side::Destination => {
            format!("Run `rwv lock --commit --project {project_name}` to refresh before syncing")
        }
    };
    // Lead with the documented phrase (`lock-freshness precondition`) so the
    // `--allow-stale-lock` doc stays accurate; keep the "stale lock" wording the
    // detailed message tests assert on.
    let mut msg = format!(
        "lock-freshness precondition failed: {side_str} workspace '{workspace_name}' has a stale \
         lock — the lock↔HEAD relation is not `ok` for:{lines}\n\
         \n\
         `ahead` (HEAD ahead of lock) means the lock is behind HEAD; on a primary source or a \
         pull destination this is only accepted with consent. `behind` = the lock records \
         commits HEAD lacks (a reset, or `update` without fast-forward). `diverged` = the lock \
         and HEAD have rewritten past a shared base. `no-lock` / `unknown` = no comparable lock \
         entry.\n\
         \n\
         Usual fix: {recovery}.\n"
    );
    if any_diverged {
        msg.push_str("\nFor a diverged repo, bless the current HEAD with `rwv lock --commit`.\n");
    }
    msg.push_str(
        "\nTo skip this check: pass `--allow-stale-lock` (use when you know the lock is \
         intentionally ahead of HEAD).",
    );
    Some(msg)
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

    // sync-to with `--strategy=ff`: replay is a no-op (CWD is strictly ahead
    // of target per guard's ff precondition). The advance-target phase does
    // all the work.
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

    // Snapshot was pinned in guard (or re-pinned on --continue). Re-entry
    // rule (§4): per-repo state is derived from the VCS itself — already-
    // converged repos no-op via `sync_one_repo`'s head-equals-target check.
    let snapshot = &ctx.snapshot;

    // Load CWD project (manifest + lock) from disk.
    let cwd_project =
        Project::from_dir(&ctx.cwd_project_dir).context("failed to load CWD project")?;

    // CWD project tip — read before any side effects so Phase 1' has the
    // pre-op starting state for its `cwd_tip == source_tip` short-circuit.
    let cwd_project_tip = GitVcs
        .head_revision(&ctx.cwd_project_dir)
        .context("failed to read CWD project HEAD")?;

    // === Phase 2 (manifest repos) — materialize missing, prune dropped, sync ===

    let mut materialize_failures: Vec<crate::manifest::RepoPath> = Vec::new();
    for repo_path in snapshot.raw_source_lock.iter_repo_paths() {
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        // Skip a checkout that already exists. Also skip a reference symlink:
        // even a *dangling* symlink (whose `exists()` follows the link and
        // returns false) must never be replaced by a `git worktree add` against
        // the shared canonical store. `classify_checkout` keys on `is_symlink`,
        // which does not follow the link, so it catches the dangling case.
        if abs.exists() || classify_checkout(&abs) == CheckoutKind::ReferenceAlias {
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
            // Skip reference symlinks: pruning would `git worktree remove`
            // against the shared canonical store. Unlinking a reference alias
            // is the workweave-delete path's job, not sync's.
            let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
            if classify_checkout(&abs) == CheckoutKind::ReferenceAlias {
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
    let unresolvable: std::collections::BTreeSet<crate::manifest::RepoPath> = source_lock_failures
        .iter()
        .map(|(p, _)| p.clone())
        .collect();

    struct SyncTask {
        repo_path: crate::manifest::RepoPath,
        abs: PathBuf,
        target: ResolvedRevisionId,
    }
    let mut sync_tasks: Vec<SyncTask> = Vec::new();

    for (repo_path, raw_entry) in snapshot.raw_source_lock.iter_entries() {
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        // The reference exclusion: a reference symlink is read-only and aliases
        // the shared canonical store, so it never becomes a sync task. Without
        // a task it is never rebased/ff'd and never gets a planned-target write,
        // so replay Phase 2 cannot move the canonical's branch.
        if !checkout_is_syncable(&abs) {
            if emit_text {
                let reason = if classify_checkout(&abs) == CheckoutKind::ReferenceAlias {
                    "reference (read-only alias)"
                } else {
                    "not on disk"
                };
                println!("  {repo_path}: skipped ({reason})");
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

    // === advanced_tips write 1: pre-write planned targets for genuine ff-movers ===
    //
    // Before the parallel fan-out, classify every sync task: if the repo's
    // current HEAD is a STRICT ancestor of the lock target (head ≠ target AND
    // head ⊏ target), this is a genuine fast-forward and the landing tip is
    // knowable now.  Pre-write target → advanced_tips so abort can attribute the
    // repo the instant it is advanced, with no window (§4 case 1).
    //
    // Repos whose HEAD equals target (NoOp) or whose HEAD is ahead of target
    // (AlreadyAhead) are skipped — savepoint already attributes the no-op case,
    // and recording an unreached target for an already-ahead repo is forgeable
    // (§10 Q4).  Repos with local commits that diverge (not strict ancestors)
    // are skipped here; their fresh rebased tip is captured post-join (write 3).
    {
        let mut entry_tips: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for task in &sync_tasks {
            if let Ok(head) = GitVcs.head_revision(&task.abs) {
                if head != task.target
                    && GitVcs
                        .is_ancestor(&task.abs, &head, &task.target)
                        .unwrap_or(false)
                {
                    entry_tips.insert(
                        task.repo_path.as_str().to_owned(),
                        task.target.as_str().to_owned(),
                    );
                }
            }
        }
        if !entry_tips.is_empty() {
            let mut owner = op_state::read_owner(&ctx.owner_workspace_dir)?.ok_or_else(|| {
                anyhow::anyhow!("internal: owner record missing during replay entry write")
            })?;
            owner
                .tips
                .advanced_mut()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "internal: advanced_tips write after convergence at replay entry"
                    )
                })?
                .extend(entry_tips);
            op_state::write_owner(&ctx.owner_workspace_dir, &owner)
                .context("failed to write advanced_tips at replay entry")?;
        }
    }

    let strategy = ctx.strategy;
    // Return type: (is_failure, Option<actual_head_if_converged>).
    // The actual HEAD is read inside the closure (single-repo reads, no shared
    // state) and returned for the post-join batch write.  No write to the owner
    // record happens inside this closure — that would be a race (§4).
    let task_results: Vec<(bool, Option<String>)> =
        run_in_parallel(&sync_tasks, ctx.jobs, |_idx, task| {
            let outcome = sync_one_repo(&task.abs, &task.target, strategy);
            let is_failure = outcome.is_failure();
            // Capture the actual post-advance HEAD if this task converged.
            // For ff-movers this equals the pre-written target (idempotent
            // overwrite in write 3); for rebased repos it is the fresh SHA.
            let converged_head = if matches!(outcome, RepoSyncOutcome::Converged) {
                GitVcs
                    .head_revision(&task.abs)
                    .ok()
                    .map(|h| h.as_str().to_owned())
            } else {
                None
            };
            if !is_failure {
                GitVcs.refresh_index_to_head_if_safe(&task.abs);
                GitVcs.refresh_working_tree_to_head_if_safe(&task.abs);
            }
            ctx.handler.record(
                task.repo_path.as_str(),
                &task.abs.to_string_lossy(),
                &outcome,
            );
            (is_failure, converged_head)
        });

    // === advanced_tips write 3: batch-write actual tips of converged manifest repos ===
    //
    // Single-threaded post-join, so no race against write_owner.  Overwrites the
    // ff-pre-written entries (same value, idempotent) and captures fresh rebased
    // SHAs for manifest repos that had local commits to replay (§4 case 2, §6).
    // Must precede the any_failure bail so partially-advanced repos are captured
    // even when the overall fan-out fails.
    {
        let post_join_tips: Vec<(String, String)> = sync_tasks
            .iter()
            .zip(task_results.iter())
            .filter_map(|(task, (_, head_opt))| {
                head_opt
                    .as_ref()
                    .map(|h| (task.repo_path.as_str().to_owned(), h.clone()))
            })
            .collect();
        if !post_join_tips.is_empty() {
            let mut owner = op_state::read_owner(&ctx.owner_workspace_dir)?.ok_or_else(|| {
                anyhow::anyhow!("internal: owner record missing during post-fan-out write")
            })?;
            let advanced = owner.tips.advanced_mut().ok_or_else(|| {
                anyhow::anyhow!("internal: advanced_tips write after convergence post fan-out")
            })?;
            for (repo_path, tip) in post_join_tips {
                advanced.insert(repo_path, tip);
            }
            op_state::write_owner(&ctx.owner_workspace_dir, &owner)
                .context("failed to write advanced_tips after fan-out join")?;
        }
    }

    if task_results.iter().any(|(f, _)| *f) {
        any_failure = true;
    }

    if any_failure {
        // A VCS-native resolution hint is only correct when a manifest repo is
        // genuinely left mid-op (Correction 4). Probe the involved repos for a
        // live conflict rather than assuming one from the strategy — a batch of
        // fetch/head-unreadable failures leaves no rebase to `--continue`.
        let live_conflict = sync_tasks.iter().find_map(|t| GitVcs.mid_op(&t.abs));
        anyhow::bail!("{}", manifest_repo_failure_message(ctx.verb, live_conflict));
    }

    // === Phase 1' (project repo) — strategy on the project repo ===

    let phase1_outcome = if ctx.discard_local_commits {
        // --discard-local-commits: hard-reset CWD's project repo to source's
        // tip, discarding any project commits not reachable from source.
        // Guard already refused on uncommitted changes; savepoint was written
        // before this phase so discarded commits stay recoverable via abort.
        GitVcs
            .hard_reset(&ctx.cwd_project_dir, &snapshot.source_project_tip)
            .map_err(anyhow::Error::from)
            .context("project repo hard-reset (--discard-local-commits) failed")
    } else {
        apply_project_strategy(
            &ctx.cwd_project_dir,
            &snapshot.source_project_tip,
            &cwd_project_tip,
            strategy,
            ctx.verb,
        )
    };

    if let Err(e) = phase1_outcome {
        if emit_text {
            eprintln!("Phase 1' (project repo) failed: {e}");
        }
        // Only teach the VCS-native resume when the project repo is actually
        // left mid-op (Correction 4). A `--discard-local-commits` hard-reset
        // failure or a non-conflict rebase error leaves no in-flight VCS op.
        let live_conflict = GitVcs.mid_op(&ctx.cwd_project_dir);
        anyhow::bail!(
            "{}",
            phase1_or_phase3_failure_message(
                Phase::One,
                &ctx.cwd_project_dir,
                ctx.verb,
                live_conflict,
            )
        );
    }

    // === advanced_tips write 2: capture actual post-Phase-1' project repo tip ===
    //
    // Phase 1' may rebase CWD's project commits onto source_project_tip, landing
    // at a fresh SHA T1 that was not knowable at replay entry.  Overwrite
    // advanced_tips["(project)"] with the actual post-rebase HEAD (§4 case 2,
    // §6).  This also covers the ff/discard-local-commits case (tip == source
    // tip, idempotent overwrite).
    {
        let project_tip = GitVcs
            .head_revision(&ctx.cwd_project_dir)
            .context("failed to read project HEAD after Phase 1'")?;
        let mut owner = op_state::read_owner(&ctx.owner_workspace_dir)?
            .ok_or_else(|| anyhow::anyhow!("internal: owner record missing after Phase 1'"))?;
        owner
            .tips
            .advanced_mut()
            .ok_or_else(|| {
                anyhow::anyhow!("internal: advanced_tips write after convergence post Phase 1'")
            })?
            .insert("(project)".to_owned(), project_tip.as_str().to_owned());
        op_state::write_owner(&ctx.owner_workspace_dir, &owner)
            .context("failed to write advanced_tips after Phase 1'")?;
    }

    Ok(())
}

/// Pin the atomic source snapshot at T0: read the source project tip once,
/// then read source manifest + lock AT that revision. Combined with the
/// no-op-in-progress check on the source (in `check_no_op_in_progress`),
/// source reads are effectively serialisable with no locks (§6).
fn pin_source_snapshot(source_project_dir: &Path) -> anyhow::Result<SourceSnapshot> {
    let source_project_tip = GitVcs
        .head_revision(source_project_dir)
        .context("failed to read source project HEAD")?;

    let raw_source_lock = {
        let content = GitVcs
            .read_file_at_revision(
                source_project_dir,
                &source_project_tip,
                Path::new("rwv.lock"),
            )
            .with_context(|| {
                format!(
                    "failed to read source lock at revision {} in {}",
                    source_project_tip,
                    source_project_dir.display()
                )
            })?;
        LockFile::from_yaml_str(&content).with_context(|| {
            format!(
                "failed to parse source lock at revision {} in {}",
                source_project_tip,
                source_project_dir.display()
            )
        })?
    };

    let source_manifest = {
        let content = GitVcs
            .read_file_at_revision(
                source_project_dir,
                &source_project_tip,
                Path::new("rwv.yaml"),
            )
            .with_context(|| {
                format!(
                    "failed to read source manifest at revision {} in {}",
                    source_project_tip,
                    source_project_dir.display()
                )
            })?;
        Manifest::from_yaml_str(&content).with_context(|| {
            format!(
                "failed to parse source manifest at revision {} in {}",
                source_project_tip,
                source_project_dir.display()
            )
        })?
    };

    Ok(SourceSnapshot {
        source_project_tip,
        source_manifest,
        raw_source_lock,
    })
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
        // Phase 3 (relock) is a lock regeneration + commit, never a VCS
        // rebase/merge — so there is never a live ConflictOp to resume with a
        // `git … --continue` (Correction 4). Pass `None`: the message points at
        // `rwv {verb} --continue`, not a spurious `git rebase --continue`.
        anyhow::bail!(
            "{}",
            phase1_or_phase3_failure_message(Phase::Three, &ctx.cwd_project_dir, ctx.verb, None,)
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
    // Build the converged table from post-replay HEADs...
    let mut converged: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for repo_path in cwd_project.manifest.iter_repo_paths() {
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            continue;
        }
        if let Ok(rev) = GitVcs.head_revision(&abs) {
            converged.insert(repo_path.as_str().to_owned(), rev.as_str().to_owned());
        }
    }
    if let Ok(rev) = GitVcs.head_revision(&ctx.cwd_project_dir) {
        converged.insert("(project)".to_owned(), rev.as_str().to_owned());
    }
    // ...then swap atomically: `PhaseTips::converge` discards the replay-phase
    // advanced_tips and installs converged_tips in one move, so they land in the
    // SAME persist (§4 "Clearing order"). The ADT makes the both-populated state
    // unrepresentable, so the prior "clear advanced before populating converged"
    // ordering hazard cannot recur.
    owner.tips.converge(converged);
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
        // Skip reference symlinks on either side: ff'ing the target alias would
        // move the shared canonical's branch. Both sides of a reference alias
        // resolve to the same canonical anyway, so there is nothing to advance.
        if !checkout_is_syncable(&cwd_repo) {
            continue;
        }
        if !checkout_is_syncable(&target_repo) {
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
        // Read target tip BEFORE the advance so we can report from_sha.
        let target_tip_before = GitVcs.head_revision(&target_repo).ok();
        match ff_advance_repo(&target_repo, &cwd_repo, &cwd_tip) {
            Ok(()) => {
                if emit_text {
                    println!(
                        "  {}: ff-advanced to {}",
                        repo_path,
                        &cwd_tip.as_str()[..8.min(cwd_tip.as_str().len())]
                    );
                }
                // Record step-3 advance iff the branch pointer actually moved.
                if let Some(ref before) = target_tip_before {
                    if before != &cwd_tip {
                        ctx.handler.record_step3_advance(
                            repo_path.as_str(),
                            before.as_str(),
                            cwd_tip.as_str(),
                        );
                    }
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

    // Read project target tip BEFORE the advance so we can report from_sha.
    let project_target_tip_before = GitVcs.head_revision(&ctx.dest_project_dir).ok();
    match ff_advance_repo(
        &ctx.dest_project_dir,
        &ctx.cwd_project_dir,
        &cwd_project_tip,
    ) {
        Ok(()) => {
            if emit_text {
                println!(
                    "  (project): ff-advanced to {}",
                    &cwd_project_tip.as_str()[..8.min(cwd_project_tip.as_str().len())]
                );
            }
            // Record step-3 advance for the project repo iff it actually moved.
            if let Some(ref before) = project_target_tip_before {
                if before != &cwd_project_tip {
                    ctx.handler.record_step3_advance(
                        "(project)",
                        before.as_str(),
                        cwd_project_tip.as_str(),
                    );
                }
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
// Retire runs the merged-check (manifest repo tips equal on both sides) and
// dirty-check, then calls `delete_workweave`. A failure from any of these
// propagates as an error from `run_retire`, which keeps the op record at
// phase=retire (cleanup never runs on error, per the driver invariant).
//
// Re-entry rule: both the merged-check and dirty-check are read-only; the
// workweave removal itself is idempotent (a missing workweave dir is a no-op
// in delete_workweave). So re-entering retire after a prior failure is safe.
//
// Abort from phase=retire: run_abort scans the target workspace for repos with
// op-id savepoints and restores them to their pre-op state. Target savepoints
// are created in guard_and_mark for verb=SyncTo (this spec's scope); sibling
// .4 adds HEAD-verification on top of the savepoint restore.

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

    // Savepoint refs (`refs/rwv/pre-op/*`) live in the shared clone refdb, not
    // in any worktree, so `git update-ref -d` from ANY live worktree of the
    // same clone drops the shared ref. Crucially, in the `sync-to --retire`
    // flow the phase order is `… → retire → cleanup`: retire deletes CWD's
    // workweave BEFORE cleanup runs, so `ctx.cwd_project_dir` /
    // `ctx.cwd_workspace_dir` now point at a deleted directory. Dropping
    // savepoints through those paths silently no-ops while the ref survives in
    // the surviving clone — the leak this code path fixes (fo-i8eq4e).
    //
    // We therefore target the CANONICAL/PRIMARY clone (`primary_path()`), which
    // survives workweave deletion: workweave repos are `git worktree add`ed
    // from the primary's clones, so the primary holds the shared refdb. When
    // CWD is itself the primary weave (plain `sync` from primary), the
    // canonical path equals CWD, so this is also correct for the non-retire
    // case.
    let primary = ctx.cwd_ctx.primary_path();
    let canonical_project_dir = primary.join("projects").join(ctx.cwd_project_name.as_str());

    // Drop savepoints. Exception: when --discard-local-commits bypassed the
    // Phase 1' ancestor check (recorded as the `discard-local-commits`
    // override), preserve the project savepoint as a tombstone — the only
    // remaining reference to the discarded commits.
    let owner = op_state::read_owner(&ctx.owner_workspace_dir)?;
    let discard_tombstone = owner
        .as_ref()
        .map(|r| r.overrides.iter().any(|o| o == "discard-local-commits"))
        .unwrap_or(false);

    if !discard_tombstone {
        delete_savepoint(&canonical_project_dir, &ctx.op_id);
    } else if emit_text {
        eprintln!(
            "note: --discard-local-commits discarded project commits; pre-sync state preserved at \
             refs/rwv/pre-op/{op_id} (recover with `git reset --hard refs/rwv/pre-op/{op_id}` \
             in {})",
            ctx.cwd_project_dir.display(),
            op_id = ctx.op_id,
        );
    }

    // Manifest savepoints: load the manifest from the canonical project repo
    // (the workweave's may be gone after retire) and drop each repo's savepoint
    // through the canonical clone. A missing ref is a harmless no-op, so no
    // existence guard is needed (and `if abs.exists()` would re-introduce the
    // leak by skipping the now-deleted workweave paths).
    if let Ok(project) = Project::from_dir_skip_lock(&canonical_project_dir) {
        for repo_path in project.manifest.iter_repo_paths() {
            let abs = primary.join(repo_path.as_path());
            delete_savepoint(&abs, &ctx.op_id);
        }
    }

    // Sync-to: drop target savepoints. These use the "<op_id>-target"
    // namespace (see target_savepoint_id). Best-effort: if the target's
    // project dir or repos are gone (e.g. after a retire that deleted the
    // workweave and the target structure changed), silently skip.
    if matches!(ctx.verb, op_state::OpVerb::SyncTo) {
        let tsp_id = OpId::from_string(target_savepoint_id(&ctx.op_id));
        // Try to load target project from dest_workspace_dir.
        let target_project_name = WorkspaceContext::resolve(&ctx.dest_workspace_dir, None)
            .ok()
            .and_then(|c| find_project_name(&c).ok());
        if let Some(tpname) = target_project_name {
            let target_project_dir = ctx.dest_workspace_dir.join("projects").join(&tpname);
            delete_savepoint(&target_project_dir, &tsp_id);
            if let Ok(tp) = Project::from_dir_skip_lock(&target_project_dir) {
                for repo_path in tp.manifest.iter_repo_paths() {
                    let abs = ctx.dest_workspace_dir.join(repo_path.as_path());
                    if abs.exists() {
                        delete_savepoint(&abs, &tsp_id);
                    }
                }
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
/// even the happy path the spec describes. Manifest tip equality is the
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
             \n\
             To reconcile: sync the divergent repo(s), then run:\n\
               rwv sync-to --continue   # re-runs the retire check\n\
             \n\
             To roll back the entire op: `rwv abort`.",
            diverged.join("\n  "),
        );
    }

    // Reuse the shared dirty-path check. Any dirty worktree blocks retire.
    let dirty = crate::workweave::collect_dirty_paths(workweave_dir, project, &manifest);
    if !dirty.is_empty() {
        anyhow::bail!(
            "--retire: workweave has uncommitted changes after sync-to; refusing to delete:\n  {}\n\
             \n\
             Commit or discard the changes, then run:\n\
               rwv sync-to --continue   # re-runs the retire check\n\
             \n\
             To roll back the entire op: `rwv abort`.",
            dirty.join("\n  "),
        );
    }

    // Both invariants hold: delete the workweave. Pass `force: false` —
    // collect_dirty_paths already returned empty, so the inner check is
    // belt-and-braces. Use the primary path (delete_workweave needs to
    // locate the workweave under the primary's parent dir). Use the
    // retire-specific entry point, which skips the cross-verb op guard: THIS op
    // still holds its `.rwv-op` record on the workweave (cleared later in
    // cleanup), so the guard would otherwise refuse the op's own retire.
    crate::workweave::delete_workweave_for_retire(
        ctx.primary_path(),
        project,
        workweave_name,
        false,
    )
    .context("--retire: workweave delete failed")?;

    eprintln!("retired workweave {}", workweave_name.as_str());
    Ok(())
}

/// Truncate a SHA to 12 chars for display (matches workweave.rs convention).
fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// Phase 1': replay CWD's unique project commits onto `source_tip` via
/// `strategy`, relying on `.gitattributes rwv.lock merge=rwv-ours` (configured
/// at `rwv init` time) to silently keep source's version of the lock through
/// the replay. Phase 3 regenerates the lock from manifest tips afterwards.
///
/// - `Ff`: requires CWD ancestor of source (caller already verified). Performs
///   a fast-forward via `git merge --ff-only`.
/// - `Rebase`: native `git rebase` via [`Vcs::rebase`]. On conflict, leaves
///   the repo mid-rebase; the operator resolves, `git add`s, then runs
///   `rwv sync --continue` (or `rwv sync-to --continue` for sync-to).
///
/// Conflicts on non-lock paths halt the operation, leaving the VCS-native
/// in-flight state for the operator to resolve and re-run sync, or
/// `rwv abort`.
///
/// `verb` is the running op's verb ([`OpVerb`]), threaded through so the
/// conflict-bail message shows the correct rwv-native resume command
/// (`rwv sync --continue` / `rwv sync-to --continue`).
fn apply_project_strategy(
    cwd_project_dir: &Path,
    source_tip: &ResolvedRevisionId,
    cwd_tip: &ResolvedRevisionId,
    strategy: SyncStrategy,
    verb: OpVerb,
) -> anyhow::Result<()> {
    // Replay re-entry (`rwv sync --continue`): a project repo that stopped
    // mid-rebase on a conflict has HEAD sitting at `source_tip` (git checked
    // it out as the rebase onto and stopped on the very first pick). The
    // no-op short-circuit below would then trip and return `Ok(())` without
    // ever driving the rebase forward — leaving the repo mid-rebase forever
    // and lying to the caller that Phase 1' completed. Mid-rebase is the
    // signal that trumps head-equality: always route through
    // `Vcs::rebase_continue` (below) when we see it under a rebase strategy.
    let mid_rebase = matches!(GitVcs.mid_op(cwd_project_dir), Some(ConflictOp::Rebase))
        && strategy == SyncStrategy::Rebase;

    if !mid_rebase && cwd_tip == source_tip {
        // No-op.
        return Ok(());
    }

    match strategy {
        SyncStrategy::Ff => {
            // CWD must be ancestor of source (caller verified). Fast-forward.
            GitVcs.advance_if_fast_forward(cwd_project_dir, source_tip)?;
        }
        SyncStrategy::Rebase => {
            // `Vcs::rebase` wires the `merge=rwv-ours` driver inline; lock-only
            // commits become empty patches and git drops them by default
            // (`--empty=drop`), so source's version of `rwv.lock` survives
            // the replay untouched. Phase 3 then regenerates the lock from
            // manifest tips.
            //
            // Replay re-entry (`rwv sync --continue`): if the project repo
            // is already mid-rebase from a previous phase that stopped on a
            // conflict, `Vcs::rebase` would fail immediately ("another
            // rebase is in progress"). Route through `Vcs::rebase_continue`
            // so `resolve → git add → rwv sync --continue` iterates per
            // conflicted pick. `rebase_continue` re-supplies the same
            // inline driver flags, so any remaining lock-only picks merge
            // to ours the same way a fresh `Vcs::rebase` would. A mid-op
            // that is NOT rebase (mid-merge, mid-cherry-pick) is not
            // rwv-initiated for this path — preserve today's behavior of
            // calling `Vcs::rebase`, which will fail loudly rather than
            // silently adopting foreign state.
            let outcome = if mid_rebase {
                GitVcs.rebase_continue(cwd_project_dir)
            } else {
                GitVcs.rebase(cwd_project_dir, source_tip, source_tip)
            };
            match outcome {
                Ok(()) => {}
                Err(VcsError::RebaseConflict { repo, op }) => {
                    let detail = GitVcs.rebase_stopped_commit_detail(&repo);
                    anyhow::bail!(
                        "{}",
                        per_conflict_bail_message(
                            &repo,
                            op,
                            "rebase (project repo)",
                            &detail,
                            verb,
                        )
                    );
                }
                Err(e) => anyhow::bail!("project repo rebase failed: {e}"),
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

    let message = auto_relock_commit_message(source_workspace_name);
    if commit_lock_file_with_message(cwd_project_dir, &message)? {
        eprintln!("  (project): re-locked after sync from {source_workspace_name}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// rwv abort
// ---------------------------------------------------------------------------

/// Workspace identity comparison for abort: canonicalize both sides
/// (fall back to the raw path when canonicalization fails, e.g. the
/// workspace was deleted). Op records hold operator-supplied paths
/// verbatim (`SyncSource::Path` does not canonicalize), so a recorded
/// path may reach the same workspace through a symlink — e.g. macOS's
/// `/var` → `/private/var` tempdirs, or a symlinked weaveroot — while
/// `WorkspaceContext::resolve` always canonicalizes. Textual comparison
/// would misclassify the workspace and pick the wrong savepoint namespace.
fn same_workspace(a: &Path, b: &Path) -> bool {
    let ca = a.canonicalize().unwrap_or_else(|_| a.to_path_buf());
    let cb = b.canonicalize().unwrap_or_else(|_| b.to_path_buf());
    ca == cb
}

/// Execute `rwv abort` — verified-restore CWD workspace to its pre-sync state.
///
/// Reads the op-state file (`.rwv-op`) to find the op-id, the involved
/// workspaces, and the per-repo `converged_tips` recorded at relock. For
/// `sync-to` ops, both CWD and the recorded target workspace are rolled
/// back.
///
/// Two hardening rails (design § 5, fo-jsbr3i.4):
///
/// 1. **Pre-abort reference**: BEFORE restoring any repo, a durable
///    [`Vcs::create_pre_abort_ref`] reference is written at the repo's
///    current tip. The reference is never deleted by abort's cleanup —
///    abort is itself information-preserving and undoable via the ref.
/// 2. **HEAD-verified restore**: the `reset --hard` to the savepoint
///    happens ONLY if the repo's current tip is attributable to the op
///    (== savepoint, == recorded converged tip, or a VCS-native mid-op
///    state). Anything else is reported with a named `foreign-tip`
///    violation and recovery hints, and the repo's tip is left untouched.
///    The op-state is RETAINED on a foreign-tip refusal so the operator
///    can re-run `rwv abort` after manually reconciling.
pub fn run_abort(cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let workspace_dir = ctx.active_path().to_path_buf();

    // resolve_to_owner follows a lease pointer if the workspace holds a lease,
    // so `rwv abort` invoked from either the owner or a leased workspace finds
    // the same full record. `workspace_dir` is still used for the repo scan below.
    let (op_id, owner_record, extra_workspace_dirs): (OpId, OwnerRecord, Vec<PathBuf>) =
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
                    if same_workspace(&resolved.owner_workspace, &workspace_dir) {
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
                (
                    OpId::from_string(resolved.record.id.clone()),
                    resolved.record,
                    extras,
                )
            }
            None => anyhow::bail!("no operation in progress"),
        };
    // The op's tip table is phase-scoped (`PhaseTips`): exactly one of the two
    // tables is populated at a time. `converged_tips` is the per-repo
    // attributable-tip table. Keys: repo path string (e.g. `github/foo/bar`)
    // for manifest repos, `"(project)"` for the project repo. Empty before
    // relock completes — in that case the attributable set reduces to
    // {savepoint, advanced_tips, mid-op}.
    //
    // `advanced_tips` is the op's replay-phase intent: the planned target
    // (ff advances) or captured actual tip (rebased advances), written before
    // or right after each advance. Source/owner side only — target tips land
    // in converged_tips post-relock (§7). Empty for pre-field records and
    // once converged — graceful degradation to pre-change behavior. The
    // inactive half reads as an empty map so the `.get()` lookups below are
    // unchanged.
    let empty_tips = std::collections::BTreeMap::new();
    let converged_tips = owner_record.tips.converged().unwrap_or(&empty_tips);
    let advanced_tips = owner_record.tips.advanced().unwrap_or(&empty_tips);

    // Side-specific restore ids: repos in the op's TARGET workspace were
    // savepointed under `<op_id>-target` (see `target_savepoint_id`) so that
    // worktree pairs sharing one refdb don't collide on savepoint or
    // pre-abort ref names; the owner/source side uses the base op id.
    // Keyed by WORKSPACE IDENTITY (is this the recorded target?), not by
    // invocation side — abort may be invoked from either workspace, which
    // inverts the cwd/extras loop-to-workspace mapping.
    let restore_id_for = |ws: &Path| -> OpId {
        if owner_record.verb == crate::op_state::OpVerb::SyncTo
            && same_workspace(ws, &owner_record.target)
        {
            OpId::from_string(target_savepoint_id(&op_id))
        } else {
            op_id.clone()
        }
    };
    let cwd_restore_id = restore_id_for(&workspace_dir);

    let cwd_project_name = find_project_name(&ctx)?;
    let cwd_project_dir = workspace_dir.join("projects").join(&cwd_project_name);
    // Use the lockless loader: abort's contract is "the state is bad, get me
    // out". rwv.lock may contain git conflict markers from the half-completed
    // rebase, so we must not try to parse it. The abort path only needs the
    // manifest (to enumerate repo paths); it never reads lock contents.
    let cwd_project =
        Project::from_dir_skip_lock(&cwd_project_dir).context("failed to load CWD project")?;

    let mut any_failure = false;
    let mut any_foreign = false;
    let mut noise_summary = AbortNoiseSummary::default();

    // Restore CWD manifest repos first.
    for repo_path in cwd_project.manifest.iter_repo_paths() {
        let abs = workspace_dir.join(repo_path.as_path());
        // Skip reference symlinks: `reset --hard` here would rewind the shared
        // canonical store. Sync never savepoints or advances a reference (the
        // savepoint/replay/advance loops exclude it via the same predicate), so
        // there is by construction nothing to restore.
        if !checkout_is_syncable(&abs) {
            continue;
        }
        let intent = advanced_tips.get(repo_path.as_str()).map(String::as_str);
        let converged = converged_tips.get(repo_path.as_str()).map(String::as_str);
        match abort_one_repo(&abs, &cwd_restore_id, intent, converged) {
            Ok(outcome) => report_abort_outcome(
                repo_path.as_str(),
                &outcome,
                Some(abs.as_path()),
                &mut noise_summary,
                &mut any_foreign,
            ),
            Err(e) => {
                eprintln!("  {repo_path}: {e}");
                any_failure = true;
            }
        }
    }

    // Restore CWD project repo.
    let project_intent = advanced_tips.get("(project)").map(String::as_str);
    let project_converged = converged_tips.get("(project)").map(String::as_str);
    match abort_one_repo(
        &cwd_project_dir,
        &cwd_restore_id,
        project_intent,
        project_converged,
    ) {
        Ok(outcome) => report_abort_outcome(
            "(project)",
            &outcome,
            Some(cwd_project_dir.as_path()),
            &mut noise_summary,
            &mut any_foreign,
        ),
        Err(e) => {
            eprintln!("  (project): {e}");
            any_failure = true;
        }
    }

    // For sync-to: also roll back repos in the extra (target) workspaces.
    // The target side does not have its own `converged_tips` entries — the
    // recorded tips key off the source-side workspace's repo paths. For
    // target-side repos, fall back to looking up by the same repo_path
    // (typically identical across source/target via shared object stores);
    // a target tip that diverged from the source convergence will surface
    // as a foreign-tip refusal, which is the desired behavior.
    for extra_dir in &extra_workspace_dirs {
        // Side-specific id: `-target` namespace when this extra IS the
        // recorded target workspace, base op id when it is the owner.
        let extra_restore_id = restore_id_for(extra_dir);
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
                    // Skip reference symlinks (shared canonical store): nothing
                    // was savepointed or advanced for them, so nothing to reset.
                    if !checkout_is_syncable(&abs) {
                        continue;
                    }
                    // Target-side repos: advanced_tips is source/owner side only (§7).
                    // Target tips land in converged_tips post-relock; no intent entry.
                    let converged = converged_tips.get(repo_path.as_str()).map(String::as_str);
                    match abort_one_repo(&abs, &extra_restore_id, None, converged) {
                        Ok(outcome) => report_abort_outcome(
                            &format!("[target] {repo_path}"),
                            &outcome,
                            Some(abs.as_path()),
                            &mut noise_summary,
                            &mut any_foreign,
                        ),
                        Err(e) => {
                            eprintln!("  [target] {repo_path}: {e}");
                            any_failure = true;
                        }
                    }
                }
                let extra_project_converged = converged_tips.get("(project)").map(String::as_str);
                match abort_one_repo(
                    &extra_project_dir,
                    &extra_restore_id,
                    None, // target-side: no advanced_tips entry (§7)
                    extra_project_converged,
                ) {
                    Ok(outcome) => report_abort_outcome(
                        "[target] (project)",
                        &outcome,
                        Some(extra_project_dir.as_path()),
                        &mut noise_summary,
                        &mut any_foreign,
                    ),
                    Err(e) => {
                        eprintln!("  [target] (project): {e}");
                        any_failure = true;
                    }
                }
                // Clear op-state from the extra workspace only when this side
                // completed without a foreign-tip refusal — otherwise the
                // operator may want to re-run abort after reconciling.
                if !any_foreign && !any_failure {
                    op_state::clear_all_at(&extra_ws_dir);
                }
            }
            Err(e) => {
                eprintln!(
                    "  warning: could not resolve workspace at {}: {e}; skipping",
                    extra_dir.display()
                );
            }
        }
    }

    // Emit the aggregate noise summary (skipped / untouched) as a single line.
    print_abort_noise_summary(&noise_summary);

    // Print the recovery-options block ONCE, only when at least one repo
    // refused (foreign-tip violation). The per-repo block above shows the
    // evidence (savepoint, tip, blocking commits); this block tells the
    // operator what to do about it.
    if any_foreign {
        print_abort_recovery_options();
    }

    // Clear op-state from CWD workspace only on a fully clean abort. If any
    // repo refused (foreign tip) or errored, retain op-state so the operator
    // can re-run `rwv abort` after manually reconciling the divergence.
    if !any_foreign && !any_failure {
        op_state::clear_all_at(&workspace_dir);
    } else if any_foreign {
        eprintln!(
            "\nabort refused on at least one repo (foreign-tip violation); \
             op-state retained at {} so you can re-run `rwv abort` after \
             reconciling.",
            workspace_dir.display()
        );
    }

    if any_foreign {
        anyhow::bail!("abort refused: foreign tip on at least one repo");
    }
    if any_failure {
        anyhow::bail!("abort completed with failures");
    }

    Ok(())
}

/// Restore a single repo as part of `rwv abort`.
///
/// Two rails (design § 5):
///
/// 1. **Pre-abort ref**: a durable reference at the repo's current tip is
///    written *before* any restore is attempted — abort is itself
///    information-preserving and undoable via that ref.
/// 2. **HEAD-verified restore**: the destructive `reset --hard` to the
///    savepoint is gated on the current tip being attributable to the op
///    (== savepoint, == recorded intent tip, == recorded converged tip,
///    or mid-op). Anything else is reported as foreign, not reset.
///
/// `recorded_intent_tip` is the SHA from the owner record's `advanced_tips`
/// map for this repo (source/owner side only — target side passes `None`).
/// `recorded_converged_tip` is from `converged_tips` (written at relock).
fn abort_one_repo(
    repo: &Path,
    op_id: &OpId,
    recorded_intent_tip: Option<&str>,
    recorded_converged_tip: Option<&str>,
) -> anyhow::Result<VerifiedRestoreOutcome> {
    // Rail 1: write the pre-abort ref BEFORE any verified restore. Even if
    // the verified restore decides to refuse, the tip is durably captured
    // so the operator can roll the branch back later if desired.
    //
    // Best-effort: if writing the pre-abort ref fails, surface as an error
    // and continue with the verified restore — but only if we can determine
    // the failure is benign. Today we propagate the error: failing to
    // preserve information is itself a violation of the doctrine.
    GitVcs
        .create_pre_abort_ref(repo, op_id.as_str())
        .context("create pre-abort ref failed")?;

    // Rail 2: HEAD-verified restore. `verified_restore_savepoint` performs
    // the classification + restore-if-attributable atomically; foreign tips
    // are returned as `ForeignTip` for the caller to report.
    GitVcs
        .verified_restore_savepoint(
            repo,
            op_id.as_str(),
            recorded_intent_tip,
            recorded_converged_tip,
        )
        .context("verified restore failed")
}

/// Accumulated counts for non-actionable per-repo abort outcomes. Gathered
/// during the reporting loop so a single summary line can be emitted at the
/// end rather than one line per boring repo.
#[derive(Default)]
struct AbortNoiseSummary {
    no_savepoint: usize,
    untouched: usize,
}

/// Number of commits to show inline per refused repo before summarising the
/// rest as "and N more".
const BLOCKING_COMMITS_CAP: usize = 5;

/// Record a per-repo abort outcome into `summary`/`any_foreign`, printing
/// actionable lines immediately and deferring non-actionable counts to the
/// summary. The recovery-options block is NOT printed here — callers print
/// it once after all repos are processed.
///
/// `repo_abs` is the absolute path to the repo on disk, used only for the
/// read-only `git log` lookup on foreign-tip refusals; pass `None` when the
/// path is unavailable (e.g. the repo does not exist on disk).
fn report_abort_outcome(
    label: &str,
    outcome: &VerifiedRestoreOutcome,
    repo_abs: Option<&Path>,
    summary: &mut AbortNoiseSummary,
    any_foreign: &mut bool,
) {
    match outcome {
        VerifiedRestoreOutcome::NoSavepoint => {
            // Demoted to the noise summary — printed as one aggregate line.
            summary.no_savepoint += 1;
        }
        VerifiedRestoreOutcome::Untouched => {
            // Demoted to the noise summary — printed as one aggregate line.
            summary.untouched += 1;
        }
        VerifiedRestoreOutcome::RestoredFromIntent => {
            println!("  {label}: restored (from recorded intent tip)");
        }
        VerifiedRestoreOutcome::RestoredFromConverged => {
            println!("  {label}: restored (from recorded converged tip)");
        }
        VerifiedRestoreOutcome::RestoredFromMidOp => {
            println!("  {label}: restored (from mid-op state)");
        }
        VerifiedRestoreOutcome::ForeignTip {
            observed_tip,
            savepoint,
            recorded_converged_tip,
            pre_abort_ref,
        } => {
            *any_foreign = true;
            let converged_text = match recorded_converged_tip {
                Some(c) => format!("recorded converged tip: {c}"),
                None => "no converged tip recorded (op crashed before relock)".to_string(),
            };

            // Determine the commit-graph shape and fetch blocking commits.
            let shape_and_commits = if let Some(repo) = repo_abs {
                let (ahead, behind) = GitVcs.ahead_behind(repo, savepoint, observed_tip);
                let shape = if behind == 0 && ahead > 0 {
                    format!("tip is {ahead} commit(s) ahead of savepoint (strictly ahead — common recoverable case)")
                } else if ahead > 0 && behind > 0 {
                    format!("tip and savepoint have diverged ({ahead} ahead, {behind} behind — requires manual reconciliation)")
                } else {
                    // ahead == 0 && behind == 0: equal — shouldn't reach ForeignTip, but be safe.
                    "tip equals savepoint (unexpected ForeignTip state)".to_string()
                };
                let (commits, total) =
                    GitVcs.log_oneline_range(repo, savepoint, observed_tip, BLOCKING_COMMITS_CAP);
                let commit_block = if commits.is_empty() {
                    "\t  (no commits in range or range unresolvable)".to_string()
                } else {
                    let mut block = commits
                        .iter()
                        .map(|c| format!("\t  {c}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if total > BLOCKING_COMMITS_CAP {
                        block.push_str(&format!(
                            "\n\t  ... and {} more",
                            total - BLOCKING_COMMITS_CAP
                        ));
                    }
                    block
                };
                format!("\tblocking commits ({shape}):\n{commit_block}")
            } else {
                "\tblocking commits: (repo path unavailable)".to_string()
            };

            eprintln!(
                "  {label}: foreign-tip violation — refusing to reset.\n\
                 \tobserved tip:  {observed_tip}\n\
                 \texpected one of: savepoint {savepoint}, {converged_text}, or a VCS-native mid-op state\n\
                 \ttip preserved at: {ref_label}\n\
                 {shape_and_commits}",
                ref_label = pre_abort_ref.label,
            );
        }
    }
}

/// Print the one-time recovery-options block. Called exactly once at the end
/// of `run_abort` when `any_foreign` is true. Kept separate so the per-repo
/// loop in `run_abort` stays free of repeated options text.
fn print_abort_recovery_options() {
    eprintln!(
        "\nrecovery options (apply to each refused repo above):\n\
         \t- if a foreign agent advanced this branch after a crash, manually move the \
           branch back (e.g. `git update-ref refs/heads/<branch> <savepoint>`) \
           and re-run `rwv abort`.\n\
         \t- if you want to keep the foreign tip and discard the op, move the branch \
           off the pre-abort ref and delete the savepoint manually."
    );
}

/// Emit a one-line summary of non-actionable (noise) outcomes. Suppressed
/// when both counts are zero (nothing to say).
fn print_abort_noise_summary(summary: &AbortNoiseSummary) {
    let mut parts: Vec<String> = Vec::new();
    if summary.no_savepoint > 0 {
        parts.push(format!(
            "{} repo(s) skipped (no savepoint)",
            summary.no_savepoint
        ));
    }
    if summary.untouched > 0 {
        parts.push(format!(
            "{} untouched (tip == savepoint)",
            summary.untouched
        ));
    }
    if !parts.is_empty() {
        println!("  summary: {}", parts.join(", "));
    }
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
/// the spec's "non-zero iff at least one repo failed" semantic: when sync
/// can't even reach the per-repo loop, there are no per-repo outcomes to
/// emit, so the structured channel has nothing to say.
#[allow(clippy::too_many_arguments)]
pub fn run_sync_json(cwd: &Path, request: SyncRequest) -> anyhow::Result<()> {
    let records: Mutex<Vec<SyncOutcomeOutput>> = Mutex::new(Vec::new());
    let stdout_lock: Mutex<()> = Mutex::new(());
    let ndjson = request.jobs > 1;
    let project_level_result = if ndjson {
        let handler = JsonNdjsonHandler {
            stdout_lock: &stdout_lock,
            records: &records,
            schema_url: SYNC_JSON_SCHEMA_URL,
        };
        run_machine(MachineVerb::Sync, cwd, &request, &handler)
    } else {
        let handler = JsonEnvelopeHandler { records: &records };
        run_machine(MachineVerb::Sync, cwd, &request, &handler)
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

/// Shared JSON exit/return tail for `run_sync_json_impl` and `run_sync_to_json`.
///
/// After serialization is done (or skipped for NDJSON), both callers share
/// the same exit logic: exit 1 when any per-repo outcome failed, otherwise
/// propagate `project_level_result`. Factored here so the logic lives in
/// exactly one place.
///
/// Uses `process::exit` rather than `Err` when repos fail so that the JSON
/// already printed to stdout is not contaminated by anyhow's stderr
/// formatter (the test harness asserts on stdout + exit code only).
fn json_exit_tail(
    any_failure: bool,
    project_level_result: anyhow::Result<()>,
) -> anyhow::Result<()> {
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

/// Post-machine JSON emitter for `rwv sync --json`.
///
/// Emits the envelope (serial mode) or a no-op (NDJSON already streamed each
/// record as it arrived), then delegates to [`json_exit_tail`] for exit/return.
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
    // extra to write). Per the spec, NDJSON does NOT emit an
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

    json_exit_tail(any_failure, project_level_result)
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
pub fn run_sync_to(cwd: &Path, request: SyncRequest) -> anyhow::Result<()> {
    let stdout_lock: Mutex<()> = Mutex::new(());
    let handler = TextHandler {
        stdout_lock: &stdout_lock,
    };
    run_machine(MachineVerb::SyncTo, cwd, &request, &handler)
}

/// Execute `rwv sync-to <target> --json`.
///
/// Emits a [`SyncToJsonOutput`] envelope with the new observability fields:
/// `source_workweave`, `target`, `retired`, per-outcome `step3_advance`, and
/// `project_repo_advance`. These fields are absent from the plain
/// `rwv sync --json` envelope ([`SyncJsonOutput`]).
pub fn run_sync_to_json(cwd: &Path, request: SyncRequest) -> anyhow::Result<()> {
    // Derive source_workweave from the CWD context before running the machine.
    // This mirrors what guard_and_mark computes internally.
    let source_workweave: Option<String> = {
        match WorkspaceContext::resolve(cwd, request.project_override.clone()) {
            Ok(ctx) => match &ctx.location {
                WorkspaceLocation::Workweave { name, .. } => Some(name.as_str().to_owned()),
                WorkspaceLocation::Weave { .. } => None,
            },
            Err(_) => None,
        }
    };

    // Derive the target path: the resolved destination workspace directory.
    // For sync-to the operator-supplied arg is the target; resolve it the same
    // way guard_and_mark does (SyncSource::resolve against the CWD context).
    // When CWD is not inside any workspace, leave target_path empty; the
    // machine (run_machine below) will surface the real no-workspace error
    // rather than panicking here.
    let target_path: String = {
        match WorkspaceContext::resolve(cwd, request.project_override.clone()) {
            Ok(cwd_ctx) => match &request.source {
                Some(src) => src
                    .resolve(&cwd_ctx)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                None => String::new(),
            },
            Err(_) => String::new(),
        }
    };

    let records: Mutex<Vec<SyncOutcomeOutput>> = Mutex::new(Vec::new());
    let step3_advances: Mutex<std::collections::HashMap<String, Step3AdvanceOutput>> =
        Mutex::new(std::collections::HashMap::new());
    let stdout_lock: Mutex<()> = Mutex::new(());
    let ndjson = request.jobs > 1;
    let project_level_result = if ndjson {
        // NDJSON mode: use the standard NDJSON handler (step3 SHAs are not
        // surfaced per-line in NDJSON; the envelope-level fields are only
        // emitted in serial mode).
        let handler = JsonNdjsonHandler {
            stdout_lock: &stdout_lock,
            records: &records,
            schema_url: SYNC_TO_JSON_SCHEMA_URL,
        };
        run_machine(MachineVerb::SyncTo, cwd, &request, &handler)
    } else {
        let handler = JsonEnvelopeSyncToHandler {
            records: &records,
            step3_advances: &step3_advances,
        };
        run_machine(MachineVerb::SyncTo, cwd, &request, &handler)
    };

    let records = records.into_inner().unwrap_or_else(|e| e.into_inner());
    let mut step3_map = step3_advances
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());

    // If we never reached the per-repo loop (project-level precondition
    // failure), propagate the error so main prints it via anyhow.
    if records.is_empty() && project_level_result.is_err() {
        return project_level_result;
    }

    let any_failure = records.iter().any(SyncOutcomeOutput::is_failure);

    if !ndjson {
        // Splice step3_advance into each per-outcome record.
        let mut outcomes: Vec<SyncOutcomeOutput> = records;
        for outcome in &mut outcomes {
            // Match by path field to look up the advance record.
            let path_key = match outcome {
                SyncOutcomeOutput::Converged { path, .. }
                | SyncOutcomeOutput::AlreadyAhead { path, .. }
                | SyncOutcomeOutput::NoOp { path, .. }
                | SyncOutcomeOutput::Failed { path, .. } => path.clone(),
            };
            if let Some(adv) = step3_map.remove(&path_key) {
                *outcome.step3_advance_mut() = Some(adv);
            }
        }

        // project_repo_advance: the "(project)" sentinel key.
        let project_repo_advance = step3_map.remove("(project)");

        // `retired` is true iff --retire was passed AND retire actually fired
        // (i.e., CWD was a workweave, not a primary weave) AND the machine
        // completed without error. When invoked from a primary weave, run_retire
        // returns Ok(()) without deleting anything (warns instead), so we gate
        // on source_workweave being Some() to distinguish the two cases.
        let actually_retired =
            request.retire && source_workweave.is_some() && project_level_result.is_ok();

        let payload = SyncToJsonOutput {
            schema: SYNC_TO_JSON_SCHEMA_URL.to_owned(),
            source_workweave,
            target: target_path,
            retired: actually_retired,
            outcomes,
            project_repo_advance,
        };
        let out =
            serde_json::to_string_pretty(&payload).context("failed to serialize sync-to output")?;
        println!("{out}");
    }

    json_exit_tail(any_failure, project_level_result)
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

    // -----------------------------------------------------------------------
    // checkout_is_syncable — the sync reference-exclusion chokepoint predicate
    // -----------------------------------------------------------------------

    #[test]
    fn checkout_is_syncable_true_for_a_real_directory_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(classify_checkout(&dir), CheckoutKind::Worktree);
        assert!(
            checkout_is_syncable(&dir),
            "a real on-disk worktree directory must be syncable"
        );
    }

    #[test]
    fn checkout_is_syncable_false_for_a_symlink_reference_alias() {
        let tmp = tempfile::tempdir().unwrap();
        let canonical = tmp.path().join("canonical");
        std::fs::create_dir_all(&canonical).unwrap();
        let link = tmp.path().join("alias");
        std::os::unix::fs::symlink(&canonical, &link).unwrap();
        // The alias resolves to a real dir (so `exists()` is true), but it is a
        // symlink, so it must be excluded — proving the predicate keys on
        // alias-ness, not on existence.
        assert!(link.exists(), "symlink target exists");
        assert_eq!(classify_checkout(&link), CheckoutKind::ReferenceAlias);
        assert!(
            !checkout_is_syncable(&link),
            "a symlink reference alias must NOT be syncable even though it exists"
        );
    }

    #[test]
    fn checkout_is_syncable_false_for_a_nonexistent_path() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        // A non-existent path classifies as Worktree, so the `exists()` term is
        // load-bearing: without it a missing checkout would wrongly be syncable.
        assert_eq!(classify_checkout(&missing), CheckoutKind::Worktree);
        assert!(
            !checkout_is_syncable(&missing),
            "a non-existent checkout must NOT be syncable"
        );
    }

    #[test]
    fn checkout_is_syncable_false_for_a_dangling_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("dangling");
        std::os::unix::fs::symlink(tmp.path().join("missing-target"), &link).unwrap();
        // A dangling symlink: `exists()` (which follows the link) is false, but
        // `classify_checkout` (which does not follow) still flags it as an
        // alias. Either way it is excluded — and must never be materialized as
        // a worktree against the canonical.
        assert!(!link.exists(), "dangling symlink target is missing");
        assert_eq!(classify_checkout(&link), CheckoutKind::ReferenceAlias);
        assert!(
            !checkout_is_syncable(&link),
            "a dangling reference symlink must NOT be syncable"
        );
    }

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

    // Correction 4 — resume verb is derived from the op's `verb` field, NEVER
    // hardcoded `rwv sync`. The plain resume string builder is the single
    // source of that vocabulary.
    #[test]
    fn resume_command_derives_from_op_verb() {
        assert_eq!(resume_command(OpVerb::Sync), "rwv sync --continue");
        assert_eq!(resume_command(OpVerb::SyncTo), "rwv sync-to --continue");
    }

    // Site 1 — manifest-repo per-repo sync loop failure summary. With a live
    // rebase conflict the VCS-native staging steps are present; the resume
    // verb is derived from the op verb; and the rebase hint must NOT spell
    // raw `git rebase --continue` (fo-w2ajvn.2: `rwv <verb> --continue` IS
    // the continue step — rwv resumes the rebase natively).
    #[test]
    fn manifest_repo_failure_message_live_conflict_includes_vcs_and_verb_resume() {
        let msg = manifest_repo_failure_message(OpVerb::SyncTo, Some(ConflictOp::Rebase));
        assert!(
            !msg.contains("git rebase --continue"),
            "rebase hint must NOT spell raw `git rebase --continue`; got: {msg}"
        );
        // Resume verb from op-state, never a hardcoded pull `rwv sync`.
        assert!(
            msg.contains("rwv sync-to --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(
            !msg.contains("rwv sync primary") && !msg.contains("rwv sync /"),
            "must NOT hardcode `rwv sync <source>`: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 1 — NO live conflict (e.g. fetch/head-unreadable failures): the
    // VCS-native hint is OMITTED (nothing to `--continue` in git), and the
    // message points straight at the verb-derived resume.
    #[test]
    fn manifest_repo_failure_message_no_conflict_omits_vcs_hint() {
        let msg = manifest_repo_failure_message(OpVerb::Sync, None);
        assert!(
            !msg.contains("git rebase --continue")
                && !msg.contains("git merge --continue")
                && !msg.contains("git cherry-pick --continue"),
            "VCS hint must be absent without a live conflict: {msg}"
        );
        assert!(
            msg.contains("rwv sync --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(msg.contains("rwv abort"), "expected rollback option: {msg}");
    }

    // Site 2 — Phase 1' (project repo) outer bail, live conflict.
    #[test]
    fn phase1_bail_message_live_conflict_includes_resolution_and_verb_resume() {
        let cwd = Path::new("/ws/projects/web-app");
        let msg = phase1_or_phase3_failure_message(
            Phase::One,
            cwd,
            OpVerb::SyncTo,
            Some(ConflictOp::Rebase),
        );
        assert!(
            msg.contains("Phase 1' (project repo)"),
            "expected phase label in: {msg}"
        );
        assert!(
            !msg.contains("git rebase --continue"),
            "rebase bail must NOT spell raw `git rebase --continue`; got: {msg}"
        );
        assert!(
            msg.contains("/ws/projects/web-app"),
            "expected repo path: {msg}"
        );
        assert!(
            msg.contains("rwv sync-to --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(
            !msg.contains("rwv sync ww1") && !msg.contains("rwv sync /"),
            "must NOT hardcode `rwv sync <source>`: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 2 — Phase 1' non-conflict failure (e.g. discard-local-commits
    // hard-reset error): no live ConflictOp, so no VCS-native hint.
    #[test]
    fn phase1_bail_message_no_conflict_omits_vcs_hint() {
        let cwd = Path::new("/ws/projects/web-app");
        let msg = phase1_or_phase3_failure_message(Phase::One, cwd, OpVerb::Sync, None);
        assert!(
            !msg.contains("git rebase --continue"),
            "VCS hint must be absent without a live conflict: {msg}"
        );
        assert!(
            msg.contains("rwv sync --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(msg.contains("rwv abort"), "expected rollback option: {msg}");
    }

    // Site 3 — Phase 3 (re-lock) outer bail: relock is NEVER a VCS conflict, so
    // the caller passes `None` and the message must NOT teach a spurious
    // `git rebase --continue` (which would print "No rebase in progress").
    #[test]
    fn phase3_bail_message_never_teaches_vcs_hint() {
        let cwd = Path::new("/ws/projects/web-app");
        let msg = phase1_or_phase3_failure_message(Phase::Three, cwd, OpVerb::SyncTo, None);
        assert!(
            msg.contains("Phase 3 (re-lock)"),
            "expected phase label in: {msg}"
        );
        assert!(
            !msg.contains("git rebase --continue"),
            "relock is never a rebase conflict — no `git rebase --continue`: {msg}"
        );
        assert!(
            msg.contains("rwv sync-to --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(
            !msg.contains("rwv sync /abs/source"),
            "must NOT hardcode `rwv sync <source>`: {msg}"
        );
    }

    // Site 4 — cherry-pick op hint (trait surface; sync no longer uses
    // cherry-pick directly but the message builder must still render the
    // op's hint correctly for any VCS impl that does). This site ALWAYS has a
    // live ConflictOp, so the VCS hint is unconditional. CherryPick has no
    // rwv-native continue, so its VCS hint retains the raw git continue; the
    // verb-derived resume line then picks the op back up.
    #[test]
    fn per_conflict_bail_cherry_pick_includes_cherry_pick_hint() {
        let repo = Path::new("/ws/projects/web-app");
        let msg = per_conflict_bail_message(
            repo,
            ConflictOp::CherryPick,
            "cherry-pick (rebase replay)",
            "commit deadbeef on paths: foo.txt",
            OpVerb::Sync,
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
            msg.contains("rwv sync --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(
            !msg.contains("rwv sync primary"),
            "must NOT hardcode `rwv sync <source>`: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 5 — Phase 1' merge inner bail.
    //
    // Merge has no rwv-native continue; continue_cmd carries the raw git form.
    #[test]
    fn per_conflict_bail_merge_includes_merge_hint() {
        let repo = Path::new("/ws/projects/web-app");
        let msg = per_conflict_bail_message(
            repo,
            ConflictOp::Merge,
            "merge",
            "paths: bar.txt",
            OpVerb::SyncTo,
        );
        assert!(
            msg.contains("git merge --continue"),
            "expected merge hint in: {msg}"
        );
        assert!(msg.contains("bar.txt"), "expected detail in: {msg}");
        assert!(
            msg.contains("rwv sync-to --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 6 — Phase 1' rebase inner bail: subject appears in rendered message.
    //
    // This tests that per_conflict_bail_message surfaces the subject when a
    // stopped-commit detail string containing the subject is passed as the
    // `detail` arg — as `apply_project_strategy` now does via
    // `Vcs::rebase_stopped_commit_detail` (now a trait method).
    //
    // Key contract: Rebase bail must use `rwv sync --continue`, NOT raw
    // `git rebase --continue`. `rwv abort` comes last.
    #[test]
    fn per_conflict_bail_rebase_project_repo_includes_commit_subject_in_detail() {
        let repo = Path::new("/ws/projects/web-app");
        let detail = "commit abc1234 (lock: refresh — post-OOB drift in gc-formulas)";
        let msg = per_conflict_bail_message(
            repo,
            ConflictOp::Rebase,
            "rebase (project repo)",
            detail,
            OpVerb::Sync,
        );
        assert!(
            msg.contains("abc1234"),
            "expected short SHA in message: {msg}"
        );
        assert!(
            msg.contains("lock: refresh"),
            "expected commit subject in message: {msg}"
        );
        assert!(
            msg.contains("post-OOB drift"),
            "expected commit subject continuation in message: {msg}"
        );
        assert!(
            !msg.contains("git rebase --continue"),
            "rebase bail must NOT spell raw `git rebase --continue`; got: {msg}"
        );
        assert!(
            msg.contains("rwv sync --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(
            !msg.contains("rwv sync primary"),
            "must NOT hardcode `rwv sync <source>`: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Correction 4 — the stale-lock recovery hint teaches the ATOMIC
    // `rwv lock --commit`, never the broken two-step `rwv lock` + "commit"
    // (whose written-but-unstaged `rwv.lock` kills a re-run mid-op).
    #[test]
    fn stale_lock_refusal_hint_names_lock_commit() {
        let offending = RepoRelation {
            repo_path: crate::manifest::RepoPath::new("github/org/lib").unwrap(),
            relation: LockRelation::Behind,
            ahead_count: None,
        };
        for side in [Side::Source, Side::Destination] {
            let msg = lock_relation_refusal(side, "ws", "app", &[&offending])
                .expect("non-empty offending set yields a refusal");
            assert!(
                msg.contains("rwv lock --commit"),
                "stale-lock hint must name `rwv lock --commit` ({side:?}): {msg}"
            );
            assert!(
                !msg.contains("and commit before syncing"),
                "must NOT teach the broken two-step form ({side:?}): {msg}"
            );
        }
    }

    // The diverged-repo bless-HEAD hint AND the recovery hint both name
    // `rwv lock --commit`.
    #[test]
    fn diverged_lock_refusal_blesses_head_with_lock_commit() {
        let diverged = RepoRelation {
            repo_path: crate::manifest::RepoPath::new("github/org/lib").unwrap(),
            relation: LockRelation::Diverged,
            ahead_count: None,
        };
        let msg = lock_relation_refusal(Side::Source, "ws", "app", &[&diverged])
            .expect("non-empty offending set yields a refusal");
        assert!(
            msg.contains("bless the current HEAD with `rwv lock --commit`"),
            "diverged repo must earn the bless-HEAD `rwv lock --commit` hint: {msg}"
        );
    }

    // The unresolvable-lock (corrupt lock) refusal also names `rwv lock --commit`.
    #[test]
    fn unresolvable_lock_refusal_hint_names_lock_commit() {
        let msg = unresolvable_lock_refusal(
            Side::Source,
            "ws",
            "app",
            &crate::manifest::RepoPath::new("github/org/lib").unwrap(),
            &crate::vcs::RawRevisionId::new("v-does-not-exist"),
        );
        assert!(
            msg.contains("rwv lock --commit"),
            "unresolvable-lock hint must name `rwv lock --commit`: {msg}"
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
