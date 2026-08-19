//! Workspace: the resolved state of a repoweave directory tree.
//!
//! A workspace is the top-level directory containing registry dirs, projects,
//! and ecosystem files. This module resolves the workspace from an *origin
//! directory* (the input to resolution) and provides the context that
//! commands operate on.
//!
//! ## Single resolution point
//!
//! rwv acquires the origin dir at most once per invocation — the single
//! resolution point in dispatch calls [`acquire_origin_dir`] when no `-C`
//! supplies an origin instead, and every downstream handler receives an
//! already-resolved [`WorkspaceContext`]. There must be no
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
//! feed a flag-derived path to [`WorkspaceContext::resolve_invocation`] instead of
//! [`acquire_origin_dir`], and everything downstream would still work.

use crate::integration_runner::IntegrationContextBase;
use crate::manifest::{Manifest, ProjectName, RepoPath, WorkweaveName};
use crate::registry::{builtin_registries, builtin_registry_names, Registry};
use crate::vcs::Vcs;
use anyhow::Context;
use saphyr::{LoadableYamlNode, YamlOwned};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

/// Acquire the origin directory for the invocation.
///
/// **This is the only `std::env::current_dir()` read whose result feeds
/// resolution.** All resolution flows through here and then
/// [`WorkspaceContext::resolve_invocation`]; handlers must receive an already-resolved
/// context and must not consult the process cwd on their own. The one
/// other reader in the tree is `workweave delete`'s step-out probe, which
/// asks whether this process holds an open handle inside the tree being
/// removed and derives no path from the answer.
///
/// The distinction between *acquire* and *resolve* is load-bearing: `-C
/// <path>` and `-w <name>` inject a different origin dir into the same
/// resolver, and nothing downstream of the resolver distinguishes those
/// origins from the process cwd.
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
///   `.workweaves/` listing, `static-files` surfacing targets).
/// - [`active_path`] — the directory the checkout points to: the primary
///   path when in a primary, the workweave directory when in a workweave.
///   Use for per-workspace state (project worktrees and their `rwv.lock` /
///   `rwv.toml`, repo worktrees the operator is working in).
///
/// [`primary_path`]: WorkspaceContext::primary_path
/// [`active_path`]: WorkspaceContext::active_path
#[derive(Debug, Clone)]
pub struct WorkspaceContext {
    primary_root: PathBuf,
    /// The witness for [`PrimaryIdentity::select_project`], present exactly
    /// when the walk landed on a primary root. A workweave resolves through
    /// the marker and so has none, which is what makes "select a project from
    /// inside a workweave" unwritable rather than merely refused.
    primary_identity: Option<PrimaryIdentity>,
    /// Which kind of checkout the origin dir resolved into: the primary,
    /// or a specific workweave.
    pub checkout: Checkout,
    /// The project name inferred from the origin dir (when the origin is
    /// inside `{root}/projects/{name}/...`), independent of the active
    /// project.
    ///
    /// Recorded for diagnostics — `rwv` bare status surfaces the
    /// divergence, and command implementations use it to build the
    /// "you're in projects/\<X\>/ but \<Y\> is active" error message now
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
/// precedence between them to get wrong. [`WorkspaceContext::resolve_invocation`]
/// refuses a root that offers both; `rwv doctor` reports and repairs it
/// ([`CheckViolation::WeaveRootIdentityConflict`]).
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
    /// The project was supplied by a caller that already held the binding —
    /// an operation resolving a workspace other than the one it was invoked
    /// from. No chain step ran and no pointer was read, so this provenance is
    /// unreachable from an invocation and never surfaces a "target:" line.
    Bound,
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
        name: WorkweaveNameRecord,
        /// The workweave directory path (e.g., `.workweaves/feat/` or `root/../ws--feat/`).
        dir: PathBuf,
        /// The project this workweave belongs to.
        project: ProjectName,
        /// The workspace this workweave was forked from, per its marker: the
        /// primary when forked from primary, the parent workweave's path
        /// when forked from another workweave.
        parent: PathBuf,
    },
}

impl Checkout {
    /// Project this checkout onto its [`ContainerKind`], discarding the
    /// per-variant payload (name, dir, project, parent).
    pub fn kind(&self) -> ContainerKind {
        match self {
            Checkout::Primary { .. } => ContainerKind::Primary,
            Checkout::Workweave { .. } => ContainerKind::Workweave,
        }
    }
}

/// What the primary-side registry calls the workweave a resolution landed in,
/// or the absence of any record naming it.
///
/// The name comes from the index entry whose recorded path is this directory,
/// matched by filesystem identity. The directory's own basename is discovery
/// and legibility — nothing derives a name from it, so a marker-bearing
/// directory the registry does not record has no name at all rather than a
/// name spelled the way the shell happened to reach it.
///
/// [`Unregistered`](Self::Unregistered) is that absence carried forward, not a
/// signal to fall back: a surface that reports what it found calls
/// [`recorded`](Self::recorded), and an operation that acts on the identity
/// calls [`require`](Self::require) and refuses without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkweaveNameRecord {
    Recorded(WorkweaveName),
    Unregistered,
}

impl WorkweaveNameRecord {
    /// The recorded name for a surface that reports what the records hold and
    /// keeps working when they hold nothing.
    pub fn recorded(&self) -> Option<&WorkweaveName> {
        match self {
            Self::Recorded(name) => Some(name),
            Self::Unregistered => None,
        }
    }

    /// The recorded name for an operation that acts on the workweave's
    /// identity and has nothing to act on without one.
    ///
    /// The refusal names two repairs, and states the condition that decides
    /// between them rather than leaving the operator to try both: adoption
    /// enumerates the recorded containers, so a directory placed outside them
    /// is never a candidate and `--fix` reports nothing for it. The condition
    /// is stated, not evaluated here — deciding it needs the container walk
    /// *and* the basename parse that `doctor_scan_container` pairs it with,
    /// and a second copy of that conjunction is a thing that can disagree with
    /// the one doctor runs.
    pub fn require(&self, dir: &Path, project: &ProjectName) -> anyhow::Result<&WorkweaveName> {
        match self {
            Self::Recorded(name) => Ok(name),
            Self::Unregistered => anyhow::bail!(
                "the workweave at {} carries a `.rwv-workweave` marker for project `{}`, \
                 but no entry in that project's workweave index records this directory, \
                 so rwv has no recorded name for it and will not take one from the \
                 directory name. This operation acts on that name. Two repairs, and \
                 where this directory sits decides which one applies. \
                 `rwv doctor --fix` adopts an unrecorded workweave only out of a \
                 container recorded in `{}`, and only when the directory is spelled \
                 `{}--<name>`; one placed anywhere else — `rwv workweave {} create \
                 <name> --dir <path>` does that — is not a candidate, and `--fix` will \
                 find nothing to adopt here. Retiring this directory and creating the \
                 workweave again works wherever it sits.",
                crate::path_spelling::operator_path(dir),
                project.as_str(),
                format_args!(
                    "{}/{}",
                    project_rel_path(project.as_str()),
                    crate::workweave_index::INDEX_FILENAME
                ),
                project.as_str(),
                project.as_str(),
            ),
        }
    }
}

/// What a diagnostic surface says about a workweave no registry entry names.
///
/// One home so that every verb which reports the state rather than refusing on
/// it reports the same state in the same words.
pub const UNREGISTERED_WORKWEAVE_NOTICE: &str =
    "Warning: no workweave index entry records this directory, so it has no recorded \
     name and verbs that act on the workweave's identity will refuse. Run \
     `rwv doctor --fix` to register it.";

/// [`Checkout`] without its payload — what an integration needs to know
/// about the container it is running in, and all it needs: which of the two
/// kinds, not which workweave or which project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    Primary,
    Workweave,
}

// ---------------------------------------------------------------------------
// Weave layout: every project lives at `projects/<name>`
// ---------------------------------------------------------------------------

/// The directory every project's files live under, relative to a weave root.
const PROJECTS_DIR: &str = "projects";

/// `<root>/projects` — where a weave keeps its projects.
pub(crate) fn projects_dir(root: &Path) -> PathBuf {
    root.join(PROJECTS_DIR)
}

/// `<root>/projects/<project>` — where one project's files live.
pub(crate) fn project_dir(root: &Path, project: &str) -> PathBuf {
    projects_dir(root).join(project)
}

/// `projects/<project>` — the same directory, relative to a weave root.
pub(crate) fn project_rel_dir(project: &str) -> PathBuf {
    Path::new(PROJECTS_DIR).join(project)
}

/// `projects/<project>` with forward slashes, whatever the platform.
///
/// The spelling [`RepoPath`] and the wire formats require; [`project_rel_dir`]
/// renders backslashes on Windows and cannot serve them.
pub(crate) fn project_rel_path(project: &str) -> String {
    format!("{PROJECTS_DIR}/{project}")
}

// ---------------------------------------------------------------------------
// Confusable-sibling lint over the recorded namespace
// ---------------------------------------------------------------------------

/// Two recorded sibling names that differ only by ASCII case.
///
/// A portability lint, and deliberately nothing more: the pair that mints
/// cleanly on a case-sensitive filesystem is the pair that strands the next
/// fetch onto one that folds. Nothing stores this fold, nothing resolves
/// through it, and identity comparison stays byte-exact — the two names remain
/// two distinct identities, which is why this warns rather than refuses.
///
/// Residue, stated: an ASCII fold does not see non-ASCII confusables
/// (`ß`/`SS`, precomposed against decomposed). Those are the same class one
/// size down and are caught only where a real filesystem folds them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConfusableSiblings {
    /// The parent whose entries these are, as the caller names it.
    pub parent: String,
    /// The two names, ordered so the pair reads the same however it was found.
    pub first: String,
    pub second: String,
}

/// Every pair among `siblings` that differs only by ASCII case fold.
///
/// `siblings` are names sharing one parent; the fold is applied to compare
/// them and is discarded here — it is never returned, stored, or used to
/// decide which name anything resolves to.
pub fn confusable_siblings(parent: &str, siblings: &[String]) -> Vec<ConfusableSiblings> {
    let mut by_fold: std::collections::BTreeMap<String, Vec<&String>> =
        std::collections::BTreeMap::new();
    for name in siblings {
        by_fold
            .entry(name.to_ascii_lowercase())
            .or_default()
            .push(name);
    }
    let mut found = Vec::new();
    for group in by_fold.values() {
        let mut distinct: Vec<&String> = group.to_vec();
        distinct.sort();
        distinct.dedup();
        for (i, first) in distinct.iter().enumerate() {
            for second in &distinct[i + 1..] {
                found.push(ConfusableSiblings {
                    parent: parent.to_owned(),
                    first: (*first).clone(),
                    second: (*second).clone(),
                });
            }
        }
    }
    found.sort();
    found
}

/// The operator-facing sentence for one confusable pair.
///
/// One home so the mint-time warning and the doctor finding say the same
/// thing; the two fire at different moments and must not diverge in what they
/// claim.
pub fn confusable_warning(pair: &ConfusableSiblings) -> String {
    format!(
        "`{}` and `{}` under {} differ only by ASCII case. rwv holds them as two \
         distinct identities and will keep doing so — but a filesystem that folds \
         case cannot hold both, so a clone or fetch of this weave onto macOS or \
         Windows collides. Rename one if this weave is meant to travel.",
        pair.first, pair.second, pair.parent
    )
}

/// Warn about any project sibling of `project` that differs from it only by
/// ASCII case.
///
/// Runs at mint on every host, including the case-sensitive ones where both
/// names are perfectly legal — that is the point: the pair is created here and
/// strands somewhere else, so here is where it is cheap to say so.
pub fn warn_confusable_project_siblings(root: &Path, project: &str) {
    let dir = project_dir(root, project);
    let (Some(parent), Some(leaf)) = (dir.parent(), dir.file_name().and_then(|n| n.to_str()))
    else {
        return;
    };
    let names: Vec<String> = match std::fs::read_dir(parent) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => return,
    };
    let label = project_rel_path(project);
    let label = label
        .rsplit_once('/')
        .map_or(PROJECTS_DIR, |(head, _)| head);
    for pair in confusable_siblings(label, &names) {
        if pair.first == leaf || pair.second == leaf {
            eprintln!("warning: {}", confusable_warning(&pair));
        }
    }
}

// ---------------------------------------------------------------------------
// Minting a directory whose final component is an identity
// ---------------------------------------------------------------------------

/// What already occupies a path an identity was about to be minted at.
#[derive(Debug, Clone)]
pub struct DirOccupant {
    requested: PathBuf,
    /// The name the parent directory lists for the occupant, which is not
    /// always the name that was asked for. `None` when the parent cannot be
    /// read or no entry matches — the collision is real either way, and
    /// nothing more precise can be said about it.
    listed: Option<String>,
}

impl DirOccupant {
    /// The clause naming what is there. A caller frames it with what it was
    /// minting and what to do instead.
    ///
    /// It says more than the requested path whenever the filesystem lists the
    /// occupant under a different name, which is the case this exists for: on
    /// a folding filesystem the operator asked for one spelling and something
    /// spelled another way answered.
    pub fn describe(&self) -> String {
        occupant_sentence(&self.requested, self.listed.as_deref())
    }
}

/// The same sentence for a caller that learned of the collision without
/// attempting a create — `git clone` mints some identity directories, and a
/// pre-check is all rwv has there.
pub fn describe_existing(dir: &Path) -> String {
    occupant_sentence(dir, listed_occupant(dir).as_deref())
}

