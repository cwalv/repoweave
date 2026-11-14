//! Workweave operations: create, delete, list, and sync workweaves.
//!
//! A workweave is a parallel working directory containing worktrees for each
//! repo in a project, including the project repo itself. Placement and
//! discovery are **recorded**, not computed: each `(primary, project)` carries
//! a workweave index (see [`crate::workweave_index`]) that names the
//! basedir `workweave create` places new workweaves under and the
//! `name → {path, parent}` inverted index every `find`-direction verb consults.
//! Each workweave still carries its own `.rwv-workweave` marker, so it is
//! self-describing without the index. The marker is its ONLY identity file:
//! `.rwv-active` is the primary root's project-selection pointer, and the two
//! are mutually exclusive.
//!
//! ## Correctness vs hygiene split
//!
//! The registry is an **advisory** index. Correctness never depends on the
//! index being uncommitted, up-to-date, or even present:
//!
//! - Every consumed entry is validated against the workweave's `.rwv-workweave`
//!   marker (round-trip: `marker.primary` canonicalizes to this primary and
//!   `marker.project` matches the queried project). A foreign or stale entry
//!   degrades to `None` — doctor prunes it as a finding.
//! - Destructive ops ([`delete_workweave`], retire) hard-require the round-trip
//!   before touching the directory. A committed / hand-edited registry cannot
//!   direct a deletion at the wrong tree.
//! - Missing index at the primary is not fatal: read paths treat it as empty,
//!   doctor's container-scoped scan reports on-disk workweaves as adoptable
//!   orphans. Silent auto-adoption in read paths is deliberately not done —
//!   adoption is a doctor act with the operator's consent.

/// The newest release that still migrates the pre-flat workweave branch shape
/// `{project}--{workweave}/<segment>`.
const PRE_FLAT_BRIDGE: &str = "0.25.0";

use crate::cli::consent::{DiscardUncommittedConsent, DiscardUnlandedConsent};
use crate::manifest::{
    LockFile, Manifest, ProjectName, RepoPath, Role, SubmoduleRecursion, WorkweaveId, WorkweaveName,
};
use crate::path_spelling::{resolve, CanonicalPath};
use crate::refusal::RefusalKind;
use crate::share::{FilePlacement, FilePrimitives, Filesystem};
use crate::symlink::LinkTarget;
use crate::vcs::{
    project_vcs, vcs_for, DestroyProof, EphemeralRefName, FoundBranch, RawRefName, RefOrigin,
    RegistrationPrune, ResolvedRevisionId, Site, Vcs, VcsError,
};
use crate::workspace::{project_dir, project_rel_dir, project_rel_path, WorkweaveMarker};
use crate::workweave_index;
use anyhow::Context;
use std::path::{Path, PathBuf};

/// Whether the member slot at `path` is a symlink, dangling or not.
///
/// Older releases materialized `reference` members that way. Every member is
/// now a worktree, and rwv no longer operates on a symlinked one: the verbs
/// that walk a workweave's members refuse it, and `rwv doctor --fix` replaces
/// it. [`Path::is_symlink`] does not follow the link, so a dangling one counts.
pub fn is_symlinked_member(path: &Path) -> bool {
    path.is_symlink()
}

/// Whether the checkout at `path` is a worktree whose link names a git
/// directory that is not there: git cannot open it, though its files stand.
pub fn is_dangling_link(path: &Path) -> bool {
    matches!(
        crate::git::git_dir_link(path),
        Some(crate::git::GitDirLink::Linked(git_dir)) if !git_dir.exists()
    )
}

/// The members of the workweave at `workweave_dir` whose slot is a symlink.
fn symlinked_members(workweave_dir: &Path, manifest: &Manifest) -> Vec<RepoPath> {
    manifest
        .iter_repo_paths()
        .filter(|repo_path| is_symlinked_member(&workweave_dir.join(repo_path.as_path())))
        .cloned()
        .collect()
}

/// Refuse to walk the members of `project` in the workspace at
/// `workspace_dir` while any of them is a symlink. A workspace that is not a
/// workweave of `project` passes: a primary's member slots are the canonical
/// clones themselves, and a symlink there is the operator's own arrangement.
pub fn refuse_symlinked_members(workspace_dir: &Path, project: &ProjectName) -> anyhow::Result<()> {
    if !is_workweave_of(workspace_dir, project)? {
        return Ok(());
    }
    refuse_symlinked_members_of(workspace_dir, &load_manifest(workspace_dir, project)?)
}

fn is_workweave_of(workspace_dir: &Path, project: &ProjectName) -> anyhow::Result<bool> {
    Ok(WorkweaveMarker::read(workspace_dir)?.is_some_and(|marker| marker.project() == project))
}

/// [`refuse_symlinked_members`] with the members `manifest` names, for a
/// caller that read the project's manifest itself.
pub fn refuse_symlinked_members_named(
    workspace_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> anyhow::Result<()> {
    if !is_workweave_of(workspace_dir, project)? {
        return Ok(());
    }
    refuse_symlinked_members_of(workspace_dir, manifest)
}

fn refuse_symlinked_members_of(workspace_dir: &Path, manifest: &Manifest) -> anyhow::Result<()> {
    let linked = symlinked_members(workspace_dir, manifest);
    if linked.is_empty() {
        return Ok(());
    }
    crate::refuse!(
        RefusalKind::WorkweaveDrift,
        "workweave {dir} holds {n} member(s) as a symlink, a shape rwv no longer operates on:\n  \
         {list}\n\n\
         Every member is now a worktree. Run `rwv -C {dir} doctor --fix` to replace each \
         link with a worktree at the revision the workweave's rwv.lock pins; doctor names \
         what a revision the canonical clone does not hold needs first. The clone a link \
         points at is left untouched.",
        dir = workspace_dir.display(),
        n = linked.len(),
        list = linked
            .iter()
            .map(|r| r.as_str())
            .collect::<Vec<_>>()
            .join("\n  "),
    )
}

/// Determine the workweave basedir for `(primary_root, project)`.
///
/// The basedir is the directory `workweave create` places new workweaves
/// under when no per-workweave `--dir` override is passed. Resolution
/// priority:
///
///   1. The `basedir` of the recorded workweave index.
///   2. `<parent-of-primary>/.workweaves` (compiled-in default).
///
/// Prefer using this in the `create` direction only. The `find` direction
/// (list, delete, sync targets, etc.) resolves via the recorded name → path
/// entries, so it does not consult this at all.
pub fn workweave_basedir(primary_root: &Path, project: &ProjectName) -> anyhow::Result<PathBuf> {
    workweave_index::resolve_basedir(primary_root, project).map(CanonicalPath::into_path_buf)
}

/// Result of validating a registry entry against its on-disk marker.
///
/// The validated variants are the only ones a destructive op or a `list`
/// entry ever consumes. The stale variants degrade to `None` at the API
/// boundary (see [`resolve_registered_workweave`]) — a foreign or stale
/// index cannot direct action; doctor is the one channel that surfaces
/// these as findings.
#[derive(Debug)]
pub enum RegistryEntryValidation {
    /// The recorded path has a `.rwv-workweave` marker whose `primary`
    /// canonicalizes to the queried primary AND whose `project` matches the
    /// queried project. Safe to act on.
    Valid,
    /// The recorded path does not exist on disk.
    MissingDirectory,
    /// The recorded path exists but has no `.rwv-workweave` marker.
    MissingMarker,
    /// The marker's `primary` does not canonicalize to the queried primary.
    ForeignPrimary,
    /// The marker's `project` differs from the queried project.
    ProjectMismatch { actual: ProjectName },
    /// Reading or parsing the marker failed. Retained separately from
    /// `MissingMarker` so callers can distinguish "file absent" from
    /// "file present but broken".
    MarkerUnreadable { detail: String },
}

/// Validate a registry entry against its on-disk `.rwv-workweave` marker.
///
/// This is the single chokepoint every consumer routes through before acting
/// on a registered path. Correctness against a stale/foreign index depends on
/// this check running.
pub fn validate_registry_entry(
    primary: &CanonicalPath,
    project: &ProjectName,
    path: &Path,
) -> RegistryEntryValidation {
    if !path.exists() {
        return RegistryEntryValidation::MissingDirectory;
    }
    let marker = match WorkweaveMarker::read(path) {
        Ok(Some(m)) => m,
        Ok(None) => return RegistryEntryValidation::MissingMarker,
        Err(e) => {
            return RegistryEntryValidation::MarkerUnreadable {
                detail: format!("{e:#}"),
            }
        }
    };
    if !marker.names_primary(primary) {
        return RegistryEntryValidation::ForeignPrimary;
    }
    if marker.project() != project {
        return RegistryEntryValidation::ProjectMismatch {
            actual: marker.project().clone(),
        };
    }
    RegistryEntryValidation::Valid
}

/// The directory the index records for `(project, name)`, unvalidated.
pub fn recorded_workweave_dir(
    primary_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
) -> anyhow::Result<Option<PathBuf>> {
    Ok(workweave_index::lookup_raw(primary_root, project, name)?.map(CanonicalPath::into_path_buf))
}

/// Look up a workweave by `(project, name)` via the recorded registry,
/// validating the marker round-trip before returning the path.
///
/// Returns `Ok(Some(path))` only when the entry exists AND its marker
/// round-trips. Any failure mode — missing index, missing entry, stale entry,
/// foreign primary, project mismatch — returns `Ok(None)`. Callers wanting a
/// destructive-op guardrail should also call [`ensure_registered_workweave`],
/// which surfaces the failure mode as an actionable error instead of silently
/// treating it as "not found".
pub fn resolve_registered_workweave(
    primary_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
) -> anyhow::Result<Option<PathBuf>> {
    let raw = match workweave_index::lookup_raw(primary_root, project, name)? {
        Some(p) => p,
        None => return Ok(None),
    };
    match validate_registry_entry(&CanonicalPath::of(primary_root), project, raw.as_path()) {
        RegistryEntryValidation::Valid => Ok(Some(raw.into_path_buf())),
        _ => Ok(None),
    }
}

/// The reverse of [`resolve_registered_workweave`]: given `path`, the name
/// `project`'s registry has recorded for it, found by matching filesystem
/// identity against every recorded entry.
///
/// For a caller that holds a path and needs the identity the registry gave
/// it. A caller with no entry to find has
/// [`crate::workspace::workweave_name_in`], which reads the basename against
/// the project it already holds — a syntax question, not a registry one.
pub fn workweave_name_for_path(
    primary_root: &Path,
    project: &ProjectName,
    path: &Path,
) -> anyhow::Result<Option<WorkweaveName>> {
    let index = match workweave_index::read(primary_root, project)? {
        Some(idx) => idx,
        None => return Ok(None),
    };
    let found = index
        .workweaves
        .into_iter()
        .find(|(_, recorded)| recorded.path.same_directory(path));
    Ok(found.map(|(name, _)| name))
}

/// Every `(project, name)` pair any project's registry records, sorted.
///
/// The population a `-w` address is resolved against: rwv renders these and
/// keeps the ones that spell what the operator typed, so an address resolves
/// to a workweave that is recorded or to none at all. Weave-wide rather than
/// per-project because the address does not say which project it names until
/// it has matched one.
///
/// Recorded, not validated: whether the matched entry still round-trips
/// through its marker is [`resolve_registered_workweave`]'s question, asked
/// once a pair is in hand.
pub fn registered_workweaves(primary_root: &Path) -> Vec<WorkweaveId> {
    let mut out = Vec::new();
    for project in crate::workspace::discover_projects(primary_root) {
        let Ok(Some(index)) = workweave_index::read(primary_root, &project) else {
            continue;
        };
        for name in index.workweaves.into_keys() {
            out.push(WorkweaveId {
                project: project.clone(),
                name,
            });
        }
    }
    out.sort();
    // A project reached twice by the walk would otherwise present one
    // workweave as two, and two matches is how a caller is told an address
    // names more than one workweave.
    out.dedup();
    out
}

/// Return the registered path for a workweave AND require the marker
/// round-trip to succeed. Used by destructive ops (`delete`, retire).
///
/// A missing entry surfaces as an actionable "no such workweave" error
/// rather than falling back to any computed path — the reconstruction
/// code path is intentionally deleted from this module, so an unknown name
/// simply has no on-disk address rwv is willing to invent.
///
/// A stale-or-foreign entry (found in the index but round-trip fails)
/// surfaces as a *distinct* error naming the specific validation failure,
/// so the operator sees the right remediation ("run `rwv doctor --fix`,
/// then retry", not "did you make a typo in the name").
pub fn ensure_registered_workweave(
    primary_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
) -> anyhow::Result<PathBuf> {
    let raw = match workweave_index::lookup_raw(primary_root, project, name)? {
        Some(p) => p,
        None => crate::refuse!(
            RefusalKind::NoSuchWorkweave,
            "no workweave named `{}` is recorded for project `{}` — either it \
             was never created, was already deleted, or exists on disk without \
             a registry entry (run `rwv doctor` to detect and adopt orphans)",
            name.as_str(),
            project.as_str()
        ),
    };
    match validate_registry_entry(&CanonicalPath::of(primary_root), project, raw.as_path()) {
        RegistryEntryValidation::Valid => {
            crate::workspace::require_agreement(
                &CanonicalPath::of(primary_root),
                project,
                raw.as_path(),
            )?;
            Ok(raw.into_path_buf())
        }
        RegistryEntryValidation::MissingDirectory => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "workweave `{}` is recorded at {} but that directory no longer \
             exists; run `rwv doctor --fix` to prune the stale entry",
            name.as_str(),
            raw.as_path().display()
        ),
        RegistryEntryValidation::MissingMarker => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "workweave `{}` is recorded at {} but the directory has no \
             `.rwv-workweave` marker — refusing to touch it. Run `rwv doctor --fix`, \
             which restores the marker from the index and the worktree relation, \
             then retry.",
            name.as_str(),
            raw.as_path().display()
        ),
        RegistryEntryValidation::ForeignPrimary => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "workweave `{}` is recorded at {} but the marker's `primary` \
             does not match this workspace — refusing to touch a foreign \
             workweave. This can arise from a hand-edited registry; run \
             `rwv doctor --fix` to restore the marker from the index and the \
             worktree relation, then retry.",
            name.as_str(),
            raw.as_path().display()
        ),
        RegistryEntryValidation::ProjectMismatch { actual } => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "workweave `{}` is recorded at {} but the marker records project \
             `{}`, not `{}` — refusing to touch it; run `rwv doctor --fix` to \
             restore the marker from the index and the worktree relation, then retry",
            name.as_str(),
            raw.as_path().display(),
            actual.as_str(),
            project.as_str()
        ),
        RegistryEntryValidation::MarkerUnreadable { detail } => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "workweave `{}` is recorded at {} but its marker could not be \
             read ({detail}); refusing to touch it. Run `rwv doctor --fix` to \
             restore it from the index and the worktree relation, then retry.",
            name.as_str(),
            raw.as_path().display()
        ),
    }
}

/// Whether `observed` is a branch this workweave's namespace could have
/// produced: the flat name itself, or a legacy `{flat}/{segment}` name from
/// before the segment was dropped.
///
/// A plain `starts_with(flat)` is wrong in the direction that matters for a
/// report: deleting workweave `feat` would list project `p`'s unrelated
/// `p--feat2` as a leftover of `p--feat`. The `/` is what makes the legacy
/// form a *sub*-namespace rather than a longer sibling name.
///
/// The minted side is brought to the parse boundary with
/// [`EphemeralRefName::to_raw`], not with `to_string()`.
fn is_this_workweaves_namespace(observed: &RawRefName, flat: &EphemeralRefName) -> bool {
    let flat = flat.to_raw();
    observed == &flat
        || observed
            .as_str()
            .starts_with(&format!("{}/", flat.as_str()))
}

