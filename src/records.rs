//! Branch records: how each weave branch is used, kept on the records branch
//! of the project repo.
//!
//! The records branch holds one small TOML file per weave branch, in one flat
//! tree, named for the branch with `%` and `/` escaped. A reader folds the records of every records remote into the
//! local branch entry by entry; a writer commits that fold first, then its own
//! change, and moves the local branch with a compare-and-swap, so two verbs
//! writing at once lose nothing: the one that loses reads again and applies
//! its change to what the winner wrote.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::vcs::{
    CommitId, CommittedFile, FileChange, RawRefName, RecordsAdvance, RecordsPublish, RecordsSite,
    RemoteHandle, Vcs, VcsError,
};

const WRITER_TRAILER: &str = "Rwv-Writer";
const WRITE_ATTEMPTS: usize = 64;

/// The name of a weave branch: a valid branch name, and not the records
/// branch or below it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BranchName(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Invalid(crate::naming::RefNameError),
    Reserved,
}

impl BranchName {
    pub fn parse(vcs: &dyn Vcs, text: &str) -> Result<Self, NameError> {
        crate::naming::validate_ref_name(text).map_err(NameError::Invalid)?;
        let records = vcs.records_branch();
        if text == records
            || text
                .strip_prefix(records)
                .is_some_and(|rest| rest.starts_with('/'))
        {
            return Err(NameError::Reserved);
        }
        Ok(Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BranchName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a weave branch is used.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
#[clap(rename_all = "kebab-case")]
pub enum Mode {
    /// Work onto a target branch: its `owned` and `fork` members sit on
    /// branches named after it.
    Feature,
    /// Members sit on the branches its member entries declare.
    Declared,
}

impl Mode {
    pub fn token(self) -> &'static str {
        match self {
            Self::Feature => "feature",
            Self::Declared => "declared",
        }
    }

    fn from_token(text: &str) -> Option<Self> {
        match text {
            "feature" => Some(Self::Feature),
            "declared" => Some(Self::Declared),
            _ => None,
        }
    }
}

/// A branch's mode and, for a feature, its landing record: the branch it
/// lands into and that branch's mode when the record was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    Declared,
    Feature { target: BranchName, as_mode: Mode },
}

impl Record {
    pub fn mode(&self) -> Mode {
        match self {
            Self::Declared => Mode::Declared,
            Self::Feature { .. } => Mode::Feature,
        }
    }

    fn target(&self) -> Option<&BranchName> {
        match self {
            Self::Declared => None,
            Self::Feature { target, .. } => Some(target),
        }
    }
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Declared => f.write_str("declared"),
            Self::Feature { target, as_mode } => {
                write!(f, "feature onto {target} (as {})", as_mode.token())
            }
        }
    }
}

/// What the records branch holds for one branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Value(Record),
    /// Two writers recorded the branch differently; neither is chosen.
    Conflict(Record, Record),
    /// The file does not read as an entry; why.
    Unreadable(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    mode: Option<String>,
    target: Option<String>,
    #[serde(rename = "as")]
    as_mode: Option<String>,
    conflict: Option<Vec<WireValue>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireValue {
    mode: Option<String>,
    target: Option<String>,
    #[serde(rename = "as")]
    as_mode: Option<String>,
}

impl WireValue {
    fn record(self) -> Result<Record, String> {
        let WireValue {
            mode,
            target,
            as_mode,
        } = self;
        match (mode.as_deref().and_then(Mode::from_token), target, as_mode) {
            (Some(Mode::Declared), None, None) => Ok(Record::Declared),
            (Some(Mode::Feature), Some(target), Some(as_mode)) => {
                crate::naming::validate_ref_name(&target)
                    .map_err(|e| format!("`target` is not a branch name: {e:#}"))?;
                let as_mode = Mode::from_token(&as_mode)
                    .ok_or_else(|| format!("`as` is {as_mode:?}, not `feature` or `declared`"))?;
                Ok(Record::Feature {
                    target: BranchName(target),
                    as_mode,
                })
            }
            _ => Err(
                "neither `mode = \"declared\"` alone nor `mode = \"feature\"` with `target` and `as`"
                    .to_owned(),
            ),
        }
    }
}

impl Entry {
    pub fn parse(bytes: &[u8]) -> Self {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Self::Unreadable("it is not UTF-8".to_owned());
        };
        let wire: Wire = match toml::from_str(text) {
            Ok(wire) => wire,
            Err(e) => return Self::Unreadable(e.message().to_owned()),
        };
        let Wire {
            mode,
            target,
            as_mode,
            conflict,
        } = wire;
        let outcome = match conflict {
            Some(values) if mode.is_none() && target.is_none() && as_mode.is_none() => values
                .into_iter()
                .map(WireValue::record)
                .collect::<Result<Vec<_>, _>>()
                .and_then(|records| match <[Record; 2]>::try_from(records) {
                    Ok([first, second]) => Ok(Self::Conflict(first, second)),
                    Err(records) => Err(format!(
                        "`conflict` holds {} values, not two",
                        records.len()
                    )),
                }),
            Some(_) => Err("`conflict` beside other keys".to_owned()),
            None => WireValue {
                mode,
                target,
                as_mode,
            }
            .record()
            .map(Self::Value),
        };
        outcome.unwrap_or_else(Self::Unreadable)
    }

    /// The file this entry is stored as; `None` for an unreadable one, which
    /// is stored as it was found.
    pub fn render(&self) -> Option<Vec<u8>> {
        let value = |record: &Record| match record {
            Record::Declared => "mode = \"declared\"\n".to_owned(),
            Record::Feature { target, as_mode } => format!(
                "mode = \"feature\"\ntarget = {}\nas = \"{}\"\n",
                toml::Value::String(target.as_str().to_owned()),
                as_mode.token()
            ),
        };
        let inline = |record: &Record| match record {
            Record::Declared => "{ mode = \"declared\" }".to_owned(),
            Record::Feature { target, as_mode } => format!(
                "{{ mode = \"feature\", target = {}, as = \"{}\" }}",
                toml::Value::String(target.as_str().to_owned()),
                as_mode.token()
            ),
        };
        match self {
            Self::Value(record) => Some(value(record).into_bytes()),
            Self::Conflict(first, second) => Some(
                format!(
                    "conflict = [\n  {},\n  {},\n]\n",
                    inline(first),
                    inline(second)
                )
                .into_bytes(),
            ),
            Self::Unreadable(_) => None,
        }
    }
}

