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
        take_in: TakeInRecord,
        repos: &[(PathBuf, Box<dyn Vcs>)],
    ) -> anyhow::Result<Self> {
        let id = OpId::new_now();
        let record = OwnerRecord::new_take_in(&id, verb, project.clone(), workspace, take_in);
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
    dir: &Path,
    vcs: &dyn Vcs,
    upstream_tip: Option<&ResolvedRevisionId>,
    dependents: &[String],
    edits: Option<crate::diagnosis::Judged>,
    report: &mut Report,
) {
    if upstream_tip.is_none() {
        report.note(format!(
            "{repo}: its upstream is gone from the remote as last fetched, so there is nothing \
             to reset to"
        ));
    }
    if !dependents.is_empty() {
        report.add(
            State::TargetDependents
                .of(repo)
                .checkout(dir)
                .listing(
                    dependents
                        .iter()
                        .map(|d| format!("{d} lands into this branch")),
                )
                .judge(vcs),
        );
    }
    report.extend(edits);
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
            op_state::journal_tip(workspace, key, upstream.id())?;
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
    /// Put back over a tip the operator abandoned, kept first as a backup.
    Abandoned {
        repo: RepoKey,
        branch: String,
        tip: CommitId,
        backup: String,
    },
}

/// Put back the branches an op moved as refs, each where it still sits at the
/// tip the op wrote, or, for a repo `abandon` names, over whatever tip it sits
/// at now once that tip is kept as a backup. A branch a checkout holds is
/// left, as one that is gone is.
pub(crate) fn restore_ref_moves(
    workspace: &Path,
    project_dir: Option<&Path>,
    moves: &[RefMove],
    abandon: &dyn Fn(&RepoKey) -> bool,
) -> Vec<RefMoveLeft> {
    let vcs = crate::vcs::probe_vcs();
    let mut left = Vec::new();
    for moved in moves.iter().rev() {
        let dir = match (&moved.repo, project_dir) {
            (RepoKey::Member(path), _) => workspace.join(path.as_path()),
            (RepoKey::Project, Some(dir)) => dir.to_path_buf(),
            (RepoKey::Project, None) => continue,
        };
        let branch = RawRefName::new(moved.branch.clone());
        let now = vcs
            .resolve_local_branch_tip(&dir, &branch)
            .ok()
            .flatten()
            .map(|now| now.id().clone());
        if now.as_ref() == Some(&moved.from) {
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
        let restored = match &now {
            Some(now) if *now != moved.to && abandon(&moved.repo) => vcs
                .create_backup(
                    &dir,
                    &BackupSource::Branch(branch.clone()),
                    &ResolvedRevisionId::from(now.clone()),
                    Some(crate::vcs::BackupMark::Abandoned),
                )
                .and_then(|kept| {
                    vcs.restore_branch_ref(&dir, &branch, now, &moved.from)?;
                    left.push(RefMoveLeft::Abandoned {
                        repo: moved.repo.clone(),
                        branch: moved.branch.clone(),
                        tip: now.clone(),
                        backup: kept.backup().name.to_string(),
                    });
                    Ok(())
                }),
            _ => vcs.restore_branch_ref(&dir, &branch, &moved.to, &moved.from),
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

    #[test]
    fn a_branch_the_op_moved_as_a_ref_is_put_back_where_it_still_sits_at_the_tip_the_op_wrote() {
        let (tmp, moved, dir) = member_with_a_moved_branch();
        let left = restore_ref_moves(tmp.path(), None, std::slice::from_ref(&moved), &|_| false);
        assert!(left.is_empty());
        assert_eq!(git(&dir, &["rev-parse", "main"]), moved.from.as_str());
    }

    #[test]
    fn a_branch_that_moved_since_the_op_wrote_it_is_left_where_it_is() {
        let (tmp, moved, dir) = member_with_a_moved_branch();
        git(&dir, &["checkout", "-q", "main"]);
        let later = commit(&dir, "three");
        git(&dir, &["checkout", "-q", "--detach"]);
        let left = restore_ref_moves(tmp.path(), None, std::slice::from_ref(&moved), &|_| false);
        assert!(matches!(left[..], [RefMoveLeft::Foreign { .. }]));
        assert_eq!(git(&dir, &["rev-parse", "main"]), later.as_str());
    }
}