/// Recursively copy a directory from `src` to `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)?.flatten() {
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// Remove orphan worktree registrations from a list of repos.
///
/// For each `(repo_abs, worktree_path)` pair, attempts `git worktree remove
/// --force <worktree_path>` first (cleans up both the on-disk directory and
/// the `.git/worktrees/` registration). If `remove` fails because the
/// on-disk directory is already gone, falls back to `git worktree prune` to
/// clear the stale administrative entry.
///
/// This is the canonical cleanup path used by:
/// - `create_workweave` rollback — called on any mid-create failure.
/// - `create_workweave`'s pre-create pass — clears any orphan registrations
///   left by a previous partial create.
///
/// # API contract for callers
///
/// ```text
/// prune_orphan_worktrees_for(&[
///     (repo_abs_path, worktree_dest_path),
///     ...
/// ]);
/// ```
///
/// The function is best-effort: errors are logged to stderr but do not
/// propagate, so a single prune failure does not block cleanup of subsequent
/// repos. This matches `delete_workweave`'s existing "continue on error, collect
/// at the end" pattern — orphan refs in a secondary repo should not prevent
/// the workweave dir from being removed.
/// `remove_dir_all` that outlasts a just-exited child's file handles.
///
/// On Windows a process that exited can hold its handles open for a beat
/// while the OS releases them, and the git children this module runs finish
/// immediately before their repo trees are removed — deletion then fails
/// with `ERROR_SHARING_VIOLATION` (os error 32), an error that resolves
/// itself when the handle closes. A bounded retry outlasts the release
/// without masking a tree something genuinely holds; every other error, and
/// every error elsewhere, returns on first sight.
pub(crate) fn remove_tree_outlasting_child_handles(dir: &Path) -> std::io::Result<()> {
    let mut last: Option<std::io::Error> = None;
    for _ in 0..10 {
        match std::fs::remove_dir_all(dir) {
            Ok(()) => return Ok(()),
            Err(e) if cfg!(windows) && e.raw_os_error() == Some(32) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.expect("loop exits early unless a retryable error was seen"))
}

pub fn prune_orphan_worktrees_for(vcs: &dyn Vcs, pairs: &[(PathBuf, PathBuf)]) {
    for (repo_abs, worktree_path) in pairs {
        if worktree_path.exists() {
            // Directory still on disk — use `remove --force` which handles
            // both the on-disk tree and the `.git/worktrees/` registration.
            if let Err(e) = vcs.remove_worktree(repo_abs, worktree_path) {
                eprintln!(
                    "rwv workweave rollback: warning: could not remove worktree {}: {e:#}",
                    worktree_path.display()
                );
                // Fall through to prune to at least clear the admin entry.
            }
        }
        // Always prune stale entries regardless of remove outcome.
        if let Err(e) = vcs.worktree_prune(repo_abs) {
            eprintln!(
                "rwv workweave rollback: warning: git worktree prune failed in {}: {e:#}",
                repo_abs.display()
            );
        }
    }
}

/// Build the `(repo_abs, worktree_dest)` pairs that
/// [`prune_orphan_worktrees_for`] operates on: `repo_abs =
/// source_root/<repo_path>`, `worktree_dest = workweave_dir/<repo_path>`.
fn orphan_prune_pairs(
    manifest: &Manifest,
    source_root: &Path,
    workweave_dir: &Path,
) -> Vec<(PathBuf, PathBuf)> {
    manifest
        .iter_repo_paths()
        .map(|repo_path| {
            (
                source_root.join(repo_path.as_path()),
                workweave_dir.join(repo_path.as_path()),
            )
        })
        .collect()
}

/// What one create attempt did to the branch it was placed on.
///
/// Rollback must not destroy a branch this create merely **adopted**:
/// deleting one takes a unique commit with it. The distinction is
/// [`Vcs::create_worktree_on`]'s answer, and this enum is where the call site
/// parks it.
enum RefBirth {
    /// `create_worktree_on` answered [`RefOrigin::Authored`]: this call wrote
    /// the ref.
    Authored,
    /// The birth call returned `Err`. `git worktree add -b` writes the branch
    /// *before* it runs post-checkout hooks, so a hook-rejected add leaves the
    /// ref behind; whatever stands at the name now is at the start point or
    /// absent.
    AuthoredOrAbsent,
    /// A branch of that name was already there. **Never** this create's to
    /// destroy.
    Adopted,
}

impl From<RefOrigin> for RefBirth {
    fn from(authorship: RefOrigin) -> Self {
        match authorship {
            RefOrigin::Authored => Self::Authored,
            RefOrigin::Adopted => Self::Adopted,
        }
    }
}

/// One repo's branch state for rollback: the branch the create placed the
/// checkout on, and what the birth call did with it.
struct RefAttempt {
    found: FoundBranch,
    birth: RefBirth,
}

/// Scope guard that rolls back a partial workweave create on drop.
///
/// Tracks:
/// - The workweave directory (`workweave_dir`): removed with `remove_dir_all`.
/// - Registered worktrees (`registered_worktrees`): each entry is
///   `(repo_abs, worktree_path)`; pruned via [`prune_orphan_worktrees_for`].
/// - Created ephemeral branches (`created_branches`): each entry is
///   `(repo_abs, branch_name)`; deleted on rollback after worktree removal.
/// - Repos to prune on rollback (`prune_on_rollback`): repos where a worktree
///   creation was ATTEMPTED (even if it failed mid-way due to a git hook).
///   A failing hook can create the worktree directory AND the `.git/worktrees/`
///   registration before returning a non-zero exit — these stale registrations
///   must be pruned even though the worktree was never fully recorded as
///   successful. After `remove_dir_all(&workweave_dir)` removes the worktree
///   path, the registration becomes prunable; running `git worktree prune` in
///   each attempted repo clears it.
///
/// Call `defuse()` to commit the create — the guard then does nothing on drop.
/// If the guard is dropped without being defused (i.e. due to any failure path,
/// including `bail!` / `?` propagation), the rollback runs automatically.
///
/// For explicit failure points (where the caller has an error to return),
/// prefer calling [`CreateRollbackGuard::rollback_and_collect_failures`]
/// before bailing so that cleanup failures can be appended to the returned
/// error message rather than only being printed to stderr.
///
/// **Design:** A single drop-based guard centralizes rollback so future code
/// cannot accidentally bypass it. Adding a new failure point that returns early
/// (via `?` or `bail!`) automatically triggers cleanup — no extra boilerplate
/// required.
struct CreateRollbackGuard {
    /// The handle the create ran through, used again to undo it. One create
    /// spans every repo in the manifest, so this is the backend of the
    /// workweave rather than of any one repo: a manifest whose entries
    /// declared different backends would need one handle per recorded
    /// worktree, not one per guard.
    vcs: Box<dyn Vcs>,
    /// The top-level workweave directory created for this attempt.
    workweave_dir: PathBuf,
    /// Pairs of `(repo_abs, worktree_dest)` for every worktree that was
    /// successfully registered during this create attempt.
    registered_worktrees: Vec<(PathBuf, PathBuf)>,
    /// Every branch this attempt placed a checkout on, with what the birth
    /// call then did. Resolved on rollback AFTER worktree removal (so the
    /// branch is no longer checked out when the DESTROY runs).
    ref_attempts: Vec<RefAttempt>,
    /// Repos where a worktree creation was attempted (success or failure).
    /// On rollback, `git worktree prune` is run in each of these repos after
    /// `remove_dir_all(&workweave_dir)` so that stale `.git/worktrees/<name>`
    /// entries left by a partial hook-failed add are cleared.
    prune_on_rollback: Vec<PathBuf>,
    /// Set to `true` when the create completes successfully OR when
    /// `rollback_and_collect_failures` has already been called. Prevents
    /// double-rollback in Drop.
    defused: bool,
}

impl CreateRollbackGuard {
    fn new(vcs: Box<dyn Vcs>, workweave_dir: PathBuf) -> Self {
        Self {
            vcs,
            workweave_dir,
            registered_worktrees: Vec::new(),
            ref_attempts: Vec::new(),
            prune_on_rollback: Vec::new(),
            defused: false,
        }
    }

    /// Record a successfully-registered worktree so it can be rolled back on failure.
    fn record_worktree(&mut self, repo_abs: PathBuf, worktree_dest: PathBuf) {
        self.registered_worktrees.push((repo_abs, worktree_dest));
    }

    /// Record a repo where a worktree creation was attempted, regardless of
    /// outcome. On rollback, `git worktree prune` will run in this repo after
    /// the workweave directory is removed, clearing any stale `.git/worktrees/`
    /// entry that a hook-failed partial `git worktree add` may have left behind.
    ///
    /// Call this BEFORE the birth call so that even a failed
    /// `create_worktree_on` leaves the repo in the prune list.
    fn record_attempted_repo(&mut self, repo_abs: PathBuf) {
        self.prune_on_rollback.push(repo_abs);
    }

    /// Record what the birth call did with the branch.
    ///
    /// Call this AFTER `create_worktree_on` returns, with whatever it
    /// returned — including on the `Err` path, where
    /// [`RefBirth::AuthoredOrAbsent`] is the honest verdict.
    fn record_ref_attempt(&mut self, found: FoundBranch, birth: RefBirth) {
        self.ref_attempts.push(RefAttempt { found, birth });
    }

    /// Commit the create — disable rollback. Call this only after
    /// `create_workweave` has fully succeeded.
    fn defuse(&mut self) {
        self.defused = true;
    }

    /// Perform rollback immediately and return a list of cleanup-failure
    /// descriptions (each item is a human-readable string describing what
    /// failed and the exact manual command to fix it).
    ///
    /// The rollback is best-effort: a failure on one repo does not skip the
    /// others. After this call the guard is defused so Drop is a no-op.
    ///
    /// Callers MUST append any returned items to the primary error message so
    /// the operator sees both the root cause and any manual-cleanup work. The
    /// original error must remain the primary error — never replace it.
    fn rollback_and_collect_failures(&mut self) -> Vec<String> {
        // Defuse first so Drop never runs this twice.
        self.defused = true;

        let mut failures: Vec<String> = Vec::new();

        // Step 1: Remove worktree directories and prune registrations for
        // repos that SUCCESSFULLY registered a worktree.
        for (repo_abs, worktree_path) in &self.registered_worktrees {
            if worktree_path.exists() {
                if let Err(e) = self.vcs.remove_worktree(repo_abs, worktree_path) {
                    eprintln!(
                        "rwv workweave rollback: warning: could not remove worktree {}: {e:#}",
                        worktree_path.display()
                    );
                    failures.push(format!(
                        "worktree {path} — remove manually with: git -C {repo} worktree remove --force {path}",
                        path = worktree_path.display(),
                        repo = repo_abs.display(),
                    ));
                }
            }
            // Always prune stale admin entries regardless of remove outcome.
            if let Err(e) = self.vcs.worktree_prune(repo_abs) {
                eprintln!(
                    "rwv workweave rollback: warning: git worktree prune failed in {}: {e:#}",
                    repo_abs.display()
                );
            }
        }

        // Step 2: Remove the partially-created workweave directory itself.
        //
        // This happens BEFORE branch deletion and the post-remove prune pass for
        // two reasons:
        // (a) Branch deletion: a branch checked out by a linked worktree cannot be
        //     force-deleted while the worktree directory exists on disk. Removing
        //     the workweave dir first ensures `git branch -D` succeeds.
        // (b) Prune: removing the workweave dir makes `.git/worktrees/<name>` admin
        //     entries prunable (they point to the now-gone directory).
        if self.workweave_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&self.workweave_dir) {
                eprintln!(
                    "rwv workweave rollback: warning: could not remove partial workweave dir {}: {e:#}",
                    self.workweave_dir.display()
                );
                failures.push(format!(
                    "partial workweave dir {dir} — remove manually with: rm -rf {dir}",
                    dir = self.workweave_dir.display(),
                ));
            }
        }

        // Step 3: Prune stale `.git/worktrees/` entries in every repo where a
        // worktree creation was ATTEMPTED (even if `git worktree add` failed
        // mid-way due to a post-checkout hook).
        //
        // Prune MUST run BEFORE branch deletion (step 4): git tracks a branch as
        // "used by worktree" based on the `.git/worktrees/<name>` registration,
        // not the on-disk directory. Even after step 2 removes the worktree
        // directory, the stale registration keeps the branch locked. Pruning
        // clears the registration so `git branch -D` succeeds.
        //
        // Step 2's dir-removal makes the admin entries prunable (they point to the
        // now-gone directory). Repos already handled in step 1 are included here
        // too — prune is idempotent.
        for repo_abs in &self.prune_on_rollback {
            if let Err(e) = self.vcs.worktree_prune(repo_abs) {
                eprintln!(
                    "rwv workweave rollback: warning: git worktree prune failed in {}: {e:#}",
                    repo_abs.display()
                );
            }
        }

        // Step 4: Undo the ref births of this attempt.
        //
        // Runs AFTER removing the workweave dir (step 2) AND after pruning stale
        // worktree registrations (step 3). The prune step is critical: git refuses
        // to force-delete a branch listed in any `.git/worktrees/<name>` entry,
        // even when the worktree directory no longer exists on disk.
        failures.extend(self.undo_ref_births());

        failures
    }

    /// Destroy the refs **this create authored**.
    ///
    /// Every destroy here holds a landed proof against the revision the
    /// create started the branch at, which [`DestroyProof::landed`] checks by
    /// ancestry. So the three ways this attempt's ref can look at rollback
    /// time are separated rather than collapsed:
    ///
    /// - present and reached by the start point → destroy it;
    /// - absent (the birth never reached the ref write) → nothing to destroy;
    /// - present and carrying a commit the start point does not reach →
    ///   something has committed on it since. Leave it and tell the operator.
    ///
    /// An [`RefBirth::Adopted`] ref is never destroyed at all, whatever its
    /// tip: this create did not write it.
    fn undo_ref_births(&self) -> Vec<String> {
        let vcs = self.vcs.as_ref();
        let mut failures: Vec<String> = Vec::new();

        for attempt in &self.ref_attempts {
            if matches!(attempt.birth, RefBirth::Adopted) {
                continue;
            }
            let found = &attempt.found;
            let store = found.store().to_path_buf();
            match DestroyProof::landed(
                vcs,
                &store,
                Site::Local,
                found.name(),
                std::slice::from_ref(found.start()),
            ) {
                Ok(Some(proof)) => {
                    if let Err(e) = vcs.delete_branch(proof) {
                        eprintln!(
                            "rwv workweave rollback: warning: could not delete ephemeral \
                             branch {found} in {}: {e:#}",
                            store.display()
                        );
                        failures.push(format!(
                            "branch {found} in {repo} — delete manually with: \
                             git -C {repo} branch -D {found}",
                            repo = store.display(),
                        ));
                    }
                }
                Ok(None) => match vcs.resolve_local_branch_tip(&store, found.name()) {
                    Ok(None) => {}
                    _ => failures.push(format!(
                        "branch {found} in {repo} holds commits past the revision rwv created \
                         it at ({at}); left in place — inspect it with \
                         `git -C {repo} log {found}` and delete it by hand if it holds \
                         nothing",
                        repo = store.display(),
                        at = found.start().display_str(),
                    )),
                },
                Err(e) => failures.push(format!(
                    "branch {found} in {repo} could not be checked: {e:#}",
                    repo = store.display(),
                )),
            }
        }

        failures
    }
}

impl Drop for CreateRollbackGuard {
    fn drop(&mut self) {
        if self.defused {
            return;
        }

        // Drop-based rollback path: used for `?` propagation failures.
        // Cleanup failures are printed to stderr only (no caller to append to).
        // For explicit `bail!` paths, prefer calling `rollback_and_collect_failures`
        // first so cleanup failures appear in the returned error.
        //
        // 1. Prune orphan worktree registrations and remove worktree directories
        //    for repos that SUCCESSFULLY registered (same as registered_worktrees).
        prune_orphan_worktrees_for(self.vcs.as_ref(), &self.registered_worktrees);

        let vcs = self.vcs.as_ref();

        // 2. Remove the partially-created workweave directory.
        //    Must happen BEFORE branch deletion: git refuses to force-delete a
        //    branch checked out by a live linked worktree; removing the dir first
        //    disconnects the worktree so the delete can proceed.
        //    Must also happen BEFORE prune: removal makes stale entries prunable.
        if self.workweave_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&self.workweave_dir) {
                eprintln!(
                    "rwv workweave rollback: warning: could not remove partial workweave dir {}: {e:#}",
                    self.workweave_dir.display()
                );
            }
        }

        // 3. Prune stale `.git/worktrees/` entries in repos where worktree
        //    creation was attempted. Prune BEFORE branch deletion: git refuses
        //    to force-delete a branch registered in any worktree entry, even
        //    when the worktree directory no longer exists on disk.
        for repo_abs in &self.prune_on_rollback {
            if let Err(e) = vcs.worktree_prune(repo_abs) {
                eprintln!(
                    "rwv workweave rollback: warning: git worktree prune failed in {}: {e:#}",
                    repo_abs.display()
                );
            }
        }

        // 4. Undo the ref births of this attempt — same landed-proof
        //    discipline as the explicit path; see `undo_ref_births`.
        //    Runs AFTER removing the workweave dir (step 2) and pruning stale
        //    worktree registrations (step 3).
        for failure in self.undo_ref_births() {
            eprintln!("rwv workweave rollback: warning: {failure}");
        }
    }
}

