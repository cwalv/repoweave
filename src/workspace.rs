//! Workspace: the resolved state of a repoweave directory tree.
//!
//! A workspace is the top-level directory containing registry dirs, projects,
//! and ecosystem files. This module resolves the workspace from CWD and
//! provides the context that commands operate on.

use crate::git::GitVcs;
use crate::integration_runner::IntegrationContextBase;
use crate::manifest::{Manifest, ProjectName, RepoPath, WorkweaveName};
use crate::registry::{builtin_registries, Registry};
use crate::vcs::Vcs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Context — where are we?
// ---------------------------------------------------------------------------

/// The resolved workspace context, inferred from CWD.
///
/// Every `rwv` command starts by resolving this. It answers:
/// - Where is the workspace root?
/// - Are we in the primary weave or a workweave?
/// - Which project is active?
#[derive(Debug)]
pub struct WorkspaceContext {
    /// The primary directory (workspace root with regular clones).
    pub root: PathBuf,
    /// The current working location: weave or a specific workweave.
    pub location: WorkspaceLocation,
}

/// Whether we're in the weave directory or inside a workweave.
#[derive(Debug)]
pub enum WorkspaceLocation {
    /// Working in the weave directory (regular clones).
    /// The active project is inferred from CWD or `--project`.
    Weave { project: Option<ProjectName> },
    /// Working in a workweave (worktrees on ephemeral branches).
    Workweave {
        name: WorkweaveName,
        /// The workweave directory path (e.g., `.workweaves/feat/` or `root/../ws--feat/`).
        dir: PathBuf,
        /// The project this workweave belongs to.
        project: ProjectName,
    },
}

/// Well-known directory names that identify a workspace root.
fn workspace_marker_names() -> Vec<String> {
    let mut names: Vec<String> = builtin_registries()
        .iter()
        .map(|r| r.name().0.clone())
        .collect();
    names.push("projects".to_string());
    names
}

/// Returns true if `dir` looks like a workspace root (contains projects/ or
/// a registry directory).
fn is_workspace_root(dir: &Path) -> bool {
    for marker in workspace_marker_names() {
        let candidate = dir.join(&marker);
        if candidate.is_dir() {
            return true;
        }
    }
    false
}

