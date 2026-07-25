//! Version control system abstraction.
//!
//! repoweave operates on repos and worktrees. The VCS layer abstracts over
//! the specific tool (git, jj, sl, hg) so core logic doesn't hardcode git.

use crate::cli::consent::{
    AdoptDetachedConsent, DetachConsent, DiscardUnmergedConsent, ReattachConsent,
};
use crate::manifest::{ProjectName, Role, WorkweaveName};
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
/// against a real repo), [`ResolvedRevisionId::from_canonical`] (mint with
/// a known SHA, e.g. directly from `head_revision` output), and
/// [`ResolvedRevisionId::from_rev_parse_output`] (mint from raw
/// ref-resolution output, verifying the canonical form). There is no
/// public way to mint a `ResolvedRevisionId` from a free string — the
/// parse boundary lives in [`RawRevisionId`].
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

    /// Mint from the raw output of a ref-resolution command, **verifying**
    /// the canonical form this type exists to guarantee.
    ///
    /// Returns `None` when `s` is not a canonical commit id, so a value
    /// obtained this way is canonical by construction rather than by
    /// assertion. Callers resolving a fully-qualified ref (a savepoint, a
    /// pre-abort reference) use this instead of re-running the value
    /// through [`Vcs::resolve_revision`]: it costs no extra VCS
    /// invocation and, unlike the assertion it replaces, it actually runs
    /// the check.
    ///
    /// No display form is preserved: the input is a ref name that resolved
    /// to this commit, not a name that identifies the commit anywhere else
    /// (`refs/rwv/pre-op/<op-id>` is meaningless outside the repo that
    /// holds it), so serializing it would produce an unresolvable scalar.
    pub fn from_rev_parse_output(s: &str) -> Option<Self> {
        let s = s.trim();
        is_canonical_commit_id(s).then(|| Self {
            canonical: s.to_owned(),
            display: None,
        })
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

/// True for a canonical commit id: a lowercase-hex object name of the
/// full width the repository's hash function produces (40 for SHA-1, 64
/// for SHA-256 repos — `extensions.objectFormat = sha256`).
///
/// Abbreviated ids are deliberately rejected: they are ambiguous by
/// construction, and the whole point of the canonical form is that two
/// values naming the same commit compare equal. Uppercase is rejected
/// because no VCS emits it for a resolved object name, so its appearance
/// means the string came from somewhere other than a resolution.
fn is_canonical_commit_id(s: &str) -> bool {
    matches!(s.len(), 40 | 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
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
/// let _ = raw == resolved; // E0308: expected RawRevisionId, found ResolvedRevisionId
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

/// One commit in a [`Vcs::unique_commits`] listing — a VCS-agnostic
/// one-line summary.
///
/// `id` is the full, stable commit identifier (for git, the 40-hex SHA);
/// `short` is the abbreviated form a human reads; `subject` is the first
/// line of the commit message. The type carries no git-specific spelling so
/// callers (e.g. `rwv workweave log`) render the same shape regardless of
/// the underlying VCS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CommitSummary {
    /// Full, stable commit identifier (for git, the 40-hex SHA).
    pub id: String,
    /// Abbreviated commit identifier for display.
    pub short: String,
    /// First line of the commit message.
    pub subject: String,
}

/// The result of [`Vcs::unique_diff`] — the unified diff of a workweave's
/// unique work vs its parent, anchored at their common ancestor.
///
/// `base` is the common-ancestor revision the diff is anchored at (the
/// point the workweave forked from), returned so callers can display the
/// anchor; it is `None` when no anchor could be computed. `text` is the
/// unified-diff body. Anchoring at the common ancestor — not the parent tip
/// directly — is what keeps a parent that advanced after the fork from
/// showing phantom reversals of work the parent gained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct UniqueDiff {
    /// The common-ancestor revision the diff is anchored at, if computable.
    pub base: Option<String>,
    /// The unified-diff text of the workweave's unique work vs `base`.
    pub text: String,
}

/// In-flight VCS operation whose conflict needs human resolution.
///
/// Passed to [`Vcs::conflict_resolution_hint`] so sync's conflict-bail
/// messages embed VCS-appropriate "how do I resume this?" text without
/// hardcoding git vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictOp {
    /// Native rebase (`git rebase`).
    ///
    /// The operator-facing resume path is `rwv sync --continue` /
    /// `rwv sync-to --continue` — not bare `git rebase --continue`. The VCS
    /// hint for this variant stops at staging (`git add <files>`); rwv core
    /// appends the `rwv <verb> --continue` line. Bare `git rebase --continue`
    /// remains a safe fallback (the durable `merge.rwv-ours.driver` config
    /// plant carries the exclusion), but it is not the primary operator path.
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
    /// A ref witness no longer describes the repo it was taken from.
    ///
    /// An [`AttachedRef`] / [`DetachedHead`] proves what HEAD was at the
    /// moment it was produced; the repo can move underneath it. Every
    /// consumer re-observes before acting and returns this rather than
    /// applying the operation to a state nobody authorized. (How wide a
    /// witness's validity window *should* be is Q15, still open — this
    /// variant is what makes the narrow answer observable.)
    StaleRefWitness {
        repo: PathBuf,
        /// What the witness asserted, rendered.
        expected: String,
        /// What the repo actually reports now, rendered.
        observed: String,
    },
    /// The repo is mid-operation and the requested ref write would yank
    /// operator state out from under it.
    ///
    /// A MOVE of an already-detached HEAD refuses here (§3.6): "rwv
    /// detached this HEAD at a lock SHA" and "the operator is mid-bisect /
    /// stopped at a `rebase -i` edit" are different situations, and only
    /// the first is rwv's to move.
    MidOperation {
        repo: PathBuf,
        /// Short label naming the in-flight operation.
        operation: String,
    },
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
            Self::StaleRefWitness { .. } => "stale-ref-witness",
            Self::MidOperation { .. } => "mid-operation",
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
    StaleRefWitness {
        repo: PathBuf,
        expected: String,
        observed: String,
    },
    MidOperation {
        repo: PathBuf,
        operation: String,
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
            VcsError::StaleRefWitness {
                repo,
                expected,
                observed,
            } => Self::StaleRefWitness {
                repo: repo.clone(),
                expected: expected.clone(),
                observed: observed.clone(),
            },
            VcsError::MidOperation { repo, operation } => Self::MidOperation {
                repo: repo.clone(),
                operation: operation.clone(),
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
            Self::StaleRefWitness {
                repo,
                expected,
                observed,
            } => write!(
                f,
                "{} moved since it was observed: expected {expected}, found {observed}",
                repo.display()
            ),
            Self::MidOperation { repo, operation } => write!(
                f,
                "{} is {operation}; finish or cancel it before rwv moves HEAD",
                repo.display()
            ),
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

// ===========================================================================
// The branch model (branch-model.md §4)
// ===========================================================================
//
// `RefName` above is one type standing in for four different notions: the
// branch a manifest entry DECLARES it tracks, the ephemeral name a create
// REQUESTS, the ref rwv holds a RECEIPT for, and the ref a checkout is
// actually ON. Comparing any two of them is a legal line that is usually
// wrong, and the tree contains such lines today.
//
// The split below is the same move `ResolvedRevisionId` / `RawRevisionId`
// already made in this file: one parse boundary, one refined value per
// notion, no cross-type comparison, and a `compile_fail` doctest per
// invariant so a later "make it easier" `PartialEq`/`From` impl fails CI
// rather than passing review. See §4.1 for the precedent and §4.2 for the
// table these types implement.
//
// `RefName` and the trait methods that take it are NOT removed here: the
// old and new surfaces run side by side until every call site has been
// restated in terms of the new one.

/// A ref name as observed or as written: the parse boundary of the branch
/// model.
///
/// `RawRefName` is to ref names what [`RawRevisionId`] is to revisions. It
/// wraps a string verbatim and, at the type level, we do not know whether
/// it names a branch a manifest declared, a branch rwv minted, a branch a
/// checkout is on, or a branch a human made by hand. Everything that
/// enters the model from outside — a manifest scalar, a porcelain listing,
/// a flag argument — arrives as one of these, and the named conversions in
/// this section are the only ways out.
///
/// It deliberately keeps [`RawRefName::as_str`]: raw VCS output has to stay
/// inspectable. The types that must *not* be inspected as strings
/// ([`TrackingRef`], [`OwnedRef`], [`AttachedRef`]) are exactly the ones
/// carrying a claim that a string comparison would silently discard.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RawRefName(String);

impl RawRefName {
    /// Construct from any string. Public because deserialization, VCS
    /// listings, and tests all need to mint raw values; the string is
    /// treated as opaque.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RawRefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a [`RawRefName`] could not be parsed into a [`TrackingRef`].
///
/// Each variant is a distinct rejection rule so callers can report which
/// one fired instead of re-deriving it from message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefNameError {
    /// The name is empty.
    Empty,
    /// The name is commit-id shaped. `version:` declares what to TRACK;
    /// the lock records where you ARE (§8.8) — a pin needs a different
    /// field, not an overloaded one.
    ShaShaped(String),
    /// The name is release-tag shaped. Same reason as [`Self::ShaShaped`]:
    /// a tag is a pin, and a tracking declaration cannot be one.
    TagShaped(String),
    /// The name is not usable as a ref name at all.
    Malformed {
        /// The rejected name.
        name: String,
        /// Which rule it broke, as a short noun phrase.
        reason: &'static str,
    },
}

impl fmt::Display for RefNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("ref name is empty"),
            Self::ShaShaped(s) => write!(
                f,
                "'{s}' is commit-id shaped; `version:` declares a branch to \
                 track, not a revision to pin"
            ),
            Self::TagShaped(s) => write!(
                f,
                "'{s}' is tag shaped; `version:` declares a branch to track, \
                 not a revision to pin"
            ),
            Self::Malformed { name, reason } => {
                write!(f, "'{name}' is not a valid ref name: {reason}")
            }
        }
    }
}

impl std::error::Error for RefNameError {}

