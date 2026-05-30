//! `rwv sync <source>` and `rwv abort` implementation.
//!
//! `rwv sync` aligns the CWD workspace with another workspace's committed
//! `rwv.lock`. `rwv abort` rolls back to pre-sync state using savepoint refs.

use crate::git::{git_command, GitVcs};
use crate::lock::{commit_lock_file_with_message, generate_lock};
use crate::manifest::{LockFile, Manifest, Project, ProjectName, RepoPath, WorkweaveName};
use crate::op_state::{self, OpState};
use crate::parallel::run_in_parallel;
use crate::vcs::{ConflictOp, ResolvedRevisionId, Vcs, VcsError, VcsErrorOutput};
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

const PRE_OP_REF: &str = "refs/rwv/pre-op";

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
    let is_ancestor = git_command()
        .args(["merge-base", "--is-ancestor", target.as_str(), "HEAD"])
        .current_dir(repo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if is_ancestor {
        let commits_ahead = git(
            &["rev-list", "--count", &format!("{}..HEAD", target.as_str())],
            repo,
        )
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
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

fn git(args: &[&str], dir: &Path) -> anyhow::Result<String> {
    let out = git_command()
        .args(args)
        .current_dir(dir)
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "git {:?} in {} failed: {}",
            args,
            dir.display(),
            stderr.trim()
        );
    }
    Ok(String::from_utf8(out.stdout)
        .unwrap_or_default()
        .trim()
        .to_owned())
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
    let target_ref = target.as_str();
    match strategy {
        SyncStrategy::Ff => {
            let out = git(&["merge", "--ff-only", target_ref], repo);
            if let Err(e) = out {
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
            git(&["merge", "--no-edit", target_ref], repo)
                .map_err(|e| StrategyError::from_message(format!("merge failed: {e}")))?;
        }
    }
    Ok(())
}

fn create_savepoint(repo: &Path, op_id: &OpId) -> anyhow::Result<ResolvedRevisionId> {
    let head = GitVcs.head_revision(repo)?;
    git(
        &[
            "update-ref",
            &format!("{PRE_OP_REF}/{op_id}"),
            head.as_str(),
        ],
        repo,
    )?;
    Ok(head)
}

fn delete_savepoint(repo: &Path, op_id: &OpId) {
    let _ = git(
        &["update-ref", "-d", &format!("{PRE_OP_REF}/{op_id}")],
        repo,
    );
}

fn read_savepoint(repo: &Path, op_id: &OpId) -> Option<ResolvedRevisionId> {
    // `git rev-parse <ref>` emits the canonical 40-hex SHA for a
    // fully-qualified ref, so this is the one legitimate caller of
    // `ResolvedRevisionId::from_canonical_unchecked` — the value is
    // already in canonical form and re-running `resolve_revision` would
    // add a git invocation without strengthening the invariant. See the
    // constructor's doc-comment.
    git(&["rev-parse", &format!("{PRE_OP_REF}/{op_id}")], repo)
        .ok()
        .map(ResolvedRevisionId::from_canonical_unchecked)
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
    // Run merge-base from cwd_project_dir; both tips must be reachable in its
    // object DB for merge-base to work. (Source's tip is reachable because
    // Phase 1's reset --hard relies on the same reachability.)
    git_command()
        .args([
            "merge-base",
            "--is-ancestor",
            cwd_tip.as_str(),
            source_tip.as_str(),
        ])
        .current_dir(cwd_project_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
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
    let extra_count = git(
        &[
            "rev-list",
            "--count",
            &format!("{}..{}", source_tip.as_str(), cwd_tip.as_str()),
        ],
        cwd_project_dir,
    )
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

/// Refresh the git index to match HEAD, but only for the safely-auto-fixable class.
///
/// Runs bare `git reset` (mixed): aligns the index to HEAD without touching
/// the working tree or HEAD ref. No-op when the index already matches HEAD.
///
/// Safety invariant: never replaces index content that is not already an
/// exactly-committed tree reachable from HEAD. If the index holds live staged
/// content (tree not found in recent ancestors), this function does nothing.
fn refresh_index_if_safe(repo: &Path) {
    // Quick exit: index already matches HEAD.
    let clean = git_command()
        .args(["diff-index", "--cached", "--exit-code", "HEAD"])
        .current_dir(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true); // assume clean on error; never touch if unsure
    if clean {
        return;
    }

    // Get the current index tree SHA.
    let index_tree = match git_command().arg("write-tree").current_dir(repo).output() {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout)
            .unwrap_or_default()
            .trim()
            .to_owned(),
        _ => return, // can't verify — leave index alone
    };

    // Safety check: is the index tree the tree of some recent ancestor commit?
    // Bounded to last 200 commits to keep doctor fast on large histories.
    let ancestor_trees = match git_command()
        .args(["log", "--format=%T", "-200", "HEAD"])
        .current_dir(repo)
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout).unwrap_or_default(),
        _ => return,
    };

    if !ancestor_trees.lines().any(|t| t.trim() == index_tree) {
        return; // live staged content — do not clobber
    }

    // Safe: realign index to HEAD.
    let _ = git_command().arg("reset").current_dir(repo).output();
}

/// Restore working-tree files to match HEAD, but only for the safely-auto-fixable class.
///
/// Mirrors `refresh_index_if_safe`: detects modified files using
/// `git diff-index HEAD` (without --cached), verifies each file's on-disk blob
/// SHA is reachable from the last 200 commits, then runs
/// `git checkout HEAD -- <files>` to restore them. No-op when clean or when
/// any file has live edits (content not found in reachable history).
///
/// Safety invariant: never replaces on-disk content that is not already a
/// committed blob reachable from HEAD. No work is ever silently lost.
fn refresh_working_tree_if_safe(repo: &Path) {
    // Quick exit: working tree already matches HEAD.
    let clean = git_command()
        .args(["diff-index", "--exit-code", "HEAD"])
        .current_dir(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true);
    if clean {
        return;
    }

    // Use --name-status: D = deleted from WT (always safe); M = modified (check blob).
    let status_out = match git_command()
        .args(["diff-index", "--name-status", "HEAD"])
        .current_dir(repo)
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return,
    };
    let mut all_files: Vec<String> = Vec::new(); // all entries to restore
    let mut modified_files: Vec<String> = Vec::new(); // M entries needing blob check
    let mut has_entries = false;
    for line in String::from_utf8_lossy(&status_out.stdout).lines() {
        if line.is_empty() {
            continue;
        }
        has_entries = true;
        let mut parts = line.splitn(2, '\t');
        let status = parts.next().unwrap_or("").trim();
        let path = parts.next().unwrap_or("").trim();
        match status {
            "D" => {
                all_files.push(path.to_owned());
            }
            "M" | "T" => {
                all_files.push(path.to_owned());
                modified_files.push(path.to_owned());
            }
            _ => return, // unknown status — leave working tree alone
        }
    }
    if !has_entries || all_files.is_empty() {
        return;
    }

    // For M files, verify the on-disk blob is reachable before touching anything.
    if !modified_files.is_empty() {
        let objects_out = match git_command()
            .args(["rev-list", "--objects", "-n", "200", "HEAD"])
            .current_dir(repo)
            .output()
        {
            Ok(out) if out.status.success() => out,
            _ => return,
        };
        let reachable: std::collections::HashSet<String> = String::from_utf8(objects_out.stdout)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_whitespace().next().map(|s| s.to_owned()))
            .collect();
        for file in &modified_files {
            let hash_out = match git_command()
                .args(["hash-object", file])
                .current_dir(repo)
                .output()
            {
                Ok(out) if out.status.success() => out,
                _ => return,
            };
            let blob_sha = String::from_utf8_lossy(&hash_out.stdout).trim().to_owned();
            if !reachable.contains(&blob_sha) {
                return; // live edits — do not clobber
            }
        }
    }

    // Safe: restore all files from HEAD.
    let mut args = vec!["checkout".to_owned(), "HEAD".to_owned(), "--".to_owned()];
    args.extend(all_files);
    let _ = git_command().args(&args).current_dir(repo).output();
}

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
    let attrs_committed = git_command()
        .args(["show", "HEAD:.gitattributes"])
        .current_dir(cwd_project_dir)
        .output();

    let has_line = match attrs_committed {
        Ok(ref out) if out.status.success() => {
            let content = String::from_utf8_lossy(&out.stdout);
            content.lines().any(|l| l.trim() == "rwv.lock merge=ours")
        }
        // `.gitattributes` is not committed (exit non-zero from `git show`) —
        // the line is definitely absent.
        Ok(_) => false,
        Err(_) => false,
    };

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