/// Detect the project name if `cwd` is inside `{root}/projects/{name}/...`.
fn detect_project(cwd: &Path, root: &Path) -> Option<ProjectName> {
    let rel = cwd.strip_prefix(root).ok()?;
    let mut components = rel.components();
    let first = components.next()?;
    if first.as_os_str() != "projects" {
        return None;
    }
    let project_name = components.next()?;
    Some(ProjectName::new(
        project_name.as_os_str().to_string_lossy().to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Active project tracking via .rwv-active
// ---------------------------------------------------------------------------

const ACTIVE_PROJECT_FILE: &str = ".rwv-active";

/// Read the active project from the `.rwv-active` file in the workspace root.
///
/// Returns `None` if the file does not exist or is empty.
pub fn read_active_project(root: &Path) -> Option<ProjectName> {
    let path = root.join(ACTIVE_PROJECT_FILE);
    let content = std::fs::read_to_string(&path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(ProjectName::new(trimmed))
}

/// Write the active project to the `.rwv-active` file in the workspace root.
pub fn set_active_project(root: &Path, project: &ProjectName) -> anyhow::Result<()> {
    let path = root.join(ACTIVE_PROJECT_FILE);
    std::fs::write(&path, format!("{}\n", project.as_str()))
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))
}

/// Scan registry directories under `root` for VCS repos on disk.
///
/// Walks the `{registry}/{owner}/{repo}` directory structure for each
/// registry, filters directories using `vcs.is_repo()`, and returns
/// relative paths from `root`.
pub fn scan_repos_on_disk(
    root: &Path,
    registries: &[Box<dyn Registry>],
    vcs: &dyn Vcs,
) -> Vec<RepoPath> {
    let mut repos = Vec::new();

    for reg in registries {
        let reg_dir = root.join(&reg.name().0);
        if !reg_dir.is_dir() {
            continue;
        }
        let owners = match std::fs::read_dir(&reg_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for owner_entry in owners.flatten() {
            let owner_path = owner_entry.path();
            if !owner_path.is_dir() {
                continue;
            }
            let repo_entries = match std::fs::read_dir(&owner_path) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for repo_entry in repo_entries.flatten() {
                let repo_path = repo_entry.path();
                if !repo_path.is_dir() {
                    continue;
                }
                if !vcs.is_repo(&repo_path) {
                    continue;
                }
                if let Ok(rel) = repo_path.strip_prefix(root) {
                    repos.push(RepoPath::new(rel.to_string_lossy()));
                }
            }
        }
    }

    repos
}

/// Discover all project names under `projects/` relative to `root`.
///
/// Returns a sorted list of directory names found under `{root}/projects/`.
pub fn discover_project_paths(root: &Path) -> Vec<String> {
    let projects_dir = root.join("projects");
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// WorkspaceSession — computed-once workspace data
// ---------------------------------------------------------------------------

/// Computed-once workspace data: registries, repos on disk, and project paths.
///
/// Call [`WorkspaceSession::new`] once per command invocation to pay the scan
/// cost a single time, then pass varying data (output_dir, project) to
/// [`WorkspaceSession::context_base`] to build an [`IntegrationContextBase`].
pub struct WorkspaceSession {
    pub root: PathBuf,
    repos_on_disk: Vec<RepoPath>,
    project_paths: Vec<String>,
}

impl WorkspaceSession {
    /// Build a `WorkspaceSession` by running the standard scan triad:
    /// `builtin_registries()` → `scan_repos_on_disk()` → `discover_project_paths()`.
    pub fn new(root: &Path) -> Self {
        let registries = builtin_registries();
        let git = GitVcs;
        let repos_on_disk = scan_repos_on_disk(root, &registries, &git);
        let project_paths = discover_project_paths(root);
        Self {
            root: root.to_path_buf(),
            repos_on_disk,
            project_paths,
        }
    }

    /// Build an [`IntegrationContextBase`] from this session's shared data
    /// combined with the per-invocation `output_dir` and `project`.
    pub fn context_base<'a>(
        &'a self,
        output_dir: &'a Path,
        project: &'a ProjectName,
        detection_cache: &'a std::collections::HashMap<String, Vec<String>>,
    ) -> IntegrationContextBase<'a> {
        IntegrationContextBase {
            output_dir,
            workspace_root: &self.root,
            project,
            all_repos_on_disk: &self.repos_on_disk,
            all_project_paths: &self.project_paths,
            detection_cache,
        }
    }

    /// The repos found on disk (relative paths from workspace root).
    pub fn repos_on_disk(&self) -> &[RepoPath] {
        &self.repos_on_disk
    }

    /// The discovered project path names (directory names under `projects/`).
    pub fn project_paths(&self) -> &[String] {
        &self.project_paths
    }
}

/// Check that `cwd` is safe to use as a workspace root for bootstrapping
/// commands (`fetch`, future `init --adopt`).
///
/// - If [`WorkspaceContext::resolve`] succeeds, returns `Ok(())` — we are
///   inside an existing workspace.
/// - If resolve fails and `cwd` is an empty directory, returns `Ok(())` —
///   bootstrapping into a fresh directory is fine.
/// - If resolve fails and `cwd` is **non-empty**, returns an error advising
///   the caller to use `--force`.
///
/// Pass `force = true` to skip the non-empty check entirely.
pub fn require_workspace_or_empty(cwd: &Path, force: bool) -> anyhow::Result<()> {
    match WorkspaceContext::resolve(cwd, None) {
        Ok(_) => return Ok(()),           // existing workspace — proceed
        Err(_) if force => return Ok(()), // user passed --force
        Err(_) => {}
    }

    // resolve failed and no --force — check whether CWD is empty.
    let is_empty = match std::fs::read_dir(cwd) {
        Ok(mut entries) => entries.next().is_none(),
        // If we cannot read the directory, let downstream code handle it.
        Err(_) => return Ok(()),
    };

    if is_empty {
        Ok(())
    } else {
        anyhow::bail!(
            "no repoweave workspace found and {} is not empty; \
             use --force to bootstrap here anyway",
            cwd.display()
        )
    }
}

impl WorkspaceContext {
    /// Resolve the workspace context by walking up from `cwd`.
    ///
    /// If `project_override` is `Some`, it overrides the auto-detected project.
    /// In Weave location, `.rwv-active` is preferred over CWD inference when
    /// no explicit override is given.
    pub fn resolve(cwd: &Path, project_override: Option<ProjectName>) -> anyhow::Result<Self> {
        let cwd = cwd
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("failed to canonicalize {}: {e}", cwd.display()))?;

        // Walk ancestors looking for a workspace root OR a workweave pattern.
        //
        // For each ancestor directory we check (in order):
        //   1. Does it have a `.rwv-workweave` marker? If so, use that.
        //   2. Does its name match `{primary}--{name}`? If so, use the sibling.
        //   3. Is it a workspace root itself?
        let mut current = cwd.as_path();
        loop {
            // 1. Check for `.rwv-workweave` marker file in the current directory.
            if let Ok(Some(marker)) = WorkweaveMarker::read(current) {
                // The marker tells us exactly where the primary workspace is and
                // which project this workweave belongs to.
                let root = marker.primary.clone();
                if is_workspace_root(&root) {
                    let workweave_name_str = current
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown");
                    let workweave_name = WorkweaveName::new(workweave_name_str);
                    let project = project_override.unwrap_or(marker.project);
                    return Ok(WorkspaceContext {
                        root,
                        location: WorkspaceLocation::Workweave {
                            name: workweave_name,
                            dir: current.to_path_buf(),
                            project,
                        },
                    });
                }
            }

            // 2. Check the `{primary}--{name}` naming convention (backward compat).
            //    A workweave directory may also contain registry subdirs (e.g. github/),
            //    so check this BEFORE workspace root markers.
            if let Some(dir_name) = current.file_name().and_then(|n| n.to_str()) {
                if let Some((primary_name, workweave_name)) = parse_weave_dir_name(dir_name) {
                    // The workspace root is the sibling with the primary name.
                    let parent = current
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("workweave directory has no parent"))?;
                    let root = parent.join(primary_name);
                    if is_workspace_root(&root) {
                        let project =
                            project_override.unwrap_or_else(|| ProjectName::new(primary_name));
                        return Ok(WorkspaceContext {
                            root: root.clone(),
                            location: WorkspaceLocation::Workweave {
                                name: workweave_name,
                                dir: current.to_path_buf(),
                                project,
                            },
                        });
                    }
                }
            }

            // 3. Check if current directory IS the workspace root.
            if is_workspace_root(current) {
                let project = project_override
                    .or_else(|| detect_project(&cwd, current))
                    .or_else(|| read_active_project(current));
                return Ok(WorkspaceContext {
                    root: current.to_path_buf(),
                    location: WorkspaceLocation::Weave { project },
                });
            }

            // Move up to parent.
            match current.parent() {
                Some(parent) if parent != current => current = parent,
                _ => break,
            }
        }

        anyhow::bail!("no repoweave workspace found above {}", cwd.display())
    }

    /// Return the effective path for `rwv resolve`: the primary root or the
    /// workweave directory.
    pub fn resolve_path(&self) -> &Path {
        match &self.location {
            WorkspaceLocation::Weave { .. } => &self.root,
            WorkspaceLocation::Workweave { dir, .. } => dir,
        }
    }

    /// Display the workspace context to stdout.
    ///
    /// Shows root path, location type (weave/workweave), active project, and
    /// available projects.
    pub fn display(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("Root: {}", self.root.display()));

        match &self.location {
            WorkspaceLocation::Weave { project } => {
                lines.push("Location: weave".to_string());
                if let Some(p) = project {
                    lines.push(format!("Active project: {}", p.as_str()));
                    // Try to load manifest and show repo count
                    let manifest_path =
                        self.root.join("projects").join(p.as_str()).join("rwv.yaml");
                    if let Ok(manifest) = Manifest::from_path(&manifest_path) {
                        lines.push(format!("Repos: {}", manifest.repositories.len()));
                    }
                }
            }
            WorkspaceLocation::Workweave { name, dir, project } => {
                lines.push(format!("Location: workweave \"{}\"", name.as_str()));
                lines.push(format!("Workweave dir: {}", dir.display()));
                lines.push(format!("Project: {}", project.as_str()));
                // Try to load manifest and show repo count
                let manifest_path = self
                    .root
                    .join("projects")
                    .join(project.as_str())
                    .join("rwv.yaml");
                if let Ok(manifest) = Manifest::from_path(&manifest_path) {
                    lines.push(format!("Repos: {}", manifest.repositories.len()));
                }
            }
        }

        // List available projects
        let projects_dir = self.root.join("projects");
        if let Ok(entries) = std::fs::read_dir(&projects_dir) {
            let mut project_names: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            project_names.sort();
            if !project_names.is_empty() {
                lines.push(format!("Projects: {}", project_names.join(", ")));
            }
        }

        lines.join("\n")
    }
}

