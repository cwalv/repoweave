//! Workspace: the resolved state of a repoweave directory tree.
//!
//! A workspace is the top-level directory containing registry dirs, projects,
//! and ecosystem files. This module resolves the workspace from CWD and
//! provides the context that commands operate on.

use crate::manifest::{Manifest, ProjectName, WeaveName};
use crate::registry::{builtin_registries, Registry};
use crate::vcs::Vcs;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Context — where are we?
// ---------------------------------------------------------------------------

/// The resolved workspace context, inferred from CWD.
///
/// Every `rwv` command starts by resolving this. It answers:
/// - Where is the workspace root?
/// - Are we in a primary or a weave?
/// - Which project is active?
#[derive(Debug)]
pub struct WorkspaceContext {
    /// The primary directory (workspace root with regular clones).
    pub root: PathBuf,
    /// The current working location: primary or a specific weave.
    pub location: WorkspaceLocation,
}

/// Whether we're in the primary directory or inside a weave.
#[derive(Debug)]
pub enum WorkspaceLocation {
    /// Working in the primary directory (regular clones).
    /// The active project is inferred from CWD or `--project`.
    Primary {
        project: Option<ProjectName>,
    },
    /// Working in a weave (worktrees on ephemeral branches).
    Weave {
        name: WeaveName,
        /// The weave directory path (e.g., `root/../web-app--agent-42/`).
        dir: PathBuf,
        /// The project this weave belongs to.
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
pub fn scan_repos_on_disk(root: &Path, registries: &[Box<dyn Registry>], vcs: &dyn Vcs) -> Vec<PathBuf> {
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
                    repos.push(rel.to_path_buf());
                }
            }
        }
    }

    repos
}

impl WorkspaceContext {
    /// Resolve the workspace context by walking up from `cwd`.
    ///
    /// If `project_override` is `Some`, it overrides the auto-detected project.
    /// In Primary location, `.rwv-active` is preferred over CWD inference when
    /// no explicit override is given.
    pub fn resolve(cwd: &Path, project_override: Option<ProjectName>) -> anyhow::Result<Self> {
        let cwd = cwd
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("failed to canonicalize {}: {e}", cwd.display()))?;

        // Walk ancestors looking for a workspace root OR a weave sibling pattern.
        // We check each ancestor directory name for the `{primary}--{weave}` pattern.
        // If found, the workspace root is a sibling directory named `{primary}`.
        let mut current = cwd.as_path();
        loop {
            // Check the weave pattern BEFORE workspace root markers, because a
            // weave directory may also contain registry subdirs (e.g. github/).
            if let Some(dir_name) = current.file_name().and_then(|n| n.to_str()) {
                if let Some((primary_name, weave_name)) = parse_weave_dir_name(dir_name) {
                    // The workspace root is the sibling with the primary name.
                    let parent = current
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("weave directory has no parent"))?;
                    let root = parent.join(primary_name);
                    if is_workspace_root(&root) {
                        let project = project_override.unwrap_or_else(|| {
                            ProjectName::new(primary_name)
                        });
                        return Ok(WorkspaceContext {
                            root: root.clone(),
                            location: WorkspaceLocation::Weave {
                                name: weave_name,
                                dir: current.to_path_buf(),
                                project,
                            },
                        });
                    }
                }
            }

            // Check if current directory IS the workspace root.
            if is_workspace_root(current) {
                let project = project_override
                    .or_else(|| detect_project(&cwd, current))
                    .or_else(|| read_active_project(current));
                return Ok(WorkspaceContext {
                    root: current.to_path_buf(),
                    location: WorkspaceLocation::Primary { project },
                });
            }

