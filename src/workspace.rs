//! Workspace: the resolved state of a repoweave directory tree.
//!
//! A workspace is the top-level directory containing registry dirs, projects,
//! and ecosystem files. This module resolves the workspace from an *origin
//! directory* (the input to resolution) and provides the context that
//! commands operate on.
//!
//! ## Single resolution point
//!
//! rwv acquires the origin dir exactly once per invocation — the top of
//! `main` calls [`acquire_origin_dir`], and every downstream handler
//! receives an already-resolved [`WorkspaceContext`]. There must be no
//! other `std::env::current_dir()` calls anywhere in the CLI code path:
//! resolution is a pure function of `(argv, origin_dir)`, and any handler
//! that consulted process-wide ambient state independently would break
//! that contract (silently retargeting under agent harnesses that reset
//! cwd, or leaking the address into spawned subprocesses if we ever
//! `chdir`'d — which we don't). The rule in one line: argv addresses;
//! cwd and env are never consulted past this point.
//!
//! The two steps — *acquire origin dir* and *resolve context from origin
//! dir* — are kept separate so a future `-C <path>` or `-w <name>` flag
//! can supply a different origin without restructuring: the CLI would
//! feed a flag-derived path to [`WorkspaceContext::resolve`] instead of
//! [`acquire_origin_dir`], and everything downstream would still work.

use crate::git::GitVcs;
use crate::integration_runner::IntegrationContextBase;
use crate::manifest::{Manifest, ProjectName, RepoPath, WorkweaveName};
use crate::registry::{builtin_registries, Registry};
use crate::vcs::Vcs;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Acquire the origin directory for the invocation.
///
/// **This is the single sanctioned `std::env::current_dir()` call site in
/// the rwv CLI.** All resolution flows through here and then
/// [`WorkspaceContext::resolve`]; handlers must receive an already-resolved
/// context and must not consult the process cwd on their own.
///
/// The distinction between *acquire* and *resolve* is load-bearing: a
/// future `-C <path>` / `-w <name>` flag will inject a different origin
/// dir into the same resolver, and the rest of the code path must not
/// need to change to accommodate that.
pub fn acquire_origin_dir() -> anyhow::Result<PathBuf> {
    std::env::current_dir().context("failed to read current directory")
}

// ---------------------------------------------------------------------------
// Context — where are we?
// ---------------------------------------------------------------------------

/// The resolved workspace context, inferred from an origin directory.
///
/// Every `rwv` command starts by resolving this. It answers:
/// - Where is the primary weave?
/// - Which kind of checkout are we in — the primary, or a workweave?
/// - Which project is active?
///
/// Two distinct paths are exposed; choose deliberately:
/// - [`primary_path`] — the primary weave directory. Use for state owned by
///   the workspace as a whole (`.rwv-active`, `projects/` enumeration,
///   `.workweaves/` listing, AGENTS.md).
/// - [`active_path`] — the directory the checkout points to: the primary
///   path when in a primary, the workweave directory when in a workweave.
///   Use for per-workspace state (project worktrees and their `rwv.lock` /
///   `rwv.yaml`, repo worktrees the operator is working in).
///
/// [`primary_path`]: WorkspaceContext::primary_path
/// [`active_path`]: WorkspaceContext::active_path
#[derive(Debug, Clone)]
pub struct WorkspaceContext {
    primary_root: PathBuf,
    /// Which kind of checkout the origin dir resolved into: the primary,
    /// or a specific workweave.
    pub checkout: Checkout,
    /// The project name inferred from the origin dir (when the origin is
    /// inside `{root}/projects/{name}/...`), independent of the active
    /// project.
    ///
    /// Recorded for diagnostics — `rwv` bare status surfaces the
    /// divergence, and command implementations use it to build the
    /// "you're in projects/<X>/ but <Y> is active" error message now
    /// that the CWD override has been removed.
    cwd_project_hint: Option<ProjectName>,
}