/// Pre-flight check: verify that every git repo involved in a workweave create
/// has at least one commit (i.e., HEAD resolves).
///
/// Checks:
/// - The project repo at `source_root/projects/<project>/` (if it is a git repo).
/// - Every repo listed in `manifest.repositories` at `source_root/<repo_path>/`.
///
/// Returns `Ok(())` when all repos are commit-bearing. Returns a structured
/// error naming every missing-HEAD path and the suggested fix for each.
///
/// Designed to be called BEFORE any disk mutation so that a fresh-project
/// failure leaves no partial workweave directory on disk.
pub fn preflight_check_heads(
    project_vcs: &dyn Vcs,
    source_root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> anyhow::Result<()> {
    let mut missing: Vec<String> = Vec::new();

    // Check project repo.
    let project_dir = project_dir(source_root, project);
    if project_vcs.is_repo(&project_dir) && project_vcs.head_revision(&project_dir).is_err() {
        missing.push(format!(
            "project {name} has no commits yet — commit its files \
             (\"git -C projects/{name} commit\") before creating a workweave.",
            name = project.as_str(),
        ));
    }

    // Check each manifest repo.
    for (repo_path, entry) in &manifest.repositories {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = source_root.join(repo_path.as_path());
        if vcs.is_repo(&repo_abs) && vcs.head_revision(&repo_abs).is_err() {
            missing.push(format!(
                "repo {path} has no commits yet — run \
                 \"git -C {path} commit\" to create an initial commit before \
                 creating a workweave.",
                path = repo_path.as_str(),
            ));
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    crate::refuse!(
        RefusalKind::RepoWithoutCommits,
        "rwv workweave create: cannot create workweave — the following repos \
         have no commits yet:\n  {}",
        missing.join("\n  ")
    )
}

/// [`crate::share::populate_submodules`] for a member whose manifest entry
/// asks for `recurse-submodules`, reporting each under `prefix`; the count left
/// uninitialized.
pub(crate) fn populate_member_submodules(
    vcs: &dyn Vcs,
    member: &Path,
    repo_path: &RepoPath,
    role: Role,
    recurse_submodules: SubmoduleRecursion,
    prefix: &str,
) -> usize {
    if recurse_submodules == SubmoduleRecursion::Skip {
        return 0;
    }
    let populated = crate::share::populate_submodules(vcs, member, role, &Filesystem);
    populated.report(prefix, repo_path);
    populated.left.len()
}

/// [`crate::share::fetch_submodules`] for the canonical clone of a member
/// whose manifest entry asks for `recurse-submodules`, reporting each under `prefix`.
pub(crate) fn fetch_member_submodules(
    vcs: &dyn Vcs,
    canonical: &Path,
    repo_path: &RepoPath,
    role: Role,
    recurse_submodules: SubmoduleRecursion,
    prefix: &str,
) {
    if recurse_submodules == SubmoduleRecursion::Skip {
        return;
    }
    crate::share::fetch_submodules(vcs, canonical, role).report(prefix, repo_path);
}

/// Scan `.gitmodules` in `worktree_path` and return the submodule `path =`
/// values whose on-disk directory is empty (or absent), indicating that
/// submodules were never initialized.
///
/// Returns an empty `Vec` when:
/// - `.gitmodules` does not exist (no submodules declared), or
/// - all listed submodule paths are non-empty directories on disk.
///
/// This is a **local-only** check (stat the path; no network). A non-empty
/// submodule directory is assumed to be correctly initialized; this scanner
/// does not verify the content against any remote.
pub fn scan_uninitialized_submodules(worktree_path: &Path) -> Vec<String> {
    let gitmodules = worktree_path.join(".gitmodules");
    if !gitmodules.exists() {
        return Vec::new();
    }
    let content = match std::fs::read_to_string(&gitmodules) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    // Parse `path = <value>` lines from .gitmodules. We don't need a full INI
    // parser — just extract the `path =` values.
    let mut empty_paths = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("path") {
            let rest = rest.trim();
            if let Some(value) = rest.strip_prefix('=') {
                let sub_path = value.trim();
                if sub_path.is_empty() {
                    continue;
                }
                let sub_dir = worktree_path.join(sub_path);
                // A submodule dir should be a non-empty directory. An absent
                // dir or an empty dir means submodule init has not run.
                let is_empty = if sub_dir.is_dir() {
                    std::fs::read_dir(&sub_dir)
                        .map(|mut rd| rd.next().is_none())
                        .unwrap_or(true)
                } else {
                    true
                };
                if is_empty {
                    empty_paths.push(sub_path.to_string());
                }
            }
        }
    }
    empty_paths
}

/// One repo's ephemeral-ref birth: what the rollback guard must be told, and
/// whether the birth call itself failed.
///
/// The two are separate because they fail at different moments. Anything that
/// goes wrong *before* the branch is placed is a plain `Err` from
/// [`birth_ephemeral_worktree`] — nothing was claimed, so there is nothing to
/// roll back. Once the birth is attempted the attempt is always reportable,
/// and a failed birth rides back inside `failure` rather than losing the
/// branch with it.
pub(crate) struct BirthOutcome {
    found: FoundBranch,
    birth: RefBirth,
    pub(crate) failure: Option<anyhow::Error>,
    /// How the born worktree's files were placed, when they were shared and
    /// the backend could say.
    pub(crate) placement: Option<FilePlacement>,
}

/// What a birth does with a branch of its name that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BranchPolicy {
    /// A workweave being created: a leftover branch standing at the start
    /// point is the create's own partial result and is taken; one anywhere
    /// else is refused.
    Fresh,
    /// A workweave's own member being restored: the branch is checked out
    /// where it stands, never moved or deleted.
    Restore,
}

/// Where a born worktree's tracked files come from.
pub(crate) enum WorktreeFiles<'a> {
    /// The VCS writes every one.
    Written,
    /// Placed from the repository's other workspaces wherever the filesystem
    /// and the member's role allow; the VCS writes the rest.
    Shared(&'a dyn FilePrimitives, Role),
}

impl BirthOutcome {
    /// The birth's failure, when it had one.
    pub(crate) fn into_result(self) -> anyhow::Result<()> {
        match self.failure {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// The canonical store a checkout's refs actually live in.
///
/// `create` sees the *fork source* (`source_root/<repo>`, which is a linked
/// worktree when forking from another workweave) and `delete` sees the
/// workweave's own checkout; both must resolve to the same store, or a branch
/// a create made is invisible to the delete that has to judge it. Resolving
/// through git's common-dir is what makes that true — the path each verb
/// happens to hold is not.
pub(crate) fn store_behind(vcs: &dyn Vcs, checkout: &Path) -> PathBuf {
    resolved_worktree_parent(vcs, checkout, checkout).into_path_buf()
}

/// A member worktree [`materialize_member`] gave a workweave.
pub(crate) struct Materialized {
    /// The revision its HEAD stands at.
    pub(crate) at: ResolvedRevisionId,
    /// How its files were placed, when the backend could say.
    pub(crate) files: Option<FilePlacement>,
    /// What survived in the canonical clone for it, and so what was done.
    pub(crate) relink: Relink,
    /// The workweave's ephemeral branch it is on; none where the member sits
    /// detached at its pin.
    pub(crate) branch: Option<EphemeralRefName>,
}

/// How [`materialize_member`] gave a workweave its member's worktree, decided
/// by what the canonical clone still held for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Relink {
    /// The clone still registered the checkout; only its link was rewritten,
    /// so HEAD, index and files are as they were.
    Repaired,
    /// The workweave's branch survived and the checkout was put on it where
    /// it stands.
    Attached,
    /// Neither survived: the branch was created at the revision asked for.
    Created,
}

impl Materialized {
    /// What was done, for a line naming the checkout before it.
    pub(crate) fn summary(&self) -> String {
        let at = self.at.id().short();
        let Some(branch) = &self.branch else {
            return format!("detached at {at}");
        };
        match self.relink {
            Relink::Repaired => format!(
                "relinked through the registration the canonical clone kept, on `{branch}` at \
                 {at}"
            ),
            Relink::Attached => format!("attached to its branch `{branch}` at its own tip {at}"),
            Relink::Created => format!("on branch `{branch}`, created at {at}"),
        }
    }
}

/// Give the workweave at `workweave_dir` the workspace it lacks for
/// `repo_path`: nothing at the slot, a symlink there (unlinked first; what it
/// pointed at is never touched), or a checkout whose link to the canonical
/// clone under `primary_root` dangles.
///
/// What the canonical clone still holds decides how, and no ref is moved or
/// deleted to get there:
/// - a registration of the slot: the link is repaired;
/// - the workweave's ephemeral branch: the worktree is attached to it at its
///   own tip, which may hold commits the lock does not;
/// - neither: the branch is created at `start`.
///
/// A registration whose directory is gone is dropped first, unless it is
/// locked. A dangling checkout keeps its files: they are registered in place.
///
/// With [`BranchPolicy::Fresh`] a surviving branch is not attached: the
/// worktree must stand at `start`, so a branch standing elsewhere is refused
/// as a workweave create refuses it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_member(
    vcs: &dyn Vcs,
    primary_root: &Path,
    workweave_dir: &Path,
    project: &ProjectName,
    workweave: &WorkweaveName,
    repo_path: &RepoPath,
    role: Role,
    recurse_submodules: SubmoduleRecursion,
    start: &str,
    policy: BranchPolicy,
) -> anyhow::Result<Materialized> {
    let canonical = primary_root.join(repo_path.as_path());
    if !canonical.exists() {
        crate::refuse!(
            RefusalKind::MissingCanonicalClone,
            "canonical clone for {repo_path} missing at {}; \
             run `rwv update --project {project}` at the primary weave first to materialize it \
             there",
            canonical.display()
        );
    }
    let dest = workweave_dir.join(repo_path.as_path());
    if is_symlinked_member(&dest) {
        crate::symlink::remove(&dest)
            .with_context(|| format!("failed to unlink the symlink at {}", dest.display()))?;
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let branch = EphemeralRefName::mint(
        &WorkweaveId {
            project: project.clone(),
            name: workweave.clone(),
        }
        .flat(),
    );
    let in_place = is_dangling_link(&dest);
    if in_place {
        if let Some(registration) = vcs.worktree_registration(&canonical, &dest)? {
            vcs.reattach_registered_worktree(&canonical, &dest, &registration)
                .with_context(|| format!("cannot relink {repo_path}"))?;
            return Ok(Materialized {
                at: vcs.head_revision(&dest)?,
                files: None,
                relink: Relink::Repaired,
                branch: Some(branch),
            });
        }
    } else if vcs.prune_stale_worktree_registration(&canonical, &dest)? == RegistrationPrune::Locked
    {
        crate::refuse!(
            RefusalKind::WorkweaveDrift,
            "{repo_path}: the canonical clone at {} holds a locked registration of {}, whose \
             directory is gone, so no worktree can be made there. Unlock it with `git -C {} \
             worktree unlock {}` if nothing else uses it, then re-run.",
            canonical.display(),
            dest.display(),
            canonical.display(),
            dest.display(),
        );
    }

    if !in_place && matches!(role, Role::Dependency | Role::Reference) {
        let at = vcs
            .resolve_revision(&canonical, start)
            .with_context(|| format!("failed to resolve {start} in canonical clone"))?;
        let files =
            crate::share::create_detached_populated(vcs, &canonical, &dest, &at, role, &Filesystem)
                .with_context(|| format!("cannot materialize {repo_path}"))?;
        crate::settle::settle(vcs, &dest, role)?;
        populate_member_submodules(vcs, &dest, repo_path, role, recurse_submodules, "rwv: ");
        return Ok(Materialized {
            at,
            files,
            relink: Relink::Created,
            branch: None,
        });
    }

    let store = store_behind(vcs, &canonical);
    let raw = branch.to_raw();
    let surviving = match policy {
        BranchPolicy::Restore => vcs.resolve_local_branch_tip(&store, &raw)?,
        BranchPolicy::Fresh => None,
    };
    let (at, relink) = match surviving {
        Some(tip) => (tip, Relink::Attached),
        None => (
            vcs.resolve_revision(&canonical, start)
                .with_context(|| format!("failed to resolve {start} in canonical clone"))?,
            Relink::Created,
        ),
    };
    let files = if in_place {
        attach_in_place(vcs, &canonical, &dest, &branch, &at, relink)
            .with_context(|| format!("cannot relink {repo_path}"))?;
        None
    } else {
        let outcome = birth_ephemeral_worktree(
            vcs,
            &canonical,
            &dest,
            &branch,
            at.clone(),
            WorktreeFiles::Shared(&Filesystem, role),
            policy,
        )
        .with_context(|| format!("cannot materialize {repo_path}"))?;
        let files = outcome.placement;
        outcome
            .into_result()
            .with_context(|| format!("worktree add for {repo_path} failed"))?;
        files
    };
    crate::settle::settle(vcs, &dest, role)?;
    populate_member_submodules(vcs, &dest, repo_path, role, recurse_submodules, "rwv: ");
    Ok(Materialized {
        at,
        files,
        relink,
        branch: Some(branch),
    })
}

/// Register the files standing at `dest` as a worktree on `branch`: attached
/// at `at`, its tip, when it exists, created there otherwise.
fn attach_in_place(
    vcs: &dyn Vcs,
    canonical: &Path,
    dest: &Path,
    branch: &EphemeralRefName,
    at: &ResolvedRevisionId,
    relink: Relink,
) -> anyhow::Result<()> {
    let raw = branch.to_raw();
    match relink {
        Relink::Attached => vcs.register_worktree_in_place(canonical, dest, &raw, None)?,
        Relink::Created | Relink::Repaired => {
            vcs.register_worktree_in_place(canonical, dest, &raw, Some(at))?
        }
    }
    Ok(())
}

/// Whether `store`, the store the VCS resolved for `checkout`, is the one
/// `checkout` holds itself — a full clone, not a workspace linked into a store
/// elsewhere.
pub(crate) fn is_own_store(store: &Path, checkout: &Path) -> bool {
    CanonicalPath::of(store) == CanonicalPath::of(&crate::git::store_path_in(checkout))
}

/// Delete the workweave's branch in `canonical`'s store when everything on it
/// is reached by `start`, so the worktree that replaces a standalone clone is
/// born on a branch at the commit the clone had checked out. A branch holding
/// anything `start` lacks stays, and the birth refuses it.
fn release_branch_the_clone_reaches(
    vcs: &dyn Vcs,
    canonical: &Path,
    project: &ProjectName,
    workweave: &WorkweaveName,
    start: &ResolvedRevisionId,
) -> anyhow::Result<()> {
    let store = store_behind(vcs, canonical);
    let branch = EphemeralRefName::mint(
        &WorkweaveId {
            project: project.clone(),
            name: workweave.clone(),
        }
        .flat(),
    )
    .to_raw();
    if let Some(proof) = DestroyProof::landed(
        vcs,
        &store,
        Site::Local,
        &branch,
        std::slice::from_ref(start),
    )? {
        vcs.delete_branch(proof)?;
    }
    Ok(())
}

/// Replace the full clone at `repo_path` in the workweave at `workweave_dir`
/// with the worktree [`materialize_member`] gives a member the workweave
/// lacks, born at the revision the clone has checked out so the files on disk
/// read as they did.
///
/// [`refuse_unless_replaceable`] runs first. The clone is renamed aside while
/// the worktree is born and removed once it is; a birth that fails before
/// anything stands at the slot renames it back.
#[allow(clippy::too_many_arguments)]
pub(crate) fn replace_standalone_member(
    vcs: &dyn Vcs,
    primary_root: &Path,
    workweave_dir: &Path,
    project: &ProjectName,
    workweave: &WorkweaveName,
    repo_path: &RepoPath,
    role: Role,
    recurse_submodules: SubmoduleRecursion,
) -> anyhow::Result<Materialized> {
    let slot = workweave_dir.join(repo_path.as_path());
    let canonical = primary_root.join(repo_path.as_path());
    let leaf = repo_path
        .as_str()
        .rsplit_once('/')
        .map_or(repo_path.as_str(), |(_, leaf)| leaf);
    let aside = slot.with_file_name(format!(".{leaf}.rwv-replaced"));
    let at = refuse_unless_replaceable(vcs, project, &slot, &canonical, &aside)?;

    std::fs::rename(&slot, &aside).with_context(|| {
        format!(
            "failed to move the full clone at {} aside to {}",
            slot.display(),
            aside.display()
        )
    })?;

    // A worktree once registered at the slot is stale now that the slot is
    // empty, and git refuses to add one where a registration stands.
    let born = vcs
        .worktree_prune(&canonical)
        .map_err(anyhow::Error::from)
        .and_then(|()| {
            release_branch_the_clone_reaches(vcs, &canonical, project, workweave, &at)?;
            materialize_member(
                vcs,
                primary_root,
                workweave_dir,
                project,
                workweave,
                repo_path,
                role,
                recurse_submodules,
                at.id().as_str(),
                BranchPolicy::Fresh,
            )
        });
    match born {
        Ok(born) => {
            remove_tree_outlasting_child_handles(&aside).with_context(|| {
                format!(
                    "created the worktree for {repo_path}, but the full clone it replaced is \
                     still at {}",
                    aside.display()
                )
            })?;
            Ok(born)
        }
        Err(e) if slot.symlink_metadata().is_err() => match std::fs::rename(&aside, &slot) {
            Ok(()) => Err(e),
            Err(restore) => Err(e.context(format!(
                "the full clone of {repo_path} is left at {} (moving it back failed: {restore})",
                aside.display()
            ))),
        },
        Err(e) => Err(e.context(format!(
            "the full clone of {repo_path} is left at {}",
            aside.display()
        ))),
    }
}

/// Refuse to replace the full clone at `standalone` while it holds anything
/// the canonical clone at `canonical` would not give back, while there is no
/// full clone at `canonical` to link a worktree into, or while something
/// already stands at `aside`, where the clone is to be moved. Every reason is
/// reported, not just the first. Returns the revision `standalone` has
/// checked out.
fn refuse_unless_replaceable(
    vcs: &dyn Vcs,
    project: &ProjectName,
    standalone: &Path,
    canonical: &Path,
    aside: &Path,
) -> anyhow::Result<ResolvedRevisionId> {
    let listed = |items: &[String]| format!("\n      {}", items.join("\n      "));
    let mut reasons: Vec<String> = Vec::new();

    if aside.symlink_metadata().is_ok() {
        reasons.push(format!(
            "{} already exists, and the clone is moved there while its worktree is \
             created. An interrupted repair leaves one behind: check that nothing in it is \
             needed, then remove it.",
            aside.display()
        ));
    }

    let canonical_is_clone = match vcs.store_of(canonical) {
        None => {
            reasons.push(format!(
                "there is no canonical clone at {} to link a worktree into; \
                 `rwv update --project {project}` in the primary weave materializes it.",
                canonical.display()
            ));
            false
        }
        Some(store) if !is_own_store(&store, canonical) => {
            reasons.push(format!(
                "the canonical slot {} is itself a worktree of {}, not a full clone to link \
                 a worktree into; `rwv doctor` reports it as clone-topology.",
                canonical.display(),
                store.display()
            ));
            false
        }
        Some(_) => true,
    };

    let own = CanonicalPath::of(standalone);
    let dependents: Vec<String> = vcs
        .list_worktrees(standalone)?
        .into_iter()
        .filter(|wt| CanonicalPath::of(wt) != own)
        .map(|wt| wt.display().to_string())
        .collect();
    if !dependents.is_empty() {
        reasons.push(format!(
            "other worktrees link into its store, and removing it would orphan them; \
             remove them first:{}",
            listed(&dependents)
        ));
    }

    let dirty = vcs.dirty_file_names(standalone)?;
    if !dirty.is_empty() {
        reasons.push(format!(
            "it holds uncommitted or untracked files; commit, move or delete them:{}",
            listed(&dirty)
        ));
    }

    let ignored = vcs.ignored_file_names(standalone)?;
    if !ignored.is_empty() {
        reasons.push(format!(
            "it holds ignored files, which no commit records either. Move out any you \
             need; if the rest are regenerable, such as build output or installed \
             dependencies, {}:{}",
            vcs.ignored_removal_hint(standalone),
            listed(&ignored)
        ));
    }

    if canonical_is_clone {
        let unreached = vcs.refs_unreachable_from(standalone, canonical)?;
        if !unreached.is_empty() {
            reasons.push(format!(
                "these refs hold commits no ref of the canonical clone at {} reaches; land \
                 those commits there, or delete the refs:{}",
                canonical.display(),
                listed(&unreached)
            ));
        }
    }

    let at = match vcs.resolve_revision(standalone, "HEAD") {
        Ok(at) => Some(at),
        Err(VcsError::RevisionNotFound { .. }) => {
            reasons.push("its HEAD names no commit to create the worktree at.".to_owned());
            None
        }
        Err(e) => return Err(e.into()),
    };

    match at {
        Some(at) if reasons.is_empty() => Ok(at),
        _ => crate::refuse!(
            RefusalKind::StandaloneInWorkweave,
            "not replacing the full clone at {} with a worktree of the canonical clone:\n  - {}\n\n\
             Nothing was changed. Re-run `rwv doctor --fix` once each of these is resolved.",
            standalone.display(),
            reasons.join("\n  - "),
        ),
    }
}

/// Whether a hook refused the operation this error reports.
///
/// The birth path converts its [`VcsError`] to `anyhow::Error` before a caller
/// sees it, so the variant is reached by downcast. anyhow sees through
/// `context` layers; an intermediate that reformats the error into a fresh
/// message does not, and would silently answer `false` here.
fn is_hook_rejection(e: &anyhow::Error) -> bool {
    matches!(
        e.downcast_ref::<VcsError>(),
        Some(VcsError::HookRejected { .. })
    )
}

/// Materialize a worktree at `dest` on this workweave's ephemeral ref in the
/// store behind `source_repo`.
///
/// The name is minted flat ([`EphemeralRefName::mint`]) and nothing observed
/// feeds into it, so there is no third component for two call sites to
/// derive differently.
///
/// The ref begins at `start`. It is the caller's because callers differ on
/// it — a create forks the source checkout's HEAD, while a member a workweave
/// lacks starts at the revision the verb giving it one aims it at.
///
/// # The states of the ref, and why none of them deletes it
///
/// The shipped retry ran `git branch -D` on "already exists" and tried again,
/// which destroyed a branch on nothing but a name match. A birth destroys
/// nothing: it classifies first.
///
/// - **absent** — the ordinary create.
/// - **present at `start`** — the create's own partial result, left by an
///   interrupted run of the same verb; taken, so re-running completes it.
/// - **present anywhere else** — a branch holding commits this create did not
///   put there. Refused under [`BranchPolicy::Fresh`], naming its tip; under
///   [`BranchPolicy::Restore`] it is the branch the workweave's member was
///   on, and the worktree is put on it where it stands.
pub(crate) fn birth_ephemeral_worktree(
    vcs: &dyn Vcs,
    source_repo: &Path,
    dest: &Path,
    ephemeral: &EphemeralRefName,
    start: ResolvedRevisionId,
    files: WorktreeFiles<'_>,
    policy: BranchPolicy,
) -> anyhow::Result<BirthOutcome> {
    let store = store_behind(vcs, source_repo);
    let raw = ephemeral.to_raw();

    // Classify the D/F collision before asking git, so it surfaces as a
    // migration error naming a command rather than as `fatal: cannot lock
    // ref`. git cannot hold both `refs/heads/p--ww` and `refs/heads/p--ww/x`,
    // so any pre-flat-name branch in this namespace blocks the flat one.
    let occupied: Vec<RawRefName> = vcs
        .list_branch_names_with_prefix(&store, raw.as_str())?
        .into_iter()
        .filter(|b| is_this_workweaves_namespace(b, ephemeral) && b != &raw)
        .collect();
    if !occupied.is_empty() {
        crate::refuse!(
            RefusalKind::PreFlatBranchShape,
            "cannot create branch `{ephemeral}` in {store}: {n} branch(es) already occupy \
             its namespace, and git cannot hold both `refs/heads/{ephemeral}` and \
             `refs/heads/{ephemeral}/...`:\n  {list}\n\n\
             These carry the shape `{{project}}--{{workweave}}/<segment>`, which rwv minted \
             before version {bridge} and no longer migrates. Install rwv {bridge}, run `rwv doctor \
             --fix --all` until `rwv doctor --all` reports clean, then upgrade again; a branch \
             you made yourself is yours to rename or delete.",
            store = store.display(),
            n = occupied.len(),
            bridge = PRE_FLAT_BRIDGE,
            list = occupied
                .iter()
                .map(|b| b.as_str().to_owned())
                .collect::<Vec<_>>()
                .join("\n  "),
        );
    }

    if policy == BranchPolicy::Fresh {
        if let Some(tip) = vcs.resolve_local_branch_tip(&store, &raw)? {
            if tip != start {
                return Err(crate::diagnosis::State::NameExists
                    .of(store.display())
                    .subject(ephemeral)
                    .listing([format!(
                        "at {}; this create would have started it at {}",
                        tip.display_str(),
                        start.display_str()
                    )])
                    .judge(vcs)
                    .refused());
            }
        }
    }

    let found = FoundBranch::minted(store, ephemeral, start);
    let populated = match files {
        WorktreeFiles::Written => crate::share::Populated {
            created: vcs.create_worktree_on(&found, dest),
            filled: Ok(None),
        },
        WorktreeFiles::Shared(fs, role) => {
            crate::share::create_worktree_populated(vcs, &found, dest, role, fs)
        }
    };
    let (placement, filling_failed) = match populated.filled {
        Ok(placement) => (placement, None),
        Err(e) => (None, Some(e)),
    };
    let (birth, failure) = match populated.created {
        Ok(authorship) => (RefBirth::from(authorship), filling_failed),
        Err(e) => (RefBirth::AuthoredOrAbsent, Some(e.into())),
    };
    Ok(BirthOutcome {
        found,
        birth,
        failure,
        placement,
    })
}

/// Where a detached member of a new workweave starts: the commit the source's
/// lock pins for it where this clone holds it, else the source's HEAD.
fn pinned_start(
    lock: &Option<LockFile>,
    vcs: &dyn Vcs,
    repo: &Path,
    repo_path: &RepoPath,
) -> anyhow::Result<ResolvedRevisionId> {
    if let Some(entry) = lock.as_ref().and_then(|lock| lock.get_entry(repo_path)) {
        if let Ok(at) = vcs.resolve_revision(repo, entry.version.as_str()) {
            return Ok(at);
        }
    }
    Ok(vcs.head_revision(repo)?)
}

/// Copy, byte for byte, an enabled integration's managed or generated file
/// that the project worktree checkout above cannot carry: the checkout is a
/// git worktree, so it holds only what `source_project_dir`'s git tracks, and
/// a file like cargo-workspace's gitignored `.cargo/config.toml` is deliberately
/// never tracked.
///
/// Three conditions per declared path, all required:
/// - `dest_project_dir` lacks the file — a context verb authors nothing, so a
///   file already there (carried by an earlier step, or genuinely tracked and
///   checked out) is left exactly as it is.
/// - the source has it as a regular file. Never a symlink: a symlink at this
///   path is a weave-root surfacing link or operator content, neither of
///   which this is the carry for.
/// - the seat's project VCS does not track the path — a tracked file is the
///   worktree's own checkout of it, authoritative there on its own terms.
///
/// A source that also lacks the file carries nothing, so the seat lacks it
/// too and the verify pass that follows reports it there exactly as it does
/// at the source. This moves bytes a VCS fork drops; it is not authoring —
/// nothing here is `Integration::activate()`, and nothing here runs for a
/// path the source itself never materialized.
fn carry_untracked_managed_files(
    vcs: &dyn Vcs,
    source_root: &Path,
    project: &ProjectName,
    manifest: &Manifest,
    source_project_dir: &Path,
    dest_project_dir: &Path,
) -> anyhow::Result<()> {
    for name in crate::activate::own_copy_owned_paths(source_root, project, manifest) {
        let dest_file = dest_project_dir.join(crate::path_spelling::native_relative(&name));
        refuse_symlinked_component(dest_project_dir, Path::new(&name))?;
        if dest_file.exists() {
            continue;
        }
        let source_file = source_project_dir.join(crate::path_spelling::native_relative(&name));
        let Ok(source_meta) = std::fs::symlink_metadata(&source_file) else {
            continue;
        };
        if !source_meta.file_type().is_file() {
            continue;
        }
        if vcs
            .is_tracked(dest_project_dir, Path::new(&name))
            .unwrap_or(true)
        {
            continue;
        }
        if let Some(parent) = dest_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&source_file, &dest_file).with_context(|| {
            format!(
                "carrying {} into {}",
                source_file.display(),
                dest_file.display()
            )
        })?;
    }
    Ok(())
}

/// Fails if any component of `rel` under `root` is a symlink, dangling or not.
/// A copy through one would land outside `root`; `symlink_metadata` needs no
/// privilege and answers the same on every platform.
fn refuse_symlinked_component(root: &Path, rel: &Path) -> anyhow::Result<()> {
    let mut walked = root.to_path_buf();
    for component in rel.components() {
        walked.push(component);
        match std::fs::symlink_metadata(&walked) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(crate::untokened!(
                    Owed,
                    "refusing to carry {}: {} is a symlink",
                    rel.display(),
                    walked.display()
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Which branch the new workweave's project repo is on, and how it came to
/// be there.
#[derive(Debug, Clone)]
pub enum BranchChoice {
    /// A feature under the name minted from the workweave's identity.
    Minted,
    /// A new branch of this name, created as `branch create` creates one.
    New(String),
    /// An existing branch, opened by its record.
    Open(String),
    /// No branch: a detached workspace at this revision.
    Detach(String),
}

/// Where `rwv workweave create` puts the workweave. Every path is already
/// absolute.
pub enum Placement {
    /// At `<basedir>/<project>--<name>`, the basedir the project records
    /// ([`workweave_basedir`]).
    DefaultBasedir,
    /// At `<basedir>/<project>--<name>` for this run, in place of the
    /// recorded one.
    Basedir(PathBuf),
    /// At exactly this path.
    Dir(PathBuf),
}

/// What `rwv workweave create` is asked to make. The two roots play different
/// roles, so each is named where it is given.
pub struct CreateWorkweave<'a> {
    /// The surrounding weave: where new workweaves live
    /// (`<primary_parent>/.workweaves/`), and the primary recorded in the
    /// `.rwv-workweave` marker.
    pub primary_root: &'a Path,
    /// The workspace forked from: the manifest, per-repo HEADs,
    /// `projects/<project>/` worktree, and `workweave:` copy/link sources are
    /// all read from it. The primary itself when forking from primary; when an
    /// agent forks a peer from inside its own workweave, that workweave.
    pub source_root: &'a Path,
    pub project: &'a ProjectName,
    pub name: &'a WorkweaveName,
    pub branch: BranchChoice,
    /// How a created or opened branch is used, where its record does not say.
    pub spelling: Option<crate::switch::Spelling>,
    /// The revision a created branch starts at.
    pub at: Option<String>,
    /// The member entries the branch's first commit sets.
    pub entries: Vec<crate::member_branch::BranchEntry>,
    /// Where the workweave lands. Whichever way it is chosen, the resulting
    /// absolute path is recorded in the registry, so a per-workweave placement
    /// is as visible to a later `list` / `delete` / doctor as a default one.
    pub placement: &'a Placement,
    /// What the workweave's activation runs with; its `no_materialize` skips
    /// the install hooks.
    pub activation: crate::activate::ActivateOptions,
}

const CREATE: &str = "workweave create";

/// What a create decided before it changed anything on disk.
struct Intent {
    start: ResolvedRevisionId,
    head: ResolvedRevisionId,
    branch: Option<crate::records::BranchName>,
    creating: Option<crate::records::Record>,
    opening: Option<crate::switch::Opening>,
    steps: Vec<crate::member_branch::EntryStep>,
}

impl Intent {
    fn feature(&self) -> bool {
        matches!(self.creating, Some(crate::records::Record::Feature { .. }))
    }
}

#[allow(clippy::too_many_arguments)]
fn judge_intent(
    ctx: &crate::workspace::WorkspaceContext,
    vcs: &dyn Vcs,
    project: &ProjectName,
    source_project: &Path,
    flat: &crate::naming::FlatAddress,
    branch: &BranchChoice,
    spelling: Option<&crate::switch::Spelling>,
    at: Option<&str>,
    entries: &[crate::member_branch::BranchEntry],
    report: &mut crate::diagnosis::Report,
) -> anyhow::Result<Option<Intent>> {
    let head = vcs.head_revision(source_project)?;
    let mut intent = match branch {
        BranchChoice::Minted | BranchChoice::New(_) => {
            let text = match branch {
                BranchChoice::New(text) => text.clone(),
                _ => EphemeralRefName::mint(flat).to_string(),
            };
            let Some(planned) = crate::switch::judge_creation(
                ctx,
                vcs,
                project,
                source_project,
                &text,
                spelling,
                at,
                CREATE,
                true,
                report,
            )?
            else {
                return Ok(None);
            };
            Intent {
                start: planned.start,
                head,
                branch: Some(planned.new),
                creating: planned.record,
                opening: None,
                steps: Vec::new(),
            }
        }
        BranchChoice::Open(text) => {
            let opening = crate::switch::judge_opening(
                ctx,
                vcs,
                project,
                source_project,
                text,
                CREATE,
                crate::switch::Checkouts::All,
                report,
            )?;
            Intent {
                start: opening.destination.clone(),
                head,
                branch: Some(opening.branch.clone()),
                creating: None,
                opening: Some(opening),
                steps: Vec::new(),
            }
        }
        BranchChoice::Detach(rev) => {
            let Ok(start) = vcs.resolve_revision(source_project, rev) else {
                report.add(crate::switch::no_such_branch(
                    vcs,
                    project,
                    source_project,
                    rev,
                    CREATE,
                ));
                return Ok(None);
            };
            Intent {
                start,
                head,
                branch: None,
                creating: None,
                opening: None,
                steps: Vec::new(),
            }
        }
    };
    if let (Some(branch), Some(record)) = (&intent.branch, &intent.creating) {
        let (manifest, lock) = crate::switch::committed_files(vcs, source_project, &intent.start)?;
        if let Ok(manifest) = manifest {
            intent.steps = crate::member_branch::judge_entries(
                ctx,
                CREATE,
                (manifest, lock),
                (branch, record),
                entries,
                false,
                report,
            )?;
        }
    }
    Ok(Some(intent))
}

/// Create a workweave from the workspace `source_root`, which is its parent
/// and nothing more: the new workweave starts from commits, so the parent
/// does not switch and what it holds uncommitted stays there, reported.
///
/// The project repo is on a branch the create makes (the minted
/// `{project}--{workweave}` unless `branch` names one), on a branch opened by
/// its record, or detached. Each member repo is a worktree at the pin of the
/// project commit the workweave starts from, on the same branch where the
/// branch is a feature and the member is `owned` or `fork`. Also processes
/// `workweave:` artifacts, writes the marker file, and runs activate. No
/// `.rwv-active` is written: the marker names the project, and the two files
/// are mutually exclusive.
///
/// Returns the absolute path of the created workweave directory.
pub fn create_workweave(request: CreateWorkweave<'_>) -> anyhow::Result<PathBuf> {
    let CreateWorkweave {
        primary_root,
        source_root,
        project,
        name,
        branch,
        spelling,
        at,
        entries,
        placement,
        activation,
    } = request;
    let project_vcs = project_vcs();
    workweave_index::require_clone(primary_root, project)?;
    refuse_symlinked_members(source_root, project)?;
    let flat = WorkweaveId {
        project: project.clone(),
        name: name.clone(),
    }
    .flat();
    let workweave_dir = match placement {
        Placement::Dir(dir) => dir.clone(),
        Placement::Basedir(basedir) => basedir.join(&flat),
        Placement::DefaultBasedir => workweave_basedir(primary_root, project)?.join(&flat),
    };
    workweave_index::refuse_without_exclusive_publish(primary_root, project)?;
    if let Some(existing) = workweave_dir.ancestors().find(|dir| dir.is_dir()) {
        crate::durable_file::refuse_without_exclusive_publish(existing)?;
    }

    // Name uniqueness is checked against the workweave INDEX, not just the
    // container directory: the directory check below sees only the slot this
    // invocation picked, so `--dir` walks straight past it, and the index
    // insert is a silent last-writer-wins.
    if let Some(recorded) = workweave_index::lookup_raw(primary_root, project, name)? {
        if recorded != CanonicalPath::of(&workweave_dir) {
            let mut listed = vec![format!("recorded at {}", recorded.as_path().display())];
            if !recorded.as_path().exists() {
                listed.push(
                    "that path no longer exists; `rwv doctor --fix` prunes the stale entry"
                        .to_owned(),
                );
            }
            return Err(name_taken(project, name, listed)
                .bind("<project>", project)
                .bind("<ww>", name)
                .in_primary(primary_root, source_root)
                .judge(project_vcs.as_ref())
                .refused());
        }
    }

    let source_project = project_dir(source_root, project);
    let ctx = crate::workspace::WorkspaceContext::resolve_for_project(source_root, project)?;

    if workweave_dir.exists() {
        // Whatever is there may not be this workweave. On a filesystem that
        // folds case the lookup above answers for a directory spelled another
        // way, and the reuse below would adopt someone else's seat.
        if crate::workspace::diverged_occupant(&workweave_dir).is_some() {
            return Err(name_taken(
                project,
                name,
                [format!(
                    "{}, a different workweave's directory",
                    crate::workspace::describe_existing(&workweave_dir)
                )],
            )
            .bind("<project>", project)
            .in_primary(primary_root, source_root)
            .judge(project_vcs.as_ref())
            .refused());
        }
        refuse_symlinked_members(&workweave_dir, project)?;
        return reuse_existing_workweave(
            &ReuseRequest {
                primary_root,
                source_root,
                project,
                name,
                flat: &flat,
                branch: &branch,
                spelling: spelling.as_ref(),
                at: at.as_deref(),
                entries: &entries,
            },
            &workweave_dir,
        );
    }

    let mut report = crate::diagnosis::Report::default();
    let Some(intent) = judge_intent(
        &ctx,
        project_vcs.as_ref(),
        project,
        &source_project,
        &flat,
        &branch,
        spelling.as_ref(),
        at.as_deref(),
        &entries,
        &mut report,
    )?
    else {
        return Err(report
            .into_result()
            .expect_err("a create with no start reports why"));
    };
    report.into_result()?;
    let (manifest, workspace_lock) =
        crate::switch::committed_files(project_vcs.as_ref(), &source_project, &intent.start)?;
    let manifest = crate::cherry_pick::refuse_unparseable_at(
        project_vcs.as_ref(),
        crate::workspace::project_rel_path(project),
        &source_project,
        intent.start.id(),
        manifest,
    )??;
    if let Some(opening) = &intent.opening {
        crate::switch::localize_opened(project_vcs.as_ref(), &source_project, opening)?;
    }

    // Pre-flight: verify every repo we are about to worktree has at least one
    // commit. `git worktree add` requires a resolvable HEAD; without this check
    // the user gets a raw "fatal: ambiguous argument 'HEAD'" from git, which
    // names no path and suggests no fix. We run this BEFORE any disk mutation
    // so a missing-HEAD failure leaves no partial workweave directory behind.
    preflight_check_heads(project_vcs.as_ref(), source_root, project, &manifest)?;

    // Clear any orphaned `.git/worktrees/<name>` registrations left over from
    // a previous (failed or manually-deleted) create attempt. Without this
    // prune, `git worktree add` fails with "a missing but already registered
    // worktree". `prune_orphan_worktrees_for` only removes a registration
    // whose directory is absent, so a live workweave's files are never touched.
    prune_orphan_worktrees_for(
        project_vcs.as_ref(),
        &orphan_prune_pairs(&manifest, source_root, &workweave_dir),
    );

    if let crate::workspace::MintedDir::Occupied(occupant) =
        crate::workspace::create_identity_dir(&workweave_dir)?
    {
        crate::refuse!(
            RefusalKind::TargetDirOccupied,
            "cannot create workweave `{}` for project `{}`: {}.",
            name.as_str(),
            project.as_str(),
            occupant.describe()
        );
    }

    // Rollback guard: undoes partial state on any failure path (including
    // `bail!` / `?` propagation). Tracks which repos got worktrees added so
    // orphan `.git/worktrees/` registrations can be pruned in addition to
    // removing the workweave directory.
    let mut rollback = CreateRollbackGuard::new(crate::vcs::project_vcs(), workweave_dir.clone());

    let ephemeral = intent
        .branch
        .as_ref()
        .map(|branch| EphemeralRefName::chosen(branch.as_str()));

    let mut errors: Vec<String> = Vec::new();
    let mut submodule_warnings: Vec<String> = Vec::new();

    // Materialize each repo in the manifest. Forks come from source_root so
    // peer workweaves rooted in another workweave's HEADs diverge cleanly from
    // that parent rather than from primary.
    for (repo_path, entry) in manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = source_root.join(repo_path.as_path());
        let worktree_dest = workweave_dir.join(repo_path.as_path());

        let mut result: anyhow::Result<()> = worktree_dest
            .parent()
            .map(|p| std::fs::create_dir_all(p).map_err(anyhow::Error::from))
            .unwrap_or(Ok(()));
        let mut placement: Option<FilePlacement> = None;

        if result.is_ok() {
            // Record the repo for the post-rollback prune pass BEFORE the
            // birth: a post-checkout hook can reject `git worktree add` after
            // git has already written the `.git/worktrees/<name>` entry.
            rollback.record_attempted_repo(repo_abs.clone());

            // `birth_ephemeral_worktree` hands back the branch it placed the
            // checkout on even when the birth call failed, which is the
            // hook-failure case: the rollback needs it to clear what the
            // failed add left.
            let on_branch = intent.feature()
                && matches!(entry.role, Role::Owned | Role::Fork)
                && ephemeral.is_some();
            let started = pinned_start(&workspace_lock, vcs.as_ref(), &repo_abs, repo_path);
            match (&ephemeral, on_branch) {
                (Some(ephemeral), true) => match started.and_then(|start| {
                    birth_ephemeral_worktree(
                        vcs.as_ref(),
                        &repo_abs,
                        &worktree_dest,
                        ephemeral,
                        start,
                        WorktreeFiles::Shared(&Filesystem, entry.role),
                        BranchPolicy::Fresh,
                    )
                }) {
                    Ok(outcome) => {
                        placement = outcome.placement;
                        rollback.record_ref_attempt(outcome.found, outcome.birth);
                        if let Some(e) = outcome.failure {
                            result = Err(e);
                        }
                    }
                    // Failed before a branch was found: nothing was placed, so
                    // there is nothing for the guard to undo in this repo.
                    Err(e) => result = Err(e),
                },
                _ => match started.and_then(|at| {
                    crate::share::create_detached_populated(
                        vcs.as_ref(),
                        &repo_abs,
                        &worktree_dest,
                        &at,
                        entry.role,
                        &Filesystem,
                    )
                }) {
                    Ok(files) => placement = files,
                    Err(e) => result = Err(e),
                },
            }
        }

        if result.is_ok() {
            rollback.record_worktree(repo_abs, worktree_dest.clone());
            result = crate::settle::settle(vcs.as_ref(), &worktree_dest, entry.role).map(|_| ());
        }
        match result {
            Ok(()) => {
                if let Some(placement) = placement {
                    eprintln!("rwv workweave create: {repo_path}: {placement}");
                }
                if populate_member_submodules(
                    vcs.as_ref(),
                    &worktree_dest,
                    repo_path,
                    entry.role,
                    entry.recurse_submodules,
                    "rwv workweave create: ",
                ) > 0
                {
                    submodule_warnings.push(repo_path.as_str().to_string());
                }
            }
            Err(e) => {
                let hook_hint = if is_hook_rejection(&e) {
                    "\n  note: a git hook in this repo rejected the worktree creation; \
                     check the repo's .git/hooks/ directory or core.hooksPath config"
                } else {
                    ""
                };
                let msg = format!("{}: {e:#}{hook_hint}", repo_path.as_str());
                eprintln!("rwv workweave create: error: {msg}");
                errors.push(msg);
            }
        }
    }

    if !errors.is_empty() {
        let total = manifest.len();
        let failed = errors.len();
        // R25: explicit rollback before bail so that cleanup failures can be
        // appended to the returned error rather than only printed to stderr.
        // The rollback guard is defused after this call so Drop is a no-op.
        let cleanup_failures = rollback.rollback_and_collect_failures();
        let cleanup_note = if cleanup_failures.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nrollback completed with {} item(s) needing manual cleanup:\n  {}",
                cleanup_failures.len(),
                cleanup_failures.join("\n  ")
            )
        };
        return Err(crate::untokened!(Tally, "workweave create completed with {failed} failure(s) out of {total} repo(s){cleanup_note}"
        ));
    }

    if !submodule_warnings.is_empty() {
        let advised: Vec<String> = submodule_warnings
            .iter()
            .map(|repo| format!("`rwv update --repo {repo}`"))
            .collect();
        eprintln!(
            "rwv workweave create: warning: submodules not initialized in {} repo(s): {}; \
             run {} in {} to fetch what they record and check them out",
            submodule_warnings.len(),
            submodule_warnings.join(", "),
            advised.join(" and "),
            workweave_dir.display(),
        );
    }

    let project_wt_dest = project_dir(&workweave_dir, project);
    let project_dir = project_dir(source_root, project);
    {
        std::fs::create_dir_all(project_wt_dest.parent().unwrap())?;
        if project_vcs.prune_stale_worktree_registration(&project_dir, &project_wt_dest)?
            == RegistrationPrune::Locked
        {
            crate::refuse!(
                RefusalKind::WorkweaveDrift,
                "the project clone at {} holds a locked registration of {}, whose directory \
                 is gone, so no worktree can be made there. Unlock it with `git -C {} worktree \
                 unlock {}` if nothing else uses it, then re-run.",
                project_dir.display(),
                project_wt_dest.display(),
                project_dir.display(),
                project_wt_dest.display(),
            );
        }
        // Record the project repo for the post-rollback prune pass BEFORE the
        // birth, for the same hook-failure reason as manifest repos above.
        // The project repo is an instance of the model, so this arm runs the
        // identical birth over the identical ephemeral name.
        rollback.record_attempted_repo(project_dir.clone());
        let birth = match &ephemeral {
            Some(ephemeral) => birth_ephemeral_worktree(
                project_vcs.as_ref(),
                &project_dir,
                &project_wt_dest,
                ephemeral,
                intent.start.clone(),
                WorktreeFiles::Written,
                BranchPolicy::Fresh,
            )
            .and_then(|outcome| {
                let failure = outcome.failure;
                let birth = if intent.opening.is_some() {
                    RefBirth::Adopted
                } else {
                    outcome.birth
                };
                rollback.record_ref_attempt(outcome.found, birth);
                match failure {
                    Some(e) => Err(e),
                    None => Ok(()),
                }
            }),
            None => crate::share::create_detached_populated(
                project_vcs.as_ref(),
                &project_dir,
                &project_wt_dest,
                &intent.start,
                Role::Owned,
                &Filesystem,
            )
            .map(|_| ()),
        };
        match birth {
            Ok(()) => {
                rollback.record_worktree(project_dir.clone(), project_wt_dest.clone());
            }
            Err(e) => {
                let hook_hint = if is_hook_rejection(&e) {
                    "; a git hook in the project repo rejected the worktree creation — \
                     check .git/hooks/ or core.hooksPath config"
                } else {
                    ""
                };
                // Explicit rollback before bail so cleanup failures are surfaced
                // in the returned error (not only on stderr).
                let cleanup_failures = rollback.rollback_and_collect_failures();
                let cleanup_note = if cleanup_failures.is_empty() {
                    String::new()
                } else {
                    format!(
                        "\n\nrollback completed with {} item(s) needing manual cleanup:\n  {}",
                        cleanup_failures.len(),
                        cleanup_failures.join("\n  ")
                    )
                };
                return Err(crate::untokened!(
                    EnvironmentPassthrough,
                    "could not create project worktree projects/{}: {e:#}{hook_hint}{cleanup_note}",
                    project.as_str()
                ));
            }
        }
    }

    // Managed/generated files the project's enabled integrations declare but
    // git does not track (cargo-workspace's `.cargo/config.toml` under
    // `rustc-wrapper`, chief among them) arrive at neither the worktree
    // checkout above nor the attested-ledger carry below, which only knows
    // about files the ledger has an entry for. Carry those bytes here, before
    // the activation pass that follows verifies against them.
    if project_dir.exists() && project_wt_dest.exists() {
        if let Err(e) = carry_untracked_managed_files(
            project_vcs.as_ref(),
            source_root,
            project,
            &manifest,
            &project_dir,
            &project_wt_dest,
        ) {
            eprintln!(
                "rwv workweave create: warning: could not carry untracked managed files \
                 into projects/{}: {e:#}",
                project.as_str()
            );
        }
    }

    // rwv-owned generated files are gitignore-eligible, so the worktree
    // checkout above arrives without the source's. Regenerating them here
    // would run an ecosystem resolver against a registry that has moved since
    // the source resolved, giving the fork a different dependency set than the
    // workspace it forked from; copying is what makes the two agree.
    if project_dir.exists() && project_wt_dest.exists() && intent.start == intent.head {
        if let Err(e) =
            crate::owned_state::carry_attested_owned_files(&project_dir, &project_wt_dest)
        {
            eprintln!(
                "rwv workweave create: warning: could not carry rwv-owned generated files \
                 into projects/{}: {e:#}",
                project.as_str()
            );
        }
    }

    // Process WorkweaveConfig artifacts. Sources resolve against source_root
    // so artifacts follow the workspace being forked from.
    if let Some(ref ww_config) = manifest.workweave {
        // Copy entries.
        for entry in &ww_config.copy {
            let source = source_root.join(entry);
            let dest = workweave_dir.join(entry);
            if source.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if source.is_dir() {
                    copy_dir_recursive(&source, &dest)?;
                } else {
                    std::fs::copy(&source, &dest)?;
                }
            }
        }

        // Link entries — absolute symlinks to the source's canonical paths.
        for entry in &ww_config.link {
            let source =
                resolve(&source_root.join(entry)).unwrap_or_else(|_| source_root.join(entry));
            let dest = workweave_dir.join(entry);
            if source.exists() {
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                crate::symlink::create(&source, &dest, LinkTarget::on_disk(&source))?;
            }
        }
    }

    // Write .rwv-workweave marker file. The marker records the primary so
    // workweaves always know how to find their parent weave regardless of
    // where they were forked from. `parent` records the workspace this
    // workweave was forked from (= source_root) so a bare `rwv land` knows
    // where to land. For workweaves forked directly from primary, parent
    // == primary; for workweaves forked from another workweave, parent is
    // that workweave's directory.
    let marker = WorkweaveMarker::new(primary_root, project.clone(), name.clone(), source_root);
    // The marker is the ONLY identity file a workweave root gets: it and
    // `.rwv-active` name the same fact and are mutually exclusive, occupying
    // one tier of the resolution chain rather than two. A pointer beside it
    // would be a second copy of this workweave's identity that nothing reads
    // and nothing keeps in agreement with the marker.
    marker.write(&workweave_dir)?;

    // Run activate in the workweave context. The project SELECTION step is
    // skipped for a workweave root (see `activate_at`). A failure before the
    // hooks leaves a seat nothing can use, so the guard rolls it back; one at
    // the hooks leaves a usable seat whose install state is incomplete, which
    // is kept, as `add` keeps a manifest change whose hooks failed.
    let hooks_failed =
        match crate::activate::activate_workweave(project, &workweave_dir, activation) {
            Ok(_) => None,
            Err(crate::activate::ActivationFailure::BeforeHooks(e)) => return Err(e),
            Err(crate::activate::ActivationFailure::AtHooks(e)) => Some(e),
        };

    // Record the workweave in the primary-side registry so `list`, `delete`,
    // and doctor find it without re-scanning. Absolute path so per-workweave
    // `--dir` overrides are as first-class as default-container placements.
    // Failure to record is surfaced (not swallowed): a workweave without a
    // registry entry becomes an on-disk orphan that consumers cannot address
    // by name, and silent auto-adoption in read paths is deliberately not
    // provided (see module docs).
    workweave_index::record_workweave(
        primary_root,
        project,
        name,
        CanonicalPath::of(&workweave_dir),
        CanonicalPath::of(source_root),
    )
    .context("failed to record workweave in the primary-side registry")?;

    let vcs = project_vcs.as_ref();
    if let (Some(branch), Some(record)) = (&intent.branch, &intent.creating) {
        if let Some(line) =
            crate::switch::write_created_record(vcs, project, &project_dir, branch, record)?
        {
            eprintln!("rwv workweave create: {line}");
        }
        if let crate::records::Record::Feature { target, as_mode } = record {
            for line in
                crate::branch::target_recorded_lines(vcs, &project_dir, branch, target, *as_mode)?
            {
                eprintln!("rwv workweave create: {line}");
            }
        }
    }
    let seat = crate::workspace::WorkspaceContext::resolve_for_project(&workweave_dir, project)?;
    crate::member_branch::apply_entries(&seat, CREATE, &intent.steps, activation)?;
    align_new_workweave(&seat, project, primary_root, intent.branch.is_none())?;

    // All steps complete — defuse the rollback guard so Drop is a no-op.
    rollback.defuse();

    report_parent_leftovers(
        vcs,
        source_root,
        project,
        &project_dir,
        &manifest,
        &workweave_dir,
    );

    if let Some(cause) = hooks_failed {
        return Err(HooksIncomplete {
            dir: workweave_dir,
            cause,
        }
        .into());
    }

    Ok(workweave_dir)
}

/// Put the members of a new workweave where `rwv update` would, and record
/// the workspace as aligned for what its project repo has checked out.
fn align_new_workweave(
    seat: &crate::workspace::WorkspaceContext,
    project: &ProjectName,
    primary_root: &Path,
    detached: bool,
) -> anyhow::Result<()> {
    let root = seat.active_path();
    let project_dir = project_dir(root, project);
    let vcs = project_vcs();
    if detached {
        let aligner = crate::alignment::Aligner::open(
            root,
            project,
            crate::alignment::Accounting::default(),
        )?;
        let mut state = aligner.state.clone();
        state.aligned = Some(crate::alignment::Aligned::Detached);
        crate::alignment::store_state(vcs.as_ref(), &project_dir, &state)?;
        return Ok(());
    }
    let manifest = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME))?;
    let lock_path = LockFile::path_in(&project_dir);
    let lock = lock_path
        .exists()
        .then(|| LockFile::from_state(&lock_path).ok())
        .flatten();
    let aligner = crate::alignment::Aligner::open(
        root,
        project,
        crate::alignment::Accounting::scan(primary_root)?,
    )?;
    for (path, entry) in manifest.iter_entries() {
        let dir = root.join(path.as_path());
        let member_vcs = vcs_for(entry.vcs_type);
        if is_symlinked_member(&dir) || is_dangling_link(&dir) || !member_vcs.is_repo(&dir) {
            continue;
        }
        let member = crate::alignment::Member {
            path,
            entry,
            dir: &dir,
            vcs: member_vcs.as_ref(),
        };
        let plan = aligner.plan(&member, lock.as_ref())?;
        match aligner.apply(&member, &plan, path)? {
            crate::alignment::Applied::Aligned { .. } => {}
            crate::alignment::Applied::Left { judged } => {
                let mut report = crate::diagnosis::Report::default();
                for line in judged {
                    report.add(line);
                }
                eprint!("{}", report.render(CREATE));
            }
        }
    }
    crate::alignment::record(root, project)?;
    Ok(())
}

