//! The checks every step is held to.
//!
//! - **stranded**: a commit some ref of the sandbox reached before the step
//!   that no ref reaches after it, with no patch-equivalent commit reachable
//!   in its place, in a repo the step does not name in `may_strand`. A reflog
//!   may still hold it; that is the case this exists to report. A project
//!   commit that changes only `rwv.lock` is derived state rwv re-creates when
//!   it relocks, and is left to the two pin checks.
//! - **unresolved pin**: an `rwv.lock` entry whose commit no ref reaches in
//!   the repository that backs that member where the lock sits: a machine's
//!   canonical clone for a lock on that machine, the remote the entry's own
//!   `url` names for a lock on a remote. An object kept only by a reflog or
//!   awaiting gc does not count: a fresh clone would not get it.
//! - **pin rewound**: an `rwv.lock` entry whose new commit's history drops a
//!   commit the old one held, with no patch-equivalent in its place, at a step
//!   that did not name the member.
//! - **unattributed remote write**: a ref of any remote created, moved or
//!   deleted by a step that did not declare that write: a push of the branch
//!   (creating it or moving it forward), a named overwrite (any move), a named
//!   delete, or the forge's sync of the whole remote.
//! - **declared branch moved back**: a declared branch whose tip moves to a
//!   commit that does not contain the old one, unless the step names the repo
//!   in `may_rewrite` and the branch lands on the tip the same branch has in
//!   another of the repo's stores, which is a move to or with its own
//!   upstream, or names it in `may_restore` and the branch moves back to a
//!   commit its old tip contains, as `rwv abort` puts back what its
//!   operation moved. Declared: a member branch some live weave branch's committed
//!   `rwv.toml` declares, and for a project its origin's default branch and
//!   every branch a record holds as `declared`.
//! - **first parents broken**: a target branch moving forward to a commit
//!   whose first-parent chain does not hold its old tip, so another lineage's
//!   history now runs along it. Target: a project branch some feature record
//!   names as its `target`, and for each member the target declares, the
//!   branch it lands in there: the one named like the target when the record
//!   was written `as = "feature"` and the member is `owned` or `fork`, else
//!   its declared branch. A target merged with its own upstream, that tip as
//!   first parent, takes the upstream's first-parent chain, as a merge of the
//!   upstream must: the new tip's first parent is the tip the same branch has
//!   in another of the repo's stores.
//!
//! What they read: every ref (`git rev-list --all`, which takes in each linked
//! worktree's HEAD) of every remote and every machine's canonical clone; the
//! `rwv.lock` in the working tree of each project at each primary and live
//! workweave, and the `rwv.lock` and `rwv.toml` committed on every local
//! branch of each project in each of those repositories; and each project's
//! records branch in each of them, through [`read_records`].
//!
//! What they cannot see: a commit that never reached a ref; a commit lost from
//! one machine while any other repository of the sandbox, a remote included,
//! still reaches it; a lock held only in the index, or only on a
//! remote-tracking branch, whose remote's own branch is read instead; a ref
//! of a machine's clone outside `refs/heads` moving, for the branch checks;
//! the remote's `HEAD`, which no write is attributed for; and content lost
//! inside a merge commit, since a merge has no patch-id and is reported
//! whenever it is stranded or dropped, which errs loud rather than blind.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use repoweave::manifest::Role;

use super::sandbox::{Place, Remote, RemoteKind, Repo, Sandbox, Store, RECORDS_BRANCH};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LockSite {
    /// The working-tree lock of a project at a place, keyed by the branch the
    /// project repo is on there (`DETACHED` when none), so switching branches
    /// moves between sites rather than moving a pin.
    Checkout(Place, Repo, String),
    /// The lock committed on a local branch of a project in one of its
    /// stores.
    Branch(Store, Repo, String),
}

impl LockSite {
    pub fn remote_main(project: Repo) -> LockSite {
        LockSite::Branch(
            Store::Remote(RemoteKind::Origin),
            project,
            "main".to_owned(),
        )
    }
}

