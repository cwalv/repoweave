//! `projects/<name>/.rwv-workweave-index` — recorded workweave placement and discovery.
//!
//! The registry inverts the workweave marker: each workweave's `.rwv-workweave`
//! marker records `(primary, project, parent)` — telling the workweave where
//! the primary is. The primary-side index records the reverse — for a given
//! `(primary, project)`, where its workweaves live on disk, plus the container
//! directory `workweave create` places new workweaves under by default.
//!
//! ## Location
//!
//! Canonical copy at the primary's project checkout only:
//!
//! ```text
//! <primary>/projects/<project>/.rwv-workweave-index
//! ```
//!
//! Dotted per the machine-local convention (`.rwv-active`, `.rwv-workweave`).
//! Named `-index` to stay more than one character from the `.rwv-workweave`
//! marker: a singular/plural pair would be a confusability trap.
//!
//! ## Format
//!
//! Machine-written JSON via serde. Format chosen per the format-by-audience
//! convention (JSON: simple, has an atomic-write model, well-served by
//! `serde_json`). Reads route to the primary; workweave-side copies (which can
//! only arise from someone committing the file) are never consulted.
//!
//! ```json
//! {
//!   "container": "/abs/path/to/.workweaves",
//!   "workweaves": {
//!     "hotfix": "/abs/path/to/.workweaves/myproj--hotfix"
//!   },
//!   "receipts": []
//! }
//! ```
//!
//! `receipts` is always written empty. An index an earlier release wrote may
//! carry entries there; they are read past and never acted on, and the next
//! write drops them. An index with no `receipts` key at all is a legacy
//! index: `workweave create` refuses against it, and `rwv doctor --fix`
//! gives it the key.
//!
//! ## Advisory, validated before use
//!
//! The index is an advisory inverted index. Every consumer that resolves an
//! entry validates the recorded path carries a `.rwv-workweave` marker whose
//! `primary` canonicalizes to this primary and whose `project` matches. A
//! foreign or stale registry degrades to doctor findings (prune / adopt /
//! flag-tracked), never to acting on wrong paths.
//!
//! Destructive ops hard-require the marker round-trip before touching the
//! directory — a foreign registry cannot direct a deletion at the wrong tree.
//!
//! ## Claimed writes, unclaimed reads
//!
//! Two `workweave create` invocations from sibling workweaves race on the
//! primary's shared index. Every write is a read-modify-write under
//! [`IndexClaim`], published by temp+rename, so a reader sees the old file or
//! the new one and no writer publishes a snapshot taken before a peer's
//! landed. Reads take no claim, as git's readers take no `index.lock`. On
//! Windows a reader holding the file open makes the replacing rename fail
//! until it lets go, which the publish waits out.

use crate::manifest::{ProjectName, WorkweaveName};
use crate::path_spelling::CanonicalPath;
use crate::refusal::RefusalKind;
use crate::state_file::{StateFile, StatePath};
use crate::workspace::project_dir;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The `.rwv-workweave-index` file name (dotted, machine-local).
pub(crate) const INDEX_FILENAME: &str = ".rwv-workweave-index";

/// File name of the claim one rwv holds across an index read-modify-write, so
/// a second rwv cannot publish a snapshot it took before the first landed.
pub(crate) const INDEX_CLAIM_FILE: &str = ".rwv-workweave-index.lock";

/// The recorded workweave registry for one `(primary, project)` pair.
///
/// `container` — where `workweave create` places new workweaves for this
/// project when no per-workweave override is passed. Absolute path.
///
/// `workweaves` — the recorded name → absolute-path index. The path is the
/// full workweave directory (e.g. `<container>/<project>--<name>`), stored
/// absolute so that per-workweave placement overrides (a `--dir` on create)
/// remain resolvable without re-consulting the container.
///
/// `receipts` — present and empty in every index this build writes; absent in
/// a legacy index, which is a distinct state that
/// [`WorkweaveIndex::is_legacy`] reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkweaveIndex {
    /// Absolute container directory for new workweaves in this project.
    pub container: CanonicalPath,
    /// Recorded `name → absolute path` entries.
    #[serde(default)]
    pub workweaves: BTreeMap<WorkweaveName, CanonicalPath>,
    #[serde(default)]
    receipts: Option<NoReceipts>,
}

