//! Version control system abstraction.
//!
//! repoweave operates on repos and worktrees. The VCS layer abstracts over
//! the specific tool (git, jj, sl, hg) so core logic doesn't hardcode git.

use crate::manifest::Role;
use schemars::JsonSchema;
use serde::Serialize;
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
    /// Sole legitimate caller: [`Vcs::resolve_savepoint`]'s `GitVcs`
    /// implementation, which reads a value produced by `git rev-parse
    /// refs/rwv/pre-op/<id>` — rev-parse on a fully-qualified ref-or-SHA
    /// always emits the canonical 40-hex SHA. Re-resolving via
    /// `Vcs::resolve_revision` would cost an extra git invocation per
    /// `rwv abort` without strengthening the invariant.
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
/// enforcing the contract that motivated the split.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
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
    /// In-flight VCS operation (rebase / merge / cherry-pick) hit a conflict.
    ///
    /// The repo is left in the VCS-native in-flight state — for git, that means
    /// mid-rebase with conflict markers in the working tree. Callers pair this
    /// with [`Vcs::conflict_resolution_hint`] to assemble the user-facing
    /// "edit conflicted files; `git add <files>`; `git rebase --continue`"
    /// message. The matching `op` tells the caller which hint to fetch.
    RebaseConflict { repo: PathBuf, op: ConflictOp },
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
            Self::RebaseConflict { .. } => "rebase-conflict",
            Self::Io { .. } => "io",
            Self::CommandFailed { .. } => "command-failed",
        }
    }
}

/// Wire-output mirror of [`VcsError`] for `--json` emission.
///
/// `VcsError` itself can't derive `Serialize` cleanly because tuple variants
/// (and `io::Error`) don't play nicely with serde's internally-tagged enum
/// representation. This struct-only mirror does: every variant carries
/// named fields, the tag matches [`VcsError::kind`], and a `From<&VcsError>`
/// impl converts at JSON-emission time.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum VcsErrorOutput {
    NotARepo {
        path: PathBuf,
    },
    RevisionNotFound {
        repo: PathBuf,
        rev: String,
    },
    BranchAlreadyExists {
        repo: PathBuf,
        branch: String,
    },
    WorktreeExists {
        path: PathBuf,
    },
    UncommittedChanges {
        path: PathBuf,
    },
    RebaseConflict {
        repo: PathBuf,
        op: ConflictOp,
    },
    Io {
        ctx: String,
        /// Display form of the underlying `io::Error`. The native source is
        /// dropped at the wire boundary since `io::Error` does not serialize.
        /// Named `message` (not `error`) to make clear this is free-form display
        /// text, not a typed discriminant that consumers can branch on.
        message: String,
    },
    CommandFailed {
        args: Vec<String>,
        repo: PathBuf,
        stderr: String,
    },
}