            // Move up to parent.
            match current.parent() {
                Some(parent) if parent != current => current = parent,
                _ => break,
            }
        }

        anyhow::bail!(
            "no repoweave workspace found above {}",
            cwd.display()
        )
    }

    /// Return the effective path for `rwv resolve`: the primary root or the
    /// weave directory.
    pub fn resolve_path(&self) -> &Path {
        match &self.location {
            WorkspaceLocation::Primary { .. } => &self.root,
            WorkspaceLocation::Weave { dir, .. } => dir,
        }
    }

    /// Display the workspace context to stdout.
    ///
    /// Shows root path, location type (primary/weave), active project, and
    /// available projects.
    pub fn display(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("Root: {}", self.root.display()));

        match &self.location {
            WorkspaceLocation::Primary { project } => {
                lines.push("Location: primary".to_string());
                if let Some(p) = project {
                    lines.push(format!("Active project: {}", p.as_str()));
                    // Try to load manifest and show repo count
                    let manifest_path = self.root.join("projects").join(p.as_str()).join("rwv.yaml");
                    if let Ok(manifest) = Manifest::from_path(&manifest_path) {
                        lines.push(format!("Repos: {}", manifest.repositories.len()));
                    }
                }
            }
            WorkspaceLocation::Weave { name, dir, project } => {
                lines.push(format!("Location: weave \"{}\"", name.as_str()));
                lines.push(format!("Weave dir: {}", dir.display()));
                lines.push(format!("Project: {}", project.as_str()));
                // Try to load manifest and show repo count
                let manifest_path = self.root.join("projects").join(project.as_str()).join("rwv.yaml");
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

/// A weave's directory name: `{primary}--{weave-name}`.
pub fn weave_dir_name(primary_name: &str, weave_name: &WeaveName) -> String {
    format!("{primary_name}--{weave_name}")
}

/// Parse a sibling directory name into `(primary_name, weave_name)` if it
/// matches the `{primary}--{name}` convention.
pub fn parse_weave_dir_name(dir_name: &str) -> Option<(&str, WeaveName)> {
    let (primary, weave) = dir_name.split_once("--")?;
    if primary.is_empty() || weave.is_empty() {
        return None;
    }
    Some((primary, WeaveName::new(weave)))
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
    // Resolve from inside a primary directory (registry subdir)
    // ========================================================================

    #[test]
    fn resolve_from_inside_primary_registry_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "myworkspace");
        let deep = root.join("github").join("acme").join("server");
        std::fs::create_dir_all(&deep).unwrap();

        let ctx = WorkspaceContext::resolve(&deep, None).unwrap();
        assert_eq!(ctx.root, root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Primary { project } => {
                assert!(project.is_none());
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Primary"),
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
            WorkspaceLocation::Primary { project } => {
                let p = project.as_ref().expect("project should be detected");
                assert_eq!(p.as_str(), "web-app");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Primary"),
        }
    }

    // ========================================================================
    // Resolve from inside a weave directory
    // ========================================================================

    #[test]
    fn resolve_from_inside_weave_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        // Create a weave sibling: ws--hotfix
        let weave_dir = tmp.path().join("ws--hotfix");
        std::fs::create_dir_all(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        assert_eq!(ctx.root, root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Weave { name, dir, project } => {
                assert_eq!(name.as_str(), "hotfix");
                assert_eq!(*dir, weave_dir.canonicalize().unwrap());
                assert_eq!(project.as_str(), "ws");
            }
            WorkspaceLocation::Primary { .. } => panic!("expected Weave"),
        }
    }

    // ========================================================================
    // Resolve from inside a repo within a weave
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
            WorkspaceLocation::Weave { name, dir, project } => {
                assert_eq!(name.as_str(), "feat-login");
                assert_eq!(*dir, weave_dir.canonicalize().unwrap());
                assert_eq!(project.as_str(), "ws");
            }
            WorkspaceLocation::Primary { .. } => panic!("expected Weave"),
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
    fn resolve_with_project_override_in_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let ctx = WorkspaceContext::resolve(
            &root,
            Some(ProjectName::new("overridden-project")),
        )
        .unwrap();
        match &ctx.location {
            WorkspaceLocation::Primary { project } => {
                let p = project.as_ref().expect("project should be set");
                assert_eq!(p.as_str(), "overridden-project");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Primary"),
        }
    }

    #[test]
    fn resolve_with_project_override_in_weave() {
        let tmp = tempfile::tempdir().unwrap();
        let _root = make_workspace(tmp.path(), "ws");
        let weave_dir = tmp.path().join("ws--hotfix");
        std::fs::create_dir_all(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(
            &weave_dir,
            Some(ProjectName::new("custom-proj")),
        )
        .unwrap();
        match &ctx.location {
            WorkspaceLocation::Weave { project, .. } => {
                assert_eq!(project.as_str(), "custom-proj");
            }
            WorkspaceLocation::Primary { .. } => panic!("expected Weave"),
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
            WorkspaceLocation::Primary { project } => {
                assert!(project.is_none());
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Primary"),
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
    // resolve prefers .rwv-active over CWD inference in Primary
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
            WorkspaceLocation::Primary { project } => {
                let p = project.as_ref().expect("project should come from .rwv-active");
                assert_eq!(p.as_str(), "web-app");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Primary"),
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
            WorkspaceLocation::Primary { project } => {
                let p = project.as_ref().expect("project should be detected from CWD");
                assert_eq!(p.as_str(), "from-cwd");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Primary"),
        }
    }

    #[test]
    fn resolve_project_override_takes_precedence_over_rwv_active() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "from-file\n").unwrap();

        let ctx = WorkspaceContext::resolve(
            &root,
            Some(ProjectName::new("explicit-override")),
        )
        .unwrap();
        match &ctx.location {
            WorkspaceLocation::Primary { project } => {
                let p = project.as_ref().expect("project should be set");
                assert_eq!(p.as_str(), "explicit-override");
            }
            WorkspaceLocation::Weave { .. } => panic!("expected Primary"),
        }
    }
}
