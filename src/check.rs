//! Convention checks: orphaned clones, dangling refs, stale locks, index drift, working-tree drift, etc.
//!
//! `rwv doctor` builds a workspace-wide inventory from all projects, then runs
//! a series of checks. Integration check hooks are run separately.

use crate::integration::Issue;
use crate::manifest::{Project, RepoPath};
use crate::vcs::RevisionId;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The kinds of convention violations `rwv doctor` can find.
///
/// Each variant carries enough data to produce a useful message.
/// Separating the description (this enum) from execution (the checker)
/// makes results testable without touching the filesystem.
#[derive(Debug)]
pub enum CheckViolation {
    /// A directory under a registry path not listed in any project's `rwv.yaml`.
    OrphanedClone { path: RepoPath },

    /// An `rwv.yaml` entry pointing to a path not present on disk.
    DanglingReference { project: String, repo: RepoPath },

    /// An `rwv.yaml` entry missing the `role` field.
    MissingRole { project: String, repo: RepoPath },

    /// A project's `rwv.lock` doesn't match current HEAD SHAs.
    StaleLock {
        project: String,
        repo: RepoPath,
        locked: RevisionId,
        actual: RevisionId,
    },

    /// A worktree missing from a workweave, or an extra worktree not in the manifest.
    WorkweaveDrift {
        workweave: String,
        kind: DriftKind,
        repo: RepoPath,
    },

    /// A git repo's index does not match its HEAD tree (silent stale-index from
    /// shared-ref advance in a sibling worktree).
    IndexDrift {
        /// Workweave name; `None` for repos in the primary weave.
        workweave: Option<String>,
        repo: RepoPath,
        kind: IndexDriftKind,
    },

    /// A git repo's working-tree files do not match its HEAD tree (stale on-disk
    /// content after shared-ref advance in a sibling worktree).
    WorkingTreeDrift {
        workweave: Option<String>,
        repo: RepoPath,
        kind: WorkingTreeDriftKind,
    },
}

#[derive(Debug)]
pub enum DriftKind {
    /// Manifest lists it, but no worktree exists.
    Missing,
    /// Worktree exists, but manifest doesn't list it.
    Extra,
}

/// How a stale index should be treated.
#[derive(Debug)]
pub enum IndexDriftKind {
    /// Index tree matches the tree of some recent ancestor commit. Safe to
    /// auto-fix with `git reset` — the displaced tree is permanently in the DAG.
    SafeToFix,
    /// Index tree is not found in recent ancestor trees. The user has live
    /// staged content; `--fix` must not touch this.
    LiveStaged,
}

/// How stale working-tree files should be treated.
#[derive(Debug)]
pub enum WorkingTreeDriftKind {
    /// All modified files' on-disk content matches blobs reachable from HEAD.
    /// Safe to restore with `git checkout HEAD -- <files>` — no work is lost.
    SafeToFix,
    /// At least one modified file has on-disk content not found in any recent
    /// ancestor's tree. The user has active edits; `--fix` must not touch this.
    LiveEdits,
}

/// Inputs for running workspace-wide checks.
pub struct CheckInput {
    /// All repos referenced by any project's `rwv.yaml`.
    pub known_repos: BTreeSet<RepoPath>,
    /// All git repos found on disk under registry directories.
    pub repos_on_disk: Vec<RepoPath>,
    /// Loaded projects.
    pub projects: Vec<Project>,
    /// Resolved HEAD revisions for repos on disk, keyed by RepoPath.
    pub head_revisions: BTreeMap<RepoPath, RevisionId>,
}

