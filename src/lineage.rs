//! The lineage rule: a member's work moves into a receiving branch only when
//! that branch contains the work's base, the newest commit of the work's
//! first-parent chain that the target's history pinned or built on.

use crate::manifest::{LockFile, RepoPath};
use crate::vcs::{CommitId, ResolvedRevisionId, Vcs, VcsError};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Every pin each member had in the locks of a project branch's history,
/// through every parent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PinHistory {
    pub(crate) pins: BTreeMap<RepoPath, BTreeSet<CommitId>>,
}

impl PinHistory {
    /// Read every lock `tip`'s history held.
    pub(crate) fn of(
        project_vcs: &dyn Vcs,
        project_dir: &Path,
        tip: &CommitId,
    ) -> Result<Self, VcsError> {
        let mut history = Self::default();
        for commit in project_vcs.commits_changing(project_dir, tip, LockFile::FILE_NAME)? {
            let Ok(text) = project_vcs.read_file_at_revision(
                project_dir,
                &ResolvedRevisionId::from(commit),
                Path::new(LockFile::FILE_NAME),
            ) else {
                continue;
            };
            let Ok(lock) = LockFile::from_json_str(&text) else {
                continue;
            };
            for (path, entry) in lock.iter_entries() {
                if let Some(pin) = entry.version.as_commit_id() {
                    history.pins.entry(path.clone()).or_default().insert(pin);
                }
            }
        }
        Ok(history)
    }

    pub(crate) fn of_member(&self, member: &RepoPath) -> BTreeSet<CommitId> {
        self.pins.get(member).cloned().unwrap_or_default()
    }
}

/// What the lineage rule finds for one member's work against a receiving
/// branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lineage {
    /// The receiving branch contains the base, or the target never pinned
    /// the member, so it has none.
    Passes { base: Option<CommitId> },
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

/// Judge `work` against `receiving` by the pins the target's history held.
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
    if !absent.is_empty()
        && !vcs.is_ancestor(dir, &ResolvedRevisionId::from(work.clone()), &receiving_rev)?
    {
        return Ok(Lineage::Unreachable { absent });
    }
    Ok(Lineage::Passes { base })
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
            judge(vcs.as_ref(), &dir, &work, &pins(&[&pin, &absent]), &u).unwrap(),
            Lineage::Unreachable {
                absent: vec![absent]
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
