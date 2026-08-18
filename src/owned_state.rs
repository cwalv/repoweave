//! The attested-generation ledger for fully-owned generated files.
//!
//! One state file records what rwv last accepted for each generated file it
//! does not author byte-for-byte. Every entry is written at the moment of
//! acceptance and read by a verify pass that may not write: a caller that
//! stamps at any other moment attests a generation rwv never accepted, and
//! the ledger then reads as freshness for as long as the inputs sit still.
//!
//! rwv cannot recompute an ecosystem lockfile's content: `cargo
//! generate-lockfile` (and `uv sync`, `npm install`, ...) own the generation
//! and their output depends on registry state, so regeneration-compare is
//! impossible in a read-only verify pass. Instead, the intent-side hook that
//! accepts a generation stamps a SHA-256 of the accepted bytes into a small
//! rwv-owned state file; `verify()` compares the current on-disk bytes
//! against the recorded digest. A mismatch means the file changed since rwv
//! last accepted it — a purely structural signal (no wall-clock, no
//! registry access).
//!
//! State file: `.rwv-owned-digests` in the directory the caller names (the
//! `.rwv-active` / `.rwv-op` naming family). Format: a flat JSON map
//! `filename -> "sha256:<hex>"` — room for future entries as more
//! integrations adopt the axis. The file is advisory bookkeeping: it is
//! wholly rewritten by the next stamp, so corruption is self-healing and
//! never worth failing a verify pass over.
//!
//! The directory every helper here wants is `output_dir`, and no signature
//! can say so: generated files live in the project dir and are SURFACED at
//! the weave root via symlinks, for the ACTIVE project alone. A caller that
//! named the weave root would key the ledger by whichever project happens to
//! be active. [`crate::workspace::WorkspaceSession::context_base`] derives
//! `output_dir` rather than accepting one, which is what leaves callers with
//! nothing else to name.
//!
//! cargo-workspace is the first consumer; uv/npm/pnpm lockfiles have the
//! identical story and can port by calling the same three helpers.

use crate::integration::{Issue, IssueKind, Severity};
use crate::workweave_index;
use anyhow::Context;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// File name of the rwv-owned digest state file, written in the same
/// directory as the generated files it records.
///
/// `tests/` cannot see this constant — it is a separate, external crate —
/// and spells the name as the literal `".rwv-owned-digests"` instead.
pub(crate) const OWNED_DIGESTS_FILE: &str = ".rwv-owned-digests";

/// File name of the claim one rwv holds across a ledger read-modify-write, so
/// a second rwv cannot publish a snapshot it took before the first landed.
pub(crate) const OWNED_DIGESTS_CLAIM_FILE: &str = ".rwv-owned-digests.lock";

/// Outcome of comparing on-disk content against the recorded digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnedDigestCheck {
    /// No digest recorded for this file: the ledger is absent, or it is
    /// present and holds no entry for this name. Both are legitimate — a fresh
    /// or pre-upgrade workspace, or a file this weave has never stamped — so
    /// the axis is skipped silently, never errored.
    ///
    /// An unreadable ledger also reaches here, because from one file's point
    /// of view there is indeed no digest to compare against. It is not a
    /// legitimate state, and it is reported on its own channel rather than
    /// through this variant — a ledger nobody can parse cannot say which files
    /// it tracked, so there is no per-file finding to raise.
    NotRecorded,
    /// On-disk content matches the last rwv-accepted generation.
    Matches,
    /// On-disk content differs from the last rwv-accepted generation.
    Differs,
}

/// SHA-256 of `content`, in the `"sha256:<hex>"` self-describing form the
/// state file records.
fn owned_digest(content: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(7 + digest.len() * 2);
    hex.push_str("sha256:");
    for b in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// One ledger entry: the digest of the content rwv accepted, and — when rwv
/// generated it rather than adopted it — the digests of the inputs that
/// generation read.
///
/// Untagged, so the pre-amendment spelling (a bare digest string) still parses.
/// Such an entry says what was accepted and nothing about what produced it,
/// which is why it can only ever read as stale: an unknown provenance is not
/// evidence of a current one. The next generation rewrites it in the attested
/// shape, so the condition heals itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum LedgerEntry {
    /// The accepted content's digest, with no record of its inputs.
    Adopted(String),
    /// The accepted content's digest and the inputs generation read, as
    /// workspace-relative path to digest.
    Generated {
        digest: String,
        inputs: BTreeMap<String, String>,
    },
}

impl LedgerEntry {
    /// The digest of the content rwv accepted.
    pub fn digest(&self) -> &str {
        match self {
            Self::Adopted(digest) | Self::Generated { digest, .. } => digest,
        }
    }

    /// The inputs generation read, or `None` when the entry does not record
    /// them.
    pub fn inputs(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            Self::Adopted(_) => None,
            Self::Generated { inputs, .. } => Some(inputs),
        }
    }
}

