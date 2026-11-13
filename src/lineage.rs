//! The lineage rule: a member's work moves into a receiving branch only when
//! that branch contains the work's base, the newest commit of the work's
//! first-parent chain that the target's history pinned or built on.

use crate::manifest::{LockFile, RepoPath};
use crate::vcs::{CommitId, ResolvedRevisionId, Vcs, VcsError};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A pin of the target's history that the history moved off by name: `lock`
/// held `pin`, and every later lock that changed the member's pin from it
/// either records the move by name or advances to a pin that was itself
/// moved off by name; `by` is the oldest record that chain reaches, written
/// by `verb`. Where this clone lacks the pin, it does not decide the base.
/// `known` where the source's project branch already holds `by`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Superseded {
    pub(crate) lock: CommitId,
    pub(crate) pin: CommitId,
    pub(crate) by: CommitId,
    pub(crate) verb: String,
    pub(crate) known: bool,
}

impl Superseded {
    pub(crate) fn line(&self, member: &RepoPath) -> String {
        skipped_line(member, &self.lock, &self.pin, &self.by, &self.verb)
    }
}

pub(crate) fn skipped_line(
    member: &RepoPath,
    lock: &CommitId,
    pin: &CommitId,
    by: &CommitId,
    verb: &str,
) -> String {
    format!(
        "{member}: history pin skipped: {} pinned {}, which this clone lacks; {} re-pinned it \
         by name later ({verb}), so it does not decide the base",
        lock.short(),
        pin.short(),
        by.short()
    )
}

/// Every pin each member had in the locks of a project branch's history,
/// through every parent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PinHistory {
    pub(crate) pins: BTreeMap<RepoPath, BTreeSet<CommitId>>,
    dir: PathBuf,
    tip: Option<CommitId>,
    locks: BTreeMap<CommitId, (usize, BTreeMap<RepoPath, CommitId>)>,
    source: Option<CommitId>,
}

fn pins_at(vcs: &dyn Vcs, dir: &Path, commit: &CommitId) -> Option<BTreeMap<RepoPath, CommitId>> {
    let text = vcs
        .read_file_at_revision(
            dir,
            &ResolvedRevisionId::from(commit.clone()),
            Path::new(LockFile::FILE_NAME),
        )
        .ok()?;
    let lock = LockFile::from_json_str(&text).ok()?;
    Some(
        lock.iter_entries()
            .filter_map(|(path, entry)| Some((path.clone(), entry.version.as_commit_id()?)))
            .collect(),
    )
}

/// A member commit this clone holds, known only by resolving it: ancestry
/// is asked of nothing else.
struct Held(ResolvedRevisionId);

/// The member's clone, as the supersession of its pins reads it.
pub(crate) struct MemberClone<'a> {
    vcs: &'a dyn Vcs,
    dir: &'a Path,
    lacking: Option<&'a BTreeSet<CommitId>>,
}

impl<'a> MemberClone<'a> {
    /// A clone known to lack exactly `lacking` of the member's history pins.
    pub(crate) fn lacking(
        vcs: &'a dyn Vcs,
        dir: &'a Path,
        lacking: &'a BTreeSet<CommitId>,
    ) -> Self {
        Self {
            vcs,
            dir,
            lacking: Some(lacking),
        }
    }

    /// A clone whose pins are each resolved where asked.
    pub(crate) fn probed(vcs: &'a dyn Vcs, dir: &'a Path) -> Self {
        Self {
            vcs,
            dir,
            lacking: None,
        }
    }

    fn held(&self, pin: &CommitId) -> Option<Held> {
        if self.lacking.is_some_and(|lacking| lacking.contains(pin)) {
            return None;
        }
        self.vcs
            .resolve_revision(self.dir, pin.as_str())
            .ok()
            .map(Held)
    }

    fn lacks(&self, pin: &CommitId) -> bool {
        match self.lacking {
            Some(lacking) => lacking.contains(pin),
            None => self.held(pin).is_none(),
        }
    }

    fn advances(&self, from: &Held, to: &Held) -> Result<bool, VcsError> {
        self.vcs.is_ancestor(self.dir, &from.0, &to.0)
    }
}

/// Where a lock commit `at` changed a member's pin from the one its parent's
/// lock, held by `holder`, pinned.
struct Exit {
    at: CommitId,
    holder: CommitId,
    to: Option<CommitId>,
}

#[derive(Default)]
struct Chain {
    off: bool,
    recorded: Option<(usize, CommitId, CommitId, String)>,
    advances: Vec<(CommitId, CommitId)>,
}

