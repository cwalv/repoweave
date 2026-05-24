//! Version control system abstraction.
//!
//! repoweave operates on repos and worktrees. The VCS layer abstracts over
//! the specific tool (git, jj, sl, hg) so core logic doesn't hardcode git.

use crate::manifest::Role;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// A resolved commit identifier — `canonical` is always a 40-hex SHA.
///
/// `display` optionally preserves a tag/branch name when the value was
/// constructed from a tag-form input (e.g., `v0.3.4`). Equality compares
/// only the canonical SHA so two `ResolvedRevisionId`s referring to the
/// same commit — one tag-form, one SHA-form — compare equal.
///
/// Construction is path-rooted: the only public constructors are
/// [`Vcs::resolve_revision`] / [`Vcs::head_revision`] (which resolve
/// against a real repo) and [`ResolvedRevisionId::from_canonical`]
/// (mint with a known SHA, e.g. directly from `head_revision` output).
/// There is no public way to mint a `ResolvedRevisionId` from a free
/// string — the parse boundary lives in [`RawRevisionId`].
///
/// Serde: only `Serialize`. Writes the display form when present, else
/// the canonical SHA. Deserialization deliberately is not implemented;
/// lock-file parsing yields [`RawRevisionId`] which must then be
/// resolved against the on-disk repo. See
/// `docs/agent-persona/fp-principles-in-rust.md` ("make illegal states
/// unrepresentable") and `docs/agent-persona/ousterhout-philosophy-of-software-design.md`
/// ("define errors out of existence by changing the data structure").
#[derive(Debug, Clone)]
pub struct ResolvedRevisionId {
    canonical: String,
    display: Option<String>,
}

impl ResolvedRevisionId {
    /// Construct with a known canonical commit SHA and optional display
    /// form. When `display` equals `canonical` it is suppressed so
    /// serialization stays clean.
    pub fn from_canonical(canonical: impl Into<String>, display: Option<String>) -> Self {
        let canonical = canonical.into();
        let display = display.filter(|d| d != &canonical);
        Self { canonical, display }
    }

    /// Construct a `ResolvedRevisionId` from a string that the caller
    /// asserts is already a canonical commit SHA, bypassing the usual
    /// path-rooted resolution. `pub(crate)` to keep this assertion
    /// crate-internal — there is no public way to mint a resolved value
    /// from a free string.
    ///
    /// Sole legitimate caller: [`crate::sync::read_savepoint`], which
    /// reads a value produced by `git rev-parse refs/rwv/pre-op/<id>` —
    /// rev-parse on a fully-qualified ref-or-SHA always emits the
    /// canonical 40-hex SHA. Re-resolving via `Vcs::resolve_revision`
    /// would cost an extra git invocation per `rwv abort` without
    /// strengthening the invariant.
    pub(crate) fn from_canonical_unchecked(s: impl Into<String>) -> Self {
        Self {
            canonical: s.into(),
            display: None,
        }
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

impl PartialEq for ResolvedRevisionId {
    fn eq(&self, other: &Self) -> bool {
        self.canonical == other.canonical
    }
}

impl Eq for ResolvedRevisionId {}

impl fmt::Display for ResolvedRevisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_str())
    }
}

impl serde::Serialize for ResolvedRevisionId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.display_str())
    }
}

// Deliberately no `Deserialize` impl: see the type-level docs. Lock-file
// scalars deserialize into `RawRevisionId`; the only way to obtain a
// `ResolvedRevisionId` is via path-rooted resolution.

