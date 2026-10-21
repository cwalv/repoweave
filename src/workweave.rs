//! Workweave operations: create, delete, list, and sync workweaves.
//!
//! A workweave is a parallel working directory containing worktrees for each
//! repo in a project, including the project repo itself. Placement and
//! discovery are **recorded**, not computed: each `(primary, project)` carries
//! a `.rwv-workweave-index` (see [`crate::workweave_index`]) that names the
//! container directory `workweave create` places new workweaves under and the
//! `name → absolute path` inverted index every `find`-direction verb consults.
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

use crate::cli::consent::{
    DiscardUncommittedConsent, DiscardUnmergedConsent, ReplaceExistingConsent,
};
use crate::manifest::{
    project_repo_key, LockFile, Manifest, ProjectName, RepoPath, Role, Submodules, WorkweaveId,
    WorkweaveName,
};
use crate::path_spelling::resolve;
use crate::refusal::RefusalKind;
use crate::share::{FilePlacement, FilePrimitives, Filesystem};
use crate::symlink::LinkTarget;
use crate::vcs::{
    project_vcs, vcs_for, BornRef, CommitId, DeletionWarrant, EphemeralRefName, OwnedRef,
    RawRefName, ResolvedRevisionId, Vcs, VcsError,
};
use crate::workspace::{
    project_dir, project_rel_dir, project_rel_path, weave_dir_name, workweave_name_in,
    CanonicalPath, WorkweaveMarker,
};
use crate::workweave_index;
use crate::workweave_index::RefRegistry;
use anyhow::{anyhow, bail, Context};
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
    match WorkweaveMarker::read(workspace_dir)? {
        Some(marker) if marker.project() == project => {}
        _ => return Ok(()),
    }
    let manifest = load_manifest(workspace_dir, project)?;
    let linked = symlinked_members(workspace_dir, &manifest);
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
        dir = crate::path_spelling::operator_path(workspace_dir),
        n = linked.len(),
        list = linked
            .iter()
            .map(|r| r.as_str())
            .collect::<Vec<_>>()
            .join("\n  "),
    )
}

