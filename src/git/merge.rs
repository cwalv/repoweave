//! Merges, as git computes them: as trees where `merge-tree --write-tree`
//! exists, and in a checkout stopped before its commit where it does not.

use super::records::{commit_id, failed, FALLBACK_IDENTITY};
use super::{git_command, local_branch, GitVcs};
use crate::vcs::{
    BackupWitness, CommitId, RawRefName, RefMerge, TreeId, TreeMerge, TreeMerges, Vcs, VcsError,
};
use std::path::Path;

impl TreeMerges for GitVcs {
    fn merge_tree(
        &self,
        repo: &Path,
        first: &CommitId,
        second: &CommitId,
    ) -> Result<TreeMerge, VcsError> {
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "-z",
            first.as_str(),
            second.as_str(),
        ];
        let output = git_command()
            .args(args)
            .current_dir(repo)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        let conflicted = match output.status.code() {
            Some(0) => false,
            Some(1) => true,
            _ => {
                return Err(failed(
                    &args,
                    repo,
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                ))
            }
        };
        let mut fields = output
            .stdout
            .split(|byte| *byte == 0)
            .map(|field| String::from_utf8_lossy(field).into_owned());
        let tree = fields
            .next()
            .filter(|tree| !tree.is_empty())
            .ok_or_else(|| failed(&args, repo, "merge-tree printed no tree".to_owned()))?;
        if !conflicted {
            return Ok(TreeMerge::Clean(TreeId::from_object_name(&tree)));
        }
        let mut paths: Vec<String> = fields.take_while(|field| !field.is_empty()).collect();
        paths.dedup();
        Ok(TreeMerge::Conflicted {
            tree: TreeId::from_object_name(&tree),
            paths,
        })
    }

    fn file_in_tree(
        &self,
        repo: &Path,
        tree: &TreeId,
        path: &str,
    ) -> Result<Option<String>, VcsError> {
        let named = Self::run_trimmed(["ls-tree", "--name-only", tree.as_str(), "--", path], repo)?;
        if named.is_empty() {
            return Ok(None);
        }
        Self::run(
            ["cat-file", "blob", &format!("{}:{path}", tree.as_str())],
            repo,
        )
        .map(Some)
    }
}

impl GitVcs {
    pub(super) fn begin_checkout_merge(
        checkout: &Path,
        other: &CommitId,
    ) -> Result<Vec<String>, VcsError> {
        let args = [
            "merge",
            "--no-commit",
            "--no-ff",
            "--no-edit",
            other.as_str(),
        ];
        let output = git_command()
            .args(args)
            .current_dir(checkout)
            .output()
            .map_err(|e| VcsError::Io {
                ctx: format!("failed to spawn git {args:?}"),
                source: e,
            })?;
        if output.status.success() {
            return Ok(Vec::new());
        }
        let unmerged = Self::unmerged(checkout)?;
        if unmerged.is_empty() {
            return Err(failed(
                &args,
                checkout,
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        Ok(unmerged)
    }

    pub(super) fn unmerged(checkout: &Path) -> Result<Vec<String>, VcsError> {
        Ok(
            Self::run_trimmed(["diff", "--name-only", "--diff-filter=U"], checkout)?
                .lines()
                .map(str::to_owned)
                .collect(),
        )
    }

    pub(super) fn merge_head(checkout: &Path) -> Result<Option<CommitId>, VcsError> {
        let args = ["rev-parse", "-q", "--verify", "MERGE_HEAD"];
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

    pub(super) fn staged(checkout: &Path, path: &str) -> Result<Option<String>, VcsError> {
        let listed = Self::run_trimmed(["ls-files", "--stage", "--", path], checkout)?;
        if !listed
            .lines()
            .any(|line| line.split_whitespace().nth(2) == Some("0"))
        {
            return Ok(None);
        }
        Self::run(["show", &format!(":0:{path}")], checkout).map(Some)
    }

    fn commit_tree_of(
        repo: &Path,
        tree: &str,
        first: &CommitId,
        second: &CommitId,
        message: &str,
    ) -> Result<CommitId, VcsError> {
        let args = [
            "commit-tree",
            tree,
            "-p",
            first.as_str(),
            "-p",
            second.as_str(),
            "-m",
            message,
        ];
        let fallback: &[(&str, &str)] = if Self::has_identity(repo)? {
            &[]
        } else {
            &FALLBACK_IDENTITY
        };
        let written = Self::run_bytes_in(&args, repo, &[], fallback)?;
        commit_id(&String::from_utf8_lossy(&written), &args, repo)
    }

    pub(super) fn commit_staged_merge(
        checkout: &Path,
        first: &CommitId,
        second: &CommitId,
        message: &str,
    ) -> Result<CommitId, VcsError> {
        let tree = Self::run_trimmed(["write-tree"], checkout)?;
        let commit = Self::commit_tree_of(checkout, &tree, first, second, message)?;
        Self::run(["update-ref", "HEAD", commit.as_str()], checkout)?;
        Self::run(["merge", "--quit"], checkout)?;
        Ok(commit)
    }

    pub(super) fn merge_commit_as_refs(
        &self,
        repo: &Path,
        first: &CommitId,
        second: &CommitId,
        message: &str,
    ) -> Result<RefMerge, VcsError> {
        if let Some(trees) = self.tree_merges() {
            return Ok(match trees.merge_tree(repo, first, second)? {
                TreeMerge::Clean(tree) => RefMerge::Merged(Self::commit_tree_of(
                    repo,
                    tree.as_str(),
                    first,
                    second,
                    message,
                )?),
                TreeMerge::Conflicted { paths, .. } => RefMerge::Conflicted { paths },
            });
        }
        let scratch = Self::run_trimmed(
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &format!("rwv-merge-scratch-{}", std::process::id()),
            ],
            repo,
        )?;
        let scratch = Path::new(&scratch);
        Self::run(
            [
                "worktree",
                "add",
                "--detach",
                &scratch.display().to_string(),
                first.as_str(),
            ],
            repo,
        )?;
        let merged = Self::begin_checkout_merge(scratch, second).and_then(|paths| {
            if paths.is_empty() {
                let tree = Self::run_trimmed(["write-tree"], scratch)?;
                Ok(RefMerge::Merged(Self::commit_tree_of(
                    scratch, &tree, first, second, message,
                )?))
            } else {
                Ok(RefMerge::Conflicted { paths })
            }
        });
        let _ = Self::run(["merge", "--abort"], scratch);
        let removed = Self::run(["worktree", "remove", &scratch.display().to_string()], repo);
        let merged = merged?;
        removed?;
        Ok(merged)
    }

    pub(super) fn update_branch_ref(
        repo: &Path,
        branch: &RawRefName,
        expected: &CommitId,
        to: &CommitId,
    ) -> Result<(), VcsError> {
        Self::run(
            [
                "update-ref",
                &local_branch(branch.as_str()),
                to.as_str(),
                expected.as_str(),
            ],
            repo,
        )
        .map(|_| ())
    }

    pub(super) fn witness_backs_up(
        repo: &Path,
        expected: &CommitId,
        kept: &BackupWitness,
    ) -> Result<(), VcsError> {
        if kept.repo() == repo && kept.backup().tip.id() == expected {
            return Ok(());
        }
        Err(VcsError::StaleRefWitness {
            repo: repo.to_path_buf(),
            expected: format!("a backup of {}", expected.short()),
            observed: format!("a backup of {}", kept.backup().tip.id().short()),
        })
    }
}