/// A raw, unresolved revision identifier as it appears in a lock file.
///
/// `RawRevisionId` wraps the YAML scalar verbatim — it may be a tag name,
/// a branch name, or a 40-hex SHA, and at the type level we do not know
/// which. This is the only revision type that participates in lock-file
/// *parsing* (the [`serde::Deserialize`] entry point). It is intentionally
/// not interchangeable with [`ResolvedRevisionId`]: there is no `PartialEq`
/// between the two, and `RawRevisionId` cannot be fed to commit-id
/// operations such as `Vcs::checkout`. To turn a raw value into a value
/// safe for SHA comparison, run it through
/// [`crate::manifest::LockFile::resolve_versions`] (which calls
/// [`Vcs::resolve_revision`] against the on-disk repo).
///
/// `Display`, `Serialize`, and `Eq` all operate on the string verbatim
/// ("same name"). Useful for "did this lock entry change name between two
/// reads"; not useful (and not provided) for "do these point at the same
/// commit".
///
/// See `docs/agent-persona/fp-principles-in-rust.md` ("make illegal states
/// unrepresentable") and `docs/agent-persona/ousterhout-philosophy-of-software-design.md`
/// ("define errors out of existence by changing the data structure").
///
/// # Compile-time invariant
///
/// A `RawRevisionId` cannot be compared against a `ResolvedRevisionId`.
/// The following doc-test fails to compile — this is the type system
/// enforcing the contract that motivated the split (fo-gvb0v).
///
/// ```compile_fail
/// use repoweave::vcs::{RawRevisionId, ResolvedRevisionId};
/// let raw = RawRevisionId::new("v1.0.0");
/// let resolved = ResolvedRevisionId::from_canonical(
///     "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
///     Some("v1.0.0".to_string()),
/// );
/// let _ = raw == resolved; // E0277: PartialEq not implemented across types
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RawRevisionId(String);

impl RawRevisionId {
    /// Construct from any string. Public because lock-file deserialization
    /// and tests need to mint raw values; the string is treated as opaque.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RawRevisionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl serde::Serialize for RawRevisionId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for RawRevisionId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

/// In-flight VCS operation whose conflict needs human resolution.
///
/// Passed to [`Vcs::conflict_resolution_hint`] so sync's conflict-bail
/// messages embed VCS-appropriate "how do I resume this?" text without
/// hardcoding git vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictOp {
    /// Native rebase (`git rebase`) — resumes with `git rebase --continue`.
    Rebase,
    /// Merge (`git merge`) — resumes with `git merge --continue`.
    Merge,
    /// Cherry-pick (`git cherry-pick`) — resumes with `git cherry-pick --continue`.
    /// Used by sync's project-repo rebase-with-lock-exclusion path.
    CherryPick,
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

/// Typed errors returned by [`Vcs`] trait methods.
///
/// Specific variants (`NotARepo`, `RevisionNotFound`, ...) let callers
/// pattern-match on the failure mode and choose recovery — re-lock for
/// `RevisionNotFound`, retry-or-abort for `Io`, surface stderr for
/// `CommandFailed`.
///
/// Implementations should map to the most specific variant they can detect
/// and fall back to `CommandFailed` for everything else.
#[derive(Debug)]
pub enum VcsError {
    /// Path is not a VCS repository (or doesn't exist).
    NotARepo(PathBuf),
    /// A named revision (SHA, tag, branch) couldn't be resolved.
    RevisionNotFound { repo: PathBuf, rev: String },
    /// Branch already exists (when caller attempted to create one).
    BranchAlreadyExists { repo: PathBuf, branch: RefName },
    /// Worktree path already exists (when caller attempted to create one).
    WorktreeExists(PathBuf),
    /// Working tree has uncommitted changes when caller required clean state.
    UncommittedChanges(PathBuf),
    /// I/O failure spawning or reading process output.
    Io { ctx: String, source: io::Error },
    /// Underlying VCS command failed for a reason not modeled above.
    /// Carries args and stderr so the caller can surface them.
    CommandFailed {
        args: Vec<String>,
        repo: PathBuf,
        stderr: String,
    },
}

impl VcsError {
    /// Stable variant tag suitable for `--json` output.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotARepo(_) => "not-a-repo",
            Self::RevisionNotFound { .. } => "revision-not-found",
            Self::BranchAlreadyExists { .. } => "branch-already-exists",
            Self::WorktreeExists(_) => "worktree-exists",
            Self::UncommittedChanges(_) => "uncommitted-changes",
            Self::Io { .. } => "io",
            Self::CommandFailed { .. } => "command-failed",
        }
    }
}

