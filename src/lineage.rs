//! The lineage rule: a member's work moves into a receiving branch only when
//! that branch contains the work's base, the newest commit of the work's
//! first-parent chain that the target's history pinned or built on.

use crate::manifest::{LockFile, RepoPath};
use crate::vcs::{CommitId, ResolvedRevisionId, Vcs, VcsError};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A pin of the target's history, and the oldest lock commit of the range
/// the lineage rule asks about that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryPin {
    pub(crate) lock: CommitId,
    pub(crate) pin: CommitId,
}

impl HistoryPin {
    pub(crate) fn absent_line(&self, member: &RepoPath) -> String {
        absent_line(member, &self.lock, &self.pin)
    }
}

pub(crate) fn absent_line(member: &RepoPath, lock: &CommitId, pin: &CommitId) -> String {
    format!(
        "{member}: history pin absent: {} pinned {}, which this clone lacks and no remote of \
         the member holds (asked just now), so it does not decide the base",
        lock.short(),
        pin.short()
    )
}

/// Every pin each member had in the locks of a project branch's history,
/// through every parent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PinHistory {
    pub(crate) pins: BTreeMap<RepoPath, BTreeSet<CommitId>>,
    dir: PathBuf,
    tip: Option<CommitId>,
    locks: Vec<(CommitId, BTreeMap<RepoPath, CommitId>)>,
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
        for commit in project_vcs.commits_changing(project_dir, tip, &[], LockFile::FILE_NAME)? {
            let held = pins_at(project_vcs, project_dir, &commit).unwrap_or_default();
            for (path, pin) in &held {
                history
                    .pins
                    .entry(path.clone())
                    .or_default()
                    .insert(pin.clone());
            }
            history.locks.push((commit, held));
        }
        Ok(history)
    }

    /// The same history, landed or synced from `source`, the source's
    /// project branch: of the pins this clone lacks, the lineage rule asks
    /// only about those the lock commits `source` does not reach hold.
    pub(crate) fn since(mut self, source: &CommitId) -> Self {
        self.source = Some(source.clone());
        self
    }

    pub(crate) fn of_member(&self, member: &RepoPath) -> BTreeSet<CommitId> {
        self.pins.get(member).cloned().unwrap_or_default()
    }

    /// Which of `absent`, pins of `member` this clone lacks, a lock commit
    /// the source does not reach holds, each with the oldest such commit.
    pub(crate) fn asked_about(
        &self,
        project_vcs: &dyn Vcs,
        member: &RepoPath,
        absent: &[CommitId],
    ) -> Result<Vec<HistoryPin>, VcsError> {
        let Some(tip) = self.tip.as_ref().filter(|_| !absent.is_empty()) else {
            return Ok(Vec::new());
        };
        let in_range: BTreeSet<CommitId> = project_vcs
            .commits_changing(&self.dir, tip, self.source.as_slice(), LockFile::FILE_NAME)?
            .into_iter()
            .collect();
        let mut held_by: BTreeMap<&CommitId, &CommitId> = BTreeMap::new();
        for (lock, held) in self
            .locks
            .iter()
            .filter(|(lock, _)| in_range.contains(lock))
        {
            if let Some(pin) = held.get(member).filter(|pin| absent.contains(pin)) {
                held_by.insert(pin, lock);
            }
        }
        Ok(absent
            .iter()
            .filter_map(|pin| {
                Some(HistoryPin {
                    lock: (*held_by.get(pin)?).clone(),
                    pin: pin.clone(),
                })
            })
            .collect())
    }
}

/// What the lineage rule finds for one member's work against a receiving
/// branch, on the pins this clone holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lineage {
    /// The receiving branch contains the base, or the target never pinned
    /// the member, so it has none.
    Passes { base: Option<CommitId> },
    /// The receiving branch lacks the base.
    Off { base: CommitId },
    /// The work passes on the pins this clone holds and is new to the
    /// receiving branch, and `absent`, pins this clone lacks, could make its
    /// base newer.
    Lacks {
        base: Option<CommitId>,
        absent: Vec<CommitId>,
    },
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

/// Judge `work` against `receiving` by the pins the target's history held
/// that this clone holds.
pub(crate) fn judge(
    vcs: &dyn Vcs,
    dir: &Path,
    work: &CommitId,
    pins: &BTreeSet<CommitId>,
    receiving: &CommitId,
) -> Result<Lineage, VcsError> {
    if pins.is_empty() {
        return Ok(Lineage::Passes { base: None });
    }
    let (present, absent) = split_present(vcs, dir, pins);
    let base = base_of(vcs, dir, work, &present)?;
    let receiving_rev = ResolvedRevisionId::from(receiving.clone());
    if let Some(base) = &base {
        if !vcs.is_ancestor(dir, &ResolvedRevisionId::from(base.clone()), &receiving_rev)? {
            return Ok(Lineage::Off { base: base.clone() });
        }
    }
    if absent.is_empty()
        || vcs.is_ancestor(dir, &ResolvedRevisionId::from(work.clone()), &receiving_rev)?
    {
        return Ok(Lineage::Passes { base });
    }
    Ok(Lineage::Lacks { base, absent })
}