impl PinHistory {
    /// Read every lock `tip`'s history held.
    pub(crate) fn of(
        project_vcs: &dyn Vcs,
        project_dir: &Path,
        tip: &CommitId,
    ) -> Result<Self, VcsError> {
        let mut history = Self {
            dir: project_dir.to_path_buf(),
            tip: Some(tip.clone()),
            ..Self::default()
        };
        for (order, commit) in project_vcs
            .commits_changing(project_dir, tip, LockFile::FILE_NAME)?
            .into_iter()
            .enumerate()
        {
            let held = pins_at(project_vcs, project_dir, &commit).unwrap_or_default();
            for (path, pin) in &held {
                history
                    .pins
                    .entry(path.clone())
                    .or_default()
                    .insert(pin.clone());
            }
            history.locks.insert(commit, (order, held));
        }
        Ok(history)
    }

    /// The same history, its re-pins judged known where `source`, the
    /// source's project branch, already holds them.
    pub(crate) fn seen_from(mut self, source: &CommitId) -> Self {
        self.source = Some(source.clone());
        self
    }

    pub(crate) fn of_member(&self, member: &RepoPath) -> BTreeSet<CommitId> {
        self.pins.get(member).cloned().unwrap_or_default()
    }

    /// Which of `pins`, pins of `member` in this history, the history moved
    /// off by name, judged against the member's `clone`: a pin is moved off
    /// unless some chain of later locks changing the member's pin without a
    /// record reaches the tip's pin, no pin, or a pin the clone holds that is
    /// no advance from the one before it.
    pub(crate) fn superseded(
        &self,
        project_vcs: &dyn Vcs,
        member: &RepoPath,
        pins: &BTreeSet<CommitId>,
        clone: &MemberClone<'_>,
    ) -> Result<Vec<Superseded>, VcsError> {
        let Some(tip) = self.tip.as_ref().filter(|_| !pins.is_empty()) else {
            return Ok(Vec::new());
        };
        let graph: BTreeMap<CommitId, Vec<CommitId>> =
            project_vcs.ancestry(&self.dir, tip)?.into_iter().collect();
        let holder = |at: &CommitId| -> Option<CommitId> {
            let mut at = at;
            loop {
                if self.locks.contains_key(at) {
                    return Some(at.clone());
                }
                at = graph.get(at)?.first()?;
            }
        };
        let pin_at = |lock: &CommitId| self.locks.get(lock)?.1.get(member).cloned();
        let mut exits: BTreeMap<CommitId, Vec<Exit>> = BTreeMap::new();
        for (at, (_, held)) in &self.locks {
            let to = held.get(member);
            for parent in graph.get(at).into_iter().flatten() {
                let Some(parent_lock) = holder(parent) else {
                    continue;
                };
                let Some(from) = pin_at(&parent_lock) else {
                    continue;
                };
                if Some(&from) != to {
                    exits.entry(from).or_default().push(Exit {
                        at: at.clone(),
                        holder: parent_lock,
                        to: to.cloned(),
                    });
                }
            }
        }
        let tip_pin = holder(tip).and_then(|lock| pin_at(&lock));
        let mut records: BTreeMap<CommitId, Vec<crate::repin::Repin>> = BTreeMap::new();
        let mut chains: BTreeMap<CommitId, Chain> = BTreeMap::new();
        let mut children = None;
        let mut pending: Vec<CommitId> = pins.iter().cloned().collect();
        while let Some(pin) = pending.pop() {
            if chains.contains_key(&pin) {
                continue;
            }
            let mut chain = Chain {
                off: tip_pin.as_ref() == Some(&pin) || !exits.contains_key(&pin),
                ..Chain::default()
            };
            let mut from_held = None;
            for exit in exits.get(&pin).into_iter().flatten() {
                let Some(to) = &exit.to else {
                    chain.off = true;
                    continue;
                };
                if !records.contains_key(&exit.at) {
                    let read = crate::repin::recorded(project_vcs, &self.dir, &exit.at)?;
                    records.insert(exit.at.clone(), read);
                }
                if let Some(record) = records[&exit.at].iter().find(|record| {
                    &record.member == member
                        && record.from.as_ref() == Some(&pin)
                        && &record.to == to
                }) {
                    let order = self.locks[&exit.at].0;
                    if chain.recorded.as_ref().is_none_or(|(o, ..)| order > *o) {
                        chain.recorded = Some((
                            order,
                            exit.at.clone(),
                            exit.holder.clone(),
                            record.verb.clone(),
                        ));
                    }
                    continue;
                }
                let advance = if clone.lacks(to) {
                    true
                } else {
                    let from = from_held.get_or_insert_with(|| clone.held(&pin));
                    match (from.as_ref(), clone.held(to)) {
                        (Some(from), Some(to)) => clone.advances(from, &to)?,
                        _ => false,
                    }
                };
                if advance {
                    chain.advances.push((to.clone(), exit.holder.clone()));
                    pending.push(to.clone());
                    continue;
                }
                let waiver = if graph.get(&exit.at).is_some_and(|parents| parents.len() > 1) {
                    let children = children.get_or_insert_with(|| {
                        let mut children: BTreeMap<&CommitId, Vec<&CommitId>> = BTreeMap::new();
                        for (commit, parents) in &graph {
                            for parent in parents {
                                children.entry(parent).or_default().push(commit);
                            }
                        }
                        children
                    });
                    let mut after = vec![&exit.at];
                    let mut seen = BTreeSet::new();
                    let mut found: Option<(usize, CommitId, String)> = None;
                    while let Some(at) = after.pop() {
                        if !seen.insert(at) {
                            continue;
                        }
                        after.extend(children.get(at).into_iter().flatten().copied());
                        let Some((order, held)) = self.locks.get(at) else {
                            continue;
                        };
                        if found.as_ref().is_some_and(|(o, ..)| o >= order) {
                            continue;
                        }
                        if !records.contains_key(at) {
                            let read = crate::repin::recorded(project_vcs, &self.dir, at)?;
                            records.insert(at.clone(), read);
                        }
                        for record in records[at].iter().filter(|r| &r.member == member) {
                            let trusted = held.get(member) == Some(&record.to)
                                && graph.get(at).into_iter().flatten().any(|parent| {
                                    holder(parent).and_then(|lock| pin_at(&lock)) == record.from
                                });
                            let Some(from) = record.from.as_ref().filter(|_| trusted) else {
                                continue;
                            };
                            let (Some(from), Some(back)) =
                                (clone.held(from), clone.held(&record.to))
                            else {
                                continue;
                            };
                            let abandons = match from_held.get_or_insert_with(|| clone.held(&pin)) {
                                Some(pin) => !clone.advances(pin, &back)?,
                                None => true,
                            };
                            if abandons && !clone.advances(&from, &back)? {
                                found = Some((*order, at.clone(), record.verb.clone()));
                            }
                        }
                    }
                    found
                } else {
                    None
                };
                match waiver {
                    Some((order, by, verb)) => {
                        if chain.recorded.as_ref().is_none_or(|(o, ..)| order > *o) {
                            chain.recorded = Some((order, by, exit.holder.clone(), verb));
                        }
                    }
                    None => chain.off = true,
                }
            }
            chains.insert(pin, chain);
        }
        loop {
            let off: BTreeSet<CommitId> = chains
                .iter()
                .filter(|(_, chain)| chain.off)
                .map(|(pin, _)| pin.clone())
                .collect();
            let mut changed = false;
            for chain in chains.values_mut().filter(|chain| !chain.off) {
                if chain.advances.iter().any(|(to, _)| off.contains(to)) {
                    chain.off = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut reached: BTreeMap<CommitId, (usize, CommitId, CommitId, String)> = chains
            .iter()
            .filter(|(_, chain)| !chain.off)
            .filter_map(|(pin, chain)| Some((pin.clone(), chain.recorded.clone()?)))
            .collect();
        loop {
            let mut changed = false;
            for (pin, chain) in chains.iter().filter(|(_, chain)| !chain.off) {
                for (to, holder) in &chain.advances {
                    let Some((order, by, _, verb)) = reached.get(to).cloned() else {
                        continue;
                    };
                    if reached.get(pin).is_none_or(|(o, ..)| order > *o) {
                        reached.insert(pin.clone(), (order, by, holder.clone(), verb));
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let mut found = Vec::new();
        for pin in pins {
            let Some((_, by, lock, verb)) = reached.get(pin) else {
                continue;
            };
            let known = match &self.source {
                Some(source) => project_vcs.is_ancestor(
                    &self.dir,
                    &ResolvedRevisionId::from(by.clone()),
                    &ResolvedRevisionId::from(source.clone()),
                )?,
                None => false,
            };
            found.push(Superseded {
                lock: lock.clone(),
                pin: pin.clone(),
                by: by.clone(),
                verb: verb.clone(),
                known,
            });
        }
        Ok(found)
    }
}

/// What the lineage rule finds for one member's work against a receiving
/// branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lineage {
    /// The receiving branch contains the base, or the target never pinned
    /// the member, so it has none; `skipped` are the pins this clone lacks
    /// that a later lock re-pinned by name, which decided nothing.
    Passes {
        base: Option<CommitId>,
        skipped: Vec<Superseded>,
    },
    /// The receiving branch lacks the base.
    Off { base: CommitId },
    /// A pin this clone lacks could make the base newer than any commit the
    /// receiving branch holds, so the answer is not this clone's to give.
    Unreachable { absent: Vec<CommitId> },
}

/// The pins of `pins` this clone holds, and those it lacks.
pub(crate) fn split_present(
    vcs: &dyn Vcs,
    dir: &Path,
    pins: &BTreeSet<CommitId>,
) -> (Vec<CommitId>, Vec<CommitId>) {
    pins.iter()
        .cloned()
        .partition(|pin| vcs.resolve_revision(dir, pin.as_str()).is_ok())
}

/// The newest commit of `chain` (newest first) that is an ancestor of
/// `of`, found by halving: the chain's commits that `of` contains are a
/// suffix of it.
fn newest_contained(
    vcs: &dyn Vcs,
    dir: &Path,
    chain: &[CommitId],
    of: &CommitId,
) -> Result<Option<usize>, VcsError> {
    let of = ResolvedRevisionId::from(of.clone());
    let contains =
        |i: usize| vcs.is_ancestor(dir, &ResolvedRevisionId::from(chain[i].clone()), &of);
    if chain.is_empty() || !contains(chain.len() - 1)? {
        return Ok(None);
    }
    let (mut lo, mut hi) = (0, chain.len() - 1);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if contains(mid)? {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    Ok(Some(lo))
}

/// The base of `work`: the newest commit on its first-parent chain that a
/// pin in `pins` is, or contains. `None` where no pin reaches the chain.
pub(crate) fn base_of(
    vcs: &dyn Vcs,
    dir: &Path,
    work: &CommitId,
    present: &[CommitId],
) -> Result<Option<CommitId>, VcsError> {
    let chain = vcs.first_parent_chain(dir, work)?;
    let mut newest: Option<usize> = None;
    for pin in present {
        if let Some(at) = newest_contained(vcs, dir, &chain, pin)? {
            newest = Some(newest.map_or(at, |n| n.min(at)));
        }
    }
    Ok(newest.map(|at| chain[at].clone()))
}

/// Judge `work` against `receiving` by the pins the target's history held,
/// less those this clone lacks that `supersede` finds the history moved off
/// by name; it is asked only of pins this clone lacks.
pub(crate) fn judge(
    vcs: &dyn Vcs,
    dir: &Path,
    work: &CommitId,
    pins: &BTreeSet<CommitId>,
    supersede: impl FnOnce(&BTreeSet<CommitId>) -> Result<Vec<Superseded>, VcsError>,
    receiving: &CommitId,
) -> Result<Lineage, VcsError> {
    if pins.is_empty() {
        return Ok(Lineage::Passes {
            base: None,
            skipped: Vec::new(),
        });
    }
    let (present, absent) = split_present(vcs, dir, pins);
    let superseded = if absent.is_empty() {
        Vec::new()
    } else {
        supersede(&absent.iter().cloned().collect())?
    };
    let (skipped, absent): (Vec<CommitId>, Vec<CommitId>) = absent
        .into_iter()
        .partition(|pin| superseded.iter().any(|s| &s.pin == pin));
    let base = base_of(vcs, dir, work, &present)?;
    let receiving_rev = ResolvedRevisionId::from(receiving.clone());
    if let Some(base) = &base {
        if !vcs.is_ancestor(dir, &ResolvedRevisionId::from(base.clone()), &receiving_rev)? {
            return Ok(Lineage::Off { base: base.clone() });
        }
    }
    let lands = (!absent.is_empty() || !skipped.is_empty())
        && !vcs.is_ancestor(dir, &ResolvedRevisionId::from(work.clone()), &receiving_rev)?;
    if lands && !absent.is_empty() {
        return Ok(Lineage::Unreachable { absent });
    }
    let skipped = if lands {
        superseded
            .into_iter()
            .filter(|s| skipped.contains(&s.pin))
            .collect()
    } else {
        Vec::new()
    };
    Ok(Lineage::Passes { base, skipped })
}

/// The commits of `work` that `receiving` lacks and that no pin of the
/// target's history reaches: the work a landing would bring, the list C15
/// prints. Pins this clone lacks reach nothing here.
pub(crate) fn new_work(
    vcs: &dyn Vcs,
    dir: &Path,
    work: &CommitId,
    receiving: Option<&CommitId>,
    pins: &BTreeSet<CommitId>,
) -> Result<Vec<crate::vcs::CommitSummary>, VcsError> {
    let (mut excluding, _) = split_present(vcs, dir, pins);
    excluding.extend(receiving.cloned());
    vcs.commits_excluding(
        dir,
        &ResolvedRevisionId::from(work.clone()),
        &excluding,
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit(dir: &Path, text: &str) -> CommitId {
        std::fs::write(dir.join(text), text).unwrap();
        git(dir, &["add", text]);
        git(dir, &["commit", "-qm", text]);
        CommitId::parse(&git(dir, &["rev-parse", "HEAD"])).unwrap()
    }

    fn repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path().join("m");
        std::fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        (tmp, dir)
    }

    fn pins(of: &[&CommitId]) -> BTreeSet<CommitId> {
        of.iter().map(|c| (*c).clone()).collect()
    }

    #[test]
    fn work_cut_from_the_targets_pin_passes_where_the_receiving_branch_holds_that_pin() {
        let (_tmp, dir) = repo();
        let vcs = crate::git::git_vcs();
        let pin = commit(&dir, "a");
        git(&dir, &["checkout", "-q", "-b", "feat"]);
        let work = commit(&dir, "f");
        git(&dir, &["checkout", "-q", "main"]);
        let u = commit(&dir, "b");
        assert_eq!(
            judge(
                vcs.as_ref(),
                &dir,
                &work,
                &pins(&[&pin]),
                |_| Ok(Vec::new()),
                &u
            )
            .unwrap(),
            Lineage::Passes {
                base: Some(pin),
                skipped: vec![]
            }
        );
    }

    #[test]
    fn work_built_on_another_lineage_is_off_lineage() {
        let (_tmp, dir) = repo();
        let vcs = crate::git::git_vcs();
        let root = commit(&dir, "a");
        git(&dir, &["checkout", "-q", "-b", "old"]);
        let old_pin = commit(&dir, "o");
        let work = commit(&dir, "f");
        git(&dir, &["checkout", "-q", "main"]);
        let u = commit(&dir, "b");
        assert_eq!(
            judge(
                vcs.as_ref(),
                &dir,
                &work,
                &pins(&[&root, &old_pin]),
                |_| Ok(Vec::new()),
                &u
            )
            .unwrap(),
            Lineage::Off { base: old_pin }
        );
    }

    #[test]
    fn a_merge_of_another_lineage_enters_as_a_second_parent_and_passes() {
        let (_tmp, dir) = repo();
        let vcs = crate::git::git_vcs();
        let pin = commit(&dir, "a");
        git(&dir, &["checkout", "-q", "-b", "other"]);
        let other_pin = commit(&dir, "o");
        git(&dir, &["checkout", "-q", "-b", "feat", pin.as_str()]);
        commit(&dir, "f");
        git(&dir, &["merge", "-q", "--no-ff", "-m", "bump", "other"]);
        let work = CommitId::parse(&git(&dir, &["rev-parse", "HEAD"])).unwrap();
        git(&dir, &["checkout", "-q", "main"]);
        let u = commit(&dir, "b");
        assert_eq!(
            judge(
                vcs.as_ref(),
                &dir,
                &work,
                &pins(&[&pin, &other_pin]),
                |_| Ok(Vec::new()),
                &u
            )
            .unwrap(),
            Lineage::Passes {
                base: Some(pin),
                skipped: vec![]
            }
        );
    }

    #[test]
    fn commits_merged_from_the_targets_old_lineage_pass() {
        let (_tmp, dir) = repo();
        let vcs = crate::git::git_vcs();
        let pin = commit(&dir, "a");
        git(&dir, &["checkout", "-q", "-b", "old"]);
        commit(&dir, "o");
        git(&dir, &["checkout", "-q", "-b", "feat", pin.as_str()]);
        commit(&dir, "f");
        git(&dir, &["merge", "-q", "--no-ff", "-m", "from old", "old"]);
        let work = CommitId::parse(&git(&dir, &["rev-parse", "HEAD"])).unwrap();
        git(&dir, &["checkout", "-q", "main"]);
        let u = commit(&dir, "b");
        assert!(matches!(
            judge(
                vcs.as_ref(),
                &dir,
                &work,
                &pins(&[&pin]),
                |_| Ok(Vec::new()),
                &u
            )
            .unwrap(),
            Lineage::Passes { .. }
        ));
    }

    #[test]
    fn a_pin_this_clone_lacks_refuses_rather_than_being_skipped() {
        let (_tmp, dir) = repo();
        let vcs = crate::git::git_vcs();
        let pin = commit(&dir, "a");
        git(&dir, &["checkout", "-q", "-b", "feat"]);
        let work = commit(&dir, "f");
        git(&dir, &["checkout", "-q", "main"]);
        let u = commit(&dir, "b");
        let absent = CommitId::parse(&"ab".repeat(20)).unwrap();
        assert_eq!(
            judge(
                vcs.as_ref(),
                &dir,
                &work,
                &pins(&[&pin, &absent]),
                |_| Ok(Vec::new()),
                &u
            )
            .unwrap(),
            Lineage::Unreachable {
                absent: vec![absent]
            }
        );
    }

    fn lock_commit(dir: &Path, pin: &CommitId, repin: Option<(&CommitId, &str)>) -> CommitId {
        std::fs::write(
            dir.join(LockFile::FILE_NAME),
            format!(
                r#"{{"repositories":{{"m":{{"type":"git","url":"u","version":"{}"}}}}}}"#,
                pin.as_str()
            ),
        )
        .unwrap();
        git(dir, &["add", LockFile::FILE_NAME]);
        let mut message = format!("lock {}", pin.short());
        if let Some((from, verb)) = repin {
            message.push_str(&format!(
                "\n\nRwv-Repin: \"m\" {} {} {verb}",
                from.as_str(),
                pin.as_str()
            ));
        }
        git(dir, &["commit", "-qm", &message]);
        CommitId::parse(&git(dir, &["rev-parse", "HEAD"])).unwrap()
    }

    fn fake(fill: char) -> CommitId {
        CommitId::parse(&fill.to_string().repeat(40)).unwrap()
    }

    fn superseded_in(dir: &Path) -> Vec<Superseded> {
        let vcs = crate::git::git_vcs();
        let tip = CommitId::parse(&git(dir, &["rev-parse", "HEAD"])).unwrap();
        let history = PinHistory::of(vcs.as_ref(), dir, &tip).unwrap();
        let member = RepoPath::new("m").unwrap();
        history
            .superseded(
                vcs.as_ref(),
                &member,
                &history.of_member(&member),
                &MemberClone::probed(vcs.as_ref(), dir),
            )
            .unwrap()
    }

    fn superseded_pins(dir: &Path) -> BTreeSet<CommitId> {
        superseded_in(dir).into_iter().map(|s| s.pin).collect()
    }

    #[test]
    fn a_pin_re_pinned_by_name_and_that_re_pin_re_pinned_again_are_both_superseded() {
        let (_tmp, dir) = repo();
        let (p1, p2, p3) = (fake('1'), fake('2'), fake('3'));
        lock_commit(&dir, &p1, None);
        lock_commit(&dir, &p2, Some((&p1, "member pin")));
        lock_commit(&dir, &p3, Some((&p2, "member pin")));
        assert_eq!(superseded_pins(&dir), pins(&[&p1, &p2]));
    }

    #[test]
    fn the_fact_names_the_record_that_moved_the_pin_off_not_a_later_one() {
        let (_tmp, dir) = repo();
        let (p1, p2, p3) = (fake('1'), fake('2'), fake('3'));
        let held = lock_commit(&dir, &p1, None);
        let first = lock_commit(&dir, &p2, Some((&p1, "member pin")));
        lock_commit(&dir, &p3, Some((&p2, "member pin")));
        let of_p1 = superseded_in(&dir)
            .into_iter()
            .find(|s| s.pin == p1)
            .unwrap();
        assert_eq!((of_p1.lock, of_p1.by), (held, first));
    }

    #[test]
    fn a_merge_landing_whose_record_moves_the_features_last_pin_supersedes_its_earlier_pins() {
        let (_tmp, dir) = repo();
        let (n, p1, p2, q) = (fake('a'), fake('b'), fake('c'), fake('d'));
        lock_commit(&dir, &n, None);
        git(&dir, &["checkout", "-q", "-b", "side"]);
        lock_commit(&dir, &p1, None);
        lock_commit(&dir, &p2, None);
        git(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "other");
        git(&dir, &["merge", "-q", "--no-commit", "-s", "ours", "side"]);
        lock_commit(&dir, &q, Some((&p2, "land")));
        assert_eq!(superseded_pins(&dir), pins(&[&p1, &p2]));
    }

    #[test]
    fn a_merge_recording_one_parents_pin_leaves_the_other_sides_unrecorded_jump_deciding() {
        let (_tmp, dir) = repo();
        let (n, p, q) = (fake('a'), fake('b'), fake('c'));
        lock_commit(&dir, &n, None);
        git(&dir, &["checkout", "-q", "-b", "side"]);
        lock_commit(&dir, &p, None);
        git(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "other");
        git(&dir, &["merge", "-q", "--no-commit", "-s", "ours", "side"]);
        lock_commit(&dir, &q, Some((&n, "land")));
        assert!(superseded_pins(&dir).is_empty());
    }

    fn merge_of_side(dir: &Path, n: &CommitId, p: &CommitId) {
        lock_commit(dir, n, None);
        git(dir, &["checkout", "-q", "-b", "side"]);
        lock_commit(dir, p, None);
        git(dir, &["checkout", "-q", "main"]);
        commit(dir, "other");
        git(dir, &["merge", "-q", "--no-commit", "-s", "ours", "side"]);
    }

    #[test]
    fn a_merge_re_pinning_from_its_second_parents_pin_is_trusted() {
        let (_tmp, dir) = repo();
        let (n, p, q) = (fake('a'), fake('b'), fake('c'));
        merge_of_side(&dir, &n, &p);
        lock_commit(&dir, &q, Some((&p, "land")));
        assert_eq!(superseded_pins(&dir), pins(&[&p]));
    }

    #[test]
    fn a_merge_re_pinning_from_a_pin_neither_parent_holds_supersedes_nothing() {
        let (_tmp, dir) = repo();
        let (n, p, q) = (fake('a'), fake('b'), fake('c'));
        merge_of_side(&dir, &n, &p);
        lock_commit(&dir, &q, Some((&fake('9'), "land")));
        assert!(superseded_pins(&dir).is_empty());
    }

    #[test]
    fn a_record_whose_pins_are_not_its_commits_supersedes_nothing() {
        let (_tmp, dir) = repo();
        let (p1, p2) = (fake('1'), fake('2'));
        lock_commit(&dir, &p1, None);
        lock_commit(&dir, &p2, Some((&fake('9'), "member pin")));
        assert!(superseded_pins(&dir).is_empty());
    }

    #[test]
    fn a_record_whose_new_pin_is_not_its_commits_supersedes_nothing() {
        let (_tmp, dir) = repo();
        let (p1, p2, stale) = (fake('1'), fake('2'), fake('8'));
        lock_commit(&dir, &p1, None);
        let text = format!(
            "\n\nRwv-Repin: \"m\" {} {} member pin",
            p1.as_str(),
            stale.as_str()
        );
        lock_commit(&dir, &p2, None);
        git(
            &dir,
            &["commit", "-q", "--amend", "-m", &format!("lock{text}")],
        );
        assert!(superseded_pins(&dir).is_empty());
    }

    #[test]
    fn a_pin_a_later_lock_holds_again_is_not_superseded() {
        let (_tmp, dir) = repo();
        let (p1, p2) = (fake('1'), fake('2'));
        lock_commit(&dir, &p1, None);
        lock_commit(&dir, &p2, Some((&p1, "member pin")));
        lock_commit(&dir, &p1, None);
        assert!(superseded_pins(&dir).is_empty());
    }

    #[test]
    fn absent_pins_advancing_to_an_absent_pin_moved_off_by_name_are_superseded() {
        let (_tmp, dir) = repo();
        let (p1, p2, p3) = (fake('1'), fake('2'), fake('3'));
        lock_commit(&dir, &p1, None);
        lock_commit(&dir, &p2, None);
        lock_commit(&dir, &p3, Some((&p2, "land")));
        assert_eq!(superseded_pins(&dir), pins(&[&p1, &p2]));
    }

    #[test]
    fn an_absent_pin_followed_by_a_held_pin_with_no_record_refuses() {
        let (_tmp, dir) = repo();
        let vcs = crate::git::git_vcs();
        let gone = fake('1');
        let base = commit(&dir, "a");
        lock_commit(&dir, &gone, None);
        lock_commit(&dir, &base, None);
        lock_commit(&dir, &fake('3'), Some((&base, "member pin")));
        assert_eq!(superseded_pins(&dir), pins(&[&base]));
        let tip = CommitId::parse(&git(&dir, &["rev-parse", "HEAD"])).unwrap();
        let history = PinHistory::of(vcs.as_ref(), &dir, &tip).unwrap();
        let member = RepoPath::new("m").unwrap();
        git(&dir, &["checkout", "-q", "-b", "feat", base.as_str()]);
        let work = commit(&dir, "f");
        let pins = history.of_member(&member);
        let judged = judge(
            vcs.as_ref(),
            &dir,
            &work,
            &pins,
            |absent| {
                history.superseded(
                    vcs.as_ref(),
                    &member,
                    absent,
                    &MemberClone::lacking(vcs.as_ref(), &dir, absent),
                )
            },
            &base,
        )
        .unwrap();
        assert!(
            matches!(&judged, Lineage::Unreachable { absent } if absent.contains(&gone)),
            "{judged:?}"
        );
    }

    #[test]
    fn a_held_pin_is_superseded_through_an_advance_and_not_through_a_jump() {
        let (_tmp, dir) = repo();
        let a = commit(&dir, "a");
        let b = commit(&dir, "b");
        lock_commit(&dir, &a, None);
        lock_commit(&dir, &b, None);
        lock_commit(&dir, &fake('3'), Some((&b, "land")));
        assert_eq!(superseded_pins(&dir), pins(&[&a, &b]));

        let (_tmp, dir) = repo();
        let root = commit(&dir, "r");
        let a = commit(&dir, "a");
        git(&dir, &["checkout", "-q", "-b", "other", root.as_str()]);
        let b = commit(&dir, "b");
        lock_commit(&dir, &a, None);
        lock_commit(&dir, &b, None);
        lock_commit(&dir, &fake('3'), Some((&b, "land")));
        assert_eq!(superseded_pins(&dir), pins(&[&b]));
    }

    fn merge_keeping(dir: &Path, kept: &CommitId, lost: &CommitId) {
        lock_commit(dir, &fake('a'), None);
        git(dir, &["checkout", "-q", "-b", "side"]);
        lock_commit(dir, lost, None);
        git(dir, &["checkout", "-q", "main"]);
        lock_commit(dir, kept, None);
        git(dir, &["merge", "-q", "--no-commit", "-s", "ours", "side"]);
        git(
            dir,
            &["commit", "-q", "-m", "merge keeps the first parent's pin"],
        );
    }

    #[test]
    fn a_pin_a_merge_left_is_waived_by_a_later_trusted_backward_re_pin() {
        let (_tmp, dir) = repo();
        let kept = commit(&dir, "n");
        let later = commit(&dir, "n2");
        let lost = fake('1');
        merge_keeping(&dir, &kept, &lost);
        lock_commit(&dir, &later, None);
        lock_commit(&dir, &kept, Some((&later, "member pin")));
        assert!(superseded_pins(&dir).contains(&lost));
    }

    #[test]
    fn a_jump_off_a_pin_outside_a_merge_is_waived_by_no_later_re_pin() {
        let (_tmp, dir) = repo();
        let kept = commit(&dir, "n");
        let later = commit(&dir, "n2");
        let lost = fake('1');
        lock_commit(&dir, &lost, None);
        lock_commit(&dir, &kept, None);
        lock_commit(&dir, &later, None);
        lock_commit(&dir, &kept, Some((&later, "member pin")));
        assert!(!superseded_pins(&dir).contains(&lost));
    }

    #[test]
    fn a_forward_re_pin_by_name_waives_no_pin_a_merge_left() {
        let (_tmp, dir) = repo();
        let earlier = commit(&dir, "n0");
        let kept = commit(&dir, "n");
        let lost = fake('1');
        merge_keeping(&dir, &kept, &lost);
        lock_commit(&dir, &earlier, None);
        lock_commit(&dir, &kept, Some((&earlier, "member pin")));
        assert!(!superseded_pins(&dir).contains(&lost));
    }

    #[test]
    fn a_re_pin_from_a_pin_its_parent_does_not_hold_waives_no_pin_a_merge_left() {
        let (_tmp, dir) = repo();
        let kept = commit(&dir, "n");
        let later = commit(&dir, "n2");
        let elsewhere = commit(&dir, "n3");
        let lost = fake('1');
        merge_keeping(&dir, &kept, &lost);
        lock_commit(&dir, &later, None);
        lock_commit(&dir, &kept, Some((&elsewhere, "member pin")));
        assert!(!superseded_pins(&dir).contains(&lost));
    }

    #[test]
    fn a_pin_with_two_recorded_exits_names_the_older_record() {
        let (_tmp, dir) = repo();
        let held = commit(&dir, "z");
        let (gone, between) = (fake('1'), fake('2'));
        lock_commit(&dir, &gone, None);
        let older = lock_commit(&dir, &between, Some((&gone, "member pin")));
        lock_commit(&dir, &gone, None);
        let newer = lock_commit(&dir, &held, Some((&gone, "member pin")));
        let of_gone = superseded_in(&dir)
            .into_iter()
            .find(|s| s.pin == gone)
            .unwrap();
        assert_eq!(of_gone.by, older);
        assert_ne!(of_gone.by, newer);
    }

    #[test]
    fn an_absent_pin_a_later_lock_re_pinned_is_skipped_and_reported_and_any_other_refuses() {
        let (_tmp, dir) = repo();
        let vcs = crate::git::git_vcs();
        let pin = commit(&dir, "a");
        git(&dir, &["checkout", "-q", "-b", "feat"]);
        let work = commit(&dir, "f");
        git(&dir, &["checkout", "-q", "main"]);
        let u = commit(&dir, "b");
        let (gone, other) = (fake('d'), fake('e'));
        let superseded = vec![Superseded {
            lock: fake('1'),
            pin: gone.clone(),
            by: fake('2'),
            verb: "member pin".to_owned(),
            known: false,
        }];
        assert_eq!(
            judge(
                vcs.as_ref(),
                &dir,
                &work,
                &pins(&[&pin, &gone]),
                |_| Ok(superseded.clone()),
                &u
            )
            .unwrap(),
            Lineage::Passes {
                base: Some(pin.clone()),
                skipped: superseded.clone()
            }
        );
        assert_eq!(
            judge(
                vcs.as_ref(),
                &dir,
                &work,
                &pins(&[&pin, &gone, &other]),
                |_| Ok(superseded.clone()),
                &u
            )
            .unwrap(),
            Lineage::Unreachable {
                absent: vec![other]
            }
        );
    }

    #[test]
    fn new_work_leaves_out_what_a_pin_of_the_target_reaches() {
        let (_tmp, dir) = repo();
        let vcs = crate::git::git_vcs();
        commit(&dir, "a");
        git(&dir, &["checkout", "-q", "-b", "feat"]);
        let pinned = commit(&dir, "p");
        let mine = commit(&dir, "f");
        git(&dir, &["checkout", "-q", "main"]);
        let u = commit(&dir, "b");
        let listed: Vec<CommitId> =
            new_work(vcs.as_ref(), &dir, &mine, Some(&u), &pins(&[&pinned]))
                .unwrap()
                .into_iter()
                .map(|c| c.id)
                .collect();
        assert_eq!(listed, vec![mine]);
    }
}