impl fmt::Display for VcsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotARepo(p) => write!(f, "{} is not a vcs repository", p.display()),
            Self::RevisionNotFound { repo, rev } => {
                write!(f, "revision '{rev}' not found in {}", repo.display())
            }
            Self::BranchAlreadyExists { repo, branch } => {
                write!(f, "branch '{branch}' already exists in {}", repo.display())
            }
            Self::WorktreeExists(p) => write!(f, "worktree path already exists: {}", p.display()),
            Self::UncommittedChanges(p) => {
                write!(f, "{} has uncommitted changes", p.display())
            }
            Self::Io { ctx, source } => write!(f, "{ctx}: {source}"),
            Self::CommandFailed { args, repo, stderr } => write!(
                f,
                "git {:?} in {} failed: {}",
                args,
                repo.display(),
                stderr.trim()
            ),
        }
    }
}

impl std::error::Error for VcsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Operations repoweave needs from a version control system.
///
/// Implementations exist for git (and eventually jj, sl, hg). Each method
/// takes a repo path and operates on it — the trait is stateless.
///
/// Methods return [`VcsError`] (rather than `anyhow::Error`) so callers
/// can pattern-match on failure modes. Application-level glue may convert
/// to `anyhow::Error` at the boundary via the `?` operator.
pub trait Vcs {
    /// Human-readable name (e.g., `"git"`, `"jj"`).
    fn name(&self) -> &str;

    /// Clone a remote URL into `dest`.
    fn clone_repo(&self, url: &str, dest: &Path) -> Result<(), VcsError>;

    /// Clone a remote URL into `dest`, naming the remote `remote_name`
    /// instead of the VCS default (`origin` for git).
    ///
    /// Used to express the `fork` role convention: the source URL is the
    /// upstream-of-record, not a push target, so it should not be aliased to
    /// `origin`.
    fn clone_repo_with_remote_name(
        &self,
        url: &str,
        dest: &Path,
        remote_name: &str,
    ) -> Result<(), VcsError>;

    /// Clone `url` into `dest`, naming the remote according to the
    /// convention this VCS uses for the given [`Role`].
    ///
    /// Pushing remote-naming policy into the VCS layer keeps git-specific
    /// vocabulary (`upstream` vs `origin`) out of the manifest types. For
    /// [`GitVcs`](crate::git::GitVcs): `Role::Fork` clones to `upstream`
    /// (so a stray `git push` does not target the source-of-record); all
    /// other roles clone to `origin`. Other VCS impls choose their own
    /// conventions.
    fn clone_with_role(&self, url: &str, dest: &Path, role: Role) -> Result<(), VcsError>;

    /// Resolve `branch` on the remote associated with `role` in `repo`.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): builds the qualified ref
    /// `"upstream/{branch}"` for `Role::Fork`, `"origin/{branch}"` for all
    /// other roles, and resolves it via [`resolve_revision`]. There is no
    /// bare-branch fallback — a missing role-conventional remote yields
    /// [`VcsError::RevisionNotFound`] so callers don't silently advance to
    /// the local branch tip.
    ///
    /// [`resolve_revision`]: Vcs::resolve_revision
    fn resolve_branch_on_remote(
        &self,
        repo: &Path,
        role: Role,
        branch: &RefName,
    ) -> Result<ResolvedRevisionId, VcsError>;

    /// Resolve the current HEAD to a revision ID.
    ///
    /// The returned `ResolvedRevisionId` carries the canonical commit SHA. When a
    /// tag points at HEAD, the implementation may also populate `display`
    /// to preserve the tag-form for human-readable output.
    fn head_revision(&self, repo: &Path) -> Result<ResolvedRevisionId, VcsError>;