pub const DETACHED: &str = "(detached)";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Invariant {
    NoStrandedCommit,
    EveryPinResolves,
    NoPinRewound,
    EveryRemoteWriteAttributed,
    DeclaredBranchesMoveForward,
    TargetsKeepFirstParents,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    Stranded {
        repo: Repo,
        commit: String,
    },
    UnresolvedPin {
        site: LockSite,
        member: Repo,
        pin: String,
    },
    PinRewound {
        site: LockSite,
        member: Repo,
        from: String,
        to: String,
        dropped: Vec<String>,
    },
    UnattributedRemoteWrite {
        remote: Remote,
        refname: String,
        change: RefChange,
    },
    DeclaredMovedBack {
        repo: Repo,
        store: Store,
        branch: String,
        from: String,
        to: String,
    },
    FirstParentsBroken {
        repo: Repo,
        store: Store,
        branch: String,
        from: String,
        to: String,
    },
}

impl Violation {
    pub fn invariant(&self) -> Invariant {
        match self {
            Violation::Stranded { .. } => Invariant::NoStrandedCommit,
            Violation::UnresolvedPin { .. } => Invariant::EveryPinResolves,
            Violation::PinRewound { .. } => Invariant::NoPinRewound,
            Violation::UnattributedRemoteWrite { .. } => Invariant::EveryRemoteWriteAttributed,
            Violation::DeclaredMovedBack { .. } => Invariant::DeclaredBranchesMoveForward,
            Violation::FirstParentsBroken { .. } => Invariant::TargetsKeepFirstParents,
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::Stranded { repo, commit } => write!(
                f,
                "{}: commit {commit} was reachable from a ref and no ref reaches it or an equivalent now",
                repo.name()
            ),
            Violation::UnresolvedPin { site, member, pin } => write!(
                f,
                "{site:?}: pin {pin} for {} is not a commit in its repository",
                member.name()
            ),
            Violation::PinRewound {
                site,
                member,
                from,
                to,
                dropped,
            } => write!(
                f,
                "{site:?}: pin for {} moved {from} -> {to}, dropping {dropped:?}",
                member.name()
            ),
            Violation::UnattributedRemoteWrite {
                remote,
                refname,
                change,
            } => write!(
                f,
                "{remote:?}: {refname} {change}, a write the step did not declare"
            ),
            Violation::DeclaredMovedBack {
                repo,
                store,
                branch,
                from,
                to,
            } => write!(
                f,
                "{} {store:?}: declared branch {branch} moved {from} -> {to}, which does not contain it",
                repo.name()
            ),
            Violation::FirstParentsBroken {
                repo,
                store,
                branch,
                from,
                to,
            } => write!(
                f,
                "{} {store:?}: target branch {branch} moved {from} -> {to}, off its first-parent chain",
                repo.name()
            ),
        }
    }
}

/// What a step may do that the checks would otherwise report.
#[derive(Debug, Clone, Default)]
pub struct Allowance {
    /// Members whose pins the step may move backwards.
    pub may_rewind: Vec<Repo>,
    /// Repos in which the step may leave a commit no ref reaches.
    pub may_strand: Vec<Repo>,
    /// Repos the step names for moving a declared branch to or with its
    /// upstream.
    pub may_rewrite: Vec<Repo>,
    /// Repos the step names for putting a declared branch back to a commit
    /// its tip contains: `rwv abort` restoring what its operation moved.
    pub may_restore: Vec<Repo>,
    /// Members whose pin the step may leave at a commit no ref of the
    /// repository that backs it reaches: a lock published ahead of its
    /// member, or a clone of one.
    pub may_leave_unresolved: Vec<Repo>,
    pub writes: Vec<RemoteWrite>,
}

/// A remote write a step is attributed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteWrite {
    /// The branch is created or moves forward.
    Push(Remote, String),
    /// The branch is created or moves anywhere.
    Overwrite(Remote, String),
    Delete(Remote, String),
    /// Any ref of the remote changes in any way.
    Every(Remote),
}