/// Width at which an all-hex name stops reading as a branch name and
/// starts reading as an abbreviated commit id.
///
/// git's default `core.abbrev` floor is 7, so a 7-hex `version:` is
/// overwhelmingly a pin someone tried to smuggle through the tracking
/// field. Below that, hex-looking names (`cafe`, `beef`, `dad`) are
/// ordinary words and are accepted.
const SHA_SHAPED_MIN_LEN: usize = 7;

/// True when `s` reads as a commit id rather than a branch name.
fn is_sha_shaped(s: &str) -> bool {
    s.len() >= SHA_SHAPED_MIN_LEN && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// True for release-shape names (e.g. `v1.2.3`, `v0.3.4-rc1`).
///
/// Two callers, deliberately sharing one definition: [`TrackingRef::parse`]
/// rejects names of this shape (§8.8 — `version:` is a declaration, not a
/// pin), and [`crate::git::GitVcs::tag_at_head`] uses it as a tiebreaker so
/// a release tag wins over an arbitrary lightweight tag. Both are asking
/// "does this read as a release tag"; one answer.
pub(crate) fn is_release_shape_name(s: &str) -> bool {
    let rest = match s.strip_prefix('v') {
        Some(r) => r,
        None => return false,
    };
    // Require at least "N.N" (e.g., "1.0") to count as release-shape.
    let mut parts = rest.split(['.', '-', '+']);
    let first = parts.next().unwrap_or("");
    let second = parts.next().unwrap_or("");
    !first.is_empty()
        && first.chars().all(|c| c.is_ascii_digit())
        && !second.is_empty()
        && second.chars().next().is_some_and(|c| c.is_ascii_digit())
}

/// Reject strings that cannot name a ref.
///
/// These rules are the conservative intersection of what VCSes accept as a
/// ref name; git's `check-ref-format` is the strictest of the ones rwv
/// targets and is what this mirrors. Validating at the seam rather than in
/// the git impl means a manifest carrying `feat/../../etc` is refused once,
/// at parse time, instead of once per VCS.
fn validate_ref_name(s: &str) -> Result<(), RefNameError> {
    let malformed = |reason: &'static str| {
        Err(RefNameError::Malformed {
            name: s.to_owned(),
            reason,
        })
    };
    if s.is_empty() {
        return Err(RefNameError::Empty);
    }
    if s == "@" {
        return malformed("`@` alone is not a ref name");
    }
    if s.contains("..") {
        return malformed("contains `..`");
    }
    if s.contains("@{") {
        return malformed("contains `@{`");
    }
    if s.contains("//") {
        return malformed("contains an empty path component");
    }
    if s.starts_with('/') || s.ends_with('/') {
        return malformed("starts or ends with `/`");
    }
    if s.ends_with('.') {
        return malformed("ends with `.`");
    }
    if let Some(bad) = s
        .chars()
        .find(|c| c.is_ascii_control() || " ~^:?*[\\\u{7f}".contains(*c))
    {
        return match bad {
            ' ' => malformed("contains a space"),
            c if c.is_control() => malformed("contains a control character"),
            _ => malformed("contains one of `~^:?*[\\`"),
        };
    }
    for component in s.split('/') {
        if component.starts_with('.') {
            return malformed("has a path component starting with `.`");
        }
        if component.ends_with(".lock") {
            return malformed("has a path component ending in `.lock`");
        }
    }
    Ok(())
}

/// Notion (1): the branch a manifest entry **declares** it tracks
/// (`version:`).
///
/// A `TrackingRef` is a statement of intent about a *remote* channel. It is
/// not a claim that any local ref of that name exists, and it is not a
/// revision — [`TrackingRef::parse`] refuses commit-id-shaped and
/// tag-shaped input for exactly that reason (§8.8).
///
/// # Display only, deliberately
///
/// There is no `as_str()`. Both shipped comparison sites in `push.rs` are
/// written with `.as_str()` on *both* sides, so a `TrackingRef` carrying
/// one would let them compile verbatim after the split and the whole
/// exercise would report nothing. To compare a declaration against an
/// observation an author must first pick a projection —
/// [`TrackingRef::local_counterpart`] or [`TrackingRef::on_remote`] — and
/// the pick is the decision the comparison was hiding.
///
/// # Compile-time invariant
///
/// A `TrackingRef` cannot be compared against an [`AttachedRef`]: a
/// declared channel and an observed attachment are different notions, and
/// the shipped bugs came from treating them as one.
///
/// ```compile_fail
/// use repoweave::vcs::{AttachedRef, RawRefName, TrackingRef};
/// fn compare(attached: AttachedRef) {
///     let declared = TrackingRef::parse(RawRefName::new("main")).unwrap();
///     let _ = attached == declared; // E0308: expected AttachedRef, found TrackingRef
/// }
/// ```
///
/// Nor can the comparison be laundered back through the parse boundary.
/// The probe names **one** type: a two-sided `a.as_str() != b.as_str()`
/// emits an error per side, so it keeps failing after either type regains
/// the method and would pin neither.
///
/// ```compile_fail
/// use repoweave::vcs::{RawRefName, TrackingRef};
/// fn spell() -> String {
///     let declared = TrackingRef::parse(RawRefName::new("main")).unwrap();
///     declared.as_str().to_owned() // E0599: no such method
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackingRef(String);

impl TrackingRef {
    /// Parse a manifest-declared `version:` scalar.
    ///
    /// Rejects commit-id-shaped and release-tag-shaped values, and anything
    /// that is not a usable ref name. This is the *only* way to obtain a
    /// `TrackingRef`; there is no `Deserialize` impl, mirroring
    /// [`ResolvedRevisionId`]'s refusal to deserialize.
    pub fn parse(raw: RawRefName) -> Result<Self, RefNameError> {
        let s = raw.as_str();
        validate_ref_name(s)?;
        if is_sha_shaped(s) {
            return Err(RefNameError::ShaShaped(s.to_owned()));
        }
        if is_release_shape_name(s) {
            return Err(RefNameError::TagShaped(s.to_owned()));
        }
        Ok(Self(raw.0))
    }

    /// The remote branch this declaration names, under `role`.
    ///
    /// Which *remote* a role selects is a VCS convention (git today maps
    /// every role to `origin`), so the projection stops at the (role,
    /// branch) pair and the VCS impl resolves the remote name. That keeps
    /// `origin` out of the seam.
    pub fn on_remote(&self, role: Role) -> RemoteRef {
        RemoteRef {
            role,
            branch: self.0.clone(),
        }
    }

    /// "The local branch of the same name."
    ///
    /// **This is not an identity.** It is a projection across namespaces,
    /// and the assumption is stated here rather than at the dozens of sites
    /// that would otherwise make it silently: rwv assumes a local branch
    /// named the same as the declared tracking branch is that branch's
    /// local counterpart. Nothing verifies it — a local `main` may have
    /// been created from anywhere.
    pub fn local_counterpart(&self) -> LocalRefName {
        LocalRefName(self.0.clone())
    }
}

impl fmt::Display for TrackingRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A branch on a remote, named by role rather than by remote name.
///
/// Produced by [`TrackingRef::on_remote`]. The VCS impl maps `role` to its
/// own remote-naming convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef {
    role: Role,
    branch: String,
}

impl RemoteRef {
    /// The role whose remote convention selects the remote.
    pub fn role(&self) -> Role {
        self.role
    }

    /// The branch name on that remote.
    pub fn branch(&self) -> &str {
        &self.branch
    }
}

impl fmt::Display for RemoteRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} on the {} remote", self.branch, self.role.as_str())
    }
}

/// A local branch name arrived at through a *named* projection.
///
/// The only producers are [`TrackingRef::local_counterpart`] and
/// [`RemoteDefaultBranch::local_counterpart`] — each a function whose doc
/// comment is where the assumption behind the projection lives. Keeping
/// `as_str()` here is safe: a `LocalRefName` is already the output of a
/// stated assumption, so reading it as a string launders nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LocalRefName(String);

impl LocalRefName {
    /// Borrow the underlying string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LocalRefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Notion (2a): the ephemeral branch name a create **requests**.
///
/// [`EphemeralRefName::mint`] is total and takes exactly two inputs — a
/// project and a workweave. There is no third component: the three
/// derivation sites in the tree disagreed about what it should be, no
/// consumer read it, and §3.5 deletes it rather than picking a winner.
/// Nothing observed can be fed in, so a name can never be derived from the
/// branch a checkout happens to be on.
///
/// Requesting a name is not owning one. Only a persisted receipt makes a
/// ref rwv's to destroy ([`OwnedRef`], R2), which is why this type has no
/// path to one that does not go through the registry.
///
/// **Q12 stays open.** The legal grammar for project and workweave names is
/// undecided (a project `p` with workweave `x--y` and a project `p--x` with
/// workweave `y` mint the same name), so `mint` deliberately does not
/// validate its components. Under R2 that collision is a legibility
/// problem, not a correctness one — ownership comes from the receipt, not
/// from parsing the name back apart.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EphemeralRefName(String);

impl EphemeralRefName {
    /// Mint the ephemeral name for `project`'s `workweave`.
    ///
    /// Total: no third input, no failure, no read of the current ref.
    pub fn mint(project: &ProjectName, workweave: &WorkweaveName) -> Self {
        Self(format!("{}--{}", project.as_str(), workweave.as_str()))
    }

