//! Version control system abstraction.
//!
//! repoweave operates on repos and worktrees. The VCS layer abstracts over
//! the specific tool (git, jj, sl, hg) so core logic doesn't hardcode git.

use std::fmt;
use std::path::{Path, PathBuf};

/// A resolved commit identifier, independent of VCS.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RevisionId(String);

impl RevisionId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RevisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A named ref (branch, tag, bookmark), independent of VCS.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RefName(String);

impl RefName {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Operations repoweave needs from a version control system.
///
/// Implementations exist for git (and eventually jj, sl, hg). Each method
/// takes a repo path and operates on it — the trait is stateless.
pub trait Vcs {
    /// Human-readable name (e.g., `"git"`, `"jj"`).
    fn name(&self) -> &str;

    /// Clone a remote URL into `dest`.
    fn clone_repo(&self, url: &str, dest: &Path) -> anyhow::Result<()>;

    /// Resolve the current HEAD to a revision ID.
    fn head_revision(&self, repo: &Path) -> anyhow::Result<RevisionId>;

    /// Get the current branch/ref name, if on one.
    fn current_ref(&self, repo: &Path) -> anyhow::Result<Option<RefName>>;

    /// Create a worktree at `dest` from `repo`, on a new branch `branch_name`
    /// starting at `start_point`.
    fn create_worktree(
        &self,
        repo: &Path,
        dest: &Path,
        branch_name: &str,
        start_point: &str,
    ) -> anyhow::Result<()>;

    /// Remove a worktree previously created at `worktree_path`.
    fn remove_worktree(&self, repo: &Path, worktree_path: &Path) -> anyhow::Result<()>;

    /// Check whether `path` is a repository (or worktree) managed by this VCS.
    fn is_repo(&self, path: &Path) -> bool;

    /// List worktrees for a repo, returning their paths.
    fn list_worktrees(&self, repo: &Path) -> anyhow::Result<Vec<PathBuf>>;

    /// Return `true` if the working tree has uncommitted changes.
    ///
    /// This includes staged but uncommitted changes, unstaged modifications,
    /// and untracked files.
    fn has_uncommitted_changes(&self, repo: &Path) -> anyhow::Result<bool>;

    /// Return the tag name pointing at HEAD, if any.
    ///
    /// When multiple tags point at HEAD the implementation may return any one
    /// of them. Returns `None` when no tag points at the current HEAD commit.
    fn tag_at_head(&self, repo: &Path) -> anyhow::Result<Option<String>>;

    /// Check out a specific revision (SHA, tag, or branch) in a repo.
    fn checkout(&self, repo: &Path, revision: &str) -> anyhow::Result<()>;

    /// Delete a local branch by name. Uses force-delete semantics.
    fn delete_branch(&self, repo: &Path, branch: &str) -> anyhow::Result<()>;

    /// Prune stale worktree administrative files from a repo.
    fn worktree_prune(&self, repo: &Path) -> anyhow::Result<()>;

    /// List local branch names that start with `prefix`.
    fn list_branches_with_prefix(&self, repo: &Path, prefix: &str) -> anyhow::Result<Vec<String>>;
}