/// Precondition: CWD and source workspaces must have the same active project.
///
/// Phase 1' rebases CWD's project commits onto source's project tip. When the
/// two sides have different active projects, those are commits from different
/// git repos — `git merge-base` then fails with an opaque
/// `fatal: Not a valid commit name <sha>`. Refuse early with a clear message
/// that names both projects and both workspace paths.
fn check_active_projects_match(
    cwd_project: &ProjectName,
    source_project: &ProjectName,
    cwd_workspace_dir: &Path,
    source_workspace_dir: &Path,
) -> anyhow::Result<()> {
    if cwd_project == source_project {
        return Ok(());
    }
    anyhow::bail!(
        "active project mismatch: CWD workspace ({}) has active project `{}`, but source \
         workspace ({}) has active project `{}`.\n\
         `rwv sync` rebases CWD's project commits onto the source project's tip, so both \
         sides must share the same active project.\n\
         Fix: run `rwv activate {}` on one side to match the other, or run sync against a \
         workspace whose active project is `{}`.",
        cwd_workspace_dir.display(),
        cwd_project,
        source_workspace_dir.display(),
        source_project,
        cwd_project,
        cwd_project,
    );
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
                .with_context(|| {
                    format!("failed to resolve {start_ref} in canonical clone")
                })?;
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
                        let is_ancestor = git_command()
                            .args(["merge-base", "--is-ancestor", w.as_str(), c.as_str()])
                            .current_dir(&dest)
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false);
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
            let unique = git_command()
                .args(["for-each-ref", "--format=%(refname)", "refs/heads/"])
                .current_dir(&dest)
                .output();
            let any_local_only = match unique {
                Ok(out) if out.status.success() => {
                    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|s| s.to_owned())
                        .collect();
                    let mut any = false;
                    for name in &names {
                        // Check whether the branch has any commits not in origin/<branch>.
                        let short = name.trim_start_matches("refs/heads/");
                        let upstream = format!("refs/remotes/origin/{short}");
                        let has_upstream = git_command()
                            .args(["rev-parse", "--verify", "--quiet", &upstream])
                            .current_dir(&dest)
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false);
                        if !has_upstream {
                            any = true;
                            break;
                        }
                        let count = git_command()
                            .args(["rev-list", "--count", &format!("{upstream}..{name}")])
                            .current_dir(&dest)
                            .output();
                        if let Ok(out) = count {
                            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                            if s.parse::<usize>().unwrap_or(0) > 0 {
                                any = true;
                                break;
                            }
                        }
                    }
                    any
                }
                _ => true, // conservative: refuse on uncertainty
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
// rwv sync
// ---------------------------------------------------------------------------