/// What an index's `receipts` holds: nothing. Any entries an earlier release
/// wrote are read past, and the key is written back as an empty list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NoReceipts;

impl Serialize for NoReceipts {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        serializer.serialize_seq(Some(0))?.end()
    }
}

impl<'de> Deserialize<'de> for NoReceipts {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer).map(|_| Self)
    }
}

impl WorkweaveIndex {
    /// Construct an empty index with `container` as the recorded container.
    pub fn new(container: CanonicalPath) -> Self {
        Self {
            container,
            workweaves: BTreeMap::new(),
            receipts: Some(NoReceipts),
        }
    }

    /// Whether this index has no `receipts` key: it was written before the
    /// key existed, and must be migrated before a workweave is created in
    /// this project — see [`crate::workspace::pending_index_migration`] for
    /// the operator-facing hook and [`migrate_legacy_index`] for the
    /// migration itself.
    pub fn is_legacy(&self) -> bool {
        self.receipts.is_none()
    }
}

/// Where the index file for `(primary_root, project)` lives.
pub fn index_path(primary_root: &Path, project: &ProjectName) -> StatePath {
    StateFile::WorkweaveIndex.in_dir(&project_dir(primary_root, project))
}

/// The default container for a primary workspace: `<parent-of-root>/.workweaves`.
///
/// This is what `workweave create` records into a fresh index when no other
/// container has been set. Callers should not use this directly for RESOLUTION —
/// go through [`resolve_container`] instead so the recorded container wins.
pub fn default_container(primary_root: &Path) -> PathBuf {
    primary_root
        .parent()
        .expect("weave root should have a parent")
        .join(".workweaves")
}

/// Read the index file for `(primary_root, project)`.
///
/// Returns `Ok(None)` if the file does not exist (bootstrap case: workspace
/// existed before the index was introduced, or no workweave has been created
/// yet). Callers treat `None` as "empty registry, default container" without
/// silently adopting any on-disk workweaves — adoption is doctor's job.
pub fn read(primary_root: &Path, project: &ProjectName) -> anyhow::Result<Option<WorkweaveIndex>> {
    let path = index_path(primary_root, project);
    if !path.exists() {
        return Ok(None);
    }
    let content = path
        .read_to_string()
        .with_context(|| format!("failed to read {path}"))?;
    let index: WorkweaveIndex =
        serde_json::from_str(&content).with_context(|| format!("failed to parse {path}"))?;
    Ok(Some(index))
}

/// Atomically and **durably** write `index` for `(primary_root, project)`.
///
/// This is one write path for the whole file, and it is the durable one (see
/// [`crate::durable_file`]).
///
/// If `projects/<project>/` does not exist yet the write fails with an
/// actionable error rather than silently succeeding into an unowned parent.
pub fn write(claim: &IndexClaim, index: &WorkweaveIndex) -> anyhow::Result<()> {
    let (primary_root, project) = (claim.primary_root.as_path(), &claim.project);
    let path = index_path(primary_root, project);
    let parent = path.dir();
    if !parent.exists() {
        crate::refuse!(
            RefusalKind::ProjectDirMissing,
            "cannot write workweave index: project directory {} does not exist",
            parent.display()
        );
    }
    // Hygiene at the chokepoint: every index write — create, delete,
    // set-default-create-basedir, doctor adoption/prune — keeps the machine-local file
    // out of VCS. Best effort: the design tolerates a committed copy
    // (doctor's tracked-index finding is the net), and a read-only ignore
    // surface must not block the write itself.
    let _ = ensure_ignore_entry(primary_root, project);
    let content =
        serde_json::to_string_pretty(index).context("failed to serialize workweave index")?;
    path.publish(content.as_bytes())
}

