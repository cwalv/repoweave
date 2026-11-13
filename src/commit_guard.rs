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
    let recorded = workspace.and_then(|workspace| {
        crate::op_state::read_owner_header(workspace)
            .ok()
            .flatten()
            .map(|header| (header.verb, workspace))
    });
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
    match recorded {
        Some((verb, workspace)) => {
            without.push(ChoiceId::C32);
            if !unmerged
                .iter()
                .any(|path| path == crate::manifest::Manifest::FILE_NAME)
            {
                without.push(ChoiceId::KeepSide);
            }
            let resume = crate::op_state::resume_command_at(verb, workspace);
            let abort = crate::op_state::abort_command_at(workspace);
            judging = judging
                .bind_list("<resume>", resume.split_whitespace())
                .bind_list("<abort>", abort.split_whitespace());
        }
        None => without.extend([
            ChoiceId::KeepSide,
            ChoiceId::ResumeOperation,
            ChoiceId::AbortOperation,
        ]),
    }
    Err(judging.without(&without).judge(vcs).refused())
}

#[cfg(test)]
mod tests {
    use crate::diagnosis::State;
    use crate::git::git_vcs;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git")
            .status
            .success()
    }

    fn write(dir: &Path, file: &str, text: &str) {
        std::fs::write(dir.join(file), text).unwrap();
    }

    fn stopped_merge(repo: &Path) {
        assert!(git(repo, &["init", "-q", "-b", "main"]));
        for (key, value) in [("user.email", "t@t"), ("user.name", "T")] {
            assert!(git(repo, &["config", key, value]));
        }
        write(repo, "rwv.toml", "[repositories]\n");
        assert!(git(repo, &["add", "."]));
        assert!(git(repo, &["commit", "-qm", "base"]));
        assert!(git(repo, &["checkout", "-qb", "side"]));
        write(repo, "NOTES.txt", "side\n");
        assert!(git(repo, &["add", "."]));
        assert!(git(repo, &["commit", "-qm", "side"]));
        assert!(git(repo, &["checkout", "-q", "main"]));
        write(repo, "NOTES.txt", "main\n");
        assert!(git(repo, &["add", "."]));
        assert!(git(repo, &["commit", "-qm", "main"]));
        assert!(!git(repo, &["merge", "-q", "side"]));
    }

    fn refused_mid_operation(err: &anyhow::Error) -> bool {
        crate::diagnosis::report_of(err).is_some_and(|report| {
            report
                .lines()
                .iter()
                .any(|line| line.state() == State::EditsCommitMidOperation)
        })
    }

    fn commit_a_setting(repo: &Path) -> anyhow::Result<bool> {
        write(repo, "rwv.toml", "[repositories]\n# set\n");
        assert!(git(repo, &["add", "rwv.toml"]));
        crate::repin::commit(
            git_vcs().as_ref(),
            repo,
            &["rwv.toml"],
            "a setting",
            None,
            &crate::repin::Naming::Unnamed,
        )
    }

    #[test]
    fn the_project_commit_is_refused_while_a_path_is_unmerged() {
        let tmp = crate::tempdir().unwrap();
        stopped_merge(tmp.path());

        let err = commit_a_setting(tmp.path()).unwrap_err();

        assert!(refused_mid_operation(&err), "{err:#}");
        assert!(git(
            tmp.path(),
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"]
        ));
    }

    #[test]
    fn the_project_commit_is_refused_once_every_path_is_staged_mid_merge() {
        let tmp = crate::tempdir().unwrap();
        stopped_merge(tmp.path());
        write(tmp.path(), "NOTES.txt", "both\n");
        assert!(git(tmp.path(), &["add", "NOTES.txt"]));

        let err = commit_a_setting(tmp.path()).unwrap_err();

        assert!(refused_mid_operation(&err), "{err:#}");
        assert!(git(
            tmp.path(),
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"]
        ));
    }
}