/// Read the ledger from `state_dir`. Absence and corruption both yield an
/// empty map, so every caller here answers "nothing is attested".
///
/// That is the right answer for absence and the wrong one for corruption, and
/// the difference is not visible from the map: two doctor axes decide from
/// what this returns, so an unreadable file takes them silent rather than
/// leaving them unaffected. [`unreadable_ledger`] is the channel that reports
/// it, and it is what keeps this total read from being a claim that nothing is
/// wrong.
fn read_owned_digests(state_dir: &Path) -> BTreeMap<String, LedgerEntry> {
    let path = state_dir.join(OWNED_DIGESTS_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Where `dir`'s ledger lives.
pub fn ledger_path(dir: &Path) -> std::path::PathBuf {
    dir.join(OWNED_DIGESTS_FILE)
}

/// Why `dir`'s ledger cannot be read as a ledger, when a file is there but is
/// not one.
///
/// Absence is not a failure — a fresh or pre-upgrade weave has no ledger, and
/// a file this weave never stamped has no entry. Both are legitimate and stay
/// silent. Bytes that are present and are not a ledger are neither: something
/// wrote over it, or a write was cut short, and every axis reading it is
/// answering from an empty map while reporting nothing.
///
/// A read error other than absence is reported here too. It is the same
/// situation for the axes downstream: the file's content did not reach them.
pub fn unreadable_ledger(dir: &Path) -> Option<String> {
    let path = ledger_path(dir);
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(format!("read failed: {e}")),
        Ok(text) => serde_json::from_str::<BTreeMap<String, LedgerEntry>>(&text)
            .err()
            .map(|e| format!("parse failed: {e}")),
    }
}

/// An exclusive claim on one directory's ledger, released when it drops.
///
/// Atomic publication settles what a reader sees; it does nothing for what a
/// writer loses. Two rwv processes that each read the ledger, insert their own
/// entry and publish the whole map leave only the later one's entry behind,
/// and the earlier stamp is gone with no error anywhere. `link(2)` onto an
/// already-written inode is the exclusion: exactly one caller creates the
/// claim, and the loser waits for it to go rather than proceeding.
///
/// A crash between acquisition and drop leaves the claim behind and every
/// later stamp in that directory refuses until someone removes it. That is
/// the cost of an exclusion with no daemon to expire it, and it is paid in
/// full view — the alternative, treating an old claim as abandoned, is a
/// second writer stamping over a first that is merely slow.
struct LedgerClaim {
    dir: std::path::PathBuf,
}

impl LedgerClaim {
    /// Claim `dir`'s ledger, waiting up to [`crate::durable_file::CLAIM_WAIT`]
    /// for a peer holding
    /// it, then refusing with the path to remove.
    fn acquire(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join(OWNED_DIGESTS_CLAIM_FILE);
        let holder = format!("pid {}\n", std::process::id());
        let deadline = std::time::Instant::now() + crate::durable_file::CLAIM_WAIT;
        loop {
            match crate::durable_file::create_new(&path, holder.as_bytes()) {
                Ok(()) => {
                    return Ok(Self {
                        dir: dir.to_path_buf(),
                    })
                }
                Err(crate::durable_file::CreateNewError::AlreadyExists) => {
                    if std::time::Instant::now() >= deadline {
                        let held_by = std::fs::read_to_string(&path)
                            .map(|s| s.trim().to_string())
                            .unwrap_or_else(|_| "an unreadable holder".to_string());
                        anyhow::bail!(
                            "another rwv still holds the owned-digest ledger of {dir} \
                             ({held_by}). If no rwv is running, delete {claim} and rerun.",
                            dir = crate::path_spelling::operator_path(dir),
                            claim = crate::path_spelling::operator_path(&path),
                        );
                    }
                    std::thread::sleep(crate::durable_file::CLAIM_POLL);
                }
                Err(crate::durable_file::CreateNewError::Io(e)) => {
                    return Err(anyhow::Error::new(e).context(format!(
                        "failed to claim the owned-digest ledger of {}",
                        dir.display()
                    )));
                }
            }
        }
    }
}

impl Drop for LedgerClaim {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.dir.join(OWNED_DIGESTS_CLAIM_FILE));
    }
}

/// Whether a ledger edit changed the map, and so whether the whole-file write
/// happens at all.
enum LedgerEdit {
    Changed,
    Unchanged,
}

/// Read `dir`'s ledger, apply `edit`, publish the result — the whole sequence
/// under one [`LedgerClaim`], which is what makes it a read-modify-write
/// rather than three steps a peer can interleave with.
fn edit_owned_digests(
    dir: &Path,
    edit: impl FnOnce(&mut BTreeMap<String, LedgerEntry>) -> LedgerEdit,
) -> anyhow::Result<()> {
    let claim = LedgerClaim::acquire(dir)?;
    let mut entries = read_owned_digests(dir);
    match edit(&mut entries) {
        LedgerEdit::Changed => write_owned_digests(&claim, &entries),
        LedgerEdit::Unchanged => Ok(()),
    }
}

/// Write `entries` as the ledger of the directory `claim` covers, and keep the
/// machine-local file out of VCS. Every writer goes through here so the
/// on-disk shape has one author, and the claim parameter is why no writer can
/// reach it without holding the exclusion.
fn write_owned_digests(
    claim: &LedgerClaim,
    entries: &BTreeMap<String, LedgerEntry>,
) -> anyhow::Result<()> {
    let dir = claim.dir.as_path();
    let path = dir.join(OWNED_DIGESTS_FILE);
    let json = serde_json::to_string_pretty(entries)
        .with_context(|| format!("serializing owned-digest state for {}", path.display()))?;
    crate::state_file::StateFile::OwnedDigests
        .publish_in(dir, json.as_bytes())
        .with_context(|| format!("writing owned-digest state {}", path.display()))?;
    // Best effort — an ignore failure must never fail the stamp itself. Under
    // the claim, so two rwv processes cannot each append one name and publish
    // an ignore surface missing the other's.
    for name in [OWNED_DIGESTS_FILE, OWNED_DIGESTS_CLAIM_FILE] {
        let _ = workweave_index::ensure_ignored_in_dir(dir, name);
    }
    Ok(())
}

/// Record `content`'s digest for `file_name` in `dir`'s state file, creating
/// it if needed and preserving other files' entries.
///
/// Call this at the moment rwv ACCEPTS a generation — the end of the
/// activation hook that ran the ecosystem generator. The stamp is what makes
/// [`check_owned_digest`]'s mismatch axis meaningful: "differs from the last
/// rwv-accepted generation".
///
/// An unparseable existing state file is replaced wholesale (its entries are
/// unreadable anyway; the fresh stamp is the only recovery).
pub fn stamp_owned_digest(dir: &Path, file_name: &str, content: &[u8]) -> anyhow::Result<()> {
    edit_owned_digests(dir, |entries| {
        entries.insert(
            file_name.to_string(),
            LedgerEntry::Adopted(owned_digest(content)),
        );
        LedgerEdit::Changed
    })
}

