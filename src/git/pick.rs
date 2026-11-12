//! Copies of a commit onto a checkout, as `git cherry-pick` makes them.

use super::records::{commit_id, failed};
use super::{git_command, lock_driver_held_args, GitVcs};
use crate::vcs::{CommitId, ConflictOp, Copied, MidOperation, VcsError};
use std::path::Path;

impl GitVcs {
    pub(super) fn copy_commit_onto(
        checkout: &Path,
        commit: &CommitId,
        mainline: Option<usize>,
    ) -> Result<Copied, VcsError> {
        let mainline = mainline.map(|n| n.to_string());
        let mut args = vec!["cherry-pick", "-x", "--allow-empty-message"];
        if let Some(n) = &mainline {
            args.extend(["-m", n.as_str()]);
        }
        args.push(commit.as_str());
        Self::settle_copy(checkout, &args, &[])
    }

    pub(super) fn continue_copy_in(checkout: &Path) -> Result<Copied, VcsError> {
        let unmerged = Self::unmerged(checkout)?;
        if !unmerged.is_empty() {
            return Ok(Copied::Conflicted(unmerged));
        }
        if Self::index_matches_head(checkout)? {
            Self::run(["cherry-pick", "--skip"], checkout)?;
            return Ok(Copied::Empty);
        }
        Self::settle_copy(
            checkout,
            &["cherry-pick", "--continue"],
            &[("GIT_EDITOR", "true")],
        )
    }

    /// Run a copy and read what it left: a commit, a conflict stopped in the
    /// checkout, or a result holding no change, which is skipped.
    fn settle_copy(
        checkout: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<Copied, VcsError> {
        let mut command = git_command();
        command.args(args).current_dir(checkout);
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command.output().map_err(|e| VcsError::Io {
            ctx: format!("failed to spawn git {args:?}"),
            source: e,
        })?;
        if output.status.success() {
            return Ok(Copied::Commit(Self::head_commit(checkout)?));
        }
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if Self::mid_op_state(checkout) != Some(MidOperation::Conflict(ConflictOp::CherryPick)) {
            return Err(failed(args, checkout, stderr));
        }
        let unmerged = Self::unmerged(checkout)?;
        if !unmerged.is_empty() {
            return Ok(Copied::Conflicted(unmerged));
        }
        if Self::index_matches_head(checkout)? {
            Self::run(["cherry-pick", "--skip"], checkout)?;
            return Ok(Copied::Empty);
        }
        Err(failed(args, checkout, stderr))
    }

    pub(super) fn copy_without_commit(
        checkout: &Path,
        commit: &CommitId,
        mainline: Option<usize>,
    ) -> Result<Vec<String>, VcsError> {
        let mainline = mainline.map(|n| n.to_string());
        let mut args = vec!["cherry-pick", "--no-commit"];
        if let Some(n) = &mainline {
            args.extend(["-m", n.as_str()]);
        }
        args.push(commit.as_str());
        let output = git_command()
            .args(lock_driver_held_args())
            .args(&args)
            .current_dir(checkout)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        let unmerged = Self::unmerged(checkout)?;
        if !output.status.success() && unmerged.is_empty() {
            return Err(failed(
                &args,
                checkout,
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        // `--no-commit` leaves no CHERRY_PICK_HEAD; written by hand, it makes
        // the checkout one stopped in a cherry-pick, which `git cherry-pick
        // --abort` undoes and the next commit ends, keeping the copied
        // commit's author.
        Self::run(
            ["update-ref", "CHERRY_PICK_HEAD", commit.as_str()],
            checkout,
        )?;
        Ok(unmerged)
    }

    pub(super) fn cherry_pick_head(checkout: &Path) -> Result<Option<CommitId>, VcsError> {
        let args = ["rev-parse", "-q", "--verify", "CHERRY_PICK_HEAD^{commit}"];
        let output = git_command()
            .args(args)
            .current_dir(checkout)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        commit_id(&String::from_utf8_lossy(&output.stdout), &args, checkout).map(Some)
    }

    pub(super) fn cherry_picked_from(
        repo: &Path,
        commit: &CommitId,
    ) -> Result<Option<CommitId>, VcsError> {
        let args = ["log", "-1", "--format=%B", commit.as_str(), "--"];
        let message = Self::run(args, repo)?;
        let Some(id) = message.lines().rev().find_map(|line| {
            line.trim()
                .strip_prefix("(cherry picked from commit ")?
                .strip_suffix(')')
        }) else {
            return Ok(None);
        };
        commit_id(id, &args, repo).map(Some)
    }

    pub(super) fn parents_of(repo: &Path, commit: &CommitId) -> Result<Vec<CommitId>, VcsError> {
        let args = ["rev-list", "--parents", "-n", "1", commit.as_str()];
        let line = Self::run_trimmed(args, repo)?;
        line.split_whitespace()
            .skip(1)
            .map(|id| commit_id(id, &args, repo))
            .collect()
    }

    pub(super) fn commit_message_without(
        repo: &Path,
        commit: &CommitId,
        keys: &[&str],
    ) -> Result<String, VcsError> {
        let message = Self::run(["log", "-1", "--format=%B", commit.as_str(), "--"], repo)?;
        let kept: Vec<&str> = message
            .lines()
            .filter(|line| {
                !keys.iter().any(|key| {
                    line.strip_prefix(key)
                        .is_some_and(|rest| rest.starts_with(':'))
                })
            })
            .collect();
        Ok(kept.join("\n").trim_end().to_owned())
    }

    fn index_matches_head(checkout: &Path) -> Result<bool, VcsError> {
        let args = ["diff-index", "--cached", "--quiet", "HEAD", "--"];
        let output = git_command()
            .args(args)
            .current_dir(checkout)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(failed(
                &args,
                checkout,
                String::from_utf8_lossy(&output.stderr).into_owned(),
            )),
        }
    }

    pub(super) fn head_commit(checkout: &Path) -> Result<CommitId, VcsError> {
        let args = ["rev-parse", "--verify", "HEAD^{commit}"];
        commit_id(&Self::run_trimmed(args, checkout)?, &args, checkout)
    }
}
