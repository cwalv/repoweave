//! `rwv sync <source>` and `rwv abort` implementation.
//!
//! `rwv sync` aligns the CWD workspace with another workspace's committed
//! `rwv.lock`. `rwv abort` rolls back to pre-sync state using savepoint refs.

use crate::git::{git_command, GitVcs};
use crate::lock::{commit_lock_file_with_message, generate_lock};
use crate::manifest::{LockFile, Manifest, Project, ProjectName, RepoPath, WorkweaveName};
use crate::parallel::run_in_parallel;
use crate::vcs::{ConflictOp, ResolvedRevisionId, Vcs, VcsError, VcsErrorOutput};
use crate::workspace::{read_active_project, WorkspaceContext, WorkspaceLocation};
use crate::workweave::workweave_path_for;
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

const SYNC_OP_MARKER: &str = ".rwv-sync-op";
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
    pub fn resolve(&self, ctx: &WorkspaceContext) -> PathBuf {
        match self {
            Self::Primary => ctx.primary_path().to_path_buf(),
            Self::Workweave(name) => {
                // Resolve the project from the current context: the workweave
                // we're syncing FROM is assumed to belong to the same project
                // as the workspace we're syncing INTO (sync is per-project).
                // Fall back to primary's `.rwv-active` when CWD is the weave.
                let project = match &ctx.location {
                    WorkspaceLocation::Workweave { project, .. } => project.clone(),
                    WorkspaceLocation::Weave { project } => project
                        .clone()
                        .or_else(|| read_active_project(ctx.primary_path()))
                        .unwrap_or_else(|| crate::manifest::ProjectName::new("")),
                };
                workweave_path_for(ctx.primary_path(), &project, name)
            }
            Self::Path(p) => {
                if p.is_absolute() {
                    p.clone()
                } else {
                    ctx.primary_path().join(p)
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
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SyncFailureOutput {
    HeadUnreadable {
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<VcsErrorOutput>,
    },
    #[serde(rename = "ff-impossible")]
    FastForwardImpossible {
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<VcsErrorOutput>,
    },
    RebaseFailed {
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<VcsErrorOutput>,
    },
    MergeFailed {
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<VcsErrorOutput>,
    },
}

impl From<&SyncFailure> for SyncFailureOutput {
    fn from(f: &SyncFailure) -> Self {
        let error = f.error().to_owned();
        let cause = f.cause().map(VcsErrorOutput::from);
        match f {
            SyncFailure::HeadUnreadable { .. } => Self::HeadUnreadable { error, cause },
            SyncFailure::FastForwardImpossible { .. } => {
                Self::FastForwardImpossible { error, cause }
            }
            SyncFailure::RebaseFailed { .. } => Self::RebaseFailed { error, cause },
            SyncFailure::MergeFailed { .. } => Self::MergeFailed { error, cause },
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

/// One NDJSON record emitted by `rwv sync --json -j N` with `N > 1`.
///
/// Under NDJSON streaming mode, the envelope wrapper is dropped and each
/// per-repo outcome becomes its own self-describing line. Per the fo-tn9uk
/// epic convention ("every record embeds `$schema`"), every NDJSON record
/// carries its own schema URL so consumers can identify a line without
/// out-of-band context.
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
        .map_err(|e| anyhow::anyhow!("failed to run git: {e}"))?;
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

    for (repo_path, lock_entry) in &resolved.repositories {
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
// Conflict-bail messages — see fo-54gz8
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

fn find_project_name(ctx: &WorkspaceContext) -> anyhow::Result<ProjectName> {
    match &ctx.location {
        WorkspaceLocation::Weave { project: Some(p) } => Ok(p.clone()),
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
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", parent.display()))?;
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
                .map_err(|e| {
                    anyhow::anyhow!("failed to resolve {start_ref} in canonical clone: {e}")
                })?;
            let branch = crate::vcs::RefName::new(format!(
                "{}--{}/{}",
                project_name.as_str(),
                name.as_str(),
                start_ref,
            ));
            GitVcs
                .create_worktree(&canonical, &dest, &branch, &head_rev)
                .map_err(|e| anyhow::anyhow!("worktree add for {repo_path} failed: {e}"))?;
        }
        WorkspaceLocation::Weave { .. } => {
            GitVcs
                .clone_repo(&entry.url.to_string(), &dest)
                .map_err(|e| {
                    anyhow::anyhow!("clone of {repo_path} from {} failed: {e}", entry.url)
                })?;
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
                    .map_err(|e| anyhow::anyhow!("worktree remove for {repo_path} failed: {e}"))?;
                let _ = GitVcs.worktree_prune(&canonical);
            } else {
                // No canonical to compare to; remove the directory as a best effort.
                std::fs::remove_dir_all(&dest)
                    .map_err(|e| anyhow::anyhow!("failed to remove {}: {e}", dest.display()))?;
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
                .map_err(|e| anyhow::anyhow!("failed to remove {}: {e}", dest.display()))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Output sink: routes per-repo text chatter + structured records.
// ---------------------------------------------------------------------------

/// Output mode for sync orchestration.
///
/// - `Text`: per-repo `println!` / `eprintln!` lines for human consumption;
///   no JSON emission. Used by `rwv sync` (text mode).
/// - `JsonEnvelope`: suppress text chatter; collect records into the sink's
///   `records` Vec. The caller (`run_sync_json`) emits the
///   `{ "$schema": ..., "outcomes": [...] }` envelope after orchestration
///   returns. Used by `rwv sync --json` under `-j 1` (or unspecified).
/// - `JsonNdjson`: suppress text chatter; collect records AND stream each
///   one as a JSON line on stdout the moment it's recorded. Used by
///   `rwv sync --json -j N` with `N > 1`. Streamed lines are guarded by
///   `stdout_lock` so two workers can't tear a single line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Text,
    JsonEnvelope,
    JsonNdjson,
}

/// Shared output sink threaded through `run_sync_impl` (and its
/// per-repo workers under `-j > 1`).
///
/// `records` is `Mutex<Vec<_>>` so concurrent workers can push outcomes
/// atomically. `stdout_lock` serialises raw stdout writes (NDJSON lines)
/// so two workers don't tear each other's output. Under text mode the
/// stdout_lock is unused — text mode is always serial today.
struct OutputSink<'a> {
    mode: OutputMode,
    stdout_lock: &'a Mutex<()>,
    records: &'a Mutex<Vec<SyncOutcomeOutput>>,
}

impl OutputSink<'_> {
    fn emit_text(&self) -> bool {
        self.mode == OutputMode::Text
    }

    /// Record a per-repo outcome.
    ///
    /// Always pushes onto `records` (consumers — `run_sync_json` for the
    /// envelope, `run_sync` for an unused but accurately-sized `any_failure`
    /// check). Under `JsonNdjson` additionally writes one self-describing
    /// JSON line to stdout, taking `stdout_lock` so concurrent workers
    /// can't interleave bytes.
    fn record(&self, outcome: SyncOutcomeOutput) {
        if matches!(self.mode, OutputMode::JsonNdjson) {
            let record = SyncOutcomeNdjsonRecord {
                schema: SYNC_JSON_SCHEMA_URL,
                outcome: &outcome,
            };
            // Best-effort: a serialization failure here would mean the
            // outcome type itself is malformed; we'd still want to retain
            // the record in `records` (the post-loop bail message uses it),
            // so swallow and continue.
            if let Ok(line) = serde_json::to_string(&record) {
                let _guard = self.stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
                let stdout = std::io::stdout();
                let mut handle = stdout.lock();
                let _ = writeln!(handle, "{line}");
                let _ = handle.flush();
            }
        }
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(outcome);
    }
}

// ---------------------------------------------------------------------------
// rwv sync
// ---------------------------------------------------------------------------

/// Execute `rwv sync [source] [--retire]`.
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
/// `source = None` means sync to the workweave's recorded parent (one hop).
/// Only valid when CWD is inside a workweave with a `parent` field in its
/// `.rwv-workweave` marker (backfilled to `primary` for legacy markers).
///
/// `retire = true` deletes the workweave on a successful sync, provided the
/// workweave's project tip equals the parent's and the working tree is clean
/// (the [`crate::workweave::collect_dirty_paths`] check). Conflicts leave the
/// workweave intact for the operator to fix and re-run.
pub fn run_sync(
    cwd: &Path,
    source: Option<&SyncSource>,
    strategy: SyncStrategy,
    force: bool,
    retire: bool,
    project_override: Option<ProjectName>,
    jobs: usize,
) -> anyhow::Result<()> {
    let records: Mutex<Vec<SyncOutcomeOutput>> = Mutex::new(Vec::new());
    let stdout_lock: Mutex<()> = Mutex::new(());
    let sink = OutputSink {
        mode: OutputMode::Text,
        stdout_lock: &stdout_lock,
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
        &sink,
    )
}

/// Shared sync orchestration body used by both text-mode (`run_sync`) and
/// JSON-mode (`run_sync_json`).
///
/// `sink.mode` selects between text per-repo chatter, JSON envelope
/// collection, and JSON NDJSON streaming. `sink.records` is the shared
/// accumulator (always populated; text mode discards it on return). Under
/// `JsonNdjson` mode the sink additionally streams each record to stdout
/// at the moment it's recorded.
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
    sink: &OutputSink<'_>,
) -> anyhow::Result<()> {
    let emit_text = sink.emit_text();
    // Resolve CWD and source workspaces.
    let ctx = WorkspaceContext::resolve(cwd, project_override.clone())?;
    let workspace_dir = ctx.active_path().to_path_buf();

    // Resolve sync target: explicit source if given, else parent from marker.
    // Bare `rwv sync` only makes sense inside a workweave; the helpful error
    // here is the entire reason we bothered to make `source` optional.
    let resolved_source: SyncSource = match source {
        Some(s) => s.clone(),
        None => match &ctx.location {
            WorkspaceLocation::Workweave { dir, .. } => {
                let marker = crate::workspace::WorkweaveMarker::read(dir)?.ok_or_else(|| {
                    anyhow::anyhow!(
                        "bare `rwv sync` requires a `.rwv-workweave` marker in the workweave; \
                         found none at {} (re-create the workweave or pass an explicit source)",
                        dir.display()
                    )
                })?;
                let parent = marker.parent.ok_or_else(|| {
                    // Defensive: WorkweaveMarker::read backfills parent so this
                    // path should be unreachable; surface a clear error if a
                    // future change breaks the invariant.
                    anyhow::anyhow!(
                        "workweave marker at {} has no `parent` (and backfill failed); \
                         pass an explicit source to `rwv sync`",
                        dir.display()
                    )
                })?;
                SyncSource::Path(parent)
            }
            WorkspaceLocation::Weave { .. } => {
                anyhow::bail!(
                    "bare `rwv sync` syncs to the workweave's recorded parent, but CWD ({}) \
                     is in the primary weave, not a workweave; pass an explicit source",
                    cwd.display()
                );
            }
        },
    };

    let source_path = resolved_source.resolve(&ctx);
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
                    .and_then(|m| m.parent)
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
        .map_err(|e| anyhow::anyhow!("failed to load CWD project: {e}"))?;

    // Precondition: CWD project repo must not be mid-op.
    if let Some(state) = crate::git::GitVcs::mid_op_state(&cwd_project_dir) {
        anyhow::bail!("CWD project repo is {state}; resolve before running sync");
    }

    let cwd_workspace_name = workspace_name(&ctx);
    let source_workspace_name = workspace_name(&source_ctx);

    // Precondition: lock freshness (unless --force).
    if !force {
        let source_project = Project::from_dir(&source_project_dir)
            .map_err(|e| anyhow::anyhow!("failed to load source project: {e}"))?;
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
        .map_err(|e| anyhow::anyhow!("failed to read source project HEAD: {e}"))?;
    let cwd_project_tip = GitVcs
        .head_revision(&cwd_project_dir)
        .map_err(|e| anyhow::anyhow!("failed to read CWD project HEAD: {e}"))?;

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

    let op_id = OpId::new_now();

    // Write op marker to CWD workspace.
    let marker_path = workspace_dir.join(SYNC_OP_MARKER);
    std::fs::write(&marker_path, op_id.as_str())
        .map_err(|e| anyhow::anyhow!("failed to write sync op marker: {e}"))?;

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
        .map_err(|e| anyhow::anyhow!("failed to read source lock: {e}"))?;

    // Load source manifest so we have URLs for any repos newly added at source
    // that need to be materialized on the CWD side.
    let source_manifest_path = source_project_dir.join("rwv.yaml");
    let source_manifest = Manifest::from_path(&source_manifest_path)
        .map_err(|e| anyhow::anyhow!("failed to read source manifest: {e}"))?;

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
    for repo_path in raw_source_lock.repositories.keys() {
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
                // advanced past a never-materialised repo (same shape as
                // fo-62glp). Record the failure so the post-loop bail fires.
                materialize_failures.push(repo_path.clone());
            }
        }
    }

    // Phase 3 prune: any repo present on disk in CWD but absent from source's
    // new lock should be dropped. Conservative — refuse to delete worktrees
    // with uncommitted changes or unique local commits.
    if let Some(ref cwd_lock) = cwd_project.lock {
        for repo_path in cwd_lock.repositories.keys() {
            if raw_source_lock.repositories.contains_key(repo_path) {
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
    // workspace-wide state. See fo-i5z14 for the safety analysis.
    struct SyncTask {
        repo_path: crate::manifest::RepoPath,
        abs: PathBuf,
        target: ResolvedRevisionId,
    }
    let mut sync_tasks: Vec<SyncTask> = Vec::new();

    for (repo_path, raw_entry) in &raw_source_lock.repositories {
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
            sink.record(SyncOutcomeOutput::from_outcome(
                repo_path.to_string(),
                abs.to_string_lossy().into_owned(),
                &outcome,
            ));
            continue;
        }
        let lock_entry = match source_lock.repositories.get(repo_path) {
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
    // to the pre-fo-i5z14 loop. Under `jobs > 1` each worker calls
    // `sync_one_repo` + the post-sync refresh helpers on its own task; on
    // completion it routes the outcome through `sink.record`, which under
    // NDJSON mode writes one JSON line to stdout (mutex-guarded so
    // concurrent workers don't tear bytes).
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
        if emit_text {
            // Text-mode chatter. Acquire the shared stdout lock so the
            // line doesn't interleave with a concurrent worker's line
            // when jobs > 1 (defensive — text mode isn't a documented
            // -j > 1 path, but the lock is cheap and prevents torn
            // lines if a future caller wires that up).
            let _guard = sink.stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
            if is_failure {
                eprintln!("  {}: {outcome}", task.repo_path);
            } else {
                println!("  {}: {outcome}", task.repo_path);
            }
        }
        sink.record(SyncOutcomeOutput::from_outcome(
            task.repo_path.to_string(),
            task.abs.to_string_lossy().into_owned(),
            &outcome,
        ));
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
        match git(
            &["reset", "--hard", source_project_tip.as_str()],
            &cwd_project_dir,
        ) {
            Ok(_) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("project repo reset --force failed: {e}")),
        }
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
    // fails, fall back to the pre-Phase-1' snapshot — but log loudly, since
    // Phase 3 will then operate on a stale manifest and may miss newly-
    // added repos. (Other architectural note in the audit.)
    let cwd_project_phase3 = match Project::from_dir(&cwd_project_dir) {
        Ok(p) => p,
        Err(e) => {
            if emit_text {
                eprintln!(
                    "warning: failed to reload project after Phase 1' ({e}); \
                     Phase 3 will use the pre-Phase-1' manifest snapshot, which may \
                     miss newly-added repos"
                );
            }
            cwd_project
        }
    };

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
    let _ = std::fs::remove_file(&marker_path);

    // --retire: sync succeeded — verify we converged on parent's tip with a
    // clean tree, then delete the workweave. Only meaningful inside a
    // workweave; in a primary weave `--retire` is a no-op (warn instead of
    // silently doing nothing so the operator notices the misuse).
    if retire {
        match &ctx.location {
            WorkspaceLocation::Workweave { dir, name, project } => {
                retire_workweave_after_sync(
                    &ctx,
                    dir,
                    name,
                    project,
                    &cwd_project_dir,
                    &source_project_dir,
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

/// `rwv sync --retire` post-sync cleanup.
///
/// Verify that the just-completed sync brought CWD's manifest repos into
/// alignment with the parent's, and that no worktree has uncommitted changes,
/// then delete the workweave. Bails (preserving the workweave) on any
/// mismatch so the operator can fix and re-run.
///
/// We deliberately compare **manifest repo tips** rather than project repo
/// tips. The project repo's post-sync state typically diverges from parent
/// by exactly the auto-relock commit (Phase 3 always writes the workweave's
/// `workweave:` field into the lock, which the primary's lock lacks). That
/// commit is purely derived — the parent will regenerate it on its next
/// sync — so refusing on project-tip inequality would refuse every retire,
/// even the happy path the bead describes. Manifest tip equality is the
/// honest "work has converged" signal: Phase 2 advances both sides to the
/// same SHAs, so post-sync the manifest repos should be byte-equal.
fn retire_workweave_after_sync(
    ctx: &WorkspaceContext,
    workweave_dir: &Path,
    workweave_name: &WorkweaveName,
    project: &crate::manifest::ProjectName,
    cwd_project_dir: &Path,
    _source_project_dir: &Path,
) -> anyhow::Result<()> {
    // Reload manifest post-Phase 3 so we see any repos newly added by sync.
    let manifest_path = cwd_project_dir.join("rwv.yaml");
    let manifest = Manifest::from_path(&manifest_path)
        .map_err(|e| anyhow::anyhow!("--retire: failed to reload manifest: {e}"))?;

    // Compare each manifest repo's HEAD in CWD vs. parent. Parent's repo
    // lives under `source_workspace_dir.join(repo_path)`; here we reuse the
    // parent path from the marker (single source of truth for the bare-sync
    // target).
    let marker = crate::workspace::WorkweaveMarker::read(workweave_dir)?.ok_or_else(|| {
        anyhow::anyhow!(
            "--retire: workweave at {} has no .rwv-workweave marker",
            workweave_dir.display()
        )
    })?;
    let parent_root = marker
        .parent
        .clone()
        .unwrap_or_else(|| marker.primary.clone());

    let mut diverged: Vec<String> = Vec::new();
    for repo_path in manifest.iter_repo_paths() {
        let cwd_repo = workweave_dir.join(repo_path.as_path());
        let parent_repo = parent_root.join(repo_path.as_path());
        if !cwd_repo.exists() || !parent_repo.exists() {
            // Missing on one side — leave the workweave alone; this is
            // unusual enough that the operator should look.
            diverged.push(format!("{}: missing on one side", repo_path.as_str()));
            continue;
        }
        let cwd_head = GitVcs
            .head_revision(&cwd_repo)
            .map_err(|e| anyhow::anyhow!("--retire: read CWD head for {}: {e}", repo_path))?;
        let parent_head = GitVcs
            .head_revision(&parent_repo)
            .map_err(|e| anyhow::anyhow!("--retire: read parent head for {}: {e}", repo_path))?;
        if cwd_head != parent_head {
            diverged.push(format!(
                "{}: CWD={} parent={}",
                repo_path.as_str(),
                short_sha(cwd_head.as_str()),
                short_sha(parent_head.as_str())
            ));
        }
    }

    if !diverged.is_empty() {
        anyhow::bail!(
            "--retire: workweave's manifest repos differ from parent after sync; \
             refusing to delete:\n  {}\n\
             Push CWD's changes to parent (e.g. `cd {} && rwv sync {}`) and re-run, \
             or `rwv workweave delete --force {}` to discard.",
            diverged.join("\n  "),
            parent_root.display(),
            workweave_name.as_str(),
            workweave_name.as_str(),
        );
    }

    // Reuse the shared dirty-path check. Any dirty worktree blocks retire.
    let dirty = crate::workweave::collect_dirty_paths(workweave_dir, project, &manifest);
    if !dirty.is_empty() {
        anyhow::bail!(
            "--retire: workweave has uncommitted changes after sync; refusing to delete:\n  {}\n\
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
        .map_err(|e| anyhow::anyhow!("--retire: workweave delete failed: {e}"))?;

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
    .map_err(|e| anyhow::anyhow!("failed to generate lock: {e}"))?;

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
pub fn run_abort(cwd: &Path) -> anyhow::Result<()> {
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let workspace_dir = ctx.active_path().to_path_buf();

    // Read the op marker.
    let marker_path = workspace_dir.join(SYNC_OP_MARKER);
    if !marker_path.exists() {
        anyhow::bail!("no operation in progress");
    }
    let op_id = std::fs::read_to_string(&marker_path)
        .map_err(|e| anyhow::anyhow!("failed to read sync op marker: {e}"))?
        .trim()
        .to_owned();
    let op_id = OpId::from_string(op_id);

    let cwd_project_name = find_project_name(&ctx)?;
    let cwd_project_dir = workspace_dir.join("projects").join(&cwd_project_name);
    let cwd_project = Project::from_dir(&cwd_project_dir)
        .map_err(|e| anyhow::anyhow!("failed to load CWD project: {e}"))?;

    let mut any_failure = false;

    // Restore code repos first.
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

    // Restore project repo.
    if let Err(e) = abort_one_repo(&cwd_project_dir, &op_id) {
        eprintln!("  (project): {e}");
        any_failure = true;
    }

    // Remove marker file.
    let _ = std::fs::remove_file(&marker_path);

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
                .map_err(|e| anyhow::anyhow!("reset --hard failed: {e}"))?;
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
///   stdout on completion. Matches the shape pinned by fo-tn9uk.4.
/// - **Parallel / NDJSON** (`jobs > 1`): each per-repo outcome is streamed
///   as one JSON line to stdout the moment its worker finishes. Per the
///   fo-tn9uk epic convention every line embeds its own `$schema` so
///   consumers can identify a record without out-of-band context. No
///   envelope is emitted — the bead's acceptance is one self-describing
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
pub fn run_sync_json(
    cwd: &Path,
    source: Option<&SyncSource>,
    strategy: SyncStrategy,
    force: bool,
    retire: bool,
    project_override: Option<ProjectName>,
    jobs: usize,
) -> anyhow::Result<()> {
    let records: Mutex<Vec<SyncOutcomeOutput>> = Mutex::new(Vec::new());
    let stdout_lock: Mutex<()> = Mutex::new(());
    let mode = if jobs > 1 {
        OutputMode::JsonNdjson
    } else {
        OutputMode::JsonEnvelope
    };
    let sink = OutputSink {
        mode,
        stdout_lock: &stdout_lock,
        records: &records,
    };
    let project_level_result = run_sync_impl(
        cwd,
        source,
        strategy,
        force,
        retire,
        project_override,
        jobs,
        &sink,
    );

    let records = records.into_inner().unwrap_or_else(|e| e.into_inner());

    // If we never reached the per-repo loop (project-level precondition
    // failure), propagate the error so main prints it via anyhow.
    if records.is_empty() {
        return project_level_result;
    }

    let any_failure = records.iter().any(SyncOutcomeOutput::is_failure);

    // Under envelope mode we still need to emit the envelope to stdout
    // (NDJSON streamed each record as it arrived, so there's nothing
    // extra to write). Per the bead spec, NDJSON does NOT emit an
    // envelope wrapper around the stream.
    if matches!(mode, OutputMode::JsonEnvelope) {
        let payload = SyncJsonOutput {
            schema: SYNC_JSON_SCHEMA_URL.to_owned(),
            outcomes: records,
        };
        let out = serde_json::to_string_pretty(&payload)
            .map_err(|e| anyhow::anyhow!("failed to serialize sync output: {e}"))?;
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
        let parsed: SyncSource = "/tmp/some/path".parse().unwrap();
        assert_eq!(parsed, SyncSource::Path(PathBuf::from("/tmp/some/path")));
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
    // Conflict-bail messages — see fo-54gz8.
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
}