impl RemoteWrite {
    fn covers(&self, remote: Remote, refname: &str, change: &RefChange) -> bool {
        let branch = |b: &String| refname == format!("refs/heads/{b}");
        match self {
            RemoteWrite::Push(r, b) => {
                *r == remote
                    && branch(b)
                    && matches!(change.kind, ChangeKind::Created | ChangeKind::Advanced)
            }
            RemoteWrite::Overwrite(r, b) => {
                *r == remote && branch(b) && change.kind != ChangeKind::Deleted
            }
            RemoteWrite::Delete(r, b) => {
                *r == remote && branch(b) && change.kind == ChangeKind::Deleted
            }
            RemoteWrite::Every(r) => *r == remote,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Advanced,
    Rewritten,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefChange {
    pub kind: ChangeKind,
    pub from: Option<String>,
    pub to: Option<String>,
}

impl fmt::Display for RefChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let tip = |t: &Option<String>| t.clone().unwrap_or_else(|| "(none)".to_owned());
        write!(
            f,
            "{:?} {} -> {}",
            self.kind,
            tip(&self.from),
            tip(&self.to)
        )
    }
}

pub type Pins = BTreeMap<Repo, String>;

/// A member entry as a weave branch's committed `rwv.toml` declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub branch: String,
    pub role: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Feature,
    Declared,
}

/// One entry of a records branch, read in the format rwv writes: a feature's
/// `mode`, `target` and `as`, a declared branch's `mode`, or a conflict's two
/// values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Feature {
        target: String,
        as_mode: Mode,
    },
    Declared,
    Conflict(Vec<Record>),
    /// What made the entry unparseable.
    Unreadable(String),
}

/// A records branch's entries, keyed by the weave branch each describes.
pub type Records = BTreeMap<String, Record>;

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    reachable: BTreeMap<Repo, BTreeSet<String>>,
    reachable_in_store: BTreeMap<PathBuf, BTreeSet<String>>,
    pub pins: BTreeMap<LockSite, Pins>,
    pin_urls: BTreeMap<LockSite, BTreeMap<Repo, String>>,
    pub declares: BTreeMap<LockSite, BTreeMap<Repo, Declaration>>,
    /// Every local branch's tip in every store.
    pub branches: BTreeMap<(Repo, Store), BTreeMap<String, String>>,
    /// Every ref's tip on every remote.
    pub remote_refs: BTreeMap<Remote, BTreeMap<String, String>>,
    pub records: BTreeMap<(Repo, Store), Records>,
    /// The branch each project's origin hands a fresh clone.
    pub default_branch: BTreeMap<Repo, String>,
}

impl Snapshot {
    pub fn reachable_from(&self, repo: Repo) -> usize {
        self.reachable.get(&repo).map_or(0, BTreeSet::len)
    }

    pub fn declared_branches(&self) -> BTreeMap<Repo, BTreeSet<String>> {
        let mut declared: BTreeMap<Repo, BTreeSet<String>> = BTreeMap::new();
        for entries in self.declares.values() {
            for (member, declaration) in entries {
                declared
                    .entry(*member)
                    .or_default()
                    .insert(declaration.branch.clone());
            }
        }
        for (project, branch) in &self.default_branch {
            declared.entry(*project).or_default().insert(branch.clone());
        }
        for ((project, _), records) in &self.records {
            for (branch, record) in records {
                if *record == Record::Declared {
                    declared.entry(*project).or_default().insert(branch.clone());
                }
            }
        }
        declared
    }

    pub fn target_branches(&self) -> BTreeMap<Repo, BTreeSet<String>> {
        let mut targets: BTreeMap<Repo, BTreeSet<String>> = BTreeMap::new();
        for ((project, _), records) in &self.records {
            for record in records.values() {
                let Record::Feature { target, as_mode } = record else {
                    continue;
                };
                targets.entry(*project).or_default().insert(target.clone());
                let target_sites = self.declares.iter().filter(|(site, _)| {
                    matches!(site, LockSite::Branch(_, p, b) if p == project && b == target)
                });
                for (_, entries) in target_sites {
                    for (member, declaration) in entries {
                        let named_like_target = *as_mode == Mode::Feature
                            && matches!(declaration.role, Role::Owned | Role::Fork);
                        let receiving = if named_like_target {
                            target.clone()
                        } else {
                            declaration.branch.clone()
                        };
                        targets.entry(*member).or_default().insert(receiving);
                    }
                }
            }
        }
        targets
    }
}

fn tips(sandbox: &Sandbox, dir: &Path, prefix: &str) -> BTreeMap<String, String> {
    sandbox
        .git_ok(
            dir,
            &["for-each-ref", "--format=%(objectname) %(refname)", prefix],
        )
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(oid, name)| (name.to_owned(), oid.to_owned()))
        .collect()
}