/// The files of one records commit, by branch name.
pub type Snapshot = BTreeMap<String, Vec<u8>>;

/// The path of `name`'s entry: the name with `%` written `%25` and `/`
/// written `%2F`, so no entry's path is a prefix of another's.
pub fn entry_path(name: &str) -> String {
    let mut path = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '%' => path.push_str("%25"),
            '/' => path.push_str("%2F"),
            c => path.push(c),
        }
    }
    path
}

/// The branch name `path` is the entry of, or `None` for a path no name
/// encodes to: one with a `/`, or a `%` not starting `%25` or `%2F`.
pub fn entry_name(path: &str) -> Option<String> {
    if path.is_empty() || path.contains('/') {
        return None;
    }
    let mut name = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(at) = rest.find('%') {
        name.push_str(&rest[..at]);
        let escape = rest.get(at..at + 3)?;
        name.push(match escape {
            "%25" => '%',
            "%2F" => '/',
            _ => return None,
        });
        rest = &rest[at + 3..];
    }
    name.push_str(rest);
    Some(name)
}

/// The entries of a records commit. A path that is not an entry's is not
/// read, and no write touches it.
fn snapshot_of(files: Vec<CommittedFile>) -> Snapshot {
    files
        .into_iter()
        .filter_map(|f| entry_name(&f.path).map(|name| (name, f.content)))
        .collect()
}

fn same_entry(left: &[u8], right: &[u8]) -> bool {
    left == right
        || match (Entry::parse(left), Entry::parse(right)) {
            (Entry::Unreadable(_), _) | (_, Entry::Unreadable(_)) => false,
            (a, b) => a == b,
        }
}

fn merge_entry(base: Option<&[u8]>, ours: Option<&[u8]>, theirs: Option<&[u8]>) -> Option<Vec<u8>> {
    let alike = |a: Option<&[u8]>, b: Option<&[u8]>| match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => same_entry(a, b),
        _ => false,
    };
    if alike(ours, theirs) || alike(theirs, base) {
        return ours.map(<[u8]>::to_vec);
    }
    if alike(ours, base) {
        return theirs.map(<[u8]>::to_vec);
    }
    let (Some(ours), Some(theirs)) = (ours, theirs) else {
        return ours.or(theirs).map(<[u8]>::to_vec);
    };
    match (Entry::parse(ours), Entry::parse(theirs)) {
        (Entry::Value(a), Entry::Value(b)) => Entry::Conflict(a, b).render(),
        (Entry::Unreadable(_), _) | (Entry::Conflict(..), _) => Some(ours.to_vec()),
        (_, Entry::Unreadable(_)) | (_, Entry::Conflict(..)) => Some(theirs.to_vec()),
    }
}

/// `ours` and `theirs` merged entry by entry against `base`: an entry changed
/// on one side takes that side, one changed alike on both that value, one
/// changed differently on both is stored as a conflict.
pub fn merge_snapshots(base: &Snapshot, ours: &Snapshot, theirs: &Snapshot) -> Snapshot {
    let names: BTreeSet<&String> = base
        .keys()
        .chain(ours.keys())
        .chain(theirs.keys())
        .collect();
    names
        .into_iter()
        .filter_map(|name| {
            merge_entry(
                base.get(name).map(Vec::as_slice),
                ours.get(name).map(Vec::as_slice),
                theirs.get(name).map(Vec::as_slice),
            )
            .map(|bytes| (name.clone(), bytes))
        })
        .collect()
}

fn changes_between(from: &Snapshot, to: &Snapshot) -> Vec<FileChange> {
    let mut changes: Vec<FileChange> = from
        .keys()
        .filter(|name| !to.contains_key(*name))
        .map(|name| FileChange {
            path: entry_path(name),
            content: None,
        })
        .collect();
    changes.extend(
        to.iter()
            .filter(|(name, bytes)| from.get(*name) != Some(*bytes))
            .map(|(name, bytes)| FileChange {
                path: entry_path(name),
                content: Some(bytes.clone()),
            }),
    );
    changes
}

/// What a branch's record reads as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Unrecorded,
    Value(Record),
    Conflict(Record, Record),
    Unreadable(String),
    /// The branch's target chain returns to it: the branches on the cycle,
    /// starting with this one.
    Cycle(Vec<BranchName>),
}

/// The records as they stand: the local branch with the records remotes'
/// merged in.
#[derive(Debug, Clone)]
pub struct View {
    snapshot: Snapshot,
}