/// One home for the sentence, so every refusal that names an occupant names it
/// the same way.
fn occupant_sentence(requested: &Path, listed: Option<&str>) -> String {
    let asked = requested.file_name().and_then(|n| n.to_str());
    match listed {
        Some(listed) if Some(listed) != asked => format!(
            "{} already exists — the filesystem lists it as `{listed}` and treats the \
             two spellings as one name",
            crate::path_spelling::operator_path(requested)
        ),
        _ => format!(
            "{} already exists",
            crate::path_spelling::operator_path(requested)
        ),
    }
}

/// The occupant's listed name when the filesystem answered a request for `dir`
/// with an entry it spells differently — the case an idempotent-reuse path
/// must not adopt, and the divergence doctor reports in steady state.
///
/// `None` when nothing is there, or when the spelling on disk is the one that
/// was asked for.
pub fn diverged_occupant(dir: &Path) -> Option<String> {
    let asked = dir.file_name().and_then(|n| n.to_str())?;
    let listed = listed_occupant(dir)?;
    (listed != asked).then_some(listed)
}

/// Outcome of [`create_identity_dir`].
#[derive(Debug)]
pub enum MintedDir {
    Created,
    Occupied(DirOccupant),
}

/// The name the parent directory lists for whatever occupies `dir`.
///
/// Not `canonicalize`: on a folding filesystem it echoes back the spelling it
/// was asked with rather than the one on disk, so it can never report the
/// divergence. The parent's own listing carries true spellings, and
/// filesystem identity is what says which entry answered.
fn listed_occupant(dir: &Path) -> Option<String> {
    let parent = dir.parent()?;
    std::fs::read_dir(parent).ok()?.flatten().find_map(|entry| {
        crate::workweave_index::same_directory(&entry.path(), dir)
            .then(|| entry.file_name().to_string_lossy().into_owned())
    })
}

/// Create `dir`, whose final component is an identity, refusing to adopt
/// whatever is already there.
///
/// `create_dir` and never `create_dir_all` at this component: `create_dir_all`
/// reports success for a directory that already exists, so on a filesystem
/// that folds case it silently adopts a directory the operator spelled another
/// way. Parents are created first — only the last component carries identity,
/// and the ones above it are layout.
pub fn create_identity_dir(dir: &Path) -> anyhow::Result<MintedDir> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    match std::fs::create_dir(dir) {
        Ok(()) => Ok(MintedDir::Created),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(MintedDir::Occupied(DirOccupant {
                requested: dir.to_path_buf(),
                listed: listed_occupant(dir),
            }))
        }
        Err(e) => Err(anyhow::Error::from(e))
            .with_context(|| format!("failed to create {}", dir.display())),
    }
}

/// What sits below `projects/` in `path`, or `None` when `path` neither starts
/// with the segment nor reaches past it.
pub(crate) fn strip_projects_prefix(path: &Path) -> Option<&Path> {
    let rest = path.strip_prefix(PROJECTS_DIR).ok()?;
    (!rest.as_os_str().is_empty()).then_some(rest)
}

/// Path components as one `/`-separated name, whatever separator the host
/// writes.
///
/// A [`PathBuf`] built from components joins them with
/// [`std::path::MAIN_SEPARATOR`], which is a backslash on Windows, and
/// rendering the source path's own bytes carries whatever spelling the caller
/// happened to write. Both make the result a property of the host or the
/// caller rather than of the name, and [`ProjectName::new`] rejects the
/// backslash and the empty component that each can produce.
fn slash_separated<'a>(components: impl Iterator<Item = Component<'a>>) -> String {
    components
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The project `dir` holds, named relative to `root`'s projects tree, so
/// `/w` + `/w/projects/chatly/web-app` yields `chatly/web-app`.
///
/// `None` when `dir` is not inside `root`'s projects tree at all.
///
/// The root is a parameter because without it the answer is a guess. A name
/// may contain `/`, and no rule bars the segment `projects` from appearing in
/// one, so a path holds no mark saying which of its `projects` components
/// belongs to the weave and which belongs to the name. Searching for one — the
/// first, the last — answers a different question than the caller asked, and
/// silently, for every name of that shape.
pub(crate) fn project_name_from_dir(root: &Path, dir: &Path) -> Option<String> {
    let rest = dir.strip_prefix(projects_dir(root)).ok()?;
    let name = slash_separated(rest.components());
    (!name.is_empty()).then_some(name)
}

/// Well-known directory names that identify a workspace root.
pub(crate) fn workspace_marker_names() -> Vec<String> {
    let mut names: Vec<String> = builtin_registry_names()
        .iter()
        .map(|n| n.as_str().to_owned())
        .collect();
    names.push(PROJECTS_DIR.to_string());
    names
}

/// Where a manifest member's checkout is for this run, and which of the two
/// kinds it turned out to be.
///
/// A workweave holds worktrees only for the members materialized in it, so
/// this is a per-member fact, not a per-invocation one: a run inside a
/// workweave still resolves to `Primary` for a member that has no slot
/// there.
#[derive(Debug, Clone)]
pub enum MemberCheckout {
    /// The workweave materialized this member; the path is its slot.
    WorkweaveSlot(PathBuf),
    /// No workweave is in play, or this member has no slot in it; the path
    /// is primary's canonical clone.
    Primary(PathBuf),
}

impl MemberCheckout {
    pub fn path(&self) -> &Path {
        match self {
            Self::WorkweaveSlot(p) | Self::Primary(p) => p,
        }
    }

    pub fn is_workweave_slot(&self) -> bool {
        matches!(self, Self::WorkweaveSlot(_))
    }
}

pub fn member_checkout_dir(
    repo_path: &RepoPath,
    primary_root: &Path,
    workweave_dir: Option<&Path>,
) -> MemberCheckout {
    if let Some(wd) = workweave_dir {
        let candidate = wd.join(repo_path.as_path());
        if candidate.exists() {
            return MemberCheckout::WorkweaveSlot(candidate);
        }
    }
    MemberCheckout::Primary(primary_root.join(repo_path.as_path()))
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
    let project_name = strip_projects_prefix(rel)?.components().next()?;
    ProjectName::new(project_name.as_os_str().to_string_lossy().to_string()).ok()
}

// ---------------------------------------------------------------------------
// Weave-root identity: `.rwv-active` XOR `.rwv-workweave`
// ---------------------------------------------------------------------------

/// The pointer file naming the project a **primary** root presents.
pub(crate) const ACTIVE_PROJECT_FILE: &str = ".rwv-active";

/// The marker file naming the project a **workweave** root belongs to.
pub(crate) const WORKWEAVE_MARKER_FILE: &str = ".rwv-workweave";

/// Read the active project from the `.rwv-active` file in the workspace root.
///
/// Returns `None` if the file does not exist or is empty.
///
/// This reads the **pointer specifically**, not "the project this root
/// presents" — [`RootObservation::presented_project`] is that question, and
/// answers it for either kind of root. Reach for this one only where the
/// pointer file itself is the subject (doctor's stale-pointer check,
/// `deactivate`'s removal).
pub fn read_active_project(root: &Path) -> Option<ProjectName> {
    let path = root.join(ACTIVE_PROJECT_FILE);
    let content = std::fs::read_to_string(&path).ok()?;
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }
    ProjectName::new(trimmed).ok()
}

