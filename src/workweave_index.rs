//! The index of a project's workweaves: where each one lives, the workspace
//! it was forked from, and the directory `workweave create` places new ones
//! under.
//!
//! The index is kept with the project clone, in the directory the clone's
//! worktrees share (`Vcs::store_of`), so a workweave reaches it from its own
//! project checkout as the primary does and `git clean` in the primary's
//! project checkout cannot reach it. It is machine-local: nothing here is
//! committed or published.
//!
//! ```json
//! {
//!   "basedir": "/abs/path/to/.workweaves",
//!   "workweaves": {
//!     "hotfix": {
//!       "path": "/abs/path/to/.workweaves/myproj--hotfix",
//!       "parent": "/abs/path/to/primary"
//!     }
//!   }
//! }
//! ```
//!
//! `basedir` is present only once `workweave set-default-create-basedir` has
//! written it. An index at `projects/<project>/.rwv-workweave-index`, where
//! earlier releases kept it, is read by nothing but [`convert_legacy_index`]:
//! every other read refuses, naming the repair.
//!
//! ## Advisory, validated before use
//!
//! Every consumer that resolves an entry validates the recorded path against
//! the marker there and the worktree relation. A foreign or stale index
//! degrades to doctor findings, never to acting on wrong paths, and a
//! destructive verb refuses on any disagreement.
//!
//! ## Claimed writes, unclaimed reads
//!
//! Every write is a read-modify-write under [`IndexClaim`], published by
//! temp+rename, so a reader sees the old file or the new one and no writer
//! publishes a snapshot taken before a peer's landed. Reads take no claim. On
//! Windows a reader holding the file open makes the replacing rename fail
//! until it lets go, which the publish waits out.

use crate::manifest::{ProjectName, WorkweaveName};
use crate::path_spelling::CanonicalPath;
use crate::refusal::RefusalKind;
use crate::state_file::{StateFile, StatePath};
use crate::vcs::project_vcs;
use crate::workspace::{project_dir, WorkweaveMarker};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The index's file name in the project clone's shared directory.
pub(crate) const INDEX_FILENAME: &str = "rwv-workweave-index";

/// File name of the claim one rwv holds across an index read-modify-write, so
/// a second rwv cannot publish a snapshot it took before the first landed.
pub(crate) const INDEX_CLAIM_FILE: &str = "rwv-workweave-index.lock";

/// The `.rwv-workweave-index` file name an earlier release kept in the
/// project checkout.
pub(crate) const LEGACY_INDEX_FILENAME: &str = ".rwv-workweave-index";

/// The recorded workweave registry for one `(primary, project)` pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkweaveIndex {
    /// Where `workweave create` places new workweaves for this project when
    /// no `--dir` or `--basedir` is passed; unset until written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basedir: Option<CanonicalPath>,
    #[serde(default)]
    pub workweaves: BTreeMap<WorkweaveName, IndexedWorkweave>,
}

/// One recorded workweave: its directory and the workspace it was forked from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexedWorkweave {
    pub path: CanonicalPath,
    pub parent: CanonicalPath,
}

impl WorkweaveIndex {
    /// The recorded entry whose directory is `dir`.
    pub fn entry_at(&self, dir: &Path) -> Option<(&WorkweaveName, &IndexedWorkweave)> {
        self.workweaves
            .iter()
            .find(|(_, entry)| entry.path.same_directory(dir))
    }
}

/// How an index at the old path was written, which decides nothing about how
/// it converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyShape {
    /// No `receipts` key: written before that key was kept.
    BeforeReceipts,
    /// Carries `receipts`, empty or not: current as of the old location.
    OldPath,
}

/// An index at the old path, parsed in any shape an earlier release wrote.
#[derive(Debug, Deserialize)]
struct LegacyIndex {
    #[serde(default)]
    container: Option<CanonicalPath>,
    #[serde(default)]
    workweaves: BTreeMap<WorkweaveName, CanonicalPath>,
    #[serde(default)]
    receipts: Option<serde_json::Value>,
}

impl LegacyIndex {
    fn shape(&self) -> LegacyShape {
        if self.receipts.is_some() {
            LegacyShape::OldPath
        } else {
            LegacyShape::BeforeReceipts
        }
    }
}

/// Where the old-path index of `(primary_root, project)` sits.
pub fn legacy_index_path(primary_root: &Path, project: &ProjectName) -> StatePath {
    StateFile::LegacyWorkweaveIndex.in_dir(&project_dir(primary_root, project))
}

/// The directory the project's clone shares with its worktrees, when
/// `projects/<project>` is a clone in its own right.
///
/// A project directory that is not itself a repository resolves to no store:
/// the VCS answers for an enclosing repository, and the index of one project
/// must not land in another's.
/// Refuse where the store holding the workweave index of `project` makes
/// neither exclusive publish mode: what `rwv workweave create` runs before it
/// builds anything.
pub(crate) fn refuse_without_exclusive_publish(
    primary_root: &Path,
    project: &ProjectName,
) -> anyhow::Result<()> {
    match store_dir(primary_root, project) {
        Some(store) => crate::durable_file::refuse_without_exclusive_publish(&store),
        None => Ok(()),
    }
}

fn store_dir(primary_root: &Path, project: &ProjectName) -> Option<PathBuf> {
    let dir = project_dir(primary_root, project);
    let vcs = project_vcs();
    let clone = CanonicalPath::existing(&vcs.main_checkout(&dir)?).ok()?;
    clone
        .same_directory(&dir)
        .then(|| vcs.store_of(&dir))
        .flatten()
}

/// Where the index file for `(primary_root, project)` lives; `None` when the
/// project has no clone to keep it in.
pub fn index_path(primary_root: &Path, project: &ProjectName) -> Option<StatePath> {
    store_dir(primary_root, project).map(|store| StateFile::WorkweaveIndex.in_dir(&store))
}

