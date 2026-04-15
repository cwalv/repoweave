//! Convention checks: orphaned clones, dangling refs, stale locks, etc.
//!
//! `rwv doctor` builds a workspace-wide inventory from all projects, then runs
//! a series of checks. Integration check hooks are run separately.

use crate::integration::Issue;
use crate::manifest::{Project, RepoPath};
use crate::vcs::RevisionId;
use std::collections::{BTreeMap, BTreeSet};

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
}

#[derive(Debug)]
pub enum DriftKind {
    /// Manifest lists it, but no worktree exists.
    Missing,
    /// Worktree exists, but manifest doesn't list it.
    Extra,
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
            };
            Issue {
                integration: "core".into(),
                severity,
                message,
            }
        })
        .collect()
}

/// Execute `rwv doctor` for the current workspace context.
///
/// Scans registry directories for repos on disk, loads all project manifests,
/// runs convention checks and integration check hooks, then displays issues.
/// Returns `Ok(true)` if there are errors (exit 1), `Ok(false)` if clean.
pub fn run_check(cwd: &std::path::Path) -> anyhow::Result<bool> {
    use crate::git::GitVcs;
    use crate::integration::Severity;
    use crate::integration_runner::run_checks;
    use crate::manifest::Project;
    use crate::vcs::Vcs;
    use crate::workspace::{WorkspaceContext, WorkspaceSession};

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
