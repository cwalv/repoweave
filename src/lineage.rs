//! The lineage rule: a member's work moves into a receiving branch only when
//! that branch contains the work's base, the newest commit of the work's
//! first-parent chain that the target's history pinned or built on.

use crate::manifest::{LockFile, RepoPath};
use crate::vcs::{CommitId, ResolvedRevisionId, Vcs, VcsError};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// A pin of the target's history that a later lock of that history
/// re-pinned by name: `lock` held `pin`, and `by`, which descends from every
/// lock holding it, records re-pinning the member by `verb`. Where this clone
/// lacks the pin, it does not decide the base. `known` where the source's
/// project branch already holds `by`.
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
/// through every parent, and those a later lock re-pinned by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PinHistory {
    pub(crate) pins: BTreeMap<RepoPath, BTreeSet<CommitId>>,
    superseded: BTreeMap<RepoPath, Vec<Superseded>>,
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

impl PinHistory {
    /// Read every lock `tip`'s history held.
    pub(crate) fn of(
        project_vcs: &dyn Vcs,
        project_dir: &Path,
        tip: &CommitId,
    ) -> Result<Self, VcsError> {
        let mut history = Self::default();
        let mut holders: BTreeMap<(RepoPath, CommitId), Vec<CommitId>> = BTreeMap::new();
        let mut repins: BTreeMap<RepoPath, Vec<(CommitId, String)>> = BTreeMap::new();
        for commit in project_vcs.commits_changing(project_dir, tip, LockFile::FILE_NAME)? {
            let Some(held) = pins_at(project_vcs, project_dir, &commit) else {
                continue;
            };
            for (path, pin) in &held {
                history
                    .pins
                    .entry(path.clone())
                    .or_default()
                    .insert(pin.clone());
                holders
                    .entry((path.clone(), pin.clone()))
                    .or_default()
                    .push(commit.clone());
            }
            let records = crate::repin::recorded(project_vcs, project_dir, &commit)?;
            if records.is_empty() {
                continue;
            }
            let parents: Vec<BTreeMap<RepoPath, CommitId>> = project_vcs
                .parents(project_dir, &commit)?
                .iter()
                .map(|parent| pins_at(project_vcs, project_dir, parent).unwrap_or_default())
                .collect();
            for record in records {
                if held.get(&record.member) == Some(&record.to)
                    && parents
                        .iter()
                        .any(|parent| parent.get(&record.member) == record.from.as_ref())
                {
                    repins
                        .entry(record.member)
                        .or_default()
                        .push((commit.clone(), record.verb));
                }
            }
        }
        for ((path, pin), locks) in holders {
            let Some(records) = repins.get(&path) else {
                continue;
            };
            let mut by = None;
            for lock in &locks {
                let mut found = None;
                for (record, verb) in records {
                    if record != lock
                        && project_vcs.is_ancestor(
                            project_dir,
                            &ResolvedRevisionId::from(lock.clone()),
                            &ResolvedRevisionId::from(record.clone()),
                        )?
                    {
                        found = Some((record, verb));
                        break;
                    }
                }
                match found {
                    Some(found) => by = by.or(Some((lock, found))),
                    None => {
                        by = None;
                        break;
                    }
                }
            }
            if let Some((lock, (record, verb))) = by {
                history
                    .superseded
                    .entry(path)
                    .or_default()
                    .push(Superseded {
                        lock: lock.clone(),
                        pin,
                        by: record.clone(),
                        verb: verb.clone(),
                        known: false,
                    });
            }
        }
        Ok(history)
    }

    /// Mark each superseded pin whose re-pin `source`, the source's project
    /// branch, already holds.
    pub(crate) fn seen_from(
        mut self,
        project_vcs: &dyn Vcs,
        project_dir: &Path,
        source: &CommitId,
    ) -> Result<Self, VcsError> {
        let source = ResolvedRevisionId::from(source.clone());
        for superseded in self.superseded.values_mut().flatten() {
            superseded.known = project_vcs.is_ancestor(
                project_dir,
                &ResolvedRevisionId::from(superseded.by.clone()),
                &source,
            )?;
        }
        Ok(self)
    }

    pub(crate) fn of_member(&self, member: &RepoPath) -> BTreeSet<CommitId> {
        self.pins.get(member).cloned().unwrap_or_default()
    }

    pub(crate) fn superseded_of(&self, member: &RepoPath) -> &[Superseded] {
        self.superseded.get(member).map_or(&[], Vec::as_slice)
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
/// less those this clone lacks that `superseded` holds.
pub(crate) fn judge(
    vcs: &dyn Vcs,
    dir: &Path,
    work: &CommitId,
    pins: &BTreeSet<CommitId>,
    superseded: &[Superseded],
    receiving: &CommitId,
) -> Result<Lineage, VcsError> {
    if pins.is_empty() {
        return Ok(Lineage::Passes {
            base: None,
            skipped: Vec::new(),
        });
    }
    let (present, absent) = split_present(vcs, dir, pins);
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
            .iter()
            .filter(|s| skipped.contains(&s.pin))
            .cloned()
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
            judge(vcs.as_ref(), &dir, &work, &pins(&[&pin]), &[], &u).unwrap(),
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
                &[],
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
                &[],
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
            judge(vcs.as_ref(), &dir, &work, &pins(&[&pin]), &[], &u).unwrap(),
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
            judge(vcs.as_ref(), &dir, &work, &pins(&[&pin, &absent]), &[], &u).unwrap(),
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

    fn superseded_pins(dir: &Path) -> BTreeSet<CommitId> {
        let vcs = crate::git::git_vcs();
        let tip = CommitId::parse(&git(dir, &["rev-parse", "HEAD"])).unwrap();
        let history = PinHistory::of(vcs.as_ref(), dir, &tip).unwrap();
        history
            .superseded_of(&RepoPath::new("m").unwrap())
            .iter()
            .map(|s| s.pin.clone())
            .collect()
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
    fn a_merge_re_pinning_by_name_supersedes_the_pins_of_the_range_it_merges() {
        let (_tmp, dir) = repo();
        let (n, p, q) = (fake('a'), fake('b'), fake('c'));
        lock_commit(&dir, &n, None);
        git(&dir, &["checkout", "-q", "-b", "side"]);
        lock_commit(&dir, &p, None);
        git(&dir, &["checkout", "-q", "main"]);
        commit(&dir, "other");
        git(&dir, &["merge", "-q", "--no-commit", "-s", "ours", "side"]);
        lock_commit(&dir, &q, Some((&n, "land")));
        assert_eq!(superseded_pins(&dir), pins(&[&n, &p]));
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
        assert_eq!(superseded_pins(&dir), pins(&[&n, &p]));
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
    fn a_pin_a_later_lock_holds_again_is_not_superseded() {
        let (_tmp, dir) = repo();
        let (p1, p2) = (fake('1'), fake('2'));
        lock_commit(&dir, &p1, None);
        lock_commit(&dir, &p2, Some((&p1, "member pin")));
        lock_commit(&dir, &p1, None);
        assert!(superseded_pins(&dir).is_empty());
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
                &superseded,
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
                &superseded,
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