/// Collect all convention violations from the check inputs.
///
/// This is a pure function: it takes data in, returns violations out.
/// Filesystem access (reading HEADs, scanning directories) happens
/// before this function is called.
pub fn find_violations(input: &CheckInput) -> Vec<CheckViolation> {
    let mut violations = Vec::new();

    // Orphaned clones: on disk but not in any project
    for repo_path in &input.repos_on_disk {
        if !input.known_repos.contains(repo_path) {
            violations.push(CheckViolation::OrphanedClone {
                path: repo_path.clone(),
            });
        }
    }

    // Per-project checks
    for project in &input.projects {
        for repo_path in project.manifest.repositories.keys() {
            // Dangling reference: in manifest but not on disk
            if !input.repos_on_disk.contains(repo_path) {
                violations.push(CheckViolation::DanglingReference {
                    project: project.name.as_str().to_owned(),
                    repo: repo_path.clone(),
                });
            }
        }

        // Compare lock entries against resolved HEADs
        if let Some(ref lock) = project.lock {
            for (repo_path, lock_entry) in &lock.repositories {
                if let Some(actual_rev) = input.head_revisions.get(repo_path) {
                    if lock_entry.version.as_str() != actual_rev.as_str() {
                        violations.push(CheckViolation::StaleLock {
                            project: project.name.as_str().to_owned(),
                            repo: repo_path.clone(),
                            locked: lock_entry.version.clone(),
                            actual: actual_rev.clone(),
                        });
                    }
                }
            }
        }
    }

    violations
}

/// Convert check violations into the same `Issue` type that integrations use,
/// so all check results have a uniform shape.
pub fn violations_to_issues(violations: Vec<CheckViolation>) -> Vec<Issue> {
    violations
        .into_iter()
        .map(|v| {
            let (severity, message) = match v {
                CheckViolation::OrphanedClone { path } => (
                    crate::integration::Severity::Error,
                    format!("orphaned clone: {path}"),
                ),
                CheckViolation::DanglingReference { project, repo } => (
                    crate::integration::Severity::Error,
                    format!("dangling reference in {project}: {repo}"),
                ),
                CheckViolation::MissingRole { project, repo } => (
                    crate::integration::Severity::Warning,
                    format!("missing role in {project}: {repo}"),
                ),
                CheckViolation::StaleLock {
                    project,
                    repo,
                    locked,
                    actual,
                } => (
                    crate::integration::Severity::Error,
                    format!(
                        "stale lock in {project}: {} locked={} actual={}",
                        repo, locked, actual
                    ),
                ),
                CheckViolation::WorkweaveDrift {
                    workweave,
                    kind,
                    repo,
                } => {
                    let kind_str = match kind {
                        DriftKind::Missing => "missing worktree",
                        DriftKind::Extra => "extra worktree",
                    };
                    (
                        crate::integration::Severity::Warning,
                        format!("workweave drift in {workweave}: {kind_str} {repo}"),
                    )
                }
                CheckViolation::IndexDrift {
                    workweave,
                    repo,
                    kind,
                } => {
                    let location = match workweave {
                        Some(ww) => format!("{ww}/{repo}"),
                        None => format!("{repo}"),
                    };
                    let detail = match kind {
                        IndexDriftKind::SafeToFix => "index stale (safe to --fix)",
                        IndexDriftKind::LiveStaged => {
                            "index has live staged changes (manual review)"
                        }
                    };
                    (
                        crate::integration::Severity::Warning,
                        format!("{location}: {detail}"),
                    )
                }
                CheckViolation::WorkingTreeDrift {
                    workweave,
                    repo,
                    kind,
                } => {
                    let location = match workweave {
                        Some(ww) => format!("{ww}/{repo}"),
                        None => format!("{repo}"),
                    };
                    let detail = match kind {
                        WorkingTreeDriftKind::SafeToFix => "working tree stale (safe to --fix)",
                        WorkingTreeDriftKind::LiveEdits => {
                            "working tree has live edits (manual review)"
                        }
                    };
                    (
                        crate::integration::Severity::Warning,
                        format!("{location}: {detail}"),
                    )
                }
            };
            Issue {
                integration: "core".into(),
                severity,
                message,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Index-drift helpers
// ---------------------------------------------------------------------------

/// Classify the index-drift state of a git repo at `repo`.
///
/// Returns `None` when the index matches HEAD (no drift).  Otherwise returns
/// `Some(IndexDriftKind)` — either `SafeToFix` (index tree is an ancestor
/// commit's tree, safely replaceable) or `LiveStaged` (user has staged content
/// that is not a committed tree; must not be auto-fixed).
pub fn classify_index_drift(repo: &Path) -> Option<IndexDriftKind> {
    // Exit-0 means index matches HEAD tree — no drift.
    let clean = std::process::Command::new("git")
        .args(["diff-index", "--cached", "--exit-code", "HEAD"])
        .current_dir(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true); // assume clean if git unavailable
    if clean {
        return None;
    }

    // Index differs from HEAD. Determine the current index tree SHA.
    let index_tree = match std::process::Command::new("git")
        .arg("write-tree")
        .current_dir(repo)
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout)
            .unwrap_or_default()
            .trim()
            .to_owned(),
        _ => return Some(IndexDriftKind::LiveStaged), // conservative
    };

    // Safety check: is the index tree the tree of some recent ancestor commit?
    // Bounded to 200 ancestors to keep performance acceptable on deep histories.
    let ancestor_trees = match std::process::Command::new("git")
        .args(["log", "--format=%T", "-200", "HEAD"])
        .current_dir(repo)
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout).unwrap_or_default(),
        _ => return Some(IndexDriftKind::LiveStaged),
    };

    if ancestor_trees.lines().any(|t| t.trim() == index_tree) {
        Some(IndexDriftKind::SafeToFix)
    } else {
        Some(IndexDriftKind::LiveStaged)
    }
}