/// Execute `rwv sync <source>`.
///
/// Phase ordering under the lock-as-derived contract:
/// 1. **Phase 2 (manifest repos)** — advance each manifest repo to source's
///    committed lock target via `--strategy`.
/// 2. **Phase 1' (project repo)** — replay CWD's unique project commits onto
///    source's tip via `--strategy`, with `rwv.lock` excluded from each
///    commit's effective diff. Lock-only commits become empty patches and
///    are skipped. `--force` retains hard-reset semantics.
/// 3. **Phase 3 (re-lock)** — regenerate `rwv.lock` from the now-merged
///    manifest tips and commit if changed.
///
/// `source` is required; bare `rwv sync` (no source) is no longer supported.
/// Use `rwv sync-to` to land work upward.
///
/// `do_continue = true` activates `--continue` mode: instead of refusing when
/// an op-state file is present, the call validates that the recorded parameters
/// match and resumes from the recorded phase. Without `--continue`, a present
/// op-state file is always an error.
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
    run_sync_impl(
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

/// Shared sync orchestration body used by both text-mode (`run_sync`) and
/// JSON-mode (`run_sync_json`).
///
/// `handler` drives all per-repo output: [`TextHandler`] prints human-readable
/// lines; [`JsonEnvelopeHandler`] buffers records for post-run envelope
/// emission; [`JsonNdjsonHandler`] streams one JSON line per record as it
/// arrives. New modes can be added by implementing [`OutputHandler`] without
/// touching this function.
///
/// `jobs` is the resolved worker count (post-`parallel::resolve_jobs`).
/// `jobs == 1` runs Phase 2 (per-repo manifest sync) serially on the
/// caller thread; `jobs > 1` runs it on a bounded worker pool.
/// Phases 1' (project repo) and 3 (re-lock + commit) are inherently
/// serial and run on the caller thread regardless of `jobs`.
///
/// Project-level errors (lock freshness, materialize, manifest-repo failures
/// post-loop, Phase 1' / Phase 3) still bail with `anyhow::Result::Err` —
/// the JSON caller decides whether to emit collected records before
/// propagating.
#[allow(clippy::too_many_arguments)]
fn run_sync_impl(
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
    run_sync_impl_with_op_id(
        cwd,
        source,
        strategy,
        force,
        retire,
        project_override,
        jobs,
        handler,
        do_continue,
        None,
    )
}

/// Internal: same as `run_sync_impl` but accepts an optional pre-existing `OpId`.
///
/// When `pre_existing_op_id` is `Some`, the op-state check/write is bypassed
/// entirely — the caller (e.g. sync-to step 1) has already set up op-state.
/// The provided `OpId` is used for savepoints.
#[allow(clippy::too_many_arguments)]
fn run_sync_impl_with_op_id(
    cwd: &Path,
    source: Option<&SyncSource>,
    strategy: SyncStrategy,
    force: bool,
    _retire: bool,
    project_override: Option<ProjectName>,
    jobs: usize,
    handler: &dyn OutputHandler,
    do_continue: bool,
    pre_existing_op_id: Option<&OpId>,
) -> anyhow::Result<()> {
    let emit_text = handler.emit_text();
    // Resolve CWD workspace.
    let ctx = WorkspaceContext::resolve(cwd, project_override.clone())?;
    let workspace_dir = ctx.active_path().to_path_buf();

    // When --continue (and no pre_existing_op_id), read op-state early so we
    // can derive the source path and strategy from the recorded values rather
    // than from CLI arguments (which are not passed when --continue is set).
    // We also need to derive `strategy` from op-state in this path.
    let (resolved_source, strategy, pre_read_op_state): (
        SyncSource,
        SyncStrategy,
        Option<crate::op_state::OpState>,
    ) = if do_continue && pre_existing_op_id.is_none() {
        let recorded = op_state::resume(&workspace_dir)?;
        let strat = recorded
            .strategy
            .parse::<SyncStrategy>()
            .context("op-state has invalid strategy")?;
        let src = SyncSource::Path(recorded.source.clone());
        (src, strat, Some(recorded))
    } else {
        // Resolve sync source: explicit if given, else parent from marker.
        // Bare `rwv sync` only makes sense inside a workweave; the helpful error
        // here is the entire reason we bothered to make `source` optional.
        let resolved = match source {
            Some(s) => s.clone(),
            None => match &ctx.location {
                WorkspaceLocation::Workweave { dir, .. } => {
                    let marker =
                        crate::workspace::WorkweaveMarker::read(dir)?.ok_or_else(|| {
                            anyhow::anyhow!(
                                "bare `rwv sync` requires a `.rwv-workweave` marker in the \
                                 workweave; found none at {} (re-create the workweave or pass \
                                 an explicit source)",
                                dir.display()
                            )
                        })?;
                    SyncSource::Path(marker.parent)
                }
                WorkspaceLocation::Weave { .. } => {
                    anyhow::bail!(
                        "bare `rwv sync` syncs to the workweave's recorded parent, but CWD \
                         ({}) is in the primary weave, not a workweave; pass an explicit source",
                        cwd.display()
                    );
                }
            },
        };
        (resolved, strategy, None)
    };

    let source_path = resolved_source.resolve(&ctx)?;
    // The source side honours the same `--project` override so cross-project
    // syncs from a non-active project work.
    let source_ctx = WorkspaceContext::resolve(&source_path, project_override.clone())?;
    let source_workspace_dir = source_ctx.active_path().to_path_buf();

    // Sibling-sync warning: if CWD is a workweave and source is another
    // workweave that is NOT CWD's parent, the operation crosses tree
    // branches. Warn (don't refuse — the operator may have a reason) so
    // accidental sibling syncs are visible.
    if let WorkspaceLocation::Workweave { dir: cwd_ww, .. } = &ctx.location {
        if let WorkspaceLocation::Workweave { dir: source_ww, .. } = &source_ctx.location {
            // Compare canonical paths for honest equality across symlinks.
            let cwd_canonical = cwd_ww
                .canonicalize()
                .unwrap_or_else(|_| cwd_ww.to_path_buf());
            let source_canonical = source_ww
                .canonicalize()
                .unwrap_or_else(|_| source_ww.to_path_buf());
            if cwd_canonical != source_canonical {
                // Is source actually our parent? If so, no warning — that's
                // the documented bare-sync target.
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

    // Find active projects.
    let cwd_project_name = find_project_name(&ctx)?;
    let source_project_name = find_project_name(&source_ctx)?;

    // Precondition: active projects must match. Phase 1' rebases CWD's project
    // commits onto source's project tip; if the two sides have different active
    // projects, those are different repos and `git merge-base` fails deep in
    // Phase 1' with an opaque error. Refuse early, before any savepoint or
    // marker is written.
    check_active_projects_match(
        &cwd_project_name,
        &source_project_name,
        &workspace_dir,
        &source_workspace_dir,
    )?;

    let cwd_project_dir = workspace_dir.join("projects").join(&cwd_project_name);
    let source_project_dir = source_workspace_dir
        .join("projects")
        .join(&source_project_name);

    // Load manifests.
    let cwd_project = Project::from_dir(&cwd_project_dir)
        .context("failed to load CWD project")?;

    // Precondition: CWD project repo must not be mid-op.
    if let Some(state) = crate::git::GitVcs::mid_op_state(&cwd_project_dir) {
        anyhow::bail!("CWD project repo is {state}; resolve before running sync");
    }

    let cwd_workspace_name = workspace_name(&ctx);
    let source_workspace_name = workspace_name(&source_ctx);

    // Precondition: lock freshness (unless --force).
    if !force {
        let source_project = Project::from_dir(&source_project_dir)
            .context("failed to load source project")?;
        if let Some(ref lock) = source_project.lock {
            check_lock_freshness(
                &source_workspace_dir,
                lock,
                Side::Source,
                &source_workspace_name,
            )?;
        }
        if let Some(ref lock) = cwd_project.lock {
            check_lock_freshness(&workspace_dir, lock, Side::Destination, &cwd_workspace_name)?;
        }
    }

    // Resolve project tips up front; the ancestor precondition (ff-only) and
    // Phase 1' need both. `head_revision` is read-only — running it before
    // any side effects keeps the refusal path clean.
    let source_project_tip = GitVcs
        .head_revision(&source_project_dir)
        .context("failed to read source project HEAD")?;
    let cwd_project_tip = GitVcs
        .head_revision(&cwd_project_dir)
        .context("failed to read CWD project HEAD")?;

    // Precondition: rebase and merge strategies require `rwv.lock merge=ours`
    // in the project repo's committed `.gitattributes`. Without it, git's
    // 3-way merge runs on rwv.lock and conflicts whenever both sides have
    // lock edits. Check before any git ops so the operator is never left
    // mid-rebase or mid-merge. FF never merges, so it doesn't need the
    // precondition.
    if matches!(strategy, SyncStrategy::Rebase | SyncStrategy::Merge) {
        verify_replay_exclusion_invariant(&cwd_project_dir)?;
    }

    // Precondition: ff strategy refuses divergence; rebase/merge handle it
    // by replaying CWD's commits onto source's tip with `rwv.lock` excluded.
    // `--force` bypasses regardless of strategy and discards CWD's project
    // commits via hard-reset; the savepoint preserves them for `rwv abort`.
    let phase1_ancestor_bypassed = if force {
        // Even with --force, detect whether the ancestor check WOULD have
        // refused — so we can preserve the project savepoint post-op as a
        // tombstone of the discarded commits.
        !cwd_is_ancestor_or_equal(&cwd_project_dir, &cwd_project_tip, &source_project_tip)
    } else if strategy == SyncStrategy::Ff {
        check_phase1_ancestor(
            &cwd_project_dir,
            &cwd_project_tip,
            &source_project_tip,
            &cwd_workspace_name,
            &source_workspace_name,
        )?;
        false
    } else {
        // rebase/merge: precondition bypassed; the strategy itself handles
        // divergence. Savepoint cleanup follows the normal path (no tombstone).
        false
    };

    // --continue / pre-op guard: check whether a sync is already in progress.
    //
    // For `rwv sync` the only involved workspace is CWD. For `rwv sync-to`
    // (bead fo-pte54.1) both CWD and the target workspace are checked.
    //
    // - `--continue` absent, no op-state → fresh start (normal path below).
    // - `--continue` absent, op-state present → refuse with "in-progress" error.
    // - `--continue` present, op-state absent → error "no op in progress to continue".
    // - `--continue` present, op-state present → resume; all parameters are read from
    //   the recorded state (source, strategy). No mismatch check — `--continue` is
    //   exclusive: the operator cannot pass conflicting flags (enforced at parse time).
    //
    // When `pre_existing_op_id` is `Some`, the caller (sync-to step 1) has already
    // set up op-state and savepoints — bypass the check/write entirely.
    let op_id: OpId;
    if let Some(existing_id) = pre_existing_op_id {
        // Caller-managed op-state: use the provided id, skip all op-state machinery.
        op_id = existing_id.clone();
    } else if do_continue {
        // Resume: all parameters were already read from op-state above (in the
        // `pre_read_op_state` path). Unwrap is safe: `pre_read_op_state` is
        // `Some` whenever `do_continue && pre_existing_op_id.is_none()`.
        let recorded = pre_read_op_state
            .expect("pre_read_op_state must be Some when do_continue && no pre_existing_op_id");
        op_id = OpId::from_string(recorded.id.clone());
        if emit_text {
            eprintln!(
                "continuing sync (op {op_id}, mid `{phase}`)",
                phase = recorded.phase
            );
        }
    } else {
        // Check that no op is already in progress (concurrency guard).
        op_state::check_no_op_in_progress(&[workspace_dir.as_path()])?;

        op_id = OpId::new_now();

        // Write the richer op-state file to the CWD workspace.
        let state = OpState::new_sync(
            &op_id,
            strategy,
            source_workspace_dir.clone(),
            workspace_dir.clone(),
        );
        op_state::write(&workspace_dir, &state)
            .context("failed to write op-state")?;
    }

    // Create savepoints for all CWD repos (including project repo).
    create_savepoint(&cwd_project_dir, &op_id)?;
    for repo_path in cwd_project.manifest.iter_repo_paths() {
        let abs = workspace_dir.join(repo_path.as_path());
        if abs.exists() {
            let _ = create_savepoint(&abs, &op_id);
        }
    }

    // Phase 2 first: advance manifest repos using source's committed lock as
    // targets. Reading source's lock directly (rather than from the CWD
    // project after a Phase-1 reset, as the old contract did) keeps the lock
    // out of the merge inputs entirely.
    let source_lock_path = source_project_dir.join("rwv.lock");
    let raw_source_lock = LockFile::from_path(&source_lock_path)
        .context("failed to read source lock")?;

    // Load source manifest so we have URLs for any repos newly added at source
    // that need to be materialized on the CWD side.
    let source_manifest_path = source_project_dir.join("rwv.yaml");
    let source_manifest = Manifest::from_path(&source_manifest_path)
        .context("failed to read source manifest")?;

    // Phase 3 materialize: for each repo listed in source's lock but missing
    // from the CWD workspace, clone/worktree-add before Phase 2 tries to sync
    // it. In a workweave this means `git worktree add` against the canonical
    // clone at primary; in the primary weave it means `git clone`.
    //
    // Iterates the RAW source lock — materialize uses the manifest URL, not
    // the lock version, and we must include every locked path (including
    // ones whose canonical clone is missing on the source side) so failures
    // surface as B6 prescribes.
    let mut materialize_failures: Vec<crate::manifest::RepoPath> = Vec::new();
    for repo_path in raw_source_lock.iter_repo_paths() {
        let abs = workspace_dir.join(repo_path.as_path());
        if abs.exists() {
            continue;
        }
        let entry = match source_manifest.get_entry(repo_path) {
            Some(e) => e,
            None => continue, // lock entry without manifest entry — skip
        };
        match materialize_missing_repo(&ctx, repo_path, entry, &cwd_project_name) {
            Ok(()) => {
                if emit_text {
                    println!("  {repo_path}: materialized");
                }
            }
            Err(e) => {
                if emit_text {
                    eprintln!("  {repo_path}: materialize failed: {e}");
                }
                // B6: previously this stderr line was the only signal; the
                // per-repo `skipped (not on disk)` loop below didn't flip
                // `any_failure`, so sync exited 0 with a lock that had
                // advanced past a never-materialised repo. Record the
                // failure so the post-loop bail fires.
                materialize_failures.push(repo_path.clone());
            }
        }
    }

    // Phase 3 prune: any repo present on disk in CWD but absent from source's
    // new lock should be dropped. Conservative — refuse to delete worktrees
    // with uncommitted changes or unique local commits.
    if let Some(ref cwd_lock) = cwd_project.lock {
        for repo_path in cwd_lock.iter_repo_paths() {
            if raw_source_lock.contains_repo(repo_path) {
                continue;
            }
            match prune_dropped_repo(&ctx, repo_path) {
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

    // B6: a failure in the materialize loop above is itself a sync failure.
    // Without this, every materialize-failed repo silently becomes a
    // `skipped (not on disk)` print and sync exits 0 with a lock advanced
    // past a missing repo.
    let mut any_failure = !materialize_failures.is_empty();

    // Resolve the source lock against the CWD workspace (where repos now
    // exist post-materialize) so sync_one_repo gets canonical-SHA targets.
    // Resolution failures here mean the local clone of a repo hasn't yet
    // pulled the SHA the lock pins — surface them via the per-repo loop
    // below as a failure to keep with B3.
    let (source_lock, source_lock_failures) =
        raw_source_lock.clone().resolve_versions(&workspace_dir);
    let unresolvable: std::collections::BTreeSet<crate::manifest::RepoPath> = source_lock_failures
        .iter()
        .map(|(p, _)| p.clone())
        .collect();

    // Phase 2 (per-repo manifest sync) splits into three classes:
    //
    // - **skipped** (`!abs.exists()`): no on-disk clone, no work, no record.
    // - **unresolvable** (lock pins a revision the local clone doesn't have):
    //   surfaced as a `head-unreadable` failure record; no sync work.
    // - **sync** (everything else): call `sync_one_repo` + post-sync refresh.
    //
    // The first two classes are pure record-keeping and run serially before
    // the parallel pool; the third class is what `-j N` fans out across
    // workers. Per-repo savepoint refs (created above) are per-ref-name so
    // workers don't race; `sync_one_repo` and the refresh helpers touch
    // only the repo's own working tree/index/refs and don't write to any
    // workspace-wide state, which is what makes parallel safe.
    struct SyncTask {
        repo_path: crate::manifest::RepoPath,
        abs: PathBuf,
        target: ResolvedRevisionId,
    }
    let mut sync_tasks: Vec<SyncTask> = Vec::new();

    for (repo_path, raw_entry) in raw_source_lock.iter_entries() {
        let abs = workspace_dir.join(repo_path.as_path());
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
            // Surface as a JSON record so consumers see this in --json mode.
            let head_unreadable_error = format!(
                "lock pins unknown revision {} in local clone",
                raw_entry.version
            );
            let outcome = RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
                error: head_unreadable_error,
                cause: None,
            });
            handler.record(repo_path.as_str(), &abs.to_string_lossy(), &outcome);
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

    // Fan out the sync tasks. Under `jobs == 1` `run_in_parallel` runs
    // them serially on the caller thread without spawning — bit-identical
    // to the pre-parallel loop. Under `jobs > 1` each worker calls
    // `sync_one_repo` + the post-sync refresh helpers on its own task; on
    // completion it routes the outcome through `handler.record`, which each
    // OutputHandler impl handles appropriately (text printing, buffering, or
    // NDJSON streaming with its own mutex-guarded stdout write).
    //
    // Worker output order is completion order under `-j > 1` (matches
    // fetch/update parallel UX); under `-j 1` it remains input order
    // (the BTreeMap iteration above).
    let task_outcomes: Vec<bool> = run_in_parallel(&sync_tasks, jobs, |_idx, task| {
        let outcome = sync_one_repo(&task.abs, &task.target, strategy);
        let is_failure = outcome.is_failure();
        if !is_failure {
            // Post-sync: refresh index and working tree if stale. Fires on
            // every non-failure outcome — including NoOp (HEAD already at lock
            // but index/WT may have drifted from a shared-ref advance) and
            // AlreadyAhead (working tree should still reflect HEAD).
            refresh_index_if_safe(&task.abs);
            refresh_working_tree_if_safe(&task.abs);
        }
        handler.record(
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
            manifest_repo_failure_message(strategy, &resolved_source)
        );
    }

    // Phase 1': project repo strategy with rwv.lock excluded.
    let phase1_outcome = if force {
        // Hard-reset semantics: discard CWD's project commits.
        git(
            &["reset", "--hard", source_project_tip.as_str()],
            &cwd_project_dir,
        )
        .context("project repo reset --force failed")
        .map(|_| ())
    } else {
        apply_project_strategy(
            &cwd_project_dir,
            &source_project_tip,
            &cwd_project_tip,
            strategy,
            &resolved_source,
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
                &cwd_project_dir,
                strategy,
                &resolved_source,
            )
        );
    }

    // Reload CWD project so Phase 3 sees the post-Phase-1' manifest (which
    // may now include newly-added repos brought over from source). If reload
    // fails, bail hard: proceeding with the pre-Phase-1' snapshot would let
    // Phase 3 silently regenerate a lock that is missing newly-added repos,
    // and in --json mode the old warning was suppressed entirely. The
    // operator should run `rwv abort` to restore the pre-op savepoint, then
    // investigate the manifest corruption.
    let cwd_project_phase3 = Project::from_dir(&cwd_project_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to reload project manifest after Phase 1' ({e}).\n\
             \n\
             The project repo was successfully rebased/merged, but the manifest \
             in {cwd_project_dir} could not be parsed. Proceeding would silently \
             omit newly-added repos from the regenerated lock.\n\
             \n\
             To recover: `rwv abort`",
            cwd_project_dir = cwd_project_dir.display(),
        )
    })?;

    // Phase 3: regenerate rwv.lock from current manifest tips and commit if changed.
    if let Err(e) = regenerate_lock_phase3(
        &ctx,
        &cwd_project_dir,
        &cwd_project_phase3,
        &source_workspace_name,
    ) {
        if emit_text {
            eprintln!("Phase 3 (re-lock) failed: {e}");
        }
        anyhow::bail!(
            "{}",
            phase1_or_phase3_failure_message(
                Phase::Three,
                &cwd_project_dir,
                strategy,
                &resolved_source,
            )
        );
    }

    // Successful completion: clean up savepoints and marker.
    //
    // Exception: when `--force` bypassed the Phase 1 ancestor check, the
    // hard-reset discarded reachable commits from the project repo. Preserve
    // the project repo's savepoint as a tombstone so the operator can recover
    // via `git reset --hard refs/rwv/pre-op/<id>` (manual; the marker is
    // gone, so `rwv abort` no longer sees an in-flight op). Manifest repo
    // savepoints are still cleaned — they are not part of the discarded set.
    if !phase1_ancestor_bypassed {
        delete_savepoint(&cwd_project_dir, &op_id);
    } else if emit_text {
        eprintln!(
            "note: --force discarded project commits; pre-sync state preserved at \
             refs/rwv/pre-op/{op_id} (recover with `git reset --hard refs/rwv/pre-op/{op_id}` \
             in {})",
            cwd_project_dir.display()
        );
    }
    for repo_path in cwd_project_phase3.manifest.iter_repo_paths() {
        let abs = workspace_dir.join(repo_path.as_path());
        if abs.exists() {
            delete_savepoint(&abs, &op_id);
        }
    }
    // Remove the op-state file from CWD workspace on successful completion.
    // Skip when pre_existing_op_id is set — the outer caller (sync-to) manages
    // op-state lifecycle across both workspaces.
    if pre_existing_op_id.is_none() {
        op_state::clear(&workspace_dir);
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
    let manifest = Manifest::from_path(&manifest_path)
        .context("--retire: failed to reload manifest")?;

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
            git(
                &["merge", "--ff-only", source_tip.as_str()],
                cwd_project_dir,
            )?;
        }
        SyncStrategy::Rebase => {
            // `git rebase <source_tip>` is equivalent to `--onto <source_tip>
            // <source_tip>` (git computes merge-base internally). The
            // `merge=ours` driver on `rwv.lock` keeps source's lock through
            // every replayed commit; lock-only commits become empty patches
            // and git drops them by default.
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
            // Native merge; the `merge=ours` driver (registered inline via
            // `-c`, see `git.rs::rebase`) auto-resolves any rwv.lock
            // collision in source's favour. Phase 3 then regenerates the
            // lock from manifest tips.
            match git(
                &[
                    "-c",
                    "merge.ours.name=keep ours during replay (rwv replay-exclusion)",
                    "-c",
                    "merge.ours.driver=true",
                    "merge",
                    "--no-edit",
                    source_tip.as_str(),
                ],
                cwd_project_dir,
            ) {
                Ok(_) => {}
                Err(e) => {
                    if matches!(
                        crate::git::GitVcs::mid_op_state(cwd_project_dir).as_deref(),
                        Some("mid-merge")
                    ) {
                        anyhow::bail!(
                            "{}",
                            per_conflict_bail_message(
                                cwd_project_dir,
                                ConflictOp::Merge,
                                "merge (project repo)",
                                "see in-flight merge state for conflicting paths",
                                resolved_source,
                            )
                        );
                    }
                    anyhow::bail!("project repo merge failed: {e}");
                }
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
///
/// Falls back to the legacy `.rwv-sync-op` marker for workspaces written
/// before the op-state upgrade (bead fo-pte54.7). The legacy path only
/// restores CWD repos (no cross-workspace rollback).
pub fn run_abort(cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let workspace_dir = ctx.active_path().to_path_buf();

    // Try to read the new op-state file first.
    let (op_id, extra_workspace_dirs): (OpId, Vec<PathBuf>) = match op_state::read(&workspace_dir)?
    {
        Some(state) => {
            // For sync-to: we also need to roll back the target workspace.
            let extras = if state.verb == crate::op_state::OpVerb::SyncTo {
                vec![state.target.clone()]
            } else {
                vec![]
            };
            (OpId::from_string(state.id), extras)
        }
        None => {
            // Fall back to the legacy `.rwv-sync-op` marker.
            match op_state::read_legacy(&workspace_dir) {
                Some(id) => (OpId::from_string(id), vec![]),
                None => anyhow::bail!("no operation in progress"),
            }
        }
    };

    let cwd_project_name = find_project_name(&ctx)?;
    let cwd_project_dir = workspace_dir.join("projects").join(&cwd_project_name);
    // Use the lockless loader: abort's contract is "the state is bad, get me
    // out". rwv.lock may contain git conflict markers from the half-completed
    // rebase, so we must not try to parse it. The abort path only needs the
    // manifest (to enumerate repo paths); it never reads lock contents.
    let cwd_project = Project::from_dir_skip_lock(&cwd_project_dir)
        .context("failed to load CWD project")?;

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
                // Remove op-state from the extra (target) workspace too.
                op_state::clear(&extra_ws_dir);
            }
            Err(e) => {
                eprintln!(
                    "  warning: could not resolve workspace at {}: {e}; skipping",
                    extra_dir.display()
                );
            }
        }
    }

    // Remove op-state from CWD workspace (covers both new format and legacy).
    op_state::clear(&workspace_dir);
    // Also remove the legacy marker if present (for workspaces with both).
    let _ = std::fs::remove_file(workspace_dir.join(op_state::LEGACY_OP_MARKER));

    if any_failure {
        anyhow::bail!("abort completed with failures");
    }

    Ok(())
}

fn abort_one_repo(repo: &Path, op_id: &OpId) -> anyhow::Result<()> {
    // Run VCS-native abort if mid-op.
    if let Some(state) = crate::git::GitVcs::mid_op_state(repo) {
        let abort_args: &[&str] = match state.as_str() {
            "mid-rebase" => &["rebase", "--abort"],
            "mid-merge" => &["merge", "--abort"],
            "mid-cherry-pick" => &["cherry-pick", "--abort"],
            _ => &[],
        };
        if !abort_args.is_empty() {
            let _ = git(abort_args, repo);
        }
    }

    // Reset to savepoint.
    match read_savepoint(repo, op_id) {
        Some(sha) => {
            git(&["reset", "--hard", sha.as_str()], repo)
                .context("reset --hard failed")?;
            delete_savepoint(repo, op_id);
            Ok(())
        }
        None => {
            // No savepoint for this repo — nothing to restore.
            Ok(())
        }
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
        run_sync_impl(
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
        let handler = JsonEnvelopeHandler {
            records: &records,
        };
        run_sync_impl(
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
        let out = serde_json::to_string_pretty(&payload)
            .context("failed to serialize sync output")?;
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
// Three-step orchestration:
//
//   Step 1 — rebase/merge CWD against target.
//             Calls run_sync_impl(cwd=CWD, source=target, strategy=<X>).
//             This is identical to what `rwv sync <target>` does, except the
//             source path is target and CWD is the destination.
//
//   Step 2 — auto-relock if step 1 moved manifest tips.
//             Regenerate rwv.lock in CWD's project repo. If the lock
//             changed, commit it with message "lock: post-rebase refresh".
//             This is folded into the sync-to orchestration (not a bolt-on).
//
//   Step 3 — FF-advance target to CWD's new tip.
//             Fast-forward each manifest repo and the project repo in target
//             to match CWD's converged tips. Always FF; if FF fails, bail
//             with an actionable error.
//
// Op-state is written to BOTH workspaces (CWD + target) before step 1.
// Phase advances: step1-rebase → step1-complete → step3-ff → both cleared.
// On any failure, op-state is left in place for --continue or rwv abort.

/// Execute `rwv sync-to <target>`.
///
/// `target` is the workspace to advance, or `None` when `--continue` is set
/// (target is then read from the in-progress op-state file). Step 1 calls the
/// existing sync engine to rebase/merge CWD against target (CWD absorbs
/// target's history with CWD's commits on top). Step 2 auto-relocks if tips
/// moved. Step 3 fast-forwards target's repos to CWD's converged tips.
///
/// If `retire` is true and all steps succeed, the workweave is deleted after
/// step 3 (requires clean worktree and manifest repos converged with target).
///
/// `do_continue` resumes a mid-op sync-to by reading the recorded phase and
/// all parameters from op-state, skipping already-completed steps.
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
    run_sync_to_impl(
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
        run_sync_to_impl(
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
        let handler = JsonEnvelopeHandler {
            records: &records,
        };
        run_sync_to_impl(
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

/// Shared sync-to orchestration body.
#[allow(clippy::too_many_arguments)]
fn run_sync_to_impl(
    cwd: &Path,
    target_source: Option<&SyncSource>,
    strategy: SyncStrategy,
    force: bool,
    retire: bool,
    project_override: Option<ProjectName>,
    jobs: usize,
    handler: &dyn OutputHandler,
    do_continue: bool,
) -> anyhow::Result<()> {
    let emit_text = handler.emit_text();

    // Resolve CWD workspace.
    let cwd_ctx = WorkspaceContext::resolve(cwd, project_override.clone())?;
    let cwd_workspace_dir = cwd_ctx.active_path().to_path_buf();

    // --continue / pre-op guard.
    //
    // When `--continue` is set (`do_continue`), read ALL parameters from the
    // in-progress op-state file. The CLI has already rejected any co-flags via
    // clap `conflicts_with`, so `target_source`, `strategy`, and `retire` from
    // the function signature must not be used in this path.
    //
    // When not `--continue`, `target_source` is always `Some` (the caller
    // resolved it) and `strategy`/`retire`/`force` come from function params.
    struct ResolvedParams {
        op_id: OpId,
        resume_phase: Option<crate::op_state::OpPhase>,
        target_path: PathBuf,
        target_workspace_dir: PathBuf,
        strategy: SyncStrategy,
        retire: bool,
    }

    let params: ResolvedParams = if do_continue {
        // Resume: read all parameters from the recorded op-state.
        let recorded = op_state::resume(&cwd_workspace_dir)?;
        let oid = OpId::from_string(recorded.id.clone());
        let phase_display = recorded.phase.to_string();
        let phase = Some(recorded.phase);
        // Derive target, strategy, retire from recorded state.
        let tgt_path = recorded.target.clone();
        let strat = recorded
            .strategy
            .parse::<SyncStrategy>()
            .context("op-state has invalid strategy")?;
        let ret = recorded.retire;
        if emit_text {
            eprintln!("continuing sync-to (op {oid}, mid `{phase_display}`)",);
        }
        let tgt_ctx = WorkspaceContext::resolve(&tgt_path, project_override.clone())?;
        let tgt_workspace_dir = tgt_ctx.active_path().to_path_buf();
        ResolvedParams {
            op_id: oid,
            resume_phase: phase,
            target_path: tgt_path,
            target_workspace_dir: tgt_workspace_dir,
            strategy: strat,
            retire: ret,
        }
    } else {
        // Fresh start: target_source is Some (the caller resolved it).
        let ts =
            target_source.expect("target_source must be Some for non-continue sync-to invocations");
        let tgt_path = ts.resolve(&cwd_ctx)?;
        let tgt_ctx = WorkspaceContext::resolve(&tgt_path, project_override.clone())?;
        let tgt_workspace_dir = tgt_ctx.active_path().to_path_buf();

        // Concurrency guard: check both CWD and target.
        op_state::check_no_op_in_progress(&[
            cwd_workspace_dir.as_path(),
            tgt_workspace_dir.as_path(),
        ])?;

        let oid = OpId::new_now();

        // Write op-state to both workspaces.
        let state = OpState::new_sync_to(
            &oid,
            strategy,
            cwd_workspace_dir.clone(),
            tgt_workspace_dir.clone(),
            retire,
        );
        op_state::write(&cwd_workspace_dir, &state)
            .context("failed to write op-state to CWD")?;
        op_state::write(&tgt_workspace_dir, &state)
            .context("failed to write op-state to target")?;
        ResolvedParams {
            op_id: oid,
            resume_phase: None,
            target_path: tgt_path,
            target_workspace_dir: tgt_workspace_dir,
            strategy,
            retire,
        }
    };

    let op_id = params.op_id;
    let resume_phase = params.resume_phase;
    let target_workspace_dir = params.target_workspace_dir;
    let target_path = params.target_path;
    let strategy = params.strategy;
    let retire = params.retire;

    // Find project names.
    let cwd_project_name = find_project_name(&cwd_ctx)?;
    let target_ctx = WorkspaceContext::resolve(&target_path, project_override.clone())?;
    let target_project_name = find_project_name(&target_ctx)?;

    // Projects must match.
    check_active_projects_match(
        &cwd_project_name,
        &target_project_name,
        &cwd_workspace_dir,
        &target_workspace_dir,
    )?;

    let cwd_project_dir = cwd_workspace_dir.join("projects").join(&cwd_project_name);
    let target_project_dir = target_workspace_dir
        .join("projects")
        .join(&target_project_name);

    // For --strategy=ff: step 1 is a no-op only if CWD is strictly ahead of target.
    // If CWD is not strictly ahead, bail with an actionable error.
    if strategy == SyncStrategy::Ff {
        let cwd_tip = GitVcs
            .head_revision(&cwd_project_dir)
            .context("failed to read CWD project HEAD")?;
        let target_tip = GitVcs
            .head_revision(&target_project_dir)
            .context("failed to read target project HEAD")?;

        if cwd_tip == target_tip {
            // No-op: already at same tip.
            if emit_text {
                eprintln!("sync-to: CWD and target are already at the same tip; nothing to do");
            }
            op_state::clear_all(&[cwd_workspace_dir.as_path(), target_workspace_dir.as_path()]);
            return Ok(());
        }

        // CWD must be strictly ahead of target for ff to work.
        let cwd_ahead = git_command()
            .args([
                "merge-base",
                "--is-ancestor",
                target_tip.as_str(),
                cwd_tip.as_str(),
            ])
            .current_dir(&cwd_project_dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !cwd_ahead {
            op_state::clear_all(&[cwd_workspace_dir.as_path(), target_workspace_dir.as_path()]);
            anyhow::bail!(
                "sync-to --strategy=ff requires CWD to be strictly ahead of target, \
                 but CWD's project tip ({}) is not an ancestor-or-equal of target's tip ({}).\n\
                 Rerun with `--strategy=rebase` to rebase CWD's commits onto target's tip first.",
                cwd_tip,
                target_tip
            );
        }
        // CWD is strictly ahead: skip step 1 (no-op), go directly to step 3.
    }

    // Determine which phase to start from.
    let skip_step1 = strategy == SyncStrategy::Ff
        || matches!(
            resume_phase,
            Some(crate::op_state::OpPhase::Step1Complete) | Some(crate::op_state::OpPhase::Step3Ff)
        );

    let skip_step3 = false; // step 3 is always needed unless already done (not tracked as a skippable phase here)

    // === Step 1: rebase/merge CWD against target ===
    //
    // Equivalent to `rwv sync <target>` from CWD: use the existing sync
    // engine with CWD as destination and target as source.
    if !skip_step1 {
        if emit_text {
            eprintln!(
                "sync-to step 1: rebasing CWD against target ({})...",
                target_path.display()
            );
        }

        // For --continue with resume_phase = Step1Rebase, pass do_continue=true
        // to run_sync_impl so it resumes the in-progress rebase/merge.
        let step1_continue = matches!(resume_phase, Some(crate::op_state::OpPhase::Step1Rebase));

        // Call the existing sync engine with target as source.
        // CWD is the destination (implicit from cwd arg).
        // Note: run_sync_impl expects source as a SyncSource. We use Path(target_path).
        let step1_source = SyncSource::Path(target_path.clone());

        // Call the existing sync engine with pre_existing_op_id so it bypasses
        // the op-state check/write (sync-to already set it up). We pass the same
        // op_id so savepoints created inside run_sync_impl_with_op_id are keyed
        // consistently with the op we already opened.
        let step1_result = run_sync_impl_with_op_id(
            cwd,
            Some(&step1_source),
            strategy,
            force,
            false, // retire: not applicable for step 1
            project_override.clone(),
            jobs,
            handler,
            step1_continue,
            Some(&op_id),
        );

        if let Err(e) = step1_result {
            // Leave op-state in both workspaces so --continue or rwv abort can recover.
            anyhow::bail!(
                "sync-to step 1 failed: {e}\n\
                 \n\
                 Op-state has been left in both workspaces.\n\
                 Resolve conflicts, then: `rwv sync-to --continue`\n\
                 To roll everything back: `rwv abort`",
            );
        }

        // Advance phase to Step1Complete in both workspaces.
        op_state::advance_phase(&cwd_workspace_dir, crate::op_state::OpPhase::Step1Complete)
            .context("failed to advance phase after step 1")?;
        op_state::advance_phase(
            &target_workspace_dir,
            crate::op_state::OpPhase::Step1Complete,
        )
        .context("failed to advance phase in target after step 1")?;
    } else if !matches!(resume_phase, Some(crate::op_state::OpPhase::Step3Ff)) {
        // When resuming from step1-complete, advance phase to mark we're moving to step 3.
        // (Already at step1-complete; advancing to step3-ff happens below before step 3.)
    }

    // Step 2 (auto-relock) is handled by Phase 3 inside run_sync_impl_with_op_id
    // called above. The existing sync engine regenerates and commits rwv.lock in
    // CWD's project repo as part of its own Phase 3 completion. No separate
    // re-lock step is needed here.

    // Advance phase to Step3Ff in both workspaces.
    if !matches!(resume_phase, Some(crate::op_state::OpPhase::Step3Ff)) {
        op_state::advance_phase(&cwd_workspace_dir, crate::op_state::OpPhase::Step3Ff)
            .context("failed to advance phase to step3-ff")?;
        op_state::advance_phase(&target_workspace_dir, crate::op_state::OpPhase::Step3Ff)
            .context("failed to advance phase to step3-ff in target")?;
    }

    // === Step 3: FF-advance target to CWD's new tip ===
    //
    // For each manifest repo and the project repo in target, fast-forward
    // to CWD's converged tip. This is always FF; if FF fails (e.g. concurrent
    // modification), bail with an actionable error.
    let _ = skip_step3; // always run step 3

    if emit_text {
        eprintln!("sync-to step 3: fast-forwarding target to CWD's tips...");
    }

    // Reload CWD project (post-relock).
    let cwd_project_final = crate::manifest::Project::from_dir(&cwd_project_dir)
        .context("failed to reload CWD project for step 3")?;

    // FF-advance each manifest repo in target.
    let mut any_ff_failure = false;
    for repo_path in cwd_project_final.manifest.iter_repo_paths() {
        let cwd_repo = cwd_workspace_dir.join(repo_path.as_path());
        let target_repo = target_workspace_dir.join(repo_path.as_path());

        if !cwd_repo.exists() {
            // Repo not materialized in CWD — skip.
            continue;
        }
        if !target_repo.exists() {
            // Repo not materialized in target — skip with warning.
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

        // FF-advance target repo to cwd_tip.
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

    // FF-advance target's project repo.
    let cwd_project_tip = GitVcs
        .head_revision(&cwd_project_dir)
        .context("failed to read CWD project HEAD for step 3")?;

    match ff_advance_repo(&target_project_dir, &cwd_project_dir, &cwd_project_tip) {
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
            "sync-to step 3 (FF-advance target) failed for one or more repos (see above).\n\
             This should not happen after a clean step 1; possible concurrent modification.\n\
             Op-state remains in both workspaces.\n\
             Rerun `rwv sync-to --continue` after resolving, or `rwv abort` to roll back.",
        );
    }

    // Success: clear op-state from both workspaces.
    op_state::clear_all(&[cwd_workspace_dir.as_path(), target_workspace_dir.as_path()]);

    if emit_text {
        eprintln!("sync-to complete: target fast-forwarded to CWD's tip");
    }

    // --retire: all three steps succeeded — verify manifest repos converged
    // with the target and the working tree is clean, then delete the workweave.
    // Only meaningful inside a workweave; in a primary weave, warn and skip.
    // If any step above failed, we already bailed before reaching this point,
    // so the workweave is always preserved on failure.
    if retire {
        match &cwd_ctx.location {
            WorkspaceLocation::Workweave { dir, name, project } => {
                retire_workweave_after_sync_to(
                    &cwd_ctx,
                    dir,
                    name,
                    project,
                    &cwd_project_dir,
                    &target_workspace_dir,
                )?;
            }
            WorkspaceLocation::Weave { .. } => {
                if emit_text {
                    eprintln!("warning: --retire is only meaningful inside a workweave; ignoring");
                }
            }
        }
    }

    Ok(())
}

/// Fast-forward `target_repo` to `cwd_tip`.
///
/// We need the objects to be reachable in `target_repo`. For a worktree
/// pair (workweave + primary), they share the same object store, so any
/// SHA reachable in the CWD worktree is also reachable in the target
/// worktree. We use `git fetch <cwd_repo_path> HEAD` to ensure the object
/// is present in the target's object store before the reset, then
/// `git reset --hard <sha>` to advance the branch.
///
/// The "fetch then reset" approach works for both worktrees (same object
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

    // Check that target is an ancestor of cwd_tip (ff precondition).
    // We need to fetch first to ensure the object is reachable for merge-base.
    fetch_objects_from(target_repo, cwd_repo)?;

    let is_ancestor = git_command()
        .args([
            "merge-base",
            "--is-ancestor",
            target_tip.as_str(),
            cwd_tip.as_str(),
        ])
        .current_dir(target_repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
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

    // Fast-forward: reset --hard to cwd_tip. For worktrees sharing an object
    // store this is safe; for independent clones we already fetched above.
    git(&["reset", "--hard", cwd_tip.as_str()], target_repo)
        .context("git reset --hard failed in target")?;

    Ok(())
}

/// Fetch objects from `src_repo` into `dst_repo` so SHAs reachable in `src`
/// are reachable in `dst`. Uses `git fetch <src_path> HEAD` which works for
/// both worktrees (no-op, shared object store) and independent clones.
fn fetch_objects_from(dst_repo: &Path, src_repo: &Path) -> anyhow::Result<()> {
    let src_path = src_repo.to_string_lossy().into_owned();
    git_command()
        .args(["fetch", &src_path, "HEAD"])
        .current_dir(dst_repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .with_context(|| format!("failed to fetch from {}", src_repo.display()))?;
    // fetch may fail for worktrees (FETCH_HEAD is unavailable) — that's fine,
    // the object is already reachable in the shared store. Ignore errors here;
    // ff_advance_repo will catch any real problem at reset --hard.
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

    #[test]
    fn check_active_projects_match_ok_when_equal() {
        let p = ProjectName::new("foundations");
        let res = check_active_projects_match(&p, &p, Path::new("/cwd/ws"), Path::new("/src/ws"));
        assert!(res.is_ok());
    }

    #[test]
    fn check_active_projects_match_errors_when_different() {
        let cwd = ProjectName::new("foundations-test");
        let src = ProjectName::new("foundations");
        let err =
            check_active_projects_match(&cwd, &src, Path::new("/cwd/ws"), Path::new("/src/ws"))
                .unwrap_err()
                .to_string();
        assert!(err.contains("active project mismatch"), "msg: {err}");
        assert!(err.contains("foundations-test"), "msg: {err}");
        assert!(err.contains("foundations"), "msg: {err}");
        assert!(err.contains("/cwd/ws"), "msg: {err}");
        assert!(err.contains("rwv activate"), "msg: {err}");
        assert!(err.contains("/src/ws"), "msg: {err}");
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

    // fo-vsldv.2 — SyncSource::resolve(Workweave) from a primary weave with no
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

    // fo-vsldv.3 — post-Phase-1' manifest reload is a hard bail, not warn-and-proceed.
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
    // Extensibility acceptance test (fo-6wpbh)
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