/// Remove the `.rwv-active` pointer at `root`, leaving the root selecting no
/// project.
///
/// A root with no pointer is already in the state this leaves it in, so the
/// absent case succeeds: a caller made to probe first would be spelling the
/// file name at its own site again to do it.
pub fn clear_active_project(root: &Path) -> anyhow::Result<()> {
    let path = root.join(ACTIVE_PROJECT_FILE);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("failed to remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Whether `root` carries the `.rwv-active` pointer, and what it names.
fn observe_active_pointer(root: &Path) -> ActivePointer {
    if root.join(ACTIVE_PROJECT_FILE).exists() {
        ActivePointer::Present(read_active_project(root))
    } else {
        ActivePointer::Absent
    }
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

/// A project directory whose path below `projects/` is not a name
/// [`ProjectName::new`] accepts.
#[derive(Debug)]
pub struct UnnameableProjectDir {
    pub dir: PathBuf,
    pub derived: String,
    pub error: crate::manifest::ProjectNameError,
}

/// What a walk of `projects/` found: the projects, and the two states a
/// directory sitting among them can be in without being one.
#[derive(Debug, Default)]
pub struct ProjectScan {
    /// Every project, sorted by name.
    pub projects: Vec<ProjectName>,
    /// Manifest-carrying directories nothing can address, sorted by path.
    pub unnameable: Vec<UnnameableProjectDir>,
    /// Directories holding no manifest at any depth below them, sorted by
    /// path. Outermost only: a directory listed here has no listed
    /// descendant.
    pub projectless: Vec<PathBuf>,
}

/// Directory entries of `dir`, dot-directories excluded.
///
/// A leading `.` marks host or VCS state — `.git`, an editor's cache — and
/// [`crate::naming::validate_ref_name`] refuses it as a name component anyway,
/// so descending into one can only mint a name no project could carry.
fn undotted_child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// Record every project at or below `dir`, and report whether there was one.
///
/// `false` propagates the barren subtree up to the caller, which is what
/// keeps [`ProjectScan::projectless`] to outermost directories: a level
/// records its barren children only once something below it is a project,
/// and otherwise hands its own emptiness to its parent to record.
fn collect_projects_below(dir: &Path, rel: &Path, scan: &mut ProjectScan) -> bool {
    if dir.join(Manifest::FILE_NAME).is_file() {
        let derived = slash_separated(rel.components());
        match ProjectName::new(derived.clone()) {
            Ok(name) => scan.projects.push(name),
            Err(error) => scan.unnameable.push(UnnameableProjectDir {
                dir: dir.to_path_buf(),
                derived,
                error,
            }),
        }
        return true;
    }

    let mut barren = Vec::new();
    let mut holds_project = false;
    for child in undotted_child_dirs(dir) {
        let Some(segment) = child.file_name() else {
            continue;
        };
        if collect_projects_below(&child, &rel.join(segment), scan) {
            holds_project = true;
        } else {
            barren.push(child);
        }
    }
    if holds_project {
        scan.projectless.append(&mut barren);
    }
    holds_project
}

/// Walk `projects/` under `root` for every project it holds.
///
/// A project is a directory under `projects/` containing a
/// [`Manifest::FILE_NAME`], named by its path relative to `projects/` — the
/// rule [`crate::manifest::Project::from_dir`] already loads by, at arbitrary
/// depth. Descent stops at the first manifest, so a manifest below a project
/// is a file in that project's working tree rather than a second project, and
/// `projects/a/rwv.toml` beside `projects/a/b/rwv.toml` has one answer.
///
/// An unlistable `projects/` reads as empty here; `rwv doctor` probes that
/// directory separately so the two cannot be confused downstream.
pub fn scan_projects(root: &Path) -> ProjectScan {
    let mut scan = ProjectScan::default();
    let mut barren = Vec::new();
    for child in undotted_child_dirs(&projects_dir(root)) {
        let Some(segment) = child.file_name() else {
            continue;
        };
        if !collect_projects_below(&child, Path::new(segment), &mut scan) {
            barren.push(child);
        }
    }
    scan.projectless.append(&mut barren);
    scan.projects.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    scan.unnameable.sort_by(|a, b| a.dir.cmp(&b.dir));
    scan.projectless.sort();
    scan
}

/// Every project under `projects/` relative to `root`, sorted by name.
pub fn discover_projects(root: &Path) -> Vec<ProjectName> {
    scan_projects(root).projects
}

/// The project a `projects/<name>/` mint would land inside, if any.
///
/// The mint's half of the rule [`scan_projects`] enumerates by: descent stops
/// at the first manifest, so a directory below a project is that project's
/// content and can never be a project of its own. Minting one there would
/// write a manifest the enumeration is guaranteed never to reach.
pub fn enclosing_project(root: &Path, name: &str) -> Option<String> {
    let mut dir = projects_dir(root);
    let mut ancestors = Vec::new();
    for segment in name.split('/') {
        ancestors.push(dir.clone());
        dir.push(segment);
    }
    ancestors
        .into_iter()
        .skip(1)
        .find(|d| d.join(Manifest::FILE_NAME).is_file())
        .and_then(|d| project_name_from_dir(root, &d))
}

/// The project names an error offers the reader as a menu.
fn spelled_project_names(root: &Path) -> Vec<String> {
    discover_projects(root)
        .into_iter()
        .map(|p| p.as_str().to_owned())
        .collect()
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
    container_kind: ContainerKind,
    repos_on_disk: Vec<RepoPath>,
    project_paths: Vec<String>,
}

impl WorkspaceSession {
    /// Build a `WorkspaceSession` by running the standard scan triad:
    /// `builtin_registries()` → `scan_repos_on_disk()` → `discover_projects()`.
    pub fn new(root: &Path) -> Self {
        let registries = builtin_registries();
        let vcs = crate::vcs::probe_vcs();
        let repos_on_disk = scan_repos_on_disk(root, &registries, vcs.as_ref());
        let project_paths = discover_projects(root)
            .into_iter()
            .map(|p| p.as_str().to_owned())
            .collect();
        let container_kind =
            observe_root(root).map_or(ContainerKind::Primary, |observed| observed.container_kind());
        Self {
            root: root.to_path_buf(),
            container_kind,
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
    /// `rwv.toml` (typically `manifest.workweave.as_ref()`). It is threaded
    /// through to integrations so they can detect cross-section collisions
    /// such as a name claimed by both `static-files.files` and
    /// `workweave.link`.
    ///
    /// `container_kind` is derived for the same reason `output_dir` is: it
    /// qualifies `workspace_root`, the integration that reads it reads it
    /// against that same directory, and a caller free to state it separately
    /// is free to describe one root while naming another. The session settles
    /// it once from its own root ([`RootObservation::container_kind`]), so the
    /// pair cannot disagree — a caller holding a resolved [`Checkout`] and a
    /// root from somewhere else has nowhere left to say the mismatch.
    pub fn context_base<'a>(
        &'a self,
        project: &'a ProjectName,
        detection_cache: &'a std::collections::HashMap<String, Vec<String>>,
        workweave: Option<&'a crate::manifest::WorkweaveConfig>,
    ) -> IntegrationContextBase<'a> {
        IntegrationContextBase {
            output_dir: project_dir(&self.root, project.as_str()),
            workspace_root: &self.root,
            container_kind: self.container_kind,
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
/// - If [`WorkspaceContext::resolve_unbound`] succeeds, returns `Ok(())` — we
///   are inside an existing workspace. The question is whether one is here at
///   all, so no project is bound and none is needed.
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
    match WorkspaceContext::resolve_unbound(cwd) {
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

/// Where the containment walk stopped, and the `cwd` it started from.
///
/// The two together are everything a [`WorkspaceContext`] needs beyond the
/// identity the root claims, which is why the walk and the projection of an
/// identity onto a context are separate: the entry points differ only in how
/// much of [`RootObservation`] they are willing to act on.
struct RootSite {
    cwd: PathBuf,
    dir: PathBuf,
}

/// Walk up from `cwd` to the nearest directory carrying weave-root identity
/// evidence, and report what that directory claims.
///
/// The walk stops at the first ancestor [`observe_root`] answers for,
/// including the answers no verb may act on: a directory claiming an identity
/// it cannot witness is not a directory to walk past, because every workweave
/// is shaped exactly like a primary and walking past one hands the tree a
/// primary's authority on the strength of its own broken claim.
fn walk_to_weave_root(cwd: &Path) -> anyhow::Result<(RootSite, RootObservation)> {
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
    // Linux), `std::env::home_dir()` returns the raw env value while `cwd`
    // above has already been canonicalized.  Without canonicalization the
    // `starts_with` test always returns false (the paths are spelled
    // differently) and the ceiling silently never fires.
    let home_dir = std::env::home_dir().and_then(|h| h.canonicalize().ok());

    let mut current = cwd.as_path();
    loop {
        if let Some(observation) = observe_root(current) {
            let site = RootSite {
                cwd: cwd.clone(),
                dir: current.to_path_buf(),
            };
            return Ok((site, observation));
        }

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

/// How one resolution learns which project it is for.
///
/// The distinction the resolve API is built on: an *ambient* resolution may
/// fall through to the root's `.rwv-active`, a *bound* one may not. Both were
/// once spelled `Option<ProjectName>`, where the ambient reading was `None` —
/// correct at the dispatch boundary and a silent wrong binding everywhere
/// else, byte-identical at both.
enum ProjectBinding {
    /// The operator's `--project`, step 1 of the resolution chain. Outranks a
    /// workweave marker by that chain's design: it is the operator speaking
    /// about this invocation, not a second record of the same fact.
    Flag(ProjectName),
    /// A binding the caller already holds — carried by value through an op, or
    /// read back off its record. Cross-checked against a marker rather than
    /// outranking it.
    Bound(ProjectName),
    /// Nothing supplied. The chain falls through to the root's own identity
    /// file; this is the only binding under which the pointer decides.
    Ambient,
    /// No project, and no fall-through. A primary root resolves without one.
    Unbound,
}

impl ProjectBinding {
    fn for_invocation(project_flag: Option<ProjectName>) -> Self {
        match project_flag {
            Some(p) => Self::Flag(p),
            None => Self::Ambient,
        }
    }
}

impl RootSite {
    fn by_identity(
        self,
        identity: WeaveRootIdentity,
        binding: ProjectBinding,
    ) -> anyhow::Result<WorkspaceContext> {
        match identity {
            WeaveRootIdentity::Workweave(identity) => {
                self.by_marker(identity.into_marker(), binding)
            }
            WeaveRootIdentity::Primary(identity) => Ok(self.by_pointer(identity, binding)),
        }
    }

    /// The marker names both the primary workspace and the project, so
    /// [`ProjectProvenance::Marker`] is reachable from here and nowhere else.
    fn by_marker(
        self,
        marker: WorkweaveMarker,
        binding: ProjectBinding,
    ) -> anyhow::Result<WorkspaceContext> {
        let name = match crate::workweave::workweave_name_for_path(
            &marker.primary,
            &marker.project,
            &self.dir,
        ) {
            Ok(Some(recorded)) => WorkweaveNameRecord::Recorded(recorded),
            Ok(None) => WorkweaveNameRecord::Unregistered,
            // An index that cannot be read names nothing, and doctor reports
            // the unreadable file itself. Resolution proceeds so that the
            // diagnostic verbs which deliver that report can run at all.
            Err(_) => WorkweaveNameRecord::Unregistered,
        };
        let (project, provenance) = match binding {
            ProjectBinding::Flag(p) => (p, ProjectProvenance::Flag),
            ProjectBinding::Bound(p) => {
                if p != marker.project {
                    anyhow::bail!(
                        "workspace {} belongs to project `{}` per its `.rwv-workweave` \
                         marker, but the operation asking for it is bound to `{}`. Two \
                         structural records disagree; nothing here can pick between \
                         them. Run the operation against a workspace of `{}`, or check \
                         whether the marker is the file that is wrong.",
                        self.dir.display(),
                        marker.project.as_str(),
                        p.as_str(),
                        p.as_str(),
                    );
                }
                (p, ProjectProvenance::Marker)
            }
            ProjectBinding::Ambient | ProjectBinding::Unbound => {
                (marker.project, ProjectProvenance::Marker)
            }
        };
        Ok(WorkspaceContext {
            cwd_project_hint: detect_project(&self.cwd, &marker.primary),
            primary_root: marker.primary,
            primary_identity: None,
            checkout: Checkout::Workweave {
                name,
                dir: self.dir,
                project,
                parent: marker.parent.into_path_buf(),
            },
            project_provenance: Some(provenance),
        })
    }

    /// The pointer is the ambient default, so
    /// [`ProjectProvenance::ActiveFile`] is reachable from here and nowhere
    /// else. Neither pointer nor override leaves both unset — the caller uses
    /// [`WorkspaceContext::require_active_project`] to surface the corrective
    /// error.
    fn by_pointer(self, identity: PrimaryIdentity, binding: ProjectBinding) -> WorkspaceContext {
        let (project, provenance) = match binding {
            ProjectBinding::Flag(p) => (Some(p), Some(ProjectProvenance::Flag)),
            ProjectBinding::Bound(p) => (Some(p), Some(ProjectProvenance::Bound)),
            ProjectBinding::Ambient => match identity.selection.clone() {
                Some(p) => (Some(p), Some(ProjectProvenance::ActiveFile)),
                None => (None, None),
            },
            ProjectBinding::Unbound => (None, None),
        };
        WorkspaceContext {
            cwd_project_hint: detect_project(&self.cwd, &self.dir),
            primary_root: self.dir,
            primary_identity: Some(identity),
            checkout: Checkout::Primary { project },
            project_provenance: provenance,
        }
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
    ///      spelling follows the identity [`observe_root`] read at the root
    ///      the walk landed on:
    ///      - [`WeaveRootIdentity::Workweave`] → `.rwv-workweave` marker,
    ///        structural: the workweave directory names its project.
    ///        Provenance = [`ProjectProvenance::Marker`].
    ///      - [`WeaveRootIdentity::Primary`] → `.rwv-active` pointer, the
    ///        ambient default.
    ///        Provenance = [`ProjectProvenance::ActiveFile`].
    ///
    /// Step 3 is one tier and not two because the identity is one
    /// observation: [`RootObservation::require_exclusive`] refuses a root
    /// carrying both files, so no invocation reaches a state where both are
    /// readable and there is no precedence between them to get wrong.
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
    pub fn resolve_invocation(
        cwd: &Path,
        project_flag: Option<ProjectName>,
    ) -> anyhow::Result<Self> {
        let (site, observation) = walk_to_weave_root(cwd)?;
        site.by_identity(
            observation.require_exclusive()?,
            ProjectBinding::for_invocation(project_flag),
        )
    }

    /// [`Self::resolve_invocation`] for `doctor` and `status`, whose subject is
    /// the root's identity rather than the work done through it.
    ///
    /// A root carrying both identity files stops every other verb. Refusing
    /// these two as well would withhold the inspection that names the state
    /// and the repair that clears it, and would demand the operator run them
    /// from somewhere else — which a copied tree, whose primary contains no
    /// record of it, may not have. They proceed by marker, which is what
    /// resolution reads at an undisputed workweave root anyway: the pointer
    /// decides nothing at either kind of root, so tolerating one guesses
    /// nothing. Every other unusable root is refused here exactly as in
    /// [`Self::resolve_invocation`].
    pub fn resolve_invocation_tolerating_disputed_root(
        cwd: &Path,
        project_flag: Option<ProjectName>,
    ) -> anyhow::Result<Self> {
        let (site, observation) = walk_to_weave_root(cwd)?;
        let binding = ProjectBinding::for_invocation(project_flag);
        match observation {
            RootObservation::Disputed { marker, .. } => site.by_marker(marker, binding),
            settled => site.by_identity(settled.require_exclusive()?, binding),
        }
    }

    /// Resolve `dir` as the workspace an operation **already bound to
    /// `project`** sees it.
    ///
    /// This is every workspace resolution that is not the invocation: the
    /// source or target of a sync, the workspace an op record names, the one an
    /// abort restores. The binding is a required argument because "resolve it
    /// under whatever that workspace points at" is not a question any of them
    /// is asking — a workspace's `.rwv-active` is ambient state its owner may
    /// retarget at any moment, including while an op sits parked, and an op
    /// that re-read it would operate on repos it never touched.
    ///
    /// Refuses when the workspace's own structural record contradicts the
    /// binding: a workweave marker naming another project is not a preference
    /// to resolve but two records disagreeing, and when `projects/<project>` is
    /// absent the refusal names what it could not find.
    pub fn resolve_for_project(dir: &Path, project: &ProjectName) -> anyhow::Result<Self> {
        let (site, observation) = walk_to_weave_root(dir)?;
        let ctx = site.by_identity(
            observation.require_exclusive()?,
            ProjectBinding::Bound(project.clone()),
        )?;
        ctx.require_bound_project_on_disk(project)?;
        Ok(ctx)
    }

    /// Resolve `dir` as a location only — no project, and the pointer
    /// unconsulted.
    ///
    /// For the callers whose subject is where a workspace sits rather than what
    /// it holds. A primary root resolves with no active project at all; a
    /// workweave still reports the project its marker names, because that is
    /// the directory's own identity rather than anything ambient.
    pub fn resolve_unbound(dir: &Path) -> anyhow::Result<Self> {
        let (site, observation) = walk_to_weave_root(dir)?;
        site.by_identity(observation.require_exclusive()?, ProjectBinding::Unbound)
    }

    /// The bound project's directory must exist under the primary this
    /// workspace belongs to. Separate from
    /// [`Self::require_active_project_on_disk`] because that one's message
    /// sends the reader to `.rwv-active`, which is exactly what a bound
    /// resolution did not read.
    fn require_bound_project_on_disk(&self, project: &ProjectName) -> anyhow::Result<()> {
        if project_dir(self.primary_path(), project.as_str()).is_dir() {
            return Ok(());
        }
        let existing = spelled_project_names(self.primary_path());
        let hint = if existing.is_empty() {
            String::new()
        } else {
            format!(" Existing projects: {}.", existing.join(", "))
        };
        anyhow::bail!(
            "workspace {} does not hold project `{}`: `projects/{}/` does not exist there.{}",
            self.primary_path().display(),
            project.as_str(),
            project.as_str(),
            hint,
        )
    }

    /// The project this already-resolved context is bound to.
    ///
    /// The one place op code learns its binding: the chain ran at the dispatch
    /// boundary and this reports what it chose, so nothing downstream resolves
    /// a project for itself. A primary-rooted context also has its project
    /// directory verified, so a stale pointer fails here rather than as a
    /// confusing git error further down.
    pub fn require_bound_project(&self) -> anyhow::Result<&ProjectName> {
        match &self.checkout {
            Checkout::Primary { .. } => self.require_active_project_on_disk(),
            Checkout::Workweave { project, .. } => Ok(project),
        }
    }

    /// The project name inferred from CWD's location under
    /// `{root}/projects/{name}/...`, or `None` when CWD is not inside any
    /// project directory.
    ///
    /// Use to (a) surface a "you are in projects/\<X\>/ but \<Y\> is active"
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
                crate::path_spelling::operator_path(&self.primary_root),
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
        let existing = spelled_project_names(self.primary_path());
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
    ///   to [`Self::require_active_project`] for the standard "no active project"
    ///   error message.
    /// - Active project named **and** directory exists: returns `Ok(name)`.
    /// - Active project named **but** directory is missing on disk (dangling
    ///   pointer): returns a clear, actionable error.
    ///
    /// All action verbs (`lock`, `add`, `remove`, `sync`, `sync-to`, `push`,
    /// `fetch`, `update`, `status`) must call this instead of
    /// [`Self::require_active_project`] so that a stale `.rwv-active` file does not
    /// silently proceed into confusing downstream errors.
    pub fn require_active_project_on_disk(&self) -> anyhow::Result<&ProjectName> {
        let name = self.require_active_project()?;

        let project_dir = project_dir(self.primary_path(), name.as_str());
        if project_dir.is_dir() {
            return Ok(name);
        }

        // Dangling pointer: named but missing. Build the actionable error.
        let existing = spelled_project_names(self.primary_path());
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
    /// and the files `static-files` surfaces to the root. These all live
    /// under the primary regardless of where CWD currently is.
    pub fn primary_path(&self) -> &Path {
        &self.primary_root
    }

    /// The witness that permits project selection, or `None` when the walk
    /// landed on a workweave root.
    ///
    /// `None` is the whole of "activate has no meaning here": the verb that
    /// writes the pointer asks for the witness, and a workweave answers by
    /// not having one.
    pub fn primary_identity(&self) -> Option<&PrimaryIdentity> {
        self.primary_identity.as_ref()
    }

    /// The directory CWD is actually in: the primary path when in a weave,
    /// the workweave directory when in a workweave.
    ///
    /// Use this for per-workspace state — project worktrees and their
    /// `rwv.lock` / `rwv.toml`, the repo worktrees the operator is working
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
                lines.push(format!(
                    "Weave: {}",
                    crate::path_spelling::operator_path(&self.primary_root)
                ));
                if let Some(p) = &active {
                    lines.push(format!("Project: {}", p.as_str()));
                    let manifest_path =
                        project_dir(&self.primary_root, p.as_str()).join(Manifest::FILE_NAME);
                    if let Ok(manifest) = Manifest::from_path(&manifest_path) {
                        lines.push(format!("Repos: {}", manifest.len()));
                    }
                }
            }
            Checkout::Workweave { name, dir, .. } => {
                lines.push(format!(
                    "Workweave: {}",
                    crate::path_spelling::operator_path(dir)
                ));
                lines.push(format!(
                    "Weave: {}",
                    crate::path_spelling::operator_path(&self.primary_root)
                ));
                if let Some(p) = &active {
                    lines.push(format!("Project: {}", p.as_str()));
                    let manifest_path =
                        project_dir(&self.primary_root, p.as_str()).join(Manifest::FILE_NAME);
                    if let Ok(manifest) = Manifest::from_path(&manifest_path) {
                        lines.push(format!("Repos: {}", manifest.len()));
                    }
                }
                if name.recorded().is_none() {
                    lines.push(UNREGISTERED_WORKWEAVE_NOTICE.to_owned());
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

        let project_names = spelled_project_names(&self.primary_root);
        if !project_names.is_empty() {
            lines.push(format!("Projects: {}", project_names.join(", ")));
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
    /// (abs path), `workweave` is the full `<project>--<name>` identity the
    /// registry records for this workweave, and `project` is the resolved
    /// project name. `workweave` is absent at the primary and absent for a
    /// workweave no registry entry names, since neither has such an identity
    /// to project. Results only; resolution
    /// provenance (which chain step chose the project) is human-surface only
    /// (stderr target line) and is deliberately excluded from this struct.
    ///
    /// [`crate::plugins::envelope_vars`] is a second serialization of this
    /// same projection, mapping these fields to the `RWV_*` env vars a
    /// spawned plugin reads. It calls this method; the values are never
    /// independently computed.
    pub fn resolution(&self) -> Option<Resolution> {
        let project = self.active_project()?;
        let workspace = crate::path_spelling::wire_path(&self.primary_root);
        let (workweave, workweave_unregistered) = match &self.checkout {
            Checkout::Primary { .. } => (None, false),
            Checkout::Workweave { name, project, .. } => match name.recorded() {
                Some(name) => (Some(weave_dir_name(project, name)), false),
                None => (None, true),
            },
        };
        Some(Resolution {
            workspace,
            workweave,
            workweave_unregistered,
            project: project.as_str().to_owned(),
        })
    }
}

/// Resolved workspace coordinates for `--json` output and the plugin env-var
/// envelope.
///
/// Carries `workspace` (primary root abs path), `workweave` (the
/// `<project>--<name>` identity the registry records), `project` (resolved
/// project name), and `workweave_unregistered`. No `kind` or `location`
/// field: the checkout is one of three states, and two of them are already
/// carried by `workweave`'s presence.
///
/// The third state needs a field of its own. A workweave whose directory no
/// registry entry names has no identity, so `workweave` is absent for it —
/// and absent is what the primary looks like. Without
/// `workweave_unregistered` the two serialize identically, and a consumer
/// reading the documented meaning of that absence is told, positively, that
/// it is at the primary checkout.
///
/// Results only — provenance (which chain step resolved the project, which
/// flag addressed the workspace) is deliberately excluded: anything in
/// default `--json` output becomes depended on, and the assertion use case
/// needs the result, not the mechanism. Provenance appears only in the
/// human-facing "target:" line printed to stderr.
///
/// Isomorphic to the plugin env-var envelope
/// (`RWV_WORKSPACE`/`RWV_WORKWEAVE`/`RWV_WORKWEAVE_UNREGISTERED`/`RWV_PROJECT`):
/// both surfaces are pure projections of [`WorkspaceContext::resolution`],
/// never independently computed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Resolution {
    /// Primary workspace root, in the wire spelling.
    ///
    /// A `String` and not a `PathBuf` so the spelling is decided at
    /// construction rather than by serde: this is a published absolute path
    /// and it owes programs the composable form, which
    /// [`crate::path_spelling::wire_path`] is the only producer of.
    pub workspace: String,
    /// Workweave identity (`<project>--<name>`), as the primary-side registry
    /// records it.
    ///
    /// Absent at the primary, and absent for a workweave whose directory no
    /// registry entry names — identity is by record, so an unregistered
    /// workweave has no identity to report and rwv will not spell one from the
    /// directory name. Those two absences are told apart by
    /// `workweave_unregistered`, not by this field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workweave: Option<String>,
    /// `true` when the invocation resolved into a workweave whose directory no
    /// registry entry names, so `workweave` above is absent for a reason that
    /// is not "this is the primary".
    ///
    /// Serialized only in that state, so the primary and a registered
    /// workweave emit exactly the bytes they emitted before this field
    /// existed. `rwv doctor --fix` registers such a directory, after which
    /// this is absent and `workweave` carries the identity.
    #[serde(default, skip_serializing_if = "is_false")]
    pub workweave_unregistered: bool,
    /// Resolved project name.
    pub project: String,
}

/// `skip_serializing_if` predicate for a flag that is only news when set.
fn is_false(b: &bool) -> bool {
    !*b
}

/// A condition worth an operator's attention that a verb's `--json` output
/// reports alongside its result, without being a failure of the verb itself.
///
/// Every field is something a consumer branches on directly — `kind` a
/// closed enum, `remedy` a verb string runnable in the checkout where the
/// advisory appears, `inputs` the workspace-relative paths that raised it.
/// None of the three is a sentence a consumer would have to parse to act on.
///
/// Shared across verbs so more than one `--json` surface can emit the same
/// vocabulary: a sync-time note and a doctor-time standing finding both fit
/// this shape without either owning it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct AdvisoryOutput {
    pub kind: AdvisoryKindOutput,
    /// The verb that resolves the advisory (e.g. `"rwv materialize"`).
    pub remedy: String,
    /// Workspace-relative paths whose state raised this advisory.
    pub inputs: Vec<String>,
}

/// Closed vocabulary for [`AdvisoryOutput::kind`]. Adding a member is
/// additive — existing consumers keep matching the members they know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdvisoryKindOutput {
    /// Generated ecosystem state may no longer agree with the inputs it was
    /// derived from.
    DerivedStateStale,
}

// The flat-address grammar and the name types it constrains live in
// `crate::naming`, which nothing in the crate sits below. These stay
// reachable here because every caller of a weave directory name is a caller
// about workspace layout.
pub use crate::naming::{flat_project_segment, weave_dir_name, workweave_name_in};

// ---------------------------------------------------------------------------
// WorkweaveMarker — `.rwv-workweave` marker file
// ---------------------------------------------------------------------------

/// A path held in the one spelling every equivalent spelling resolves to, so
/// that two values denoting the same directory compare equal.
///
/// A path that is not on disk cannot be resolved and is kept verbatim — a
/// marker naming another machine's tree still round-trips through this type.
/// Equality between two such values is therefore textual, which is the most
/// any comparison can offer for a path the filesystem cannot speak about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CanonicalPath(PathBuf);

impl CanonicalPath {
    pub fn of(path: &Path) -> Self {
        CanonicalPath(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl<'de> Deserialize<'de> for CanonicalPath {
    /// Deriving this instead would let a hand-edited or migrated file put a
    /// value in this type that its name promises is impossible, and every
    /// comparison downstream would then be textual against an arbitrary
    /// spelling.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(CanonicalPath::of(&PathBuf::deserialize(d)?))
    }
}

/// Metadata written to `.rwv-workweave` in a workweave root.
///
/// Records the relationship to the primary workspace and the workspace this
/// workweave was forked from.
///
/// `parent` is the workspace the workweave was created from: `primary` when
/// created from the primary, the parent workweave's path when created from
/// inside another workweave. Workweaves form a tree; `parent` is the edge a
/// bare `rwv sync-to` lands along, one hop toward the primary.
///
/// All three fields (`primary`, `project`, `parent`) are required. Written
/// and read as JSON; a YAML marker, or one written before `parent` was
/// introduced, is a legacy marker and must be migrated with
/// `rwv doctor --fix` before the workweave can be used.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkweaveMarker {
    primary: PathBuf,
    project: ProjectName,
    parent: CanonicalPath,
}

impl WorkweaveMarker {
    /// Where this type's file sits inside `dir`.
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join(WORKWEAVE_MARKER_FILE)
    }

    /// Build a marker for a workweave forked from `parent_source`.
    ///
    /// `primary` is recorded as given. Resolving it here would change the
    /// bytes this type writes to `.rwv-workweave` for a non-canonical input,
    /// which is an on-disk format change; [`Self::names_primary`] carries the
    /// resolution instead, at every comparison.
    pub fn new(primary: PathBuf, project: ProjectName, parent_source: &Path) -> Self {
        WorkweaveMarker {
            primary,
            project,
            parent: CanonicalPath::of(parent_source),
        }
    }

    pub fn primary(&self) -> &Path {
        &self.primary
    }

    pub fn primary_resolved(&self) -> CanonicalPath {
        CanonicalPath::of(&self.primary)
    }

    /// Whether `candidate` is the primary workspace this marker names.
    pub fn names_primary(&self, candidate: &Path) -> bool {
        self.primary_resolved() == CanonicalPath::of(candidate)
    }

    pub fn project(&self) -> &ProjectName {
        &self.project
    }

    pub fn parent(&self) -> &CanonicalPath {
        &self.parent
    }

    pub fn repoint_parent(&mut self, parent_source: &Path) {
        self.parent = CanonicalPath::of(parent_source);
    }

    /// Read the marker file from `dir`.
    ///
    /// Returns `Ok(None)` if the marker file is absent.
    ///
    /// Returns `Err` if the file is present but is not a valid JSON marker.
    /// A YAML-format marker with a `primary:` to migrate from (written before
    /// markers were JSON, possibly also missing `parent:`) directs the
    /// operator to `rwv doctor --fix`; one with no `primary:`, or anything
    /// else unreadable, names what to write by hand instead — nothing
    /// migrates a marker with no `primary:` of its own. All three fields
    /// (`primary`, `project`, `parent`) must be present; there is no silent
    /// backfill.
    pub fn read(dir: &Path) -> anyhow::Result<Option<Self>> {
        let path = Self::path_in(dir);
        match observe_marker(&path) {
            MarkerPresence::Absent => Ok(None),
            MarkerPresence::Usable(marker) => Ok(Some(marker)),
            MarkerPresence::Defective { defect, .. } => Err(anyhow::anyhow!(defect.refusal(&path))),
        }
    }

    pub fn write(&self, dir: &Path) -> anyhow::Result<()> {
        let path = Self::path_in(dir);
        let content =
            serde_json::to_string_pretty(self).context("failed to serialize .rwv-workweave")?;
        crate::state_file::StateFile::WorkweaveMarker
            .publish_in(dir, format!("{content}\n").as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(())
    }

    /// Migrate a legacy marker in `dir` — one `observe_marker` classifies
    /// [`MarkerDefect::Legacy`], i.e. YAML with a `primary:` to migrate from
    /// — by backfilling `parent` to `primary` where `parent:` is missing,
    /// then rewriting through [`Self::write`] (which produces JSON).
    ///
    /// Returns `Ok(false)` if `dir` already holds a JSON marker — idempotent,
    /// so callers can retry across a race without double-writing. `Err` on
    /// I/O failure or if the file is not readable as a legacy (YAML) marker
    /// with at least the `primary:` and `project:` fields it requires.
    ///
    /// MIGRATORY arm: repairs markers written by rwv <= v0.16.0 (YAML
    /// shape) and < v0.10.0 (missing `parent:`). Removable once every
    /// owned weave's health floor records a clean doctor at >= v0.18 (see
    /// [`crate::health_floor`]).
    pub fn migrate_legacy(dir: &Path) -> anyhow::Result<bool> {
        let path = Self::path_in(dir);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {} for --fix", path.display()))?;
        if serde_json::from_str::<Self>(&content).is_ok() {
            return Ok(false);
        }
        let docs = YamlOwned::load_from_str(&content)
            .with_context(|| format!("failed to parse .rwv-workweave at {}", path.display()))?;
        let raw = docs
            .first()
            .ok_or_else(|| anyhow::anyhow!("{} is not a YAML mapping", path.display()))?;
        let field = |key: &str| raw.as_mapping_get(key).and_then(|v| v.as_str());
        let primary = field("primary").ok_or_else(|| {
            anyhow::anyhow!(
                "{} is missing the required `primary:` field",
                path.display()
            )
        })?;
        let project = field("project").ok_or_else(|| {
            anyhow::anyhow!(
                "{} is missing the required `project:` field",
                path.display()
            )
        })?;
        let parent = field("parent").unwrap_or(primary);
        let marker = Self {
            primary: PathBuf::from(primary),
            project: ProjectName::new(project)
                .with_context(|| format!("{} has an invalid `project:` value", path.display()))?,
            parent: CanonicalPath::of(Path::new(parent)),
        };
        marker.write(dir)?;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Root identity — what one directory claims to be
// ---------------------------------------------------------------------------

/// Why a `.rwv-workweave` file cannot witness the identity it claims.
///
/// `Legacy` covers every YAML marker [`WorkweaveMarker::migrate_legacy`] can
/// repair: `primary:` present, with or without the `parent:` field that
/// became required before the format changed (it backfills from `primary`).
/// A YAML marker with no `primary:` of its own has nothing to backfill from,
/// so it is `Unreadable` instead — `migrate_legacy` requires the field
/// unconditionally, and a defect naming a repair nothing performs is worse
/// than one that names none.
///
/// `Serialize`/`JsonSchema` so `check::WeaveRootIdentityConflictKind` can
/// carry a defect straight into a doctor finding's wire shape — the same
/// value `require_exclusive` refuses on, not a re-description of it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MarkerDefect {
    DanglingPrimary {
        #[serde(serialize_with = "crate::path_spelling::serialize_wire_path")]
        primary: PathBuf,
    },
    Legacy,
    Unreadable {
        detail: String,
    },
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
                "{} is a legacy workweave marker (YAML format, or missing the required \
                 `parent:` field). Run `rwv doctor --fix` from the primary weave, not \
                 from inside this workweave: resolving the marker precedes the repair, \
                 so a self-invoked `--fix` hits this same refusal and changes nothing. \
                 The file is still readable YAML, so its own `primary:` names the weave \
                 to run from — `rwv doctor --fix -C <that path>` needs no `cd`.",
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
        primary_hint: Option<PathBuf>,
    },
}

/// Parse `.rwv-workweave` once, for both the readers that need a marker and
/// the readers that must classify a broken one.
///
/// Tries JSON first — the format every current write produces — and falls
/// back to YAML only to recognize a legacy marker precisely enough to refuse
/// it and let [`WorkweaveMarker::migrate_legacy`] rewrite it; a YAML parse
/// failure past that point means the file witnesses neither format.
///
/// `project_hint` and `primary_hint` are carried out of the defective arms
/// because a root whose marker no verb may act on still presents a project
/// (for surfacing) and, if the defect is [`MarkerDefect::Legacy`], a primary
/// (for `rwv doctor`'s legacy-marker report) — and this is the last point
/// anything can read either from the raw YAML.
fn observe_marker(marker_path: &Path) -> MarkerPresence {
    if !marker_path.exists() {
        return MarkerPresence::Absent;
    }
    let unreadable =
        |detail: String, project_hint: Option<ProjectName>, primary_hint: Option<PathBuf>| {
            MarkerPresence::Defective {
                defect: MarkerDefect::Unreadable { detail },
                project_hint,
                primary_hint,
            }
        };
    let content = match std::fs::read_to_string(marker_path) {
        Ok(content) => content,
        Err(e) => {
            return unreadable(
                format!("failed to read {}: {e}", marker_path.display()),
                None,
                None,
            );
        }
    };
    if let Ok(marker) = serde_json::from_str::<WorkweaveMarker>(&content) {
        return MarkerPresence::Usable(marker);
    }
    let docs = match YamlOwned::load_from_str(&content) {
        Ok(docs) => docs,
        Err(e) => {
            return unreadable(
                format!(
                    "failed to parse .rwv-workweave at {}: {e}",
                    marker_path.display()
                ),
                None,
                None,
            );
        }
    };
    let Some(raw) = docs.first().filter(|raw| raw.as_mapping().is_some()) else {
        return unreadable(
            format!("{} is not a JSON or YAML mapping", marker_path.display()),
            None,
            None,
        );
    };
    let project_hint = raw
        .as_mapping_get("project")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|project| !project.is_empty())
        .and_then(|project| ProjectName::new(project).ok());
    let Some(primary_hint) = raw
        .as_mapping_get("primary")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
    else {
        return unreadable(
            format!(
                "{} is a legacy (YAML) workweave marker with no `primary:` field, so it \
                 cannot be migrated automatically. Write it by hand as JSON with the three \
                 required fields: `primary`, `project`, and `parent`",
                marker_path.display()
            ),
            project_hint,
            None,
        );
    };
    MarkerPresence::Defective {
        defect: MarkerDefect::Legacy,
        project_hint,
        primary_hint: Some(primary_hint),
    }
}

/// The `primary:` value of a legacy `.rwv-workweave` marker at `dir` — what
/// `rwv doctor --fix` would migrate it to, for `rwv doctor`'s legacy-marker
/// scan, which reports on the marker rather than constructing a
/// [`WorkweaveMarker`] from it.
///
/// `None` covers every case but the one it names: no marker at `dir`, a
/// marker that reads as usable, or one broken some other way
/// ([`MarkerDefect::Unreadable`], [`MarkerDefect::DanglingPrimary`]) —
/// [`MarkerDefect::Legacy`] always carries a `primary_hint`, since a legacy
/// marker with no `primary:` of its own classifies `Unreadable` instead.
pub(crate) fn legacy_marker_primary(dir: &Path) -> Option<PathBuf> {
    match observe_marker(&WorkweaveMarker::path_in(dir)) {
        MarkerPresence::Defective {
            defect: MarkerDefect::Legacy,
            primary_hint,
            ..
        } => primary_hint,
        _ => None,
    }
}

/// The truthful "cannot be migrated" detail for a `.rwv-workweave` marker at
/// `dir` — for a doctor scan reporting on a broken marker
/// [`WorkweaveMarker::migrate_legacy`] has no way to repair.
///
/// `None` covers no marker, a usable marker, and [`MarkerDefect::Legacy`]:
/// that shape is `migrate_legacy`'s to fix, so it is
/// [`legacy_marker_primary`]'s to report, not this one's.
pub(crate) fn unmigratable_marker_detail(dir: &Path) -> Option<String> {
    match observe_marker(&WorkweaveMarker::path_in(dir)) {
        MarkerPresence::Defective {
            defect: MarkerDefect::Unreadable { detail },
            ..
        } => Some(detail),
        _ => None,
    }
}

/// The `.rwv-active` pointer as a root carries it.
///
/// Presence and content are separate facts. An empty pointer names no project
/// and is still a present file — still a second copy of an identity the marker
/// beside it already states — so a reader answering only "which project" would
/// lose the state doctor reports on.
#[derive(Debug)]
pub enum ActivePointer {
    Absent,
    Present(Option<ProjectName>),
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
    Primary {
        root: PathBuf,
        selection: Option<ProjectName>,
    },
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
    ///
    /// The only arm that carries the pointer's presence, because it is the
    /// only one where the presence is still an open question: `Workweave` and
    /// `Disputed` are the two halves a verified marker splits into on exactly
    /// that test.
    MarkerUnverifiable {
        marker_path: PathBuf,
        defect: MarkerDefect,
        project_hint: Option<ProjectName>,
        pointer: ActivePointer,
    },
}

/// Observe what identity evidence `dir` carries.
///
/// `None` is "not a root of either kind" — a containment walk continues past
/// it. Every other answer is terminal for the walk, including the two that no
/// verb may act on.
pub fn observe_root(dir: &Path) -> Option<RootObservation> {
    let marker_path = WorkweaveMarker::path_in(dir);
    let observation = match observe_marker(&marker_path) {
        MarkerPresence::Defective {
            defect,
            project_hint,
            ..
        } => RootObservation::MarkerUnverifiable {
            marker_path,
            defect,
            project_hint,
            pointer: observe_active_pointer(dir),
        },
        MarkerPresence::Usable(marker) => {
            if !is_workspace_root(&marker.primary) {
                RootObservation::MarkerUnverifiable {
                    marker_path,
                    project_hint: Some(marker.project),
                    defect: MarkerDefect::DanglingPrimary {
                        primary: marker.primary,
                    },
                    pointer: observe_active_pointer(dir),
                }
            } else {
                match observe_active_pointer(dir) {
                    ActivePointer::Present(pointer) => RootObservation::Disputed {
                        root: dir.to_path_buf(),
                        pointer,
                        marker,
                    },
                    ActivePointer::Absent => RootObservation::Workweave { marker },
                }
            }
        }
        MarkerPresence::Absent => {
            if !is_workspace_root(dir) {
                return None;
            }
            RootObservation::Primary {
                root: dir.to_path_buf(),
                selection: read_active_project(dir),
            }
        }
    };
    Some(observation)
}

impl RootObservation {
    /// The identity a verb may act on, or the refusal that names the repair.
    pub fn require_exclusive(self) -> anyhow::Result<WeaveRootIdentity> {
        match self {
            RootObservation::Workweave { marker } => {
                Ok(WeaveRootIdentity::Workweave(WorkweaveIdentity { marker }))
            }
            RootObservation::Primary { root, selection } => {
                Ok(WeaveRootIdentity::Primary(PrimaryIdentity {
                    root,
                    selection,
                }))
            }
            RootObservation::Disputed { root, .. } => anyhow::bail!(
                "{} and {} both exist: a weave root carries the workweave marker \
                 or the active-project pointer, never both. Run `rwv doctor --fix`; \
                 it removes the redundant file where the primary-side workweave \
                 registry names this tree, and reports the conflict where nothing \
                 outside the tree settles which file is the stray.",
                WorkweaveMarker::path_in(&root).display(),
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
            RootObservation::Primary { selection, .. } => selection.as_ref(),
            RootObservation::Disputed { marker, .. } => Some(&marker.project),
            RootObservation::MarkerUnverifiable { project_hint, .. } => project_hint.as_ref(),
        }
    }

    /// Which kind of container this root is, for [`IntegrationContext`].
    ///
    /// Lenient on the same three arms as [`Self::presented_project`], and for
    /// the same reason: a root whose marker is disputed or unmigrated is a
    /// workweave to every integration that asks, because the alternative is
    /// handing it a primary's authority on the strength of the claim its own
    /// marker failed to witness.
    ///
    /// [`IntegrationContext`]: crate::integration::IntegrationContext
    pub fn container_kind(&self) -> ContainerKind {
        match self {
            RootObservation::Primary { .. } => ContainerKind::Primary,
            _ => ContainerKind::Workweave,
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

/// A primary root, and the only thing that can select a project.
///
/// The root travels inside the witness rather than beside it. A witness that
/// only attested "some primary root exists" would leave the directory a free
/// argument at the write, so the caller could still name a marker root and
/// nothing in the type would object.
#[derive(Debug, Clone)]
pub struct PrimaryIdentity {
    root: PathBuf,
    selection: Option<ProjectName>,
}

impl PrimaryIdentity {
    pub fn into_selection(self) -> Option<ProjectName> {
        self.selection
    }

    /// Write the `.rwv-active` pointer — project SELECTION, and rwv's sole
    /// write path for it.
    ///
    /// Selection is primary-only: a workweave's project is fixed at creation
    /// by its `.rwv-workweave` marker and cannot be switched, so a pointer
    /// there would be a second, unread copy of the workweave's own identity
    /// beside the marker — the state `rwv doctor` reports as
    /// [`CheckViolation::WeaveRootIdentityConflict`]. That is why this hangs
    /// off the witness instead of taking a root: the arm that carries a
    /// marker has no such method, so the write is not expressible there.
    ///
    /// [`CheckViolation::WeaveRootIdentityConflict`]: crate::check::CheckViolation::WeaveRootIdentityConflict
    pub fn select_project(&self, project: &ProjectName) -> anyhow::Result<()> {
        let path = self.root.join(ACTIVE_PROJECT_FILE);
        crate::state_file::StateFile::ActiveProject
            .publish_in(&self.root, format!("{}\n", project.as_str()).as_bytes())
            .with_context(|| format!("failed to write {}", path.display()))
    }
}

/// The index-side counterpart of the legacy-marker check in
/// [`WorkweaveMarker::read`]: does `(primary_root, project)`'s workweave
/// index predate ref-ownership receipts?
///
/// Two legacy shapes migrate in the same `rwv doctor --fix` pass and each is
/// detected where it lives — the marker's YAML-or-missing-`parent:` shape
/// above, the index's missing `receipts` field here. `Some(path)` is the
/// file to migrate, and
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

    /// Record `dir` under `name` in `project`'s primary-side index — the
    /// registration `workweave create` performs, and the only thing that gives
    /// a marker-bearing directory a name a resolution can find.
    fn register_workweave(primary: &Path, project: &str, name: &str, dir: &Path) {
        let project = ProjectName::new(project).unwrap();
        std::fs::create_dir_all(project_dir(primary, project.as_str())).unwrap();
        crate::workweave_index::record_workweave(
            primary,
            &project,
            name,
            crate::workweave_index::canonical_recorded_path(dir),
        )
        .unwrap();
    }

    // ========================================================================
    // project_name_from_dir renders a name, not a path
    // ========================================================================

    #[test]
    fn nested_project_name_drops_the_spelling_of_the_path_it_came_from() {
        let name =
            project_name_from_dir(Path::new(""), Path::new("projects//chatly//web-app")).unwrap();
        ProjectName::new(name.clone())
            .expect("a derived project name must construct a ProjectName");
        assert_eq!(name, "chatly/web-app");
    }

    /// Relative and absolute spellings answer alike, and on a host whose
    /// separator is already `/` they cannot disagree — the equality is
    /// load-bearing only where `std::path::MAIN_SEPARATOR` is a backslash.
    #[test]
    fn nested_project_name_is_the_same_relative_and_absolute() {
        let relative =
            project_name_from_dir(Path::new(""), Path::new("projects/chatly/web-app")).unwrap();
        let absolute = project_name_from_dir(
            Path::new("/w"),
            &Path::new("/w")
                .join("projects")
                .join("chatly")
                .join("web-app"),
        )
        .unwrap();
        ProjectName::new(absolute.clone())
            .expect("a derived project name must construct a ProjectName");
        assert_eq!(relative, absolute);
    }

    /// A name may contain the layout segment. The weave's `projects` is the
    /// one the root names; every later one belongs to the project.
    #[test]
    fn a_name_may_contain_the_layout_segment_and_keeps_all_of_it() {
        let root = Path::new("/w");
        let dir = projects_dir(root).join("a").join("projects").join("b");
        assert_eq!(
            project_name_from_dir(root, &dir).as_deref(),
            Some("a/projects/b")
        );
    }

    /// A directory outside the root's projects tree has no name here, and
    /// saying so is the answer — the previous reading searched the path for a
    /// `projects` component and answered anyway.
    #[test]
    fn a_directory_outside_the_projects_tree_has_no_project_name() {
        assert_eq!(
            project_name_from_dir(Path::new("/w"), Path::new("/elsewhere/projects/x")),
            None
        );
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

        let ctx = WorkspaceContext::resolve_invocation(&deep, None).unwrap();
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

        let ctx = WorkspaceContext::resolve_invocation(&project_dir, None).unwrap();
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
        let result = WorkspaceContext::resolve_invocation(&weave_dir, None);
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
        let ctx = WorkspaceContext::resolve_invocation(&repo_dir, None).unwrap();
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
        let result = WorkspaceContext::resolve_invocation(tmp.path(), None);
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

        let ctx = WorkspaceContext::resolve_invocation(
            &root,
            Some(ProjectName::new("overridden-project").unwrap()),
        )
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
            parent: CanonicalPath::of(&primary_canon),
        };
        marker.write(&weave_dir).unwrap();

        let ctx = WorkspaceContext::resolve_invocation(
            &weave_dir,
            Some(ProjectName::new("custom-proj").unwrap()),
        )
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

        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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
    // PrimaryIdentity::select_project
    // ========================================================================

    /// The witness for `root`, minted the only way there is.
    fn witness(root: &Path) -> PrimaryIdentity {
        match observe_root(root).unwrap().require_exclusive() {
            Ok(WeaveRootIdentity::Primary(identity)) => identity,
            other => panic!("{} is not a primary root: {other:?}", root.display()),
        }
    }

    #[test]
    fn select_project_writes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let project = ProjectName::new("web-app").unwrap();
        witness(&root).select_project(&project).unwrap();

        let content = std::fs::read_to_string(root.join(".rwv-active")).unwrap();
        assert_eq!(content, "web-app\n");
    }

    #[test]
    fn select_project_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let identity = witness(&root);
        identity
            .select_project(&ProjectName::new("old-project").unwrap())
            .unwrap();
        identity
            .select_project(&ProjectName::new("new-project").unwrap())
            .unwrap();

        let project = read_active_project(&root).expect("should return project");
        assert_eq!(project.as_str(), "new-project");
    }

    #[test]
    fn select_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let project = ProjectName::new("mobile-app").unwrap();
        witness(&root).select_project(&project).unwrap();

        let read_back = read_active_project(&root).expect("should return project");
        assert_eq!(read_back, project);
    }

    /// The witness writes to the root it was observed at, not to a root the
    /// caller names — there is no second argument to disagree with the first.
    #[test]
    fn select_project_writes_to_the_root_the_witness_came_from() {
        let tmp = tempfile::tempdir().unwrap();
        let observed = make_workspace(tmp.path(), "ws");
        let other = make_workspace(tmp.path(), "elsewhere");

        witness(&observed)
            .select_project(&ProjectName::new("web-app").unwrap())
            .unwrap();

        assert!(observed.join(".rwv-active").exists());
        assert!(!other.join(".rwv-active").exists());
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

        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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
        let ctx = WorkspaceContext::resolve_invocation(&project_dir, None).unwrap();
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

        let ctx = WorkspaceContext::resolve_invocation(
            &root,
            Some(ProjectName::new("explicit-override").unwrap()),
        )
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
            parent: CanonicalPath::of(Path::new("/home/user/weaveroot")),
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
            parent: CanonicalPath::of(&parent),
        };
        marker.write(dir).unwrap();

        let read_back = WorkweaveMarker::read(dir).unwrap().unwrap();
        assert_eq!(read_back.parent.as_path(), parent);
    }

    /// The file is the one place a `parent` this program never wrote can get
    /// in, so reading is where the type's promise has to be re-established —
    /// a consumer comparing against it cannot tell where the value came from.
    #[test]
    fn workweave_marker_resolves_a_hand_written_parent_on_read() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("ww");
        let primary = tmp.path().join("primary");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&primary).unwrap();

        let detoured = primary.join("..").join("primary");
        std::fs::write(
            dir.join(".rwv-workweave"),
            serde_json::to_string_pretty(&serde_json::json!({
                "primary": primary,
                "project": "p",
                "parent": detoured,
            }))
            .unwrap(),
        )
        .unwrap();

        let marker = WorkweaveMarker::read(&dir).unwrap().unwrap();
        assert_eq!(
            marker.parent().as_path(),
            primary.canonicalize().unwrap(),
            "a parent read off disk must arrive in the same spelling one built here would"
        );
    }

    /// `parent` is a JSON string, as it was before it acquired a type. A
    /// marker this build writes must stay readable by one that predates it.
    #[test]
    fn workweave_marker_parent_is_written_as_a_bare_string() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        WorkweaveMarker {
            primary: PathBuf::from("/home/user/primary"),
            project: ProjectName::new("p").unwrap(),
            parent: CanonicalPath::of(Path::new("/home/user/parent")),
        }
        .write(dir)
        .unwrap();

        let written = std::fs::read_to_string(dir.join(".rwv-workweave")).unwrap();
        assert!(
            written.contains(r#""parent": "/home/user/parent""#),
            "got: {written}"
        );
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

    /// A legacy marker with no `primary:` of its own has nothing for
    /// `migrate_legacy` to migrate from — unlike a missing `parent:`, which
    /// backfills from `primary`. `read()` must say so rather than pointing
    /// at `rwv doctor --fix`, which would leave the file untouched.
    #[test]
    fn workweave_marker_missing_primary_is_unmigratable() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let legacy = "project: legacy-project\n";
        std::fs::write(dir.join(".rwv-workweave"), legacy).unwrap();

        let result = WorkweaveMarker::read(dir);
        assert!(
            result.is_err(),
            "read() should fail for a legacy marker missing primary:"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            !msg.contains("rwv doctor --fix"),
            "nothing migrates a marker with no primary: to migrate from: {msg}"
        );
        for field in ["primary", "project", "parent"] {
            assert!(
                msg.contains(field),
                "the message must name `{field}` as one of the three fields to write \
                 by hand: {msg}"
            );
        }

        assert!(
            WorkweaveMarker::migrate_legacy(dir).is_err(),
            "migrate_legacy must also refuse a marker with no primary: to migrate from"
        );
    }

    #[test]
    fn workweave_marker_read_refuses_a_complete_yaml_marker() {
        // Markers are JSON now. A YAML marker carrying every field —
        // `primary`, `project`, and `parent` — is not silently accepted as
        // usable just because it has the right shape; it is a legacy marker
        // like any other, refused at read and convertible by migrate_legacy.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let yaml = "primary: /home/user/primary\n\
                    project: p\n\
                    parent: /home/user/primary\n";
        std::fs::write(dir.join(".rwv-workweave"), yaml).unwrap();

        let result = WorkweaveMarker::read(dir);
        assert!(
            result.is_err(),
            "a complete YAML marker must still be refused, not silently parsed"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("legacy workweave marker"),
            "error should name it a legacy marker: {msg}"
        );

        assert!(
            WorkweaveMarker::migrate_legacy(dir).unwrap(),
            "migrate_legacy must convert the YAML marker to JSON"
        );
        let migrated = WorkweaveMarker::read(dir)
            .expect("migrated marker must parse")
            .expect("migrated marker must be present");
        assert_eq!(migrated.parent.as_path(), Path::new("/home/user/primary"));
    }

    #[test]
    fn workweave_marker_migrate_legacy_preserves_an_explicit_parent() {
        // migrate_legacy's parent-backfill only fires when parent is
        // missing/null; one already present in the legacy YAML (e.g. a
        // marker forked from another workweave) must survive migration
        // rather than get overwritten with primary.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let yaml = "primary: /home/user/primary\n\
                    project: p\n\
                    parent: /home/user/.workweaves/primary--ww1\n";
        std::fs::write(dir.join(".rwv-workweave"), yaml).unwrap();

        assert!(WorkweaveMarker::migrate_legacy(dir).unwrap());

        let marker = WorkweaveMarker::read(dir).unwrap().unwrap();
        assert_eq!(
            marker.parent.as_path(),
            Path::new("/home/user/.workweaves/primary--ww1"),
            "explicit parent must survive migration"
        );
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
        assert_eq!(marker.parent.as_path(), marker.primary);
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
            "the file is already a JSON marker on the second call; must not rewrite"
        );
    }

    /// A plain YAML scalar cannot contain `: ` — the parser reads it as a
    /// nested mapping key. A primary path with that shape (quoted here the
    /// way a hand-written legacy marker would need to) exercises the raw
    /// `Value` surgery migrate_legacy does to backfill `parent:`, distinct
    /// from splicing the text directly.
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
        assert_eq!(marker.parent.as_path(), marker.primary);
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

        let claim = crate::workweave_index::IndexClaim::acquire(&primary, &project).unwrap();
        crate::workweave_index::write(
            &claim,
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
            parent: CanonicalPath::of(&primary_canon),
        };
        marker.write(&weave_dir).unwrap();
        register_workweave(&primary_canon, "web-app", "feat", &weave_dir);

        let ctx = WorkspaceContext::resolve_invocation(&weave_dir, None).unwrap();
        assert_eq!(ctx.primary_path(), primary_canon);
        match &ctx.checkout {
            Checkout::Workweave {
                name, dir, project, ..
            } => {
                assert_eq!(name.recorded().unwrap().as_str(), "feat");
                assert_eq!(*dir, weave_dir.canonicalize().unwrap());
                assert_eq!(project.as_str(), "web-app");
            }
            Checkout::Primary { .. } => panic!("expected Workweave"),
        }
    }

    /// A primary root and a workweave root beside it, both real enough for
    /// workspace resolution to land on them.
    fn primary_and_workweave_roots(tmp: &Path) -> (PathBuf, PathBuf) {
        let root = make_workspace(tmp, "ws");
        let weave_dir = tmp.join(".workweaves").join("feat");
        std::fs::create_dir_all(&weave_dir).unwrap();
        let primary_canon = root.canonicalize().unwrap();
        WorkweaveMarker::new(
            primary_canon.clone(),
            ProjectName::new("web-app").unwrap(),
            &primary_canon,
        )
        .write(&weave_dir)
        .unwrap();
        (root, weave_dir)
    }

    /// The kind carried into `IntegrationContext` matches the resolved
    /// `Checkout`, for both a primary run and a workweave run — the one
    /// production path from a real `WorkspaceContext` down to the value an
    /// integration reads.
    ///
    /// The session derives the kind from its own root and the `Checkout`
    /// resolves it by walking, so the two sides are computed independently and
    /// this asserts they agree.
    #[test]
    fn container_kind_reaches_integration_context_matching_the_resolved_checkout() {
        use crate::integration::IntegrationContext;
        use crate::manifest::IntegrationConfig;

        let manifest = Manifest::from_toml_str("[repositories]\n").unwrap();
        let config = IntegrationConfig::default();
        let cache = std::collections::HashMap::new();
        let project = ProjectName::new("web-app").unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let (root, weave_dir) = primary_and_workweave_roots(tmp.path());

        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
        let session = WorkspaceSession::new(&root);
        let base = session.context_base(&project, &cache, None);
        let int_ctx: IntegrationContext = base.build_context(&config, &manifest);
        assert_eq!(ctx.checkout.kind(), ContainerKind::Primary);
        assert_eq!(int_ctx.container_kind, ctx.checkout.kind());

        let ww_ctx = WorkspaceContext::resolve_invocation(&weave_dir, None).unwrap();
        let ww_session = WorkspaceSession::new(&weave_dir);
        let ww_base = ww_session.context_base(&project, &cache, None);
        let ww_int_ctx: IntegrationContext = ww_base.build_context(&config, &manifest);
        assert_eq!(ww_ctx.checkout.kind(), ContainerKind::Workweave);
        assert_eq!(ww_int_ctx.container_kind, ww_ctx.checkout.kind());
    }

    /// The kind describes the root the session was built at, not the checkout
    /// the invocation resolved.
    ///
    /// Both crossings are asserted, because each alone is satisfied by a
    /// constant: a session at primary answers `Primary` while the invocation
    /// sits in a workweave, and a session at the workweave answers `Workweave`
    /// while the invocation sits at primary. `activate_intent` is the crossing
    /// that reaches production — it authors at `primary_path()` from whatever
    /// checkout the operator invoked it in.
    #[test]
    fn container_kind_describes_the_session_root_not_the_resolved_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, weave_dir) = primary_and_workweave_roots(tmp.path());

        let primary_ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
        let workweave_ctx = WorkspaceContext::resolve_invocation(&weave_dir, None).unwrap();
        assert_eq!(primary_ctx.checkout.kind(), ContainerKind::Primary);
        assert_eq!(workweave_ctx.checkout.kind(), ContainerKind::Workweave);

        assert_eq!(
            WorkspaceSession::new(primary_ctx.primary_path()).container_kind,
            ContainerKind::Primary
        );
        assert_eq!(
            WorkspaceSession::new(workweave_ctx.primary_path()).container_kind,
            ContainerKind::Primary
        );
        assert_eq!(
            WorkspaceSession::new(workweave_ctx.active_path()).container_kind,
            ContainerKind::Workweave
        );
        assert_eq!(
            WorkspaceSession::new(&weave_dir).container_kind,
            ContainerKind::Workweave
        );
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
            parent: CanonicalPath::of(&primary_canon),
        };
        marker.write(&weave_dir).unwrap();
        register_workweave(&primary_canon, "web-app", "feat", &weave_dir);

        let ctx = WorkspaceContext::resolve_invocation(&repo_dir, None).unwrap();
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
        match &ctx.checkout {
            Checkout::Workweave {
                name, dir, project, ..
            } => {
                assert_eq!(name.recorded().unwrap().as_str(), "feat");
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
            parent: CanonicalPath::of(&primary_canon),
        };
        marker.write(&weave_dir).unwrap();
        register_workweave(&primary_canon, "marker-project", "dash-name", &weave_dir);

        let ctx = WorkspaceContext::resolve_invocation(&weave_dir, None).unwrap();
        // Marker takes precedence for project (from marker, not from the
        // directory's left component).
        match &ctx.checkout {
            Checkout::Workweave { name, project, .. } => {
                assert_eq!(name.recorded().unwrap().as_str(), "dash-name");
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
        let result = WorkspaceContext::resolve_invocation(&weave_dir, None);
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("no repoweave workspace found"),
            "unexpected error: {msg}"
        );
    }

    // ========================================================================
    // Root identity drives resolution
    //
    // The one-tier collapse used to be enforced by the ORDER of two checks in
    // the walk. It is now one observation per ancestor, so the states no verb
    // may act on are refusals rather than fall-throughs.
    // ========================================================================

    /// A copied workweave: workspace-shaped, carrying a marker whose
    /// `primary:` names a directory that is not a workspace root.
    ///
    /// This tree used to resolve as a `Checkout::Primary` at itself and pick
    /// its project from its own stray `.rwv-active` — the marker was dropped
    /// silently and the structural shape decided. Every workweave is shaped
    /// exactly like a primary, so that handed the tree a primary's authority
    /// on the strength of its own broken claim.
    fn make_copied_workweave(parent: &Path) -> (PathBuf, PathBuf) {
        let dir = make_workspace(parent, "web-app--copied");
        let dangling = parent.join("gone");
        std::fs::create_dir_all(&dangling).unwrap();
        WorkweaveMarker {
            primary: dangling.clone(),
            project: ProjectName::new("web-app").unwrap(),
            parent: CanonicalPath::of(&dangling),
        }
        .write(&dir)
        .unwrap();
        (dir, dangling)
    }

    #[test]
    fn resolve_refuses_a_marker_whose_primary_is_not_a_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, dangling) = make_copied_workweave(tmp.path());
        std::fs::write(dir.join(ACTIVE_PROJECT_FILE), "web-app\n").unwrap();

        let err = WorkspaceContext::resolve_invocation(&dir, None).unwrap_err();
        let msg = format!("{err}");
        let canon = dir.canonicalize().unwrap();
        assert!(
            msg.contains(&canon.join(WORKWEAVE_MARKER_FILE).display().to_string()),
            "must name the marker: {msg}"
        );
        assert!(
            msg.contains(&dangling.display().to_string()),
            "must name the dangling primary: {msg}"
        );
        assert!(
            msg.contains("Repair `primary:`"),
            "must offer the repair: {msg}"
        );
        assert!(
            msg.contains("standalone weave"),
            "must offer the adopt-as-standalone exit: {msg}"
        );
    }

    /// The refusal is terminal for the walk: an unverifiable marker does not
    /// fall through to the enclosing workspace either.
    #[test]
    fn resolve_refuses_from_inside_a_copied_workweave() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = make_workspace(tmp.path(), "ws");
        let (dir, _) = make_copied_workweave(&outer);
        let repo_dir = dir.join("github").join("acme").join("server");
        std::fs::create_dir_all(&repo_dir).unwrap();

        let err = WorkspaceContext::resolve_invocation(&repo_dir, None).unwrap_err();
        assert!(
            format!("{err}").contains("not a repoweave workspace root"),
            "unexpected error: {err}"
        );
    }

    /// A workweave root carrying both identity files, verified marker.
    fn make_disputed_workweave(parent: &Path) -> (PathBuf, PathBuf) {
        let root = make_workspace(parent, "ws");
        let dir = parent.join(".workweaves").join("web-app--feat");
        std::fs::create_dir_all(&dir).unwrap();
        let primary_canon = root.canonicalize().unwrap();
        WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("web-app").unwrap(),
            parent: CanonicalPath::of(&primary_canon),
        }
        .write(&dir)
        .unwrap();
        std::fs::write(dir.join(ACTIVE_PROJECT_FILE), "other-project\n").unwrap();
        register_workweave(&primary_canon, "web-app", "feat", &dir);
        (root, dir)
    }

    #[test]
    fn resolve_refuses_a_root_carrying_both_identity_files() {
        let tmp = tempfile::tempdir().unwrap();
        let (_, dir) = make_disputed_workweave(tmp.path());

        let err = WorkspaceContext::resolve_invocation(&dir, None).unwrap_err();
        let msg = format!("{err}");
        let canon = dir.canonicalize().unwrap();
        assert!(
            msg.contains(&canon.join(WORKWEAVE_MARKER_FILE).display().to_string()),
            "must name the marker: {msg}"
        );
        assert!(
            msg.contains(&canon.join(ACTIVE_PROJECT_FILE).display().to_string()),
            "must name the pointer: {msg}"
        );
        assert!(
            msg.contains("rwv doctor --fix"),
            "must name the repair: {msg}"
        );
    }

    /// `doctor` and `status` reach the same walk through
    /// `resolve_tolerating_disputed_root`, and proceed by marker — the
    /// pointer's `other-project` decides nothing.
    #[test]
    fn the_exempt_entry_point_proceeds_by_marker_from_a_disputed_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (root, dir) = make_disputed_workweave(tmp.path());

        let ctx =
            WorkspaceContext::resolve_invocation_tolerating_disputed_root(&dir, None).unwrap();
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
        assert_eq!(ctx.project_provenance(), Some(ProjectProvenance::Marker));
        match &ctx.checkout {
            Checkout::Workweave { name, project, .. } => {
                assert_eq!(name.recorded().unwrap().as_str(), "feat");
                assert_eq!(project.as_str(), "web-app");
            }
            Checkout::Primary { .. } => panic!("expected Workweave"),
        }
    }

    /// The exemption is scoped to the disputed state alone: an unverifiable
    /// marker refuses `doctor` and `status` too, because there is no verified
    /// primary for either of them to inspect or repair through. The refusal is
    /// what those two would otherwise have reported, so it carries the same
    /// remediation the non-exempt one does.
    ///
    /// The fixture writes no `.rwv-active`: the pointer is not what makes an
    /// unverifiable marker terminal, and a copied tree that never carried one
    /// reaches the same refusal.
    #[test]
    fn the_exempt_entry_point_still_refuses_an_unverifiable_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, dangling) = make_copied_workweave(tmp.path());
        assert!(!dir.join(ACTIVE_PROJECT_FILE).exists());

        let err =
            WorkspaceContext::resolve_invocation_tolerating_disputed_root(&dir, None).unwrap_err();
        let msg = format!("{err}");
        let canon = dir.canonicalize().unwrap();
        assert!(
            msg.contains("not a repoweave workspace root"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains(&canon.join(WORKWEAVE_MARKER_FILE).display().to_string()),
            "must name the marker: {msg}"
        );
        assert!(
            msg.contains(&dangling.display().to_string()),
            "must name the dangling primary: {msg}"
        );
        assert!(
            msg.contains("Repair `primary:`"),
            "must offer the repair: {msg}"
        );
        assert!(
            msg.contains("standalone weave"),
            "must offer the adopt-as-standalone exit: {msg}"
        );
    }

    #[test]
    fn checkout_workweave_carries_the_markers_parent_when_forked_from_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let dir = tmp.path().join(".workweaves").join("web-app--feat");
        std::fs::create_dir_all(&dir).unwrap();
        let primary_canon = root.canonicalize().unwrap();
        WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("web-app").unwrap(),
            parent: CanonicalPath::of(&primary_canon),
        }
        .write(&dir)
        .unwrap();

        let ctx = WorkspaceContext::resolve_invocation(&dir, None).unwrap();
        match &ctx.checkout {
            Checkout::Workweave { parent, .. } => assert_eq!(*parent, primary_canon),
            Checkout::Primary { .. } => panic!("expected Workweave"),
        }
    }

    /// The case the field exists for: a workweave forked from another
    /// workweave, whose parent is not the primary. `rwv sync` with no source
    /// syncs one hop toward the primary, which is the parent and not the
    /// primary_root beside it.
    #[test]
    fn checkout_workweave_carries_a_parent_that_is_not_the_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let primary_canon = root.canonicalize().unwrap();

        let upper = tmp.path().join(".workweaves").join("web-app--upper");
        std::fs::create_dir_all(&upper).unwrap();
        WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("web-app").unwrap(),
            parent: CanonicalPath::of(&primary_canon),
        }
        .write(&upper)
        .unwrap();

        let lower = tmp.path().join(".workweaves").join("web-app--lower");
        std::fs::create_dir_all(&lower).unwrap();
        let upper_canon = upper.canonicalize().unwrap();
        WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("web-app").unwrap(),
            parent: CanonicalPath::of(&upper_canon),
        }
        .write(&lower)
        .unwrap();

        let ctx = WorkspaceContext::resolve_invocation(&lower, None).unwrap();
        assert_eq!(ctx.primary_path(), primary_canon);
        match &ctx.checkout {
            Checkout::Workweave { parent, .. } => {
                assert_eq!(*parent, upper_canon);
                assert_ne!(*parent, primary_canon);
            }
            Checkout::Primary { .. } => panic!("expected Workweave"),
        }
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
    // that `std::env::home_dir()` (which returns the REAL home dir) does not affect
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
        let ctx = WorkspaceContext::resolve_invocation(&deep, None).unwrap();
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
        let home = match std::env::home_dir() {
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
        let ctx = WorkspaceContext::resolve_invocation(&deep, None).unwrap();
        assert_eq!(ctx.primary_path(), root.canonicalize().unwrap());
    }

    // ========================================================================
    // $HOME ceiling — symlinked home path
    //
    // `resolve()` canonicalizes `cwd` but the original code
    // bound `home_dir` as the raw (un-canonicalized) value from
    // `std::env::home_dir()`.  On systems where $HOME contains a symlinked
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
        crate::symlink::create(
            &real_home,
            &link_home,
            crate::symlink::LinkTarget::Directory,
        )
        .unwrap();

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
        crate::symlink::create(
            &real_home,
            &link_home,
            crate::symlink::LinkTarget::Directory,
        )
        .unwrap();

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
        let ctx = WorkspaceContext::resolve_invocation(&cwd_via_link, None).unwrap();

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
            let dir = root.join("projects").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(Manifest::FILE_NAME), Manifest::SKELETON).unwrap();
        }
    }

    /// Helper: write a workweave marker at `dir` pointing at `primary` and
    /// naming `project`.
    fn write_marker(dir: &Path, primary: &Path, project: &str) {
        let marker = WorkweaveMarker {
            primary: primary.to_path_buf(),
            project: ProjectName::new(project).unwrap(),
            parent: CanonicalPath::of(primary),
        };
        marker.write(dir).unwrap();
    }

    // ========================================================================
    // resolve_for_project: the binding is the caller's, and the workspace's
    // own records are cross-checked against it rather than consulted for it
    // ========================================================================

    #[test]
    fn resolve_for_project_does_not_consult_the_pointer() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["one", "two"]);
        std::fs::write(root.join(".rwv-active"), "two\n").unwrap();

        let ctx = WorkspaceContext::resolve_for_project(&root, &ProjectName::new("one").unwrap())
            .unwrap();
        assert_eq!(
            ctx.active_project().unwrap().as_str(),
            "one",
            "a bound resolution answers with the caller's project, not the pointer's"
        );
        assert_eq!(ctx.project_provenance(), Some(ProjectProvenance::Bound));
    }

    #[test]
    fn resolve_for_project_refuses_a_marker_naming_another_project() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "primary");
        make_projects(&primary, &["web-app", "other-app"]);
        let ww = make_workweave(tmp.path(), "other-app--feat", &primary, "other-app");

        let err = WorkspaceContext::resolve_for_project(&ww, &ProjectName::new("web-app").unwrap())
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("other-app") && msg.contains("web-app"),
            "the refusal must name both records — the marker's project and the \
             op's binding — because nothing here can pick between them; got: {msg}"
        );
    }

    #[test]
    fn resolve_for_project_proceeds_when_the_marker_corroborates() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "primary");
        make_projects(&primary, &["web-app"]);
        let ww = make_workweave(tmp.path(), "web-app--feat", &primary, "web-app");

        let ctx = WorkspaceContext::resolve_for_project(&ww, &ProjectName::new("web-app").unwrap())
            .unwrap();
        assert_eq!(ctx.active_project().unwrap().as_str(), "web-app");
    }

    #[test]
    fn resolve_for_project_refuses_when_the_project_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["one"]);

        let err = WorkspaceContext::resolve_for_project(&root, &ProjectName::new("two").unwrap())
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("two"),
            "the refusal must name the project it could not find; got: {msg}"
        );
        assert!(
            !msg.contains(".rwv-active"),
            "a bound resolution never read the pointer, so its refusal must not \
             send the reader to it; got: {msg}"
        );
    }

    #[test]
    fn resolve_unbound_leaves_a_primary_with_no_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["one"]);
        std::fs::write(root.join(".rwv-active"), "one\n").unwrap();

        let ctx = WorkspaceContext::resolve_unbound(&root).unwrap();
        assert!(
            ctx.active_project().is_none(),
            "an unbound resolution must not consult the pointer, and this root's \
             names a project"
        );
        assert_eq!(ctx.project_provenance(), None);
    }

    /// A workweave's marker is the directory's own identity rather than
    /// anything ambient, so an unbound resolution still reports it — that is
    /// what makes `resolve_unbound` usable for the workweave-identity
    /// comparisons that motivated it.
    #[test]
    fn resolve_unbound_still_reports_a_workweave_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = make_workspace(tmp.path(), "primary");
        make_projects(&primary, &["web-app"]);
        let ww = make_workweave(tmp.path(), "web-app--feat", &primary, "web-app");

        let ctx = WorkspaceContext::resolve_unbound(&ww).unwrap();
        assert_eq!(ctx.active_project().unwrap().as_str(), "web-app");
    }

    /// Chain step 1: `--project` at a primary wins even when `.rwv-active`
    /// is set. Provenance = Flag.
    #[test]
    fn provenance_flag_at_primary_beats_active_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        make_projects(&root, &["one", "two"]);
        std::fs::write(root.join(".rwv-active"), "one\n").unwrap();

        let ctx =
            WorkspaceContext::resolve_invocation(&root, Some(ProjectName::new("two").unwrap()))
                .unwrap();
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

        let ctx = WorkspaceContext::resolve_invocation(
            &weave_dir,
            Some(ProjectName::new("two").unwrap()),
        )
        .unwrap();
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

        let ctx = WorkspaceContext::resolve_invocation(&weave_dir, None).unwrap();
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

        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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

        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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

        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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

        let ctx = WorkspaceContext::resolve_invocation(
            &weave_dir,
            Some(ProjectName::new("flag-p").unwrap()),
        )
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
        let ctx = WorkspaceContext::resolve_invocation(&root, Some(ProjectName::new("p").unwrap()))
            .unwrap();
        assert_eq!(ctx.project_provenance(), Some(ProjectProvenance::Flag));
        ctx.emit_target_line(); // must not panic; policy says silent

        // Marker: silent.
        let weave_dir = tmp.path().join("ws--feat");
        std::fs::create_dir_all(&weave_dir).unwrap();
        write_marker(&weave_dir, &root.canonicalize().unwrap(), "p");
        let ctx = WorkspaceContext::resolve_invocation(&weave_dir, None).unwrap();
        assert_eq!(ctx.project_provenance(), Some(ProjectProvenance::Marker));
        ctx.emit_target_line(); // must not panic; policy says silent

        // None: silent.
        let root2 = make_workspace(tmp.path(), "ws2");
        make_projects(&root2, &["p"]);
        let ctx = WorkspaceContext::resolve_invocation(&root2, None).unwrap();
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

        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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
        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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
        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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
        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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
        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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

    /// Windows canonicalization returns paths in this form, and it is what a
    /// published field must never carry: `git -C`, `cmd` and `xargs` all
    /// choke on it. On Unix no path starts with it, so the assertions that
    /// name it hold there without saying anything.
    const VERBATIM_PREFIX: &str = r"\\?\";

    /// At primary with an active project: resolution is present, workweave
    /// absent (no workweave checkout), workspace and project match the context.
    #[test]
    fn resolution_at_primary_has_workspace_and_project_no_workweave() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "myproject\n").unwrap();

        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
        let res = ctx
            .resolution()
            .expect("resolution must be present with an active project");

        assert_eq!(
            res.workspace,
            crate::path_spelling::wire_path(&root.canonicalize().unwrap()),
            "the published root is the wire mint of the resolved root, not the \
             resolved root as this host holds it"
        );
        assert!(
            !res.workspace.starts_with(VERBATIM_PREFIX),
            "an internal spelling reached a published field: {}",
            res.workspace
        );
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

        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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

        // Create a registered workweave directory with the marker.
        let weave_dir = tmp.path().join("ws--fo-abc");
        std::fs::create_dir_all(&weave_dir).unwrap();
        let marker = WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("myproject").unwrap(),
            parent: CanonicalPath::of(&primary_canon),
        };
        marker.write(&weave_dir).unwrap();
        register_workweave(&primary_canon, "myproject", "fo-abc", &weave_dir);

        let ctx = WorkspaceContext::resolve_invocation(&weave_dir, None).unwrap();
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
        assert_eq!(
            res.workspace,
            crate::path_spelling::wire_path(
                &weave_dir
                    .canonicalize()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("ws")
                    .canonicalize()
                    .unwrap()
            ),
            "the workspace a workweave publishes is the primary root in the \
             wire spelling"
        );
        assert!(
            !res.workspace.starts_with(VERBATIM_PREFIX),
            "an internal spelling reached a published field: {}",
            res.workspace
        );
    }

    /// One root, two seats, one spelling. Built from the two published values
    /// and nothing else: whatever the mint does, a consumer keying a workweave
    /// off the workspace root its own resolution names has to match what the
    /// primary named, and that agreement is the property the wire seam exists
    /// to hold.
    #[test]
    fn primary_and_workweave_publish_one_spelling_of_the_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "myproject\n").unwrap();
        let primary_canon = root.canonicalize().unwrap();

        let weave_dir = tmp.path().join("ws--w1");
        std::fs::create_dir_all(&weave_dir).unwrap();
        WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("myproject").unwrap(),
            parent: CanonicalPath::of(&primary_canon),
        }
        .write(&weave_dir)
        .unwrap();
        register_workweave(&primary_canon, "myproject", "w1", &weave_dir);

        let at_primary = WorkspaceContext::resolve_invocation(&root, None)
            .unwrap()
            .resolution()
            .expect("resolution must be present at primary");
        let in_workweave = WorkspaceContext::resolve_invocation(&weave_dir, None)
            .unwrap()
            .resolution()
            .expect("resolution must be present in a workweave");

        assert_eq!(
            at_primary.workspace, in_workweave.workspace,
            "the same root published from two seats must be the same bytes"
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

        let ctx = WorkspaceContext::resolve_invocation(&root, None).unwrap();
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
            parent: CanonicalPath::of(&primary_canon),
        };
        marker.write(&weave_dir).unwrap();
        register_workweave(&primary_canon, "myproject", "fo-abc", &weave_dir);

        let ctx = WorkspaceContext::resolve_invocation(&weave_dir, None).unwrap();
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

    /// A workweave the registry does not name resolves to something a
    /// consumer can tell apart from the primary.
    ///
    /// Both have no `<project>--<name>` identity to report, so `workweave` is
    /// absent in both — and that absence is documented as meaning "at the
    /// primary". Without the flag below, a plugin spawned inside an
    /// unregistered workweave is told, positively and in rwv's own published
    /// contract, that it is somewhere it is not.
    #[test]
    fn an_unregistered_workweave_does_not_serialize_as_the_primary() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        std::fs::write(root.join(".rwv-active"), "myproject\n").unwrap();
        let primary_canon = root.canonicalize().unwrap();

        let weave_dir = tmp.path().join("myproject--unrecorded");
        std::fs::create_dir_all(&weave_dir).unwrap();
        WorkweaveMarker {
            primary: primary_canon.clone(),
            project: ProjectName::new("myproject").unwrap(),
            parent: CanonicalPath::of(&primary_canon),
        }
        .write(&weave_dir)
        .unwrap();
        // Deliberately NOT registered: that is the state under test.

        let unregistered = WorkspaceContext::resolve_invocation(&weave_dir, None)
            .unwrap()
            .resolution()
            .expect("a resolution exists — the marker names the project");
        let primary = WorkspaceContext::resolve_invocation(&root, None)
            .unwrap()
            .resolution()
            .expect("the pointer names the project");

        // The property first, then the mechanism that delivers it: a
        // mutation collapsing the states should be reported as the states
        // collapsing, not as one field losing its value.
        assert!(
            unregistered.workweave.is_none(),
            "an unregistered workweave has no identity to report"
        );
        assert_ne!(
            serde_json::to_string(&unregistered).unwrap(),
            serde_json::to_string(&primary).unwrap(),
            "an unregistered workweave and the primary must not serialize alike"
        );

        let vars = |r: &Resolution| -> std::collections::BTreeMap<String, String> {
            crate::plugins::envelope_vars(Some(r))
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect()
        };
        assert_ne!(
            vars(&unregistered),
            vars(&primary),
            "the env envelope a plugin reads must not be identical either"
        );

        assert!(
            unregistered.workweave_unregistered,
            "the state must be represented, not merely absent"
        );
        assert_eq!(
            vars(&unregistered).get("RWV_WORKWEAVE_UNREGISTERED"),
            Some(&"1".to_owned()),
            "the envelope carries the state as its own variable"
        );
    }

    /// The two states that could serialize before this field still serialize
    /// to exactly the same bytes, field order included. A consumer parsing
    /// today's output sees nothing new unless it is in the new state.
    #[test]
    fn the_representable_states_serialize_byte_for_byte_as_before() {
        let primary = Resolution {
            workspace: "/ws".to_owned(),
            workweave: None,
            workweave_unregistered: false,
            project: "myproject".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&primary).unwrap(),
            r#"{"workspace":"/ws","project":"myproject"}"#
        );

        let registered = Resolution {
            workspace: "/ws".to_owned(),
            workweave: Some("myproject--feat".to_owned()),
            workweave_unregistered: false,
            project: "myproject".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&registered).unwrap(),
            r#"{"workspace":"/ws","workweave":"myproject--feat","project":"myproject"}"#
        );

        let unregistered = Resolution {
            workspace: "/ws".to_owned(),
            workweave: None,
            workweave_unregistered: true,
            project: "myproject".to_owned(),
        };
        assert_eq!(
            serde_json::to_string(&unregistered).unwrap(),
            r#"{"workspace":"/ws","workweave_unregistered":true,"project":"myproject"}"#
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
            parent: CanonicalPath::of(primary),
        };
        marker.write(&dir).unwrap();
        dir
    }

    fn write_pointer(root: &Path, project: &str) {
        std::fs::write(root.join(ACTIVE_PROJECT_FILE), format!("{project}\n")).unwrap();
    }

    fn observe(dir: &Path) -> RootObservation {
        observe_root(dir).unwrap_or_else(|| panic!("expected an observation at {}", dir.display()))
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
            RootObservation::Primary {
                root: observed,
                selection,
            } => {
                assert_eq!(selection.as_ref().map(ProjectName::as_str), Some("web-app"));
                assert_eq!(observed, root);
            }
            other => panic!("expected Primary, got {other:?}"),
        }
    }

    #[test]
    fn observe_root_reads_a_bare_workspace_shaped_root_as_primary_with_no_selection() {
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        match observe(&root) {
            RootObservation::Primary { selection, .. } => assert!(selection.is_none()),
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
                pointer,
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
                assert!(matches!(pointer, ActivePointer::Absent));
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

    /// A legacy marker with no `primary:` is not `Legacy` — that
    /// classification is reserved for a shape `migrate_legacy` can actually
    /// repair, and this one has nothing to backfill from.
    /// `unmigratable_marker_detail` is what a doctor scan reads instead of
    /// silently treating the broken marker as someone else's to report.
    #[test]
    fn observe_root_reads_a_legacy_marker_with_no_primary_as_unreadable() {
        let tmp = tempfile::tempdir().unwrap();
        let weave_dir = tmp.path().join("ws--feat");
        std::fs::create_dir_all(&weave_dir).unwrap();
        std::fs::write(
            weave_dir.join(WORKWEAVE_MARKER_FILE),
            "project: myproject\n",
        )
        .unwrap();

        match observe(&weave_dir) {
            RootObservation::MarkerUnverifiable {
                defect,
                project_hint,
                ..
            } => {
                assert!(
                    matches!(defect, MarkerDefect::Unreadable { .. }),
                    "expected Unreadable, got {defect:?}"
                );
                assert_eq!(
                    project_hint.as_ref().map(ProjectName::as_str),
                    Some("myproject"),
                    "a marker unreadable for lack of primary: still names its project"
                );
            }
            other => panic!("expected MarkerUnverifiable, got {other:?}"),
        }

        assert!(
            legacy_marker_primary(&weave_dir).is_none(),
            "a marker with no primary: is not migrate_legacy's to report"
        );
        let detail =
            unmigratable_marker_detail(&weave_dir).expect("an unreadable marker has a detail");
        for field in ["primary", "project", "parent"] {
            assert!(
                detail.contains(field),
                "the detail must name `{field}` as one of the three fields to write \
                 by hand: {detail}"
            );
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

        assert!(observe_root(&plain).is_none());
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

        assert!(observe_root(&plain).is_none());
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
                pointer: ActivePointer::Absent,
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