/// Say what the parent holds uncommitted, which the new workweave does not
/// carry: it starts from commits.
fn report_parent_leftovers(
    project_vcs: &dyn Vcs,
    source_root: &Path,
    project: &ProjectName,
    source_project: &Path,
    manifest: &Manifest,
    workweave_dir: &Path,
) {
    let mut held: Vec<String> = Vec::new();
    for name in project_vcs
        .dirty_file_names(source_project)
        .unwrap_or_default()
    {
        held.push(format!("{}/{name}", project_rel_path(project)));
    }
    for (path, entry) in manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
        let dir = source_root.join(path.as_path());
        if vcs.is_repo(&dir) && vcs.has_uncommitted_changes(&dir).unwrap_or(false) {
            held.push(format!("{path} (uncommitted changes)"));
        }
    }
    if !held.is_empty() {
        eprintln!(
            "rwv workweave create: {} holds uncommitted changes that stay there, as {} starts \
             from commits: {}",
            source_root.display(),
            workweave_dir.display(),
            held.join(", ")
        );
    }
}

/// [`create_workweave`]'s error when the workweave was created and registered
/// and only its install hooks failed: the seat is usable and kept, so an agent
/// runtime's create hook can hand it over and leave the hooks to `rwv
/// materialize`.
#[derive(Debug)]
pub struct HooksIncomplete {
    pub dir: PathBuf,
    pub cause: anyhow::Error,
}

impl HooksIncomplete {
    /// The failure as a warning on stderr, for a caller that hands the seat
    /// over instead of failing.
    pub fn warn(&self) {
        eprintln!("rwv workweave create: warning: {self}: {:#}", self.cause);
    }
}

impl std::fmt::Display for HooksIncomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "workweave created at {}; only its install hooks did not complete. Fix \
             the cause, then run `rwv materialize` inside it",
            self.dir.display()
        )
    }
}

impl std::error::Error for HooksIncomplete {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

/// What an agent runtime's create hook gets from [`create_workweave`]'s
/// result: the seat even when only its install hooks failed, that failure
/// turned into a warning, and every other error unchanged.
pub fn hand_over_seat(created: anyhow::Result<PathBuf>) -> anyhow::Result<PathBuf> {
    match created {
        Err(e) => match e.downcast::<HooksIncomplete>() {
            Ok(incomplete) => {
                incomplete.warn();
                Ok(incomplete.dir)
            }
            Err(e) => Err(e),
        },
        created => created,
    }
}

/// The spelling of a create, as an existing workweave is compared against it.
struct ReuseRequest<'a> {
    primary_root: &'a Path,
    source_root: &'a Path,
    project: &'a ProjectName,
    name: &'a WorkweaveName,
    flat: &'a crate::naming::FlatAddress,
    branch: &'a BranchChoice,
    spelling: Option<&'a crate::switch::Spelling>,
    at: Option<&'a str>,
    entries: &'a [crate::member_branch::BranchEntry],
}

