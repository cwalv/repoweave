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

use crate::integration_runner::IntegrationContextBase;
use crate::manifest::{Manifest, ProjectName, RepoPath, WorkweaveName};
use crate::registry::{builtin_registries, builtin_registry_names, Registry};
use crate::vcs::Vcs;
use anyhow::Context;
use schemars::JsonSchema;
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
    /// Which chain step chose the active project, when one was chosen.
    ///
    /// The resolution chain is
    /// `--project > -w prefix > (.rwv-active | .rwv-workweave)`; each step
    /// maps to one [`ProjectProvenance`] variant, with the last tier's two
    /// files mapping to `ActiveFile` and `Marker` respectively. `None` when
    /// no project was resolved (bare primary with neither `--project` nor
    /// `.rwv-active` set).
    ///
    /// Used solely for the human-facing "target:" line printed to stderr
    /// when the resolution fell through to `.rwv-active` — the incident
    /// class this field exists to prevent (silent pointer-driven
    /// mis-targeting) surfaces only when the pointer decides. Provenance is
    /// deliberately excluded from machine output (`--json`): anything in
    /// default JSON becomes depended on, and the assertion use case needs
    /// the *result*, not the mechanism.
    project_provenance: Option<ProjectProvenance>,
}

/// Which step of the project resolution chain chose the active project.
///
/// The chain, in priority order:
/// 1. `--project <name>` flag on the invocation → [`ProjectProvenance::Flag`].
/// 2. `-w/--workweave <project>--<name>` global flag → [`ProjectProvenance::WorkweaveFlag`]
///    (reserved slot; the flag lands with a later change and this variant is
///    unconstructed until then).
/// 3. **The weave root's own identity file** — one tier, two spellings,
///    selected by which kind of root resolution landed on:
///    - `.rwv-workweave` marker in a workweave root →
///      [`ProjectProvenance::Marker`]. Structural: the workweave directory
///      itself names its project.
///    - `.rwv-active` pointer at a primary root →
///      [`ProjectProvenance::ActiveFile`]. Ambient default — the case whose
///      silence caused the incident this design fixes.
///
/// The two files are **mutually exclusive**, so this is one tier rather than
/// two ranked ones: no root can offer both answers, and there is no
/// precedence between them to get wrong. `rwv doctor` enforces the
/// exclusivity ([`CheckViolation::WeaveRootIdentityConflict`]).
///
/// Used by the resolver to distinguish structurally-determined targets
/// (steps 1–2 and the marker spelling of step 3, silent) from the
/// pointer-default (the `.rwv-active` spelling, printed as a "target:" line
/// to stderr before the verb acts).
///
/// [`CheckViolation::WeaveRootIdentityConflict`]: crate::check::CheckViolation::WeaveRootIdentityConflict
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectProvenance {
    /// The project came from an explicit `--project` flag.
    Flag,
    /// The project came from the `<project>--` prefix of a `-w` argument.
    ///
    /// The `-w/--workweave` global flag takes a `<project>--<name>` argument;
    /// the project is inferred from the `<project>--` prefix. This provenance
    /// sits between `Flag` (explicit `--project`) and `Marker` (workweave
    /// marker file) in the resolution chain.
    WorkweaveFlag,
    /// The project came from the `.rwv-workweave` marker inside the
    /// resolved workweave directory.
    Marker,
    /// The project came from the `.rwv-active` pointer at the primary root.
    ///
    /// Fall-through resolution; the "target:" line prints for this
    /// provenance only, before the verb acts.
    ActiveFile,
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
    let mut names: Vec<String> = builtin_registry_names()
        .iter()
        .map(|n| n.as_str().to_owned())
        .collect();
    names.push("projects".to_string());
    names
}

/// Where a manifest member's checkout is for this run.
///
/// A workweave holds worktrees only for the members materialized in it, so
/// the answer is per member, not per invocation: the workweave's slot when
/// it exists on disk, primary's canonical clone otherwise.
pub fn member_checkout_dir(
    repo_path: &RepoPath,
    primary_root: &Path,
    workweave_dir: Option<&Path>,
) -> PathBuf {
    if let Some(wd) = workweave_dir {
        let candidate = wd.join(repo_path.as_path());
        if candidate.exists() {
            return candidate;
        }
    }
    primary_root.join(repo_path.as_path())
}

/// Returns true if `dir` looks like a workspace root (contains projects/ or
/// a registry directory).
pub fn is_workspace_root(dir: &Path) -> bool {
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
/// This is a *soft hint* only: action verbs must not use it to override
/// `.rwv-active`. It feeds [`WorkspaceContext::cwd_project_hint`], which is
/// read only to warn about — never to act on — a CWD ≠ active-project
/// divergence.
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
    ProjectName::new(project_name.as_os_str().to_string_lossy().to_string()).ok()
}

// ---------------------------------------------------------------------------
// Weave-root identity: `.rwv-active` XOR `.rwv-workweave`
// ---------------------------------------------------------------------------

/// The pointer file naming the project a **primary** root presents.
pub const ACTIVE_PROJECT_FILE: &str = ".rwv-active";

/// The marker file naming the project a **workweave** root belongs to.
pub const WORKWEAVE_MARKER_FILE: &str = ".rwv-workweave";

/// Whether `root` is a workweave root — i.e. carries the marker file.
///
/// A raw existence test, not a parse: a legacy or hand-mangled marker still
/// makes the directory a workweave root for the purpose of the exclusivity
/// rule below. Reading the marker's *contents* is [`WorkweaveMarker::read`],
/// which refuses a legacy shape.
pub fn is_workweave_root(root: &Path) -> bool {
    root.join(WORKWEAVE_MARKER_FILE).exists()
}

/// Read the active project from the `.rwv-active` file in the workspace root.
///
/// Returns `None` if the file does not exist or is empty.
///
/// This reads the **pointer specifically**, not "the project this root
/// presents" — see [`read_weave_root_project`] for that. Reach for this one
/// only where the pointer file itself is the subject (doctor's stale-pointer
/// check, `deactivate`'s removal).
pub fn read_active_project(root: &Path) -> Option<ProjectName> {
    let path = root.join(ACTIVE_PROJECT_FILE);
    let content = std::fs::read_to_string(&path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    ProjectName::new(trimmed).ok()
}

/// The project a weave root presents, read from whichever file names it.
///
/// `.rwv-active` and `.rwv-workweave` are **mutually exclusive** and occupy
/// one tier of the resolution chain, not two: a primary root carries the
/// pointer, a workweave root carries the marker, never both. This function is
/// that tier — ask it "which project does this directory present?" and it
/// consults the one file that can answer for this kind of root.
///
/// The marker is checked first so a tree that has somehow acquired both
/// answers with the structural fact rather than the ambient one; that
/// ordering is a tiebreak for a state `rwv doctor` reports as
/// [`CheckViolation::WeaveRootIdentityConflict`], not a precedence the design
/// relies on.
///
/// A legacy marker (missing `parent:`) still names its project, and this
/// function returns it: the caller is asking which project the root presents,
/// which the legacy shape answers perfectly well. `WorkweaveMarker::read`'s
/// refusal exists to stop *parent-chain* consumers, and re-refusing here would
/// make surfacing collapse on a workweave that `rwv doctor --fix` can migrate.
///
/// [`CheckViolation::WeaveRootIdentityConflict`]: crate::check::CheckViolation::WeaveRootIdentityConflict
pub fn read_weave_root_project(root: &Path) -> Option<ProjectName> {
    let marker_path = root.join(WORKWEAVE_MARKER_FILE);
    if marker_path.exists() {
        let content = std::fs::read_to_string(&marker_path).ok()?;
        let raw: serde_yaml::Value = serde_yaml::from_str(&content).ok()?;
        let project = raw.get("project")?.as_str()?.trim();
        if project.is_empty() {
            return None;
        }
        return ProjectName::new(project).ok();
    }
    read_active_project(root)
}

/// Write the active project to the `.rwv-active` file in the workspace root.
///
/// **`root` must be a primary root.** The pointer is project *selection*, and
/// selection is primary-only: a workweave's project is fixed at creation by
/// its `.rwv-workweave` marker and cannot be switched. Writing the pointer
/// into a workweave root would put a second, unread copy of the workweave's
/// own identity beside the marker — the state `rwv doctor` reports as
/// [`CheckViolation::WeaveRootIdentityConflict`]. Callers establish the
/// precondition before calling; [`is_workweave_root`] is the test.
///
/// [`CheckViolation::WeaveRootIdentityConflict`]: crate::check::CheckViolation::WeaveRootIdentityConflict
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
/// cost a single time, then pass the varying `project` to
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
        let vcs = crate::vcs::probe_vcs();
        let repos_on_disk = scan_repos_on_disk(root, &registries, vcs.as_ref());
        let project_paths = discover_project_paths(root);
        Self {
            root: root.to_path_buf(),
            repos_on_disk,
            project_paths,
        }
    }

    /// Build an [`IntegrationContextBase`] from this session's shared data
    /// combined with the per-invocation `project` and that project's optional
    /// `workweave:` config.
    ///
    /// `output_dir` is **derived**, not passed: it is always
    /// `<self.root>/projects/<project>`, the committed location where an
    /// integration's managed and generated files actually live. It is not a
    /// caller's choice, because the weave root is the other thing a caller
    /// could plausibly hand over — and there the same files appear only as
    /// surfacing symlinks, for the **active** project alone. A context bound
    /// to the root view reads the active project's inode no matter which
    /// project it claims to describe, and names a path that does not exist at
    /// all for a file that is missing. Deriving the directory here is what
    /// makes that unrepresentable; whether the root carries the symlink is a
    /// separate axis, answered by [`crate::activate::verify_surfacing`].
    ///
    /// The `workweave` argument is the project's `workweave:` section from
    /// `rwv.yaml` (typically `manifest.workweave.as_ref()`). It is threaded
    /// through to integrations so they can detect cross-section collisions
    /// such as a name claimed by both `static-files.files` and
    /// `workweave.link`.
    pub fn context_base<'a>(
        &'a self,
        project: &'a ProjectName,
        detection_cache: &'a std::collections::HashMap<String, Vec<String>>,
        workweave: Option<&'a crate::manifest::WorkweaveConfig>,
    ) -> IntegrationContextBase<'a> {
        IntegrationContextBase {
            output_dir: self.root.join("projects").join(project.as_str()),
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
///   the caller to use `--allow-non-empty-dir`.
///
/// Pass `allow_non_empty_dir = true` to skip the non-empty check entirely.
///
/// Callers that expose a different interface (e.g. `init`, which has no
/// such flag) should map the returned error to a command-specific
/// message rather than surfacing the raw `--allow-non-empty-dir` hint.
pub fn require_workspace_or_empty(cwd: &Path, allow_non_empty_dir: bool) -> anyhow::Result<()> {
    match WorkspaceContext::resolve(cwd, None) {
        Ok(_) => return Ok(()), // existing workspace — proceed
        Err(_) if allow_non_empty_dir => return Ok(()),
        Err(_) => {}
    }

    // resolve failed and the check was not waived — check whether CWD is empty.
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
             use --allow-non-empty-dir to bootstrap here anyway",
            cwd.display()
        )
    }
}

