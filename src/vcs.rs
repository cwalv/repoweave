//! Version control system abstraction.
//!
//! repoweave operates on repos and worktrees. The VCS layer abstracts over
//! the specific tool (git, jj, sl, hg) so core logic doesn't hardcode git.

use std::fmt;
use std::path::{Path, PathBuf};

/// A resolved commit identifier, independent of VCS.
///
/// `canonical` is the resolved commit SHA. `display` optionally preserves a
/// tag/branch name when the value was constructed from a tag-form input
/// (e.g., `v0.3.4`). Equality compares only the canonical SHA so two
/// `RevisionId`s referring to the same commit — one tag-form, one SHA-form
/// — compare equal.
///
/// Construction:
/// - [`RevisionId::raw`] — a value where the canonical may not be a SHA yet
///   (e.g., during YAML deserialization, before [`Vcs::resolve_revision`]
///   runs against the on-disk repo).
/// - [`RevisionId::from_canonical`] — a value where the canonical SHA is
///   known (lock generation, head resolution, post-resolve).
///
/// Serde: a `RevisionId` round-trips through a single YAML string. On
/// serialization the display form is preferred (preserves tag-form in lock
/// files); on deserialization the string lands in `canonical` with
/// `display: None` — resolution to a real SHA happens later via
/// [`Vcs::resolve_revision`].
#[derive(Debug, Clone)]
pub struct RevisionId {
    canonical: String,
    display: Option<String>,
}

impl RevisionId {
    /// Construct from a raw string where `canonical` may be a tag/branch
    /// rather than a SHA. Used for deserialization and tests; should be
    /// resolved against a repo via [`Vcs::resolve_revision`] before being
    /// compared against a SHA from `head_revision`.
    pub fn raw(s: impl Into<String>) -> Self {
        Self {
            canonical: s.into(),
            display: None,
        }
    }

    /// Construct with a known canonical commit SHA and optional display
    /// form. When `display` equals `canonical` it is suppressed so
    /// serialization stays clean.
    pub fn from_canonical(canonical: impl Into<String>, display: Option<String>) -> Self {
        let canonical = canonical.into();
        let display = display.filter(|d| d != &canonical);
        Self { canonical, display }
    }

    /// The canonical SHA (after resolution) or the raw input (before).
    pub fn as_str(&self) -> &str {
        &self.canonical
    }

    /// The display form (tag-form when present), falling back to canonical.
    pub fn display_str(&self) -> &str {
        self.display.as_deref().unwrap_or(&self.canonical)
    }
}

impl PartialEq for RevisionId {
    fn eq(&self, other: &Self) -> bool {
        self.canonical == other.canonical
    }
}

impl Eq for RevisionId {}

impl fmt::Display for RevisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_str())
    }
}

impl serde::Serialize for RevisionId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.display_str())
    }
}

impl<'de> serde::Deserialize<'de> for RevisionId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::raw)
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

/// Resolve a VCS implementation from a [`crate::manifest::VcsType`].
pub(crate) fn vcs_for(vcs_type: crate::manifest::VcsType) -> Box<dyn Vcs> {
    match vcs_type {
        crate::manifest::VcsType::Git => Box::new(crate::git::GitVcs),
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
    ///
    /// The returned `RevisionId` carries the canonical commit SHA. When a
    /// tag points at HEAD, the implementation may also populate `display`
    /// to preserve the tag-form for human-readable output.
    fn head_revision(&self, repo: &Path) -> anyhow::Result<RevisionId>;

    /// Resolve a revision string (SHA, tag, branch) to a fully-resolved
    /// [`RevisionId`] with the canonical commit SHA filled in.
    ///
    /// When the input string differs from the canonical SHA, it is
    /// preserved as the display form for round-tripping in lock files and
    /// human-readable output. Returns an error if the revision is unknown
    /// in this repo.
    fn resolve_revision(&self, repo: &Path, rev: &str) -> anyhow::Result<RevisionId>;

    /// Get the current branch/ref name, if on one.
    fn current_ref(&self, repo: &Path) -> anyhow::Result<Option<RefName>>;

    /// Create a worktree at `dest` from `repo`, on a new branch `branch_name`
    /// starting at `start_point`.
    fn create_worktree(
        &self,
        repo: &Path,
        dest: &Path,
        branch_name: &str,
        start_point: &RevisionId,
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
    fn tag_at_head(&self, repo: &Path) -> anyhow::Result<Option<RefName>>;

    /// Check out a specific revision in a repo.
    fn checkout(&self, repo: &Path, revision: &RevisionId) -> anyhow::Result<()>;

    /// Delete a local branch by name. Uses force-delete semantics.
    fn delete_branch(&self, repo: &Path, branch: &str) -> anyhow::Result<()>;

    /// Prune stale worktree administrative files from a repo.
    fn worktree_prune(&self, repo: &Path) -> anyhow::Result<()>;

    /// List local branch names that start with `prefix`.
    fn list_branches_with_prefix(&self, repo: &Path, prefix: &str) -> anyhow::Result<Vec<String>>;

    /// Return the default branch name for `repo`.
    ///
    /// Reads `refs/remotes/origin/HEAD` via `git symbolic-ref` and strips the
    /// `refs/remotes/origin/` prefix to obtain the branch name (e.g., `main`).
    /// Falls back to `"main"` when no remote or no `origin/HEAD` is configured.
    fn default_branch(&self, repo: &Path) -> anyhow::Result<RefName>;
}