/// Return the workweave at `workweave_dir` when it is clean, its marker names
/// this primary, project and parent, and it holds what the same spelling
/// would make; anything else is refused as `name` taken, listing what
/// differs. Nothing changes either way, so non-git state an agent wrote
/// between invocations survives.
fn reuse_existing_workweave(
    request: &ReuseRequest<'_>,
    workweave_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let ReuseRequest {
        primary_root,
        source_root,
        project,
        name,
        ..
    } = *request;
    let taken = |held_by: Vec<String>| {
        name_taken(project, name, held_by)
            .bind("<project>", project)
            .in_primary(primary_root, source_root)
            .judge(project_vcs().as_ref())
            .refused()
    };
    let Some(marker) = WorkweaveMarker::read(workweave_dir)? else {
        return Err(taken(vec![format!(
            "{} has no .rwv-workweave marker, so it is no workweave of this project",
            workweave_dir.display()
        )]));
    };
    if marker.project() != project {
        return Err(taken(vec![format!(
            "{} is project `{}`'s workweave",
            workweave_dir.display(),
            marker.project()
        )]));
    }
    if !marker.names_primary(&CanonicalPath::of(primary_root)) {
        return Err(taken(vec![format!(
            "{} is a workweave of the primary at {}",
            workweave_dir.display(),
            marker.primary().as_path().display()
        )]));
    }
    if *marker.parent() != CanonicalPath::of(source_root) {
        return Err(taken(vec![format!(
            "{} was created from {}, not from {}",
            workweave_dir.display(),
            marker.parent().as_path().display(),
            source_root.display()
        )]));
    }

    let manifest =
        Manifest::from_path(&project_dir(workweave_dir, project).join(Manifest::FILE_NAME))?;
    let mut differing: Vec<String> = Vec::new();
    for (repo_path, entry) in manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
        let dest = workweave_dir.join(repo_path.as_path());
        if dest.exists() && vcs.has_uncommitted_changes(&dest)? {
            differing.push(format!("{}: uncommitted changes", repo_path.as_str()));
        }
    }
    let project_vcs = project_vcs();
    let seat_project = project_dir(workweave_dir, project);
    if project_vcs.is_repo(&seat_project) && project_vcs.has_uncommitted_changes(&seat_project)? {
        differing.push(format!(
            "{}: uncommitted changes",
            project_rel_path(project)
        ));
    }
    differing.extend(spelling_differences(
        request,
        project_vcs.as_ref(),
        &seat_project,
        &manifest,
    )?);
    if !differing.is_empty() {
        return Err(taken(differing));
    }
    record_what_a_stopped_create_left(request, project_vcs.as_ref())?;
    eprintln!(
        "rwv workweave create: {name} already exists at {} and holds what this create would \
         make; returned as it is",
        workweave_dir.display()
    );
    Ok(workweave_dir.to_path_buf())
}

/// A create that stopped before it recorded its branch left the workweave
/// without a record: the same spelling, run again, writes the one it would
/// have written.
fn record_what_a_stopped_create_left(
    request: &ReuseRequest<'_>,
    vcs: &dyn Vcs,
) -> anyhow::Result<()> {
    let text = match request.branch {
        BranchChoice::Minted => EphemeralRefName::mint(request.flat).to_string(),
        BranchChoice::New(text) => text.clone(),
        BranchChoice::Open(_) | BranchChoice::Detach(_) => return Ok(()),
    };
    let project = request.project;
    let source_project = project_dir(request.source_root, project);
    let branch = crate::switch::named(vcs, project, &source_project, &text, CREATE)?;
    let store = crate::records::Store::open(vcs, &source_project)?;
    let view = store.view()?;
    if !matches!(view.lookup(&branch), crate::records::Lookup::Unrecorded) {
        return Ok(());
    }
    let record = match request.spelling {
        Some(crate::switch::Spelling::Declared) => crate::records::Record::Declared,
        Some(crate::switch::Spelling::Target(target)) => {
            let target = crate::switch::named(vcs, project, &source_project, target, CREATE)?;
            crate::branch::feature_record(&view, &branch, &target).map_err(|e| {
                crate::branch::write_refusal(vcs, &store, project, &source_project, &branch, e)
            })?
        }
        None => {
            let Some(parent) = crate::branch::workweave_parent(&source_project, project)? else {
                return Ok(());
            };
            crate::branch::feature_record(&view, &branch, &parent).map_err(|e| {
                crate::branch::write_refusal(vcs, &store, project, &source_project, &branch, e)
            })?
        }
    };
    if let Some(line) =
        crate::switch::write_created_record(vcs, project, &source_project, &branch, &record)?
    {
        eprintln!("rwv workweave create: {line}");
    }
    if let crate::records::Record::Feature { target, as_mode } = &record {
        for line in
            crate::branch::target_recorded_lines(vcs, &source_project, &branch, target, *as_mode)?
        {
            eprintln!("rwv workweave create: {line}");
        }
    }
    Ok(())
}

/// What the workweave's project repo holds that the create's spelling would
/// not have made.
fn spelling_differences(
    request: &ReuseRequest<'_>,
    vcs: &dyn Vcs,
    seat_project: &Path,
    seat_manifest: &Manifest,
) -> anyhow::Result<Vec<String>> {
    let source_project = project_dir(request.source_root, request.project);
    let on = crate::branch::checked_out(vcs, seat_project)?;
    let mut differing = Vec::new();
    let wanted = match request.branch {
        BranchChoice::Minted => Some(EphemeralRefName::mint(request.flat).to_string()),
        BranchChoice::New(text) | BranchChoice::Open(text) => Some(text.clone()),
        BranchChoice::Detach(rev) => {
            let head = vcs.head_revision(seat_project)?;
            let wanted = vcs.resolve_revision(&source_project, rev).ok();
            if on.is_some() || wanted.as_ref() != Some(&head) {
                differing.push(format!("it is not a detached workspace at {rev}"));
            }
            return Ok(differing);
        }
    };
    if on != wanted {
        differing.push(format!(
            "its project repo is on {}, not {}",
            on.as_deref().unwrap_or("no branch"),
            wanted.as_deref().unwrap_or("no branch")
        ));
        return Ok(differing);
    }
    let branch = crate::switch::named(
        vcs,
        request.project,
        &source_project,
        wanted.as_deref().expect("a branch choice names a branch"),
        CREATE,
    )?;
    if let Some(spelling) = request.spelling {
        let store = crate::records::Store::open(vcs, &source_project)?;
        let view = store.view()?;
        let agrees = match view.lookup(&branch) {
            crate::records::Lookup::Value(record) => crate::switch::spelling_agrees(
                &view,
                &branch,
                spelling,
                &record,
                vcs,
                (request.project, &source_project, CREATE),
            )?,
            _ => false,
        };
        if !agrees {
            differing.push(format!(
                "the record of {branch} is not the one this spelling gives"
            ));
        }
    }
    if let Some(at) = request.at {
        let seat_head = vcs.head_revision(seat_project)?;
        let reaches = vcs
            .resolve_revision(&source_project, at)
            .ok()
            .is_some_and(|at| {
                vcs.is_ancestor(seat_project, &at, &seat_head)
                    .unwrap_or(false)
            });
        if !reaches {
            differing.push(format!("it does not start at {at}"));
        }
    }
    for entry in request.entries {
        let declared = seat_manifest
            .get_entry(&entry.member)
            .map(|held| held.branch.local_counterpart().as_str().to_owned());
        if declared.as_deref() != Some(entry.branch.as_str()) {
            differing.push(format!(
                "{} does not declare {}",
                entry.member, entry.branch
            ));
        }
    }
    Ok(differing)
}

/// `name` judged taken for a new workweave of `project`, with what holds it.
fn name_taken(
    project: &ProjectName,
    name: &WorkweaveName,
    held_by: impl IntoIterator<Item = String>,
) -> crate::diagnosis::Judging {
    crate::diagnosis::State::NameTaken
        .of(project_rel_path(project))
        .subject(name)
        .by_verb("workweave create")
        .listing(held_by)
}

