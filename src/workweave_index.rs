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
//!   "receipts": [
//!     {
//!       "store": "/abs/path/to/weave/github/acme/server",
//!       "name": "myproj--hotfix",
//!       "created_at": "1f0c…40 hex"
//!     }
//!   ]
//! }
//! ```
//!
//! ## The ownership-receipt registry
//!
//! `receipts` is the [`RefRegistry`] — the store behind R2 of the branch
//! model: *rwv may only destroy a ref it recorded creating*. It is homed
//! here, in the primary's
//! project checkout, and **not** in the workweave's `.rwv-workweave`
//! marker, because the refs outlive the workweave directory: `workweave
//! delete` runs `remove_dir_all` over the directory, so a marker-homed
//! receipt would die with the very directory whose leftover refs it exists
//! to account for.
//!
//! Receipts are keyed by **(canonical store, ref name)**. Same name, two
//! stores, two receipts — one workweave's repos each get an ephemeral ref
//! of the same name in different object stores, and they are different
//! refs.
//!
//! Durability is part of the contract, not an implementation detail; see
//! [`write`] and [`RefRegistry::record_created`].
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
//! ## Atomic writes
//!
//! Two `workweave create` invocations from sibling workweaves race on the
//! primary's shared index. `write` uses temp+rename so a concurrent writer
//! never sees a half-written file. Read-modify-write is not lock-serialised
//! (rwv has no daemon); the last writer wins for its whole snapshot. Callers
//! that need read-modify-atomicity (e.g. registering a new workweave) use the
//! `record_workweave` helper, which re-reads immediately before writing so a
//! late-losing race at worst drops an entry another writer already recorded —
//! doctor's container scan re-adopts it.

use crate::manifest::ProjectName;
use crate::vcs::{
    EphemeralRefName, LegacyEphemeralRefName, OwnedRef, RawRefName, ResolvedRevisionId,
};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The `.rwv-workweave-index` file name (dotted, machine-local).
///
/// Kept as a constant so the ignore-hygiene layer and doctor's tracked-index
/// scan can reference the same string.
pub const INDEX_FILENAME: &str = ".rwv-workweave-index";

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
/// `receipts` — the ownership receipts of [`RefRegistry`]. `None` means the
/// file was written before the registry existed (a **legacy index**), which
/// is a distinct state from "registry present, nothing recorded yet"
/// (`Some(vec![])`): the first needs the migration hook, the second is a
/// fresh workspace with no ephemeral refs. Keeping them distinct is why the
/// field is an `Option` rather than a plain `Vec` with `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkweaveIndex {
    /// Absolute container directory for new workweaves in this project.
    pub container: PathBuf,
    /// Recorded `name → absolute path` entries.
    #[serde(default)]
    pub workweaves: BTreeMap<String, PathBuf>,
    /// Ownership receipts. Private: [`RefRegistry`] is the only thing that
    /// may add or remove one — a receipt written any other way would be an
    /// ownership claim nobody checked, which is what R2 exists to prevent.
    /// Read access is [`WorkweaveIndex::has_receipt_registry`] and the
    /// registry's own accessors.
    #[serde(default)]
    receipts: Option<Vec<RefReceipt>>,
}

impl WorkweaveIndex {
    /// Construct an empty index with `container` as the recorded container.
    ///
    /// The receipt registry is present-and-empty, not absent: an index this
    /// build wrote is not a legacy index, and
    /// [`WorkweaveIndex::has_receipt_registry`] must not claim otherwise.
    pub fn new(container: PathBuf) -> Self {
        Self {
            container,
            workweaves: BTreeMap::new(),
            receipts: Some(Vec::new()),
        }
    }

    /// Whether this index carries the ownership-receipt registry at all.
    ///
    /// `false` for an index written before the registry existed. Such an
    /// index must be migrated before rwv may create or destroy refs for this
    /// project — see
    /// [`crate::workspace::pending_index_migration`] for the operator-facing
    /// hook and [`RefRegistry::migrate_legacy_index`] for the migration
    /// itself.
    pub fn has_receipt_registry(&self) -> bool {
        self.receipts.is_some()
    }
}

/// One ownership receipt: "rwv created ref `name` in store `store`, at
/// revision `created_at`".
///
/// The key — `(store, name)` — is stored **structurally**, as two fields,
/// never as one joined string. A ref name may contain `/` and a store path
/// may contain almost any byte, so every separator available for a
/// composite key is legal inside at least one half of a real key: `("/a/b",
/// "c")` and `("/a", "b/c")` would collide under a `/` join. Two different
/// receipts must never be able to answer for each other — that is the whole
/// content of R2.
///
/// `created_at` is the **canonical** SHA as a string, because
/// [`ResolvedRevisionId`] deliberately has no `Deserialize` impl (`vcs.rs`:
/// resolution is the only way to obtain one) and its `Serialize` writes the
/// *display* form, which for a tag-resolved revision is not a commit id at
/// all. Reading a receipt therefore re-parses through
/// [`ResolvedRevisionId::from_rev_parse_output`], which validates the
/// canonical shape rather than trusting the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RefReceipt {
    /// Absolute path of the canonical store the ref lives in.
    ///
    /// Absolute, like `container` and the `workweaves` values in this same
    /// file: the index is machine-local (dotted, ignored, never committed),
    /// and a receipt has to name a path the VCS can be pointed at — every
    /// consumer of [`OwnedRef::store`] hands it straight to a `Vcs` method.
    store: PathBuf,
    /// The recorded ref name, as written into the store.
    name: RawRefName,
    /// The canonical commit id the ref was created at.
    created_at: String,
}

/// The absolute path of the index file for `(primary_root, project)`.
pub fn index_path(primary_root: &Path, project: &ProjectName) -> PathBuf {
    primary_root
        .join("projects")
        .join(project.as_str())
        .join(INDEX_FILENAME)
}

/// The default container for a primary workspace: `<parent-of-root>/.workweaves`.
///
/// This is what `workweave create` records into a fresh index when no other
/// container has been set. Callers should not use this directly for RESOLUTION —
/// go through [`resolve_container`] instead so the recorded container wins.
pub fn default_container(primary_root: &Path) -> PathBuf {
    primary_root
        .parent()
        .expect("workspace root should have a parent")
        .join(".workweaves")
}

/// The form in which this index records an absolute path, for a directory that
/// may not exist yet.
///
/// Recorded paths are canonicalized, so comparing a recorded entry against a
/// directory no one has created yet needs the *parent* canonicalized and the
/// leaf rejoined — otherwise a `/tmp` that is a symlink makes every
/// not-yet-created path look different from its own recorded entry. The
/// container is subject to the same rule as the placements under it: on macOS
/// a temporary directory reached through `/var` records one way and resolves
/// the other.
pub(crate) fn canonical_recorded_path(dir: &Path) -> PathBuf {
    if let Ok(p) = dir.canonicalize() {
        return p;
    }
    match (dir.parent(), dir.file_name()) {
        (Some(parent), Some(leaf)) => parent
            .canonicalize()
            .map(|p| p.join(leaf))
            .unwrap_or_else(|_| dir.to_path_buf()),
        _ => dir.to_path_buf(),
    }
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
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let index: WorkweaveIndex = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(index))
}

