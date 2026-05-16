//! `rwv sync <source>` and `rwv abort` implementation.
//!
//! `rwv sync` aligns the CWD workspace with another workspace's committed
//! `rwv.lock`. `rwv abort` rolls back to pre-sync state using savepoint refs.

use crate::git::{git_command, GitVcs};
use crate::lock::{commit_lock_file_with_message, generate_lock};
use crate::manifest::{LockFile, Manifest, Project, ProjectName, RepoPath, WorkweaveName};
use crate::vcs::{ResolvedRevisionId, Vcs};
use crate::workspace::{read_active_project, WorkspaceContext, WorkspaceLocation};
use crate::workweave::workweave_path_for;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

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
#[derive(Debug)]
pub enum SyncFailure {
    /// Couldn't read HEAD on the repo (e.g. not a repo, or I/O failure).
    HeadUnreadable { error: String },
    /// `--strategy ff` cannot proceed (divergence, conflict).
    FastForwardImpossible { error: String },
    /// `--strategy rebase` failed (conflict or git error).
    RebaseFailed { error: String },
    /// `--strategy merge` failed (conflict or git error).
    MergeFailed { error: String },
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

    fn for_strategy(strategy: SyncStrategy, error: String) -> Self {
        match strategy {
            SyncStrategy::Ff => Self::FastForwardImpossible { error },
            SyncStrategy::Rebase => Self::RebaseFailed { error },
            SyncStrategy::Merge => Self::MergeFailed { error },
        }
    }
}

impl fmt::Display for SyncFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeadUnreadable { error }
            | Self::FastForwardImpossible { error }
            | Self::RebaseFailed { error }
            | Self::MergeFailed { error } => f.write_str(error),
        }
    }
}

#[derive(Debug)]
enum RepoSyncOutcome {
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
    fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

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
            return RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
                error: e.to_string(),
            })
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
        Err(e) => RepoSyncOutcome::Failed(SyncFailure::for_strategy(strategy, e.to_string())),
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

