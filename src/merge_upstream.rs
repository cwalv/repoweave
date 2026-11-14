//! `--merge-upstream` and `--reset-to-upstream`: the take-in operations of
//! `pull` and `update --remote` that change a branch that has diverged from
//! its upstream.
//!
//! Both run under the op record: its savepoints, the tips it wrote, and the
//! flags it was given, which `--continue` re-applies.

use crate::diagnosis::{Report, State};
use crate::manifest::{ProjectName, RepoKey, RepoOperand, ResetOperand};
use crate::op_state::{
    self, AcquiredOp, OpId, OpVerb, OwnerRecord, RefMove, SavepointKey, TakeInRecord,
    TouchedWorkspaces,
};
use crate::refusal::RefusalKind;
use crate::vcs::{
    AttachedRef, BackupSource, CommitId, RawRefName, ResolvedRevisionId, Vcs, VcsError,
};
use crate::workspace::WorkspaceContext;
use anyhow::Context;
use std::path::{Path, PathBuf};

/// The flags `pull` and `update --remote` take for a diverged branch.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub merge_upstream: bool,
    pub reset_to_upstream: Vec<ResetOperand>,
}

impl Flags {
    pub fn any(&self) -> bool {
        self.merge_upstream || !self.reset_to_upstream.is_empty()
    }

    pub(crate) fn resets(&self, operand: &RepoOperand) -> bool {
        self.reset_to_upstream
            .iter()
            .any(|named| named.repo == *operand && named.branch.is_none())
    }

    /// Whether `--reset-to-upstream <m>:<branch>` names `branch` of `operand`.
    pub(crate) fn resets_branch(&self, operand: &RepoOperand, branch: &str) -> bool {
        self.reset_to_upstream
            .iter()
            .any(|named| named.repo == *operand && named.branch.as_deref() == Some(branch))
    }

    pub(crate) fn recorded(&self, take_in: &TakeInRecord) -> Self {
        Self {
            merge_upstream: take_in.merge_upstream,
            reset_to_upstream: take_in
                .reset_to_upstream
                .iter()
                .filter_map(|text| text.parse().ok())
                .collect(),
        }
    }

    pub(crate) fn to_record(&self) -> TakeInRecord {
        TakeInRecord {
            merge_upstream: self.merge_upstream,
            reset_to_upstream: self
                .reset_to_upstream
                .iter()
                .map(ToString::to_string)
                .collect(),
            ..TakeInRecord::default()
        }
    }
}

/// The op a take-in with a flag runs under.
pub(crate) struct Op {
    pub(crate) workspace: PathBuf,
    pub(crate) id: OpId,
}

impl Op {
    /// Acquire the op for `workspace` and savepoint every repo it may move.
    pub(crate) fn begin(
        workspace: &Path,
        project: &ProjectName,
        verb: OpVerb,
        history: Option<crate::manifest::History>,
        take_in: TakeInRecord,
        repos: &[(PathBuf, Box<dyn Vcs>)],
    ) -> anyhow::Result<Self> {
        let id = OpId::new_now();
        let mut record = OwnerRecord::new_take_in(&id, verb, project.clone(), workspace, take_in);
        record.history = history;
        let touched = TouchedWorkspaces::of(verb, workspace, Some(workspace));
        let held: AcquiredOp = op_state::acquire_op(&touched, &record)?;
        let key = SavepointKey::Op(id.clone());
        for (filed, (dir, vcs)) in repos.iter().enumerate() {
            if let Err(e) = vcs.create_savepoint(dir, &key) {
                for (dir, vcs) in &repos[..filed] {
                    vcs.drop_savepoint(dir, &key);
                }
                op_state::release_acquired(&held);
                crate::refuse!(
                    RefusalKind::SavepointUnfiled,
                    "could not file a savepoint in {}, so the operation did not start: {e:#}",
                    dir.display()
                );
            }
            crate::test_pause::point("op-savepoint-taken");
        }
        Ok(Self {
            workspace: workspace.to_path_buf(),
            id,
        })
    }

    /// The op a `--continue` resumes: the record kept where it runs.
    pub(crate) fn resume(
        ctx: &WorkspaceContext,
        verb: OpVerb,
    ) -> anyhow::Result<(Self, ProjectName, TakeInRecord)> {
        let resolved = op_state::resume(ctx.active_path())?;
        let record = resolved.record;
        if record.verb != verb {
            crate::refuse!(
                RefusalKind::ResumeContradictsRecord,
                "in-progress op is `{recorded}` but `{invoked}` was invoked. Run `{resume}` \
                 instead, or `rwv abort` to discard.",
                recorded = record.verb,
                invoked = op_state::resume_command(verb),
                resume = op_state::resume_command(record.verb),
            );
        }
        let take_in = record
            .take_in
            .context("the op record holds no take-in to resume")?;
        Ok((
            Self {
                workspace: resolved.owner_workspace.as_path().to_path_buf(),
                id: record.id,
            },
            record.project,
            take_in,
        ))
    }

