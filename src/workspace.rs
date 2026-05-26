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
/// - Where is the primary weave?
/// - Are we currently in the primary weave or in a workweave?
/// - Which project is active?
///
/// Two distinct paths are exposed; choose deliberately:
/// - [`primary_path`] — the primary weave directory. Use for state owned by
///   the workspace as a whole (`.rwv-active`, `projects/` enumeration,
///   `.workweaves/` listing, AGENTS.md).
/// - [`active_path`] — the directory CWD is actually in: the primary path
///   when in a weave, the workweave directory when in a workweave. Use for
///   per-workspace state (project worktrees and their `rwv.lock` /
///   `rwv.yaml`, repo worktrees the operator is working in).
///
/// [`primary_path`]: WorkspaceContext::primary_path
/// [`active_path`]: WorkspaceContext::active_path
#[derive(Debug)]
pub struct WorkspaceContext {
    primary_root: PathBuf,
    /// The current working location: weave or a specific workweave.
    pub location: WorkspaceLocation,
    /// The project name inferred from CWD (when CWD is inside
    /// `{root}/projects/{name}/...`), independent of the active project.
    ///
    /// Recorded for diagnostics — `rwv` bare status surfaces the
    /// divergence, and command implementations use it to build the
    /// "you're in projects/<X>/ but <Y> is active" error message now
    /// that the CWD override has been removed.
    cwd_project_hint: Option<ProjectName>,
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
        .map(|r| r.name().as_str().to_owned())
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
///
/// This is a *soft hint* only: action verbs no longer use it to override
/// `.rwv-active`. Two callers remain:
/// - [`WorkspaceContext::cwd_project_hint`] for divergence warnings and
///   for the helpful error when CWD ≠ active project.
/// - `rwv` bare status, to flag the divergence to the user.
///
/// The previous behaviour — silently substituting this for the active
/// project on `rwv lock`, `rwv add`, etc. — let symlinks and manifests
/// disagree without any signal. Removing the override collapses the two
/// notions of "active" into one (`.rwv-active`).
pub fn detect_project(cwd: &Path, root: &Path) -> Option<ProjectName> {
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
        let reg_dir = root.join(reg.name().as_str());
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
                    // Normalize to forward slashes before constructing a RepoPath
                    // so the invariant holds on all platforms. On Windows,
                    // Path::to_string_lossy() produces backslashes; replace them
                    // here at the OS boundary before the validated constructor runs.
                    let fwd = rel.to_string_lossy().replace('\\', "/");
                    repos.push(
                        RepoPath::new(fwd)
                            .expect("path after backslash-to-slash normalization cannot fail"),
                    );
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
    /// Project resolution order:
    ///   1. `project_override` — explicit `--project <name>` flag.
    ///   2. `.rwv-active` — the single source of truth for the active
    ///      project. There is no CWD override anymore.
    ///
    /// The "CWD is inside `projects/<X>/`" inference is still computed
    /// and recorded on the context as [`cwd_project_hint`] so that
    /// diagnostics and `rwv` bare status can surface a divergence
    /// warning — it is no longer consulted for verb resolution.
    pub fn resolve(cwd: &Path, project_override: Option<ProjectName>) -> anyhow::Result<Self> {
        let cwd = cwd
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("failed to canonicalize {}: {e}", cwd.display()))?;

        // Walk ancestors looking for a workspace root OR a workweave pattern.
        //
        // For each ancestor directory we check (in order):
        //   1. Does it have a `.rwv-workweave` marker? If so, use that.
        //   2. Does its name match `{project}--{name}` (or legacy
        //      `{primary}--{name}`)? If so, fall back to sibling-resolution
        //      using the parsed left component. The marker is authoritative;
        //      this path only fires for workweaves missing a marker.
        //   3. Is it a workspace root itself?
        let mut current = cwd.as_path();
        loop {
            // 1. Check for `.rwv-workweave` marker file in the current directory.
            if let Ok(Some(marker)) = WorkweaveMarker::read(current) {
                // The marker tells us exactly where the primary workspace is and
                // which project this workweave belongs to.
                let root = marker.primary.clone();
                if is_workspace_root(&root) {
                    let dir_basename = current
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown");
                    // Workweave directories follow `<project>--<name>`. Strip
                    // the `<project>--` prefix so the workweave name can be
                    // round-tripped through `workweave_path_for` for delete /
                    // retire flows. parse_weave_dir_name returns the
                    // right-hand side; fall back to the full basename for
                    // exotic shapes that don't parse.
                    let workweave_name = parse_weave_dir_name(dir_basename)
                        .map(|(_, n)| n)
                        .unwrap_or_else(|| WorkweaveName::new(dir_basename));
                    let project = project_override.unwrap_or(marker.project);
                    let cwd_project_hint = detect_project(&cwd, &root);
                    return Ok(WorkspaceContext {
                        primary_root: root,
                        location: WorkspaceLocation::Workweave {
                            name: workweave_name,
                            dir: current.to_path_buf(),
                            project,
                        },
                        cwd_project_hint,
                    });
                }
            }

            // 2. Check the `{left}--{name}` naming convention (legacy
            //    sibling-resolution fallback for workweaves missing a marker).
            //    A workweave directory may also contain registry subdirs (e.g.
            //    github/), so check this BEFORE workspace root markers.
            //
            //    The left component is taken as the legacy primary's basename
            //    (the only form that ever shipped without a marker). If the
            //    sibling resolves to a workspace root we use it; otherwise we
            //    fall through.
            if let Some(dir_name) = current.file_name().and_then(|n| n.to_str()) {
                if let Some((left_name, workweave_name)) = parse_weave_dir_name(dir_name) {
                    // The workspace root is the sibling named after the left
                    // component (legacy primary-name convention).
                    let parent = current
                        .parent()
                        .ok_or_else(|| anyhow::anyhow!("workweave directory has no parent"))?;
                    let root = parent.join(left_name);
                    if is_workspace_root(&root) {
                        let project =
                            project_override.unwrap_or_else(|| ProjectName::new(left_name));
                        let cwd_project_hint = detect_project(&cwd, &root);
                        return Ok(WorkspaceContext {
                            primary_root: root.clone(),
                            location: WorkspaceLocation::Workweave {
                                name: workweave_name,
                                dir: current.to_path_buf(),
                                project,
                            },
                            cwd_project_hint,
                        });
                    }
                }
            }

            // 3. Check if current directory IS the workspace root.
            if is_workspace_root(current) {
                let cwd_project_hint = detect_project(&cwd, current);
                let project = project_override.or_else(|| read_active_project(current));
                return Ok(WorkspaceContext {
                    primary_root: current.to_path_buf(),
                    location: WorkspaceLocation::Weave { project },
                    cwd_project_hint,
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

    /// The project name inferred from CWD's location under
    /// `{root}/projects/{name}/...`, or `None` when CWD is not inside any
    /// project directory.
    ///
    /// Use to (a) surface a "you are in projects/<X>/ but <Y> is active"
    /// warning in `rwv` bare status, and (b) construct a helpful error
    /// when action verbs are run from a project directory whose name
    /// disagrees with `.rwv-active`. Do *not* use it as a project
    /// override for action verbs — that silent override was the bug we
    /// removed.
    pub fn cwd_project_hint(&self) -> Option<&ProjectName> {
        self.cwd_project_hint.as_ref()
    }

    /// The active project from the resolved context, or `None` when no
    /// project is active (no `.rwv-active`, no `--project`).
    pub fn active_project(&self) -> Option<&ProjectName> {
        match &self.location {
            WorkspaceLocation::Weave { project } => project.as_ref(),
            WorkspaceLocation::Workweave { project, .. } => Some(project),
        }
    }

    /// Returns `Ok(name)` for the active project, or an `Err` whose
    /// message guides the user to either `rwv activate <X>` or
    /// `--project <X>` when CWD is inside a non-active project directory.
    ///
    /// The CWD-vs-active divergence error is the user-facing successor
    /// to the silent CWD override that used to live here. Calling this
    /// from every action-verb entry point keeps the error in one place.
    pub fn require_active_project(&self) -> anyhow::Result<&ProjectName> {
        if let Some(name) = self.active_project() {
            // Active project is set; but if CWD hints at a different
            // project, warn (stderr) so the user knows the divergence.
            if let Some(hint) = self.cwd_project_hint() {
                if hint != name {
                    eprintln!(
                        "warning: you are in projects/{}/, but the active project is {}; \
                         pass `--project {}` to operate on the CWD project, or \
                         `rwv activate {}` to switch.",
                        hint.as_str(),
                        name.as_str(),
                        hint.as_str(),
                        hint.as_str(),
                    );
                }
            }
            return Ok(name);
        }

        // No active project. Build a helpful error.
        if let Some(hint) = self.cwd_project_hint() {
            anyhow::bail!(
                "no active project set, but CWD is inside projects/{}/. \
                 Run `rwv activate {}` to make it active, or pass `--project {}` \
                 for a one-shot operation.",
                hint.as_str(),
                hint.as_str(),
                hint.as_str(),
            );
        }
        anyhow::bail!(
            "no active project found; run `rwv activate <name>` or pass `--project <name>`"
        );
    }

    /// The primary weave directory.
    ///
    /// Use this for state owned by the workspace as a whole — the
    /// `.rwv-active` file, the `projects/` directory used to enumerate
    /// projects, the `.workweaves/` directory used to enumerate workweaves,
    /// and workspace-level config files like `AGENTS.md`. These all live
    /// under the primary regardless of where CWD currently is.
    pub fn primary_path(&self) -> &Path {
        &self.primary_root
    }

    /// The directory CWD is actually in: the primary path when in a weave,
    /// the workweave directory when in a workweave.
    ///
    /// Use this for per-workspace state — project worktrees and their
    /// `rwv.lock` / `rwv.yaml`, the repo worktrees the operator is working
    /// in, integration outputs that follow CWD's workspace. A workweave is
    /// itself a workspace; reading or writing through the primary from
    /// inside a workweave clobbers the workweave's view of the world.
    pub fn active_path(&self) -> &Path {
        match &self.location {
            WorkspaceLocation::Weave { .. } => &self.primary_root,
            WorkspaceLocation::Workweave { dir, .. } => dir,
        }
    }

    /// Display the workspace context to stdout.
    ///
    /// Shows weave path, workweave (if applicable), active project, and
    /// available projects. Also surfaces a warning line when CWD is
    /// inside a project directory whose name differs from the active
    /// project, so the user sees the divergence the silent CWD override
    /// used to hide.
    pub fn display(&self) -> String {
        let mut lines = Vec::new();

        let active = self.active_project().cloned();
        match &self.location {
            WorkspaceLocation::Weave { .. } => {
                lines.push(format!("Weave: {}", self.primary_root.display()));
                if let Some(p) = &active {
                    lines.push(format!("Project: {}", p.as_str()));
                    let manifest_path = self
                        .primary_root
                        .join("projects")
                        .join(p.as_str())
                        .join("rwv.yaml");
                    if let Ok(manifest) = Manifest::from_path(&manifest_path) {
                        lines.push(format!("Repos: {}", manifest.repositories.len()));
                    }
                }
            }
            WorkspaceLocation::Workweave { name: _, dir, .. } => {
                lines.push(format!("Workweave: {}", dir.display()));
                lines.push(format!("Weave: {}", self.primary_root.display()));
                if let Some(p) = &active {
                    lines.push(format!("Project: {}", p.as_str()));
                    let manifest_path = self
                        .primary_root
                        .join("projects")
                        .join(p.as_str())
                        .join("rwv.yaml");
                    if let Ok(manifest) = Manifest::from_path(&manifest_path) {
                        lines.push(format!("Repos: {}", manifest.repositories.len()));
                    }
                }
            }
        }

        // Surface CWD vs active divergence.
        if let (Some(hint), Some(active_p)) = (self.cwd_project_hint(), active.as_ref()) {
            if hint != active_p {
                lines.push(format!(
                    "Warning: CWD is in projects/{}/, but {} is the active project (pass `--project {}` for a one-shot, or `rwv activate {}` to switch)",
                    hint.as_str(),
                    active_p.as_str(),
                    hint.as_str(),
                    hint.as_str(),
                ));
            }
        }

        // List available projects
        let projects_dir = self.primary_root.join("projects");
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

/// Build a workweave directory name using the `{project}--{name}` convention.
///
/// Workweaves are keyed by the project they're created for so that the directory
/// layout makes the project explicit and `<project>--<name>` is stable across
/// fork sources. Old workweaves on disk may follow the legacy
/// `{primary}--{name}` form (where the left side was the primary weave's
/// directory basename); both are accepted by [`WorkspaceContext::resolve`] via
/// the `.rwv-workweave` marker file (see [`WorkweaveMarker`]), which is
/// authoritative.
pub fn weave_dir_name(project_name: &str, workweave_name: &WorkweaveName) -> String {
    format!("{project_name}--{workweave_name}")
}

/// Parse a directory name into `(left, workweave_name)` if it matches the
/// `{left}--{name}` shape. The `left` component is the project name under the
/// current convention; for legacy on-disk workweaves it is the primary weave's
/// directory basename. Disambiguation, when it matters, is done by reading the
/// `.rwv-workweave` marker.
pub fn parse_weave_dir_name(dir_name: &str) -> Option<(&str, WorkweaveName)> {
    let (left, workweave) = dir_name.split_once("--")?;
    if left.is_empty() || workweave.is_empty() {
        return None;
    }
    Some((left, WorkweaveName::new(workweave)))
}

// ---------------------------------------------------------------------------
// WorkweaveMarker — `.rwv-workweave` marker file
// ---------------------------------------------------------------------------

/// Metadata written to `.rwv-workweave` in a workweave root.
///
/// Records the relationship to the primary workspace and the workspace this
/// workweave was forked from.
///
/// `parent` is the workspace the workweave was created from: `primary` when
/// created from the primary, the parent workweave's path when created from
/// inside another workweave. Workweaves form a tree; `parent` lets `rwv sync`
/// (with no explicit source) sync one hop toward the primary.
///
/// The field is optional on disk (`#[serde(default)]`) so that markers written
/// before parent tracking parse cleanly; [`Self::read`] backfills missing
/// values to `primary` so callers can rely on a `Some` parent for any marker
/// that exists on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkweaveMarker {
    pub primary: PathBuf,
    pub project: ProjectName,
    /// Workspace this workweave was forked from. Backfilled to `primary` for
    /// pre-existing markers that predate this field.
    #[serde(default)]
    pub parent: Option<PathBuf>,
}

impl WorkweaveMarker {
    /// Read the marker file from `dir`.
    ///
    /// Returns `Ok(None)` if the marker is absent. When present, missing
    /// `parent` (legacy markers written before parent tracking landed) is
    /// backfilled to `primary` so callers always see a `Some(_)` value.
    pub fn read(dir: &Path) -> anyhow::Result<Option<Self>> {
        let path = dir.join(".rwv-workweave");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", path.display()))?;
        let mut marker: Self = serde_yaml::from_str(&content).map_err(|e| {
            anyhow::anyhow!("failed to parse .rwv-workweave at {}: {e}", path.display())
        })?;
        // Backfill missing parent (legacy markers): default to primary so
        // bare `rwv sync` works on workweaves that predate parent tracking.
        if marker.parent.is_none() {
            marker.parent = Some(marker.primary.clone());
        }
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
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Weave { project } => {
                assert!(project.is_none());
            }
            WorkspaceLocation::Workweave { .. } => panic!("expected Weave"),
        }
    }

    // ========================================================================
    // Resolve from inside a project directory
    //
    // CWD location no longer drives the active project. The project is
    // `None` (no `.rwv-active` set), but the cwd_project_hint records
    // the directory's name for diagnostics.
    // ========================================================================

    #[test]
    fn resolve_from_inside_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let project_dir = root.join("projects").join("web-app");
        std::fs::create_dir_all(&project_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&project_dir, None).unwrap();
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
        match &ctx.location {
            WorkspaceLocation::Weave { project } => {
                assert!(
                    project.is_none(),
                    "without .rwv-active or --project, location.project is None"
                );
            }
            WorkspaceLocation::Workweave { .. } => panic!("expected Weave"),
        }
        // The CWD hint must still be populated for diagnostics.
        let hint = ctx.cwd_project_hint().expect("CWD hint should be set");
        assert_eq!(hint.as_str(), "web-app");
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
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
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
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
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
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
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
    fn resolve_rwv_active_wins_over_cwd_location() {
        // `.rwv-active` is the single source of truth. The previous
        // behaviour — silently substituting CWD's project directory for
        // the active one — let symlinks and manifests diverge.
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let project_dir = root.join("projects").join("from-cwd");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(root.join(".rwv-active"), "from-file\n").unwrap();

        // CWD is inside projects/from-cwd, but `.rwv-active` should still win.
        let ctx = WorkspaceContext::resolve(&project_dir, None).unwrap();
        match &ctx.location {
            WorkspaceLocation::Weave { project } => {
                let p = project
                    .as_ref()
                    .expect("project should come from .rwv-active");
                assert_eq!(p.as_str(), "from-file");
            }
            WorkspaceLocation::Workweave { .. } => panic!("expected Weave"),
        }
        // The hint should still record the CWD directory for diagnostics.
        let hint = ctx
            .cwd_project_hint()
            .expect("CWD hint should be set when CWD is inside projects/<name>/");
        assert_eq!(hint.as_str(), "from-cwd");
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
            parent: None,
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

    #[test]
    fn workweave_marker_parent_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let primary = PathBuf::from("/home/user/weaveroot/primary");
        let parent = PathBuf::from("/home/user/weaveroot/.workweaves/primary--ww1");
        let marker = WorkweaveMarker {
            primary: primary.clone(),
            project: ProjectName::new("p"),
            parent: Some(parent.clone()),
        };
        marker.write(dir).unwrap();

        let read_back = WorkweaveMarker::read(dir).unwrap().unwrap();
        assert_eq!(read_back.parent, Some(parent));
    }

    #[test]
    fn workweave_marker_missing_parent_backfills_to_primary() {
        // Legacy marker written before parent tracking: only primary and
        // project fields present on disk. read() must backfill parent to
        // primary so bare `rwv sync` works without re-writing the marker.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let legacy = "primary: /home/user/weaveroot/primary\nproject: legacy-project\n";
        std::fs::write(dir.join(".rwv-workweave"), legacy).unwrap();

        let marker = WorkweaveMarker::read(dir).unwrap().unwrap();
        assert_eq!(
            marker.parent,
            Some(PathBuf::from("/home/user/weaveroot/primary")),
            "missing parent should backfill to primary"
        );
    }

    #[test]
    fn workweave_marker_explicit_parent_not_backfilled() {
        // A marker with an explicit parent (e.g. forked from another
        // workweave) must round-trip without being overwritten by primary.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let yaml = "primary: /home/user/primary\n\
                    project: p\n\
                    parent: /home/user/.workweaves/primary--ww1\n";
        std::fs::write(dir.join(".rwv-workweave"), yaml).unwrap();

        let marker = WorkweaveMarker::read(dir).unwrap().unwrap();
        assert_eq!(
            marker.parent,
            Some(PathBuf::from("/home/user/.workweaves/primary--ww1")),
            "explicit parent must survive read"
        );
        // And primary remains its own value, not overwritten.
        assert_eq!(marker.primary, PathBuf::from("/home/user/primary"));
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
            parent: None,
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
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
            parent: None,
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&repo_dir, None).unwrap();
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
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
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
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
            parent: None,
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        // Marker takes precedence for project (from marker, not from the
        // directory's left component). Workweave name comes from the
        // right-hand side of `<project>--<name>` so downstream lookups
        // (workweave_path_for, delete_workweave) can reconstruct the on-disk
        // directory: `<workweave_parent>/<project-from-marker>--<name>`.
        match &ctx.location {
            WorkspaceLocation::Workweave { name, project, .. } => {
                assert_eq!(name.as_str(), "dash-name");
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