fn apply_strategy(
    repo: &Path,
    target: &ResolvedRevisionId,
    strategy: SyncStrategy,
) -> anyhow::Result<()> {
    let target_ref = target.as_str();
    match strategy {
        SyncStrategy::Ff => {
            let out = git(&["merge", "--ff-only", target_ref], repo);
            if let Err(e) = out {
                anyhow::bail!(
                    "cannot fast-forward; rerun with --strategy rebase or --strategy merge. {}",
                    e
                );
            }
        }
        SyncStrategy::Rebase => {
            git(&["rebase", target_ref], repo)?;
        }
        SyncStrategy::Merge => {
            // Merge with auto-generated commit message.
            git(&["merge", "--no-edit", target_ref], repo)?;
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
            let names = crate::workspace::discover_project_paths(ctx.active_path());
            let name = names.into_iter().next().ok_or_else(|| {
                anyhow::anyhow!(
                    "no project found under {}; is this a workspace?",
                    ctx.active_path().display()
                )
            })?;
            Ok(ProjectName::new(name))
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
pub fn run_sync(
    cwd: &Path,
    source: &SyncSource,
    strategy: SyncStrategy,
    force: bool,
) -> anyhow::Result<()> {
    // Resolve CWD and source workspaces.
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let workspace_dir = ctx.active_path().to_path_buf();

    let source_path = source.resolve(&ctx);
    let source_ctx = WorkspaceContext::resolve(&source_path, None)?;
    let source_workspace_dir = source_ctx.active_path().to_path_buf();

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
    for repo_path in cwd_project.manifest.repositories.keys() {
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
        let entry = match source_manifest.repositories.get(repo_path) {
            Some(e) => e,
            None => continue, // lock entry without manifest entry — skip
        };
        match materialize_missing_repo(&ctx, repo_path, entry, &cwd_project_name) {
            Ok(()) => println!("  {repo_path}: materialized"),
            Err(e) => {
                eprintln!("  {repo_path}: materialize failed: {e}");
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
                Ok(()) => println!("  {repo_path}: pruned (dropped from lock)"),
                Err(e) => eprintln!("  {repo_path}: prune skipped: {e}"),
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

    for (repo_path, raw_entry) in &raw_source_lock.repositories {
        let abs = workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            println!("  {repo_path}: skipped (not on disk)");
            continue;
        }
        if unresolvable.contains(repo_path) {
            eprintln!(
                "  {repo_path}: lock pins unknown revision {} in local clone",
                raw_entry.version
            );
            any_failure = true;
            continue;
        }
        let lock_entry = match source_lock.repositories.get(repo_path) {
            Some(e) => e,
            None => continue,
        };

        let outcome = sync_one_repo(&abs, &lock_entry.version, strategy);
        if outcome.is_failure() {
            eprintln!("  {repo_path}: {outcome}");
            any_failure = true;
        } else {
            // Post-sync: refresh index and working tree if stale. Fires on
            // every non-failure outcome — including NoOp (HEAD already at lock
            // but index/WT may have drifted from a shared-ref advance) and
            // AlreadyAhead (working tree should still reflect HEAD).
            refresh_index_if_safe(&abs);
            refresh_working_tree_if_safe(&abs);
            println!("  {repo_path}: {outcome}");
        }
    }

    if any_failure {
        anyhow::bail!("sync completed with failures; fix conflicts and re-run, or run `rwv abort`");
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
        apply_project_strategy_excluding_lock(
            &cwd_project_dir,
            &source_project_tip,
            &cwd_project_tip,
            strategy,
        )
    };

    if let Err(e) = phase1_outcome {
        eprintln!("Phase 1' (project repo) failed: {e}");
        anyhow::bail!("sync failed in Phase 1' (project repo); run `rwv abort` to restore");
    }

    // Reload CWD project so Phase 3 sees the post-Phase-1' manifest (which
    // may now include newly-added repos brought over from source). If reload
    // fails, fall back to the pre-Phase-1' snapshot — but log loudly, since
    // Phase 3 will then operate on a stale manifest and may miss newly-
    // added repos. (Other architectural note in the audit.)
    let cwd_project_phase3 = match Project::from_dir(&cwd_project_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "warning: failed to reload project after Phase 1' ({e}); \
                 Phase 3 will use the pre-Phase-1' manifest snapshot, which may \
                 miss newly-added repos"
            );
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
        eprintln!("Phase 3 (re-lock) failed: {e}");
        anyhow::bail!("sync failed in Phase 3 (re-lock); run `rwv abort` to restore");
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
    } else {
        eprintln!(
            "note: --force discarded project commits; pre-sync state preserved at \
             refs/rwv/pre-op/{op_id} (recover with `git reset --hard refs/rwv/pre-op/{op_id}` \
             in {})",
            cwd_project_dir.display()
        );
    }
    for repo_path in cwd_project_phase3.manifest.repositories.keys() {
        let abs = workspace_dir.join(repo_path.as_path());
        if abs.exists() {
            delete_savepoint(&abs, &op_id);
        }
    }
    let _ = std::fs::remove_file(&marker_path);

    Ok(())
}

/// Phase 1': replay CWD's unique project commits onto `source_tip` via
/// `strategy`, with `rwv.lock` excluded from each commit's effective diff.
///
/// - `Ff`: requires CWD ancestor of source (caller already verified). Performs
///   a fast-forward via `git merge --ff-only`.
/// - `Rebase`: cherry-picks each CWD-unique commit onto source, dropping
///   `rwv.lock` from the staged changes. Lock-only commits become empty
///   patches and are skipped silently.
/// - `Merge`: produces a merge commit on top of CWD whose tree matches a
///   regular merge with source, but with `rwv.lock` taken from source (Phase 3
///   regenerates it).
///
/// Conflicts on non-lock paths halt the operation with an error naming the
/// conflicting paths; the operator resolves and re-runs sync, or invokes
/// `rwv abort`.
fn apply_project_strategy_excluding_lock(
    cwd_project_dir: &Path,
    source_tip: &ResolvedRevisionId,
    cwd_tip: &ResolvedRevisionId,
    strategy: SyncStrategy,
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
            apply_rebase_excluding_lock(cwd_project_dir, source_tip)?;
        }
        SyncStrategy::Merge => {
            apply_merge_excluding_lock(cwd_project_dir, source_tip)?;
        }
    }
    Ok(())
}

/// Cherry-pick each CWD-unique commit (in chronological order) onto
/// `source_tip`, with `rwv.lock` excluded from each commit's effective diff.
fn apply_rebase_excluding_lock(repo: &Path, source_tip: &ResolvedRevisionId) -> anyhow::Result<()> {
    let source_ref = source_tip.as_str();

    // Find merge-base between CWD's HEAD and source.
    let merge_base = git(&["merge-base", "HEAD", source_ref], repo)
        .map_err(|e| anyhow::anyhow!("failed to find merge-base with source: {e}"))?;

    // List CWD's unique commits since merge-base, oldest-first.
    let commits_str = git(
        &["rev-list", "--reverse", &format!("{merge_base}..HEAD")],
        repo,
    )
    .map_err(|e| anyhow::anyhow!("failed to list unique commits: {e}"))?;
    let commits: Vec<String> = commits_str
        .lines()
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();

    if commits.is_empty() {
        // CWD is ancestor of source — fast-forward.
        git(&["reset", "--hard", source_ref], repo)?;
        return Ok(());
    }

    // Reset onto source's tip; we'll replay each unique commit on top.
    git(&["reset", "--hard", source_ref], repo)?;

    for sha in &commits {
        // Cherry-pick with --no-commit so we can manipulate the index/WT
        // before deciding whether to commit.
        let _ = git_command()
            .args(["cherry-pick", "--allow-empty", "--no-commit", sha])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        // Drop any rwv.lock changes (resolves any lock conflict by taking
        // HEAD's version, then unstages so it's effectively excluded).
        let _ = git_command()
            .args(["checkout", "HEAD", "--", "rwv.lock"])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let _ = git_command()
            .args(["reset", "HEAD", "--", "rwv.lock"])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        // Surface real (non-lock) conflicts as a halt.
        let unmerged_str =
            git(&["diff", "--name-only", "--diff-filter=U"], repo).unwrap_or_default();
        let real_conflicts: Vec<String> = unmerged_str
            .lines()
            .filter(|p| !p.is_empty() && *p != "rwv.lock")
            .map(String::from)
            .collect();
        if !real_conflicts.is_empty() {
            anyhow::bail!(
                "rebase replay hit conflict at commit {sha} on paths: {}. \
                 Resolve in {} and re-run sync, or run `rwv abort` to restore.",
                real_conflicts.join(", "),
                repo.display()
            );
        }

        // Empty-patch detection: nothing staged means the commit was
        // lock-only (or otherwise empty after lock exclusion). Skip with a
        // log line and clear cherry-pick state.
        let nothing_staged = git_command()
            .args(["diff", "--cached", "--quiet"])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true);

        if nothing_staged {
            // Clear CHERRY_PICK_HEAD if set so the next iteration is clean.
            clear_cherry_pick_state(repo);
            eprintln!("  (project): skipped lock-only commit {sha}");
            continue;
        }

        // Commit with original commit's metadata (author, message, date).
        git(&["commit", "-C", sha], repo)
            .map_err(|e| anyhow::anyhow!("failed to commit replayed {sha}: {e}"))?;
    }

    Ok(())
}

/// Merge `source_tip` into CWD, dropping any `rwv.lock` change/conflict by
/// taking HEAD's version (Phase 3 regenerates the lock from manifest tips).
fn apply_merge_excluding_lock(repo: &Path, source_tip: &ResolvedRevisionId) -> anyhow::Result<()> {
    let source_ref = source_tip.as_str();

    // Try a regular merge with --no-commit so we can manipulate the index/WT.
    let _ = git_command()
        .args(["merge", "--no-commit", "--no-edit", source_ref])
        .current_dir(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Resolve any rwv.lock change/conflict by taking HEAD's version.
    let _ = git_command()
        .args(["checkout", "HEAD", "--", "rwv.lock"])
        .current_dir(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    let _ = git_command()
        .args(["reset", "HEAD", "--", "rwv.lock"])
        .current_dir(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Surface real (non-lock) conflicts as a halt.
    let unmerged_str = git(&["diff", "--name-only", "--diff-filter=U"], repo).unwrap_or_default();
    let real_conflicts: Vec<String> = unmerged_str
        .lines()
        .filter(|p| !p.is_empty() && *p != "rwv.lock")
        .map(String::from)
        .collect();
    if !real_conflicts.is_empty() {
        anyhow::bail!(
            "merge conflict in {} on paths: {}. \
             Resolve and re-run sync, or run `rwv abort` to restore.",
            repo.display(),
            real_conflicts.join(", ")
        );
    }

    // Detect whether MERGE_HEAD is still set (i.e. there's a merge in
    // progress). A clean fast-forward leaves no MERGE_HEAD and no commit to
    // make; if we're already at source_tip, nothing to do.
    let git_dir = git(&["rev-parse", "--git-dir"], repo)
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo.join(".git"));
    let merge_head = if git_dir.is_absolute() {
        git_dir.join("MERGE_HEAD")
    } else {
        repo.join(&git_dir).join("MERGE_HEAD")
    };

    if merge_head.exists() {
        git(&["commit", "--no-edit"], repo)
            .map_err(|e| anyhow::anyhow!("failed to commit merge: {e}"))?;
    }

    Ok(())
}

/// Clear cherry-pick mid-op state without aborting — used when a commit is
/// dropped after lock exclusion.
fn clear_cherry_pick_state(repo: &Path) {
    let git_dir = match git(&["rev-parse", "--git-dir"], repo) {
        Ok(s) => {
            let p = PathBuf::from(&s);
            if p.is_absolute() {
                p
            } else {
                repo.join(p)
            }
        }
        Err(_) => return,
    };
    let _ = std::fs::remove_file(git_dir.join("CHERRY_PICK_HEAD"));
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
    for repo_path in cwd_project.manifest.repositories.keys() {
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
            SyncFailure::HeadUnreadable { error: "x".into() }.kind(),
            "head-unreadable"
        );
        assert_eq!(
            SyncFailure::FastForwardImpossible { error: "x".into() }.kind(),
            "ff-impossible"
        );
        assert_eq!(
            SyncFailure::RebaseFailed { error: "x".into() }.kind(),
            "rebase-failed"
        );
        assert_eq!(
            SyncFailure::MergeFailed { error: "x".into() }.kind(),
            "merge-failed"
        );
    }

    #[test]
    fn sync_failure_for_strategy_picks_matching_variant() {
        assert!(matches!(
            SyncFailure::for_strategy(SyncStrategy::Ff, "e".into()),
            SyncFailure::FastForwardImpossible { .. }
        ));
        assert!(matches!(
            SyncFailure::for_strategy(SyncStrategy::Rebase, "e".into()),
            SyncFailure::RebaseFailed { .. }
        ));
        assert!(matches!(
            SyncFailure::for_strategy(SyncStrategy::Merge, "e".into()),
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
        assert!(err.contains("/src/ws"), "msg: {err}");
        assert!(err.contains("rwv activate"), "msg: {err}");
    }
}