    /// The requested name at the parse boundary, for the receipt store to
    /// key on and for the VCS impl to spell.
    ///
    /// Deliberately not `as_str()`: the only legitimate question about a
    /// requested name is "does rwv hold a receipt for it", which is
    /// `RefRegistry::lookup`, not a string comparison. Handing back a
    /// [`RawRefName`] grants nothing `RawRefName::new` did not already
    /// grant — a raw name owns nothing and can destroy nothing.
    pub fn to_raw(&self) -> RawRefName {
        RawRefName(self.0.clone())
    }
}

impl fmt::Display for EphemeralRefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An observed ref that a **live** workweave's own minted namespace claims —
/// the only observed name `branch-model.md` §7.1's migration may adopt into
/// a receipt.
///
/// Before §3.5 flattened the scheme, rwv minted `{project}--{workweave}/<segment>`.
/// Those refs predate ownership receipts, so under R2 they are nobody's:
/// unownable, and therefore undeletable — including by the arm-1 rename,
/// which §3.4 spells out is a DESTROY of the old name and needs a receipt for
/// it. This type is the one route from an observation to that receipt, and it
/// is deliberately narrow.
///
/// # What [`claim`](Self::claim) asks, and what it does not
///
/// It takes the [`EphemeralRefName`] a live workweave **mints** and asks
/// whether the observed name sits under it. The observed name is never split
/// into parts and no part of it is handed on: the answer is this whole name
/// or nothing. So the migration cannot reconstruct which workweave a stray
/// `<a>--<b>/<c>` belonged to (§7.3) — it can only recognise a ref inside a
/// namespace it is standing in.
///
/// That is not "ownership by name shape". It authorizes exactly one thing: a
/// receipt whose immediate and only use is a rename that **preserves the
/// tip**. No commit can be lost through this type; a mistaken claim costs the
/// operator a branch name, not work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyEphemeralRefName(RawRefName);

impl LegacyEphemeralRefName {
    /// `Some` iff `observed` sits strictly under the namespace `flat` claims
    /// — i.e. it is `{flat}/<something non-empty>`.
    ///
    /// `flat` itself is **not** a legacy name and yields `None`: adopting the
    /// flat name is §7.1 arm 2, which goes through
    /// [`RefRegistry::record_created`] on the minted name and needs no
    /// observation at all.
    ///
    /// [`RefRegistry::record_created`]: crate::workweave_index::RefRegistry::record_created
    pub fn claim(flat: &EphemeralRefName, observed: &RawRefName) -> Option<Self> {
        let rest = observed.as_str().strip_prefix(&format!("{}/", flat.0))?;
        (!rest.is_empty()).then(|| Self(observed.clone()))
    }

    /// The claimed name at the parse boundary, for the receipt store to key
    /// on and for the VCS impl to spell.
    pub fn to_raw(&self) -> RawRefName {
        self.0.clone()
    }
}

impl fmt::Display for LegacyEphemeralRefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

/// Notion (2b): an ephemeral ref rwv holds a persisted **receipt** for.
///
/// R2: a ref is rwv's to delete iff rwv holds a receipt for that exact name
/// in that exact store. A ref that merely *looks* like rwv's is not rwv's,
/// which is why an [`EphemeralRefName`] cannot become one of these and a
/// [`RawRefName`] from a listing cannot either.
///
/// Carries the store it was recorded against, so [`Vcs::delete_owned_ref`]
/// and [`Vcs::create_worktree_on`] derive their target from the receipt
/// rather than from an independent path argument — the same
/// carry-your-provenance rule [`AttachedRef`] follows, for the same reason.
///
/// # Display only, deliberately
///
/// No `as_str()`. The question "is this the ref that checkout is on" is
/// [`OwnedRef::is_attached_by`], a named predicate that yields a `bool` and
/// no witness; the question "may I delete this" is a
/// [`DeletionWarrant`]. Neither is a string comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedRef {
    store: PathBuf,
    name: RawRefName,
    created_at: ResolvedRevisionId,
}

impl OwnedRef {
    /// Mint from a receipt the registry has already **persisted**.
    ///
    /// `pub(crate)` because the receipt store is the producer (§4.2:
    /// `RefRegistry::record_created` / `RefRegistry::lookup`), and receipts
    /// are written before the ref they describe so a crash leaves a
    /// dangling receipt (benign) rather than an unreceipted ref
    /// (permanently disowned under R2).
    ///
    /// Its callers are exactly the two registry producers in
    /// [`crate::workweave_index`]: the one that persists a new receipt and
    /// the one that re-derives a stored one.
    pub(crate) fn from_receipt(
        store: PathBuf,
        name: RawRefName,
        created_at: ResolvedRevisionId,
    ) -> Self {
        Self {
            store,
            name,
            created_at,
        }
    }

    /// The canonical store the receipt is keyed to.
    pub fn store(&self) -> &Path {
        &self.store
    }

    /// The tip the ref had when rwv recorded creating it. This is what
    /// [`DeletionWarrant::unmoved`] compares against.
    pub fn created_at(&self) -> &ResolvedRevisionId {
        &self.created_at
    }

    /// The recorded name, for the receipt store and the VCS impl to spell.
    pub(crate) fn name(&self) -> &RawRefName {
        &self.name
    }

    /// Whether `a` is a checkout sitting on this exact ref.
    ///
    /// A named predicate, not an operator: it answers `bool` and yields no
    /// witness, so "the receipt and the attachment agree" can never be
    /// mistaken for "I now hold proof of attachment".
    ///
    /// Compares names only. The store correspondence is the caller's: it
    /// looked this receipt up *by* store, and `a`'s repo is a checkout of
    /// that store.
    pub fn is_attached_by(&self, a: &AttachedRef) -> bool {
        self.name == a.name
    }
}

impl fmt::Display for OwnedRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name.as_str())
    }
}

/// Notion (3): the ref a checkout is on — a **witness**.
///
/// Unlike [`ResolvedRevisionId`], which refines an immutable value, an
/// `AttachedRef` observes mutable per-repo state. The proposition it
/// carries — "at the moment this was produced, *that repo's* HEAD was
/// symbolic and pointed here" — is bound to a place and can expire. So it
/// carries its provenance, and every operation that consumes it derives
/// the target repo *from the witness*: there is no independent `&Path`
/// parameter to point a MOVE somewhere else. Without that binding a
/// witness taken from the cwd repo (always attached inside a workweave)
/// could authorize a MOVE on a detached target, which is the shipped
/// cross-repo pass at `sync.rs`'s `ff_advance_repo`.
///
/// What is guaranteed: the MOVE lands on the repo whose attachment was
/// actually observed. What is *not* guaranteed: that the attachment still
/// holds at consumption time — each consumer re-observes and refuses a
/// stale witness. The wider validity-window question is Q15 and stays
/// open.
///
/// # No string access at all
///
/// Not even `pub(crate)`. Every consumer in this crate is a place the
/// laundered comparison could reappear, so the impls that act on a witness
/// re-observe the repo and compare `AttachedRef` to `AttachedRef` — a
/// same-type comparison, which is exactly the staleness check they need
/// anyway. [`Display`](fmt::Display) exists for messages and for the
/// "which ref is this checkout on" assertion shape the suite was missing.
///
/// # Compile-time invariant
///
/// A witness cannot be forged: the fields are private and the only
/// producer is [`Vcs::head_attachment`].
///
/// ```compile_fail
/// use repoweave::vcs::{AttachedRef, RawRefName};
/// use std::path::PathBuf;
/// let forged = AttachedRef {
///     repo: PathBuf::from("/tmp/repo"),
///     name: RawRefName::new("main"),
/// }; // E0451: fields `repo` and `name` are private
/// ```
///
/// Nor can it be read out as a string. This probe names only
/// `AttachedRef`, so it starts passing the moment the method comes back —
/// `push.rs` holds a one-sided `.as_str()` on the value that becomes one
/// of these, so a two-sided probe would leave that site re-enabled and
/// still green.
///
/// ```compile_fail
/// use repoweave::vcs::AttachedRef;
/// fn spell(attached: &AttachedRef) -> String {
///     attached.as_str().to_owned() // E0599: no such method
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachedRef {
    repo: PathBuf,
    name: RawRefName,
}

impl AttachedRef {
    /// The repo this attachment was observed in. Consumers derive their
    /// target from here.
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// Whether this attachment names the same local branch as `name`.
    ///
    /// The comparison the L1 publish gate needs — "is the checkout on the
    /// branch a projection names" — without ever exposing the witness's
    /// name as a string. Both `push.rs` call sites (§4.6(2)) go through
    /// this: the project gate against `RemoteDefaultBranch::local_counterpart`,
    /// the member gate against `TrackingRef::local_counterpart`.
    pub fn is_named(&self, name: &LocalRefName) -> bool {
        self.name.as_str() == name.as_str()
    }

    /// Whether this checkout is on exactly the ref `flat` requests.
    ///
    /// The healthy case of the I3 branch-discipline scan, asked through the
    /// **minted** name so the scan never derives an expectation from what it
    /// observed.
    pub fn is_minted(&self, flat: &EphemeralRefName) -> bool {
        self.name.as_str() == flat.0
    }

    /// The pre-flat ref this checkout is on, when `flat`'s namespace claims
    /// it — `branch-model.md` §7.1 arm 1's classifier.
    ///
    /// `None` for the flat name itself and for every ref outside the
    /// namespace, including another workweave's.
    pub fn legacy_name_under(&self, flat: &EphemeralRefName) -> Option<LegacyEphemeralRefName> {
        LegacyEphemeralRefName::claim(flat, &self.name)
    }
}

impl fmt::Display for AttachedRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name.as_str())
    }
}

/// HEAD is symbolic but the branch has no commits yet.
///
/// A distinct payload type from [`AttachedRef`], deliberately: MOVE
/// semantics on an unborn HEAD are undefined (a fast-forward merge fails
/// while a reset would stamp the branch into existence), so the model makes
/// the call unrepresentable rather than picking one — an `UnbornRef` cannot
/// be passed to [`Vcs::advance_attached_ref`]. This is the Q9 lesson
/// applied inside the Q9 fix.
///
/// Carries the branch name because `git symbolic-ref --short HEAD` succeeds
/// here, so the state is reportable ("on branch `main`, no commits yet")
/// rather than merely diagnosable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnbornRef {
    repo: PathBuf,
    name: RawRefName,
}

impl UnbornRef {
    /// The repo this observation came from.
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// The branch HEAD points at, which has no commits yet.
    pub fn name(&self) -> &RawRefName {
        &self.name
    }
}

impl fmt::Display for UnbornRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name.as_str())
    }
}

/// HEAD is not symbolic. Carries the commit, so a caller that wants to
/// MOVE a detached HEAD has its witness, and carries its repo for the same
/// reason [`AttachedRef`] does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedHead {
    repo: PathBuf,
    at: ResolvedRevisionId,
}

impl DetachedHead {
    /// The repo this observation came from.
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// The commit HEAD names directly.
    pub fn at(&self) -> &ResolvedRevisionId {
        &self.at
    }
}

