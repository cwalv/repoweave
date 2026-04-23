//! `rwv sync <source>` and `rwv abort` implementation.
//!
//! `rwv sync` aligns the CWD workspace with another workspace's committed
//! `rwv.lock`. `rwv abort` rolls back to pre-sync state using savepoint refs.

use crate::manifest::{LockFile, Project};
use crate::workspace::{WorkspaceContext, WorkspaceLocation};
use std::path::{Path, PathBuf};
use std::process::Command;

const SYNC_OP_MARKER: &str = ".rwv-sync-op";
const PRE_OP_REF: &str = "refs/rwv/pre-op";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn git(args: &[&str], dir: &Path) -> anyhow::Result<String> {
    let out = Command::new("git")
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

fn git_head(repo: &Path) -> anyhow::Result<String> {
    git(&["rev-parse", "HEAD"], repo)
}

fn apply_strategy(repo: &Path, target: &str, strategy: &str) -> anyhow::Result<()> {
    match strategy {
        "ff" => {
            let out = git(&["merge", "--ff-only", target], repo);
            if let Err(e) = out {
                anyhow::bail!(
                    "cannot fast-forward; rerun with --strategy rebase or --strategy merge. {}",
                    e
                );
            }
        }
        "rebase" => {
            git(&["rebase", target], repo)?;
        }
        "merge" => {
            // Merge with auto-generated commit message.
            git(&["merge", "--no-edit", target], repo)?;
        }
        _ => anyhow::bail!("unknown strategy {strategy:?}; expected ff, rebase, or merge"),
    }
    Ok(())
}

fn create_savepoint(repo: &Path, op_id: &str) -> anyhow::Result<String> {
    let sha = git_head(repo)?;
    git(
        &["update-ref", &format!("{PRE_OP_REF}/{op_id}"), &sha],
        repo,
    )?;
    Ok(sha)
}

fn delete_savepoint(repo: &Path, op_id: &str) {
    let _ = git(
        &["update-ref", "-d", &format!("{PRE_OP_REF}/{op_id}")],
        repo,
    );
}

fn read_savepoint(repo: &Path, op_id: &str) -> Option<String> {
    git(&["rev-parse", &format!("{PRE_OP_REF}/{op_id}")], repo).ok()
}

fn check_lock_freshness(workspace_dir: &Path, lock: &LockFile, label: &str) -> anyhow::Result<()> {
    for (repo_path, lock_entry) in &lock.repositories {
        let abs = workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            continue;
        }
        if let Ok(actual) = git_head(&abs) {
            if actual != lock_entry.version.as_str() {
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
    let clean = std::process::Command::new("git")
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
    let index_tree = match std::process::Command::new("git")
        .arg("write-tree")
        .current_dir(repo)
        .output()
    {
        Ok(out) if out.status.success() => {
            String::from_utf8(out.stdout).unwrap_or_default().trim().to_owned()
        }
        _ => return, // can't verify — leave index alone
    };

    // Safety check: is the index tree the tree of some recent ancestor commit?
    // Bounded to last 200 commits to keep doctor fast on large histories.
    let ancestor_trees = match std::process::Command::new("git")
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
    let _ = std::process::Command::new("git")
        .arg("reset")
        .current_dir(repo)
        .output();
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
    let clean = std::process::Command::new("git")
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
    let status_out = match std::process::Command::new("git")
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
        let objects_out = match std::process::Command::new("git")
            .args(["rev-list", "--objects", "-n", "200", "HEAD"])
            .current_dir(repo)
            .output()
        {
            Ok(out) if out.status.success() => out,
            _ => return,
        };
        let reachable: std::collections::HashSet<String> =
            String::from_utf8(objects_out.stdout)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| l.split_whitespace().next().map(|s| s.to_owned()))
                .collect();
        for file in &modified_files {
            let hash_out = match std::process::Command::new("git")
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
    let _ = std::process::Command::new("git")
        .args(&args)
        .current_dir(repo)
        .output();
}

fn find_project_name(ctx: &WorkspaceContext) -> anyhow::Result<String> {
    let name = match &ctx.location {
        WorkspaceLocation::Weave { project: Some(p) } => p.as_str().to_owned(),
        WorkspaceLocation::Workweave { project, .. } => project.as_str().to_owned(),
        WorkspaceLocation::Weave { project: None } => {
            let names = crate::workspace::discover_project_paths(&ctx.root);
            names.into_iter().next().ok_or_else(|| {
                anyhow::anyhow!(
                    "no project found under {}; is this a workspace?",
                    ctx.root.display()
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
        return Ok(ctx.root.clone());
    }
    let p = PathBuf::from(source);
    if p.is_absolute() {
        return Ok(p);
    }
    // Relative path: resolve against workspace root.
    Ok(ctx.root.join(source))
}

// ---------------------------------------------------------------------------
// rwv sync
// ---------------------------------------------------------------------------

/// Execute `rwv sync <source>`.
pub fn run_sync(cwd: &Path, source: &str, strategy: &str, force: bool) -> anyhow::Result<()> {
    // Validate strategy.
    if !matches!(strategy, "ff" | "rebase" | "merge") {
        anyhow::bail!("unknown strategy {strategy:?}; expected ff, rebase, or merge");
    }

    // Resolve CWD and source workspaces.
    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let workspace_dir = ctx.resolve_path().to_path_buf();

    let source_path = resolve_source_path(&ctx, source)?;
    let source_ctx = WorkspaceContext::resolve(&source_path, None)?;
    let source_workspace_dir = source_ctx.resolve_path().to_path_buf();

    // Find active projects.
    let cwd_project_name = find_project_name(&ctx)?;
    let source_project_name = find_project_name(&source_ctx)?;

    let cwd_project_dir = ctx.root.join("projects").join(&cwd_project_name);
    let source_project_dir = source_ctx.root.join("projects").join(&source_project_name);

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

    // Generate op-id (nanosecond timestamp).
    let op_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_else(|_| "0".to_owned());

    // Write op marker to CWD workspace.
    let marker_path = workspace_dir.join(SYNC_OP_MARKER);
    std::fs::write(&marker_path, &op_id)
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
    let source_project_tip = git_head(&source_project_dir)
        .map_err(|e| anyhow::anyhow!("failed to read source project HEAD: {e}"))?;

    if let Err(e) = git(&["reset", "--hard", &source_project_tip], &cwd_project_dir) {
        eprintln!("Phase 1 (project repo reset) failed: {e}");
        // Don't clean up savepoints — leave them for `rwv abort`.
        anyhow::bail!("sync failed in Phase 1 (project repo); run `rwv abort` to restore");
    }

    // Phase 2: advance per-repo branches using the now-visible lock.
    let updated_lock_path = cwd_project_dir.join("rwv.lock");
    let updated_lock = LockFile::from_path(&updated_lock_path)
        .map_err(|e| anyhow::anyhow!("failed to read lock after Phase 1: {e}"))?;

    let mut any_failure = false;

    for (repo_path, lock_entry) in &updated_lock.repositories {
        let abs = workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            println!("  {repo_path}: skipped (not on disk)");
            continue;
        }

        match apply_strategy(&abs, lock_entry.version.as_str(), strategy) {
            Ok(()) => {
                // Post-sync: refresh index and working tree if stale from a
                // shared-ref advance (HEAD advanced but index/WT were not updated).
                refresh_index_if_safe(&abs);
                refresh_working_tree_if_safe(&abs);
                println!("  {repo_path}: ok");
            }
            Err(e) => {
                eprintln!("  {repo_path}: {e}");
                any_failure = true;
                println!("  {repo_path}: failed");
            }
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
    let workspace_dir = ctx.resolve_path().to_path_buf();

    // Read the op marker.
    let marker_path = workspace_dir.join(SYNC_OP_MARKER);
    if !marker_path.exists() {
        anyhow::bail!("no operation in progress");
    }
    let op_id = std::fs::read_to_string(&marker_path)
        .map_err(|e| anyhow::anyhow!("failed to read sync op marker: {e}"))?
        .trim()
        .to_owned();

    let cwd_project_name = find_project_name(&ctx)?;
    let cwd_project_dir = ctx.root.join("projects").join(&cwd_project_name);
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

fn abort_one_repo(repo: &Path, op_id: &str) -> anyhow::Result<()> {
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
            git(&["reset", "--hard", &sha], repo)
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