    /// The op a resumed run goes on under.
    pub(crate) fn adopt(op: &Op) -> Op {
        Op {
            workspace: op.workspace.clone(),
            id: op.id.clone(),
        }
    }

    /// The op is done: clear its record and drop its savepoints.
    pub(crate) fn finish(&self, repos: &[(PathBuf, Box<dyn Vcs>)]) {
        for (dir, vcs) in repos {
            vcs.drop_savepoint(dir, &SavepointKey::Op(self.id.clone()));
            let _ = vcs.forget_replays(dir);
        }
        op_state::clear_all_at(&self.workspace);
    }

    pub(crate) fn record_upstream(&self, key: RepoKey, tip: &CommitId) -> anyhow::Result<()> {
        let mut record = op_state::read_owner(&self.workspace)?
            .context("the op record vanished while it was running")?;
        if let Some(take_in) = record.take_in.as_mut() {
            take_in.upstream_tips.insert(key, tip.clone());
        }
        op_state::write_owner(&self.workspace, &record)
    }
}

/// The branches `branch` has as dependents, as the records read: a branch
/// moved to its upstream while another record lands into it would leave that
/// record on commits it never named.
pub(crate) fn dependents_of(
    project_vcs: &dyn Vcs,
    project_dir: &Path,
    branch: &str,
) -> anyhow::Result<Vec<String>> {
    let Ok(name) = crate::records::BranchName::parse(project_vcs, branch) else {
        return Ok(Vec::new());
    };
    let store = crate::records::Store::open(project_vcs, project_dir)?;
    let view = store.view()?;
    Ok(view
        .dependents(&name)
        .into_iter()
        .map(|dependent| dependent.to_string())
        .collect())
}

/// What a reset to the upstream needs judged before anything moves: the
/// upstream still exists, nothing records a dependent on the branch, and the
/// checkout holding it has no edits.
pub(crate) fn judge_reset(
    repo: &str,
    upstream_tip: Option<&ResolvedRevisionId>,
    edits: Option<crate::diagnosis::Judged>,
    report: &mut Report,
) {
    if upstream_tip.is_none() {
        report.note(format!(
            "{repo}: its upstream is gone from the remote as last fetched, so there is nothing \
             to reset to"
        ));
    }
    report.extend(edits);
}

/// One `dependents` state per branch recorded as landing into `branch`, its
/// choices addressed to the workspace holding that branch.
pub(crate) fn judge_dependents(
    vcs: &dyn Vcs,
    project_dir: &Path,
    branch: &str,
    report: &mut Report,
) -> anyhow::Result<()> {
    let dependents = dependents_of(vcs, project_dir, branch)?;
    if dependents.is_empty() {
        return Ok(());
    }
    let label = format!(
        "{}/{}",
        crate::workspace::PROJECTS_DIR,
        project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
    );
    let store = crate::records::Store::open(vcs, project_dir)?;
    let target = crate::records::BranchName::parse(vcs, branch)
        .ok()
        .and_then(|name| match store.view().ok()?.lookup(&name) {
            crate::records::Lookup::Value(record) => record.target().map(|t| t.to_string()),
            _ => None,
        });
    for dependent in dependents {
        report.add(
            State::TargetDependents
                .of(&label)
                .project_repo()
                .subject(&dependent)
                .listing([format!("{dependent} lands into this branch")])
                .dependent(vcs, project_dir, &dependent, target.as_deref())
                .judge(vcs),
        );
    }
    Ok(())
}

/// Move `branch`, which the checkout `on` holds or nothing holds, to
/// `upstream`, after a backup of the tip it leaves. Returns the backup's name.
pub(crate) fn reset_to_upstream(
    workspace: &Path,
    key: RepoKey,
    vcs: &dyn Vcs,
    dir: &Path,
    branch: &RawRefName,
    on: Option<&AttachedRef>,
    upstream: &ResolvedRevisionId,
) -> anyhow::Result<String> {
    let tip = vcs
        .resolve_local_branch_tip(dir, branch)?
        .with_context(|| format!("'{branch}' does not exist here"))?;
    let kept = vcs.create_backup(dir, &BackupSource::Branch(branch.clone()), &tip, None)?;
    match on {
        Some(on) => {
            vcs.reset_attached_ref_keeping(on, upstream, &kept)?;
        }
        None => {
            op_state::journal_ref_move(
                workspace,
                RefMove {
                    repo: key,
                    branch: branch.as_str().to_owned(),
                    from: tip.id().clone(),
                    to: upstream.id().clone(),
                },
            )?;
            vcs.reset_branch_ref(dir, branch, tip.id(), upstream.id(), &kept)?;
        }
    }
    crate::test_pause::point("reset-to-upstream");
    Ok(kept.backup().name.to_string())
}