pub fn snapshot(sandbox: &Sandbox) -> Snapshot {
    let mut snap = Snapshot::default();
    for repo in all_repos(sandbox) {
        let mut commits = BTreeSet::new();
        for store in sandbox.stores(repo) {
            let dir = sandbox.store_dir(repo, store);
            let listed: BTreeSet<String> = sandbox
                .git_ok(&dir, &["rev-list", "--all"])
                .lines()
                .map(str::to_owned)
                .collect();
            commits.extend(listed.iter().cloned());
            snap.reachable_in_store.insert(dir.clone(), listed);
            let heads: BTreeMap<String, String> = tips(sandbox, &dir, "refs/heads/")
                .into_iter()
                .map(|(name, oid)| (name["refs/heads/".len()..].to_owned(), oid))
                .collect();
            if repo.is_project() {
                for (branch, oid) in &heads {
                    read_branch_site(sandbox, &mut snap, &dir, store, repo, branch, oid);
                }
                if let Some(records) = read_records(sandbox, &dir) {
                    snap.records.insert((repo, store), records);
                }
            }
            snap.branches.insert((repo, store), heads);
            if let Store::Remote(kind) = store {
                snap.remote_refs
                    .insert(Remote { repo, kind }, tips(sandbox, &dir, "refs/"));
            }
        }
        snap.reachable.insert(repo, commits);
        if repo.is_project() {
            let origin = sandbox.remote_store(Remote::origin(repo));
            let head = sandbox.git(&origin, &["symbolic-ref", "-q", "--short", "HEAD"]);
            if head.status.success() {
                snap.default_branch.insert(
                    repo,
                    String::from_utf8(head.stdout).unwrap().trim().to_owned(),
                );
            }
        }
    }

    for place in sandbox.live_places() {
        for project in [Repo::Proj, Repo::Proj2] {
            let checkout = sandbox.checkout(&place, project);
            if let Ok(text) = std::fs::read_to_string(checkout.join("rwv.lock")) {
                let unmerged = sandbox.git(&checkout, &["ls-files", "-u", "--", "rwv.lock"]);
                if !unmerged.stdout.is_empty() {
                    continue;
                }
                let out = sandbox.git(&checkout, &["symbolic-ref", "--short", "-q", "HEAD"]);
                let branch = if out.status.success() {
                    String::from_utf8(out.stdout).unwrap().trim().to_owned()
                } else {
                    DETACHED.to_owned()
                };
                let site = LockSite::Checkout(place.clone(), project, branch);
                let (pins, urls) = parse_pins(&site, &text);
                snap.pins.insert(site.clone(), pins);
                snap.pin_urls.insert(site, urls);
            }
        }
    }
    snap
}

fn file_at(sandbox: &Sandbox, dir: &Path, oid: &str, path: &str) -> Option<String> {
    let out = sandbox.git(dir, &["cat-file", "blob", &format!("{oid}:{path}")]);
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).expect("a committed lock or manifest is UTF-8"))
}

fn read_branch_site(
    sandbox: &Sandbox,
    snap: &mut Snapshot,
    dir: &Path,
    store: Store,
    project: Repo,
    branch: &str,
    oid: &str,
) {
    let site = LockSite::Branch(store, project, branch.to_owned());
    if let Some(text) = file_at(sandbox, dir, oid, "rwv.lock") {
        let (pins, urls) = parse_pins(&site, &text);
        snap.pins.insert(site.clone(), pins);
        snap.pin_urls.insert(site.clone(), urls);
    }
    let declared = file_at(sandbox, dir, oid, "rwv.toml")
        .and_then(|text| repoweave::manifest::Manifest::from_toml_str(&text).ok());
    if let Some(manifest) = declared {
        let entries = manifest
            .iter_entries()
            .map(|(key, entry)| {
                (
                    Repo::from_member_key(&key.to_string()),
                    Declaration {
                        branch: entry.branch.to_string(),
                        role: entry.role,
                    },
                )
            })
            .collect();
        snap.declares.insert(site, entries);
    }
}