impl WorkspaceContext {
    /// Resolve the workspace context by walking up from `cwd`.
    ///
    /// Project resolution chain (highest priority first):
    ///   1. `project_override` — explicit `--project <name>` flag.
    ///      Provenance = [`ProjectProvenance::Flag`].
    ///   2. `-w/--workweave` prefix — handled in `cli::dispatch` by looking
    ///      up the workweave path and re-resolving from it; the dispatch path
    ///      then calls [`WorkspaceContext::with_workweave_flag_provenance`] to
    ///      correct `Marker` → `WorkweaveFlag`.
    ///      Provenance = [`ProjectProvenance::WorkweaveFlag`].
    ///   3. The weave root's own identity file — **one tier**, whose
    ///      spelling follows the kind of root the walk landed on:
    ///      - workweave root → `.rwv-workweave` marker, structural: the
    ///        workweave directory names its project.
    ///        Provenance = [`ProjectProvenance::Marker`].
    ///      - primary root → `.rwv-active` pointer, the ambient default.
    ///        Provenance = [`ProjectProvenance::ActiveFile`].
    ///
    /// Step 3 is one tier and not two because the two files are mutually
    /// exclusive: the loop below returns from whichever arm matches, so no
    /// invocation ever consults both, and there is no precedence between them
    /// to document or to get wrong. `rwv doctor` enforces the exclusivity
    /// (`weave-root-identity-conflict`).
    ///
    /// The chosen chain step is recorded on the returned context as
    /// [`WorkspaceContext::project_provenance`] so downstream code can
    /// distinguish structurally-determined targets (steps 1–2 and step 3's
    /// marker spelling, silent) from the pointer-default (step 3's
    /// `.rwv-active` spelling, which callers surface as a "target:" line via
    /// [`WorkspaceContext::emit_target_line`]).
    ///
    /// The "CWD is inside `projects/<X>/`" inference is still computed
    /// and recorded on the context as [`WorkspaceContext::cwd_project_hint`]
    /// so that diagnostics and `rwv` bare status can surface a divergence
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
                    let workweave_name = match parse_weave_dir_name(dir_basename) {
                        Some((_, n)) => n,
                        None => WorkweaveName::new(dir_basename)?,
                    };
                    // Provenance: `--project` wins if set, else the marker
                    // determines the project (structural — a workweave root
                    // carries no `.rwv-active`, so there is no ambient
                    // pointer here to consult or to rank against).
                    let (project, provenance) = match project_override {
                        Some(p) => (p, ProjectProvenance::Flag),
                        None => (marker.project, ProjectProvenance::Marker),
                    };
                    let cwd_project_hint = detect_project(&cwd, &root);
                    return Ok(WorkspaceContext {
                        primary_root: root,
                        checkout: Checkout::Workweave {
                            name: workweave_name,
                            dir: current.to_path_buf(),
                            project,
                        },
                        cwd_project_hint,
                        project_provenance: Some(provenance),
                    });
                }
            }

            // 2. Check if current directory IS the workspace root.
            if is_workspace_root(current) {
                let cwd_project_hint = detect_project(&cwd, current);
                // Provenance: `--project` wins; otherwise the `.rwv-active`
                // pointer decides (if present). No pointer + no override
                // leaves `project` and provenance unset — the caller uses
                // `require_active_project` to surface the corrective error.
                let (project, provenance) = match project_override {
                    Some(p) => (Some(p), Some(ProjectProvenance::Flag)),
                    None => match read_active_project(current) {
                        Some(p) => (Some(p), Some(ProjectProvenance::ActiveFile)),
                        None => (None, None),
                    },
                };
                return Ok(WorkspaceContext {
                    primary_root: current.to_path_buf(),
                    checkout: Checkout::Primary { project },
                    cwd_project_hint,
                    project_provenance: provenance,
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

    /// The chain step that chose the active project, if one was chosen.
    ///
    /// See [`ProjectProvenance`] for the chain-step vocabulary. `None` is
    /// isomorphic to [`active_project`] returning `None` — no chain step
    /// fires when no project is resolved.
    ///
    /// [`active_project`]: WorkspaceContext::active_project
    pub fn project_provenance(&self) -> Option<ProjectProvenance> {
        self.project_provenance
    }

    /// Re-mark the project provenance as [`ProjectProvenance::WorkweaveFlag`].
    ///
    /// Called by the `-w/--workweave` dispatch path after resolving a context
    /// from a registry-looked-up workweave directory: the context's internal
    /// provenance reflects the containment walk (which sees the
    /// `.rwv-workweave` marker → `Marker`), but the chain step that actually
    /// decided is the `-w` flag. Correcting the provenance here ensures that
    /// `emit_target_line` stays silent (as intended for all explicit addressing
    /// forms) without requiring changes to the resolver's marker-walk logic.
    ///
    /// Only applies when the current provenance is `Marker` — if `--project`
    /// is also present it sets `Flag` provenance, which outranks `-w` in the
    /// chain and must not be overwritten.
    pub fn with_workweave_flag_provenance(mut self) -> Self {
        if self.project_provenance == Some(ProjectProvenance::Marker) {
            self.project_provenance = Some(ProjectProvenance::WorkweaveFlag);
        }
        self
    }

    /// Print the "target:" line to stderr when the active project was
    /// chosen by the `.rwv-active` pointer fall-through.
    ///
    /// The line format is:
    ///
    /// ```text
    /// target: workspace <primary-path> · project <name> (.rwv-active)
    /// ```
    ///
    /// Silent for every other provenance — explicitly (`--project`) or
    /// structurally (workweave marker) resolved invocations already name
    /// their target and gain nothing from the surfacing.
    ///
    /// Written to stderr because it is operator-facing prose and must not
    /// contaminate stdout, which every JSON-capable verb owns exclusively.
    ///
    /// Idempotent — call once per project-scoped verb, at the top of
    /// dispatch before the verb acts. Callers that already know they need
    /// no active project (workspace-scoped verbs: `init`, `abort`,
    /// `resolve`, `prime`, `explain`, cross-project doctor scan) skip it.
    pub fn emit_target_line(&self) {
        if self.project_provenance != Some(ProjectProvenance::ActiveFile) {
            return;
        }
        // Under ActiveFile provenance the chain step guarantees the
        // project is set; the None branch is unreachable but handled
        // defensively so a future refactor that decouples the two cannot
        // panic here.
        if let Some(project) = self.active_project() {
            eprintln!(
                "target: workspace {} · project {} (.rwv-active)",
                self.primary_root.display(),
                project.as_str(),
            );
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

        // No active project. Build a corrective error naming the exact
        // commands the operator can run — the pointer is total by
        // construction (init/fetch/workweave-create all activate on
        // create), so reaching this branch means the pointer was
        // hand-removed. Naming existing projects (when any exist) turns
        // the error into a menu the operator can act on without a
        // separate discovery step.
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
        let existing = discover_project_paths(self.primary_path());
        if existing.is_empty() {
            anyhow::bail!(
                "no active project; run `rwv activate <name>` or pass `--project <name>` \
                 (no projects exist under this workspace yet — `rwv init <name>` to create one)"
            );
        }
        anyhow::bail!(
            "no active project; run `rwv activate <name>` or pass `--project <name>`. \
             Existing projects: {}",
            existing.join(", "),
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
                        .join(Manifest::FILE_NAME);
                    if let Ok(manifest) = Manifest::from_path(&manifest_path) {
                        lines.push(format!("Repos: {}", manifest.len()));
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
                        .join(Manifest::FILE_NAME);
                    if let Ok(manifest) = Manifest::from_path(&manifest_path) {
                        lines.push(format!("Repos: {}", manifest.len()));
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

    /// Project the resolved context into the machine-readable `resolution` block.
    ///
    /// Returns `None` when no project is resolved (bare primary with neither
    /// `--project` nor `.rwv-active` set) — in that case the block is omitted
    /// from `--json` output entirely.
    ///
    /// The returned value is a pure projection: `workspace` is the primary root
    /// (abs path), `workweave` is the full `<project>--<name>` identity when in
    /// a workweave (absent at primary — presence IS the checkout kind), and
    /// `project` is the resolved project name. Results only; resolution
    /// provenance (which chain step chose the project) is human-surface only
    /// (stderr target line) and is deliberately excluded from this struct.
    ///
    /// A future change will emit an env-var envelope
    /// (`RWV_WORKSPACE`/`RWV_WORKWEAVE`/`RWV_PROJECT`) as a second
    /// serialization of this same projection. That path will call this method
    /// and map fields to env vars; the values must never be independently
    /// computed.
    pub fn resolution(&self) -> Option<Resolution> {
        let project = self.active_project()?;
        let workspace = self.primary_root.clone();
        let workweave = match &self.checkout {
            Checkout::Primary { .. } => None,
            Checkout::Workweave { name, project, .. } => {
                Some(weave_dir_name(project.as_str(), name))
            }
        };
        Some(Resolution {
            workspace,
            workweave,
            project: project.as_str().to_owned(),
        })
    }
}

/// Resolved workspace coordinates for `--json` output and (future) plugin
/// env-var envelope.
///
/// Carries exactly the three result fields — `workspace` (primary root abs
/// path), `workweave` (`<project>--<name>` identity when in a workweave,
/// absent at primary), and `project` (resolved project name). Presence of
/// `workweave` encodes the checkout kind; no separate `kind` or `location`
/// field is needed.
///
/// Results only — provenance (which chain step resolved the project, which
/// flag addressed the workspace) is deliberately excluded: anything in
/// default `--json` output becomes depended on, and the assertion use case
/// needs the result, not the mechanism. Provenance appears only in the
/// human-facing "target:" line printed to stderr.
///
/// Isomorphic to the plugin env-var envelope
/// (`RWV_WORKSPACE`/`RWV_WORKWEAVE`/`RWV_PROJECT`): both surfaces are pure
/// projections of [`WorkspaceContext::resolution`], never independently
/// computed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Resolution {
    /// Primary workspace root (absolute path).
    pub workspace: PathBuf,
    /// Workweave identity (`<project>--<name>`).
    ///
    /// Present when the invocation resolved into a workweave; absent at the
    /// primary. Presence encodes the checkout kind — no separate `kind` field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workweave: Option<String>,
    /// Resolved project name.
    pub project: String,
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
    Some((left, WorkweaveName::new(workweave).ok()?))
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
        let path = dir.join(WORKWEAVE_MARKER_FILE);
        match observe_marker(&path) {
            MarkerPresence::Absent => Ok(None),
            MarkerPresence::Usable(marker) => Ok(Some(marker)),
            MarkerPresence::Defective { defect, .. } => Err(anyhow::anyhow!(defect.refusal(&path))),
        }
    }

    pub fn write(&self, dir: &Path) -> anyhow::Result<()> {
        let path = dir.join(".rwv-workweave");
        let content = serde_yaml::to_string(self).context("failed to serialize .rwv-workweave")?;
        std::fs::write(&path, content)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    /// Migrate a legacy marker in `dir` (missing the `parent:` field checked
    /// by [`Self::read`]) by backfilling `parent` to `primary` and rewriting
    /// through [`Self::write`].
    ///
    /// Returns `Ok(false)` if `parent:` is already present and non-null —
    /// idempotent, so callers can retry across a race without double-writing.
    /// `Err` on I/O failure or if the file doesn't even have the `primary:`
    /// field a legacy marker requires.
    pub fn migrate_legacy(dir: &Path) -> anyhow::Result<bool> {
        let path = dir.join(".rwv-workweave");
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {} for --fix", path.display()))?;
        let mut raw: serde_yaml::Value = serde_yaml::from_str(&content)
            .with_context(|| format!("failed to parse .rwv-workweave at {}", path.display()))?;
        if !raw.get("parent").map(|v| v.is_null()).unwrap_or(true) {
            return Ok(false);
        }
        let primary = raw.get("primary").cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "{} is missing the required `primary:` field",
                path.display()
            )
        })?;
        raw.as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("{} is not a YAML mapping", path.display()))?
            .insert(serde_yaml::Value::String("parent".into()), primary);
        let marker: Self = serde_yaml::from_value(raw)
            .with_context(|| format!("failed to parse .rwv-workweave at {}", path.display()))?;
        marker.write(dir)?;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Root identity — what one directory claims to be
// ---------------------------------------------------------------------------

/// Why a `.rwv-workweave` file cannot witness the identity it claims.
///
/// `Legacy` is the marker shape written before `parent:` became required;
/// [`WorkweaveMarker::migrate_legacy`] is what `rwv doctor --fix` runs on it.
#[derive(Debug)]
pub enum MarkerDefect {
    DanglingPrimary { primary: PathBuf },
    Legacy,
    Unreadable { detail: String },
}

impl MarkerDefect {
    /// The operator-facing refusal for a marker carrying this defect: what is
    /// wrong, the value that is wrong, and every exit that does not require
    /// the reader to guess an identity on the tree's behalf.
    pub fn refusal(&self, marker_path: &Path) -> String {
        match self {
            MarkerDefect::DanglingPrimary { primary } => format!(
                "{} names primary: {}, which is not a repoweave workspace root. \
                 A copied or rsync'd workweave tree is the usual cause. Repair \
                 `primary:` to name the workspace this tree belongs to, or delete \
                 {} to adopt this tree as a standalone weave.",
                marker_path.display(),
                primary.display(),
                marker_path.display()
            ),
            MarkerDefect::Legacy => format!(
                "{} is a legacy workweave marker missing the required `parent:` field. \
                 Run `rwv doctor --fix` to migrate it before using this workweave.",
                marker_path.display()
            ),
            MarkerDefect::Unreadable { detail } => format!(
                "{detail}. Repair {}, or delete it to adopt this tree as a \
                 standalone weave.",
                marker_path.display()
            ),
        }
    }
}

enum MarkerPresence {
    Absent,
    Usable(WorkweaveMarker),
    Defective {
        defect: MarkerDefect,
        project_hint: Option<ProjectName>,
    },
}

/// Parse `.rwv-workweave` once, for both the readers that need a marker and
/// the readers that must classify a broken one.
///
/// `project_hint` is carried out of the defective arms because a root whose
/// marker no verb may act on still presents a project to surfacing, and this
/// is the last point where anything can read it.
fn observe_marker(marker_path: &Path) -> MarkerPresence {
    if !marker_path.exists() {
        return MarkerPresence::Absent;
    }
    let unreadable =
        |detail: String, project_hint: Option<ProjectName>| MarkerPresence::Defective {
            defect: MarkerDefect::Unreadable { detail },
            project_hint,
        };
    let content = match std::fs::read_to_string(marker_path) {
        Ok(content) => content,
        Err(e) => {
            return unreadable(
                format!("failed to read {}: {e}", marker_path.display()),
                None,
            );
        }
    };
    let raw: serde_yaml::Value = match serde_yaml::from_str(&content) {
        Ok(raw) => raw,
        Err(e) => {
            return unreadable(
                format!(
                    "failed to parse .rwv-workweave at {}: {e}",
                    marker_path.display()
                ),
                None,
            );
        }
    };
    let project_hint = raw
        .get("project")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|project| !project.is_empty())
        .and_then(|project| ProjectName::new(project).ok());
    if raw.get("parent").map(|v| v.is_null()).unwrap_or(true) {
        return MarkerPresence::Defective {
            defect: MarkerDefect::Legacy,
            project_hint,
        };
    }
    match serde_yaml::from_value(raw) {
        Ok(marker) => MarkerPresence::Usable(marker),
        Err(e) => unreadable(
            format!(
                "failed to parse .rwv-workweave at {}: {e}",
                marker_path.display()
            ),
            project_hint,
        ),
    }
}

/// The identity evidence one directory carries.
///
/// The arms are the states a directory can be in, not the states a verb is
/// willing to act on: [`RootObservation::require_exclusive`] is where the
/// unusable ones become refusals.
#[derive(Debug)]
pub enum RootObservation {
    /// The marker alone, and its `primary:` verifies as a workspace root.
    Workweave { marker: WorkweaveMarker },
    /// Workspace-shaped, no marker. `selection` is the `.rwv-active` pointer,
    /// which a primary root may legitimately be without.
    Primary { selection: Option<ProjectName> },
    /// Both files present, marker verified. The pointer is duplicate identity
    /// no writer produces — a hygiene violation, not an ambiguity, since the
    /// marker remains the sole discriminator between the two kinds of root.
    Disputed {
        root: PathBuf,
        marker: WorkweaveMarker,
        pointer: Option<ProjectName>,
    },
    /// A marker that cannot witness what it claims. A containment walk must
    /// stop here rather than reading the tree's structural shape instead: the
    /// shape of every workweave is the shape of a primary, so falling through
    /// hands the tree a primary's authority on the strength of its own broken
    /// claim to be something else.
    MarkerUnverifiable {
        marker_path: PathBuf,
        defect: MarkerDefect,
        project_hint: Option<ProjectName>,
    },
}

/// Observe what identity evidence `dir` carries.
///
/// `Ok(None)` is "not a root of either kind" — a containment walk continues
/// past it. Every other answer is terminal for the walk, including the two
/// that no verb may act on.
pub fn observe_root(dir: &Path) -> anyhow::Result<Option<RootObservation>> {
    let marker_path = dir.join(WORKWEAVE_MARKER_FILE);
    let observation = match observe_marker(&marker_path) {
        MarkerPresence::Defective {
            defect,
            project_hint,
        } => RootObservation::MarkerUnverifiable {
            marker_path,
            defect,
            project_hint,
        },
        MarkerPresence::Usable(marker) => {
            if !is_workspace_root(&marker.primary) {
                RootObservation::MarkerUnverifiable {
                    marker_path,
                    project_hint: Some(marker.project),
                    defect: MarkerDefect::DanglingPrimary {
                        primary: marker.primary,
                    },
                }
            } else if dir.join(ACTIVE_PROJECT_FILE).exists() {
                RootObservation::Disputed {
                    root: dir.to_path_buf(),
                    pointer: read_active_project(dir),
                    marker,
                }
            } else {
                RootObservation::Workweave { marker }
            }
        }
        MarkerPresence::Absent => {
            if !is_workspace_root(dir) {
                return Ok(None);
            }
            RootObservation::Primary {
                selection: read_active_project(dir),
            }
        }
    };
    Ok(Some(observation))
}

impl RootObservation {
    /// The identity a verb may act on, or the refusal that names the repair.
    pub fn require_exclusive(self) -> anyhow::Result<WeaveRootIdentity> {
        match self {
            RootObservation::Workweave { marker } => {
                Ok(WeaveRootIdentity::Workweave(WorkweaveIdentity { marker }))
            }
            RootObservation::Primary { selection } => {
                Ok(WeaveRootIdentity::Primary(PrimaryIdentity { selection }))
            }
            RootObservation::Disputed { root, .. } => anyhow::bail!(
                "{} and {} both exist: a weave root carries the workweave marker \
                 or the active-project pointer, never both. Run `rwv doctor --fix`; \
                 it removes the redundant file where the primary-side workweave \
                 registry names this tree, and reports the conflict where nothing \
                 outside the tree settles which file is the stray.",
                root.join(WORKWEAVE_MARKER_FILE).display(),
                root.join(ACTIVE_PROJECT_FILE).display()
            ),
            RootObservation::MarkerUnverifiable {
                marker_path,
                defect,
                ..
            } => Err(anyhow::anyhow!(defect.refusal(&marker_path))),
        }
    }

    /// The project this root presents, for surfacing.
    ///
    /// Deliberately lenient where [`Self::require_exclusive`] refuses: a root
    /// whose identity files disagree, or whose marker `rwv doctor --fix` can
    /// still migrate, presents a project all the same, and answering `None`
    /// there would collapse symlink surfacing on a tree the operator has a
    /// one-command repair for.
    pub fn presented_project(&self) -> Option<&ProjectName> {
        match self {
            RootObservation::Workweave { marker } => Some(&marker.project),
            RootObservation::Primary { selection } => selection.as_ref(),
            RootObservation::Disputed { marker, .. } => Some(&marker.project),
            RootObservation::MarkerUnverifiable { project_hint, .. } => project_hint.as_ref(),
        }
    }
}

/// The identity of a weave root that carries exactly one identity file.
///
/// [`RootObservation::require_exclusive`] is the only producer, and the arms
/// it refuses have no representation here. Both payloads hold private fields
/// for that reason: a consumer that could assemble one from a marker and a
/// pointer would be re-deciding, at its own site, the tiebreak this projection
/// exists to have already refused.
#[derive(Debug)]
pub enum WeaveRootIdentity {
    Workweave(WorkweaveIdentity),
    Primary(PrimaryIdentity),
}

#[derive(Debug)]
pub struct WorkweaveIdentity {
    marker: WorkweaveMarker,
}

impl WorkweaveIdentity {
    pub fn into_marker(self) -> WorkweaveMarker {
        self.marker
    }
}

#[derive(Debug)]
pub struct PrimaryIdentity {
    selection: Option<ProjectName>,
}

impl PrimaryIdentity {
    pub fn into_selection(self) -> Option<ProjectName> {
        self.selection
    }
}

/// The index-side counterpart of the legacy-marker check in
/// [`WorkweaveMarker::read`]: does `(primary_root, project)`'s workweave
/// index predate ref-ownership receipts?
///
/// Two legacy shapes migrate in the same `rwv doctor --fix` pass and each is
/// detected where it lives — the marker's missing `parent:` field above, the
/// index's
/// missing `receipts` field here. `Some(path)` is the file to migrate, and
/// [`crate::workweave_index::RefRegistry::migrate_legacy_index`] is what
/// migrates it; the pass then records a receipt per ref it adopts or
/// renames, receipt-first like every other arm.
///
/// The two detections differ in one deliberate way. A legacy *marker* is
/// refused at read: nothing downstream can proceed without knowing the
/// workweave's parent. A legacy *index* is reported, not refused, because
/// the migration pass has to be able to read the index it is about to
/// migrate — and because an unmigrated index fails closed on its own
/// (no receipts, so nothing is destroyable under R2). The verbs that must
/// refuse are the ones that write refs, and they refuse at the registry
/// (`RefRegistry::record_created`), which is the last point before an
/// unowned ref would be created.
///
/// `None` when the index is current, or when there is no index file at all
/// — an absent file records no workweaves, so there is no field to migrate.
pub fn pending_index_migration(
    primary_root: &Path,
    project: &ProjectName,
) -> anyhow::Result<Option<PathBuf>> {
    let Some(index) = crate::workweave_index::read(primary_root, project)? else {
        return Ok(None);
    };
    if index.has_receipt_registry() {
        return Ok(None);
    }
    Ok(Some(crate::workweave_index::index_path(
        primary_root,
        project,
    )))
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
            WorkspaceContext::resolve(&root, Some(ProjectName::new("overridden-project").unwrap()))
                .unwrap();
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
            project: ProjectName::new("ws").unwrap(),
            parent: primary_canon,
        };
        marker.write(&weave_dir).unwrap();

        let ctx =
            WorkspaceContext::resolve(&weave_dir, Some(ProjectName::new("custom-proj").unwrap()))
                .unwrap();
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
        let project = ProjectName::new("web-app").unwrap();
        set_active_project(&root, &project).unwrap();

        let content = std::fs::read_to_string(root.join(".rwv-active")).unwrap();
        assert_eq!(content, "web-app\n");
    }

    #[test]
    fn set_active_project_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        set_active_project(&root, &ProjectName::new("old-project").unwrap()).unwrap();
        set_active_project(&root, &ProjectName::new("new-project").unwrap()).unwrap();

        let project = read_active_project(&root).expect("should return project");
        assert_eq!(project.as_str(), "new-project");
    }

    #[test]
    fn set_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let project = ProjectName::new("mobile-app").unwrap();
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
            WorkspaceContext::resolve(&root, Some(ProjectName::new("explicit-override").unwrap()))
                .unwrap();
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
        assert!(
            msg.contains("--allow-non-empty-dir"),
            "expected --allow-non-empty-dir hint, got: {msg}"
        );
    }

    #[test]
    fn require_workspace_or_empty_allow_non_empty_dir_bypasses_check() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("messy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("random.txt"), "stuff").unwrap();
        // Non-empty + --allow-non-empty-dir — should succeed.
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
            project: ProjectName::new("my-project").unwrap(),
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
            project: ProjectName::new("p").unwrap(),
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

    #[test]
    fn workweave_marker_migrate_legacy_backfills_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let legacy = "primary: /home/user/weaveroot/primary\nproject: legacy-project\n";
        std::fs::write(dir.join(".rwv-workweave"), legacy).unwrap();

        assert!(
            WorkweaveMarker::migrate_legacy(dir).unwrap(),
            "a genuinely legacy marker should be rewritten"
        );

        let marker = WorkweaveMarker::read(dir).unwrap().unwrap();
        assert_eq!(
            marker.primary,
            PathBuf::from("/home/user/weaveroot/primary")
        );
        assert_eq!(marker.parent, marker.primary);
        assert_eq!(marker.project.as_str(), "legacy-project");
    }

    #[test]
    fn workweave_marker_migrate_legacy_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let legacy = "primary: /home/user/weaveroot/primary\nproject: legacy-project\n";
        std::fs::write(dir.join(".rwv-workweave"), legacy).unwrap();

        assert!(WorkweaveMarker::migrate_legacy(dir).unwrap());
        assert!(
            !WorkweaveMarker::migrate_legacy(dir).unwrap(),
            "parent: is already present on the second call; must not rewrite"
        );
    }

    /// A plain YAML scalar cannot contain `: ` — the parser reads it as a
    /// nested mapping key. A primary path with that shape (quoted here the
    /// way the real serializer already quotes `primary:`) forces `parent:`
    /// through the same quoting when migrate_legacy backfills it.
    #[test]
    fn workweave_marker_migrate_legacy_quotes_yaml_special_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let legacy =
            "primary: '/home/user/weaveroot/has: a colon-space'\nproject: legacy-project\n";
        std::fs::write(dir.join(".rwv-workweave"), legacy).unwrap();

        assert!(WorkweaveMarker::migrate_legacy(dir).unwrap());

        let marker = WorkweaveMarker::read(dir)
            .expect("migrated marker must parse")
            .expect("migrated marker must be present");
        assert_eq!(
            marker.primary,
            PathBuf::from("/home/user/weaveroot/has: a colon-space")
        );
        assert_eq!(marker.parent, marker.primary);
    }

    /// Fixture: a primary with `projects/<name>/`, ready for an index.
    fn primary_with_project(root: &Path, name: &str) -> ProjectName {
        std::fs::create_dir_all(root.join("projects").join(name)).unwrap();
        ProjectName::new(name).unwrap()
    }

    #[test]
    fn pending_index_migration_reports_an_index_without_receipts() {
        // The index-side legacy shape, next to the marker-side one above:
        // written before ref-ownership receipts existed.
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = primary_with_project(&primary, "web-app");
        let path = crate::workweave_index::index_path(&primary, &project);
        std::fs::write(&path, r#"{"container":"/c","workweaves":{}}"#).unwrap();

        assert_eq!(
            pending_index_migration(&primary, &project).unwrap(),
            Some(path),
            "an index with no receipts field needs the §7.1 arm-7 migration"
        );
    }

    #[test]
    fn pending_index_migration_is_quiet_for_current_and_absent_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = primary_with_project(&primary, "web-app");

        assert_eq!(
            pending_index_migration(&primary, &project).unwrap(),
            None,
            "no index file records no workweaves, so there is no field to migrate"
        );

        crate::workweave_index::write(
            &primary,
            &project,
            &crate::workweave_index::WorkweaveIndex::new(PathBuf::from("/c")),
        )
        .unwrap();
        assert_eq!(
            pending_index_migration(&primary, &project).unwrap(),
            None,
            "an index this build wrote is not a legacy one, empty registry or not"
        );
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
            project: ProjectName::new("web-app").unwrap(),
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
            project: ProjectName::new("web-app").unwrap(),
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
            project: ProjectName::new("marker-project").unwrap(),
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
    // When tests run from inside a workweave that is itself under
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
    // `resolve()` canonicalizes `cwd` but the original code
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

    // ========================================================================
    // ProjectProvenance — chain-step tracking
    //
    // The chain is `--project > -w prefix > (.rwv-active | .rwv-workweave)`,
    // whose last tier is one step with two spellings — the marker in a
    // workweave root, the pointer at primary.
    // These tests exercise every constructed variant (Flag / Marker /
    // ActiveFile / WorkweaveFlag) across primary and workweave checkouts,
    // both single-project and multi-project workspaces, and the None case
    // (no chain step fires). The WorkweaveFlag variant is constructed via
    // `with_workweave_flag_provenance` — see `tests/workweave_flag_test.rs`
    // for the end-to-end -w flag tests.
    // ========================================================================

    /// Helper: create N project directories under `<root>/projects/`.
    fn make_projects(root: &Path, names: &[&str]) {
        for name in names {
            std::fs::create_dir_all(root.join("projects").join(name)).unwrap();
        }
    }

    /// Helper: write a workweave marker at `dir` pointing at `primary` and
    /// naming `project`.
    fn write_marker(dir: &Path, primary: &Path, project: &str) {
        let marker = WorkweaveMarker {
            primary: primary.to_path_buf(),
            project: ProjectName::new(project).unwrap(),
            parent: primary.to_path_buf(),
        };
        marker.write(dir).unwrap();
    }

    /// Chain step 1: `--project` at a primary wins even when `.rwv-active`
    /// is set. Provenance = Flag.
    #[test]
    fn provenance_flag_at_primary_beats_active_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["one", "two"]);
        std::fs::write(root.join(".rwv-active"), "one\n").unwrap();

        let ctx = WorkspaceContext::resolve(&root, Some(ProjectName::new("two").unwrap())).unwrap();
        assert_eq!(ctx.active_project().unwrap().as_str(), "two");
        assert_eq!(ctx.project_provenance(), Some(ProjectProvenance::Flag));
    }

    /// Chain step 1: `--project` in a workweave wins even when the marker
    /// names a different project. Provenance = Flag.
    #[test]
    fn provenance_flag_in_workweave_beats_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["one", "two"]);
        let weave_dir = tmp.path().join("ws--feat");
        std::fs::create_dir_all(&weave_dir).unwrap();
        write_marker(&weave_dir, &root.canonicalize().unwrap(), "one");

        let ctx =
            WorkspaceContext::resolve(&weave_dir, Some(ProjectName::new("two").unwrap())).unwrap();
        assert_eq!(ctx.active_project().unwrap().as_str(), "two");
        assert_eq!(ctx.project_provenance(), Some(ProjectProvenance::Flag));
    }

    /// Chain step 3: inside a workweave with no `--project`, the marker
    /// determines the project. Provenance = Marker. `.rwv-active` at the
    /// primary is not consulted for a workweave — the workweave is
    /// structurally scoped to one project.
    #[test]
    fn provenance_marker_in_workweave_ignores_active_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["one", "two"]);
        // Pointer at primary names a different project — must not leak.
        std::fs::write(root.join(".rwv-active"), "two\n").unwrap();
        let weave_dir = tmp.path().join("ws--feat");
        std::fs::create_dir_all(&weave_dir).unwrap();
        write_marker(&weave_dir, &root.canonicalize().unwrap(), "one");

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        assert_eq!(ctx.active_project().unwrap().as_str(), "one");
        assert_eq!(ctx.project_provenance(), Some(ProjectProvenance::Marker));
    }

    /// Chain step 4: at a primary, no `--project`, `.rwv-active` names an
    /// existing project. Provenance = ActiveFile. Single-project workspace.
    #[test]
    fn provenance_active_file_at_primary_single_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["only"]);
        std::fs::write(root.join(".rwv-active"), "only\n").unwrap();

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        assert_eq!(ctx.active_project().unwrap().as_str(), "only");
        assert_eq!(
            ctx.project_provenance(),
            Some(ProjectProvenance::ActiveFile)
        );
    }

    /// Chain step 4: at a primary in an N-project workspace with no
    /// `--project`, `.rwv-active` is what decides. Provenance = ActiveFile.
    /// This is the incident-class case — the pointer silently selects
    /// among alternatives, so the target-line surfacing exists.
    #[test]
    fn provenance_active_file_at_primary_multi_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["a", "b", "c"]);
        std::fs::write(root.join(".rwv-active"), "b\n").unwrap();

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        assert_eq!(ctx.active_project().unwrap().as_str(), "b");
        assert_eq!(
            ctx.project_provenance(),
            Some(ProjectProvenance::ActiveFile)
        );
    }

    /// No chain step fires when no `--project`, no `.rwv-active`, and CWD
    /// is at the primary. `active_project()` returns None; provenance is
    /// also None. Caller surfaces via `require_active_project`.
    #[test]
    fn provenance_none_when_no_project_resolvable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["only"]);
        // No .rwv-active written.

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        assert!(ctx.active_project().is_none());
        assert_eq!(ctx.project_provenance(), None);
    }

    /// Chain step 1 wins even when a marker AND an active-file would both
    /// otherwise fire — the flag is the topmost step.
    #[test]
    fn provenance_flag_beats_marker_and_active_file_together() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["marker-p", "active-p", "flag-p"]);
        std::fs::write(root.join(".rwv-active"), "active-p\n").unwrap();
        let weave_dir = tmp.path().join("ws--feat");
        std::fs::create_dir_all(&weave_dir).unwrap();
        write_marker(&weave_dir, &root.canonicalize().unwrap(), "marker-p");

        let ctx = WorkspaceContext::resolve(&weave_dir, Some(ProjectName::new("flag-p").unwrap()))
            .unwrap();
        assert_eq!(ctx.active_project().unwrap().as_str(), "flag-p");
        assert_eq!(ctx.project_provenance(), Some(ProjectProvenance::Flag));
    }

    // ========================================================================
    // Target-line surfacing — emit_target_line writes to stderr only for
    // ActiveFile provenance.
    //
    // Rust's println!/eprintln! macros are not directly capturable in unit
    // tests without setting up a custom writer. We test the *policy*
    // (which provenance fires the surfacing) at the accessor level and
    // cover the actual stderr output in the integration-shaped test that
    // spawns the binary (see rwv-cli-level assertions elsewhere in the
    // test suite). The unit tests below assert the boolean policy that
    // `emit_target_line` follows.
    // ========================================================================

    /// The target-line policy fires only for ActiveFile provenance.
    #[test]
    fn target_line_policy_fires_only_for_active_file() {
        // ActiveFile → fires.
        assert!(matches!(
            Some(ProjectProvenance::ActiveFile),
            Some(ProjectProvenance::ActiveFile)
        ));

        // Flag / Marker / None → silent.
        for prov in [
            Some(ProjectProvenance::Flag),
            Some(ProjectProvenance::Marker),
            None,
        ] {
            assert_ne!(prov, Some(ProjectProvenance::ActiveFile));
        }
    }

    /// End-to-end smoke: `emit_target_line` is a no-op when provenance is
    /// not ActiveFile. Can't easily capture stderr, but we can verify the
    /// call does not panic under each variant and that the provenance-gated
    /// path is exercised.
    #[test]
    fn emit_target_line_is_noop_for_non_active_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["p"]);

        // Flag: silent.
        let ctx = WorkspaceContext::resolve(&root, Some(ProjectName::new("p").unwrap())).unwrap();
        assert_eq!(ctx.project_provenance(), Some(ProjectProvenance::Flag));
        ctx.emit_target_line(); // must not panic; policy says silent

        // Marker: silent.
        let weave_dir = tmp.path().join("ws--feat");
        std::fs::create_dir_all(&weave_dir).unwrap();
        write_marker(&weave_dir, &root.canonicalize().unwrap(), "p");
        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        assert_eq!(ctx.project_provenance(), Some(ProjectProvenance::Marker));
        ctx.emit_target_line(); // must not panic; policy says silent

        // None: silent.
        let root2 = make_workspace(tmp.path(), "ws2");
        make_projects(&root2, &["p"]);
        let ctx = WorkspaceContext::resolve(&root2, None).unwrap();
        assert_eq!(ctx.project_provenance(), None);
        ctx.emit_target_line(); // must not panic; policy says silent
    }

    /// End-to-end smoke: `emit_target_line` runs (and would print) when
    /// provenance is ActiveFile. Verified structurally — the accessor
    /// reports the expected variant and the method returns without panic;
    /// the stderr contents are covered by the `target_line_*` integration
    /// tests in the CLI test suite.
    #[test]
    fn emit_target_line_runs_for_active_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["a", "b"]);
        std::fs::write(root.join(".rwv-active"), "b\n").unwrap();

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        assert_eq!(
            ctx.project_provenance(),
            Some(ProjectProvenance::ActiveFile)
        );
        // Would print to stderr; test harness ignores stderr for passing tests.
        ctx.emit_target_line();
    }

    // ========================================================================
    // Missing-pointer error text — corrective advice.
    //
    // The pointer is total by construction (init/fetch/workweave-create
    // all activate on creation), so reaching `require_active_project` at
    // a primary with no pointer means hand-surgery. The error must name
    // the fix commands (`rwv activate <name>` / `--project <name>`) and,
    // when projects exist, list them so the operator has a menu.
    // ========================================================================

    /// No `.rwv-active`, no projects on disk: the error names `rwv init`
    /// as the bootstrapping command since there are no existing projects
    /// to activate.
    #[test]
    fn require_active_project_no_projects_names_init() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let err = ctx.require_active_project().unwrap_err().to_string();
        assert!(err.contains("no active project"), "err: {err}");
        assert!(err.contains("rwv activate"), "err: {err}");
        assert!(err.contains("--project"), "err: {err}");
        assert!(
            err.contains("rwv init"),
            "err should suggest init when no projects exist: {err}"
        );
    }

    /// No `.rwv-active`, some projects on disk: the error lists them so
    /// the operator has a menu — corrective, not just diagnostic.
    #[test]
    fn require_active_project_lists_existing_projects() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["alpha", "beta"]);
        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let err = ctx.require_active_project().unwrap_err().to_string();
        assert!(err.contains("no active project"), "err: {err}");
        assert!(err.contains("rwv activate"), "err: {err}");
        assert!(err.contains("alpha"), "err should list existing: {err}");
        assert!(err.contains("beta"), "err should list existing: {err}");
    }

    /// Stale pointer (`.rwv-active` names a project whose directory is
    /// missing): `require_active_project_on_disk` errors with an
    /// actionable message. This case is what the doctor
    /// `DanglingActiveProject` check surfaces at scan time.
    #[test]
    fn require_active_project_on_disk_stale_pointer_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        // .rwv-active names a project whose directory does NOT exist.
        std::fs::write(root.join(".rwv-active"), "ghost\n").unwrap();
        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let err = ctx
            .require_active_project_on_disk()
            .unwrap_err()
            .to_string();
        // Message must name the stale project and point at the corrective
        // commands, following the house error style.
        assert!(err.contains("ghost"), "err: {err}");
        assert!(err.contains("rwv activate"), "err: {err}");
        assert!(
            err.contains(".rwv-active") || err.contains("projects/"),
            "err: {err}"
        );
    }

    /// Stale pointer error lists existing valid projects so the operator
    /// can pick one directly.
    #[test]
    fn require_active_project_on_disk_stale_lists_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["real-one", "real-two"]);
        std::fs::write(root.join(".rwv-active"), "ghost\n").unwrap();
        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let err = ctx
            .require_active_project_on_disk()
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("real-one") && err.contains("real-two"),
            "existing projects should be listed: {err}"
        );
    }

    // ========================================================================
    // Resolution projection tests
    //
    // These tests pin the contract that `WorkspaceContext::resolution()` is a
    // pure projection of the resolved context — one function, never
    // independently computed. A future change will emit an env-var envelope
    // (RWV_WORKSPACE / RWV_WORKWEAVE / RWV_PROJECT) as a second serialization
    // of this same projection; the envelope-agreement half of the test lands
    // with that work.
    // ========================================================================

    /// At primary with an active project: resolution is present, workweave
    /// absent (no workweave checkout), workspace and project match the context.
    #[test]
    fn resolution_at_primary_has_workspace_and_project_no_workweave() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "myproject\n").unwrap();

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let res = ctx
            .resolution()
            .expect("resolution must be present with an active project");

        assert_eq!(res.workspace, root.canonicalize().unwrap());
        assert_eq!(res.project, "myproject");
        assert!(
            res.workweave.is_none(),
            "workweave must be absent at primary; got {:?}",
            res.workweave
        );
    }

    /// At primary without an active project: resolution is absent entirely.
    #[test]
    fn resolution_at_primary_without_active_project_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        // No .rwv-active file — no project resolved.

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        assert!(
            ctx.resolution().is_none(),
            "resolution must be absent when no project is resolved"
        );
    }

    /// Inside a workweave: resolution has workweave = "<project>--<name>" and
    /// the identity matches the context's own checkout exactly.
    #[test]
    fn resolution_in_workweave_has_workweave_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let primary_canon = root.canonicalize().unwrap();

        // Create a workweave directory with the marker.
        let weave_dir = tmp.path().join("ws--fo-abc");
        std::fs::create_dir_all(&weave_dir).unwrap();
        let marker = WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("myproject").unwrap(),
            parent: primary_canon,
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        let res = ctx
            .resolution()
            .expect("resolution must be present in a workweave");

        // workweave must be present and match the <project>--<name> identity.
        let ww = res
            .workweave
            .as_deref()
            .expect("workweave must be present in workweave checkout");
        assert_eq!(
            ww, "myproject--fo-abc",
            "workweave identity must be '<project>--<name>', got {ww:?}"
        );
        assert_eq!(res.project, "myproject");
        // workspace is the primary root.
        assert_eq!(
            res.workspace,
            weave_dir
                .canonicalize()
                .unwrap()
                .parent()
                .unwrap()
                .join("ws")
                .canonicalize()
                .unwrap()
        );
    }

    /// Key-set contract: JSON serialization of the resolution block at primary
    /// has exactly two keys (workspace, project) — workweave omitted, no
    /// provenance fields. This guards against accidental leakage.
    #[test]
    fn resolution_json_key_set_at_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "myproject\n").unwrap();

        let ctx = WorkspaceContext::resolve(&root, None).unwrap();
        let res = ctx.resolution().unwrap();
        let json = serde_json::to_value(&res).expect("serializes");
        let obj = json.as_object().expect("is a JSON object");

        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> =
            ["workspace", "project"].iter().copied().collect();
        assert_eq!(
            keys, expected,
            "at primary: exactly {{workspace, project}} — no workweave, no provenance fields"
        );
    }

    /// Key-set contract: JSON serialization of the resolution block in a
    /// workweave has exactly three keys (workspace, workweave, project) —
    /// no provenance fields.
    #[test]
    fn resolution_json_key_set_in_workweave() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let primary_canon = root.canonicalize().unwrap();

        let weave_dir = tmp.path().join("ws--fo-abc");
        std::fs::create_dir_all(&weave_dir).unwrap();
        let marker = WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("myproject").unwrap(),
            parent: primary_canon,
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve(&weave_dir, None).unwrap();
        let res = ctx.resolution().unwrap();
        let json = serde_json::to_value(&res).expect("serializes");
        let obj = json.as_object().expect("is a JSON object");

        let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
        let expected: std::collections::BTreeSet<&str> = ["workspace", "workweave", "project"]
            .iter()
            .copied()
            .collect();
        assert_eq!(
            keys, expected,
            "in workweave: exactly {{workspace, workweave, project}} — no provenance fields"
        );
    }

    // ========================================================================
    // observe_root — one reader for the two identity files
    //
    // The arms are the states a directory can be in. Which of them a verb may
    // act on is `require_exclusive`'s question, pinned separately below.
    // ========================================================================

    /// A workweave root whose marker points at `primary`.
    fn make_workweave(parent: &Path, name: &str, primary: &Path, project: &str) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = WorkweaveMarker {
            primary: primary.to_path_buf(),
            project: ProjectName::new(project).unwrap(),
            parent: primary.to_path_buf(),
        };
        marker.write(&dir).unwrap();
        dir
    }

    fn write_pointer(root: &Path, project: &str) {
        std::fs::write(root.join(ACTIVE_PROJECT_FILE), format!("{project}\n")).unwrap();
    }

    fn observe(dir: &Path) -> RootObservation {
        observe_root(dir)
            .unwrap()
            .unwrap_or_else(|| panic!("expected an observation at {}", dir.display()))
    }

    #[test]
    fn observe_root_reads_a_verified_marker_as_a_workweave() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "ws");
        // Deliberately not workspace-shaped: the marker arm does not consult
        // the tree's own shape, and a workweave that has not materialized any
        // member yet still resolves.
        let weave_dir = make_workweave(tmp.path(), "ws--feat", &primary, "myproject");

        match observe(&weave_dir) {
            RootObservation::Workweave { marker } => {
                assert_eq!(marker.project.as_str(), "myproject");
                assert_eq!(marker.primary, primary);
            }
            other => panic!("expected Workweave, got {other:?}"),
        }
    }

    #[test]
    fn observe_root_reads_a_pointer_at_a_workspace_shaped_root_as_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        write_pointer(&root, "web-app");

        match observe(&root) {
            RootObservation::Primary { selection } => {
                assert_eq!(selection.as_ref().map(ProjectName::as_str), Some("web-app"));
            }
            other => panic!("expected Primary, got {other:?}"),
        }
    }

    #[test]
    fn observe_root_reads_a_bare_workspace_shaped_root_as_primary_with_no_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        match observe(&root) {
            RootObservation::Primary { selection } => assert!(selection.is_none()),
            other => panic!("expected Primary, got {other:?}"),
        }
    }

    #[test]
    fn observe_root_reads_both_files_as_disputed() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "ws");
        let weave_dir = make_workweave(tmp.path(), "ws--feat", &primary, "myproject");
        write_pointer(&weave_dir, "myproject");

        match observe(&weave_dir) {
            RootObservation::Disputed {
                root,
                marker,
                pointer,
            } => {
                assert_eq!(root, weave_dir);
                assert_eq!(marker.project.as_str(), "myproject");
                assert_eq!(pointer.as_ref().map(ProjectName::as_str), Some("myproject"));
            }
            other => panic!("expected Disputed, got {other:?}"),
        }
    }

    /// An empty `.rwv-active` is still a present file, so the root is still
    /// disputed — the pointer it cannot parse is the one thing the arm does
    /// not need in order to say so.
    #[test]
    fn observe_root_reads_an_empty_pointer_beside_a_marker_as_disputed() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "ws");
        let weave_dir = make_workweave(tmp.path(), "ws--feat", &primary, "myproject");
        std::fs::write(weave_dir.join(ACTIVE_PROJECT_FILE), "\n").unwrap();

        match observe(&weave_dir) {
            RootObservation::Disputed { pointer, .. } => assert!(pointer.is_none()),
            other => panic!("expected Disputed, got {other:?}"),
        }
    }

    #[test]
    fn observe_root_reads_a_dangling_primary_as_marker_unverifiable() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("moved-away");
        let weave_dir = make_workweave(tmp.path(), "ws--feat", &gone, "myproject");
        // A copied tree is workspace-shaped on its own; the marker's claim is
        // what makes reading that shape a guess.
        std::fs::create_dir_all(weave_dir.join("github")).unwrap();

        match observe(&weave_dir) {
            RootObservation::MarkerUnverifiable {
                marker_path,
                defect,
                project_hint,
            } => {
                assert_eq!(marker_path, weave_dir.join(WORKWEAVE_MARKER_FILE));
                match defect {
                    MarkerDefect::DanglingPrimary { primary } => assert_eq!(primary, gone),
                    other => panic!("expected DanglingPrimary, got {other:?}"),
                }
                assert_eq!(
                    project_hint.as_ref().map(ProjectName::as_str),
                    Some("myproject")
                );
            }
            other => panic!("expected MarkerUnverifiable, got {other:?}"),
        }
    }

    #[test]
    fn observe_root_reads_a_legacy_marker_as_marker_unverifiable() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "ws");
        let weave_dir = tmp.path().join("ws--feat");
        std::fs::create_dir_all(&weave_dir).unwrap();
        std::fs::write(
            weave_dir.join(WORKWEAVE_MARKER_FILE),
            format!("primary: {}\nproject: myproject\n", primary.display()),
        )
        .unwrap();

        match observe(&weave_dir) {
            RootObservation::MarkerUnverifiable {
                defect,
                project_hint,
                ..
            } => {
                assert!(
                    matches!(defect, MarkerDefect::Legacy),
                    "expected Legacy, got {defect:?}"
                );
                assert_eq!(
                    project_hint.as_ref().map(ProjectName::as_str),
                    Some("myproject"),
                    "a legacy marker still names its project, and surfacing needs it"
                );
            }
            other => panic!("expected MarkerUnverifiable, got {other:?}"),
        }
    }

    #[test]
    fn observe_root_reads_an_unparseable_marker_as_marker_unverifiable() {
        let tmp = tempfile::tempdir().unwrap();
        let weave_dir = tmp.path().join("ws--feat");
        std::fs::create_dir_all(&weave_dir).unwrap();
        std::fs::write(
            weave_dir.join(WORKWEAVE_MARKER_FILE),
            "primary: [unclosed\n",
        )
        .unwrap();

        match observe(&weave_dir) {
            RootObservation::MarkerUnverifiable { defect, .. } => assert!(
                matches!(defect, MarkerDefect::Unreadable { .. }),
                "expected Unreadable, got {defect:?}"
            ),
            other => panic!("expected MarkerUnverifiable, got {other:?}"),
        }
    }

    #[test]
    fn observe_root_reads_a_directory_that_is_neither_as_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("just-a-dir");
        std::fs::create_dir_all(&plain).unwrap();

        assert!(observe_root(&plain).unwrap().is_none());
    }

    /// A pointer outside a workspace-shaped tree names nothing: `.rwv-active`
    /// is not itself a witness of root-ness, and reading it as one would make
    /// any directory a weave root.
    #[test]
    fn observe_root_reads_a_lone_pointer_as_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let plain = tmp.path().join("just-a-dir");
        std::fs::create_dir_all(&plain).unwrap();
        write_pointer(&plain, "web-app");

        assert!(observe_root(&plain).unwrap().is_none());
    }

    // ========================================================================
    // require_exclusive — the one collapse, and the two arms it refuses
    // ========================================================================

    /// Every `MarkerDefect`, with a match that stops compiling when a variant
    /// is added. A new defect that nobody adds here is a defect the refusal
    /// test below would never see.
    fn all_marker_defects() -> Vec<MarkerDefect> {
        let all = vec![
            MarkerDefect::DanglingPrimary {
                primary: PathBuf::from("/nowhere"),
            },
            MarkerDefect::Legacy,
            MarkerDefect::Unreadable {
                detail: "failed to parse .rwv-workweave".to_string(),
            },
        ];
        for defect in &all {
            match defect {
                MarkerDefect::DanglingPrimary { .. }
                | MarkerDefect::Legacy
                | MarkerDefect::Unreadable { .. } => {}
            }
        }
        all
    }

    #[test]
    fn require_exclusive_projects_the_two_usable_arms() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "ws");
        let weave_dir = make_workweave(tmp.path(), "ws--feat", &primary, "myproject");
        write_pointer(&primary, "web-app");

        match observe(&weave_dir).require_exclusive().unwrap() {
            WeaveRootIdentity::Workweave(workweave) => {
                assert_eq!(workweave.into_marker().project.as_str(), "myproject");
            }
            other => panic!("expected the workweave arm, got {other:?}"),
        }
        match observe(&primary).require_exclusive().unwrap() {
            WeaveRootIdentity::Primary(root) => {
                assert_eq!(
                    root.into_selection().as_ref().map(ProjectName::as_str),
                    Some("web-app")
                );
            }
            other => panic!("expected the primary arm, got {other:?}"),
        }
    }

    #[test]
    fn require_exclusive_refuses_a_disputed_root_naming_both_files_and_the_repair() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "ws");
        let weave_dir = make_workweave(tmp.path(), "ws--feat", &primary, "myproject");
        write_pointer(&weave_dir, "something-else");

        let err = observe(&weave_dir)
            .require_exclusive()
            .expect_err("a root carrying both identity files is not actionable")
            .to_string();

        for expected in [
            weave_dir.join(WORKWEAVE_MARKER_FILE).display().to_string(),
            weave_dir.join(ACTIVE_PROJECT_FILE).display().to_string(),
            "rwv doctor --fix".to_string(),
        ] {
            assert!(
                err.contains(&expected),
                "the refusal must name {expected}; got: {err}"
            );
        }
    }

    /// Agreement between the two files does not soften the refusal: the state
    /// no writer produces is the state that must not survive being tolerated.
    #[test]
    fn require_exclusive_refuses_a_disputed_root_whose_files_agree() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "ws");
        let weave_dir = make_workweave(tmp.path(), "ws--feat", &primary, "myproject");
        write_pointer(&weave_dir, "myproject");

        assert!(observe(&weave_dir).require_exclusive().is_err());
    }

    #[test]
    fn require_exclusive_refuses_every_marker_defect() {
        let marker_path = PathBuf::from("/weave/ws--feat/.rwv-workweave");
        for defect in all_marker_defects() {
            let described = format!("{defect:?}");
            let err = RootObservation::MarkerUnverifiable {
                marker_path: marker_path.clone(),
                defect,
                project_hint: None,
            }
            .require_exclusive()
            .map(|_| ())
            .expect_err(&format!(
                "{described} must be terminal — falling through to Primary \
                 hands the tree the authority its own broken claim denies it"
            ))
            .to_string();

            assert!(
                err.contains(&marker_path.display().to_string()),
                "{described}: the refusal must name the marker; got: {err}"
            );
        }
    }

    #[test]
    fn require_exclusive_names_the_dangling_value_and_both_exits() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("moved-away");
        let weave_dir = make_workweave(tmp.path(), "ws--feat", &gone, "myproject");

        let err = observe(&weave_dir)
            .require_exclusive()
            .expect_err("a dangling primary: is not actionable")
            .to_string();

        assert!(
            err.contains(&gone.display().to_string()),
            "the refusal must name the value that does not verify; got: {err}"
        );
        assert!(
            err.contains("primary:") && err.contains("delete"),
            "the refusal must name both exits — repair the marker, or delete \
             it to adopt the tree standalone; got: {err}"
        );
    }

    // ========================================================================
    // presented_project — lenient exactly where require_exclusive is strict
    // ========================================================================

    #[test]
    fn presented_project_answers_for_a_disputed_root() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "ws");
        let weave_dir = make_workweave(tmp.path(), "ws--feat", &primary, "myproject");
        write_pointer(&weave_dir, "something-else");

        assert!(
            observe(&weave_dir).require_exclusive().is_err(),
            "the same root a verb must refuse"
        );
        assert_eq!(
            observe(&weave_dir)
                .presented_project()
                .map(ProjectName::as_str),
            Some("myproject"),
            "the marker names what the root presents; the pointer is the stray"
        );
    }

    #[test]
    fn presented_project_answers_for_a_legacy_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "ws");
        let weave_dir = tmp.path().join("ws--feat");
        std::fs::create_dir_all(&weave_dir).unwrap();
        std::fs::write(
            weave_dir.join(WORKWEAVE_MARKER_FILE),
            format!("primary: {}\nproject: myproject\n", primary.display()),
        )
        .unwrap();

        assert_eq!(
            observe(&weave_dir)
                .presented_project()
                .map(ProjectName::as_str),
            Some("myproject"),
            "surfacing must keep working on a workweave `rwv doctor --fix` can \
             still migrate"
        );
    }

    #[test]
    fn presented_project_answers_for_a_primary_from_its_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        write_pointer(&root, "web-app");

        assert_eq!(
            observe(&root).presented_project().map(ProjectName::as_str),
            Some("web-app")
        );
    }
}