/// Return the relative paths within `workweave_dir` whose worktrees have
/// uncommitted changes (staged, unstaged, or untracked).
///
/// Checks the project worktree (`projects/<project>`), every manifest
/// member, and every checkout at member depth the manifest does not name. A
/// repo that is missing on disk is skipped; a repo whose dirty check itself
/// fails is reported as dirty (conservative: "we couldn't confirm clean").
pub fn collect_dirty_paths(
    project_vcs: &dyn Vcs,
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> Vec<String> {
    dirty_checkouts(project_vcs, workweave_dir, project, manifest)
        .into_iter()
        .map(|dirty| {
            let label = if dirty.in_manifest {
                dirty.repo
            } else {
                format!("{} (not a manifest member)", dirty.repo)
            };
            match dirty.files {
                Ok(_) => label,
                Err(why) => format!("{label}: status check failed: {why}"),
            }
        })
        .collect()
}

/// [`collect_dirty_paths`]'s checkouts, judged: one `edits` line each, its
/// files listed, a status that could not be read listed as that. `deleting`
/// is the workweave `rwv workweave delete` refuses to delete, whose refusal
/// also offers deleting it past the edits.
pub fn judge_dirty_checkouts(
    project_vcs: &dyn Vcs,
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
    deleting: Option<&WorkweaveName>,
) -> crate::diagnosis::Report {
    let mut report = crate::diagnosis::Report::default();
    for busy in judge_mid_operation_checkouts(project_vcs, workweave_dir, project, manifest) {
        report.add(busy);
    }
    for dirty in dirty_checkouts(project_vcs, workweave_dir, project, manifest) {
        if report.lines().iter().any(|line| line.repo() == dirty.repo) {
            continue;
        }
        let vcs = dirty.vcs.as_deref().unwrap_or(project_vcs);
        let listed = match dirty.files {
            Ok(files) => files,
            Err(why) => vec![format!("(status check failed: {why})")],
        };
        let judging = match deleting {
            Some(_) => crate::diagnosis::dirty_beside_drift(vcs, &dirty.repo, &dirty.dir, listed),
            None => Some(crate::diagnosis::dirty(
                vcs,
                &dirty.repo,
                &dirty.dir,
                listed,
            )),
        };
        let Some(judging) = judging else {
            continue;
        };
        let mut judging = judging.checkout(&dirty.dir);
        if !dirty.in_manifest {
            judging = judging.subject("not a manifest member");
        }
        if let Some(name) = deleting {
            judging = judging.by_verb("workweave delete").bind("<ww>", name);
            if matches!(
                judging.judged_state(),
                crate::diagnosis::State::EditsMovedAndEdited
                    | crate::diagnosis::State::EditsSetBack
                    | crate::diagnosis::State::EditsDriftLeftInPlace
            ) {
                judging = judging.elsewhere(workweave_dir);
            }
        }
        report.add(judging.judge(vcs));
    }
    report
}

/// Each checkout in a workweave stopped in a VCS operation, judged: only that
/// operation's own continue or abort ends it, so no discard flag lifts it.
pub fn judge_mid_operation_checkouts(
    project_vcs: &dyn Vcs,
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> Vec<crate::diagnosis::Judged> {
    let mut busy = Vec::new();
    let project_wt = project_dir(workweave_dir, project);
    if project_vcs.is_repo(&project_wt) {
        busy.extend(crate::diagnosis::judge_edits(
            project_vcs,
            project_rel_path(project),
            &project_wt,
            Vec::new(),
            None,
        ));
    }
    for checkout in member_depth_checkouts(workweave_dir, manifest) {
        let wt = workweave_dir.join(checkout.repo_path.as_path());
        if checkout.vcs.is_repo(&wt) {
            busy.extend(crate::diagnosis::judge_edits(
                checkout.vcs.as_ref(),
                &checkout.repo_path,
                &wt,
                Vec::new(),
                None,
            ));
        }
    }
    busy
}

/// A checkout in a workweave holding uncommitted changes: its path, whether
/// the manifest names it, where it is, its backend (none for the project
/// repo, whose backend the caller holds), and its dirty files or why they
/// could not be read.
struct DirtyCheckout {
    repo: String,
    in_manifest: bool,
    dir: PathBuf,
    vcs: Option<Box<dyn Vcs>>,
    files: Result<Vec<String>, String>,
}

fn dirty_checkouts(
    project_vcs: &dyn Vcs,
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> Vec<DirtyCheckout> {
    let mut dirty = Vec::new();
    let mut judge = |repo: String, in_manifest: bool, dir: PathBuf, vcs: Option<Box<dyn Vcs>>| {
        let files = vcs
            .as_deref()
            .unwrap_or(project_vcs)
            .dirty_file_names(&dir)
            .map_err(|e| format!("{e:#}"));
        if !matches!(&files, Ok(names) if names.is_empty()) {
            dirty.push(DirtyCheckout {
                repo,
                in_manifest,
                dir,
                vcs,
                files,
            });
        }
    };

    let project_wt = project_dir(workweave_dir, project);
    if project_vcs.is_repo(&project_wt) {
        judge(project_rel_path(project), true, project_wt, None);
    }

    for checkout in member_depth_checkouts(workweave_dir, manifest) {
        let wt = workweave_dir.join(checkout.repo_path.as_path());
        if wt.exists() {
            judge(
                checkout.repo_path.to_string(),
                checkout.in_manifest,
                wt,
                Some(checkout.vcs),
            );
        }
    }

    dirty
}

/// A slot at member depth in a workweave that removing the workweave would
/// take with it.
struct MemberDepthCheckout {
    repo_path: RepoPath,
    vcs: Box<dyn Vcs>,
    in_manifest: bool,
}

impl MemberDepthCheckout {
    fn label(&self) -> String {
        if self.in_manifest {
            self.repo_path.as_str().to_string()
        } else {
            format!("{} (not a manifest member)", self.repo_path)
        }
    }
}

/// Every member the manifest names, whether or not it is on disk, then every
/// checkout at member depth it does not name: a member replay dropped but
/// could not prune, or one this workweave added that the manifest has not
/// taken. Removing the workweave directory takes the latter with it, so every
/// check and cleanup of a removal treats them as it treats members. A
/// symlinked slot is not a checkout of this workweave and is left out.
fn member_depth_checkouts(workweave_dir: &Path, manifest: &Manifest) -> Vec<MemberDepthCheckout> {
    let members = manifest
        .iter_entries()
        .map(|(repo_path, entry)| MemberDepthCheckout {
            repo_path: repo_path.clone(),
            vcs: vcs_for(entry.vcs_type),
            in_manifest: true,
        });
    let outside =
        crate::workspace::scan_member_depth_repos(workweave_dir, crate::vcs::probe_vcs().as_ref())
            .into_iter()
            .filter(|repo_path| !manifest.contains_repo(repo_path))
            .filter(|repo_path| !is_symlinked_member(&workweave_dir.join(repo_path.as_path())))
            .map(|repo_path| MemberDepthCheckout {
                repo_path,
                vcs: crate::vcs::probe_vcs(),
                in_manifest: false,
            });
    members.chain(outside).collect()
}

/// Merge baselines for a workweave: workspace roots whose lineage counts as
/// "this work has landed". The recorded parent (the workspace the workweave
/// was forked from, and the default land target) comes first when the
/// marker is readable and the path still exists; then `landed_into`, the
/// workspace the op deleting this workweave has just synced it into; the
/// primary weave root is always included, last. With stacked workweaves a
/// child workweave's work lands in its parent workweave and may reach primary
/// only after further land hops — checking primary alone would refuse
/// every child retire. Each workspace is listed once, however it is spelled.
fn merge_baselines(
    workweave_dir: &Path,
    ws_root: &CanonicalPath,
    landed_into: Option<&CanonicalPath>,
) -> Vec<PathBuf> {
    let parent = WorkweaveMarker::read(workweave_dir)
        .ok()
        .flatten()
        .map(|marker| marker.parent().clone())
        .filter(|parent| parent.as_path().exists());
    let mut baselines: Vec<CanonicalPath> = Vec::new();
    for held in parent.into_iter().chain(landed_into.cloned()) {
        if held != *ws_root && !baselines.contains(&held) {
            baselines.push(held);
        }
    }
    baselines.push(ws_root.clone());
    baselines
        .into_iter()
        .map(CanonicalPath::into_path_buf)
        .collect()
}

/// Resolve the canonical store path that owns a workweave checkout, for the
/// `worktree remove` / `worktree prune` calls below — and for any caller that
/// needs to tell a linked workspace apart from a store that merely sits where
/// one was expected (sync's prune of a dropped repo asks exactly that before
/// it deletes a directory).
///
/// Under the clone-topology invariants (see
/// `docs/explanation/joints/clone-topology.md`) every workweave checkout is a
/// linked workspace whose `.git/` resolves into a canonical store somewhere
/// in the weave. `git worktree remove` and `git worktree prune` only know
/// about the worktree they're invoked from when called in that canonical
/// store — running them in some other "looks like a clone of the same repo"
/// directory either silently no-ops (the registration stays stale) or fails
/// loudly with "is not a working tree".
///
/// Resolution order:
/// - If the checkout doesn't exist on disk, fall back to `fallback`. Nothing
///   to remove anyway; this just keeps the prune/branch-cleanup loop simple.
/// - Otherwise ask the VCS for the checkout's actual canonical store. If
///   that query fails (the checkout is a plain dir, not a repo), fall back
///   to `fallback`.
///
/// The fallback path is the `<ws_root>/<repo_path>` slot the legacy code
/// used unconditionally — preserving behavior for the (now-explicit)
/// "checkout is gone" branch while making the canonical resolution
/// authoritative for the live case.
pub(crate) fn resolved_worktree_parent(
    vcs: &dyn Vcs,
    checkout: &Path,
    fallback: &Path,
) -> CanonicalPath {
    if !checkout.exists() {
        return CanonicalPath::of(fallback);
    }
    match vcs
        .main_checkout(checkout)
        .map(|main| CanonicalPath::of(&main))
    {
        Some(p) => p,
        None => CanonicalPath::of(fallback),
    }
}

/// Refuse `rwv workweave delete` when a checkout under `workweave_dir`
/// holds the canonical store that OTHER worktrees link into — the
/// catastrophic case the clone-topology joint flags as inverted topology.
///
/// The precondition: each per-repo workweave checkout MUST be a linked
/// workspace, not a canonical store with foreign dependents. Deleting a
/// canonical store while other worktrees still link into it would orphan
/// every dependent worktree on disk.
///
/// Returns `Err` pointing the operator at
/// `rwv doctor` (where the topology check lives, per the joint). This refusal
/// is NOT bypassable by the discard flags — they consent to losing this
/// workweave's work, not to corrupting other workweaves whose object DAG we
/// happen to be hosting. The operator must repair topology first (operator
/// work, out of scope for this verb).
///
/// Returns `Ok(())` when no per-repo checkout is a canonical store with
/// foreign dependents, OR when the only worktree the canonical store knows
/// about is the workweave's own checkout (the topology is fine — git just
/// records the checkout as its own worktree).
fn refuse_if_checkouts_host_foreign_worktrees(
    project_vcs: &dyn Vcs,
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> anyhow::Result<()> {
    let mut hazards: Vec<String> = Vec::new();
    let mut stranded: Vec<String> = Vec::new();

    let mut check = |vcs: &dyn Vcs, checkout: &Path, label: String| {
        if !checkout.exists() {
            return;
        }
        let canonical = match vcs
            .main_checkout(checkout)
            .map(|main| CanonicalPath::of(&main))
        {
            Some(p) => p,
            None => return, // not a repo; nothing to host
        };
        let checkout_canonical = CanonicalPath::of(checkout);
        if canonical != checkout_canonical {
            // Linked workspace — the canonical store lives elsewhere; this
            // checkout cannot have foreign dependents.
            return;
        }
        // checkout IS the canonical store. Enumerate every worktree this
        // store knows about and flag any whose path is NOT under our
        // workweave_dir (foreign — would be orphaned by delete).
        let worktrees = match vcs.list_worktrees(checkout) {
            Ok(ws) => ws,
            Err(_) => return,
        };
        let ww_canonical = CanonicalPath::of(workweave_dir);
        let mut foreign: Vec<PathBuf> = Vec::new();
        for wt in worktrees {
            let wt_canon = CanonicalPath::of(&wt);
            if wt_canon == checkout_canonical {
                continue; // the canonical store's own "main" entry
            }
            if !ww_canonical.contains(&wt_canon) {
                foreign.push(wt);
            }
        }
        if !foreign.is_empty() {
            let mut lines = vec![format!(
                "{label}: checkout is itself a canonical store with {} dependent worktree(s):",
                foreign.len()
            )];
            for wt in &foreign {
                lines.push(format!("    - {}", wt.display()));
            }
            hazards.push(lines.join("\n"));
            return;
        }
        let holding = match vcs.list_local_branch_names(checkout) {
            Ok(branches) => branches
                .iter()
                .filter_map(|branch| match vcs.unpushed_commits(checkout, branch) {
                    Ok(commits) if commits.is_empty() => None,
                    Ok(commits) => Some(format!("{branch} ({} commit(s))", commits.len())),
                    Err(_) => Some(format!("{branch} (could not be read)")),
                })
                .collect::<Vec<_>>(),
            Err(_) => vec!["its branches could not be listed".to_owned()],
        };
        if !holding.is_empty() {
            stranded.push(format!("{label}: {}", holding.join(", ")));
        }
    };

    // Project worktree.
    let project_wt = project_dir(workweave_dir, project);
    check(project_vcs, &project_wt, project_rel_path(project));

    for checkout in member_depth_checkouts(workweave_dir, manifest) {
        check(
            checkout.vcs.as_ref(),
            &workweave_dir.join(checkout.repo_path.as_path()),
            checkout.label(),
        );
    }

    let workweave_label = workweave_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| workweave_dir.display().to_string());

    if !hazards.is_empty() {
        crate::refuse!(
            RefusalKind::StandaloneInWorkweave,
            "rwv workweave delete: refusing — inverted clone topology detected.\n\
             The following checkout(s) inside workweave {} are themselves canonical \
             stores that other worktrees link into; deleting this workweave would orphan \
             those dependents:\n  {}\n\n\
             Run `rwv doctor` for a full topology audit and remediation guidance. \
             This refusal is NOT bypassable with --discard-uncommitted or \
             --discard-unlanded: those consent to losing this workweave's work, \
             not to corrupting unrelated worktrees whose object store we happen to be \
             hosting.",
            workweave_label,
            hazards.join("\n  "),
        )
    }
    if !stranded.is_empty() {
        crate::refuse!(
            RefusalKind::StandaloneInWorkweave,
            "rwv workweave delete: refusing — the following checkout(s) inside workweave {} \
             are themselves canonical stores holding commits no remote has, and a backup \
             written there would go with them:\n  {}\n\n\
             Push those commits, or move the clone out of the workweave, then delete. \
             `rwv doctor` reports the clone as `standalone-in-workweave`. \
             --discard-uncommitted and --discard-unlanded do not waive this refusal.",
            workweave_label,
            stranded.join("\n  "),
        )
    }
    Ok(())
}

/// Resolve the parent that children of the workweave at `retiree_dir` should
/// be adopted by when the retiree is retired or deleted.
///
/// Children re-point to the RETIREE'S OWN recorded parent
/// (the grandparent), so the lineage stays transitive by construction — the
/// retiree's unique commits have just landed in that grandparent. If the
/// retiree's marker is unreadable, records no parent, or records a parent that
/// no longer exists on disk (itself already retired), fall back to `primary`,
/// which always exists.
fn adoptive_parent_for_children(retiree_dir: &Path, primary_root: &Path) -> PathBuf {
    let grandparent = match WorkweaveMarker::read(retiree_dir) {
        Ok(Some(marker)) if marker.parent().as_path().exists() => Some(marker.parent().clone()),
        _ => None,
    };
    grandparent
        .unwrap_or_else(|| CanonicalPath::of(primary_root))
        .into_path_buf()
}

/// Enumerate the live children of the workweave at `retiree_dir` and re-point
/// each child's `.rwv-workweave` `parent:` to `new_parent`, printing one loud
/// line per adopted child.
///
/// A "child" is any workweave under the same primary whose marker's `parent`
/// field canonically resolves to `retiree_dir`. Branch names are creation-time
/// namespaces, NOT lineage records, so they are deliberately left untouched —
/// this is exactly why consumers must read parent from the marker, never from
/// the branch name.
///
/// Shared by both the retire (`land --retire`) and `workweave delete` paths
/// so the adoption semantics live in exactly one place. Runs BEFORE the retiree
/// directory is removed (the enumeration reads on-disk markers, but the
/// grandparent resolution in [`adoptive_parent_for_children`] must see the
/// retiree's own marker while it still exists).
///
/// Best-effort per child: a marker that can't be rewritten is reported to
/// stderr but does not abort the retire/delete (the alternative — leaving the
/// retiree in place — is strictly worse, and `rwv doctor --fix` re-points any
/// child left dangling by a partial failure).
fn adopt_children_of(retiree_dir: &Path, primary_root: &Path) {
    let new_parent = adoptive_parent_for_children(retiree_dir, primary_root);

    let retiree_canonical = CanonicalPath::of(retiree_dir);

    // A retiree that IS its own adoptive parent (should not happen — the
    // grandparent is a different workspace) would create a self-loop; guard
    // against it defensively.
    let new_parent_canonical = CanonicalPath::of(&new_parent);

    for (child, child_dir) in list_workweave_dirs(primary_root) {
        let child_canonical = CanonicalPath::of(&child_dir);
        // Never re-point the retiree's own marker.
        if child_canonical == retiree_canonical {
            continue;
        }
        let marker = match WorkweaveMarker::read(&child_dir) {
            Ok(Some(m)) => m,
            _ => continue,
        };
        if *marker.parent() != retiree_canonical {
            continue;
        }
        // Defensive: don't create a self-parent marker.
        if new_parent_canonical == child_canonical {
            eprintln!(
                "warning: not adopting child workweave {child}: adoptive parent \
                 {} is the child itself; run `rwv doctor --fix` to re-point to primary",
                new_parent.display()
            );
            continue;
        }
        if let Err(e) = crate::workspace::reparent(primary_root, &child_dir, marker, &new_parent) {
            eprintln!(
                "warning: failed to adopt child workweave {child}: could not rewrite \
                 marker ({e:#}); run `rwv doctor --fix` to re-point its parent"
            );
            continue;
        }
        eprintln!(
            "adopted child workweave {child}: parent now {}",
            new_parent.display()
        );
    }
}

/// The baselines the branch a workweave left in a store may be measured
/// against, as tips, restricted to the ones sharing `store`'s object DAG.
///
/// An `is_ancestor` query whose operands live in different object DAGs is
/// silently unsound (docs/explanation/joints/clone-topology.md), so a baseline
/// whose canonical store differs is simply not a baseline here.
fn baseline_tips_in_store(
    vcs: &dyn Vcs,
    store: &Path,
    baselines: &[PathBuf],
    rel: &Path,
) -> Vec<ResolvedRevisionId> {
    let mut tips = Vec::new();
    for base in baselines {
        let candidate = base.join(rel);
        if !vcs.is_repo(&candidate) || store_behind(vcs, &candidate) != store {
            continue;
        }
        if let Ok(tip) = vcs.head_revision(&candidate) {
            tips.push(tip);
        }
    }
    tips
}

/// Delete the branch a workweave was given in `store` when the baselines'
/// checkouts reach it, and say so when they do not. Backups, pins and the
/// branch it lands into are the deletion rule's; this is the lineage judgment
/// of the verbs that take a checkout out of a store without having a weave
/// branch to judge it by.
fn retire_lineage_branch(
    vcs: &dyn Vcs,
    store: &Path,
    ephemeral: &EphemeralRefName,
    label: &str,
    baseline_tips: &[ResolvedRevisionId],
    releaser: Releaser,
) {
    let say = releaser.prefix();
    let raw = ephemeral.to_raw();
    match DestroyProof::landed(vcs, store, Site::Local, &raw, baseline_tips) {
        Ok(Some(proof)) => {
            let why = proof.describe();
            if let Err(e) = vcs.delete_branch(proof) {
                eprintln!("{say}warning: {label}: could not delete branch {raw} ({why}): {e:#}");
            }
        }
        Ok(None) => {
            if matches!(vcs.resolve_local_branch_tip(store, &raw), Ok(Some(_))) {
                eprintln!(
                    "{say}left: {label}: branch {raw} holds commits no baseline contains; \
                     left in place"
                );
            }
        }
        Err(e) => eprintln!("{say}warning: {label}: could not judge branch {raw}: {e:#}"),
    }
}

/// The verb taking a workweave's checkout out of its store, as the lines about
/// the branch it retires there say it.
#[derive(Clone, Copy)]
pub(crate) enum Releaser {
    Prune,
    Remove,
}

impl Releaser {
    fn prefix(self) -> &'static str {
        match self {
            Self::Prune => "  ",
            Self::Remove => "",
        }
    }
}

/// A workweave's checkout of one repo, on its way out of the store it links
/// into, with the branch the workweave was given there and the tips that
/// count as having landed that branch.
pub(crate) struct WorkweaveCheckout {
    store: PathBuf,
    path: PathBuf,
    ephemeral: EphemeralRefName,
    baseline_tips: Vec<ResolvedRevisionId>,
}

impl WorkweaveCheckout {
    pub(crate) fn store(&self) -> &Path {
        &self.store
    }

    /// `baselines` are the workspaces whose checkout of `rel` counts as
    /// landed work ([`merge_baselines`]); only those sharing `store` vouch.
    fn new(
        vcs: &dyn Vcs,
        store: PathBuf,
        path: PathBuf,
        workweave: &WorkweaveId,
        baselines: &[PathBuf],
        rel: &Path,
    ) -> Self {
        Self {
            baseline_tips: baseline_tips_in_store(vcs, &store, baselines, rel),
            ephemeral: EphemeralRefName::mint(&workweave.flat()),
            store,
            path,
        }
    }

    /// For a workweave that stays: what lands its work is its parent's
    /// lineage and the primary weave's.
    pub(crate) fn of_live_workweave(
        vcs: &dyn Vcs,
        primary_root: &Path,
        workweave_dir: &Path,
        workweave: &WorkweaveId,
        store: PathBuf,
        path: PathBuf,
        rel: &Path,
    ) -> Self {
        let baselines = merge_baselines(workweave_dir, &CanonicalPath::of(primary_root), None);
        Self::new(vcs, store, path, workweave, &baselines, rel)
    }
}

/// Take one of a workweave's checkouts out of the store it links into: remove
/// the worktree, drop its registration, and delete the branch the workweave
/// was given in that store when its baselines reach it.
///
/// The branch pass runs whatever the removal did: a branch still held by a
/// checkout that would not come off is reported, not skipped. The removal's
/// own failure is what returns.
pub(crate) fn release_checkout(
    vcs: &dyn Vcs,
    checkout: &WorkweaveCheckout,
    label: &str,
    releaser: Releaser,
) -> Result<(), VcsError> {
    let removed = remove_checkout(vcs, &checkout.store, &checkout.path);
    retire_lineage_branch(
        vcs,
        &checkout.store,
        &checkout.ephemeral,
        label,
        &checkout.baseline_tips,
        releaser,
    );
    removed
}

/// Remove the worktree at `path` from `store` and drop its registration.
fn remove_checkout(vcs: &dyn Vcs, store: &Path, path: &Path) -> Result<(), VcsError> {
    let removed = if path.exists() {
        vcs.remove_worktree(store, path)
    } else {
        Ok(())
    };
    let _ = vcs.worktree_prune(store);
    removed
}

/// What a workweave delete discarded, by kind. Each is kept as a backup first.
#[derive(Debug, Clone, Default, serde::Serialize, schemars::JsonSchema)]
pub struct Discarded {
    /// Uncommitted edits, by repo.
    pub edits: Vec<KeptWork>,
    /// What a detached HEAD held that no branch reaches, by repo.
    pub detached_commits: Vec<KeptWork>,
}

#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct KeptWork {
    pub path: String,
    /// The name `rwv backup drop` takes.
    pub backup: String,
}

/// What `rwv workweave delete` did.
#[derive(Debug, Clone, Default)]
pub struct WorkweaveDeletion {
    pub discarded: Discarded,
    /// The deletion rule's outcome, when the workweave's aligned branch is a
    /// feature.
    pub rule: Option<crate::deletion::Outcome>,
}

impl WorkweaveDeletion {
    pub fn left_something(&self) -> bool {
        self.rule.as_ref().is_some_and(|r| r.left_something())
    }

    /// The branches the rule deleted after backing them up first.
    pub fn unlanded_branches(&self) -> Vec<&crate::deletion::Deleted> {
        self.rule
            .iter()
            .flat_map(crate::deletion::Outcome::backed_up)
            .collect()
    }

    pub fn print(&self) {
        for kept in &self.discarded.edits {
            eprintln!(
                "discarded edits: {} kept as backup {} (`rwv backup list`)",
                kept.path, kept.backup
            );
        }
        for kept in &self.discarded.detached_commits {
            eprintln!(
                "discarded detached commits: {} kept as backup {} (`rwv backup list`)",
                kept.path, kept.backup
            );
        }
        if let Some(rule) = &self.rule {
            for deleted in self.unlanded_branches() {
                eprintln!(
                    "discarded unlanded branch: {} {} kept as backup {} (`rwv backup list`)",
                    deleted.path,
                    deleted.branch,
                    deleted.backup.as_deref().unwrap_or_default()
                );
            }
            rule.print();
        }
    }
}

/// Delete a workweave: remove worktrees (including project repo), delete the
/// workweave directory, and apply the deletion rule to its aligned branch when
/// that is a feature.
///
/// Refuses to delete a workweave with uncommitted changes (in the project
/// worktree or any manifest-repo worktree) unless `discard_uncommitted` carries
/// the operator's consent, and one with a detached HEAD holding commits no
/// branch reaches unless `discard_unlanded` does; each is kept as a backup
/// first. A branch of the workweave that has not landed is not a refusal: the
/// rule keeps it, and `discard_unlanded` backs it up and deletes it. Only CLI
/// dispatch can mint either token, so no other caller can waive either refusal.
///
/// Independently of both waivers, refuses when any per-repo checkout in the
/// workweave is itself a canonical store with foreign worktrees linked into
/// it. This is the tier-0 invariant the clone-topology joint defines; delete
/// cannot safely proceed because the destructive worktree-remove + dir
/// removal would orphan the dependents. The operator must repair topology
/// via `rwv doctor` first.
///
/// The directory comes from the registry, and a missing, stale or foreign
/// entry refuses (see [`ensure_registered_workweave`]) rather than guessing
/// where the workweave lives. That round-trip witnesses that the victim is a
/// legitimate workweave of `project`, never that it is the one the caller
/// meant. `expected_dir` witnesses the latter: a caller that reached this
/// workweave through a path passes the directory it believes `name` denotes,
/// and a disagreement with the registry refuses instead of deleting somewhere
/// else. A caller holding nothing but a name passes `None`.
pub fn delete_workweave(
    ws_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    expected_dir: Option<&Path>,
    discard_uncommitted: Option<DiscardUncommittedConsent>,
    discard_unlanded: Option<DiscardUnlandedConsent>,
    remote_deletes: &crate::deletion::RemoteDeletes,
) -> anyhow::Result<WorkweaveDeletion> {
    let workweave_dir = ensure_registered_workweave(ws_root, project, name)?;
    if let Some(expected) = expected_dir {
        let expected = CanonicalPath::of(expected);
        if CanonicalPath::of(&workweave_dir) != expected {
            crate::refuse!(
                RefusalKind::RegistryPathDisagreement,
                "workweave `{name}` of project `{project}` is registered at {registered}, \
                 but the caller reached it through {expected} — refusing to delete either.\n\n\
                 Two directories cannot both be this workweave. Run `rwv doctor` to find \
                 out which of them the registry should name.",
                name = name.as_str(),
                project = project.as_str(),
                registered = workweave_dir.display(),
                expected = expected.as_path().display(),
            );
        }
    }
    delete_workweave_inner_at(
        ws_root,
        project,
        name,
        &workweave_dir,
        discard_uncommitted,
        discard_unlanded,
        remote_deletes,
        Deleter::Operator,
    )
}

/// Keep, as backups, the work in `workweave_dir`'s checkouts that the discard
/// flags consent to losing and nothing else preserves: each linked checkout's
/// uncommitted edits, and what a detached HEAD holds that no branch reaches. A
/// checkout that is its own store is removed with its refs, so a backup
/// written there would keep nothing.
fn keep_work_before_discarding(
    ws_root: &Path,
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
    discard_uncommitted: bool,
    discard_unlanded: bool,
) -> anyhow::Result<Discarded> {
    let mut discarded = Discarded::default();
    if !discard_uncommitted && !discard_unlanded {
        return Ok(discarded);
    }
    let project_vcs = project_vcs();
    let project_rel = project_rel_dir(project);
    let mut checkouts: Vec<(String, PathBuf, PathBuf, Box<dyn Vcs>)> = Vec::new();
    let project_worktree = workweave_dir.join(&project_rel);
    if project_worktree.exists() && crate::git::is_linked_worktree(&project_worktree) {
        checkouts.push((
            project_rel_path(project),
            project_worktree,
            ws_root.join(&project_rel),
            project_vcs,
        ));
    }
    for checkout in member_depth_checkouts(workweave_dir, manifest) {
        let worktree = workweave_dir.join(checkout.repo_path.as_path());
        if worktree.exists() {
            checkouts.push((
                checkout.label(),
                worktree,
                ws_root.join(checkout.repo_path.as_path()),
                checkout.vcs,
            ));
        }
    }
    for (label, worktree, fallback, vcs) in checkouts {
        if resolved_worktree_parent(vcs.as_ref(), &worktree, &fallback)
            == CanonicalPath::of(&worktree)
        {
            continue;
        }
        if discard_uncommitted {
            if let Some(witness) = vcs.backup_edits(&worktree).with_context(|| {
                format!("{label}: its uncommitted edits could not be kept as a backup")
            })? {
                discarded.edits.push(KeptWork {
                    path: label.clone(),
                    backup: witness.backup().name.to_string(),
                });
            }
        }
        if discard_unlanded {
            if let Some(witness) = vcs.backup_unpreserved_head(&worktree).with_context(|| {
                format!(
                    "{label}: the commits its detached HEAD holds could not be kept as a backup"
                )
            })? {
                discarded.detached_commits.push(KeptWork {
                    path: label.clone(),
                    backup: witness.backup().name.to_string(),
                });
            }
        }
    }
    Ok(discarded)
}

/// The checkouts of `workweave_dir` whose HEAD is detached holding commits no
/// branch or remote-tracking branch reaches, judged on the `alignment` axis.
fn judge_detached_commits(
    project_vcs: &dyn Vcs,
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
) -> anyhow::Result<crate::diagnosis::Report> {
    let mut report = crate::diagnosis::Report::default();
    let mut judge = |vcs: &dyn Vcs, label: String, worktree: PathBuf| -> anyhow::Result<()> {
        if !worktree.exists() || !vcs.is_repo(&worktree) {
            return Ok(());
        }
        let crate::vcs::HeadObservation::Detached { at } = vcs.observe_head(&worktree)? else {
            return Ok(());
        };
        let stray =
            vcs.reached_by_no_branch(&worktree, &at, crate::vcs::RemoteTracking::Counted)?;
        if stray.is_empty() {
            return Ok(());
        }
        report.add(
            crate::diagnosis::State::AlignmentDetachedUnpushed
                .of(label)
                .listing(
                    stray
                        .iter()
                        .map(|c| format!("{} {}", c.id.short(), c.subject)),
                )
                .checkout(&worktree)
                .judge(vcs),
        );
        Ok(())
    };
    judge(
        project_vcs,
        project_rel_path(project),
        workweave_dir.join(project_rel_dir(project)),
    )?;
    for checkout in member_depth_checkouts(workweave_dir, manifest) {
        judge(
            checkout.vcs.as_ref(),
            checkout.label(),
            workweave_dir.join(checkout.repo_path.as_path()),
        )?;
    }
    Ok(report)
}

/// The feature branch a workweave's deletion judges, and the branch it lands
/// into: the branch its project checkout is aligned for, when its record is a
/// feature's. A detached or declared workspace has none.
fn feature_of(
    ws_root: &Path,
    project: &ProjectName,
    workweave_dir: &Path,
) -> anyhow::Result<Option<(crate::records::BranchName, crate::records::BranchName)>> {
    let vcs = project_vcs();
    let checkout = project_dir(workweave_dir, project);
    if !vcs.is_repo(&checkout) {
        return Ok(None);
    }
    let crate::alignment::WorkspaceState {
        aligned: Some(crate::alignment::Aligned::Branch(aligned)),
        ..
    } = crate::alignment::read_state(vcs.as_ref(), &checkout)?.state
    else {
        return Ok(None);
    };
    let primary_project = project_dir(ws_root, project);
    let branch = match crate::records::BranchName::parse(vcs.as_ref(), &aligned) {
        Ok(branch) => branch,
        Err(_) => return Ok(None),
    };
    let store = crate::records::Store::open(vcs.as_ref(), &primary_project)?;
    let view = store.view()?;
    match view.lookup(&branch) {
        crate::records::Lookup::Value(crate::records::Record::Feature { target, .. }) => {
            Ok(Some((branch, target)))
        }
        crate::records::Lookup::Value(crate::records::Record::Declared) => Ok(None),
        lookup => Err(crate::branch::judge_lookup(
            vcs.as_ref(),
            &store,
            project,
            &primary_project,
            &branch,
            lookup,
        )?
        .refused()),
    }
}

/// The envelope `rwv workweave delete --json` prints.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct WorkweaveDeleteJsonOutput {
    #[serde(rename = "$schema")]
    pub schema_url: String,
    pub project: String,
    pub discarded: DiscardedOutput,
    /// The deletion rule's outcome; empty when the workweave's aligned
    /// branch is not a feature.
    #[serde(flatten)]
    pub outcome: crate::deletion::Outcome,
}

/// What a workweave delete discarded, by kind.
#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct DiscardedOutput {
    pub edits: Vec<KeptWork>,
    pub detached_commits: Vec<KeptWork>,
    /// Branches that had not landed, each deleted after the backup named.
    pub unlanded_branches: Vec<UnlandedBranch>,
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct UnlandedBranch {
    pub path: String,
    pub branch: String,
    pub backup: String,
}

pub const WORKWEAVE_SCHEMA_URL: &str = crate::schema_url::schema_url!("workweave");

/// Print what a workweave delete did: by kind what it discarded, then the
/// deletion rule's outcome.
pub fn report_deletion(
    deletion: &WorkweaveDeletion,
    project: &ProjectName,
    output: crate::parallel::OutputMode,
) -> anyhow::Result<()> {
    if output.is_json() {
        let envelope = WorkweaveDeleteJsonOutput {
            schema_url: WORKWEAVE_SCHEMA_URL.to_owned(),
            project: project.to_string(),
            discarded: DiscardedOutput {
                edits: deletion.discarded.edits.clone(),
                detached_commits: deletion.discarded.detached_commits.clone(),
                unlanded_branches: deletion
                    .unlanded_branches()
                    .into_iter()
                    .map(|d| UnlandedBranch {
                        path: d.path.clone(),
                        branch: d.branch.clone(),
                        backup: d.backup.clone().unwrap_or_default(),
                    })
                    .collect(),
            },
            outcome: deletion.rule.clone().unwrap_or_default(),
        };
        outln!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        deletion.print();
    }
    Ok(())
}

/// Delete a workweave as the terminal step of the op that owns it (`land
/// --retire`).
///
/// `workweave_dir` is the on-disk path resolved by the sync engine at op
/// start, so no name is looked up; the marker, the index and the worktree
/// relation must still agree on it, as for `workweave delete`, however long
/// the op was parked.
///
/// `--retire` has no waiver flag, so it holds neither consent: uncommitted
/// work and a detached HEAD holding unlanded commits refuse, and a branch
/// that has not landed survives the rule.
pub(crate) fn delete_workweave_for_retire(
    ws_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    workweave_dir: &Path,
    landed_into: crate::records::BranchName,
) -> anyhow::Result<WorkweaveDeletion> {
    crate::workspace::require_agreement(&CanonicalPath::of(ws_root), project, workweave_dir)?;
    delete_workweave_inner_at(
        ws_root,
        project,
        name,
        workweave_dir,
        None,
        None,
        &crate::deletion::RemoteDeletes::default(),
        Deleter::RetiringOp { landed_into },
    )
}

/// Who deletes a workweave: whether an in-flight op record refuses the delete.
enum Deleter {
    /// `rwv workweave delete`, which an op record in the workweave refuses.
    Operator,
    /// The `land --retire` op that has just landed the workweave's branch
    /// into `landed_into`, which the deletion rule judges it against. It
    /// holds its record there until its own cleanup phase, after the delete.
    RetiringOp {
        landed_into: crate::records::BranchName,
    },
}