/// What asking a member's remotes found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Asked {
    /// Every remote of the member was fetched; none, where it has none.
    Fetched,
    /// A remote could not be fetched, for `cause`.
    Unreachable { cause: String },
}

/// The lineage rule's verdict once the pins it asks about are looked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// `absent` are the pins asked about that no remote holds, which do not
    /// decide the base.
    Passes {
        base: Option<CommitId>,
        absent: Vec<HistoryPin>,
    },
    Off {
        base: CommitId,
    },
    /// The pins asked about could not be looked for: a remote could not be
    /// reached, for `cause`.
    Unasked {
        pins: Vec<HistoryPin>,
        cause: String,
    },
}

/// The judging of `member`'s work as [`Verdict::Unasked`] leaves it, against
/// `target`.
pub(crate) fn unasked(
    target: impl std::fmt::Display,
    member: &RepoPath,
    pins: &[HistoryPin],
    cause: String,
) -> crate::diagnosis::Judging {
    crate::diagnosis::State::PinUnasked
        .of(member)
        .member()
        .bind("<m>", member)
        .subject(format!(
            "'{target}' pinned {} for it, which this clone lacks, and the member's remotes \
             could not be asked whether they hold it",
            pins.iter()
                .map(|p| p.pin.short().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .listing(
            pins.iter()
                .map(|p| format!("{} pinned by {}", p.pin.short(), p.lock.short()))
                .chain([cause]),
        )
}

/// The lineage rule for `member`'s `work` against `receiving`, by every pin
/// of `history`. A pin this clone lacks decides nothing unless it is asked
/// about ([`PinHistory::asked_about`]); then `ask` fetches the member's
/// remotes once and the rule judges again on what that brought.
#[allow(clippy::too_many_arguments)]
pub(crate) fn judge_asking(
    project_vcs: &dyn Vcs,
    history: &PinHistory,
    member: &RepoPath,
    vcs: &dyn Vcs,
    dir: &Path,
    work: &CommitId,
    receiving: &CommitId,
    ask: impl FnOnce() -> anyhow::Result<Asked>,
) -> anyhow::Result<Verdict> {
    let pins = history.of_member(member);
    let asked = match judge(vcs, dir, work, &pins, receiving)? {
        Lineage::Passes { base } => {
            return Ok(Verdict::Passes {
                base,
                absent: Vec::new(),
            })
        }
        Lineage::Off { base } => return Ok(Verdict::Off { base }),
        Lineage::Lacks { base, absent } => {
            let asked = history.asked_about(project_vcs, member, &absent)?;
            if asked.is_empty() {
                return Ok(Verdict::Passes {
                    base,
                    absent: Vec::new(),
                });
            }
            asked
        }
    };
    if let Asked::Unreachable { cause } = ask()? {
        return Ok(Verdict::Unasked { pins: asked, cause });
    }
    Ok(match judge(vcs, dir, work, &pins, receiving)? {
        Lineage::Passes { base } => Verdict::Passes {
            base,
            absent: Vec::new(),
        },
        Lineage::Off { base } => Verdict::Off { base },
        Lineage::Lacks { base, absent } => Verdict::Passes {
            base,
            absent: asked
                .into_iter()
                .filter(|asked| absent.contains(&asked.pin))
                .collect(),
        },
    })
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
            judge(vcs.as_ref(), &dir, &work, &pins(&[&pin]), &u).unwrap(),
            Lineage::Passes { base: Some(pin) }
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
            judge(vcs.as_ref(), &dir, &work, &pins(&[&root, &old_pin]), &u).unwrap(),
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
            judge(vcs.as_ref(), &dir, &work, &pins(&[&pin, &other_pin]), &u).unwrap(),
            Lineage::Passes { base: Some(pin) }
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
            judge(vcs.as_ref(), &dir, &work, &pins(&[&pin]), &u).unwrap(),
            Lineage::Passes { .. }
        ));
    }

    #[test]
    fn a_pin_this_clone_lacks_is_named_where_the_work_would_otherwise_pass() {
        let (_tmp, dir) = repo();
        let vcs = crate::git::git_vcs();
        let pin = commit(&dir, "a");
        git(&dir, &["checkout", "-q", "-b", "feat"]);
        let work = commit(&dir, "f");
        git(&dir, &["checkout", "-q", "main"]);
        let u = commit(&dir, "b");
        let absent = CommitId::parse(&"ab".repeat(20)).unwrap();
        assert_eq!(
            judge(vcs.as_ref(), &dir, &work, &pins(&[&pin, &absent]), &u).unwrap(),
            Lineage::Lacks {
                base: Some(pin),
                absent: vec![absent]
            }
        );
    }

    fn lock_commit(dir: &Path, pin: &CommitId) -> CommitId {
        std::fs::write(
            dir.join(LockFile::FILE_NAME),
            format!(
                r#"{{"repositories":{{"m":{{"type":"git","url":"u","version":"{}"}}}}}}"#,
                pin.as_str()
            ),
        )
        .unwrap();
        git(dir, &["add", LockFile::FILE_NAME]);
        git(dir, &["commit", "-qm", &format!("lock {}", pin.short())]);
        CommitId::parse(&git(dir, &["rev-parse", "HEAD"])).unwrap()
    }

    fn fake(fill: char) -> CommitId {
        CommitId::parse(&fill.to_string().repeat(40)).unwrap()
    }

    fn head(dir: &Path) -> CommitId {
        CommitId::parse(&git(dir, &["rev-parse", "HEAD"])).unwrap()
    }

    fn history_in(dir: &Path, since: Option<&CommitId>) -> (PinHistory, RepoPath) {
        let vcs = crate::git::git_vcs();
        let history = PinHistory::of(vcs.as_ref(), dir, &head(dir)).unwrap();
        let history = match since {
            Some(source) => history.since(source),
            None => history,
        };
        (history, RepoPath::new("m").unwrap())
    }

    fn asked(history: &PinHistory, member: &RepoPath, absent: &[&CommitId]) -> Vec<HistoryPin> {
        let vcs = crate::git::git_vcs();
        let absent: Vec<CommitId> = absent.iter().map(|c| (*c).clone()).collect();
        history.asked_about(vcs.as_ref(), member, &absent).unwrap()
    }

    #[test]
    fn only_lock_commits_the_source_does_not_reach_are_asked_about() {
        let (_tmp, dir) = repo();
        let (old, new) = (fake('a'), fake('b'));
        lock_commit(&dir, &fake('0'));
        let common = lock_commit(&dir, &old);
        lock_commit(&dir, &fake('1'));
        git(&dir, &["branch", "seat"]);
        let source = head(&dir);
        let since = lock_commit(&dir, &new);
        lock_commit(&dir, &fake('2'));
        let (history, m) = history_in(&dir, Some(&source));
        assert_eq!(
            asked(&history, &m, &[&old, &new]),
            vec![HistoryPin {
                lock: since.clone(),
                pin: new.clone()
            }]
        );
        let (whole, m) = history_in(&dir, None);
        assert_eq!(
            asked(&whole, &m, &[&old, &new]),
            vec![
                HistoryPin {
                    lock: common,
                    pin: old
                },
                HistoryPin {
                    lock: since,
                    pin: new
                }
            ]
        );
    }

    #[test]
    fn a_pin_only_the_sources_side_of_a_criss_cross_held_is_not_asked_about() {
        let (_tmp, dir) = repo();
        let (left, right, after) = (fake('a'), fake('b'), fake('c'));
        lock_commit(&dir, &fake('0'));
        git(&dir, &["checkout", "-q", "-b", "seat"]);
        let seat_lock = lock_commit(&dir, &right);
        git(&dir, &["checkout", "-q", "main"]);
        lock_commit(&dir, &left);
        git(&dir, &["checkout", "-q", "seat"]);
        git(
            &dir,
            &["merge", "-q", "-s", "ours", "-m", "main into seat", "main"],
        );
        git(&dir, &["checkout", "-q", "main"]);
        git(
            &dir,
            &[
                "merge",
                "-q",
                "-s",
                "ours",
                "-m",
                "seat into main",
                seat_lock.as_str(),
            ],
        );
        let merge = head(&dir);
        let since = lock_commit(&dir, &after);
        let source = CommitId::parse(&git(&dir, &["rev-parse", "seat"])).unwrap();
        assert_eq!(
            git(&dir, &["merge-base", "--all", "main", "seat"])
                .lines()
                .count(),
            2
        );
        let (history, m) = history_in(&dir, Some(&source));
        assert_eq!(
            asked(&history, &m, &[&left, &right, &after]),
            vec![
                HistoryPin {
                    lock: merge,
                    pin: left
                },
                HistoryPin {
                    lock: since,
                    pin: after
                }
            ]
        );
    }

    #[test]
    fn every_lock_commit_of_a_history_unrelated_to_the_source_is_asked_about() {
        let (_tmp, dir) = repo();
        let old = fake('a');
        let lock = lock_commit(&dir, &old);
        git(&dir, &["checkout", "-q", "--orphan", "seat"]);
        git(&dir, &["rm", "-qrf", "."]);
        commit(&dir, "unrelated");
        let source = head(&dir);
        git(&dir, &["checkout", "-q", "main"]);
        let (history, m) = history_in(&dir, Some(&source));
        assert_eq!(
            asked(&history, &m, &[&old]),
            vec![HistoryPin { lock, pin: old }]
        );
    }

    struct Pair {
        _tmp: tempfile::TempDir,
        project: PathBuf,
        member: PathBuf,
        remote: PathBuf,
    }

    /// A member whose `main` pinned `b`, then `d` (on `c`), then `e`, which
    /// holds `b` and not `c`; `d` lives only in `remote`, on its `x`, and
    /// this clone holds `c` on branch `y`.
    fn dropped_lineage(common: bool) -> (Pair, CommitId, CommitId, CommitId, CommitId) {
        let tmp = crate::tempdir().unwrap();
        let [project, member, remote] = ["p", "m", "r"].map(|n| tmp.path().join(n));
        for dir in [&project, &remote] {
            std::fs::create_dir(dir).unwrap();
            git(dir, &["init", "-q", "-b", "main"]);
        }
        let b = commit(&remote, "b");
        git(&remote, &["checkout", "-q", "-b", "x"]);
        let c = commit(&remote, "c");
        git(tmp.path(), &["clone", "-q", "-b", "main", "r", "m"]);
        git(&member, &["branch", "y", "origin/x"]);
        let d = commit(&remote, "d");
        git(&remote, &["checkout", "-q", "main"]);
        let e = commit(&remote, "e");
        git(&member, &["fetch", "-q", "origin", "main"]);
        lock_commit(&project, &b);
        if !common {
            git(&project, &["branch", "seat"]);
        }
        lock_commit(&project, &d);
        lock_commit(&project, &e);
        if common {
            git(&project, &["branch", "seat"]);
        }
        git(&member, &["checkout", "-q", "-b", "work", "y"]);
        let work = commit(&member, "w");
        (
            Pair {
                _tmp: tmp,
                project,
                member,
                remote,
            },
            c,
            d,
            e,
            work,
        )
    }

    fn judged(
        pair: &Pair,
        work: &CommitId,
        e: &CommitId,
        ask: impl FnOnce() -> anyhow::Result<Asked>,
    ) -> Verdict {
        let vcs = crate::git::git_vcs();
        let source = CommitId::parse(&git(&pair.project, &["rev-parse", "seat"])).unwrap();
        let (history, m) = history_in(&pair.project, Some(&source));
        judge_asking(
            vcs.as_ref(),
            &history,
            &m,
            vcs.as_ref(),
            &pair.member,
            work,
            e,
            ask,
        )
        .unwrap()
    }

    #[test]
    fn a_pin_asked_about_that_a_fetch_brings_is_judged_and_can_refuse() {
        let (pair, c, d, e, work) = dropped_lineage(false);
        let member = pair.member.clone();
        let judged = judged(&pair, &work, &e, || {
            git(&member, &["fetch", "-q", "origin"]);
            Ok(Asked::Fetched)
        });
        assert_eq!(judged, Verdict::Off { base: c });
        assert!(git(&pair.member, &["cat-file", "-t", d.as_str()]) == "commit");
    }

    #[test]
    fn a_pin_asked_about_that_no_remote_holds_is_named_and_decides_nothing() {
        let (pair, _, d, e, work) = dropped_lineage(false);
        git(&pair.remote, &["branch", "-q", "-D", "x"]);
        let member = pair.member.clone();
        let judged = judged(&pair, &work, &e, || {
            git(&member, &["fetch", "-q", "--prune", "origin"]);
            Ok(Asked::Fetched)
        });
        let Verdict::Passes { absent, .. } = judged else {
            panic!("{judged:?}");
        };
        assert_eq!(absent.iter().map(|a| &a.pin).collect::<Vec<_>>(), vec![&d]);
    }

    #[test]
    fn a_pin_asked_about_whose_remote_cannot_be_reached_is_unasked() {
        let (pair, _, d, e, work) = dropped_lineage(false);
        let judged = judged(&pair, &work, &e, || {
            Ok(Asked::Unreachable {
                cause: "gone".to_owned(),
            })
        });
        let Verdict::Unasked { pins, cause } = judged else {
            panic!("{judged:?}");
        };
        assert_eq!((pins[0].pin.clone(), cause.as_str()), (d, "gone"));
    }

    #[test]
    fn a_lacked_pin_the_source_already_reached_is_not_asked_about_even_where_a_remote_holds_it_and_would_refuse(
    ) {
        let (pair, _, _, e, work) = dropped_lineage(true);
        let judged = judged(&pair, &work, &e, || panic!("no remote is asked"));
        assert!(
            matches!(&judged, Verdict::Passes { absent, .. } if absent.is_empty()),
            "{judged:?}"
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
