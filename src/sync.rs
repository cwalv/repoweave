//! `rwv sync <source>` and `rwv abort` implementation.
//!
//! `rwv sync` aligns the CWD workspace with another workspace's committed
//! `rwv.lock`. `rwv abort` rolls back to pre-sync state using savepoint refs.

use crate::git::{git_command, GitVcs};
use crate::manifest::{LockFile, Project};
use crate::vcs::{RevisionId, Vcs};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use std::fmt;
use std::path::{Path, PathBuf};

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
// RepoSyncOutcome — per-repo result of a sync operation
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum RepoSyncOutcome {
    /// HEAD advanced to the lock SHA.
    Converged,
    /// Lock SHA is already an ancestor of HEAD; no change made.
    AlreadyAhead { commits_ahead: usize },
    /// HEAD was already equal to the lock SHA before sync.
    NoOp,
    /// Strategy failed (conflict, divergence, etc.).
    Failed { reason: String },
}

impl RepoSyncOutcome {
    fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
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
            Self::Failed { reason } => f.write_str(reason),
        }
    }
}

fn sync_one_repo(repo: &Path, target: &RevisionId, strategy: SyncStrategy) -> RepoSyncOutcome {
    let head = match GitVcs.head_revision(repo) {
        Ok(h) => h,
        Err(e) => {
            return RepoSyncOutcome::Failed {
                reason: e.to_string(),
            }
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
        Err(e) => RepoSyncOutcome::Failed {
            reason: e.to_string(),
        },
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
    pub fn new_now() -> Self {
        let s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos().to_string())
            .unwrap_or_else(|_| "0".to_owned());
        Self(s)
    }

    pub fn from_string(s: impl Into<String>) -> Self {
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

fn apply_strategy(repo: &Path, target: &RevisionId, strategy: SyncStrategy) -> anyhow::Result<()> {
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

fn create_savepoint(repo: &Path, op_id: &OpId) -> anyhow::Result<RevisionId> {
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

fn read_savepoint(repo: &Path, op_id: &OpId) -> Option<RevisionId> {
    git(&["rev-parse", &format!("{PRE_OP_REF}/{op_id}")], repo)
        .ok()
        .map(RevisionId::raw)
}

fn check_lock_freshness(workspace_dir: &Path, lock: &LockFile, label: &str) -> anyhow::Result<()> {
    // Resolve lock entries against on-disk repos so the comparison below is
    // purely a canonical-SHA equality check. Tag-form entries (e.g. v0.3.4)
    // resolve to the canonical SHA; SHA-form entries pass through unchanged.
    let mut resolved = lock.clone();
    let failures = resolved.resolve_versions(workspace_dir);
    if let Some(repo_path) = failures.first() {
        let raw = resolved.repositories[repo_path]
            .version
            .as_str()
            .to_string();
        anyhow::bail!(
            "{label} lock references unknown revision {raw}: {repo_path}  \
             (run `rwv lock` on the {label} workspace, or use --force to bypass)",
        );
    }

    for (repo_path, lock_entry) in &resolved.repositories {
        let abs = workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            continue;
        }
        if let Ok(actual) = GitVcs.head_revision(&abs) {
            if actual != lock_entry.version {
                anyhow::bail!(
                    "{label} lock is stale: {repo_path} tip={actual} lock={}  \
                     (run `rwv lock` on the {label} workspace, or use --force to bypass)",
                    lock_entry.version
                );
            }
        }
    }
    Ok(())
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

fn find_project_name(ctx: &WorkspaceContext) -> anyhow::Result<String> {
    let name = match &ctx.location {
        WorkspaceLocation::Weave { project: Some(p) } => p.as_str().to_owned(),
        WorkspaceLocation::Workweave { project, .. } => project.as_str().to_owned(),
        WorkspaceLocation::Weave { project: None } => {
            let names = crate::workspace::discover_project_paths(ctx.active_path());
            names.into_iter().next().ok_or_else(|| {
                anyhow::anyhow!(
                    "no project found under {}; is this a workspace?",
                    ctx.active_path().display()
                )
            })?
        }
    };
    Ok(name)
}

/// Resolve `source` to a filesystem path.
///
/// Accepts:
/// - An absolute or relative path.
/// - `primary` — the primary workspace root (resolved from CWD context).
fn resolve_source_path(ctx: &WorkspaceContext, source: &str) -> anyhow::Result<PathBuf> {
    if source == "primary" {
        return Ok(ctx.primary_path().to_path_buf());
    }
    let p = PathBuf::from(source);
    if p.is_absolute() {
        return Ok(p);
    }
    // Relative path: resolve against the primary weave so workweave names
    // (which live alongside primary under `.workweaves/`) resolve consistently
    // regardless of CWD.
    Ok(ctx.primary_path().join(source))
}

// ---------------------------------------------------------------------------
// rwv sync
// ---------------------------------------------------------------------------

/// Execute `rwv sync <source>`.
pub fn run_sync(
    cwd: &Path,
    source: &str,
    strategy: SyncStrategy,
    force: bool,
) -> anyhow::Result<()> {
    // Resolve CWD and source workspaces.
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let workspace_dir = ctx.active_path().to_path_buf();

    let source_path = resolve_source_path(&ctx, source)?;
    let source_ctx = WorkspaceContext::resolve(&source_path, None)?;
    let source_workspace_dir = source_ctx.active_path().to_path_buf();

    // Find active projects.
    let cwd_project_name = find_project_name(&ctx)?;
    let source_project_name = find_project_name(&source_ctx)?;

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

    // Precondition: lock freshness (unless --force).
    if !force {
        let source_project = Project::from_dir(&source_project_dir)
            .map_err(|e| anyhow::anyhow!("failed to load source project: {e}"))?;
        if let Some(ref lock) = source_project.lock {
            check_lock_freshness(&source_workspace_dir, lock, "source")?;
        }
        if let Some(ref lock) = cwd_project.lock {
            check_lock_freshness(&workspace_dir, lock, "CWD")?;
        }
    }

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

    // Phase 1: reset CWD project repo to source's tip to expose source's rwv.lock.
    // Always a hard reset (not strategy-based): the project repo tracks lock state,
    // not diverging development; merging/rebasing lock files always conflicts.
    let source_project_tip = GitVcs
        .head_revision(&source_project_dir)
        .map_err(|e| anyhow::anyhow!("failed to read source project HEAD: {e}"))?;

    if let Err(e) = git(
        &["reset", "--hard", source_project_tip.as_str()],
        &cwd_project_dir,
    ) {
        eprintln!("Phase 1 (project repo reset) failed: {e}");
        // Don't clean up savepoints — leave them for `rwv abort`.
        anyhow::bail!("sync failed in Phase 1 (project repo); run `rwv abort` to restore");
    }

    // Phase 2: advance per-repo branches using the now-visible lock.
    let updated_lock_path = cwd_project_dir.join("rwv.lock");
    let mut updated_lock = LockFile::from_path(&updated_lock_path)
        .map_err(|e| anyhow::anyhow!("failed to read lock after Phase 1: {e}"))?;
    // Resolve the freshly-loaded lock against on-disk repos so apply_strategy
    // operates on the canonical SHA.
    let _ = updated_lock.resolve_versions(&workspace_dir);

    let mut any_failure = false;

    for (repo_path, lock_entry) in &updated_lock.repositories {
        let abs = workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            println!("  {repo_path}: skipped (not on disk)");
            continue;
        }

        let outcome = sync_one_repo(&abs, &lock_entry.version, strategy);
        if outcome.is_failure() {
            eprintln!("  {repo_path}: {outcome}");
            any_failure = true;
        } else {
            if matches!(outcome, RepoSyncOutcome::Converged) {
                // Post-sync: refresh index and working tree if stale from a
                // shared-ref advance (HEAD advanced but index/WT were not updated).
                refresh_index_if_safe(&abs);
                refresh_working_tree_if_safe(&abs);
            }
            println!("  {repo_path}: {outcome}");
        }
    }

    if any_failure {
        anyhow::bail!("sync completed with failures; fix conflicts and re-run, or run `rwv abort`");
    }

    // Successful completion: clean up savepoints and marker.
    delete_savepoint(&cwd_project_dir, &op_id);
    for repo_path in cwd_project.manifest.repositories.keys() {
        let abs = workspace_dir.join(repo_path.as_path());
        if abs.exists() {
            delete_savepoint(&abs, &op_id);
        }
    }
    let _ = std::fs::remove_file(&marker_path);

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
