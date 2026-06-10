//! Convention checks: orphaned clones, dangling refs, stale locks, index drift, working-tree drift, etc.
//!
//! `rwv doctor` builds a workspace-wide inventory from all projects, then runs
//! a series of checks. Integration check hooks are run separately.

use crate::git::git_command;
use crate::integration::Issue;
use crate::manifest::{Project, ProjectName, RepoPath, Role, WorkweaveName};
use crate::vcs::ResolvedRevisionId;
use anyhow::Context;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
    DanglingReference {
        project: ProjectName,
        repo: RepoPath,
    },

    /// An `rwv.yaml` entry missing the `role` field.
    MissingRole {
        project: ProjectName,
        repo: RepoPath,
    },

    /// A project's `rwv.lock` doesn't match current HEAD SHAs.
    StaleLock {
        project: ProjectName,
        repo: RepoPath,
        locked: ResolvedRevisionId,
        actual: ResolvedRevisionId,
    },

    /// A worktree missing from a workweave, or an extra worktree not in the manifest.
    WorkweaveDrift {
        workweave: WorkweaveName,
        kind: DriftKind,
        repo: RepoPath,
    },

    /// A git repo's index does not match its HEAD tree (silent stale-index from
    /// shared-ref advance in a sibling worktree).
    IndexDrift {
        /// Workweave name; `None` for repos in the primary weave.
        workweave: Option<WorkweaveName>,
        repo: RepoPath,
        kind: IndexDriftKind,
    },

    /// A git repo's working-tree files do not match its HEAD tree (stale on-disk
    /// content after shared-ref advance in a sibling worktree).
    WorkingTreeDrift {
        workweave: Option<WorkweaveName>,
        repo: RepoPath,
        kind: WorkingTreeDriftKind,
    },

    /// A project repo is missing the `rwv.lock merge=ours` entry in
    /// `.gitattributes`. Without it, `rwv sync`'s native rebase would carry
    /// user lock-edits through the merge inputs instead of letting Phase 3
    /// regenerate them. Auto-fixable: append the line.
    MissingReplayExclusion { project: ProjectName },

    /// A project's `rwv.yaml` uses the legacy `role: primary` spelling
    /// (replaced by `role: owned`; the back-compat alias has since been
    /// dropped). Auto-fixable: rewrite each affected line in place,
    /// preserving comments and key order.
    LegacyRolePrimary {
        /// Project the manifest belongs to (or a synthetic name when the
        /// detector runs without a fully-loaded project — manifests with
        /// `role: primary` can't reach `Project::from_dir` since the
        /// parse fails).
        project: ProjectName,
        /// Absolute path to the offending `rwv.yaml`.
        manifest_path: PathBuf,
    },

    /// `.rwv-active` names a project whose `projects/<name>/` directory does
    /// not exist on disk. Any action verb that reads the active project will
    /// fail with a confusing downstream error. Auto-fixable: clear
    /// `.rwv-active` (or prompt to pick from existing projects under
    /// `--fix`).
    DanglingActiveProject {
        /// The project name recorded in `.rwv-active`.
        project: ProjectName,
        /// The `projects/` directory that does not exist on disk.
        missing_dir: PathBuf,
    },

    /// A `.rwv-workweave` marker file is missing the required `parent:` field
    /// (written before parent tracking landed). Auto-fixable: append
    /// `parent: <primary value>` to the file on disk.
    LegacyWorkweaveMarker {
        /// Absolute path to the offending `.rwv-workweave` file.
        marker_path: PathBuf,
        /// The `primary:` value from the marker, used as the backfill value.
        primary: PathBuf,
    },

    /// A project's `rwv.yaml` exists but cannot be parsed.
    ///
    /// Reported as an `Error`-severity violation so the operator is not
    /// left with zero violations (i.e. an apparent "clean" result) for a
    /// project whose manifest is broken. `--fix` does NOT auto-repair this
    /// — the operator must fix the YAML by hand and re-run `rwv doctor`.
    UnparseableProject {
        /// Relative project path (e.g. `my-app`, `org/repo`).
        project: ProjectName,
        /// Absolute path to the offending `rwv.yaml`.
        manifest_path: PathBuf,
        /// Free-form display string of the parse error (from `anyhow::Error::to_string`).
        /// No structured parse-error type is available at this boundary.
        message: String,
    },
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum DriftKind {
    /// Manifest lists it, but no worktree exists.
    Missing,
    /// Worktree exists, but manifest doesn't list it.
    Extra,
}

/// How a stale index should be treated.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum IndexDriftKind {
    /// Index tree matches the tree of some recent ancestor commit. Safe to
    /// auto-fix with `git reset` — the displaced tree is permanently in the DAG.
    SafeToFix,
    /// Index tree is not found in recent ancestor trees. The user has live
    /// staged content; `--fix` must not touch this.
    LiveStaged,
}

/// How stale working-tree files should be treated.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WorkingTreeDriftKind {
    /// All modified files' on-disk content matches blobs reachable from HEAD.
    /// Safe to restore with `git checkout HEAD -- <files>` — no work is lost.
    SafeToFix,
    /// At least one modified file has on-disk content not found in any recent
    /// ancestor's tree. The user has active edits; `--fix` must not touch this.
    LiveEdits,
}

// ---------------------------------------------------------------------------
// ViolationOutput — wire-format mirror of CheckViolation for `--json`
// ---------------------------------------------------------------------------
//
// The internal `CheckViolation` enum carries a `RepoPath` (manifest-relative).
// The wire shape needs both `path` (manifest-relative string) and
// `absolute_path` (resolved against the workspace root or workweave dir),
// which the internal type cannot supply alone. We mirror the variants here
// and convert at serialize time via [`ViolationOutput::from_violation`].
//
// The kebab-case tag mapping:
//     OrphanedClone       -> "orphaned-clone"
//     DanglingReference   -> "dangling-reference"
//     MissingRole         -> "missing-role"
//     StaleLock           -> "stale-lock"
//     WorkweaveDrift      -> "workweave-drift"  (sub-kind via `DriftKind`)
//     IndexDrift          -> "index-drift"      (sub-kind via `IndexDriftKind`)
//     WorkingTreeDrift    -> "working-tree-drift" (sub-kind via `WorkingTreeDriftKind`)
//     MissingReplayExclusion -> "missing-replay-exclusion"

/// One violation as it appears in `rwv doctor --json` output.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ViolationOutput {
    OrphanedClone {
        path: String,
        absolute_path: String,
    },
    DanglingReference {
        path: String,
        absolute_path: String,
        project: String,
    },
    MissingRole {
        path: String,
        absolute_path: String,
        project: String,
    },
    StaleLock {
        path: String,
        absolute_path: String,
        project: String,
        locked: String,
        actual: String,
    },
    WorkweaveDrift {
        path: String,
        absolute_path: String,
        workweave: String,
        #[serde(rename = "sub_kind")]
        sub_kind: DriftKind,
    },
    IndexDrift {
        path: String,
        absolute_path: String,
        /// `None` for the primary weave.
        workweave: Option<String>,
        #[serde(rename = "sub_kind")]
        sub_kind: IndexDriftKind,
    },
    WorkingTreeDrift {
        path: String,
        absolute_path: String,
        /// `None` for the primary weave.
        workweave: Option<String>,
        #[serde(rename = "sub_kind")]
        sub_kind: WorkingTreeDriftKind,
    },
    MissingReplayExclusion {
        project: String,
    },
    LegacyRolePrimary {
        project: String,
        manifest_path: String,
    },
    DanglingActiveProject {
        project: String,
        missing_dir: String,
    },
    LegacyWorkweaveMarker {
        marker_path: String,
        primary: String,
    },
    UnparseableProject {
        project: String,
        manifest_path: String,
        /// Free-form display string of the YAML parse error. Named `message`
        /// (not `error`) to signal this is display text, not a typed discriminant.
        message: String,
    },
}