/// Atomically and **durably** write `index` for `(primary_root, project)`.
///
/// Writes to `<path>.tmp.<pid>.<n>` in the same directory then renames over
/// the target — `rename(2)` is atomic within a filesystem, so a concurrent
/// writer or reader never observes a half-written file. If
/// `projects/<project>/` does not exist yet the write fails with an
/// actionable error (rather than silently succeeding into an unowned
/// parent).
///
/// ## Why this path fsyncs
///
/// Atomic is not durable. A temp file written with `write(2)` and renamed
/// leaves both the bytes and the rename in the page cache: a power loss or
/// kernel panic can lose either, and ext4's delayed-allocation flush on
/// rename-over-an-existing-file is a heuristic, not a guarantee. That gap is
/// tolerable for the placement entries and **not** tolerable for the
/// ownership receipts sharing this file, whose whole contract is that the
/// receipt reaches disk *before* the ref it describes. git fsyncs a loose
/// ref it writes; if the receipt were only in the
/// page cache, the surviving state after a crash would be a ref with no
/// receipt — the one state R2 permanently disowns.
///
/// So this is one write path, and it is the durable one: fsync the temp
/// file's contents, rename, then fsync the containing directory so the
/// rename itself survives. A separate fast-but-lossy path for the placement
/// entries would only be a way for a receipt write to end up on the wrong
/// one.
pub fn write(
    primary_root: &Path,
    project: &ProjectName,
    index: &WorkweaveIndex,
) -> anyhow::Result<()> {
    let path = index_path(primary_root, project);
    let parent = path
        .parent()
        .expect("index_path always has a parent (projects/<name>/)");
    if !parent.exists() {
        anyhow::bail!(
            "cannot write workweave index: project directory {} does not exist",
            parent.display()
        );
    }
    // Hygiene at the chokepoint: every index write — create, delete,
    // set-container, doctor adoption/prune — keeps the machine-local file
    // out of VCS. Best effort: the design tolerates a committed copy
    // (doctor's tracked-index finding is the net), and a read-only ignore
    // surface must not block the write itself.
    let _ = ensure_ignore_entry(primary_root, project);
    let content =
        serde_json::to_string_pretty(index).context("failed to serialize workweave index")?;
    write_durably(&path, parent, &content)
}

/// Serialises read-modify-write sequences on the index **within one
/// process**.
///
/// The placement entries could live with last-writer-wins: a dropped entry
/// is re-adopted by doctor's container scan, which is what the module docs
/// above describe. A dropped **receipt** cannot be re-adopted — under R2 a
/// ref with no receipt is not rwv's, permanently — so a whole-snapshot
/// write that lost one would disown a live ref. That is the failure this
/// registry exists to prevent, and it is reachable from a per-repo loop
/// fanned out across worker threads ([`crate::parallel`]), where every
/// repo's receipt lands in the same project index.
///
/// In-process only, deliberately. rwv has no daemon and this is not a file
/// lock: two concurrent `rwv` invocations mutating the *same* project's
/// index can still race, and the mutating verbs are gated against each
/// other by the op-state lease rather than here. Widening this to a
/// cross-process lock is a design decision with its own blast radius, and
/// receipt lifecycle beyond the home is Q14.
static INDEX_RMW: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`INDEX_RMW`], ignoring poisoning: a writer that panicked mid-way
/// left the file either as it was or fully replaced (the write is atomic),
/// so the next writer's read-modify-write is still sound. Wedging every
/// later write would turn one panic into a dead workspace.
fn index_rmw_guard() -> std::sync::MutexGuard<'static, ()> {
    INDEX_RMW
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Serial number for temp files, so two threads writing the same index in
/// one process cannot pick the same name and clobber each other's temp.
///
/// The pid keeps concurrent `rwv` invocations apart; the serial keeps
/// writers within one process apart, which matters now that a per-repo loop
/// can fan out across worker threads ([`crate::parallel`]) and each repo's
/// receipt lands in the same project index. Structural uniqueness, no
/// wall-clock input — the same rule `op_state::atomic_write_new` follows,
/// and for the same reason it learned to.
static TMP_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Write `content` to `path` via a temp file in `parent`, atomically and
/// durably. See [`write`] for why the fsyncs are here.
fn write_durably(path: &Path, parent: &Path, content: &str) -> anyhow::Result<()> {
    let serial = TMP_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp_path = parent.join(format!(
        "{}.tmp.{}.{}",
        INDEX_FILENAME,
        std::process::id(),
        serial
    ));

    // Contents first, and synced before the rename: a rename can only
    // publish bytes that have reached the disk.
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()
    })();
    if let Err(e) = written {
        // Do not leave the temp behind for a later reader to trip over.
        let _ = std::fs::remove_file(&tmp_path);
        return Err(
            anyhow::Error::new(e).context(format!("failed to write {}", tmp_path.display()))
        );
    }

    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(anyhow::Error::new(e).context(format!(
            "failed to rename {} to {}",
            tmp_path.display(),
            path.display()
        )));
    }

    sync_dir(parent)
}

/// fsync the directory so the **rename** is durable, not just the bytes it
/// published.
///
/// Without this, a crash can resurrect the pre-rename directory entry with
/// the new file's contents already on disk — the file is intact and the
/// index still points at the old one.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .with_context(|| format!("failed to fsync directory {}", dir.display()))
}

/// No portable directory-fsync exists off unix; `File::open` on a directory
/// is itself an error on Windows. The atomic rename still holds there.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Resolve the effective container for new workweaves in `(primary_root, project)`.
///
/// Priority:
///   1. The `container` field of the recorded index, if the index exists.
///   2. [`default_container`] (`<parent-of-root>/.workweaves`).
pub fn resolve_container(primary_root: &Path, project: &ProjectName) -> anyhow::Result<PathBuf> {
    if let Some(idx) = read(primary_root, project)? {
        return Ok(idx.container);
    }
    Ok(canonical_recorded_path(&default_container(primary_root)))
}

/// Set the container in the index for `(primary_root, project)`, creating
/// the index file with an empty `workweaves` map if it did not exist.
///
/// The recorded entries are preserved. `container` must be absolute; it is
/// recorded in [`canonical_recorded_path`] form, like the placements under it,
/// and that recorded form is returned.
///
/// Canonicalizing here rather than in each caller is what keeps the rule true:
/// the container and the placements beneath it are written by different code
/// paths, and a rule every caller has to remember is one this index spent a
/// release not obeying.
pub fn set_container(
    primary_root: &Path,
    project: &ProjectName,
    container: PathBuf,
) -> anyhow::Result<PathBuf> {
    let _guard = index_rmw_guard();
    let container = canonical_recorded_path(&container);
    let mut index =
        read(primary_root, project)?.unwrap_or_else(|| WorkweaveIndex::new(container.clone()));
    index.container = container.clone();
    write(primary_root, project, &index)?;
    Ok(container)
}