/// A branch an op moved as a ref that abort did not simply put back.
#[derive(Debug)]
pub(crate) enum RefMoveLeft {
    /// Left where it is: something other than the op moved it, or a checkout
    /// holds it, or it is gone.
    Foreign {
        repo: RepoKey,
        branch: String,
        dir: PathBuf,
        now: Option<CommitId>,
        held_at: Vec<PathBuf>,
        error: Option<VcsError>,
    },
    /// Put back over a tip holding a commit nothing else reaches, kept first
    /// in the branch's backup slot.
    Kept {
        repo: RepoKey,
        branch: String,
        dir: PathBuf,
        to: CommitId,
        backup: String,
    },
}

/// Put back the branches an op moved as refs, each from whatever tip it sits
/// at now, that tip kept first in the branch's backup slot where it holds a
/// commit nothing else reaches; a branch moved more than once is kept from
/// its last tip only. A branch a checkout holds is left, as one that is gone
/// is.
pub(crate) fn restore_ref_moves(
    workspace: &Path,
    project_dir: Option<&Path>,
    moves: &[RefMove],
) -> Vec<RefMoveLeft> {
    walk_ref_moves(workspace, project_dir, moves, true)
}

/// What [`restore_ref_moves`] would leave, each as it would report it,
/// writing nothing.
pub(crate) fn judge_ref_moves(
    workspace: &Path,
    project_dir: Option<&Path>,
    moves: &[RefMove],
) -> Vec<RefMoveLeft> {
    walk_ref_moves(workspace, project_dir, moves, false)
}