fn claim_path(store: &Path) -> StatePath {
    StateFile::IndexClaim.in_dir(store)
}

fn no_store(primary_root: &Path, project: &ProjectName, doing: &str) -> anyhow::Error {
    let dir = project_dir(primary_root, project);
    crate::refusal::refusal(
        RefusalKind::ProjectDirMissing,
        format!(
            "cannot {doing}: {} is not a repository, so there is no clone to keep the \
             workweave index with",
            dir.display()
        ),
    )
}

/// The refusal an index at the old path earns every reader of the index.
fn legacy_index_refusal(primary_root: &Path, project: &ProjectName) -> anyhow::Error {
    let path = legacy_index_path(primary_root, project);
    crate::diagnosis::State::LegacyWorkweaveIndexOldPath
        .of(crate::workspace::project_rel_path(project))
        .listing(vec![path.to_string()])
        .judge(project_vcs().as_ref())
        .refused()
}

/// Refuse unless `projects/<project>` is a clone that can keep the index, so a
/// verb that will record a workweave learns so before it changes anything.
pub fn require_clone(primary_root: &Path, project: &ProjectName) -> anyhow::Result<()> {
    match store_dir(primary_root, project) {
        Some(_) => Ok(()),
        None => Err(no_store(primary_root, project, "record a workweave")),
    }
}

fn refuse_legacy(primary_root: &Path, project: &ProjectName) -> anyhow::Result<()> {
    if legacy_index_path(primary_root, project).exists() {
        return Err(legacy_index_refusal(primary_root, project));
    }
    Ok(())
}

/// Read the index file for `(primary_root, project)`.
///
/// `Ok(None)` when no workweave has been recorded yet. An index at the old
/// path refuses, whatever the file holds.
pub fn read(primary_root: &Path, project: &ProjectName) -> anyhow::Result<Option<WorkweaveIndex>> {
    refuse_legacy(primary_root, project)?;
    read_current(primary_root, project)
}

/// [`read`] without the old-path refusal, for the reader that reports the
/// state and the conversion that ends it.
pub(crate) fn read_current(
    primary_root: &Path,
    project: &ProjectName,
) -> anyhow::Result<Option<WorkweaveIndex>> {
    let Some(path) = index_path(primary_root, project) else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    let content = path
        .read_to_string()
        .with_context(|| format!("failed to read {path}"))?;
    match serde_json::from_str::<WorkweaveIndex>(&content) {
        Ok(index) => Ok(Some(index)),
        Err(e) => crate::refuse!(
            RefusalKind::UnreadableWorkweaveIndex,
            "the workweave index of project {project} at {path} does not read ({e:#}). This \
             rebuilds it from the workweaves on disk:\n  {rebuild}",
            rebuild = rebuild_command(primary_root),
        ),
    }
}

/// The verb that rebuilds an unreadable index of the primary at
/// `primary_root`.
pub fn rebuild_command(primary_root: &Path) -> String {
    format!("rwv -C {} doctor --fix", primary_root.display())
}

/// Replace the unreadable index of `(primary_root, project)` with one listing
/// `found`, each workweave with the parent its marker records. Nothing is
/// written where the index reads by the time the claim is held.
pub fn rebuild(
    primary_root: &Path,
    project: &ProjectName,
    found: &[(WorkweaveName, CanonicalPath, CanonicalPath)],
) -> anyhow::Result<bool> {
    IndexClaim::hold(primary_root, project, |claim| {
        match read_for_edit(claim) {
            Err(e)
                if crate::refusal::kind_of(&e) == Some(RefusalKind::UnreadableWorkweaveIndex) => {}
            _ => return Ok(false),
        }
        let index = WorkweaveIndex {
            basedir: None,
            workweaves: found
                .iter()
                .map(|(name, path, parent)| {
                    (
                        name.clone(),
                        IndexedWorkweave {
                            path: path.clone(),
                            parent: parent.clone(),
                        },
                    )
                })
                .collect(),
        };
        write(claim, &index)?;
        Ok(true)
    })
}

/// Atomically and **durably** write `index` for the pair `claim` covers.
///
/// This is one write path for the whole file, and it is the durable one (see
/// [`crate::durable_file`]).
pub fn write(claim: &IndexClaim, index: &WorkweaveIndex) -> anyhow::Result<()> {
    let path = StateFile::WorkweaveIndex.in_dir(&claim.store);
    let content =
        serde_json::to_string_pretty(index).context("failed to serialize workweave index")?;
    path.publish(content.as_bytes())
}

/// An exclusive claim on one project's index, released when it drops.
///
/// Two concurrent `rwv` processes creating workweaves in the same project
/// each read the index and write it back whole, so without this claim the
/// second write drops the first's entry. The lock on the claim file is
/// the exclusion, and it holds one winner across threads as well as across
/// processes.
///
/// A crash between acquisition and release leaves a file that the next
/// claimant takes over: the claim is a lock the kernel drops with the process.
#[derive(Debug)]
pub struct IndexClaim {
    primary_root: PathBuf,
    project: ProjectName,
    store: PathBuf,
    file: crate::durable_file::ClaimFile,
}