/// Reset the index to match HEAD, leaving the working tree and HEAD untouched.
///
/// Only call after confirming `classify_index_drift` returns `SafeToFix`.
/// Uses bare `git reset` (equivalent to `git reset --mixed HEAD`).
pub fn reset_index_to_head(repo: &Path) -> anyhow::Result<()> {
    let out = std::process::Command::new("git")
        .arg("reset")
        .current_dir(repo)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git reset: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("git reset failed in {}: {}", repo.display(), stderr.trim());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Working-tree-drift helpers
// ---------------------------------------------------------------------------

/// Classify the working-tree-drift state of a git repo at `repo`.
///
/// Returns `None` when the working tree matches HEAD (no drift). Otherwise
/// returns `Some(WorkingTreeDriftKind)` — either `SafeToFix` (all modified
/// files' on-disk content matches a reachable committed blob) or `LiveEdits`
/// (at least one file has content not found in recent ancestors; must not be
/// auto-fixed).
///
/// Uses `git diff-index HEAD` (without `--cached`) so detection works
/// regardless of whether index drift has already been resolved.
pub fn classify_working_tree_drift(repo: &Path) -> Option<WorkingTreeDriftKind> {
    // Exit-0 means working tree matches HEAD — no drift.
    let clean = std::process::Command::new("git")
        .args(["diff-index", "--exit-code", "HEAD"])
        .current_dir(repo)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true);
    if clean {
        return None;
    }

    // Use --name-status to distinguish two cases:
    //   D = file exists in HEAD but is absent from the working tree — content is
    //       in HEAD and by definition reachable; always safe to restore.
    //   M = file differs between HEAD and working tree — must verify the on-disk
    //       blob is reachable before treating it as safely fixable.
    let status_out = match std::process::Command::new("git")
        .args(["diff-index", "--name-status", "HEAD"])
        .current_dir(repo)
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return Some(WorkingTreeDriftKind::LiveEdits),
    };
    let mut modified_files: Vec<String> = Vec::new();
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
                // Deleted from working tree; restore from HEAD → safely fixable.
            }
            "M" | "T" => {
                modified_files.push(path.to_owned());
            }
            _ => return Some(WorkingTreeDriftKind::LiveEdits),
        }
    }
    if !has_entries {
        return None;
    }
    if modified_files.is_empty() {
        // Only D (deleted-from-WT) entries — always safely restorable.
        return Some(WorkingTreeDriftKind::SafeToFix);
    }

    // Gather all reachable object SHAs from the last 200 commits.
    let objects_out = match std::process::Command::new("git")
        .args(["rev-list", "--objects", "-n", "200", "HEAD"])
        .current_dir(repo)
        .output()
    {
        Ok(out) if out.status.success() => out,
        _ => return Some(WorkingTreeDriftKind::LiveEdits),
    };
    let reachable: std::collections::HashSet<String> = String::from_utf8(objects_out.stdout)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(|s| s.to_owned()))
        .collect();

    // For each M file, verify its on-disk blob is reachable.
    for file in &modified_files {
        let hash_out = match std::process::Command::new("git")
            .args(["hash-object", file])
            .current_dir(repo)
            .output()
        {
            Ok(out) if out.status.success() => out,
            _ => return Some(WorkingTreeDriftKind::LiveEdits),
        };
        let blob_sha = String::from_utf8_lossy(&hash_out.stdout).trim().to_owned();
        if !reachable.contains(&blob_sha) {
            return Some(WorkingTreeDriftKind::LiveEdits);
        }
    }

    Some(WorkingTreeDriftKind::SafeToFix)
}