/// Build a workweave directory name using the legacy `{primary}--{name}` convention.
///
/// This naming convention is used for backward compatibility with the old
/// sibling-directory layout. The new convention uses `.workweaves/{name}/`
/// with a `.rwv-workweave` marker file. Both are supported by
/// [`WorkspaceContext::resolve`].
pub fn weave_dir_name(primary_name: &str, workweave_name: &WorkweaveName) -> String {
    format!("{primary_name}--{workweave_name}")
}

/// Parse a directory name into `(primary_name, workweave_name)` if it
/// matches the legacy `{primary}--{name}` convention.
///
/// Used for backward compatibility. The preferred resolution method is via
/// the `.rwv-workweave` marker file (see [`WorkweaveMarker`]).
pub fn parse_weave_dir_name(dir_name: &str) -> Option<(&str, WorkweaveName)> {
    let (primary, workweave) = dir_name.split_once("--")?;
    if primary.is_empty() || workweave.is_empty() {
        return None;
    }
    Some((primary, WorkweaveName::new(workweave)))
}

// ---------------------------------------------------------------------------
// WorkweaveMarker — `.rwv-workweave` marker file
// ---------------------------------------------------------------------------

/// Metadata written to `.rwv-workweave` in a workweave root.
/// Records the relationship to the primary workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkweaveMarker {
    pub primary: PathBuf,
    pub project: ProjectName,
}