/// The entries of the records branch in `dir`, or `None` when it has none.
/// An entry that is not a file, not UTF-8, or not one of the record shapes
/// reads as [`Record::Unreadable`] with the reason, and every other entry
/// reads on.
pub fn read_records(sandbox: &Sandbox, dir: &Path) -> Option<Records> {
    let tip = format!("refs/heads/{RECORDS_BRANCH}");
    if !sandbox
        .git(dir, &["rev-parse", "--verify", "-q", &tip])
        .status
        .success()
    {
        return None;
    }
    let listing = sandbox.git_ok_raw(dir, &["ls-tree", "-r", "-z", &tip]);
    let mut records = Records::new();
    for line in listing.split(|b| *b == 0).filter(|l| !l.is_empty()) {
        let line = String::from_utf8(line.to_vec()).expect("a records path is UTF-8");
        let (meta, path) = line.split_once('\t').expect("ls-tree names each entry");
        let mut fields = meta.split(' ');
        let (mode, kind, oid) = (fields.next(), fields.next(), fields.next());
        let record = match (mode, kind, oid) {
            (Some("100644" | "100755"), Some("blob"), Some(oid)) => {
                match String::from_utf8(sandbox.git_ok_raw(dir, &["cat-file", "blob", oid])) {
                    Ok(text) => parse_record(&text),
                    Err(_) => Record::Unreadable("not UTF-8".to_owned()),
                }
            }
            _ => Record::Unreadable(format!("not a file: {meta}")),
        };
        records.insert(path.to_owned(), record);
    }
    Some(records)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordEntry {
    mode: Option<String>,
    target: Option<String>,
    #[serde(rename = "as")]
    as_mode: Option<String>,
    conflict: Option<Vec<RecordEntry>>,
}

fn parse_mode(mode: &str) -> Option<Mode> {
    match mode {
        "feature" => Some(Mode::Feature),
        "declared" => Some(Mode::Declared),
        _ => None,
    }
}

impl RecordEntry {
    fn value(self) -> Result<Record, String> {
        match self {
            RecordEntry {
                mode: Some(mode),
                target: None,
                as_mode: None,
                conflict: None,
            } if mode == "declared" => Ok(Record::Declared),
            RecordEntry {
                mode: Some(mode),
                target: Some(target),
                as_mode: Some(as_mode),
                conflict: None,
            } if mode == "feature" => parse_mode(&as_mode)
                .map(|as_mode| Record::Feature { target, as_mode })
                .ok_or_else(|| format!("`as` is {as_mode:?}")),
            RecordEntry { conflict: None, .. } => Err(
                "neither `mode = \"declared\"` alone nor `mode = \"feature\"` with `target` and `as`"
                    .to_owned(),
            ),
            RecordEntry { .. } => Err("`conflict` inside a value".to_owned()),
        }
    }

    fn record(self) -> Result<Record, String> {
        match self {
            RecordEntry {
                mode: None,
                target: None,
                as_mode: None,
                conflict: Some(values),
            } => {
                if values.len() != 2 {
                    return Err(format!("`conflict` holds {} values", values.len()));
                }
                let values = values
                    .into_iter()
                    .map(RecordEntry::value)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Record::Conflict(values))
            }
            RecordEntry {
                conflict: Some(_), ..
            } => Err("`conflict` beside other keys".to_owned()),
            entry => entry.value(),
        }
    }
}

pub fn parse_record(text: &str) -> Record {
    toml::from_str::<RecordEntry>(text)
        .map_err(|e| e.to_string())
        .and_then(RecordEntry::record)
        .unwrap_or_else(Record::Unreadable)
}

fn all_repos(sandbox: &Sandbox) -> Vec<Repo> {
    Repo::ALL
        .into_iter()
        .filter(|r| sandbox.has_remote(Remote::origin(*r)))
        .collect()
}

fn parse_pins(site: &LockSite, text: &str) -> (Pins, BTreeMap<Repo, String>) {
    let lock = repoweave::manifest::LockFile::from_json_str(text)
        .unwrap_or_else(|e| panic!("the rwv.lock at {site:?} does not parse: {e:#}\n{text}"));
    let mut pins = Pins::new();
    let mut urls = BTreeMap::new();
    for (key, entry) in lock.iter_entries() {
        let member = Repo::from_member_key(&key.to_string());
        pins.insert(member, entry.version.to_string());
        urls.insert(member, entry.url.to_string());
    }
    (pins, urls)
}

