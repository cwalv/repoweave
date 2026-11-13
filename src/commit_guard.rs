//! No commit of the project repo while an operation is in flight there: a
//! commit made then becomes the operation's commit, concluding it with only
//! what that commit carries.

use crate::diagnosis::{ChoiceId, State};
use crate::vcs::{CommitId, ConflictOp, MidOperation, Vcs};
use std::fmt;
use std::path::Path;

/// The one in-flight operation a commit may conclude: rwv's own copy of
/// `.0`, all of whose paths are merged.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConcludingCopy<'a>(pub(crate) &'a CommitId);

/// Refuse while the project repo at `dir` is in the middle of a merge,
/// rebase, cherry-pick or revert, or holds an unmerged path. `workspace` is
/// where an rwv operation that stopped there is recorded, for the refusal to
/// name its continue and abort.
pub(crate) fn refuse_commit_mid_operation(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    workspace: Option<&Path>,
) -> anyhow::Result<()> {
    judge(vcs, repo, dir, workspace, None)
}

/// [`refuse_commit_mid_operation`] for the commit that concludes `copy`.
pub(crate) fn refuse_commit_mid_operation_but(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    copy: ConcludingCopy<'_>,
) -> anyhow::Result<()> {
    judge(vcs, repo, dir, None, Some(copy))
}

fn judge(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    workspace: Option<&Path>,
    concluding: Option<ConcludingCopy<'_>>,
) -> anyhow::Result<()> {
    if !vcs.is_repo(dir) {
        return Ok(());
    }
    let op = vcs.mid_operation(dir).and_then(MidOperation::conflict);
    let unmerged = vcs.unmerged_paths(dir)?;
    if op.is_none() && unmerged.is_empty() {
        return Ok(());
    }
    if let (Some(ConflictOp::CherryPick), Some(ConcludingCopy(copied)), true) =
        (op, concluding, unmerged.is_empty())
    {
        if vcs.copy_in_flight(dir)?.as_ref() == Some(copied) {
            return Ok(());
        }
    }
    let resume = workspace
        .and_then(|workspace| crate::op_state::read_owner_header(workspace).ok().flatten())
        .map(|header| crate::op_state::resume_command(header.verb));
    let subject = match op {
        Some(op) => format!("a {op} in progress"),
        None => "unmerged paths".to_owned(),
    };
    let mut without = Vec::new();
    if unmerged.is_empty() {
        without.push(ChoiceId::MarkResolved);
    }
    let mut judging = State::EditsCommitMidOperation
        .of(repo)
        .project_repo()
        .subject(subject)
        .listing(&unmerged)
        .bind_list("<paths>", &unmerged)
        .checkout(dir);
    match resume {
        Some(resume) => {
            without.push(ChoiceId::C32);
            judging = judging.bind_list("<resume>", resume.split_whitespace().skip(1));
        }
        None => without.extend([ChoiceId::ResumeOperation, ChoiceId::AbortOperation]),
    }
    Err(judging.without(&without).judge(vcs).refused())
}