/// Record a workweave entry `name → path` in the index for
/// `(primary_root, project)`, creating the index if it does not exist.
///
/// Read-modify-write with atomic rename. Concurrent writers may race on
/// this: the last writer wins with its whole snapshot. A losing writer's
/// entry then goes missing until doctor's container-scoped scan re-adopts
/// the on-disk workweave into the registry — which is exactly the reconcile
/// path the design describes.
///
/// `default_container` is used to seed the container when the index does
/// not yet exist and no override is available.
pub fn record_workweave(
    primary_root: &Path,
    project: &ProjectName,
    name: &str,
    path: PathBuf,
) -> anyhow::Result<()> {
    let _guard = index_rmw_guard();
    let mut index = read_or_seed(primary_root, project)?;
    index.workweaves.insert(name.to_string(), path);
    write(primary_root, project, &index)
}

/// Read the index, or seed a fresh one when the file does not exist.
///
/// Bootstrap seeds the container with the compiled-in default so the next
/// create's default lands in the same place.
///
/// A seeded index is **not** a legacy index — see
/// [`WorkweaveIndex::has_receipt_registry`]. An absent file records no
/// workweaves, so it has no refs whose ownership the legacy-index migration
/// could have missed; the refs of a workweave that predates the index file
/// entirely are reached by that pass's per-repo arms, which enumerate refs
/// per store rather than reading this file.
fn read_or_seed(primary_root: &Path, project: &ProjectName) -> anyhow::Result<WorkweaveIndex> {
    if let Some(idx) = read(primary_root, project)? {
        return Ok(idx);
    }
    Ok(WorkweaveIndex::new(canonical_recorded_path(
        &default_container(primary_root),
    )))
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
    name: &str,
) -> anyhow::Result<()> {
    let _guard = index_rmw_guard();
    let mut index = match read(primary_root, project)? {
        Some(idx) => idx,
        None => return Ok(()),
    };
    if index.workweaves.remove(name).is_none() {
        return Ok(());
    }
    write(primary_root, project, &index)
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
    name: &str,
) -> anyhow::Result<Option<PathBuf>> {
    Ok(read(primary_root, project)?.and_then(|idx| idx.workweaves.get(name).cloned()))
}

// ---------------------------------------------------------------------------
// RefRegistry — the ownership-receipt store
// ---------------------------------------------------------------------------

/// The ownership receipts for one `(primary, project)` pair: the answer to
/// "is this ref rwv's to destroy?".
///
/// R2 makes ownership a matter of **record**, not of name shape. A ref that
/// merely looks like one of rwv's is not rwv's; a ref rwv holds a persisted
/// receipt for is. This type is where that record lives, and
/// [`RefRegistry::record_created`] is the sole producer of [`OwnedRef`] —
/// the receipt value every DESTROY in the branch model has to be holding.
///
/// ## Receipt-first, and what that costs
///
/// `record_created` persists the receipt **durably, before** the caller
/// creates the ref it describes. That ordering binds every path that creates
/// a ref. The two crash windows are therefore not symmetric, deliberately:
///
/// - Crash after the receipt, before the ref: a **dangling receipt**. It
///   names a ref that does not exist, authorizes nothing (no
///   [`crate::vcs::DeletionWarrant`] can be built against an absent ref),
///   and a later pass retracts it. Benign.
/// - Crash after the ref, before the receipt: an **unreceipted ref**. Under
///   R2 it is not rwv's, forever — nothing can ever clean it up. This is
///   the state the ordering exists to make unreachable.
///
/// ## No cached state
///
/// Every method re-reads the file. `rwv` has no daemon, sibling workweaves
/// write this same index, and a registry holding a snapshot would write
/// back a whole stale file — silently retracting receipts another process
/// recorded in between. `&mut self` on the writers marks them as writers;
/// it does not mean there is a buffer to flush.
///
/// ## What this type deliberately does not decide
///
/// [`RefRegistry::retract`] is policy-free: it removes a record and asks
/// for no warrant. Which callers may retract, whether a store-destroy
/// retracts per-ref (each with its own warrant) or in bulk under one
/// consent, and whether receipts are ever reclaimed, are Q14 and stay open.
/// Putting a warrant parameter here would answer Q14 by implementation.
///
/// # Only a minted name can become an owned one
///
/// ```no_run
/// use repoweave::manifest::{ProjectName, WorkweaveName};
/// use repoweave::vcs::{EphemeralRefName, ResolvedRevisionId};
/// use repoweave::workweave_index::RefRegistry;
/// use std::path::Path;
/// let project = ProjectName::new("p").unwrap();
/// let mut registry = RefRegistry::for_project(Path::new("/ws"), &project);
/// let name = EphemeralRefName::mint(&project, &WorkweaveName::new("ww").unwrap());
/// let at = ResolvedRevisionId::from_canonical("a".repeat(40), None);
/// let _ = registry.record_created(Path::new("/ws/store"), name, at);
/// ```
///
/// A name that was merely *observed* — a listing entry, a flag argument —
/// cannot be recorded as created, because recording it would mint the
/// receipt that authorizes destroying it:
///
/// ```compile_fail
/// use repoweave::manifest::ProjectName;
/// use repoweave::vcs::{RawRefName, ResolvedRevisionId};
/// use repoweave::workweave_index::RefRegistry;
/// use std::path::Path;
/// let project = ProjectName::new("p").unwrap();
/// let mut registry = RefRegistry::for_project(Path::new("/ws"), &project);
/// let at = ResolvedRevisionId::from_canonical("a".repeat(40), None);
/// let _ = registry.record_created(Path::new("/ws/store"), RawRefName::new("p--ww"), at);
/// ```
///
/// # A receipt cannot be written around the registry
///
/// ```no_run
/// use repoweave::workweave_index::WorkweaveIndex;
/// use std::path::PathBuf;
/// let _index = WorkweaveIndex::new(PathBuf::from("/container"));
/// ```
///
/// ```compile_fail
/// use repoweave::workweave_index::WorkweaveIndex;
/// use std::path::PathBuf;
/// let _index = WorkweaveIndex {
///     container: PathBuf::from("/container"),
///     workweaves: Default::default(),
///     receipts: None,
/// };
/// ```
pub struct RefRegistry {
    primary_root: PathBuf,
    project: ProjectName,
}

impl RefRegistry {
    /// A registry handle for `(primary_root, project)`.
    ///
    /// Cheap and infallible: it records where to look, and touches nothing
    /// until a method is called.
    pub fn for_project(primary_root: &Path, project: &ProjectName) -> Self {
        Self {
            primary_root: primary_root.to_path_buf(),
            project: project.clone(),
        }
    }

