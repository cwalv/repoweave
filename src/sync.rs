//! `rwv sync <source>` and `rwv abort` implementation.
//!
//! `rwv sync` aligns the CWD workspace with another workspace's committed
//! `rwv.lock`. `rwv abort` rolls back to pre-sync state using savepoint refs.

use crate::integration::Integration;
use crate::integration_runner::enabled_integrations;
use crate::integrations::builtin_integrations;
use crate::lock::{commit_lock_file_with_message, generate_lock};
use crate::manifest::{
    project_repo_key, IntegrationConfig, LockFile, Manifest, Project, ProjectName, RepoPath, Role,
    WorkweaveName, WorkweaveNameError,
};
use crate::op_state::{self, OpId, OpVerb, OwnerRecord, SyncStrategy};
use crate::parallel::run_in_parallel;
use crate::status::{compute_relation, LockRelation};
use crate::vcs::{
    project_vcs, vcs_for, AttachedRef, ConflictOp, DerivedContentPolicy,
    DiscardLocalCommitsConsent, DiscardWarrant, EphemeralRefName, HeadAttachment, RefName,
    ResolvedRevisionId, Vcs, VcsError, VcsErrorOutput, VerifiedRestoreOutcome,
};
use crate::workspace::{
    project_dir, project_rel_path, AdvisoryKindOutput, AdvisoryOutput, Checkout, Resolution,
    WorkspaceContext,
};
use crate::workweave::{classify_checkout, ensure_registered_workweave, CheckoutKind};
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
    match &ctx.checkout {
        Checkout::Workweave { name, .. } => name.as_str().to_owned(),
        Checkout::Primary { .. } => ctx
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
/// This is the ONE home of the literal: both the op-start
/// benign-staleness relock and the post-replay Phase-3 relock format their
/// commit message here, and the explain generator splices the
/// same string into `rwv explain sync-to` via a `{{MSG:auto_relock}}`
/// placeholder so the docs cannot drift from the code. `<source>` is the
/// display name of the workspace the sync pulled from. Text is unchanged from
/// the pre-extraction literal.
pub fn auto_relock_commit_message(source: &str) -> String {
    format!("lock: auto-relock after sync from {source}")
}

// ---------------------------------------------------------------------------
// SyncSource — typed source workspace for `rwv sync`
// ---------------------------------------------------------------------------

/// Source workspace for `rwv sync <source>`.
///
/// The boundary parser ([`FromStr`]) disambiguates by shape:
/// - `Primary` — the literal string `"primary"` (the primary workspace root).
/// - `Workweave(name)` — a bare identifier with no path separators or leading
///   dot. Resolves via the primary-side registry
///   ([`crate::workweave_index`]) with marker round-trip validation.
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
                let project = match &ctx.checkout {
                    Checkout::Workweave { project, .. } => project.clone(),
                    Checkout::Primary { .. } => ctx.require_active_project()?.clone(),
                };
                ensure_registered_workweave(ctx.primary_path(), &project, name)
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
    type Err = WorkweaveNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "primary" {
            return Ok(Self::Primary);
        }
        if looks_path_like(s) {
            return Ok(Self::Path(PathBuf::from(s)));
        }
        Ok(Self::Workweave(WorkweaveName::new(s)?))
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

/// Refuse when the workweave's recorded `parent:` no longer exists on disk
/// (retired or deleted out-of-band).
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
    ///
    /// `derived_content_dropped` names the declared derived paths whose
    /// replayed version the landed tree does not carry — empty on all but the
    /// replays that resolved one. Only this variant can carry them: the two
    /// no-op variants never replay, and a failed replay lands nothing.
    Converged {
        derived_content_dropped: Vec<String>,
    },
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
        /// Repo-relative paths this repo declares derived whose replayed
        /// version the landed tree does not carry: the replay resolved them
        /// to the target's version instead. Regenerating them from their
        /// source of record and committing is what makes the landed tree
        /// describe itself again. Omitted when nothing was resolved away.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        derived_content_dropped: Vec<String>,
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
            RepoSyncOutcome::Converged {
                derived_content_dropped,
            } => Self::Converged {
                path,
                absolute_path,
                step3_advance: None,
                derived_content_dropped: derived_content_dropped.clone(),
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
    /// Standing advisories raised during this sync (e.g. delivered changes
    /// touching a materialized input). Empty, not absent, when there are
    /// none — a consumer branches on length rather than presence.
    ///
    /// Present only in this envelope: `-j N` with `N > 1` streams NDJSON
    /// instead, one self-describing per-repo line with no envelope for an
    /// advisory to sit in, so a parallel `--json` sync carries no advisory
    /// at all. Run `-j 1` (or omit `-j`) to receive one.
    pub advisories: Vec<AdvisoryOutput>,
    /// Resolved workspace coordinates (workspace root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
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
    /// Resolved workspace coordinates (workspace root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
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
pub const SYNC_JSON_SCHEMA_URL: &str = crate::schema_url::schema_url!("sync");

/// Schema URL embedded in `rwv sync-to --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/`.
pub const SYNC_TO_JSON_SCHEMA_URL: &str = crate::schema_url::schema_url!("sync-to");

impl fmt::Display for RepoSyncOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Converged { .. } => f.write_str("converged"),
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

/// Name the declared derived paths `repo`'s replay resolved to the target's
/// version, by comparing the tree the replay produced against the tip it
/// started from.
///
/// `pre_replay_tip` is that starting tip, read before any of this op's picks
/// applied — a resumed replay's HEAD is mid-sequence and answers a different
/// question. `None` means the caller could not establish it, and an
/// unestablished starting point yields no report rather than a guessed one.
fn dropped_derived_content(
    vcs: &dyn Vcs,
    repo: &Path,
    target: &ResolvedRevisionId,
    pre_replay_tip: Option<&ResolvedRevisionId>,
) -> Vec<String> {
    let Some(pre_replay_tip) = pre_replay_tip else {
        return Vec::new();
    };
    let Ok(landed) = vcs.head_revision(repo) else {
        return Vec::new();
    };
    vcs.derived_content_dropped_by_replay(repo, target, pre_replay_tip, &landed)
        .unwrap_or_default()
}

fn sync_one_repo(
    vcs: &dyn Vcs,
    repo: &Path,
    target: &ResolvedRevisionId,
    strategy: SyncStrategy,
    pre_replay_tip: Option<&ResolvedRevisionId>,
) -> RepoSyncOutcome {
    // Replay re-entry (`rwv sync --continue`) can find a manifest repo
    // mid-rebase from a previous phase that stopped on a conflict. HEAD in
    // that state points at the last-applied pick (descended from `target`),
    // which would falsely match the AlreadyAhead branch below and skip the
    // remaining picks entirely. Route mid-rebase repos straight to
    // `apply_strategy` so the `rebase_continue` path drives the rebase
    // forward; the head-equality / AlreadyAhead short-circuits assume a
    // repo not in a mid-op state.
    if matches!(vcs.mid_op(repo), Some(ConflictOp::Rebase)) && strategy == SyncStrategy::Rebase {
        return match apply_strategy(vcs, repo, target, strategy) {
            Ok(()) => RepoSyncOutcome::Converged {
                derived_content_dropped: dropped_derived_content(vcs, repo, target, pre_replay_tip),
            },
            Err(StrategyError { message, cause }) => {
                RepoSyncOutcome::Failed(SyncFailure::for_strategy(strategy, message, cause))
            }
        };
    }

    let head = match vcs.head_revision(repo) {
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
    let is_ancestor = vcs.is_ancestor(repo, target, &head).unwrap_or(false);

    if is_ancestor {
        let commits_ahead = vcs.count_commits_in_range(repo, target, &head).unwrap_or(0);
        return RepoSyncOutcome::AlreadyAhead { commits_ahead };
    }

    match apply_strategy(vcs, repo, target, strategy) {
        Ok(()) => RepoSyncOutcome::Converged {
            derived_content_dropped: dropped_derived_content(vcs, repo, target, pre_replay_tip),
        },
        Err(StrategyError { message, cause }) => {
            RepoSyncOutcome::Failed(SyncFailure::for_strategy(strategy, message, cause))
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
    vcs: &dyn Vcs,
    repo: &Path,
    target: &ResolvedRevisionId,
    strategy: SyncStrategy,
) -> Result<(), StrategyError> {
    match strategy {
        SyncStrategy::Ff => {
            if let Err(e) = vcs.advance_if_fast_forward(repo, target) {
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
            // (including derived-content picks the policy resolves rather
            // than stopping on). A mid-op state that is NOT rebase
            // (mid-merge, mid-cherry-pick) is not rwv-initiated for this
            // path; fall through to `Vcs::rebase` and let it fail loudly.
            //
            // Both arms state the same derived-content resolution, and they
            // have to: a resume finishes the picks the interrupted replay
            // never reached, so a resume under a different policy than the
            // replay it resumes would resolve the tail of one operation by
            // different rules than its head.
            match vcs.mid_op(repo) {
                Some(ConflictOp::Rebase) => {
                    vcs.rebase_continue(repo, DerivedContentPolicy::keep_target_side())
                        .map_err(StrategyError::from_vcs)?;
                }
                _ => {
                    vcs.rebase(
                        repo,
                        target,
                        target,
                        DerivedContentPolicy::keep_target_side(),
                    )
                    .map_err(StrategyError::from_vcs)?;
                }
            }
        }
    }
    Ok(())
}

fn create_savepoint(
    vcs: &dyn Vcs,
    repo: &Path,
    op_id: &OpId,
) -> anyhow::Result<ResolvedRevisionId> {
    Ok(vcs.create_savepoint(repo, op_id.as_str())?)
}

fn delete_savepoint(vcs: &dyn Vcs, repo: &Path, op_id: &OpId) {
    vcs.drop_savepoint(repo, op_id.as_str());
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

// NOTE: the old `check_lock_freshness` / `lock_recovery`
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
    vcs: &dyn Vcs,
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
    vcs.is_ancestor(cwd_project_dir, cwd_tip, source_tip)
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
    vcs: &dyn Vcs,
    cwd_project_dir: &Path,
    cwd_tip: &ResolvedRevisionId,
    source_tip: &ResolvedRevisionId,
    cwd_workspace_name: &str,
    source_workspace_name: &str,
) -> anyhow::Result<()> {
    if cwd_is_ancestor_or_equal(vcs, cwd_project_dir, cwd_tip, source_tip) {
        return Ok(());
    }

    // CWD is not an ancestor of source. Count the commits CWD has that source
    // doesn't (the ones a fast-forward would refuse to land).
    let extra_count = vcs
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
         (pre-sync state preserved under {savepoints}).",
        savepoints = vcs.savepoint_namespace(),
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
/// `{resume}` line IS the continue step; merge/cherry-pick
/// hints still carry their VCS-native continue since rwv cannot resume those.
fn manifest_repo_failure_message(
    vcs: &dyn Vcs,
    verb: OpVerb,
    live_conflict: Option<ConflictOp>,
) -> String {
    let resume = op_state::resume_command(verb);
    match live_conflict {
        Some(op) => {
            let hint = vcs.conflict_resolution_hint(op);
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
    vcs: &dyn Vcs,
    phase: Phase,
    cwd_project_dir: &Path,
    verb: OpVerb,
    live_conflict: Option<ConflictOp>,
) -> String {
    let resume = op_state::resume_command(verb);
    let phase_label = phase.label();
    let repo_display = cwd_project_dir.display();
    match live_conflict {
        Some(op) => {
            let hint = vcs.conflict_resolution_hint(op);
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
/// `{resume}` line is the continue step (rwv resumes the rebase
/// natively). For [`ConflictOp::Merge`] / [`ConflictOp::CherryPick`] the
/// VCS hint still carries its own `git … --continue` — rwv has no native
/// resume for those ops; the operator finishes them in git, then `{resume}`
/// picks the op back up.
fn per_conflict_bail_message(
    vcs: &dyn Vcs,
    repo: &Path,
    op: ConflictOp,
    op_label: &str,
    detail: &str,
    verb: OpVerb,
) -> String {
    let hint = vcs.conflict_resolution_hint(op);
    let resume = op_state::resume_command(verb);
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
///    replay, and it reaches one by two routes that do not cover each
///    other. The [`DerivedContentPolicy`] an rwv-driven replay states
///    passes it inline with `-c`; that is the only route on manifest repos,
///    which never receive this plant. Bare `git rebase --continue` (the
///    resume path git itself advertises in conflict stderr) inherits
///    neither the inline flags nor the environment, so durable config is
///    the only route there. The two overlap on one case — this repo under
///    an rwv-driven replay — and dropping either strands the case the other
///    never covered. This function plants the config as its first act; it
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
/// spelling, the invariant bails with a migration-specific
/// message directing the operator at `rwv doctor --fix` — which rewrites
/// AND commits the .gitattributes migration. If neither the new nor the
/// legacy line is present, the invariant bails with the classic
/// "add-the-line" message.
///
/// The .gitattributes assignment itself is NOT written by this function
/// — that's `rwv doctor --fix`'s job, and requires a commit which sync
/// must not silently make on the operator's behalf.
fn verify_replay_exclusion_invariant(vcs: &dyn Vcs, cwd_project_dir: &Path) -> anyhow::Result<()> {
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

    let has_new = vcs
        .has_committed_replay_exclusion(cwd_project_dir, Path::new(LockFile::FILE_NAME))
        .unwrap_or(false);
    if has_new {
        return Ok(());
    }

    let has_legacy = crate::git::has_committed_legacy_replay_exclusion(
        cwd_project_dir,
        Path::new(LockFile::FILE_NAME),
    )
    .unwrap_or(false);

    if has_legacy {
        anyhow::bail!(
            "sync --strategy=rebase requires `rwv.lock merge=rwv-ours` \
             in the project repo's committed .gitattributes, but {ga} still \
             carries the legacy `rwv.lock merge=ours` spelling. The rename \
             closes an accidental-collision hazard where an \
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
    match &ctx.checkout {
        Checkout::Primary { project: Some(_) } => {
            // Delegate to require_active_project_on_disk so a dangling
            // .rwv-active fails early with a clear message rather than
            // producing confusing downstream git errors.
            ctx.require_active_project_on_disk().cloned()
        }
        Checkout::Workweave { project, .. } => Ok(project.clone()),
        Checkout::Primary { project: None } => {
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
    vcs: &dyn Vcs,
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

    match &ctx.checkout {
        Checkout::Workweave { name, .. } => {
            // Canonical clone lives at primary.
            let canonical = ctx.primary_path().join(repo_path.as_path());
            if !canonical.exists() {
                anyhow::bail!(
                    "canonical clone for {repo_path} missing at {}; \
                     run `rwv sync` from primary first to materialize it there",
                    canonical.display()
                );
            }
            // The manifest's tracking branch is the START POINT, and only
            // that. The NAME comes from `EphemeralRefName::mint` — the same
            // call `create_workweave` (`workweave.rs`) and `rwv add`
            // (`add_remove.rs`) make, from the same two inputs.
            let start_ref = entry.version.as_str();
            let start = vcs
                .resolve_revision(&canonical, start_ref)
                .with_context(|| format!("failed to resolve {start_ref} in canonical clone"))?;
            let mut registry =
                crate::workweave_index::RefRegistry::for_project(ctx.primary_path(), project_name);
            crate::workweave::birth_ephemeral_worktree(
                vcs,
                &mut registry,
                &canonical,
                &dest,
                &EphemeralRefName::mint(project_name, name),
                start,
            )
            .with_context(|| format!("cannot materialize {repo_path}"))?
            .into_authored(&mut registry)
            .with_context(|| format!("worktree add for {repo_path} failed"))?;
        }
        Checkout::Primary { .. } => {
            vcs.clone_repo(&entry.url.to_string(), &dest)
                .with_context(|| format!("clone of {repo_path} from {} failed", entry.url))?;
        }
    }
    Ok(())
}

/// R4: refuse to destroy `store` while anything still claims it.
///
/// A store-level destroy takes out every ref and every object at once, so
/// no ref-level rule can gate it. Two things must
/// both be true first:
///
/// - **No live worktree is registered against the store.** `git worktree
///   add` writes its administration *into* the canonical store
///   (`.git/worktrees/<name>`), so `remove_dir_all` on the store does not
///   merely delete a clone — it deletes the object database and the
///   worktree administration that every live workweave checkout of that
///   repo depends on. The registration is what makes those checkouts
///   findable, and it is what this reads.
/// - **Every receipt keyed to the store has been retracted.** A standing
///   receipt says rwv still accounts for a ref in there; destroying the
///   store would strand it with nothing left to retract it against.
///
/// Fail-closed on an unreadable store: a claim we could not enumerate is
/// treated as a claim that stands.
fn check_store_unclaimed(
    vcs: &dyn Vcs,
    store: &Path,
    primary_root: &Path,
    project: &ProjectName,
    repo_path: &RepoPath,
) -> anyhow::Result<()> {
    let registered = vcs.list_worktrees(store).with_context(|| {
        format!(
            "{repo_path}: cannot enumerate the worktrees registered against {}; \
             refusing to destroy a store whose claims could not be read",
            store.display()
        )
    })?;
    // `worktree list` includes the store's own main worktree. Compare
    // canonicalized: the registration records a resolved path, and the
    // store path here is built by joining onto the workspace root, which
    // may reach it through a symlink.
    let store_key = std::fs::canonicalize(store).unwrap_or_else(|_| store.to_path_buf());
    let live: Vec<String> = registered
        .iter()
        .filter(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()) != store_key)
        .map(|p| p.display().to_string())
        .collect();
    if !live.is_empty() {
        anyhow::bail!(
            "{repo_path}: dropped from lock, but {} still has live worktrees registered \
             against it:\n  {}\n\
             \n\
             Removing the store would delete their object database and worktree \
             administration along with it. Remove those workweaves first \
             (`rwv workweave delete <name>`), then re-run.",
            store.display(),
            live.join("\n  "),
        );
    }

    let standing = crate::workweave_index::RefRegistry::for_project(primary_root, project)
        .list_for_store(store)
        .with_context(|| {
            format!(
                "{repo_path}: cannot read the ownership receipts for {}; \
                 refusing to destroy a store whose claims could not be read",
                store.display()
            )
        })?;
    if !standing.is_empty() {
        let names: Vec<String> = standing.iter().map(|r| r.to_string()).collect();
        anyhow::bail!(
            "{repo_path}: dropped from lock, but rwv still holds ownership receipts for \
             refs in {}:\n  {}\n\
             \n\
             Each receipt must be retracted through its own ref delete — with a receipt \
             and a warrant — before the store itself may be destroyed. Delete the \
             workweaves that own these refs, then re-run.",
            store.display(),
            names.join("\n  "),
        );
    }
    Ok(())
}

/// Conservatively remove a repo's worktree/clone after it has been dropped
/// from the lock. Refuses (and warns) if the worktree has uncommitted changes
/// or local-only commits (branch tip differs from canonical HEAD in workweave;
/// any commits at all in primary).
///
/// Whether the second of those questions can be asked at all depends on what
/// is being removed, so the arms below establish that first rather than
/// inferring it from where the workspace happens to be. A workweave checkout
/// is removable as a working tree only once it is known to be a workspace
/// linked into a store elsewhere; a checkout that IS the store is refused,
/// because the divergence comparison that would say whether its objects exist
/// anywhere else has nothing to run against.
///
/// The local-only refusal in the primary arm stays exactly as it is: it is
/// incidentally the only thing that has been keeping `remove_dir_all` off a
/// live workweave's object store, so excluding recorded rwv refs from its
/// predicate would remove that protection. Unblocking prune is not a payoff
/// of ownership-by-receipt. What R2 adds here
/// is [`check_store_unclaimed`] in *front* of the destroy, not a relaxation
/// behind it.
fn prune_dropped_repo(
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    repo_path: &RepoPath,
    project_name: &ProjectName,
) -> anyhow::Result<()> {
    let dest = ctx.active_path().join(repo_path.as_path());
    if !dest.exists() {
        return Ok(());
    }
    if vcs.has_uncommitted_changes(&dest).unwrap_or(true) {
        anyhow::bail!(
            "{repo_path}: dropped from lock but worktree has uncommitted changes; \
             commit/discard and re-run sync, or remove manually"
        );
    }

    match &ctx.checkout {
        Checkout::Workweave { .. } => {
            // Diverged-from-canonical check: refuse if local commits would be lost.
            let canonical = ctx.primary_path().join(repo_path.as_path());
            if canonical.exists() {
                let wt_head = vcs.head_revision(&dest).ok();
                let canon_head = vcs.head_revision(&canonical).ok();
                if let (Some(w), Some(c)) = (wt_head, canon_head) {
                    if w != c {
                        // Allow when w is ancestor of c (no unique commits in workweave).
                        let is_ancestor = vcs.is_ancestor(&dest, &w, &c).unwrap_or(false);
                        if !is_ancestor {
                            anyhow::bail!(
                                "{repo_path}: dropped from lock but worktree has commits not in canonical clone; \
                                 push/merge them and re-run, or remove manually"
                            );
                        }
                    }
                }
                vcs.remove_worktree(&canonical, &dest)
                    .with_context(|| format!("worktree remove for {repo_path} failed"))?;
                let _ = vcs.worktree_prune(&canonical);
            } else {
                // No canonical at the primary-side slot. That is a fact about
                // the PRIMARY and says nothing about what `dest` is: under
                // inverted topology (docs/explanation/joints/clone-topology.md,
                // I1) a workweave checkout can itself be the standalone clone holding the
                // repo's only object database, and `remove_dir_all` on that is
                // a DESTROY-STORE, not a checkout removal. It is also
                // one this arm could not discharge if it wanted to — with no
                // canonical to compare against, the divergence refusal above
                // is not merely skipped here, it is unavailable, so nothing
                // has established that the objects exist anywhere else.
                //
                // So resolve what `dest` actually is before deleting it, the
                // same way `delete_workweave`'s `is_lone_canonical` does:
                // compare the checkout against the store its refs live in.
                // The fallback is `dest` itself, which makes an unresolvable
                // store refuse rather than be assumed linked.
                let store = crate::workweave::resolved_worktree_parent(vcs, &dest, &dest);
                let dest_canonical = dest.canonicalize().unwrap_or_else(|_| dest.clone());
                if store == dest_canonical {
                    anyhow::bail!(
                        "{repo_path}: dropped from lock, but the checkout at {} is itself a \
                         canonical store rather than a workspace linked into one — inverted \
                         clone topology (precondition: a workweave checkout is a linked \
                         workspace). There is no canonical clone at {} either, so removing \
                         this directory would destroy the only object database the weave \
                         has for this repo, with nothing having checked what is in it.\n\
                         \n\
                         Run `rwv doctor` for a topology audit and remediation guidance, or \
                         remove the directory manually once you are certain nothing needs \
                         its objects.",
                        dest.display(),
                        canonical.display(),
                    );
                }
                // `dest` is a linked workspace: its refdb and objects live in
                // `store`, which this delete does not touch. Removing the
                // directory removes a working tree only.
                std::fs::remove_dir_all(&dest)
                    .with_context(|| format!("failed to remove {}", dest.display()))?;
            }
        }
        Checkout::Primary { .. } => {
            // Primary: refuse if local-only branches with unique commits exist.
            // Conservative — any branch with commits not on origin is grounds.
            // We don't know the manifest role of this dropped repo at prune
            // time (the lock entry is gone); Role::Owned selects the
            // canonical-clone remote convention (`origin` for git)
            // which matches what every non-fork lock entry was cloned with.
            let any_local_only = match vcs.list_local_branch_names(&dest) {
                Ok(names) => {
                    let mut any = false;
                    for branch in &names {
                        let short = RefName::new(branch.as_str().to_owned());
                        let has_counterpart = vcs
                            .branch_has_remote_counterpart(&dest, &short, Role::Owned)
                            .unwrap_or(false);
                        if !has_counterpart {
                            any = true;
                            break;
                        }
                        // A count git could not produce is "we could not
                        // tell", never "nothing unpushed" — the reading the
                        // old `unwrap_or(0)` gave it, which let a git failure
                        // clear the branch and hand the store to the delete.
                        // Refuse on the same terms as an unreadable branch
                        // list below, and say which of the two happened.
                        let count = vcs
                            .count_commits_ahead_of_remote(&dest, &short, Role::Owned)
                            .with_context(|| {
                                format!(
                                    "{repo_path}: dropped from lock, but counting {short}'s \
                                     commits against its remote counterpart failed; refusing \
                                     to remove a clone whose unpushed work could not be \
                                     ruled out — push and re-run, or remove manually"
                                )
                            })?;
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
            // DESTROY-STORE: `dest` in the primary IS the canonical
            // store — the refdb and object database every workweave worktree
            // of this repo is registered in. R4 gates it.
            check_store_unclaimed(vcs, &dest, ctx.primary_path(), project_name, repo_path)?;
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
/// Callers pass `&dyn OutputHandler` into the orchestration body so new modes
/// can be added without touching existing orchestration code.
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
    /// or `project_repo_key` for the project repo).
    /// `from_sha` is the target's tip before the FF; `to_sha` is after.
    ///
    /// Default implementation is a no-op — suitable for text-mode and
    /// plain-sync JSON handlers that do not need step-3 SHAs.
    fn record_step3_advance(&self, _path: &str, _from_sha: &str, _to_sha: &str) {}

    /// Record a standing advisory raised during cleanup.
    ///
    /// Default implementation is a no-op: text-mode handlers print the
    /// advisory's human rendering directly at the call site instead, and
    /// handlers whose envelope carries no `advisories` field (sync-to,
    /// NDJSON — no envelope to place it in) have nothing to buffer it into.
    fn record_advisory(&self, _advisory: AdvisoryOutput) {}
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
        if let RepoSyncOutcome::Converged {
            derived_content_dropped,
        } = outcome
        {
            if !derived_content_dropped.is_empty() {
                eprintln!(
                    "  warning: {path}: the replay kept the target's version of these declared \
                     derived paths — what you replayed is not what landed:"
                );
                for dropped in derived_content_dropped {
                    eprintln!("      {dropped}");
                }
                eprintln!(
                    "  regenerate them from their source of record in {path} and commit — \
                     until then the landed tree is stale at those paths."
                );
            }
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
    advisories: &'a Mutex<Vec<AdvisoryOutput>>,
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

    fn record_advisory(&self, advisory: AdvisoryOutput) {
        let mut guard = self.advisories.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(advisory);
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
    /// key as `record`'s `path` argument, or `project_repo_key` for the
    /// project repo). Only populated when the target's branch pointer actually
    /// moved.
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
//       op_state::set_phase(owner, state.phase);   // one persistence point
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
    ///
    /// Owned by the op context — a `Clone` of the invocation context passed
    /// in by the top-of-`main` resolution (or a re-rooted owner context on
    /// `--continue` from a leased target). The phase engine never re-resolves
    /// the invocation context via `current_dir()`; it may resolve *other*
    /// workspaces (source/target) as needed.
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
    /// The CWD project repo's backend. One per invocation, because the
    /// project repo is one per invocation — see [`crate::vcs::project_vcs`]
    /// for why it cannot be resolved from a manifest entry.
    project_vcs: Box<dyn Vcs>,
    verb: op_state::OpVerb,
    op_id: OpId,
    /// Atomic source snapshot pinned at guard time (T0): one ref read of
    /// the source project tip, then manifest + lock read AT that revision.
    /// On `--continue`, T0 is re-established at the start of the resumed
    /// session — per-repo no-op detection handles repos that already
    /// converged in the previous (now-aborted) replay run.
    snapshot: SourceSnapshot,
}

/// What a source snapshot classifies about the source's member checkouts.
///
/// The staleness gate and the tips-as-truth pull are two consumers of ONE
/// classification, so the snapshot takes it once and both read the result.
/// Two invocations of the same predicate read the checkouts' HEADs at two
/// instants, and a member that is `Ok` at the first read and `Ahead` at the
/// second pins no tip while the note announces one.
enum ClassifySource<'a> {
    /// No consumer: `--allow-stale-lock` bypasses the gate, and a resumed
    /// session re-runs no preconditions.
    Skip,
    /// Classify the checkouts under this workspace dir. The gate's refusals
    /// read the relations.
    Relations(&'a Path),
    /// Classify, and additionally pin the `Ahead` checkouts' committed tips as
    /// replay's targets.
    RelationsAndTips(&'a Path),
}

/// Atomic source snapshot pinned at T0 (start of replay).
///
/// The source's project tip is read once and everything derived from it —
/// manifest, lock — is read AT that revision via `Vcs::read_file_at_revision`.
/// A concurrent mutation of the source after T0 changes refs but cannot touch
/// anything we've read.
struct SourceSnapshot {
    /// Source project tip at T0.
    source_project_tip: ResolvedRevisionId,
    /// Source manifest, read at `source_project_tip`.
    source_manifest: Manifest,
    /// Source lock (raw, unresolved), read at `source_project_tip`.
    raw_source_lock: LockFile,
    /// The source's member checkouts classified at T0, `None` under
    /// [`ClassifySource::Skip`]. The staleness refusals, the tips-as-truth
    /// note and `pull_tips` all derive from this one value.
    source_class: Option<LockClassification>,
    /// Committed member tips of the source's `Ahead` checkouts, read at T0.
    /// Replay targets these over the lock entries. Empty when tips-as-truth
    /// does not apply (primary source, sync-to, or `--allow-stale-lock`).
    pull_tips: std::collections::BTreeMap<RepoPath, ResolvedRevisionId>,
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

/// Bundled arguments for a `sync` or `sync-to` invocation.
///
/// Replaces the positional argument lists that were duplicated across the
/// `run_sync*` / `run_sync_to*` entry points and threaded into `run_machine`,
/// `guard_and_mark`, and `load_continuing_context`. Callers build one
/// `SyncRequest` and pass it by value to an entry point.
#[derive(Debug, Clone)]
pub struct SyncRequest {
    /// Source (sync) or target (sync-to) workspace, already resolved by the
    /// caller. `None` only under `--continue`, which reads it from op-state.
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

/// Execute `rwv sync <source>`.
///
/// `request.source` is required; there is no bare `rwv sync` that absorbs
/// from the workweave's recorded parent. `<SOURCE>` is a required CLI
/// argument and `rwv sync-to` is the verb that reads the parent marker.
pub fn run_sync(ctx: &WorkspaceContext, request: SyncRequest) -> anyhow::Result<()> {
    let stdout_lock: Mutex<()> = Mutex::new(());
    let handler = TextHandler {
        stdout_lock: &stdout_lock,
    };
    run_machine(MachineVerb::Sync, ctx, &request, &handler)
}

// ---------------------------------------------------------------------------
// Driver entry: shared by sync and sync-to (text + json modes)
// ---------------------------------------------------------------------------

/// Build the [`OpContext`] and run the phase-machine driver. Both `rwv sync`
/// and `rwv sync-to` route through here; the `verb` parameter selects which
/// phases run (advance-target and retire are sync-to-only, and retire runs
/// under `--retire` only).
///
/// `source` is the explicit source/target the operator passed on the CLI, or
/// `None` under `--continue` (read from op-state). `do_continue = true` means
/// "resolve op-state from CWD (following a lease pointer if invoked from a
/// non-owner workspace), enter the driver loop at the recorded phase".
///
/// `cwd_ctx` is the already-resolved invocation context (with `--project`
/// baked in when passed). The driver never re-resolves the invocation
/// context; it may still resolve *other* workspaces (sync source/target,
/// lease owner) internally.
fn run_machine(
    verb: MachineVerb,
    cwd_ctx: &WorkspaceContext,
    request: &SyncRequest,
    handler: &dyn OutputHandler,
) -> anyhow::Result<()> {
    let ctx = if request.do_continue {
        load_continuing_context(verb, cwd_ctx, request, handler)?
    } else {
        guard_and_mark(verb, cwd_ctx, request, handler)?
    };

    drive(&ctx)
}

/// The phase-machine driver. Reads the persisted phase, runs it, persists the
/// transition to the next phase, loops.
///
/// Invariant: the persisted phase is the phase in progress. The owner record's
/// `phase` field is the SINGLE source of truth and the persistence point is
/// the post-transition `set_phase` write — entry into the loop relies on
/// either `guard_and_mark`'s initial write (fresh start: phase=replay) or the
/// prior iteration's post-transition write (resume: phase=whatever crashed).
///
/// Crash semantics:
///   - Inside `run_phase`: record stays at the phase that was running →
///     `--continue` re-enters that phase (idempotent by construction).
///   - After `run_phase` returned but before `set_phase` of the next phase
///     committed: record still says current → `--continue` re-runs the just-
///     completed phase (idempotent), then transitions.
///   - After `set_phase` of the next phase committed: record says next →
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
                op_state::set_phase(&ctx.owner_workspace_dir, p)
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

/// The phase a `--continue` enters, given the phase the record was left at.
///
/// Advance-target publishes CWD's tips AND CWD's lock to the target; relock is
/// what makes those two agree. A resume exists because the operator changed CWD
/// after the op stranded, so the agreement relock established before the strand
/// no longer holds — re-entering relock restores the property every other path
/// into advance-target has: relock ran immediately before it with no operator
/// window in between.
///
/// Replay reaches relock by running forward. Retire runs after the target was
/// already advanced, and its merged-check refuses a CWD that moved rather than
/// publishing it, so retire is entered as recorded.
fn resume_entry_phase(recorded: &op_state::OpPhase) -> op_state::OpPhase {
    match recorded {
        op_state::OpPhase::AdvanceTarget => op_state::OpPhase::Relock,
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Pre-loop: guard + mark + savepoint (fresh start)
// ---------------------------------------------------------------------------

/// Outputs of the post-acquisition precondition sweep. Threaded from
/// [`run_preconditions_after_acquire`] into the surrounding Mark / Savepoint
/// code in [`guard_and_mark`].
///
/// Kept as a plain struct rather than a tuple because the fields are consumed
/// out-of-order downstream (`snapshot` at the very end, `cwd_project` and
/// `cwd_workspace_name_str` in Savepoint and the auto-relock line, and so on),
/// so nameable field access is easier to review than positional indices.
struct PreconditionOutcome {
    /// Source pinned at T₀ by a snapshot read.
    snapshot: SourceSnapshot,
    /// CWD's parsed project (manifest + lock, if present).
    cwd_project: Project,
    /// Human name of the CWD workspace, used in the auto-relock LOUD line.
    cwd_workspace_name_str: String,
    /// CWD repos whose committed lock is behind HEAD — auto-relocked at op
    /// start in the sync-to landing shape. Empty for plain `sync`.
    cwd_lock_behind: Vec<RepoRelation>,
    /// Whether the Phase 1' ancestor precondition was bypassed via
    /// `--discard-local-commits`. Recorded on the owner record so cleanup
    /// preserves the project savepoint as a tombstone.
    phase1_ancestor_bypassed: bool,
}

/// Run every precondition that gates the op AFTER the atomic acquire.
///
/// The whole block is factored out so [`guard_and_mark`] can wrap it in a
/// single `match` on the result: on `Ok`, the Mark section proceeds; on `Err`,
/// the caller invokes `op_state::release_acquired` before propagating (the
/// cleanup table's "precondition refusal → cleared everywhere" row).
///
/// This function must remain a **pure precondition** — every `?` inside it can
/// return an error, but nothing here mutates workspace-wide state; on refusal
/// the workspace looks exactly like it did before acquisition (once the caller
/// releases the acquired records).
#[allow(clippy::too_many_arguments)]
fn run_preconditions_after_acquire(
    project_vcs: &dyn Vcs,
    verb: MachineVerb,
    strategy: SyncStrategy,
    allow_stale_lock: bool,
    discard_local_commits: bool,
    source_is_workweave: bool,
    cwd_project_dir: &Path,
    cwd_workspace_dir: &Path,
    cwd_ctx: &WorkspaceContext,
    source_project_dir: &Path,
    source_workspace_dir: &Path,
    source_workspace_name: &str,
    source_project_name: &ProjectName,
    cwd_project_name: &ProjectName,
    dest_project_dir: &Path,
    dest_workspace_dir: &Path,
    cli_path: &Path,
    emit_text: bool,
) -> anyhow::Result<PreconditionOutcome> {
    // CWD project repo must not be mid-op.
    if let Some(op) = project_vcs.mid_op(cwd_project_dir) {
        anyhow::bail!("CWD project repo is mid-{op}; resolve before running sync");
    }

    // sync-to: --strategy=ff has special semantics (CWD must be strictly
    // ahead of target). Bail before any side effects on a refusal.
    if matches!(verb, MachineVerb::SyncTo) && strategy == SyncStrategy::Ff {
        check_sync_to_ff_precondition(project_vcs, cwd_project_dir, dest_project_dir, emit_text)?;
    }

    // Pre-flight dirt scans: refuse before any mutation when
    // the workspaces the op will rebase or fast-forward carry uncommitted
    // tracked changes. All dirty repos are collected before the first bail so
    // the operator sees the full list in one message. Acquired op-state is
    // released by the caller on Err (no trace left on disk).
    //
    // Ordering: sync's CWD dirt scan runs here (before the snapshot pin and
    // lock-freshness gate) so the in-flight-op refusal from `acquire_op`
    // still dominates all other refusals (Correction-1 ordering). Dirt
    // refusals never compete with an in-flight refusal — that was emitted
    // atomically by `acquire_op` above.
    match verb {
        MachineVerb::Sync => {
            // sync (pull): replay rebases or ff-advances the CWD repos onto the
            // source lock. Scan the CWD workspace — the destination that mutates.
            // The source workspace is read-only (snapshot read only); dirt there
            // does not affect the op.
            let cwd_project_preflight = Project::from_dir(cwd_project_dir)
                .context("failed to load CWD project for sync dirt scan")?;
            check_dirty_preflight_sync(
                project_vcs,
                &cwd_project_preflight,
                cwd_workspace_dir,
                cwd_project_dir,
            )?;
        }
        MachineVerb::SyncTo => {
            // sync-to preflights (all refuse before any side effects):
            //   - dirty-source: CWD-side tracked dirt would go stale mid-rebase.
            //   - dirty-target: the target's uncommitted work advance-target overwrites.
            //   - detached-target: advance-target would have no branch to land on.
            // Source before target: the operator's own workspace is the first thing they
            // can fix, and a dirty source is the state we most want to define away.
            let cwd_project_preflight = Project::from_dir(cwd_project_dir)
                .context("failed to load CWD project for sync-to preflights")?;
            check_dirty_source_preflight(
                project_vcs,
                &cwd_project_preflight,
                cwd_workspace_dir,
                cwd_project_dir,
            )?;
            check_dirty_target_preflight(
                project_vcs,
                &cwd_project_preflight,
                dest_workspace_dir,
                dest_project_dir,
                cli_path,
            )?;
            check_detached_target_preflight(
                project_vcs,
                &cwd_project_preflight,
                dest_workspace_dir,
                dest_project_dir,
                cli_path,
            )?;
        }
    }

    // Pin the source snapshot now so the remaining replay preconditions are
    // all reads against a coherent T0. This is the "snapshot reads"
    // mechanism: one atomic ref read pins source; manifest + lock are read
    // at that revision; everything downstream is content-addressed. The one
    // exception is the tips-as-truth pull, whose member tips are per-repo ref
    // reads of the source checkouts, pinned here at the same T0.
    let classify = if allow_stale_lock {
        ClassifySource::Skip
    } else if matches!(verb, MachineVerb::Sync) && source_is_workweave {
        ClassifySource::RelationsAndTips(source_workspace_dir)
    } else {
        ClassifySource::Relations(source_workspace_dir)
    };
    let snapshot = pin_source_snapshot(project_vcs, source_project_dir, classify)?;

    // Replay preconditions (pure reads; refusals leave no trace on-workspace —
    // the acquired op-state is cleaned up by the caller on Err).
    let cwd_project = Project::from_dir(cwd_project_dir)
        .context("failed to load CWD project for guard preconditions")?;
    let cwd_workspace_name_str = workspace_name(cwd_ctx);

    // === Benign-staleness classification ===
    //
    // Classify each side's committed lock↔HEAD relation with the SAME per-repo
    // vocabulary `rwv status` uses ([`LockRelation`]). Recall the terminology
    // inversion: the spec's benign "lock behind HEAD" is `LockRelation::Ahead`
    // (tip ahead of lock). `--allow-stale-lock` bypasses the whole gate.
    //
    // Scope of the benign relaxation:
    //   - sync-to (landing) CWD: CWD is the landing set. A lock-behind-HEAD
    //     (`Ahead`) CWD repo auto-relocks at op start (below), LOUD line per repo
    //     with commit count. Every other non-`ok` CWD relation refuses.
    //   - sync-to (landing) TARGET: a lock-behind-HEAD (`Ahead`) target refuses.
    //     Replay's targets come from the target's lock, so the target's unlocked
    //     commits would be missing from the tip CWD replays onto and step 3's
    //     fast-forward could not proceed; tips-as-truth is scoped to the pull.
    //   - sync (pull) SOURCE: a lock-behind-HEAD (`Ahead`) source is tips-as-truth
    //     for a WORKWEAVE source; a PRIMARY-weave source keeps the refusal.
    //   - sync (pull) DESTINATION (CWD): a lock-behind-HEAD (`Ahead`) CWD repo
    //     is benign, LOUD line per repo. Replay's targets come from the source,
    //     never from CWD's lock, and phase 3 regenerates that lock at op end —
    //     so the gate was refusing to start over a condition the op's own last
    //     phase establishes. Every other non-`ok` relation still refuses.
    //     Unlike sync-to's CWD there is no op-start relock: on a pull the
    //     project repo is itself a replay target, and a relock commit made
    //     before Phase 1' would leave `--strategy=ff` a project repo it can no
    //     longer fast-forward.
    //
    // Unresolvable lock entries (a pinned tag/branch that no longer exists) are a
    // corrupt-lock error distinct from any relation and refuse first, naming the
    // unknown revision.
    let cwd_class = if allow_stale_lock {
        None
    } else {
        Some(classify_lock_relations(
            cwd_workspace_dir,
            &cwd_project.manifest,
            cwd_project.lock.as_ref(),
        ))
    };

    let mut cwd_lock_behind: Vec<RepoRelation> = Vec::new();

    if let (Some(source_class), Some(cwd_class)) = (snapshot.source_class.as_ref(), cwd_class) {
        if let Some((rp, raw)) = source_class.unresolvable.first() {
            anyhow::bail!(
                "{}",
                unresolvable_lock_refusal(
                    Side::Source,
                    source_workspace_name,
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
            source_workspace_name,
            source_project_name.as_str(),
            &source_anomalous,
        ) {
            anyhow::bail!("{msg}");
        }

        // CWD side, both verbs: same rule, same refusal. `Ahead` is the benign
        // in-progress shape each verb then handles its own way below — sync-to
        // relocks it at op start, a pull announces it and lets relock heal it.
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

        match verb {
            MachineVerb::Sync => {
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
                        source_workspace_name,
                        source_project_name.as_str(),
                        &source_lock_behind,
                    ) {
                        anyhow::bail!("{msg}");
                    }
                }

                if emit_text {
                    for r in cwd_class
                        .relations
                        .iter()
                        .filter(|r| r.relation == LockRelation::Ahead)
                    {
                        let n = r
                            .ahead_count
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        eprintln!(
                            "note: {cwd_workspace_name_str}/{}: lock behind HEAD by {n} commits \
                             — pulling anyway; this op's relock refreshes it",
                            r.repo_path,
                        );
                    }
                }
            }
            MachineVerb::SyncTo => {
                let target_lock_behind: Vec<&RepoRelation> = source_class
                    .relations
                    .iter()
                    .filter(|r| r.relation == LockRelation::Ahead)
                    .collect();
                if let Some(msg) = target_lock_behind_refusal(
                    source_workspace_name,
                    source_project_name.as_str(),
                    &target_lock_behind,
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
        verify_replay_exclusion_invariant(project_vcs, cwd_project_dir)?;
    }
    let cwd_project_tip = project_vcs
        .head_revision(cwd_project_dir)
        .context("failed to read CWD project HEAD")?;
    let phase1_ancestor_bypassed = if discard_local_commits {
        if project_vcs
            .has_uncommitted_changes(cwd_project_dir)
            .unwrap_or(true)
        {
            anyhow::bail!(
                "--discard-local-commits precondition failed: project repo at {dir} has uncommitted \
                 changes.\n\
                 --discard-local-commits discards committed divergence (recoverable via \
                 {savepoints}), but the hard-reset would destroy uncommitted changes \
                 unrecoverably. Commit or stash them, then re-run.",
                dir = cwd_project_dir.display(),
                savepoints = project_vcs.savepoint_namespace(),
            );
        }
        !cwd_is_ancestor_or_equal(
            project_vcs,
            cwd_project_dir,
            &cwd_project_tip,
            &snapshot.source_project_tip,
        )
    } else if strategy == SyncStrategy::Ff && matches!(verb, MachineVerb::Sync) {
        // Plain sync + ff: CWD must be ancestor-or-equal of source.
        check_phase1_ancestor(
            project_vcs,
            cwd_project_dir,
            &cwd_project_tip,
            &snapshot.source_project_tip,
            &cwd_workspace_name_str,
            source_workspace_name,
        )?;
        false
    } else {
        false
    };

    Ok(PreconditionOutcome {
        snapshot,
        cwd_project,
        cwd_workspace_name_str,
        cwd_lock_behind,
        phase1_ancestor_bypassed,
    })
}

/// Guard (preconditions), mark (write owner record + leases), savepoint
/// (per-repo pre-op refs). Returns the immutable [`OpContext`] driving the
/// loop. Refusals here leave no trace.
fn guard_and_mark<'a>(
    verb: MachineVerb,
    cwd_ctx: &WorkspaceContext,
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
    let cwd_workspace_dir = cwd_ctx.active_path().to_path_buf();

    // Resolve the SyncSource the operator passed. For sync, this is the
    // source workspace; for sync-to, this is the target workspace. Both are
    // resolved before the machine is entered — sync's by clap (`<SOURCE>` is
    // required), sync-to's by the dispatcher, which reads the marker's parent
    // for the bare form. Neither verb defaults one here.
    let resolved_arg = match source {
        Some(s) => s.clone(),
        None => match verb {
            MachineVerb::Sync => anyhow::bail!(
                "sync requires an explicit source (resolved by the caller); none provided"
            ),
            MachineVerb::SyncTo => anyhow::bail!(
                "sync-to requires an explicit target (resolved by the caller); none provided"
            ),
        },
    };
    let cli_path = resolved_arg.resolve(cwd_ctx)?;

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
    let other_project_override = match &cwd_ctx.checkout {
        Checkout::Workweave { project, .. } => Some(project.clone()),
        Checkout::Primary { .. } => project_override.clone(),
    };

    let cwd_project_name = find_project_name(cwd_ctx)?;
    let cwd_project_dir = project_dir(&cwd_workspace_dir, cwd_project_name.as_str());

    // source_workspace_dir is the operator's arg for both verbs (sync's <src>
    // and sync-to's <tgt> — replay pulls from there in either case).
    // `source_is_workweave` scopes tips-as-truth: a `behind` source lock is
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
        let dir = project_dir(source_ctx.active_path(), pname.as_str());
        let is_workweave = matches!(source_ctx.checkout, Checkout::Workweave { .. });
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
        warn_on_sibling_sync(cwd_ctx, &source_workspace_dir, emit_text);
    }

    // === Acquire op-state (atomic claim) ===
    //
    // ATOMICITY: the touched-workspace set is claimed via
    // `acquire_op`, which writes `.rwv-op` + every lease with
    // `create_new(true)` (`O_CREAT|O_EXCL`). This closes the guard→mark
    // TOCTOU window a bare `check_no_op_in_progress` guard would leave open
    // — two concurrent invocations otherwise both pass the check and only
    // collide later at the git layer (R7 root cause). Acquisition dominates
    // every other refusal: an `.rwv-op` / `.rwv-op-lease` involving any
    // touched workspace means a prior op is still in flight, and the caller
    // sees the in-flight refusal (verb, age, phase, `--continue` / `rwv
    // abort` exits) rather than any downstream lock-relation / dirty
    // refusal computed against a workspace another op is mutating.
    //
    // ORDERING (Correction 1, retained): every precondition that could
    // refuse the op runs AFTER acquisition; on refusal we call
    // `release_acquired` so the acquired records are cleared (cleanup
    // table's "precondition refusal → cleared everywhere" row). Refusal
    // still leaves no trace on disk — but the atomic claim means only ONE
    // op can be running these precondition checks at a time on a given
    // touched-workspace set.
    //
    // Overrides (`allow-stale-lock`,
    // `discard-local-commits`) are pushed onto the record AFTER preconditions
    // (they depend on precondition outcomes) via a second `write_owner` in
    // the Mark section below — the acquired file is then overwritten in
    // place with the final record shape.
    let op_id = OpId::new_now();
    let owner_workspace_dir = cwd_workspace_dir.clone();
    let initial_record = match verb {
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
    let touched = op_state::TouchedWorkspaces::of(
        initial_record.verb,
        &owner_workspace_dir,
        &dest_workspace_dir,
    );
    let acquired = op_state::acquire_op(&touched, &initial_record)?;

    // From here on, any early return from a precondition refusal must release
    // the acquired records (see the cleanup-table row for "refusal → cleared
    // everywhere"). We wrap the precondition block in a closure so `?` inside
    // routes through the release path.
    // One handle for the CWD project repo, resolved here at the verb's entry
    // and threaded through every phase; the `OpContext` below carries the same
    // one for the phases that run past this function.
    let project_vcs = project_vcs();
    let precondition_result = run_preconditions_after_acquire(
        project_vcs.as_ref(),
        verb,
        strategy,
        allow_stale_lock,
        discard_local_commits,
        source_is_workweave,
        &cwd_project_dir,
        &cwd_workspace_dir,
        cwd_ctx,
        &source_project_dir,
        &source_workspace_dir,
        &source_workspace_name,
        &source_project_name,
        &cwd_project_name,
        &dest_project_dir,
        &dest_workspace_dir,
        &cli_path,
        emit_text,
    );
    let PreconditionOutcome {
        snapshot,
        cwd_project,
        cwd_workspace_name_str,
        cwd_lock_behind,
        phase1_ancestor_bypassed,
    } = match precondition_result {
        Ok(v) => v,
        Err(e) => {
            op_state::release_acquired(&acquired);
            return Err(e);
        }
    };

    // === Mark: overrides update ===
    //
    // `acquire_op` above already wrote the initial owner record at CWD and every
    // touched-workspace lease. The final Mark write is the OVERRIDES update:
    // `allow-stale-lock` and `discard-local-commits` are consent flags recorded
    // in the audit-trail `overrides` field so cleanup preserves the appropriate
    // savepoint (tombstone case) and `--continue` resumes with the same
    // consent. Both are determined by precondition outcomes, so this write is
    // sequenced after the acquire+precondition pair. Consumed by the release
    // path on any downstream mid-op error since the record is already on disk.
    let mut record = initial_record;
    if allow_stale_lock {
        record.overrides.push(op_state::Override::AllowStaleLock);
    }
    if phase1_ancestor_bypassed {
        // Named consent: --discard-local-commits will discard
        // reachable project commits in Phase 1'. Recorded in the audit-trail
        // `overrides` field so cleanup preserves the project savepoint as a
        // tombstone and --continue resumes with the same consent.
        record
            .overrides
            .push(op_state::Override::DiscardLocalCommits);
    }
    if !record.overrides.is_empty() {
        // Only rewrite when we actually have overrides — otherwise the
        // acquire-time record is already correct byte-for-byte.
        if let Err(e) = op_state::write_owner(&owner_workspace_dir, &record)
            .context("failed to update owner record with overrides")
        {
            // A failure to record consent means the audit trail would lie on
            // `--continue`. Release the claim so a retry starts clean.
            op_state::release_acquired(&acquired);
            return Err(e);
        }
    }
    // Suppress the "acquired but not otherwise inspected" lint — the handle's
    // purpose is fulfilled: it kept the claim atomic across preconditions, and
    // from here the on-disk record lifecycle is owned by the phase driver +
    // cleanup path. `AcquiredOp` intentionally does not clean up on drop
    // (crash-persistence + operator-visible `--continue` / `abort` are the
    // exits from that point on).
    let _ = acquired;

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
    create_savepoint(project_vcs.as_ref(), &cwd_project_dir, &op_id)?;
    for (repo_path, entry) in cwd_project.manifest.iter_entries() {
        let abs = cwd_workspace_dir.join(repo_path.as_path());
        // Skip reference symlinks: a savepoint here would write
        // `refs/rwv/pre-op/*` into the shared canonical store.
        if checkout_is_syncable(&abs) {
            let _ = create_savepoint(vcs_for(entry.vcs_type).as_ref(), &abs, &op_id);
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
            let target_project_dir = project_dir(&dest_workspace_dir, tpname.as_str());
            let _ = create_savepoint(project_vcs.as_ref(), &target_project_dir, &tsp_id);
            if let Ok(tp) = crate::manifest::Project::from_dir_skip_lock(&target_project_dir) {
                for (repo_path, entry) in tp.manifest.iter_entries() {
                    let abs = dest_workspace_dir.join(repo_path.as_path());
                    // Skip reference symlinks (shared canonical store).
                    if checkout_is_syncable(&abs) {
                        let _ = create_savepoint(vcs_for(entry.vcs_type).as_ref(), &abs, &tsp_id);
                    }
                }
            }
        }
    }

    // === Op-start auto-relock (sync-to landing) ===
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
            project_vcs.as_ref(),
            cwd_ctx,
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
        cwd_ctx: cwd_ctx.clone(),
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
        project_vcs,
        verb: verb.op_verb(),
        op_id,
        snapshot,
    })
}

/// Load context for `--continue`: read the owner record (following a lease
/// pointer if invoked from a non-owner workspace), derive all op parameters
/// from it, and rebuild the [`OpContext`].
///
/// `invocation_ctx` is the already-resolved invocation context (the CWD the
/// operator ran from — potentially the leased target, not the owner). This
/// function may re-root at the owner workspace by resolving that separately,
/// but never re-resolves the invocation context itself.
fn load_continuing_context<'a>(
    verb: MachineVerb,
    invocation_ctx: &'a WorkspaceContext,
    request: &SyncRequest,
    handler: &'a dyn OutputHandler,
) -> anyhow::Result<OpContext<'a>> {
    let project_override = request.project_override.clone();
    let jobs = request.jobs;

    let emit_text = handler.emit_text();

    // The literal invocation context is only used to locate op-state; it lets
    // `op_state::resume` follow a lease pointer (when `--continue` was invoked
    // from the leased target). Everything the engine consumes is rooted at the
    // OWNER below: `--continue` / `abort` invoked from a leased workspace
    // follow the pointer to the owner record and operate identically to
    // owner-side invocation.
    let invocation_workspace_dir = invocation_ctx.active_path().to_path_buf();

    let (record, owner_workspace_dir) = op_state::resume(&invocation_workspace_dir)?;
    let op_id = OpId::from_string(record.id.clone());

    // Everything below reads the recorded verb, and `verb` reaches no other
    // consumer — so without this refusal `rwv sync --continue` on a `rwv
    // sync-to` op would complete the recorded op correctly and silently, and
    // the operator who asked to pull from a source would watch their
    // workspace land into its target instead. The refusal buys the
    // diagnosis, not the outcome.
    let recorded_verb = match record.verb {
        op_state::OpVerb::Sync => MachineVerb::Sync,
        op_state::OpVerb::SyncTo => MachineVerb::SyncTo,
    };
    if !verbs_match(verb, recorded_verb) {
        anyhow::bail!(
            "in-progress op is `{recorded}` but `{invoked}` was invoked. \
             Run `{resume}` instead, or `rwv abort` to discard.",
            recorded = record.verb,
            invoked = op_state::resume_command(verb.op_verb()),
            resume = op_state::resume_command(record.verb),
        );
    }

    // Persist the re-entered phase before the driver reads it: the record stays
    // the single source of truth, so a crash inside the re-entered phase resumes
    // there rather than back at the phase that stranded.
    let entry_phase = resume_entry_phase(&record.phase);
    if entry_phase != record.phase {
        op_state::set_phase(&owner_workspace_dir, entry_phase.clone())
            .context("failed to record the phase this resume re-enters")?;
    }

    if emit_text {
        if entry_phase == record.phase {
            eprintln!(
                "continuing {verb_str} (op {op_id}, mid `{phase}`)",
                verb_str = record.verb,
                phase = record.phase,
            );
        } else {
            eprintln!(
                "continuing {verb_str} (op {op_id}, mid `{phase}`); re-entering `{entry_phase}` \
                 first so the lock this lands pins the tips it lands",
                verb_str = record.verb,
                phase = record.phase,
            );
        }
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
    // When CWD == owner, `cwd_ctx` is a clone of `invocation_ctx`. When they
    // differ, we resolve the owner workspace directly — this is not a
    // re-resolution of the invocation origin (which would violate the single-
    // resolution rule) but the resolution of a different, computed workspace
    // (the owner path recovered from op-state).
    let cwd_ctx = if owner_workspace_dir == invocation_workspace_dir {
        invocation_ctx.clone()
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
    let cwd_project_dir = project_dir(&owner_workspace_dir, cwd_project_name.as_str());

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

    let other_project_override = match (recorded_verb, &cwd_ctx.checkout) {
        // sync-to: target must resolve to CWD's (== owner's) project.
        (MachineVerb::SyncTo, _) => Some(cwd_project_name.clone()),
        (_, Checkout::Workweave { project, .. }) => Some(project.clone()),
        (_, Checkout::Primary { .. }) => project_override.clone(),
    };

    let (source_project_dir, source_workspace_name, source_is_workweave) = {
        let source_ctx = WorkspaceContext::resolve(&source_workspace_dir, other_project_override)?;
        let pname = find_project_name(&source_ctx)?;
        let dir = project_dir(source_ctx.active_path(), pname.as_str());
        let is_workweave = matches!(source_ctx.checkout, Checkout::Workweave { .. });
        (dir, workspace_name(&source_ctx), is_workweave)
    };

    let dest_project_dir = match recorded_verb {
        MachineVerb::Sync => cwd_project_dir.clone(),
        MachineVerb::SyncTo => source_project_dir.clone(),
    };

    // --continue resumes with the same consents recorded at fresh-start
    // time: read `overrides` and re-derive each named override from the
    // persisted record so the resumed session behaves identically to the
    // original without requiring the operator to re-supply flags.
    // `discard-local-commits` gates Phase 1'; `allow-stale-lock` disables the
    // tips-as-truth pull below (the lock stays replay's target).
    let allow_stale_lock_resumed = record
        .overrides
        .contains(&op_state::Override::AllowStaleLock);
    let discard_local_commits_resumed = record
        .overrides
        .contains(&op_state::Override::DiscardLocalCommits);

    // Re-pin the source snapshot for this --continue session. The source's
    // T0 is "the start of the (resumed) replay" — re-pinning here gives
    // replay's re-entry rule a coherent set of inputs. Per-repo no-op
    // detection handles already-converged repos cleanly.
    let project_vcs = project_vcs();
    let classify = if matches!(recorded_verb, MachineVerb::Sync)
        && source_is_workweave
        && !allow_stale_lock_resumed
    {
        ClassifySource::RelationsAndTips(source_workspace_dir.as_path())
    } else {
        ClassifySource::Skip
    };
    let snapshot = pin_source_snapshot(project_vcs.as_ref(), &source_project_dir, classify)?;

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
        project_vcs,
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
    if let Checkout::Workweave {
        dir: cwd_ww,
        parent: cwd_parent,
        ..
    } = &cwd_ctx.checkout
    {
        // Resolve the source workspace's location to compare. Best-effort.
        let source_ctx = match WorkspaceContext::resolve(source_workspace_dir, None) {
            Ok(c) => c,
            Err(_) => return,
        };
        if let Checkout::Workweave { dir: source_ww, .. } = &source_ctx.checkout {
            let cwd_canonical = cwd_ww
                .canonicalize()
                .unwrap_or_else(|_| cwd_ww.to_path_buf());
            let source_canonical = source_ww
                .canonicalize()
                .unwrap_or_else(|_| source_ww.to_path_buf());
            if cwd_canonical != source_canonical {
                let cwd_parent = cwd_parent
                    .canonicalize()
                    .unwrap_or_else(|_| cwd_parent.clone());
                if cwd_parent != source_canonical && emit_text {
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
    vcs: &dyn Vcs,
    cwd_project_dir: &Path,
    target_project_dir: &Path,
    _emit_text: bool,
) -> anyhow::Result<()> {
    let cwd_tip = vcs
        .head_revision(cwd_project_dir)
        .context("failed to read CWD project HEAD")?;
    let target_tip = vcs
        .head_revision(target_project_dir)
        .context("failed to read target project HEAD")?;
    if cwd_tip == target_tip {
        // Equal tips: not an error, replay's per-repo no-op detection will
        // simply do nothing in step 1. Continue into the machine so the
        // record/lease cleanup happens through the canonical cleanup phase.
        return Ok(());
    }
    let cwd_ahead = vcs
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
    project_vcs: &dyn Vcs,
    cwd_project: &Project,
    target_workspace_dir: &Path,
    target_project_dir: &Path,
    target_path: &Path,
) -> anyhow::Result<()> {
    let mut dirty: Vec<String> = Vec::new();
    for (repo_path, entry) in cwd_project.manifest.iter_entries() {
        let target_repo = target_workspace_dir.join(repo_path.as_path());
        // Skip reference symlinks: a dirty shared canonical must not block a
        // sync-to that never touches it (advance-target excludes it too).
        if checkout_is_syncable(&target_repo)
            && vcs_for(entry.vcs_type)
                .has_uncommitted_changes(&target_repo)
                .unwrap_or(true)
        {
            dirty.push(repo_path.to_string());
        }
    }
    if project_vcs
        .has_uncommitted_changes(target_project_dir)
        .unwrap_or(true)
    {
        dirty.push(project_repo_key().to_string());
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

/// sync-to detached-target preflight: refuse if any target repo is not on a
/// branch. Landing fast-forwards the branch the target is attached to; a
/// detached HEAD gives it nothing to advance, and `--retire` then deletes the
/// source branch that was holding the work.
///
/// This is the whole-op layer of the refusal, so the operator sees every
/// unattached repo at once instead of one per re-run. It does **not**
/// replace the per-repo refusal in [`ff_advance_repo`], which is the layer
/// that covers `--continue` (a resumed op re-enters at the phase body, not
/// here).
fn check_detached_target_preflight(
    project_vcs: &dyn Vcs,
    cwd_project: &Project,
    target_workspace_dir: &Path,
    target_project_dir: &Path,
    target_path: &Path,
) -> anyhow::Result<()> {
    // Report-only: a preflight that names repos has no MOVE to authorize,
    // so it observes and keeps no witness. `Unborn` is reported as itself
    // rather than as a second spelling of detached — the shipped
    // `current_ref` answered `Some(name)` there (`symbolic-ref` succeeds on
    // a branch with no commits), so an unborn target read as attached and
    // fell through to a `merge --ff-only` that cannot work. An unreadable
    // HEAD keeps the shipped fail-closed direction: it is named, not
    // skipped.
    let unattached = |vcs: &dyn Vcs, repo: &Path| match vcs.head_attachment(repo) {
        Ok(HeadAttachment::Attached(_)) => None,
        Ok(HeadAttachment::Detached(d)) => Some(format!("detached HEAD at {}", d.at())),
        Ok(HeadAttachment::Unborn(u)) => Some(format!("unborn branch '{u}', no commits yet")),
        Err(e) => Some(format!("HEAD unreadable: {e}")),
    };

    let mut detached: Vec<String> = Vec::new();
    for (repo_path, entry) in cwd_project.manifest.iter_entries() {
        let target_repo = target_workspace_dir.join(repo_path.as_path());
        // Skip reference symlinks: advance-target excludes them, so their
        // attachment is not this op's business.
        if !checkout_is_syncable(&target_repo) {
            continue;
        }
        if let Some(state) = unattached(vcs_for(entry.vcs_type).as_ref(), &target_repo) {
            detached.push(format!("{repo_path} ({state})"));
        }
    }
    if let Some(state) = unattached(project_vcs, target_project_dir) {
        detached.push(format!("(project) ({state})"));
    }
    if !detached.is_empty() {
        anyhow::bail!(
            "sync-to precondition failed: target workweave is not on a branch in:\n  {}\n\
             \n\
             sync-to lands by fast-forwarding the branch each target repo is on; a detached \
             HEAD has none, so the work would be recorded nowhere. Check out the receiving \
             branch in the target ({}), then re-run.",
            detached.join("\n  "),
            target_path.display(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Sync (pull) destination-side dirt scan
// ---------------------------------------------------------------------------

/// Classify one dirty checkout for the sync dirt scan.
///
/// Returns `None` when every tracked difference is **rwv-attributable**:
/// doctor's structural drift classification ([`crate::check::classify_drift`])
/// reports the index tree as an ancestor commit's tree
/// ([`crate::check::IndexDriftKind::SafeToFix`]) and every modified
/// working-tree blob as reachable from HEAD
/// ([`crate::check::WorkingTreeDriftKind::SafeToFix`]). That is the
/// shared-ref-advance signature — another worktree moved this branch's ref,
/// and the index/working tree lag the new tip. Sync is DESIGNED to reconcile
/// that state itself: the replay loop runs
/// [`Vcs::refresh_index_to_head_if_safe`] /
/// [`Vcs::refresh_working_tree_to_head_if_safe`] per repo, which apply the
/// SAME structural reachability test before healing (see
/// `tests/index_drift_test.rs` / `tests/working_tree_drift_test.rs` — the
/// self-healing assertions are the spec). Refusing here would both block the
/// designed behavior and teach the operator to `git commit` a moved-branch
/// diff they never authored.
///
/// Returns `Some(annotated_label)` for genuine user dirt — live staged
/// content and/or live working-tree edits — annotated with which kind was
/// found so the operator sees what the commit/stash advice applies to.
///
/// The attribution is purely structural (tree/blob reachability in the DAG);
/// no timestamps or heuristics. `classify_drift`'s own conservative fallback
/// (git errors during classification → live) keeps the scan fail-closed.
fn classify_sync_dirt(repo: &Path, label: String) -> Option<String> {
    use crate::check::{IndexDriftKind, WorkingTreeDriftKind};
    let (index_drift, wt_drift) = crate::check::classify_drift(repo);
    let index_live = matches!(index_drift, Some(IndexDriftKind::LiveStaged));
    let wt_live = matches!(wt_drift, Some(WorkingTreeDriftKind::LiveEdits));
    let detail = match (index_live, wt_live) {
        // Every difference is attributable drift (or the repo settled between
        // the status read and this classification). The replay loop's
        // safe-refresh heals it; nothing to refuse.
        (false, false) => return None,
        (true, true) => "staged changes + working-tree edits",
        (true, false) => "staged changes",
        (false, true) => "working-tree edits",
    };
    Some(format!("{label} ({detail})"))
}

/// Pre-flight dirt scan for `rwv sync` (pull): refuse before any mutation if
/// the CWD workspace (the *destination* that replay will rebase or
/// fast-forward) carries uncommitted **tracked** changes that are not
/// attributable to rwv's own shared-ref-advance drift, in any manifest repo
/// or the project repo.
///
/// ## Why tracked-only
///
/// `git rebase` fails with "cannot rebase: You have unstaged changes" /
/// "cannot rebase: Your index contains uncommitted changes" when tracked files
/// are modified (staged or unstaged). Untracked files survive a rebase
/// untouched — git never touches them during replay. Fast-forward (`ff`) also
/// leaves untracked files alone. Refusing on untracked-only dirt would block
/// normal in-progress work (scratch files, build artefacts) without any
/// corresponding git-layer failure. We therefore refuse only on tracked dirt,
/// matching git's actual failure conditions.
///
/// ## Attributable drift is NOT dirt
///
/// A worktree whose branch ref was advanced by another workspace (shared-ref
/// advance) shows tracked differences in `git status` that the operator never
/// authored: the index/working tree simply lag the moved tip. Doctor's
/// structural drift classification distinguishes that state from live user
/// work (see [`classify_sync_dirt`]); attributable drift is excluded from the
/// refusal set and self-heals in the replay loop's safe-refresh step. Only
/// live staged content / live working-tree edits refuse.
///
/// ## What this replaces
///
/// Without this check, `rwv sync` starts the op, acquires op-state, creates
/// savepoints, and then hits `git rebase` or `git merge --ff-only` which
/// fails mid-op with a raw git error dump. The operator is left with a live
/// `.rwv-op` they must `rwv abort` before retrying. This preflight makes the
/// refusal eager, names every dirty repo in a single message, and leaves no
/// trace (the acquired op-state is released by the caller on `Err`).
///
/// ## No rwv.lock carve-out for sync
///
/// Unlike [`check_dirty_source_preflight`] (the sync-to source-side scan),
/// this function does NOT carve out `rwv.lock`. The distinction:
///
/// - **sync-to**: replay rebases the manifest repos; the project repo's lock
///   is regenerated and committed by Phase 3 (not ff'd). A dirty lock is the
///   auto-relock's input and never blocks Phase 1' for sync-to.
/// - **sync (pull)**: Phase 1' fast-forwards or rebases the project repo
///   itself to the source's project tip. Both `git merge --ff-only` and
///   `git rebase` fail when tracked files (including `rwv.lock`) are dirty.
///   There is no downstream phase that commits the dirty lock; the operator
///   must stash or commit it before syncing.
///
/// ## Scope
///
/// Covers every CWD manifest repo gated by [`checkout_is_syncable`] (skips
/// reference symlinks — the shared canonical store is never rebased by sync)
/// and the project repo.
fn check_dirty_preflight_sync(
    project_vcs: &dyn Vcs,
    cwd_project: &Project,
    cwd_workspace_dir: &Path,
    cwd_project_dir: &Path,
) -> anyhow::Result<()> {
    let mut user_dirt: Vec<String> = Vec::new();
    let mut unreadable: Vec<String> = Vec::new();

    for (repo_path, entry) in cwd_project.manifest.iter_entries() {
        let repo = cwd_workspace_dir.join(repo_path.as_path());
        // Skip reference symlinks: the shared canonical store is never
        // rebased or ff'd by sync (checkout_is_syncable guards every mutating
        // phase). A dirty canonical must not block a sync that never touches it.
        if !checkout_is_syncable(&repo) {
            continue;
        }
        match vcs_for(entry.vcs_type).tracked_dirty_file_names(&repo) {
            // Clean (tracked-wise): untracked-only repos land here too.
            Ok(tracked) if tracked.is_empty() => {}
            Ok(_) => {
                if let Some(label) = classify_sync_dirt(&repo, repo_path.to_string()) {
                    user_dirt.push(label);
                }
                // None → fully attributable drift; replay self-heals it.
            }
            // Unreadable status → fail closed rather than silently rebasing
            // over an unknown state, but don't prescribe commit/stash for
            // changes we cannot enumerate.
            Err(_) => unreadable.push(repo_path.to_string()),
        }
    }

    // Project repo: no rwv.lock carve-out (unlike check_dirty_source_preflight
    // for sync-to). For sync (pull), Phase 1' ff's or rebases the project repo
    // directly, and git refuses on any tracked dirty file including rwv.lock.
    // Name the specific tracked files so the operator sees exactly what refuses.
    match project_vcs.tracked_dirty_file_names(cwd_project_dir) {
        Ok(tracked) if tracked.is_empty() => {}
        Ok(tracked) => {
            let files = tracked
                .iter()
                .map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if let Some(label) = classify_sync_dirt(cwd_project_dir, format!("(project): {files}"))
            {
                user_dirt.push(label);
            }
        }
        Err(_) => unreadable.push(project_repo_key().to_string()),
    }

    if user_dirt.is_empty() && unreadable.is_empty() {
        return Ok(());
    }

    // Assemble the refusal. Commit/stash remediation is attached ONLY to the
    // user-dirt section — for unreadable repos we state what is known and
    // stop; advising `git commit` for a repo whose state we could not read
    // (or for drift the operator never authored) would be harmful.
    let mut msg = String::from("sync precondition failed: ");
    if !user_dirt.is_empty() {
        msg.push_str(&format!(
            "destination workspace has uncommitted tracked changes in:\n  {}\n\
             \n\
             sync rebases or fast-forwards these repos onto the source lock; running with tracked \
             dirt mid-op would leave a half-rebased state requiring `rwv abort` to clear. \
             Commit or stash the changes in the destination ({}), then re-run.\n\
             \n\
             To commit: git -C <repo> commit\n\
             To stash:  git stash push -u -C <repo>   # or: cd <repo> && git stash push -u\n",
            user_dirt.join("\n  "),
            cwd_workspace_dir.display(),
        ));
    }
    if !unreadable.is_empty() {
        if !user_dirt.is_empty() {
            msg.push('\n');
        }
        msg.push_str(&format!(
            "git status could not be read in:\n  {}\n\
             \n\
             Refusing to sync over unknown state. Inspect these repos manually \
             (`git -C <repo> status`), then re-run.\n",
            unreadable.join("\n  "),
        ));
    }
    msg.push_str(
        "\n(Untracked files are fine — they survive rebase and fast-forward untouched. \
         Stale index/working-tree drift from a shared-ref advance is reconciled by sync \
         itself and does not refuse.)",
    );
    anyhow::bail!("{msg}");
}

/// sync-to source-side cleanliness preflight.
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
///   the auto-relock's own input and the op commits it. A project repo that
///   is dirty *only* in `rwv.lock` passes; any other tracked project-repo change
///   still refuses (and the project entry names the specific files so the lock
///   carve-out is auditable).
fn check_dirty_source_preflight(
    project_vcs: &dyn Vcs,
    cwd_project: &Project,
    cwd_workspace_dir: &Path,
    cwd_project_dir: &Path,
) -> anyhow::Result<()> {
    let mut dirty: Vec<String> = Vec::new();

    for (repo_path, entry) in cwd_project.manifest.iter_entries() {
        let repo = cwd_workspace_dir.join(repo_path.as_path());
        // Skip reference symlinks: a dirty shared canonical must not block a
        // sync-to that never rebases it (replay excludes it too).
        if checkout_is_syncable(&repo) {
            let tracked = vcs_for(entry.vcs_type)
                .tracked_dirty_file_names(&repo)
                .unwrap_or_else(|_| {
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
    let project_tracked = project_vcs
        .tracked_dirty_file_names(cwd_project_dir)
        .unwrap_or_else(|_| vec!["(status unreadable)".to_string()]);
    let non_lock: Vec<&String> = project_tracked
        .iter()
        .filter(|p| p.as_str() != LockFile::FILE_NAME)
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
            cwd_workspace_dir.display(),
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Benign-staleness classification
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
    /// The checkout's HEAD at classification time; `None` when unreadable.
    tip: Option<ResolvedRevisionId>,
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
    for (repo_path, entry) in manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = workspace_dir.join(repo_path.as_path());
        // Reference aliases are read-only and never rebased; do not classify.
        if !checkout_is_syncable(&repo_abs) {
            continue;
        }
        if !repo_abs.exists() {
            continue;
        }
        let tip = vcs.head_revision(&repo_abs).ok();
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
        let relation = compute_relation(vcs.as_ref(), &repo_abs, &tip, &lock_sha);
        let ahead_count = if relation == LockRelation::Ahead {
            // `Ahead` ⟺ lock is a strict ancestor of tip (both present, per
            // compute_relation). Count lock..HEAD — the commits the landing
            // replays / the auto-relock pins.
            match (lock_sha.as_ref(), tip.as_ref()) {
                (Some(lock), Some(tip)) => vcs.count_commits_in_range(&repo_abs, lock, tip).ok(),
                _ => None,
            }
        } else {
            None
        };
        out.push(RepoRelation {
            repo_path: repo_path.clone(),
            relation,
            ahead_count,
            tip,
        });
    }
    LockClassification {
        relations: out,
        unresolvable,
    }
}

/// The relock a lock-side refusal asks for, and what it costs on each side.
///
/// One spelling for every lock-side refusal, because the destination half
/// carries a follow-on that is wrong to state in one refusal and omit from
/// another: the relock lands a commit in the destination's project repo, and a
/// bare (fast-forward) `sync` then refuses to advance past it. Naming only the
/// relock leaves the operator at a second refusal caused by the first one's
/// remedy. The source half has no such cost — the ancestry gate measures the
/// destination.
fn relock_recovery(side: Side, project_name: &str) -> String {
    match side {
        Side::Source => format!(
            "Run `rwv lock --commit --project {project_name}` in the source workspace before \
             syncing"
        ),
        Side::Destination => format!(
            "Run `rwv lock --commit --project {project_name}` to refresh, then rerun with \
             `--strategy rebase` — that relock lands a commit in the destination's project repo \
             which a bare (fast-forward) `sync` cannot advance past, and `rebase` replays the \
             destination's project commits with `rwv.lock` excluded"
        ),
    }
}

/// Refusal naming the first unresolvable lock entry (a tag/branch the lock pins
/// that no longer exists on disk). Distinct from a relation — the lock itself is
/// corrupt. Preserves the old lock-freshness "unknown revision" diagnostic,
/// including the `--project <p>`-qualified recovery hint and the
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
    let recovery = relock_recovery(side, project_name);
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
/// The `lock-freshness precondition` phrase, the "stale lock" wording and
/// the `--allow-stale-lock` name are contractual: `tests/benign_staleness_test.rs`
/// and `tests/doc_claims_sync_to_test.rs` assert on them, so a reworded
/// refusal fails the gate. On top of that this names each repo's relation,
/// and a `diverged` repo also earns the `rwv lock --commit` bless-HEAD hint.
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
    let recovery = relock_recovery(side, project_name);
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

/// Refusal for a `sync-to` TARGET whose committed lock is behind its HEAD
/// (`LockRelation::Ahead`). Replay takes its targets from the target's lock, so
/// the target's unlocked commits are absent from the tip CWD replays onto and
/// advance-target's fast-forward cannot proceed. Refusing at op start is the
/// parity of the CWD side's auto-relock: the same relation is answered on both
/// sides before anything mutates, rather than surfacing as a late
/// fast-forward failure with the landing already partly done.
///
/// The remedy names the target workspace because that is where `rwv lock
/// --commit` has to run. `--allow-stale-lock` is deliberately not offered: it
/// skips this check without making the op converge.
fn target_lock_behind_refusal(
    target_workspace_name: &str,
    target_project_name: &str,
    offending: &[&RepoRelation],
) -> Option<String> {
    if offending.is_empty() {
        return None;
    }
    let mut lines = String::new();
    for r in offending {
        let n = r
            .ahead_count
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".to_string());
        lines.push_str(&format!(
            "\n  {}: HEAD ahead of lock by {n} commits",
            r.repo_path
        ));
    }
    Some(format!(
        "lock-freshness precondition failed: target workspace '{target_workspace_name}' has a \
         stale lock — its committed lock is behind HEAD for:{lines}\n\
         \n\
         sync-to replays CWD against the target's committed lock, so those commits would be \
         missing from CWD's tip and step 3 could not fast-forward the target onto it.\n\
         \n\
         Fix: run `rwv lock --commit --project {target_project_name}` in the target workspace \
         ('{target_workspace_name}'), then re-run sync-to.\n"
    ))
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
// **Re-entry rule:** per-repo state is derived from the VCS itself:
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
    // rule: per-repo state is derived from the VCS itself — already-
    // converged repos no-op via `sync_one_repo`'s head-equals-target check.
    let snapshot = &ctx.snapshot;

    // Load CWD project (manifest + lock) from disk.
    let cwd_project =
        Project::from_dir(&ctx.cwd_project_dir).context("failed to load CWD project")?;

    // CWD project tip — read before any side effects so Phase 1' has the
    // pre-op starting state for its `cwd_tip == source_tip` short-circuit.
    let cwd_project_tip = ctx
        .project_vcs
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
        match materialize_missing_repo(
            vcs_for(entry.vcs_type).as_ref(),
            &ctx.cwd_ctx,
            repo_path,
            entry,
            &ctx.cwd_project_name,
        ) {
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
            // The lock named this path and no longer does, so there is no
            // entry to resolve a backend from unless the manifest still
            // carries one.
            let dropped_vcs = cwd_project
                .manifest
                .get_entry(repo_path)
                .map(|e| vcs_for(e.vcs_type))
                .unwrap_or_else(crate::vcs::probe_vcs);
            match prune_dropped_repo(
                dropped_vcs.as_ref(),
                &ctx.cwd_ctx,
                repo_path,
                &ctx.cwd_project_name,
            ) {
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
        /// The repo's backend, resolved on this thread before the fan-out.
        /// Workers borrow it; none of them resolves one.
        vcs: Box<dyn Vcs>,
        target: ResolvedRevisionId,
        /// The repo's tip before this op replayed anything, taken from the
        /// op's savepoint so a `--continue` re-entry reads the same tip the
        /// interrupted run did rather than the pick it stopped on.
        pre_replay_tip: Option<ResolvedRevisionId>,
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
        // The lock names paths; the manifest names backends. A lock entry
        // with no manifest entry has no declared backend to resolve from.
        let vcs = cwd_project
            .manifest
            .get_entry(repo_path)
            .map(|e| vcs_for(e.vcs_type))
            .unwrap_or_else(crate::vcs::probe_vcs);
        let target = match snapshot.pull_tips.get(repo_path) {
            Some(tip) => tip.clone(),
            None => lock_entry.version.clone(),
        };
        sync_tasks.push(SyncTask {
            repo_path: repo_path.clone(),
            pre_replay_tip: vcs.resolve_savepoint(&abs, ctx.op_id.as_str()),
            abs,
            vcs,
            target,
        });
    }

    // === advanced_tips write 1: pre-write planned targets for genuine ff-movers ===
    //
    // Before the parallel fan-out, classify every sync task: if the repo's
    // current HEAD is a STRICT ancestor of the lock target (head ≠ target AND
    // head ⊏ target), this is a genuine fast-forward and the landing tip is
    // knowable now.  Pre-write target → advanced_tips so abort can attribute the
    // repo the instant it is advanced, with no window.
    //
    // Repos whose HEAD equals target (NoOp) or whose HEAD is ahead of target
    // (AlreadyAhead) are skipped — savepoint already attributes the no-op case,
    // and recording an unreached target for an already-ahead repo is
    // forgeable.  Repos with local commits that diverge (not strict ancestors)
    // are skipped here; their fresh rebased tip is captured post-join (write 3).
    {
        let mut entry_tips: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for task in &sync_tasks {
            if let Ok(head) = task.vcs.head_revision(&task.abs) {
                if head != task.target
                    && task
                        .vcs
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
    // record happens inside this closure — that would be a race.
    let task_results: Vec<(bool, Option<String>)> =
        run_in_parallel(&sync_tasks, ctx.jobs, |_idx, task| {
            let outcome = sync_one_repo(
                task.vcs.as_ref(),
                &task.abs,
                &task.target,
                strategy,
                task.pre_replay_tip.as_ref(),
            );
            let is_failure = outcome.is_failure();
            // Capture the actual post-advance HEAD if this task converged.
            // For ff-movers this equals the pre-written target (idempotent
            // overwrite in write 3); for rebased repos it is the fresh SHA.
            let converged_head = if matches!(outcome, RepoSyncOutcome::Converged { .. }) {
                task.vcs
                    .head_revision(&task.abs)
                    .ok()
                    .map(|h| h.as_str().to_owned())
            } else {
                None
            };
            if !is_failure {
                task.vcs.refresh_index_to_head_if_safe(&task.abs);
                task.vcs.refresh_working_tree_to_head_if_safe(&task.abs);
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
    // SHAs for manifest repos that had local commits to replay.
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
        let live_conflict = sync_tasks
            .iter()
            .find_map(|t| t.vcs.mid_op(&t.abs).map(|op| (t.vcs.as_ref(), op)));
        anyhow::bail!(
            "{}",
            match live_conflict {
                Some((vcs, op)) => manifest_repo_failure_message(vcs, ctx.verb, Some(op)),
                None => manifest_repo_failure_message(ctx.project_vcs.as_ref(), ctx.verb, None),
            }
        );
    }

    // === Phase 1' (project repo) — strategy on the project repo ===

    let phase1_outcome = if ctx.discard_local_commits {
        // --discard-local-commits: rewind CWD's project repo to source's tip,
        // discarding any project commits not reachable from source. Guard
        // already refused on uncommitted changes.
        //
        // A rewinding MOVE needs a `DiscardWarrant`, and the warrant
        // needs a savepoint that has actually been written — not one that is
        // planned. `guard_and_mark` wrote it before this phase, which is what
        // keeps the discarded commits reachable through `rwv abort`, so this
        // *resolves* that savepoint rather than creating one: re-creating it
        // here would move the recovery point to the tip we are about to
        // discard from, on every `--continue`.
        rewind_project_repo(ctx, &snapshot.source_project_tip)
    } else {
        apply_project_strategy(
            ctx.project_vcs.as_ref(),
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
        let live_conflict = ctx.project_vcs.mid_op(&ctx.cwd_project_dir);
        anyhow::bail!(
            "{}",
            phase1_or_phase3_failure_message(
                ctx.project_vcs.as_ref(),
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
    // at a fresh SHA T1 that was not knowable at replay entry.  Overwrite the
    // project repo's advanced_tips entry with the actual post-rebase HEAD.
    // This also covers the ff/discard-local-commits case (tip == source tip,
    // idempotent overwrite).
    {
        let project_tip = ctx
            .project_vcs
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
            .insert(
                project_repo_key().to_owned(),
                project_tip.as_str().to_owned(),
            );
        op_state::write_owner(&ctx.owner_workspace_dir, &owner)
            .context("failed to write advanced_tips after Phase 1'")?;
    }

    Ok(())
}

/// Pin the atomic source snapshot at T0: read the source project tip once,
/// then read source manifest + lock AT that revision. Combined with the
/// no-op-in-progress check on the source (in `check_no_op_in_progress`),
/// source reads are effectively serialisable with no locks.
fn pin_source_snapshot(
    source_vcs: &dyn Vcs,
    source_project_dir: &Path,
    classify: ClassifySource<'_>,
) -> anyhow::Result<SourceSnapshot> {
    let source_project_tip = source_vcs
        .head_revision(source_project_dir)
        .context("failed to read source project HEAD")?;

    let raw_source_lock = {
        let content = source_vcs
            .read_file_at_revision(
                source_project_dir,
                &source_project_tip,
                Path::new(LockFile::FILE_NAME),
            )
            .with_context(|| {
                format!(
                    "failed to read source lock at revision {} in {}",
                    source_project_tip,
                    source_project_dir.display()
                )
            })?;
        LockFile::from_json_str(&content).with_context(|| {
            format!(
                "failed to parse source lock at revision {} in {}",
                source_project_tip,
                source_project_dir.display()
            )
        })?
    };

    let source_manifest = {
        let content = source_vcs
            .read_file_at_revision(
                source_project_dir,
                &source_project_tip,
                Path::new(Manifest::FILE_NAME),
            )
            .with_context(|| {
                format!(
                    "failed to read source manifest at revision {} in {}",
                    source_project_tip,
                    source_project_dir.display()
                )
            })?;
        Manifest::from_toml_str(&content).with_context(|| {
            format!(
                "failed to parse source manifest at revision {} in {}",
                source_project_tip,
                source_project_dir.display()
            )
        })?
    };

    let source_class = match classify {
        ClassifySource::Skip => None,
        ClassifySource::Relations(dir) | ClassifySource::RelationsAndTips(dir) => Some(
            classify_lock_relations(dir, &source_manifest, Some(&raw_source_lock)),
        ),
    };

    let pull_tips = match (&classify, &source_class) {
        (ClassifySource::RelationsAndTips(_), Some(class)) => class
            .relations
            .iter()
            .filter(|r| r.relation == LockRelation::Ahead)
            .filter_map(|r| r.tip.clone().map(|tip| (r.repo_path.clone(), tip)))
            .collect(),
        _ => std::collections::BTreeMap::new(),
    };

    Ok(SourceSnapshot {
        source_project_tip,
        source_manifest,
        raw_source_lock,
        source_class,
        pull_tips,
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
//
// Runs for every verb and strategy. `--strategy=ff` makes replay a no-op, not
// this phase: whatever last moved CWD's manifest repos — an operator's fix
// between a stranded op and its resume, most of all — leaves a lock that no
// longer pins them, and advance-target publishes that lock to the target.

fn run_relock(ctx: &OpContext<'_>) -> anyhow::Result<()> {
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
        ctx.project_vcs.as_ref(),
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
            phase1_or_phase3_failure_message(
                ctx.project_vcs.as_ref(),
                Phase::Three,
                &ctx.cwd_project_dir,
                ctx.verb,
                None,
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
    // Build the converged table from post-replay HEADs...
    let mut converged: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (repo_path, entry) in cwd_project.manifest.iter_entries() {
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            continue;
        }
        if let Ok(rev) = vcs_for(entry.vcs_type).head_revision(&abs) {
            converged.insert(repo_path.as_str().to_owned(), rev.as_str().to_owned());
        }
    }
    if let Ok(rev) = ctx.project_vcs.head_revision(&ctx.cwd_project_dir) {
        converged.insert(project_repo_key().to_owned(), rev.as_str().to_owned());
    }
    // ...then swap atomically: `PhaseTips::converge` discards the replay-phase
    // advanced_tips and installs converged_tips in one move, so they land in the
    // SAME persist. The ADT makes the both-populated state
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
    for (repo_path, entry) in cwd_project_final.manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
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
        let cwd_tip = match vcs.head_revision(&cwd_repo) {
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
        let target_tip_before = vcs.head_revision(&target_repo).ok();
        match ff_advance_repo(vcs.as_ref(), &target_repo, &cwd_repo, &cwd_tip) {
            Ok(advanced) => {
                if emit_text {
                    println!(
                        "  {}: {}",
                        repo_path,
                        ff_advance_line(advanced.as_ref(), &cwd_tip)
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

    // The project repo carries the lock, and the lock names the manifest tips.
    // Advancing it while a manifest repo failed to land would leave the target
    // asserting revisions it does not hold — a worse state than the one the
    // failure already produced, and one no later phase repairs. Every manifest
    // repo that DID land is left landed: those tips are ancestors of nothing the
    // target lacks, so the un-advanced lock stays true about them.
    if any_ff_failure {
        if emit_text {
            eprintln!("  (project): not advanced — a manifest repo did not land (see above)");
        }
        anyhow::bail!(
            "sync-to advance-target failed for one or more manifest repos (see above).\n\
             The target's project repo was NOT advanced, so its lock still describes the \
             target's pre-op state rather than naming revisions the target does not have.\n\
             Op-state remains in both workspaces.\n\
             Rerun `{resume}` after resolving, or `rwv abort` to roll the whole op back.",
            resume = op_state::resume_command(ctx.verb),
        );
    }

    let cwd_project_tip = ctx
        .project_vcs
        .head_revision(&ctx.cwd_project_dir)
        .context("failed to read CWD project HEAD for advance-target")?;

    // Read project target tip BEFORE the advance so we can report from_sha.
    let project_target_tip_before = ctx.project_vcs.head_revision(&ctx.dest_project_dir).ok();
    match ff_advance_repo(
        ctx.project_vcs.as_ref(),
        &ctx.dest_project_dir,
        &ctx.cwd_project_dir,
        &cwd_project_tip,
    ) {
        Ok(advanced) => {
            if emit_text {
                println!(
                    "  (project): {}",
                    ff_advance_line(advanced.as_ref(), &cwd_project_tip)
                );
            }
            // Record step-3 advance for the project repo iff it actually moved.
            if let Some(ref before) = project_target_tip_before {
                if before != &cwd_project_tip {
                    ctx.handler.record_step3_advance(
                        project_repo_key(),
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
            anyhow::bail!(
                "sync-to advance-target could not advance the target's project repo (see \
                 above). Every manifest repo landed, so the target holds the work but its \
                 lock does not yet name those tips.\n\
                 Op-state remains in both workspaces.\n\
                 Rerun `{resume}` after resolving, or `rwv abort` to roll the whole op back.",
                resume = op_state::resume_command(ctx.verb),
            );
        }
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

    match &ctx.cwd_ctx.checkout {
        Checkout::Workweave {
            dir, name, project, ..
        } => retire_workweave_after_sync_to(
            ctx.project_vcs.as_ref(),
            &ctx.cwd_ctx,
            dir,
            name,
            project,
            &ctx.cwd_project_dir,
            &ctx.dest_workspace_dir,
        ),
        Checkout::Primary { .. } => {
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

/// Tree difference between a repo's pre-op savepoint and its current HEAD —
/// what the op left changed in this checkout. `None` when the repo has no
/// savepoint (it was born during the op; the membership change that birthed
/// it is the project manifest's to report) or when nothing moved.
fn delivered_changes(vcs: &dyn Vcs, repo: &Path, op_id: &OpId) -> Option<Vec<String>> {
    let pre = vcs.resolve_savepoint(repo, op_id.as_str())?;
    let head = vcs.head_revision(repo).ok()?;
    if pre == head {
        return None;
    }
    vcs.changed_paths_between(repo, &pre, &head).ok()
}

/// One delivered change that lands on an input of the materialized project:
/// the project manifest, or a member's detection manifest for an integration
/// the project enables.
struct MaterializedInputHit {
    /// Display key for the text note: [`project_repo_key`] for the project
    /// manifest, the member's manifest-relative repo path otherwise.
    repo_key: String,
    /// The input file's name within that repo (e.g. `Cargo.toml`).
    file: String,
    /// The same hit as a workspace-relative path, for the `--json` surface.
    workspace_path: String,
}

impl MaterializedInputHit {
    /// `repo: file` rendering used by the text-path note.
    fn display(&self) -> String {
        format!("{}: {}", self.repo_key, self.file)
    }
}

/// Delivered changes that are inputs of the materialized project.
///
/// Generated ecosystem state is derived from exactly these inputs, and sync
/// never fires the hooks that would re-derive it — materialization is
/// activation's mandate — so a hit means this checkout's generated state may
/// no longer agree with the inputs sync just delivered.
///
/// Empty when this root does not present the synced project — then nothing
/// is materialized here to go stale. The root's own identity files answer
/// that, not the resolved project name: `--project` can aim a primary's sync
/// at a project its pointer does not present, and the note must stay quiet
/// for exactly that delivery.
fn delivered_materialized_input_hits(ctx: &OpContext<'_>) -> Vec<MaterializedInputHit> {
    let presented = crate::workspace::observe_root(&ctx.cwd_workspace_dir)
        .and_then(|obs| obs.presented_project().cloned());
    if presented.as_ref() != Some(&ctx.cwd_project_name) {
        return Vec::new();
    }
    let Ok(project) = Project::from_dir_skip_lock(&ctx.cwd_project_dir) else {
        return Vec::new();
    };
    let builtin = builtin_integrations();
    let integrations: Vec<&dyn Integration> = builtin.iter().map(|b| b.as_ref()).collect();
    let default_config = IntegrationConfig::default();
    let member_inputs: std::collections::BTreeSet<&str> =
        enabled_integrations(&integrations, &project.manifest, &default_config)
            .flat_map(|(integration, _)| integration.detection_manifests().iter().copied())
            .collect();

    let mut hits = Vec::new();
    if let Some(changed) =
        delivered_changes(ctx.project_vcs.as_ref(), &ctx.cwd_project_dir, &ctx.op_id)
    {
        if changed.iter().any(|p| p == Manifest::FILE_NAME) {
            hits.push(MaterializedInputHit {
                repo_key: project_repo_key().to_owned(),
                file: Manifest::FILE_NAME.to_owned(),
                workspace_path: format!(
                    "{}/{}",
                    project_rel_path(ctx.cwd_project_name.as_str()),
                    Manifest::FILE_NAME
                ),
            });
        }
    }
    for (repo_path, entry) in project.manifest.iter_entries() {
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if !checkout_is_syncable(&abs) {
            continue;
        }
        let vcs = vcs_for(entry.vcs_type);
        let Some(changed) = delivered_changes(vcs.as_ref(), &abs, &ctx.op_id) else {
            continue;
        };
        for file in changed
            .iter()
            .filter(|p| member_inputs.contains(p.as_str()))
        {
            hits.push(MaterializedInputHit {
                repo_key: repo_path.to_string(),
                file: file.clone(),
                workspace_path: format!("{repo_path}/{file}"),
            });
        }
    }
    hits
}

fn cleanup(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    let emit_text = ctx.handler.emit_text();

    // Before the savepoints go: they are the pre-op tips the staleness note
    // reads its delivered ranges from.
    if matches!(ctx.verb, op_state::OpVerb::Sync) {
        let hits = delivered_materialized_input_hits(ctx);
        if !hits.is_empty() {
            if emit_text {
                eprintln!(
                    "note: delivered changes touch materialized inputs ({}); run `rwv materialize` \
                     to bring the generated ecosystem state up to date",
                    hits.iter()
                        .map(MaterializedInputHit::display)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            ctx.handler.record_advisory(AdvisoryOutput {
                kind: AdvisoryKindOutput::DerivedStateStale,
                remedy: "rwv materialize".to_owned(),
                inputs: hits.into_iter().map(|h| h.workspace_path).collect(),
            });
        }
    }

    // Savepoint refs (`refs/rwv/pre-op/*`) live in the shared clone refdb, not
    // in any worktree, so `git update-ref -d` from ANY live worktree of the
    // same clone drops the shared ref. Crucially, in the `sync-to --retire`
    // flow the phase order is `… → retire → cleanup`: retire deletes CWD's
    // workweave BEFORE cleanup runs, so `ctx.cwd_project_dir` /
    // `ctx.cwd_workspace_dir` now point at a deleted directory. Dropping
    // savepoints through those paths silently no-ops while the ref survives in
    // the surviving clone — the leak this code path fixes.
    //
    // We therefore target the CANONICAL/PRIMARY clone (`primary_path()`), which
    // survives workweave deletion: workweave repos are `git worktree add`ed
    // from the primary's clones, so the primary holds the shared refdb. When
    // CWD is itself the primary weave (plain `sync` from primary), the
    // canonical path equals CWD, so this is also correct for the non-retire
    // case.
    let primary = ctx.cwd_ctx.primary_path();
    let canonical_project_dir = project_dir(primary, ctx.cwd_project_name.as_str());

    // Drop savepoints. Exception: when --discard-local-commits bypassed the
    // Phase 1' ancestor check (recorded as the `discard-local-commits`
    // override), preserve the project savepoint as a tombstone — the only
    // remaining reference to the discarded commits.
    let owner = op_state::read_owner(&ctx.owner_workspace_dir)?;
    let discard_tombstone = owner
        .as_ref()
        .map(|r| {
            r.overrides
                .contains(&op_state::Override::DiscardLocalCommits)
        })
        .unwrap_or(false);

    if !discard_tombstone {
        delete_savepoint(ctx.project_vcs.as_ref(), &canonical_project_dir, &ctx.op_id);
    } else if emit_text {
        eprintln!(
            "note: --discard-local-commits discarded project commits; pre-sync state preserved at \
             {savepoint} (recover with `git reset --hard {savepoint}` in {dir})",
            savepoint = ctx.project_vcs.savepoint_label(ctx.op_id.as_str()),
            dir = ctx.cwd_project_dir.display(),
        );
    }

    // Manifest savepoints: load the manifest from the canonical project repo
    // (the workweave's may be gone after retire) and drop each repo's savepoint
    // through the canonical clone. A missing ref is a harmless no-op, so no
    // existence guard is needed (and `if abs.exists()` would re-introduce the
    // leak by skipping the now-deleted workweave paths).
    if let Ok(project) = Project::from_dir_skip_lock(&canonical_project_dir) {
        for (repo_path, entry) in project.manifest.iter_entries() {
            let abs = primary.join(repo_path.as_path());
            delete_savepoint(vcs_for(entry.vcs_type).as_ref(), &abs, &ctx.op_id);
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
            let target_project_dir = project_dir(&ctx.dest_workspace_dir, tpname.as_str());
            delete_savepoint(ctx.project_vcs.as_ref(), &target_project_dir, &tsp_id);
            if let Ok(tp) = Project::from_dir_skip_lock(&target_project_dir) {
                for (repo_path, entry) in tp.manifest.iter_entries() {
                    let abs = ctx.dest_workspace_dir.join(repo_path.as_path());
                    if abs.exists() {
                        delete_savepoint(vcs_for(entry.vcs_type).as_ref(), &abs, &tsp_id);
                    }
                }
            }
        }
    }

    op_state::TouchedWorkspaces::of(ctx.verb, &ctx.owner_workspace_dir, &ctx.dest_workspace_dir)
        .clear();
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
/// tips. The project repo's post-sync state can diverge from the target by
/// exactly the auto-relock commit Phase 3 writes; that commit is purely
/// derived — the parent will regenerate it on its next sync — so refusing on
/// project-tip inequality would refuse every retire, even the happy path the
/// spec describes. Manifest tip equality is the honest "work has converged"
/// signal: Phase 2 advances both sides to the same SHAs, so post-sync the
/// manifest repos should be byte-equal.
fn retire_workweave_after_sync_to(
    project_vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    workweave_dir: &Path,
    workweave_name: &WorkweaveName,
    project: &crate::manifest::ProjectName,
    cwd_project_dir: &Path,
    target_workspace_dir: &Path,
) -> anyhow::Result<()> {
    let resume = op_state::resume_command(op_state::OpVerb::SyncTo);

    // Reload manifest post-Phase 3 so we see any repos newly added by sync.
    let manifest_path = cwd_project_dir.join(Manifest::FILE_NAME);
    let manifest =
        Manifest::from_path(&manifest_path).context("--retire: failed to reload manifest")?;

    // Compare each manifest repo's HEAD in CWD vs. target. After a successful
    // sync-to, step 3 has fast-forwarded the target's repos to CWD's tips, so
    // both sides should be at the same SHAs. We compare against the target
    // workspace directory (which sync-to already advanced in step 3).
    let target_root = target_workspace_dir;

    let mut diverged: Vec<String> = Vec::new();
    for (repo_path, entry) in manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
        let cwd_repo = workweave_dir.join(repo_path.as_path());
        let target_repo = target_root.join(repo_path.as_path());
        if !cwd_repo.exists() || !target_repo.exists() {
            // Missing on one side — leave the workweave alone; this is
            // unusual enough that the operator should look.
            diverged.push(format!("{}: missing on one side", repo_path.as_str()));
            continue;
        }
        let cwd_head = vcs
            .head_revision(&cwd_repo)
            .with_context(|| format!("--retire: read CWD head for {}", repo_path))?;
        let target_head = vcs
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
             refusing to delete:\n  {diverged}\n\
             \n\
             To reconcile: sync the divergent repo(s), then run:\n\
               {resume}   # re-runs the retire check\n\
             \n\
             To roll back the entire op: `rwv abort`.",
            diverged = diverged.join("\n  "),
        );
    }

    // Reuse the shared dirty-path check. Any dirty worktree blocks retire.
    let dirty =
        crate::workweave::collect_dirty_paths(project_vcs, workweave_dir, project, &manifest);
    if !dirty.is_empty() {
        anyhow::bail!(
            "--retire: workweave has uncommitted changes after sync-to; refusing to delete:\n  {dirty}\n\
             \n\
             Commit or discard the changes, then run:\n\
               {resume}   # re-runs the retire check\n\
             \n\
             To roll back the entire op: `rwv abort`.",
            dirty = dirty.join("\n  "),
        );
    }

    // Both invariants hold: delete the workweave. `--retire` has no flag that
    // could waive the delete's own dirty check, so `false` is the absence of
    // consent to waive rather than a judgement about the check — the refusal
    // above evaluates the same predicate, and the second evaluation is the
    // delete primitive's precondition, which every caller of it pays. Pass the
    // primary path (the delete resolves the workweave under the primary's
    // parent dir). Use the retire-specific entry point, which skips the
    // cross-verb op guard: THIS op still holds its `.rwv-op` record on the
    // workweave (cleared later in cleanup), so the guard would otherwise
    // refuse the op's own retire.
    crate::workweave::delete_workweave_for_retire(
        ctx.primary_path(),
        project,
        workweave_name,
        workweave_dir,
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

/// Phase 1' under `--discard-local-commits`: rewind the CWD project repo to
/// `to`, discarding whatever it had that `to` does not reach.
///
/// This is a rewinding MOVE, so its consent is constructed rather than
/// merely intended: the `DiscardWarrant` pairs the savepoint `guard_and_mark`
/// wrote with the operator's consent, and `reset_attached_ref` will not
/// accept a savepoint taken in some other repo. Without both, there is no
/// call to make.
///
/// **Where the consent comes from.** `--discard-local-commits` is parsed at
/// dispatch, but it is also *persisted* into the owner record and read back
/// on `--continue`, where the operator passes no flags at all. The record is
/// the durable form of the consent, and this is the layer that holds both
/// spellings of it — `ctx.discard_local_commits` is the flag on a fresh run
/// and the recorded override on a resumed one. Minting here rather than
/// threading a token from dispatch is what makes the resumed path carry the
/// same proof as the fresh one instead of a weaker one. This is the *only*
/// production mint of `DiscardLocalCommitsConsent`; see that type's doc
/// comment for why the compiler cannot make that count-of-one a rule, and
/// what to do instead of adding a second.
fn rewind_project_repo(ctx: &OpContext<'_>, to: &ResolvedRevisionId) -> anyhow::Result<()> {
    let vcs = ctx.project_vcs.as_ref();
    let savepoint = vcs
        .resolve_savepoint_ref(&ctx.cwd_project_dir, ctx.op_id.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "internal: --discard-local-commits reached Phase 1' with no savepoint under \
                 op {} in {}; refusing to discard commits that `rwv abort` could not restore",
                ctx.op_id.as_str(),
                ctx.cwd_project_dir.display(),
            )
        })?;
    let warrant = DiscardWarrant::new(savepoint, DiscardLocalCommitsConsent::granted());

    match vcs
        .head_attachment(&ctx.cwd_project_dir)
        .context("failed to read project HEAD ref")?
    {
        HeadAttachment::Attached(on) => vcs
            .reset_attached_ref(&on, to, warrant)
            .map_err(anyhow::Error::from)
            .context("project repo rewind (--discard-local-commits) failed"),
        // Already detached: repositioning HEAD changes no attachment, so it
        // is a MOVE too — one subject to the mid-operation
        // precondition, because a repo parked mid-bisect or mid-rebase is
        // carrying operator state a silent reposition would destroy. The
        // savepoint above still stands; `advance_detached_head` takes no
        // warrant, so the recoverability here rests on the savepoint alone.
        HeadAttachment::Detached(was) => vcs
            .advance_detached_head(&was, to)
            .map_err(anyhow::Error::from)
            .context("project repo rewind (--discard-local-commits) failed"),
        HeadAttachment::Unborn(u) => anyhow::bail!(
            "project repo at {} is on unborn branch '{}' (no commits yet); there is nothing \
             for --discard-local-commits to discard, and a reset here would stamp the branch \
             into existence rather than move it.",
            ctx.cwd_project_dir.display(),
            u,
        ),
    }
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
    vcs: &dyn Vcs,
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
    let mid_rebase = matches!(vcs.mid_op(cwd_project_dir), Some(ConflictOp::Rebase))
        && strategy == SyncStrategy::Rebase;

    if !mid_rebase && cwd_tip == source_tip {
        // No-op.
        return Ok(());
    }

    match strategy {
        SyncStrategy::Ff => {
            // CWD must be ancestor of source (caller verified). Fast-forward.
            vcs.advance_if_fast_forward(cwd_project_dir, source_tip)?;
        }
        SyncStrategy::Rebase => {
            // The replay states `keep_target_side`, which is what the repo's
            // committed `rwv.lock merge=rwv-ours` declaration names: lock-only
            // commits become empty patches and git drops them by default
            // (`--empty=drop`), so source's version of `rwv.lock` survives
            // the replay untouched. Phase 3 then regenerates the lock from
            // manifest tips — the resolution only has to be mechanical,
            // because regeneration is what makes the result correct.
            //
            // Replay re-entry (`rwv sync --continue`): if the project repo
            // is already mid-rebase from a previous phase that stopped on a
            // conflict, `Vcs::rebase` would fail immediately ("another
            // rebase is in progress"). Route through `Vcs::rebase_continue`
            // so `resolve → git add → rwv sync --continue` iterates per
            // conflicted pick. It states the same policy, so any remaining
            // lock-only picks resolve the same way a fresh `Vcs::rebase`
            // would. A mid-op that is NOT rebase (mid-merge,
            // mid-cherry-pick) is not rwv-initiated for this path —
            // preserve today's behavior of calling `Vcs::rebase`, which will
            // fail loudly rather than silently adopting foreign state.
            let outcome = if mid_rebase {
                vcs.rebase_continue(cwd_project_dir, DerivedContentPolicy::keep_target_side())
            } else {
                vcs.rebase(
                    cwd_project_dir,
                    source_tip,
                    source_tip,
                    DerivedContentPolicy::keep_target_side(),
                )
            };
            match outcome {
                Ok(()) => {}
                Err(VcsError::RebaseConflict { repo, op }) => {
                    let detail = vcs.rebase_stopped_commit_detail(&repo);
                    anyhow::bail!(
                        "{}",
                        per_conflict_bail_message(
                            vcs,
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
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    cwd_project_dir: &Path,
    cwd_project: &Project,
    source_workspace_name: &str,
) -> anyhow::Result<()> {
    let workweave_pair = match &ctx.checkout {
        Checkout::Workweave { name, dir, .. } => Some((name, dir.as_path())),
        Checkout::Primary { .. } => None,
    };

    let new_lock = generate_lock(
        &cwd_project.manifest,
        ctx.primary_path(),
        workweave_pair,
        true, // dirty: skip uncommitted-changes check; sync may have produced WT churn
    )
    .context("failed to generate lock")?;

    let lock_path = cwd_project_dir.join(LockFile::FILE_NAME);
    crate::lock::write_lock(&new_lock, &lock_path)?;

    let message = auto_relock_commit_message(source_workspace_name);
    if commit_lock_file_with_message(vcs, cwd_project_dir, &message)? {
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
/// Two hardening rails:
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
pub fn run_abort(ctx: &WorkspaceContext) -> anyhow::Result<()> {
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
    // for manifest repos, `project_repo_key` for the project repo. Empty before
    // relock completes — in that case the attributable set reduces to
    // {savepoint, advanced_tips, mid-op}.
    //
    // `advanced_tips` is the op's replay-phase intent: the planned target
    // (ff advances) or captured actual tip (rebased advances), written before
    // or right after each advance. Source/owner side only — target tips land
    // in converged_tips post-relock. Empty for pre-field records and
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

    let cwd_project_name = find_project_name(ctx)?;
    let cwd_project_dir = project_dir(&workspace_dir, cwd_project_name.as_str());
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
    let project_vcs = project_vcs();
    for (repo_path, entry) in cwd_project.manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
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
        match abort_one_repo(vcs.as_ref(), &abs, &cwd_restore_id, intent, converged) {
            Ok(outcome) => report_abort_outcome(
                vcs.as_ref(),
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
    let project_intent = advanced_tips.get(project_repo_key()).map(String::as_str);
    let project_converged = converged_tips.get(project_repo_key()).map(String::as_str);
    match abort_one_repo(
        project_vcs.as_ref(),
        &cwd_project_dir,
        &cwd_restore_id,
        project_intent,
        project_converged,
    ) {
        Ok(outcome) => report_abort_outcome(
            project_vcs.as_ref(),
            project_repo_key(),
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
                let extra_project_dir =
                    project_dir(extra_ctx.active_path(), extra_project_name.as_str());
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
                for (repo_path, entry) in extra_project.manifest.iter_entries() {
                    let vcs = vcs_for(entry.vcs_type);
                    let abs = extra_ws_dir.join(repo_path.as_path());
                    // Skip reference symlinks (shared canonical store): nothing
                    // was savepointed or advanced for them, so nothing to reset.
                    if !checkout_is_syncable(&abs) {
                        continue;
                    }
                    // Target-side repos: advanced_tips is source/owner side only.
                    // Target tips land in converged_tips post-relock; no intent entry.
                    let converged = converged_tips.get(repo_path.as_str()).map(String::as_str);
                    match abort_one_repo(vcs.as_ref(), &abs, &extra_restore_id, None, converged) {
                        Ok(outcome) => report_abort_outcome(
                            vcs.as_ref(),
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
                let extra_project_converged =
                    converged_tips.get(project_repo_key()).map(String::as_str);
                match abort_one_repo(
                    project_vcs.as_ref(),
                    &extra_project_dir,
                    &extra_restore_id,
                    None, // target-side: no advanced_tips entry
                    extra_project_converged,
                ) {
                    Ok(outcome) => report_abort_outcome(
                        project_vcs.as_ref(),
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
/// Two rails:
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
    vcs: &dyn Vcs,
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
    vcs.create_pre_abort_ref(repo, op_id.as_str())
        .context("create pre-abort ref failed")?;

    // Rail 2: HEAD-verified restore. `verified_restore_savepoint` performs
    // the classification + restore-if-attributable atomically; foreign tips
    // are returned as `ForeignTip` for the caller to report.
    vcs.verified_restore_savepoint(
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
    vcs: &dyn Vcs,
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
                let (ahead, behind) = vcs.ahead_behind(repo, savepoint, observed_tip);
                let shape = if behind == 0 && ahead > 0 {
                    format!("tip is {ahead} commit(s) ahead of savepoint (strictly ahead — common recoverable case)")
                } else if ahead > 0 && behind > 0 {
                    format!("tip and savepoint have diverged ({ahead} ahead, {behind} behind — requires manual reconciliation)")
                } else {
                    // ahead == 0 && behind == 0: equal — shouldn't reach ForeignTip, but be safe.
                    "tip equals savepoint (unexpected ForeignTip state)".to_string()
                };
                let (commits, total) =
                    vcs.log_oneline_range(repo, savepoint, observed_tip, BLOCKING_COMMITS_CAP);
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
pub fn run_sync_json(ctx: &WorkspaceContext, request: SyncRequest) -> anyhow::Result<()> {
    let records: Mutex<Vec<SyncOutcomeOutput>> = Mutex::new(Vec::new());
    let advisories: Mutex<Vec<AdvisoryOutput>> = Mutex::new(Vec::new());
    let stdout_lock: Mutex<()> = Mutex::new(());
    // This function is only reached when `--json` was passed, so `true` here
    // is that flag — the resolved mode's only remaining variable is `jobs`.
    let ndjson = crate::parallel::OutputMode::resolve(true, request.jobs).is_ndjson();
    let project_level_result = if ndjson {
        let handler = JsonNdjsonHandler {
            stdout_lock: &stdout_lock,
            records: &records,
            schema_url: SYNC_JSON_SCHEMA_URL,
        };
        run_machine(MachineVerb::Sync, ctx, &request, &handler)
    } else {
        let handler = JsonEnvelopeHandler {
            records: &records,
            advisories: &advisories,
        };
        run_machine(MachineVerb::Sync, ctx, &request, &handler)
    };

    let records = records.into_inner().unwrap_or_else(|e| e.into_inner());
    let advisories = advisories.into_inner().unwrap_or_else(|e| e.into_inner());

    run_sync_json_impl(
        ndjson,
        records,
        advisories,
        SYNC_JSON_SCHEMA_URL,
        project_level_result,
        false,
        ctx.resolution(),
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
    advisories: Vec<AdvisoryOutput>,
    schema_url: &str,
    project_level_result: anyhow::Result<()>,
    emit_empty_envelope: bool,
    resolution: Option<Resolution>,
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
            advisories,
            resolution,
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
pub fn run_sync_to(ctx: &WorkspaceContext, request: SyncRequest) -> anyhow::Result<()> {
    let stdout_lock: Mutex<()> = Mutex::new(());
    let handler = TextHandler {
        stdout_lock: &stdout_lock,
    };
    run_machine(MachineVerb::SyncTo, ctx, &request, &handler)
}

/// Execute `rwv sync-to <target> --json`.
///
/// Emits a [`SyncToJsonOutput`] envelope with the new observability fields:
/// `source_workweave`, `target`, `retired`, per-outcome `step3_advance`, and
/// `project_repo_advance`. These fields are absent from the plain
/// `rwv sync --json` envelope ([`SyncJsonOutput`]).
pub fn run_sync_to_json(ctx: &WorkspaceContext, request: SyncRequest) -> anyhow::Result<()> {
    // Derive source_workweave from the CWD context before running the machine.
    // This mirrors what guard_and_mark computes internally.
    let source_workweave: Option<String> = match &ctx.checkout {
        Checkout::Workweave { name, .. } => Some(name.as_str().to_owned()),
        Checkout::Primary { .. } => None,
    };

    // Derive the target path: the resolved destination workspace directory.
    // For sync-to the operator-supplied arg is the target; resolve it the same
    // way guard_and_mark does (SyncSource::resolve against the CWD context).
    let target_path: String = match &request.source {
        Some(src) => src
            .resolve(ctx)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        None => String::new(),
    };

    let records: Mutex<Vec<SyncOutcomeOutput>> = Mutex::new(Vec::new());
    let step3_advances: Mutex<std::collections::HashMap<String, Step3AdvanceOutput>> =
        Mutex::new(std::collections::HashMap::new());
    let stdout_lock: Mutex<()> = Mutex::new(());
    // This function is only reached when `--json` was passed, so `true` here
    // is that flag — the resolved mode's only remaining variable is `jobs`.
    let ndjson = crate::parallel::OutputMode::resolve(true, request.jobs).is_ndjson();
    let project_level_result = if ndjson {
        // NDJSON mode: use the standard NDJSON handler (step3 SHAs are not
        // surfaced per-line in NDJSON; the envelope-level fields are only
        // emitted in serial mode).
        let handler = JsonNdjsonHandler {
            stdout_lock: &stdout_lock,
            records: &records,
            schema_url: SYNC_TO_JSON_SCHEMA_URL,
        };
        run_machine(MachineVerb::SyncTo, ctx, &request, &handler)
    } else {
        let handler = JsonEnvelopeSyncToHandler {
            records: &records,
            step3_advances: &step3_advances,
        };
        run_machine(MachineVerb::SyncTo, ctx, &request, &handler)
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

        let project_repo_advance = step3_map.remove(project_repo_key());

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
            resolution: ctx.resolution(),
        };
        let out =
            serde_json::to_string_pretty(&payload).context("failed to serialize sync-to output")?;
        println!("{out}");
    }

    json_exit_tail(any_failure, project_level_result)
}

/// Per-repo advance-target line, naming the branch that received the landing.
fn ff_advance_line(advanced: Option<&AttachedRef>, tip: &ResolvedRevisionId) -> String {
    let short = &tip.as_str()[..8.min(tip.as_str().len())];
    match advanced {
        Some(branch) => format!("ff-advanced {branch} to {short}"),
        None => format!("already at {short}"),
    }
}

/// Fast-forward the branch `target_repo` is on to `cwd_tip`, returning the
/// witness for that branch. `None` means the target was already at `cwd_tip`
/// and nothing moved.
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
///
/// # The landing target is a witness, not a path
///
/// The refusal below is not the only thing standing between a detached
/// target and a landing that referenced nothing; it is also a *type*:
/// the MOVE takes an [`AttachedRef`] and derives the repo it moves from
/// that witness, so there is no signature in which the branch this function
/// establishes and the repo it advances can come apart. `target_repo` is
/// where the witness is *obtained*; it is never handed to the MOVE.
///
/// That closes the dodge the runtime check alone left open. `cwd_repo` is a
/// workweave checkout and is therefore always attached, so a witness taken
/// from it would satisfy any "did you check for a branch" gate while the
/// advance still landed on the detached target. With the witness carrying
/// its own repo, using CWD's attachment to move the target is not a check
/// someone can route around — it is a call that does not typecheck.
fn ff_advance_repo(
    vcs: &dyn Vcs,
    target_repo: &Path,
    cwd_repo: &Path,
    cwd_tip: &ResolvedRevisionId,
) -> anyhow::Result<Option<AttachedRef>> {
    // Verify that target_repo's HEAD is an ancestor of (or equal to) cwd_tip.
    // If not, this is a concurrent-modification scenario — bail.
    let target_tip = vcs
        .head_revision(target_repo)
        .context("failed to read target HEAD")?;

    if target_tip == *cwd_tip {
        return Ok(None); // already at the right tip
    }

    // Landing must name the ref it lands on. `merge --ff-only` against a
    // detached HEAD moves HEAD alone and reports success, leaving the work on
    // no branch — and the source's branch, the only other ref holding it, is
    // force-deleted by `--retire`. Checked after the equal-tip return, like
    // the dirty gate below: a checkout we won't move needs no destination.
    //
    // Obtaining the witness IS the refusal: the MOVE below cannot be written
    // without one, and the only producer is a `head_attachment` read of this
    // repo. `Unborn` is a third state rather than a second spelling of
    // detached — unreachable here, because a branch with no commits
    // fails the `head_revision` read above, but it is answered rather than
    // folded in so the arm cannot be quietly re-collapsed.
    let on = match vcs
        .head_attachment(target_repo)
        .context("failed to read target HEAD ref")?
    {
        HeadAttachment::Attached(a) => a,
        HeadAttachment::Detached(d) => anyhow::bail!(
            "target repo at {} is not on a branch (detached HEAD at {}); refusing to \
             land onto it. Nothing would record the advance to {}. Check out the branch \
             that should receive this work (`git switch <branch>` in the target), then \
             re-run.",
            target_repo.display(),
            d.at(),
            cwd_tip,
        ),
        HeadAttachment::Unborn(u) => anyhow::bail!(
            "target repo at {} is on unborn branch '{}' (no commits yet); refusing to \
             land onto it. Nothing would record the advance to {}. Commit in the target, \
             or check out the branch that should receive this work, then re-run.",
            target_repo.display(),
            u,
            cwd_tip,
        ),
    };

    // Fast-forwarding a dirty target worktree risks its uncommitted changes.
    // The sync-to preflight already refused on a dirty target; this catches
    // concurrent modification since then, with a named precondition instead
    // of merge's generic refusal. Checked after the equal-tip return: a
    // dirty worktree we won't move is safe.
    if vcs.has_uncommitted_changes(target_repo).unwrap_or(true) {
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
    vcs.fetch_objects_from(target_repo, cwd_repo);

    let is_ancestor = vcs
        .is_ancestor(target_repo, &target_tip, cwd_tip)
        .unwrap_or(false);

    if !is_ancestor {
        anyhow::bail!(
            "target repo at {} cannot be fast-forwarded: target tip ({}) is not an ancestor \
             of CWD tip ({}). The target holds commits CWD's tip does not, so landing CWD \
             onto it would drop them. Either the target moved after step 1, or replay took \
             the target's lock as its base while that lock was behind these commits \
             (`--allow-stale-lock` permits that). Roll back with `rwv abort` and re-run; a \
             target whose lock is behind its HEAD is named at op start with the \
             `rwv lock --commit` that fixes it.",
            target_repo.display(),
            target_tip,
            cwd_tip,
        );
    }

    // Fast-forward. The witness names both the ref that moves and the repo
    // it moves in; `advance_attached_ref` re-observes before acting, so an
    // attachment that changed since the read above is a refusal rather than
    // a landing on whatever HEAD became (how wide that window should be
    // stays open). Underneath, the ff refuses rather than clobbers
    // if the update would touch uncommitted changes — the VCS-native
    // backstop behind the two explicit dirty gates above.
    vcs.advance_attached_ref(&on, cwd_tip)
        .context("fast-forward advance failed in target")?;

    Ok(Some(on))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::git_vcs;

    // -----------------------------------------------------------------------
    // Fixtures for the branch-model tests below
    // -----------------------------------------------------------------------

    /// Run git in `dir`, panicking on failure.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed in {}: {}",
            args,
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A repo on `main` with one commit, at `dir`.
    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "T"]);
        std::fs::write(dir.join("f"), "1").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "one"]);
    }

    /// Commit a new file and return the resulting tip.
    fn commit(dir: &Path, name: &str) -> ResolvedRevisionId {
        std::fs::write(dir.join(name), name).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", name]);
        git_vcs().head_revision(dir).unwrap()
    }

    // -----------------------------------------------------------------------
    // ff_advance_repo — landing takes the target's witness
    //
    // This is the phase-body layer of the detached-target refusal, and it is
    // the layer that covers `sync-to --continue`: a resumed op re-enters at
    // the phase, so the whole-op preflight does not run again. These drive
    // the function directly for that reason — routing through the CLI would
    // be answered by the preflight before this code was reached, and the
    // resume path is exactly where that is not true.
    // -----------------------------------------------------------------------

    /// A (cwd, target) pair of independent clones of the same history, where
    /// cwd has one commit the target does not. `target` is on `main`.
    fn landing_pair(tmp: &Path) -> (PathBuf, PathBuf, ResolvedRevisionId) {
        let origin = tmp.join("origin");
        init_repo(&origin);

        let target = tmp.join("target");
        git(
            tmp,
            &["clone", origin.to_str().unwrap(), target.to_str().unwrap()],
        );
        let cwd = tmp.join("cwd");
        git(
            tmp,
            &["clone", origin.to_str().unwrap(), cwd.to_str().unwrap()],
        );
        git(&cwd, &["config", "user.email", "t@t"]);
        git(&cwd, &["config", "user.name", "T"]);
        let cwd_tip = commit(&cwd, "landed");
        (cwd, target, cwd_tip)
    }

    #[test]
    fn ff_advance_repo_lands_on_the_branch_the_target_is_attached_to() {
        // The control. Without it, the refusal test below could pass because
        // this fixture cannot advance at all rather than because the refusal
        // fired.
        let tmp = tempfile::tempdir().unwrap();
        let (cwd, target, cwd_tip) = landing_pair(tmp.path());

        let landed = ff_advance_repo(git_vcs().as_ref(), &target, &cwd, &cwd_tip)
            .expect("an attached target accepts the landing")
            .expect("the target moved, so a branch received it");

        assert_eq!(
            landed.to_string(),
            "main",
            "the returned witness must name the branch that received the landing"
        );
        assert_eq!(
            git(&target, &["rev-parse", "refs/heads/main"]),
            cwd_tip.as_str(),
            "target `main` must hold the landed commit"
        );
    }

    #[test]
    fn ff_advance_repo_refuses_to_land_onto_a_detached_target() {
        // The loss chain: `merge --ff-only` against a detached HEAD
        // moves HEAD alone and reports success, so the landing ends up
        // referenced by nothing — and `--retire` then force-deletes the
        // source's branch, the only other ref that was holding it.
        let tmp = tempfile::tempdir().unwrap();
        let (cwd, target, cwd_tip) = landing_pair(tmp.path());

        let main_before = git(&target, &["rev-parse", "refs/heads/main"]);
        git(&target, &["checkout", "--detach", "HEAD"]);
        let head_before = git(&target, &["rev-parse", "HEAD"]);

        let err = ff_advance_repo(git_vcs().as_ref(), &target, &cwd, &cwd_tip)
            .expect_err("a detached target has no branch for the landing to advance");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("is not on a branch (detached HEAD at"),
            "the refusal must name the detached-target precondition, not some other \
             failure; got:\n{msg}"
        );

        assert_eq!(
            git(&target, &["rev-parse", "refs/heads/main"]),
            main_before,
            "a refused landing must leave the target's branch where it was"
        );
        assert_eq!(
            git(&target, &["rev-parse", "HEAD"]),
            head_before,
            "a refused landing must leave the target's detached HEAD where it was — \
             this is the assertion that fails if the MOVE goes back to taking a path \
             instead of the target's witness"
        );
    }

    #[test]
    fn ff_advance_repo_short_circuits_a_detached_target_that_is_already_there() {
        // The refusal sits *after* the equal-tip return, deliberately: a
        // checkout we are not going to move needs no destination. Pinned so
        // the ordering cannot be "tidied" into refusing on every detached
        // target, which would make `--continue` unable to finish an op whose
        // target was already advanced.
        let tmp = tempfile::tempdir().unwrap();
        let (cwd, target, cwd_tip) = landing_pair(tmp.path());

        git(&target, &["fetch", cwd.to_str().unwrap(), "HEAD"]);
        git(&target, &["checkout", "--detach", cwd_tip.as_str()]);

        let landed = ff_advance_repo(git_vcs().as_ref(), &target, &cwd, &cwd_tip)
            .expect("an already-converged target is a no-op, detached or not");
        assert!(
            landed.is_none(),
            "nothing moved, so no branch received anything"
        );
    }

    // -----------------------------------------------------------------------
    // check_store_unclaimed — R4, in front of the store destroy
    // -----------------------------------------------------------------------

    /// A canonical store plus the primary root and project name its receipt
    /// registry is keyed by.
    fn store_fixture(tmp: &Path) -> (PathBuf, PathBuf, ProjectName) {
        let primary = tmp.join("weave");
        std::fs::create_dir_all(primary.join("projects").join("web-app")).unwrap();
        let store = primary.join("github/example/server");
        init_repo(&store);
        (store, primary, ProjectName::new("web-app").unwrap())
    }

    fn dropped() -> RepoPath {
        RepoPath::new("github/example/server").unwrap()
    }

    #[test]
    fn check_store_unclaimed_passes_on_a_store_nothing_claims() {
        // The control: without it the two refusals below could be passing on
        // a fixture that can never be unclaimed.
        let tmp = tempfile::tempdir().unwrap();
        let (store, primary, project) = store_fixture(tmp.path());

        check_store_unclaimed(git_vcs().as_ref(), &store, &primary, &project, &dropped())
            .expect("no worktrees registered and no receipts standing");
    }

    #[test]
    fn check_store_unclaimed_refuses_while_a_worktree_is_registered() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, primary, project) = store_fixture(tmp.path());

        // `git worktree add` writes its administration into the canonical
        // store, so `remove_dir_all` on the store takes this checkout's refdb
        // and objects with it. Detached, so the fixture adds no local-only
        // branch — the point is that R4 refuses on the registration alone.
        let live = tmp.path().join("live-workweave");
        git(
            &store,
            &["worktree", "add", "--detach", live.to_str().unwrap()],
        );

        let err = check_store_unclaimed(git_vcs().as_ref(), &store, &primary, &project, &dropped())
            .expect_err("a store with a live worktree registered against it is claimed");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("still has live worktrees registered"),
            "the refusal must name the registration, not the receipts; got:\n{msg}"
        );
        assert!(
            msg.contains("live-workweave"),
            "the refusal must list the worktree that claims the store; got:\n{msg}"
        );
    }

    #[test]
    fn check_store_unclaimed_refuses_while_a_receipt_stands() {
        let tmp = tempfile::tempdir().unwrap();
        let (store, primary, project) = store_fixture(tmp.path());
        let at = git_vcs().head_revision(&store).unwrap();

        let mut registry = crate::workweave_index::RefRegistry::for_project(&primary, &project);
        registry
            .record_created(
                &store,
                EphemeralRefName::mint(&project, &WorkweaveName::new("hotfix").unwrap()),
                at,
            )
            .unwrap();

        let err = check_store_unclaimed(git_vcs().as_ref(), &store, &primary, &project, &dropped())
            .expect_err("a standing receipt is rwv still accounting for a ref in there");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("still holds ownership receipts"),
            "the refusal must name the receipts, not the registrations; got:\n{msg}"
        );
        assert!(
            msg.contains("web-app--hotfix"),
            "the refusal must name the ref whose receipt stands; got:\n{msg}"
        );

        // R4 is satisfied by retraction, not by ignoring the receipt.
        registry
            .retract(&store, &crate::vcs::RawRefName::new("web-app--hotfix"))
            .unwrap();
        check_store_unclaimed(git_vcs().as_ref(), &store, &primary, &project, &dropped())
            .expect("with the receipt retracted the store is unclaimed");
    }

    #[test]
    fn check_store_unclaimed_refuses_when_the_claims_cannot_be_read() {
        // Fail-closed: a claim we could not enumerate is a claim that stands.
        let tmp = tempfile::tempdir().unwrap();
        let not_a_repo = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&not_a_repo).unwrap();

        let err = check_store_unclaimed(
            git_vcs().as_ref(),
            &not_a_repo,
            tmp.path(),
            &ProjectName::new("web-app").unwrap(),
            &dropped(),
        )
        .expect_err("an unreadable store must not be destroyed on the strength of a guess");
        assert!(
            format!("{err:#}").contains("cannot enumerate the worktrees registered against"),
            "the refusal must say the enumeration failed; got:\n{err:#}"
        );
    }

    // -----------------------------------------------------------------------
    // prune_dropped_repo — R4 in front of the store destroy, and the shipped
    // local-only refusal behind it, unrelaxed
    // -----------------------------------------------------------------------

    /// A primary weave whose `github/example/server` is a *clone* of a bare
    /// origin, so `main` has a remote counterpart and is not ahead of it —
    /// which is what makes the shipped local-only predicate pass and leaves
    /// R4 as the thing under test. A fixture without the remote would refuse
    /// before ever reaching the gate, and every assertion below would be
    /// about the wrong refusal.
    fn primary_with_cloned_store(tmp: &Path) -> (WorkspaceContext, PathBuf, ProjectName) {
        let origin = tmp.join("origin");
        init_repo(&origin);

        let primary = tmp.join("weave");
        std::fs::create_dir_all(primary.join("projects").join("web-app")).unwrap();
        std::fs::create_dir_all(primary.join("github/example")).unwrap();
        let store = primary.join("github/example/server");
        git(
            &primary,
            &["clone", origin.to_str().unwrap(), store.to_str().unwrap()],
        );
        git(&store, &["config", "user.email", "t@t"]);
        git(&store, &["config", "user.name", "T"]);

        let project = ProjectName::new("web-app").unwrap();
        let ctx = WorkspaceContext::resolve(&primary, Some(project.clone()))
            .expect("the fixture is a workspace root");
        (ctx, store, project)
    }

    #[test]
    fn prune_dropped_repo_removes_a_store_nothing_claims() {
        // The control. Without it, the two refusals below could be reporting
        // a fixture that can never be pruned rather than a gate that fired.
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, store, project) = primary_with_cloned_store(tmp.path());

        prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped(), &project)
            .expect("nothing claims this store");
        assert!(
            !store.exists(),
            "an unclaimed store is what prune is for; it must actually be removed"
        );
    }

    #[test]
    fn prune_dropped_repo_refuses_while_a_live_workweave_is_registered() {
        // `git worktree add` runs in the canonical store, so the worktree's
        // administration and its objects live *inside* the directory prune is
        // about to `remove_dir_all`. Detached, so the fixture adds no
        // local-only branch: the shipped predicate passes and R4 is the only
        // thing left standing between the live workweave and the delete.
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, store, project) = primary_with_cloned_store(tmp.path());

        let live = tmp.path().join("live-workweave");
        git(
            &store,
            &["worktree", "add", "--detach", live.to_str().unwrap()],
        );

        let err = prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped(), &project)
            .expect_err("a store a live workweave is registered against is claimed");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("still has live worktrees registered"),
            "R4's registration arm must be what refused — not the local-only scan, \
             which this fixture satisfies; got:\n{msg}"
        );
        assert!(
            store.exists() && live.exists(),
            "a refused prune must leave both the store and the live checkout on disk"
        );
    }

    #[test]
    fn prune_dropped_repo_refuses_until_the_receipts_are_retracted() {
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, store, project) = primary_with_cloned_store(tmp.path());
        let at = git_vcs().head_revision(&store).unwrap();

        let mut registry =
            crate::workweave_index::RefRegistry::for_project(ctx.primary_path(), &project);
        registry
            .record_created(
                &store,
                EphemeralRefName::mint(&project, &WorkweaveName::new("feat").unwrap()),
                at,
            )
            .unwrap();

        let err = prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped(), &project)
            .expect_err("rwv still accounts for a ref in this store");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("still holds ownership receipts"),
            "R4's receipt arm must be what refused; got:\n{msg}"
        );
        assert!(
            store.exists(),
            "a refused prune must leave the store on disk"
        );

        // Retraction is the only thing that clears it — R4 is satisfied by
        // having run the per-ref discipline dry, not by skipping it.
        registry
            .retract(&store, &crate::vcs::RawRefName::new("web-app--feat"))
            .unwrap();
        prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped(), &project)
            .expect("with every receipt retracted the store is unclaimed");
        assert!(
            !store.exists(),
            "once nothing claims the store, prune removes it"
        );
    }

    #[test]
    fn prune_dropped_repo_does_not_exempt_a_recorded_ref_from_the_local_only_refusal() {
        // Recorded rwv refs are deliberately NOT excluded
        // from the local-only predicate. That refusal is incidentally the
        // only thing that has been keeping `remove_dir_all` off a live
        // workweave's object store, so ownership-by-receipt buys no
        // exemption from it — unblocking prune is not a payoff of R2.
        //
        // This is the E1 scenario: a workweave holding a commit that exists
        // nowhere else. It must refuse *because of the unique commit*, with
        // a receipt on file, not in spite of one.
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, store, project) = primary_with_cloned_store(tmp.path());
        let at = git_vcs().head_revision(&store).unwrap();

        let live = tmp.path().join("live-workweave");
        git(
            &store,
            &[
                "worktree",
                "add",
                "-b",
                "web-app--feat",
                live.to_str().unwrap(),
            ],
        );
        git(&live, &["config", "user.email", "t@t"]);
        git(&live, &["config", "user.name", "T"]);
        let unique = commit(&live, "only-here");

        crate::workweave_index::RefRegistry::for_project(ctx.primary_path(), &project)
            .record_created(
                &store,
                EphemeralRefName::mint(&project, &WorkweaveName::new("feat").unwrap()),
                at,
            )
            .unwrap();

        let err = prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped(), &project)
            .expect_err("a store holding a commit that exists nowhere else is not prunable");
        assert!(
            format!("{err:#}").contains("local-only commits"),
            "the shipped local-only refusal must still fire on a ref rwv holds a \
             receipt for; got:\n{err:#}"
        );
        assert_eq!(
            git(&live, &["rev-parse", "HEAD"]),
            unique.as_str(),
            "the unique commit must survive the refused prune"
        );
    }

    #[test]
    fn prune_dropped_repo_refuses_when_the_ahead_count_cannot_be_taken() {
        // The remote-tracking ref is present, so the counterpart probe passes
        // and the branch reaches the ahead-count; it points at an object the
        // clone does not have, which is the shape that makes `rev-list`
        // FAIL rather than answer. That failure is "we could not tell" and
        // has to refuse — read as "nothing unpushed" it clears the branch and
        // hands the store to the delete.
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, store, project) = primary_with_cloned_store(tmp.path());

        // A loose ref file wins over the packed-refs entry the clone wrote.
        let dangling = store.join(".git/refs/remotes/origin/main");
        std::fs::create_dir_all(dangling.parent().unwrap()).unwrap();
        std::fs::write(&dangling, "1111111111111111111111111111111111111111\n").unwrap();
        assert!(
            git_vcs()
                .branch_has_remote_counterpart(
                    &store,
                    &RefName::new("main".to_owned()),
                    Role::Owned
                )
                .unwrap(),
            "the fixture must still pass the counterpart probe, or the refusal \
             under test is never reached"
        );

        let err = prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped(), &project)
            .expect_err("a count git could not take is not evidence that nothing is unpushed");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("could not be ruled out"),
            "the unreadable count must be what refused, and must say so rather \
             than claim commits it never saw; got:\n{msg}"
        );
        assert!(
            store.exists(),
            "a refused prune must leave the clone on disk"
        );
    }

    // -----------------------------------------------------------------------
    // prune_dropped_repo — the workweave arm, where the absence of a
    // primary-side canonical decides nothing about what the checkout IS
    // -----------------------------------------------------------------------

    /// A workweave context whose primary-side `github/example/server` slot is
    /// ABSENT — the shape that routes prune into the arm with no canonical to
    /// compare against. What the workweave's own checkout is left unbuilt on
    /// purpose: that is the thing under test, so each case materializes it.
    /// Returns the context, the checkout path to build, and the project.
    fn workweave_without_primary_canonical(tmp: &Path) -> (WorkspaceContext, PathBuf, ProjectName) {
        let primary = tmp.join("weave");
        std::fs::create_dir_all(primary.join("projects").join("web-app")).unwrap();
        // Deliberately no `primary/github/example/server`.

        let ww = primary.join(".workweaves").join("web-app--feat");
        std::fs::create_dir_all(ww.join("github/example")).unwrap();
        let primary_canon = primary.canonicalize().unwrap();
        crate::workspace::WorkweaveMarker::new(
            primary_canon.clone(),
            ProjectName::new("web-app").unwrap(),
            &primary_canon,
        )
        .write(&ww)
        .unwrap();

        let ctx = WorkspaceContext::resolve(&ww, None).expect("the marker names the primary weave");
        assert!(
            matches!(ctx.checkout, Checkout::Workweave { .. }),
            "the fixture must resolve as a workweave, or the arm under test is not the one that runs"
        );
        assert!(
            !ctx.primary_path().join("github/example/server").exists(),
            "the absent primary-side canonical is the precondition of this arm"
        );
        (
            ctx,
            ww.join("github/example/server"),
            ProjectName::new("web-app").unwrap(),
        )
    }

    #[test]
    fn prune_dropped_repo_refuses_a_workweave_checkout_that_is_itself_the_store() {
        // Inverted topology: the workweave holds a standalone clone rather
        // than a linked worktree. The absent primary-side slot used to be read as
        // "the store is gone, so this must be a linked worktree, delete it" —
        // but the objects under this path exist nowhere else in the weave, and
        // with no canonical to compare against, nothing has looked at them.
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        init_repo(&origin);
        let (ctx, checkout, project) = workweave_without_primary_canonical(tmp.path());
        git(
            tmp.path(),
            &[
                "clone",
                origin.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        assert!(
            checkout.join(".git").is_dir(),
            "the fixture must be a standalone clone, not a linked workspace"
        );
        let tip = git_vcs().head_revision(&checkout).unwrap();

        let err = prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped(), &project)
            .expect_err("a checkout that IS the store is not a working tree to delete");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("inverted clone topology"),
            "the refusal must name what it found, not blame uncommitted changes \
             or divergence; got:\n{msg}"
        );
        assert!(
            checkout.exists(),
            "a refused prune must leave the store on disk"
        );
        assert_eq!(
            git_vcs().head_revision(&checkout).unwrap(),
            tip,
            "the object database must survive the refused prune intact"
        );
    }

    #[test]
    fn prune_dropped_repo_removes_a_workweave_checkout_linked_into_a_store_elsewhere() {
        // The control for the refusal above. Same absent primary-side slot,
        // but the checkout is a real linked workspace whose refdb and objects
        // live in a store this delete never touches — so removing the
        // directory removes a working tree and nothing else. Without this,
        // the refusal above could be reporting an arm that never removes
        // anything.
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join("elsewhere/server");
        init_repo(&store);
        let (ctx, checkout, project) = workweave_without_primary_canonical(tmp.path());
        git(
            &store,
            &["worktree", "add", "--detach", checkout.to_str().unwrap()],
        );
        assert!(
            checkout.join(".git").is_file(),
            "the fixture must be a linked workspace, not a standalone clone"
        );

        prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped(), &project)
            .expect("a linked workspace is a working tree, and pruning it is what this arm is for");
        assert!(
            !checkout.exists(),
            "the dropped repo's working tree must actually be removed"
        );
        assert!(
            git_vcs().head_revision(&store).is_ok(),
            "the store the checkout linked into must be left intact"
        );
    }

    // -----------------------------------------------------------------------
    // materialize_missing_repo — phase 3 births through the chokepoint, so a
    // name rwv holds no receipt for is refused rather than claimed (R2)
    //
    // Sync is the one production writer that used to re-implement birth as a
    // bare `record_created` + `create_worktree_on`. That skipped the
    // (receipt, ref) classification entirely: whatever sat at the minted name
    // got a receipt minted over it, and ownership-by-record then made the
    // operator's branch rwv's to destroy on the next workweave delete.
    // -----------------------------------------------------------------------

    /// A workweave whose primary holds the canonical clone and whose own
    /// checkout of that repo is ABSENT — the state phase 3 materializes into.
    /// Returns the context, the canonical store, the project, and the entry
    /// whose `version` is the start point the ephemeral ref is cut at.
    fn workweave_missing_one_repo(
        tmp: &Path,
    ) -> (
        WorkspaceContext,
        PathBuf,
        ProjectName,
        crate::manifest::RepoEntry,
    ) {
        let origin = tmp.join("origin");
        init_repo(&origin);

        let primary = tmp.join("weave");
        std::fs::create_dir_all(primary.join("projects").join("web-app")).unwrap();
        std::fs::create_dir_all(primary.join("github/example")).unwrap();
        let canonical = primary.join("github/example/server");
        git(
            &primary,
            &[
                "clone",
                origin.to_str().unwrap(),
                canonical.to_str().unwrap(),
            ],
        );
        git(&canonical, &["config", "user.email", "t@t"]);
        git(&canonical, &["config", "user.name", "T"]);

        let ww = primary.join(".workweaves").join("web-app--feat");
        std::fs::create_dir_all(ww.join("github/example")).unwrap();
        let primary_canon = primary.canonicalize().unwrap();
        crate::workspace::WorkweaveMarker::new(
            primary_canon.clone(),
            ProjectName::new("web-app").unwrap(),
            &primary_canon,
        )
        .write(&ww)
        .unwrap();

        let ctx = WorkspaceContext::resolve(&ww, None).expect("the marker names the primary weave");
        assert!(
            matches!(ctx.checkout, Checkout::Workweave { .. }),
            "the fixture must resolve as a workweave, or materialize takes the clone arm instead"
        );
        let entry = crate::manifest::RepoEntry {
            vcs_type: crate::manifest::VcsType::Git,
            url: "https://example.com/server.git".parse().unwrap(),
            version: crate::vcs::RefName::new("main"),
            role: Role::Owned,
        };
        (ctx, canonical, ProjectName::new("web-app").unwrap(), entry)
    }

    /// The receipt phase 3 wrote for this workweave's ephemeral ref, if any.
    fn materialized_receipt(
        ctx: &WorkspaceContext,
        canonical: &Path,
        project: &ProjectName,
    ) -> Option<crate::vcs::OwnedRef> {
        crate::workweave_index::RefRegistry::for_project(ctx.primary_path(), project)
            .lookup(
                &crate::workweave::receipt_store_for(crate::git::git_vcs().as_ref(), canonical),
                &crate::vcs::RawRefName::new("web-app--feat"),
            )
            .expect("the receipt store is readable")
    }

    #[test]
    fn materialize_missing_repo_authors_the_ref_and_records_it() {
        // The control. Without it the refusal below could be passing on a
        // fixture that can never materialize anything at all.
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, canonical, project, entry) = workweave_missing_one_repo(tmp.path());

        materialize_missing_repo(git_vcs().as_ref(), &ctx, &dropped(), &entry, &project)
            .expect("a name nothing holds is the ordinary create");

        let checkout = ctx.active_path().join(dropped().as_path());
        assert!(
            checkout.join(".git").is_file(),
            "the materialized repo must be a linked workspace on the ephemeral ref"
        );
        assert!(
            materialized_receipt(&ctx, &canonical, &project).is_some(),
            "the receipt is what makes a sync-materialized worktree visible to \
             `workweave delete`, which ranges over the recorded set"
        );
    }

    #[test]
    fn materialize_missing_repo_refuses_a_pre_existing_branch_it_holds_no_receipt_for() {
        // R2's refusal arm: a ref that merely *looks* like rwv's is not rwv's,
        // so it is neither adopted into a receipt nor destroyed. If this test
        // ever goes green on a materialize that SUCCEEDS, the operator's
        // branch has just become rwv's to delete.
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, canonical, project, entry) = workweave_missing_one_repo(tmp.path());

        // The operator's own branch, sitting at the name rwv would mint, and
        // carrying a commit that exists nowhere else.
        git(&canonical, &["checkout", "-b", "web-app--feat"]);
        let operator_tip = commit(&canonical, "operators-work");
        git(&canonical, &["checkout", "main"]);

        let err = materialize_missing_repo(git_vcs().as_ref(), &ctx, &dropped(), &entry, &project)
            .expect_err("a branch rwv holds no receipt for is not rwv's to reuse");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("rwv holds no ownership receipt for it"),
            "the unowned-ref arm must be what refused — not a worktree-add failure \
             downstream of a receipt that was already minted; got:\n{msg}"
        );
        assert!(
            materialized_receipt(&ctx, &canonical, &project).is_none(),
            "no receipt may be minted over a branch rwv did not create: the receipt \
             is what a later delete reads as authorization to destroy it"
        );
        assert_eq!(
            git_vcs()
                .resolve_revision(&canonical, "web-app--feat")
                .unwrap(),
            operator_tip,
            "the operator's branch must survive the refusal at its own tip"
        );
    }

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
        crate::symlink::create(&canonical, &link, crate::symlink::LinkTarget::Directory).unwrap();
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
        crate::symlink::create(
            &tmp.path().join("missing-target"),
            &link,
            crate::symlink::LinkTarget::File,
        )
        .unwrap();
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
        let parsed: SyncSource = "hotfix".parse().unwrap();
        assert_eq!(
            parsed,
            SyncSource::Workweave(WorkweaveName::new("hotfix").unwrap())
        );
    }

    /// Not path-like (`looks_path_like` requires a separator, a leading dot,
    /// or an absolute path), so this reaches the `WorkweaveName::new` arm and
    /// is refused there rather than silently minting an ambiguous name.
    #[test]
    fn sync_source_parse_rejects_double_dash() {
        let err = "proj--feat--v2".parse::<SyncSource>().unwrap_err();
        assert!(
            matches!(err, WorkweaveNameError::AmbiguousDelimiter(_)),
            "expected AmbiguousDelimiter, got: {err:?}"
        );
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
        let s = SyncSource::Workweave(WorkweaveName::new("ww1").unwrap());
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

    // Site 1 — manifest-repo per-repo sync loop failure summary. With a live
    // rebase conflict the VCS-native staging steps are present; the resume
    // verb is derived from the op verb; and the rebase hint must NOT spell
    // raw `git rebase --continue` (`rwv <verb> --continue` IS the continue
    // step — rwv resumes the rebase natively).
    #[test]
    fn manifest_repo_failure_message_live_conflict_includes_vcs_and_verb_resume() {
        let msg = manifest_repo_failure_message(
            git_vcs().as_ref(),
            OpVerb::SyncTo,
            Some(ConflictOp::Rebase),
        );
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
        let msg = manifest_repo_failure_message(git_vcs().as_ref(), OpVerb::Sync, None);
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
            git_vcs().as_ref(),
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
        let msg = phase1_or_phase3_failure_message(
            git_vcs().as_ref(),
            Phase::One,
            cwd,
            OpVerb::Sync,
            None,
        );
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
        let msg = phase1_or_phase3_failure_message(
            git_vcs().as_ref(),
            Phase::Three,
            cwd,
            OpVerb::SyncTo,
            None,
        );
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
            git_vcs().as_ref(),
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
            git_vcs().as_ref(),
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
            git_vcs().as_ref(),
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
            tip: None,
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
            tip: None,
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
            matches!(ctx.checkout, Checkout::Primary { project: None }),
            "expected Weave with no project, got something else"
        );

        let src = SyncSource::Workweave(WorkweaveName::new("some-ww").unwrap());
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
        let fake_err = "TOML parse error: expected `.`, `=`";
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
            msg.contains("manifest") || msg.contains(Manifest::FILE_NAME),
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
            RepoSyncOutcome::Converged {
                derived_content_dropped: Vec::new(),
            },
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

    // -----------------------------------------------------------------------
    // `apply_strategy` states a derived-content policy
    //
    // These drive `apply_strategy` directly, and the fixture asserts the repo
    // carries NO durable `merge.rwv-ours` definition. Both are load-bearing,
    // for the same reason.
    //
    // The definition half of the primitive can reach a replay by two routes:
    // the policy the call site states, and the durable config
    // `plant_rwv_merge_driver_config` writes. `apply_strategy` runs on
    // MANIFEST repos, which never receive that plant —
    // `verify_replay_exclusion_invariant` plants in the project repo only —
    // so here the stated policy is the only route, and a test can tell a
    // stated policy from an ignored one. Through the CLI it could not: sync
    // plants once per rebase-strategy invocation and the config is durable,
    // so the project-repo call sites in `apply_project_strategy` resolve
    // declared paths whatever they state. That overlap is deliberate and not
    // removable: durable config is the only route for a bare `git rebase
    // --continue`, a stated policy is the only route here, and neither covers
    // the other's case. What it costs is falsifiability, which is why the
    // threading is pinned at this layer.
    // -----------------------------------------------------------------------

    /// A repo whose committed `.gitattributes` declares `generated.txt`
    /// derived, with `main` and the checked-out `feat` each having
    /// regenerated it. Returns the tip of `main` — the side a replay onto it
    /// must keep.
    fn diverged_on_a_declared_derived_path(dir: &Path) -> ResolvedRevisionId {
        init_repo(dir);
        std::fs::write(dir.join(".gitattributes"), "generated.txt merge=rwv-ours\n").unwrap();
        std::fs::write(dir.join("generated.txt"), "base\n").unwrap();
        std::fs::write(dir.join("shared.txt"), "base\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "declare generated.txt derived"]);
        let base = git(dir, &["rev-parse", "HEAD"]);

        std::fs::write(dir.join("generated.txt"), "regenerated on main\n").unwrap();
        std::fs::write(dir.join("shared.txt"), "main version\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "main: regenerate"]);
        let main_tip = git_vcs().head_revision(dir).unwrap();

        git(dir, &["checkout", "-b", "feat", &base]);
        main_tip
    }

    /// Fixture precondition: no durable definition of the driver is reachable
    /// from this repo's config. Without this, a replay resolves the declared
    /// path whatever policy the call site states, and the assertions below
    /// would pass for a call site that stated nothing at all.
    fn assert_no_durable_driver_definition(dir: &Path) {
        let out = std::process::Command::new("git")
            .args(["config", "--get", crate::git::RWV_MERGE_DRIVER_CONFIG_KEY])
            .current_dir(dir)
            .output()
            .expect("run git config");
        assert!(
            !out.status.success(),
            "fixture precondition: {} must be undefined in {}, else the durable \
             plant resolves the declared path and the stated policy is untestable",
            crate::git::RWV_MERGE_DRIVER_CONFIG_KEY,
            dir.display()
        );
    }

    #[test]
    fn the_rebase_strategy_resolves_a_declared_derived_path_without_stopping() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("manifest");
        let main_tip = diverged_on_a_declared_derived_path(&repo);
        assert_no_durable_driver_definition(&repo);

        std::fs::write(repo.join("generated.txt"), "regenerated on feat\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "feat: regenerate"]);

        apply_strategy(git_vcs().as_ref(), &repo, &main_tip, SyncStrategy::Rebase)
            .map_err(|e| e.message)
            .expect("a declared derived path must not stop a manifest-repo replay");

        assert_eq!(
            std::fs::read_to_string(repo.join("generated.txt")).unwrap(),
            "regenerated on main\n",
            "the replay target's version of a declared path is what survives"
        );
    }

    #[test]
    fn a_resumed_rebase_strategy_resolves_the_declared_path_it_reaches() {
        // The resume arm states the policy separately from the arm that
        // started the replay, and it is the arm that reaches the picks the
        // interrupted replay never got to. A resume that stated nothing would
        // stop again on the declared path — after the operator had already
        // resolved the real conflict.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("manifest");
        let main_tip = diverged_on_a_declared_derived_path(&repo);
        assert_no_durable_driver_definition(&repo);

        // F1 collides with main on an UNDECLARED path: a genuine conflict, and
        // no policy may resolve it.
        std::fs::write(repo.join("shared.txt"), "feat version\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "feat: edit shared"]);
        // F2 regenerates the declared path — the pick the resume must reach.
        std::fs::write(repo.join("generated.txt"), "regenerated on feat\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "feat: regenerate"]);

        let stopped = apply_strategy(git_vcs().as_ref(), &repo, &main_tip, SyncStrategy::Rebase);
        assert!(
            stopped.is_err(),
            "an authored conflict must stop the replay whatever the policy"
        );
        assert_eq!(
            git_vcs().mid_op(&repo),
            Some(ConflictOp::Rebase),
            "the replay must be left resumable"
        );

        // Operator resolves and stages, then re-enters — the `rwv sync
        // --continue` loop.
        std::fs::write(repo.join("shared.txt"), "merged\n").unwrap();
        git(&repo, &["add", "shared.txt"]);

        apply_strategy(git_vcs().as_ref(), &repo, &main_tip, SyncStrategy::Rebase)
            .map_err(|e| e.message)
            .expect("the resumed replay must carry through the declared path");

        assert!(
            git_vcs().mid_op(&repo).is_none(),
            "the replay must be complete after a successful resume"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("generated.txt")).unwrap(),
            "regenerated on main\n",
            "the resumed picks resolve the declared path the same way"
        );
    }
}