/// Say, before the workweave's root goes whole, which of its entries rwv did
/// not create: none of the project, a member's first path component, a file
/// an integration owns, or rwv's own state.
fn name_foreign_root_entries(workweave_dir: &Path, project: &ProjectName, primary: &Manifest) {
    let own = load_manifest(workweave_dir, project).ok();
    let mut made: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::from([crate::workspace::PROJECTS_DIR.to_owned()]);
    for manifest in [Some(primary), own.as_ref()].into_iter().flatten() {
        made.extend(manifest.iter_entries().filter_map(|(path, _)| {
            path.as_path()
                .components()
                .next()
                .map(|first| first.as_os_str().to_string_lossy().into_owned())
        }));
        made.extend(
            crate::activate::owned_paths(workweave_dir, project, manifest)
                .into_iter()
                .filter_map(|path| path.split('/').next().map(str::to_owned)),
        );
    }
    let Ok(entries) = std::fs::read_dir(workweave_dir) else {
        return;
    };
    let mut foreign: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !made.contains(name) && !name.starts_with(".rwv"))
        .collect();
    if foreign.is_empty() {
        return;
    }
    foreign.sort();
    const SHOWN: usize = 5;
    let more = match foreign.len().saturating_sub(SHOWN) {
        0 => String::new(),
        more => format!(" and {more} more"),
    };
    eprintln!(
        "rwv: removing {} whole, with {} entries rwv did not create: {}{more}",
        workweave_dir.display(),
        foreign.len(),
        foreign
            .iter()
            .take(SHOWN)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    );
}

/// Delete a workweave whose on-disk path is already known.
///
/// The path is validated by the caller (retire has already round-tripped
/// the marker via workspace resolution). This bypasses the
/// registry lookup so an unrecorded workweave (crash-matrix scaffolding,
/// bootstrap workspace) is still delete-able by callers that already hold
/// the resolved path. Callers arriving through a name route through
/// [`delete_workweave`], which enforces the registry lookup first.
///
/// The registry entry is still removed at the end (best effort), so a
/// mid-crash resume that finds a pre-crash entry keeps the index in sync.
#[allow(clippy::too_many_arguments)]
fn delete_workweave_inner_at(
    ws_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    workweave_dir: &Path,
    discard_uncommitted: Option<DiscardUncommittedConsent>,
    discard_unlanded: Option<DiscardUnlandedConsent>,
    remote_deletes: &crate::deletion::RemoteDeletes,
    deleter: Deleter,
) -> anyhow::Result<WorkweaveDeletion> {
    let project_vcs = project_vcs();
    let manifest = load_manifest(ws_root, project)?;
    let workweave_dir = workweave_dir.to_path_buf();

    refuse_symlinked_members(&workweave_dir, project)?;

    // Tier-0 topology precondition: refuse when a per-repo checkout inside
    // the workweave is itself a canonical store with foreign dependents.
    // Runs before the dirty / unlanded checks (and is not bypassable by any
    // override flag) because the hazard is to OTHER workspaces, not the
    // workweave's own work. See docs/explanation/joints/clone-topology.md and
    // docs/explanation/destructive-operations.md (precondition-or-stop).
    if workweave_dir.exists() {
        refuse_if_checkouts_host_foreign_worktrees(
            project_vcs.as_ref(),
            &workweave_dir,
            project,
            &manifest,
        )?;
    }

    // Refuse before the dirty/unlanded checks so a mid-op delete reports the
    // in-flight op. No override: `rwv abort` is the way to clear a stale record.
    if workweave_dir.exists() && matches!(deleter, Deleter::Operator) {
        crate::op_state::check_no_op_in_progress(&[workweave_dir.as_path()])?;
    }

    // Every refusal is judged before anything changes.
    let mut refusals = crate::diagnosis::Report::default();
    if discard_uncommitted.is_some() && workweave_dir.exists() {
        for line in
            judge_mid_operation_checkouts(project_vcs.as_ref(), &workweave_dir, project, &manifest)
        {
            refusals.add(line);
        }
    }
    if discard_uncommitted.is_none() && workweave_dir.exists() {
        for line in judge_dirty_checkouts(
            project_vcs.as_ref(),
            &workweave_dir,
            project,
            &manifest,
            matches!(deleter, Deleter::Operator).then_some(name),
        )
        .lines()
        {
            refusals.add(line.clone());
        }
    }
    if discard_unlanded.is_none() && workweave_dir.exists() {
        let detached =
            judge_detached_commits(project_vcs.as_ref(), &workweave_dir, project, &manifest)?;
        if !detached.is_empty() {
            for line in detached.lines() {
                refusals.add(line.clone());
            }
            refusals.note(
                "`--discard-unlanded` keeps these commits as a backup and removes the workweave",
            );
        }
    }
    refusals.into_result()?;

    let rule = match feature_of(ws_root, project, &workweave_dir)? {
        Some((branch, recorded)) => Some(crate::deletion::Rule::open(crate::deletion::Request {
            primary: ws_root,
            project,
            branch,
            target: match &deleter {
                Deleter::Operator => recorded,
                Deleter::RetiringOp { landed_into } => landed_into.clone(),
            },
            discard_unlanded,
            remote_deletes,
            leaving: Some(&workweave_dir),
            verb: match &deleter {
                Deleter::Operator => "workweave delete",
                Deleter::RetiringOp { .. } => "land",
            },
        })?),
        None => None,
    };

    // A discard flag consents to losing work, never to losing it with no
    // way back: what each consents to is kept as a backup before anything is
    // removed, and a backup that cannot be written stops the delete here.
    let discarded = if workweave_dir.exists() {
        keep_work_before_discarding(
            ws_root,
            &workweave_dir,
            project,
            &manifest,
            discard_uncommitted.is_some(),
            discard_unlanded.is_some(),
        )?
    } else {
        Discarded::default()
    };

    // Strip the managed regions this checkout's integrations own before the
    // checkout is taken apart. The regions exist because this workweave
    // presented the project; that ends here.
    if workweave_dir.exists() {
        let project_checkout = workweave_dir.join(project_rel_dir(project));
        for issue in crate::activate::strip_project_regions(&project_checkout, &manifest) {
            eprintln!(
                "rwv workweave delete: warning: {}: {}",
                issue.source, issue.message
            );
        }
    }

    // Adopt any living children BEFORE destroying the retiree. This re-points
    // each child's recorded parent to the retiree's own recorded parent (the
    // grandparent; fall back to primary) so a bare `rwv land` from a child
    // does not later die on a dangling parent. Runs while the retiree's marker
    // still exists (it names the grandparent). Shared by delete and retire.
    if workweave_dir.exists() {
        adopt_children_of(&workweave_dir, ws_root);
    }

    // Remove worktrees for each repo, collecting errors.
    let mut errors: Vec<String> = Vec::new();

    for checkout in member_depth_checkouts(&workweave_dir, &manifest) {
        let (repo_path, vcs) = (&checkout.repo_path, &checkout.vcs);
        let worktree_path = workweave_dir.join(repo_path.as_path());

        // Resolve the worktree's ACTUAL canonical store on disk rather than
        // assuming `ws_root.join(repo_path)` is the parent. Under
        // tier-0-correct topology these match; under inverted topology
        // the canonical store lives in another workweave and
        // `ws_root.join(repo_path)` is a disconnected clone that doesn't
        // know about this checkout — running `worktree remove` there leaves
        // a stale registration. See docs/explanation/joints/clone-topology.md.
        let fallback_repo_abs = ws_root.join(repo_path.as_path());
        let repo_store = resolved_worktree_parent(vcs.as_ref(), &worktree_path, &fallback_repo_abs);

        // A checkout that is its own canonical store has no parent to
        // unregister from, and nothing else can know about its refs once the
        // directory is gone. `refuse_if_checkouts_host_foreign_worktrees`
        // above has already refused one with dependents, so removing the
        // workweave directory is all this one needs.
        if worktree_path.exists() && repo_store == CanonicalPath::of(&worktree_path) {
            continue;
        }

        if let Err(e) = remove_checkout(vcs.as_ref(), repo_store.as_path(), &worktree_path) {
            let msg = format!("{}: {e:#}", checkout.label());
            eprintln!("rwv workweave delete: error: {msg}");
            errors.push(msg);
        }
    }

    // Remove the project repo worktree.
    // Only call remove_worktree if the workweave copy is actually a git worktree,
    // indicated by .git being a FILE (not a directory). If .git is a directory
    // (or absent), the workweave copy was a plain directory copy — just let
    // remove_dir_all below handle it.
    let project_rel = project_rel_dir(project);
    let project_dir_fallback = ws_root.join(&project_rel);
    let project_worktree = workweave_dir.join(&project_rel);
    // Resolve the project worktree's actual canonical store, same as for
    // manifest repos above. Resolved before the removal, while the checkout
    // is still there to be asked.
    let project_store = resolved_worktree_parent(
        project_vcs.as_ref(),
        &project_worktree,
        &project_dir_fallback,
    )
    .into_path_buf();
    if project_worktree.exists() && crate::git::is_linked_worktree(&project_worktree) {
        if let Err(e) = remove_checkout(project_vcs.as_ref(), &project_store, &project_worktree) {
            let msg = format!("{}: {e:#}", project_rel_path(project));
            eprintln!("rwv workweave delete: error: {msg}");
            errors.push(msg);
        }
    }

    // Remove the workweave directory itself. A process's working directory
    // is an open handle on Windows, and `land --retire` drives this
    // delete standing inside the workweave — the removal then fails with a
    // sharing violation whose holder is the deleting process itself. Step
    // out to the parent first, but only when the current directory sits
    // under the tree being removed: the hosting process may not be rwv's
    // own (a test harness shares one working directory across every
    // concurrent thread), so a directory this delete does not hold hostage
    // stays where it is. A working directory that cannot be read or resolved
    // cannot be proven outside, and stepping out of it is harmless. The live cwd — not the resolved origin — is the subject on
    // purpose: `-C` points the origin elsewhere while the handle stays put.
    if workweave_dir.exists() {
        name_foreign_root_entries(&workweave_dir, project, &manifest);
        let cwd_inside = match (
            std::env::current_dir().and_then(|d| CanonicalPath::existing(&d)),
            CanonicalPath::existing(&workweave_dir),
        ) {
            (Ok(cwd), Ok(ww)) => ww.contains(&cwd),
            _ => true,
        };
        if cwd_inside {
            if let Some(parent) = workweave_dir.parent() {
                let _ = std::env::set_current_dir(parent);
            }
        }
        remove_tree_outlasting_child_handles(&workweave_dir)?;
    }

    // Retire the registry entry. Best-effort: a delete that removes the
    // on-disk directory but fails to update the index leaves a stale entry
    // that doctor will prune on the next round; that is strictly worse than
    // silently succeeding on a locked / read-only registry but strictly
    // better than the disk-and-registry drifting silently. We warn but do
    // not bail because the primary destructive act already succeeded.
    if let Err(e) = workweave_index::forget_workweave(ws_root, project, name) {
        eprintln!(
            "rwv workweave delete: warning: workweave directory removed but registry \
             entry for `{}` could not be updated ({e:#}); run `rwv doctor --fix` to \
             prune the stale entry",
            name.as_str()
        );
    }

    if !errors.is_empty() {
        let total = manifest.len() + 1;
        let failed = errors.len();
        return Err(crate::untokened!(
            Tally,
            "workweave delete completed with {failed} failure(s) out of {total} repo(s)"
        ));
    }

    let rule = rule.map(crate::deletion::Rule::apply).transpose()?;
    Ok(WorkweaveDeletion { discarded, rule })
}

/// List workweaves for `project` under `ws_root`'s primary.
///
/// Registry-backed: reads the recorded `name → path` entries and returns
/// those whose marker round-trips (`marker.primary` == this primary AND
/// `marker.project` == this project). Stale / foreign entries are silently
/// omitted here; doctor is the channel that flags them.
///
/// **Missing index is not fatal.** A workspace that predates the registry
/// (or that has never been touched by `workweave create` since the index
/// landed) has no workweave index. List returns an empty vector in
/// that case; `rwv doctor --fix` scans the container and adopts on-disk
/// workweaves into the registry with the operator's consent.
pub fn list_workweaves(
    ws_root: &Path,
    project: &ProjectName,
) -> anyhow::Result<Vec<WorkweaveName>> {
    let mut names: Vec<WorkweaveName> = list_workweave_dirs_for_project(ws_root, project)?
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    names.sort();
    Ok(names)
}