/// Each check, run on the state before and after one step. An unresolved
/// pin is charged to the step that produced it, not to every step it
/// outlives.
pub fn check(
    sandbox: &Sandbox,
    before: &Snapshot,
    after: &Snapshot,
    allow: &Allowance,
) -> Vec<Violation> {
    let mut violations = stranded(sandbox, before, after, &allow.may_strand);
    let already = unresolved_pins(sandbox, before);
    violations.extend(unresolved_pins(sandbox, after).into_iter().filter(|v| {
        let spared = matches!(
            v,
            Violation::UnresolvedPin { member, .. } if allow.may_leave_unresolved.contains(member)
        );
        !already.contains(v) && !spared
    }));
    violations.extend(rewound_pins(sandbox, before, after, &allow.may_rewind));
    violations.extend(unattributed_remote_writes(
        sandbox,
        before,
        after,
        &allow.writes,
    ));
    violations.extend(declared_moved_back(
        sandbox,
        before,
        after,
        &allow.may_rewrite,
        &allow.may_restore,
    ));
    violations.extend(first_parents_broken(sandbox, before, after));
    violations
}

pub fn stranded(
    sandbox: &Sandbox,
    before: &Snapshot,
    after: &Snapshot,
    may_strand: &[Repo],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (repo, was) in &before.reachable {
        if may_strand.contains(repo) {
            continue;
        }
        let empty = BTreeSet::new();
        let now = after.reachable.get(repo).unwrap_or(&empty);
        let lost: Vec<&String> = was.difference(now).collect();
        if lost.is_empty() {
            continue;
        }
        let surviving = reachable_patch_ids(sandbox, *repo);
        for commit in lost {
            if repo.is_project() && touches_only_the_lock(sandbox, *repo, commit) {
                continue;
            }
            let equivalent =
                patch_id_anywhere(sandbox, *repo, commit).is_some_and(|id| surviving.contains(&id));
            if !equivalent {
                violations.push(Violation::Stranded {
                    repo: *repo,
                    commit: commit.clone(),
                });
            }
        }
    }
    violations
}

pub fn unresolved_pins(sandbox: &Sandbox, after: &Snapshot) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (site, pins) in &after.pins {
        for (member, pin) in pins {
            let resolves = store_for(sandbox, after, site, *member)
                .and_then(|store| after.reachable_in_store.get(&store))
                .is_some_and(|commits| commits.contains(pin));
            if !resolves {
                violations.push(Violation::UnresolvedPin {
                    site: site.clone(),
                    member: *member,
                    pin: pin.clone(),
                });
            }
        }
    }
    violations
}

pub fn rewound_pins(
    sandbox: &Sandbox,
    before: &Snapshot,
    after: &Snapshot,
    may_rewind: &[Repo],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    for (site, now) in &after.pins {
        let Some(was) = before.pins.get(site) else {
            continue;
        };
        for (member, to) in now {
            let Some(from) = was.get(member) else {
                continue;
            };
            if from == to || may_rewind.contains(member) {
                continue;
            }
            let Some(store) = store_for(sandbox, after, site, *member) else {
                continue;
            };
            if !is_commit(sandbox, &store, from) || !is_commit(sandbox, &store, to) {
                continue;
            }
            let dropped = sandbox.git_ok(&store, &["rev-list", from, &format!("^{to}")]);
            if dropped.is_empty() {
                continue;
            }
            let gained = sandbox.git_ok(&store, &["rev-list", to, &format!("^{from}")]);
            let gained_ids: BTreeSet<String> = patch_ids(sandbox, &store, gained.lines())
                .into_values()
                .collect();
            let dropped_ids = patch_ids(sandbox, &store, dropped.lines());
            let unreplaced: Vec<String> = dropped
                .lines()
                .filter(|c| {
                    !dropped_ids
                        .get(*c)
                        .is_some_and(|id| gained_ids.contains(id))
                })
                .map(str::to_owned)
                .collect();
            if !unreplaced.is_empty() {
                violations.push(Violation::PinRewound {
                    site: site.clone(),
                    member: *member,
                    from: from.clone(),
                    to: to.clone(),
                    dropped: unreplaced,
                });
            }
        }
    }
    violations
}