    /// Persist a receipt for `name` in `store`, then hand back the
    /// [`OwnedRef`] that proves it.
    ///
    /// The receipt is on disk — fsynced, file and directory — by the time
    /// this returns, so the caller may create the ref immediately after.
    /// Creating it *first* inverts the ordering and is the one thing this
    /// API exists to prevent.
    ///
    /// **Idempotent per key.** If a receipt already exists for
    /// `(store, name)` it is returned unchanged and `created_at` is
    /// ignored — no write happens at all. Two reasons, both load-bearing:
    ///
    /// - The legacy-index migration must be re-runnable over its own partial
    ///   output, and its adopt arm reaches the same end state only if
    ///   re-recording is a no-op.
    /// - Overwriting `created_at` with a freshly observed tip would forge
    ///   an [`Unmoved`](crate::vcs::DeletionWarrant::unmoved) warrant: the
    ///   check compares the ref's tip against the recorded one, so
    ///   re-recording a ref that has *moved* (operator commits on it) would
    ///   certify it as untouched and authorize destroying that work. A
    ///   caller that genuinely re-creates a ref retracts the old receipt as
    ///   part of destroying the old ref, then records anew.
    ///
    /// # Errors
    ///
    /// - The store does not exist. A receipt names the store a ref is about
    ///   to be created in; if that store cannot be resolved there is
    ///   nothing to key the receipt to, and a receipt under an unresolvable
    ///   key would be unfindable by the DESTROY that needs it later.
    /// - The index is a legacy index (no registry field). Recording into it
    ///   would erase the only signal that the migration has not run, leaving
    ///   every pre-existing ref permanently disowned. Run
    ///   [`RefRegistry::migrate_legacy_index`] first.
    /// - The write failed. It returns a `Result` rather than a bare
    ///   `OwnedRef` because a receipt that failed to persist must not be able
    ///   to look like one that did — handing back the receipt anyway is
    ///   exactly the unreceipted-ref state.
    pub fn record_created(
        &mut self,
        store: &Path,
        name: EphemeralRefName,
        created_at: ResolvedRevisionId,
    ) -> anyhow::Result<OwnedRef> {
        self.persist_receipt(store, name.to_raw(), created_at)
    }

    /// Adopt a pre-flat ref into a receipt.
    ///
    /// The migration's only route from an observation to ownership, and the
    /// narrowness is the point: a [`LegacyEphemeralRefName`] exists only for a
    /// ref that sits under the namespace a **live** workweave mints, and its
    /// sole consumer is the rename that gives that ref its flat name. A
    /// rename is a DESTROY of the old name plus a birth, and a
    /// DESTROY needs a receipt — this is that receipt, and rwv genuinely did
    /// create the ref, before receipts existed to say so.
    ///
    /// Recording the tip **as observed** is what makes
    /// [`DeletionWarrant::unmoved`](crate::vcs::DeletionWarrant::unmoved) hold
    /// for the rename a moment later, and — because
    /// [`record_created`](Self::record_created)'s no-op-on-existing rule
    /// applies here too — what keeps a re-run after a crash from re-stamping
    /// `created_at` over a tip the operator has since moved.
    pub fn adopt_legacy(
        &mut self,
        store: &Path,
        name: LegacyEphemeralRefName,
        observed_tip: ResolvedRevisionId,
    ) -> anyhow::Result<OwnedRef> {
        self.persist_receipt(store, name.to_raw(), observed_tip)
    }

    /// The shared body of [`record_created`](Self::record_created) and
    /// [`adopt_legacy`](Self::adopt_legacy).
    ///
    /// Private and taking a [`RawRefName`]: the two public routes differ only
    /// in *which* names they will accept, and that difference is carried by
    /// their argument types. Keeping the durability and idempotency rules in
    /// one body means a second recording route cannot quietly acquire
    /// different ones.
    fn persist_receipt(
        &mut self,
        store: &Path,
        raw_name: RawRefName,
        created_at: ResolvedRevisionId,
    ) -> anyhow::Result<OwnedRef> {
        let key_store = std::fs::canonicalize(store).with_context(|| {
            format!(
                "cannot record an ownership receipt for {}: the store does not exist",
                store.display()
            )
        })?;

        let _guard = index_rmw_guard();
        let mut index = read_or_seed(&self.primary_root, &self.project)?;
        let receipts = index
            .receipts
            .as_mut()
            .ok_or_else(|| self.legacy_index_error())?;

        if let Some(existing) = receipts
            .iter()
            .find(|r| same_store(&r.store, &key_store) && r.name == raw_name)
        {
            return existing.to_owned_ref(&self.index_path());
        }

        receipts.push(RefReceipt {
            store: key_store.clone(),
            name: raw_name.clone(),
            created_at: created_at.as_str().to_owned(),
        });
        write(&self.primary_root, &self.project, &index)?;

        Ok(OwnedRef::from_receipt(key_store, raw_name, created_at))
    }

    /// The receipt for `(store, name)`, if rwv holds one.
    ///
    /// `None` means "not rwv's" — the ref may well exist; nothing here
    /// asks. A legacy index also reads as `None`: it holds no receipts, so
    /// no ref in it is destroyable, which is the fail-closed direction.
    pub fn lookup(&self, store: &Path, name: &RawRefName) -> anyhow::Result<Option<OwnedRef>> {
        let Some(index) = read(&self.primary_root, &self.project)? else {
            return Ok(None);
        };
        let Some(receipts) = index.receipts.as_ref() else {
            return Ok(None);
        };
        let key_store = store_key(store);
        receipts
            .iter()
            .find(|r| same_store(&r.store, &key_store) && &r.name == name)
            .map(|r| r.to_owned_ref(&self.index_path()))
            .transpose()
    }

    /// Every receipt keyed to `store`.
    ///
    /// R4's precondition for destroying a whole store is that each of these
    /// has been retracted; this is how a caller enumerates them. Ordered as
    /// recorded.
    pub fn list_for_store(&self, store: &Path) -> anyhow::Result<Vec<OwnedRef>> {
        let key_store = store_key(store);
        self.list_matching(|r| same_store(&r.store, &key_store))
    }

    /// Every receipt in this project's registry, across stores.
    ///
    /// What a doctor pass walks to find receipts whose ref never appeared.
    pub fn list_all(&self) -> anyhow::Result<Vec<OwnedRef>> {
        self.list_matching(|_| true)
    }

    /// Drop the receipt for `(store, name)`. Returns whether one was
    /// there.
    ///
    /// The retraction **primitive** — R4 requires retraction before a store
    /// may be destroyed, and a dangling receipt (the benign residue of a
    /// crash between the receipt and the ref) is cleaned up with this. It
    /// takes no warrant on purpose: what consent a retraction needs is
    /// undecided, and a warrant parameter would decide it by implementation.
    ///
    /// Idempotent, and it does not write when there is nothing to remove —
    /// so retracting into a legacy index cannot quietly create the registry
    /// field and hide that the migration is still pending.
    pub fn retract(&mut self, store: &Path, name: &RawRefName) -> anyhow::Result<bool> {
        let _guard = index_rmw_guard();
        let Some(mut index) = read(&self.primary_root, &self.project)? else {
            return Ok(false);
        };
        let Some(receipts) = index.receipts.as_mut() else {
            return Ok(false);
        };
        let key_store = store_key(store);
        let before = receipts.len();
        receipts.retain(|r| !(same_store(&r.store, &key_store) && &r.name == name));
        if receipts.len() == before {
            return Ok(false);
        }
        write(&self.primary_root, &self.project, &index)?;
        Ok(true)
    }