/// Which kind of checkout the resolved origin dir sits inside.
///
/// A workspace has two kinds of checkouts: the primary (regular clones),
/// and any number of workweaves (worktrees on ephemeral branches). The
/// design vocabulary is `checkout ∈ {primary, workweave}`; this enum
/// answers "which of the two".
#[derive(Debug, Clone)]
pub enum Checkout {
    /// The origin dir resolved into the primary weave directory.
    /// The active project is drawn from `.rwv-active` or `--project`.
    Primary { project: Option<ProjectName> },
    /// The origin dir resolved into a workweave (worktrees on ephemeral branches).
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

/// Returns `true` when the $HOME ceiling should block the walk from `current`
/// to `parent`.
///
/// The ceiling fires when `current` is inside `home` but `parent` is not —
/// i.e., the walk is about to cross above the home directory boundary.
///
/// Both `current` and `parent` must already be canonicalized (symlinks
/// resolved), and `home` must likewise be the canonicalized home path so that
/// the `starts_with` comparison is reliable on symlinked-home systems.
///
/// Extracted as a pure function so tests can drive it with an arbitrary
/// (possibly symlinked) home path without mutating process-wide state.
fn home_ceiling_blocks(current: &Path, parent: &Path, home: Option<&Path>) -> bool {
    match home {
        Some(h) => current.starts_with(h) && !parent.starts_with(h),
        None => false,
    }
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
        .with_context(|| format!("failed to write {}", path.display()))
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
    /// combined with the per-invocation `output_dir`, `project`, and the
    /// project's optional `workweave:` config.
    ///
    /// The `workweave` argument is the project's `workweave:` section from
    /// `rwv.yaml` (typically `manifest.workweave.as_ref()`). It is threaded
    /// through to integrations so they can detect cross-section collisions
    /// such as a name claimed by both `static-files.files` and
    /// `workweave.link` (see rwv-c5h / plan §5h).
    pub fn context_base<'a>(
        &'a self,
        output_dir: &'a Path,
        project: &'a ProjectName,
        detection_cache: &'a std::collections::HashMap<String, Vec<String>>,
        workweave: Option<&'a crate::manifest::WorkweaveConfig>,
    ) -> IntegrationContextBase<'a> {
        IntegrationContextBase {
            output_dir,
            workspace_root: &self.root,
            project,
            all_repos_on_disk: &self.repos_on_disk,
            all_project_paths: &self.project_paths,
            detection_cache,
            workweave,
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
/// commands (`fetch`, `init`).
///
/// - If [`WorkspaceContext::resolve`] succeeds, returns `Ok(())` — we are
///   inside an existing workspace.
/// - If resolve fails and `cwd` is an empty directory, returns `Ok(())` —
///   bootstrapping into a fresh directory is fine.
/// - If resolve fails and `cwd` is **non-empty**, returns an error advising
///   the caller to use `--force`.
///
/// Pass `force = true` to skip the non-empty check entirely.
///
/// Callers that expose a different interface (e.g. `init`, which has no
/// `--force` flag) should map the returned error to a command-specific
/// message rather than surfacing the raw `--force` hint.
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
            .with_context(|| format!("failed to canonicalize {}", cwd.display()))?;

        // Hard ceiling: never search above $HOME.
        //
        // Without this boundary, a walk-up that starts inside $HOME (e.g.
        // from inside a workweave directory) could escape to /, then find
        // workspace markers in an unrelated filesystem branch.  A test
        // sandbox whose subprocess CWD is accidentally inside the active
        // workweave would resolve the *real* primary workspace instead of its
        // own temp-dir workspace and mutate it.
        //
        // The ceiling fires when `current` is about to cross *above* $HOME:
        // if `current` is still within $HOME but its parent is not, we stop
        // rather than walking into territory that can never legitimately
        // contain a user workspace.  Paths already outside $HOME (e.g.
        // /tmp/…) walk normally until the filesystem root — there is no
        // cross-branch escape risk from those trees because their ancestors
        // never include $HOME-rooted directories.
        // Canonicalize home so that the ceiling check compares against the
        // real path.  On systems where $HOME contains a symlinked component
        // (e.g. /home -> /private/home on macOS, or a bespoke symlink on
        // Linux), `dirs::home_dir()` returns the raw env value while `cwd`
        // above has already been canonicalized.  Without canonicalization the
        // `starts_with` test always returns false (the paths are spelled
        // differently) and the ceiling silently never fires.
        let home_dir = dirs::home_dir().and_then(|h| h.canonicalize().ok());

        // Walk ancestors looking for a workspace root OR a workweave pattern.
        //
        // For each ancestor directory we check (in order):
        //   1. Does it have a `.rwv-workweave` marker? If so, use that.
        //      The marker is authoritative; all live workweaves carry one.
        //   2. Is it a workspace root itself?
        let mut current = cwd.as_path();
        loop {
            // 1. Check for `.rwv-workweave` marker file in the current directory.
            // Propagate read errors (including legacy-marker errors) immediately
            // so the operator sees an actionable message rather than a silent
            // fallback to name-based resolution.
            let marker_result = WorkweaveMarker::read(current)?;
            if let Some(marker) = marker_result {
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
                    // resolved via the primary-side registry for delete /
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
                        checkout: Checkout::Workweave {
                            name: workweave_name,
                            dir: current.to_path_buf(),
                            project,
                        },
                        cwd_project_hint,
                    });
                }
            }

            // 2. Check if current directory IS the workspace root.
            if is_workspace_root(current) {
                let cwd_project_hint = detect_project(&cwd, current);
                let project = project_override.or_else(|| read_active_project(current));
                return Ok(WorkspaceContext {
                    primary_root: current.to_path_buf(),
                    checkout: Checkout::Primary { project },
                    cwd_project_hint,
                });
            }

            // Move up to parent.
            match current.parent() {
                Some(parent) if parent != current => {
                    // Apply the $HOME ceiling: if `current` is inside $HOME but
                    // `parent` is not, stop here rather than walking above $HOME.
                    // This prevents workspace resolution from escaping to an
                    // unrelated filesystem branch (e.g. a test sandbox running
                    // inside a workweave from reaching the real primary weave).
                    if home_ceiling_blocks(current, parent, home_dir.as_deref()) {
                        break;
                    }
                    current = parent;
                }
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
        match &self.checkout {
            Checkout::Primary { project } => project.as_ref(),
            Checkout::Workweave { project, .. } => Some(project),
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

    /// Returns `Ok(name)` for the active project after verifying the project
    /// directory exists on disk under `projects/<name>/`.
    ///
    /// Distinguishes three cases:
    /// - No active project set (no `.rwv-active`, no `--project`): delegates
    ///   to [`require_active_project`] for the standard "no active project"
    ///   error message.
    /// - Active project named **and** directory exists: returns `Ok(name)`.
    /// - Active project named **but** directory is missing on disk (dangling
    ///   pointer): returns a clear, actionable error.
    ///
    /// All action verbs (`lock`, `add`, `remove`, `sync`, `sync-to`, `push`,
    /// `fetch`, `update`, `status`) must call this instead of
    /// [`require_active_project`] so that a stale `.rwv-active` file does not
    /// silently proceed into confusing downstream errors.
    pub fn require_active_project_on_disk(&self) -> anyhow::Result<&ProjectName> {
        let name = self.require_active_project()?;

        // Check that `projects/<name>/` exists on disk.
        let project_dir = self.primary_path().join("projects").join(name.as_str());
        if project_dir.is_dir() {
            return Ok(name);
        }

        // Dangling pointer: named but missing. Build the actionable error.
        let existing = discover_project_paths(self.primary_path());
        let hint = if existing.is_empty() {
            String::new()
        } else {
            format!(" Existing projects: {}.", existing.join(", "))
        };
        anyhow::bail!(
            "active project `{}` is set in `.rwv-active` but `projects/{}/` does not exist; \
             run `rwv activate <existing-project>` or remove `.rwv-active`.{}",
            name.as_str(),
            name.as_str(),
            hint,
        )
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
        match &self.checkout {
            Checkout::Primary { .. } => &self.primary_root,
            Checkout::Workweave { dir, .. } => dir,
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
        match &self.checkout {
            Checkout::Primary { .. } => {
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
            Checkout::Workweave { name: _, dir, .. } => {
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
/// fork sources. The `.rwv-workweave` marker (see [`WorkweaveMarker`]) is
/// authoritative for all live workweaves.
pub fn weave_dir_name(project_name: &str, workweave_name: &WorkweaveName) -> String {
    format!("{project_name}--{workweave_name}")
}

/// Parse a directory name into `(left, workweave_name)` if it matches the
/// `{left}--{name}` shape. The `left` component is the project name. Used by
/// resolve() to extract the workweave name from a marker-bearing directory.
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
/// All three fields (`primary`, `project`, `parent`) are required. Markers
/// written before `parent` was introduced (legacy markers) must be migrated
/// with `rwv doctor --fix` before the workweave can be used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkweaveMarker {
    pub primary: PathBuf,
    pub project: ProjectName,
    /// Workspace this workweave was forked from.
    pub parent: PathBuf,
}

impl WorkweaveMarker {
    /// Read the marker file from `dir`.
    ///
    /// Returns `Ok(None)` if the marker file is absent.
    ///
    /// Returns `Err` if the file is present but missing the required `parent:`
    /// field (legacy marker written before parent tracking landed). The error
    /// message names the file and directs the operator to run
    /// `rwv doctor --fix` to migrate. All three fields (`primary`, `project`,
    /// `parent`) must be present; there is no silent backfill.
    pub fn read(dir: &Path) -> anyhow::Result<Option<Self>> {
        let path = dir.join(".rwv-workweave");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        // Pre-check: if the YAML parses but `parent` is absent, reject early
        // with an actionable error rather than letting serde emit a cryptic
        // "missing field" message.
        let raw: serde_yaml::Value = serde_yaml::from_str(&content)
            .with_context(|| format!("failed to parse .rwv-workweave at {}", path.display()))?;
        if raw.get("parent").map(|v| v.is_null()).unwrap_or(true) {
            anyhow::bail!(
                "{} is a legacy workweave marker missing the required `parent:` field. \
                 Run `rwv doctor --fix` to migrate it before using this workweave.",
                path.display()
            );
        }
        let marker: Self = serde_yaml::from_value(raw)
            .with_context(|| format!("failed to parse .rwv-workweave at {}", path.display()))?;
        Ok(Some(marker))
    }

    pub fn write(&self, dir: &Path) -> anyhow::Result<()> {
        let path = dir.join(".rwv-workweave");
        let content = serde_yaml::to_string(self).context("failed to serialize .rwv-workweave")?;
        std::fs::write(&path, content)
            .with_context(|| format!("failed to write {}", path.display()))?;
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
        match &ctx.checkout {
            Checkout::Primary { project } => {
                assert!(project.is_none());
            }
            Checkout::Workweave { .. } => panic!("expected Primary"),
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
        match &ctx.checkout {
            Checkout::Primary { project } => {
                assert!(
                    project.is_none(),
                    "without .rwv-active or --project, location.project is None"
                );
            }
            Checkout::Workweave { .. } => panic!("expected Primary"),
        }
        // The CWD hint must still be populated for diagnostics.
        let hint = ctx.cwd_project_hint().expect("CWD hint should be set");
        assert_eq!(hint.as_str(), "web-app");
    }

    // ========================================================================
    // Resolve from inside a workweave directory without a marker — should fail
    //
    // The legacy marker-less {left}--{name} sibling-resolution fallback has
    // been removed. A `{left}--{name}` directory without a `.rwv-workweave`
    // marker is not recognized as a workweave; resolve() must return an error.
    // ========================================================================

    #[test]
    fn resolve_from_inside_weave_dir_without_marker_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let _root = make_workspace(tmp.path(), "ws");
        // Create a workweave-shaped sibling with no marker.
        let weave_dir = tmp.path().join("ws--hotfix");
        std::fs::create_dir_all(&weave_dir).unwrap();

        // Without a .rwv-workweave marker the directory is not recognized as a
        // workweave; resolution should fail.
        let result = WorkspaceContext::resolve(&weave_dir, None);
        assert!(
            result.is_err(),
            "expected error for marker-less workweave dir, got Ok"
        );
    }

    // ========================================================================
    // Resolve from inside a repo within a marker-less {left}--{name} dir.
    //
    // Without the legacy fallback the {left}--{name} directory is not treated
    // as a workweave. If the directory happens to contain registry subdirs
    // (github/ etc.) it is recognized as a workspace root (Weave) instead —
    // the same behaviour as any other directory that has workspace markers.
    // `rwv doctor` will flag such a directory as an unregistered workweave-
    // shaped directory.
    // ========================================================================

    #[test]
    fn resolve_from_repo_inside_weave_without_marker_resolves_as_weave() {
        let tmp = tempfile::tempdir().unwrap();
        let _root = make_workspace(tmp.path(), "ws");
        let weave_dir = tmp.path().join("ws--feat-login");
        let repo_dir = weave_dir.join("github").join("acme").join("server");
        std::fs::create_dir_all(&repo_dir).unwrap();

        // No marker in ws--feat-login. The walk-up finds `github/` inside
        // ws--feat-login and treats it as a workspace root.
        let ctx = WorkspaceContext::resolve(&repo_dir, None).unwrap();
        match &ctx.checkout {
            Checkout::Primary { .. } => {
                // Correct: treated as an anonymous workspace root, not a workweave.
                assert_eq!(
                    ctx.primary_path(),
                    weave_dir.canonicalize().unwrap(),
                    "should resolve to the marker-less dir as workspace root"
                );
            }
            Checkout::Workweave { .. } => {
                panic!("should NOT be resolved as a workweave without a marker");
            }
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
        match &ctx.checkout {
            Checkout::Primary { project } => {
                let p = project.as_ref().expect("project should be set");
                assert_eq!(p.as_str(), "overridden-project");
            }
            Checkout::Workweave { .. } => panic!("expected Primary"),
        }
    }

    #[test]
    fn resolve_with_project_override_in_workweave() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let weave_dir = tmp.path().join("ws--hotfix");
        std::fs::create_dir_all(&weave_dir).unwrap();

        // Write a marker so the workweave is recognized.
        let primary_canon = root.canonicalize().unwrap();
        let marker = WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("ws"),
            parent: primary_canon,
        };
        marker.write(&weave_dir).unwrap();

        let ctx =
            WorkspaceContext::resolve(&weave_dir, Some(ProjectName::new("custom-proj"))).unwrap();
        match &ctx.checkout {
            Checkout::Workweave { project, .. } => {
                assert_eq!(project.as_str(), "custom-proj");
            }
            Checkout::Primary { .. } => panic!("expected Workweave"),
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
        match &ctx.checkout {
            Checkout::Primary { project } => {
                assert!(project.is_none());
            }
            Checkout::Workweave { .. } => panic!("expected Primary"),
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
        match &ctx.checkout {
            Checkout::Primary { project } => {
                let p = project
                    .as_ref()
                    .expect("project should come from .rwv-active");
                assert_eq!(p.as_str(), "web-app");
            }
            Checkout::Workweave { .. } => panic!("expected Primary"),
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
        match &ctx.checkout {
            Checkout::Primary { project } => {
                let p = project
                    .as_ref()
                    .expect("project should come from .rwv-active");
                assert_eq!(p.as_str(), "from-file");
            }
            Checkout::Workweave { .. } => panic!("expected Primary"),
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
        match &ctx.checkout {
            Checkout::Primary { project } => {
                let p = project.as_ref().expect("project should be set");
                assert_eq!(p.as_str(), "explicit-override");
            }
            Checkout::Workweave { .. } => panic!("expected Primary"),
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
            parent: PathBuf::from("/home/user/weaveroot"),
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
            parent: parent.clone(),
        };
        marker.write(dir).unwrap();

        let read_back = WorkweaveMarker::read(dir).unwrap().unwrap();
        assert_eq!(read_back.parent, parent);
    }

    #[test]
    fn workweave_marker_missing_parent_returns_error() {
        // Legacy marker written before parent tracking: only primary and
        // project fields present on disk. read() must reject the marker with
        // an actionable error — no silent backfill.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let legacy = "primary: /home/user/weaveroot/primary\nproject: legacy-project\n";
        std::fs::write(dir.join(".rwv-workweave"), legacy).unwrap();

        let result = WorkweaveMarker::read(dir);
        assert!(
            result.is_err(),
            "read() should fail for a legacy marker missing parent:"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("legacy workweave marker") || msg.contains("parent"),
            "error should mention the missing parent field: {msg}"
        );
        assert!(
            msg.contains("rwv doctor --fix"),
            "error should mention rwv doctor --fix: {msg}"
        );
    }

    #[test]
    fn workweave_marker_explicit_parent_round_trips() {
        // A marker with an explicit parent (e.g. forked from another
        // workweave) must round-trip intact.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let yaml = "primary: /home/user/primary\n\
                    project: p\n\
                    parent: /home/user/.workweaves/primary--ww1\n";
        std::fs::write(dir.join(".rwv-workweave"), yaml).unwrap();

        let marker = WorkweaveMarker::read(dir).unwrap().unwrap();
        assert_eq!(
            marker.parent,
            PathBuf::from("/home/user/.workweaves/primary--ww1"),
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

        let primary_canon = root.canonicalize().unwrap();
        let marker = WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("web-app"),
            parent: primary_canon.clone(),
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        assert_eq!(ctx.primary_path(), primary_canon);
        match &ctx.checkout {
            Checkout::Workweave { name, dir, project } => {
                assert_eq!(name.as_str(), "feat");
                assert_eq!(*dir, weave_dir.canonicalize().unwrap());
                assert_eq!(project.as_str(), "web-app");
            }
            Checkout::Primary { .. } => panic!("expected Workweave"),
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

        let primary_canon = root.canonicalize().unwrap();
        let marker = WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("web-app"),
            parent: primary_canon,
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&repo_dir, None).unwrap();
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
        match &ctx.checkout {
            Checkout::Workweave { name, dir, project } => {
                assert_eq!(name.as_str(), "feat");
                assert_eq!(*dir, weave_dir.canonicalize().unwrap());
                assert_eq!(project.as_str(), "web-app");
            }
            Checkout::Primary { .. } => panic!("expected Workweave"),
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

        let primary_canon = root.canonicalize().unwrap();
        let marker = WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("marker-project"),
            parent: primary_canon,
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        // Marker takes precedence for project (from marker, not from the
        // directory's left component). Workweave name comes from the
        // right-hand side of `<project>--<name>` so downstream lookups
        // (list, delete) can find the on-disk directory through the
        // primary-side `.rwv-workweave-index` registry.
        match &ctx.checkout {
            Checkout::Workweave { name, project, .. } => {
                assert_eq!(name.as_str(), "dash-name");
                assert_eq!(project.as_str(), "marker-project");
            }
            Checkout::Primary { .. } => panic!("expected Workweave"),
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

    // ========================================================================
    // $HOME ceiling — walk-up never escapes above $HOME
    //
    // fo-eli0oa: when tests run from inside a workweave that is itself under
    // $HOME, workspace resolution must not walk above $HOME to find a workspace
    // in an unrelated branch of the filesystem (e.g., a sibling directory that
    // happens to have `github/` or `projects/` subdirs).
    //
    // We simulate this by building a fake home-like subtree: a "home" dir
    // containing a "real" workspace with workspace markers. Resolving from a
    // deep path inside that "home" dir should find the "real" workspace, and
    // (with the ceiling) should NOT escape to any workspace we place ABOVE the
    // fake home dir.
    //
    // Note: this test creates the workspace hierarchy in a real temp dir so
    // that `dirs::home_dir()` (which returns the REAL home dir) does not affect
    // the directory layout. The boundary is exercised with the real home dir
    // value; the test relies on the fact that the temp dir is NOT under $HOME
    // (i.e., it's in /tmp), so the ceiling does not fire for paths under /tmp,
    // and the test is structurally about "workspace under $HOME is found even
    // with the ceiling active."
    // ========================================================================

    /// The $HOME ceiling must not block resolution of workspaces that are
    /// legitimately inside $HOME.
    #[test]
    fn resolve_ceiling_does_not_block_workspace_inside_home() {
        // tempfile creates dirs under $TMPDIR or /tmp, which is NOT under
        // $HOME. This test verifies that the ceiling does not affect paths
        // that are already outside $HOME (they walk normally to the root).
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        // Walk from deep inside the workspace — ceiling should not interfere
        // (we're in /tmp, not inside $HOME).
        let deep = root.join("github").join("acme").join("deep");
        std::fs::create_dir_all(&deep).unwrap();
        let ctx = WorkspaceContext::resolve(&deep, None).unwrap();
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
    }

    /// The $HOME ceiling fires when the walk-up tries to go above $HOME.
    ///
    /// We build a workspace INSIDE the real $HOME (using a temp dir created
    /// under $HOME). The walk-up from inside that workspace should still find
    /// the workspace because the ceiling only fires when the walk would cross
    /// ABOVE $HOME — not below it.
    #[test]
    fn resolve_ceiling_workspace_inside_home_is_found() {
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => {
                // If we can't determine $HOME, skip.
                return;
            }
        };

        // Create a temp dir inside $HOME so we can test the ceiling behavior
        // without escaping $HOME.
        let tmp_under_home = match tempfile::TempDir::new_in(&home) {
            Ok(t) => t,
            Err(_) => {
                // If we can't create a temp dir inside $HOME (e.g., permissions),
                // skip rather than fail.
                return;
            }
        };

        let root = make_workspace(tmp_under_home.path(), "ws");
        let deep = root.join("github").join("acme").join("repo");
        std::fs::create_dir_all(&deep).unwrap();

        // Should find the workspace even with the $HOME ceiling active.
        let ctx = WorkspaceContext::resolve(&deep, None).unwrap();
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
    }

    // ========================================================================
    // $HOME ceiling — symlinked home path
    //
    // fo-wbbqof.3: `resolve()` canonicalizes `cwd` but the original code
    // bound `home_dir` as the raw (un-canonicalized) value from
    // `dirs::home_dir()`.  On systems where $HOME contains a symlinked
    // component the `starts_with` comparison always returns false (the
    // spellings differ) and the ceiling silently never fires.
    //
    // We test the pure helper `home_ceiling_blocks` directly with a
    // symlinked-home layout so the test is independent of the process-level
    // $HOME value and safe under parallel test execution.
    // ========================================================================

    /// `home_ceiling_blocks` must return `true` when `current` is inside the
    /// REAL (canonicalized) home but `parent` is not — even when the caller
    /// passes in the symlink spelling of home (pre-fix behaviour would have
    /// returned `false` and let the walk escape).
    ///
    /// Layout built in /tmp:
    ///   /tmp/rwv-test-XXXX/
    ///     real_home/          ← the actual directory
    ///     link_home -> real_home   ← symlink spelling of home
    ///     above/              ← directory that lives *above* home
    ///
    /// We set `current = link_home/subdir` and `parent = above`.
    /// With the symlink spelling as `home`, `current.starts_with(home)` is
    /// false (path prefix mismatch) so the pre-fix code would return `false`.
    /// After the fix we canonicalize home before passing it in, so the helper
    /// gets the real path and correctly returns `true`.
    #[test]
    fn home_ceiling_blocks_symlinked_home() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        // 1. Create the real home directory and a symlinked alias.
        let real_home = base.join("real_home");
        std::fs::create_dir_all(&real_home).unwrap();
        let link_home = base.join("link_home");
        std::os::unix::fs::symlink(&real_home, &link_home).unwrap();

        // 2. Create a directory inside the real home (reached via symlink).
        let subdir = link_home.join("subdir");
        std::fs::create_dir_all(&subdir).unwrap();

        // 3. Create a directory that sits *above* home (sibling of real_home).
        let above = base.join("above");
        std::fs::create_dir_all(&above).unwrap();

        // Canonicalize paths as `resolve()` does for `current` and `parent`.
        let current_canon = subdir.canonicalize().unwrap();
        let parent_canon = above.canonicalize().unwrap();

        // Pre-fix: passing the raw symlink spelling → ceiling silently no-ops.
        assert!(
            !home_ceiling_blocks(&current_canon, &parent_canon, Some(&link_home)),
            "raw symlink path does NOT match canonicalized current — ceiling is blind (this is the bug)"
        );

        // Post-fix: passing the canonicalized home → ceiling fires correctly.
        let canon_home = link_home.canonicalize().unwrap();
        assert!(
            home_ceiling_blocks(&current_canon, &parent_canon, Some(&canon_home)),
            "canonicalized home must make the ceiling fire and block the walk"
        );
    }

    /// `home_ceiling_blocks` must NOT fire when both `current` and `parent`
    /// are inside the (canonicalized) home — the walk stays within home and
    /// should continue.
    #[test]
    fn home_ceiling_blocks_does_not_fire_within_home() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        let real_home = base.join("real_home");
        std::fs::create_dir_all(real_home.join("deep").join("inner")).unwrap();

        let current = real_home.join("deep").join("inner").canonicalize().unwrap();
        let parent = real_home.join("deep").canonicalize().unwrap();
        let canon_home = real_home.canonicalize().unwrap();

        assert!(
            !home_ceiling_blocks(&current, &parent, Some(&canon_home)),
            "ceiling must NOT fire when both current and parent are inside home"
        );
    }

    /// Integration smoke test: `resolve()` from a symlink-spelled cwd must
    /// canonicalize and resolve to the workspace inside the real home.
    ///
    /// Layout:
    ///   /tmp/rwv-test-XXXX/
    ///     real_home/
    ///       ws/               ← workspace root (has github/ + projects/)
    ///         github/acme/repo/   ← cwd (reached via the symlink alias)
    ///     link_home -> real_home
    ///     decoy/              ← workspace root ABOVE home
    ///
    /// Note: this does NOT exercise the $HOME ceiling. `ws` is the nearest
    /// workspace root, so it is found before the walk could ever reach the
    /// decoy — and `resolve()` reads the real process `$HOME`, not this
    /// tempdir, so the ceiling never applies here. The ceiling itself is
    /// covered directly by `home_ceiling_blocks_symlinked_home`. What this
    /// verifies: a symlink-spelled cwd resolves to the canonical `real_home/ws`.
    #[test]
    fn resolve_symlinked_cwd_resolves_to_canonical_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();

        // Real home directory.
        let real_home = base.join("real_home");
        std::fs::create_dir_all(&real_home).unwrap();

        // Symlinked alias of home.
        let link_home = base.join("link_home");
        std::os::unix::fs::symlink(&real_home, &link_home).unwrap();

        // Workspace inside home (accessible via symlink path).
        let ws_via_link = link_home.join("ws");
        let cwd_via_link = ws_via_link.join("github").join("acme").join("repo");
        std::fs::create_dir_all(&cwd_via_link).unwrap();
        // Create workspace markers via the real path so make_workspace works.
        let ws_real = real_home.join("ws");
        std::fs::create_dir_all(ws_real.join("github")).unwrap();
        std::fs::create_dir_all(ws_real.join("projects")).unwrap();

        // Decoy workspace above home — must NOT be found.
        let decoy = make_workspace(base, "decoy");

        // Resolve from the symlink-spelled cwd.
        let ctx = WorkspaceContext::resolve(&cwd_via_link, None).unwrap();

        // Must find the workspace inside home, not the decoy above.
        let found = ctx.primary_path();
        assert_ne!(
            found,
            decoy.canonicalize().unwrap(),
            "nearest workspace root (ws) must win; the decoy above home is never reached"
        );
        assert_eq!(
            found,
            ws_real.canonicalize().unwrap(),
            "must find the workspace inside the (symlinked) home"
        );
    }
}