impl ViolationOutput {
    /// Convert an internal [`CheckViolation`] into its wire-format
    /// counterpart, resolving `path` against `workspace_dir` for
    /// non-workweave variants and against `workweave_dirs` for
    /// workweave-scoped variants.
    pub fn from_violation(
        violation: CheckViolation,
        workspace_dir: &Path,
        workweave_dirs: &std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
    ) -> Self {
        fn abs(workspace_dir: &Path, repo: &RepoPath) -> String {
            workspace_dir
                .join(repo.as_path())
                .to_string_lossy()
                .into_owned()
        }
        fn abs_in(
            workweave: &Option<WorkweaveName>,
            workspace_dir: &Path,
            workweave_dirs: &std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
            repo: &RepoPath,
        ) -> String {
            match workweave {
                Some(ww) => match workweave_dirs.get(ww) {
                    Some(dir) => dir.join(repo.as_path()).to_string_lossy().into_owned(),
                    None => workspace_dir
                        .join(repo.as_path())
                        .to_string_lossy()
                        .into_owned(),
                },
                None => workspace_dir
                    .join(repo.as_path())
                    .to_string_lossy()
                    .into_owned(),
            }
        }

        match violation {
            CheckViolation::OrphanedClone { path } => Self::OrphanedClone {
                absolute_path: abs(workspace_dir, &path),
                path: path.to_string(),
            },
            CheckViolation::DanglingReference { project, repo } => Self::DanglingReference {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.to_string(),
            },
            CheckViolation::MissingRole { project, repo } => Self::MissingRole {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.to_string(),
            },
            CheckViolation::StaleLock {
                project,
                repo,
                locked,
                actual,
            } => Self::StaleLock {
                absolute_path: abs(workspace_dir, &repo),
                path: repo.to_string(),
                project: project.to_string(),
                locked: locked.display_str().to_owned(),
                actual: actual.display_str().to_owned(),
            },
            CheckViolation::WorkweaveDrift {
                workweave,
                kind,
                repo,
            } => {
                let dir_for_ww = workweave_dirs
                    .get(&workweave)
                    .cloned()
                    .unwrap_or_else(|| workspace_dir.to_path_buf());
                Self::WorkweaveDrift {
                    absolute_path: dir_for_ww
                        .join(repo.as_path())
                        .to_string_lossy()
                        .into_owned(),
                    path: repo.to_string(),
                    workweave: workweave.to_string(),
                    sub_kind: kind,
                }
            }
            CheckViolation::IndexDrift {
                workweave,
                repo,
                kind,
            } => Self::IndexDrift {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.map(|w| w.to_string()),
                sub_kind: kind,
            },
            CheckViolation::WorkingTreeDrift {
                workweave,
                repo,
                kind,
            } => Self::WorkingTreeDrift {
                absolute_path: abs_in(&workweave, workspace_dir, workweave_dirs, &repo),
                path: repo.to_string(),
                workweave: workweave.map(|w| w.to_string()),
                sub_kind: kind,
            },
            CheckViolation::MissingReplayExclusion { project } => Self::MissingReplayExclusion {
                project: project.to_string(),
            },
            CheckViolation::LegacyRolePrimary {
                project,
                manifest_path,
            } => Self::LegacyRolePrimary {
                project: project.to_string(),
                manifest_path: manifest_path.to_string_lossy().into_owned(),
            },
            CheckViolation::DanglingActiveProject {
                project,
                missing_dir,
            } => Self::DanglingActiveProject {
                project: project.to_string(),
                missing_dir: missing_dir.to_string_lossy().into_owned(),
            },
            CheckViolation::LegacyWorkweaveMarker {
                marker_path,
                primary,
            } => Self::LegacyWorkweaveMarker {
                marker_path: marker_path.to_string_lossy().into_owned(),
                primary: primary.to_string_lossy().into_owned(),
            },
            CheckViolation::UnparseableProject {
                project,
                manifest_path,
                message,
            } => Self::UnparseableProject {
                project: project.to_string(),
                manifest_path: manifest_path.to_string_lossy().into_owned(),
                message,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy-role-primary scanning
// ---------------------------------------------------------------------------

/// One project manifest carrying the legacy `role: primary` spelling.
///
/// Carries both the project name and the absolute manifest path so the
/// finding can be reported (without --fix) and rewritten (with --fix)
/// without re-walking the workspace.
#[derive(Debug, Clone)]
pub struct LegacyRolePrimaryManifest {
    pub project: ProjectName,
    pub manifest_path: PathBuf,
}

/// Walk every `projects/*/rwv.yaml` under `workspace_dir` and collect
/// manifests that contain the legacy `role: primary` spelling.
///
/// Pre-parse text scan — the doctor needs to detect the legacy spelling
/// *before* `Project::from_dir`, since the parser rejects it. Without
/// this scan, the only signal would be the parse error from
/// `Project::from_dir`, which doesn't fan out across all manifests in
/// the workspace.
pub fn scan_workspace_for_legacy_role_primary(
    workspace_dir: &Path,
) -> Vec<LegacyRolePrimaryManifest> {
    let projects_dir = workspace_dir.join("projects");
    let mut found = Vec::new();
    if !projects_dir.is_dir() {
        return found;
    }
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return found,
    };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let project_dir = entry.path();
        if !project_dir.is_dir() {
            continue;
        }
        scan_project_dir_for_legacy(&projects_dir, &project_dir, &mut found);
    }
    found
}

/// Recursively walk a project directory in `projects/` for `rwv.yaml`
/// files using `role: primary`. Project names are derived as the
/// path relative to `projects/` (so `projects/chatly/web-app/rwv.yaml`
/// yields project name `chatly/web-app`), matching the existing
/// nested-project convention used by `Project::from_dir`.
fn scan_project_dir_for_legacy(
    projects_dir: &Path,
    project_dir: &Path,
    out: &mut Vec<LegacyRolePrimaryManifest>,
) {
    let manifest_path = project_dir.join("rwv.yaml");
    if manifest_path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&manifest_path) {
            if crate::manifest::manifest_has_legacy_role_primary(&content) {
                let project_name = project_dir
                    .strip_prefix(projects_dir)
                    .unwrap_or(project_dir)
                    .to_string_lossy()
                    .into_owned();
                out.push(LegacyRolePrimaryManifest {
                    project: ProjectName::new(project_name),
                    manifest_path,
                });
            }
        }
    }
    // Recurse into subdirectories for the `projects/<owner>/<repo>` nested
    // case. Skip `.git` and similar hidden directories.
    if let Ok(entries) = std::fs::read_dir(project_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                scan_project_dir_for_legacy(projects_dir, &path, out);
            }
        }
    }
}

/// Apply the `rwv doctor --fix` migration to a single manifest path.
///
/// Idempotent: if no `role: primary` lines remain, the file is not
/// rewritten and the returned count is `0`. Returns the number of
/// rewritten lines so the caller can print a meaningful "[fixed]" line.
pub fn fix_legacy_role_primary(manifest_path: &Path) -> anyhow::Result<usize> {
    let content = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {} for --fix", manifest_path.display()))?;
    let (new_content, count) = crate::manifest::migrate_legacy_role_primary(&content);
    if count > 0 {
        std::fs::write(manifest_path, new_content)
            .with_context(|| format!("failed to write {} during --fix", manifest_path.display()))?;
    }
    Ok(count)
}

// ---------------------------------------------------------------------------
// Legacy-workweave-marker scanning and fixing
// ---------------------------------------------------------------------------

/// One workweave directory whose `.rwv-workweave` file is missing `parent:`.
#[derive(Debug, Clone)]
pub struct LegacyWorkweaveMarkerFile {
    /// Absolute path to the `.rwv-workweave` file.
    pub marker_path: PathBuf,
    /// The `primary:` value read from the file (used as the backfill value).
    pub primary: PathBuf,
}

