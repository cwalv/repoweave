//! Git implementation of the [`Vcs`] trait.

use crate::vcs::{RefName, RevisionId, Vcs};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Git-based version control operations.
pub struct GitVcs;

impl GitVcs {
    /// Run a git command in `dir` and return trimmed stdout on success.
    fn run(args: &[&str], dir: &Path) -> anyhow::Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .map_err(|e| anyhow::anyhow!("failed to run git: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git {:?} failed: {}", args, stderr.trim());
        }

        Ok(String::from_utf8(output.stdout)
            .map_err(|e| anyhow::anyhow!("git output not valid UTF-8: {e}"))?
            .trim()
            .to_string())
    }
}

impl GitVcs {
    /// Initialize a new git repo at `dest`.
    pub fn init_repo(&self, dest: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dest)
            .map_err(|e| anyhow::anyhow!("failed to create directory {}: {e}", dest.display()))?;
        Self::run(&["init", "--initial-branch=main"], dest)?;
        Ok(())
    }
}

impl Vcs for GitVcs {
    fn name(&self) -> &str {
        "git"
    }

    fn clone_repo(&self, url: &str, dest: &Path) -> anyhow::Result<()> {
        let dest_str = dest
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("destination path is not valid UTF-8"))?;
        Self::run(&["clone", url, dest_str], Path::new("."))?;
        Ok(())
    }

    fn head_revision(&self, repo: &Path) -> anyhow::Result<RevisionId> {
        let sha = Self::run(&["rev-parse", "HEAD"], repo)?;
        Ok(RevisionId::new(sha))
    }

    fn current_ref(&self, repo: &Path) -> anyhow::Result<Option<RefName>> {
        match Self::run(&["symbolic-ref", "--short", "HEAD"], repo) {
            Ok(name) => Ok(Some(RefName::new(name))),
            Err(_) => Ok(None), // detached HEAD
        }
    }

    fn create_worktree(
        &self,
        repo: &Path,
        dest: &Path,
        branch_name: &str,
        start_point: &str,
    ) -> anyhow::Result<()> {
        let dest_str = dest
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("worktree path is not valid UTF-8"))?;

        // First try creating a new branch with -b.
        let result = Self::run(
            &["worktree", "add", "-b", branch_name, dest_str, start_point],
            repo,
        );

        if let Err(e) = result {
            // If the branch already exists, try using it as-is (no -b).
            // This handles the case where a previous delete didn't clean up branches.
            let err_str = e.to_string();
            if err_str.contains("already exists") || err_str.contains("already a worktree") {
                // Delete the stale branch first, then retry with -b.
                // If delete fails, fall back to using the existing branch directly.
                let deleted = Self::run(&["branch", "-D", branch_name], repo).is_ok();
                if deleted {
                    Self::run(
                        &["worktree", "add", "-b", branch_name, dest_str, start_point],
                        repo,
                    )?;
                } else {
                    Self::run(&["worktree", "add", dest_str, branch_name], repo)?;
                }
            } else {
                return Err(e);
            }
        }

        Ok(())
    }

    fn remove_worktree(&self, repo: &Path, worktree_path: &Path) -> anyhow::Result<()> {
        let wt_str = worktree_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("worktree path is not valid UTF-8"))?;
        Self::run(&["worktree", "remove", "--force", wt_str], repo)?;
        Ok(())
    }

    fn is_repo(&self, path: &Path) -> bool {
        Self::run(&["rev-parse", "--git-dir"], path).is_ok()
    }

    fn list_worktrees(&self, repo: &Path) -> anyhow::Result<Vec<PathBuf>> {
        let output = Self::run(&["worktree", "list", "--porcelain"], repo)?;
        let paths = output
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(PathBuf::from)
            .collect();
        Ok(paths)
    }

    fn has_uncommitted_changes(&self, repo: &Path) -> anyhow::Result<bool> {
        // `git status --porcelain` prints one line per dirty entry;
        // empty output means the tree is clean.
        let output = Self::run(&["status", "--porcelain"], repo)?;
        Ok(!output.is_empty())
    }

    fn tag_at_head(&self, repo: &Path) -> anyhow::Result<Option<String>> {
        // `git tag --points-at HEAD` lists tags that resolve to HEAD.
        let output = Self::run(&["tag", "--points-at", "HEAD"], repo)?;
        Ok(output.lines().next().map(|s| s.to_string()))
    }

    fn checkout(&self, repo: &Path, revision: &str) -> anyhow::Result<()> {
        Self::run(&["checkout", revision], repo)?;
        Ok(())
    }

    fn delete_branch(&self, repo: &Path, branch: &str) -> anyhow::Result<()> {
        Self::run(&["branch", "-D", branch], repo)?;
        Ok(())
    }

    fn worktree_prune(&self, repo: &Path) -> anyhow::Result<()> {
        Self::run(&["worktree", "prune"], repo)?;
        Ok(())
    }

    fn list_branches_with_prefix(&self, repo: &Path, prefix: &str) -> anyhow::Result<Vec<String>> {
        // `git branch --list 'prefix/*'` lists all local branches under the prefix.
        let pattern = format!("{prefix}/*");
        let output = Self::run(&["branch", "--list", &pattern], repo)?;
        let branches = output
            .lines()
            .map(|line| {
                // Lines from `git branch` are prefixed with "* " (current) or "  ".
                line.trim_start_matches('*').trim().to_string()
            })
            .filter(|s| !s.is_empty())
            .collect();
        Ok(branches)
    }

    fn default_branch(&self, repo: &Path) -> anyhow::Result<RefName> {
        const FALLBACK: &str = "main";
        const PREFIX: &str = "refs/remotes/origin/";

        match Self::run(&["symbolic-ref", "refs/remotes/origin/HEAD"], repo) {
            Ok(sym_ref) => {
                let branch = sym_ref.strip_prefix(PREFIX).unwrap_or(FALLBACK).to_string();
                Ok(RefName::new(branch))
            }
            Err(_) => Ok(RefName::new(FALLBACK)),
        }
    }
}