impl View {
    pub fn entry(&self, branch: &BranchName) -> Option<Entry> {
        self.snapshot
            .get(branch.as_str())
            .map(|bytes| Entry::parse(bytes))
    }

    pub fn lookup(&self, branch: &BranchName) -> Lookup {
        let Some(entry) = self.entry(branch) else {
            return Lookup::Unrecorded;
        };
        let cycle = match &entry {
            Entry::Value(record) => self.cycle_through(branch, std::slice::from_ref(record)),
            Entry::Conflict(first, second) => {
                self.cycle_through(branch, &[first.clone(), second.clone()])
            }
            Entry::Unreadable(_) => None,
        };
        match (cycle, entry) {
            (Some(chain), _) => Lookup::Cycle(chain),
            (None, Entry::Value(record)) => Lookup::Value(record),
            (None, Entry::Conflict(first, second)) => Lookup::Conflict(first, second),
            (None, Entry::Unreadable(why)) => Lookup::Unreadable(why),
        }
    }

    fn targets_of(&self, name: &str) -> Vec<BranchName> {
        match self.snapshot.get(name).map(|bytes| Entry::parse(bytes)) {
            Some(Entry::Value(record)) => record.target().cloned().into_iter().collect(),
            Some(Entry::Conflict(first, second)) => [first, second]
                .iter()
                .filter_map(|record| record.target().cloned())
                .collect(),
            Some(Entry::Unreadable(_)) | None => Vec::new(),
        }
    }

    fn cycle_through(&self, start: &BranchName, own: &[Record]) -> Option<Vec<BranchName>> {
        let first_hops: Vec<BranchName> = own
            .iter()
            .filter_map(|record| record.target().cloned())
            .collect();
        self.chain_back_to(start, first_hops)
    }

    /// The chain of targets from `hops` that returns to `start`, listed from
    /// `start`, or `None` when no chain does.
    pub fn chain_back_to(
        &self,
        start: &BranchName,
        hops: Vec<BranchName>,
    ) -> Option<Vec<BranchName>> {
        let mut seen: BTreeSet<BranchName> = BTreeSet::new();
        let mut stack: Vec<Vec<BranchName>> = hops
            .into_iter()
            .map(|hop| vec![start.clone(), hop])
            .collect();
        while let Some(path) = stack.pop() {
            let last = path.last().expect("a path has a branch").clone();
            if last == *start {
                return Some(path);
            }
            if !seen.insert(last.clone()) {
                continue;
            }
            for next in self.targets_of(last.as_str()) {
                let mut longer = path.clone();
                longer.push(next);
                stack.push(longer);
            }
        }
        None
    }

    /// The name of every branch the records hold an entry for.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.snapshot.keys().map(String::as_str)
    }

    /// Every branch whose record names `branch` as its target.
    pub fn dependents(&self, branch: &BranchName) -> Vec<BranchName> {
        self.snapshot
            .keys()
            .filter(|name| self.targets_of(name).iter().any(|target| target == branch))
            .map(|name| BranchName(name.clone()))
            .collect()
    }

    /// The bytes held for `branch`.
    pub fn raw(&self, branch: &BranchName) -> Option<&[u8]> {
        self.snapshot.get(branch.as_str()).map(Vec::as_slice)
    }
}

/// A record to write: `branch` takes `record`, replacing whatever it held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write {
    pub branch: BranchName,
    pub record: Record,
}

/// Why a write was refused before anything changed.
#[derive(Debug)]
pub enum WriteError {
    /// The branch's target chain would return to it.
    Cycle(Vec<BranchName>),
    /// The target has no record to take the mode of.
    TargetUnrecorded(BranchName),
    /// The branch has no record to move.
    Unrecorded(BranchName),
    /// The target's record is not one value.
    TargetUnreadable(BranchName, Lookup),
    /// Another verb kept winning the compare-and-swap.
    Busy,
    Vcs(VcsError),
}