    /// Give a legacy index an empty receipt registry.
    ///
    /// The migration pass runs this first, then records a receipt per ref
    /// it adopts or renames; the receipt-first ordering is what makes the
    /// pass re-runnable after a crash at any point in it.
    ///
    /// Returns whether the index needed migrating. Idempotent, and a no-op
    /// (no write) on an index that already has the field.
    ///
    /// Adding the field is not itself an ownership claim: it produces an
    /// *empty* registry, so every pre-existing ref stays unowned until an
    /// arm of the pass records it explicitly.
    pub fn migrate_legacy_index(&mut self) -> anyhow::Result<bool> {
        let _guard = index_rmw_guard();
        let Some(mut index) = read(&self.primary_root, &self.project)? else {
            // No file: nothing recorded, nothing legacy. The next write
            // seeds a current-shape index.
            return Ok(false);
        };
        if index.receipts.is_some() {
            return Ok(false);
        }
        index.receipts = Some(Vec::new());
        write(&self.primary_root, &self.project, &index)?;
        Ok(true)
    }

    /// The index file this registry reads and writes.
    fn index_path(&self) -> PathBuf {
        index_path(&self.primary_root, &self.project)
    }

    /// Receipts passing `keep`, as receipts.
    fn list_matching(&self, keep: impl Fn(&RefReceipt) -> bool) -> anyhow::Result<Vec<OwnedRef>> {
        let Some(index) = read(&self.primary_root, &self.project)? else {
            return Ok(Vec::new());
        };
        let Some(receipts) = index.receipts.as_ref() else {
            return Ok(Vec::new());
        };
        let path = self.index_path();
        receipts
            .iter()
            .filter(|r| keep(r))
            .map(|r| r.to_owned_ref(&path))
            .collect()
    }

    /// The refusal an unmigrated index earns, shaped like the legacy-marker
    /// refusal in [`crate::workspace::WorkweaveMarker::read`]: name the
    /// file, name the command.
    fn legacy_index_error(&self) -> anyhow::Error {
        legacy_index_error(&self.index_path())
    }
}

/// The refusal a legacy index earns, in the shape the legacy-*marker*
/// refusal already uses: name the file, name the command that fixes it.
///
/// Free function rather than a method so the wording has one home; the
/// detection side lives at [`crate::workspace::pending_index_migration`],
/// next to the marker check it mirrors.
fn legacy_index_error(path: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "{} is a legacy workweave index written before ref-ownership receipts \
         existed (no `receipts` field). Run `rwv doctor --fix` to migrate it \
         before creating or destroying refs in this project.",
        path.display()
    )
}

impl RefReceipt {
    /// Re-derive the receipt value, validating the stored revision.
    ///
    /// `path` is the index file, for the error: a malformed `created_at` is
    /// a corrupt receipt, and the alternative to refusing is handing out an
    /// [`OwnedRef`] whose recorded tip is not a commit id — which every
    /// warrant check would then compare against and silently never match.
    fn to_owned_ref(&self, path: &Path) -> anyhow::Result<OwnedRef> {
        let created_at =
            ResolvedRevisionId::from_rev_parse_output(&self.created_at).ok_or_else(|| {
                anyhow::anyhow!(
                    "{}: ownership receipt for '{}' records '{}' as its created-at \
                     revision, which is not a canonical commit id",
                    path.display(),
                    self.name,
                    self.created_at
                )
            })?;
        Ok(OwnedRef::from_receipt(
            self.store.clone(),
            self.name.clone(),
            created_at,
        ))
    }
}

/// The comparable form of a store path: canonicalized when the store is
/// there, verbatim when it is not.
///
/// Receipts outlive their store — R4 retracts them *before* destroying it,
/// and a doctor pass reads them after a directory has gone missing — so
/// this cannot require the path to resolve. Recording canonicalizes (see
/// [`RefRegistry::record_created`], which refuses otherwise), so the
/// fallback only ever compares a query against an already-canonical
/// recorded key: it matches when the caller spells the store the way it was
/// recorded, which is what every in-tree caller does. No lexical
/// `..`-folding is attempted; guessing a symlinked path's identity wrong
/// would mean matching a receipt from a *different* store, and claiming
/// ownership of someone else's ref is the failure R2 exists to prevent.
fn store_key(store: &Path) -> PathBuf {
    std::fs::canonicalize(store).unwrap_or_else(|_| store.to_path_buf())
}

/// Whether a recorded store key and an already-keyed query name the same
/// store. `query_key` must have come from [`store_key`].
fn same_store(recorded: &Path, query_key: &Path) -> bool {
    recorded == query_key || store_key(recorded).as_path() == query_key
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
    let project_dir = primary_root.join("projects").join(project.as_str());
    ensure_ignored_in_dir(&project_dir, INDEX_FILENAME)
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
/// Two candidate targets, prioritised for zero shared-repo footprint:
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
    let git_entry = dir.join(".git");
    if git_entry.is_dir() {
        let info = git_entry.join("info");
        std::fs::create_dir_all(&info).ok()?;
        return Some(info);
    }
    if git_entry.is_file() {
        let content = std::fs::read_to_string(&git_entry).ok()?;
        // Format: `gitdir: <path>` (possibly relative).
        let stripped = content.trim().strip_prefix("gitdir:")?.trim();
        let gitdir = PathBuf::from(stripped);
        let gitdir = if gitdir.is_absolute() {
            gitdir
        } else {
            dir.join(gitdir)
        };
        // For linked worktrees the `info/` we want to touch is the
        // COMMON info dir, not the worktree-specific one.
        let common = gitdir.join("commondir");
        let common_target = if common.exists() {
            let s = std::fs::read_to_string(&common).ok()?;
            let rel = PathBuf::from(s.trim());
            if rel.is_absolute() {
                rel
            } else {
                gitdir.join(rel)
            }
        } else {
            gitdir
        };
        let info = common_target.join("info");
        std::fs::create_dir_all(&info).ok()?;
        return Some(info);
    }
    None
}

/// Append `filename` to the ignore file at `target` if it is not already present.
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
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(needle);
    new_content.push('\n');
    // Ensure parent (for `.git/info/`) exists — best-effort; append_ignore_line
    // may be called with a `.gitignore` whose parent always exists.
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(target, new_content)
        .with_context(|| format!("failed to update {}", target.display()))?;
    Ok(())
}