pub fn unattributed_remote_writes(
    sandbox: &Sandbox,
    before: &Snapshot,
    after: &Snapshot,
    writes: &[RemoteWrite],
) -> Vec<Violation> {
    let mut violations = Vec::new();
    let empty = BTreeMap::new();
    let remotes: BTreeSet<&Remote> = before
        .remote_refs
        .keys()
        .chain(after.remote_refs.keys())
        .collect();
    for remote in remotes {
        let was = before.remote_refs.get(remote).unwrap_or(&empty);
        let now = after.remote_refs.get(remote).unwrap_or(&empty);
        let refnames: BTreeSet<&String> = was.keys().chain(now.keys()).collect();
        for refname in refnames {
            let (from, to) = (was.get(refname), now.get(refname));
            if from == to {
                continue;
            }
            let kind = match (from, to) {
                (None, _) => ChangeKind::Created,
                (_, None) => ChangeKind::Deleted,
                (Some(from), Some(to)) => {
                    if is_ancestor(sandbox, &sandbox.remote_store(*remote), from, to) {
                        ChangeKind::Advanced
                    } else {
                        ChangeKind::Rewritten
                    }
                }
            };
            let change = RefChange {
                kind,
                from: from.cloned(),
                to: to.cloned(),
            };
            if !writes.iter().any(|w| w.covers(*remote, refname, &change)) {
                violations.push(Violation::UnattributedRemoteWrite {
                    remote: *remote,
                    refname: refname.clone(),
                    change,
                });
            }
        }
    }
    violations
}