/// Record `content` as the generation rwv produced for `file_name` in `dir`,
/// together with the digests of the inputs that generation read — or refuse,
/// when one of them moved between [`ObservedInputs::observe`] and here.
///
/// The distinction from [`stamp_owned_digest`] is not bookkeeping detail. That
/// one accepts bytes; this one attests a derivation. Recording inputs beside
/// bytes rwv did not derive would claim a provenance that does not exist, and
/// the claim would then read as freshness for as long as those inputs sat
/// still.
///
/// Refusing writes nothing: the generated file stays as the generator left it,
/// and a generation nobody recorded is what [`stale_generations`] reports until
/// a rerun earns the record — where a stamp taken across the disagreement would
/// be silent forever.
pub fn stamp_owned_generation(
    dir: &Path,
    file_name: &str,
    content: &[u8],
    inputs: ObservedInputs,
) -> anyhow::Result<()> {
    let (at_record, moved) = inputs.reread();
    if !moved.is_empty() {
        let listed = moved
            .iter()
            .map(|path| format!("\n  {path}"))
            .collect::<String>();
        anyhow::bail!(
            "refusing to record {} as generated: {} input(s) it was derived \
             from changed while it was being generated, so the record would \
             attest a derivation that did not happen:{listed}\n\
             Nothing was recorded. Re-run this command to derive the file from \
             the inputs as they now stand — where what the generator left on \
             disk differs from the last generation rwv accepted, it stops to \
             ask which of the two to keep before regenerating.",
            dir.join(file_name).display(),
            moved.len()
        );
    }
    edit_owned_digests(dir, |entries| {
        entries.insert(
            file_name.to_string(),
            LedgerEntry::Generated {
                digest: owned_digest(content),
                inputs: at_record,
            },
        );
        LedgerEdit::Changed
    })
}

/// The inputs of a generation, read before the generator that consumes them
/// runs.
///
/// rwv never locks the working tree, so an editor, a build, a landing sync or a
/// second rwv may write one of these files while the generator is running or
/// while its output is on its way to the ledger. This carries the reading taken
/// before the generation so [`stamp_owned_generation`] can take a second one at
/// the moment of recording and refuse across a disagreement: what the ledger
/// claims is then always something rwv observed, whoever else was writing.
///
/// The window before the first reading is not covered and does not need to be —
/// what precedes the observation is what the generation ran on.
pub struct ObservedInputs {
    project_dir: std::path::PathBuf,
    project: crate::manifest::ProjectName,
    workspace_root: std::path::PathBuf,
    at_observation: BTreeMap<String, String>,
}

impl ObservedInputs {
    /// Read what a generation in `project_dir` is about to consume.
    pub fn observe(
        project_dir: &Path,
        project: &crate::manifest::ProjectName,
        workspace_root: &Path,
    ) -> Self {
        Self {
            at_observation: generation_inputs(project_dir, project, workspace_root),
            project_dir: project_dir.to_path_buf(),
            project: project.clone(),
            workspace_root: workspace_root.to_path_buf(),
        }
    }

    /// The inputs as they stand now, and which of them moved since the
    /// observation.
    fn reread(&self) -> (BTreeMap<String, String>, Vec<String>) {
        let now = generation_inputs(&self.project_dir, &self.project, &self.workspace_root);
        let moved = moved_inputs(&self.at_observation, &now);
        (now, moved)
    }
}

/// The paths `before` and `after` disagree about, keyed on the union and
/// sorted.
///
/// Both directions: an input that has appeared since `before` moved the
/// derivation just as much as one whose bytes changed, and an input that has
/// gone away is not evidence that what it produced still holds.
fn moved_inputs(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<String> {
    before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// The inputs a generation reads, as workspace-relative path to digest, for the
/// project `output_dir` belongs to.
///
/// The project manifest decides membership and integration configuration; the
/// rwv lock decides which commit of each member is on disk. Both are the
/// weave's own record of itself, and both are files rwv writes.
///
/// **What is deliberately absent: the members' own manifests.** A member
/// `Cargo.toml` is a genuine input to the `Cargo.lock` cargo-workspace
/// produces, so a member manifest edited in place under a still lock makes the
/// generated file wrong while every digest here still matches.
///
/// What keeps that from being a hole is that no route from such an edit to a
/// quiet doctor avoids moving the lock. Uncommitted, the member reports
/// `working-tree-drift`; committed, its HEAD leaves the lock behind and the
/// project reports `stale-lock`; and the `rwv lock` that settles either one
/// moves an input recorded here, at which point the staleness axis fires.
/// Hashing every member manifest would grow the ledger with membership,
/// re-hash every member on every doctor run, and still miss an edit to a
/// source file — so the axis stops at the join point every route passes
/// through.
///
/// **What IS included: `path =` dependency target manifests that resolve
/// OUTSIDE the member set.** These have no commit in `rwv.lock` to pin them —
/// the argument above supplies no join point — so their `Cargo.toml` is
/// hashed directly. Editing one and re-running `rwv materialize` would
/// otherwise regenerate `Cargo.lock` and re-stamp within one call, absorbing
/// the change with no signal on any axis; the digest input is the anchor that
/// makes the staleness surface fire between the edit and the next generation.
/// Path targets INSIDE the member set stay absent — the member's own commit
/// still supplies the join point, unchanged.
///
/// **Member SOURCE files are absent for a structural reason, not an
/// oversight.** `cargo generate-lockfile` / `cargo fetch` never execute a
/// build script during resolution, so nothing a `build.rs` or `include!`
/// reads — inside a member or outside every member and every git repo this
/// weave tracks — can move `Cargo.lock`'s bytes. There is no channel here
/// for this map to fail to cover.
///
/// **A digest here describes one instant, and the pairing with the bytes it is
/// stamped beside is [`ObservedInputs`]'s job, not this map's.** Widening the
/// map would not help: an actor writing a tracked input while the generator
/// runs moves an input this map already covers, which is why the guard is a
/// second reading rather than another path.
fn generation_inputs(
    project_dir: &Path,
    project: &crate::manifest::ProjectName,
    workspace_root: &Path,
) -> BTreeMap<String, String> {
    let project_rel = crate::workspace::project_rel_path(project.as_str());
    let mut inputs: BTreeMap<String, String> = [
        crate::manifest::Manifest::FILE_NAME,
        crate::manifest::LockFile::FILE_NAME,
    ]
    .into_iter()
    .filter_map(|name| {
        let content = std::fs::read(project_dir.join(name)).ok()?;
        Some((format!("{project_rel}/{name}"), owned_digest(&content)))
    })
    .collect();
    inputs.extend(non_member_path_dep_manifest_digests(workspace_root));
    inputs
}

/// Each `Cargo.toml` a `path =` dependency chain from the workspace members
/// lands on that is NOT itself a member, keyed by a stable workspace-relative
/// spelling of its path.
///
/// The walker is deliberately narrow to the shape it exists to close:
/// cargo-workspace, `path =` deps in the standard `[dependencies]`,
/// `[dev-dependencies]`, `[build-dependencies]` tables (and their
/// `[target.<cfg>.<key>]` variants). It follows deps transitively so a chain
/// through several outside directories is not silently truncated — cargo
/// resolves through every hop, and a hole at hop 2 would be the same bug
/// again one step deeper.
///
/// A missing / unparseable workspace manifest yields an empty map: this is
/// input to a bookkeeping stamp, not the manifest gate, and other axes report
/// the same finding as their own error.
///
/// Symlink resolution is best-effort: `std::fs::canonicalize` is used to
/// classify a target as inside-vs-outside the member set (a symlink into a
/// member must count as inside), and to key the ledger by a canonical
/// absolute path made relative to `workspace_root`. When canonicalization
/// fails the raw path is used, which keeps the map deterministic on
/// filesystems that reject the syscall.
fn non_member_path_dep_manifest_digests(workspace_root: &Path) -> BTreeMap<String, String> {
    let ws_manifest = workspace_root.join("Cargo.toml");
    let Ok(ws_text) = std::fs::read_to_string(&ws_manifest) else {
        return BTreeMap::new();
    };
    let Ok(ws_doc) = ws_text.parse::<toml_edit::DocumentMut>() else {
        return BTreeMap::new();
    };
    let members = workspace_members(&ws_doc);
    let member_dirs: BTreeSet<std::path::PathBuf> = members
        .iter()
        .map(|m| canonicalize_or_owned(&workspace_root.join(m)))
        .collect();

    let ws_root_canon = canonicalize_or_owned(workspace_root);

    let mut queue: Vec<std::path::PathBuf> = members
        .iter()
        .map(|m| workspace_root.join(m).join("Cargo.toml"))
        .collect();
    let mut seen: BTreeSet<std::path::PathBuf> = BTreeSet::new();
    let mut outside: BTreeMap<String, String> = BTreeMap::new();

    while let Some(manifest_path) = queue.pop() {
        let manifest_canon = canonicalize_or_owned(&manifest_path);
        if !seen.insert(manifest_canon.clone()) {
            continue;
        }
        let manifest_dir = manifest_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| workspace_root.to_path_buf());
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
            continue;
        };

        for target_manifest in path_dep_targets(&doc, &manifest_dir) {
            let target_canon = canonicalize_or_owned(&target_manifest);
            let target_dir_canon = target_canon
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| target_canon.clone());
            let inside_member = member_dirs
                .iter()
                .any(|m| target_dir_canon == *m || target_dir_canon.starts_with(m));
            if inside_member {
                // Covered by the member's own commit pin — the outer doc
                // spells out the argument. Still enqueue for the walk so a
                // path-dep chain that leaves the member set later is not
                // truncated at an intra-member hop.
                queue.push(target_manifest);
                continue;
            }
            let Ok(content) = std::fs::read(&target_canon) else {
                queue.push(target_manifest);
                continue;
            };
            let key = workspace_relative_key(&ws_root_canon, workspace_root, &target_canon);
            outside.insert(key, owned_digest(&content));
            queue.push(target_manifest);
        }
    }

    outside
}