/// Walk the workweave parent directory and collect `.rwv-workweave` files that
/// are missing the required `parent:` field.
///
/// A marker is "legacy" if the YAML is valid but `parent:` is absent or null.
/// Files that fail to parse at all are not included (they are a different
/// failure mode).
pub fn scan_for_legacy_workweave_markers(ws_root: &Path) -> Vec<LegacyWorkweaveMarkerFile> {
    let parent_dir = crate::workweave::workweave_parent_pub(ws_root);
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(&parent_dir) {
        Ok(e) => e,
        Err(_) => return found,
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let marker_path = dir.join(".rwv-workweave");
        if !marker_path.is_file() {
            continue;
        }
        let content = match std::fs::read_to_string(&marker_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let raw: serde_yaml::Value = match serde_yaml::from_str(&content) {
            Ok(v) => v,
            Err(_) => continue, // unparseable — not our concern here
        };
        // Legacy if `parent` is absent or null.
        if raw.get("parent").map(|v| v.is_null()).unwrap_or(true) {
            // Extract primary path.
            if let Some(primary_str) = raw.get("primary").and_then(|v| v.as_str()) {
                found.push(LegacyWorkweaveMarkerFile {
                    marker_path,
                    primary: PathBuf::from(primary_str),
                });
            }
        }
    }
    found.sort_by(|a, b| a.marker_path.cmp(&b.marker_path));
    found
}

/// Append `parent: <primary>` to a legacy `.rwv-workweave` file.
///
/// Idempotent: if `parent:` is already present, the file is not rewritten.
/// Returns `true` if the file was rewritten, `false` if it was already
/// up to date.
pub fn fix_legacy_workweave_marker(finding: &LegacyWorkweaveMarkerFile) -> anyhow::Result<bool> {
    let content = std::fs::read_to_string(&finding.marker_path)
        .with_context(|| format!("failed to read {} for --fix", finding.marker_path.display()))?;
    let raw: serde_yaml::Value = serde_yaml::from_str(&content).with_context(|| {
        format!(
            "failed to re-parse {} for --fix",
            finding.marker_path.display()
        )
    })?;
    // Re-check: don't rewrite if already has a non-null parent.
    if !raw.get("parent").map(|v| v.is_null()).unwrap_or(true) {
        return Ok(false);
    }
    // Append the parent line. Using a simple string append preserves any
    // comments and existing key order, consistent with fix_legacy_role_primary.
    let primary_str = finding.primary.to_string_lossy();
    let line = format!("parent: {primary_str}\n");
    let new_content = if content.ends_with('\n') {
        format!("{content}{line}")
    } else {
        format!("{content}\n{line}")
    };
    std::fs::write(&finding.marker_path, new_content).with_context(|| {
        format!(
            "failed to write {} during --fix",
            finding.marker_path.display()
        )
    })?;
    Ok(true)
}

/// `$schema` URL embedded in `rwv doctor --json` output. Points at the
/// committed schema artifact in the main branch (Agent D regenerates this
/// file via `cargo run --bin generate-schemas` and CI fails on drift).
pub const DOCTOR_SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/cwalv/repoweave/main/docs/reference/schemas/doctor.json";

/// Inputs for running workspace-wide checks.
pub struct CheckInput {
    /// All repos referenced by any project's `rwv.yaml`.
    pub known_repos: BTreeSet<RepoPath>,
    /// All git repos found on disk under registry directories.
    pub repos_on_disk: Vec<RepoPath>,
    /// Loaded projects.
    pub projects: Vec<Project>,
    /// Resolved HEAD revisions for repos on disk, keyed by RepoPath.
    pub head_revisions: BTreeMap<RepoPath, ResolvedRevisionId>,
    /// Resolved lock files keyed by project name. Built by the caller via
    /// [`crate::manifest::LockFile::resolve_versions`] before invoking
    /// [`find_violations`]; only projects whose lock could be resolved
    /// appear here. The split out of `Project.lock` (which stays raw)
    /// keeps the parse/resolve boundary explicit at the type level.
    pub resolved_locks: std::collections::HashMap<ProjectName, crate::manifest::ResolvedLockFile>,
    /// When `false`, the orphan-clone check is skipped. Orphan detection is
    /// inherently weave-wide (a repo is only "orphaned" if it belongs to *no*
    /// project), so running it in single-project mode would produce false
    /// positives for repos that belong to other projects. Set to `true` only
    /// when all projects have been loaded into `projects` (i.e. `--all` mode).
    pub check_orphans: bool,
}

/// Collect all convention violations from the check inputs.
///
/// This is a pure function: it takes data in, returns violations out.
/// Filesystem access (reading HEADs, scanning directories) happens
/// before this function is called.
pub fn find_violations(input: &CheckInput) -> Vec<CheckViolation> {
    let mut violations = Vec::new();

    // Orphaned clones: on disk but not in any project.
    // Only run when check_orphans is true (i.e. all projects are loaded).
    // In single-project mode, a repo absent from the active project may still
    // belong to another project — flagging it as orphaned would be a false positive.
    if input.check_orphans {
        for repo_path in &input.repos_on_disk {
            if !input.known_repos.contains(repo_path) {
                violations.push(CheckViolation::OrphanedClone {
                    path: repo_path.clone(),
                });
            }
        }
    }

    // Per-project checks
    for project in &input.projects {
        for (repo_path, entry) in project.manifest.iter_entries() {
            // Dangling reference: in manifest but not on disk.
            // Reference repos are allowed to be missing (e.g. after fetch --no-reference).
            if !input.repos_on_disk.contains(repo_path) && entry.role != Role::Reference {
                violations.push(CheckViolation::DanglingReference {
                    project: project.name.clone(),
                    repo: repo_path.clone(),
                });
            }
        }

        // Compare lock entries against resolved HEADs. The lock entries
        // are pulled from `input.resolved_locks` (built by the caller via
        // `LockFile::resolve_versions`), so equality is purely a
        // canonical-SHA comparison — the raw-vs-resolved confusion that
        // produced the historical B3/B6 bugs is now a compile-time
        // impossibility.
        if let Some(lock) = input.resolved_locks.get(&project.name) {
            for (repo_path, lock_entry) in lock.iter_entries() {
                if let Some(actual_rev) = input.head_revisions.get(repo_path) {
                    if &lock_entry.version != actual_rev {
                        violations.push(CheckViolation::StaleLock {
                            project: project.name.clone(),
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
                CheckViolation::MissingReplayExclusion { project } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{project}: project repo missing `rwv.lock merge=ours` in .gitattributes \
                         (run `rwv doctor --fix` to add)"
                    ),
                ),
                CheckViolation::LegacyRolePrimary {
                    project,
                    manifest_path,
                } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{project}: manifest at {} uses deprecated `role: primary`; \
                         run `rwv doctor --fix` to migrate to `role: owned`",
                        manifest_path.display()
                    ),
                ),
                CheckViolation::LegacyWorkweaveMarker { marker_path, .. } => (
                    crate::integration::Severity::Warning,
                    format!(
                        "{} is a legacy workweave marker missing `parent:`; \
                         run `rwv doctor --fix` to migrate",
                        marker_path.display()
                    ),
                ),
                CheckViolation::UnparseableProject {
                    project,
                    manifest_path,
                    message,
                } => (
                    crate::integration::Severity::Error,
                    format!(
                        "{project}: manifest at {} cannot be parsed: {message}",
                        manifest_path.display()
                    ),
                ),
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
                CheckViolation::DanglingActiveProject {
                    project,
                    missing_dir,
                } => (
                    crate::integration::Severity::Error,
                    format!(
                        "active project `{}` is set in `.rwv-active` but `{}` does not exist; \
                         run `rwv activate <existing-project>` or remove `.rwv-active`",
                        project,
                        missing_dir.display()
                    ),
                ),
            };
            Issue {
                integration: "core".into(),
                severity,
                message,
                safe_to_fix: true,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Index-drift helpers
// ---------------------------------------------------------------------------

/// Combined index + working-tree drift classification using one git
/// invocation for the common "clean worktree" fast path.
///
/// The doctor's per-worktree scan calls both [`classify_index_drift`] and
/// [`classify_working_tree_drift`] for every worktree it sees. Each of those
/// functions previously paid the cost of a fresh `git diff-index` subprocess
/// just to determine "clean vs dirty" before doing any further work — so the
/// minimum cost per worktree was two `git` invocations, and at workspace
/// scale (80+ workweaves × ~13 repos = ~1000 worktrees) the doubled
/// process-startup overhead dominated wall-clock time.
///
/// This helper runs `git status --porcelain` once. If the output is empty
/// (workspace and index both clean against HEAD), both classifiers return
/// `None` without spawning any further subprocesses. If the output is
/// non-empty, the caller falls back to the per-kind classifiers, which
/// continue to issue the detail probes needed to bucket the drift into
/// `SafeToFix` / `LiveStaged` / `LiveEdits`.
///
/// Returns `(index_drift, working_tree_drift)`. Either side may be `None`
/// independently — e.g., index dirty but working tree clean.
pub fn classify_drift(repo: &Path) -> (Option<IndexDriftKind>, Option<WorkingTreeDriftKind>) {
    // `git status --porcelain` (without `-z`) emits one record per dirty
    // entry; an empty stdout means both the index and the working tree
    // match HEAD, which is the overwhelmingly common case in a healthy
    // workspace. We only need the empty/non-empty signal here; the
    // detail-level classification still goes through the per-kind helpers
    // below when drift is detected.
    let status_out = git_command()
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .stderr(std::process::Stdio::null())
        .output();
    let porcelain = match status_out {
        Ok(out) if out.status.success() => out.stdout,
        // Treat any error reading status as "potentially dirty" and fall
        // through to the per-kind classifiers — they're already defensive
        // about transient git failures.
        _ => {
            return (
                classify_index_drift(repo),
                classify_working_tree_drift(repo),
            )
        }
    };
    if porcelain.is_empty() {
        return (None, None);
    }
    (
        classify_index_drift(repo),
        classify_working_tree_drift(repo),
    )
}

/// Classify the index-drift state of a git repo at `repo`.
///
/// Returns `None` when the index matches HEAD (no drift).  Otherwise returns
/// `Some(IndexDriftKind)` — either `SafeToFix` (index tree is an ancestor
/// commit's tree, safely replaceable) or `LiveStaged` (user has staged content
/// that is not a committed tree; must not be auto-fixed).
///
/// For workspace-scale scans (where most worktrees are clean), prefer
/// [`classify_drift`] — it short-circuits both this check and
/// [`classify_working_tree_drift`] with a single `git status` invocation.
pub fn classify_index_drift(repo: &Path) -> Option<IndexDriftKind> {
    // Exit-0 means index matches HEAD tree — no drift.
    let clean = git_command()
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
    let index_tree = match git_command().arg("write-tree").current_dir(repo).output() {
        Ok(out) if out.status.success() => String::from_utf8(out.stdout)
            .unwrap_or_default()
            .trim()
            .to_owned(),
        _ => return Some(IndexDriftKind::LiveStaged), // conservative
    };

    // Safety check: is the index tree the tree of some recent ancestor commit?
    // Bounded to 200 ancestors to keep performance acceptable on deep histories.
    let ancestor_trees = match git_command()
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
    let out = git_command()
        .arg("reset")
        .current_dir(repo)
        .output()
        .context("failed to run git reset")?;
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
///
/// For workspace-scale scans, prefer [`classify_drift`] — it short-circuits
/// the clean-worktree case with a single `git status` invocation that
/// covers both this check and [`classify_index_drift`].
pub fn classify_working_tree_drift(repo: &Path) -> Option<WorkingTreeDriftKind> {
    // Exit-0 means working tree matches HEAD — no drift.
    let clean = git_command()
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
    let status_out = match git_command()
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
    let objects_out = match git_command()
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
        let hash_out = match git_command()
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
    let modified_out = git_command()
        .args(["diff-index", "--name-only", "HEAD"])
        .current_dir(repo)
        .output()
        .context("failed to run git diff-index")?;
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
    let out = git_command()
        .args(&args)
        .current_dir(repo)
        .output()
        .context("failed to run git checkout")?;
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

/// Execute `rwv doctor --locked` for the current workspace context.
///
/// Compares each repo's HEAD SHA against its `rwv.lock` entry. Outputs per-repo
/// status to stdout. Returns `Ok(true)` if any repo's tip differs from its lock
/// entry (exit 1), `Ok(false)` if all match (exit 0).
///
/// When `project_override` is `Some`, only that project is checked
/// (does not change `.rwv-active`).
pub fn run_check_locked(
    cwd: &std::path::Path,
    project_override: Option<crate::manifest::ProjectName>,
) -> anyhow::Result<bool> {
    use crate::git::GitVcs;
    use crate::manifest::Project;
    use crate::vcs::Vcs;
    use crate::workspace::{WorkspaceContext, WorkspaceLocation};

    let ctx = WorkspaceContext::resolve(cwd, project_override)?;
    let git = GitVcs;
    let workspace_dir = ctx.active_path().to_path_buf();

    let project_names: Vec<String> = match &ctx.location {
        WorkspaceLocation::Weave { project: Some(p) } => vec![p.as_str().to_owned()],
        WorkspaceLocation::Workweave { project, .. } => vec![project.as_str().to_owned()],
        WorkspaceLocation::Weave { project: None } => {
            crate::workspace::discover_project_paths(&workspace_dir)
        }
    };

    let mut any_drift = false;

    for pname in &project_names {
        let project_dir = workspace_dir.join("projects").join(pname);
        let project = match Project::from_dir(&project_dir) {
            Ok(p) => p,
            Err(e) => {
                // Warn and skip; `rwv doctor` surfaces the canonical
                // `unparseable-project` violation for this project.
                eprintln!(
                    "warning: skipping project {pname}: manifest unreadable ({e}); \
                     run `rwv doctor` for details"
                );
                continue;
            }
        };

        let raw_lock = match project.lock {
            Some(l) => l,
            None => continue,
        };

        // Resolve lock entries against on-disk repos. Repos whose revision
        // can't be resolved (unknown tag/branch) come back in `failures`
        // along with the raw string, so we can report them with a distinct
        // "unknown revision" message. The raw lock is iterated below so
        // that "missing on disk" entries (which are silently dropped by
        // `resolve_versions`) still get a diagnostic.
        let raw_entries = raw_lock.repo_map().clone();
        let (resolved, failures) = raw_lock.resolve_versions(&workspace_dir);
        let unresolved: std::collections::BTreeMap<RepoPath, crate::vcs::RawRevisionId> =
            failures.into_iter().collect();

        for (repo_path, raw_entry) in &raw_entries {
            let repo_abs = workspace_dir.join(repo_path.as_path());

            let actual = match git.head_revision(&repo_abs) {
                Ok(rev) => rev,
                Err(_) => {
                    println!(
                        "{repo_path}: missing on disk (lock pins {}); run `rwv sync` to materialize",
                        raw_entry.version
                    );
                    any_drift = true;
                    continue;
                }
            };

            if let Some(raw_rev) = unresolved.get(repo_path) {
                println!("{repo_path}: lock pins unknown revision {}", raw_rev);
                any_drift = true;
                continue;
            }

            let Some(resolved_entry) = resolved.get_entry(repo_path) else {
                // Resolve dropped this entry without surfacing it as a
                // failure — shouldn't happen for an on-disk repo with a
                // valid rev, but stay defensive.
                continue;
            };

            if actual == resolved_entry.version {
                println!("{repo_path}: ok");
            } else {
                println!(
                    "{repo_path}: tip {} ≠ lock {}",
                    actual, resolved_entry.version
                );
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
/// When `scope_all` is `false` (the default), only the active project is
/// checked: stale locks, dangling references, and integration hooks are
/// scoped to that project, and orphan detection is skipped (a repo absent
/// from the active project may belong to another project). Pass `scope_all =
/// true` (via `--all`) to reproduce the previous weave-wide behaviour,
/// including orphan detection across every project.
///
/// Returns `Ok(true)` if there are errors (exit 1), `Ok(false)` if clean.
/// When `project_override` is `Some`, the context resolves to that
/// project for the purposes of activation/check scoping (does not
/// change `.rwv-active`).
pub fn run_check(
    cwd: &std::path::Path,
    fix: bool,
    project_override: Option<crate::manifest::ProjectName>,
    scope_all: bool,
) -> anyhow::Result<bool> {
    use crate::git::GitVcs;
    use crate::integration::Severity;
    use crate::integration_runner::run_checks;
    use crate::manifest::Project;
    use crate::vcs::Vcs;
    use crate::workspace::{WorkspaceContext, WorkspaceLocation, WorkspaceSession};

    let ctx = WorkspaceContext::resolve(cwd, project_override)?;
    let workspace_dir = ctx.active_path().to_path_buf();

    // Dangling active-project check: if `.rwv-active` names a project whose
    // `projects/<name>/` directory does not exist on disk, report it as an
    // error. With `--fix`, clear `.rwv-active` so the workspace is no longer
    // broken. Either way, doctor continues to report other violations.
    let dangling_active: Option<CheckViolation> = {
        use crate::workspace::read_active_project;
        if let Some(active_name) = read_active_project(ctx.primary_path()) {
            let project_dir = ctx
                .primary_path()
                .join("projects")
                .join(active_name.as_str());
            if !project_dir.is_dir() {
                Some(CheckViolation::DanglingActiveProject {
                    project: active_name.clone(),
                    missing_dir: project_dir.clone(),
                })
            } else {
                None
            }
        } else {
            None
        }
    };

    // Build session (runs builtin_registries → scan_repos_on_disk → discover_project_paths).
    let session = WorkspaceSession::new(&workspace_dir);
    let git = GitVcs;

    // Legacy `role: primary` scan + optional --fix migration. Runs before
    // `Project::from_dir`, since manifests with the legacy spelling fail
    // to parse now that the back-compat alias is gone. With `--fix`, the
    // rewrite happens here so subsequent loaders see the migrated
    // manifests.
    // In default (project-scoped) mode with an active project, only report
    // findings for that project. Without an active project, report all
    // (matches the fall-through in project loading).
    let active_project_name: Option<crate::manifest::ProjectName> = ctx.active_project().cloned();
    let legacy_role_primary_all = scan_workspace_for_legacy_role_primary(&workspace_dir);
    let legacy_role_primary: Vec<_> = if scope_all || active_project_name.is_none() {
        legacy_role_primary_all
    } else {
        legacy_role_primary_all
            .into_iter()
            .filter(|f| {
                active_project_name
                    .as_ref()
                    .map(|a| f.project.as_str() == a.as_str())
                    .unwrap_or(true)
            })
            .collect()
    };
    let mut legacy_role_primary_warnings: Vec<(crate::manifest::ProjectName, PathBuf)> = Vec::new();
    let mut legacy_role_primary_errors: Vec<(crate::manifest::ProjectName, String)> = Vec::new();
    for finding in &legacy_role_primary {
        if fix {
            match fix_legacy_role_primary(&finding.manifest_path) {
                Ok(0) => {
                    // Race: detector saw the legacy spelling but the
                    // rewriter found none. Treat as a no-op.
                }
                Ok(count) => {
                    println!(
                        "[fixed] core: migrated {count} `role: primary` → `role: owned` in {}",
                        finding.manifest_path.display()
                    );
                }
                Err(e) => {
                    legacy_role_primary_errors.push((finding.project.clone(), e.to_string()));
                }
            }
        } else {
            legacy_role_primary_warnings
                .push((finding.project.clone(), finding.manifest_path.clone()));
        }
    }

    // Legacy workweave-marker scan + optional --fix migration. Runs from the
    // primary weave only (workweave markers live in the workweave-parent dir
    // which is sibling to the primary). Scans even from a workweave CWD so
    // `rwv doctor --fix` works from wherever the operator runs it.
    let legacy_ww_markers = scan_for_legacy_workweave_markers(ctx.primary_path());
    let mut legacy_ww_marker_warnings: Vec<LegacyWorkweaveMarkerFile> = Vec::new();
    let mut legacy_ww_marker_errors: Vec<String> = Vec::new();
    for finding in &legacy_ww_markers {
        if fix {
            match fix_legacy_workweave_marker(finding) {
                Ok(true) => {
                    println!(
                        "[fixed] core: appended `parent:` to {}",
                        finding.marker_path.display()
                    );
                }
                Ok(false) => {
                    // Race: already had parent: by the time we tried to fix.
                }
                Err(e) => {
                    legacy_ww_marker_errors.push(e.to_string());
                }
            }
        } else {
            legacy_ww_marker_warnings.push(finding.clone());
        }
    }

    // Resolve HEAD revisions for each repo on disk. Errors are kept (not
    // dropped) so that `find_violations` can flag on-disk repos whose HEAD
    // could not be read (corrupted, mid-rebase, permissions). Audit B4.
    let mut head_revisions = BTreeMap::new();
    let mut head_read_failures: Vec<(RepoPath, String)> = Vec::new();
    for repo_path in session.repos_on_disk() {
        let abs = workspace_dir.join(repo_path.as_path());
        match git.head_revision(&abs) {
            Ok(rev) => {
                head_revisions.insert(repo_path.clone(), rev);
            }
            Err(e) => {
                head_read_failures.push((repo_path.clone(), e.to_string()));
            }
        }
    }

    // Determine which project(s) to load. In default (project-scoped) mode
    // only the active project is loaded so that stale-lock, dangling-reference,
    // and integration findings stay within the project the operator cares about.
    // Under `--all`, every project is loaded and weave-wide checks (orphan
    // detection, cross-project stale locks) run as before.
    let active_project_name: Option<crate::manifest::ProjectName> = ctx.active_project().cloned();

    // Load project manifest(s) from projects/*/rwv.yaml.
    // In default mode: only the active project (identified by active_project_name).
    // In --all mode: every project under projects/.
    let projects_dir = workspace_dir.join("projects");
    let mut projects = Vec::new();
    let mut known_repos = BTreeSet::new();
    let mut lock_resolve_failures: Vec<(crate::manifest::ProjectName, RepoPath)> = Vec::new();
    // Projects whose rwv.yaml exists but fails to parse — surfaced as
    // `unparseable-project` violations so the workspace is never silent-clean
    // when a manifest is broken.
    let mut unparseable_projects: Vec<(crate::manifest::ProjectName, PathBuf, String)> = Vec::new();

    let mut resolved_locks: std::collections::HashMap<
        crate::manifest::ProjectName,
        crate::manifest::ResolvedLockFile,
    > = std::collections::HashMap::new();

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
            let rel_dir = project_dir
                .strip_prefix(&workspace_dir)
                .unwrap_or(&project_dir);
            let name_from_rel = rel_dir
                .strip_prefix("projects")
                .unwrap_or(rel_dir)
                .to_string_lossy()
                .into_owned();

            // In default mode with an active project, skip other projects so
            // that stale-lock, dangling-reference, and integration findings
            // stay within the project the operator cares about.
            // If no active project is set, fall back to loading every project
            // (preserves behaviour in simple workspaces that don't call `rwv
            // activate`).
            if !scope_all {
                if let Some(ref active) = active_project_name {
                    if name_from_rel != active.as_str() {
                        continue;
                    }
                }
                // No active project → don't skip; fall through to load all.
            }

            match Project::from_dir(&project_dir) {
                Ok(project) => {
                    // Resolve lock entries against on-disk repos so the
                    // canonical-SHA equality used by `find_violations` works
                    // uniformly for tag-form, branch-form, and SHA-form locks.
                    //
                    // B3: capture unresolvable entries instead of discarding
                    // them. An unresolvable rev means the local clone has
                    // never seen the SHA/tag the lock pinned; without this
                    // diagnostic, `find_violations` either flags nothing
                    // (no head_revisions entry) or falsely reports StaleLock
                    // by comparing the raw tag string against a real SHA.
                    if let Some(raw_lock) = project.lock.clone() {
                        let project_name_for_issue = project.name.clone();
                        let (resolved, failures) = raw_lock.resolve_versions(&workspace_dir);
                        for (unresolved, _raw_rev) in failures {
                            lock_resolve_failures
                                .push((project_name_for_issue.clone(), unresolved));
                        }
                        resolved_locks.insert(project.name.clone(), resolved);
                    }

                    for repo_path in project.manifest.iter_repo_paths() {
                        known_repos.insert(repo_path.clone());
                    }
                    projects.push(project);
                }
                Err(e) => {
                    // Surface unparseable manifests as a violation so
                    // operators get a clear signal instead of zero findings
                    // (which looks identical to a healthy project). --fix
                    // does not auto-repair broken YAML; the operator must
                    // fix by hand and re-run.
                    // Defer: collect violations after the input is built.
                    // We push directly into `all_issues` at display time.
                    // Store for now in a side-channel parallel to the other
                    // failure vecs already used in this function.
                    unparseable_projects.push((
                        crate::manifest::ProjectName::new(name_from_rel),
                        manifest_path,
                        e.to_string(),
                    ));
                }
            }
        }
    }

    // Orphan detection requires all projects to be loaded (otherwise repos
    // that belong to non-loaded projects look orphaned). Run it when:
    //   - `--all` was passed (all projects loaded), OR
    //   - no active project is set (fall-through path also loaded all projects).
    let loaded_all_projects = scope_all || active_project_name.is_none();

    // Build CheckInput and find violations
    let input = CheckInput {
        known_repos,
        repos_on_disk: session.repos_on_disk().to_vec(),
        projects,
        head_revisions,
        resolved_locks,
        check_orphans: loaded_all_projects,
    };

    let mut violations = find_violations(&input);
    for (project, manifest_path) in &legacy_role_primary_warnings {
        violations.push(CheckViolation::LegacyRolePrimary {
            project: project.clone(),
            manifest_path: manifest_path.clone(),
        });
    }

    // Dangling active-project: emit the violation or apply the --fix.
    // Fix errors are collected here so they can be appended to all_issues
    // after the violations batch is converted below.
    let mut dangling_fix_errors: Vec<String> = Vec::new();
    if let Some(CheckViolation::DanglingActiveProject {
        project: dap_project,
        missing_dir: dap_dir,
    }) = dangling_active
    {
        if fix {
            let active_file = ctx.primary_path().join(".rwv-active");
            match std::fs::remove_file(&active_file) {
                Ok(()) => println!(
                    "[fixed] core: cleared `.rwv-active` (was pointing at missing project `{}`)",
                    dap_project
                ),
                Err(e) => {
                    dangling_fix_errors.push(format!(
                        "dangling-active-project fix failed for `{}`: {e}",
                        dap_project
                    ));
                }
            }
        } else {
            violations.push(CheckViolation::DanglingActiveProject {
                project: dap_project,
                missing_dir: dap_dir,
            });
        }
    }

    for finding in &legacy_ww_marker_warnings {
        violations.push(CheckViolation::LegacyWorkweaveMarker {
            marker_path: finding.marker_path.clone(),
            primary: finding.primary.clone(),
        });
    }

    // Surface unparseable manifests as first-class violations so the
    // workspace is never reported as "clean" when a manifest is broken.
    for (project, manifest_path, message) in &unparseable_projects {
        violations.push(CheckViolation::UnparseableProject {
            project: project.clone(),
            manifest_path: manifest_path.clone(),
            message: message.clone(),
        });
    }

    let mut all_issues = violations_to_issues(violations);

    for msg in dangling_fix_errors {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: msg,
            safe_to_fix: true,
        });
    }

    for (project_name, err) in &legacy_role_primary_errors {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: format!("{project_name}: legacy `role: primary` fix failed: {err}"),
            safe_to_fix: true,
        });
    }
    for err in &legacy_ww_marker_errors {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: format!("legacy workweave marker fix failed: {err}"),
            safe_to_fix: true,
        });
    }

    // B3: surface lock entries that couldn't be resolved against the local
    // repo. Doctor is the diagnostic of last resort — swallowing this signal
    // is exactly the wrong place to drop information.
    for (project_name, repo_path) in &lock_resolve_failures {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: format!(
                "{project_name}: lock references unknown revision for {repo_path}; run `rwv lock` or fetch"
            ),
            safe_to_fix: true,
        });
    }

    // B4: surface on-disk repos whose HEAD could not be read. Previously the
    // Err was silently dropped, so `find_violations` produced zero
    // violations for these repos and doctor reported clean.
    for (repo_path, err_msg) in &head_read_failures {
        all_issues.push(Issue {
            integration: "core".into(),
            severity: Severity::Error,
            message: format!("{repo_path}: HEAD unreadable ({err_msg})"),
            safe_to_fix: true,
        });
    }

    // Run integration check hooks for each project
    let builtin = crate::integrations::builtin_integrations();
    let integrations: Vec<&dyn crate::integration::Integration> =
        builtin.iter().map(|b| b.as_ref()).collect();

    for project in &input.projects {
        let detection_cache = crate::integration_runner::build_detection_cache(
            &workspace_dir,
            project.manifest.iter_entries(),
        );
        let ctx_base = session.context_base(
            &workspace_dir,
            &project.name,
            &detection_cache,
            project.manifest.workweave.as_ref(),
        );
        let integration_issues = run_checks(&integrations, &project.manifest, &ctx_base);
        all_issues.extend(integration_issues);

        // Trigger-model drift check (see `trigger-model.md`): the integrations'
        // `verify()` pass reports drift between on-disk managed/generated content
        // and what `activate()` would produce. Under `--fix`, doctor invokes the
        // intent-mode write path to regenerate safe-to-fix drift. Without `--fix`,
        // all drift findings surface as warnings — `doctor` is the detector and
        // the fixer.
        //
        // USER-HELD findings (`safe_to_fix = false`) are always surfaced as-is,
        // even under `--fix` — these are cases where the user holds the pen on a
        // managed file region and auto-repair would silently destroy user content.
        // Doctor never auto-takes-over a user-held file.
        let verify_issues = crate::integration_runner::run_verifications(
            &integrations,
            &project.manifest,
            &ctx_base,
        );
        // Split into auto-fixable (safe_to_fix=true) and user-held (safe_to_fix=false).
        let (fixable_issues, user_held_issues): (Vec<_>, Vec<_>) =
            verify_issues.into_iter().partition(|i| i.safe_to_fix);
        // USER-HELD findings always surface — we never auto-rewrite them.
        all_issues.extend(user_held_issues);
        if fix && !fixable_issues.is_empty() {
            // Regenerate by running intent-mode activation against this project,
            // from the workspace dir. This is the canonical write path; any
            // integration whose verify() flagged safe-to-fix drift will re-author
            // its content here. The activation also re-runs activate-hooks (install
            // commands); doctor --fix is meant to fully repair, so that's correct.
            match crate::activate::activate_intent(project.name.as_str(), &workspace_dir) {
                Ok(()) => println!(
                    "[fixed] core: regenerated integration content for project `{}` (drift detected)",
                    project.name
                ),
                Err(e) => all_issues.push(Issue {
                    integration: "core".into(),
                    severity: Severity::Error,
                    message: format!(
                        "doctor --fix: failed to regenerate integration content for `{}`: {e}",
                        project.name
                    ),
                    safe_to_fix: true,
                }),
            }
        } else {
            all_issues.extend(fixable_issues);
        }
    }

    // Index-drift + working-tree-drift detection.
    //
    // Scan list: every materialized worktree referenced by a manifest. From
    // the primary weave we additionally enumerate each workweave's repos.
    // The build loop dedupes by absolute path so multiple projects that share
    // a repo only pay one round of git subprocess cost per physical worktree.
    // The two drift kinds are then classified in a single
    // pass using [`classify_drift`], which collapses the common
    // "worktree is clean" case to one `git status` invocation instead of two
    // back-to-back `git diff-index` invocations.
    let mut index_scan: Vec<(Option<String>, std::path::PathBuf, String)> = Vec::new();
    let mut scan_seen: std::collections::HashSet<(Option<String>, std::path::PathBuf)> =
        std::collections::HashSet::new();

    for project in &input.projects {
        for repo_path in project.manifest.iter_repo_paths() {
            let abs = workspace_dir.join(repo_path.as_path());
            if abs.exists() && scan_seen.insert((None, abs.clone())) {
                index_scan.push((None, abs, repo_path.to_string()));
            }
        }
    }

    // From the primary weave: also scan every known workweave.
    if matches!(ctx.location, WorkspaceLocation::Weave { .. }) {
        for (ww_name, ww_dir) in crate::workweave::list_workweave_dirs(ctx.primary_path()) {
            for project in &input.projects {
                for repo_path in project.manifest.iter_repo_paths() {
                    let abs = ww_dir.join(repo_path.as_path());
                    if abs.exists() && scan_seen.insert((Some(ww_name.clone()), abs.clone())) {
                        index_scan.push((Some(ww_name.clone()), abs, repo_path.to_string()));
                    }
                }
            }
        }
    }
    drop(scan_seen);

    // Progress output: workspace-scale doctor runs (80+ workweaves × ~13
    // repos) were previously silent for many seconds. Emit a heartbeat
    // to stderr so the operator can tell "in progress" from "hung". The
    // line goes to stderr to keep stdout free of noise for the human-
    // facing report below. JSON callers go through `run_check_json` and
    // don't see this.
    let total_scans = index_scan.len();
    let progress_every = total_scans.div_ceil(20).max(1);
    if total_scans > 0 {
        eprintln!("doctor: scanning {total_scans} worktree(s) for drift...");
    }

    for (i, (ww_label, repo_abs, repo_display)) in index_scan.iter().enumerate() {
        if total_scans >= 50 && (i + 1) % progress_every == 0 {
            eprintln!("doctor: scanned {}/{total_scans}", i + 1);
        }
        let location = match ww_label {
            Some(ww) => format!("{ww}/{repo_display}"),
            None => repo_display.clone(),
        };

        let (idx_drift, wt_drift) = classify_drift(repo_abs);

        if let Some(drift_kind) = idx_drift {
            match drift_kind {
                IndexDriftKind::SafeToFix => {
                    if fix {
                        match reset_index_to_head(repo_abs) {
                            Ok(()) => println!("[fixed] core: index refreshed for {location}"),
                            Err(e) => all_issues.push(Issue {
                                integration: "core".into(),
                                severity: Severity::Error,
                                message: format!("{location}: index fix failed: {e}"),
                                safe_to_fix: true,
                            }),
                        }
                    } else {
                        all_issues.push(Issue {
                            integration: "core".into(),
                            severity: Severity::Warning,
                            message: format!("{location}: index stale (safe to --fix)"),
                            safe_to_fix: true,
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
                        safe_to_fix: true,
                    });
                }
            }
        }

        if let Some(drift_kind) = wt_drift {
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
                                safe_to_fix: true,
                            }),
                        }
                    } else {
                        all_issues.push(Issue {
                            integration: "core".into(),
                            severity: Severity::Warning,
                            message: format!("{location}: working tree stale (safe to --fix)"),
                            safe_to_fix: true,
                        });
                    }
                }
                WorkingTreeDriftKind::LiveEdits => {
                    all_issues.push(Issue {
                        integration: "core".into(),
                        severity: Severity::Warning,
                        message: format!("{location}: working tree has live edits (manual review)"),
                        safe_to_fix: true,
                    });
                }
            }
        }
    }

    // Replay-exclusion check: each project repo should carry
    // `rwv.lock merge=ours` in `.gitattributes`. Older projects don't
    // have it; `--fix` writes the line in place (idempotent — re-running
    // on a fixed repo is a no-op).
    for project in &input.projects {
        let project_repo = workspace_dir.join("projects").join(project.name.as_str());
        if !project_repo.is_dir() {
            continue;
        }
        match git.has_replay_exclusion(&project_repo, std::path::Path::new("rwv.lock")) {
            Ok(true) => {}
            Ok(false) => {
                if fix {
                    match git.set_replay_exclusion(&project_repo, std::path::Path::new("rwv.lock"))
                    {
                        Ok(()) => println!(
                            "[fixed] core: wrote `rwv.lock merge=ours` to {}/.gitattributes",
                            project.name
                        ),
                        Err(e) => all_issues.push(Issue {
                            integration: "core".into(),
                            severity: Severity::Error,
                            message: format!(
                                "{}: failed to write replay-exclusion: {e}",
                                project.name
                            ),
                            safe_to_fix: true,
                        }),
                    }
                } else {
                    all_issues.push(Issue {
                        integration: "core".into(),
                        severity: Severity::Warning,
                        message: format!(
                            "{}: project repo missing `rwv.lock merge=ours` in .gitattributes \
                             (run `rwv doctor --fix` to add)",
                            project.name
                        ),
                        safe_to_fix: true,
                    });
                }
            }
            Err(e) => all_issues.push(Issue {
                integration: "core".into(),
                severity: Severity::Warning,
                message: format!(
                    "{}: failed to read .gitattributes for replay-exclusion check: {e}",
                    project.name
                ),
                safe_to_fix: true,
            }),
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

/// Build the JSON payload for `rwv doctor --json` from a vector of
/// violations and the resolved workspace context. Extracted from
/// [`run_check_json`] so tests can drive the serialization shape without
/// reaching for a real workspace on disk.
///
/// Returns `{ "$schema": ..., "violations": [...] }`.
pub fn build_doctor_json(
    violations: Vec<CheckViolation>,
    workspace_dir: &Path,
    workweave_dirs: &std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
) -> serde_json::Value {
    let outputs: Vec<ViolationOutput> = violations
        .into_iter()
        .map(|v| ViolationOutput::from_violation(v, workspace_dir, workweave_dirs))
        .collect();
    serde_json::json!({
        "$schema": DOCTOR_SCHEMA_URL,
        "violations": outputs,
    })
}

/// Collect every `CheckViolation` `rwv doctor` knows how to produce.
///
/// Mirrors the scaffolding in [`run_check`] but returns a typed enum vector
/// instead of mixing `Issue`s and `CheckViolation`s. Integration-runner
/// findings and lock-resolution / HEAD-read failures are out of scope: they
/// are not `CheckViolation` variants today and the bead explicitly excludes
/// them from `--json` (the acceptance criterion is "each `CheckViolation`
/// variant serializes").
///
/// When `scope_all` is `false`, only the active project is loaded and the
/// orphan check is skipped (matching the default scoping of `run_check`).
/// Pass `scope_all = true` (`--all`) for the weave-wide scan.
///
/// Returns `(violations, workweave_dirs)` so the caller can resolve
/// workweave-scoped `absolute_path` fields.
fn collect_doctor_violations(
    cwd: &Path,
    project_override: Option<crate::manifest::ProjectName>,
    scope_all: bool,
) -> anyhow::Result<(
    Vec<CheckViolation>,
    std::path::PathBuf,
    std::collections::HashMap<WorkweaveName, std::path::PathBuf>,
)> {
    use crate::git::GitVcs;
    use crate::vcs::Vcs;
    use crate::workspace::{WorkspaceContext, WorkspaceLocation, WorkspaceSession};

    let ctx = WorkspaceContext::resolve(cwd, project_override)?;
    let workspace_dir = ctx.active_path().to_path_buf();

    let session = WorkspaceSession::new(&workspace_dir);
    let git = GitVcs;

    // Resolve HEAD revisions for each repo on disk. HEAD-read failures are
    // surfaced by the non-JSON `run_check` as `Issue`s; they have no
    // `CheckViolation` variant and are therefore not emitted under `--json`.
    let mut head_revisions = BTreeMap::new();
    for repo_path in session.repos_on_disk() {
        let abs = workspace_dir.join(repo_path.as_path());
        if let Ok(rev) = git.head_revision(&abs) {
            head_revisions.insert(repo_path.clone(), rev);
        }
    }

    // Active project name for project-scoped filtering.
    let active_project_name: Option<crate::manifest::ProjectName> = ctx.active_project().cloned();

    // Load projects + resolve lock files.
    // In default mode: only the active project. In --all mode: every project.
    let projects_dir = workspace_dir.join("projects");
    let mut projects = Vec::new();
    let mut known_repos = BTreeSet::new();
    let mut resolved_locks: std::collections::HashMap<
        crate::manifest::ProjectName,
        crate::manifest::ResolvedLockFile,
    > = std::collections::HashMap::new();
    // Unparseable manifests collected here and emitted as violations below.
    let mut unparseable_projects_json: Vec<(crate::manifest::ProjectName, PathBuf, String)> =
        Vec::new();

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
            let rel_dir = project_dir
                .strip_prefix(&workspace_dir)
                .unwrap_or(&project_dir);
            let name_from_rel = rel_dir
                .strip_prefix("projects")
                .unwrap_or(rel_dir)
                .to_string_lossy()
                .into_owned();

            // In default mode with an active project, skip other projects.
            // If no active project is set, fall back to loading every project.
            if !scope_all {
                if let Some(ref active) = active_project_name {
                    if name_from_rel != active.as_str() {
                        continue;
                    }
                }
                // No active project → fall through to load all.
            }

            match Project::from_dir(&project_dir) {
                Ok(project) => {
                    if let Some(raw_lock) = project.lock.clone() {
                        let (resolved, _failures) = raw_lock.resolve_versions(&workspace_dir);
                        resolved_locks.insert(project.name.clone(), resolved);
                    }
                    for repo_path in project.manifest.iter_repo_paths() {
                        known_repos.insert(repo_path.clone());
                    }
                    projects.push(project);
                }
                Err(e) => {
                    unparseable_projects_json.push((
                        crate::manifest::ProjectName::new(name_from_rel),
                        manifest_path,
                        e.to_string(),
                    ));
                }
            }
        }
    }

    let loaded_all_projects_json = scope_all || active_project_name.is_none();

    let input = CheckInput {
        known_repos,
        repos_on_disk: session.repos_on_disk().to_vec(),
        projects,
        head_revisions,
        resolved_locks,
        check_orphans: loaded_all_projects_json,
    };

    let mut violations = find_violations(&input);

    // Dangling active-project: .rwv-active names a project whose directory
    // doesn't exist. The JSON channel never auto-fixes; that's for run_check.
    {
        use crate::workspace::read_active_project;
        if let Some(active_name) = read_active_project(ctx.primary_path()) {
            let project_dir = ctx
                .primary_path()
                .join("projects")
                .join(active_name.as_str());
            if !project_dir.is_dir() {
                violations.push(CheckViolation::DanglingActiveProject {
                    project: active_name,
                    missing_dir: project_dir,
                });
            }
        }
    }

    // Unparseable manifests: surface as violations in the JSON channel too.
    for (project, manifest_path, message) in unparseable_projects_json {
        violations.push(CheckViolation::UnparseableProject {
            project,
            manifest_path,
            message,
        });
    }

    // Legacy `role: primary` findings — text-scan over `projects/*/rwv.yaml`
    // since the parser rejects the spelling and a `Project` wouldn't load.
    // The JSON channel never auto-fixes; `--fix` is reserved for the
    // human-facing `run_check`.
    // In default mode with an active project, restrict to that project only.
    // Without an active project, report all (same fall-through as project loading).
    let legacy_role_all = scan_workspace_for_legacy_role_primary(&workspace_dir);
    let legacy_role_findings: Vec<_> = if loaded_all_projects_json {
        legacy_role_all
    } else {
        // scope_all=false and active project is set
        legacy_role_all
            .into_iter()
            .filter(|f| {
                active_project_name
                    .as_ref()
                    .map(|a| f.project.as_str() == a.as_str())
                    .unwrap_or(true) // no active → include all
            })
            .collect()
    };
    for finding in legacy_role_findings {
        violations.push(CheckViolation::LegacyRolePrimary {
            project: finding.project,
            manifest_path: finding.manifest_path,
        });
    }

    // Legacy workweave-marker findings — scan the workweave-parent directory.
    // These are workspace-level infrastructure checks; always run.
    for finding in scan_for_legacy_workweave_markers(ctx.primary_path()) {
        violations.push(CheckViolation::LegacyWorkweaveMarker {
            marker_path: finding.marker_path,
            primary: finding.primary,
        });
    }

    // Index-drift + working-tree-drift detection. Same scan list as
    // `run_check`: CWD workspace repos plus, from the primary weave, every
    // known workweave. Dedupe by `(workweave, abs)` so multiple projects
    // referencing the same physical worktree don't multiply git subprocess
    // cost. Drift is classified via [`classify_drift`] (single `git status`
    // fast-path for clean worktrees).
    let mut workweave_dirs: std::collections::HashMap<WorkweaveName, std::path::PathBuf> =
        std::collections::HashMap::new();
    let mut index_scan: Vec<(Option<WorkweaveName>, std::path::PathBuf, RepoPath)> = Vec::new();
    let mut scan_seen: std::collections::HashSet<(Option<WorkweaveName>, std::path::PathBuf)> =
        std::collections::HashSet::new();

    for project in &input.projects {
        for repo_path in project.manifest.iter_repo_paths() {
            let abs = workspace_dir.join(repo_path.as_path());
            if abs.exists() && scan_seen.insert((None, abs.clone())) {
                index_scan.push((None, abs, repo_path.clone()));
            }
        }
    }

    if matches!(ctx.location, WorkspaceLocation::Weave { .. }) {
        for (ww_name_str, ww_dir) in crate::workweave::list_workweave_dirs(ctx.primary_path()) {
            let ww_name = WorkweaveName::new(ww_name_str);
            workweave_dirs.insert(ww_name.clone(), ww_dir.clone());
            for project in &input.projects {
                for repo_path in project.manifest.iter_repo_paths() {
                    let abs = ww_dir.join(repo_path.as_path());
                    if abs.exists() && scan_seen.insert((Some(ww_name.clone()), abs.clone())) {
                        index_scan.push((Some(ww_name.clone()), abs, repo_path.clone()));
                    }
                }
            }
        }
    }
    drop(scan_seen);

    for (ww_label, repo_abs, repo_path) in &index_scan {
        let (idx_drift, wt_drift) = classify_drift(repo_abs);
        if let Some(drift_kind) = idx_drift {
            violations.push(CheckViolation::IndexDrift {
                workweave: ww_label.clone(),
                repo: repo_path.clone(),
                kind: drift_kind,
            });
        }
        if let Some(drift_kind) = wt_drift {
            violations.push(CheckViolation::WorkingTreeDrift {
                workweave: ww_label.clone(),
                repo: repo_path.clone(),
                kind: drift_kind,
            });
        }
    }

    // Replay-exclusion check: each project repo should carry
    // `rwv.lock merge=ours` in `.gitattributes`.
    for project in &input.projects {
        let project_repo = workspace_dir.join("projects").join(project.name.as_str());
        if !project_repo.is_dir() {
            continue;
        }
        if let Ok(false) = git.has_replay_exclusion(&project_repo, std::path::Path::new("rwv.lock"))
        {
            violations.push(CheckViolation::MissingReplayExclusion {
                project: project.name.clone(),
            });
        }
    }

    Ok((violations, workspace_dir, workweave_dirs))
}