impl WorkweaveMarker {
    pub fn read(dir: &Path) -> anyhow::Result<Option<Self>> {
        let path = dir.join(".rwv-workweave");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        let marker: Self = serde_yaml::from_str(&content).map_err(|e| {
            anyhow::anyhow!("failed to parse .rwv-workweave at {}: {e}", path.display())
        })?;
        Ok(Some(marker))
    }

    pub fn write(&self, dir: &Path) -> anyhow::Result<()> {
        let path = dir.join(".rwv-workweave");
        let content = serde_yaml::to_string(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize .rwv-workweave: {e}"))?;
        std::fs::write(&path, content)
            .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a minimal workspace directory structure under `parent`.
    /// Returns the workspace root path.
    fn make_workspace(parent: &Path, name: &str) -> PathBuf {
        let root = parent.join(name);
        std::fs::create_dir_all(root.join("github")).unwrap();
        std::fs::create_dir_all(root.join("projects")).unwrap();
        root
    }

    // ========================================================================
    // Resolve from inside a primary weave directory (registry subdir)
    // ========================================================================

    #[test]
    fn resolve_from_inside_weave_registry_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "myworkspace");
        let deep = root.join("github").join("acme").join("server");
        std::fs::create_dir_all(&deep).unwrap();

        let ctx = WorkspaceContext::resolve(&deep, None).unwrap();
        assert_eq!(ctx.root, root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Weave { project } => {
                assert!(project.is_none());
            }
            WorkspaceLocation::Workweave { .. } => panic!("expected Weave"),
        }
    }

    // ========================================================================
    // Resolve from inside a project directory
    // ========================================================================