/// An exclusive claim on one project's index, released when it drops.
///
/// Two concurrent `rwv` processes creating workweaves in the same project
/// each read the index and write it back whole, so without this claim the
/// second write drops the first's entry. `link(2)` onto an already-written
/// inode is the exclusion, and it holds one winner across threads as well as
/// across processes.
///
/// A crash between acquisition and release leaves the claim behind and every
/// later write to this index refuses until someone removes it — the cost of
/// an exclusion with no daemon to expire it, paid in full view.
#[derive(Debug)]
pub struct IndexClaim {
    primary_root: PathBuf,
    project: ProjectName,
    file: crate::durable_file::ClaimFile,
}

impl IndexClaim {
    /// Run `edit` holding `(primary_root, project)`'s index claim, waiting the
    /// shared claim budget (`durable_file::CLAIM_WAIT`) for a peer holding it,
    /// then refusing with the path to remove.
    ///
    /// The claim is released before this returns, and a release that fails is
    /// an error naming the file left behind, whatever `edit` returned.
    pub fn hold<T>(
        primary_root: &Path,
        project: &ProjectName,
        edit: impl FnOnce(&Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let claim = Self::acquire(primary_root, project)?;
        let outcome = edit(&claim);
        let released = claim.file.release().with_context(|| {
            format!(
                "failed to release the claim on the workweave index of project \
                 {project} at {root}. Every later write to it refuses until {claim} \
                 is deleted",
                root = primary_root.display(),
                claim = claim_path(primary_root, project),
            )
        });
        crate::durable_file::after_release(outcome, released)
    }

    fn acquire(primary_root: &Path, project: &ProjectName) -> anyhow::Result<Self> {
        let path = claim_path(primary_root, project);
        let parent = path.dir();
        if !parent.exists() {
            crate::refuse!(
                RefusalKind::ProjectDirMissing,
                "cannot claim workweave index: project directory {} does not exist",
                parent.display()
            );
        }
        match path.claim() {
            Ok(file) => Ok(Self {
                primary_root: primary_root.to_path_buf(),
                project: project.clone(),
                file,
            }),
            Err(crate::durable_file::ClaimError::Held(held_by)) => {
                crate::refuse!(
                    RefusalKind::StateClaimHeld,
                    "another rwv still holds the workweave index of project \
                     {project} at {root} ({held_by}). If no rwv is running, \
                     delete {claim} and rerun.",
                    root = primary_root.display(),
                    claim = path,
                );
            }
            Err(crate::durable_file::ClaimError::Io(e)) => Err(anyhow::Error::new(e)
                .context(format!("failed to claim the workweave index at {path}"))),
        }
    }
}

/// Where `(primary_root, project)`'s index claim lives.
fn claim_path(primary_root: &Path, project: &ProjectName) -> StatePath {
    StateFile::IndexClaim.in_dir(&project_dir(primary_root, project))
}

/// Resolve the effective container for new workweaves in `(primary_root, project)`.
///
/// Priority:
///   1. The `container` field of the recorded index, if the index exists.
///   2. [`default_container`] (`<parent-of-root>/.workweaves`).
pub fn resolve_container(
    primary_root: &Path,
    project: &ProjectName,
) -> anyhow::Result<CanonicalPath> {
    if let Some(idx) = read(primary_root, project)? {
        return Ok(idx.container);
    }
    Ok(CanonicalPath::of(&default_container(primary_root)))
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
    read(&claim.primary_root, &claim.project)
}

/// Set the container in the index for `(primary_root, project)`, creating
/// the index file with an empty `workweaves` map if it did not exist.
///
/// The recorded entries are preserved, and the recorded container is
/// returned.
pub fn set_container(
    primary_root: &Path,
    project: &ProjectName,
    container: CanonicalPath,
) -> anyhow::Result<CanonicalPath> {
    IndexClaim::hold(primary_root, project, |claim| {
        let mut index =
            read_for_edit(claim)?.unwrap_or_else(|| WorkweaveIndex::new(container.clone()));
        index.container = container.clone();
        write(claim, &index)
    })?;
    Ok(container)
}

/// Record a workweave entry `name → path` in the index for
/// `(primary_root, project)`, creating the index if it does not exist.
///
/// Read-modify-write under the index claim.
///
/// `default_container` is used to seed the container when the index does
/// not yet exist and no override is available.
pub fn record_workweave(
    primary_root: &Path,
    project: &ProjectName,
    name: &WorkweaveName,
    path: CanonicalPath,
) -> anyhow::Result<()> {
    IndexClaim::hold(primary_root, project, |claim| {
        let mut index = read_or_seed(claim)?;
        index.workweaves.insert(name.clone(), path);
        write(claim, &index)
    })
}

/// Read the index, or seed a fresh one when the file does not exist.
///
/// Bootstrap seeds the container with the compiled-in default so the next
/// create's default lands in the same place. A legacy index is refused,
/// naming the command that migrates it.
fn read_or_seed(claim: &IndexClaim) -> anyhow::Result<WorkweaveIndex> {
    match read_for_edit(claim)? {
        Some(idx) if idx.is_legacy() => Err(legacy_index_error(&index_path(
            &claim.primary_root,
            &claim.project,
        ))),
        Some(idx) => Ok(idx),
        None => Ok(WorkweaveIndex::new(CanonicalPath::of(&default_container(
            &claim.primary_root,
        )))),
    }
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
    Ok(read(primary_root, project)?.and_then(|idx| idx.workweaves.get(name).cloned()))
}

/// Give a legacy index its `receipts` key. Returns whether the index needed
/// migrating. Idempotent, and no write on an index that already has the key.
pub fn migrate_legacy_index(primary_root: &Path, project: &ProjectName) -> anyhow::Result<bool> {
    IndexClaim::hold(primary_root, project, |claim| {
        let Some(mut index) = read_for_edit(claim)? else {
            return Ok(false);
        };
        if !index.is_legacy() {
            return Ok(false);
        }
        index.receipts = Some(NoReceipts);
        write(claim, &index)?;
        Ok(true)
    })
}

/// The refusal a legacy index earns, in the shape the legacy-*marker*
/// refusal already uses: name the file, name the command that fixes it.
fn legacy_index_error(path: &StatePath) -> anyhow::Error {
    crate::refusal::refusal(
        RefusalKind::LegacyWorkweaveIndex,
        format!(
            "{path} is a legacy workweave index (no `receipts` key). Run \
             `rwv doctor --fix` to migrate it before creating a workweave in this project."
        ),
    )
}

/// Ensure the project repo's ignore-surface excludes `.rwv-workweave-index`.
///
/// Hygiene, not correctness: the design tolerates a committed copy (reads
/// route to the primary; doctor flags a tracked index as a finding).
///
/// Best effort: silently succeeds on any I/O error. Doctor's
/// `tracked-index` finding is the correctness net if a committed copy
/// slips through.
pub fn ensure_ignore_entry(primary_root: &Path, project: &ProjectName) -> anyhow::Result<()> {
    let project_dir = project_dir(primary_root, project);
    ensure_ignored_in_dir(&project_dir, INDEX_FILENAME)?;
    ensure_ignored_in_dir(&project_dir, INDEX_CLAIM_FILE)
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::WorkweaveName;

    fn make_project(primary: &Path, name: &str) -> ProjectName {
        let p = primary.join("projects").join(name);
        std::fs::create_dir_all(&p).unwrap();
        ProjectName::new(name).unwrap()
    }

    /// A canonical store directory under `weave`, canonicalized (temp roots
    /// are symlinked on some platforms).
    fn make_store(weave: &Path, rel: &str) -> PathBuf {
        let p = weave.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        CanonicalPath::existing(&p).unwrap().into_path_buf()
    }

    /// A finished write holds the claim only while it runs. Left behind, it
    /// is the wedge every later write to this index would hit.
    #[test]
    fn a_finished_write_leaves_no_claim_behind() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(
            &primary,
            &project,
            &crate::manifest::WorkweaveName::new("hotfix").unwrap(),
            CanonicalPath::of(Path::new("/c/web-app--hotfix")),
        )
        .unwrap();
        assert!(
            !claim_path(&primary, &project).exists(),
            "the claim must be released when the read-modify-write ends"
        );
        // The no-op arm takes the claim too, and must release it.
        forget_workweave(
            &primary,
            &project,
            &crate::manifest::WorkweaveName::new("never-recorded").unwrap(),
        )
        .unwrap();
        assert!(
            !claim_path(&primary, &project).exists(),
            "an edit that changes nothing must still release its claim"
        );
    }