impl From<VcsError> for WriteError {
    fn from(e: VcsError) -> Self {
        Self::Vcs(e)
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cycle(chain) => write!(f, "a target cycle: {}", chain_text(chain)),
            Self::TargetUnrecorded(target) | Self::Unrecorded(target) => {
                write!(f, "{target} has no record")
            }
            Self::TargetUnreadable(target, _) => {
                write!(f, "the record of {target} reads as no one value")
            }
            Self::Busy => write!(
                f,
                "the records branch kept changing under this write; run it again"
            ),
            Self::Vcs(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for WriteError {}

pub fn chain_text(chain: &[BranchName]) -> String {
    chain
        .iter()
        .map(BranchName::as_str)
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// Each records remote's tip, as one reading of them.
#[derive(Clone)]
struct RemoteTips(Vec<(RemoteHandle, CommitId)>);

impl RemoteTips {
    fn of(&self, remote: &RemoteHandle) -> Option<&CommitId> {
        self.0
            .iter()
            .find(|(held, _)| held == remote)
            .map(|(_, tip)| tip)
    }

    fn distinct(&self) -> Vec<CommitId> {
        let mut tips: Vec<CommitId> = Vec::new();
        for (_, tip) in &self.0 {
            if !tips.contains(tip) {
                tips.push(tip.clone());
            }
        }
        tips
    }
}

/// A fold of the local records branch and the records remotes' tips.
struct Fold {
    heads: Vec<CommitId>,
    snapshot: Snapshot,
    /// The local branch's own entries, which `snapshot` may differ from by
    /// what a remote's merge brought in.
    local: Snapshot,
}

/// The records of one project repo.
pub struct Store<'a> {
    vcs: &'a dyn Vcs,
    repo: &'a Path,
    remotes: Vec<RemoteHandle>,
}

impl<'a> Store<'a> {
    /// The store of `repo`, reading the records remotes: the project repo's
    /// url remote and every remote a branch of it publishes to.
    pub fn open(vcs: &'a dyn Vcs, repo: &'a Path) -> Result<Self, VcsError> {
        let url = vcs.project_url_remote(repo)?;
        let mut remotes: Vec<RemoteHandle> = url.iter().cloned().collect();
        for target in vcs.branch_push_targets(repo, url.as_ref())? {
            if !remotes.contains(&target) {
                remotes.push(target);
            }
        }
        Ok(Self { vcs, repo, remotes })
    }

    pub fn remotes(&self) -> &[RemoteHandle] {
        &self.remotes
    }

    fn snapshot_at(&self, commit: &CommitId) -> Result<Snapshot, VcsError> {
        self.vcs.files_at(self.repo, commit).map(snapshot_of)
    }

    fn remote_tips(&self) -> Result<RemoteTips, VcsError> {
        let mut tips = Vec::new();
        for remote in &self.remotes {
            if let Some(tip) = self
                .vcs
                .records_tip(self.repo, RecordsSite::Remote(remote))?
            {
                tips.push((remote.clone(), tip));
            }
        }
        Ok(RemoteTips(tips))
    }

    fn is_ancestor(&self, ancestor: &CommitId, of: &CommitId) -> Result<bool, VcsError> {
        if ancestor == of {
            return Ok(true);
        }
        Ok(self
            .vcs
            .merge_bases(self.repo, ancestor, of)?
            .contains(ancestor))
    }

    fn fold(&self, local: Option<CommitId>, remotes: &RemoteTips) -> Result<Fold, VcsError> {
        let mut heads: Vec<CommitId> = local.iter().cloned().collect();
        for tip in remotes.distinct() {
            let mut covered = false;
            for head in &heads {
                if self.is_ancestor(&tip, head)? {
                    covered = true;
                    break;
                }
            }
            if covered {
                continue;
            }
            let mut kept = Vec::new();
            for head in heads {
                if !self.is_ancestor(&head, &tip)? {
                    kept.push(head);
                }
            }
            kept.push(tip);
            heads = kept;
        }
        let mut snapshot = match heads.split_first() {
            None => Snapshot::new(),
            Some((first, rest)) => {
                let mut merged = self.snapshot_at(first)?;
                for head in rest {
                    let base = match self.vcs.merge_bases(self.repo, first, head)?.first() {
                        Some(base) => self.snapshot_at(base)?,
                        None => Snapshot::new(),
                    };
                    merged = merge_snapshots(&base, &merged, &self.snapshot_at(head)?);
                }
                merged
            }
        };
        let held = match &local {
            Some(tip) => self.snapshot_at(tip)?,
            None => Snapshot::new(),
        };
        for (name, bytes) in &held {
            if !snapshot.contains_key(name) && self.exists_here(name)? {
                snapshot.insert(name.clone(), bytes.clone());
            }
        }
        Ok(Fold {
            heads,
            snapshot,
            local: held,
        })
    }

    /// Whether a branch of this name is a local branch, or a remote-tracking
    /// branch of a records remote.
    fn exists_here(&self, name: &str) -> Result<bool, VcsError> {
        let branch = RawRefName::new(name);
        if self
            .vcs
            .resolve_local_branch_tip(self.repo, &branch)?
            .is_some()
        {
            return Ok(true);
        }
        Ok(self
            .vcs
            .remote_copies(self.repo, &branch)?
            .iter()
            .any(|copy| self.remotes.contains(&copy.remote)))
    }

    /// The records as they stand, computed in memory: nothing is written.
    pub fn view(&self) -> Result<View, VcsError> {
        let local = self.vcs.records_tip(self.repo, RecordsSite::Local)?;
        Ok(View {
            snapshot: self.fold(local, &self.remote_tips()?)?.snapshot,
        })
    }

    /// The records as they stand, the remotes' merged into the local branch
    /// first.
    pub fn settle(&self) -> Result<View, WriteError> {
        self.write(|_| Ok(Vec::new())).map(|written| written.view)
    }

    /// Apply the writes `decide` returns for the records as they stand, after
    /// the remotes' have been merged in. `decide` runs again against whatever
    /// the winner wrote when the compare-and-swap is lost.
    pub fn write(
        &self,
        decide: impl Fn(&View) -> Result<Vec<Write>, WriteError>,
    ) -> Result<Written, WriteError> {
        self.write_dropping(decide, &|_| false)
    }

    /// [`Store::write`], and in the same commit the drop rule: every entry the
    /// local branch already held whose branch `droppable` names goes. An entry
    /// a merge brought in is never among them, so a record that reached a
    /// remote just before its branch is not lost in the gap.
    pub fn write_dropping(
        &self,
        decide: impl Fn(&View) -> Result<Vec<Write>, WriteError>,
        droppable: &dyn Fn(&str) -> bool,
    ) -> Result<Written, WriteError> {
        self.write_folding(&|| self.remote_tips(), decide, droppable)
    }

    /// [`Store::write_dropping`], folding in the remotes' tips `remotes`
    /// reads on each attempt.
    fn write_folding(
        &self,
        remotes: &dyn Fn() -> Result<RemoteTips, VcsError>,
        decide: impl Fn(&View) -> Result<Vec<Write>, WriteError>,
        droppable: &dyn Fn(&str) -> bool,
    ) -> Result<Written, WriteError> {
        for _ in 0..WRITE_ATTEMPTS {
            let local = self.vcs.records_tip(self.repo, RecordsSite::Local)?;
            let fold = self.fold(local.clone(), &remotes()?)?;
            let folded = View {
                snapshot: fold.snapshot.clone(),
            };
            let writes: Vec<Write> = decide(&folded)?
                .into_iter()
                .filter(|w| folded.entry(&w.branch) != Some(Entry::Value(w.record.clone())))
                .collect();
            let drops: Vec<String> = fold
                .local
                .keys()
                .filter(|name| {
                    fold.snapshot.contains_key(*name)
                        && droppable(name)
                        && !writes.iter().any(|w| w.branch.as_str() == name.as_str())
                })
                .cloned()
                .collect();
            let first_snapshot = match fold.heads.first() {
                Some(first) => self.snapshot_at(first)?,
                None => Snapshot::new(),
            };
            let mut tip = match fold.heads.as_slice() {
                [] => None,
                [only] if fold.snapshot == first_snapshot => Some(only.clone()),
                heads => Some(self.vcs.write_commit(
                    self.repo,
                    heads,
                    &changes_between(&first_snapshot, &fold.snapshot),
                    &commit_message("merge records"),
                )?),
            };
            let mut after = fold.snapshot;
            if !writes.is_empty() || !drops.is_empty() {
                let before = after.clone();
                for name in &drops {
                    after.remove(name);
                }
                for write in &writes {
                    let entry = Entry::Value(write.record.clone());
                    after.insert(
                        write.branch.as_str().to_owned(),
                        entry.render().expect("a value renders"),
                    );
                }
                let mut parts: Vec<String> = Vec::new();
                if !writes.is_empty() {
                    parts.push(format!(
                        "set {}",
                        writes
                            .iter()
                            .map(|w| format!("{} {}", w.branch, w.record.mode().token()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if !drops.is_empty() {
                    parts.push(format!("drop {}", drops.join(", ")));
                }
                tip = Some(self.vcs.write_commit(
                    self.repo,
                    &tip.iter().cloned().collect::<Vec<_>>(),
                    &changes_between(&before, &after),
                    &commit_message(&parts.join("; ")),
                )?);
            }
            let committed = tip != local;
            if let (true, Some(to)) = (committed, &tip) {
                if self.vcs.advance_records(self.repo, local.as_ref(), to)? == RecordsAdvance::Lost
                {
                    continue;
                }
            }
            return Ok(Written {
                view: View { snapshot: after },
                committed,
                dropped: drops,
            });
        }
        Err(WriteError::Busy)
    }

    /// Move `from`'s entry to `to`, and point every entry whose target is
    /// `from` at `to`, in one commit: nothing is left under the old name. The
    /// entry moved is `from`'s, or `to`'s own where `from` has none, the
    /// state an earlier run of the same rename left.
    pub fn rename(&self, from: &BranchName, to: &BranchName) -> Result<Written, WriteError> {
        self.write_dropping(
            |view| {
                let moved = match (view.lookup(from), view.lookup(to)) {
                    (Lookup::Value(record), _) | (Lookup::Unrecorded, Lookup::Value(record)) => {
                        record
                    }
                    (Lookup::Unrecorded, _) => return Err(WriteError::Unrecorded(from.clone())),
                    (other, _) => return Err(WriteError::TargetUnreadable(from.clone(), other)),
                };
                if let Some(target) = moved.target() {
                    if let Some(chain) = view.chain_back_to(to, vec![target.clone()]) {
                        return Err(WriteError::Cycle(chain));
                    }
                }
                let mut writes = vec![Write {
                    branch: to.clone(),
                    record: moved,
                }];
                for dependent in view.dependents(from) {
                    if dependent == *to {
                        continue;
                    }
                    match view.lookup(&dependent) {
                        Lookup::Value(Record::Feature { as_mode, .. }) => writes.push(Write {
                            branch: dependent,
                            record: Record::Feature {
                                target: to.clone(),
                                as_mode,
                            },
                        }),
                        other => return Err(WriteError::TargetUnreadable(dependent, other)),
                    }
                }
                Ok(writes)
            },
            &|name| name == from.as_str(),
        )
    }

    /// Publish `branch`'s record, and the drops this machine has made, to
    /// `remote`: one commit on the tip of `remote`'s records branch as last
    /// fetched, pushed as a fast-forward. The tip the commit is built on is the
    /// one the records merged before it folded in: an entry is dropped only
    /// when the merge of that same tip left it out. It changes `branch`'s own
    /// entry and removes the entries the local branch no longer holds; no other entry
    /// of the local branch is sent, and a conflicted or unreadable one is
    /// never sent. A remote that moved meanwhile is fetched through `refetch`,
    /// the records merged again and the commit rebuilt.
    pub fn publish(
        &self,
        branch: &BranchName,
        remote: &RemoteHandle,
        refetch: &dyn Fn() -> anyhow::Result<()>,
    ) -> anyhow::Result<Published> {
        for _ in 0..WRITE_ATTEMPTS {
            let tips = self.remote_tips()?;
            let settled = self
                .write_folding(&|| Ok(tips.clone()), |_| Ok(Vec::new()), &|_| false)
                .map_err(anyhow::Error::new)?
                .view;
            let settled = SettledView { view: settled };
            crate::test_pause::point("records-publish-settled");
            let remote_tip = tips.of(remote).cloned();
            let base = match &remote_tip {
                Some(tip) => self.snapshot_at(tip)?,
                None => Snapshot::new(),
            };
            let mut next = base.clone();
            let dropped: Vec<String> = base
                .keys()
                .filter(|name| !settled.view.snapshot.contains_key(*name))
                .cloned()
                .collect();
            for name in &dropped {
                next.remove(name);
            }
            if let (Some(Entry::Value(_)), Some(mine)) =
                (settled.view.entry(branch), settled.view.raw(branch))
            {
                if !base
                    .get(branch.as_str())
                    .is_some_and(|held| same_entry(held, mine))
                {
                    next.insert(branch.as_str().to_owned(), mine.to_vec());
                }
            }
            if next == base {
                return Ok(Published::AlreadyThere);
            }
            let mut parts: Vec<String> = Vec::new();
            if next.get(branch.as_str()) != base.get(branch.as_str()) {
                parts.push(format!("publish {branch}"));
            }
            if !dropped.is_empty() {
                parts.push(format!("drop {}", dropped.join(", ")));
            }
            let commit = self.vcs.write_commit(
                self.repo,
                &remote_tip.iter().cloned().collect::<Vec<_>>(),
                &changes_between(&base, &next),
                &commit_message(&parts.join("; ")),
            )?;
            crate::test_pause::point("records-publish");
            match self.vcs.publish_records(self.repo, remote, &commit)? {
                RecordsPublish::Published => return Ok(Published::Sent { dropped }),
                RecordsPublish::Rejected(why) => return Ok(Published::Rejected(why)),
                RecordsPublish::RemoteMoved => refetch()?,
            }
        }
        Err(anyhow::Error::new(WriteError::Busy))
    }

    /// The release the commit that last changed `branch`'s entry names, when
    /// it names one.
    pub fn writer_of(&self, branch: &BranchName) -> Result<Option<String>, VcsError> {
        let mut tips: Vec<CommitId> = self
            .vcs
            .records_tip(self.repo, RecordsSite::Local)?
            .into_iter()
            .collect();
        tips.extend(self.remote_tips()?.distinct());
        for tip in tips {
            if let Some(message) =
                self.vcs
                    .last_message_touching(self.repo, &tip, &entry_path(branch.as_str()))?
            {
                let release = message.lines().rev().find_map(|line| {
                    line.strip_prefix(WRITER_TRAILER)
                        .and_then(|rest| rest.strip_prefix(": "))
                });
                return Ok(release.map(str::to_owned));
            }
        }
        Ok(None)
    }

    /// The branches whose entry in `view` some records remote, as last fetched,
    /// holds exactly.
    pub fn published_names(&self, view: &View) -> Result<BTreeSet<String>, VcsError> {
        let mut names = BTreeSet::new();
        for tip in self.remote_tips()?.distinct() {
            let theirs = self.snapshot_at(&tip)?;
            for (name, held) in &view.snapshot {
                if theirs
                    .get(name)
                    .is_some_and(|bytes| same_entry(bytes, held))
                {
                    names.insert(name.clone());
                }
            }
        }
        Ok(names)
    }
}

struct SettledView {
    view: View,
}

/// How publishing a record to a remote ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Published {
    /// The remote's records branch now holds the entry, and has lost the
    /// entries listed as dropped.
    Sent { dropped: Vec<String> },
    /// The remote already held everything a publish would send.
    AlreadyThere,
    /// The remote refused its records branch, for the reason it gave.
    Rejected(String),
}

/// What a write left behind.
pub struct Written {
    /// The records after the write.
    pub view: View,
    /// Whether the local records branch moved.
    pub committed: bool,
    /// The branches whose entries the write dropped.
    pub dropped: Vec<String>,
}

fn commit_message(subject: &str) -> String {
    format!(
        "records: {subject}\n\n{WRITER_TRAILER}: {}\n",
        crate::rwv_version()
    )
}

/// The path of the project repo in `workspace` for `project`.
pub fn project_repo(workspace: &Path, project: &crate::manifest::ProjectName) -> PathBuf {
    crate::workspace::project_dir(workspace, project)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> BranchName {
        BranchName(text.to_owned())
    }

    #[test]
    fn every_name_round_trips_through_its_path_and_no_path_is_below_another() {
        let alphabet = ['a', '/', '%', '2', 'F', '5', '.'];
        let mut names: Vec<String> = vec![String::new()];
        for _ in 0..4 {
            let longer: Vec<String> = names
                .iter()
                .flat_map(|n| alphabet.iter().map(move |c| format!("{n}{c}")))
                .collect();
            names.extend(longer);
        }
        names.sort();
        names.dedup();
        let names: Vec<String> = names.into_iter().filter(|n| !n.is_empty()).collect();
        for name in &names {
            let path = entry_path(name);
            assert!(!path.contains('/'), "{name:?} -> {path:?}");
            assert_eq!(entry_name(&path).as_deref(), Some(name.as_str()));
        }
        assert_eq!(entry_path("a/b%c"), "a%2Fb%25c");
    }

    #[test]
    fn a_path_no_name_encodes_to_is_no_entry() {
        for path in ["", "a/b", "a%2fb", "a%", "a%2", "%41", "%2Fb/c"] {
            assert_eq!(entry_name(path), None, "{path:?}");
        }
        let files = vec![
            CommittedFile {
                path: "a%2Fb".to_owned(),
                content: b"x".to_vec(),
            },
            CommittedFile {
                path: "stray/dir".to_owned(),
                content: b"y".to_vec(),
            },
        ];
        assert_eq!(snapshot_of(files), snapshot(&[("a/b", b"x".to_vec())]));
    }

    #[test]
    fn a_name_and_a_name_below_it_are_written_side_by_side() {
        let from = snapshot(&[("a", b"declared".to_vec())]);
        let to = snapshot(&[("a", b"declared".to_vec()), ("a/b", b"declared".to_vec())]);

        let paths: Vec<(String, bool)> = changes_between(&from, &to)
            .into_iter()
            .map(|c| (c.path, c.content.is_some()))
            .collect();

        assert_eq!(paths, vec![("a%2Fb".to_owned(), true)]);
    }

    fn feature(target: &str, as_mode: Mode) -> Record {
        Record::Feature {
            target: name(target),
            as_mode,
        }
    }

    fn file(record: &Record) -> Vec<u8> {
        Entry::Value(record.clone()).render().unwrap()
    }

    fn snapshot(entries: &[(&str, Vec<u8>)]) -> Snapshot {
        entries
            .iter()
            .map(|(name, bytes)| ((*name).to_owned(), bytes.clone()))
            .collect()
    }

    fn view(entries: &[(&str, Vec<u8>)]) -> View {
        View {
            snapshot: snapshot(entries),
        }
    }

    #[test]
    fn an_entry_renders_to_the_file_the_format_names_and_reads_back() {
        let declared = Entry::Value(Record::Declared);
        assert_eq!(declared.render().unwrap(), b"mode = \"declared\"\n");
        let value = Entry::Value(feature("main", Mode::Declared));
        assert_eq!(
            value.render().unwrap(),
            b"mode = \"feature\"\ntarget = \"main\"\nas = \"declared\"\n"
        );
        let conflict = Entry::Conflict(Record::Declared, feature("dev", Mode::Feature));
        assert_eq!(
            String::from_utf8(conflict.render().unwrap()).unwrap(),
            "conflict = [\n  { mode = \"declared\" },\n  { mode = \"feature\", target = \"dev\", as = \"feature\" },\n]\n"
        );
        for entry in [declared, value, conflict] {
            assert_eq!(Entry::parse(&entry.render().unwrap()), entry);
        }
    }

    #[test]
    fn a_branch_name_with_a_quote_is_escaped_and_read_back_whole() {
        let record = feature("we\"ird", Mode::Declared);
        let bytes = file(&record);
        assert_eq!(Entry::parse(&bytes), Entry::Value(record));
    }

    #[test]
    fn what_does_not_read_as_an_entry_is_unreadable_with_a_reason() {
        let unreadable = |text: &[u8]| matches!(Entry::parse(text), Entry::Unreadable(_));
        assert!(unreadable(b"mode = \"declared\"\nfuture = 1\n"));
        assert!(unreadable(b"mode = \"declared\"\ntarget = \"main\"\n"));
        assert!(unreadable(b"mode = \"feature\"\n"));
        assert!(unreadable(
            b"mode = \"feature\"\ntarget = \"main\"\nas = \"both\"\n"
        ));
        assert!(unreadable(b"mode = \"sideways\"\n"));
        assert!(unreadable(b"not toml {"));
        assert!(unreadable(&[0xff, 0xfe]));
        assert!(unreadable(b""));
        assert!(unreadable(b"mode = \"declared\"\nconflict = []\n"));
        assert!(unreadable(b"conflict = [{ mode = \"declared\" }]\n"));
        assert!(unreadable(
            b"conflict = [{ mode = \"declared\" }, { mode = \"declared\" }, { mode = \"declared\" }]\n"
        ));
        assert!(unreadable(
            b"mode = \"feature\"\ntarget = \"\"\nas = \"declared\"\n"
        ));
    }

    #[test]
    fn an_entry_changed_on_one_side_takes_that_side() {
        let declared = file(&Record::Declared);
        let moved = file(&feature("dev", Mode::Declared));
        let base = snapshot(&[("a", declared.clone())]);
        let changed = snapshot(&[("a", moved.clone())]);
        assert_eq!(merge_snapshots(&base, &changed, &base), changed);
        assert_eq!(merge_snapshots(&base, &base, &changed), changed);
    }

    #[test]
    fn entries_for_different_branches_never_conflict() {
        let declared = file(&Record::Declared);
        let base = snapshot(&[]);
        let ours = snapshot(&[("a", declared.clone())]);
        let theirs = snapshot(&[("b", declared.clone())]);
        assert_eq!(
            merge_snapshots(&base, &ours, &theirs),
            snapshot(&[("a", declared.clone()), ("b", declared)])
        );
    }

    #[test]
    fn an_entry_changed_alike_on_both_sides_is_that_value() {
        let moved = file(&feature("dev", Mode::Declared));
        let base = snapshot(&[("a", file(&Record::Declared))]);
        let both = snapshot(&[("a", moved)]);
        assert_eq!(merge_snapshots(&base, &both, &both), both);
    }

    #[test]
    fn an_entry_changed_differently_on_both_sides_stores_both_values() {
        let base = snapshot(&[("a", file(&Record::Declared))]);
        let ours = snapshot(&[("a", file(&feature("dev", Mode::Declared)))]);
        let theirs = snapshot(&[("a", file(&feature("other", Mode::Feature)))]);
        let merged = merge_snapshots(&base, &ours, &theirs);
        assert_eq!(
            Entry::parse(&merged["a"]),
            Entry::Conflict(
                feature("dev", Mode::Declared),
                feature("other", Mode::Feature)
            )
        );
        let two_adds = merge_snapshots(&snapshot(&[]), &ours, &theirs);
        assert_eq!(
            two_adds, merged,
            "an entry both sides added differently clashes too"
        );
    }

    #[test]
    fn an_unreadable_side_is_kept_when_both_sides_changed_so_the_entry_stays_loud() {
        let base = snapshot(&[("a", file(&Record::Declared))]);
        let hand = b"from-a-later-release = 1\n".to_vec();
        let ours = snapshot(&[("a", file(&feature("dev", Mode::Declared)))]);
        let theirs = snapshot(&[("a", hand.clone())]);
        for merged in [
            merge_snapshots(&base, &ours, &theirs),
            merge_snapshots(&base, &theirs, &ours),
        ] {
            assert_eq!(merged["a"], hand);
        }
    }

    #[test]
    fn a_fix_on_one_side_replaces_an_unreadable_entry_the_other_left_alone() {
        let hand = b"from-a-later-release = 1\n".to_vec();
        let base = snapshot(&[("a", hand)]);
        let fixed = snapshot(&[("a", file(&Record::Declared))]);
        assert_eq!(merge_snapshots(&base, &base, &fixed), fixed);
    }

    #[test]
    fn a_removal_against_a_change_keeps_the_change() {
        let base = snapshot(&[("a", file(&Record::Declared))]);
        let gone = snapshot(&[]);
        let moved = snapshot(&[("a", file(&feature("dev", Mode::Declared)))]);
        assert_eq!(merge_snapshots(&base, &gone, &moved), moved);
        assert_eq!(merge_snapshots(&base, &moved, &gone), moved);
        assert_eq!(merge_snapshots(&base, &gone, &base), gone);
    }

    #[test]
    fn a_value_written_another_way_is_the_same_entry() {
        let reformatted = b"as = \"declared\"\ntarget = \"dev\"\nmode = \"feature\"\n".to_vec();
        let canonical = file(&feature("dev", Mode::Declared));
        let base = snapshot(&[("a", file(&Record::Declared))]);
        let merged = merge_snapshots(
            &base,
            &snapshot(&[("a", canonical.clone())]),
            &snapshot(&[("a", reformatted)]),
        );
        assert_eq!(merged["a"], canonical);
    }

    #[test]
    fn a_branch_reads_as_recorded_unrecorded_conflicted_unreadable_or_on_a_cycle() {
        let records = view(&[
            ("main", file(&Record::Declared)),
            ("feat", file(&feature("main", Mode::Declared))),
            (
                "clash",
                Entry::Conflict(Record::Declared, feature("main", Mode::Declared))
                    .render()
                    .unwrap(),
            ),
            ("odd", b"x = 1\n".to_vec()),
            ("a", file(&feature("b", Mode::Feature))),
            ("b", file(&feature("a", Mode::Feature))),
            ("me", file(&feature("me", Mode::Feature))),
        ]);
        assert_eq!(
            records.lookup(&name("main")),
            Lookup::Value(Record::Declared)
        );
        assert_eq!(
            records.lookup(&name("feat")),
            Lookup::Value(feature("main", Mode::Declared))
        );
        assert_eq!(records.lookup(&name("gone")), Lookup::Unrecorded);
        assert!(matches!(
            records.lookup(&name("clash")),
            Lookup::Conflict(..)
        ));
        assert!(matches!(
            records.lookup(&name("odd")),
            Lookup::Unreadable(_)
        ));
        assert_eq!(
            records.lookup(&name("a")),
            Lookup::Cycle(vec![name("a"), name("b"), name("a")])
        );
        assert_eq!(
            records.lookup(&name("me")),
            Lookup::Cycle(vec![name("me"), name("me")])
        );
    }

    #[test]
    fn a_branch_whose_chain_runs_into_a_cycle_it_is_not_on_still_reads() {
        let records = view(&[
            ("tail", file(&feature("a", Mode::Feature))),
            ("a", file(&feature("b", Mode::Feature))),
            ("b", file(&feature("a", Mode::Feature))),
        ]);
        assert!(matches!(records.lookup(&name("tail")), Lookup::Value(_)));
        assert!(matches!(records.lookup(&name("a")), Lookup::Cycle(_)));
    }

    #[test]
    fn a_chain_back_to_a_branch_is_found_through_every_target_of_a_conflict() {
        let records = view(&[
            ("main", file(&Record::Declared)),
            (
                "m",
                Entry::Conflict(feature("main", Mode::Declared), feature("w", Mode::Feature))
                    .render()
                    .unwrap(),
            ),
        ]);
        assert_eq!(
            records.chain_back_to(&name("w"), vec![name("m")]),
            Some(vec![name("w"), name("m"), name("w")])
        );
        assert_eq!(records.chain_back_to(&name("x"), vec![name("m")]), None);
    }

    #[test]
    fn the_dependents_of_a_branch_are_the_records_that_name_it() {
        let records = view(&[
            ("main", file(&Record::Declared)),
            ("one", file(&feature("main", Mode::Declared))),
            ("two", file(&feature("one", Mode::Feature))),
            ("three", file(&feature("main", Mode::Declared))),
        ]);
        assert_eq!(
            records.dependents(&name("main")),
            vec![name("one"), name("three")]
        );
        assert_eq!(records.dependents(&name("two")), Vec::<BranchName>::new());
    }
}