fn walk_ref_moves(
    workspace: &Path,
    project_dir: Option<&Path>,
    moves: &[RefMove],
    write: bool,
) -> Vec<RefMoveLeft> {
    let vcs = crate::vcs::probe_vcs();
    let mut left = Vec::new();
    let mut put_back: std::collections::BTreeMap<(PathBuf, String), CommitId> =
        std::collections::BTreeMap::new();
    let mut slotted: std::collections::BTreeSet<(PathBuf, String)> =
        std::collections::BTreeSet::new();
    for (at, moved) in moves.iter().enumerate().rev() {
        let dir = match (&moved.repo, project_dir) {
            (RepoKey::Member(path), _) => workspace.join(path.as_path()),
            (RepoKey::Project, Some(dir)) => dir.to_path_buf(),
            (RepoKey::Project, None) => continue,
        };
        let branch = RawRefName::new(moved.branch.clone());
        let key = (dir.clone(), moved.branch.clone());
        let now = match put_back.get(&key) {
            Some(tip) => Some(tip.clone()),
            None => vcs
                .resolve_local_branch_tip(&dir, &branch)
                .ok()
                .flatten()
                .map(|now| now.id().clone()),
        };
        let back_before = moves[..=at]
            .iter()
            .filter(|m| m.repo == moved.repo && m.branch == moved.branch)
            .any(|m| now.as_ref() == Some(&m.from));
        if back_before {
            continue;
        }
        let held_at = vcs.checkouts_of_branch(&dir, &branch).unwrap_or_default();
        if now.is_some() && !held_at.is_empty() {
            left.push(RefMoveLeft::Foreign {
                repo: moved.repo.clone(),
                branch: moved.branch.clone(),
                dir,
                now,
                held_at,
                error: None,
            });
            continue;
        }
        if !write {
            if now.is_some() {
                put_back.insert(key, moved.from.clone());
            } else {
                left.push(RefMoveLeft::Foreign {
                    repo: moved.repo.clone(),
                    branch: moved.branch.clone(),
                    dir,
                    now,
                    held_at,
                    error: None,
                });
            }
            continue;
        }
        let restored = match &now {
            Some(now) => (|| {
                let kept = if slotted.insert(key.clone()) {
                    vcs.keep_in_slot(
                        &dir,
                        &BackupSource::Branch(branch.clone()),
                        now,
                        &moved.from,
                    )?
                } else {
                    None
                };
                vcs.restore_branch_ref(&dir, &branch, now, &moved.from)?;
                if let Some(kept) = kept {
                    left.push(RefMoveLeft::Kept {
                        repo: moved.repo.clone(),
                        branch: moved.branch.clone(),
                        dir: dir.clone(),
                        to: moved.from.clone(),
                        backup: kept.backup().name.to_string(),
                    });
                }
                Ok(())
            })(),
            None => vcs.restore_branch_ref(&dir, &branch, &moved.to, &moved.from),
        };
        if let Err(error) = restored {
            left.push(RefMoveLeft::Foreign {
                repo: moved.repo.clone(),
                branch: moved.branch.clone(),
                dir,
                now,
                held_at,
                error: Some(error),
            });
        }
    }
    left
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
        std::fs::write(dir.join("f"), text).unwrap();
        git(dir, &["add", "f"]);
        git(dir, &["commit", "-qm", text]);
        CommitId::parse(&git(dir, &["rev-parse", "HEAD"])).unwrap()
    }

    fn member_with_a_moved_branch() -> (tempfile::TempDir, RefMove, PathBuf) {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path().join("m");
        std::fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        let from = commit(&dir, "one");
        let to = commit(&dir, "two");
        git(&dir, &["checkout", "-q", "--detach"]);
        let moved = RefMove {
            repo: RepoKey::Member(crate::manifest::RepoPath::new("m").unwrap()),
            branch: "main".to_owned(),
            from,
            to,
        };
        (tmp, moved, dir)
    }

    fn slot_tip(dir: &Path) -> Option<CommitId> {
        crate::vcs::probe_vcs()
            .list_backups(dir, crate::vcs::RemoteTracking::Counted)
            .unwrap()
            .into_iter()
            .find(|listed| listed.backup.name.as_str() == "main/aborted")
            .map(|listed| listed.backup.tip.id().clone())
    }

    #[test]
    fn a_branch_the_op_moved_as_a_ref_is_put_back_its_tip_kept_in_the_branch_slot() {
        let (tmp, moved, dir) = member_with_a_moved_branch();
        let left = restore_ref_moves(tmp.path(), None, std::slice::from_ref(&moved));
        assert!(matches!(&left[..], [RefMoveLeft::Kept { .. }]), "{left:?}");
        assert_eq!(slot_tip(&dir).as_ref(), Some(&moved.to));
        assert_eq!(git(&dir, &["rev-parse", "main"]), moved.from.as_str());
    }

    #[test]
    fn a_branch_whose_tip_another_branch_reaches_is_put_back_keeping_nothing() {
        let (tmp, moved, dir) = member_with_a_moved_branch();
        git(&dir, &["branch", "other", moved.to.as_str()]);
        let left = restore_ref_moves(tmp.path(), None, std::slice::from_ref(&moved));
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(slot_tip(&dir), None);
        assert_eq!(git(&dir, &["rev-parse", "main"]), moved.from.as_str());
    }

    #[test]
    fn a_branch_that_moved_since_the_op_wrote_it_is_put_back_its_tip_kept() {
        let (tmp, moved, dir) = member_with_a_moved_branch();
        git(&dir, &["checkout", "-q", "main"]);
        let later = commit(&dir, "three");
        git(&dir, &["checkout", "-q", "--detach"]);
        let left = restore_ref_moves(tmp.path(), None, std::slice::from_ref(&moved));
        assert!(matches!(&left[..], [RefMoveLeft::Kept { .. }]), "{left:?}");
        assert_eq!(slot_tip(&dir), Some(later));
        assert_eq!(git(&dir, &["rev-parse", "main"]), moved.from.as_str());
    }

    #[test]
    fn a_branch_the_op_moved_twice_is_judged_as_its_restore_walks_it() {
        let (tmp, first, dir) = member_with_a_moved_branch();
        git(&dir, &["checkout", "-q", "main"]);
        let later = commit(&dir, "three");
        git(&dir, &["checkout", "-q", "--detach"]);
        let second = RefMove {
            from: first.to.clone(),
            to: later,
            ..first.clone()
        };
        let moves = [first.clone(), second];
        assert!(judge_ref_moves(tmp.path(), None, &moves).is_empty());
        let left = restore_ref_moves(tmp.path(), None, &moves);
        assert!(matches!(&left[..], [RefMoveLeft::Kept { .. }]), "{left:?}");
        assert_eq!(slot_tip(&dir).as_ref(), Some(&moves[1].to));
        assert_eq!(git(&dir, &["rev-parse", "main"]), first.from.as_str());
        assert!(judge_ref_moves(tmp.path(), None, &moves).is_empty());
    }
}