/// Return `(name, path)` pairs for workweaves of `project` under `ws_root`'s
/// primary. Registry-backed with marker round-trip validation.
///
/// An entry that fails validation (missing dir, missing / foreign marker,
/// project mismatch) is silently omitted so a stale registry does not
/// pollute the operator-facing list. Doctor surfaces those cases separately
/// as [`crate::check::WorkweaveTreeIntegrityKind`] findings.
fn list_workweave_dirs_for_project(
    ws_root: &Path,
    project: &ProjectName,
) -> anyhow::Result<Vec<(WorkweaveName, PathBuf)>> {
    let index = match workweave_index::read(ws_root, project)? {
        Some(idx) => idx,
        None => return Ok(Vec::new()),
    };
    let primary = CanonicalPath::of(ws_root);
    let mut result: Vec<(WorkweaveName, PathBuf)> = index
        .workweaves
        .into_iter()
        .filter(|(_, entry)| {
            matches!(
                validate_registry_entry(&primary, project, entry.path.as_path()),
                RegistryEntryValidation::Valid
            )
        })
        .map(|(name, entry)| (name, entry.path.into_path_buf()))
        .collect();
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

/// Every marker-bearing workweave directory under `ws_root`'s primary, across
/// every workweave container, with its identity.
///
/// **Scanning-based, not registry-backed.** Consumers that need to see every
/// workweave regardless of registry state — doctor's per-workweave drift
/// scans, `adopt_children_of` (child re-pointing by parent path) — use this.
/// The user-facing `rwv workweave list` uses [`list_workweaves`] instead so
/// unregistered on-disk directories are not silently visible in `list` output;
/// doctor's `unregistered-workweave` finding is the one channel that surfaces
/// them.
///
/// Enumerates every unique container (default `<parent-of-primary>/.workweaves`
/// plus every recorded per-project container) and returns marker-bearing
/// directories whose `.rwv-workweave` `primary` canonicalizes to `ws_root`.
pub fn list_workweave_dirs(ws_root: &Path) -> Vec<(WorkweaveId, PathBuf)> {
    let mut containers = std::collections::BTreeSet::new();
    containers.insert(CanonicalPath::of(&workweave_index::default_basedir(
        ws_root,
    )));
    for project in crate::workspace::discover_projects(ws_root) {
        if let Ok(Some(idx)) = workweave_index::read(ws_root, &project) {
            if let Some(basedir) = idx.basedir {
                containers.insert(basedir);
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut result: Vec<(WorkweaveId, PathBuf)> = Vec::new();
    for container in &containers {
        for (id, dir) in doctor_scan_container(ws_root, container.as_path()) {
            if seen.insert(CanonicalPath::of(&dir)) {
                result.push((id, dir));
            }
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Container-scoped scan used ONLY by doctor for reconciliation.
///
/// Enumerates directories on disk under `container` that carry a valid
/// `.rwv-workweave` marker whose `primary` resolves to `ws_root`, each with
/// its identity.
///
/// **Identity is by record, never by name shape.** The PROJECT comes from
/// the marker — the record the directory itself carries — and the NAME
/// comes from the registry entry naming this path when one exists
/// ([`workweave_name_for_path`]), falling back for unregistered directories
/// to the name half the marker's project would have rendered, which is an
/// exact strip of that project's own spelling and not a split at the first
/// separator in the basename. The basename is discovery, not identity: a
/// hand-renamed directory keeps its recorded identity, so the branch scans
/// keep validating the branch the records own instead of deriving a new
/// expectation from the rename. A directory whose identity is unrecoverable
/// (no registry entry, and a basename the marker's project does not render)
/// is skipped here; the tree-integrity scan's misnamed-dir finding owns
/// reporting it.
///
/// This is the ONLY surviving on-disk scan (the pre-registry list/delete
/// scan was deleted). Every other code path resolves via the registry.
pub fn doctor_scan_container(ws_root: &Path, container: &Path) -> Vec<(WorkweaveId, PathBuf)> {
    let primary = CanonicalPath::of(ws_root);
    let mut result = Vec::new();
    let entries = match std::fs::read_dir(container) {
        Ok(e) => e,
        Err(_) => return result,
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let marker = match WorkweaveMarker::read(&dir) {
            Ok(Some(m)) => m,
            _ => continue,
        };
        if !marker.names_primary(&primary) {
            continue;
        }
        let project_name = marker.project().clone();
        let name = marker.name().clone();
        result.push((
            WorkweaveId {
                project: project_name,
                name,
            },
            dir,
        ));
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    result
}

/// Load the project manifest from the workspace.
fn load_manifest(ws_root: &Path, project: &ProjectName) -> anyhow::Result<Manifest> {
    let manifest_path = project_dir(ws_root, project).join(Manifest::FILE_NAME);
    Manifest::from_path(&manifest_path)
}

// ---------------------------------------------------------------------------
// Claude Code hook mode
// ---------------------------------------------------------------------------

/// Input JSON sent by Claude Code for WorktreeCreate / WorktreeRemove hooks.
#[derive(serde::Deserialize)]
struct ClaudeHookInput {
    cwd: Option<String>,
    branch_name: Option<String>,
    session_id: Option<String>,
    hook_event_name: Option<crate::setup::HookEvent>,
    worktree_path: Option<String>,
}

/// Derive a workweave name from the hook payload.
///
/// Priority: branch_name → timestamp+nanos fallback.
/// Session ID is not used — it's constant within a session, causing
/// collisions when multiple workweaves are created concurrently in one
/// session.
/// Slashes are replaced with dashes for filesystem safety.
fn derive_workweave_name(branch_name: Option<&str>, _session_id: Option<&str>) -> String {
    let raw = match branch_name {
        Some(b) if !b.is_empty() && b != "null" => b.to_string(),
        _ => {
            let d = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            // Mix seconds and nanos into a short hex suffix for uniqueness
            let hash = d.as_secs() ^ (d.subsec_nanos() as u64);
            format!("workweave-{:08x}", hash)
        }
    };
    raw.replace('/', "-")
}

/// Handle a Claude Code hook invocation.
///
/// Reads JSON from stdin, dispatches on `hook_event_name`:
/// - `WorktreeCreate` — creates a workweave and prints its path to stdout.
/// - `WorktreeRemove` — deletes the workweave (fire-and-forget; always exits 0).
pub fn handle_claude_hook() -> anyhow::Result<()> {
    let input: ClaudeHookInput = serde_json::from_reader(std::io::stdin())
        .context("failed to parse hook JSON from stdin")?;

    use crate::setup::HookEvent;

    match input.hook_event_name {
        Some(HookEvent::WorktreeCreate) => {
            let cwd = input
                .cwd
                .ok_or_else(|| crate::untokened!(HookInput, "missing cwd in hook input"))?;

            let ws_ctx =
                crate::workspace::WorkspaceContext::resolve_invocation(Path::new(&cwd), None)?;
            let primary_root = ws_ctx.primary_path();
            let source_root = ws_ctx.active_path();

            let project = ws_ctx
                .active_project()
                .ok_or_else(|| {
                    crate::refusal::refusal(
                        RefusalKind::NoActiveProject,
                        format!("no .rwv-active found in {}", primary_root.display()),
                    )
                })?
                .clone();

            let name =
                derive_workweave_name(input.branch_name.as_deref(), input.session_id.as_deref());

            let path = hand_over_seat(create_workweave(CreateWorkweave {
                primary_root,
                source_root,
                project: &project,
                name: &WorkweaveName::new(&name)?,
                branch: BranchChoice::Minted,
                spelling: None,
                at: None,
                entries: Vec::new(),
                placement: &Placement::DefaultBasedir,
                activation: crate::activate::ActivateOptions::default(),
            }))?;
            outln!("{}", path.display());
        }
        Some(HookEvent::WorktreeRemove) => {
            let worktree_path = input.worktree_path.ok_or_else(|| {
                crate::untokened!(HookInput, "missing worktree_path in hook input")
            })?;

            // Fire-and-forget: log errors but always succeed.
            if let Ok(Some(marker)) = WorkweaveMarker::read(Path::new(&worktree_path)) {
                let outcome = workweave_name_for_path(
                    marker.primary().as_path(),
                    marker.project(),
                    Path::new(&worktree_path),
                )
                .and_then(|found| {
                    found.ok_or_else(|| {
                        crate::refusal::refusal(
                            RefusalKind::UnregisteredWorkweave,
                            format!(
                                "{worktree_path} carries a workweave marker but is not \
                                 registered for project `{}` — refusing to delete rather \
                                 than guess its name",
                                marker.project()
                            ),
                        )
                    })
                })
                .and_then(|name| {
                    // "Claude moved on" is not the operator consenting to lose
                    // work: dirty or unmerged work refuses on stderr.
                    delete_workweave(
                        marker.primary().as_path(),
                        marker.project(),
                        &name,
                        Some(Path::new(&worktree_path)),
                        None,
                        None,
                        &crate::deletion::RemoteDeletes::default(),
                    )
                    .map(|deletion| deletion.print())
                });
                if let Err(e) = outcome {
                    eprintln!("rwv workweave --claude-hook WorktreeRemove: warning: {e:#}");
                }
            }
            // Always exit 0.
        }
        Some(other) => {
            return Err(crate::untokened!(HookInput, "`{other}` is not a workweave hook event: `rwv workweave --claude-hook` handles {} and {}",
                HookEvent::WORKWEAVE[0],
                HookEvent::WORKWEAVE[1]
            ));
        }
        None => {
            return Err(crate::untokened!(
                HookInput,
                "missing hook_event_name in hook input"
            ))
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::git_vcs;

    // -----------------------------------------------------------------------
    // Create rollback
    //
    // These drive the guard directly because the distinction it keys on —
    // `create_worktree_on` answering authored or adopted — is not observable
    // from the CLI once create refuses a name held by commits it did not make.
    // -----------------------------------------------------------------------

    /// Run git in `dir`, panicking on failure.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = crate::git::git_command_in_test()
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?} failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A primary weave holding one project and one repo with a commit.
    /// Returns `(tempdir, primary_root, project, store)`.
    fn weave() -> (tempfile::TempDir, PathBuf, ProjectName, PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = ProjectName::new("web-app").unwrap();
        std::fs::create_dir_all(primary.join("projects/web-app")).unwrap();
        let store = primary.join("github/org/repo");
        std::fs::create_dir_all(&store).unwrap();
        git(&store, &["init", "-b", "main"]);
        git(&store, &["config", "user.email", "t@t"]);
        git(&store, &["config", "user.name", "T"]);
        std::fs::write(store.join("f"), "1").unwrap();
        git(&store, &["add", "."]);
        git(&store, &["commit", "-m", "one"]);
        (tmp, primary, project, store)
    }

    /// A guard with nothing but the ref attempt under test recorded.
    fn guard_for(primary: &Path) -> CreateRollbackGuard {
        let mut g = CreateRollbackGuard::new(git_vcs(), primary.join("never-created"));
        g.defuse(); // Drop must not re-run what the test drives explicitly.
        g
    }

    fn minted(project: &ProjectName) -> EphemeralRefName {
        EphemeralRefName::mint(
            &crate::manifest::WorkweaveId {
                project: project.clone(),
                name: WorkweaveName::new("ww").unwrap(),
            }
            .flat(),
        )
    }

    #[test]
    fn missing_canonical_clone_names_the_fetch_of_its_own_project_at_primary() {
        let (_tmp, primary, project, _store) = weave();
        let err = materialize_member(
            git_vcs().as_ref(),
            &primary,
            &primary.join("ww"),
            &project,
            &WorkweaveName::new("ww").unwrap(),
            &RepoPath::new("github/org/absent").unwrap(),
            Role::Owned,
            SubmoduleRecursion::Recurse,
            "main",
            BranchPolicy::Restore,
        )
        .err()
        .expect("a member with no canonical clone is refused");
        let message = format!("{err:#}");
        assert!(
            message.contains("run `rwv update --project web-app` at the primary weave first"),
            "{message}"
        );
        assert!(
            !message.contains("rwv sync"),
            "sync takes a source and is not the repair: {message}"
        );
    }

    #[test]
    fn rollback_destroys_a_ref_this_create_authored() {
        let (_tmp, primary, project, store) = weave();
        let name = minted(&project);
        let at = git_vcs().head_revision(&store).unwrap();
        let found = FoundBranch::minted(store.clone(), &name, at);

        let dest = primary.join("wt");
        let origin = git_vcs().create_worktree_on(&found, &dest).unwrap();
        assert_eq!(
            origin,
            RefOrigin::Authored,
            "a fresh name must be AUTHORED, not adopted"
        );
        git_vcs().remove_worktree(&store, &dest).unwrap();

        let mut g = guard_for(&primary);
        g.record_ref_attempt(found, RefBirth::from(origin));
        assert!(g.undo_ref_births().is_empty(), "the destroy should succeed");

        assert!(
            git_vcs()
                .resolve_local_branch_tip(&store, &name.to_raw())
                .unwrap()
                .is_none(),
            "an authored ref must be destroyed on rollback"
        );
    }

    #[test]
    fn rollback_leaves_an_adopted_ref_alone() {
        let (_tmp, primary, project, store) = weave();
        let name = minted(&project);

        // A branch that was already there, carrying a commit of its own.
        git(&store, &["checkout", "-b", &name.to_string()]);
        std::fs::write(store.join("theirs"), "operator work").unwrap();
        git(&store, &["add", "."]);
        git(&store, &["commit", "-m", "theirs"]);
        let their_tip = git(&store, &["rev-parse", &name.to_string()]);
        git(&store, &["checkout", "main"]);

        let found = FoundBranch::minted(
            store.clone(),
            &name,
            git_vcs().head_revision(&store).unwrap(),
        );
        let dest = primary.join("wt");
        let origin = git_vcs().create_worktree_on(&found, &dest).unwrap();
        assert_eq!(
            origin,
            RefOrigin::Adopted,
            "fixture: a name already in the store must be ADOPTED, not authored"
        );
        git_vcs().remove_worktree(&store, &dest).unwrap();

        let mut g = guard_for(&primary);
        g.record_ref_attempt(found, RefBirth::from(origin));
        assert!(g.undo_ref_births().is_empty());

        assert_eq!(
            git(&store, &["rev-parse", &name.to_string()]),
            their_tip,
            "a create that ADOPTED a branch must not destroy it on rollback"
        );
    }

    #[test]
    fn rollback_reports_a_ref_that_moved_instead_of_destroying_it() {
        let (_tmp, primary, project, store) = weave();
        let name = minted(&project);
        let at = git_vcs().head_revision(&store).unwrap();
        let found = FoundBranch::minted(store.clone(), &name, at);

        let dest = primary.join("wt");
        let origin = git_vcs().create_worktree_on(&found, &dest).unwrap();
        assert_eq!(origin, RefOrigin::Authored);

        // A commit lands on the ephemeral branch after its birth, so no
        // landed proof against the start point can be made for it.
        std::fs::write(dest.join("later"), "committed after the birth").unwrap();
        git(&dest, &["add", "."]);
        git(&dest, &["commit", "-m", "later"]);
        let moved_tip = git(&store, &["rev-parse", &name.to_string()]);
        git_vcs().remove_worktree(&store, &dest).unwrap();

        let mut g = guard_for(&primary);
        g.record_ref_attempt(found, RefBirth::from(origin));
        let failures = g.undo_ref_births();

        assert_eq!(
            failures.len(),
            1,
            "the moved ref must be reported: {failures:?}"
        );
        assert!(
            failures[0].contains(&name.to_string()) && failures[0].contains("holds commits past"),
            "the report must name the branch and why it was kept: {failures:?}"
        );
        assert_eq!(
            git(&store, &["rev-parse", &name.to_string()]),
            moved_tip,
            "a ref that moved since its birth must survive rollback"
        );
    }

    // -----------------------------------------------------------------------
    // derive_workweave_name
    // -----------------------------------------------------------------------

    #[test]
    fn derive_name_uses_branch_name() {
        assert_eq!(
            derive_workweave_name(Some("feat/my-branch"), None),
            "feat-my-branch"
        );
    }

    #[test]
    fn derive_name_null_branch_uses_timestamp() {
        let name = derive_workweave_name(Some("null"), Some("abc-session-123"));
        assert!(
            name.starts_with("workweave-"),
            "session_id ignored, expected ww-<timestamp>, got {name}"
        );
    }

    #[test]
    fn derive_name_empty_branch_uses_timestamp() {
        let name = derive_workweave_name(Some(""), Some("sess-xyz"));
        assert!(
            name.starts_with("workweave-"),
            "session_id ignored, expected ww-<timestamp>, got {name}"
        );
    }

    #[test]
    fn derive_name_timestamps_are_unique() {
        let a = derive_workweave_name(None, None);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let b = derive_workweave_name(None, None);
        assert_ne!(a, b, "sequential calls should produce different names");
    }

    #[test]
    fn derive_name_all_none_produces_timestamp() {
        let name = derive_workweave_name(None, None);
        assert!(
            name.starts_with("workweave-"),
            "expected ww-<timestamp>, got {name}"
        );
    }

    #[test]
    fn derive_name_replaces_slashes() {
        assert_eq!(derive_workweave_name(Some("a/b/c"), None), "a-b-c");
    }

    // -----------------------------------------------------------------------
    // is_hook_rejection — classification by variant, never by text
    // -----------------------------------------------------------------------

    #[test]
    fn a_hook_rejection_is_recognized_through_the_anyhow_conversion() {
        let e = anyhow::Error::from(VcsError::HookRejected {
            repo: PathBuf::from("/store"),
            stderr: "denied by policy".to_owned(),
        });
        assert!(is_hook_rejection(&e));
    }

    #[test]
    fn a_hook_rejection_survives_added_context() {
        let e = anyhow::Error::from(VcsError::HookRejected {
            repo: PathBuf::from("/store"),
            stderr: "denied by policy".to_owned(),
        })
        .context("could not create workweave");
        assert!(
            is_hook_rejection(&e),
            "a caller that adds context must not lose the classification"
        );
    }

    /// Everything a text matcher keys on is present here and no hook was
    /// involved: the word appears in the repo path, in the args, and in
    /// git's stderr.
    #[test]
    fn a_command_failure_that_merely_says_hook_is_not_a_hook_rejection() {
        let e = anyhow::Error::from(VcsError::CommandFailed {
            args: vec!["worktree".to_owned(), "add".to_owned()],
            repo: PathBuf::from("/srv/webhook-service"),
            stderr: "fatal: '/srv/webhook-service/hooks/wt' already exists".to_owned(),
        });
        assert!(
            e.to_string().contains("hook"),
            "fixture must say the word, or it does not test what it claims: {e}"
        );
        assert!(
            !is_hook_rejection(&e),
            "classification must read the variant, not the message"
        );
    }

    // -----------------------------------------------------------------------
    // handle_claude_hook — JSON parsing via serde
    // -----------------------------------------------------------------------

    #[test]
    fn claude_hook_unknown_event_does_not_parse() {
        let json = r#"{"hook_event_name":"UnknownEvent"}"#;
        let err = serde_json::from_str::<ClaudeHookInput>(json)
            .err()
            .expect("an event Claude Code does not define is not an input");
        assert!(err.to_string().contains("UnknownEvent"), "{err}");
    }

    #[test]
    fn claude_hook_null_branch_uses_timestamp_not_session() {
        let name = derive_workweave_name(Some("null"), Some("my-session-id"));
        assert!(
            name.starts_with("workweave-"),
            "session_id ignored, expected ww-*, got {name}"
        );
    }

    #[test]
    fn claude_hook_input_deserializes_fully() {
        let wt_path = std::env::temp_dir().join("wt");
        let wt_str = wt_path.to_str().unwrap();
        let json = serde_json::json!({
            "cwd": "/home/user/ws",
            "branch_name": "feat/new-thing",
            "session_id": "sess-001",
            "hook_event_name": "WorktreeCreate",
            "worktree_path": wt_str,
        })
        .to_string();
        let input: ClaudeHookInput = serde_json::from_str(&json).unwrap();
        assert_eq!(input.cwd.as_deref(), Some("/home/user/ws"));
        assert_eq!(input.branch_name.as_deref(), Some("feat/new-thing"));
        assert_eq!(input.session_id.as_deref(), Some("sess-001"));
        assert_eq!(
            input.hook_event_name,
            Some(crate::setup::HookEvent::WorktreeCreate)
        );
        assert_eq!(input.worktree_path.as_deref(), Some(wt_str));
    }

    #[test]
    fn claude_hook_input_all_optional_fields_missing() {
        let json = r#"{}"#;
        let input: ClaudeHookInput = serde_json::from_str(json).unwrap();
        assert!(input.cwd.is_none());
        assert!(input.branch_name.is_none());
        assert!(input.session_id.is_none());
        assert!(input.hook_event_name.is_none());
        assert!(input.worktree_path.is_none());
    }

    // -----------------------------------------------------------------------
    // A birth and a retire across the Vcs seam's failure arms
    //
    // Each of these turns on something a real repo will not do on request: a
    // ref appearing inside the interval between classification and birth, and
    // a destroy that fails.
    //
    // Every scenario is paired with the setup that reaches a different arm
    // from the same double, so a fake that ignored its ref store would fail
    // one of the pair rather than pass both.
    // -----------------------------------------------------------------------

    use crate::vcs::testing::{FakeVcs, VcsCall};
    use crate::vcs::VcsError;

    /// A distinct canonical revision per `hex` character.
    fn fake_rev(hex: char) -> ResolvedRevisionId {
        ResolvedRevisionId::from_canonical(
            crate::vcs::CommitId::parse(&hex.to_string().repeat(40)).unwrap(),
            None,
        )
    }

    /// A weave whose store is a plain directory, canonicalized: [`FakeVcs`]
    /// keys refs on the path it is handed.
    fn fake_weave() -> (tempfile::TempDir, PathBuf, ProjectName, PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = ProjectName::new("web-app").unwrap();
        std::fs::create_dir_all(primary.join("projects/web-app")).unwrap();
        let store = primary.join("github/org/repo");
        std::fs::create_dir_all(&store).unwrap();
        let store = resolve(&store).unwrap();
        (tmp, primary, project, store)
    }

    #[test]
    fn a_ref_born_inside_the_window_is_adopted() {
        let (_tmp, primary, project, store) = fake_weave();
        let ephemeral = minted(&project);
        let raw = ephemeral.to_raw();

        let fake = FakeVcs::new();
        fake.before_next(VcsCall::MaterializeWorktreeOnRef, {
            let (store, raw) = (store.clone(), raw.clone());
            move |vcs| vcs.put_branch(&store, &raw, fake_rev('c'))
        });

        let outcome = birth_ephemeral_worktree(
            &fake,
            &store,
            &primary.join("wt"),
            &ephemeral,
            fake_rev('a'),
            WorktreeFiles::Written,
            BranchPolicy::Fresh,
        )
        .unwrap();

        assert_eq!(
            fake.calls(),
            vec![
                VcsCall::ListBranchNamesWithPrefix,
                VcsCall::ResolveLocalBranchTip,
                VcsCall::MaterializeWorktreeOnRef,
            ],
            "no destroy and no refusal: classification ran first and found the name free"
        );
        assert!(
            matches!(outcome.birth, RefBirth::Adopted),
            "a ref that appeared inside the window is not this create's to destroy"
        );
        assert!(outcome.failure.is_none());
    }

    #[test]
    fn the_same_ref_standing_before_classification_refuses_instead() {
        let (_tmp, primary, project, store) = fake_weave();
        let ephemeral = minted(&project);

        let fake = FakeVcs::new();
        fake.put_branch(&store, &ephemeral.to_raw(), fake_rev('c'));

        let err = match birth_ephemeral_worktree(
            &fake,
            &store,
            &primary.join("wt"),
            &ephemeral,
            fake_rev('a'),
            WorktreeFiles::Written,
            BranchPolicy::Fresh,
        ) {
            Err(e) => e,
            Ok(_) => panic!("a branch holding commits the create did not make must refuse"),
        };

        let report = crate::diagnosis::refused_output(&err);
        assert_eq!(report.len(), 1, "{err:#}");
        assert_eq!(
            fake.calls(),
            vec![
                VcsCall::ListBranchNamesWithPrefix,
                VcsCall::ResolveLocalBranchTip,
            ],
            "the refusal must land before anything is written"
        );
        assert_eq!(
            fake.branch_tip(&store, &ephemeral.to_raw()),
            Some(fake_rev('c')),
            "the branch is untouched"
        );
    }

    #[test]
    fn a_create_over_its_own_partial_result_takes_it() {
        let (_tmp, primary, project, store) = fake_weave();
        let ephemeral = minted(&project);
        let fake = FakeVcs::new();
        fake.put_branch(&store, &ephemeral.to_raw(), fake_rev('a'));

        let outcome = birth_ephemeral_worktree(
            &fake,
            &store,
            &primary.join("wt"),
            &ephemeral,
            fake_rev('a'),
            WorktreeFiles::Written,
            BranchPolicy::Fresh,
        )
        .unwrap();

        assert!(matches!(outcome.birth, RefBirth::Adopted));
        assert!(outcome.failure.is_none());
        assert!(
            !fake.calls().contains(&VcsCall::DeleteBranch),
            "taking the partial result deletes nothing"
        );
    }

    #[test]
    fn restoring_a_member_attaches_its_branch_where_it_stands() {
        let (_tmp, primary, project, store) = fake_weave();
        let ephemeral = minted(&project);
        let raw = ephemeral.to_raw();
        let fake = FakeVcs::new();
        fake.put_branch(&store, &raw, fake_rev('c'));

        let outcome = birth_ephemeral_worktree(
            &fake,
            &store,
            &primary.join("wt"),
            &ephemeral,
            fake_rev('c'),
            WorktreeFiles::Written,
            BranchPolicy::Restore,
        )
        .unwrap();
        outcome.into_result().unwrap();

        assert!(
            !fake.calls().contains(&VcsCall::DeleteBranch),
            "restoring never deletes the branch"
        );
        assert_eq!(
            fake.resolve_local_branch_tip(&store, &raw).unwrap(),
            Some(fake_rev('c'))
        );
    }

    #[test]
    fn a_birth_that_failed_reports_its_failure_and_leaves_the_branch_to_rollback() {
        let (_tmp, primary, project, store) = fake_weave();
        let ephemeral = minted(&project);

        let fake = FakeVcs::new();
        fake.fail_next(
            VcsCall::MaterializeWorktreeOnRef,
            VcsError::NotARepo(store.clone()),
        );

        let outcome = birth_ephemeral_worktree(
            &fake,
            &store,
            &primary.join("wt"),
            &ephemeral,
            fake_rev('a'),
            WorktreeFiles::Written,
            BranchPolicy::Fresh,
        )
        .unwrap();
        assert!(outcome.failure.is_some());
        assert!(matches!(outcome.birth, RefBirth::AuthoredOrAbsent));

        let err = outcome.into_result().unwrap_err();
        assert!(
            err.to_string().contains("is not a vcs repository"),
            "the birth's own failure must be what surfaces: {err}"
        );
    }

    #[test]
    fn a_lineage_branch_whose_delete_failed_is_left() {
        let (_tmp, _primary, project, store) = fake_weave();
        let ephemeral = minted(&project);
        let raw = ephemeral.to_raw();

        let fake = FakeVcs::new();
        fake.put_branch(&store, &raw, fake_rev('a'));
        fake.declare_ancestor(&fake_rev('a'), &fake_rev('b'));
        fake.fail_next(VcsCall::DeleteBranch, VcsError::NotARepo(store.clone()));

        retire_lineage_branch(
            &fake,
            &store,
            &ephemeral,
            "github/org/repo",
            &[fake_rev('b')],
            Releaser::Remove,
        );

        assert!(
            fake.branch_tip(&store, &raw).is_some(),
            "the destroy failed, so the ref is still there"
        );
    }

    #[test]
    fn a_lineage_branch_the_baselines_reach_is_deleted() {
        let (_tmp, _primary, project, store) = fake_weave();
        let ephemeral = minted(&project);
        let raw = ephemeral.to_raw();

        let fake = FakeVcs::new();
        fake.put_branch(&store, &raw, fake_rev('a'));
        fake.declare_ancestor(&fake_rev('a'), &fake_rev('b'));

        retire_lineage_branch(
            &fake,
            &store,
            &ephemeral,
            "github/org/repo",
            &[fake_rev('b')],
            Releaser::Remove,
        );

        assert!(fake.branch_tip(&store, &raw).is_none());
    }

    #[test]
    fn a_lineage_branch_that_moves_after_its_proof_is_kept() {
        let (_tmp, _primary, project, store) = fake_weave();
        let ephemeral = minted(&project);
        let raw = ephemeral.to_raw();

        let fake = FakeVcs::new();
        fake.put_branch(&store, &raw, fake_rev('a'));
        fake.declare_ancestor(&fake_rev('a'), &fake_rev('b'));
        let (moved_store, moved_raw) = (store.clone(), raw.clone());
        fake.before_next(VcsCall::DeleteBranch, move |fake| {
            fake.put_branch(&moved_store, &moved_raw, fake_rev('c'));
        });

        retire_lineage_branch(
            &fake,
            &store,
            &ephemeral,
            "github/org/repo",
            &[fake_rev('b')],
            Releaser::Remove,
        );

        assert!(fake.calls().contains(&VcsCall::DeleteBranch));
        assert_eq!(fake.branch_tip(&store, &raw), Some(fake_rev('c')));
    }

    #[test]
    fn a_lineage_branch_the_baselines_do_not_reach_is_kept() {
        let (_tmp, _primary, project, store) = fake_weave();
        let ephemeral = minted(&project);
        let raw = ephemeral.to_raw();

        let fake = FakeVcs::new();
        fake.put_branch(&store, &raw, fake_rev('a'));

        retire_lineage_branch(
            &fake,
            &store,
            &ephemeral,
            "github/org/repo",
            &[fake_rev('b')],
            Releaser::Remove,
        );

        assert!(fake.branch_tip(&store, &raw).is_some());
        assert!(!fake.calls().contains(&VcsCall::DeleteBranch));
    }

    // -----------------------------------------------------------------------
    // Merge baselines: one directory must not appear twice
    //
    // `merge_baselines` returns the workspaces whose lineage counts as "this
    // work has landed", and `delete_workweave` names them in its refusal.
    // Whether the recorded parent IS the workspace already being appended is
    // a comparison of two identities, however the caller spelled its root.
    // -----------------------------------------------------------------------

    /// A primary and a workweave whose marker records that primary as its
    /// parent. Returns `(tempdir, primary, workweave_dir)`.
    fn forked_from_primary() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let workweave_dir = tmp.path().join(".workweaves").join("web-app--feat");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&workweave_dir).unwrap();
        WorkweaveMarker::new(
            &primary,
            ProjectName::new("web-app").unwrap(),
            crate::manifest::WorkweaveName::new("ww").unwrap(),
            &primary,
        )
        .write(&workweave_dir)
        .unwrap();
        (tmp, primary, workweave_dir)
    }

    #[test]
    fn merge_baselines_recognizes_an_unresolved_spelling_of_the_parent() {
        let (_tmp, primary, workweave_dir) = forked_from_primary();
        let detoured = primary.join("..").join("ws");
        assert_eq!(
            resolve(&detoured).unwrap(),
            resolve(&primary).unwrap(),
            "precondition: the detoured spelling resolves to the primary"
        );

        let baselines = merge_baselines(&workweave_dir, &CanonicalPath::of(&detoured), None);

        assert_eq!(
            baselines,
            vec![resolve(&primary).unwrap()],
            "the recorded parent IS this workspace, so it must not be listed a second \
             time under another spelling"
        );
    }

    /// The macOS `/var` geometry: the root the caller holds and the parent the
    /// marker records name one directory through a symlink.
    #[test]
    fn merge_baselines_recognizes_a_symlinked_spelling_of_the_parent() {
        let (tmp, primary, workweave_dir) = forked_from_primary();
        let link = tmp.path().join("link");
        crate::symlink::create(tmp.path(), &link, crate::symlink::LinkTarget::Directory).unwrap();
        let symlinked = link.join(primary.file_name().unwrap());

        let baselines = merge_baselines(&workweave_dir, &CanonicalPath::of(&symlinked), None);

        assert_eq!(
            baselines,
            vec![resolve(&primary).unwrap()],
            "the recorded parent IS this workspace, so it must not be listed a second \
             time under another spelling"
        );
    }

    #[test]
    fn merge_baselines_lists_a_retiring_op_s_target_once() {
        let (tmp, primary, workweave_dir) = forked_from_primary();
        let root = CanonicalPath::of(&primary);
        assert_eq!(
            merge_baselines(&workweave_dir, &root, Some(&root)),
            merge_baselines(&workweave_dir, &root, None),
            "a bare retire's target is the recorded parent, so it adds nothing"
        );

        let elsewhere = tmp.path().join("p2");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let target = CanonicalPath::of(&elsewhere);
        assert_eq!(
            merge_baselines(&workweave_dir, &root, Some(&target)),
            vec![target.as_path().to_path_buf(), root.as_path().to_path_buf()],
        );

        // Seen from a primary that is not the recorded parent, the parent is a
        // baseline of its own; a target that is that parent is not a second one.
        assert_eq!(
            merge_baselines(&workweave_dir, &target, Some(&root)),
            merge_baselines(&workweave_dir, &target, None),
            "a target that is the recorded parent is listed once"
        );
    }

    #[test]
    fn merge_baselines_keeps_a_parent_that_is_a_different_workspace() {
        let (tmp, _primary, workweave_dir) = forked_from_primary();
        let elsewhere = tmp.path().join("other");
        std::fs::create_dir_all(&elsewhere).unwrap();

        let baselines = merge_baselines(&workweave_dir, &CanonicalPath::of(&elsewhere), None);

        assert_eq!(
            baselines.len(),
            2,
            "a parent that is not this workspace is a baseline in its own right, got \
             {baselines:?}"
        );
    }
}