impl IndexClaim {
    /// Run `edit` holding `(primary_root, project)`'s index claim, waiting the
    /// shared claim budget (`durable_file::CLAIM_WAIT`) for a peer holding it,
    /// then refusing with the path to remove.
    ///
    /// An index at the old path refuses before the claim is taken. The claim
    /// is released before this returns, and a release that fails is an error
    /// naming the file left behind, whatever `edit` returned.
    pub fn hold<T>(
        primary_root: &Path,
        project: &ProjectName,
        edit: impl FnOnce(&Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        refuse_legacy(primary_root, project)?;
        Self::hold_over_legacy(primary_root, project, edit)
    }

    fn hold_over_legacy<T>(
        primary_root: &Path,
        project: &ProjectName,
        edit: impl FnOnce(&Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let claim = Self::acquire(primary_root, project)?;
        let outcome = edit(&claim);
        let claim_file = claim_path(&claim.store);
        let released = claim.file.release().with_context(|| {
            format!(
                "failed to remove the claim on the workweave index of project \
                 {project} at {root} at {claim_file}; the next write to it takes it over \
                 once this process has ended",
                root = primary_root.display(),
            )
        });
        crate::durable_file::after_release(outcome, released)
    }

    fn acquire(primary_root: &Path, project: &ProjectName) -> anyhow::Result<Self> {
        let Some(store) = store_dir(primary_root, project) else {
            return Err(no_store(primary_root, project, "claim the workweave index"));
        };
        let path = claim_path(&store);
        match path.claim() {
            Ok(file) => Ok(Self {
                primary_root: primary_root.to_path_buf(),
                project: project.clone(),
                store,
                file,
            }),
            Err(crate::durable_file::ClaimError::Held(held_by)) => {
                crate::refuse!(
                    RefusalKind::StateClaimHeld,
                    "another rwv still holds the workweave index of project \
                     {project} at {root} ({held_by}). {advice}",
                    root = primary_root.display(),
                    advice = crate::durable_file::held_advice(&path.reported()),
                );
            }
            Err(crate::durable_file::ClaimError::NoExclusivePublish(e)) => Err(e),
            Err(crate::durable_file::ClaimError::Io(e)) => Err(anyhow::Error::new(e)
                .context(format!("failed to claim the workweave index at {path}"))),
        }
    }
}

/// The default basedir for a primary workspace: `<parent-of-root>/.workweaves`.
///
/// Callers resolving where a create places a workweave go through
/// [`resolve_basedir`], so a recorded basedir wins.
pub fn default_basedir(primary_root: &Path) -> PathBuf {
    primary_root
        .parent()
        .expect("weave root should have a parent")
        .join(".workweaves")
}

/// Resolve the effective basedir for new workweaves in `(primary_root, project)`.
///
/// The index's `basedir` when one was written, else [`default_basedir`].
pub fn resolve_basedir(
    primary_root: &Path,
    project: &ProjectName,
) -> anyhow::Result<CanonicalPath> {
    if let Some(basedir) = read(primary_root, project)?.and_then(|index| index.basedir) {
        return Ok(basedir);
    }
    Ok(CanonicalPath::of(&default_basedir(primary_root)))
}

/// Read the index of the `(primary_root, project)` pair `claim` covers, for a
/// read-modify-write.
///
/// ```no_run
/// use repoweave::manifest::ProjectName;
/// use repoweave::workweave_index::{read_for_edit, IndexClaim};
/// use std::path::Path;
/// let project = ProjectName::new("p").unwrap();
/// let _index = IndexClaim::hold(Path::new("/ws"), &project, read_for_edit).unwrap();
/// ```
///
/// ```compile_fail
/// use repoweave::manifest::ProjectName;
/// use repoweave::workweave_index::read_for_edit;
/// use std::path::Path;
/// let project = ProjectName::new("p").unwrap();
/// let _index = read_for_edit(Path::new("/ws"), &project);
/// ```
///
/// ```compile_fail
/// use repoweave::manifest::ProjectName;
/// use repoweave::workweave_index::{read_for_edit, IndexClaim};
/// use std::path::PathBuf;
/// let project = ProjectName::new("p").unwrap();
/// let forged = IndexClaim {
///     primary_root: PathBuf::from("/ws"),
///     project,
/// };
/// let _index = read_for_edit(&forged);
/// ```
pub fn read_for_edit(claim: &IndexClaim) -> anyhow::Result<Option<WorkweaveIndex>> {
    read_current(&claim.primary_root, &claim.project)
}

/// Set the basedir in the index for `(primary_root, project)`, creating the
/// index file if it did not exist.
///
/// The recorded entries are preserved, and the recorded basedir is returned.
pub fn set_basedir(
    primary_root: &Path,
    project: &ProjectName,
    basedir: CanonicalPath,
) -> anyhow::Result<CanonicalPath> {
    IndexClaim::hold(primary_root, project, |claim| {
        let mut index = read_for_edit(claim)?.unwrap_or_default();
        index.basedir = Some(basedir.clone());
        write(claim, &index)
    })?;
    Ok(basedir)
}

/// Record a workweave entry `name → path`, forked from `parent`, in the index
/// for `(primary_root, project)`, creating the index if it does not exist.
///
/// Read-modify-write under the index claim.
pub fn record_workweave(
    primary_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    path: CanonicalPath,
    parent: CanonicalPath,
) -> anyhow::Result<()> {
    IndexClaim::hold(primary_root, project, |claim| {
        let mut index = read_for_edit(claim)?.unwrap_or_default();
        index
            .workweaves
            .insert(name.clone(), IndexedWorkweave { path, parent });
        write(claim, &index)
    })
}

/// Point the recorded parent of the workweave at `dir` at `parent`. No-op
/// when no entry records `dir`.
pub fn set_parent_of(
    primary_root: &Path,
    project: &ProjectName,
    dir: &Path,
    parent: CanonicalPath,
) -> anyhow::Result<()> {
    IndexClaim::hold(primary_root, project, |claim| {
        let Some(mut index) = read_for_edit(claim)? else {
            return Ok(());
        };
        let Some(name) = index.entry_at(dir).map(|(name, _)| name.clone()) else {
            return Ok(());
        };
        let entry = index
            .workweaves
            .get_mut(&name)
            .expect("the name was just read from this index");
        if entry.parent == parent {
            return Ok(());
        }
        entry.parent = parent;
        write(claim, &index)
    })
}

/// Remove a workweave entry from the index. No-op if the entry (or the index
/// file) does not exist.
///
/// Idempotent: a delete that races with another writer's insert may leave
/// the entry, but doctor will prune it on the next round (marker
/// round-trip against the missing directory).
pub fn forget_workweave(
    primary_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
) -> anyhow::Result<()> {
    IndexClaim::hold(primary_root, project, |claim| {
        let mut index = match read_for_edit(claim)? {
            Some(idx) => idx,
            None => return Ok(()),
        };
        if index.workweaves.remove(name).is_none() {
            return Ok(());
        }
        write(claim, &index)
    })
}

/// Look up the recorded path for a workweave without any marker validation.
///
/// Callers that consume the path (list rendering, destructive ops) MUST
/// validate the marker round-trip via [`crate::workweave::validate_registry_entry`]
/// before acting on the path. This helper is a raw registry read; validation
/// is a separate step so tests can exercise the invalid-entry paths.
pub fn lookup_raw(
    primary_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
) -> anyhow::Result<Option<CanonicalPath>> {
    Ok(read(primary_root, project)?
        .and_then(|idx| idx.workweaves.get(name).map(|e| e.path.clone())))
}

/// The shape of the index at the old path, or `None` when there is none.
///
/// A file that does not parse is an error: nothing converts it.
pub fn legacy_shape(
    primary_root: &Path,
    project: &ProjectName,
) -> anyhow::Result<Option<(StatePath, LegacyShape)>> {
    let path = legacy_index_path(primary_root, project);
    if !path.exists() {
        return Ok(None);
    }
    let legacy = parse_legacy(&path)?;
    Ok(Some((path, legacy.shape())))
}

fn parse_legacy(path: &StatePath) -> anyhow::Result<LegacyIndex> {
    let content = path
        .read_to_string()
        .with_context(|| format!("failed to read {path}"))?;
    serde_json::from_str(&content).with_context(|| format!("failed to parse {path}"))
}

/// What converting an old-path index did.
#[derive(Debug)]
pub struct Converted {
    pub from: StatePath,
    pub to: StatePath,
    pub workweaves: usize,
}

/// Convert the index at the old path: `container` becomes `basedir`, each
/// workweave gains the parent its marker records (the primary where the
/// marker cannot be read), `receipts` is dropped, and the file moves to the
/// project clone's shared directory.
///
/// An index already at the new path keeps its own `basedir` and entries; the
/// old file adds what it lacks. The old file is removed only after the new
/// one is durable, so a crash leaves a conversion that re-runs. `None` when
/// there is no old-path index.
pub fn convert_legacy_index(
    primary_root: &Path,
    project: &ProjectName,
) -> anyhow::Result<Option<Converted>> {
    let from = legacy_index_path(primary_root, project);
    if !from.exists() {
        return Ok(None);
    }
    let legacy = parse_legacy(&from)?;
    IndexClaim::hold_over_legacy(primary_root, project, |claim| {
        let mut index = read_for_edit(claim)?.unwrap_or_default();
        if index.basedir.is_none() {
            index.basedir = legacy.container.clone();
        }
        for (name, path) in &legacy.workweaves {
            let parent = WorkweaveMarker::read(path.as_path())
                .ok()
                .flatten()
                .map(|marker| marker.parent().clone())
                .unwrap_or_else(|| CanonicalPath::of(primary_root));
            index
                .workweaves
                .entry(name.clone())
                .or_insert_with(|| IndexedWorkweave {
                    path: path.clone(),
                    parent,
                });
        }
        write(claim, &index)?;
        from.remove()
            .with_context(|| format!("failed to remove {from} after converting it"))?;
        Ok(Some(Converted {
            from: from.clone(),
            to: StateFile::WorkweaveIndex.in_dir(&claim.store),
            workweaves: index.workweaves.len(),
        }))
    })
}

/// Ensure `filename` appears in the ignore surface of the git repo rooted at
/// `dir` (or, if `dir` is not a repo root, in a `.gitignore` next to it as a
/// best-effort fallback).
///
/// `dir` is checked for `.git` directly — there is no upward walk to an
/// enclosing repo. Every caller writes its machine-local file next to a
/// canonical file whose directory is either a checkout root or a plain
/// directory, so the two branches below cover the real cases.
///
/// Two candidate targets, prioritized for zero shared-repo footprint:
///
/// 1. `.git/info/exclude` — per-clone, invisible, never touches the
///    working tree, so it does not perturb any dirty-tree check running
///    concurrently. Preferred when `dir` is a repo root.
/// 2. `.gitignore` — fallback for non-git directories. Committed
///    alongside the project, at the cost of adding an rwv-specific entry.
///
/// Silent no-op when `dir` does not exist.
/// Best effort throughout: an I/O error on the ignore surface must never
/// fail the caller's write.
pub(crate) fn ensure_ignored_in_dir(dir: &Path, filename: &str) -> anyhow::Result<()> {
    if !dir.exists() {
        // Not our failure — caller writes into non-existent dirs are caught
        // elsewhere; here we just stay quiet.
        return Ok(());
    }
    // Prefer `.git/info/exclude` when the directory is a repo root.
    if let Some(info_dir) = git_info_dir(dir) {
        let exclude = info_dir.join("exclude");
        return append_ignore_line(&exclude, filename);
    }
    // Fall back to a `.gitignore` next to the file.
    let gitignore = dir.join(".gitignore");
    append_ignore_line(&gitignore, filename)
}

/// Resolve the `.git/info/` directory for the repo rooted at `dir`.
///
/// Handles a plain-`.git`-dir repo, a linked worktree (`.git` file pointing
/// at the actual gitdir), and follows `commondir` so the per-repo (not
/// per-worktree) exclude file is used.
///
/// Returns `None` when `dir` itself is not the root of a git-managed
/// checkout — no upward walk is attempted.
fn git_info_dir(dir: &Path) -> Option<PathBuf> {
    let store = match crate::git::git_dir_link(dir)? {
        crate::git::GitDirLink::Owned(git_dir) => git_dir,
        // A linked worktree has an `info/` of its own, but the exclude file
        // worth writing is the one every worktree of the store shares. Absent
        // a `commondir` the git dir is a store in its own right
        // (`--separate-git-dir`) and already is the shared one.
        crate::git::GitDirLink::Linked(git_dir) => {
            crate::git::commondir_target(&git_dir).unwrap_or(git_dir)
        }
    };
    let info = store.join("info");
    std::fs::create_dir_all(&info).ok()?;
    Some(info)
}

/// Append `filename` to the ignore file at `target` if it is not already present.
///
/// Opened in append mode deliberately: one surface serves writers holding
/// different claims — the index's and the ledger's, both records of the same
/// project directory — so a whole-file publish here can drop the line the
/// other writer added between this writer's read and its write. An append
/// cannot lose a peer's line; the worst concurrent outcome is a duplicate,
/// which the presence check keeps from recurring.
fn append_ignore_line(target: &Path, filename: &str) -> anyhow::Result<()> {
    let existing = std::fs::read_to_string(target).unwrap_or_default();
    let needle = filename;
    let already_present = existing
        .lines()
        .map(str::trim)
        .any(|line| line == needle || line == format!("/{needle}"));
    if already_present {
        return Ok(());
    }
    let mut addition = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        addition.push('\n');
    }
    addition.push_str(needle);
    addition.push('\n');
    // Ensure parent (for `.git/info/`) exists — best-effort; append_ignore_line
    // may be called with a `.gitignore` whose parent always exists.
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(target)
        .and_then(|mut file| file.write_all(addition.as_bytes()))
        .with_context(|| format!("failed to update {}", target.display()))?;
    Ok(())
}

/// A project directory under `primary_root` that is a repository, so the
/// project has a clone to keep its index with.
#[cfg(test)]
pub(crate) fn init_project_clone(primary_root: &Path, project: &ProjectName) {
    let dir = project_dir(primary_root, project);
    std::fs::create_dir_all(&dir).unwrap();
    project_vcs().init_repo(&dir).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::WorkweaveName;

    fn make_project(primary: &Path, name: &str) -> ProjectName {
        let project = ProjectName::new(name).unwrap();
        init_project_clone(primary, &project);
        project
    }

    fn name(text: &str) -> WorkweaveName {
        WorkweaveName::new(text).unwrap()
    }

    fn at(path: &str) -> CanonicalPath {
        CanonicalPath::of(Path::new(path))
    }

    fn record(primary: &Path, project: &ProjectName, ww: &str) {
        record_workweave(
            primary,
            project,
            &name(ww),
            at(&format!("/c/web-app--{ww}")),
            CanonicalPath::of(primary),
        )
        .unwrap();
    }

    fn claim_file(primary: &Path, project: &ProjectName) -> StatePath {
        claim_path(&store_dir(primary, project).unwrap())
    }

    fn new_index_file(primary: &Path, project: &ProjectName) -> StatePath {
        index_path(primary, project).unwrap()
    }

    fn write_old_path(primary: &Path, project: &ProjectName, content: &[u8]) {
        legacy_index_path(primary, project)
            .publish(content)
            .unwrap();
    }

    fn refusal_kind(err: &anyhow::Error) -> Option<String> {
        crate::diagnosis::report_of(err).map(|report| {
            report
                .kinds()
                .iter()
                .map(|axis| axis.token())
                .collect::<Vec<_>>()
                .join(",")
        })
    }

    #[test]
    fn the_index_is_kept_in_the_clones_shared_dir_and_never_written_at_the_old_path() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record(&primary, &project, "a");
        set_basedir(&primary, &project, at("/b")).unwrap();
        forget_workweave(&primary, &project, &name("a")).unwrap();

        let store = primary.join("projects/web-app/.git");
        assert!(
            store.join("rwv-workweave-index").is_file(),
            "the index must sit in the clone's shared dir"
        );
        assert!(
            !legacy_index_path(&primary, &project).exists(),
            "no write may land at the path earlier releases used"
        );
        let in_project: Vec<_> = std::fs::read_dir(primary.join("projects/web-app"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("workweave-index"))
            .collect();
        assert!(
            in_project.is_empty(),
            "the project checkout holds no index file: {in_project:?}"
        );
    }

    #[test]
    fn a_worktree_of_the_clone_reaches_the_same_index() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        record(&primary, &project, "a");

        let vcs = project_vcs();
        let clone = primary.join("projects/web-app");
        let linked = tmp.path().join("linked");
        let sha_path = clone.join("f");
        std::fs::write(&sha_path, "x").unwrap();
        let run = |args: &[&str]| {
            let status = crate::git::git_command_in_test()
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(&clone)
                .status()
                .unwrap();
            assert!(status.success());
        };
        run(&["add", "f"]);
        run(&["commit", "-qm", "c"]);
        run(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ]);