/// Enumerate every project directly under `<primary_root>/projects/`.
///
/// A helper for callers that need to iterate every project's registry
/// (e.g. adopting children across the workspace when a workweave is
/// retired). Returns projects sorted by name; directories missing an
/// `rwv.yaml` are still included (a project can register workweaves before
/// its manifest is populated).
pub fn projects_on_disk(primary_root: &Path) -> Vec<ProjectName> {
    let projects_dir = primary_root.join("projects");
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<ProjectName> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|s| ProjectName::new(s).ok())
        .collect();
    names.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    names
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

    /// A canonical store directory under `weave`, canonicalized so tests
    /// compare against the same spelling [`RefRegistry::record_created`]
    /// records (temp roots are symlinked on some platforms).
    fn make_store(weave: &Path, rel: &str) -> PathBuf {
        let p = weave.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    /// A distinct canonical revision per `hex` character.
    fn rev(hex: char) -> ResolvedRevisionId {
        ResolvedRevisionId::from_canonical(hex.to_string().repeat(40), None)
    }

    fn ephemeral(project: &ProjectName, workweave: &str) -> EphemeralRefName {
        EphemeralRefName::mint(project, &WorkweaveName::new(workweave).unwrap())
    }

    #[test]
    fn read_missing_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let got = read(&primary, &project).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn write_then_read_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let mut index = WorkweaveIndex::new(PathBuf::from("/abs/container"));
        index.workweaves.insert(
            "hotfix".to_string(),
            PathBuf::from("/abs/container/web-app--hotfix"),
        );
        write(&primary, &project, &index).unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        assert_eq!(got.container, PathBuf::from("/abs/container"));
        assert_eq!(
            got.workweaves.get("hotfix").unwrap(),
            &PathBuf::from("/abs/container/web-app--hotfix")
        );
    }

    #[test]
    fn record_workweave_seeds_index_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(
            &primary,
            &project,
            "feat-a",
            PathBuf::from("/abs/container/web-app--feat-a"),
        )
        .unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        // Container defaults to `<parent-of-root>/.workweaves` when no env var
        // set, recorded in the same canonical form as the placements under it.
        assert_eq!(
            got.container,
            canonical_recorded_path(&primary.parent().unwrap().join(".workweaves"))
        );
        assert_eq!(got.workweaves.len(), 1);
    }

    #[test]
    fn forget_workweave_removes_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(&primary, &project, "a", PathBuf::from("/x/web-app--a")).unwrap();
        record_workweave(&primary, &project, "b", PathBuf::from("/x/web-app--b")).unwrap();
        forget_workweave(&primary, &project, "a").unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        assert!(!got.workweaves.contains_key("a"));
        assert!(got.workweaves.contains_key("b"));
    }

    #[test]
    fn forget_workweave_noop_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        // No index file; must not error.
        forget_workweave(&primary, &project, "nonexistent").unwrap();
    }

    #[test]
    fn set_container_preserves_existing_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(&primary, &project, "a", PathBuf::from("/orig/web-app--a")).unwrap();
        set_container(&primary, &project, PathBuf::from("/new-container")).unwrap();

        let got = read(&primary, &project).unwrap().unwrap();
        assert_eq!(got.container, PathBuf::from("/new-container"));
        assert_eq!(
            got.workweaves.get("a").unwrap(),
            &PathBuf::from("/orig/web-app--a")
        );
    }

    #[test]
    fn ensure_ignore_entry_creates_gitignore_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        ensure_ignore_entry(&primary, &project).unwrap();
        let content = std::fs::read_to_string(primary.join("projects/web-app/.gitignore")).unwrap();
        assert!(content.contains(INDEX_FILENAME));
    }

    #[test]
    fn ensure_ignore_entry_idempotent_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let gitignore = primary.join("projects/web-app/.gitignore");
        std::fs::write(&gitignore, "target/\n.rwv-workweave-index\nnode_modules/\n").unwrap();
        ensure_ignore_entry(&primary, &project).unwrap();
        let content = std::fs::read_to_string(&gitignore).unwrap();
        // Line count should be unchanged (3 non-empty lines).
        let occurrences = content.matches(INDEX_FILENAME).count();
        assert_eq!(occurrences, 1, "must not duplicate the ignore entry");
    }

    #[test]
    fn write_ensures_ignore_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let index = WorkweaveIndex::new(PathBuf::from("/container"));
        write(&primary, &project, &index).unwrap();

        // The chokepoint owns hygiene: any write path (create, delete,
        // set-container, doctor adoption/prune) must leave the index
        // ignored without the caller doing anything.
        let content = std::fs::read_to_string(primary.join("projects/web-app/.gitignore")).unwrap();
        assert!(
            content.contains(INDEX_FILENAME),
            "index write must ensure the ignore entry"
        );
    }

    #[test]
    fn write_fails_without_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        // Note: no projects/web-app dir.
        let project = ProjectName::new("web-app").unwrap();

        let index = WorkweaveIndex::new(PathBuf::from("/x"));
        let result = write(&primary, &project, &index);
        assert!(result.is_err(), "write must fail without project dir");
    }

    #[test]
    fn resolve_container_prefers_recorded_over_default() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        set_container(&primary, &project, PathBuf::from("/recorded")).unwrap();
        let got = resolve_container(&primary, &project).unwrap();
        assert_eq!(got, PathBuf::from("/recorded"));
    }

    #[test]
    fn resolve_container_falls_back_to_default_when_no_index() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        let got = resolve_container(&primary, &project).unwrap();
        assert_eq!(
            got,
            canonical_recorded_path(&primary.parent().unwrap().join(".workweaves"))
        );
    }

    // -----------------------------------------------------------------
    // RefRegistry — receipts
    // -----------------------------------------------------------------

    /// The benign crash window: the receipt is persisted, then the process
    /// dies before the ref is created.
    ///
    /// "Crash" here is what the tree's crash tests already mean by it (see
    /// `tests/crash_matrix_test.rs`): the on-disk state a kill at that
    /// point leaves behind — the receipt written, the ref never created —
    /// re-read through a **fresh registry handle**, which is all a new
    /// process would have.
    #[test]
    fn crash_after_the_receipt_and_before_the_ref_leaves_a_retractable_receipt() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");

        let mut registry = RefRegistry::for_project(&primary, &project);
        let owned = registry
            .record_created(&store, ephemeral(&project, "hotfix"), rev('a'))
            .unwrap();
        assert_eq!(owned.store(), store.as_path());
        // <- the ref would be created here; the process dies instead.
        drop(registry);

        let recovered = RefRegistry::for_project(&primary, &project)
            .lookup(&store, &RawRefName::new("web-app--hotfix"))
            .unwrap()
            .expect("the receipt outlives the crash");
        assert_eq!(recovered.created_at(), &rev('a'));
        assert_eq!(recovered.store(), store.as_path());

        // Dangling, and retractable: the whole reason this window is the
        // benign one.
        let mut registry = RefRegistry::for_project(&primary, &project);
        assert!(registry
            .retract(&store, &RawRefName::new("web-app--hotfix"))
            .unwrap());
        assert!(registry
            .lookup(&store, &RawRefName::new("web-app--hotfix"))
            .unwrap()
            .is_none());
        assert!(
            !registry
                .retract(&store, &RawRefName::new("web-app--hotfix"))
                .unwrap(),
            "retraction is idempotent"
        );
    }

    /// The forbidden crash window, from the registry's side: a ref that
    /// exists without a receipt is not rwv's, and no amount of asking by
    /// name changes that.
    ///
    /// The ref-exists half of this needs a real store; it is pinned in
    /// `tests/ref_registry_test.rs`. What is pinned here is that a receipt
    /// recorded for a *different* workweave in the same store does not
    /// answer for this name — ownership is per exact name, not per prefix.
    #[test]
    fn a_name_with_no_receipt_of_its_own_is_not_owned() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");

        let mut registry = RefRegistry::for_project(&primary, &project);
        registry
            .record_created(&store, ephemeral(&project, "hotfix"), rev('a'))
            .unwrap();

        for unowned in [
            "web-app--feature",  // a sibling workweave's ref
            "web-app",           // the prefix alone
            "web-app--hotfix/x", // the legacy segmented shape
            "main",
        ] {
            assert!(
                registry
                    .lookup(&store, &RawRefName::new(unowned))
                    .unwrap()
                    .is_none(),
                "'{unowned}' has no receipt and must not be owned"
            );
        }
    }

    /// One name, two stores, two refs. The key is the pair.
    #[test]
    fn receipts_key_on_the_store_as_well_as_the_name() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let server = make_store(tmp.path(), "weave/github/acme/server");
        let client = make_store(tmp.path(), "weave/github/acme/client");

        let name = ephemeral(&project, "hotfix");
        let raw = name.to_raw();
        let mut registry = RefRegistry::for_project(&primary, &project);
        registry
            .record_created(&server, name.clone(), rev('a'))
            .unwrap();
        registry.record_created(&client, name, rev('b')).unwrap();

        let from_server = registry.lookup(&server, &raw).unwrap().unwrap();
        let from_client = registry.lookup(&client, &raw).unwrap().unwrap();
        assert_eq!(from_server.created_at(), &rev('a'));
        assert_eq!(from_client.created_at(), &rev('b'));
        assert_ne!(from_server, from_client);

        assert_eq!(registry.list_for_store(&server).unwrap().len(), 1);
        assert_eq!(registry.list_all().unwrap().len(), 2);

        // Retracting one store's receipt leaves the other's alone — R4
        // retracts per store, and a name-keyed retraction would empty both.
        assert!(registry.retract(&server, &raw).unwrap());
        assert!(registry.lookup(&server, &raw).unwrap().is_none());
        assert_eq!(
            registry
                .lookup(&client, &raw)
                .unwrap()
                .unwrap()
                .created_at(),
            &rev('b')
        );
        assert!(registry.list_for_store(&server).unwrap().is_empty());
    }

    /// A per-repo loop fanned out across threads records one receipt per
    /// store into one file. Every one of them has to be there afterwards: a
    /// receipt lost to a read-modify-write race disowns a live ref, and
    /// nothing downstream can recover it.
    #[test]
    fn concurrent_receipt_writers_lose_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let stores: Vec<PathBuf> = (0..8)
            .map(|i| make_store(tmp.path(), &format!("weave/github/acme/repo{i}")))
            .collect();

        std::thread::scope(|scope| {
            for (i, store) in stores.iter().enumerate() {
                let project = &project;
                let primary = &primary;
                scope.spawn(move || {
                    RefRegistry::for_project(primary, project)
                        .record_created(store, ephemeral(project, &format!("ww{i}")), rev('a'))
                        .unwrap();
                });
            }
        });

        let registry = RefRegistry::for_project(&primary, &project);
        assert_eq!(registry.list_all().unwrap().len(), stores.len());
        for (i, store) in stores.iter().enumerate() {
            assert!(
                registry
                    .lookup(store, &RawRefName::new(format!("web-app--ww{i}")))
                    .unwrap()
                    .is_some(),
                "receipt for repo{i} was dropped"
            );
        }
    }

    /// The receipt has to be readable after `workweave delete`'s
    /// `remove_dir_all`, because the refs it accounts for are exactly the
    /// ones that directory left behind.
    #[test]
    fn receipts_outlive_the_workweave_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");

        // A workweave directory, marker and all, of the shape `workweave
        // delete` destroys.
        let workweave = tmp.path().join(".workweaves/web-app--hotfix");
        std::fs::create_dir_all(&workweave).unwrap();
        std::fs::write(workweave.join(".rwv-workweave"), "primary: /ws\n").unwrap();
        record_workweave(&primary, &project, "hotfix", workweave.clone()).unwrap();

        let mut registry = RefRegistry::for_project(&primary, &project);
        registry
            .record_created(&store, ephemeral(&project, "hotfix"), rev('a'))
            .unwrap();

        std::fs::remove_dir_all(&workweave).unwrap();
        assert!(
            !workweave.join(".rwv-workweave").exists(),
            "the marker is gone — a marker-homed receipt would have gone with it"
        );

        let survivor = registry
            .lookup(&store, &RawRefName::new("web-app--hotfix"))
            .unwrap()
            .expect("the receipt is homed in the primary, not in the workweave");
        assert_eq!(survivor.created_at(), &rev('a'));
    }

    /// Re-recording must not relabel a ref that has moved: the recorded tip
    /// is what `DeletionWarrant::unmoved` compares against, so overwriting
    /// it with a fresh observation would certify operator commits as
    /// "untouched since rwv created it" and authorize destroying them.
    #[test]
    fn re_recording_keeps_the_first_created_at() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");

        let mut registry = RefRegistry::for_project(&primary, &project);
        let first = registry
            .record_created(&store, ephemeral(&project, "hotfix"), rev('a'))
            .unwrap();
        // The ref has since moved (operator commits); a naive retry records
        // the ref's *current* tip.
        let second = registry
            .record_created(&store, ephemeral(&project, "hotfix"), rev('c'))
            .unwrap();

        assert_eq!(first.created_at(), &rev('a'));
        assert_eq!(
            second.created_at(),
            &rev('a'),
            "the recorded tip is the one rwv created the ref at"
        );
        assert_eq!(registry.list_all().unwrap().len(), 1, "no duplicate key");
    }

    #[test]
    fn recording_against_a_store_that_is_not_there_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let absent = tmp.path().join("weave/github/acme/ghost");

        let err = RefRegistry::for_project(&primary, &project)
            .record_created(&absent, ephemeral(&project, "hotfix"), rev('a'))
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("the store does not exist") && msg.contains("ghost"),
            "refusal must name the store: {msg}"
        );
        assert!(
            read(&primary, &project).unwrap().is_none(),
            "a refused receipt writes nothing"
        );
    }

    /// The registry shares its file with the placement entries, so an
    /// unrelated index write must carry the receipts through.
    #[test]
    fn an_unrelated_index_write_preserves_receipts() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");

        let mut registry = RefRegistry::for_project(&primary, &project);
        registry
            .record_created(&store, ephemeral(&project, "hotfix"), rev('a'))
            .unwrap();

        record_workweave(
            &primary,
            &project,
            "other",
            PathBuf::from("/x/web-app--other"),
        )
        .unwrap();
        set_container(&primary, &project, PathBuf::from("/new-container")).unwrap();
        forget_workweave(&primary, &project, "other").unwrap();

        assert!(registry
            .lookup(&store, &RawRefName::new("web-app--hotfix"))
            .unwrap()
            .is_some());
    }

    /// Write a legacy index: current shape minus the registry field.
    fn write_legacy_index(primary: &Path, project: &ProjectName) {
        std::fs::write(
            index_path(primary, project),
            r#"{"container":"/abs/container","workweaves":{"hotfix":"/abs/container/web-app--hotfix"}}"#,
        )
        .unwrap();
    }

    #[test]
    fn a_legacy_index_refuses_a_receipt_and_names_the_fix() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");
        write_legacy_index(&primary, &project);

        let before = std::fs::read_to_string(index_path(&primary, &project)).unwrap();
        let err = RefRegistry::for_project(&primary, &project)
            .record_created(&store, ephemeral(&project, "hotfix"), rev('a'))
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
            std::fs::read_to_string(index_path(&primary, &project)).unwrap(),
            before,
            "a refused record must not half-migrate the index — the missing \
             field is the only signal the migration has not run"
        );
        // Fails closed meanwhile: an unmigrated index owns nothing.
        assert!(RefRegistry::for_project(&primary, &project)
            .lookup(&store, &RawRefName::new("web-app--hotfix"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn retracting_against_a_legacy_index_does_not_migrate_it() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");
        write_legacy_index(&primary, &project);

        let mut registry = RefRegistry::for_project(&primary, &project);
        assert!(!registry
            .retract(&store, &RawRefName::new("web-app--hotfix"))
            .unwrap());
        assert!(!read(&primary, &project)
            .unwrap()
            .unwrap()
            .has_receipt_registry());
    }

    #[test]
    fn migrating_a_legacy_index_adds_an_empty_registry_and_keeps_placements() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");
        write_legacy_index(&primary, &project);

        let mut registry = RefRegistry::for_project(&primary, &project);
        assert!(!read(&primary, &project)
            .unwrap()
            .unwrap()
            .has_receipt_registry());
        assert!(registry.migrate_legacy_index().unwrap());

        let migrated = read(&primary, &project).unwrap().unwrap();
        assert!(migrated.has_receipt_registry());
        assert_eq!(migrated.container, PathBuf::from("/abs/container"));
        assert!(migrated.workweaves.contains_key("hotfix"));
        assert!(
            registry.list_all().unwrap().is_empty(),
            "migration adds the field, never an ownership claim"
        );

        assert!(
            !registry.migrate_legacy_index().unwrap(),
            "migration is idempotent"
        );
        // And the pass can now do its job.
        registry
            .record_created(&store, ephemeral(&project, "hotfix"), rev('a'))
            .unwrap();
        assert_eq!(registry.list_all().unwrap().len(), 1);
    }

    #[test]
    fn a_fresh_index_is_not_a_legacy_one() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");

        record_workweave(&primary, &project, "a", PathBuf::from("/x/web-app--a")).unwrap();
        assert!(read(&primary, &project)
            .unwrap()
            .unwrap()
            .has_receipt_registry());
        assert!(!RefRegistry::for_project(&primary, &project)
            .migrate_legacy_index()
            .unwrap());
    }

    /// A receipt whose revision is not a canonical commit id is corrupt.
    /// Refusing beats handing back an `OwnedRef` no warrant check could
    /// ever match — that would read as "this ref moved" forever.
    #[test]
    fn a_malformed_recorded_revision_is_refused_not_laundered() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");

        std::fs::write(
            index_path(&primary, &project),
            format!(
                r#"{{"container":"/c","workweaves":{{}},"receipts":[{{"store":{:?},"name":"web-app--hotfix","created_at":"HEAD~1"}}]}}"#,
                store.display().to_string()
            ),
        )
        .unwrap();

        let err = RefRegistry::for_project(&primary, &project)
            .lookup(&store, &RawRefName::new("web-app--hotfix"))
            .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("HEAD~1") && msg.contains("canonical commit id"),
            "refusal must name the bad value: {msg}"
        );
    }

    /// The same store, spelled through a symlink, is the same store. A
    /// receipt that could not be found again by an equivalent spelling
    /// would strand the ref it accounts for.
    #[test]
    #[cfg(unix)]
    fn a_symlinked_spelling_finds_the_same_receipt() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");

        let link = tmp.path().join("server-link");
        std::os::unix::fs::symlink(&store, &link).unwrap();

        let mut registry = RefRegistry::for_project(&primary, &project);
        registry
            .record_created(&store, ephemeral(&project, "hotfix"), rev('a'))
            .unwrap();

        assert!(registry
            .lookup(&link, &RawRefName::new("web-app--hotfix"))
            .unwrap()
            .is_some());
        assert!(registry
            .retract(&link, &RawRefName::new("web-app--hotfix"))
            .unwrap());
    }

    /// The other direction of the same question: a receipt whose *recorded*
    /// store is spelled non-canonically — a hand-edited index, or one
    /// carried over from a writer that did not canonicalize — must still be
    /// found by the canonical spelling. Losing it would strand the ref it
    /// accounts for with no way to retract or destroy it.
    #[test]
    #[cfg(unix)]
    fn a_receipt_recorded_under_a_non_canonical_spelling_is_still_found() {
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");

        let link = tmp.path().join("server-link");
        std::os::unix::fs::symlink(&store, &link).unwrap();
        std::fs::write(
            index_path(&primary, &project),
            format!(
                r#"{{"container":"/c","workweaves":{{}},"receipts":[{{"store":{:?},"name":"web-app--hotfix","created_at":{:?}}}]}}"#,
                link.display().to_string(),
                "a".repeat(40)
            ),
        )
        .unwrap();

        let found = RefRegistry::for_project(&primary, &project)
            .lookup(&store, &RawRefName::new("web-app--hotfix"))
            .unwrap()
            .expect("the recorded spelling resolves to the queried store");
        assert_eq!(found.created_at(), &rev('a'));
    }

    /// Atomicity, pinned by the mechanism rather than by hoping: a
    /// successful write replaces the file by `rename(2)`, so the target's
    /// inode changes and no reader can ever have seen a partial one. An
    /// in-place rewrite would keep the inode — and would be observable
    /// half-written.
    #[test]
    #[cfg(unix)]
    fn an_index_write_replaces_the_file_by_rename() {
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");
        let path = index_path(&primary, &project);

        let mut registry = RefRegistry::for_project(&primary, &project);
        registry
            .record_created(&store, ephemeral(&project, "a"), rev('a'))
            .unwrap();
        let first_inode = std::fs::metadata(&path).unwrap().ino();

        registry
            .record_created(&store, ephemeral(&project, "b"), rev('b'))
            .unwrap();
        assert_ne!(
            std::fs::metadata(&path).unwrap().ino(),
            first_inode,
            "the index must be replaced by rename, not rewritten in place"
        );

        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
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
        let tmp = tempfile::tempdir().unwrap();
        let primary = tmp.path().join("ws");
        let project = make_project(&primary, "web-app");
        let store = make_store(tmp.path(), "weave/github/acme/server");

        let mut registry = RefRegistry::for_project(&primary, &project);
        registry
            .record_created(&store, ephemeral(&project, "hotfix"), rev('a'))
            .unwrap();

        // Exactly what a kill between `write` and `rename` leaves.
        let orphan = primary
            .join("projects/web-app")
            .join(format!("{INDEX_FILENAME}.tmp.999999.0"));
        std::fs::write(&orphan, "{ this is a half-written file").unwrap();

        assert!(registry
            .lookup(&store, &RawRefName::new("web-app--hotfix"))
            .unwrap()
            .is_some());
        registry
            .record_created(&store, ephemeral(&project, "second"), rev('b'))
            .unwrap();
        assert_eq!(registry.list_all().unwrap().len(), 2);
        assert!(orphan.exists(), "the orphan is not this write's to reap");
    }
}