/// Restore working-tree files to match HEAD.
///
/// Only call after confirming `classify_working_tree_drift` returns `SafeToFix`.
/// Restores each tracked file that differs from HEAD using
/// `git checkout HEAD -- <files>`, leaving unstaged files and the index alone.
pub fn restore_working_tree_to_head(repo: &Path) -> anyhow::Result<()> {
    let modified_out = std::process::Command::new("git")
        .args(["diff-index", "--name-only", "HEAD"])
        .current_dir(repo)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git diff-index: {e}"))?;
    let files: Vec<String> = String::from_utf8_lossy(&modified_out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_owned())
        .collect();
    if files.is_empty() {
        return Ok(());
    }

    let mut args = vec!["checkout".to_owned(), "HEAD".to_owned(), "--".to_owned()];
    args.extend(files);
    let out = std::process::Command::new("git")
        .args(&args)
        .current_dir(repo)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run git checkout: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!(
            "git checkout HEAD -- <files> failed in {}: {}",
            repo.display(),
            stderr.trim()
        );
    }
    Ok(())
}

/// Execute `rwv check --locked` for the current workspace context.
///
/// Compares each repo's HEAD SHA against its `rwv.lock` entry. Outputs per-repo
/// status to stdout. Returns `Ok(true)` if any repo's tip differs from its lock
/// entry (exit 1), `Ok(false)` if all match (exit 0).
pub fn run_check_locked(cwd: &std::path::Path) -> anyhow::Result<bool> {
    use crate::git::GitVcs;
    use crate::manifest::Project;
    use crate::vcs::Vcs;
    use crate::workspace::{WorkspaceContext, WorkspaceLocation};

    let ctx = WorkspaceContext::resolve(cwd, None)?;
    let git = GitVcs;
    let workspace_dir = ctx.resolve_path().to_path_buf();

    let project_names: Vec<String> = match &ctx.location {
        WorkspaceLocation::Weave { project: Some(p) } => vec![p.as_str().to_owned()],
        WorkspaceLocation::Workweave { project, .. } => vec![project.as_str().to_owned()],
        WorkspaceLocation::Weave { project: None } => {
            crate::workspace::discover_project_paths(&ctx.root)
        }
    };

    let mut any_drift = false;

    for pname in &project_names {
        let project_dir = ctx.root.join("projects").join(pname);
        let project = match Project::from_dir(&project_dir) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let lock = match project.lock {
            Some(l) => l,
            None => continue,
        };

        for (repo_path, lock_entry) in &lock.repositories {
            let repo_abs = workspace_dir.join(repo_path.as_path());

            let actual = match git.head_revision(&repo_abs) {
                Ok(rev) => rev,
                Err(_) => {
                    println!("{repo_path}: not on disk (lock: {})", lock_entry.version);
                    any_drift = true;
                    continue;
                }
            };

            // Resolve the lock version to a commit SHA before comparing.
            // Handles tag names, branch names, and SHAs uniformly.
            let lock_sha = match GitVcs::resolve_revision(&repo_abs, lock_entry.version.as_str()) {
                Ok(sha) => sha,
                Err(_) => {
                    println!(
                        "{repo_path}: lock pins unknown revision {}",
                        lock_entry.version
                    );
                    any_drift = true;
                    continue;
                }
            };

            if actual.as_str() == lock_sha.as_str() {
                println!("{repo_path}: ok");
            } else {
                println!("{repo_path}: tip {} ≠ lock {}", actual, lock_entry.version);
                any_drift = true;
            }
        }
    }

    Ok(any_drift)
}