/// Determine the workweave container for `(primary_root, project)`.
///
/// The container is the directory `workweave create` places new workweaves
/// under when no per-workweave `--dir` override is passed. Resolution
/// priority:
///
///   1. The `container` field of the recorded `.rwv-workweave-index`.
///   2. `<parent-of-primary>/.workweaves` (compiled-in default).
///
/// Prefer using this in the `create` direction only. The `find` direction
/// (list, delete, sync targets, etc.) resolves via the recorded name → path
/// entries, so it does not consult this at all.
pub fn workweave_container(primary_root: &Path, project: &ProjectName) -> anyhow::Result<PathBuf> {
    workweave_index::resolve_container(primary_root, project)
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
    primary_root: &Path,
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
    if !marker.names_primary(primary_root) {
        return RegistryEntryValidation::ForeignPrimary;
    }
    if marker.project() != project {
        return RegistryEntryValidation::ProjectMismatch {
            actual: marker.project().clone(),
        };
    }
    RegistryEntryValidation::Valid
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
    match validate_registry_entry(primary_root, project, &raw) {
        RegistryEntryValidation::Valid => Ok(Some(raw)),
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
        .find(|(_, recorded)| workweave_index::same_directory(recorded, path));
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
pub fn registered_workweaves(primary_root: &Path) -> Vec<(ProjectName, WorkweaveName)> {
    let mut out = Vec::new();
    for project in crate::workspace::discover_projects(primary_root) {
        let Ok(Some(index)) = workweave_index::read(primary_root, &project) else {
            continue;
        };
        for name in index.workweaves.into_keys() {
            out.push((project.clone(), name));
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
    match validate_registry_entry(primary_root, project, &raw) {
        RegistryEntryValidation::Valid => Ok(raw),
        RegistryEntryValidation::MissingDirectory => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "workweave `{}` is recorded at {} but that directory no longer \
             exists; run `rwv doctor --fix` to prune the stale entry",
            name.as_str(),
            raw.display()
        ),
        RegistryEntryValidation::MissingMarker => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "workweave `{}` is recorded at {} but the directory has no \
             `.rwv-workweave` marker — refusing to touch it; run `rwv doctor` \
             for guidance",
            name.as_str(),
            raw.display()
        ),
        RegistryEntryValidation::ForeignPrimary => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "workweave `{}` is recorded at {} but the marker's `primary` \
             does not match this workspace — refusing to touch a foreign \
             workweave. This can arise from a committed / hand-edited \
             registry; run `rwv doctor` to investigate.",
            name.as_str(),
            raw.display()
        ),
        RegistryEntryValidation::ProjectMismatch { actual } => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "workweave `{}` is recorded at {} but the marker records project \
             `{}`, not `{}` — refusing to touch it; run `rwv doctor` to \
             investigate the registry / marker disagreement",
            name.as_str(),
            raw.display(),
            actual.as_str(),
            project.as_str()
        ),
        RegistryEntryValidation::MarkerUnreadable { detail } => crate::refuse!(
            RefusalKind::StaleRegistryEntry,
            "workweave `{}` is recorded at {} but its marker could not be \
             read ({detail}); refusing to touch it. Run `rwv doctor` for \
             remediation.",
            name.as_str(),
            raw.display()
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
/// This is a predicate over an observation and grants nothing. Under R2
/// matching here is not ownership — every DESTROY still goes through
/// [`RefRegistry::lookup`]; this only decides which unowned names are worth
/// telling the operator about.
///
/// The minted side is brought to the parse boundary with
/// [`EphemeralRefName::to_raw`], not with `to_string()`. Both spell the same
/// characters, so this is not a bug fix — it is keeping the one legal
/// conversion distinct from the rendering. `Display` is what an
/// `EphemeralRefName` offers a human; laundering that rendering into a string
/// in order to compare it with an observed name is precisely what removing
/// `as_str()` prevents, and a comparison written that way keeps compiling if
/// the two notions ever diverge.
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
/// - The `create --replace-existing` path — call this before recreating to
///   clear any orphan registrations left by a previous partial create.
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

/// What one create attempt did to the ref its receipt names.
///
/// Rollback must not destroy a branch this create merely **adopted**:
/// force-deleting one takes a unique commit with it. The distinction is
/// [`Vcs::create_worktree_on`]'s return value, and this enum is where the
/// call site parks it.
enum RefBirth {
    /// `create_worktree_on` returned a [`BornRef`]: this call wrote the ref.
    Authored(BornRef),
    /// The birth call returned `Err` after the pre-flight had established
    /// that no branch of this name existed. `git worktree add -b` writes the
    /// branch *before* it runs post-checkout hooks, so a hook-rejected add
    /// leaves the ref behind; anything now at that name was written by this
    /// attempt. No `BornRef` exists to prove it — the proof is the pre-flight
    /// plus the `Unmoved` warrant rollback still has to obtain.
    AuthoredOrAbsent,
    /// `create_worktree_on` returned `None`: it adopted a branch that was
    /// already there. **Never** this create's to destroy — only the
    /// receipt this attempt wrote is retracted.
    Adopted,
}

/// One repo's ref state for rollback: the receipt rwv persisted, and what
/// the birth call did with it.
struct RefAttempt {
    owned: OwnedRef,
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
    /// The primary weave and project the ownership receipts are keyed to.
    /// Rollback needs a [`RefRegistry`] handle: a DESTROY takes a receipt
    /// (R2) and a warrant (R3), neither of which a `(repo, name)` pair is.
    primary_root: PathBuf,
    project: ProjectName,
    /// Pairs of `(repo_abs, worktree_dest)` for every worktree that was
    /// successfully registered during this create attempt.
    registered_worktrees: Vec<(PathBuf, PathBuf)>,
    /// Every ref this attempt wrote a receipt for, with what the birth call
    /// then did. Resolved on rollback AFTER worktree removal (so the branch
    /// is no longer checked out when the DESTROY runs).
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
    fn new(
        vcs: Box<dyn Vcs>,
        workweave_dir: PathBuf,
        primary_root: &Path,
        project: &ProjectName,
    ) -> Self {
        Self {
            vcs,
            workweave_dir,
            primary_root: primary_root.to_path_buf(),
            project: project.clone(),
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

    /// Record what the birth call did with the receipt this attempt wrote.
    ///
    /// Call this AFTER `create_worktree_on` returns, with whatever it
    /// returned — including on the `Err` path, where [`RefBirth::AuthoredOrAbsent`]
    /// is the honest verdict. The old shape recorded the *intended* name
    /// before the call and unconditionally force-deleted it on the way out;
    /// under R2 there is nothing to record before the call, because the
    /// receipt (written by the registry, before the ref) is what a rollback
    /// destroy has to be holding.
    fn record_ref_attempt(&mut self, owned: OwnedRef, birth: RefBirth) {
        self.ref_attempts.push(RefAttempt { owned, birth });
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

    /// Destroy the refs **this create authored** and retract their receipts.
    ///
    /// Every destroy here is a full R2+R3 DESTROY: the receipt comes from the
    /// registry and the warrant is [`DeletionWarrant::unmoved`], which re-reads
    /// the ref and returns `Some` only while its tip is still the one the
    /// receipt records. So the three ways this attempt's ref can look at
    /// rollback time are separated rather than collapsed:
    ///
    /// - present at the recorded tip → destroy it, then retract the receipt
    ///   (that order: a crash between the two leaves a dangling receipt, which
    ///   authorizes nothing, rather than an unreceipted ref, which R2 disowns
    ///   forever);
    /// - absent (the birth never reached the ref write) → nothing to destroy;
    ///   retract the dangling receipt;
    /// - present at a *different* tip → something has committed on it since.
    ///   Leave the ref **and** the receipt alone and tell the operator. This
    ///   is the case the old unconditional `branch -D` destroyed silently.
    ///
    /// An [`RefBirth::Adopted`] ref is never destroyed at all, whatever its
    /// tip: this create did not write it.
    fn undo_ref_births(&self) -> Vec<String> {
        let vcs = self.vcs.as_ref();
        let mut registry = RefRegistry::for_project(&self.primary_root, &self.project);
        let mut failures: Vec<String> = Vec::new();

        for attempt in &self.ref_attempts {
            let owned = &attempt.owned;
            let store = owned.store().to_path_buf();

            if matches!(attempt.birth, RefBirth::Adopted) {
                // Not ours: retract only the claim this attempt made.
                if let Err(e) = registry.retract(&store, owned.name()) {
                    eprintln!(
                        "rwv workweave rollback: warning: could not retract the ownership \
                         receipt for adopted branch {owned} in {}: {e:#}",
                        store.display()
                    );
                }
                continue;
            }

            match DeletionWarrant::unmoved(vcs, owned) {
                Some(warrant) => {
                    if let Err(e) = vcs.delete_owned_ref(owned, warrant) {
                        eprintln!(
                            "rwv workweave rollback: warning: could not delete ephemeral \
                             branch {owned} in {}: {e:#}",
                            store.display()
                        );
                        failures.push(format!(
                            "branch {owned} in {repo} — delete manually with: \
                             git -C {repo} branch -D {owned}",
                            repo = store.display(),
                        ));
                        continue;
                    }
                    if let Err(e) = registry.retract(&store, owned.name()) {
                        eprintln!(
                            "rwv workweave rollback: warning: branch {owned} deleted but its \
                             ownership receipt could not be retracted ({e:#}); run \
                             `rwv doctor --fix`"
                        );
                    }
                }
                None => match vcs.resolve_local_branch_tip(&store, owned.name()) {
                    Ok(None) => {
                        // The birth never reached the ref write. Dangling
                        // receipt — benign, and this is the pass that clears it.
                        if let Err(e) = registry.retract(&store, owned.name()) {
                            eprintln!(
                                "rwv workweave rollback: warning: could not retract the \
                                 dangling ownership receipt for {owned} in {}: {e:#}",
                                store.display()
                            );
                        }
                    }
                    _ => {
                        // The revision this attempt put the ref at. With a
                        // `BornRef` that is the birth's own record; without
                        // one — the failed-birth case — the receipt is the
                        // only witness, and it records the start point the
                        // birth was asked for.
                        let born_at = match &attempt.birth {
                            RefBirth::Authored(born) => born.at(),
                            _ => owned.created_at(),
                        };
                        failures.push(format!(
                            "branch {owned} in {repo} moved off the revision rwv created it \
                             at ({at}); left in place with its receipt — inspect it with \
                             `git -C {repo} log {owned}` and delete it by hand if it holds \
                             nothing",
                            repo = store.display(),
                            at = born_at.display_str(),
                        ))
                    }
                },
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

        // 4. Undo the ref births of this attempt — same receipt-and-warrant
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
///
/// Siblings `.2` (rollback) and `.3` (replace-existing prune) may call this same
/// function before their own mutations.
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
            "project {name} has no commits yet — run \
             \"git -C projects/{name} commit\" to land the activate-generated \
             artifacts before creating a workweave.",
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
/// asks for `submodules`, reporting each under `prefix`; the count left
/// uninitialized.
pub(crate) fn populate_member_submodules(
    vcs: &dyn Vcs,
    member: &Path,
    repo_path: &RepoPath,
    role: Role,
    submodules: Submodules,
    prefix: &str,
) -> usize {
    if submodules == Submodules::Skip {
        return 0;
    }
    let populated = crate::share::populate_submodules(vcs, member, role, &Filesystem);
    populated.report(prefix, repo_path);
    populated.left.len()
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
/// goes wrong *before* the receipt is written is a plain `Err` from
/// [`birth_ephemeral_worktree`] — nothing was claimed, so there is nothing to
/// roll back. Once the receipt exists the attempt is always reportable, and a
/// failed birth rides back inside `failure` rather than losing the receipt
/// with it.
pub(crate) struct BirthOutcome {
    owned: OwnedRef,
    birth: RefBirth,
    pub(crate) failure: Option<anyhow::Error>,
    /// How the born worktree's files were placed, when they were shared and
    /// the backend could say.
    pub(crate) placement: Option<FilePlacement>,
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
    /// Resolve the outcome here, for a caller with no rollback guard to hand
    /// it to: a receipt survives only if this call authored the ref it names.
    ///
    /// An adopted ref is not this call's to hold a claim on, and with
    /// no guard there is no later pass to notice — so the receipt is retracted
    /// before the error returns, rather than being left standing over a branch
    /// rwv did not create. A birth that *failed* keeps its receipt: the ref may
    /// exist regardless, and under R2 only a receipt can authorize cleaning it
    /// up later.
    pub(crate) fn into_authored(self, registry: &mut RefRegistry) -> anyhow::Result<()> {
        match self.birth {
            RefBirth::Adopted => {
                let store = self.owned.store().to_path_buf();
                registry.retract(&store, self.owned.name())?;
                crate::refuse!(
                    RefusalKind::UnownedBranchInNamespace,
                    "branch `{owned}` already existed in {store}, so materializing it \
                     adopted a ref rwv did not create. The ownership receipt has been \
                     retracted — rwv does not claim a branch it did not author.\n\n\
                     Rename or delete that branch yourself, then re-run.",
                    owned = self.owned,
                    store = store.display(),
                )
            }
            RefBirth::Authored(_) | RefBirth::AuthoredOrAbsent => match self.failure {
                Some(e) => Err(e),
                None => Ok(()),
            },
        }
    }
}

/// The canonical store a checkout's refs actually live in — the key every
/// ownership receipt for that repo is recorded and looked up under.
///
/// `create` sees the *fork source* (`source_root/<repo>`, which is a linked
/// worktree when forking from another workweave) and `delete` sees the
/// workweave's own checkout; both must resolve to the same key or the receipt
/// a create wrote is invisible to the delete that has to consume it. Resolving
/// through git's common-dir is what makes that true — the path each verb
/// happens to hold is not.
pub(crate) fn receipt_store_for(vcs: &dyn Vcs, checkout: &Path) -> PathBuf {
    resolved_worktree_parent(vcs, checkout, checkout)
}

/// A member worktree [`materialize_member`] gave a workweave.
pub(crate) struct Materialized {
    /// The revision it was born at.
    pub(crate) at: ResolvedRevisionId,
    /// How its files were placed, when the backend could say.
    pub(crate) files: Option<FilePlacement>,
}

/// Give the workweave at `workweave_dir` the workspace it lacks for
/// `repo_path`: an ephemeral worktree of the canonical clone under
/// `primary_root`, born at `start` with its files shared where they can be,
/// settled for `role`, and its submodules populated as `submodules` asks. A
/// symlink in the slot, dangling or not, is unlinked first; what it pointed
/// at is never touched.
#[allow(clippy::too_many_arguments)]
pub(crate) fn materialize_member(
    vcs: &dyn Vcs,
    primary_root: &Path,
    workweave_dir: &Path,
    project: &ProjectName,
    workweave: &WorkweaveName,
    repo_path: &RepoPath,
    role: Role,
    submodules: Submodules,
    start: &str,
) -> anyhow::Result<Materialized> {
    let canonical = primary_root.join(repo_path.as_path());
    if !canonical.exists() {
        crate::refuse!(
            RefusalKind::MissingCanonicalClone,
            "canonical clone for {repo_path} missing at {}; \
             run `rwv fetch --project {project}` at the primary weave first to materialize it \
             there",
            canonical.display()
        );
    }
    let start = vcs
        .resolve_revision(&canonical, start)
        .with_context(|| format!("failed to resolve {start} in canonical clone"))?;
    let dest = workweave_dir.join(repo_path.as_path());
    if is_symlinked_member(&dest) {
        crate::symlink::remove(&dest)
            .with_context(|| format!("failed to unlink the symlink at {}", dest.display()))?;
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut registry = RefRegistry::for_project(primary_root, project);
    let outcome = birth_ephemeral_worktree(
        vcs,
        &mut registry,
        &canonical,
        &dest,
        &EphemeralRefName::mint(project, workweave),
        start.clone(),
        WorktreeFiles::Shared(&Filesystem, role),
    )
    .with_context(|| format!("cannot materialize {repo_path}"))?;
    let files = outcome.placement;
    outcome
        .into_authored(&mut registry)
        .with_context(|| format!("worktree add for {repo_path} failed"))?;
    crate::settle::settle(vcs, &dest, role)?;
    populate_member_submodules(vcs, &dest, repo_path, role, submodules, "rwv: ");
    Ok(Materialized { at: start, files })
}

/// Whether `store`, the store the VCS resolved for `checkout`, is the one
/// `checkout` holds itself — a full clone, not a workspace linked into a store
/// elsewhere.
pub(crate) fn is_own_store(store: &Path, checkout: &Path) -> bool {
    let own = crate::git::store_path_in(checkout);
    let resolved = |p: &Path| resolve(p).unwrap_or_else(|_| p.to_path_buf());
    resolved(store) == resolved(&own)
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
    submodules: Submodules,
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
            crate::path_spelling::operator_path(&slot),
            crate::path_spelling::operator_path(&aside)
        )
    })?;

    // A worktree once registered at the slot is stale now that the slot is
    // empty, and git refuses to add one where a registration stands.
    let born = vcs
        .worktree_prune(&canonical)
        .map_err(anyhow::Error::from)
        .and_then(|()| {
            materialize_member(
                vcs,
                primary_root,
                workweave_dir,
                project,
                workweave,
                repo_path,
                role,
                submodules,
                at.id().as_str(),
            )
        });
    match born {
        Ok(born) => {
            remove_tree_outlasting_child_handles(&aside).with_context(|| {
                format!(
                    "created the worktree for {repo_path}, but the full clone it replaced is \
                     still at {}",
                    crate::path_spelling::operator_path(&aside)
                )
            })?;
            Ok(born)
        }
        Err(e) if slot.symlink_metadata().is_err() => match std::fs::rename(&aside, &slot) {
            Ok(()) => Err(e),
            Err(restore) => Err(e.context(format!(
                "the full clone of {repo_path} is left at {} (moving it back failed: {restore})",
                crate::path_spelling::operator_path(&aside)
            ))),
        },
        Err(e) => Err(e.context(format!(
            "the full clone of {repo_path} is left at {}",
            crate::path_spelling::operator_path(&aside)
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
            crate::path_spelling::operator_path(aside)
        ));
    }

    let canonical_is_clone = match vcs.resolve_canonical_store(canonical) {
        None => {
            reasons.push(format!(
                "there is no canonical clone at {} to link a worktree into; \
                 `rwv fetch --project {project}` in the primary weave materializes it.",
                crate::path_spelling::operator_path(canonical)
            ));
            false
        }
        Some(store) if !is_own_store(&store, canonical) => {
            reasons.push(format!(
                "the canonical slot {} is itself a worktree of {}, not a full clone to link \
                 a worktree into; `rwv doctor` reports it as clone-topology.",
                crate::path_spelling::operator_path(canonical),
                crate::path_spelling::operator_path(&store)
            ));
            false
        }
        Some(_) => true,
    };

    let own = resolve(standalone).unwrap_or_else(|_| standalone.to_path_buf());
    let dependents: Vec<String> = vcs
        .list_worktrees(standalone)?
        .into_iter()
        .filter(|wt| resolve(wt).unwrap_or_else(|_| wt.clone()) != own)
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
                crate::path_spelling::operator_path(canonical),
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
            crate::path_spelling::operator_path(standalone),
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
/// store behind `source_repo`, writing the ownership receipt first.
///
/// The name is minted flat ([`EphemeralRefName::mint`]) and nothing observed
/// feeds into it, so there is no third component for two call sites to
/// derive differently.
///
/// The ref begins at `start`, which is also what the receipt records:
/// [`Vcs::create_worktree_on`] takes the start point from the receipt, so the
/// two cannot disagree. It is the caller's because callers differ on it — a
/// create forks the source checkout's HEAD, while a member a workweave lacks
/// starts at the revision the verb giving it one aims it at.
///
/// # The four states of `(receipt, ref)`, and why none of them force-deletes
///
/// The shipped retry ran `git branch -D` on "already exists" and tried again,
/// which destroyed a branch on nothing but a name match. Classifying first
/// separates the cases the name collapsed:
///
/// - **no receipt, no ref** — the ordinary create.
/// - **no receipt, ref present** — the name is taken by a ref rwv does not
///   own. Refuse, naming the ref's tip and the start point this create wanted.
///   R2: a ref that merely *looks* like rwv's is not rwv's, so it is neither
///   adopted into a receipt nor destroyed.
/// - **receipt, ref present at the recorded tip** — a stale leftover of a
///   failed create, and the only case the shipped justification ever claimed.
///   The [`DeletionWarrant::unmoved`] that proves it *runs* the comparison, so
///   the claim is checked rather than asserted: destroy, retract, recreate.
/// - **receipt, ref present at a different tip** — something committed on it.
///   Refuse and name both tips.
/// - **receipt, no ref** — a dangling receipt (crash between the receipt and
///   the ref). Retract it and record afresh, so `created_at` describes the ref
///   this call is about to write rather than a previous attempt's start point.
pub(crate) fn birth_ephemeral_worktree(
    vcs: &dyn Vcs,
    registry: &mut RefRegistry,
    source_repo: &Path,
    dest: &Path,
    ephemeral: &EphemeralRefName,
    start: ResolvedRevisionId,
    files: WorktreeFiles<'_>,
) -> anyhow::Result<BirthOutcome> {
    let store = receipt_store_for(vcs, source_repo);
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
            RefusalKind::UnmigratedEphemeralBranch,
            "cannot create branch `{ephemeral}` in {store}: {n} branch(es) already occupy \
             its namespace, and git cannot hold both `refs/heads/{ephemeral}` and \
             `refs/heads/{ephemeral}/...`:\n  {list}\n\n\
             These carry the pre-flat shape `{{project}}--{{workweave}}/<segment>`, which \
             rwv no longer mints. Run `rwv doctor --fix` to migrate the ones rwv created; \
             one you made yourself is yours to rename or delete — rwv will not touch it.",
            store = store.display(),
            n = occupied.len(),
            list = occupied
                .iter()
                .map(|b| b.as_str().to_owned())
                .collect::<Vec<_>>()
                .join("\n  "),
        );
    }

    let existing_tip = vcs.resolve_local_branch_tip(&store, &raw)?;
    match (registry.lookup(&store, &raw)?, &existing_tip) {
        (None, Some(tip)) => crate::refuse!(
            RefusalKind::UnownedBranchInNamespace,
            "branch `{ephemeral}` already exists in {store} at {tip} and rwv holds no \
             ownership receipt for it, so it is not rwv's to reuse or delete. This \
             create would have started it at {start}.\n\n\
             Rename or delete that branch yourself, or create the workweave under a \
             different name.",
            store = store.display(),
            tip = tip.display_str(),
            start = start.display_str(),
        ),
        (Some(recorded), Some(tip)) => match DeletionWarrant::unmoved(vcs, &recorded) {
            Some(warrant) => {
                // Stale leftover of a previous failed create: the ref is still
                // exactly where rwv left it, so nothing has been added to it.
                vcs.delete_owned_ref(&recorded, warrant)?;
                registry.retract(&store, &raw)?;
            }
            None => crate::refuse!(
                RefusalKind::OwnedBranchMoved,
                "branch `{ephemeral}` in {store} is recorded as rwv's but has moved since \
                 rwv created it: recorded at {recorded_tip}, now at {tip}. Refusing to \
                 destroy it to make room for a new workweave.\n\n\
                 Merge or delete those commits, then re-run; the create would have \
                 started the branch at {start}.",
                store = store.display(),
                recorded_tip = recorded.created_at().display_str(),
                tip = tip.display_str(),
                start = start.display_str(),
            ),
        },
        (Some(_), None) => {
            // Dangling receipt: retract so the fresh record's `created_at`
            // describes the ref this call writes, not a previous attempt's.
            registry.retract(&store, &raw)?;
        }
        (None, None) => {}
    }

    // Receipt first, durably, then the ref. A crash between the two
    // leaves a dangling receipt (benign, retracted above on the next pass),
    // never an unreceipted ref (permanently disowned under R2).
    let owned = registry.record_created(&store, ephemeral.clone(), start)?;

    let populated = match files {
        WorktreeFiles::Written => crate::share::Populated {
            created: vcs.create_worktree_on(&owned, dest),
            filled: Ok(None),
        },
        WorktreeFiles::Shared(fs, role) => {
            crate::share::create_worktree_populated(vcs, &owned, dest, role, fs)
        }
    };
    let (placement, filling_failed) = match populated.filled {
        Ok(placement) => (placement, None),
        Err(e) => (None, Some(e)),
    };
    let (birth, failure) = match populated.created {
        Ok(Some(born)) => (RefBirth::Authored(born), filling_failed),
        Ok(None) => (RefBirth::Adopted, filling_failed),
        Err(e) => (RefBirth::AuthoredOrAbsent, Some(e.into())),
    };
    Ok(BirthOutcome {
        owned,
        birth,
        failure,
        placement,
    })
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
        let dest_file = dest_project_dir.join(&name);
        refuse_symlinked_component(dest_project_dir, Path::new(&name))?;
        if dest_file.exists() {
            continue;
        }
        let source_file = source_project_dir.join(&name);
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
                anyhow::bail!(
                    "refusing to carry {}: {} is a symlink",
                    rel.display(),
                    walked.display()
                );
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// What [`create_workweave`] does with uncommitted changes in the source
/// project directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirtySource {
    /// Refuse, naming the dirty files.
    Refuse,
    /// Move uncommitted edits to `rwv.toml` and `rwv.lock` into the new
    /// workweave's project worktree, discarding from the source only what the
    /// copy holds, once the workweave exists.
    Capture,
}

/// Create a workweave: for each repo in the manifest, create a worktree in the
/// workweave directory on an ephemeral branch `{project}--{workweave_name}`.
/// Also creates a worktree for the project repo, processes `workweave:` artifacts,
/// writes the marker file, and runs activate. No `.rwv-active` is written: the
/// marker names the project, and the two files are mutually exclusive.
///
/// `primary_root` locates the surrounding weave: it determines where new
/// workweaves live (`<primary_parent>/.workweaves/`) and is recorded in the
/// `.rwv-workweave` marker so the workweave knows its primary.
///
/// `source_root` is the workspace forked from: the manifest, per-repo HEADs,
/// `projects/<project>/` worktree, and `workweave:` copy/link sources are
/// all read from `source_root`. When forking from primary, pass the same path
/// for both. When forking from another workweave (an agent creating a peer
/// workweave from inside its own), `source_root` is that workweave's
/// directory while `primary_root` remains the primary weave.
///
/// If the workweave directory already exists, behavior depends on
/// `replace_existing`:
/// - `None`: validate that the existing workweave matches this
///   `(primary, project)` pair and has no local modifications relative to
///   `source_root`, then short-circuit. This preserves non-git state (e.g.
///   `.runtime/`, `.claude/`) written by agents between invocations, which
///   agent runtimes rely on across a restart.
///   Returns an error if the marker is missing or for a different project,
///   or if any worktree has uncommitted changes or has diverged from the
///   source.
/// - `Some`: destroy the existing workweave and recreate from scratch,
///   refusing while it holds uncommitted or unmerged work.
///
/// `activation` is what the workweave's activation runs with; its
/// `no_materialize` skips the install hooks.
///
/// `dir_override` is the optional per-invocation placement override. When
/// `Some(p)`, the workweave lands at `p`, which the caller has already made
/// absolute; when `None`, it lands at
/// `<container>/<project>--<name>` where `container` comes from
/// [`workweave_container`]. Either way the resulting absolute path is
/// recorded in the registry so per-workweave overrides are as visible to
/// later `list` / `delete` / doctor as default-container placements.
///
/// Returns the absolute path of the created workweave directory.
#[allow(clippy::too_many_arguments)]
pub fn create_workweave(
    primary_root: &Path,
    source_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    replace_existing: Option<ReplaceExistingConsent>,
    dirty_source: DirtySource,
    dir_override: Option<&Path>,
    activation: crate::activate::ActivateOptions,
) -> anyhow::Result<PathBuf> {
    let project_vcs = project_vcs();
    let manifest = load_manifest(source_root, project)?;
    refuse_symlinked_members(source_root, project)?;
    // Placement is authoritative here (create direction): either the caller
    // named an explicit path (recorded verbatim) or the recorded container
    // provides the default. The registry entry is written after the marker
    // lands — see the bottom of this function.
    let workweave_dir = match dir_override {
        Some(p) => p.to_path_buf(),
        None => {
            let container = workweave_container(primary_root, project)?;
            container.join(weave_dir_name(project, name))
        }
    };

    // Name uniqueness is checked against the workweave INDEX, not just the
    // container directory. The directory check below sees only the slot this
    // invocation picked, so `--dir` walks straight past it, and the index
    // insert is a silent last-writer-wins. Two workweaves of one project
    // sharing a name mint the *same* flat branch in the same store, where the
    // second create's collision handling would be pointed at the first's live
    // ref. The two
    // guards are complementary: this one refuses the duplicate name outright,
    // and `birth_ephemeral_worktree` refuses to destroy a ref it cannot prove
    // is a stale leftover.
    if let Some(recorded) = workweave_index::lookup_raw(primary_root, project, name)? {
        if workweave_index::canonical_recorded_path(&recorded)
            != workweave_index::canonical_recorded_path(&workweave_dir)
        {
            crate::refuse!(
                RefusalKind::WorkweaveNameTaken,
                "project `{project}` already records a workweave named `{name}` at {recorded}; \
                 refusing to create a second one at {requested}.\n\n\
                 Both would mint the ephemeral branch `{branch}` in the same store. Pick \
                 another name, delete the recorded workweave first, or — if that path is \
                 gone — run `rwv doctor --fix` to prune the stale entry.",
                project = project.as_str(),
                name = name.as_str(),
                recorded = recorded.display(),
                requested = workweave_dir.display(),
                branch = EphemeralRefName::mint(project, name),
            );
        }
    }

    if workweave_dir.exists() {
        // Whatever is there may not be this workweave. On a filesystem that
        // folds case the lookup above answers for a directory spelled another
        // way, and both arms below would then act on someone else's seat —
        // reuse would adopt it, replace would destroy it.
        if crate::workspace::diverged_occupant(&workweave_dir).is_some() {
            crate::refuse!(
                RefusalKind::ForeignWorkweaveInTargetDir,
                "cannot create workweave `{name}` for project `{project}`: {}. That is \
                 a different workweave, not this one. Address it by its own name, or \
                 choose a name whose directory does not collide.",
                crate::workspace::describe_existing(&workweave_dir),
                name = name.as_str(),
                project = project.as_str(),
            );
        }
        refuse_symlinked_members(&workweave_dir, project)?;
        if replace_existing.is_some() {
            // Destructive reuse. Prefer delete_workweave (which also
            // prunes worktrees and ephemeral branches) when the marker
            // belongs to this project; fall back to a raw remove
            // otherwise since delete_workweave loads a manifest tied to
            // `project` and would fail on wrong-marker / missing-marker
            // cases.
            let can_use_structured_delete = match WorkweaveMarker::read(&workweave_dir)? {
                Some(m) => m.project() == project,
                None => false,
            };
            // Even under --replace-existing, refuse to replace a workweave
            // holding uncommitted work. create's --replace-existing consents
            // to replacing the directory, but the operator never saw what was
            // inside it — unlike `workweave delete`, which lists the dirty
            // paths before a discard flag is retried. Explicit destruction of
            // dirty workweaves stays with `workweave delete
            // --discard-uncommitted`.
            let at_risk = if can_use_structured_delete {
                // Uncommitted changes plus committed-but-unmerged work —
                // both are destroyed by the replace.
                let mut paths =
                    collect_dirty_paths(project_vcs.as_ref(), &workweave_dir, project, &manifest);
                let baselines = merge_baselines(&workweave_dir, primary_root);
                paths.extend(collect_diverged_paths(
                    project_vcs.as_ref(),
                    &workweave_dir,
                    project,
                    &manifest,
                    &baselines,
                ));
                paths
            } else {
                // Marker missing/foreign: no manifest can be trusted to
                // enumerate the contents, so scan for repos directly.
                collect_dirty_repos_by_walk(project_vcs.as_ref(), &workweave_dir)
            };
            if !at_risk.is_empty() {
                crate::refuse!(
                    RefusalKind::ReplaceTargetHoldsWork,
                    "workweave {} already exists and holds unsaved or unmerged work; \
                     refusing to replace it:\n  {}\n\
                     Commit/merge that work, or delete it explicitly with \
                     `rwv workweave {} delete {} --discard-uncommitted \
                     --discard-unmerged-commits`.",
                    name.as_str(),
                    at_risk.join("\n  "),
                    project.as_str(),
                    name.as_str(),
                );
            }
            if can_use_structured_delete {
                // No waiver: the scan above found nothing to lose, so the
                // delete's own preconditions pass on their merits.
                delete_workweave(
                    primary_root,
                    project,
                    name,
                    Some(&workweave_dir),
                    None,
                    None,
                )?;
            } else {
                // No valid marker for this project, so delete_workweave
                // cannot be used (it would load the wrong manifest).
                // Still prune orphan worktree registrations — a previous
                // partial create may have left `.git/worktrees/<name>`
                // entries in the primary repos even though the workweave
                // directory survived (or had its marker stripped).
                // Build (repo_abs, worktree_dest) pairs from the
                // manifest and prune before the raw remove.
                let orphan_pairs = orphan_prune_pairs(&manifest, source_root, &workweave_dir);
                prune_orphan_worktrees_for(project_vcs.as_ref(), &orphan_pairs);
                std::fs::remove_dir_all(&workweave_dir)?;
            }
        } else {
            return reuse_existing_workweave(
                primary_root,
                source_root,
                project,
                name,
                &workweave_dir,
                &manifest,
            );
        }
    }

    // Dirty state captured into a workweave becomes an obstacle at retire
    // time ("Your local changes to the following files would be overwritten
    // by merge"), so capturing it is opt-in.
    //
    // Only the project directory is checked here — manifest-repo worktrees are
    // forked at HEAD (committed state) by `git worktree add`, so dirty state
    // in those repos is not captured. The project dir is special because we
    // explicitly overlay its working-tree `rwv.toml`/`rwv.lock` below.
    if dirty_source == DirtySource::Refuse {
        let project_dir = project_dir(source_root, project);
        if project_vcs.is_repo(&project_dir) {
            match project_vcs.dirty_file_names(&project_dir) {
                Ok(dirty) if !dirty.is_empty() => {
                    crate::refuse!(
                        RefusalKind::DirtyProjectDir,
                        "rwv workweave create: refusing to create workweave — \
                         projects/{project} has uncommitted changes:\n  {files}\n\n\
                         To proceed, do one of:\n  \
                         1. commit the changes: git -C projects/{project} commit\n  \
                         2. stash the changes: git -C projects/{project} stash\n  \
                         3. capture them into the workweave: rwv workweave {project} create {name} --capture-dirty",
                        project = project.as_str(),
                        name = name.as_str(),
                        files = dirty.join("\n  "),
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    // Cannot determine dirty status — be conservative and refuse.
                    crate::refuse!(
                        RefusalKind::UnreadableStatus,
                        "rwv workweave create: refusing to create workweave — \
                         could not check projects/{project} for uncommitted changes: {e:#}\n\n\
                         To bypass this check, use --capture-dirty.",
                        project = project.as_str(),
                    );
                }
            }
        }
    }

    // Pre-flight: verify every repo we are about to worktree has at least one
    // commit. `git worktree add` requires a resolvable HEAD; without this check
    // the user gets a raw "fatal: ambiguous argument 'HEAD'" from git, which
    // names no path and suggests no fix. We run this BEFORE any disk mutation
    // so a missing-HEAD failure leaves no partial workweave directory behind.
    preflight_check_heads(project_vcs.as_ref(), source_root, project, &manifest)?;

    // Pre-add prune: clear any orphaned `.git/worktrees/<name>` registrations
    // left over from a previous (failed or manually-deleted) create attempt.
    //
    // The failure mode this targets: the workweave directory is fully gone (rm
    // -rf'd, or a partial create was interrupted before the directory was
    // written) yet the `.git/worktrees/<name>` administrative entry survives in
    // one or more canonical repos. Without this prune, the subsequent
    // `git worktree add` calls fail with:
    //
    //   fatal: '<path>' is a missing but already registered worktree;
    //          use 'add -f' to override, or 'prune'/'remove' to clear
    //
    // This is idempotent: `prune_orphan_worktrees_for` only calls
    // `git worktree remove --force` when the worktree directory EXISTS on disk
    // (so a live workweave's files are never touched), and then always calls
    // `git worktree prune`, which only removes administrative entries whose
    // worktree directory is absent. A clean repo with no stale registration
    // is a no-op.
    //
    // This does NOT duplicate the dir-exists path above (which either runs
    // delete_workweave or prune_orphan_worktrees_for on the surviving dir): at
    // this point we know workweave_dir does NOT exist (the block above handled
    // the exists() case and either returned or deleted the directory).
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

    // B7: Rollback guard — automatically undoes partial state on any failure
    // path (including `bail!` / `?` propagation). Tracks which repos got
    // worktrees added so orphan `.git/worktrees/` registrations can be pruned
    // in addition to removing the workweave directory.
    let mut rollback = CreateRollbackGuard::new(
        crate::vcs::project_vcs(),
        workweave_dir.clone(),
        primary_root,
        project,
    );

    // The one ephemeral name this create uses, in every store it touches.
    // Flat: `{project}--{workweave}`, minted from two inputs, with nothing
    // observed feeding in. Two repos of one workweave live in
    // different object stores, so one name per store is enough.
    let ephemeral = EphemeralRefName::mint(project, name);
    let mut registry = RefRegistry::for_project(primary_root, project);

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

            // Receipt first, then the ref — and the receipt is what a rollback
            // DESTROY has to be holding, so nothing is "pre-recorded" by name
            // any more. `birth_ephemeral_worktree` hands back what it claimed
            // even when the birth call failed, which is the hook-failure case
            // the old pre-recording existed for.
            match vcs
                .head_revision(&repo_abs)
                .map_err(anyhow::Error::from)
                .and_then(|start| {
                    birth_ephemeral_worktree(
                        vcs.as_ref(),
                        &mut registry,
                        &repo_abs,
                        &worktree_dest,
                        &ephemeral,
                        start,
                        WorktreeFiles::Shared(&Filesystem, entry.role),
                    )
                }) {
                Ok(outcome) => {
                    placement = outcome.placement;
                    rollback.record_ref_attempt(outcome.owned, outcome.birth);
                    if let Some(e) = outcome.failure {
                        result = Err(e);
                    }
                }
                // Failed before any receipt existed: nothing was claimed, so
                // there is nothing for the guard to undo in this repo.
                Err(e) => result = Err(e),
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
                    entry.submodules,
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
        bail!(
            "workweave create completed with {failed} failure(s) out of {total} repo(s){cleanup_note}"
        );
    }

    if !submodule_warnings.is_empty() {
        eprintln!(
            "rwv workweave create: warning: submodules not initialized in {} repo(s): {}; \
             once the canonical clone holds what they record, `rwv doctor --fix` checks them out",
            submodule_warnings.len(),
            submodule_warnings.join(", ")
        );
    }

    // Create worktree for the project repo (if it is a git repo).
    // If the project directory exists but is not a git repo, copy it into the
    // workweave so that activate_workweave can find rwv.toml there.
    let project_wt_dest = project_dir(&workweave_dir, project);
    let project_dir = project_dir(source_root, project);
    if project_vcs.is_repo(&project_dir) {
        // B8: project-worktree creation failure must NOT silently fall
        // through to a static directory copy. The copy fallback is for
        // the "project dir exists but is not a git repo" branch only.
        // Producing a non-worktree copy here looks identical to a real
        // workweave on disk but has no upstream — commits go nowhere.
        std::fs::create_dir_all(project_wt_dest.parent().unwrap())?;
        // Record the project repo for the post-rollback prune pass BEFORE the
        // birth, for the same hook-failure reason as manifest repos above.
        // The project repo is an instance of the model, so this arm runs the
        // identical receipt-first birth over the identical ephemeral name.
        rollback.record_attempted_repo(project_dir.clone());
        let birth = match project_vcs
            .head_revision(&project_dir)
            .map_err(anyhow::Error::from)
            .and_then(|start| {
                birth_ephemeral_worktree(
                    project_vcs.as_ref(),
                    &mut registry,
                    &project_dir,
                    &project_wt_dest,
                    &ephemeral,
                    start,
                    WorktreeFiles::Written,
                )
            }) {
            Ok(outcome) => {
                let failure = outcome.failure;
                rollback.record_ref_attempt(outcome.owned, outcome.birth);
                match failure {
                    Some(e) => Err(e),
                    None => Ok(()),
                }
            }
            Err(e) => Err(e),
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
                bail!(
                    "could not create project worktree projects/{}: {e:#}{hook_hint}{cleanup_note}",
                    project.as_str()
                );
            }
        }
    } else if project_dir.exists() {
        // Project dir is not a git repo — copy it so activate has access to rwv.toml.
        copy_dir_recursive(&project_dir, &project_wt_dest)?;
    }

    // The project worktree above was checked out from a ref, so its `rwv.toml`
    // is the last committed version — any uncommitted edits in source_root's
    // working tree were dropped. Overlay the source's
    // working-tree `rwv.toml` (and `rwv.lock` for completeness) so the
    // workweave captures the operator's in-flight state. Warn loudly when
    // we're doing this so dirty creates don't surprise.
    //
    // Limited to `rwv.toml` / `rwv.lock` deliberately: these are the files
    // that change workweave behavior (manifest = what worktrees to create,
    // workweave config; lock = lockfile shared with downstream). Other
    // uncommitted project files remain at their committed state, matching
    // the existing worktree-from-ref contract for everything else.
    // Whether either file is dirty is git's question, not a byte
    // comparison's: the worktree checkout above sits on the far side of
    // git's clean/smudge filter, so under `core.autocrlf` its bytes differ
    // from the source working tree's for a file git considers unchanged,
    // and an overlay driven by byte inequality writes the source's line
    // endings into a tree whose index expects the filtered ones — tracked
    // dirt in a workweave that started clean.
    let mut captured = CapturedEdits::default();
    if project_dir.exists() && project_wt_dest.exists() && project_vcs.is_repo(&project_dir) {
        let dirty_names = project_vcs
            .dirty_file_names(&project_dir)
            .context("failed to read project repo status for the dirty-state overlay")?;
        captured.uncovered = dirty_names
            .iter()
            .filter(|n| ![Manifest::FILE_NAME, LockFile::FILE_NAME].contains(&n.as_str()))
            .cloned()
            .collect();
        for fname in [Manifest::FILE_NAME, LockFile::FILE_NAME] {
            let src = project_dir.join(fname);
            let dst = project_wt_dest.join(fname);
            if !src.exists() || !dirty_names.iter().any(|n| n == fname) {
                continue;
            }
            eprintln!(
                "rwv workweave create: using working-tree projects/{}/{fname} \
                 (uncommitted changes; workweave captures dirty state)",
                project.as_str(),
            );
            match std::fs::read(&src).and_then(|bytes| std::fs::write(&dst, &bytes).map(|_| bytes))
            {
                Ok(bytes) => captured.files.push((fname, bytes)),
                Err(e) => {
                    captured.incomplete = true;
                    eprintln!(
                        "rwv workweave create: warning: failed to overlay {}: {e:#}",
                        dst.display()
                    );
                }
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
    if project_dir.exists() && project_wt_dest.exists() {
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
    // workweave was forked from (= source_root) so a bare `rwv sync-to` knows
    // where to land. For workweaves forked directly from primary, parent
    // == primary; for workweaves forked from another workweave, parent is
    // that workweave's directory.
    let marker = WorkweaveMarker::new(primary_root.to_path_buf(), project.clone(), source_root);
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
    let recorded_dir = resolve(&workweave_dir).unwrap_or_else(|_| workweave_dir.clone());
    workweave_index::record_workweave(primary_root, project, name, recorded_dir)
        .context("failed to record workweave in the primary-side registry")?;

    // All steps complete — defuse the rollback guard so Drop is a no-op.
    rollback.defuse();

    // Only now does the workweave certainly keep what was copied into it: a
    // failure above rolled it back, and with it the only other copy.
    if dirty_source == DirtySource::Capture {
        release_captured_edits(project_vcs.as_ref(), &project_dir, project, &captured);
    }

    if let Some(cause) = hooks_failed {
        return Err(HooksIncomplete {
            dir: workweave_dir,
            cause,
        }
        .into());
    }

    Ok(workweave_dir)
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
            crate::path_spelling::operator_path(&self.dir)
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

/// What `--capture-dirty` copied into a new workweave, held so the source
/// gives up exactly what the copy took.
#[derive(Default)]
struct CapturedEdits {
    /// Each copied file's name and the bytes the workweave received.
    files: Vec<(&'static str, Vec<u8>)>,
    /// A file the capture covers failed to copy.
    incomplete: bool,
    /// Uncommitted files in the source the capture does not cover.
    uncovered: Vec<String>,
}

/// Discard from the source project directory the edits `captured` moved into
/// the new workweave, so the source is not left carrying a second copy that
/// would stop the workweave landing back on it.
///
/// Nothing is discarded unless every covered file copied; and a file is
/// discarded only while it still holds the bytes copied, with nothing staged
/// that those bytes do not show. Whatever is left is named, and none of it
/// fails the create: the workweave already holds a copy.
fn release_captured_edits(
    vcs: &dyn Vcs,
    project_dir: &Path,
    project: &ProjectName,
    captured: &CapturedEdits,
) {
    if captured.incomplete {
        eprintln!(
            "rwv workweave create: warning: the uncommitted edits stay in projects/{project} \
             as well: copying them into the workweave did not complete, so nothing was \
             discarded from the source"
        );
        return;
    }
    for (fname, bytes) in &captured.files {
        let left = match vcs.discard_captured_change(project_dir, Path::new(fname), bytes) {
            Ok(crate::vcs::DiscardOutcome::Discarded) => {
                eprintln!(
                    "rwv workweave create: moved the uncommitted changes to \
                     projects/{project}/{fname} into the workweave; the source holds its \
                     committed version"
                );
                continue;
            }
            Ok(crate::vcs::DiscardOutcome::ChangedSinceCapture) => {
                "it changed after it was copied".to_string()
            }
            Ok(crate::vcs::DiscardOutcome::HoldsUncapturedChange) => {
                "it has staged changes its working file does not show, and only the \
                 working file was copied"
                    .to_string()
            }
            Err(e) => format!("discarding it failed: {e:#}"),
        };
        eprintln!(
            "rwv workweave create: warning: projects/{project}/{fname} stays uncommitted in \
             the source as well as in the workweave: {left}"
        );
    }
    if !captured.uncovered.is_empty() {
        eprintln!(
            "rwv workweave create: these uncommitted files in projects/{project} are not \
             captured and stay in the source only: {}",
            captured.uncovered.join(", ")
        );
    }
}

/// Validate that an existing workweave directory matches `(primary_root, project, name)`
/// and is in a clean state relative to `source_root`, then return its path
/// without modifying anything.
///
/// Called from [`create_workweave`] on re-invocation without
/// `--replace-existing`. Refuses
/// if the `.rwv-workweave` marker is missing or for a different primary/project,
/// or if any per-repo worktree has uncommitted changes or has diverged from the
/// source's HEAD.
fn reuse_existing_workweave(
    primary_root: &Path,
    source_root: &Path,
    project: &ProjectName,
    _name: &WorkweaveName,
    workweave_dir: &Path,
    manifest: &Manifest,
) -> anyhow::Result<PathBuf> {
    let marker = WorkweaveMarker::read(workweave_dir)?.ok_or_else(|| {
        crate::refusal::refusal(
            RefusalKind::TargetDirOccupied,
            format!(
                "workweave directory {} exists but has no .rwv-workweave marker — \
                 likely a partially created workweave from a previous failed attempt; \
                 safe to recreate with --replace-existing",
                workweave_dir.display()
            ),
        )
    })?;

    if marker.project() != project {
        crate::refuse!(
            RefusalKind::ForeignWorkweaveInTargetDir,
            "workweave at {} is for project '{}', refusing to recreate for project '{}'; \
             rerun with --replace-existing to overwrite",
            workweave_dir.display(),
            marker.project().as_str(),
            project
        );
    }

    if !marker.names_primary(primary_root) {
        crate::refuse!(
            RefusalKind::ForeignWorkweaveInTargetDir,
            "workweave at {} is for primary workspace {}, refusing to recreate for {}; \
             rerun with --replace-existing to overwrite",
            workweave_dir.display(),
            marker.primary().display(),
            primary_root.display()
        );
    }

    // Detect local modifications (uncommitted changes or HEAD divergence
    // from source) in any existing worktree. Missing worktrees are not
    // "modified" — a manifest may have grown since the workweave was
    // created; `rwv workweave sync` is the path for adding them.
    let mut modified: Vec<String> = Vec::new();
    for (repo_path, entry) in manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
        let worktree_dest = workweave_dir.join(repo_path.as_path());
        if !worktree_dest.exists() {
            continue;
        }
        if vcs.has_uncommitted_changes(&worktree_dest)? {
            modified.push(format!("{}: uncommitted changes", repo_path.as_str()));
            continue;
        }
        let repo_abs = source_root.join(repo_path.as_path());
        let wt_head = vcs.head_revision(&worktree_dest)?;
        let source_head = vcs.head_revision(&repo_abs)?;
        if wt_head != source_head {
            modified.push(format!(
                "{}: worktree has diverged from source ({} vs {})",
                repo_path.as_str(),
                wt_head.id().short(),
                source_head.id().short(),
            ));
        }
    }

    if !modified.is_empty() {
        crate::refuse!(
            RefusalKind::ReplaceTargetHoldsWork,
            "workweave at {} has local modifications; refusing to recreate without \
             --replace-existing:\n  {}",
            workweave_dir.display(),
            modified.join("\n  ")
        );
    }

    Ok(workweave_dir.to_path_buf())
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
    let mut dirty = Vec::new();

    // Project worktree.
    let project_wt = project_dir(workweave_dir, project);
    if project_vcs.is_repo(&project_wt) {
        let rel = project_rel_path(project);
        match project_vcs.has_uncommitted_changes(&project_wt) {
            Ok(true) => dirty.push(rel),
            Ok(false) => {}
            Err(e) => dirty.push(format!("{rel}: status check failed: {e:#}")),
        }
    }

    for checkout in member_depth_checkouts(workweave_dir, manifest) {
        let wt = workweave_dir.join(checkout.repo_path.as_path());
        if !wt.exists() {
            continue;
        }
        match checkout.vcs.has_uncommitted_changes(&wt) {
            Ok(true) => dirty.push(checkout.label()),
            Ok(false) => {}
            Err(e) => dirty.push(format!("{}: status check failed: {e:#}", checkout.label())),
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
/// was forked from, and the default sync-to target) comes first when the
/// marker is readable and the path still exists; the primary weave root is
/// always included. With stacked workweaves a child workweave's work lands
/// in its parent workweave and may reach primary only after further
/// sync-to hops — checking primary alone would refuse every child retire.
fn merge_baselines(workweave_dir: &Path, ws_root: &Path) -> Vec<PathBuf> {
    let mut baselines: Vec<PathBuf> = Vec::new();
    let root = CanonicalPath::of(ws_root);
    if let Ok(Some(marker)) = WorkweaveMarker::read(workweave_dir) {
        if marker.parent().as_path().exists() && *marker.parent() != root {
            baselines.push(marker.parent().as_path().to_path_buf());
        }
    }
    baselines.push(ws_root.to_path_buf());
    baselines
}

/// Walk the workweave's repos and report those whose worktree HEAD holds
/// commits not reachable from the same repo's HEAD in ANY of the `baselines`
/// (see [`merge_baselines`]).
///
/// Deleting such a workweave destroys committed work: the ephemeral-branch
/// cleanup force-deletes the only ref pointing at those commits. A worktree
/// whose HEAD cannot be read is reported as diverged (conservative: "we
/// couldn't confirm safe"); a member present in no baseline at all is
/// skipped, matching the dirty check's missing-repo behavior, and a checkout
/// outside the manifest is reported.
fn collect_diverged_paths(
    project_vcs: &dyn Vcs,
    workweave_dir: &Path,
    project: &ProjectName,
    manifest: &Manifest,
    baselines: &[PathBuf],
) -> Vec<String> {
    let mut diverged = Vec::new();

    let mut check = |vcs: &dyn Vcs, wt: &Path, rel: &Path, label: String, vouch_required: bool| {
        if !wt.exists() {
            return;
        }
        let wt_head = match vcs.head_revision(wt) {
            Ok(h) => h,
            Err(e) => {
                diverged.push(format!("{label}: HEAD check failed: {e:#}"));
                return;
            }
        };
        // Resolve the workweave checkout's actual canonical store. The
        // `is_ancestor` query below MUST run in a DAG that contains BOTH
        // wt_head AND the baseline tip; the workweave checkout shares its
        // canonical store with the linked-workspace baselines (under
        // tier-0 invariants), so asking in the resolved canonical store
        // gives a sound answer even when the workweave dir is itself a
        // worktree. Under tier-0 violations the canonical store for the
        // workweave checkout differs from the baseline's canonical store,
        // and we conservatively decline to vouch — see below.
        let wt_canonical = match vcs
            .resolve_canonical_store(wt)
            .and_then(|s| s.parent().map(|p| p.to_path_buf()))
        {
            Some(p) => resolve(&p).unwrap_or(p),
            None => {
                diverged.push(format!(
                    "{label}: canonical-store lookup failed: not a repo"
                ));
                return;
            }
        };
        let mut candidates = 0;
        for base in baselines {
            let canonical = base.join(rel);
            if !vcs.is_repo(&canonical) {
                continue;
            }
            candidates += 1;
            // Refuse to vouch across distinct canonical stores: an
            // is_ancestor query whose operands live in different object
            // DAGs is silently unsound
            // (see docs/explanation/joints/clone-topology.md).
            // When the baseline's canonical store differs from the
            // workweave checkout's, treat as not-vouched-by-this-baseline
            // and let the operator run `rwv doctor`.
            let base_canonical = match vcs
                .resolve_canonical_store(&canonical)
                .and_then(|s| s.parent().map(|p| p.to_path_buf()))
            {
                Some(p) => resolve(&p).unwrap_or(p),
                None => continue,
            };
            if base_canonical != wt_canonical {
                continue;
            }
            if let Ok(c) = vcs.head_revision(&canonical) {
                // Run is_ancestor in the resolved canonical store so the
                // query is rooted in the DAG that contains both refs.
                if vcs
                    .is_ancestor(&wt_canonical, &wt_head, &c)
                    .unwrap_or(false)
                {
                    return; // vouched: this baseline contains the work
                }
            }
        }
        if candidates > 0 || vouch_required {
            diverged.push(label);
        }
    };

    let project_rel = project_rel_dir(project);
    let project_wt = workweave_dir.join(&project_rel);
    if project_vcs.is_repo(&project_wt) {
        check(
            project_vcs,
            &project_wt,
            &project_rel,
            project_rel_path(project),
            false,
        );
    }

    // No manifest entry says where a checkout outside it came from, so a
    // baseline lacking the repo is no reason to skip it: nothing else may
    // hold its commits.
    for checkout in member_depth_checkouts(workweave_dir, manifest) {
        check(
            checkout.vcs.as_ref(),
            &workweave_dir.join(checkout.repo_path.as_path()),
            &checkout.repo_path.as_path(),
            checkout.label(),
            !checkout.in_manifest,
        );
    }

    diverged
}

/// Manifest-independent dirty scan: walk `dir` for git repos (worktrees or
/// clones) and report those with uncommitted changes, relative to `dir`.
///
/// Used when a workweave's marker is missing or belongs to another project,
/// so no manifest can be trusted to enumerate its contents. Descent stops at
/// each repo root (no nested-repo scanning), and a repo whose dirty check
/// fails is reported as dirty (conservative: "we couldn't confirm clean").
fn collect_dirty_repos_by_walk(vcs: &dyn Vcs, dir: &Path) -> Vec<String> {
    fn walk(vcs: &dyn Vcs, base: &Path, cur: &Path, dirty: &mut Vec<String>) {
        if crate::git::has_git_dir(cur) {
            if vcs.has_uncommitted_changes(cur).unwrap_or(true) {
                let rel = cur.strip_prefix(base).unwrap_or(cur);
                dirty.push(rel.display().to_string());
            }
            return;
        }
        if let Ok(entries) = std::fs::read_dir(cur) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(vcs, base, &path, dirty);
                }
            }
        }
    }
    let mut dirty = Vec::new();
    walk(vcs, dir, dir, &mut dirty);
    dirty
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
pub(crate) fn resolved_worktree_parent(vcs: &dyn Vcs, checkout: &Path, fallback: &Path) -> PathBuf {
    if !checkout.exists() {
        return fallback.to_path_buf();
    }
    match vcs
        .resolve_canonical_store(checkout)
        .and_then(|s| s.parent().map(|p| p.to_path_buf()))
    {
        Some(p) => resolve(&p).unwrap_or(p),
        None => fallback.to_path_buf(),
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

    let mut check = |vcs: &dyn Vcs, checkout: &Path, label: String| {
        if !checkout.exists() {
            return;
        }
        let canonical = match vcs
            .resolve_canonical_store(checkout)
            .and_then(|s| s.parent().map(|p| p.to_path_buf()))
        {
            Some(p) => resolve(&p).unwrap_or(p),
            None => return, // not a repo; nothing to host
        };
        let checkout_canonical = resolve(checkout).unwrap_or(checkout.to_path_buf());
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
        let ww_canonical = resolve(workweave_dir).unwrap_or(workweave_dir.to_path_buf());
        let mut foreign: Vec<PathBuf> = Vec::new();
        for wt in worktrees {
            let wt_canon = resolve(&wt).unwrap_or(wt.clone());
            if wt_canon == checkout_canonical {
                continue; // the canonical store's own "main" entry
            }
            if !wt_canon.starts_with(&ww_canonical) {
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

    if hazards.is_empty() {
        return Ok(());
    }

    crate::refuse!(
        RefusalKind::StandaloneInWorkweave,
        "rwv workweave delete: refusing — inverted clone topology detected.\n\
         The following checkout(s) inside workweave {} are themselves canonical \
         stores that other worktrees link into; deleting this workweave would orphan \
         those dependents:\n  {}\n\n\
         Run `rwv doctor` for a full topology audit and remediation guidance. \
         This refusal is NOT bypassable with --discard-uncommitted or \
         --discard-unmerged-commits: those consent to losing this workweave's work, \
         not to corrupting unrelated worktrees whose object store we happen to be \
         hosting.",
        workweave_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| workweave_dir.display().to_string()),
        hazards.join("\n  "),
    )
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
/// Shared by both the retire (`sync-to --retire`) and `workweave delete` paths
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
        let mut marker = match WorkweaveMarker::read(&child_dir) {
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
        marker.repoint_parent(&new_parent);
        if let Err(e) = marker.write(&child_dir) {
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

/// The baselines a `Merged` warrant for `rel` may be measured against, as
/// tips, restricted to the ones sharing `store`'s object DAG.
///
/// An `is_ancestor` query whose operands live in different object DAGs is
/// silently unsound (docs/explanation/joints/clone-topology.md), so a baseline
/// whose canonical store differs is simply not a baseline here — the same refusal-to-vouch
/// [`collect_diverged_paths`] makes, applied to the warrant that authorizes
/// the deletion rather than to the precondition that permits the verb.
fn baseline_tips_in_store(
    vcs: &dyn Vcs,
    store: &Path,
    baselines: &[PathBuf],
    rel: &Path,
) -> Vec<ResolvedRevisionId> {
    let mut tips = Vec::new();
    for base in baselines {
        let candidate = base.join(rel);
        if !vcs.is_repo(&candidate) || receipt_store_for(vcs, &candidate) != store {
            continue;
        }
        if let Ok(tip) = vcs.head_revision(&candidate) {
            tips.push(tip);
        }
    }
    tips
}

/// Destroy the refs rwv **recorded creating** in `store` for this workweave,
/// and *report* every other branch in the workweave's namespace.
///
/// The merged-check and the deletion range over the same thing — the receipt
/// — so a check that inspects one HEAD per repo cannot authorize a deletion
/// that sweeps a whole prefix.
///
/// The listing pass is report-only **by type**. `list_branch_names_with_prefix`
/// answers in [`RawRefName`]s, and no route leads from one of those to
/// [`Vcs::delete_owned_ref`]; the only producer of the [`OwnedRef`] that
/// method takes is [`RefRegistry::record_created`]. So a hand-made
/// `my--feature/wip` is reported and left alone — not because a check
/// remembered to skip it, but because nothing here can spell its deletion.
///
/// Receipts are retracted **after** the ref they describe is gone, never
/// before: the reverse order would leave an unreceipted ref, which R2 disowns
/// permanently. Retraction is what later lets R4 pass for `prune_dropped_repo`.
fn retire_recorded_refs(
    vcs: &dyn Vcs,
    registry: &mut RefRegistry,
    store: &Path,
    ephemeral: &EphemeralRefName,
    label: &str,
    baseline_tips: &[ResolvedRevisionId],
    discard_unmerged: Option<DiscardUnmergedConsent>,
) {
    let raw = ephemeral.to_raw();
    match registry.lookup(store, &raw) {
        Ok(Some(owned)) => match vcs.resolve_local_branch_tip(store, &raw) {
            Ok(None) => {
                // Recorded, but the ref is already gone. Nothing to destroy;
                // retract the dangling receipt so the store can later satisfy R4.
                if let Err(e) = registry.retract(store, &raw) {
                    eprintln!(
                        "rwv workweave delete: warning: {label}: branch {owned} is already \
                         gone but its ownership receipt could not be retracted ({e:#}); run \
                         `rwv doctor --fix`"
                    );
                }
            }
            _ => {
                let warrant = baseline_tips
                    .iter()
                    .find_map(|baseline| DeletionWarrant::merged(vcs, &owned, baseline))
                    .or_else(|| discard_unmerged.map(DeletionWarrant::operator_discarded));
                match warrant {
                    Some(warrant) => {
                        let why = warrant.describe();
                        match vcs.delete_owned_ref(&owned, warrant) {
                            Ok(()) => {
                                if let Err(e) = registry.retract(store, &raw) {
                                    eprintln!(
                                        "rwv workweave delete: warning: {label}: branch \
                                         {owned} deleted ({why}) but its ownership receipt \
                                         could not be retracted ({e:#}); run `rwv doctor --fix`"
                                    );
                                }
                            }
                            Err(e) => eprintln!(
                                "rwv workweave delete: warning: {label}: could not delete \
                                 branch {owned} ({why}): {e:#}"
                            ),
                        }
                    }
                    None => eprintln!(
                        "rwv workweave delete: warning: {label}: branch {owned} is rwv's but \
                         holds commits no baseline contains; left in place. Merge it, or \
                         re-run with --discard-unmerged-commits to consent to losing it."
                    ),
                }
            }
        },
        Ok(None) => {}
        Err(e) => eprintln!(
            "rwv workweave delete: warning: {label}: could not read the ownership receipts \
             in {}: {e:#}",
            store.display()
        ),
    }

    // Report-only. Anything still standing in this workweave's namespace is a
    // ref rwv did not record creating, so under R2 it is not rwv's to delete —
    // it is the operator's, and the only useful thing to do with it is say so.
    match vcs.list_branch_names_with_prefix(store, ephemeral.to_raw().as_str()) {
        Ok(observed) => {
            for branch in observed {
                if is_this_workweaves_namespace(&branch, ephemeral) {
                    eprintln!(
                        "rwv workweave delete: {label}: branch {branch} is not recorded as \
                         rwv's; left in place (delete it yourself with \
                         `git -C {repo} branch -d {branch}` if you no longer want it)",
                        repo = store.display(),
                    );
                }
            }
        }
        Err(e) => eprintln!(
            "rwv workweave delete: warning: {label}: could not list branches in {}: {e:#}",
            store.display()
        ),
    }
}

/// Delete a workweave: remove worktrees (including project repo) and delete
/// the workweave directory.
///
/// Refuses to delete a workweave with uncommitted changes (in the project
/// worktree or any manifest-repo worktree) unless `discard_uncommitted` carries
/// the operator's consent, and one holding commits not merged into its parent
/// weave unless `discard_unmerged` does. Each error lists the paths so the
/// operator knows what would have been lost. Only CLI dispatch can mint either
/// token, so no other caller can waive either refusal. `discard_unmerged` is
/// also the [`DeletionWarrant::operator_discarded`] warrant every unmerged
/// ref's DESTROY needs.
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
    discard_unmerged: Option<DiscardUnmergedConsent>,
) -> anyhow::Result<()> {
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
                registered = crate::path_spelling::operator_path(&workweave_dir),
                expected = crate::path_spelling::operator_path(expected.as_path()),
            );
        }
    }
    delete_workweave_inner_at(
        ws_root,
        project,
        name,
        &workweave_dir,
        discard_uncommitted,
        discard_unmerged,
        OpGuard::Enforce,
    )
}

/// Delete a workweave as the terminal step of the op that owns it (`sync-to
/// --retire`).
///
/// `workweave_dir` is the on-disk path resolved by the sync engine at op
/// start; this path is authoritative here and bypasses the primary-side
/// registry lookup. Retire runs from inside the workweave and has already
/// validated its marker; a missing registry entry (mid-crash resume, or
/// bootstrap workspace that never wrote its index) must not prevent retire
/// from cleaning up. The marker round-trip is still enforced inside
/// `delete_workweave_inner_at`.
///
/// `--retire` has no waiver flag, so it holds neither consent: uncommitted or
/// unmerged work refuses, which the retire phase's own refusals already
/// guarantee before this runs.
pub(crate) fn delete_workweave_for_retire(
    ws_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    workweave_dir: &Path,
) -> anyhow::Result<()> {
    delete_workweave_inner_at(
        ws_root,
        project,
        name,
        workweave_dir,
        None,
        None,
        OpGuard::HeldByRetiringOp,
    )
}

/// Whether a delete refuses a workweave that carries an in-flight op record.
enum OpGuard {
    Enforce,
    /// The op deleting the workweave holds its record there until its own
    /// cleanup phase, after the delete.
    HeldByRetiringOp,
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
fn delete_workweave_inner_at(
    ws_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    workweave_dir: &Path,
    discard_uncommitted: Option<DiscardUncommittedConsent>,
    discard_unmerged: Option<DiscardUnmergedConsent>,
    op_guard: OpGuard,
) -> anyhow::Result<()> {
    let project_vcs = project_vcs();
    let manifest = load_manifest(ws_root, project)?;
    let workweave_dir = workweave_dir.to_path_buf();
    let ephemeral = EphemeralRefName::mint(project, name);
    let mut registry = RefRegistry::for_project(ws_root, project);

    refuse_symlinked_members(&workweave_dir, project)?;

    // Tier-0 topology precondition: refuse when a per-repo checkout inside
    // the workweave is itself a canonical store with foreign dependents.
    // Runs before the dirty / unmerged checks (and is not bypassable by any
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

    // Refuse before the dirty/unmerged checks so a mid-op delete reports the
    // in-flight op. No override: `rwv abort` is the way to clear a stale record.
    if workweave_dir.exists() && matches!(op_guard, OpGuard::Enforce) {
        crate::op_state::check_no_op_in_progress(&[workweave_dir.as_path()])?;
    }

    // Safety checks: each refusal has its own waiver. Skipped when the
    // workweave directory doesn't exist (nothing to lose).
    if discard_uncommitted.is_none() && workweave_dir.exists() {
        let dirty = collect_dirty_paths(project_vcs.as_ref(), &workweave_dir, project, &manifest);
        if !dirty.is_empty() {
            crate::refuse!(
                RefusalKind::UncommittedWork,
                "workweave {} has uncommitted changes; refusing to delete without \
                 --discard-uncommitted:\n  {}",
                name.as_str(),
                dirty.join("\n  ")
            );
        }
    }
    // The baselines the per-ref `Merged` warrants are measured against, read
    // while the workweave's marker still exists (it names the parent). Work
    // counts as merged when its recorded parent OR the primary weave contains
    // it — nested workweaves land in their parent first.
    let baselines = merge_baselines(&workweave_dir, ws_root);

    // Committed-but-unmerged work is just as lost as uncommitted work:
    // the ephemeral-ref cleanup below destroys the only ref to those
    // commits. This is the verb-level precondition; each destroy below
    // additionally has to hold its own warrant (R3).
    if discard_unmerged.is_none() && workweave_dir.exists() {
        let diverged = collect_diverged_paths(
            project_vcs.as_ref(),
            &workweave_dir,
            project,
            &manifest,
            &baselines,
        );
        if !diverged.is_empty() {
            crate::refuse!(
                RefusalKind::UnmergedCommits,
                "workweave {} has commits not merged into {}; \
                 refusing to delete without --discard-unmerged-commits:\n  {}",
                name.as_str(),
                baselines
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" or "),
                diverged.join("\n  ")
            );
        }
    }

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
    // grandparent; fall back to primary) so a bare `rwv sync-to` from a child
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
        let repo_abs = resolved_worktree_parent(vcs.as_ref(), &worktree_path, &fallback_repo_abs);

        if worktree_path.exists() {
            // Detect "checkout is its own canonical store" — `git worktree
            // remove` would fail with "is a main working tree". The
            // `refuse_if_checkouts_host_foreign_worktrees` precondition
            // above has already rejected the unsafe (with-dependents)
            // case; reaching this branch means the canonical-checkout is
            // lone, and removing the workweave dir is sufficient. Skip
            // the registration cleanup (there is no parent to clean) and
            // also the prune / branch-delete loop (nothing else can know
            // about this store's refs once the dir is gone).
            let checkout_canonical =
                resolve(&worktree_path).unwrap_or_else(|_| worktree_path.clone());
            let is_lone_canonical = repo_abs == checkout_canonical;
            if is_lone_canonical {
                // Nothing to unregister; the dir-removal at the end of
                // delete_workweave will clean up the on-disk store.
                continue;
            }
            if let Err(e) = vcs.remove_worktree(&repo_abs, &worktree_path) {
                let msg = format!("{}: {e:#}", checkout.label());
                eprintln!("rwv workweave delete: error: {msg}");
                errors.push(msg);
                // Fall through to the ref pass anyway: the receipt set
                // does not depend on whether the worktree could be removed,
                // and a ref left standing because its branch is still checked
                // out gets reported rather than silently skipped.
            }
        }

        // Prune stale worktree metadata, then retire the recorded refs.
        // Same resolved-parent rationale applies — pruning the wrong repo
        // leaves the actual canonical store's stale entries in place.
        let _ = vcs.worktree_prune(&repo_abs);
        let baseline_tips =
            baseline_tips_in_store(vcs.as_ref(), &repo_abs, &baselines, &repo_path.as_path());
        retire_recorded_refs(
            vcs.as_ref(),
            &mut registry,
            &repo_abs,
            &ephemeral,
            &checkout.label(),
            &baseline_tips,
            discard_unmerged,
        );
    }

    // Remove the project repo worktree.
    // Only call remove_worktree if the workweave copy is actually a git worktree,
    // indicated by .git being a FILE (not a directory). If .git is a directory
    // (or absent), the workweave copy was a plain directory copy — just let
    // remove_dir_all below handle it.
    //
    // The project repo is an instance of the branch model, so the ref pass
    // below is NOT nested inside the worktree-removal outcome. Under R2 both
    // arms are the same operation over the same receipt set, so whether
    // `.git` was a file and whether `remove_worktree` returned `Ok` do not
    // change which receipts are owed a DESTROY.
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
    );
    if project_worktree.exists() && crate::git::is_linked_worktree(&project_worktree) {
        if let Err(e) = project_vcs.remove_worktree(&project_store, &project_worktree) {
            let msg = format!("{}: {e:#}", project_rel_path(project));
            eprintln!("rwv workweave delete: error: {msg}");
            errors.push(msg);
        }
        let _ = project_vcs.worktree_prune(&project_store);
    }
    let project_baseline_tips = baseline_tips_in_store(
        project_vcs.as_ref(),
        &project_store,
        &baselines,
        &project_rel,
    );
    retire_recorded_refs(
        project_vcs.as_ref(),
        &mut registry,
        &project_store,
        &ephemeral,
        &project_rel_path(project),
        &project_baseline_tips,
        discard_unmerged,
    );

    // Remove the workweave directory itself. A process's working directory
    // is an open handle on Windows, and `sync-to --retire` drives this
    // delete standing inside the workweave — the removal then fails with a
    // sharing violation whose holder is the deleting process itself. Step
    // out to the parent first, but only when the current directory sits
    // under the tree being removed: the hosting process may not be rwv's
    // own (a test harness shares one working directory across every
    // concurrent thread), so a directory this delete does not hold hostage
    // stays where it is. Both sides are canonicalized so `starts_with`
    // compares one spelling; a working directory that cannot be read or
    // canonicalized cannot be proven outside, and stepping out of it is
    // harmless. The live cwd — not the resolved origin — is the subject on
    // purpose: `-C` points the origin elsewhere while the handle stays put.
    if workweave_dir.exists() {
        let cwd_inside = match (
            std::env::current_dir().and_then(|d| resolve(&d)),
            resolve(&workweave_dir),
        ) {
            (Ok(cwd), Ok(ww)) => cwd.starts_with(&ww),
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

    if errors.is_empty() {
        Ok(())
    } else {
        let total = manifest.len() + 1;
        let failed = errors.len();
        bail!("workweave delete completed with {failed} failure(s) out of {total} repo(s)")
    }
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
/// landed) has no `.rwv-workweave-index`. List returns an empty vector in
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
    let mut result: Vec<(WorkweaveName, PathBuf)> = index
        .workweaves
        .into_iter()
        .filter(|(_, path)| {
            matches!(
                validate_registry_entry(ws_root, project, path),
                RegistryEntryValidation::Valid
            )
        })
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
    let mut containers: Vec<PathBuf> = Vec::new();
    let push_unique = |p: PathBuf, containers: &mut Vec<PathBuf>| {
        let canonical = resolve(&p).unwrap_or(p);
        if !containers.contains(&canonical) {
            containers.push(canonical);
        }
    };
    push_unique(workweave_index::default_container(ws_root), &mut containers);
    for project in crate::workspace::discover_projects(ws_root) {
        if let Ok(Some(idx)) = workweave_index::read(ws_root, &project) {
            push_unique(idx.container, &mut containers);
        }
    }

    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut result: Vec<(WorkweaveId, PathBuf)> = Vec::new();
    for container in &containers {
        for (id, dir) in doctor_scan_container(ws_root, container) {
            let canonical = resolve(&dir).unwrap_or_else(|_| dir.clone());
            if seen.insert(canonical) {
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
        if !marker.names_primary(ws_root) {
            continue;
        }
        let project_name = marker.project().clone();
        let dir_name = entry.file_name().to_string_lossy().into_owned();
        let recorded = workweave_name_for_path(ws_root, &project_name, &dir)
            .ok()
            .flatten();
        let name = match recorded {
            Some(n) => n,
            None => match workweave_name_in(&project_name, &dir_name) {
                Some(from_basename) => from_basename,
                // No record, and a basename this project could not have
                // rendered: identity is not recoverable. The misnamed-dir
                // finding owns this state.
                None => continue,
            },
        };
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
// `rwv workweave log [--diff]` — parent-relative history
// ---------------------------------------------------------------------------

/// A single commit in a `workweave log` listing, for `--json`.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct WorkweaveLogCommit {
    /// Full commit id.
    pub sha: CommitId,
    /// Abbreviated commit SHA.
    pub short: String,
    /// First line of the commit message.
    pub subject: String,
}

/// Per-repo entry in a `workweave log [--diff]` result.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct WorkweaveLogRepo {
    /// Manifest-relative repo path (e.g. `github/org/lib`).
    pub path: String,
    /// This workweave checkout's HEAD, if readable.
    pub head: Option<CommitId>,
    /// The recorded parent's tip for THIS repo — the HEAD of the parent's
    /// checkout of the same path — if resolvable.
    pub parent_tip: Option<CommitId>,
    /// The workweave's UNIQUE commits vs the parent tip — the commits in the
    /// workweave's history but not the parent's, newest first. Populated for
    /// `log`; empty in `--diff` mode.
    pub unique_commits: Vec<WorkweaveLogCommit>,
    /// The diff anchor: the common ancestor of the workweave tip and the
    /// parent tip. Populated only in `--diff` mode. Anchoring at the common
    /// ancestor (not the parent tip) avoids phantom reversals when the parent
    /// advanced after the fork.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_base: Option<CommitId>,
    /// The unified diff text of the workweave's unique work vs `diff_base`,
    /// in `--diff` mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// A non-fatal note when this repo could not be fully processed (parent
    /// checkout missing, unreadable HEAD, etc.). The repo is still listed so
    /// the operator sees the gap rather than a silent omission.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Top-level `workweave log [--diff] --json` envelope.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct WorkweaveLogOutput {
    /// The workweave name.
    pub workweave: String,
    /// The recorded parent workspace path (from the `.rwv-workweave` marker).
    pub parent: String,
    /// Whether this is a `diff` result (`true`) or a `log` result (`false`).
    pub diff: bool,
    /// Per-repo results, one per manifest repo.
    pub repos: Vec<WorkweaveLogRepo>,
    /// Log result for the project repo (`projects/<project>/.git`). Omitted
    /// when the project repo's working tree is not found. Carries
    /// `project_repo_key` in its `path` field, matching the convention sync-to
    /// uses for `project_repo_advance`. Separate keyed field (not a peer in
    /// `repos[]`) to mirror the sync-to JSON representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_repo: Option<WorkweaveLogRepo>,
}

fn missing_checkout_note(which: &str, path: &Path) -> String {
    format!(
        "{which} checkout missing at {}; run `rwv fetch` in that workweave to re-materialize it",
        crate::path_spelling::operator_path(path)
    )
}

/// Print (or JSON-emit) the workweave's UNIQUE commits vs the recorded parent,
/// per manifest repo.
///
/// Semantics:
///   - Parent identity comes from the `.rwv-workweave` marker, never the branch
///     name: the flat `<project>--<name>` branch names no parent, and adoption
///     can re-point the parent to primary.
///   - `unique` commits are those in the workweave's history but not the
///     parent's, resolving the parent tip from the parent's checkout of the
///     same repo. This stays correct when the parent ADVANCED since the fork:
///     commits the parent already has are excluded.
///   - `diff` mode anchors at the COMMON ANCESTOR of the workweave tip and the
///     parent tip, NOT the parent tip directly: diffing against a parent tip
///     that advanced after the fork would show phantom reversals of the work
///     the parent gained in the meantime.
///
/// All VCS specifics are delegated to the [`Vcs`] impl for each repo's
/// `vcs_type` via `vcs_for`; this function stays VCS-agnostic.
///
/// `cwd` must be inside a workweave. `diff` selects diff mode; `json` selects
/// machine output.
pub fn workweave_log(
    ctx: &crate::workspace::WorkspaceContext,
    diff: bool,
    json: bool,
) -> anyhow::Result<()> {
    use crate::workspace::Checkout;

    let (ww_name, ww_dir, project, parent_path) = match &ctx.checkout {
        Checkout::Workweave {
            name,
            dir,
            project,
            parent,
        } => (
            name.require(dir, project)?.clone(),
            dir.clone(),
            project.clone(),
            parent.clone(),
        ),
        Checkout::Primary { .. } => {
            crate::refuse!(
                RefusalKind::WrongCheckoutKind,
                "`rwv workweave log` reports a workweave's history relative to its \
                 recorded parent, but CWD ({}) is in the primary weave, not a workweave.",
                ctx.active_path().display()
            );
        }
    };

    if !parent_path.exists() {
        // Reuse the friendly dangling-parent remediation the sync path uses.
        crate::sync::check_parent_not_dangling(&parent_path, ctx.primary_path())?;
    }

    let manifest = load_manifest(ctx.primary_path(), &project)?;

    let mut repos: Vec<WorkweaveLogRepo> = Vec::new();
    for (repo_path, entry) in manifest.iter_entries() {
        let repo_rel = repo_path.as_path();
        let ww_repo = ww_dir.join(&repo_rel);
        let parent_repo = parent_path.join(&repo_rel);
        let vcs = vcs_for(entry.vcs_type);

        let mut note: Option<String> = None;

        // This workweave checkout's tip. Kept as the resolved id for the
        // output's `head` field; the trait methods re-resolve HEAD internally.
        let head = if !ww_repo.exists() {
            note = Some(missing_checkout_note("workweave", &ww_repo));
            None
        } else {
            match vcs.head_revision(&ww_repo) {
                Ok(rev) => Some(rev),
                Err(e) => {
                    note = Some(format!("workweave checkout HEAD unreadable: {e:#}"));
                    None
                }
            }
        };

        // The recorded parent's tip for THIS repo — HEAD in the parent's
        // checkout of the same path. Used as the exclusion boundary for
        // unique commits and as one endpoint of the diff's common ancestor.
        let parent_tip = if !parent_repo.exists() {
            if note.is_none() {
                note = Some(missing_checkout_note("parent", &parent_repo));
            }
            None
        } else {
            match vcs.head_revision(&parent_repo) {
                Ok(rev) => Some(rev),
                Err(e) => {
                    if note.is_none() {
                        note = Some(format!("parent checkout HEAD unreadable: {e:#}"));
                    }
                    None
                }
            }
        };

        let mut unique_commits: Vec<WorkweaveLogCommit> = Vec::new();
        let mut diff_base: Option<CommitId> = None;
        let mut diff_text: Option<String> = None;

        if let Some(parent_rev) = &parent_tip {
            if head.is_some() {
                if diff {
                    // The workweave's unique work vs the parent, anchored at
                    // their common ancestor so a parent that advanced after
                    // the fork does not show phantom reversals.
                    match vcs.unique_diff(&ww_repo, parent_rev) {
                        Ok(ud) => {
                            diff_base = ud.base;
                            diff_text = Some(ud.text);
                        }
                        Err(e) => note = Some(format!("diff failed: {e:#}")),
                    }
                } else {
                    match vcs.unique_commits(&ww_repo, parent_rev) {
                        Ok(entries) => {
                            unique_commits = entries
                                .into_iter()
                                .map(|c| WorkweaveLogCommit {
                                    short: c.id.short().to_owned(),
                                    sha: c.id,
                                    subject: c.subject,
                                })
                                .collect();
                        }
                        Err(e) => note = Some(format!("log failed: {e:#}")),
                    }
                }
            }
        }

        repos.push(WorkweaveLogRepo {
            path: repo_path.to_string(),
            head: head.map(|r| r.id().clone()),
            parent_tip: parent_tip.map(|r| r.id().clone()),
            unique_commits,
            diff_base,
            diff: diff_text,
            note,
        });
    }

    // Parent tip is read from the parent's project checkout, exactly as the
    // parent marker recorded it — no branch-name reconstruction.
    let project_repo = {
        let ww_project = project_dir(&ww_dir, &project);
        let parent_project = project_dir(&parent_path, &project);
        let vcs = project_vcs();

        let mut note: Option<String> = None;

        let head = match vcs.head_revision(&ww_project) {
            Ok(rev) => Some(rev),
            Err(e) => {
                note = Some(format!("workweave project checkout HEAD unreadable: {e:#}"));
                None
            }
        };

        let parent_tip = match vcs.head_revision(&parent_project) {
            Ok(rev) => Some(rev),
            Err(e) => {
                if note.is_none() {
                    note = Some(format!("parent project checkout HEAD unreadable: {e:#}"));
                }
                None
            }
        };

        let mut unique_commits: Vec<WorkweaveLogCommit> = Vec::new();
        let mut diff_base: Option<CommitId> = None;
        let mut diff_text: Option<String> = None;

        if let Some(parent_rev) = &parent_tip {
            if head.is_some() {
                if diff {
                    match vcs.unique_diff(&ww_project, parent_rev) {
                        Ok(ud) => {
                            diff_base = ud.base;
                            diff_text = Some(ud.text);
                        }
                        Err(e) => note = Some(format!("diff failed: {e:#}")),
                    }
                } else {
                    match vcs.unique_commits(&ww_project, parent_rev) {
                        Ok(entries) => {
                            unique_commits = entries
                                .into_iter()
                                .map(|c| WorkweaveLogCommit {
                                    short: c.id.short().to_owned(),
                                    sha: c.id,
                                    subject: c.subject,
                                })
                                .collect();
                        }
                        Err(e) => note = Some(format!("log failed: {e:#}")),
                    }
                }
            }
        }

        Some(WorkweaveLogRepo {
            path: project_repo_key().to_string(),
            head: head.map(|r| r.id().clone()),
            parent_tip: parent_tip.map(|r| r.id().clone()),
            unique_commits,
            diff_base,
            diff: diff_text,
            note,
        })
    };

    let output = WorkweaveLogOutput {
        workweave: ww_name.as_str().to_string(),
        parent: parent_path.to_string_lossy().to_string(),
        diff,
        repos,
        project_repo,
    };

    if json {
        let out = serde_json::to_string_pretty(&output)
            .context("failed to serialize workweave log to JSON")?;
        println!("{out}");
    } else {
        print_workweave_log_text(&output);
    }

    Ok(())
}

/// Human-readable rendering of a `workweave log [--diff]` result.
fn print_workweave_log_text(output: &WorkweaveLogOutput) {
    let verb = if output.diff { "diff" } else { "log" };
    println!(
        "workweave {} {} vs parent {}",
        output.workweave, verb, output.parent
    );
    // Manifest repos first, then the project repo at the end.
    let all_repos: Vec<&WorkweaveLogRepo> = output
        .repos
        .iter()
        .chain(output.project_repo.iter())
        .collect();
    for repo in all_repos {
        println!();
        println!("=== {} ===", repo.path);
        if let Some(note) = &repo.note {
            println!("  note: {note}");
        }
        if output.diff {
            match &repo.diff {
                Some(d) if !d.is_empty() => {
                    if let Some(base) = &repo.diff_base {
                        println!("  (diff range: {}..HEAD)", base.short());
                    }
                    println!("{d}");
                }
                _ => println!("  (no diff vs parent)"),
            }
        } else if repo.unique_commits.is_empty() {
            println!("  (no unique commits vs parent)");
        } else {
            for c in &repo.unique_commits {
                println!("  {} {}", c.short, c.subject);
            }
        }
    }
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
    hook_event_name: Option<String>,
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

    match input.hook_event_name.as_deref() {
        Some("WorktreeCreate") => {
            let cwd = input
                .cwd
                .ok_or_else(|| anyhow!("missing cwd in hook input"))?;

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

            let path = hand_over_seat(create_workweave(
                primary_root,
                source_root,
                &project,
                &WorkweaveName::new(&name)?,
                None,
                DirtySource::Refuse,
                None,
                crate::activate::ActivateOptions::default(),
            ))?;
            println!("{}", path.display());
        }
        Some("WorktreeRemove") => {
            let worktree_path = input
                .worktree_path
                .ok_or_else(|| anyhow!("missing worktree_path in hook input"))?;

            // Fire-and-forget: log errors but always succeed.
            if let Ok(Some(marker)) = WorkweaveMarker::read(Path::new(&worktree_path)) {
                let outcome = workweave_name_for_path(
                    marker.primary(),
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
                        marker.primary(),
                        marker.project(),
                        &name,
                        Some(Path::new(&worktree_path)),
                        None,
                        None,
                    )
                });
                if let Err(e) = outcome {
                    eprintln!("rwv workweave --claude-hook WorktreeRemove: warning: {e:#}");
                }
            }
            // Always exit 0.
        }
        other => {
            anyhow::bail!("unknown hook_event_name: {:?}", other);
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
    // `create_worktree_on` returning a `BornRef` or not — is not observable
    // from the CLI once create refuses an unowned name outright.
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
        let tmp = tempfile::tempdir().unwrap();
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
    fn guard_for(primary: &Path, project: &ProjectName) -> CreateRollbackGuard {
        let mut g =
            CreateRollbackGuard::new(git_vcs(), primary.join("never-created"), primary, project);
        g.defuse(); // Drop must not re-run what the test drives explicitly.
        g
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
            Submodules::Local,
            "main",
        )
        .err()
        .expect("a member with no canonical clone is refused");
        let message = format!("{err:#}");
        assert!(
            message.contains("run `rwv fetch --project web-app` at the primary weave first"),
            "{message}"
        );
        assert!(
            !message.contains("rwv sync"),
            "sync takes a source and is not the repair: {message}"
        );
    }

    #[test]
    fn rollback_destroys_a_ref_this_create_authored_and_retracts_its_receipt() {
        let (_tmp, primary, project, store) = weave();
        let mut registry = RefRegistry::for_project(&primary, &project);
        let name = EphemeralRefName::mint(&project, &WorkweaveName::new("ww").unwrap());
        let at = git_vcs().head_revision(&store).unwrap();
        let owned = registry.record_created(&store, name.clone(), at).unwrap();

        // A real birth, so the `BornRef` is the one `create_worktree_on`
        // produced rather than a value the test made up.
        let dest = primary.join("wt");
        let born = git_vcs()
            .create_worktree_on(&owned, &dest)
            .unwrap()
            .expect("a fresh name must be AUTHORED, not adopted");
        git_vcs().remove_worktree(&store, &dest).unwrap();

        let mut g = guard_for(&primary, &project);
        g.record_ref_attempt(owned, RefBirth::Authored(born));
        assert!(g.undo_ref_births().is_empty(), "the destroy should succeed");

        assert!(
            git_vcs()
                .resolve_local_branch_tip(&store, &name.to_raw())
                .unwrap()
                .is_none(),
            "an authored ref must be destroyed on rollback"
        );
        assert!(
            registry.lookup(&store, &name.to_raw()).unwrap().is_none(),
            "the receipt must be retracted with the ref it describes"
        );
    }

    #[test]
    fn rollback_leaves_an_adopted_ref_alone() {
        let (_tmp, primary, project, store) = weave();
        let mut registry = RefRegistry::for_project(&primary, &project);
        let name = EphemeralRefName::mint(&project, &WorkweaveName::new("ww").unwrap());

        // A branch that was already there, carrying a commit of its own.
        git(&store, &["checkout", "-b", &name.to_string()]);
        std::fs::write(store.join("theirs"), "operator work").unwrap();
        git(&store, &["add", "."]);
        git(&store, &["commit", "-m", "theirs"]);
        let their_tip = git(&store, &["rev-parse", &name.to_string()]);
        git(&store, &["checkout", "main"]);

        // The receipt this create wrote before discovering it had adopted.
        let owned = registry
            .record_created(
                &store,
                name.clone(),
                git_vcs().head_revision(&store).unwrap(),
            )
            .unwrap();
        let dest = primary.join("wt");
        let born = git_vcs().create_worktree_on(&owned, &dest).unwrap();
        assert!(
            born.is_none(),
            "fixture: a name already in the store must be ADOPTED, not authored"
        );
        git_vcs().remove_worktree(&store, &dest).unwrap();

        let mut g = guard_for(&primary, &project);
        g.record_ref_attempt(owned, RefBirth::Adopted);
        assert!(g.undo_ref_births().is_empty());

        assert_eq!(
            git(&store, &["rev-parse", &name.to_string()]),
            their_tip,
            "a create that ADOPTED a branch must not destroy it on rollback"
        );
        assert!(
            registry.lookup(&store, &name.to_raw()).unwrap().is_none(),
            "the claim this create made over a branch it did not author is retracted"
        );
    }

    #[test]
    fn rollback_reports_a_ref_that_moved_instead_of_destroying_it() {
        let (_tmp, primary, project, store) = weave();
        let mut registry = RefRegistry::for_project(&primary, &project);
        let name = EphemeralRefName::mint(&project, &WorkweaveName::new("ww").unwrap());
        let at = git_vcs().head_revision(&store).unwrap();
        let owned = registry.record_created(&store, name.clone(), at).unwrap();

        let dest = primary.join("wt");
        let born = git_vcs()
            .create_worktree_on(&owned, &dest)
            .unwrap()
            .expect("authored");

        // A commit lands on the ephemeral branch after its birth, so
        // `DeletionWarrant::unmoved` can no longer be built for it.
        std::fs::write(dest.join("later"), "committed after the birth").unwrap();
        git(&dest, &["add", "."]);
        git(&dest, &["commit", "-m", "later"]);
        let moved_tip = git(&store, &["rev-parse", &name.to_string()]);
        git_vcs().remove_worktree(&store, &dest).unwrap();

        let mut g = guard_for(&primary, &project);
        g.record_ref_attempt(owned, RefBirth::Authored(born));
        let failures = g.undo_ref_births();

        assert_eq!(
            failures.len(),
            1,
            "the moved ref must be reported: {failures:?}"
        );
        assert!(
            failures[0].contains(&name.to_string()) && failures[0].contains("moved off"),
            "the report must name the branch and why it was kept: {failures:?}"
        );
        assert_eq!(
            git(&store, &["rev-parse", &name.to_string()]),
            moved_tip,
            "a ref that moved since its birth must survive rollback"
        );
        assert!(
            registry.lookup(&store, &name.to_raw()).unwrap().is_some(),
            "its receipt survives too — the ref is still rwv's, just not safe to lose"
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
    fn claude_hook_unknown_event_errors() {
        // Deserialize directly and call the dispatch logic via the public API.
        // We simulate by constructing the input struct.
        let json = r#"{"hook_event_name":"UnknownEvent"}"#;
        let input: ClaudeHookInput = serde_json::from_str(json).unwrap();
        // The match arm should hit the `other` branch.
        assert_eq!(input.hook_event_name.as_deref(), Some("UnknownEvent"));
        assert!(input.cwd.is_none());
        assert!(input.worktree_path.is_none());
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
        assert_eq!(input.hook_event_name.as_deref(), Some("WorktreeCreate"));
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
    // Receipt survival across the Vcs seam's failure arms
    //
    // Each of these turns on something a real repo will not do on request: a
    // ref appearing inside the interval between classification and birth, and
    // a destroy that fails. What they pin is the same question in three
    // answers — whether the ownership receipt survives — because a receipt
    // that outlives a ref rwv did not author claims it, and one retracted
    // over a ref that may exist disowns it permanently.
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
    /// keys refs on the path it is handed, and the receipt is keyed on the
    /// canonical spelling.
    fn fake_weave() -> (tempfile::TempDir, PathBuf, ProjectName, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = ProjectName::new("web-app").unwrap();
        std::fs::create_dir_all(primary.join("projects/web-app")).unwrap();
        let store = primary.join("github/org/repo");
        std::fs::create_dir_all(&store).unwrap();
        let store = resolve(&store).unwrap();
        (tmp, primary, project, store)
    }

    #[test]
    fn a_ref_born_inside_the_window_is_adopted_and_its_receipt_retracted() {
        let (_tmp, primary, project, store) = fake_weave();
        let mut registry = RefRegistry::for_project(&primary, &project);
        let ephemeral = EphemeralRefName::mint(&project, &WorkweaveName::new("ww").unwrap());
        let raw = ephemeral.to_raw();

        let fake = FakeVcs::new();
        fake.before_next(VcsCall::MaterializeWorktreeOnRef, {
            let (store, raw) = (store.clone(), raw.clone());
            move |vcs| vcs.put_branch(&store, &raw, fake_rev('c'))
        });

        let outcome = birth_ephemeral_worktree(
            &fake,
            &mut registry,
            &store,
            &primary.join("wt"),
            &ephemeral,
            fake_rev('a'),
            WorktreeFiles::Written,
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
        assert!(registry.lookup(&store, &raw).unwrap().is_some());

        let err = outcome.into_authored(&mut registry).unwrap_err();
        assert!(
            err.to_string().contains("adopted a ref rwv did not create"),
            "unexpected error: {err}"
        );
        assert!(
            registry.lookup(&store, &raw).unwrap().is_none(),
            "an adopted ref is not rwv's, so its receipt must not survive"
        );
    }

    #[test]
    fn the_same_ref_standing_before_classification_refuses_instead() {
        let (_tmp, primary, project, store) = fake_weave();
        let mut registry = RefRegistry::for_project(&primary, &project);
        let ephemeral = EphemeralRefName::mint(&project, &WorkweaveName::new("ww").unwrap());
        let raw = ephemeral.to_raw();

        let fake = FakeVcs::new();
        fake.put_branch(&store, &raw, fake_rev('c'));

        let err = match birth_ephemeral_worktree(
            &fake,
            &mut registry,
            &store,
            &primary.join("wt"),
            &ephemeral,
            fake_rev('a'),
            WorktreeFiles::Written,
        ) {
            Err(e) => e,
            Ok(_) => panic!("a ref rwv holds no receipt for must refuse, not be adopted"),
        };

        assert!(
            err.to_string().contains("rwv holds no ownership receipt"),
            "unexpected error: {err}"
        );
        assert_eq!(
            fake.calls(),
            vec![
                VcsCall::ListBranchNamesWithPrefix,
                VcsCall::ResolveLocalBranchTip,
            ],
            "the refusal must land before anything is written"
        );
        assert!(registry.lookup(&store, &raw).unwrap().is_none());
    }

    #[test]
    fn a_birth_that_failed_keeps_its_receipt() {
        let (_tmp, primary, project, store) = fake_weave();
        let mut registry = RefRegistry::for_project(&primary, &project);
        let ephemeral = EphemeralRefName::mint(&project, &WorkweaveName::new("ww").unwrap());
        let raw = ephemeral.to_raw();

        let fake = FakeVcs::new();
        fake.fail_next(
            VcsCall::MaterializeWorktreeOnRef,
            VcsError::NotARepo(store.clone()),
        );

        let outcome = birth_ephemeral_worktree(
            &fake,
            &mut registry,
            &store,
            &primary.join("wt"),
            &ephemeral,
            fake_rev('a'),
            WorktreeFiles::Written,
        )
        .unwrap();
        assert!(outcome.failure.is_some());

        let err = outcome.into_authored(&mut registry).unwrap_err();
        assert!(
            err.to_string().contains("is not a vcs repository"),
            "the birth's own failure must be what surfaces: {err}"
        );
        assert!(
            registry.lookup(&store, &raw).unwrap().is_some(),
            "the ref may exist regardless, and only a receipt can authorize removing it later"
        );
    }

    #[test]
    fn a_destroy_that_failed_leaves_the_receipt_standing() {
        let (_tmp, primary, project, store) = fake_weave();
        let mut registry = RefRegistry::for_project(&primary, &project);
        let ephemeral = EphemeralRefName::mint(&project, &WorkweaveName::new("ww").unwrap());
        let raw = ephemeral.to_raw();
        registry
            .record_created(&store, ephemeral.clone(), fake_rev('a'))
            .unwrap();

        let fake = FakeVcs::new();
        fake.put_branch(&store, &raw, fake_rev('a'));
        fake.declare_ancestor(&fake_rev('a'), &fake_rev('b'));
        fake.fail_next(VcsCall::DestroyLocalRef, VcsError::NotARepo(store.clone()));

        retire_recorded_refs(
            &fake,
            &mut registry,
            &store,
            &ephemeral,
            "github/org/repo",
            &[fake_rev('b')],
            None,
        );

        assert!(
            fake.branch_tip(&store, &raw).is_some(),
            "the destroy failed, so the ref is still there"
        );
        assert!(
            registry.lookup(&store, &raw).unwrap().is_some(),
            "retracting over a ref that survived would disown it permanently"
        );
    }

    #[test]
    fn a_destroy_that_succeeded_retracts_the_receipt() {
        let (_tmp, primary, project, store) = fake_weave();
        let mut registry = RefRegistry::for_project(&primary, &project);
        let ephemeral = EphemeralRefName::mint(&project, &WorkweaveName::new("ww").unwrap());
        let raw = ephemeral.to_raw();
        registry
            .record_created(&store, ephemeral.clone(), fake_rev('a'))
            .unwrap();

        let fake = FakeVcs::new();
        fake.put_branch(&store, &raw, fake_rev('a'));
        fake.declare_ancestor(&fake_rev('a'), &fake_rev('b'));

        retire_recorded_refs(
            &fake,
            &mut registry,
            &store,
            &ephemeral,
            "github/org/repo",
            &[fake_rev('b')],
            None,
        );

        assert!(fake.branch_tip(&store, &raw).is_none());
        assert!(registry.lookup(&store, &raw).unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // Merge baselines: one directory must not appear twice
    //
    // `merge_baselines` returns the workspaces whose lineage counts as "this
    // work has landed", and `delete_workweave` names them in its refusal.
    // Whether the recorded parent IS the workspace already being appended is
    // a comparison between a value stored resolved and one handed in raw, so
    // it holds only if both sides are brought to the same spelling.
    // -----------------------------------------------------------------------

    /// A primary and a workweave whose marker records that primary as its
    /// parent. Returns `(tempdir, primary, workweave_dir)`.
    fn forked_from_primary() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let workweave_dir = tmp.path().join(".workweaves").join("web-app--feat");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&workweave_dir).unwrap();
        WorkweaveMarker::new(
            primary.clone(),
            ProjectName::new("web-app").unwrap(),
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

        let baselines = merge_baselines(&workweave_dir, &detoured);

        assert_eq!(
            baselines,
            vec![detoured],
            "the recorded parent IS this workspace, so it must not be listed a second \
             time under another spelling"
        );
    }

    #[test]
    fn merge_baselines_keeps_a_parent_that_is_a_different_workspace() {
        let (tmp, _primary, workweave_dir) = forked_from_primary();
        let elsewhere = tmp.path().join("other");
        std::fs::create_dir_all(&elsewhere).unwrap();

        let baselines = merge_baselines(&workweave_dir, &elsewhere);

        assert_eq!(
            baselines.len(),
            2,
            "a parent that is not this workspace is a baseline in its own right, got \
             {baselines:?}"
        );
    }
}