/// Run `rwv doctor --json`.
///
/// Emits `{ "$schema": "...", "violations": [...] }` to stdout. Exit
/// semantics match [`run_check`]: returns `Ok(true)` iff any violations
/// were found, so the caller can exit non-zero.
///
/// Only `CheckViolation` variants are surfaced — integration-runner
/// findings (which are `Issue`s, not `CheckViolation`s) and ad-hoc
/// failures (HEAD-unreadable, lock-resolve failures) are intentionally
/// out of scope for the JSON channel (see the bead body for rationale).
///
/// When `scope_all` is `false` (the default), only the active project is
/// checked and orphan detection is skipped. Pass `scope_all = true` (`--all`)
/// to reproduce the weave-wide scan.
pub fn run_check_json(
    cwd: &std::path::Path,
    project_override: Option<crate::manifest::ProjectName>,
    scope_all: bool,
) -> anyhow::Result<bool> {
    let (violations, workspace_dir, workweave_dirs) =
        collect_doctor_violations(cwd, project_override, scope_all)?;
    let has_violations = !violations.is_empty();
    let payload = build_doctor_json(violations, &workspace_dir, &workweave_dirs);
    let out =
        serde_json::to_string_pretty(&payload).context("failed to serialize doctor output")?;
    println!("{out}");
    Ok(has_violations)
}