/// `[workspace].members` as declared.
///
/// A missing `[workspace]` table or `members` array yields an empty list:
/// this is what the cargo-workspace integration writes, so its absence means
/// no members to walk from. Glob entries are skipped — a pattern that
/// matches nothing is not an error, so a glob cannot be "missing", and the
/// bug shape this axis exists to close is measured against named entries.
fn workspace_members(doc: &toml_edit::DocumentMut) -> Vec<String> {
    let Some(members) = doc
        .get("workspace")
        .and_then(|w| w.as_table())
        .and_then(|t| t.get("members"))
        .and_then(|m| m.as_array())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in members.iter().filter_map(|v| v.as_str()) {
        // Glob members are skipped for the same reason `unfetched_members`
        // skips them: a pattern that matches nothing is not an error, so a
        // glob cannot be "missing" — and a walker over glob matches is
        // wider than this axis wants for v1. Named-entry members carry the
        // shape the bug fixture measures.
        if entry.contains(['*', '?', '[']) {
            continue;
        }
        out.push(entry.to_string());
    }
    out.sort();
    out.dedup();
    out
}

/// Absolute paths to `Cargo.toml` files named by `path = "<rel>"` entries in
/// `doc`, across the standard dep tables and their `[target.<cfg>.<key>]`
/// variants. Missing / non-string `path` values are dropped.
fn path_dep_targets(doc: &toml_edit::DocumentMut, manifest_dir: &Path) -> Vec<std::path::PathBuf> {
    const DEP_KEYS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut targets = Vec::new();

    // Direct dep tables.
    for key in DEP_KEYS {
        collect_path_deps(doc.get(key), manifest_dir, &mut targets);
    }

    // `[target.<cfg>.<key>]` — cargo honors path deps here just like the
    // top-level tables, so a silence here would be the same bug behind a
    // `cfg`.
    if let Some(target) = doc.get("target").and_then(|t| t.as_table()) {
        for (_cfg, cfg_item) in target.iter() {
            let cfg_table = cfg_item.as_table();
            for key in DEP_KEYS {
                let sub = cfg_table.and_then(|t| t.get(key));
                collect_path_deps(sub, manifest_dir, &mut targets);
            }
        }
    }

    targets
}

/// Push every `Cargo.toml` an entry in a dep table's `path = "<rel>"` value
/// resolves to onto `out`. Silent on `path` absent, non-string, or the entry
/// not being a table-shaped dep at all.
fn collect_path_deps(
    deps_item: Option<&toml_edit::Item>,
    manifest_dir: &Path,
    out: &mut Vec<std::path::PathBuf>,
) {
    let Some(deps) = deps_item.and_then(|i| i.as_table()) else {
        return;
    };
    for (_dep_name, dep_item) in deps.iter() {
        let path_str = if let Some(inline) = dep_item.as_inline_table() {
            inline.get("path").and_then(|v| v.as_str())
        } else if let Some(table) = dep_item.as_table() {
            table.get("path").and_then(|i| i.as_str())
        } else {
            None
        };
        if let Some(rel) = path_str {
            out.push(manifest_dir.join(rel).join("Cargo.toml"));
        }
    }
}