    /// Resolve a revision string (SHA, tag, branch) to a fully-resolved
    /// [`ResolvedRevisionId`] with the canonical commit SHA filled in.
    ///
    /// When the input string differs from the canonical SHA, it is
    /// preserved as the display form for round-tripping in lock files and
    /// human-readable output. Returns [`VcsError::RevisionNotFound`] if
    /// the revision is unknown in this repo.
    fn resolve_revision(&self, repo: &Path, rev: &str) -> Result<ResolvedRevisionId, VcsError>;

    /// Get the current branch/ref name, if on one.
    fn current_ref(&self, repo: &Path) -> Result<Option<RefName>, VcsError>;

    /// Create a worktree at `dest` from `repo`, on a new branch `branch_name`
    /// starting at `start_point`.
    fn create_worktree(
        &self,
        repo: &Path,
        dest: &Path,
        branch_name: &RefName,
        start_point: &ResolvedRevisionId,
    ) -> Result<(), VcsError>;

    /// Remove a worktree previously created at `worktree_path`.
    fn remove_worktree(&self, repo: &Path, worktree_path: &Path) -> Result<(), VcsError>;

    /// Check whether `path` is a repository (or worktree) managed by this VCS.
    fn is_repo(&self, path: &Path) -> bool;

    /// List worktrees for a repo, returning their paths.
    fn list_worktrees(&self, repo: &Path) -> Result<Vec<PathBuf>, VcsError>;

    /// Return `true` if the working tree has uncommitted changes.
    ///
    /// This includes staged but uncommitted changes, unstaged modifications,
    /// and untracked files.
    fn has_uncommitted_changes(&self, repo: &Path) -> Result<bool, VcsError>;

    /// Return the tag name pointing at HEAD, if any.
    ///
    /// When multiple tags point at HEAD the implementation may return any one
    /// of them. Returns `None` when no tag points at the current HEAD commit.
    fn tag_at_head(&self, repo: &Path) -> Result<Option<RefName>, VcsError>;

    /// Check out a specific revision in a repo.
    fn checkout(&self, repo: &Path, revision: &ResolvedRevisionId) -> Result<(), VcsError>;

    /// Delete a local branch by name. Uses force-delete semantics.
    fn delete_branch(&self, repo: &Path, branch: &RefName) -> Result<(), VcsError>;

    /// Prune stale worktree administrative files from a repo.
    fn worktree_prune(&self, repo: &Path) -> Result<(), VcsError>;

    /// List local branch names that start with `prefix`.
    fn list_branches_with_prefix(
        &self,
        repo: &Path,
        prefix: &RefName,
    ) -> Result<Vec<RefName>, VcsError>;

    /// Return the default branch name for `repo`.
    ///
    /// Reads `refs/remotes/origin/HEAD` via `git symbolic-ref` and strips the
    /// `refs/remotes/origin/` prefix to obtain the branch name (e.g., `main`).
    /// Falls back to `"main"` when no remote or no `origin/HEAD` is configured.
    fn default_branch(&self, repo: &Path) -> Result<RefName, VcsError>;

    /// Human-readable hint text for resuming `op` after the user resolves
    /// conflicts left in the working tree.
    ///
    /// Embedded verbatim in sync's conflict-bail messages so the operator
    /// sees concrete next steps (`git add <files>`; `git rebase --continue`)
    /// instead of an opaque "fix conflicts and re-run". Returned text is a
    /// short multi-line block suitable for splicing into a larger message —
    /// callers are expected to add surrounding context (which repo, how to
    /// re-run sync, how to abort) themselves.
    ///
    /// `op` is the in-flight operation that produced the conflict (rebase,
    /// merge, cherry-pick); the hint text varies per VCS and per op. No
    /// `repo` param: the hint text for [`GitVcs`](crate::git::GitVcs)
    /// doesn't vary per-repo, and adding a parameter we don't read would be
    /// noise. Add one if a future VCS needs to inspect on-disk state.
    fn conflict_resolution_hint(&self, op: ConflictOp) -> String;
}