    /// A claim that cannot be removed refuses every later write to this
    /// index, so the edit that left it behind must say so — even though the
    /// edit itself succeeded.
    #[test]
    #[cfg(unix)]
    fn a_release_that_keeps_failing_is_reported_with_the_file_to_delete() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let project_dir = primary.join("projects/web-app");
        let writable = std::fs::metadata(&project_dir).unwrap().permissions();

        let result = IndexClaim::hold(&primary, &project, |_| {
            std::fs::set_permissions(&project_dir, std::fs::Permissions::from_mode(0o555))?;
            Ok(())
        });
        std::fs::set_permissions(&project_dir, writable).unwrap();

        let message = format!(
            "{:#}",
            result.expect_err("a release that failed must not be reported as success")
        );
        assert!(
            message.contains(INDEX_CLAIM_FILE) && message.contains("deleted"),
            "the error must name the claim left behind and the exit: {message}"
        );
        assert!(
            claim_path(&primary, &project).exists(),
            "the release failed, so the claim is still there to report"
        );
    }

    /// The decided behavior for a claim whose holder died: later writes
    /// refuse, after a bounded wait, naming the file to remove. Waiting
    /// forever hangs the verb; treating an old claim as abandoned publishes
    /// over a holder that is merely slow, which is the lost entry this claim
    /// exists to prevent; failing instantly makes ordinary contention look
    /// like a fault.
    #[test]
    fn an_abandoned_claim_refuses_later_writes_and_names_what_to_remove() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let claim = claim_path(&primary, &project);
        claim.create(b"pid 999999\n").unwrap();

        let started = std::time::Instant::now();
        let err = record_workweave(
            &primary,
            &project,
            &crate::manifest::WorkweaveName::new("hotfix").unwrap(),
            CanonicalPath::of(Path::new("/c/web-app--hotfix")),
        )
        .expect_err("a held claim must refuse the write, not drop it silently");
        let waited = started.elapsed();
        let message = format!("{err:#}");

        assert!(
            waited >= crate::durable_file::CLAIM_WAIT,
            "the refusal must come after the full wait, so ordinary contention \
             is not reported as a fault: waited {waited:?}"
        );
        assert!(
            message.contains(INDEX_CLAIM_FILE),
            "the refusal must name the file to remove: {message}"
        );
        assert!(
            message.contains("delete") && message.contains("rerun"),
            "the refusal must carry the operator's exit: {message}"
        );
        assert!(
            message.contains("pid 999999"),
            "the refusal must carry what the claim records about its holder, \
             which is how an operator decides whether it is abandoned: {message}"
        );
        assert!(
            claim.exists(),
            "a refused write must leave the holder's claim alone — clearing it \
             would make the exclusion a suggestion"
        );
        assert!(
            read(&primary, &project).unwrap().is_none(),
            "and must not have written the entry it refused to record"
        );
    }

    #[test]
    fn read_missing_returns_none() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let got = read(&primary, &project).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn write_then_read_round_trips() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let mut index = WorkweaveIndex::new(CanonicalPath::of(Path::new("/abs/container")));
        index.workweaves.insert(
            crate::manifest::WorkweaveName::new("hotfix").unwrap(),
            CanonicalPath::of(Path::new("/abs/container/web-app--hotfix")),
        );
        IndexClaim::hold(&primary, &project, |claim| write(claim, &index)).unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        assert_eq!(
            got.container,
            CanonicalPath::of(Path::new("/abs/container"))
        );
        assert_eq!(
            got.workweaves
                .get(&crate::manifest::WorkweaveName::new("hotfix").unwrap())
                .unwrap(),
            &CanonicalPath::of(Path::new("/abs/container/web-app--hotfix"))
        );
    }

    #[test]
    fn record_workweave_seeds_index_when_missing() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(
            &primary,
            &project,
            &crate::manifest::WorkweaveName::new("feat-a").unwrap(),
            CanonicalPath::of(Path::new("/abs/container/web-app--feat-a")),
        )
        .unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        // Container defaults to `<parent-of-root>/.workweaves` when no env var
        // set, recorded in the same canonical form as the placements under it.
        assert_eq!(
            got.container,
            CanonicalPath::of(&primary.parent().unwrap().join(".workweaves"))
        );
        assert_eq!(got.workweaves.len(), 1);
    }

    #[test]
    fn forget_workweave_removes_entry() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(
            &primary,
            &project,
            &crate::manifest::WorkweaveName::new("a").unwrap(),
            CanonicalPath::of(Path::new("/x/web-app--a")),
        )
        .unwrap();
        record_workweave(
            &primary,
            &project,
            &crate::manifest::WorkweaveName::new("b").unwrap(),
            CanonicalPath::of(Path::new("/x/web-app--b")),
        )
        .unwrap();
        forget_workweave(
            &primary,
            &project,
            &crate::manifest::WorkweaveName::new("a").unwrap(),
        )
        .unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        assert!(!got
            .workweaves
            .contains_key(&crate::manifest::WorkweaveName::new("a").unwrap()));
        assert!(got
            .workweaves
            .contains_key(&crate::manifest::WorkweaveName::new("b").unwrap()));
    }

    #[test]
    fn forget_workweave_noop_when_missing() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        // No index file; must not error.
        forget_workweave(
            &primary,
            &project,
            &crate::manifest::WorkweaveName::new("nonexistent").unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn set_container_preserves_existing_entries() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let container = tmp.path().join("new-container");
        std::fs::create_dir_all(&container).unwrap();

        record_workweave(
            &primary,
            &project,
            &crate::manifest::WorkweaveName::new("a").unwrap(),
            CanonicalPath::of(Path::new("/orig/web-app--a")),
        )
        .unwrap();
        set_container(&primary, &project, CanonicalPath::of(&container)).unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        assert_eq!(got.container, CanonicalPath::of(&container));
        assert_eq!(
            got.workweaves
                .get(&crate::manifest::WorkweaveName::new("a").unwrap())
                .unwrap(),
            &CanonicalPath::of(Path::new("/orig/web-app--a"))
        );
    }

    #[test]
    fn ensure_ignore_entry_creates_gitignore_when_missing() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        ensure_ignore_entry(&primary, &project).unwrap();
        let content = std::fs::read_to_string(primary.join("projects/web-app/.gitignore")).unwrap();
        assert!(content.contains(INDEX_FILENAME));
    }

    #[test]
    fn ensure_ignore_entry_idempotent_when_present() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let gitignore = primary.join("projects/web-app/.gitignore");
        std::fs::write(&gitignore, "target/\n.rwv-workweave-index\nnode_modules/\n").unwrap();
        ensure_ignore_entry(&primary, &project).unwrap();
        ensure_ignore_entry(&primary, &project).unwrap();
        let content = std::fs::read_to_string(&gitignore).unwrap();
        // Counted by whole line: the claim file's name has the index's as a
        // prefix, so a substring count reads one entry for each of the two
        // names.
        for name in [INDEX_FILENAME, INDEX_CLAIM_FILE] {
            let occurrences = content.lines().filter(|line| line.trim() == name).count();
            assert_eq!(
                occurrences, 1,
                "must not duplicate the ignore entry for {name}: {content:?}"
            );
        }
    }

    #[test]
    fn write_ensures_ignore_entry() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let index = WorkweaveIndex::new(CanonicalPath::of(Path::new("/container")));
        IndexClaim::hold(&primary, &project, |claim| write(claim, &index)).unwrap();

        // The chokepoint owns hygiene: any write path (create, delete,
        // set-default-create-basedir, doctor adoption/prune) must leave the index
        // ignored without the caller doing anything.
        let content = std::fs::read_to_string(primary.join("projects/web-app/.gitignore")).unwrap();
        assert!(
            content.contains(INDEX_FILENAME),
            "index write must ensure the ignore entry"
        );
    }

    /// The claim sits in front of every write, so its own refusal is the one
    /// an operator sees when the project directory is missing. A bare I/O
    /// error from the claim would replace an actionable message with the
    /// path of a lock file nobody asked about.
    #[test]
    fn writing_without_a_project_dir_names_the_missing_directory() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        // Note: no projects/web-app dir.
        let project = ProjectName::new("web-app").unwrap();

        let claim_err = IndexClaim::hold(&primary, &project, |_| Ok(()))
            .expect_err("claim must fail without project dir");
        let message = format!("{claim_err:#}");
        assert!(
            message.contains("project directory") && message.contains("does not exist"),
            "the claim must name the missing directory: {message}"
        );
        assert!(
            message.contains("projects"),
            "and name which directory it means: {message}"
        );
    }

    #[test]
    fn resolve_container_prefers_recorded_over_default() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let recorded = tmp.path().join("recorded");
        std::fs::create_dir_all(&recorded).unwrap();

        set_container(&primary, &project, CanonicalPath::of(&recorded)).unwrap();
        let got = resolve_container(&primary, &project).unwrap();
        assert_eq!(got, CanonicalPath::of(&recorded));
    }

    #[test]
    fn resolve_container_falls_back_to_default_when_no_index() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let got = resolve_container(&primary, &project).unwrap();
        assert_eq!(
            got,
            CanonicalPath::of(&primary.parent().unwrap().join(".workweaves"))
        );
    }

    /// Both spellings are written out here rather than computed, and that is
    /// the point: every other assertion in this file that names
    /// `CanonicalPath::of` builds its expected value by calling it, so each of
    /// those holds whatever the function does — including nothing. The
    /// missing-tail cases are the arm the container takes before anyone
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

    // -----------------------------------------------------------------
    // Receipts: a key that is written empty
    // -----------------------------------------------------------------

    fn name(text: &str) -> WorkweaveName {
        WorkweaveName::new(text).unwrap()
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
                scope.spawn(move || {
                    record_workweave(
                        primary,
                        project,
                        &name(&format!("ww{i}")),
                        CanonicalPath::of(Path::new(&format!("/c/web-app--ww{i}"))),
                    )
                    .unwrap();
                });
            }
        });

        let index = read(&primary, &project).unwrap().unwrap();
        assert_eq!(index.workweaves.len(), 8, "an entry was dropped");
    }

    /// Write a legacy index: current shape minus the `receipts` key.
    fn write_legacy_index(primary: &Path, project: &ProjectName) {
        index_path(primary, project)
            .publish(
                br#"{"container":"/abs/container","workweaves":{"hotfix":"/abs/container/web-app--hotfix"}}"#,
            )
            .unwrap();
    }

    #[test]
    fn a_legacy_index_refuses_a_new_workweave_and_names_the_fix() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        write_legacy_index(&primary, &project);

        let before = index_path(&primary, &project).read_to_string().unwrap();
        let err = record_workweave(
            &primary,
            &project,
            &name("other"),
            CanonicalPath::of(Path::new("/x/web-app--other")),
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(INDEX_FILENAME),
            "refusal names the file: {msg}"
        );
        assert!(
            msg.contains("rwv doctor --fix"),
            "refusal names the command: {msg}"
        );
        assert_eq!(
            index_path(&primary, &project).read_to_string().unwrap(),
            before,
            "a refused record must not half-migrate the index"
        );
    }

    #[test]
    fn migrating_a_legacy_index_adds_the_key_and_keeps_placements() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        write_legacy_index(&primary, &project);

        assert!(read(&primary, &project).unwrap().unwrap().is_legacy());
        assert!(migrate_legacy_index(&primary, &project).unwrap());

        let migrated = read(&primary, &project).unwrap().unwrap();
        assert!(!migrated.is_legacy());
        assert_eq!(
            migrated.container,
            CanonicalPath::of(Path::new("/abs/container"))
        );
        assert!(migrated.workweaves.contains_key(&name("hotfix")));
        assert!(
            !migrate_legacy_index(&primary, &project).unwrap(),
            "migration is idempotent"
        );
        record_workweave(
            &primary,
            &project,
            &name("other"),
            CanonicalPath::of(Path::new("/x/web-app--other")),
        )
        .unwrap();
    }

    #[test]
    fn a_fresh_index_is_not_a_legacy_one() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(
            &primary,
            &project,
            &name("a"),
            CanonicalPath::of(Path::new("/x/web-app--a")),
        )
        .unwrap();
        assert!(!read(&primary, &project).unwrap().unwrap().is_legacy());
        assert!(!migrate_legacy_index(&primary, &project).unwrap());
    }

    #[test]
    fn an_index_is_written_with_an_empty_receipts_list() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(
            &primary,
            &project,
            &name("a"),
            CanonicalPath::of(Path::new("/x/web-app--a")),
        )
        .unwrap();

        let written: serde_json::Value =
            serde_json::from_str(&index_path(&primary, &project).read_to_string().unwrap())
                .unwrap();
        assert_eq!(written["receipts"], serde_json::json!([]));
    }

    #[test]
    fn receipts_an_earlier_release_wrote_are_read_past_and_dropped_by_the_next_write() {
        let tmp = crate::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");
        index_path(&primary, &project)
            .publish(
                format!(
                    r#"{{"container":"/c","workweaves":{{}},"receipts":[{{"store":{:?},"name":"web-app--hotfix","created_at":"{}"}}]}}"#,
                    store.display().to_string(),
                    "a".repeat(40)
                )
                .as_bytes(),
            )
            .unwrap();

        let index = read(&primary, &project).unwrap().unwrap();
        assert!(!index.is_legacy(), "the key is there, whatever it holds");

        record_workweave(
            &primary,
            &project,
            &name("b"),
            CanonicalPath::of(Path::new("/x/web-app--b")),
        )
        .unwrap();
        let written: serde_json::Value =
            serde_json::from_str(&index_path(&primary, &project).read_to_string().unwrap())
                .unwrap();
        assert_eq!(written["receipts"], serde_json::json!([]));
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
        let path = index_path(&primary, &project);

        record_workweave(
            &primary,
            &project,
            &name("a"),
            CanonicalPath::of(Path::new("/x/web-app--a")),
        )
        .unwrap();
        let first_inode = path.symlink_metadata().unwrap().ino();

        record_workweave(
            &primary,
            &project,
            &name("b"),
            CanonicalPath::of(Path::new("/x/web-app--b")),
        )
        .unwrap();
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

        record_workweave(
            &primary,
            &project,
            &name("hotfix"),
            CanonicalPath::of(Path::new("/x/web-app--hotfix")),
        )
        .unwrap();

        let orphan = primary
            .join("projects/web-app")
            .join(format!("{INDEX_FILENAME}.tmp.999999.0"));
        std::fs::write(&orphan, "{ this is a half-written file").unwrap();

        assert!(read(&primary, &project)
            .unwrap()
            .unwrap()
            .workweaves
            .contains_key(&name("hotfix")));
        record_workweave(
            &primary,
            &project,
            &name("second"),
            CanonicalPath::of(Path::new("/x/web-app--second")),
        )
        .unwrap();
        assert_eq!(
            read(&primary, &project).unwrap().unwrap().workweaves.len(),
            2
        );
        assert!(orphan.exists(), "the orphan is not this write's to reap");
    }
}