impl fmt::Display for DetachedHead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "detached at {}", self.at.display_str())
    }
}

/// Proof that *this call* created a ref, as opposed to adopting a
/// pre-existing one.
///
/// Returned by [`Vcs::create_worktree_on`] only on the authoring path. Its
/// consumer is rollback: a failed create deletes only refs it holds a
/// `BornRef` for, so it can no longer destroy a branch the create merely
/// adopted — which is how a create's cleanup used to take a unique commit
/// with it.
///
/// Carries no registry duty. The receipt was written *before* the birth by
/// the registry; this type separates "authored" from "adopted", nothing
/// more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BornRef {
    store: PathBuf,
    name: RawRefName,
    at: ResolvedRevisionId,
}

impl BornRef {
    /// The canonical store the ref was authored in.
    pub fn store(&self) -> &Path {
        &self.store
    }

    /// The authored ref's name.
    pub fn name(&self) -> &RawRefName {
        &self.name
    }

    /// The revision the ref was authored at.
    pub fn at(&self) -> &ResolvedRevisionId {
        &self.at
    }
}

impl fmt::Display for BornRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name.as_str())
    }
}

/// The remote's own declaration of its primary branch.
///
/// This is none of the four notions and it is not a fifth kind: rwv never
/// writes it, so it sits outside the MOVE/ATTACH/DESTROY classification
/// entirely. It is a read-only *input* to the L1 publish gate. It gets its
/// own type rather than reusing [`RemoteRef`] because provenance differs —
/// a `RemoteRef` is the projection of a *declared* [`TrackingRef`], a
/// `RemoteDefaultBranch` is *observed* remote state.
///
/// Its sole producer is [`Vcs::remote_default_branch`], which returns
/// `None` when the remote's HEAD is unset or malformed. **There is no
/// fallback.** The shipped implementation fabricated `"main"` on any
/// failure, so the publish gate compared an observation against an
/// invention; with `None`, the gate has to refuse and say the remote's HEAD
/// is unset. Q6 — *which* ref publishes, and where a non-default channel's
/// identity is recorded — is policy and stays open.
///
/// Display only, like the other types the publish gate touches: the gate
/// compares through [`RemoteDefaultBranch::local_counterpart`], the same
/// assumption-stating move [`TrackingRef::local_counterpart`] makes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDefaultBranch(String);

impl RemoteDefaultBranch {
    /// Parse the target of the remote's HEAD symbolic ref.
    ///
    /// `target` is the raw symref target (git: the contents of
    /// `refs/remotes/origin/HEAD`, e.g.
    /// `refs/remotes/origin/main`); `namespace` is the prefix that target
    /// must sit under. `None` when the target does not sit under
    /// `namespace` or names nothing after it — a malformed symref is an
    /// absence, never a default.
    ///
    /// The rule lives here rather than in the VCS impl so "malformed means
    /// absent" is stated once and testable without a repo.
    pub(crate) fn from_symref_target(target: &str, namespace: &str) -> Option<Self> {
        let branch = target.trim().strip_prefix(namespace)?;
        if branch.is_empty() || validate_ref_name(branch).is_err() {
            return None;
        }
        Some(Self(branch.to_owned()))
    }

    /// "The local branch of the same name." Not an identity — the same
    /// cross-namespace projection [`TrackingRef::local_counterpart`] makes,
    /// stated in one place so the publish gate cannot make it silently.
    pub fn local_counterpart(&self) -> LocalRefName {
        LocalRefName(self.0.clone())
    }
}

impl fmt::Display for RemoteDefaultBranch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The ref a publish targets.
///
/// An opaque wrapper whose constructors are visible only inside this crate
/// and whose *use* is confined to one decision site in `push.rs`'s publish
/// gate. Q6 — is a member's publish ref the attached ref or the manifest's
/// declared tracking branch; is `version:` a constraint or a default — is
/// **open**, and this type is where the deferral is visible: a deferred
/// decision with a producer, rather than a placeholder without one. The
/// gate calls `from_attached`, preserving today's behaviour; nothing here
/// decides the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishRef(RawRefName);

impl PublishRef {
    /// Publish the ref the checkout is actually on.
    ///
    /// This is the constructor `push.rs`'s publish gate calls (§4.6(2)),
    /// the single site the type's doc comment promises: it accepts exactly
    /// what today's shipped behaviour publishes — the attached ref — so
    /// wiring the gate through `PublishRef` changes no observable
    /// behaviour. `from_local` stays defined and is called by no gate — only
    /// by `push.rs`'s `test_publish_ref` helper, to fabricate a value — so
    /// Q6's other answer keeps a producer even though one branch is now live.
    pub(crate) fn from_attached(a: &AttachedRef) -> Self {
        Self(a.name.clone())
    }

    /// Publish the local counterpart of a declared tracking branch.
    ///
    /// Unused by the shipped gate (see `from_attached`). Left in place as
    /// Q6's other answer: if the decision goes to "publish the manifest's
    /// declared branch", this is the constructor the call site switches to.
    #[allow(dead_code)]
    pub(crate) fn from_local(l: &LocalRefName) -> Self {
        Self(RawRefName(l.0.clone()))
    }

    /// The name to hand the VCS.
    pub(crate) fn name(&self) -> &RawRefName {
        &self.0
    }
}

impl fmt::Display for PublishRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_str())
    }
}

// ---------------------------------------------------------------------------
// Q9: "no current branch" is not one state (§4.5)
// ---------------------------------------------------------------------------

/// What a VCS reports about HEAD, before the model interprets it.
///
/// The VCS-specific half of [`Vcs::head_attachment`]. Splitting it this way
/// is what makes the witnesses unforgeable: an impl reports an observation,
/// and the *only* code that turns an observation into an [`AttachedRef`] /
/// [`UnbornRef`] / [`DetachedHead`] is `head_attachment`'s body in this
/// module, where those types' private fields are visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadObservation {
    /// HEAD is symbolic and the branch it names has at least one commit.
    Attached {
        /// The branch HEAD points at.
        name: RawRefName,
    },
    /// HEAD is symbolic and the branch it names has no commits yet.
    Unborn {
        /// The branch HEAD points at.
        name: RawRefName,
    },
    /// HEAD names a commit directly.
    Detached {
        /// The commit HEAD names.
        at: ResolvedRevisionId,
    },
}

/// What HEAD is, in a workspace that is **known to be a repo**.
///
/// The shipped `current_ref` collapsed four distinct conditions into a
/// single `Ok(None)` — on a branch, unborn, detached, and not-a-repo-at-all
/// — which is how `rwv push` came to report "is on a detached HEAD" for a
/// directory that was not a repo. Two of the four are errors rather than
/// states ([`VcsError::NotARepo`], [`VcsError::CommandFailed`]), so
/// [`Vcs::head_attachment`] is total over the remaining three and every
/// caller's `match` is exhaustive. The value that meant all four does not
/// exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadAttachment {
    /// HEAD is symbolic and the branch has at least one commit.
    Attached(AttachedRef),
    /// HEAD is symbolic but the branch has no commits yet.
    Unborn(UnbornRef),
    /// HEAD is not symbolic.
    Detached(DetachedHead),
}

impl HeadAttachment {
    /// The repo this observation came from, whichever state it found.
    pub fn repo(&self) -> &Path {
        match self {
            Self::Attached(a) => a.repo(),
            Self::Unborn(u) => u.repo(),
            Self::Detached(d) => d.repo(),
        }
    }

    /// The witness, when HEAD is attached. `None` for the other two states
    /// — which is *not* the collapsed `Ok(None)`: the caller still has the
    /// full value and can say which of unborn / detached it saw.
    pub fn attached(&self) -> Option<&AttachedRef> {
        match self {
            Self::Attached(a) => Some(a),
            _ => None,
        }
    }
}

impl fmt::Display for HeadAttachment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Attached(a) => write!(f, "on branch '{a}'"),
            Self::Unborn(u) => write!(f, "on unborn branch '{u}' (no commits yet)"),
            Self::Detached(d) => write!(f, "{d}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Consent and warrant tokens (§4.4)
// ---------------------------------------------------------------------------
//
// `DetachConsent`, `ReattachConsent`, and `DiscardUnmergedConsent` live in
// `crate::cli::consent` — the CLI layer's flag module, which is the only
// place that can construct them (private-field idiom). This module takes
// each as an opaque parameter and never constructs one; see that module's
// doc comment for why the home has to be there rather than here.
// `DiscardLocalCommitsConsent` and the warrant types below stay in this
// module: they are minted from checks this module runs
// (`DeletionWarrant::unmoved`/`merged`) or paired with a savepoint this
// module writes (`DiscardWarrant::new`), not from a bare flag.
//
// House rule: escape hatches are named for the precondition they waive,
// never a bare `--force`. `--detach-checkouts` and `--reattach-checkouts`
// name two categorically different consequences — losing the name your
// commits hang off, versus moving which name they hang off — so they are
// two tokens, not one `ChangeAttachmentConsent`.

/// Proof that the operator consented to discarding local commits during a
/// rewinding MOVE. Minted from `--discard-local-commits`.
///
/// The one token whose home is this module rather than `cli::consent`, and
/// the reason is that its flag has a *second* spelling: sync records
/// `--discard-local-commits` in the owner record's overrides, and
/// `rwv sync --continue` resumes a rewinding op from that record with no
/// flags on the command line at all. A `from_flag` mint at dispatch would
/// therefore cover only the fresh path and leave the resumed one — the
/// path that actually crashes and gets re-run — unable to prove the same
/// consent. `granted()` is `pub(crate)`, so the crate-internal call sites
/// are the whole minting surface, and they are pinned by
/// `tests/consent_minting_audit_test.rs`.
#[derive(Debug)]
pub struct DiscardLocalCommitsConsent(());

impl DiscardLocalCommitsConsent {
    /// Mint from the operator's `--discard-local-commits`, in whichever of
    /// its two spellings the caller is holding — the parsed flag, or the
    /// override recorded from it that `--continue` reads back.
    pub(crate) fn granted() -> Self {
        Self(())
    }
}

/// A savepoint that has actually been written.
///
/// Minted only by [`Vcs::create_savepoint_ref`], whose body is in this
/// module — so a value of this type is proof the ref exists on disk, not a
/// claim that one will be written. Carries its repo so a
/// [`DiscardWarrant`] cannot authorize rewinding a *different* repo than
/// the one whose state it captured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavepointRef {
    repo: PathBuf,
    op_id: String,
    at: ResolvedRevisionId,
}