    #[test]
    fn resolve_from_inside_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let project_dir = root.join("projects").join("web-app");
        std::fs::create_dir_all(&project_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&project_dir, None).unwrap();
        assert_eq!(ctx.root, root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Weave { project } => {
                let p = project.as_ref().expect("project should be detected");
                assert_eq!(p.as_str(), "web-app");
            }
            WorkspaceLocation::Workweave { .. } => panic!("expected Weave"),
        }
    }

    // ========================================================================
    // Resolve from inside a workweave directory (legacy -- naming)
    // ========================================================================

    #[test]
    fn resolve_from_inside_weave_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        // Create a workweave sibling: ws--hotfix
        let weave_dir = tmp.path().join("ws--hotfix");
        std::fs::create_dir_all(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        assert_eq!(ctx.root, root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Workweave { name, dir, project } => {
                assert_eq!(name.as_str(), "hotfix");
                assert_eq!(*dir, weave_dir.canonicalize().unwrap());
                assert_eq!(project.as_str(), "ws");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Workweave"),
        }
    }

    // ========================================================================
    // Resolve from inside a repo within a workweave
    // ========================================================================

    #[test]
    fn resolve_from_repo_inside_weave() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let weave_dir = tmp.path().join("ws--feat-login");
        let repo_dir = weave_dir.join("github").join("acme").join("server");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&repo_dir, None).unwrap();
        assert_eq!(ctx.root, root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Workweave { name, dir, project } => {
                assert_eq!(name.as_str(), "feat-login");
                assert_eq!(*dir, weave_dir.canonicalize().unwrap());
                assert_eq!(project.as_str(), "ws");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Workweave"),
        }
    }

    // ========================================================================
    // Resolve from outside any workspace — should error
    // ========================================================================

    #[test]
    fn resolve_outside_workspace_errors() {
        let tmp = tempfile::tempdir().unwrap();
        // No workspace markers here
        let result = WorkspaceContext::resolve(tmp.path(), None);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("no repoweave workspace found"),
            "unexpected error message: {msg}"
        );
    }

    // ========================================================================
    // Resolve with --project override
    // ========================================================================

    #[test]
    fn resolve_with_project_override_in_weave_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let ctx =
            WorkspaceContext::resolve(&root, Some(ProjectName::new("overridden-project"))).unwrap();
        match &ctx.location {
            WorkspaceLocation::Weave { project } => {
                let p = project.as_ref().expect("project should be set");
                assert_eq!(p.as_str(), "overridden-project");
            }
            WorkspaceLocation::Workweave { .. } => panic!("expected Weave"),
        }
    }

    #[test]
    fn resolve_with_project_override_in_workweave() {
        let tmp = tempfile::tempdir().unwrap();
        let _root = make_workspace(tmp.path(), "ws");
        let weave_dir = tmp.path().join("ws--hotfix");
        std::fs::create_dir_all(&weave_dir).unwrap();

        let ctx =
            WorkspaceContext::resolve(&weave_dir, Some(ProjectName::new("custom-proj"))).unwrap();
        match &ctx.location {
            WorkspaceLocation::Workweave { project, .. } => {
                assert_eq!(project.as_str(), "custom-proj");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Workweave"),
        }
    }

    // ========================================================================
    // Resolve at workspace root itself
    // ========================================================================

    #[test]
    fn resolve_at_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        assert_eq!(ctx.root, root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Weave { project } => {
                assert!(project.is_none());
            }
            WorkspaceLocation::Workweave { .. } => panic!("expected Weave"),
        }
    }

    // ========================================================================
    // read_active_project
    // ========================================================================

    #[test]
    fn read_active_project_returns_none_when_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        assert!(read_active_project(&root).is_none());
    }

    #[test]
    fn read_active_project_returns_none_when_file_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "").unwrap();
        assert!(read_active_project(&root).is_none());
    }

    #[test]
    fn read_active_project_returns_none_when_file_whitespace_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "  \n  \n").unwrap();
        assert!(read_active_project(&root).is_none());
    }

    #[test]
    fn read_active_project_returns_project_name_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();
        let project = read_active_project(&root).expect("should return project");
        assert_eq!(project.as_str(), "web-app");
    }

    #[test]
    fn read_active_project_trims_whitespace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "  my-project  \n").unwrap();
        let project = read_active_project(&root).expect("should return project");
        assert_eq!(project.as_str(), "my-project");
    }

    // ========================================================================
    // set_active_project
    // ========================================================================

    #[test]
    fn set_active_project_writes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let project = ProjectName::new("web-app");
        set_active_project(&root, &project).unwrap();

        let content = std::fs::read_to_string(root.join(".rwv-active")).unwrap();
        assert_eq!(content, "web-app\n");
    }

    #[test]
    fn set_active_project_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        set_active_project(&root, &ProjectName::new("old-project")).unwrap();
        set_active_project(&root, &ProjectName::new("new-project")).unwrap();

        let project = read_active_project(&root).expect("should return project");
        assert_eq!(project.as_str(), "new-project");
    }

    #[test]
    fn set_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let project = ProjectName::new("mobile-app");
        set_active_project(&root, &project).unwrap();

        let read_back = read_active_project(&root).expect("should return project");
        assert_eq!(read_back, project);
    }

    // ========================================================================
    // resolve prefers .rwv-active over CWD inference in Weave
    // ========================================================================

    #[test]
    fn resolve_prefers_rwv_active_over_no_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        // CWD is workspace root (not inside projects/), so CWD inference yields None.
        // But .rwv-active is set.
        std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        match &ctx.location {
            WorkspaceLocation::Weave { project } => {
                let p = project
                    .as_ref()
                    .expect("project should come from .rwv-active");
                assert_eq!(p.as_str(), "web-app");
            }
            WorkspaceLocation::Workweave { .. } => panic!("expected Weave"),
        }
    }

    #[test]
    fn resolve_cwd_inference_takes_precedence_over_rwv_active() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        // Create a project directory so CWD inference works.
        let project_dir = root.join("projects").join("from-cwd");
        std::fs::create_dir_all(&project_dir).unwrap();
        // Set a different active project.
        std::fs::write(root.join(".rwv-active"), "from-file\n").unwrap();

        // CWD is inside projects/from-cwd, so CWD inference should win.
        let ctx = WorkspaceContext::resolve(&project_dir, None).unwrap();
        match &ctx.location {
            WorkspaceLocation::Weave { project } => {
                let p = project
                    .as_ref()
                    .expect("project should be detected from CWD");
                assert_eq!(p.as_str(), "from-cwd");
            }
            WorkspaceLocation::Workweave { .. } => panic!("expected Weave"),
        }
    }

    #[test]
    fn resolve_project_override_takes_precedence_over_rwv_active() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "from-file\n").unwrap();

        let ctx =
            WorkspaceContext::resolve(&root, Some(ProjectName::new("explicit-override"))).unwrap();
        match &ctx.location {
            WorkspaceLocation::Weave { project } => {
                let p = project.as_ref().expect("project should be set");
                assert_eq!(p.as_str(), "explicit-override");
            }
            WorkspaceLocation::Workweave { .. } => panic!("expected Weave"),
        }
    }

    // ========================================================================
    // require_workspace_or_empty
    // ========================================================================

    #[test]
    fn require_workspace_or_empty_ok_in_existing_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        // Inside a valid workspace — should succeed.
        assert!(require_workspace_or_empty(&root, false).is_ok());
    }

    #[test]
    fn require_workspace_or_empty_ok_in_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("fresh");
        std::fs::create_dir_all(&empty).unwrap();
        // Empty directory, no workspace markers — should succeed.
        assert!(require_workspace_or_empty(&empty, false).is_ok());
    }

    #[test]
    fn require_workspace_or_empty_errors_in_non_empty_non_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("messy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("random.txt"), "stuff").unwrap();
        // Non-empty, no workspace — should error.
        let err = require_workspace_or_empty(&dir, false).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--force"), "expected --force hint, got: {msg}");
    }

    #[test]
    fn require_workspace_or_empty_force_bypasses_check() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("messy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("random.txt"), "stuff").unwrap();
        // Non-empty + --force — should succeed.
        assert!(require_workspace_or_empty(&dir, true).is_ok());
    }

    // ========================================================================
    // WorkweaveMarker
    // ========================================================================

    #[test]
    fn workweave_marker_write_then_read() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let marker = WorkweaveMarker {
            primary: PathBuf::from("/home/user/weaveroot"),
            project: ProjectName::new("my-project"),
        };
        marker.write(dir).unwrap();

        let read_back = WorkweaveMarker::read(dir)
            .unwrap()
            .expect("marker should be Some");
        assert_eq!(read_back.primary, marker.primary);
        assert_eq!(read_back.project.as_str(), "my-project");
    }

    #[test]
    fn workweave_marker_read_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let result = WorkweaveMarker::read(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    // ========================================================================
    // Marker-based workweave resolution
    // ========================================================================

    #[test]
    fn resolve_from_workweave_with_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        // Create .workweaves/feat/ with a marker
        let workweaves_dir = tmp.path().join(".workweaves");
        let weave_dir = workweaves_dir.join("feat");
        std::fs::create_dir_all(&weave_dir).unwrap();

        let marker = WorkweaveMarker {
            primary: root.canonicalize().unwrap(),
            project: ProjectName::new("web-app"),
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        assert_eq!(ctx.root, root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Workweave { name, dir, project } => {
                assert_eq!(name.as_str(), "feat");
                assert_eq!(*dir, weave_dir.canonicalize().unwrap());
                assert_eq!(project.as_str(), "web-app");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Workweave"),
        }
    }

    #[test]
    fn resolve_from_repo_inside_workweave_with_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let workweaves_dir = tmp.path().join(".workweaves");
        let weave_dir = workweaves_dir.join("feat");
        let repo_dir = weave_dir.join("github").join("acme").join("server");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let marker = WorkweaveMarker {
            primary: root.canonicalize().unwrap(),
            project: ProjectName::new("web-app"),
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&repo_dir, None).unwrap();
        assert_eq!(ctx.root, root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Workweave { name, dir, project } => {
                assert_eq!(name.as_str(), "feat");
                assert_eq!(*dir, weave_dir.canonicalize().unwrap());
                assert_eq!(project.as_str(), "web-app");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Workweave"),
        }
    }

    #[test]
    fn resolve_from_workweave_with_dash_naming_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        // Sibling with -- naming, no marker file
        let weave_dir = tmp.path().join("ws--hotfix");
        std::fs::create_dir_all(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        assert_eq!(ctx.root, root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Workweave { name, dir, project } => {
                assert_eq!(name.as_str(), "hotfix");
                assert_eq!(*dir, weave_dir.canonicalize().unwrap());
                assert_eq!(project.as_str(), "ws");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Workweave"),
        }
    }

    #[test]
    fn resolve_marker_takes_precedence_over_naming() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        // Create a directory with BOTH a -- naming convention AND a marker
        // The marker says the project is "marker-project", name is "marker-name"
        // The dir name says primary is "ws", name is "dash-name"
        let weave_dir = tmp.path().join("ws--dash-name");
        std::fs::create_dir_all(&weave_dir).unwrap();

        let marker = WorkweaveMarker {
            primary: root.canonicalize().unwrap(),
            project: ProjectName::new("marker-project"),
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        // Marker takes precedence: project should be from marker
        match &ctx.location {
            WorkspaceLocation::Workweave { name, project, .. } => {
                // The workweave name comes from the directory name (last component)
                assert_eq!(name.as_str(), "ws--dash-name");
                assert_eq!(project.as_str(), "marker-project");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Workweave"),
        }
    }

    #[test]
    fn resolve_workweave_missing_marker_in_workweaves_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // No workspace root set up — just a .workweaves/feat/ dir with no marker
        let workweaves_dir = tmp.path().join(".workweaves");
        let weave_dir = workweaves_dir.join("feat");
        std::fs::create_dir_all(&weave_dir).unwrap();

        // Should NOT resolve as a workweave — no workspace found
        let result = WorkspaceContext::resolve(&weave_dir, None);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("no repoweave workspace found"),
            "unexpected error: {msg}"
        );
    }
}