/// Each branch of `branches` that exists in a store before and after the
/// step at a different tip: `(repo, store, branch, from, to)`.
fn moves<'a>(
    before: &'a Snapshot,
    after: &'a Snapshot,
    branches: &'a BTreeMap<Repo, BTreeSet<String>>,
) -> impl Iterator<Item = (Repo, Store, &'a str, &'a str, &'a str)> + 'a {
    after.branches.iter().flat_map(move |((repo, store), now)| {
        let was = before.branches.get(&(*repo, *store));
        branches
            .get(repo)
            .into_iter()
            .flatten()
            .filter_map(move |branch| {
                let from = was?.get(branch)?;
                let to = now.get(branch)?;
                (from != to).then_some((*repo, *store, branch.as_str(), from.as_str(), to.as_str()))
            })
    })
}

fn union(
    a: BTreeMap<Repo, BTreeSet<String>>,
    b: BTreeMap<Repo, BTreeSet<String>>,
) -> BTreeMap<Repo, BTreeSet<String>> {
    let mut all = a;
    for (repo, branches) in b {
        all.entry(repo).or_default().extend(branches);
    }
    all
}

pub fn declared_moved_back(
    sandbox: &Sandbox,
    before: &Snapshot,
    after: &Snapshot,
    may_rewrite: &[Repo],
    may_restore: &[Repo],
) -> Vec<Violation> {
    let declared = union(before.declared_branches(), after.declared_branches());
    let mut violations = Vec::new();
    for (repo, store, branch, from, to) in moves(before, after, &declared) {
        if is_ancestor(sandbox, &sandbox.store_dir(repo, store), from, to) {
            continue;
        }
        let with_its_upstream = after
            .branches
            .iter()
            .filter(|((r, s), _)| *r == repo && *s != store)
            .any(|(_, tips)| tips.get(branch).map(String::as_str) == Some(to));
        if may_rewrite.contains(&repo) && with_its_upstream {
            continue;
        }
        if may_restore.contains(&repo)
            && is_ancestor(sandbox, &sandbox.store_dir(repo, store), to, from)
        {
            continue;
        }
        violations.push(Violation::DeclaredMovedBack {
            repo,
            store,
            branch: branch.to_owned(),
            from: from.to_owned(),
            to: to.to_owned(),
        });
    }
    violations
}

pub fn first_parents_broken(
    sandbox: &Sandbox,
    before: &Snapshot,
    after: &Snapshot,
) -> Vec<Violation> {
    let targets = union(before.target_branches(), after.target_branches());
    let mut violations = Vec::new();
    for (repo, store, branch, from, to) in moves(before, after, &targets) {
        let dir = sandbox.store_dir(repo, store);
        if !is_ancestor(sandbox, &dir, from, to) {
            continue;
        }
        let on_first_parents = sandbox
            .git_ok(&dir, &["rev-list", "--first-parent", to])
            .lines()
            .any(|commit| commit == from);
        let first_parent = sandbox.git_ok(&dir, &["rev-parse", &format!("{to}^1")]);
        let merged_with_its_upstream = after
            .branches
            .iter()
            .filter(|((r, s), _)| *r == repo && *s != store)
            .filter_map(|(_, tips)| tips.get(branch))
            .any(|upstream| upstream != from && upstream == first_parent.trim());
        if !on_first_parents && !merged_with_its_upstream {
            violations.push(Violation::FirstParentsBroken {
                repo,
                store,
                branch: branch.to_owned(),
                from: from.to_owned(),
                to: to.to_owned(),
            });
        }
    }
    violations
}

pub fn store_for(
    sandbox: &Sandbox,
    snapshot: &Snapshot,
    site: &LockSite,
    member: Repo,
) -> Option<PathBuf> {
    let store = match site {
        LockSite::Checkout(place, _, _) => sandbox.canonical(place.machine()?, member),
        LockSite::Branch(Store::Clone(machine), _, _) => sandbox.canonical(*machine, member),
        LockSite::Branch(Store::Remote(_), _, _) => {
            let url = snapshot.pin_urls.get(site)?.get(&member)?;
            sandbox.remote_store(sandbox.remote_for_url(url)?)
        }
    };
    store.is_dir().then_some(store)
}

fn is_ancestor(sandbox: &Sandbox, dir: &Path, ancestor: &str, descendant: &str) -> bool {
    sandbox
        .git(dir, &["merge-base", "--is-ancestor", ancestor, descendant])
        .status
        .success()
}

fn is_commit(sandbox: &Sandbox, store: &Path, rev: &str) -> bool {
    sandbox
        .git(store, &["cat-file", "-e", &format!("{rev}^{{commit}}")])
        .status
        .success()
}

/// `git patch-id --stable` for each of `commits` that has one, over every
/// path but `rwv.lock`; a merge, an empty commit, or one touching only the
/// lock has none. The lock is left out because rwv rewrites it when it
/// replays a commit, so a replay's lock diff never matches the original's.
fn patch_ids<'a>(
    sandbox: &Sandbox,
    store: &Path,
    commits: impl Iterator<Item = &'a str>,
) -> BTreeMap<String, String> {
    let commits: Vec<&str> = commits.collect();
    if commits.is_empty() {
        return BTreeMap::new();
    }
    let mut show = vec!["show", "--no-color", "--format=commit %H", "-p"];
    show.extend(commits.iter().copied());
    show.extend(["--", ".", ":(exclude)rwv.lock"]);
    let diff = sandbox.git_ok_raw(store, &show);
    sandbox
        .git_with_stdin(store, &["patch-id", "--stable"], &diff)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(id, commit)| (commit.to_owned(), id.to_owned()))
        .collect()
}

fn reachable_patch_ids(sandbox: &Sandbox, repo: Repo) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for store in sandbox.ref_stores(repo) {
        let listed = sandbox.git_ok(&store, &["rev-list", "--all", "--no-merges"]);
        ids.extend(patch_ids(sandbox, &store, listed.lines()).into_values());
    }
    ids
}

fn patch_id_anywhere(sandbox: &Sandbox, repo: Repo, commit: &str) -> Option<String> {
    sandbox
        .ref_stores(repo)
        .iter()
        .find(|store| is_commit(sandbox, store, commit))
        .and_then(|store| patch_ids(sandbox, store, std::iter::once(commit)).remove(commit))
}

fn touches_only_the_lock(sandbox: &Sandbox, repo: Repo, commit: &str) -> bool {
    sandbox
        .ref_stores(repo)
        .iter()
        .find(|store| is_commit(sandbox, store, commit))
        .is_some_and(|store| {
            sandbox.git_ok(
                store,
                &["show", "--no-renames", "--name-only", "--format=", commit],
            ) == "rwv.lock"
        })
}