impl SavepointRef {
    /// The repo whose tip was captured.
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// The opaque op id the savepoint is filed under.
    pub fn op_id(&self) -> &str {
        &self.op_id
    }

    /// The captured tip — the revision `rwv abort` restores to.
    pub fn at(&self) -> &ResolvedRevisionId {
        &self.at
    }
}

/// Proof that a rewinding MOVE (a non-fast-forward) may proceed.
///
/// Two things must both hold, and the type is constructed from both: a
/// savepoint under the recovery namespace has been **written** (not
/// planned), and the operator passed the verb's named override. A rewind
/// without a savepoint is therefore unrepresentable rather than
/// discouraged, which closes the asymmetry where deleting a fully-merged
/// empty branch needed a receipt and a warrant while resetting a branch to
/// an older revision needed nothing.
#[derive(Debug)]
pub struct DiscardWarrant {
    savepoint: SavepointRef,
}

impl DiscardWarrant {
    /// Pair a written savepoint with the operator's consent.
    pub fn new(savepoint: SavepointRef, consent: DiscardLocalCommitsConsent) -> Self {
        let DiscardLocalCommitsConsent(()) = consent;
        Self { savepoint }
    }

    /// The savepoint this warrant rests on. Consumers check it captured
    /// the repo they are about to rewind.
    pub fn savepoint(&self) -> &SavepointRef {
        &self.savepoint
    }
}

/// Why this ref is safe to destroy now (R3).
///
/// The receipt ([`OwnedRef`]) says "this is mine"; the warrant says "and it
/// is safe to lose now". Three warrants, and no others.
///
/// An opaque struct over a **private** enum, not a `pub enum`: Rust cannot
/// make a public enum's variant constructors private, so a public enum
/// would let any code in the crate write the "unmoved" variant while
/// filling `recorded_tip` from a fresh read of the very ref it claims is
/// unmoved — vacuously true, and exactly today's unguarded force-delete
/// with extra ceremony. The `pub fn` checkers below **run** the check they
/// certify.
///
/// Direction matters: unlike [`VerifiedRestoreOutcome`], which reports what
/// a check inside a primitive decided, a `DeletionWarrant` is caller-supplied
/// proof, because the destroy site ([`Vcs::delete_owned_ref`]) and the check
/// sites (the registry, the merged-check) are different code. That
/// inversion is why constructibility is load-bearing here.
#[derive(Debug)]
pub struct DeletionWarrant(WarrantKind);

/// Private: see [`DeletionWarrant`]'s docs for why this is not `pub`.
#[derive(Debug)]
enum WarrantKind {
    /// The ref's tip is exactly the tip rwv recorded creating it at.
    Unmoved { recorded_tip: ResolvedRevisionId },
    /// The ref's tip is an ancestor of a named baseline.
    Merged { baseline: ResolvedRevisionId },
    /// The operator passed the named override that consents to this loss.
    OperatorDiscarded,
}

impl DeletionWarrant {
    /// `Some` iff the ref's current tip equals the receipt's recorded tip
    /// — nothing has happened to it since rwv created it. This is the
    /// warrant a create's retry can hold, and it is what makes the retry's
    /// shipped justification ("deletes a *stale* branch") true rather than
    /// aspirational.
    ///
    /// `None` when the ref is gone, unreadable, or has moved.
    pub fn unmoved(vcs: &dyn Vcs, r: &OwnedRef) -> Option<Self> {
        let tip = vcs.resolve_local_branch_tip(r.store(), r.name()).ok()??;
        (tip == *r.created_at()).then_some(Self(WarrantKind::Unmoved { recorded_tip: tip }))
    }

    /// `Some` iff the ref's tip is an ancestor of `baseline` — every commit
    /// on it is reachable from a name that outlives it.
    ///
    /// `None` when the ref is gone, unreadable, or carries commits the
    /// baseline does not.
    pub fn merged(vcs: &dyn Vcs, r: &OwnedRef, baseline: &ResolvedRevisionId) -> Option<Self> {
        let tip = vcs.resolve_local_branch_tip(r.store(), r.name()).ok()??;
        vcs.is_ancestor(r.store(), &tip, baseline)
            .ok()?
            .then_some(Self(WarrantKind::Merged {
                baseline: baseline.clone(),
            }))
    }

    /// The operator passed `--discard-unmerged-commits`. No check to run —
    /// the consent *is* the warrant, and the token proves it was given.
    ///
    /// `consent` is not otherwise inspected: its field is private to
    /// `cli::consent`, so this module (a different one) cannot even
    /// destructure it. Holding a value of the type is the whole proof.
    pub fn operator_discarded(consent: DiscardUnmergedConsent) -> Self {
        let _ = consent;
        Self(WarrantKind::OperatorDiscarded)
    }

    /// The operator passed `--adopt-detached-checkouts`, which
    /// `branch-model.md` §7.1 arm 3 makes the consent for stranding a legacy
    /// branch's tip.
    ///
    /// A second constructor rather than a widened `operator_discarded`
    /// because the two flags consent to different losses and the house rule
    /// (§4.4) is one token per consequence: `--discard-unmerged-commits`
    /// gives up commits at `workweave delete`, `--adopt-detached-checkouts`
    /// gives up the *name* a legacy branch holds so the flat one can exist in
    /// its place. Arm 3 requires the caller to warn when that strands a tip;
    /// the token records only that the operator asked for it.
    pub fn adopt_detached(consent: AdoptDetachedConsent) -> Self {
        let _ = consent;
        Self(WarrantKind::OperatorDiscarded)
    }