/// Best-effort canonicalize; fall back to the input on error so unmapped
/// filesystems and missing files still yield a deterministic key.
fn canonicalize_or_owned(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A stable workspace-relative string spelling of `target`. Paths inside
/// `workspace_root` render without a `..` prefix (using forward slashes);
/// paths outside are spelled with the leading `..` segments that get from
/// `workspace_root` to `target`. Always forward-slash for cross-platform
/// determinism (matches the rest of the ledger's key format).
fn workspace_relative_key(ws_root_canon: &Path, ws_root_raw: &Path, target_canon: &Path) -> String {
    // Prefer the canonicalized workspace root for stripping, so a symlinked
    // workspace root does not double-render its own segments.
    if let Ok(rel) = target_canon.strip_prefix(ws_root_canon) {
        return rel.to_string_lossy().replace('\\', "/");
    }
    if let Ok(rel) = target_canon.strip_prefix(ws_root_raw) {
        return rel.to_string_lossy().replace('\\', "/");
    }
    // Not under the workspace root — compute a `../..` spelling.
    let up = ws_root_canon
        .ancestors()
        .enumerate()
        .find_map(|(depth, ancestor)| {
            target_canon
                .strip_prefix(ancestor)
                .ok()
                .map(|rel| (depth, rel))
        });
    if let Some((depth, rel)) = up {
        let mut s = String::new();
        for i in 0..depth {
            if i > 0 {
                s.push('/');
            }
            s.push_str("..");
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if !rel_str.is_empty() {
            if !s.is_empty() {
                s.push('/');
            }
            s.push_str(&rel_str);
        }
        return s;
    }
    // Truly disjoint (different filesystem root, e.g.). Fall back to the
    // absolute path; still deterministic, just less pretty.
    target_canon.to_string_lossy().replace('\\', "/")
}

/// Drop `file_name`'s entry from `dir`'s state file, leaving the other
/// entries. A no-op when there is no entry to drop.
///
/// The counterpart of [`stamp_owned_digest`], for the moment the attested file
/// stops existing: an attestation of what is not there describes nothing, and
/// left behind it is a drift report on a file no verb can produce.
pub fn forget_owned_digest(dir: &Path, file_name: &str) -> anyhow::Result<()> {
    edit_owned_digests(dir, |entries| match entries.remove(file_name) {
        Some(_) => LedgerEdit::Changed,
        None => LedgerEdit::Unchanged,
    })
}

/// The files `dir`'s ledger attests, in ledger order.
pub fn attested_owned_files(dir: &Path) -> Vec<String> {
    read_owned_digests(dir).into_keys().collect()
}

/// Compare `content` against the digest `dir`'s state file records for
/// `file_name`.
///
/// Total (never errors): a missing state file, a missing entry, or an
/// unparseable state file all yield [`OwnedDigestCheck::NotRecorded`] — the
/// caller skips the axis silently. The first two are the backward-compat
/// contract for pre-upgrade workspaces that have generated files but no digest
/// state. The third is a fault, and the silence here is only sound because
/// [`unreadable_ledger`] reports it once for the ledger rather than once per
/// file it can no longer enumerate.
pub fn check_owned_digest(dir: &Path, file_name: &str, content: &[u8]) -> OwnedDigestCheck {
    match read_owned_digests(dir).get(file_name) {
        None => OwnedDigestCheck::NotRecorded,
        Some(recorded) if recorded.digest() == owned_digest(content) => OwnedDigestCheck::Matches,
        Some(_) => OwnedDigestCheck::Differs,
    }
}

/// Reproduce `source_dir`'s attested owned generated files in `dest_dir`:
/// every file the source's state file names, plus the state file itself.
/// Returns the names carried.
///
/// The ledger is the manifest of what to carry. A file rwv has accepted a
/// generation for cannot be reproduced by re-running the generator — the
/// ecosystem tool resolves against a registry that moves — so a copy is the
/// only way a second checkout can hold the same content.
///
/// Recorded digests are carried verbatim rather than recomputed: a source
/// sitting on drift it never accepted then reports that same drift on the
/// copy, instead of the copy presenting it as an accepted generation.
pub fn carry_attested_owned_files(
    source_dir: &Path,
    dest_dir: &Path,
) -> anyhow::Result<Vec<String>> {
    let mut carried = BTreeMap::new();
    for (name, entry) in read_owned_digests(source_dir) {
        let Ok(bytes) = std::fs::read(source_dir.join(&name)) else {
            continue;
        };
        let dest = dest_dir.join(&name);
        std::fs::write(&dest, &bytes)
            .with_context(|| format!("copying attested owned file to {}", dest.display()))?;
        carried.insert(name, entry);
    }
    if carried.is_empty() {
        return Ok(vec![]);
    }
    let names: Vec<String> = carried.keys().cloned().collect();
    edit_owned_digests(dest_dir, move |entries| {
        *entries = carried;
        LedgerEdit::Changed
    })?;
    Ok(names)
}

/// An attested owned file whose current bytes are not the ones rwv accepted,
/// carrying the content that was read to reach that verdict.
///
/// The bytes travel with the verdict because the caller that adopts them must
/// attest what was compared: a second read could stamp content the drift check
/// never saw.
pub struct DriftedOwnedFile {
    pub name: String,
    pub content: Vec<u8>,
}

/// Every attested owned file in `dir` whose on-disk bytes differ from the
/// digest recorded for it, in ledger order.
///
/// The ledger is the enumeration, not the integrations' declared file sets: an
/// entry exists only where some generator's output was accepted, and that
/// acceptance is the thing drift is measured against. An entry whose file is
/// gone is absent from the result — a missing generated file is regenerable
/// and reports on its own axis.
pub fn drifted_attested_owned_files(dir: &Path) -> Vec<DriftedOwnedFile> {
    read_owned_digests(dir)
        .into_iter()
        .filter_map(|(name, recorded)| {
            let content = std::fs::read(dir.join(&name)).ok()?;
            (owned_digest(&content) != recorded.digest())
                .then_some(DriftedOwnedFile { name, content })
        })
        .collect()
}

/// Generated state whose attested inputs no longer describe the checkout.
///
/// One read produces both renderings — the operator's sentence and the typed
/// advisory — because two surfaces that must agree cannot each ask the
/// question separately and still be guaranteed the same answer.
pub struct StaleGeneration {
    /// The generated file, workspace-relative.
    pub generated: String,
    /// The attested inputs that no longer match, workspace-relative and
    /// sorted. Empty when the entry records no inputs at all.
    pub moved_inputs: Vec<String>,
}

impl StaleGeneration {
    /// Whether the entry predates input attestation, in which case nothing is
    /// known to have moved and nothing is known to have stayed.
    pub fn provenance_unknown(&self) -> bool {
        self.moved_inputs.is_empty()
    }
}

/// Attested generated files in `dir` whose recorded inputs no longer match what
/// is on disk under `workspace_root`, in ledger order.
///
/// Present state on both sides: the ledger says which inputs a generation read
/// and what they hashed to, and this re-hashes them now. Nothing is
/// regenerated, no other checkout is consulted, and no history is needed — a
/// workweave answers for itself.
///
/// An entry with no recorded inputs is stale. It is the honest answer: rwv
/// accepted those bytes without recording what produced them, so it cannot say
/// they still follow from anything.
pub fn stale_generations(
    project_dir: &Path,
    project: &crate::manifest::ProjectName,
    workspace_root: &Path,
) -> Vec<StaleGeneration> {
    let current = generation_inputs(project_dir, project, workspace_root);
    let project_rel = crate::workspace::project_rel_path(project.as_str());
    read_owned_digests(project_dir)
        .into_iter()
        .filter_map(|(name, entry)| {
            let generated = format!("{project_rel}/{name}");
            let Some(recorded) = entry.inputs() else {
                return Some(StaleGeneration {
                    generated,
                    moved_inputs: Vec::new(),
                });
            };
            let moved = moved_inputs(recorded, &current);
            (!moved.is_empty()).then_some(StaleGeneration {
                generated,
                moved_inputs: moved,
            })
        })
        .collect()
}

/// The canonical DRIFT-state issue for a **fully-owned** generated file whose
/// on-disk content no longer matches the digest recorded when rwv last
/// accepted a generation ([`stamp_owned_digest`]).
///
/// Report-not-mandate: the ecosystem tool rewriting its own lockfile is
/// legitimate behavior the operator should SEE, not an error that fails
/// doctor — so the severity is `Warning`.
///
/// `safe_to_fix` is **false**, and stays false for a stronger reason than
/// caution: the two exits destroy opposite things. Regenerating discards
/// content the operator may have produced on purpose; adopting attests content
/// that may be an accident. `--fix` has no way to know which, so choosing
/// either on the operator's behalf is the laundering the consent flags exist to
/// prevent, and the finding names them instead.
///
/// Both named remedies run in the checkout where this fires. Its origin is a
/// workweave carrying an attestation it never re-earned, and `rwv activate` is
/// refused there.
pub fn fully_owned_digest_mismatch_issue(name: &str, path: &Path) -> Issue {
    Issue {
        integration: name.to_string(),
        severity: Severity::Warning,
        message: format!(
            "{name} generated file has drift: {}; content differs from the last \
             rwv-accepted generation. Run `rwv materialize --adopt-drifted` to \
             record the current content as the accepted generation, \
             `rwv materialize --regenerate-drifted` to discard it and regenerate \
             from the current inputs, or restore the file to the recorded content",
            path.display()
        ),
        kind: IssueKind::ManagedFileDrift,
        safe_to_fix: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn check_without_state_file_is_not_recorded() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            check_owned_digest(tmp.path(), "Cargo.lock", b"anything"),
            OwnedDigestCheck::NotRecorded,
            "absent state file must skip silently (backward compat)"
        );
    }

    #[test]
    fn check_without_entry_is_not_recorded() {
        let tmp = TempDir::new().unwrap();
        stamp_owned_digest(tmp.path(), "uv.lock", b"other file").unwrap();
        assert_eq!(
            check_owned_digest(tmp.path(), "Cargo.lock", b"anything"),
            OwnedDigestCheck::NotRecorded,
            "state file present but no entry for this file must skip silently"
        );
    }

    #[test]
    fn stamp_then_check_same_content_matches() {
        let tmp = TempDir::new().unwrap();
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"version = 3\n").unwrap();
        assert_eq!(
            check_owned_digest(tmp.path(), "Cargo.lock", b"version = 3\n"),
            OwnedDigestCheck::Matches
        );
    }

    #[test]
    fn stamp_then_check_mutated_content_differs() {
        let tmp = TempDir::new().unwrap();
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"version = 3\n").unwrap();
        assert_eq!(
            check_owned_digest(tmp.path(), "Cargo.lock", b"version = 4\n"),
            OwnedDigestCheck::Differs,
            "any byte-level mutation must be visible"
        );
    }

    #[test]
    fn restamp_updates_recorded_digest() {
        let tmp = TempDir::new().unwrap();
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"old").unwrap();
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"new").unwrap();
        assert_eq!(
            check_owned_digest(tmp.path(), "Cargo.lock", b"new"),
            OwnedDigestCheck::Matches,
            "re-stamp must accept the new content"
        );
        assert_eq!(
            check_owned_digest(tmp.path(), "Cargo.lock", b"old"),
            OwnedDigestCheck::Differs,
            "the previously-recorded digest must be replaced"
        );
    }

    #[test]
    fn stamp_preserves_other_entries() {
        let tmp = TempDir::new().unwrap();
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"cargo bytes").unwrap();
        stamp_owned_digest(tmp.path(), "uv.lock", b"uv bytes").unwrap();
        assert_eq!(
            check_owned_digest(tmp.path(), "Cargo.lock", b"cargo bytes"),
            OwnedDigestCheck::Matches,
            "stamping a second file must not clobber the first entry"
        );
        assert_eq!(
            check_owned_digest(tmp.path(), "uv.lock", b"uv bytes"),
            OwnedDigestCheck::Matches
        );
    }

    #[test]
    fn corrupt_state_file_is_reported_and_stamp_recovers() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(OWNED_DIGESTS_FILE), "not json {{{").unwrap();
        assert_eq!(
            check_owned_digest(tmp.path(), "Cargo.lock", b"x"),
            OwnedDigestCheck::NotRecorded,
            "a ledger nobody can parse records no digest for any file, so the \
             per-file question still has to be total"
        );
        assert!(
            unreadable_ledger(tmp.path()).is_some(),
            "and the fault has to be visible somewhere — the per-file answer \
             above is indistinguishable from a weave that never stamped"
        );
        // A fresh stamp rewrites the file wholesale — self-healing.
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"x").unwrap();
        assert_eq!(
            check_owned_digest(tmp.path(), "Cargo.lock", b"x"),
            OwnedDigestCheck::Matches
        );
        assert_eq!(
            unreadable_ledger(tmp.path()),
            None,
            "and the fault clears with it"
        );
    }

    #[test]
    fn absence_is_not_a_fault() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(unreadable_ledger(tmp.path()), None, "no ledger at all");
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"x").unwrap();
        assert_eq!(
            unreadable_ledger(tmp.path()),
            None,
            "a readable ledger holding no entry for some other file"
        );
        assert_eq!(
            check_owned_digest(tmp.path(), "uv.lock", b"y"),
            OwnedDigestCheck::NotRecorded
        );
    }

    #[test]
    fn state_file_is_json_map_with_sha256_prefixed_digests() {
        let tmp = TempDir::new().unwrap();
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"content").unwrap();
        let text = std::fs::read_to_string(tmp.path().join(OWNED_DIGESTS_FILE)).unwrap();
        let map: BTreeMap<String, String> = serde_json::from_str(&text).unwrap();
        let digest = map.get("Cargo.lock").expect("entry must exist");
        assert!(
            digest.starts_with("sha256:"),
            "digest must be self-describing: {digest}"
        );
        assert_eq!(
            digest.len(),
            7 + 64,
            "sha256 hex digest must be 64 chars after the prefix: {digest}"
        );
    }

    /// The ledger's location is a function of the named directory alone.
    ///
    /// A symlink at `<dir>/<file>` whose target sits in another directory
    /// is the one fixture that separates that from anchoring the ledger
    /// beside the generated file's real inode — the two agree everywhere
    /// else, because the kernel resolves symlinked *directory* components
    /// on the way to `<dir>/.rwv-owned-digests` regardless. Anchoring to
    /// the target would put the ledger where none of this module's readers
    /// look, starting with [`carry_attested_owned_files`].
    #[test]
    fn ledger_anchors_to_the_named_directory() {
        let tmp = TempDir::new().unwrap();
        let project_dir = tmp.path().join("projects/web-app");
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("Cargo.lock"), b"version = 3\n").unwrap();
        crate::symlink::create(
            &elsewhere.join("Cargo.lock"),
            &project_dir.join("Cargo.lock"),
            crate::symlink::LinkTarget::File,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(project_dir.join("Cargo.lock")).unwrap(),
            b"version = 3\n",
            "fixture must resolve through the symlink to elsewhere's content"
        );

        stamp_owned_digest(&project_dir, "Cargo.lock", b"version = 3\n").unwrap();

        assert!(
            project_dir.join(OWNED_DIGESTS_FILE).exists(),
            "the stamp must write the ledger in the directory it was given"
        );
        assert!(
            !elsewhere.join(OWNED_DIGESTS_FILE).exists(),
            "and nowhere else — the file's target directory is not the caller's"
        );
        assert_eq!(
            check_owned_digest(&project_dir, "Cargo.lock", b"version = 3\n"),
            OwnedDigestCheck::Matches
        );
        assert_eq!(
            check_owned_digest(&elsewhere, "Cargo.lock", b"version = 3\n"),
            OwnedDigestCheck::NotRecorded,
            "the check must read the same directory the stamp wrote"
        );
    }

    /// [`carry_attested_owned_files`] takes the directory straight and has
    /// no file path to resolve, so a stamp that resolved one would put the
    /// ledger somewhere the fork cannot read: the source would hand over
    /// nothing and the copy would arrive unattested, silently.
    #[test]
    fn carry_reads_the_ledger_the_stamp_wrote_for_a_symlinked_file() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let elsewhere = tmp.path().join("elsewhere");
        let dest = tmp.path().join("dest");
        for dir in [&source, &elsewhere, &dest] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(elsewhere.join("Cargo.lock"), b"version = 3\n").unwrap();
        crate::symlink::create(
            &elsewhere.join("Cargo.lock"),
            &source.join("Cargo.lock"),
            crate::symlink::LinkTarget::File,
        )
        .unwrap();
        stamp_owned_digest(&source, "Cargo.lock", b"version = 3\n").unwrap();

        assert_eq!(
            carry_attested_owned_files(&source, &dest).unwrap(),
            vec!["Cargo.lock"]
        );
        assert_eq!(
            std::fs::read(dest.join("Cargo.lock")).unwrap(),
            b"version = 3\n"
        );
        assert_eq!(
            check_owned_digest(&dest, "Cargo.lock", b"version = 3\n"),
            OwnedDigestCheck::Matches
        );
    }

    #[test]
    fn stamp_ensures_digests_file_ignored() {
        // Non-git directory: stamp must write a .gitignore fallback so the
        // state file never appears as an untracked file in a dirty-tree
        // check. This mirrors the workweave_index::write chokepoint for
        // .rwv-workweave-index.
        let tmp = TempDir::new().unwrap();
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"version = 3\n").unwrap();

        let gitignore = tmp.path().join(".gitignore");
        assert!(
            gitignore.exists(),
            "stamp must create .gitignore when dir is not a git repo"
        );
        let content = std::fs::read_to_string(&gitignore).unwrap();
        assert!(
            content.contains(OWNED_DIGESTS_FILE),
            "stamp must ensure {OWNED_DIGESTS_FILE} is ignored: {content:?}"
        );
    }

    #[test]
    fn stamp_ignore_is_idempotent() {
        // A second stamp must not duplicate the ignore entry. Counted by whole
        // line: the claim file's name has the ledger's as a prefix, so a
        // substring count reads one entry for each of the two names.
        let tmp = TempDir::new().unwrap();
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"v1").unwrap();
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"v2").unwrap();

        let gitignore = tmp.path().join(".gitignore");
        let content = std::fs::read_to_string(&gitignore).unwrap();
        for name in [OWNED_DIGESTS_FILE, OWNED_DIGESTS_CLAIM_FILE] {
            let occurrences = content.lines().filter(|line| line.trim() == name).count();
            assert_eq!(
                occurrences, 1,
                "ignore entry for {name} must not be duplicated on re-stamp: {content:?}"
            );
        }
    }

    /// A stamp holds the claim only while it runs. Left behind, it is the
    /// wedge every later stamp in this directory would hit.
    #[test]
    fn a_finished_stamp_leaves_no_claim_behind() {
        let tmp = TempDir::new().unwrap();
        stamp_owned_digest(tmp.path(), "Cargo.lock", b"v1").unwrap();
        assert!(
            !tmp.path().join(OWNED_DIGESTS_CLAIM_FILE).exists(),
            "the claim must be released when the read-modify-write ends"
        );
        // The no-op arm of the edit takes the claim too, and must release it.
        forget_owned_digest(tmp.path(), "never-stamped.lock").unwrap();
        assert!(
            !tmp.path().join(OWNED_DIGESTS_CLAIM_FILE).exists(),
            "an edit that changes nothing must still release its claim"
        );
    }

    /// The decided behaviour for a claim whose holder died: later stamps
    /// refuse, after a bounded wait, naming the file to remove. Waiting
    /// forever hangs the verb; treating an old claim as abandoned stamps over
    /// a holder that is merely slow; failing instantly makes ordinary
    /// contention look like a fault.
    #[test]
    fn an_abandoned_claim_refuses_later_stamps_and_names_what_to_remove() {
        let tmp = TempDir::new().unwrap();
        let claim = tmp.path().join(OWNED_DIGESTS_CLAIM_FILE);
        std::fs::write(&claim, "pid 999999\n").unwrap();

        let started = std::time::Instant::now();
        let err = stamp_owned_digest(tmp.path(), "Cargo.lock", b"v1")
            .expect_err("a held claim must refuse the stamp, not drop it silently");
        let waited = started.elapsed();
        let message = format!("{err:#}");

        assert!(
            waited >= crate::durable_file::CLAIM_WAIT,
            "the refusal must come after the full wait, so ordinary contention \
             is not reported as a fault: waited {waited:?}"
        );
        assert!(
            message.contains(OWNED_DIGESTS_CLAIM_FILE),
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
            "a refused stamp must leave the holder's claim alone — clearing it \
             would make the exclusion a suggestion"
        );
        assert_eq!(
            check_owned_digest(tmp.path(), "Cargo.lock", b"v1"),
            OwnedDigestCheck::NotRecorded,
            "and must not have written the entry it refused to stamp"
        );
    }

    /// A source whose on-disk bytes have drifted from its record is the
    /// case that separates carrying the recorded digest from recomputing
    /// one: the copy must report the same drift the source does.
    #[test]
    fn carry_reproduces_content_and_the_source_verdict() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&dest).unwrap();

        std::fs::write(source.join("accepted.lock"), b"accepted bytes").unwrap();
        stamp_owned_digest(&source, "accepted.lock", b"accepted bytes").unwrap();

        let drifted = source.join("drifted.lock");
        std::fs::write(&drifted, b"stamped bytes").unwrap();
        stamp_owned_digest(&source, "drifted.lock", b"stamped bytes").unwrap();
        std::fs::write(&drifted, b"bytes written behind rwv's back").unwrap();

        let carried = carry_attested_owned_files(&source, &dest).unwrap();
        assert_eq!(carried, vec!["accepted.lock", "drifted.lock"]);

        assert_eq!(
            std::fs::read(dest.join("accepted.lock")).unwrap(),
            b"accepted bytes"
        );
        assert_eq!(
            check_owned_digest(&dest, "accepted.lock", b"accepted bytes"),
            OwnedDigestCheck::Matches
        );
        assert_eq!(
            std::fs::read(dest.join("drifted.lock")).unwrap(),
            b"bytes written behind rwv's back"
        );
        assert_eq!(
            check_owned_digest(&dest, "drifted.lock", b"bytes written behind rwv's back"),
            OwnedDigestCheck::Differs,
            "the copy must inherit the source's record, not a fresh stamp of \
             content rwv never accepted"
        );
    }

    #[test]
    fn carry_attests_only_what_it_copied() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&dest).unwrap();

        std::fs::write(source.join("Cargo.lock"), b"version = 4\n").unwrap();
        stamp_owned_digest(&source, "Cargo.lock", b"version = 4\n").unwrap();
        stamp_owned_digest(&source, "deleted.lock", b"gone").unwrap();

        let carried = carry_attested_owned_files(&source, &dest).unwrap();
        assert_eq!(carried, vec!["Cargo.lock"]);
        assert!(
            !dest.join("deleted.lock").exists(),
            "an entry whose file is gone has nothing to reproduce"
        );
        assert_eq!(
            check_owned_digest(&dest, "deleted.lock", b"gone"),
            OwnedDigestCheck::NotRecorded,
            "and the copy must not attest a file it does not hold"
        );
    }

    #[test]
    fn carry_from_a_source_with_no_record_writes_nothing() {
        let tmp = TempDir::new().unwrap();
        let source = tmp.path().join("source");
        let dest = tmp.path().join("dest");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(source.join("Cargo.lock"), b"unstamped").unwrap();

        assert!(carry_attested_owned_files(&source, &dest)
            .unwrap()
            .is_empty());
        assert!(
            !dest.join(OWNED_DIGESTS_FILE).exists(),
            "no record at the source means no record to carry"
        );
        assert!(
            !dest.join("Cargo.lock").exists(),
            "an unstamped file is not attested owned state"
        );
    }

    #[test]
    fn mismatch_issue_shape() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("Cargo.lock");
        let i = fully_owned_digest_mismatch_issue("cargo-workspace", &path);
        assert_eq!(i.integration, "cargo-workspace");
        assert_eq!(
            i.severity,
            Severity::Warning,
            "report-not-mandate: warning severity, doctor exit unchanged"
        );
        assert!(
            !i.safe_to_fix,
            "digest mismatch must NOT be auto-fixed — operator chooses an exit"
        );
        // House pattern: name the file, the state, and both exits — each
        // spelled as it is invoked, and each runnable in a workweave,
        // which is where a carried attestation puts this finding.
        assert!(i.message.contains("Cargo.lock"));
        assert!(i.message.contains("last rwv-accepted generation"));
        assert!(i.message.contains("rwv materialize --adopt-drifted"));
        assert!(i.message.contains("rwv materialize --regenerate-drifted"));
        assert!(i.message.contains("restore the file"));
        assert!(
            !i.message.contains("rwv activate"),
            "a workweave refuses that verb, so naming it is the dead end \
             this finding used to be"
        );
    }
}