impl From<&VcsError> for VcsErrorOutput {
    fn from(e: &VcsError) -> Self {
        match e {
            VcsError::NotARepo(p) => Self::NotARepo { path: p.clone() },
            VcsError::RevisionNotFound { repo, rev } => Self::RevisionNotFound {
                repo: repo.clone(),
                rev: rev.clone(),
            },
            VcsError::BranchAlreadyExists { repo, branch } => Self::BranchAlreadyExists {
                repo: repo.clone(),
                branch: branch.as_str().to_owned(),
            },
            VcsError::WorktreeExists(p) => Self::WorktreeExists { path: p.clone() },
            VcsError::UncommittedChanges(p) => Self::UncommittedChanges { path: p.clone() },
            VcsError::RebaseConflict { repo, op } => Self::RebaseConflict {
                repo: repo.clone(),
                op: *op,
            },
            VcsError::Io { ctx, source } => Self::Io {
                ctx: ctx.clone(),
                message: source.to_string(),
            },
            VcsError::CommandFailed { args, repo, stderr } => Self::CommandFailed {
                args: args.clone(),
                repo: repo.clone(),
                stderr: stderr.clone(),
            },
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
            Self::RebaseConflict { repo, op } => {
                write!(
                    f,
                    "{op:?} in {} hit a conflict; resolve and continue, or abort to roll back",
                    repo.display()
                )
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

/// A pre-abort reference: the durable record of a repo's tip captured by
/// [`Vcs::create_pre_abort_ref`] just before `rwv abort` restores it.
///
/// `label` is an operator-spellable, VCS-native name for the reference
/// (e.g. for git: `refs/rwv/pre-abort/<op-id>`). The label is the recovery
/// hint surfaced in abort output so an operator can locate the pre-abort
/// tip and undo the abort. `revision` is the captured tip itself.
///
/// Information-preserving doctrine: pre-abort refs are written before any
/// restore and never deleted by abort's cleanup — abort is itself
/// undoable, and the pre-abort ref is the cheapest path back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreAbortRef {
    /// VCS-native, operator-spellable name of the reference (impl detail
    /// of the underlying VCS, surfaced for recovery hints only).
    pub label: String,
    /// The captured pre-abort tip.
    pub revision: ResolvedRevisionId,
}

/// Outcome of a [`Vcs::verified_restore_savepoint`] call.
///
/// Encodes the classification of the repo's current tip *before* any
/// restore happened, plus what (if anything) was restored. Foreign tips
/// are reported with a named violation and the pre-abort reference so the
/// caller can decline the destructive action and surface recovery hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifiedRestoreOutcome {
    /// No savepoint exists for `op_id` in this repo. Nothing to do.
    NoSavepoint,
    /// The current tip equals the savepoint — the op never moved this
    /// repo. Restore is a no-op; HEAD was not touched.
    Untouched,
    /// The current tip equals the recorded converged tip for this repo
    /// (from the owner record). The op moved the repo to convergence
    /// before crashing; the repo was reset back to the savepoint.
    RestoredFromConverged,
    /// The current tip equals the op's recorded intent tip for this repo
    /// (from the owner record's `advanced_tips` map). The op advanced
    /// the repo during replay before crashing; the repo was reset back
    /// to the savepoint. Exact-match only — no heuristic.
    RestoredFromIntent,
    /// The repo was in a VCS-native mid-op state (rebase/merge/cherry-
    /// pick). The mid-op was cancelled and the repo was reset back to
    /// the savepoint.
    RestoredFromMidOp,
    /// The repo's tip is not attributable to the op — it is neither the
    /// savepoint, nor the recorded converged tip, nor a mid-op state.
    /// Restore was REFUSED. The caller is expected to report the
    /// violation and continue with other repos; do not silently reset.
    ForeignTip {
        /// The repo's current tip at the moment of classification (as a
        /// canonical SHA string).
        observed_tip: String,
        /// The savepoint the op recorded at start (as a canonical SHA
        /// string).
        savepoint: String,
        /// The converged tip recorded for this repo by the owner record,
        /// if any. `None` when the op crashed before relock recorded any
        /// converged tips, or when no entry exists for this repo.
        recorded_converged_tip: Option<String>,
        /// The pre-abort reference that already captured `observed_tip`
        /// (written by [`Vcs::create_pre_abort_ref`] before this call).
        /// Surfaced in the refusal so operators can locate the tip if it
        /// is later determined safe to keep.
        pre_abort_ref: PreAbortRef,
    },
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

    /// Push the currently-checked-out branch in `repo` to the remote
    /// associated with `role`.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): resolves the current branch via
    /// [`current_ref`] (returning [`VcsError::CommandFailed`] with a
    /// "detached HEAD" stderr when there is no current branch — the
    /// [`crate::push`] caller is expected to refuse detached HEAD before
    /// reaching this point), then runs `git push <remote> <branch>` from
    /// the repo dir. The remote is selected by the same role convention as
    /// [`clone_with_role`] — `upstream` for `Role::Fork`, `origin`
    /// otherwise. Other VCS impls choose their own conventions.
    ///
    /// Trait-level Fork policy is neutral: `push_with_role(Role::Fork)`
    /// will push to `upstream` if that is what the role convention selects.
    /// Caller-side policy ("skip forks with an info line") lives in
    /// [`crate::push`]; the trait stays a thin shell over the VCS surface.
    ///
    /// When `force` is `true`, the push uses force semantics (for git,
    /// `--force`); when `false`, the push refuses non-fast-forward updates.
    ///
    /// [`current_ref`]: Vcs::current_ref
    /// [`clone_with_role`]: Vcs::clone_with_role
    fn push_with_role(&self, repo: &Path, role: Role, force: bool) -> Result<(), VcsError>;

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

    /// Rebase commits in the range `upstream..` of `repo`'s current branch
    /// onto `onto`.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs `git rebase --onto <onto>
    /// <upstream>`. On conflict, leaves the repo in the VCS-native in-flight
    /// state (for git: mid-rebase, with conflict markers in the working tree
    /// and `.git/rebase-merge/`) and returns
    /// [`VcsError::RebaseConflict { repo, op: ConflictOp::Rebase }`] so the
    /// caller can pair with [`Vcs::conflict_resolution_hint`] to assemble the
    /// user-facing resolution text.
    ///
    /// Lock-file exclusion happens via [`set_replay_exclusion`] — set it once
    /// on the repo (e.g. at `rwv init` time) and every rebase silently keeps
    /// the rebase target's version of the configured path. This is git's
    /// built-in `merge=ours` driver wired through `.gitattributes`; the trait
    /// hides the spelling so other VCS impls can use their own mechanism.
    ///
    /// [`set_replay_exclusion`]: Vcs::set_replay_exclusion
    fn rebase(
        &self,
        repo: &Path,
        onto: &ResolvedRevisionId,
        upstream: &ResolvedRevisionId,
    ) -> Result<(), VcsError>;

    /// Configure `repo` so that during replay (rebase, merge) any changes to
    /// `path` are silently overridden — the replay target's version of `path`
    /// always wins.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): appends a `<path> merge=ours` line
    /// to `<repo>/.gitattributes` (idempotent — re-running is a no-op if the
    /// line is already present). Other VCS impls choose their own mechanism.
    ///
    /// Used by rwv to keep `rwv.lock` out of the merge inputs during sync's
    /// project-repo rebase: the lock is regenerated from manifest tips in
    /// Phase 3, so carrying user lock-edits through a rebase would only
    /// produce noise. Configuring this once (in `rwv init`) replaces the
    /// custom cherry-pick loop that previously did per-commit exclusion.
    fn set_replay_exclusion(&self, repo: &Path, path: &Path) -> Result<(), VcsError>;

    /// `true` when [`set_replay_exclusion`] has been configured for `path` in
    /// `repo`.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): true iff `<repo>/.gitattributes`
    /// contains a `<path> merge=ours` line.
    ///
    /// Used by `rwv doctor` to detect projects initialised before the
    /// replay-exclusion path landed and offer to add the missing entry.
    ///
    /// [`set_replay_exclusion`]: Vcs::set_replay_exclusion
    fn has_replay_exclusion(&self, repo: &Path, path: &Path) -> Result<bool, VcsError>;

    /// `true` when [`set_replay_exclusion`] has been configured for `path` in
    /// `repo`'s **committed** tree (not just the working tree).
    ///
    /// Different from [`has_replay_exclusion`]: that one reads the on-disk
    /// `.gitattributes`; this one reads the committed-at-HEAD copy. The
    /// committed form is the one that survives a rebase (the replay starts
    /// from the committed tree, not the working tree), so sync's precondition
    /// check ("can rebase/merge rely on `merge=ours` to keep `rwv.lock`
    /// out of the merge inputs?") must consult the committed form.
    ///
    /// Returns `false` when the file isn't committed yet.
    ///
    /// [`set_replay_exclusion`]: Vcs::set_replay_exclusion
    /// [`has_replay_exclusion`]: Vcs::has_replay_exclusion
    fn has_committed_replay_exclusion(&self, repo: &Path, path: &Path) -> Result<bool, VcsError>;

    /// Fast-forward `repo`'s current branch to `to`, refusing rather than
    /// clobbering if a fast-forward isn't possible.
    ///
    /// Safe-by-construction: when `to` is not a descendant of the current
    /// HEAD, the operation fails with [`VcsError::CommandFailed`] (the VCS
    /// is asked to do an FF-only advance and refuses). The working tree and
    /// branch ref are touched only on success. No conflict markers are ever
    /// left in the working tree.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs `git merge --ff-only <to>`.
    /// Holds the safe-by-construction property established by the
    /// fo-5cqa74 fix (no `reset --hard` replacement that could discard
    /// reachable history).
    fn advance_if_fast_forward(&self, repo: &Path, to: &ResolvedRevisionId)
        -> Result<(), VcsError>;

    /// Hard-reset `repo`'s current branch to `to`, discarding any divergent
    /// commits and overwriting the working tree.
    ///
    /// Destructive — discarded commits are not recoverable through this VCS
    /// call alone. Used only by `rwv sync --discard-local-commits` after a
    /// Phase 1 ancestor check would have refused, with a savepoint already
    /// in place under [`refs/rwv/pre-op/<op-id>`] for `rwv abort` to roll
    /// back to.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs `git reset --hard <to>`.
    ///
    /// [`refs/rwv/pre-op/<op-id>`]: Vcs::create_savepoint
    fn hard_reset(&self, repo: &Path, to: &ResolvedRevisionId) -> Result<(), VcsError>;

    /// Return `true` when `ancestor` is an ancestor of `descendant` in
    /// `repo`. A revision counts as its own ancestor, so equal revisions
    /// return `true` (non-strict, matching `git merge-base --is-ancestor`).
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs
    /// `git merge-base --is-ancestor <ancestor> <descendant>`.
    fn is_ancestor(
        &self,
        repo: &Path,
        ancestor: &ResolvedRevisionId,
        descendant: &ResolvedRevisionId,
    ) -> Result<bool, VcsError>;

    /// Count commits reachable from `to` but not from `from` in `repo`
    /// (i.e. the size of the `from..to` range).
    ///
    /// Returns 0 when `to` is an ancestor of (or equal to) `from`.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs
    /// `git rev-list --count <from>..<to>`. The bounded count is what
    /// callers like `rwv sync`'s `AlreadyAhead` reporting actually need —
    /// no commit list is materialised.
    fn count_commits_in_range(
        &self,
        repo: &Path,
        from: &ResolvedRevisionId,
        to: &ResolvedRevisionId,
    ) -> Result<usize, VcsError>;

    /// Create a savepoint capturing the current `HEAD` of `repo` under an
    /// op-id-namespaced ref.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): writes
    /// `refs/rwv/pre-op/<op_id>` pointing at `HEAD`. The ref namespace is an
    /// impl detail — callers pass the opaque `op_id` and never spell the
    /// ref directly.
    ///
    /// Returns the captured `HEAD` revision so the caller can record it
    /// in op-state.
    fn create_savepoint(&self, repo: &Path, op_id: &str) -> Result<ResolvedRevisionId, VcsError>;

    /// Look up the savepoint captured under `op_id` in `repo`, returning
    /// the SHA it points at, or `None` when no such savepoint exists.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): reads `refs/rwv/pre-op/<op_id>`.
    fn resolve_savepoint(&self, repo: &Path, op_id: &str) -> Option<ResolvedRevisionId>;

    /// Restore `repo` to the savepoint captured under `op_id`, then drop
    /// the savepoint.
    ///
    /// Returns `Ok(true)` when a savepoint existed and was restored;
    /// `Ok(false)` when no savepoint was present (nothing to do).
    ///
    /// For [`GitVcs`](crate::git::GitVcs): when the savepoint exists, runs
    /// `git reset --hard refs/rwv/pre-op/<op_id>` followed by
    /// `git update-ref -d refs/rwv/pre-op/<op_id>`. The destructive
    /// `reset --hard` is the operation's contract — restoring the
    /// pre-op state is what `rwv abort` consents to.
    fn restore_savepoint(&self, repo: &Path, op_id: &str) -> Result<bool, VcsError>;

    /// Drop the savepoint captured under `op_id` in `repo`. No-op when
    /// no such savepoint exists; ignores ref-update failures (the
    /// savepoint is purely a recovery aid — its absence is benign).
    fn drop_savepoint(&self, repo: &Path, op_id: &str);

    /// Capture `repo`'s current tip in a durable pre-abort reference
    /// keyed by `op_id`, returning the captured tip and the operator-
    /// spellable label of the reference.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): writes
    /// `refs/rwv/pre-abort/<op_id>` pointing at `HEAD`. The ref namespace
    /// is a VCS impl detail — callers receive the [`PreAbortRef`] back
    /// with `label` containing the spellable name for refusal messages.
    ///
    /// **Information-preserving contract:** `rwv abort` calls this for
    /// EVERY repo it is about to restore, BEFORE any restore is attempted.
    /// The reference is NEVER deleted by abort's cleanup — abort is
    /// itself undoable, and the pre-abort ref is the cheapest recovery
    /// path. (`rwv doctor` may garbage-collect these refs once their op
    /// is provably no longer referenced; abort itself does not.)
    ///
    /// **First write wins:** if a pre-abort reference already exists for
    /// this `op_id`, it is returned unchanged rather than overwritten — a
    /// re-run of abort for the same op must not clobber the original
    /// capture, which may by then be the only reference to that tip.
    fn create_pre_abort_ref(&self, repo: &Path, op_id: &str) -> Result<PreAbortRef, VcsError>;

    /// Resolve the pre-abort reference captured under `op_id` in `repo`,
    /// returning `None` when no such reference exists.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): reads
    /// `refs/rwv/pre-abort/<op_id>`.
    ///
    /// Used by tests and recovery tooling to verify that abort wrote the
    /// pre-abort ref and to locate the captured tip.
    fn resolve_pre_abort_ref(&self, repo: &Path, op_id: &str) -> Option<PreAbortRef>;

    /// HEAD-verified restore: classify the repo's current tip and restore
    /// to the savepoint ONLY when the tip is attributable to the op.
    ///
    /// Classification (evaluated in order):
    /// - no savepoint exists for `op_id` → [`VerifiedRestoreOutcome::NoSavepoint`].
    /// - repo is in a VCS-native mid-op state (rebase/merge/cherry-pick)
    ///   → mid-op is cancelled and restore proceeds, returning
    ///   [`VerifiedRestoreOutcome::RestoredFromMidOp`].
    /// - `tip == savepoint` → [`VerifiedRestoreOutcome::Untouched`],
    ///   restore is a no-op (HEAD is not touched).
    /// - `tip ==` `recorded_intent_tip` → the op advanced this repo during
    ///   replay before crashing; restore proceeds and returns
    ///   [`VerifiedRestoreOutcome::RestoredFromIntent`]. Exact-match only.
    /// - `tip ==` `recorded_converged_tip` → the op moved this repo to
    ///   convergence; restore proceeds and returns
    ///   [`VerifiedRestoreOutcome::RestoredFromConverged`].
    /// - **anything else** → [`VerifiedRestoreOutcome::ForeignTip`] is
    ///   returned without touching HEAD. The caller is responsible for
    ///   surfacing the named violation and continuing with other repos.
    ///   The destructive primitive is fenced behind this enumerable set
    ///   of attributable states.
    ///
    /// `recorded_intent_tip` is the SHA the owner record's `advanced_tips`
    /// map holds for this repo (written at replay entry), or `None` when no
    /// entry exists (op predates the field, or the op had not yet reached
    /// replay). `recorded_converged_tip` is the SHA the owner record's
    /// `converged_tips` map holds for this repo (relock-completed), or
    /// `None` when the op crashed before relock recorded any converged tips.
    /// The caller derives the key (e.g. the repo's path relative to the
    /// workspace root, or `"(project)"` for the project repo).
    ///
    /// For [`GitVcs`](crate::git::GitVcs): when restoring, runs
    /// `git reset --hard refs/rwv/pre-op/<op_id>` and drops the savepoint
    /// — the same primitive as [`restore_savepoint`], gated by the
    /// classification above. The destructive `reset --hard` is now
    /// reachable only for tips the op itself created.
    ///
    /// [`restore_savepoint`]: Vcs::restore_savepoint
    fn verified_restore_savepoint(
        &self,
        repo: &Path,
        op_id: &str,
        recorded_intent_tip: Option<&str>,
        recorded_converged_tip: Option<&str>,
    ) -> Result<VerifiedRestoreOutcome, VcsError>;

    /// Return the in-flight VCS operation `repo` is currently mid-way
    /// through, if any.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): detects mid-rebase
    /// (`rebase-merge/` or `rebase-apply/` present), mid-merge
    /// (`MERGE_HEAD` present), or mid-cherry-pick (`CHERRY_PICK_HEAD`
    /// present). Returns `None` when the repo is in a clean (non-in-flight)
    /// state.
    fn mid_op(&self, repo: &Path) -> Option<ConflictOp>;

    /// Cancel any in-flight VCS operation in `repo` (rebase, merge,
    /// cherry-pick). No-op when the repo is in a clean state.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs
    /// `git rebase --abort` / `git merge --abort` / `git cherry-pick --abort`
    /// depending on what [`mid_op`] reports. Errors from the underlying
    /// abort command are swallowed — the call is a best-effort cleanup
    /// before the surrounding flow (e.g. `rwv abort`) does its own
    /// recovery.
    ///
    /// [`mid_op`]: Vcs::mid_op
    fn cancel_in_flight_op(&self, repo: &Path);

    /// Return `true` when `branch` in `repo` has a counterpart on the
    /// role-conventional remote (e.g. `refs/remotes/origin/<branch>` for
    /// `Role::Primary` in [`GitVcs`](crate::git::GitVcs)).
    ///
    /// Used by `prune_dropped_repo` to refuse pruning a clone that has
    /// local-only branches: a branch with no remote counterpart is
    /// conservatively assumed to carry unique work.
    fn branch_has_remote_counterpart(
        &self,
        repo: &Path,
        branch: &RefName,
        role: Role,
    ) -> Result<bool, VcsError>;

    /// Count commits reachable from `branch` but not from its
    /// role-conventional remote counterpart in `repo`.
    ///
    /// Returns 0 when the branch is fully merged into its counterpart, and
    /// `>0` when it carries unique local commits. Caller is responsible
    /// for verifying the counterpart exists first via
    /// [`branch_has_remote_counterpart`]; this method may error when it
    /// does not.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs
    /// `git rev-list --count refs/remotes/<remote>/<branch>..<branch>`.
    ///
    /// [`branch_has_remote_counterpart`]: Vcs::branch_has_remote_counterpart
    fn count_commits_ahead_of_remote(
        &self,
        repo: &Path,
        branch: &RefName,
        role: Role,
    ) -> Result<usize, VcsError>;

    /// List every local branch in `repo`.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): enumerates
    /// `refs/heads/` via `git for-each-ref`. Differs from
    /// [`list_branches_with_prefix`] in that it returns every branch
    /// regardless of name.
    ///
    /// [`list_branches_with_prefix`]: Vcs::list_branches_with_prefix
    fn list_local_branches(&self, repo: &Path) -> Result<Vec<RefName>, VcsError>;

    /// Fetch objects from `src_repo` into `dst_repo` so SHAs reachable in
    /// `src_repo` are reachable in `dst_repo`.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs `git fetch <src_path> HEAD`
    /// in `dst_repo`. For sibling worktrees that share an object store this
    /// is a no-op; for independent clones it copies objects across. Errors
    /// are swallowed — the caller (e.g. `sync-to` step 3) is expected to
    /// verify reachability by a subsequent operation that will fail loudly
    /// if the fetch was needed but didn't run.
    fn fetch_objects_from(&self, dst_repo: &Path, src_repo: &Path);

    /// Refresh `repo`'s index to match `HEAD` when the divergence is the
    /// auto-fixable class — every drifted tree must already be a committed
    /// tree reachable from `HEAD`. No-op when the index already matches
    /// `HEAD` or when any divergent tree is unverifiable.
    ///
    /// Safety invariant: never replaces index content that is not already
    /// a committed tree reachable from `HEAD`. Live staged content is left
    /// untouched.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs `git reset` (mixed) after
    /// verifying via `git write-tree` + `git log` that the index tree is
    /// among the last 200 commits' trees. Infallible — failures along the
    /// way silently leave the index alone.
    fn refresh_index_to_head_if_safe(&self, repo: &Path);

    /// Restore `repo`'s working tree to match `HEAD` when the divergence
    /// is the auto-fixable class — every drifted file's on-disk blob must
    /// already be reachable from `HEAD`. No-op when the working tree
    /// already matches `HEAD` or when any divergent file is unverifiable.
    ///
    /// Safety invariant: never replaces on-disk content that is not already
    /// a committed blob reachable from `HEAD`. Live edits are left
    /// untouched.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs `git checkout HEAD --
    /// <files>` after verifying each modified file's blob is reachable
    /// via `git rev-list --objects` of the last 200 commits. Infallible —
    /// failures along the way silently leave the working tree alone.
    fn refresh_working_tree_to_head_if_safe(&self, repo: &Path);

    /// Return the fetch URL of a named remote in `repo`, or `None` when
    /// that remote does not exist.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs
    /// `git remote get-url <remote>`. Returns `None` when the remote is
    /// absent (git exits non-zero with "No such remote"). Returns
    /// `Some(url)` with the URL string as git reports it. Other errors
    /// (I/O failures, non-UTF-8 output) propagate as [`VcsError`].
    ///
    /// Used by `rwv doctor`'s provenance checks to compare the clone's
    /// `origin` URL against the manifest URL.
    fn remote_url(&self, repo: &Path, remote: &str) -> Result<Option<String>, VcsError>;

    /// Return `true` when `sha` names a commit object that exists in
    /// `repo`'s object store.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs
    /// `git cat-file -e <sha>^{commit}`. Exit 0 means the object exists
    /// and is a commit; any other exit (including the case where `sha`
    /// resolves to a non-commit object type) returns `false`.
    ///
    /// Used by `rwv doctor`'s provenance checks to detect lock SHAs that
    /// are absent from the local clone — e.g. after an upstream
    /// force-push removed the commit from reachable history, or after a
    /// fresh clone that has never fetched a now-detached SHA.
    fn commit_object_exists(&self, repo: &Path, sha: &str) -> Result<bool, VcsError>;

    /// Resolve the absolute path of the canonical object/refs store backing
    /// the workspace at `workspace`.
    ///
    /// The returned path is the location of the object DAG and refs that
    /// commits made in `workspace` will end up in. Two workspaces share an
    /// object DAG iff this method returns the same path for both. This is
    /// the primitive `rwv doctor`'s clone-topology check uses to enforce
    /// the I1 (single canonical store) and I2 (workweave checkouts are
    /// linked into the canonical store) invariants from the
    /// [clone-topology](../../docs/explanation/joints/clone-topology.md)
    /// joint.
    ///
    /// Returns `None` when `workspace` is not a VCS workspace at all
    /// (e.g. the path does not exist, or the directory is not a repo).
    ///
    /// Semantics on key inputs:
    /// - **Full clone at `workspace`.** Returns `workspace/.git` (or
    ///   wherever the VCS keeps its store under the workspace root).
    /// - **Linked worktree at `workspace`.** Returns the path of the
    ///   shared store the worktree was created against — not anything
    ///   under `workspace`.
    /// - **Path does not exist or has no VCS metadata.** Returns `None`.
    ///   The caller distinguishes "not a workspace" from "a workspace
    ///   linked elsewhere" by inspecting the returned path itself.
    ///
    /// **The returned path is NOT canonicalized** (symlinks and `..`
    /// components are not resolved). The path is absolute, but two stores
    /// whose byte-identical strings differ only in symlinks or trailing
    /// slashes will compare unequal. Call `.canonicalize()` on both sides
    /// before equality when that matters — see `workweave.rs` call sites
    /// that compare against `checkout.canonicalize()`.
    ///
    /// Callers that need the canonical clone *directory* (for operations
    /// like `git worktree remove` that run in the repo root, not the
    /// `.git/` subdir) should call `.parent()` on the returned path.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs `git rev-parse
    /// --path-format=absolute --git-common-dir` in `workspace`.
    fn resolve_canonical_store(&self, workspace: &Path) -> Option<PathBuf>;
    /// List paths registered as worktrees of `repo` whose on-disk
    /// directories no longer exist. The administrative entries are still
    /// in the VCS state and will be dropped by [`worktree_prune`].
    ///
    /// For [`GitVcs`](crate::git::GitVcs): parses
    /// `git worktree list --porcelain` and returns the `worktree` path
    /// from every record marked `prunable`. Returns paths verbatim from
    /// the porcelain output; the caller is responsible for any further
    /// resolution.
    ///
    /// Used by `rwv doctor` to surface stale worktree registrations as a
    /// state-hygiene finding; `--fix` calls [`worktree_prune`] to drop
    /// the registrations (information-preserving by construction — the
    /// only state being removed is a pointer to a directory that already
    /// no longer exists).
    ///
    /// [`worktree_prune`]: Vcs::worktree_prune
    fn list_stale_worktree_registrations(&self, repo: &Path) -> Result<Vec<PathBuf>, VcsError>;

    /// List the opaque `op_id` strings of every savepoint currently
    /// recorded in `repo`.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): enumerates
    /// `refs/rwv/pre-op/*` via `git for-each-ref` and returns the
    /// trailing path component (the `op_id`) of each ref. The ref
    /// namespace (`refs/rwv/pre-op/<id>`) is an impl detail — callers
    /// receive opaque op-id strings and feed them back through
    /// [`resolve_savepoint`] / [`drop_savepoint`].
    ///
    /// Used by `rwv doctor`'s orphaned-savepoint check to find
    /// savepoints that no longer correspond to a live `.rwv-op` file.
    ///
    /// [`resolve_savepoint`]: Vcs::resolve_savepoint
    /// [`drop_savepoint`]: Vcs::drop_savepoint
    fn list_savepoint_op_ids(&self, repo: &Path) -> Result<Vec<String>, VcsError>;

    /// Read the content of `file_path` at `revision` in `repo`.
    ///
    /// Returns the raw byte content of the file as committed at that
    /// revision. Useful for reading files (e.g. manifests, lock files)
    /// at a pinned commit without touching the working tree — the
    /// snapshot-reads primitive (§6 of the sync design).
    ///
    /// `file_path` is a path relative to the repo root (e.g.
    /// `Path::new("rwv.lock")` or `Path::new("rwv.yaml")`).
    ///
    /// Returns [`VcsError::RevisionNotFound`] when `revision` is not
    /// reachable in `repo`, and also when the file does not exist at that
    /// revision (the error's `rev` field carries the full
    /// `<revision>:<path>` spec, naming the absent path).
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs
    /// `git show <revision>:<file_path>` in `repo`.
    fn read_file_at_revision(
        &self,
        repo: &Path,
        revision: &ResolvedRevisionId,
        file_path: &Path,
    ) -> Result<String, VcsError>;
}