        let from_linked = vcs.store_of(&linked).unwrap();
        assert_eq!(
            CanonicalPath::existing(&from_linked).unwrap(),
            CanonicalPath::existing(&clone.join(".git")).unwrap(),
            "a worktree's shared dir is the clone's own"
        );
    }

    #[test]
    fn a_clone_whose_store_sits_apart_from_it_keeps_the_index() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let clone = primary.join("projects/web-app");
        std::fs::create_dir_all(&clone).unwrap();
        let store = tmp.path().join("web-app.git");
        let status = crate::git::git_command_in_test()
            .args(["init", "-q", "--separate-git-dir"])
            .arg(&store)
            .arg(&clone)
            .status()
            .unwrap();
        assert!(status.success());
        let project = ProjectName::new("web-app").unwrap();

        require_clone(&primary, &project).unwrap();
        record(&primary, &project, "a");
        assert!(read(&primary, &project).unwrap().is_some());
        assert!(
            StateFile::WorkweaveIndex.in_dir(&store).exists(),
            "the index is kept in the clone's store, wherever that sits"
        );
    }

    #[test]
    fn each_workweave_records_its_parent() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(
            &primary,
            &project,
            &name("child"),
            at("/c/web-app--child"),
            at("/c/web-app--parent"),
        )
        .unwrap();

        let index = read(&primary, &project).unwrap().unwrap();
        assert_eq!(
            index.workweaves.get(&name("child")).unwrap().parent,
            at("/c/web-app--parent")
        );
        let written: serde_json::Value =
            serde_json::from_str(&new_index_file(&primary, &project).read_to_string().unwrap())
                .unwrap();
        assert_eq!(
            written["workweaves"]["child"]["parent"],
            serde_json::json!("/c/web-app--parent")
        );
        assert!(written.get("receipts").is_none());
    }

    #[test]
    fn a_parent_is_repointed_by_the_directory_it_belongs_to() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        record(&primary, &project, "a");

        set_parent_of(
            &primary,
            &project,
            Path::new("/c/web-app--a"),
            at("/elsewhere"),
        )
        .unwrap();

        let index = read(&primary, &project).unwrap().unwrap();
        assert_eq!(
            index.workweaves.get(&name("a")).unwrap().parent,
            at("/elsewhere")
        );
    }

    #[test]
    fn the_basedir_is_unset_until_written_and_then_recorded() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        assert_eq!(
            resolve_basedir(&primary, &project).unwrap(),
            CanonicalPath::of(&primary.parent().unwrap().join(".workweaves"))
        );
        record(&primary, &project, "a");
        let written: serde_json::Value =
            serde_json::from_str(&new_index_file(&primary, &project).read_to_string().unwrap())
                .unwrap();
        assert!(
            written.get("basedir").is_none(),
            "recording a workweave must not write the default into the index: {written}"
        );

        let recorded = tmp.path().join("recorded");
        std::fs::create_dir_all(&recorded).unwrap();
        set_basedir(&primary, &project, CanonicalPath::of(&recorded)).unwrap();
        assert_eq!(
            resolve_basedir(&primary, &project).unwrap(),
            CanonicalPath::of(&recorded)
        );
        assert!(
            read(&primary, &project)
                .unwrap()
                .unwrap()
                .workweaves
                .contains_key(&name("a")),
            "setting the basedir keeps the entries"
        );
    }

    #[test]
    fn a_project_directory_that_is_no_clone_of_its_own_has_no_index() {
        let tmp = crate::tempdir().unwrap();
        let enclosing = tmp.path().join("enclosing");
        std::fs::create_dir_all(&enclosing).unwrap();
        project_vcs().init_repo(&enclosing).unwrap();
        let primary = enclosing.join("ws");
        let project = ProjectName::new("web-app").unwrap();
        std::fs::create_dir_all(project_dir(&primary, &project)).unwrap();

        assert!(index_path(&primary, &project).is_none());
        assert!(read(&primary, &project).unwrap().is_none());
        let err = record_workweave(
            &primary,
            &project,
            &name("a"),
            at("/c/a"),
            CanonicalPath::of(&primary),
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("not a repository"),
            "the refusal must say there is no clone: {err:#}"
        );
        assert!(
            !enclosing.join(".git/rwv-workweave-index").exists(),
            "an enclosing repository must not be given this project's index"
        );
    }

    #[test]
    fn an_old_path_index_refuses_every_reader_before_anything_changes() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let content = br#"{"container":"/abs/container","workweaves":{"hotfix":"/abs/container/web-app--hotfix"},"receipts":[]}"#;
        write_old_path(&primary, &project, content);

        let refusals = [
            read(&primary, &project).map(|_| ()),
            lookup_raw(&primary, &project, &name("hotfix")).map(|_| ()),
            resolve_basedir(&primary, &project).map(|_| ()),
            record_workweave(&primary, &project, &name("b"), at("/c/b"), at("/p")),
            set_basedir(&primary, &project, at("/d")).map(|_| ()),
            forget_workweave(&primary, &project, &name("hotfix")),
            set_parent_of(&primary, &project, Path::new("/x"), at("/p")),
        ];
        for result in refusals {
            let err = result.expect_err("an old-path index must refuse");
            assert_eq!(
                refusal_kind(&err).as_deref(),
                Some("legacy-workweave-index"),
                "{err:#}"
            );
            assert!(
                format!("{err:#}").contains("rwv doctor --fix"),
                "the refusal must name the repair: {err:#}"
            );
        }
        assert_eq!(
            legacy_index_path(&primary, &project).read().unwrap(),
            content,
            "a refused reader leaves the old file as it was"
        );
        assert!(
            !new_index_file(&primary, &project).exists(),
            "and writes no index at the new path"
        );
        assert!(
            !claim_file(&primary, &project).exists(),
            "and takes no claim"
        );
    }

    #[test]
    fn converting_an_old_path_index_renames_container_drops_receipts_and_moves_the_file() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let boxed = tmp.path().join(".workweaves").join("web-app--boxed");
        let elsewhere = tmp.path().join("elsewhere");
        for ww in [&boxed, &elsewhere] {
            std::fs::create_dir_all(ww).unwrap();
        }
        let parent = tmp.path().join("web-app--parent");
        WorkweaveMarker::new(&primary, project.clone(), &parent)
            .write(&boxed)
            .unwrap();
        let content = format!(
            r#"{{"container":{:?},"workweaves":{{"boxed":{:?},"dir":{:?}}},"receipts":[{{"store":"/s","name":"web-app--boxed","created_at":"{}"}}]}}"#,
            tmp.path().join(".workweaves").display().to_string(),
            boxed.display().to_string(),
            elsewhere.display().to_string(),
            "a".repeat(40)
        );
        write_old_path(&primary, &project, content.as_bytes());

        let converted = convert_legacy_index(&primary, &project).unwrap().unwrap();
        assert_eq!(converted.workweaves, 2);

        assert!(
            !legacy_index_path(&primary, &project).exists(),
            "the old file is moved, not copied"
        );
        let index = read(&primary, &project).unwrap().unwrap();
        assert_eq!(
            index.basedir,
            Some(CanonicalPath::of(&tmp.path().join(".workweaves"))),
            "`container` becomes `basedir` with its value"
        );
        assert_eq!(
            index.workweaves.get(&name("boxed")).unwrap().parent,
            CanonicalPath::of(&parent),
            "the parent comes from the marker the workweave carries"
        );
        let unmarked = index.workweaves.get(&name("dir")).unwrap();
        assert_eq!(unmarked.path, CanonicalPath::of(&elsewhere));
        assert_eq!(
            unmarked.parent,
            CanonicalPath::of(&primary),
            "a workweave whose marker cannot be read has the primary for a parent"
        );
        let written = new_index_file(&primary, &project).read_to_string().unwrap();
        assert!(
            !written.contains("receipts") && !written.contains("container"),
            "the converted file carries neither: {written}"
        );
        assert!(convert_legacy_index(&primary, &project).unwrap().is_none());
    }

    #[test]
    fn an_old_path_index_without_receipts_converts_the_same_way() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        write_old_path(
            &primary,
            &project,
            br#"{"container":"/abs/container","workweaves":{"hotfix":"/abs/container/web-app--hotfix"}}"#,
        );
        assert_eq!(
            legacy_shape(&primary, &project).unwrap().unwrap().1,
            LegacyShape::BeforeReceipts
        );

        convert_legacy_index(&primary, &project).unwrap().unwrap();

        let index = read(&primary, &project).unwrap().unwrap();
        assert_eq!(index.basedir, Some(at("/abs/container")));
        assert!(index.workweaves.contains_key(&name("hotfix")));
    }

    #[test]
    fn converting_keeps_what_an_index_already_at_the_new_path_holds() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        set_basedir(&primary, &project, at("/kept")).unwrap();
        record(&primary, &project, "a");
        write_old_path(
            &primary,
            &project,
            br#"{"container":"/dropped","workweaves":{"a":"/old/a","b":"/old/b"},"receipts":[]}"#,
        );

        // The new-path index is read under the old-path refusal, so the
        // conversion is the only reader that sees both.
        convert_legacy_index(&primary, &project).unwrap().unwrap();

        let index = read(&primary, &project).unwrap().unwrap();
        assert_eq!(index.basedir, Some(at("/kept")));
        assert_eq!(
            index.workweaves.get(&name("a")).unwrap().path,
            at("/c/web-app--a"),
            "an entry already recorded is not overwritten"
        );
        assert!(index.workweaves.contains_key(&name("b")));
    }

    #[test]
    fn an_old_path_file_that_does_not_parse_is_not_converted() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        write_old_path(&primary, &project, b"{ this is not an index");

        assert!(legacy_shape(&primary, &project).is_err());
        assert!(convert_legacy_index(&primary, &project).is_err());
        assert!(
            legacy_index_path(&primary, &project).exists(),
            "a file nothing could read is left for the operator"
        );
        assert!(!new_index_file(&primary, &project).exists());
    }

    /// A finished write holds the claim only while it runs. Left behind, it
    /// is the wedge every later write to this index would hit.
    #[test]
    fn a_finished_write_leaves_no_claim_behind() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record(&primary, &project, "hotfix");
        assert!(
            !claim_file(&primary, &project).exists(),
            "the claim must be released when the read-modify-write ends"
        );
        forget_workweave(&primary, &project, &name("never-recorded")).unwrap();
        assert!(
            !claim_file(&primary, &project).exists(),
            "an edit that changes nothing must still release its claim"
        );
    }

    /// A claim that cannot be removed refuses every later write to this
    /// index, so the edit that left it behind must say so — even though the
    /// edit itself succeeded.
    #[test]
    #[cfg(unix)]
    fn a_release_that_keeps_failing_is_reported_with_the_file_left_behind() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = store_dir(&primary, &project).unwrap();
        let writable = std::fs::metadata(&store).unwrap().permissions();

        let result = IndexClaim::hold(&primary, &project, |_| {
            std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555))?;
            Ok(())
        });
        std::fs::set_permissions(&store, writable).unwrap();

        let message = format!(
            "{:#}",
            result.expect_err("a release that failed must not be reported as success")
        );
        assert!(
            message.contains("rwv-workweave-index.lock") && message.contains("takes it over"),
            "the error must name the claim left behind and what clears it: {message}"
        );
        assert!(
            claim_file(&primary, &project).exists(),
            "the release failed, so the claim is still there to report"
        );
    }

    /// A claim file whose holder died is taken over by the next write at once.
    #[test]
    fn an_abandoned_claim_is_taken_over_by_the_next_write() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let claim = claim_file(&primary, &project);
        claim.create(b"pid 999999 host elsewhere\n").unwrap();

        let started = std::time::Instant::now();
        record_workweave(
            &primary,
            &project,
            &name("hotfix"),
            at("/c/web-app--hotfix"),
            CanonicalPath::of(&primary),
        )
        .expect("a claim nobody holds must not refuse the write");

        assert!(started.elapsed() < crate::durable_file::CLAIM_WAIT);
        assert!(!claim.exists());
        assert!(read(&primary, &project).unwrap().is_some());
    }

    /// A claim a live process holds refuses later writes after a bounded
    /// wait, naming its holder and never a file to delete. Waiting forever
    /// hangs the verb; writing over a holder that is merely slow is the lost
    /// entry this claim exists to prevent; failing instantly makes ordinary
    /// contention look like a fault.
    #[test]
    fn a_held_claim_refuses_later_writes_after_the_wait() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let claim = claim_file(&primary, &project);
        let held = crate::durable_file::claim(&claim.reported()).unwrap();

        let started = std::time::Instant::now();
        let err = record_workweave(
            &primary,
            &project,
            &name("hotfix"),
            at("/c/web-app--hotfix"),
            CanonicalPath::of(&primary),
        )
        .expect_err("a held claim must refuse the write, not drop it silently");
        let waited = started.elapsed();
        let message = format!("{err:#}");

        assert!(
            waited >= crate::durable_file::CLAIM_WAIT,
            "the refusal must come after the full wait, so ordinary contention \
             is not reported as a fault: waited {waited:?}"
        );
        assert!(message.contains("pid "), "{message}");
        assert!(!message.contains("delete"), "{message}");
        assert!(
            claim.exists(),
            "a refused write must leave the holder's claim alone"
        );
        assert!(
            read(&primary, &project).unwrap().is_none(),
            "and must not have written the entry it refused to record"
        );
        held.release().unwrap();
    }

    #[test]
    fn read_missing_returns_none() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        assert!(read(&primary, &project).unwrap().is_none());
    }

    #[test]
    fn forget_workweave_removes_entry() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record(&primary, &project, "a");
        record(&primary, &project, "b");
        forget_workweave(&primary, &project, &name("a")).unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        assert!(!got.workweaves.contains_key(&name("a")));
        assert!(got.workweaves.contains_key(&name("b")));
    }

    #[test]
    fn forget_workweave_noop_when_missing() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        forget_workweave(&primary, &project, &name("nonexistent")).unwrap();
    }

    /// Both spellings are written out here rather than computed, and that is
    /// the point: every other assertion in this file that names
    /// `CanonicalPath::of` builds its expected value by calling it, so each of
    /// those holds whatever the function does — including nothing. The
    /// missing-tail cases are the arm the basedir takes before anyone
    /// creates it, and they resolve an ancestor no `canonicalize` of the whole
    /// path can reach.
    #[test]
    fn a_recorded_path_resolves_a_symlinked_spelling() {
        let tmp = crate::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        crate::symlink::create(&real, &link, crate::symlink::LinkTarget::Directory).unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "fixture precondition: `link` must be a symlink, or neither spelling differs"
        );

        let resolved = CanonicalPath::existing(&real).unwrap();
        assert_eq!(CanonicalPath::of(&link), resolved);
        assert_eq!(
            CanonicalPath::of(&link.join("not-created-yet")).as_path(),
            resolved.as_path().join("not-created-yet")
        );
        assert_eq!(
            CanonicalPath::of(&link.join("not").join("created")).as_path(),
            resolved.as_path().join("not").join("created")
        );
    }

    /// Concurrent creators each add their own entry; every entry has to be
    /// there afterwards.
    #[test]
    fn concurrent_writers_lose_nothing() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        std::thread::scope(|scope| {
            for i in 0..8 {
                let (project, primary) = (&project, &primary);
                scope.spawn(move || record(primary, project, &format!("ww{i}")));
            }
        });

        let index = read(&primary, &project).unwrap().unwrap();
        assert_eq!(index.workweaves.len(), 8, "an entry was dropped");
    }

    /// Atomicity, pinned by the mechanism rather than by hoping: a
    /// successful write replaces the file by `rename(2)`, so the target's
    /// inode changes and no reader can ever have seen a partial one. An
    /// in-place rewrite would keep the inode — and would be observable
    /// half-written.
    ///
    /// Gated because the INSTRUMENT is unverified: this proves the replacement by
    /// watching the inode change, and whether NTFS gives a renamed-over file a new
    /// file index is not something this repository can check. Ported on the
    /// assumption that it does, it would go red against correct code.
    #[test]
    #[cfg(unix)]
    fn an_index_write_replaces_the_file_by_rename() {
        use std::os::unix::fs::MetadataExt;

        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let path = new_index_file(&primary, &project);

        record(&primary, &project, "a");
        let first_inode = path.symlink_metadata().unwrap().ino();

        record(&primary, &project, "b");
        assert_ne!(
            path.symlink_metadata().unwrap().ino(),
            first_inode,
            "the index must be replaced by rename, not rewritten in place"
        );

        let leftovers: Vec<_> = std::fs::read_dir(path.dir())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    /// A crash between the temp write and the rename leaves the previous
    /// index intact plus an orphan temp. Neither may be mistaken for the
    /// index, and the next write must still land.
    #[test]
    fn an_orphan_temp_from_a_crashed_write_changes_nothing() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record(&primary, &project, "hotfix");

        let orphan = new_index_file(&primary, &project)
            .dir()
            .join("rwv-workweave-index.tmp.999999.0");
        std::fs::write(&orphan, "{ this is a half-written file").unwrap();

        assert!(read(&primary, &project)
            .unwrap()
            .unwrap()
            .workweaves
            .contains_key(&name("hotfix")));
        record(&primary, &project, "second");
        assert_eq!(
            read(&primary, &project).unwrap().unwrap().workweaves.len(),
            2
        );
        assert!(orphan.exists(), "the orphan is not this write's to reap");
    }
}