    /// One line naming which warrant this is and what it rests on, for
    /// reports that have to say why a ref was destroyed.
    pub fn describe(&self) -> String {
        match &self.0 {
            WarrantKind::Unmoved { recorded_tip } => {
                format!(
                    "unmoved since rwv created it at {}",
                    recorded_tip.display_str()
                )
            }
            WarrantKind::Merged { baseline } => {
                format!("merged into {}", baseline.display_str())
            }
            WarrantKind::OperatorDiscarded => {
                "operator passed --discard-unmerged-commits".to_owned()
            }
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

    /// Human-readable hint text for the VCS-level steps to resolve a conflict
    /// and stage the result. Embedded verbatim in sync's conflict-bail messages.
    ///
    /// Returned text is a short multi-line block suitable for splicing into a
    /// larger message — callers are expected to add surrounding context (which
    /// repo, the `rwv <verb> --continue` line for Rebase, and `rwv abort` as
    /// the rollback option).
    ///
    /// `op` is the in-flight operation that produced the conflict (rebase,
    /// merge, cherry-pick); the hint text varies per VCS and per op. No
    /// `repo` param: the hint text for [`GitVcs`](crate::git::GitVcs)
    /// doesn't vary per-repo, and adding a parameter we don't read would be
    /// noise. Add one if a future VCS needs to inspect on-disk state.
    ///
    /// ## Seam rule
    ///
    /// The VCS impl owns git vocabulary only. For [`ConflictOp::Merge`] and
    /// [`ConflictOp::CherryPick`], the hint includes the git `--continue`
    /// command (those ops have no rwv-native resume path from the sync rebase
    /// flow). For [`ConflictOp::Rebase`], the hint stops at `git add <files>`;
    /// the caller (rwv core) appends the appropriate `rwv sync --continue` /
    /// `rwv sync-to --continue` line — rwv vocabulary must not appear inside
    /// a VCS impl.
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
    /// the rebase target's version of the configured path. For git this is a
    /// per-path merge driver (`merge=rwv-ours` in `.gitattributes` paired
    /// with `merge.rwv-ours.driver=true` in config); the trait hides the
    /// spelling so other VCS impls can use their own mechanism.
    ///
    /// [`set_replay_exclusion`]: Vcs::set_replay_exclusion
    fn rebase(
        &self,
        repo: &Path,
        onto: &ResolvedRevisionId,
        upstream: &ResolvedRevisionId,
    ) -> Result<(), VcsError>;

    /// Resume an in-flight rebase in `repo` after the operator has resolved
    /// (and staged) the conflicting paths that stopped the previous
    /// [`rebase`] or [`rebase_continue`] call.
    ///
    /// Contract:
    /// - Caller MUST ensure `repo` is mid-rebase before calling. The mid-op
    ///   check lives at the caller (which already inspects [`mid_op`] to
    ///   decide between [`rebase`] and this method), so a call on a repo
    ///   that is NOT mid-rebase is a caller bug, not an in-band condition —
    ///   returns [`VcsError::CommandFailed`] rather than silently no-op'ing.
    /// - When the resumed rebase completes (all remaining picks apply
    ///   cleanly, or drop as empty via `--empty=drop` set by [`rebase`]):
    ///   `Ok(())`.
    /// - When the resumed rebase stops again on a further genuine
    ///   non-lock conflict, or when the operator's resolution was
    ///   incomplete (unstaged conflict markers): repo is left in the same
    ///   mid-rebase state git leaves it, and returns
    ///   [`VcsError::RebaseConflict { repo, op: ConflictOp::Rebase }`] so
    ///   the operator loop stays: resolve → `git add` → `rwv sync
    ///   --continue`, iterating per conflicted pick.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): runs `git rebase --continue` with
    /// the `rwv-ours` merge-driver flags supplied inline (same flags as
    /// [`rebase`] so any remaining lock-only picks resolve to the target's
    /// version — the durable config plant makes bare `git rebase --continue`
    /// safe too, but the inline flags are the source-of-truth path). The
    /// invocation runs non-interactively — git's editor spawn for the
    /// stopped commit's message is suppressed so a `--continue` never hangs
    /// waiting for `$EDITOR` in an automated pipeline.
    ///
    /// [`rebase`]: Vcs::rebase
    /// [`rebase_continue`]: Vcs::rebase_continue
    /// [`mid_op`]: Vcs::mid_op
    fn rebase_continue(&self, repo: &Path) -> Result<(), VcsError>;

    /// Configure `repo` so that during replay (rebase, merge) any changes to
    /// `path` are silently overridden — the replay target's version of `path`
    /// always wins.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): appends a
    /// `<path> merge=rwv-ours` line to `<repo>/.gitattributes` (idempotent
    /// — re-running is a no-op if the line is already present). If the
    /// file still carries the legacy `<path> merge=ours` line, the writer
    /// migrates it in place rather than appending a
    /// second, ambiguous assignment. Other VCS impls choose their own
    /// mechanism.
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
    /// contains a `<path> merge=rwv-ours` line. The legacy `merge=ours`
    /// spelling is NOT accepted — a repo carrying only the legacy line is
    /// reported as missing so `rwv doctor --fix` migrates it. See
    /// [`crate::git::has_working_tree_legacy_replay_exclusion`] for
    /// migration detection.
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
    /// check ("can rebase/merge rely on `merge=rwv-ours` to keep `rwv.lock`
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
    /// Safe by construction: no `reset --hard` replacement that could
    /// discard reachable history.
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

    /// Return a human-readable detail string identifying the commit that
    /// stopped an in-flight rebase in `repo`, for use in conflict-bail messages.
    ///
    /// During a rebase conflict the stopped commit's SHA is written to a
    /// VCS-specific path (for git: `.git/rebase-merge/stopped-sha`). This
    /// method reads that file, resolves the short SHA, and fetches the commit
    /// subject. If any step fails it returns a generic fallback so the
    /// caller's conflict message still renders.
    ///
    /// The returned string is suitable as the `detail` arg to
    /// `per_conflict_bail_message` in sync, e.g.:
    /// `"commit abc1234 (lock: refresh — post-OOB drift in gc-formulas)"`
    fn rebase_stopped_commit_detail(&self, repo: &Path) -> String;

    /// Return up to `cap` one-line commit summaries for the range `from..to`
    /// plus the total commit count in that range.
    ///
    /// Returns `(vec![], 0)` on any error so callers can degrade gracefully
    /// when the range is unresolvable (e.g. the object is unreachable in a
    /// shallow clone or the SHA is malformed).
    fn log_oneline_range(
        &self,
        repo: &Path,
        from: &str,
        to: &str,
        cap: usize,
    ) -> (Vec<String>, usize);

    /// Return `(ahead, behind)` commit counts for `savepoint..tip` and
    /// `tip..savepoint` respectively. Used to determine whether `tip` is
    /// strictly ahead of `savepoint` (`behind == 0, ahead > 0`) or diverged
    /// (both > 0).
    ///
    /// Returns `(0, 0)` on any VCS error.
    fn ahead_behind(&self, repo: &Path, savepoint: &str, tip: &str) -> (usize, usize);

    /// List the commits reachable from `repo`'s current tip but NOT from
    /// `parent_tip`, newest first.
    ///
    /// These are the workweave's UNIQUE commits: the work that landed on top
    /// of the parent. Returns an empty vec when nothing landed on top of the
    /// parent (the tip is an ancestor of, or equal to, `parent_tip`). The
    /// result stays correct when the parent ADVANCED after the fork —
    /// commits the parent already has are excluded because they are
    /// reachable from `parent_tip`.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): computes `git log
    /// <parent-tip>..<tip>` and maps each entry into a [`CommitSummary`].
    fn unique_commits(
        &self,
        repo: &Path,
        parent_tip: &ResolvedRevisionId,
    ) -> Result<Vec<CommitSummary>, VcsError>;

    /// Produce the unified diff of `repo`'s unique work vs `parent_tip`,
    /// anchored at the COMMON ANCESTOR of the current tip and `parent_tip`.
    ///
    /// Anchoring at the common ancestor — not `parent_tip` directly — is
    /// what keeps a parent that advanced after the fork from producing
    /// phantom reversals: work the parent gained after the fork is not part
    /// of the workweave's unique history, so it must not appear (as a
    /// deletion) in the workweave's diff. The returned [`UniqueDiff::base`]
    /// carries the anchor revision so callers can display it.
    ///
    /// For [`GitVcs`](crate::git::GitVcs): anchors at `git merge-base
    /// <parent-tip> <tip>` and diffs `<anchor>..<tip>`.
    fn unique_diff(
        &self,
        repo: &Path,
        parent_tip: &ResolvedRevisionId,
    ) -> Result<UniqueDiff, VcsError>;

    // =======================================================================
    // The branch model (branch-model.md §4.3)
    // =======================================================================
    //
    // Every ref write rwv performs is exactly one of four kinds — MOVE,
    // ATTACH, DESTROY, DESTROY-STORE — and the kind decides what consent is
    // required (§3.2). The methods below are grouped by kind, and each
    // takes the proof its kind needs: a witness for a MOVE, a consent token
    // for an ATTACH that is not a birth, a receipt plus a warrant for a
    // DESTROY. Store-level destroys (R4) are not ref operations and have no
    // method here.
    //
    // These land ALONGSIDE `checkout` / `delete_branch` / `current_ref`
    // above; the old surface is removed once every call site has been
    // restated in terms of this one, and that restatement is the audit.

    // ---- observation (§4.5) -----------------------------------------------

    /// Report what `repo`'s HEAD is, without interpreting it.
    ///
    /// The VCS-specific half of [`head_attachment`]. Implementations must
    /// distinguish "not a repo" ([`VcsError::NotARepo`]) from "the ref
    /// database is unreadable" ([`VcsError::CommandFailed`]) — collapsing
    /// either into a state is the defect this split exists to remove.
    ///
    /// [`head_attachment`]: Vcs::head_attachment
    fn observe_head(&self, repo: &Path) -> Result<HeadObservation, VcsError>;

    /// What HEAD is in `repo`. Total over the three states, with the two
    /// non-states as typed errors.
    ///
    /// Provided, not implementable per-VCS: this body is the only place
    /// [`AttachedRef`], [`UnbornRef`], and [`DetachedHead`] are ever
    /// constructed, which is what makes a witness proof of an observation
    /// rather than a struct anyone can fill in.
    fn head_attachment(&self, repo: &Path) -> Result<HeadAttachment, VcsError> {
        Ok(match self.observe_head(repo)? {
            HeadObservation::Attached { name } => HeadAttachment::Attached(AttachedRef {
                repo: repo.to_path_buf(),
                name,
            }),
            HeadObservation::Unborn { name } => HeadAttachment::Unborn(UnbornRef {
                repo: repo.to_path_buf(),
                name,
            }),
            HeadObservation::Detached { at } => HeadAttachment::Detached(DetachedHead {
                repo: repo.to_path_buf(),
                at,
            }),
        })
    }

    /// Re-observe a witness's repo and confirm the attachment still holds.
    ///
    /// Returns [`VcsError::StaleRefWitness`] when the repo has moved on —
    /// switched branches, been detached, or become unborn — since the
    /// witness was produced. Every MOVE and post-birth ATTACH runs this
    /// first, so "the attachment I planned against is the attachment I am
    /// acting on" is checked rather than assumed.
    fn verify_attachment(&self, witness: &AttachedRef) -> Result<(), VcsError> {
        let observed = self.head_attachment(witness.repo())?;
        match &observed {
            HeadAttachment::Attached(now) if now == witness => Ok(()),
            _ => Err(VcsError::StaleRefWitness {
                repo: witness.repo().to_path_buf(),
                expected: format!("on branch '{witness}'"),
                observed: observed.to_string(),
            }),
        }
    }

    /// Resolve the tip of a **local branch** by name, or `None` when no
    /// such branch exists.
    ///
    /// Resolves in the local-branch namespace specifically, so a tag of
    /// the same name cannot answer instead. Used by
    /// [`DeletionWarrant::unmoved`] and [`DeletionWarrant::merged`], which
    /// have to compare a receipt against the ref it describes.
    fn resolve_local_branch_tip(
        &self,
        repo: &Path,
        name: &RawRefName,
    ) -> Result<Option<ResolvedRevisionId>, VcsError>;

    /// A short label naming the in-flight operation `repo` is mid-way
    /// through, or `None` when it is in a clean state.
    ///
    /// Broader than [`mid_op`]: that one reports only the operations with a
    /// conflict-resume path, and returns `None` for a bisect, which is
    /// exactly the state §3.6's precondition has to see.
    ///
    /// [`mid_op`]: Vcs::mid_op
    fn mid_operation(&self, repo: &Path) -> Option<String>;

    // ---- MOVE (§3.2) ------------------------------------------------------

    /// Fast-forward the ref `on` witnesses, refusing rather than clobbering
    /// when a fast-forward is not possible.
    ///
    /// The target repo is derived **from the witness**. There is no
    /// independent path parameter, so a witness obtained from one repo
    /// cannot be used to move another — the cross-repo pass is not a check
    /// that can be forgotten, it is a signature that cannot be written.
    fn advance_attached_ref(
        &self,
        on: &AttachedRef,
        to: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        self.verify_attachment(on)?;
        self.advance_if_fast_forward(on.repo(), to)
    }

    /// Rewind the ref `on` witnesses to `to`, discarding divergent commits.
    ///
    /// A rewinding MOVE needs a [`DiscardWarrant`], which cannot exist
    /// without a savepoint having been written; the warrant is checked to
    /// belong to *this* repo, so a savepoint taken elsewhere cannot
    /// authorize this reset.
    fn reset_attached_ref(
        &self,
        on: &AttachedRef,
        to: &ResolvedRevisionId,
        warrant: DiscardWarrant,
    ) -> Result<(), VcsError> {
        if warrant.savepoint().repo() != on.repo() {
            return Err(VcsError::StaleRefWitness {
                repo: on.repo().to_path_buf(),
                expected: format!("a savepoint taken in {}", on.repo().display()),
                observed: format!(
                    "a savepoint taken in {}",
                    warrant.savepoint().repo().display()
                ),
            });
        }
        self.verify_attachment(on)?;
        self.hard_reset(on.repo(), to)
    }

    /// Move an already-detached HEAD to `to`.
    ///
    /// This is a MOVE, not an ATTACH: HEAD's symbolic-ness does not change.
    /// It is subject to the mid-operation precondition (§3.6) — a repo
    /// stopped mid-bisect or mid-rebase is carrying operator state that a
    /// silent reposition would destroy, and `Detached` alone cannot tell
    /// that apart from "rwv detached this at a lock SHA".
    fn advance_detached_head(
        &self,
        was: &DetachedHead,
        to: &ResolvedRevisionId,
    ) -> Result<(), VcsError> {
        if let Some(operation) = self.mid_operation(was.repo()) {
            return Err(VcsError::MidOperation {
                repo: was.repo().to_path_buf(),
                operation,
            });
        }
        let observed = self.head_attachment(was.repo())?;
        match &observed {
            HeadAttachment::Detached(now) if now == was => {}
            _ => {
                return Err(VcsError::StaleRefWitness {
                    repo: was.repo().to_path_buf(),
                    expected: was.to_string(),
                    observed: observed.to_string(),
                })
            }
        }
        self.set_detached_head(was.repo(), to)
    }

    // ---- ATTACH (§3.2) ----------------------------------------------------

    /// Materialize a worktree at `dest` on the ref a receipt describes.
    ///
    /// This is a **birth**: no consent token, because there was no prior
    /// attachment to lose. The store, the name, and the start point all
    /// come from the receipt, which the registry persisted *before* this
    /// call — so a crash here leaves a dangling receipt (benign) rather
    /// than an unreceipted ref (permanently disowned under R2).
    ///
    /// Returns `Some(BornRef)` iff this call **authored** the ref, `None`
    /// when it adopted a pre-existing one. Rollback keys on that: a create
    /// that adopted a branch must not delete it on the way out.
    fn create_worktree_on(
        &self,
        owned: &OwnedRef,
        dest: &Path,
    ) -> Result<Option<BornRef>, VcsError> {
        let authored = self.materialize_worktree_on_ref(
            owned.store(),
            dest,
            owned.name(),
            owned.created_at(),
        )?;
        Ok(authored.then(|| BornRef {
            store: owned.store().to_path_buf(),
            name: owned.name().clone(),
            at: owned.created_at().clone(),
        }))
    }

    /// VCS-specific half of [`create_worktree_on`]: create a worktree at
    /// `dest` on branch `name`, starting at `start_point`.
    ///
    /// Returns `true` when the branch was created by this call and `false`
    /// when a branch of that name already existed and was adopted.
    /// Implementations must **not** delete a pre-existing branch to force
    /// the authoring path: destroying a ref needs a receipt and a warrant,
    /// and this call has neither.
    ///
    /// [`create_worktree_on`]: Vcs::create_worktree_on
    fn materialize_worktree_on_ref(
        &self,
        store: &Path,
        dest: &Path,
        name: &RawRefName,
        start_point: &ResolvedRevisionId,
    ) -> Result<bool, VcsError>;

    /// Materialize `url` at `dest` with the checkout attached to `name` and
    /// positioned at the lock scalar `at`, which is resolved *inside* the
    /// new clone and returned.
    ///
    /// The **birth** arm of `rwv fetch` (§5's `fetch` (absent clone) row):
    /// no consent token, because there was no prior attachment to lose.
    /// The birth target is the lock revision, not the remote tip. Cloning
    /// onto the tip and then aligning would make bootstrapping a weave from
    /// a lock that is behind origin a *rewind*, and a rewinding MOVE needs a
    /// `DiscardWarrant` — so that sequence would refuse on every repo and
    /// mass-produce the fully-detached weave §6 item 2 calls a problem.
    ///
    /// One call rather than a clone plus an align so there is no way to
    /// point the positioning half at a repo this call did not just create:
    /// a checkout that relocates a branch it does not own is a MOVE, and
    /// the caller holds no witness for one.
    fn clone_attached_at(
        &self,
        url: &str,
        dest: &Path,
        role: Role,
        name: &LocalRefName,
        at: &RawRevisionId,
    ) -> Result<ResolvedRevisionId, VcsError>;

    /// Leave the checkout `from` witnesses on no branch, at `to`.
    ///
    /// Post-birth attachment change: requires the operator's consent,
    /// because what is lost is the name their commits hang off. `consent`
    /// is not otherwise inspected: its field is private to `cli::consent`,
    /// so this module cannot even destructure it. Holding a value of the
    /// type is the whole proof.
    fn detach_head(
        &self,
        from: &AttachedRef,
        to: &ResolvedRevisionId,
        consent: DetachConsent,
    ) -> Result<(), VcsError> {
        let _ = consent;
        self.verify_attachment(from)?;
        self.set_detached_head(from.repo(), to)
    }

    /// Point the checkout `from` describes at the local branch `to`.
    ///
    /// Takes the whole [`HeadAttachment`] rather than a witness: reattach
    /// is reachable from all three states (including `Unborn`, where the
    /// operator wants off a branch that never got a commit), and the state
    /// it planned against must still hold. `consent` is not otherwise
    /// inspected — see [`detach_head`](Vcs::detach_head)'s doc comment.
    fn reattach_head(
        &self,
        from: HeadAttachment,
        to: &LocalRefName,
        consent: ReattachConsent,
    ) -> Result<(), VcsError> {
        let _ = consent;
        let repo = from.repo().to_path_buf();
        let observed = self.head_attachment(&repo)?;
        if observed != from {
            return Err(VcsError::StaleRefWitness {
                repo,
                expected: from.to_string(),
                observed: observed.to_string(),
            });
        }
        self.attach_head_to(&repo, to)
    }

    /// VCS-specific half of the detaching ATTACH: put `repo`'s HEAD on
    /// `to` directly, naming no branch. Never forced — the VCS's own
    /// refusal to overwrite modified paths is a precondition, not an
    /// obstacle.
    fn set_detached_head(&self, repo: &Path, to: &ResolvedRevisionId) -> Result<(), VcsError>;

    /// VCS-specific half of [`reattach_head`]: put `repo`'s HEAD on the
    /// existing local branch `name`.
    ///
    /// **The impl must check that the branch exists and refuse when it does
    /// not.** Creating one would be a birth, which is a different operation
    /// with a different consent shape — and leaving the refusal to the VCS
    /// is not enough. Git, asked to switch to a name it cannot find as a
    /// local branch, will invent the branch from a remote-tracking ref of
    /// the same name, or detach when the name is a tag's, or read the name
    /// as a *path* and revert the operator's edits to it. All three exit 0.
    /// A `LocalRefName` is a projection of a remote branch name, so the
    /// first of those is the ordinary case, not a corner.
    ///
    /// [`reattach_head`]: Vcs::reattach_head
    fn attach_head_to(&self, repo: &Path, name: &LocalRefName) -> Result<(), VcsError>;

    // ---- DESTROY (§3.2) ---------------------------------------------------

    /// Destroy a ref rwv holds a receipt for.
    ///
    /// Receipt **and** warrant, and no overload takes a name: there is no
    /// way to spell "delete this branch I recognised". The store comes from
    /// the receipt, so a receipt cannot authorize a delete in a different
    /// store.
    fn delete_owned_ref(
        &self,
        branch: &OwnedRef,
        warrant: DeletionWarrant,
    ) -> Result<(), VcsError> {
        let _ = warrant.describe();
        self.destroy_local_ref(branch.store(), branch.name())
    }

    /// VCS-specific half of [`delete_owned_ref`]. Not to be called
    /// directly: it takes a raw name and no warrant, which is precisely
    /// the shape R2 and R3 exist to forbid at the call sites above it.
    ///
    /// [`delete_owned_ref`]: Vcs::delete_owned_ref
    fn destroy_local_ref(&self, store: &Path, name: &RawRefName) -> Result<(), VcsError>;

    // ---- rename (§3.4: a DESTROY of the old name plus a birth) ------------

    /// Rename `from` to `to` — `branch-model.md` §7.1 arm 1's migration.
    ///
    /// Both halves are receipts, because §3.4 derives a rename as a DESTROY
    /// of the old name plus a birth of the new: the DESTROY needs `from`'s
    /// receipt and `warrant`, and the birth's receipt (`to`) has to be on
    /// disk before the ref write, which it is — an [`OwnedRef`] exists only
    /// after [`RefRegistry::record_created`] has fsynced it.
    ///
    /// One VCS operation rather than a delete plus a create because **neither
    /// half can go first**. git cannot hold `refs/heads/p--w` and
    /// `refs/heads/p--w/<segment>` at the same time (a ref and a directory of
    /// the same name), so the birth cannot precede the DESTROY; and git
    /// refuses to delete the branch a worktree's HEAD is on, which for arm 1
    /// is exactly the branch being renamed, so the DESTROY cannot precede the
    /// birth either. The rename resolves both at once, and moves every
    /// worktree HEAD that pointed at the old name.
    ///
    /// The store comes from `from`'s receipt, so a receipt cannot authorize a
    /// rename in a different refdb; `to`'s store must agree, and this refuses
    /// when it does not.
    ///
    /// [`RefRegistry::record_created`]: crate::workweave_index::RefRegistry::record_created
    fn rename_owned_ref(
        &self,
        from: &OwnedRef,
        to: &OwnedRef,
        warrant: DeletionWarrant,
    ) -> Result<(), VcsError> {
        let _ = warrant.describe();
        if from.store != to.store {
            return Err(VcsError::CommandFailed {
                args: vec!["rename".to_owned()],
                repo: from.store.clone(),
                stderr: format!(
                    "receipt for `{}` is keyed to {} but the receipt for `{}` is keyed to {}",
                    from.name,
                    from.store.display(),
                    to.name,
                    to.store.display()
                ),
            });
        }
        self.rename_local_ref(from.store(), from.name(), to.name())
    }

    /// VCS-specific half of [`rename_owned_ref`]. Not to be called directly:
    /// it takes raw names and no warrant.
    ///
    /// **Must not force.** Renaming over an existing name would destroy that
    /// name's ref with neither receipt nor warrant.
    ///
    /// [`rename_owned_ref`]: Vcs::rename_owned_ref
    fn rename_local_ref(
        &self,
        store: &Path,
        from: &RawRefName,
        to: &RawRefName,
    ) -> Result<(), VcsError>;

    // ---- adoption of a detached checkout (§7.1 arms 3 and 5) --------------

    /// Birth `to` at the commit `from` is detached on, and attach the
    /// checkout to it — `branch-model.md` §7.1 arms 3 and 5.
    ///
    /// A birth *and* an ATTACH, which is why it takes a consent token: R1
    /// makes detached → attached an attachment change, and the workweave's
    /// HEAD is at the lock SHA, not at whatever a legacy branch reached.
    ///
    /// No start point is passed to the birth. The ref is created where HEAD
    /// already is, so the working tree does not move and the call cannot be a
    /// rewind wearing a birth's clothes — `to`'s recorded tip is the same
    /// commit by construction (the caller records the receipt from this very
    /// observation), and re-deriving it from the receipt would only create a
    /// way for the two to disagree.
    fn adopt_detached_checkout(
        &self,
        from: &DetachedHead,
        to: &OwnedRef,
        consent: AdoptDetachedConsent,
    ) -> Result<(), VcsError> {
        let _ = consent;
        let repo = from.repo().to_path_buf();
        let observed = self.head_attachment(&repo)?;
        if observed != HeadAttachment::Detached(from.clone()) {
            return Err(VcsError::StaleRefWitness {
                repo,
                expected: HeadAttachment::Detached(from.clone()).to_string(),
                observed: observed.to_string(),
            });
        }
        self.birth_ref_at_head(&repo, to.name())
    }

    /// VCS-specific half of [`adopt_detached_checkout`]: create `name` at
    /// `repo`'s current HEAD and attach HEAD to it.
    ///
    /// **The impl must refuse when `name` already exists.** Reusing an
    /// existing branch here would be an adoption of a ref this call holds no
    /// receipt for, and moving it to HEAD would be an unwitnessed MOVE.
    ///
    /// [`adopt_detached_checkout`]: Vcs::adopt_detached_checkout
    fn birth_ref_at_head(&self, repo: &Path, name: &RawRefName) -> Result<(), VcsError>;

    // ---- publish ----------------------------------------------------------

    /// Push `r` to the remote `role` selects.
    ///
    /// The ref is a **parameter**, so the choice of what to publish is made
    /// at one site in `push.rs` instead of being implicit inside the VCS
    /// impl. Q6 decides what that site passes; this signature only makes
    /// the decision visible.
    fn push_ref(
        &self,
        repo: &Path,
        role: Role,
        r: &PublishRef,
        force: bool,
    ) -> Result<(), VcsError>;

    /// The remote's declared primary branch, or `None` when it is unset or
    /// malformed. **No fallback** — see [`RemoteDefaultBranch`].
    fn remote_default_branch(&self, repo: &Path) -> Result<Option<RemoteDefaultBranch>, VcsError>;

    // ---- listing ----------------------------------------------------------

    /// Local branch names starting with `prefix`, as observed.
    ///
    /// Report-only by type: a [`RawRefName`] is not an [`OwnedRef`], so
    /// nothing in the result can be deleted without a registry lookup. That
    /// is the difference between "destroy this prefix-scoped set" and
    /// "report what is left over".
    fn list_branch_names_with_prefix(
        &self,
        repo: &Path,
        prefix: &str,
    ) -> Result<Vec<RawRefName>, VcsError>;

    /// Every local branch name in `repo`, as observed. Report-only, for
    /// the same reason as [`list_branch_names_with_prefix`].
    ///
    /// [`list_branch_names_with_prefix`]: Vcs::list_branch_names_with_prefix
    fn list_local_branch_names(&self, repo: &Path) -> Result<Vec<RawRefName>, VcsError>;

    // ---- savepoints as proof ----------------------------------------------

    /// Write a savepoint and return **proof it exists**.
    ///
    /// Provided: this body is the only place a [`SavepointRef`] is
    /// constructed, so the type cannot be minted for a savepoint that was
    /// only planned. [`DiscardWarrant::new`] takes one, which is what makes
    /// a rewind without a savepoint unrepresentable.
    fn create_savepoint_ref(&self, repo: &Path, op_id: &str) -> Result<SavepointRef, VcsError> {
        let at = self.create_savepoint(repo, op_id)?;
        Ok(SavepointRef {
            repo: repo.to_path_buf(),
            op_id: op_id.to_owned(),
            at,
        })
    }

    /// Proof for a savepoint an *earlier* call wrote. `None` when there is
    /// none under `op_id`.
    ///
    /// The same proposition as [`create_savepoint_ref`] — "this savepoint
    /// exists on disk, at this revision, in this repo" — reached by reading
    /// rather than writing, so it is a second producer of the type and not
    /// a weaker one. It exists because a long-running op writes its
    /// savepoint once, before the phases, and a `--continue` resumes into a
    /// phase that still needs the warrant: re-*creating* the savepoint there
    /// would silently move the recovery point to the post-crash tip, which
    /// is the one thing the savepoint is for.
    ///
    /// [`create_savepoint_ref`]: Vcs::create_savepoint_ref
    fn resolve_savepoint_ref(&self, repo: &Path, op_id: &str) -> Option<SavepointRef> {
        self.resolve_savepoint(repo, op_id).map(|at| SavepointRef {
            repo: repo.to_path_buf(),
            op_id: op_id.to_owned(),
            at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // The rules that live in the types rather than in a VCS impl. Each is
    // exercised here because "malformed means absent" and "canonical means
    // checked" have to hold without a repo to consult.
    // -----------------------------------------------------------------------

    #[test]
    fn canonical_commit_ids_are_full_width_lowercase_hex() {
        assert!(is_canonical_commit_id(&"a".repeat(40)));
        assert!(is_canonical_commit_id(&"0123456789abcdef".repeat(4)[..64]));
        // Abbreviated ids are ambiguous by construction; uppercase means the
        // string did not come from a resolution.
        assert!(!is_canonical_commit_id(&"a".repeat(7)));
        assert!(!is_canonical_commit_id(&"a".repeat(39)));
        assert!(!is_canonical_commit_id(&"a".repeat(41)));
        assert!(!is_canonical_commit_id(&"A".repeat(40)));
        assert!(!is_canonical_commit_id(&"g".repeat(40)));
        assert!(!is_canonical_commit_id(""));
    }

    #[test]
    fn a_malformed_remote_head_symref_names_no_default() {
        const NS: &str = "refs/remotes/origin/";
        // Outside the namespace entirely.
        assert!(RemoteDefaultBranch::from_symref_target("refs/heads/main", NS).is_none());
        // Inside the namespace but naming nothing.
        assert!(RemoteDefaultBranch::from_symref_target(NS, NS).is_none());
        // Empty output — git printed nothing.
        assert!(RemoteDefaultBranch::from_symref_target("", NS).is_none());
        // Inside the namespace but not a usable ref name.
        assert!(RemoteDefaultBranch::from_symref_target(&format!("{NS}bad..name"), NS).is_none());
        // The one shape that yields a value.
        let ok = RemoteDefaultBranch::from_symref_target(&format!("{NS}main\n"), NS)
            .expect("well-formed symref");
        assert_eq!(ok.to_string(), "main");
        assert_eq!(ok.local_counterpart().as_str(), "main");
    }

    #[test]
    fn sha_shape_has_a_stated_floor() {
        assert!(is_sha_shaped(&"a".repeat(40)));
        assert!(is_sha_shaped("0123456"));
        assert!(!is_sha_shaped("012345"), "below git's abbreviation floor");
        assert!(!is_sha_shaped("main"));
        assert!(!is_sha_shaped("deadbeefs"), "s is not hex");
    }

    #[test]
    fn release_shape_is_one_definition_for_two_questions() {
        for yes in ["v1.0", "v1.2.3", "v0.3.4-rc1", "v10.0+build"] {
            assert!(is_release_shape_name(yes), "{yes}");
        }
        for no in ["main", "v1", "vnext", "1.2.3", "release/1.x", "v"] {
            assert!(!is_release_shape_name(no), "{no}");
        }
    }

    #[test]
    fn ref_name_validation_mirrors_the_strictest_rules_rwv_targets() {
        assert!(validate_ref_name("main").is_ok());
        assert!(validate_ref_name("release/1.x").is_ok());
        assert!(validate_ref_name("p--ww").is_ok());
        assert_eq!(validate_ref_name(""), Err(RefNameError::Empty));
        for bad in [
            "a..b", "a@{0}", "@", "a//b", "/a", "a/", "a.", ".a", "a/.b", "a.lock", "a/b.lock",
            "a b", "a~1", "a^", "a:b", "a?", "a*", "a[", "a\\b", "a\tb",
        ] {
            assert!(
                matches!(validate_ref_name(bad), Err(RefNameError::Malformed { .. })),
                "{bad:?} should be Malformed"
            );
        }
    }

    #[test]
    fn a_receipt_and_an_attachment_are_compared_by_a_named_predicate() {
        // `is_attached_by` is the only bridge between notion (2b) and notion
        // (3), it answers `bool`, and it yields no witness — so "the receipt
        // matches" can never be mistaken for "I hold proof of attachment".
        let owned = OwnedRef::from_receipt(
            PathBuf::from("/tmp/store/.git"),
            RawRefName::new("p--ww"),
            ResolvedRevisionId::from_canonical("a".repeat(40), None),
        );
        let same = AttachedRef {
            repo: PathBuf::from("/tmp/checkout"),
            name: RawRefName::new("p--ww"),
        };
        let other = AttachedRef {
            repo: PathBuf::from("/tmp/checkout"),
            name: RawRefName::new("main"),
        };
        assert!(owned.is_attached_by(&same));
        assert!(!owned.is_attached_by(&other));
    }

    #[test]
    fn a_warrant_says_which_check_licensed_the_destroy() {
        let w = DeletionWarrant::operator_discarded(DiscardUnmergedConsent::granted());
        assert_eq!(w.describe(), "operator passed --discard-unmerged-commits");
    }
}