/// Execute `rwv doctor` for the current workspace context.
///
/// Scans registry directories for repos on disk, loads all project manifests,
/// runs convention checks and integration check hooks, then displays issues.
/// When `fix` is `true`, safely-auto-fixable index-drift cases are remediated
/// in place with `git reset` (index ← HEAD, working tree untouched).
///
/// Returns `Ok(true)` if there are errors (exit 1), `Ok(false)` if clean.
pub fn run_check(cwd: &std::path::Path, fix: bool) -> anyhow::Result<bool> {
    use crate::git::GitVcs;
    use crate::integration::Severity;
    use crate::integration_runner::run_checks;
    use crate::manifest::Project;
    use crate::vcs::Vcs;
    use crate::workspace::{WorkspaceContext, WorkspaceLocation, WorkspaceSession};

    let ctx = WorkspaceContext::resolve(cwd, None)?;

    // Build session (runs builtin_registries → scan_repos_on_disk → discover_project_paths).
    let session = WorkspaceSession::new(&ctx.root);
    let git = GitVcs;

    // Resolve HEAD revisions for each repo on disk.
    let mut head_revisions = BTreeMap::new();
    for repo_path in session.repos_on_disk() {
        let abs = ctx.root.join(repo_path.as_path());
        if let Ok(rev) = git.head_revision(&abs) {
            head_revisions.insert(repo_path.clone(), rev);
        }
    }

    // Load all project manifests from projects/*/rwv.yaml
    let projects_dir = ctx.root.join("projects");
    let mut projects = Vec::new();
    let mut known_repos = BTreeSet::new();

    if projects_dir.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(&projects_dir)?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let project_dir = entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            let manifest_path = project_dir.join("rwv.yaml");
            if !manifest_path.exists() {
                continue;
            }
            // Use relative path for Project::from_dir so project name derivation works
            let rel_dir = project_dir.strip_prefix(&ctx.root).unwrap_or(&project_dir);
            match Project::from_dir(&project_dir) {
                Ok(mut project) => {
                    // Fix the project name to use relative path
                    let name_from_rel = rel_dir
                        .strip_prefix("projects")
                        .unwrap_or(rel_dir)
                        .to_string_lossy()
                        .into_owned();
                    project.name = crate::manifest::ProjectName::new(name_from_rel);

                    for repo_path in project.manifest.repositories.keys() {
                        known_repos.insert(repo_path.clone());
                    }
                    projects.push(project);
                }
                Err(e) => {
                    eprintln!(
                        "warning: failed to load project at {}: {e}",
                        project_dir.display()
                    );
                }
            }
        }
    }

    // Build CheckInput and find violations
    let input = CheckInput {
        known_repos,
        repos_on_disk: session.repos_on_disk().to_vec(),
        projects,
        head_revisions,
    };

    let violations = find_violations(&input);
    let mut all_issues = violations_to_issues(violations);

    // Run integration check hooks for each project
    let builtin = crate::integrations::builtin_integrations();
    let integrations: Vec<&dyn crate::integration::Integration> =
        builtin.iter().map(|b| b.as_ref()).collect();

    for project in &input.projects {
        let detection_cache = crate::integration_runner::build_detection_cache(
            &ctx.root,
            &project.manifest.repositories,
        );
        let ctx_base = session.context_base(&ctx.root, &project.name, &detection_cache);
        let integration_issues = run_checks(&integrations, &project.manifest, &ctx_base);
        all_issues.extend(integration_issues);
    }

    // Index-drift detection: check repos in the current workspace and, when
    // running from the primary weave, all workweave repos too.
    //
    // Collects (workweave_label, repo_abs, repo_path_display) triples.
    let mut index_scan: Vec<(Option<String>, std::path::PathBuf, String)> = Vec::new();

    let workspace_dir = ctx.resolve_path().to_path_buf();
    for project in &input.projects {
        for repo_path in project.manifest.repositories.keys() {
            let abs = workspace_dir.join(repo_path.as_path());
            if abs.exists() {
                index_scan.push((None, abs, repo_path.to_string()));
            }
        }
    }

    // From the primary weave: also scan every known workweave.
    if matches!(ctx.location, WorkspaceLocation::Weave { .. }) {
        for (ww_name, ww_dir) in crate::workweave::list_workweave_dirs(&ctx.root) {
            for project in &input.projects {
                for repo_path in project.manifest.repositories.keys() {
                    let abs = ww_dir.join(repo_path.as_path());
                    if abs.exists() {
                        index_scan.push((Some(ww_name.clone()), abs, repo_path.to_string()));
                    }
                }
            }
        }
    }

    for (ww_label, repo_abs, repo_display) in &index_scan {
        let location = match ww_label {
            Some(ww) => format!("{ww}/{repo_display}"),
            None => repo_display.clone(),
        };

        if let Some(drift_kind) = classify_index_drift(repo_abs) {
            match drift_kind {
                IndexDriftKind::SafeToFix => {
                    if fix {
                        match reset_index_to_head(repo_abs) {
                            Ok(()) => println!("[fixed] core: index refreshed for {location}"),
                            Err(e) => all_issues.push(Issue {
                                integration: "core".into(),
                                severity: Severity::Error,
                                message: format!("{location}: index fix failed: {e}"),
                            }),
                        }
                    } else {
                        all_issues.push(Issue {
                            integration: "core".into(),
                            severity: Severity::Warning,
                            message: format!("{location}: index stale (safe to --fix)"),
                        });
                    }
                }
                IndexDriftKind::LiveStaged => {
                    all_issues.push(Issue {
                        integration: "core".into(),
                        severity: Severity::Warning,
                        message: format!(
                            "{location}: index has live staged changes (manual review)"
                        ),
                    });
                }
            }
        }
    }

    // Working-tree drift detection: same scan list, same workweave scope.
    // Uses `git diff-index HEAD` (without --cached) so it works whether or not
    // index drift has just been fixed above.
    for (ww_label, repo_abs, repo_display) in &index_scan {
        let location = match ww_label {
            Some(ww) => format!("{ww}/{repo_display}"),
            None => repo_display.clone(),
        };

        if let Some(drift_kind) = classify_working_tree_drift(repo_abs) {
            match drift_kind {
                WorkingTreeDriftKind::SafeToFix => {
                    if fix {
                        match restore_working_tree_to_head(repo_abs) {
                            Ok(()) => {
                                println!("[fixed] core: working tree refreshed for {location}")
                            }
                            Err(e) => all_issues.push(Issue {
                                integration: "core".into(),
                                severity: Severity::Error,
                                message: format!("{location}: working-tree fix failed: {e}"),
                            }),
                        }
                    } else {
                        all_issues.push(Issue {
                            integration: "core".into(),
                            severity: Severity::Warning,
                            message: format!("{location}: working tree stale (safe to --fix)"),
                        });
                    }
                }
                WorkingTreeDriftKind::LiveEdits => {
                    all_issues.push(Issue {
                        integration: "core".into(),
                        severity: Severity::Warning,
                        message: format!("{location}: working tree has live edits (manual review)"),
                    });
                }
            }
        }
    }

    // Display issues and determine exit status
    let mut has_errors = false;
    for issue in &all_issues {
        let prefix = match issue.severity {
            Severity::Warning => "warning",
            Severity::Error => {
                has_errors = true;
                "error"
            }
        };
        // The tests check stdout for the issue messages
        println!("[{prefix}] {}: {}", issue.integration, issue.message);
    }

    Ok(has_errors)
}
