//! Version control system abstraction.
//!
//! repoweave operates on repos and worktrees. The VCS layer abstracts over
//! the specific tool (git, jj, sl, hg) so core logic doesn't hardcode git.

use crate::alignment::Departure;
use crate::cli::consent::{DiscardUnlandedConsent, Lease};
use crate::naming::FlatAddress;
use crate::op_state::SavepointKey;
use crate::registry::CloneUrl;
use crate::wire_token::{parse_wire_token, wire_token};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(test)]
pub(crate) mod testing;

/// A full commit id: a lowercase-hex object name of the width the
/// repository's hash function produces (40 for SHA-1, 64 for SHA-256 repos —
/// `extensions.objectFormat = sha256`).
///
/// [`CommitId::parse`] is the only way to obtain one, and deserializing goes
/// through it, so a tag or branch name read from a state file is refused
/// where the file is read. Abbreviated ids are rejected: they are ambiguous
/// by construction. Uppercase is rejected because no VCS emits it for a
/// resolved object name, so its appearance means the string came from
/// somewhere other than a resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommitId(String);

/// Why a string is not a [`CommitId`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitIdError(String);

impl fmt::Display for CommitIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "'{}' is not a commit id: expected 40 or 64 lowercase hex digits",
            self.0
        )
    }
}

impl std::error::Error for CommitIdError {}

impl CommitId {
    pub const SHORT_WIDTH: usize = 12;

    pub fn parse(s: &str) -> Result<Self, CommitIdError> {
        if is_canonical_commit_id(s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(CommitIdError(s.to_owned()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The abbreviation every operator surface prints.
    pub fn short(&self) -> &str {
        &self.0[..Self::SHORT_WIDTH]
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for CommitId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl JsonSchema for CommitId {
    fn schema_name() -> String {
        "CommitId".to_owned()
    }

    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            string: Some(Box::new(schemars::schema::StringValidation {
                pattern: Some("^([0-9a-f]{40}|[0-9a-f]{64})$".to_owned()),
                ..Default::default()
            })),
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "A full commit id: 40 lowercase hex digits, or 64 in a SHA-256 repository."
                        .to_owned(),
                ),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

impl<'de> serde::Deserialize<'de> for CommitId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Self::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// A resolved commit identifier: a [`CommitId`], and the name it was
/// reached by when there was one.
///
/// `display` optionally preserves a tag/branch name when the value was
/// constructed from a tag-form input (e.g., `v0.3.4`). Equality compares
/// only the canonical SHA so two `ResolvedRevisionId`s referring to the
/// same commit — one tag-form, one SHA-form — compare equal.
///
/// Every constructor takes a [`CommitId`] or parses into one
/// ([`ResolvedRevisionId::from_rev_parse_output`]), so there is no way to
/// mint a `ResolvedRevisionId` from a free string; a recorded scalar that
/// may be a name is a [`RawRevisionId`] until it is resolved.
///
/// Serde: only `Serialize`, and it writes the commit id, never the
/// display form. What is serialized is read back as a pin, and a tag or
/// branch name can move or vanish after it is written. Deserialization
/// deliberately is not implemented; lock-file parsing yields
/// [`RawRevisionId`] which must then be resolved against the on-disk repo.
#[derive(Debug, Clone)]
pub struct ResolvedRevisionId {
    canonical: CommitId,
    display: Option<String>,
}

impl ResolvedRevisionId {
    /// Construct from a commit id and optional display form. When `display`
    /// spells the id itself it is suppressed.
    pub fn from_canonical(canonical: CommitId, display: Option<String>) -> Self {
        let display = display.filter(|d| d != canonical.as_str());
        Self { canonical, display }
    }

    /// Mint from the raw output of a ref-resolution command, **verifying**
    /// the canonical form this type exists to guarantee.
    ///
    /// Returns `None` when `s` is not a canonical commit id, so a value
    /// obtained this way is canonical by construction rather than by
    /// assertion. Callers resolving a fully-qualified ref (a savepoint, a
    /// backup) use this instead of re-running the value
    /// through [`Vcs::resolve_revision`]: it costs no extra VCS
    /// invocation and, unlike the assertion it replaces, it actually runs
    /// the check.
    ///
    /// No display form is preserved: the input is a ref name that resolved
    /// to this commit, not a name that identifies the commit anywhere else
    /// (`refs/rwv/pre-op/<op-id>` is meaningless outside the repo that
    /// holds it), so serializing it would produce an unresolvable scalar.
    pub fn from_rev_parse_output(s: &str) -> Option<Self> {
        CommitId::parse(s.trim()).ok().map(Self::from)
    }

    pub fn id(&self) -> &CommitId {
        &self.canonical
    }

    /// The display form (tag-form when present), falling back to the id.
    pub fn display_str(&self) -> &str {
        self.display.as_deref().unwrap_or(self.canonical.as_str())
    }

    /// The display form when present, falling back to the abbreviated id.
    pub fn short_display(&self) -> &str {
        self.display
            .as_deref()
            .unwrap_or_else(|| self.canonical.short())
    }
}

impl From<CommitId> for ResolvedRevisionId {
    fn from(canonical: CommitId) -> Self {
        Self {
            canonical,
            display: None,
        }
    }
}

/// A revision as operator text shows it: a commit id abbreviated as
/// [`CommitId::short`] abbreviates one, a name in full.
pub fn short_revision(text: &str) -> &str {
    if is_canonical_commit_id(text) {
        &text[..CommitId::SHORT_WIDTH]
    } else {
        text
    }
}

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
        self.canonical.serialize(serializer)
    }
}

// Deliberately no `Deserialize` impl: see the type-level docs. Lock-file
// scalars deserialize into `RawRevisionId`; the only way to obtain a
// `ResolvedRevisionId` is via path-rooted resolution.

/// A raw, unresolved revision identifier as it appears in a lock file.
///
/// `RawRevisionId` wraps the recorded scalar verbatim — it may be a tag name,
/// a branch name, or a 40-hex SHA, and at the type level we do not know
/// which. This is the only revision type that participates in lock-file
/// *parsing* (the [`serde::Deserialize`] entry point). It is intentionally
/// not interchangeable with [`ResolvedRevisionId`]: there is no `PartialEq`
/// between the two, and `RawRevisionId` cannot be fed to commit-id
/// operations such as `Vcs::advance_attached_ref`. To turn a raw value into a value
/// safe for SHA comparison, run it through
/// [`crate::manifest::LockFile::resolve_versions`] (which calls
/// [`Vcs::resolve_revision`] against the on-disk repo).
///
/// `Display`, `Serialize`, and `Eq` all operate on the string verbatim
/// ("same name"). Useful for "did this lock entry change name between two
/// reads"; not useful (and not provided) for "do these point at the same
/// commit".
///
/// # Compile-time invariant
///
/// A `RawRevisionId` cannot be compared against a `ResolvedRevisionId`.
/// The following doc-test fails to compile — this is the type system
/// enforcing the contract that motivated the split.
///
/// ```compile_fail
/// use repoweave::vcs::{CommitId, RawRevisionId, ResolvedRevisionId};
/// let raw = RawRevisionId::new("v1.0.0");
/// let resolved = ResolvedRevisionId::from_canonical(
///     CommitId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
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

    /// Whether the recorded string is a full commit id rather than a name
    /// (a tag, a branch, an abbreviated id) that resolves to wherever it
    /// points when read.
    pub fn is_commit_id(&self) -> bool {
        self.as_commit_id().is_some()
    }

    /// The recorded string as a [`CommitId`], when it is one.
    pub fn as_commit_id(&self) -> Option<CommitId> {
        CommitId::parse(&self.0).ok()
    }

    /// The recorded string as operator text shows it.
    pub fn short_display(&self) -> &str {
        short_revision(&self.0)
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

/// One commit in a [`Vcs::commits_beyond`] or [`Vcs::log`] listing — a
/// VCS-agnostic one-line summary.
///
/// `subject` is the first line of the commit message. The type carries no
/// git-specific spelling, so callers render the same shape regardless of the
/// underlying VCS.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommitSummary {
    pub id: CommitId,
    pub subject: String,
}

/// What a branch's unlanded commits are equivalent to on the branch they
/// land on: each commit with an equivalent there, paired with it, the
/// commits with none, and the commit that carries their whole change as one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PatchEquivalents {
    pub matched: Vec<(CommitId, CommitId)>,
    pub unmatched: Vec<CommitId>,
    pub combined: Option<CommitId>,
}

impl PatchEquivalents {
    /// Whether the backend found anything to name.
    pub fn names_any(&self) -> bool {
        !self.matched.is_empty() || self.combined.is_some()
    }
}

/// A key and value a commit records beside its message, read back by key
/// with [`Vcs::annotation_values`]. The value holds no line break.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub key: &'static str,
    pub value: String,
}

/// What copying one commit onto a checkout came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Copied {
    /// The new commit, which records the one it copies.
    Commit(CommitId),
    /// Its change was already on the checkout: nothing was committed.
    Empty,
    /// The copy stopped on a conflict in these paths, the checkout left in
    /// the VCS's own copy in progress for the operator to resolve and stage.
    Conflicted(Vec<String>),
}

/// A commit of a pushed range and the commits it joins: one for an ordinary
/// commit, several for a merge, the first the branch the merge was made on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeCommit {
    pub id: CommitId,
    pub parents: Vec<CommitId>,
}

/// The result of [`Vcs::diff_beyond`] — the unified diff of what a checkout's
/// HEAD adds to a base revision, anchored at their common ancestor.
///
/// `base` is the common-ancestor revision the diff is anchored at, returned so
/// callers can display it; it is `None` when no anchor could be computed.
/// `text` is the unified-diff body. Anchoring at the common ancestor — not the
/// base revision directly — is what keeps a base that has moved on since HEAD
/// forked from showing phantom reversals of work HEAD never had.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffBeyond {
    /// The common-ancestor revision the diff is anchored at, if computable.
    pub base: Option<CommitId>,
    /// The unified-diff text of what HEAD adds beyond `base`.
    pub text: String,
}

/// A range endpoint as the operator spelled it, and the commit it names in one
/// repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEndpoint {
    pub spelled: String,
    pub id: CommitId,
}

/// What [`Vcs::log`] read in one repo: the commits of the range, newest first,
/// and the commit each of its endpoints names there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeListing {
    pub endpoints: Vec<ResolvedEndpoint>,
    pub commits: Vec<CommitSummary>,
}

/// One action in one repo that a refusal can print as a choice: the backend
/// spells it, as a line the operator runs by hand.
///
/// A spelling may hold `<placeholder>` words, which core fills from what the
/// refusal judged and leaves standing where the operator chooses the value.
/// `<checkout>` is the repo's checkout directory, and `<b-checkout>` the
/// checkout of the same repo that has `<b>` checked out. No action may drop a commit
/// or an edit, or name a force, reset, delete, reflog or another hint's
/// commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VcsAction {
    AddAll,
    Commit,
    Stash,
    StashUntracked,
    StashIgnored,
    Status,
    MarkResolved,
    ShowFromBackup,
    ShowStagedFromBackup,
    Unstage,
    TakeBackSetAside,
    CompareMarked,
    CompareOutside,
    RemoveWorktree,
    BranchAt,
    SwitchTo,
    SwitchDetached,
    Detach,
    CherryPick,
    CherryPickOntoCheckedOut,
    CherryPickMainline,
    Revert,
    MergeNoFastForward,
    PushCommitAs,
    SetPushRemote,
    SetPushDefault,
    SetDefaultRemote,
    AddRemote,
    UnsetUpstream,
    SetCommitIdentity,
    TurnOnCheckoutRecording,
}

impl VcsAction {
    pub const ALL: [VcsAction; 31] = [
        Self::AddAll,
        Self::Commit,
        Self::Stash,
        Self::StashUntracked,
        Self::StashIgnored,
        Self::Status,
        Self::MarkResolved,
        Self::ShowFromBackup,
        Self::ShowStagedFromBackup,
        Self::Unstage,
        Self::TakeBackSetAside,
        Self::CompareMarked,
        Self::CompareOutside,
        Self::RemoveWorktree,
        Self::BranchAt,
        Self::SwitchTo,
        Self::SwitchDetached,
        Self::Detach,
        Self::CherryPick,
        Self::CherryPickOntoCheckedOut,
        Self::CherryPickMainline,
        Self::Revert,
        Self::MergeNoFastForward,
        Self::PushCommitAs,
        Self::SetPushRemote,
        Self::SetPushDefault,
        Self::SetDefaultRemote,
        Self::AddRemote,
        Self::UnsetUpstream,
        Self::SetCommitIdentity,
        Self::TurnOnCheckoutRecording,
    ];
}

/// What setting aside the uncommitted state a restore would discard must
/// take with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InTheWay {
    /// Tracked edits and untracked files.
    Edits,
    /// An ignored file as well.
    Ignored,
    /// An operation stopped there, or an index one left unresolved, which
    /// no stash sets aside: it is ended first.
    Unresolved,
}

/// The three sides a merge or replay recorded for a file it left conflicted:
/// the common ancestor's, the side of the branch checked out ("yours") and
/// the side it takes in ("incoming"), each `None` where that side has no
/// such file. In a replay, yours is the branch being replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictSides {
    pub base: Option<String>,
    pub yours: Option<String>,
    pub incoming: Option<String>,
}

/// In-flight VCS operation whose conflict needs human resolution.
///
/// Passed to [`Vcs::conflict_resolution_hint`] so sync's conflict-bail
/// messages embed VCS-appropriate "how do I resume this?" text without
/// hardcoding git vocabulary.
///
/// [`Vcs::mid_op`] constructs any variant from a repo's on-disk state, but a
/// given field can be narrower than the type: every
/// [`VcsError::RebaseConflict`] raised carries `Rebase`, so `Rebase` is the
/// only value that field will actually carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictOp {
    /// Native rebase (`git rebase`).
    ///
    /// The operator-facing resume path is `rwv sync --continue` /
    /// `rwv land --continue` — not bare `git rebase --continue`. The VCS
    /// hint for this variant stops at marking the files resolved (`git add
    /// <files>`); rwv core
    /// appends the `rwv <verb> --continue` line. Bare `git rebase --continue`
    /// resumes without rwv's hold on the lock merge driver, so the clone's
    /// own definition merges the lock of each remaining pick.
    Rebase,
    /// Merge (`git merge`) — resumes with `git merge --continue`.
    Merge,
    /// Cherry-pick (`git cherry-pick`) — resumes with `git cherry-pick --continue`.
    /// Used by sync's project-repo rebase-with-lock-exclusion path.
    CherryPick,
    /// Revert (`git revert`) — resumes with `git revert --continue`.
    Revert,
}

impl ConflictOp {
    pub const ALL: [Self; 4] = [Self::Rebase, Self::Merge, Self::CherryPick, Self::Revert];
}

impl std::fmt::Display for ConflictOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&wire_token(self))
    }
}

/// The operation a repository is stopped in, spelled `mid-<operation>`.
///
/// A conflict has a resume path and is a [`ConflictOp`]. A bisect has none, so
/// no conflict-resume code sees it, but it is operator state that a
/// repositioned HEAD would lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidOperation {
    Conflict(ConflictOp),
    Bisect,
}

impl MidOperation {
    const PREFIX: &'static str = "mid-";
    const BISECT: &'static str = "bisect";

    pub fn all() -> impl Iterator<Item = Self> {
        ConflictOp::ALL
            .into_iter()
            .map(Self::Conflict)
            .chain([Self::Bisect])
    }

    pub fn conflict(self) -> Option<ConflictOp> {
        match self {
            Self::Conflict(op) => Some(op),
            Self::Bisect => None,
        }
    }

    /// The operation's own name, `rebase` for a repo stopped mid-rebase.
    pub fn operation(self) -> String {
        match self {
            Self::Conflict(op) => op.to_string(),
            Self::Bisect => Self::BISECT.to_owned(),
        }
    }

    fn parse(token: &str) -> Option<Self> {
        let word = token.strip_prefix(Self::PREFIX)?;
        if word == Self::BISECT {
            return Some(Self::Bisect);
        }
        parse_wire_token(word).map(Self::Conflict)
    }
}

impl fmt::Display for MidOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(Self::PREFIX)?;
        match self {
            Self::Conflict(op) => write!(f, "{op}"),
            Self::Bisect => f.write_str(Self::BISECT),
        }
    }
}

impl Serialize for MidOperation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MidOperation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let token = String::deserialize(deserializer)?;
        Self::parse(&token).ok_or_else(|| {
            serde::de::Error::custom(format!("`{token}` is not a mid-operation token"))
        })
    }
}

impl JsonSchema for MidOperation {
    fn schema_name() -> String {
        "MidOperation".to_owned()
    }

    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            enum_values: Some(
                Self::all()
                    .map(|op| serde_json::Value::String(op.to_string()))
                    .collect(),
            ),
            metadata: Some(Box::new(schemars::schema::Metadata {
                description: Some(
                    "The operation a repository is stopped in: a conflict being resolved, or a bisect."
                        .to_owned(),
                ),
                ..Default::default()
            })),
            ..Default::default()
        }
        .into()
    }
}

/// Whether the tool a backend drives can carry rwv's operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolReadiness {
    Ready,
    /// The tool runs, and its release is below [`Vcs::minimum_release`].
    TooOld {
        installed: String,
    },
    /// The tool runs, and what it answered names no release.
    Unreadable {
        reported: String,
    },
    /// The tool could not be started: `cause` is what the OS said.
    Unrunnable {
        cause: String,
    },
}

/// Refuse unless every backend this binary can resolve has a tool it runs on.
///
/// Every backend, not only the ones a weave's manifests name: the project
/// repo has no manifest entry, and the check runs before anything is read.
pub(crate) fn refuse_unready_tools() -> anyhow::Result<()> {
    match crate::manifest::VcsType::Git {
        vcs_type @ crate::manifest::VcsType::Git => refuse_unready(vcs_for(vcs_type).as_ref()),
    }
}

fn refuse_unready(vcs: &dyn Vcs) -> anyhow::Result<()> {
    use crate::refusal::RefusalKind;
    let tool = vcs.name();
    let minimum = vcs.minimum_release();
    match vcs.tool_readiness() {
        ToolReadiness::Ready => Ok(()),
        ToolReadiness::TooOld { installed } => crate::refuse!(
            RefusalKind::UnsupportedVcsTool,
            "rwv needs {tool} {minimum} or newer, the oldest release with every command it \
             runs, and the `{tool}` on PATH is {installed}: install {tool} {minimum} or newer \
             and put it first on PATH"
        ),
        ToolReadiness::Unreadable { reported } => crate::refuse!(
            RefusalKind::UnsupportedVcsTool,
            "rwv needs {tool} {minimum} or newer, and the `{tool}` on PATH names no release \
             when asked its version (it answered {reported:?}): install {tool} {minimum} or \
             newer and put it first on PATH"
        ),
        ToolReadiness::Unrunnable { cause } => crate::refuse!(
            RefusalKind::MissingVcsTool,
            "rwv runs `{tool}`, and none could be started from PATH ({cause}): install {tool} \
             {minimum} or newer and put it on PATH"
        ),
    }
}

/// Resolve a VCS implementation from a [`crate::manifest::VcsType`].
pub(crate) fn vcs_for(vcs_type: crate::manifest::VcsType) -> Box<dyn Vcs> {
    match vcs_type {
        crate::manifest::VcsType::Git => crate::git::git_vcs(),
    }
}

/// Resolve the VCS backing `projects/<project>/`, which has no manifest entry
/// and therefore no [`crate::manifest::VcsType`] to resolve from.
///
/// It is git by construction: `rwv init` creates the project repo with `git
/// init` and `rwv clone` clones it with git, so no other backend can reach it.
/// Call this once per invocation at a verb's entry point and pass the handle
/// down; the frames below it must not re-resolve.
pub(crate) fn project_vcs() -> Box<dyn Vcs> {
    crate::git::git_vcs()
}

/// Resolve the backend for a repo path that has no manifest entry behind it.
///
/// Two situations reach here and they are the same situation from opposite
/// ends: repo discovery and the `rwv doctor` sweeps run *before* any manifest,
/// producing the set an entry could name; a path dropped from the lock has an
/// entry that is already gone. Neither has a `VcsType` to resolve from, so
/// both probe, and they probe for git. A second backend makes this a
/// probe-every-backend problem rather than a single handle, and that is the
/// limit this function exists to name.
pub(crate) fn probe_vcs() -> Box<dyn Vcs> {
    crate::git::git_vcs()
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
    /// witness's validity window *should* be is still open — this variant
    /// is what makes the narrow answer observable.)
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
    /// A MOVE of an already-detached HEAD refuses here: "rwv
    /// detached this HEAD at a lock SHA" and "the operator is mid-bisect /
    /// stopped at a `rebase -i` edit" are different situations, and only
    /// the first is rwv's to move.
    MidOperation {
        repo: PathBuf,
        operation: MidOperation,
    },
    /// A VCS hook refused an operation the VCS had otherwise completed.
    ///
    /// For git this is a `post-checkout` hook rejecting `worktree add`: the
    /// worktree and its registration are already written when the hook runs,
    /// so the destination is registered even though the command failed.
    /// `stderr` is the hook author's text, not the VCS's — git's own output
    /// names no hook, which is why this cannot be recognized by reading it.
    ///
    /// **The evidence is weaker than the name.** What the producer observes is
    /// that the add registered the destination and then failed, which says the
    /// command died *after* git wrote the admin entry rather than that a hook
    /// is what killed it. The registration must be one this call created: an
    /// add onto a destination that was already a registered worktree fails on
    /// the taken path with no hook involved, and reading only the after-state
    /// reports that as a rejection. That case is excluded and tested.
    ///
    /// What is not excluded is a checkout that dies of its own accord after
    /// git writes the entry. Nothing here has reproduced one — it is an
    /// unmeasured gap in the discriminator, not an observed behavior — so
    /// widen the evidence before relying on this variant to mean a hook
    /// specifically.
    ///
    /// From [`WorkspacePopulation::finish_checkout`] the evidence is direct:
    /// the hook is run on its own and its exit status is the failure.
    HookRejected { repo: PathBuf, stderr: String },
    /// A fast-forward or checkout would overwrite an untracked file that
    /// collides with a path the incoming tree writes.
    ///
    /// Git refuses this natively rather than clobbering it; `paths` is
    /// pulled out of git's own refusal so a caller can name the specific
    /// files instead of re-parsing [`VcsError::CommandFailed`] stderr itself.
    UntrackedCollision { repo: PathBuf, paths: Vec<String> },
    /// The checkout holds no record of where it last stood, so a move of its
    /// branch cannot be told from edits.
    ///
    /// `recording_off` is the line that turns the VCS's recording of it on,
    /// when the VCS is set never to record it.
    CheckoutUnrecorded {
        repo: PathBuf,
        recording_off: Option<String>,
    },
    /// The checkout's branch was moved from where the checkout last stood to
    /// a commit that does not contain it, by a move the checkout did not
    /// make, so what it holds cannot be told from edits.
    BranchRewritten {
        repo: PathBuf,
        stood_at: CommitId,
        head: CommitId,
    },
    /// The checkout took part of its branch's move by something its history
    /// does not show, and `paths` still hold the commit it moved from, which
    /// cannot be told from a return to that commit.
    MoveTakenInPart { repo: PathBuf, paths: Vec<String> },
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

/// Which [`VcsError`] variant a failure is, as the tag it travels under in
/// `--json` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VcsErrorKind {
    NotARepo,
    RevisionNotFound,
    RebaseConflict,
    StaleRefWitness,
    MidOperation,
    HookRejected,
    UntrackedCollision,
    CheckoutUnrecorded,
    BranchRewritten,
    MoveTakenInPart,
    Io,
    CommandFailed,
}

impl VcsErrorKind {
    pub const ALL: [Self; 12] = [
        Self::NotARepo,
        Self::RevisionNotFound,
        Self::RebaseConflict,
        Self::StaleRefWitness,
        Self::MidOperation,
        Self::HookRejected,
        Self::UntrackedCollision,
        Self::CheckoutUnrecorded,
        Self::BranchRewritten,
        Self::MoveTakenInPart,
        Self::Io,
        Self::CommandFailed,
    ];
}

impl fmt::Display for VcsErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&wire_token(self))
    }
}

impl VcsError {
    pub fn kind(&self) -> VcsErrorKind {
        match self {
            Self::NotARepo(_) => VcsErrorKind::NotARepo,
            Self::RevisionNotFound { .. } => VcsErrorKind::RevisionNotFound,
            Self::RebaseConflict { .. } => VcsErrorKind::RebaseConflict,
            Self::StaleRefWitness { .. } => VcsErrorKind::StaleRefWitness,
            Self::MidOperation { .. } => VcsErrorKind::MidOperation,
            Self::HookRejected { .. } => VcsErrorKind::HookRejected,
            Self::UntrackedCollision { .. } => VcsErrorKind::UntrackedCollision,
            Self::CheckoutUnrecorded { .. } => VcsErrorKind::CheckoutUnrecorded,
            Self::BranchRewritten { .. } => VcsErrorKind::BranchRewritten,
            Self::MoveTakenInPart { .. } => VcsErrorKind::MoveTakenInPart,
            Self::Io { .. } => VcsErrorKind::Io,
            Self::CommandFailed { .. } => VcsErrorKind::CommandFailed,
        }
    }
}

/// Wire-output mirror of [`VcsError`] for `--json` emission.
///
/// `VcsError` itself can't derive `Serialize` cleanly because tuple variants
/// (and `io::Error`) don't play nicely with serde's internally-tagged enum
/// representation. This struct-only mirror does: every variant carries
/// named fields, and a `From<&VcsError>` impl converts at JSON-emission time.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum VcsErrorOutput {
    NotARepo {
        path: crate::path_spelling::WirePath,
    },
    RevisionNotFound {
        repo: crate::path_spelling::WirePath,
        rev: String,
    },
    RebaseConflict {
        repo: crate::path_spelling::WirePath,
        op: ConflictOp,
    },
    StaleRefWitness {
        repo: crate::path_spelling::WirePath,
        expected: String,
        observed: String,
    },
    MidOperation {
        repo: crate::path_spelling::WirePath,
        operation: MidOperation,
    },
    HookRejected {
        repo: crate::path_spelling::WirePath,
        stderr: String,
    },
    UntrackedCollision {
        repo: crate::path_spelling::WirePath,
        paths: Vec<String>,
    },
    CheckoutUnrecorded {
        repo: crate::path_spelling::WirePath,
        recording_off: bool,
    },
    BranchRewritten {
        repo: crate::path_spelling::WirePath,
        stood_at: String,
        head: String,
    },
    MoveTakenInPart {
        repo: crate::path_spelling::WirePath,
        paths: Vec<String>,
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
        repo: crate::path_spelling::WirePath,
        stderr: String,
    },
}

impl From<&VcsError> for VcsErrorOutput {
    fn from(e: &VcsError) -> Self {
        match e {
            VcsError::NotARepo(p) => Self::NotARepo {
                path: crate::path_spelling::WirePath::of(p),
            },
            VcsError::RevisionNotFound { repo, rev } => Self::RevisionNotFound {
                repo: crate::path_spelling::WirePath::of(repo),
                rev: rev.clone(),
            },
            VcsError::RebaseConflict { repo, op } => Self::RebaseConflict {
                repo: crate::path_spelling::WirePath::of(repo),
                op: *op,
            },
            VcsError::StaleRefWitness {
                repo,
                expected,
                observed,
            } => Self::StaleRefWitness {
                repo: crate::path_spelling::WirePath::of(repo),
                expected: expected.clone(),
                observed: observed.clone(),
            },
            VcsError::MidOperation { repo, operation } => Self::MidOperation {
                repo: crate::path_spelling::WirePath::of(repo),
                operation: *operation,
            },
            VcsError::HookRejected { repo, stderr } => Self::HookRejected {
                repo: crate::path_spelling::WirePath::of(repo),
                stderr: stderr.clone(),
            },
            VcsError::UntrackedCollision { repo, paths } => Self::UntrackedCollision {
                repo: crate::path_spelling::WirePath::of(repo),
                paths: paths.clone(),
            },
            VcsError::CheckoutUnrecorded {
                repo,
                recording_off,
            } => Self::CheckoutUnrecorded {
                repo: crate::path_spelling::WirePath::of(repo),
                recording_off: recording_off.is_some(),
            },
            VcsError::BranchRewritten {
                repo,
                stood_at,
                head,
            } => Self::BranchRewritten {
                repo: crate::path_spelling::WirePath::of(repo),
                stood_at: stood_at.to_string(),
                head: head.to_string(),
            },
            VcsError::MoveTakenInPart { repo, paths } => Self::MoveTakenInPart {
                repo: crate::path_spelling::WirePath::of(repo),
                paths: paths.clone(),
            },
            VcsError::Io { ctx, source } => Self::Io {
                ctx: ctx.clone(),
                message: source.to_string(),
            },
            VcsError::CommandFailed { args, repo, stderr } => Self::CommandFailed {
                args: args.clone(),
                repo: crate::path_spelling::WirePath::of(repo),
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
            Self::HookRejected { repo, stderr } => write!(
                f,
                "a hook in {} rejected the operation: {}",
                repo.display(),
                stderr.trim()
            ),
            Self::UntrackedCollision { repo, paths } => write!(
                f,
                "{} has untracked file(s) that would be overwritten: {}. Move or remove \
                 them, then re-run.",
                repo.display(),
                paths.join(", ")
            ),
            Self::CheckoutUnrecorded {
                repo,
                recording_off: None,
            } => write!(
                f,
                "the VCS holds no record of where this checkout of {} last stood, so rwv \
                 cannot tell drift from edits; setting every difference aside makes the VCS \
                 record where the checkout stands and ends this; where every difference is \
                 yours, taking back what was set aside brings them back as edits",
                repo.display()
            ),
            Self::CheckoutUnrecorded {
                repo,
                recording_off: Some(turn_on),
            } => write!(
                f,
                "the VCS is set never to record where this checkout of {} stands, so rwv \
                 cannot tell drift from edits; run the {turn_on} to let it record it, after which \
                 setting every difference aside ends this, and taking back what was set aside \
                 brings the differences back as edits",
                repo.display()
            ),
            Self::BranchRewritten {
                repo,
                stood_at,
                head,
            } => write!(
                f,
                "the branch of {} was moved from {} to {}, a commit that does not contain {}, \
                 by a move this checkout did not make, so rwv cannot tell what it holds from \
                 edits; where none of the differences is yours, setting them aside leaves it at \
                 {} and ends this",
                repo.display(),
                stood_at.short(),
                head.short(),
                stood_at.short(),
                head.short()
            ),
            Self::MoveTakenInPart { repo, paths } => write!(
                f,
                "{} took part of its branch's move by something its history does not show, and \
                 these files still hold the commit it moved from, which rwv cannot tell from a \
                 return to it: {}; setting the differences aside leaves the checkout at HEAD \
                 and ends this",
                repo.display(),
                paths.join(", ")
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

/// Whether an op's own steps run in a checkout, so an operation stopped there
/// may be one it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Integrates {
    /// The op rebases, merges or copies here.
    Yes,
    /// The op never starts an operation here.
    No,
    /// The record that would say could not be read.
    #[default]
    Unknown,
}

/// A VCS operation an op journaled starting in a repo, by what it takes in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnStep {
    Merge {
        source: CommitId,
    },
    /// The commits `base..from` replayed onto `onto`.
    Rebase {
        base: CommitId,
        from: CommitId,
        onto: CommitId,
    },
    Copy {
        commit: CommitId,
    },
}

/// What an op recorded about one checkout: whether its own steps run there,
/// and which. It decides only whether a stopped operation is the op's to
/// cancel, never whether abort keeps what it discards.
#[derive(Debug, Clone, Copy, Default)]
pub struct Attribution<'a> {
    pub integrates: Integrates,
    /// The operations the op journaled starting here.
    pub own_steps: &'a [OwnStep],
}

/// Outcome of a [`Vcs::verified_restore_savepoint`] call.
///
/// Encodes the classification of the repo's current tip *before* any
/// restore happened, plus what (if anything) was restored. A foreign tip is
/// reported with a named violation so the caller can decline the destructive
/// action and surface recovery hints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifiedRestoreOutcome {
    /// No savepoint exists for the key in this repo. Nothing to do.
    NoSavepoint,
    /// The current tip equals the savepoint — the op never moved this
    /// repo. Restore is a no-op; HEAD was not touched.
    Untouched,
    /// The checkout was reset to its savepoint.
    Restored,
    /// The checkout was stopped in an operation the op started, or one the
    /// operator gave up by name: it was canceled, and `then` is the outcome
    /// of judging the tip it returned to.
    Canceled {
        stopped: ConflictOp,
        then: Box<VerifiedRestoreOutcome>,
    },
    /// The checkout is stopped in an operation the op never starts there,
    /// begun at the savepoint: it is left as it is and the savepoint dropped.
    LeftStopped { stopped: ConflictOp },
    /// The checkout is stopped in an operation abort would cancel, and the
    /// VCS refused to cancel it, saying `why`. Its edits, where any were kept
    /// first, are in the backup the restore reports.
    CancelRefused { stopped: ConflictOp, why: String },
    /// The checkout, at a tip abort may restore, is off the branch its
    /// savepoint names, and that branch cannot be checked out here: it is
    /// gone, or another checkout holds it. Nothing was touched.
    CannotSwitchBack {
        branch: RawRefName,
        savepoint: CommitId,
        /// The checkouts holding the branch; empty where it is gone.
        held_at: Vec<PathBuf>,
    },
    /// What the restore discarded was kept first, in the backup slots of the
    /// checkout and of the branch it reset; `then` is what the restore did.
    Kept {
        kept: Vec<BackupWitness>,
        then: Box<VerifiedRestoreOutcome>,
    },
    /// The checkout moved after abort planned its restore: nothing of it
    /// was touched.
    ForeignTip {
        /// The repo's current tip at the moment of classification.
        observed_tip: CommitId,
        /// The savepoint the op recorded at start.
        savepoint: CommitId,
        /// The converged tip recorded for this repo by the owner record,
        /// if any. `None` when the op crashed before relock recorded any
        /// converged tips, or when no entry exists for this repo.
        recorded_converged_tip: Option<CommitId>,
    },
    /// The checkout was on another branch than the one its savepoint names,
    /// or detached where it was not; it was put back where the savepoint was
    /// taken, leaving the branch it was on where it stands, and `then` is
    /// that restored checkout's own outcome.
    SwitchedBack {
        /// The branch the checkout was on, or `None` where it was detached.
        off: Option<RawRefName>,
        then: Box<VerifiedRestoreOutcome>,
    },
}

/// A branch's tip as it was before an operation moved that branch as a ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSavepoint {
    pub branch: RawRefName,
    pub at: CommitId,
}

/// What [`Vcs::execute_ref_restore`] did with one branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefRestoreOutcome {
    /// The branch was still at its savepoint.
    Untouched,
    /// The branch was at the tip the op wrote and was put back.
    Restored,
    /// The branch is somewhere the op did not put it, or gone, or checked
    /// out: it is left as it is and the savepoint kept.
    Foreign {
        observed: Option<CommitId>,
        why: String,
    },
    /// The branch held a commit nothing else reaches: its tip was kept in
    /// the branch's backup slot and the branch put back.
    Kept {
        tip: CommitId,
        backup: BackupWitness,
    },
}

/// What restoring a checkout to its savepoint would do, judged from what
/// the checkout holds before anything moves. Each step names the state it
/// expects, and executing it writes nothing where the checkout no longer
/// holds that state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutRestorePlan {
    NoSavepoint,
    /// Stopped in an operation the op never starts there: left as it is,
    /// its savepoint dropped.
    LeaveStopped {
        stopped: ConflictOp,
    },
    /// At its savepoint: the savepoint is dropped.
    Untouched {
        at: CommitId,
    },
    /// Move the checkout hard from `from` to its savepoint, `from` kept
    /// first where it holds a commit nothing else reaches.
    Reset {
        from: CommitId,
        savepoint: CommitId,
    },
    /// Cancel the operation the checkout is stopped in, its working copy,
    /// or the commits its HEAD holds that no branch reaches, kept in the
    /// checkout's backup slot first. Where the VCS would refuse the cancel,
    /// `clear_first` names the untracked paths in its way, removed with the
    /// working copy's edits once it is kept.
    Cancel {
        stopped: ConflictOp,
        clear_first: Option<Vec<String>>,
        then: Box<CheckoutRestorePlan>,
    },
    /// Put the checkout back where its savepoint was taken: onto `onto`, or
    /// detached at the savepoint where `onto` is `None`.
    SwitchBack {
        off: Option<RawRefName>,
        from: CommitId,
        onto: Option<RawRefName>,
        savepoint: CommitId,
        /// Whether the detached HEAD it leaves holds commits to keep.
        keep_detached: bool,
        then: Box<CheckoutRestorePlan>,
    },
    /// Something abort may not move past: the outcome that says what, with
    /// nothing of this checkout touched.
    Blocked(VerifiedRestoreOutcome),
}

impl CheckoutRestorePlan {
    /// What stops the plan, where something does.
    pub fn blocked(&self) -> Option<&VerifiedRestoreOutcome> {
        match self {
            Self::Blocked(outcome) => Some(outcome),
            Self::Cancel { then, .. } | Self::SwitchBack { then, .. } => then.blocked(),
            Self::NoSavepoint
            | Self::LeaveStopped { .. }
            | Self::Untouched { .. }
            | Self::Reset { .. } => None,
        }
    }

    /// The tip this plan moves the checkout to, where it moves it.
    pub fn restores_to(&self) -> Option<&CommitId> {
        match self {
            Self::Reset { savepoint, .. } | Self::SwitchBack { savepoint, .. } => Some(savepoint),
            Self::Cancel { then, .. } => then.restores_to(),
            Self::NoSavepoint
            | Self::LeaveStopped { .. }
            | Self::Untouched { .. }
            | Self::Blocked(_) => None,
        }
    }

    /// The branches this plan switches the checkout off.
    pub fn switches_off(&self) -> Vec<&RawRefName> {
        match self {
            Self::SwitchBack { off, then, .. } => off.iter().chain(then.switches_off()).collect(),
            Self::Cancel { then, .. } => then.switches_off(),
            _ => Vec::new(),
        }
    }
}

/// What restoring a branch moved as a ref to its savepoint would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefRestorePlan {
    /// At its savepoint: the savepoint is dropped.
    Untouched,
    /// Move the branch from `from` to its savepoint by compare-and-swap,
    /// keeping `from` in the branch's backup slot first where it holds a
    /// commit nothing else reaches.
    Restore { from: CommitId },
    /// Left as it is: gone, or checked out.
    Blocked {
        observed: Option<CommitId>,
        why: String,
    },
}

// ===========================================================================
// Backups
// ===========================================================================
//
// A backup is a ref an rwv verb makes before any step that would leave a
// commit or an edit unpreserved. It is named for the branch it backs up and
// an id, listed and dropped by name, and counts as a ref that reaches its
// commits wherever the design asks whether something is preserved. How a
// backend spells one is its own business: core holds [`BackupName`]s it was
// handed and never composes one.

/// What a backup says about the step that made it. An ordinary backup says
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BackupMark {
    /// Made before a rebase rewrote the branch.
    Rebased,
    /// The tip of a remote-tracking branch a fetch found rewritten.
    Superseded,
    /// The branch's one slot for the tip `rwv abort` last reset it from;
    /// the next abort that resets the branch overwrites it.
    Aborted,
    /// The checkout's one slot for the working copy `rwv abort` last
    /// discarded on it; the next abort that discards one there overwrites it.
    AbortedWorkingCopy,
}

/// What a backup backs up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupSource {
    /// A branch's tip, or the edits made on it.
    Branch(RawRefName),
    /// What a detached HEAD held, or the edits made on it.
    DetachedHead,
}

/// The name a backup is listed and dropped by: the branch it backs up and an
/// id, in the backend's spelling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BackupName(String);

impl BackupName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BackupName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A backup a repo holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    pub name: BackupName,
    pub tip: ResolvedRevisionId,
    pub mark: Option<BackupMark>,
    pub source: BackupSource,
}

/// A backup and the commits it keeps that nothing else does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedBackup {
    pub backup: Backup,
    /// The commits it reaches that no branch reaches, newest first.
    pub held: Vec<CommitSummary>,
}

impl ListedBackup {
    /// A branch reaches everything the backup holds, so dropping it loses
    /// nothing.
    pub fn redundant(&self) -> bool {
        self.held.is_empty()
    }
}

/// Something a repository's store holds whose commits no remote-tracking
/// branch reaches: removing the store removes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldAlone {
    pub kind: HeldKind,
    /// The name the backend gives it; empty for edits set aside.
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldKind {
    Branch,
    Tag,
    Backup,
    /// Uncommitted edits the VCS keeps aside from the working copy.
    SetAsideEdits,
    /// A savepoint an operation keeps until it ends.
    Savepoint,
    /// The commit a checkout stands on, on no branch.
    CheckedOut,
    Other,
}

impl fmt::Display for HeldAlone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = &self.name;
        match self.kind {
            HeldKind::Branch => write!(f, "branch {name}"),
            HeldKind::Tag => write!(f, "tag {name}"),
            HeldKind::Backup => write!(f, "backup {name}"),
            HeldKind::SetAsideEdits => f.write_str("edits set aside"),
            HeldKind::Savepoint => write!(f, "an operation's savepoint {name}"),
            HeldKind::CheckedOut => write!(f, "the checked-out commit {name}"),
            HeldKind::Other => f.write_str(name),
        }
    }
}

/// Which branches count as reaching a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTracking {
    /// Local branches and remote-tracking branches, as last fetched.
    Counted,
    /// Local branches only: the remote-tracking branches are not known to be
    /// current.
    Ignored,
}

/// A remote a pruning fetch could not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnconfirmedRemote {
    pub remote: String,
    pub cause: String,
}

/// A remote of a repo, as its backend names one.
///
/// Core receives a handle from the method that resolved a role and gives it
/// back to the methods that act on it; it never spells one. Rendering a handle
/// into an operator message is the one use of its name, so it displays and
/// does nothing else: it has no `as_str`, no constructor outside the backends
/// and no comparison with text.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoteHandle(String);

impl RemoteHandle {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub(crate) fn name(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RemoteHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a repo stands to the remote that has a url.
#[derive(Debug, Clone)]
pub enum UrlRemote {
    /// A remote has the url.
    Present(RemoteHandle),
    /// No remote has the url and the name one would take is free:
    /// [`Vcs::add_url_remote`] adds it.
    Absent(AbsentRemote),
    /// No remote has the url, and the name one would take holds another.
    NameTaken(RemoteNameTaken),
}

/// A remote [`Vcs::url_remote`] found missing, and the name it would take.
#[derive(Debug, Clone)]
pub struct AbsentRemote {
    remote: RemoteHandle,
    url: CloneUrl,
}

impl AbsentRemote {
    pub(crate) fn new(remote: RemoteHandle, url: CloneUrl) -> Self {
        Self { remote, url }
    }

    pub(crate) fn remote(&self) -> &RemoteHandle {
        &self.remote
    }

    pub(crate) fn url(&self) -> &CloneUrl {
        &self.url
    }
}

impl fmt::Display for AbsentRemote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} for {}", self.remote, self.url)
    }
}

/// The name a url remote would take is held by a remote with another url.
#[derive(Debug, Clone)]
pub struct RemoteNameTaken {
    pub remote: RemoteHandle,
    /// The url the remote holds, as recorded.
    pub holds: String,
    /// The url the remote was wanted for.
    pub wanted: CloneUrl,
}

/// One remote of a repo and the url it records, as configured: not as a
/// rewrite rule resolves it for a transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedUrl {
    pub remote: RemoteHandle,
    pub url: String,
}

/// The remote branch a branch takes in from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    pub remote: RemoteHandle,
    /// The branch on that remote.
    pub branch: RawRefName,
}

/// A remote's copy of a branch, as last fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteTip {
    pub remote: RemoteHandle,
    pub tip: ResolvedRevisionId,
}

/// How a branch tip stands to another, by ancestry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// The same commit.
    Even,
    /// The tip is an ancestor of the other.
    Behind,
    /// The other is an ancestor of the tip.
    Ahead,
    /// Neither is an ancestor of the other.
    Diverged,
}

/// A branch of a repo and where it stands, local or remote-tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchTip {
    /// The branch as the backend spells it for an operator.
    pub label: String,
    pub branch: crate::branch_ref::BranchRef,
    pub tip: ResolvedRevisionId,
}

/// A file of a committed tree: its path below the tree's root, `/`-separated,
/// and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedFile {
    pub path: String,
    pub content: Vec<u8>,
}

/// A change to one file of a tree: its new bytes, or `None` to remove it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub content: Option<Vec<u8>>,
}

/// What a replay made of the commits it replayed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Replayed {
    /// Each commit replayed, oldest first, with the commit it became; `None`
    /// where the replay dropped it.
    pub images: Vec<(CommitId, Option<CommitId>)>,
    /// Each commit the replay wrote, oldest first, those that are no
    /// commit's image included.
    pub written: Vec<CommitId>,
}

impl Replayed {
    /// The commit `old` became; `None` where it was dropped or not replayed.
    pub fn image(&self, old: &CommitId) -> Option<&CommitId> {
        self.images
            .iter()
            .find(|(o, _)| o == old)
            .and_then(|(_, new)| new.as_ref())
    }

    /// The commit replayed into `new`; `None` for one no commit became.
    pub fn original(&self, new: &CommitId) -> Option<&CommitId> {
        self.images
            .iter()
            .find(|(_, n)| n.as_ref() == Some(new))
            .map(|(old, _)| old)
    }
}

/// A tree of files, named by the backend's object name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeId(String);

impl TreeId {
    pub(crate) fn from_object_name(name: &str) -> Self {
        Self(name.trim().to_owned())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// How a merge computed as a tree ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeMerge {
    Clean(TreeId),
    Conflicted { tree: TreeId, paths: Vec<String> },
}

/// How a merge computed with no checkout and no ref moved ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefMerge {
    Merged(CommitId),
    Conflicted { paths: Vec<String> },
}

/// Merges a backend computes as trees, without a checkout. A backend answers
/// these through [`Vcs::tree_merges`] or not at all; where it does not, the
/// merge runs in a checkout, stopped before its commit.
pub trait TreeMerges: Send + Sync {
    /// The merge of `second` into `first`, as a tree; a conflict names its
    /// paths and writes nothing a checkout could keep.
    fn merge_tree(
        &self,
        repo: &Path,
        first: &CommitId,
        second: &CommitId,
    ) -> Result<TreeMerge, VcsError>;

    /// The content of `path` in `tree`, `None` where the tree has none.
    fn file_in_tree(
        &self,
        repo: &Path,
        tree: &TreeId,
        path: &str,
    ) -> Result<Option<String>, VcsError>;
}

/// Where the records branch is read: this repo's own, or a remote's as last
/// fetched.
#[derive(Debug, Clone, Copy)]
pub enum RecordsSite<'a> {
    Local,
    Remote(&'a RemoteHandle),
}

/// How a compare-and-swap of the local records branch ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordsAdvance {
    /// The branch held the tip the caller expected and now holds the new one.
    Advanced,
    /// The branch held another tip, and nothing was written.
    Lost,
}

/// How publishing a records commit to a remote ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordsPublish {
    /// The remote's records branch now holds the commit.
    Published,
    /// The remote's records branch moved past the tip the commit was built
    /// on, and nothing was written.
    RemoteMoved,
    /// The remote refused the records branch, for the reason it gave.
    Rejected(String),
}

/// A branch of a repo, local or only on a remote, with the tip a reader of its
/// files uses: the local branch's, else a remote-tracking branch's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedBranch {
    pub name: String,
    pub tip: CommitId,
    pub local: bool,
}

/// Proof that a backup has been written.
///
/// Minted only by [`Vcs::create_backup`], whose body is in this module — so a
/// value of this type is proof the ref exists on disk, not a claim that one
/// will be written. [`DestroyProof::backed_up`] takes the proof by running the
/// backup itself, which is what makes a discard without one unrepresentable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupWitness {
    repo: PathBuf,
    backup: Backup,
}

impl BackupWitness {
    /// The repo holding the backup.
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    pub fn backup(&self) -> &Backup {
        &self.backup
    }
}

// ===========================================================================
// The branch model
// ===========================================================================
//
// A ref name means one of three notions: the branch a manifest entry DECLARES
// it tracks, the ephemeral name a create REQUESTS, and the ref a checkout is
// actually ON. Comparing any two of them is a legal line that is usually wrong,
// so each has its own type.
//
// The split is the same move `ResolvedRevisionId` / `RawRevisionId` make in
// this file: one parse boundary, one refined value per notion, no cross-type
// comparison, and a `compile_fail` doctest per invariant so a later "make it
// easier" `PartialEq`/`From` impl fails CI rather than passing review. A name
// read from the VCS that none of the three claims stays a `RawRefName`.
//
// Destroying a branch is a fourth thing, and takes no name at all: a
// `DestroyProof` carries the branch, its store and the tip it was proved
// about, and is minted only by a check that ran.

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
/// ([`TrackingRef`], [`AttachedRef`]) are exactly the ones
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

pub use crate::naming::RefNameError;

/// Width at which an all-hex name stops reading as a branch name and
/// starts reading as an abbreviated commit id.
///
/// git's default `core.abbrev` floor is 7, so a 7-hex `branch` is
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
/// rejects names of this shape (`branch` is a declaration, not a pin), and
/// git's [`Vcs::tag_at_head`] uses it as a tiebreaker so a release
/// tag wins over an arbitrary lightweight tag. Both are asking
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

/// Notion (1): the branch a manifest entry **declares** it tracks
/// (`branch`).
///
/// A `TrackingRef` is a statement of intent about a *remote* channel. It is
/// not a claim that any local ref of that name exists, and it is not a
/// revision — [`TrackingRef::parse`] refuses commit-id-shaped and
/// tag-shaped input for exactly that reason.
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(try_from = "RawRefName")]
pub struct TrackingRef(String);

impl TrackingRef {
    /// Parse a manifest-declared `branch` scalar.
    ///
    /// Rejects commit-id-shaped and release-tag-shaped values, and anything
    /// that is not a usable ref name. Deserializing and every `TryFrom` impl
    /// go through here.
    pub fn parse(raw: RawRefName) -> Result<Self, RefNameError> {
        let s = raw.as_str();
        crate::naming::validate_ref_name(s)?;
        if is_sha_shaped(s) {
            return Err(RefNameError::ShaShaped(s.to_owned()));
        }
        if is_release_shape_name(s) {
            return Err(RefNameError::TagShaped(s.to_owned()));
        }
        Ok(Self(raw.0))
    }

    /// The remote branch this declaration names.
    ///
    /// The projection stops at the branch: which *remote* carries it is a
    /// VCS convention, and naming it here would put `origin` on the domain
    /// side of the seam.
    pub fn on_remote(&self) -> RemoteRef {
        RemoteRef {
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

impl TryFrom<RawRefName> for TrackingRef {
    type Error = RefNameError;

    fn try_from(raw: RawRefName) -> Result<Self, Self::Error> {
        Self::parse(raw)
    }
}

impl TryFrom<&AttachedRef> for TrackingRef {
    type Error = RefNameError;

    fn try_from(attached: &AttachedRef) -> Result<Self, Self::Error> {
        Self::parse(attached.name.clone())
    }
}

impl TryFrom<&RemoteDefaultBranch> for TrackingRef {
    type Error = RefNameError;

    fn try_from(remote_default: &RemoteDefaultBranch) -> Result<Self, Self::Error> {
        Self::parse(RawRefName(remote_default.0.clone()))
    }
}

impl Serialize for TrackingRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl fmt::Display for TrackingRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A branch in the remote namespace, carrying no remote name.
///
/// Produced by [`TrackingRef::on_remote`]. Which remote holds it is the
/// backend's to decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef {
    branch: String,
}

impl RemoteRef {
    /// The branch name on the remote.
    pub fn branch(&self) -> &str {
        &self.branch
    }
}

impl fmt::Display for RemoteRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} on the remote", self.branch)
    }
}

/// What a member's lock entry came to when its checkout was placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinOutcome {
    /// The checkout is at the commit the lock pins.
    Reached,
    /// The lock holds no entry for the member: the checkout sits where the
    /// declared branch is.
    None,
    /// The lock pins a commit no ref in the clone reaches: the checkout sits
    /// where the declared branch is.
    Unreachable,
}

/// A checkout [`Vcs::clone_attached_at`] created: where it sits, and what
/// became of the pin it was asked to start at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberBirth {
    pub at: ResolvedRevisionId,
    pub pin: PinOutcome,
}

/// What a pruning fetch did, and what the VCS said while it did it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruningFetch {
    pub moves: Vec<RemoteBranchMove>,
    /// The remotes the fetch completed.
    pub fetched: Vec<RemoteHandle>,
    /// The VCS's own progress report, for a caller that relays it.
    pub log: String,
    /// The remotes the fetch could not complete. The others are fetched and
    /// pruned all the same.
    pub unconfirmed: Vec<UnconfirmedRemote>,
}

/// What a pruning fetch did to one remote-tracking branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBranchMove {
    /// The branch as the repo spells it remote-qualified (git: `origin/main`).
    pub branch: String,
    pub change: BranchChange,
}

/// How a remote-tracking branch moved in a pruning fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchChange {
    /// The remote has the branch and the repo had no record of it.
    Created { to: String },
    /// The new tip descends from the old one.
    Advanced { from: String, to: String },
    /// The new tip does not descend from the old one. The old tip is kept as
    /// the branch's superseded tip.
    Rewritten { from: String, to: String },
    /// The remote no longer has the branch. Nothing is kept: a prune
    /// supersedes nothing.
    Pruned { from: String },
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
    /// The local branch the engine found for a member, named at the point it
    /// attaches a checkout to it.
    pub fn of_branch(found: &RawRefName) -> Self {
        Self(found.as_str().to_owned())
    }

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

/// Notion (2): the ephemeral branch name a create **requests**.
///
/// [`EphemeralRefName::mint`] is total and takes one input, the workweave's
/// [`FlatAddress`]: the rendering of its (project, workweave) identity. There
/// is no third component: the three derivation sites in the tree disagreed
/// about what it should be, no consumer read it, and it was deleted rather
/// than a winner picked. Nothing observed can be fed in — a `FlatAddress`
/// has one producer, [`crate::naming::WorkweaveId::flat`] — so a name can never be derived
/// from the branch a checkout happens to be on.
///
/// `mint` itself performs no validation and cannot fail:
/// [`crate::manifest::ProjectName::new`] and
/// [`crate::manifest::WorkweaveName::new`] already reject anything that would make two
/// distinct (project, workweave) pairs mint the same name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EphemeralRefName(String);

impl EphemeralRefName {
    /// Mint the ephemeral name for the workweave `flat` renders.
    ///
    /// Total: no other input, no failure, no read of the current ref.
    pub fn mint(flat: &FlatAddress) -> Self {
        Self(flat.as_str().to_owned())
    }

    /// The branch a create was told to put the workweave on, rather than the
    /// name its identity mints.
    pub fn chosen(branch: &str) -> Self {
        Self(branch.to_owned())
    }

    /// The requested name at the parse boundary, for the VCS impl to spell.
    ///
    /// Deliberately not `as_str()`: handing back a [`RawRefName`] grants
    /// nothing `RawRefName::new` did not already grant — a raw name can
    /// destroy nothing.
    pub fn to_raw(&self) -> RawRefName {
        RawRefName(self.0.clone())
    }
}

impl fmt::Display for EphemeralRefName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The branch a checkout is placed on: its name, the store it lives in, and
/// the revision it starts at when it is created.
///
/// Carries its store, so a birth derives its target from the branch rather
/// than from an independent path argument — the same carry-your-provenance
/// rule [`AttachedRef`] follows, for the same reason.
///
/// Display only, deliberately: no `as_str()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundBranch {
    store: PathBuf,
    name: RawRefName,
    start: ResolvedRevisionId,
}

impl FoundBranch {
    /// The branch the workweave `name` mints, in `store`, created at `start`
    /// when it does not exist.
    pub fn minted(store: PathBuf, name: &EphemeralRefName, start: ResolvedRevisionId) -> Self {
        Self {
            store,
            name: name.to_raw(),
            start,
        }
    }

    #[cfg(test)]
    pub(crate) fn named(store: PathBuf, name: &str, start: ResolvedRevisionId) -> Self {
        Self {
            store,
            name: RawRefName::new(name),
            start,
        }
    }

    pub fn store(&self) -> &Path {
        &self.store
    }

    pub fn start(&self) -> &ResolvedRevisionId {
        &self.start
    }

    pub(crate) fn name(&self) -> &RawRefName {
        &self.name
    }
}

impl fmt::Display for FoundBranch {
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
/// stale witness. How wide that window should be stays open.
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
    /// name as a string. Every gate that asks whether a checkout sits on the
    /// branch some projection names goes through this, against whichever
    /// `local_counterpart` that projection yields.
    pub fn is_named(&self, name: &LocalRefName) -> bool {
        self.name.as_str() == name.as_str()
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
/// be passed to [`Vcs::advance_attached_ref`] — the same refusal to collapse
/// distinct states that splits [`HeadAttachment`], applied one level in.
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

/// Whether a worktree birth wrote its branch or took one that was already
/// there.
///
/// Rollback destroys a branch only when the birth authored it, so the two
/// outcomes are named rather than left to a bool whose `true` an
/// implementation could read the other way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefOrigin {
    /// This call created the branch.
    Authored,
    /// A branch of that name already existed, and the worktree was put on it.
    Adopted,
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
/// fallback** — a guessed name here would let a caller compare an
/// observation against an invention instead of refusing and saying the
/// remote's HEAD is unset. *Which* ref publishes, and where a non-default
/// channel's identity is recorded, is policy and stays open.
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
        if branch.is_empty() || crate::naming::validate_ref_name(branch).is_err() {
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

/// What a publish sends: the commit that was judged, to the branch of that
/// name on the remote. The local branch may have moved since; the commit is
/// what goes out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishRef {
    name: RawRefName,
    commit: ResolvedRevisionId,
}

impl PublishRef {
    /// Publish `commit` as the local branch `l`'s push branch.
    pub(crate) fn judged(l: &LocalRefName, commit: ResolvedRevisionId) -> Self {
        Self {
            name: RawRefName(l.0.clone()),
            commit,
        }
    }

    /// The branch name the remote receives it under.
    pub(crate) fn name(&self) -> &RawRefName {
        &self.name
    }

    pub(crate) fn commit(&self) -> &ResolvedRevisionId {
        &self.commit
    }
}

impl fmt::Display for PublishRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}", self.name.as_str(), self.commit.id().short())
    }
}

// ---------------------------------------------------------------------------
// "No current branch" is not one state
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

/// How current a clone's copies of its remote's branches are known to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteBranchCopies {
    /// Made by the clone this invocation just ran.
    JustCloned,
    /// Whatever the clone last fetched, which may be nothing at all.
    AsLastFetched,
}

/// What HEAD is, in a workspace that is **known to be a repo**.
///
/// The `current_ref` this replaced (now deleted) collapsed four distinct
/// conditions into a single `Ok(None)` — on a branch, unborn, detached, and
/// not-a-repo-at-all — which is how `rwv push` came to report "is on a
/// detached HEAD" for a directory that was not a repo. Two of the four are
/// errors rather than
/// states ([`VcsError::NotARepo`], [`VcsError::CommandFailed`]), so
/// [`Vcs::head_attachment`] is total over the remaining three and every
/// caller's `match` is exhaustive. The value that meant all four does not
/// exist.
/// What one of a path's index entry or file holds, against `HEAD` and the
/// tip the checkout's branch moved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holds {
    /// `HEAD`'s content, or its absence where `HEAD` lacks the path.
    Head,
    /// The moved-from tip's content, or its absence where that tip lacks the
    /// path, and not `HEAD`'s.
    LeftTip,
    /// Anything else, a conflicted entry and an unreadable one included.
    Other,
}

/// What a drift refresh writes for a path: its file alone, or its record
/// entry alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshScope {
    FilesOnly,
    RecordOnly,
}

/// What one drift refresh write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshWrite {
    Written,
    /// Nothing was written to the index: a path no longer held what it was
    /// read holding, something stood in the way of a file, or the VCS could
    /// not write. `vcs_said` is the VCS's own account, for the operator only.
    /// A failure while writing files may have written some before it stopped.
    Declined {
        vcs_said: String,
    },
    /// Nothing was written: the index holds unresolved conflicts.
    Unresolved,
}

/// A checkout rwv pointed at the tip its files hold, off the branch it was
/// on, so that only the edits differ from its HEAD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetBack {
    pub at: CommitId,
    pub branch: String,
}

/// What a VCS command did that the VCS may refuse: its result, or its own
/// account of the refusal, nothing having been written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusable<T> {
    Done(T),
    Refused(String),
}

/// What the merge of edits onto a moved tip does with each edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditResolution {
    /// Merge it onto the moved tip.
    Merge,
    /// Keep it in the VCS's set-aside record, the file taking the moved
    /// tip's content.
    SetAside,
}

/// Paths the VCS does not compare with what it records: `ignored` files its
/// index lacks, and entries marked `assumed` unchanged or `skipped` as
/// outside the working copy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UncomparedPaths {
    pub ignored: Vec<String>,
    pub assumed: Vec<String>,
    pub skipped: Vec<String>,
}

impl UncomparedPaths {
    pub fn is_empty(&self) -> bool {
        self.ignored.is_empty() && self.assumed.is_empty() && self.skipped.is_empty()
    }

    pub fn marked(&self) -> Vec<String> {
        let mut marked = self.assumed.clone();
        for path in &self.skipped {
            if !marked.contains(path) {
                marked.push(path.clone());
            }
        }
        marked
    }
}

/// One path of a checkout whose index entry or file differs from `HEAD`, or
/// that the move under it changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStanding {
    pub path: String,
    /// The moved-from tip and `HEAD` differ at this path.
    pub moved: bool,
    pub index: Holds,
    pub file: Holds,
    /// The VCS compares this path's file with its recorded entry: it has one,
    /// or no file stands there, and nothing marks it to be skipped.
    pub compared: bool,
}

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
// Consent and proof tokens
// ---------------------------------------------------------------------------
//
// The consent tokens live in `crate::cli::consent` — the CLI layer's flag
// module, which is the only place that can construct them. This module
// takes each as an opaque parameter and never constructs one, and that is
// not a promise a reviewer has to keep: both of their construction routes
// are sealed against this module by the compiler (private field, and a mint
// visible only within `crate::cli`), so a mint written here does not build.
// See that module's doc comment.
//
// The proof types below stay in this module: they are minted from checks
// this module runs (`DestroyProof::landed`), or from a backup this module
// writes (`DestroyProof::backed_up`).
//
// House rule: escape hatches are named for the precondition they waive,
// never a bare `--force`.

/// Whether a push may replace a remote tip that is not an ancestor of the one
/// it pushes.
#[derive(Debug, Clone)]
pub enum Replace {
    FastForwardOnly,
    /// Replaces the push branch only while it still holds the lease's tip.
    Leased(Lease),
}

/// A savepoint that has actually been written.
///
/// Minted only by [`Vcs::create_savepoint_ref`], whose body is in this
/// module — so a value of this type is proof the ref exists on disk, not a
/// claim that one will be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavepointRef {
    repo: PathBuf,
    key: SavepointKey,
    at: ResolvedRevisionId,
}

impl SavepointRef {
    /// The repo whose tip was captured.
    pub fn repo(&self) -> &Path {
        &self.repo
    }

    /// The key the savepoint is filed under.
    pub fn key(&self) -> &SavepointKey {
        &self.key
    }

    /// The captured tip — the revision `rwv abort` restores to.
    pub fn at(&self) -> &ResolvedRevisionId {
        &self.at
    }
}

/// What a repo's savepoint namespace holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavepointListing {
    /// The savepoints rwv filed.
    pub keys: Vec<SavepointKey>,
    /// Refs under the namespace that no key names, spelled as the backend
    /// spells a reference. rwv filed none of them.
    pub unrecognized: Vec<String>,
}

/// Where a branch is destroyed: in the repo's own refs, or on a remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Site {
    Local,
    Remote(RemoteHandle),
}

/// Why a branch may be destroyed now: every commit on it is reached by a ref
/// that outlives it (**landed**), or a backup of it was made first.
///
/// The proof names the branch, the store it lives in and the tip it was
/// proved about, and [`Vcs::delete_branch`] takes nothing else, so there is
/// no way to spell "delete this branch I recognized". A branch that has moved
/// since the proof was made is not deleted.
///
/// An opaque struct over a private enum, not a `pub enum`: Rust cannot make a
/// public enum's variant constructors private, so a public enum would let any
/// code in the crate write the landed variant without running the ancestry
/// check it certifies. The constructors below **run** the check.
#[derive(Debug)]
pub struct DestroyProof {
    site: Site,
    store: PathBuf,
    branch: RawRefName,
    tip: ResolvedRevisionId,
    ground: Ground,
}

#[derive(Debug)]
enum Ground {
    Landed { onto: ResolvedRevisionId },
    BackedUp { backup: BackupWitness },
}

impl DestroyProof {
    fn tip_at(
        vcs: &dyn Vcs,
        store: &Path,
        site: &Site,
        branch: &RawRefName,
    ) -> Result<Option<ResolvedRevisionId>, VcsError> {
        match site {
            Site::Local => vcs.resolve_local_branch_tip(store, branch),
            Site::Remote(remote) => Ok(vcs
                .remote_copies(store, branch)?
                .into_iter()
                .find(|copy| copy.remote == *remote)
                .map(|copy| copy.tip)),
        }
    }

    /// `Some` iff the branch exists at `site` and its tip is an ancestor of
    /// one of `onto`: every commit on it is reached by a name that outlives it.
    pub fn landed(
        vcs: &dyn Vcs,
        store: &Path,
        site: Site,
        branch: &RawRefName,
        onto: &[ResolvedRevisionId],
    ) -> Result<Option<Self>, VcsError> {
        let Some(tip) = Self::tip_at(vcs, store, &site, branch)? else {
            return Ok(None);
        };
        for target in onto {
            if vcs.is_ancestor(store, &tip, target)? {
                return Ok(Some(Self {
                    site,
                    store: store.to_path_buf(),
                    branch: branch.clone(),
                    tip,
                    ground: Ground::Landed {
                        onto: target.clone(),
                    },
                }));
            }
        }
        Ok(None)
    }

    /// [`Self::landed`] onto named branches. A branch of the destroyed
    /// branch's own name, at any site, is never what it landed on.
    pub fn landed_on(
        vcs: &dyn Vcs,
        store: &Path,
        site: Site,
        branch: &RawRefName,
        onto: &[(crate::branch_ref::BranchRef, ResolvedRevisionId)],
    ) -> Result<Option<Self>, VcsError> {
        let others: Vec<ResolvedRevisionId> = onto
            .iter()
            .filter(|(named, _)| named.name() != branch)
            .map(|(_, tip)| tip.clone())
            .collect();
        Self::landed(vcs, store, site, branch, &others)
    }

    /// The branch's tip is kept as a backup, and the proof rests on that
    /// backup. `consent` is not otherwise inspected: its field is private to
    /// `cli::consent`, so holding one is the operator's say-so.
    pub fn backed_up(
        vcs: &dyn Vcs,
        store: &Path,
        site: Site,
        branch: &RawRefName,
        consent: DiscardUnlandedConsent,
    ) -> Result<Self, VcsError> {
        let _ = consent;
        let tip =
            Self::tip_at(vcs, store, &site, branch)?.ok_or_else(|| VcsError::RevisionNotFound {
                repo: store.to_path_buf(),
                rev: branch.as_str().to_owned(),
            })?;
        let backup = vcs.create_backup(store, &BackupSource::Branch(branch.clone()), &tip, None)?;
        Ok(Self {
            site,
            store: store.to_path_buf(),
            branch: branch.clone(),
            tip,
            ground: Ground::BackedUp { backup },
        })
    }

    pub(crate) fn site(&self) -> &Site {
        &self.site
    }

    pub(crate) fn store(&self) -> &Path {
        &self.store
    }

    pub(crate) fn branch(&self) -> &RawRefName {
        &self.branch
    }

    pub(crate) fn tip(&self) -> &ResolvedRevisionId {
        &self.tip
    }

    /// The backup this proof rests on, when it rests on one.
    pub fn backup(&self) -> Option<&BackupWitness> {
        match &self.ground {
            Ground::BackedUp { backup } => Some(backup),
            Ground::Landed { .. } => None,
        }
    }

    pub fn describe(&self) -> String {
        match &self.ground {
            Ground::Landed { onto } => format!("landed on {}", onto.display_str()),
            Ground::BackedUp { backup } => {
                format!("kept as backup {}", backup.backup().name)
            }
        }
    }
}

// ===========================================================================
// Derived content
// ===========================================================================
//
// Some tracked paths are not authored, they are *derived*: regenerated from
// a source of record rather than edited by hand. `rwv.lock` is derived from
// the manifest tips; a repo's generated reference material is derived from
// its own sources. Carrying one side's edit to such a path across a replay
// is noise — the content that ships is whatever the generator next produces,
// and the repo's own blocking gates are what force it to run.
//
// The mechanism has two halves, and they live in different places on
// purpose:
//
//   - WHICH paths are derived is the repo's own declaration, recorded in
//     tracked metadata so it travels with a clone (for git: `.gitattributes`
//     — see [`Vcs::lock_driver_setup`]). Per-repo, self-contained.
//   - HOW a derived path resolves is a definition the declaration only names
//     and cannot carry. rwv is the carrier, supplying it per operation.
//
// `DerivedContentPolicy` is that supply, named and typed. It is a parameter
// on every seam operation that can honor it, so which operations do is
// visible in the signature rather than buried in an impl — and a call site
// picks a resolution by name instead of spelling one VCS's flags.
//
// The resolution is a deterministic side-pick and nothing else: no generator
// runs while an operation is in flight, no clock or environment is read. A
// resolver that regenerated mid-replay would make the resolved content
// depend on the machine that happened to run it.

/// How an operation that replays or merges content resolves the paths a repo
/// has declared **derived**.
///
/// Values name a resolution; they do not describe a mechanism. A VCS impl
/// translates the one it is handed into whatever its own merge machinery
/// takes (git: an inline merge-driver
/// definition), which is what keeps the choice spellable by callers that
/// know nothing about git.
///
/// # Minted here, spelled nowhere else
///
/// The field is private and the resolution vocabulary is crate-internal:
/// the only values that exist are the ones the constructors below name.
/// A caller states which resolution it wants; it cannot assemble one, and
/// it cannot reach past the seam to the flags that implement one.
///
/// ```compile_fail
/// use repoweave::vcs::DerivedContentPolicy;
/// // E0603/E0616: the resolution vocabulary is private to `vcs`.
/// let _ = DerivedContentPolicy(repoweave::vcs::DerivedContentResolution::KeepTargetSide);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedContentPolicy(DerivedContentResolution);

/// The closed set of resolutions rwv defines for derived content.
///
/// Crate-internal: a VCS impl matches on it to translate, and the match is
/// exhaustive, so adding a resolution is a compile error in every impl until
/// that impl says what the new one means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DerivedContentResolution {
    /// rwv supplies nothing. A declaration that names a resolver finds none
    /// defined *by this operation*, and the VCS resolves the path the way it
    /// resolves any other content — for git, a textual three-way merge that
    /// can conflict.
    VcsDefault,
    /// rwv supplies its deterministic side-pick: a declared path keeps the
    /// version already on the side being replayed onto (rebase) or merged
    /// into (merge), and the incoming edit is discarded.
    KeepTargetSide,
    /// [`DerivedContentResolution::KeepTargetSide`], keeping a replayed
    /// commit that ends up empty, for a caller that writes the declared paths
    /// of every replayed commit itself afterward.
    KeepTargetSideKeepingEmpty,
    /// A declared path keeps the version of the side a merge brings in,
    /// for a merge whose checkout holds the work and whose incoming side is
    /// the one receiving it, so the result is the one a rebase onto that side
    /// reaches.
    KeepIncomingSide,
}

impl DerivedContentPolicy {
    /// Declared derived paths keep the **target-side** version.
    ///
    /// The policy rwv operations carry: a replayed edit to derived content
    /// is dropped in favor of the version at the destination, so a
    /// derived-only commit lands as an empty patch instead of a conflict.
    /// What makes the result correct is not this resolution — it is the
    /// regeneration the repo's gates force afterwards. The resolution only
    /// has to be *mechanical*, so that two machines replaying the same
    /// histories reach the same tree.
    pub fn keep_target_side() -> Self {
        Self(DerivedContentResolution::KeepTargetSide)
    }

    /// rwv supplies no resolver to this operation.
    ///
    /// The shape of an operation rwv did not make — a hand-run VCS command
    /// resolves declared paths textually, because the definition a
    /// declaration names does not travel with the repo. Available as a value
    /// so a call site that wants that behavior *states* it rather than
    /// omitting an argument.
    ///
    /// Not a promise that no resolver is defined: a definition the repo
    /// already carries durably in its own configuration still applies (for
    /// git, the clone's `merge.rwv-lock.driver`). This value governs what the operation supplies,
    /// which is the only half rwv owns per invocation.
    pub fn vcs_default() -> Self {
        Self(DerivedContentResolution::VcsDefault)
    }

    /// Declared derived paths keep the target-side version while the replay
    /// runs, and a replayed commit left with no change is kept rather than
    /// dropped: the caller rewrites each replayed commit's declared paths
    /// once the replay ends, so a commit that changed only them still has a
    /// change to carry.
    pub fn rewritten_after() -> Self {
        Self(DerivedContentResolution::KeepTargetSideKeepingEmpty)
    }

    /// Declared derived paths keep the version of the side a merge brings
    /// in. For a merge made in the checkout of the work being integrated, with
    /// the receiving side incoming: it resolves a path both sides regenerated
    /// to the receiving side, as [`DerivedContentPolicy::keep_target_side`]
    /// does for a rebase onto it.
    pub fn keep_incoming_side() -> Self {
        Self(DerivedContentResolution::KeepIncomingSide)
    }

    /// Which resolution this policy names, for a [`Vcs`] impl to translate.
    ///
    /// `pub(crate)`: the vocabulary belongs to the seam and its impls. A
    /// caller chooses a policy, hands it to an operation, and never asks
    /// what is inside it.
    pub(crate) fn resolution(self) -> DerivedContentResolution {
        self.0
    }
}

/// One half of the lock merge driver's setup in a project repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockDriverPart {
    /// Set up as this release sets it up.
    Current,
    /// Not set up.
    Absent,
    /// Set up by an earlier release, for a driver that kept one side. It is
    /// never honored.
    Superseded,
}

/// How the lock merge driver stands in a project repo, as of one read: the
/// declaration the checked-out commit carries, and the command the clone
/// defines for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockDriverSetup {
    pub declaration: LockDriverPart,
    pub definition: LockDriverPart,
}

/// What [`Vcs::discard_captured_change`] did with one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardOutcome {
    /// The path is back to what the working copy's parent commit records.
    Discarded,
    /// Left alone: the working file no longer holds the captured bytes.
    ChangedSinceCapture,
    /// Left alone: the VCS holds an uncommitted version of the path that the
    /// working file does not show, so the captured bytes are not all of it.
    HoldsUncapturedChange,
}

/// What [`Vcs::prune_stale_worktree_registration`] did with one registration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationPrune {
    /// The registration is gone; its directory already was.
    Pruned,
    /// Left alone: the registration is locked.
    Locked,
    /// Left alone: something stands at the registered path.
    DirectoryPresent,
    /// The store registers no worktree at that path.
    NotRegistered,
}

/// What a checkout writes at one tracked path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedFile {
    /// Relative to the workspace root, `/`-separated.
    pub path: String,
    pub kind: TrackedFileKind,
}

impl TrackedFile {
    pub fn identity(&self) -> Option<&ContentIdentity> {
        match &self.kind {
            TrackedFileKind::File { identity, .. } => identity.as_ref(),
            TrackedFileKind::Symlink | TrackedFileKind::NestedRepository => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackedFileKind {
    /// `identity` is `None` when no file placed here from elsewhere can be
    /// verified to hold what the checkout writes, so nothing may be.
    File {
        executable: bool,
        identity: Option<ContentIdentity>,
    },
    Symlink,
    /// Another repository's checkout, which this one records only a
    /// placeholder for.
    NestedRepository,
}

/// The bytes a checkout writes for a file, opaque outside the backend that
/// minted it.
///
/// Equal identities from workspaces of one repository check out to equal
/// bytes. Unequal identities promise nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentIdentity(String);

impl ContentIdentity {
    pub(crate) fn new(minted: impl Into<String>) -> Self {
        Self(minted.into())
    }
}

/// Where a checkout's branch stands against where the checkout last put it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeftBehind {
    /// Nothing shows the branch moved under the checkout.
    No,
    /// The branch moved forward from this commit by a move the checkout did
    /// not make.
    At(CommitId),
    /// The branch moved from this commit, by a move the checkout did not
    /// make, to a commit that does not contain it.
    Rewritten(CommitId),
}

/// Why a checkout holds no record of where it stood.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unrecorded {
    /// The VCS recorded where the checkout stood and no longer holds it.
    Expired,
    /// The VCS is configured never to record it.
    RecordingOff,
}

/// Operations repoweave needs from a version control system.
///
/// Implementations exist for git (and eventually jj, sl, hg). Each method
/// takes a repo path and operates on it — the trait is stateless.
///
/// Methods return [`VcsError`] (rather than `anyhow::Error`) so callers
/// can pattern-match on failure modes. Application-level glue may convert
/// to `anyhow::Error` at the boundary via the `?` operator.
///
/// `Send + Sync` is required because a `&dyn Vcs` is captured by the closure
/// `run_in_parallel` hands to worker threads. Implementations use interior
/// mutability (e.g. `Mutex`) for any mutable state.
///
/// Each method below says what git does under **For git:**. The git
/// implementation is a private type; [`crate::git::git_vcs`] hands out the
/// only handle to it, so a caller documents against this trait and can be
/// given a different backend.
pub trait Vcs: Send + Sync {
    /// Human-readable name (e.g., `"git"`, `"jj"`).
    fn name(&self) -> &str;

    /// The oldest release of the tool this backend drives that it runs on,
    /// spelled as the tool numbers its releases.
    ///
    /// For git: 2.36.
    fn minimum_release(&self) -> String;

    /// Whether the installed tool is one this backend runs on. Asked once per
    /// invocation; the answer is cached for the life of the process.
    ///
    /// For git: reads `git version`, the same reading every detected
    /// capability answers from.
    fn tool_readiness(&self) -> ToolReadiness;

    /// Create an empty repository at `dest`, creating the directory and any
    /// missing parents first.
    ///
    /// For git: runs
    /// `git init --initial-branch=main`.
    fn init_repo(&self, dest: &Path) -> Result<(), VcsError>;

    /// Create an empty **bare** repository at `dest` — no working tree —
    /// creating the directory and any missing parents first.
    ///
    /// A registry that creates its own upstream (`local`) needs one: a
    /// non-bare `init_repo` upstream refuses a push to its checked-out
    /// branch, so the first `git push` from the member it backs would fail
    /// against the very thing creation was for.
    fn init_bare_repo(&self, dest: &Path) -> Result<(), VcsError>;

    /// Clone `url` into `dest`, and the remote the clone made.
    ///
    /// The VCS names it as it names the remote of any clone it makes, and no
    /// caller relies on the name: every later use of a remote resolves its
    /// role, the url remote of a member by the url it records.
    fn clone_repo(&self, url: &CloneUrl, dest: &Path) -> Result<RemoteHandle, VcsError>;

    /// Bring `remote`'s branches and tags into `repo`'s object store, updating
    /// only its remote-tracking refs: no local branch and no checkout moves.
    fn fetch_remote(&self, repo: &Path, remote: &RemoteHandle) -> Result<(), VcsError>;

    /// The tip of the branch `on` takes in from — the remote branch the VCS
    /// records for it — or `None` when it records none, or the remote has
    /// not got the branch (git: `@{upstream}`).
    fn upstream_tip(&self, on: &AttachedRef) -> Result<Option<ResolvedRevisionId>, VcsError>;

    /// The remote branch `on` takes in from, or `None` when the VCS records
    /// none (git: `branch.<b>.remote` and `branch.<b>.merge`). The branch may
    /// not exist on the remote: this reads the record, not the remote.
    fn upstream(&self, on: &AttachedRef) -> Result<Option<Upstream>, VcsError>;

    /// Where a branch publishes, in the VCS's own order, ending with the url
    /// remote `url_remote` names; `None` where the order yields nothing and
    /// `url_remote` is `None`: the branch has no push target.
    ///
    /// For git: `branch.<b>.pushRemote`, then `remote.pushDefault`, then
    /// `branch.<b>.remote`, then `url_remote`.
    fn push_target(
        &self,
        on: &AttachedRef,
        url_remote: Option<&RemoteHandle>,
    ) -> Result<Option<RemoteHandle>, VcsError>;

    /// [`Vcs::push_target`] for a branch that need not be checked out.
    fn push_target_of(
        &self,
        repo: &Path,
        branch: &RawRefName,
        url_remote: Option<&RemoteHandle>,
    ) -> Result<Option<RemoteHandle>, VcsError>;

    /// The remote the VCS was told to push `branch` to, apart from the remote
    /// it takes in from: `None` where it was told nothing (git:
    /// `branch.<b>.pushRemote`, else `remote.pushDefault`).
    fn own_push_remote(
        &self,
        repo: &Path,
        branch: &RawRefName,
    ) -> Result<Option<RemoteHandle>, VcsError>;

    /// The tip of the branch on `target` named as `on` is, as last fetched, or
    /// `None` when `target` has no such branch: the push branch.
    fn push_branch(
        &self,
        on: &AttachedRef,
        target: &RemoteHandle,
    ) -> Result<Option<ResolvedRevisionId>, VcsError>;

    /// Where a new branch is taken from: the remote the operator configured
    /// for it, else `url_remote` (git: `checkout.defaultRemote`).
    fn home_remote(&self, repo: &Path, url_remote: &RemoteHandle)
        -> Result<RemoteHandle, VcsError>;

    /// The remote that has `url`. Nothing is written: an absent remote comes
    /// back as the one [`Vcs::add_url_remote`] would add, so a verb judges
    /// every repo before it changes one.
    ///
    /// Urls are compared as [`CloneUrl::names_same_remote`] compares them. The
    /// name an absent remote would take is the VCS's (git: the url's owner
    /// segment, `origin` for a repo with no remote yet), and a name that
    /// holds another url is [`UrlRemote::NameTaken`].
    fn url_remote(&self, repo: &Path, url: &CloneUrl) -> Result<UrlRemote, VcsError>;

    /// Add the remote [`Vcs::url_remote`] found absent.
    fn add_url_remote(&self, repo: &Path, absent: &AbsentRemote) -> Result<RemoteHandle, VcsError>;

    /// The project repo's url remote: the one `record_project_url_remote`
    /// recorded; else its only remote; else `None`. A recorded remote the repo
    /// no longer has counts as none recorded.
    fn project_url_remote(&self, repo: &Path) -> Result<Option<RemoteHandle>, VcsError>;

    /// Record `remote` as the project repo's url remote, in the repo's own
    /// configuration (git: `rwv.url-remote` in the local config).
    fn record_project_url_remote(&self, repo: &Path, remote: &RemoteHandle)
        -> Result<(), VcsError>;

    /// Every remote of `repo` with the url it records.
    fn recorded_urls(&self, repo: &Path) -> Result<Vec<RecordedUrl>, VcsError>;

    /// Whether a remote-tracking branch of `remote` reaches `commit`, as last
    /// fetched.
    fn reached_by_remote(
        &self,
        repo: &Path,
        commit: &ResolvedRevisionId,
        remote: &RemoteHandle,
    ) -> Result<bool, VcsError>;

    /// How `tip` stands to `other`, by ancestry.
    fn relation(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        other: &ResolvedRevisionId,
    ) -> Result<Relation, VcsError> {
        if tip == other {
            return Ok(Relation::Even);
        }
        Ok(
            match (
                self.is_ancestor(repo, other, tip)?,
                self.is_ancestor(repo, tip, other)?,
            ) {
                (true, _) => Relation::Ahead,
                (_, true) => Relation::Behind,
                _ => Relation::Diverged,
            },
        )
    }

    /// Bring every remote's branches and tags into `repo`'s object store (the
    /// remotes `only` names, when it names some), dropping remote-tracking
    /// branches a remote no longer has, and report each remote-tracking
    /// branch that moved. No local branch and no checkout moves.
    ///
    /// A branch that moved to a tip not descending from its old one keeps the
    /// old tip as its superseded tip, a backup marked [`BackupMark::Superseded`]
    /// recorded before this returns (git: `refs/rwv/backup/<remote>/<branch>/
    /// superseded-<old tip>`). `claim` is the
    /// proof that no other rwv fetches into this store meanwhile.
    fn fetch_pruning(
        &self,
        claim: &crate::fetch_claim::FetchClaim,
        repo: &Path,
        only: Option<&[RemoteHandle]>,
    ) -> Result<PruningFetch, VcsError>;

    /// Resolve `branch` on `remote`, as last fetched.
    ///
    /// For git: resolves the remote-tracking ref `refs/remotes/<remote>/{branch}`
    /// via [`resolve_revision`] and displays it as `<remote>/{branch}`. There
    /// is no bare-branch fallback — a branch the remote lacks yields
    /// [`VcsError::RevisionNotFound`] naming the qualified ref, so callers
    /// don't silently advance to the local branch tip.
    ///
    /// [`resolve_revision`]: Vcs::resolve_revision
    fn resolve_branch_on_remote(
        &self,
        repo: &Path,
        remote: &RemoteHandle,
        branch: &RemoteRef,
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
    /// preserved as the display form for human-readable output. Returns
    /// [`VcsError::RevisionNotFound`] if the revision is unknown in this repo.
    fn resolve_revision(&self, repo: &Path, rev: &str) -> Result<ResolvedRevisionId, VcsError>;

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

    /// Return every path the VCS's status reports as changed in `repo` — the
    /// same population [`Vcs::has_uncommitted_changes`] collapses to a
    /// `bool`. Empty when status reports nothing.
    ///
    /// A rename names both its source and its destination. A path whose
    /// edits the VCS has been told not to look at is absent, so this answers
    /// "is anything reported dirty", never "does this one file hold its
    /// committed bytes"; that is [`Vcs::holds_committed_content`].
    ///
    /// Paths are repo-relative, in the implementation's own order.
    ///
    /// For git: parses `git status --porcelain -z`. A file marked
    /// assume-unchanged or skip-worktree is not reported.
    fn dirty_file_names(&self, repo: &Path) -> Result<Vec<String>, VcsError>;

    /// [`Vcs::dirty_file_names`] with untracked files excluded.
    ///
    /// The source-side cleanliness signal a replay-based landing refuses on:
    /// tracked dirt would go stale under the replay, untracked scratch
    /// survives it untouched.
    ///
    /// For git: parses
    /// `git status --porcelain -z --untracked-files=no`.
    fn tracked_dirty_file_names(&self, repo: &Path) -> Result<Vec<String>, VcsError>;

    /// The paths the VCS's ignore rules hide from [`Vcs::dirty_file_names`]:
    /// files no commit records that status does not report. Together the two
    /// are everything deleting the workspace would lose that its history would
    /// not give back.
    ///
    /// For git: the `!!` records of `git status --porcelain -z --ignored`,
    /// which reports a wholly ignored directory once, as `<dir>/`.
    fn ignored_file_names(&self, repo: &Path) -> Result<Vec<String>, VcsError>;

    /// The native commands that preview and then delete every file
    /// [`Vcs::ignored_file_names`] reports for the workspace at `workspace`,
    /// for an operator to run once they judge those files regenerable. rwv
    /// never runs them.
    fn ignored_removal_hint(&self, workspace: &Path) -> String;

    /// Return `true` when the regular file at `path` (relative to `repo`)
    /// holds exactly the bytes the working copy's parent commit records for
    /// it, so the VCS can give back what overwriting it would lose.
    ///
    /// Read from the file itself: no index state, status flag or staged
    /// rename bears on the answer. `false` when either side has no regular
    /// file at `path`, including before the first commit, and when `repo` is
    /// not a repository at all. `Err` is a failure of the VCS inside one.
    ///
    /// For git: compares `git hash-object --no-filters` of the file against
    /// the blob `git ls-tree HEAD` records at `path`.
    fn holds_committed_content(&self, repo: &Path, path: &Path) -> Result<bool, VcsError>;

    /// Return `true` when `path` is under version control in `repo`. A
    /// relative `path` is resolved against `repo`.
    ///
    /// A path the VCS does not know — untracked, ignored, or outside the repo
    /// entirely — is `Ok(false)`. `Err` is reserved for the VCS itself being
    /// unreachable, so a caller that wants the historical
    /// never-fabricate-a-finding behavior writes `.unwrap_or(false)` and says
    /// so at its own site.
    ///
    /// For git: runs
    /// `git ls-files --error-unmatch <path>`.
    fn is_tracked(&self, repo: &Path, path: &Path) -> Result<bool, VcsError>;

    /// Mark `paths` (relative to `repo`) for inclusion in the next commit.
    ///
    /// Every path must exist and be committable; callers derive the list from
    /// a status read, which reports neither absent nor ignored files.
    ///
    /// For git: runs
    /// `git add -- <paths>`.
    fn stage_paths(&self, repo: &Path, paths: &[&str]) -> Result<(), VcsError>;

    /// Mark `content` as the next commit's version of `path` (relative to
    /// `repo`) without reading or writing the file on disk, so the commit
    /// records `content` while the working copy keeps whatever edits it holds.
    ///
    /// For git: writes the blob with `git hash-object -w --stdin` and points
    /// the index at it with `git update-index --add --cacheinfo`.
    fn stage_content(&self, repo: &Path, path: &Path, content: &[u8]) -> Result<(), VcsError>;

    /// Return `paths` in the index to their committed state, leaving the work
    /// tree alone: what undoes [`Vcs::stage_paths`] when the commit it was
    /// staging for is refused.
    ///
    /// For git: runs
    /// `git reset -q -- <paths>`.
    fn unstage_paths(&self, repo: &Path, paths: &[&str]) -> Result<(), VcsError>;

    /// Put `path` (relative to `repo`) back to what the working copy's parent
    /// commit records — absent, where it records nothing — when `captured`
    /// is all of the uncommitted change to it: the working file holds exactly
    /// `captured`, and the VCS holds no other uncommitted version of it.
    /// Otherwise nothing is touched, and the outcome says which condition
    /// failed.
    ///
    /// The caller has copied `captured` somewhere else; this is the other half
    /// of the move, and it must never discard a byte the copy does not hold.
    ///
    /// For git: a staged version that differs from the working tree is the
    /// uncommitted version the file does not show, as is any rename, type
    /// change or conflict. Otherwise runs `git reset -q -- <path>` and
    /// `git checkout-index -f -u -- <path>` where HEAD records the path, and
    /// removes it from the index and the working tree where HEAD does not.
    fn discard_captured_change(
        &self,
        repo: &Path,
        path: &Path,
        captured: &[u8],
    ) -> Result<DiscardOutcome, VcsError>;

    /// Return `true` when something is marked for the next commit.
    ///
    /// The question a caller asks before committing, so an empty commit is
    /// refused rather than created.
    ///
    /// For git: runs
    /// `git diff --cached --quiet` and reads its exit status.
    fn has_staged_changes(&self, repo: &Path) -> Result<bool, VcsError>;

    /// Return every path marked for the next commit.
    ///
    /// A caller that must refuse to bundle unrelated work compares this
    /// against the paths it means to commit. Paths never staged — untracked
    /// files among them — are absent.
    ///
    /// For git: parses
    /// `git status --porcelain --untracked-files=no`, keeping entries whose
    /// index column is set; a rename names both its source and its
    /// destination.
    fn staged_paths(&self, repo: &Path) -> Result<Vec<String>, VcsError>;

    /// The tracked paths whose working file differs from what the index
    /// holds: edits not staged.
    ///
    /// For git: the worktree column of
    /// `git status --porcelain --untracked-files=no`.
    fn unstaged_paths(&self, repo: &Path) -> Result<Vec<String>, VcsError>;

    /// Record everything staged as a new commit with `message`.
    ///
    /// Fails when nothing is staged; call [`Vcs::has_staged_changes`] first.
    ///
    /// For git: runs
    /// `git commit -m <message>`.
    fn commit(&self, repo: &Path, message: &str) -> Result<(), VcsError>;

    /// Record what the index holds for `paths`, and nothing else it holds, as
    /// a new commit with `message` and `annotations`: every other staged
    /// change stays staged, and the working files are not read. `false` where
    /// `paths` hold no change, and no commit is made.
    ///
    /// For git: a private index read from `HEAD` takes the index's entries
    /// for `paths`, and `git commit` runs over it, hooks included; each
    /// annotation is a `<key>: <value>` trailer.
    fn commit_only(
        &self,
        repo: &Path,
        paths: &[&str],
        message: &str,
        annotations: &[Annotation],
    ) -> Result<bool, VcsError>;

    /// The values `commit` records under the annotation `key`, in the order
    /// written.
    ///
    /// For git: `git log -1 --format=%(trailers:key=<key>,valueonly,unfold)`.
    fn annotation_values(
        &self,
        repo: &Path,
        commit: &CommitId,
        key: &str,
    ) -> Result<Vec<String>, VcsError>;

    /// Whether a commit made in `dir` has an author to record. `dir` need not
    /// be a repository; outside one the user-level setting answers.
    ///
    /// For git: `git var GIT_COMMITTER_IDENT` succeeds.
    fn has_commit_identity(&self, dir: &Path) -> Result<bool, VcsError>;

    /// Return the tag name pointing at HEAD, if any.
    ///
    /// When multiple tags point at HEAD the implementation may return any one
    /// of them. Returns `None` when no tag points at the current HEAD commit.
    fn tag_at_head(&self, repo: &Path) -> Result<Option<RawRefName>, VcsError>;

    /// Prune stale worktree administrative files from a repo.
    fn worktree_prune(&self, repo: &Path) -> Result<(), VcsError>;

    /// Human-readable hint text for the VCS-level steps to resolve a conflict
    /// and stage the result. Embedded verbatim in sync's conflict-bail messages.
    ///
    /// Returned text is a short multi-line block suitable for splicing into a
    /// larger message — callers are expected to add surrounding context (which
    /// repo, the `rwv <verb> --continue` line for Rebase, and `rwv abort` as
    /// the rollback option).
    ///
    /// `op` is the in-flight operation that produced the conflict; the hint text varies per VCS and per op. No
    /// `repo` param: the hint text for git
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
    /// `rwv land --continue` line — rwv vocabulary must not appear inside
    /// a VCS impl.
    fn conflict_resolution_hint(&self, op: ConflictOp) -> String;

    /// The line an operator runs for `action`, with its placeholders unfilled.
    fn hint(&self, action: VcsAction) -> &'static str;

    /// Rebase commits in the range `upstream..` of `repo`'s current branch
    /// onto `onto`, resolving declared derived content per `derived`.
    ///
    /// For git: runs `git rebase --onto <onto>
    /// <upstream>`. On conflict, leaves the repo in the VCS-native in-flight
    /// state (for git: mid-rebase, with conflict markers in the working tree
    /// and `.git/rebase-merge/`) and returns
    /// [`VcsError::RebaseConflict { repo, op: ConflictOp::Rebase }`] so the
    /// caller can pair with [`Vcs::conflict_resolution_hint`] to assemble the
    /// user-facing resolution text.
    ///
    /// # Derived content
    ///
    /// WHICH paths are derived is the repo's own declaration, the lock merge
    /// driver's, carried in tracked metadata (for git: `merge=rwv-lock` in
    /// `.gitattributes`). HOW they resolve is `derived`,
    /// stated per call: [`DerivedContentPolicy::keep_target_side`] resolves a
    /// declared path to the version at `onto` without stopping the replay,
    /// and [`DerivedContentPolicy::vcs_default`] lets it conflict like any
    /// other content. A repo that declares nothing derived is unaffected by
    /// either value.
    ///
    /// The parameter is not a convenience: it is what makes the resolution a
    /// caller's stated choice rather than a property of whichever impl runs.
    /// For git it becomes the inline merge-driver
    /// definition (`-c merge.<name>.driver=…`) for that single invocation, so
    /// it is in force for exactly this operation and leaves no configuration
    /// behind; the trait hides that spelling so other VCS impls can use their
    /// own mechanism.
    ///
    /// The replay records which commit each replayed commit became, in a
    /// record that outlives a stop and a continue finished by hand, read
    /// back by [`Vcs::replayed`]. The operator's own configuration never
    /// changes which refs it moves, which commits it replays, or where it
    /// stops.
    fn rebase(
        &self,
        repo: &Path,
        onto: &ResolvedRevisionId,
        upstream: &ResolvedRevisionId,
        derived: DerivedContentPolicy,
    ) -> Result<(), VcsError>;

    /// Which commit each commit of `from..old` became, where a replay of
    /// `old` onto `onto` by [`Vcs::rebase`] wrote `onto..new`, as that
    /// replay recorded it, or as [`Vcs::record_replayed`] recorded a rewrite
    /// of what it wrote. A written commit no commit became is one added
    /// during the replay. `None` where `from..old` holds a commit and no
    /// record of that replay describes `new`: one rwv did not start, or a
    /// branch changed since in a way the record does not cover.
    fn replayed(
        &self,
        repo: &Path,
        from: &CommitId,
        old: &CommitId,
        onto: &CommitId,
        new: &CommitId,
    ) -> Result<Option<Replayed>, VcsError>;

    /// Record `replayed` as what the replay of `old` onto `onto` made, once
    /// rwv has rewritten the commits that replay wrote.
    fn record_replayed(
        &self,
        repo: &Path,
        old: &CommitId,
        onto: &CommitId,
        replayed: &Replayed,
    ) -> Result<(), VcsError>;

    /// Drop every record a replay in the checkout at `repo` kept, once no
    /// operation reads it.
    fn forget_replays(&self, repo: &Path) -> Result<(), VcsError>;

    /// A copy of `commit` whose parent is `parent`, its message and
    /// authorship kept and its tree `commit`'s with `changes` applied,
    /// referenced by nothing. Where `annotations` is `Some((key, values))`,
    /// the copy records `values` under `key` in place of what `commit`
    /// recorded under it.
    fn copy_replayed(
        &self,
        repo: &Path,
        commit: &CommitId,
        parent: &CommitId,
        changes: &[FileChange],
        annotations: Option<(&str, &[Annotation])>,
    ) -> Result<CommitId, VcsError>;

    /// Move the branch `on` witnesses from `from`, where a replay left it, to
    /// `to`, a copy of that replay whose commits differ from it only in
    /// paths the checkout holds unchanged or already stages as `to` holds
    /// them. Refuses a branch not at `from` and an uncommitted change the
    /// move would overwrite. What the checkout stages in paths the move
    /// leaves alone stays staged.
    ///
    /// For git: `git checkout -B <branch> <to>`, a two-way merge from `from`.
    fn take_rewritten(
        &self,
        on: &AttachedRef,
        from: &CommitId,
        to: &CommitId,
    ) -> Result<(), VcsError>;

    /// Resume an in-flight rebase in `repo` after the operator has resolved
    /// (and staged) the conflicting paths that stopped the previous
    /// [`rebase`] or [`rebase_continue`] call, resolving declared derived
    /// content per `derived`.
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
    ///   conflict, or when the operator's resolution was incomplete
    ///   (unstaged conflict markers): repo is left in the same mid-rebase
    ///   state git leaves it, and returns
    ///   [`VcsError::RebaseConflict { repo, op: ConflictOp::Rebase }`] so
    ///   the operator loop stays: resolve → `git add` → `rwv sync
    ///   --continue`, iterating per conflicted pick.
    ///
    /// A resumed replay reaches picks the interrupted one never got to, so
    /// `derived` is the policy that governs them. Handing this method a
    /// different policy than the [`rebase`] call it resumes is legal and
    /// means what it says — the remaining picks resolve differently from the
    /// ones already replayed — which is a decision a caller has to make
    /// deliberately, not a detail it can leave to whichever value happened to
    /// be in scope.
    ///
    /// For git: runs `git rebase --continue`,
    /// spelling `derived` the same way [`rebase`] does, so a replay that
    /// stopped on a conflict and the resume that finishes it cannot disagree
    /// about how a declared path resolves. The invocation runs
    /// non-interactively — git's editor spawn for the stopped commit's
    /// message is suppressed so a `--continue` never hangs waiting for
    /// `$EDITOR` in an automated pipeline.
    ///
    /// [`rebase`]: Vcs::rebase
    /// [`rebase_continue`]: Vcs::rebase_continue
    /// [`mid_op`]: Vcs::mid_op
    fn rebase_continue(&self, repo: &Path, derived: DerivedContentPolicy) -> Result<(), VcsError>;

    /// Declared derived paths the range `base..source` changed whose
    /// `source`-side content the tree at `landed` does not carry.
    ///
    /// The observable half of [`DerivedContentPolicy::keep_target_side`].
    /// That resolution leaves no conflict, no marker and no entry in the
    /// landed commit's diff, so comparing what went into a replay against
    /// what came out is the only thing that can name what it dropped.
    ///
    /// Restricting to paths the range changed is what separates a drop from
    /// an ordinary difference: a declared path `source` never touched differs
    /// from `landed` whenever the target moved it, and a caller reporting
    /// that would be reporting someone else's work.
    ///
    /// The question is about the landed tree, not about resolver
    /// invocations: a path an intermediate step resolved away and a later one
    /// put back lost nothing, and is not in the result.
    ///
    /// `Ok(vec![])` is the normal outcome and carries no suspicion — it is
    /// what a replay with nothing to resolve returns.
    ///
    /// For git: intersects
    /// `git diff --name-only <base>...<source>` with `git diff --name-only`
    /// \<landed\> `<source>`, then
    /// asks `git check-attr merge` which survivors carry the rwv resolution.
    /// Delegating the pattern evaluation to git is what keeps a `!merge`
    /// carve-out inside a declared subtree honored here without a second
    /// matcher to keep in step with git's own.
    fn derived_content_dropped_by_replay(
        &self,
        repo: &Path,
        base: &ResolvedRevisionId,
        source: &ResolvedRevisionId,
        landed: &ResolvedRevisionId,
    ) -> Result<Vec<String>, VcsError>;

    /// File paths (relative to `repo`'s root) whose content differs between
    /// the trees at `from` and `to`.
    ///
    /// A comparison of the two endpoint trees, not a history walk: a path
    /// changed and changed back along the way is absent, and which side a
    /// difference originated on is not part of the answer.
    ///
    /// For git: `git diff --name-only -z <from> <to>`.
    fn changed_paths_between(
        &self,
        repo: &Path,
        from: &ResolvedRevisionId,
        to: &ResolvedRevisionId,
    ) -> Result<Vec<String>, VcsError>;

    /// How the lock merge driver stands in the project repo `repo`.
    ///
    /// The declaration is read from the checked-out commit, or from the
    /// working copy where no commit is checked out yet: the committed form is
    /// the one that travels with the branch. The definition is read from every
    /// scope of configuration the VCS consults.
    ///
    /// For git: `.gitattributes` carrying `rwv.lock merge=rwv-lock`, and
    /// `merge.rwv-lock.driver` naming `rwv lock --merge-driver %O %A %B`.
    /// A `merge=rwv-ours` or `merge=ours` line for the lock, a local
    /// `merge.rwv-ours.*` entry, or `merge.rwv-lock.driver` naming another
    /// command, is superseded.
    fn lock_driver_setup(&self, repo: &Path) -> Result<LockDriverSetup, VcsError>;

    /// The file, relative to a project repo's root, that declares the lock
    /// merge driver.
    fn lock_driver_declaration_file(&self) -> &'static str;

    /// `text`, the content of [`Vcs::lock_driver_declaration_file`] (`None`
    /// for no file), rewritten to declare the lock merge driver once and
    /// nothing superseded beside it; `None` when it already does exactly that.
    fn declare_lock_driver_in(&self, text: Option<&str>) -> Option<String>;

    /// Define the lock merge driver's command in `repo`'s clone, replacing a
    /// superseded definition. Every checkout of the clone shares it.
    fn define_lock_driver(&self, repo: &Path) -> Result<(), VcsError>;

    /// The two commits a merge in progress in `repo` joins, checked-out side
    /// first, when the VCS names both to a merge driver it runs; `None` for a
    /// replay, a copy, or a merge computed without a checkout, whose sides
    /// are not two heads.
    fn merging_commits(&self, repo: &Path) -> Option<(CommitId, CommitId)>;

    /// Fast-forward `repo`'s current branch to `to`, refusing rather than
    /// clobbering if a fast-forward isn't possible.
    ///
    /// Safe-by-construction: when `to` is not a descendant of the current
    /// HEAD, the operation fails with [`VcsError::CommandFailed`] (the VCS
    /// is asked to do an FF-only advance and refuses). The working tree and
    /// branch ref are touched only on success. No conflict markers are ever
    /// left in the working tree.
    ///
    /// For git: runs `git merge --ff-only <to>` with `-c
    /// advice.diverging=false`, so a refusal's stderr carries git's `fatal:`
    /// cause without git's own hint block recommending `git merge --no-ff`
    /// or `git rebase` — either of which would bypass whatever op called
    /// this.
    /// Safe by construction: no `reset --hard` replacement that could
    /// discard reachable history.
    fn advance_if_fast_forward(&self, repo: &Path, to: &ResolvedRevisionId)
        -> Result<(), VcsError>;

    /// Hard-reset `repo`'s current branch to `to`, discarding any divergent
    /// commits and overwriting the working tree.
    ///
    /// Destructive — discarded commits are not recoverable through this VCS
    /// call alone. Reached only through
    /// [`Vcs::reset_attached_ref_keeping`], which requires a backup of the
    /// tip being left.
    ///
    /// For git: runs `git reset --hard <to>`.
    fn hard_reset(&self, repo: &Path, to: &ResolvedRevisionId) -> Result<(), VcsError>;

    /// Return `true` when `ancestor` is an ancestor of `descendant` in
    /// `repo`. A revision counts as its own ancestor, so equal revisions
    /// return `true` (non-strict, matching `git merge-base --is-ancestor`).
    ///
    /// For git: runs
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
    /// For git: runs
    /// `git rev-list --count <from>..<to>`. The bounded count is what
    /// callers like `rwv sync`'s `AlreadyAhead` reporting actually need —
    /// no commit list is materialized.
    fn count_commits_in_range(
        &self,
        repo: &Path,
        from: &ResolvedRevisionId,
        to: &ResolvedRevisionId,
    ) -> Result<usize, VcsError>;

    /// Create a savepoint capturing the checkout `repo`: its `HEAD` and the
    /// branch it is on, or that it is detached, so a restore puts the
    /// checkout back on that branch and resets no other.
    ///
    /// For git: writes `refs/rwv/pre-op/<key>/checkout/<branch>`, or
    /// `refs/rwv/pre-op/<key>/detached`, pointing at `HEAD`, where `<key>`
    /// is the key's text form. The ref namespace is an impl detail — callers
    /// pass the [`SavepointKey`] and never spell the ref directly; those that
    /// must show an operator where the state went take the name from
    /// [`Vcs::savepoint_label`] or [`Vcs::savepoint_namespace`].
    ///
    /// Returns the captured `HEAD` revision so the caller can record it
    /// in op-state.
    fn create_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
    ) -> Result<ResolvedRevisionId, VcsError>;

    /// Create a savepoint of the local branch `branch` of `repo`'s store at
    /// `at`, the tip the op read before it moves that branch as a ref.
    ///
    /// For git: `refs/rwv/pre-op/<key>/ref/<branch>`.
    fn create_ref_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
        branch: &RawRefName,
        at: &CommitId,
    ) -> Result<(), VcsError>;

    /// The branch savepoints filed under `key` in `repo`'s store.
    fn ref_savepoints(
        &self,
        repo: &Path,
        key: &SavepointKey,
    ) -> Result<Vec<RefSavepoint>, VcsError>;

    /// Drop the savepoint of `branch` filed under `key`, leaving the branch.
    fn drop_ref_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
        branch: &RawRefName,
    ) -> Result<(), VcsError>;

    /// What putting `saved.branch` back at `saved.at` would do, writing
    /// nothing: a branch at its savepoint is left; one gone or checked out
    /// anywhere is never moved. A checkout in `freed` is one the caller
    /// switches off the branch first, so it does not hold it.
    fn plan_ref_restore(
        &self,
        repo: &Path,
        saved: &RefSavepoint,
        freed: &[PathBuf],
    ) -> Result<RefRestorePlan, VcsError>;

    /// Carry out `plan`, writing nothing where the branch is no longer
    /// where the plan found it or a checkout now holds it.
    fn execute_ref_restore(
        &self,
        repo: &Path,
        key: &SavepointKey,
        saved: &RefSavepoint,
        plan: &RefRestorePlan,
    ) -> Result<RefRestoreOutcome, VcsError>;

    /// The operator-spellable, VCS-native name of the savepoint reference
    /// filed under `key`, for recovery hints that must name a resolvable
    /// reference.
    ///
    /// For git: `refs/rwv/pre-op/<key>`.
    fn savepoint_label(&self, key: &SavepointKey) -> String;

    /// The reference namespace savepoints live under, for operator text
    /// written where no op exists yet to key a [`Vcs::savepoint_label`].
    ///
    /// For git: `refs/rwv/pre-op`.
    fn savepoint_namespace(&self) -> String;

    /// Look up the savepoint captured under `key` in `repo`, returning
    /// the SHA it points at, or `None` when no such savepoint exists.
    ///
    /// For git: reads `refs/rwv/pre-op/<key>`.
    fn resolve_savepoint(&self, repo: &Path, key: &SavepointKey) -> Option<ResolvedRevisionId>;

    /// Drop every savepoint filed under `key` in `repo`'s store, the
    /// checkout's and each branch's. No-op when there is none; ignores
    /// ref-update failures (a savepoint is purely a recovery aid — its
    /// absence is benign).
    fn drop_savepoint(&self, repo: &Path, key: &SavepointKey);

    /// Drop the savepoint of the checkout `repo` filed under `key`, keeping
    /// the branch savepoints filed beside it.
    fn drop_checkout_savepoint(&self, repo: &Path, key: &SavepointKey) -> Result<(), VcsError>;

    /// Restore the checkout to its savepoint, keeping first whatever the
    /// restore discards:
    /// - no savepoint exists for `key` → [`VerifiedRestoreOutcome::NoSavepoint`],
    ///   or [`VerifiedRestoreOutcome::LeftStopped`] where it is stopped in an
    ///   operation, which the op cannot have started where it saved nothing.
    /// - stopped in an operation the op never starts here, begun at the
    ///   savepoint → left ([`VerifiedRestoreOutcome::LeftStopped`]). Any other
    ///   stopped operation is canceled once the working copy is kept, whichever
    ///   workspace abort runs in; the tip it returns to is classified in turn
    ///   ([`VerifiedRestoreOutcome::Canceled`]).
    /// - on another branch than the savepoint names, or detached where it was
    ///   attached or the reverse → put back where the savepoint was taken
    ///   without moving the branch it leaves
    ///   ([`VerifiedRestoreOutcome::SwitchedBack`]).
    /// - `tip == savepoint` → [`VerifiedRestoreOutcome::Untouched`].
    /// - any other tip is reset to the savepoint, kept first where it holds a
    ///   commit nothing else reaches, whoever made it.
    ///
    /// What is kept goes in one slot per branch and one per checkout
    /// ([`BackupMark::Aborted`], [`BackupMark::AbortedWorkingCopy`]), which
    /// the next restore that keeps something there overwrites
    /// ([`VerifiedRestoreOutcome::Kept`]).
    fn verified_restore_savepoint(
        &self,
        repo: &Path,
        key: &SavepointKey,
        attribution: &Attribution<'_>,
    ) -> Result<VerifiedRestoreOutcome, VcsError> {
        let plan = self.plan_checkout_restore(repo, key, attribution)?;
        self.execute_checkout_restore(repo, key, &plan)
    }

    /// What [`Vcs::verified_restore_savepoint`] would do, writing nothing:
    /// each step it would take, judged against the state the steps before
    /// it leave.
    fn plan_checkout_restore(
        &self,
        repo: &Path,
        key: &SavepointKey,
        attribution: &Attribution<'_>,
    ) -> Result<CheckoutRestorePlan, VcsError>;

    /// Carry out `plan` up to the first step it blocks at. A step whose
    /// checkout no longer holds the state the plan expects writes nothing
    /// and ends the restore of that checkout as a
    /// [`VerifiedRestoreOutcome::ForeignTip`] at the tip it holds.
    fn execute_checkout_restore(
        &self,
        repo: &Path,
        key: &SavepointKey,
        plan: &CheckoutRestorePlan,
    ) -> Result<VerifiedRestoreOutcome, VcsError>;

    /// Where canceling the operation `repo` is stopped in would leave HEAD;
    /// `None` when none is in progress.
    fn cancel_returns_to(&self, repo: &Path) -> Result<Option<ResolvedRevisionId>, VcsError>;

    /// Return the in-flight operation with a conflict-resume path that
    /// `repo` is currently mid-way through, if any.
    ///
    /// A bisect has no such path and is not reported here; see
    /// [`mid_operation`].
    ///
    /// [`mid_operation`]: Vcs::mid_operation
    fn mid_op(&self, repo: &Path) -> Option<ConflictOp> {
        self.mid_operation(repo).and_then(MidOperation::conflict)
    }

    /// Cancel any in-flight VCS operation in `repo` that has a resume path.
    /// No-op when the repo is in a clean state.
    ///
    /// For git: runs that operation's own `--abort`, by what [`mid_op`]
    /// reports. Errors from the underlying
    /// abort command are swallowed — the call is a best-effort cleanup
    /// before the surrounding flow (e.g. `rwv abort`) does its own
    /// recovery.
    ///
    /// [`mid_op`]: Vcs::mid_op
    fn cancel_in_flight_op(&self, repo: &Path);

    /// Everything `repo`'s store holds whose commits no remote-tracking
    /// branch reaches: each ref outside the remote-tracking ones, and a
    /// checkout's HEAD on no branch. Empty when every commit the store holds
    /// is on a remote as last fetched.
    ///
    /// For git: `git rev-list --all --not --remotes`, then every ref and a
    /// detached HEAD whose tip that lists.
    fn held_by_no_remote(&self, repo: &Path) -> Result<Vec<HeldAlone>, VcsError>;

    /// The commits of `branch` that no remote-tracking branch reaches and no
    /// superseded tip reaches, newest first: its unpushed commits.
    ///
    /// For git: `git log refs/heads/<branch> --not --remotes
    /// --glob=refs/rwv/backup/*/superseded-*`.
    fn unpushed_commits(
        &self,
        repo: &Path,
        branch: &RawRefName,
    ) -> Result<Vec<CommitSummary>, VcsError>;

    /// The commits `tip` reaches that `base` does not, and that no
    /// remote-tracking branch and no superseded tip reaches either, newest
    /// first.
    ///
    /// For git: `git log <tip> --not <base> --remotes
    /// --glob=refs/rwv/backup/*/superseded-*`.
    fn unpushed_beyond(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        base: &ResolvedRevisionId,
    ) -> Result<Vec<CommitSummary>, VcsError>;

    /// The commits a push of `tip` would publish, oldest first, each with its
    /// parents: those the push branch `pushed` lacks, or, where the branch has
    /// no push branch, those no remote-tracking branch reaches.
    ///
    /// For git: `git rev-list --parents --reverse --topo-order <tip> --not
    /// <pushed>` or `--not --remotes`.
    fn pushed_range(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        pushed: Option<&ResolvedRevisionId>,
    ) -> Result<Vec<RangeCommit>, VcsError>;

    /// Fetch objects from `src_repo` into `dst_repo` so SHAs reachable in
    /// `src_repo` are reachable in `dst_repo`.
    ///
    /// For git: runs `git fetch <src_path> HEAD`
    /// in `dst_repo`. For sibling worktrees that share an object store this
    /// is a no-op; for independent clones it copies objects across. Errors
    /// are swallowed — the caller (e.g. `land` step 3) is expected to
    /// verify reachability by a subsequent operation that will fail loudly
    /// if the fetch was needed but didn't run.
    fn fetch_objects_from(&self, dst_repo: &Path, src_repo: &Path);

    /// Where `repo`'s checkout last recorded itself, when the branch it has
    /// checked out has since moved from there by a move the checkout did not
    /// make: [`LeftBehind::At`] for a move forward, [`LeftBehind::Rewritten`]
    /// for one to a commit that does not contain it. [`LeftBehind::No`] when
    /// HEAD is where the checkout last put it, when HEAD is not on a branch,
    /// when the index lists no path, and when nothing records where the
    /// checkout was.
    ///
    /// For git: the newest entry of the worktree's own `HEAD` reflog, which
    /// every move made in that worktree appends to and a move made from
    /// another worktree does not.
    fn left_behind_at(&self, _repo: &Path) -> Result<LeftBehind, VcsError> {
        Ok(LeftBehind::No)
    }

    /// Whether `repo` is on a branch and its checkout holds no record at all
    /// of where it last stood, so [`Vcs::left_behind_at`]'s `None` says
    /// nothing about whether the branch moved.
    ///
    /// For git: the worktree's `HEAD` reflog has no entry, as after git's
    /// expiry of an idle checkout's log or a hand expiry, or because
    /// `core.logAllRefUpdates=false` has kept one from ever being written.
    fn checkout_unrecorded(&self, _repo: &Path) -> Result<Option<Unrecorded>, VcsError> {
        Ok(None)
    }

    /// Each path whose index entry or file differs from `HEAD`, with what
    /// each holds measured against `HEAD` and against `left` — where the
    /// checkout's branch moved from, or `None` when it did not move under the
    /// checkout.
    fn path_standings(
        &self,
        _repo: &Path,
        _left: Option<&CommitId>,
    ) -> Result<Vec<PathStanding>, VcsError> {
        Ok(Vec::new())
    }

    /// Give each of `paths` `head`'s content in the index, or with
    /// [`RefreshScope::FilesOnly`] in the working tree alone, removing those
    /// `head` lacks; only where each still holds `left`'s content in the index
    /// and, with files, on disk, each file `left` and `head` both hold is still
    /// on disk, and nothing on disk is in the way of a file `head` adds. Where
    /// any does not, the answer is [`RefreshWrite::Declined`] and nothing is
    /// written. Nothing else in the checkout changes either way.
    fn refresh_paths_if_left(
        &self,
        _repo: &Path,
        _left: &CommitId,
        _head: &CommitId,
        _paths: &[String],
        _scope: RefreshScope,
    ) -> Result<RefreshWrite, VcsError> {
        Ok(RefreshWrite::Written)
    }

    /// Point `repo`'s HEAD at `at`, the tip its files hold, off the branch it
    /// is on, recording which branch it left; index and files are untouched,
    /// so only the edits differ from HEAD afterwards.
    fn set_back(&self, _repo: &Path, _at: &CommitId) -> Result<(), VcsError> {
        Ok(())
    }

    /// Where [`Vcs::set_back`] left `repo`, while HEAD is still there.
    fn set_back_from(&self, _repo: &Path) -> Result<Option<SetBack>, VcsError> {
        Ok(None)
    }

    /// Put `repo` on `branch`, merging every edit against HEAD onto the
    /// branch's tip as the VCS merges local changes on a checkout; a path
    /// that does not merge is left conflicted, and a path declared derived
    /// takes the branch's content. The VCS refuses, writing nothing, where
    /// something in the checkout stops it.
    fn carry_edits_onto(&self, repo: &Path, branch: &str) -> Result<Refusable<()>, VcsError>;

    /// Put `repo` on `branch` as a plain checkout does, carrying the edits
    /// left in place where the branch's tip does not change their paths; the
    /// VCS refuses, writing nothing, where it would have to merge one.
    fn check_out_branch(&self, repo: &Path, branch: &str) -> Result<Refusable<()>, VcsError>;

    /// Keep the edits to `paths` against HEAD, index and working copy, as
    /// the VCS's own set-aside record named for `branch`, and take them out
    /// of the checkout; the record's id.
    fn set_edits_aside(
        &self,
        repo: &Path,
        paths: &[String],
        branch: &str,
    ) -> Result<Refusable<CommitId>, VcsError>;

    /// The records [`Vcs::set_edits_aside`] made in `repo` of edits against
    /// `at` set aside from `branch`, still kept, oldest first. A branch is
    /// checked out in one workspace at a time, so another workspace's record
    /// is never among them.
    fn set_aside_from(
        &self,
        repo: &Path,
        at: &CommitId,
        branch: &str,
    ) -> Result<Vec<CommitId>, VcsError>;

    /// The paths whose index entry differs from HEAD's, an entry only
    /// intended for adding included.
    fn staged_against_head(&self, _repo: &Path) -> Result<Vec<String>, VcsError> {
        Ok(Vec::new())
    }

    /// The paths whose index entry holds a version differing from HEAD's; an
    /// entry only intended for adding holds none.
    fn staged_versions_against_head(&self, _repo: &Path) -> Result<Vec<String>, VcsError> {
        Ok(Vec::new())
    }

    /// The lock a killed VCS process left on `repo`'s index, if any.
    fn index_lock(&self, _repo: &Path) -> Result<Option<std::path::PathBuf>, VcsError> {
        Ok(None)
    }

    /// Record in the checkout's own history that it stands at `head`, so
    /// [`Vcs::left_behind_at`] answers `None` until the branch next moves
    /// under it. Fails, recording nothing, where HEAD no longer names `head`.
    fn record_checkout_at_head(&self, _repo: &Path, _head: &CommitId) -> Result<(), VcsError> {
        Ok(())
    }

    /// Return `true` when `id` names a commit object that exists in
    /// `repo`'s object store.
    ///
    /// For git: runs
    /// `git cat-file -e <id>^{commit}`. Exit 0 means the object exists
    /// and is a commit; any other exit (including the case where `id`
    /// names a non-commit object) returns `false`.
    ///
    /// Used by `rwv doctor`'s provenance checks to detect lock SHAs that
    /// are absent from the local clone — e.g. after an upstream
    /// force-push removed the commit from reachable history, or after a
    /// fresh clone that has never fetched a now-detached SHA.
    fn commit_object_exists(&self, repo: &Path, id: &CommitId) -> Result<bool, VcsError>;

    /// Every ref of the repository at `repo`, and its HEAD when detached,
    /// whose tip no ref of the repository at `other` reaches: history `other`
    /// lacks outright, or holds only where nothing names it. Empty when
    /// `other` reaches all of `repo`'s named history.
    ///
    /// Names are spelled as the VCS spells them in full (`refs/heads/main`,
    /// `refs/stash`, `HEAD`), sorted.
    ///
    /// For git: lists `git for-each-ref` and a detached `HEAD` in `repo`,
    /// asks `other` which tips it holds with `git cat-file --batch-check`,
    /// and which of those its refs reach with `git rev-list <tip>... --not
    /// --all`.
    fn refs_unreachable_from(&self, repo: &Path, other: &Path) -> Result<Vec<String>, VcsError>;

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
    /// Where the store sits says nothing about which checkout owns it:
    /// [`Vcs::main_checkout`] answers that.
    ///
    /// For git: runs `git rev-parse
    /// --path-format=absolute --git-common-dir` in `workspace`.
    fn store_of(&self, workspace: &Path) -> Option<PathBuf>;

    /// The checkout that owns the store `checkout` draws from: `checkout`
    /// itself when it is that checkout, another when `checkout` is linked to
    /// it. `None` when `checkout` is no checkout, or its store has no
    /// checkout of its own.
    ///
    /// For git: the checkout's own top level when its git dir is the common
    /// dir, which holds wherever the clone keeps its store
    /// (`--separate-git-dir` included); else the main worktree `git worktree
    /// list` names first; none for a bare repo.
    fn main_checkout(&self, checkout: &Path) -> Option<PathBuf>;
    /// List paths registered as worktrees of `repo` whose on-disk
    /// directories no longer exist. The administrative entries are still
    /// in the VCS state and will be dropped by [`worktree_prune`].
    ///
    /// For git: parses
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

    /// Drop `repo`'s registration of the worktree at `worktree` when nothing
    /// stands at that path and the registration is not locked; otherwise
    /// leave it and say why. A path whose directory is gone is free for a
    /// new worktree only once its registration is.
    ///
    /// For git: reads `git worktree list --porcelain`, then runs
    /// `git worktree remove` without `--force` on the registered path, which
    /// on a missing directory deletes the administrative entry alone.
    fn prune_stale_worktree_registration(
        &self,
        repo: &Path,
        worktree: &Path,
    ) -> Result<RegistrationPrune, VcsError>;

    /// The local branch each **live** worktree of `repo` has checked out —
    /// the complement of [`list_stale_worktree_registrations`] over the same
    /// listing, minus the detached ones, which hold no name.
    ///
    /// A branch in this set cannot be deleted: git refuses to delete a
    /// branch a worktree is on. A registration whose directory is gone is
    /// refused too, and is deliberately **not** reported here — that one is
    /// a stale registration, which has its own finding and its own prune,
    /// and a delete blocked by it is blocked by a pointer rather than by a
    /// checkout.
    ///
    /// For git: parses `git worktree list --porcelain` and returns the
    /// `branch` line of every record not marked `prunable`, stripped to the
    /// name `list_local_branch_names` would report.
    ///
    /// [`list_stale_worktree_registrations`]: Vcs::list_stale_worktree_registrations
    fn live_worktree_branches(&self, repo: &Path) -> Result<Vec<RawRefName>, VcsError>;

    /// The administrative record through which `store` registers a worktree
    /// at `checkout`, found from the record's own note of where its checkout
    /// is, so it answers whether or not `checkout`'s link still resolves.
    fn worktree_registration(
        &self,
        store: &Path,
        checkout: &Path,
    ) -> Result<Option<PathBuf>, VcsError>;

    /// Point `checkout` back at `registration`, a record of `store` that
    /// names `checkout` as its worktree. HEAD, index and files are kept as
    /// they stand: the checkout is relinked, not checked out.
    fn reattach_registered_worktree(
        &self,
        store: &Path,
        checkout: &Path,
        registration: &Path,
    ) -> Result<(), VcsError>;

    /// Register `checkout`, a directory of files whose link to a store
    /// resolves nowhere, as a worktree of `store` on branch `branch`:
    /// created at `create_at` when given, attached where it stands
    /// otherwise. No file in `checkout` is written, so what differs from the
    /// branch tip reads as an edit.
    fn register_worktree_in_place(
        &self,
        store: &Path,
        checkout: &Path,
        branch: &RawRefName,
        create_at: Option<&ResolvedRevisionId>,
    ) -> Result<(), VcsError>;

    /// Everything the savepoint namespace of `repo` holds: the key of every
    /// savepoint rwv filed, and the refs under the namespace that no key
    /// names.
    ///
    /// For git: enumerates `refs/rwv/pre-op/*` via `git for-each-ref` and
    /// reads the trailing path component of each ref as a [`SavepointKey`].
    /// The ref namespace is an impl detail — callers feed the keys back
    /// through [`resolve_savepoint`] / [`drop_savepoint`].
    ///
    /// Used by `rwv doctor`'s savepoint checks: savepoints that no longer
    /// correspond to a live `.rwv-op` file, and refs that are no savepoint.
    ///
    /// [`resolve_savepoint`]: Vcs::resolve_savepoint
    /// [`drop_savepoint`]: Vcs::drop_savepoint
    fn list_savepoints(&self, repo: &Path) -> Result<SavepointListing, VcsError>;

    /// Read the content of `file_path` at `revision` in `repo`.
    ///
    /// Returns the raw byte content of the file as committed at that
    /// revision. Useful for reading files (e.g. manifests, lock files)
    /// at a pinned commit without touching the working tree — the
    /// snapshot-reads primitive.
    ///
    /// `file_path` is a path relative to the repo root (e.g.
    /// `Path::new("rwv.lock")` or `Path::new("rwv.toml")`).
    ///
    /// Returns [`VcsError::RevisionNotFound`] when `revision` is not
    /// reachable in `repo`, and also when the file does not exist at that
    /// revision (the error's `rev` field carries the full
    /// `<revision>:<path>` spec, naming the absent path).
    ///
    /// For git: runs
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
    /// `"commit 0123456789ab (lock: refresh — post-OOB drift in gc-formulas)"`
    fn rebase_stopped_commit_detail(&self, repo: &Path) -> String;

    /// Return up to `cap` commits of the range `from..to`, newest first,
    /// plus the total commit count in that range.
    ///
    /// Returns `(vec![], 0)` on any error so callers can degrade gracefully
    /// when the range is unresolvable (e.g. the object is unreachable in a
    /// shallow clone).
    fn commits_in_range(
        &self,
        repo: &Path,
        from: &CommitId,
        to: &CommitId,
        cap: usize,
    ) -> (Vec<CommitSummary>, usize);

    /// Return `(ahead, behind)` commit counts for `savepoint..tip` and
    /// `tip..savepoint` respectively. Used to determine whether `tip` is
    /// strictly ahead of `savepoint` (`behind == 0, ahead > 0`) or diverged
    /// (both > 0).
    ///
    /// Returns `(0, 0)` on any VCS error.
    fn ahead_behind(&self, repo: &Path, savepoint: &CommitId, tip: &CommitId) -> (usize, usize);

    /// List the commits reachable from `tip` but NOT from `base`, newest first,
    /// read from `repo`'s store.
    ///
    /// Returns an empty vec when `tip` is an ancestor of, or equal to, `base`.
    /// The result stays correct when `base` has moved on since `tip` forked:
    /// commits `base` already has are excluded because they are reachable
    /// from it.
    ///
    /// For git: computes `git log <base>..<tip>` and maps each entry into a
    /// [`CommitSummary`].
    fn commits_beyond(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        base: &ResolvedRevisionId,
    ) -> Result<Vec<CommitSummary>, VcsError>;

    /// The commits `tip` reaches that none of `excluding` reaches, newest
    /// first; with `superseded`, none that a superseded tip reaches either.
    ///
    /// For git: `git log <tip> --not <excluding>... [--glob=refs/rwv/backup/*/superseded-*]`.
    fn commits_excluding(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        excluding: &[CommitId],
        superseded: bool,
    ) -> Result<Vec<CommitSummary>, VcsError>;

    /// `tip` and its first parents back to the root, newest first.
    ///
    /// For git: `git rev-list --first-parent <tip>`.
    fn first_parent_chain(&self, repo: &Path, tip: &CommitId) -> Result<Vec<CommitId>, VcsError>;

    /// Whether `commit` is `tip` or one of its first parents, read only as
    /// far back as the first commit `commit` reaches.
    ///
    /// For git: `git rev-list --parents --first-parent <tip> --not <commit>`,
    /// whose last line names the chain's first commit `commit` reaches.
    fn on_first_parents(
        &self,
        repo: &Path,
        tip: &CommitId,
        commit: &CommitId,
    ) -> Result<bool, VcsError>;

    /// Every commit `tip` reaches and none of `excluding` reaches, through
    /// any parent, whose content of `path` differs from one of its parents',
    /// or that adds it, newest first: the commits that hold each version of
    /// the file that history has held.
    ///
    /// For git: `git rev-list --full-history <tip> --not <excluding>... -- <path>`.
    fn commits_changing(
        &self,
        repo: &Path,
        tip: &CommitId,
        excluding: &[CommitId],
        path: &str,
    ) -> Result<Vec<CommitId>, VcsError>;

    /// Every commit `tip` reaches, each with its parents in order.
    ///
    /// For git: `git rev-list --parents <tip>`.
    fn ancestry(
        &self,
        repo: &Path,
        tip: &CommitId,
    ) -> Result<Vec<(CommitId, Vec<CommitId>)>, VcsError>;

    /// Which of `unlanded`'s commits carry a change equivalent to a commit
    /// `onto` holds beyond their merge-base, commit by commit, and which
    /// commit holds their combined change; `None` where the backend cannot
    /// tell.
    ///
    /// For git: patch ids, `git cherry <onto> <unlanded>` and the patch id of
    /// the combined diff from the merge-base against each commit of
    /// `<merge-base>..<onto>`.
    fn patch_equivalents(
        &self,
        repo: &Path,
        unlanded: &CommitId,
        onto: &CommitId,
    ) -> Option<PatchEquivalents>;

    /// Produce the unified diff of what `repo`'s HEAD adds beyond `base`,
    /// anchored at the COMMON ANCESTOR of HEAD and `base`.
    ///
    /// Anchoring at the common ancestor — not `base` directly — is what keeps
    /// a `base` that moved on after HEAD forked from producing phantom
    /// reversals: work `base` gained after the fork is not part of HEAD's
    /// history, so it must not appear (as a deletion) in the diff. The
    /// returned [`DiffBeyond::base`] carries the anchor revision so callers
    /// can display it.
    ///
    /// For git: anchors at `git merge-base <base> HEAD` and diffs
    /// `<anchor>..HEAD`.
    fn diff_beyond(&self, repo: &Path, base: &ResolvedRevisionId) -> Result<DiffBeyond, VcsError>;

    /// The commits of `range` in `repo`, newest first, with the commit each
    /// endpoint of the range names there. `range` is in the VCS's own range
    /// syntax and each endpoint resolves as the VCS would resolve it; `None`
    /// is the VCS's log of HEAD.
    ///
    /// An endpoint that names nothing in `repo` is
    /// [`VcsError::RevisionNotFound`], its `rev` the endpoint as spelled.
    ///
    /// For git: `A..B` and `A...B` have endpoints `A` and `B` (an empty side
    /// is HEAD, which names no endpoint), a leading `^` marks one endpoint,
    /// and any other spelling is one endpoint; the listing is `git log` of the
    /// range.
    fn log(&self, repo: &Path, range: Option<&str>) -> Result<RangeListing, VcsError>;

    // =======================================================================
    // The branch model
    // =======================================================================
    //
    // Every ref write rwv performs is exactly one of four kinds — MOVE,
    // ATTACH, DESTROY, DESTROY-STORE — and the kind decides what consent is
    // required. The methods below are grouped by kind, and each
    // takes the proof its kind needs: a witness for a MOVE, a consent token
    // for an ATTACH that is not a birth, a `DestroyProof` for a DESTROY.
    // Store-level destroys are not ref operations and have no method here.
    //
    // These REPLACED `checkout` / `delete_branch` / `current_ref` /
    // `restore_savepoint`, which no longer exist. They were deleted only once
    // every call site had been restated in terms of this surface — and that
    // restatement was the audit: a site that could not say which replacement
    // it meant was a site nobody had classified. The rest of the pre-model
    // surface went with them: `create_worktree` (superseded by
    // `create_worktree_on` / `materialize_worktree_on_ref`), `push_with_role`
    // (by `push_ref`), and `list_branches_with_prefix` (by
    // `list_branch_names_with_prefix`).

    // ---- observation -------------------------------------------------------

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
    /// the same name cannot answer instead.
    fn resolve_local_branch_tip(
        &self,
        repo: &Path,
        name: &RawRefName,
    ) -> Result<Option<ResolvedRevisionId>, VcsError>;

    /// The in-flight operation `repo` is mid-way through, or `None` when it
    /// is in a clean state.
    ///
    /// For git: mid-rebase (`rebase-merge/` or `rebase-apply/` present),
    /// mid-merge (`MERGE_HEAD`), mid-cherry-pick (`CHERRY_PICK_HEAD`) or
    /// mid-bisect (`BISECT_LOG`).
    ///
    /// Broader than [`mid_op`]: that one reports only the operations with a
    /// conflict-resume path, and returns `None` for a bisect, which is
    /// exactly the state the detached-MOVE precondition has to see.
    ///
    /// [`mid_op`]: Vcs::mid_op
    fn mid_operation(&self, repo: &Path) -> Option<MidOperation>;

    /// The directory the VCS keeps `checkout`'s own state in, apart from
    /// what it shares with every other checkout of its store (git: the
    /// worktree's private dir; `.git` itself for a clone).
    fn private_dir(&self, checkout: &Path) -> Result<PathBuf, VcsError>;

    /// Every checkout of `repo`'s store that has `branch` checked out, the
    /// checkout `repo` names included.
    fn checkouts_of_branch(
        &self,
        repo: &Path,
        branch: &RawRefName,
    ) -> Result<Vec<PathBuf>, VcsError>;

    /// The remote branch the VCS records for the local branch `branch`,
    /// checked out or not, or `None` when it records none.
    fn upstream_of(&self, repo: &Path, branch: &RawRefName) -> Result<Option<Upstream>, VcsError>;

    /// Each remote's copy of `branch` as last fetched, one per remote that
    /// has it.
    fn remote_copies(&self, repo: &Path, branch: &RawRefName) -> Result<Vec<RemoteTip>, VcsError>;

    /// Record `upstream` as the branch `branch` takes in from.
    fn set_upstream(
        &self,
        repo: &Path,
        branch: &RawRefName,
        upstream: &Upstream,
    ) -> Result<(), VcsError>;

    /// Rename the local branch `from` to `to`, checked out or not, and stop it
    /// taking in from any remote branch: the branch the VCS recorded for
    /// `from` does not follow the name. Refuses a `to` already taken.
    fn rename_branch(
        &self,
        repo: &Path,
        from: &RawRefName,
        to: &RawRefName,
    ) -> Result<(), VcsError>;

    /// Create the local branch `name` at `at`, taking in from `upstream` when
    /// one is given. No checkout moves. Refuses a name already taken.
    fn create_branch(
        &self,
        repo: &Path,
        name: &RawRefName,
        at: &ResolvedRevisionId,
        upstream: Option<&Upstream>,
    ) -> Result<(), VcsError>;

    /// The commits `tip` reaches that neither `onto`, nor any branch other
    /// than `leaving`, nor any remote-tracking branch, nor any backup
    /// reaches, newest first: what leaving `leaving` for `onto` would leave
    /// on no other ref.
    fn stray_commits(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        leaving: Option<&RawRefName>,
        onto: Option<&ResolvedRevisionId>,
    ) -> Result<Vec<CommitSummary>, VcsError>;

    /// Materialize a worktree of `store` at `dest`, detached at `at`.
    fn materialize_worktree_detached(
        &self,
        store: &Path,
        dest: &Path,
        at: &ResolvedRevisionId,
    ) -> Result<(), VcsError>;

    // ---- MOVE --------------------------------------------------------------

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

    /// Move an already-detached HEAD to `to`.
    ///
    /// This is a MOVE, not an ATTACH: HEAD's symbolic-ness does not change.
    /// It is subject to the mid-operation precondition — a repo
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

    // ---- ATTACH ------------------------------------------------------------

    /// Materialize a worktree at `dest` on `found`.
    ///
    /// This is a **birth**: no consent token, because there was no prior
    /// attachment to lose. The store, the name, and the start point all come
    /// from `found`.
    ///
    /// Says whether this call **authored** the ref or took one that was
    /// already there. Rollback keys on that: a create that adopted a branch
    /// must not delete it on the way out.
    fn create_worktree_on(&self, found: &FoundBranch, dest: &Path) -> Result<RefOrigin, VcsError> {
        self.materialize_worktree_on_ref(found.store(), dest, found.name(), found.start())
    }

    /// VCS-specific half of [`create_worktree_on`]: create a worktree at
    /// `dest` on branch `name`, starting at `start_point`.
    ///
    /// Returns [`RefOrigin::Authored`] when the branch was created by this
    /// call and [`RefOrigin::Adopted`] when a branch of that name already
    /// existed. Implementations must **not** delete a pre-existing branch to
    /// force the authoring path: destroying a ref needs a [`DestroyProof`],
    /// and this call has none.
    ///
    /// [`create_worktree_on`]: Vcs::create_worktree_on
    fn materialize_worktree_on_ref(
        &self,
        store: &Path,
        dest: &Path,
        name: &RawRefName,
        start_point: &ResolvedRevisionId,
    ) -> Result<RefOrigin, VcsError>;

    /// Materialize `url` at `dest` with the checkout attached to `name`,
    /// positioned at the lock scalar `pin` when the new clone holds it, else at
    /// the tip of `declared`; the result says which, and where.
    ///
    /// The **birth** arm of `rwv clone` and of `rwv update` for a member not
    /// on disk: no consent token, because there was no prior attachment to
    /// lose. The birth target is the pin, not the remote tip. Cloning onto the
    /// tip and then aligning would make bootstrapping a weave from a lock that
    /// is behind origin a *rewind*, and a rewinding MOVE needs a backup of the
    /// tip it leaves — so that sequence would back up every repo for nothing. A pin the clone does
    /// not hold is [`PinOutcome::Unreachable`], and a member with no `pin`
    /// starts at the tip with [`PinOutcome::None`]; neither is a failure.
    ///
    /// One call rather than a clone plus an align so there is no way to
    /// point the positioning half at a repo this call did not just create:
    /// a checkout that relocates a branch it does not own is a MOVE, and
    /// the caller holds no witness for one.
    fn clone_attached_at(
        &self,
        url: &CloneUrl,
        dest: &Path,
        name: &LocalRefName,
        declared: &RemoteRef,
        pin: Option<&RawRevisionId>,
    ) -> Result<MemberBirth, VcsError>;

    /// Leave the checkout `from` witnesses on no branch, at `to`.
    ///
    /// Post-birth attachment change: the [`Departure`] is the proof that
    /// alignment found the checkout may leave the branch it is on, which
    /// keeps every commit the branch holds.
    fn detach_head(
        &self,
        from: &AttachedRef,
        to: &ResolvedRevisionId,
        departure: &Departure,
    ) -> Result<(), VcsError> {
        let _ = departure;
        self.verify_attachment(from)?;
        self.set_detached_head(from.repo(), to)
    }

    /// Point the checkout `from` describes at the local branch `to`.
    ///
    /// Takes the whole [`HeadAttachment`] rather than a witness: reattach
    /// is reachable from all three states (including `Unborn`, where the
    /// operator wants off a branch that never got a commit), and the state
    /// it planned against must still hold.
    fn reattach_head(
        &self,
        from: HeadAttachment,
        to: &LocalRefName,
        departure: &Departure,
    ) -> Result<(), VcsError> {
        let _ = departure;
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

    /// Create the local branch `name` at `at` and put `repo`'s HEAD on it in
    /// one step, carrying uncommitted edits as the VCS's own branch creation
    /// does. Refuses, with nothing created and nothing moved, a name already
    /// taken and edits the move to `at` would overwrite.
    fn create_branch_and_attach(
        &self,
        repo: &Path,
        name: &RawRefName,
        at: &ResolvedRevisionId,
        departure: &Departure,
    ) -> Result<(), VcsError>;

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

    // ---- DESTROY -----------------------------------------------------------

    /// Destroy the branch `proof` was made about, at the site it names.
    ///
    /// The proof and no name: there is no way to spell "delete this branch I
    /// recognized". A branch that is checked out, or has moved off the tip
    /// the proof names, is not deleted.
    fn delete_branch(&self, proof: DestroyProof) -> Result<(), VcsError>;

    // ---- publish ----------------------------------------------------------

    /// Push `r` to `target`.
    ///
    /// The ref and the remote are **parameters**, so the choice of what to
    /// publish and where is made at one site in `push.rs` instead of being
    /// implicit inside the VCS impl. What that site passes is still policy;
    /// this signature only makes the decision visible.
    fn push_ref(
        &self,
        repo: &Path,
        target: &RemoteHandle,
        r: &PublishRef,
        replace: &Replace,
    ) -> Result<(), VcsError>;

    /// `remote`'s declared primary branch, or `None` when it is unset or
    /// malformed. **No fallback** — see [`RemoteDefaultBranch`].
    fn remote_default_branch(
        &self,
        repo: &Path,
        remote: &RemoteHandle,
    ) -> Result<Option<RemoteDefaultBranch>, VcsError>;

    /// Human-readable hint for the `None` arm of [`remote_default_branch`]:
    /// what is unset, and the VCS-native ways to record it. Embedded verbatim
    /// in the bail messages of verbs that need the canonical branch and refuse
    /// to guess one.
    ///
    /// Returned text is a clause, not a whole message — the caller supplies
    /// the repo it is talking about before it and the `rwv` verb to re-run
    /// after it.
    ///
    /// ## Seam rule
    ///
    /// This is the one place the repair is spelled. It carries the name of the
    /// unset ref and the native command that sets it, which is why core does
    /// not: respelling the remote name at a call site would leave the command
    /// beside it still naming git's. The impl owns both or neither. It must
    /// not name an rwv verb — that half is the caller's, the same split
    /// [`conflict_resolution_hint`] makes for `ConflictOp::Rebase`.
    ///
    /// [`remote_default_branch`]: Vcs::remote_default_branch
    /// [`conflict_resolution_hint`]: Vcs::conflict_resolution_hint
    fn remote_default_branch_repair_hint(&self, remote: &RemoteHandle) -> String;

    /// [`remote_default_branch_repair_hint`] for `clone`, read from the
    /// clone's copies of the remote's branches. Where those copies show
    /// branches, the remote's default names none of them, and the hint names
    /// the ones it has. Where they show none, `copies` decides what that
    /// means: on a fresh clone the remote has no branch yet and the repair is
    /// a first push; on a clone of unknown age the remote may simply not have
    /// been fetched, so the hint leads with the fetch and names both repairs.
    /// The seam rule above holds here too.
    ///
    /// [`remote_default_branch_repair_hint`]: Vcs::remote_default_branch_repair_hint
    fn clone_default_branch_repair_hint(
        &self,
        clone: &Path,
        remote: &RemoteHandle,
        copies: RemoteBranchCopies,
    ) -> Result<String, VcsError>;

    // ---- listing ----------------------------------------------------------

    /// Local branch names starting with `prefix`, as observed.
    ///
    /// Report-only by type: a [`RawRefName`] is not a [`DestroyProof`], so
    /// nothing in the result can be deleted without a check that ran. That
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
    /// only planned.
    fn create_savepoint_ref(
        &self,
        repo: &Path,
        key: &SavepointKey,
    ) -> Result<SavepointRef, VcsError> {
        let at = self.create_savepoint(repo, key)?;
        Ok(SavepointRef {
            repo: repo.to_path_buf(),
            key: key.clone(),
            at,
        })
    }

    /// Proof for a savepoint an *earlier* call wrote. `None` when there is
    /// none under `key`.
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
    fn resolve_savepoint_ref(&self, repo: &Path, key: &SavepointKey) -> Option<SavepointRef> {
        self.resolve_savepoint(repo, key).map(|at| SavepointRef {
            repo: repo.to_path_buf(),
            key: key.clone(),
            at,
        })
    }

    // ---- backups ----------------------------------------------------------

    /// Write the backup of `tip` for `source`, and return it.
    ///
    /// Idempotent: backing up a tip already held under the same source and
    /// mark names the backup that exists. A caller that must hold proof goes
    /// through [`create_backup`], which is the only producer of one.
    ///
    /// [`create_backup`]: Vcs::create_backup
    fn write_backup(
        &self,
        repo: &Path,
        source: &BackupSource,
        tip: &ResolvedRevisionId,
        mark: Option<BackupMark>,
    ) -> Result<Backup, VcsError>;

    /// Write a backup and return **proof it exists**.
    ///
    /// Provided: this body is the only place a [`BackupWitness`] is
    /// constructed, so the type cannot be minted for a backup that was only
    /// planned.
    fn create_backup(
        &self,
        repo: &Path,
        source: &BackupSource,
        tip: &ResolvedRevisionId,
        mark: Option<BackupMark>,
    ) -> Result<BackupWitness, VcsError> {
        let backup = self.write_backup(repo, source, tip, mark)?;
        Ok(BackupWitness {
            repo: repo.to_path_buf(),
            backup,
        })
    }

    /// Every backup `repo` holds, with the commits each keeps that no branch
    /// reaches. `remote_tracking` says whether remote-tracking branches count
    /// as reaching them.
    fn list_backups(
        &self,
        repo: &Path,
        remote_tracking: RemoteTracking,
    ) -> Result<Vec<ListedBackup>, VcsError>;

    /// Drop the backup `name`. The pin rule is the caller's to apply first.
    fn drop_backup(&self, repo: &Path, name: &BackupName) -> Result<(), VcsError>;

    /// Whether `tip` holds a commit that neither `beside` nor any local or
    /// remote-tracking branch reaches, `branch` itself left out of those.
    fn holds_only_its_own(
        &self,
        repo: &Path,
        tip: &CommitId,
        branch: Option<&RawRefName>,
        beside: &CommitId,
    ) -> Result<bool, VcsError>;

    /// The command an operator runs in `checkout` to see what the backups
    /// `kept` hold that neither `base` nor any local or remote-tracking
    /// branch reaches: each commit with its changes, a working copy's edits
    /// among them.
    fn show_kept(&self, checkout: &Path, base: &CommitId, kept: &[&BackupName]) -> String;

    /// The commits `tip` reaches that no branch reaches, newest first.
    fn reached_by_no_branch(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        remote_tracking: RemoteTracking,
    ) -> Result<Vec<CommitSummary>, VcsError>;

    /// A commit on HEAD holding `repo`'s uncommitted edits as the working
    /// tree stands (untracked files included, ignored ones not), and, where
    /// the index holds content the working tree does not, the index as its
    /// second parent; referenced by nothing; `None` when the checkout is
    /// clean. The checkout is not touched.
    fn snapshot_edits(&self, repo: &Path) -> Result<Option<ResolvedRevisionId>, VcsError>;

    /// Keep `repo`'s uncommitted edits as a backup before anything discards
    /// them; `None` when there are none.
    fn backup_edits(&self, repo: &Path) -> Result<Option<BackupWitness>, VcsError> {
        let Some(snapshot) = self.snapshot_edits(repo)? else {
            return Ok(None);
        };
        let source = self.head_backup_source(repo)?;
        self.create_backup(repo, &source, &snapshot, None).map(Some)
    }

    /// Keep `tip` in the one backup slot of `source`, overwriting what the
    /// slot held, where it holds a commit that neither `beside` nor any
    /// other branch reaches; `None` where it holds none.
    fn keep_in_slot(
        &self,
        repo: &Path,
        source: &BackupSource,
        tip: &CommitId,
        beside: &CommitId,
    ) -> Result<Option<BackupWitness>, VcsError> {
        let branch = match source {
            BackupSource::Branch(branch) => Some(branch),
            BackupSource::DetachedHead => None,
        };
        if !self.holds_only_its_own(repo, tip, branch, beside)? {
            return Ok(None);
        }
        self.create_backup(
            repo,
            source,
            &ResolvedRevisionId::from(tip.clone()),
            Some(BackupMark::Aborted),
        )
        .map(Some)
    }

    /// Keep what a detached HEAD holds as a backup when no branch reaches it;
    /// `None` when HEAD is attached or a branch holds its commits.
    fn backup_unpreserved_head(&self, repo: &Path) -> Result<Option<BackupWitness>, VcsError> {
        let HeadObservation::Detached { at } = self.observe_head(repo)? else {
            return Ok(None);
        };
        if self
            .reached_by_no_branch(repo, &at, RemoteTracking::Counted)?
            .is_empty()
        {
            return Ok(None);
        }
        self.create_backup(repo, &BackupSource::DetachedHead, &at, None)
            .map(Some)
    }

    /// What a backup of `repo`'s HEAD is a backup of.
    fn head_backup_source(&self, repo: &Path) -> Result<BackupSource, VcsError> {
        Ok(match self.observe_head(repo)? {
            HeadObservation::Attached { name } | HeadObservation::Unborn { name } => {
                BackupSource::Branch(name)
            }
            HeadObservation::Detached { .. } => BackupSource::DetachedHead,
        })
    }

    /// Every local and remote-tracking branch of `repo` with its tip.
    fn branch_tips(&self, repo: &Path) -> Result<Vec<BranchTip>, VcsError>;

    // ---- records ----------------------------------------------------------

    /// The name of the branch that holds the weave branches' records.
    ///
    /// It shares no history with a weave branch and is never one: a weave
    /// branch named like it, or below it, is refused.
    fn records_branch(&self) -> &'static str;

    /// The tip of the records branch at `site`, or `None` where it has none.
    fn records_tip(&self, repo: &Path, site: RecordsSite<'_>)
        -> Result<Option<CommitId>, VcsError>;

    /// Move the local records branch from `expected` (`None`: it does not
    /// exist) to `to`, only if it still holds `expected`.
    fn advance_records(
        &self,
        repo: &Path,
        expected: Option<&CommitId>,
        to: &CommitId,
    ) -> Result<RecordsAdvance, VcsError>;

    /// Publish `commit` as the new tip of `remote`'s records branch, a
    /// fast-forward of the tip it was built on: a remote whose records branch
    /// holds anything else is [`RecordsPublish::RemoteMoved`], and one that
    /// refuses the branch is [`RecordsPublish::Rejected`]. A remote that
    /// cannot be reached is an error. No other ref of the remote is written.
    fn publish_records(
        &self,
        repo: &Path,
        remote: &RemoteHandle,
        commit: &CommitId,
    ) -> Result<RecordsPublish, VcsError>;

    /// Every file of `commit`'s tree.
    fn files_at(&self, repo: &Path, commit: &CommitId) -> Result<Vec<CommittedFile>, VcsError>;

    /// The distinct contents `path` has had in the history `tip` reaches,
    /// newest first; none where it never existed.
    fn file_versions(
        &self,
        repo: &Path,
        tip: &ResolvedRevisionId,
        path: &Path,
    ) -> Result<Vec<String>, VcsError>;

    /// A commit with `message` on top of `parents` (none: a commit with no
    /// history), referenced by nothing, whose tree is the first parent's
    /// (empty where there is none) with `changes` applied in order. The author
    /// is the configured one, else rwv. A caller that removes a name and adds
    /// one below it lists the removal first, for a backend that cannot hold both.
    fn write_commit(
        &self,
        repo: &Path,
        parents: &[CommitId],
        changes: &[FileChange],
        message: &str,
    ) -> Result<CommitId, VcsError>;

    /// The best common ancestors of `a` and `b`; empty when they share no
    /// history.
    fn merge_bases(
        &self,
        repo: &Path,
        a: &CommitId,
        b: &CommitId,
    ) -> Result<Vec<CommitId>, VcsError>;

    /// The best common ancestor of `commit` and a merge of every one of
    /// `merged`; `None` when they share no history.
    fn merge_base_against(
        &self,
        repo: &Path,
        commit: &CommitId,
        merged: &[CommitId],
    ) -> Result<Option<CommitId>, VcsError>;

    /// The message of the newest commit reachable from `tip` that changed
    /// `path`; `None` when none did.
    fn last_message_touching(
        &self,
        repo: &Path,
        tip: &CommitId,
        path: &str,
    ) -> Result<Option<String>, VcsError>;

    /// Every remote some local branch of `repo` publishes to, `url_remote`
    /// standing in for a branch the VCS records no other remote for.
    fn branch_push_targets(
        &self,
        repo: &Path,
        url_remote: Option<&RemoteHandle>,
    ) -> Result<Vec<RemoteHandle>, VcsError>;

    /// Every branch of `repo`, local or remote-tracking, once by name.
    fn named_branches(&self, repo: &Path) -> Result<Vec<NamedBranch>, VcsError>;

    // ---- population -------------------------------------------------------

    /// The population questions, when this backend answers them.
    ///
    /// `None` is a complete answer, not a degraded one: every workspace is
    /// still created by [`Vcs::create_worktree_on`], which writes its own
    /// files.
    fn population(&self) -> Option<&dyn WorkspacePopulation> {
        None
    }

    /// The merges this backend computes as trees, or `None` where its release
    /// has no way to (git: before 2.38).
    fn tree_merges(&self) -> Option<&dyn TreeMerges> {
        None
    }

    /// Merge `other` into the checkout, leaving the result staged and
    /// uncommitted, and list the paths left conflicted (none: a clean merge,
    /// ready for [`Vcs::commit_merge`]). A merge already in flight refuses.
    fn start_merge(
        &self,
        checkout: &Path,
        other: &CommitId,
        derived: DerivedContentPolicy,
    ) -> Result<Vec<String>, VcsError>;

    /// The commit a merge in flight in `checkout` is merging in.
    fn merge_in_flight(&self, checkout: &Path) -> Result<Option<CommitId>, VcsError>;

    /// The commit a rebase stopped in `checkout` replays onto, if one is.
    fn rebase_in_flight(&self, checkout: &Path) -> Result<Option<CommitId>, VcsError>;

    /// Forget a merge in flight in `checkout`, leaving its HEAD, index and
    /// working tree as they are.
    ///
    /// For git: `git merge --quit`.
    fn end_merge(&self, checkout: &Path) -> Result<(), VcsError>;

    /// Apply what `commit` introduced, against its parent number `mainline`
    /// (from 1; a merge commit needs one), as a new commit on the checkout,
    /// recording which commit it copies. A copy holding no change is dropped;
    /// a conflict stops in the VCS's own copy in progress, as a merge does.
    ///
    /// For git: `git cherry-pick -x [-m <n>] <commit>`, and `--skip` for a
    /// copy that comes out empty.
    fn copy_commit(
        &self,
        checkout: &Path,
        commit: &CommitId,
        mainline: Option<usize>,
    ) -> Result<Copied, VcsError>;

    /// Finish the copy a conflict stopped in `checkout`, once the operator
    /// staged the resolution; a resolution holding no change drops it.
    ///
    /// For git: `git cherry-pick --continue`, or `--skip` for an empty one.
    fn continue_copy(&self, checkout: &Path) -> Result<Copied, VcsError>;

    /// Apply what `commit` introduced, against `mainline` as in
    /// [`Vcs::copy_commit`], to the checkout's index and files and stop
    /// before the commit, leaving the copy in progress: the next commit made
    /// there ends it, as the copy's, and canceling the operation in flight
    /// undoes it. The lock is held as it is in a merge rwv makes. Returns the
    /// paths left conflicted.
    ///
    /// For git: `git cherry-pick --no-commit [-m <n>] <commit>`, with
    /// `CHERRY_PICK_HEAD` written.
    fn copy_uncommitted(
        &self,
        checkout: &Path,
        commit: &CommitId,
        mainline: Option<usize>,
    ) -> Result<Vec<String>, VcsError>;

    /// The commit a copy in progress in `checkout` is copying, `None` where
    /// no copy is in progress.
    ///
    /// For git: `CHERRY_PICK_HEAD`.
    fn copy_in_flight(&self, checkout: &Path) -> Result<Option<CommitId>, VcsError>;

    /// The commit `commit` records it copies, as [`Vcs::copy_commit`] records
    /// it; `None` where it records none.
    ///
    /// For git: the last `(cherry picked from commit <id>)` line of its
    /// message.
    fn copied_from(&self, repo: &Path, commit: &CommitId) -> Result<Option<CommitId>, VcsError>;

    /// The commits of `commits` that `target` holds a recorded copy of: a
    /// commit `target` reaches, and none of `commits` does, that records it
    /// copies one of them, as [`Vcs::copied_from`] reads it. Never matched by
    /// content.
    fn copied_into(
        &self,
        repo: &Path,
        commits: &[CommitId],
        target: &ResolvedRevisionId,
    ) -> Result<std::collections::BTreeSet<CommitId>, VcsError>;

    /// The parents of `commit`, in order; none for a root commit.
    ///
    /// For git: `git rev-list --parents -n 1 <commit>`.
    fn parents(&self, repo: &Path, commit: &CommitId) -> Result<Vec<CommitId>, VcsError>;

    /// The message of `commit`, without the annotations it records under
    /// `keys`.
    ///
    /// For git: `git log -1 --format=%B`, less each `<key>:` line.
    fn message_without(
        &self,
        repo: &Path,
        commit: &CommitId,
        keys: &[&str],
    ) -> Result<String, VcsError>;

    /// The paths of `checkout` a merge left conflicted and nothing has staged.
    fn unmerged_paths(&self, checkout: &Path) -> Result<Vec<String>, VcsError>;

    /// Of `paths` in `checkout`, those the VCS does not compare with what it
    /// records, so a backup of the working copy would not keep them.
    fn uncompared_paths(
        &self,
        _checkout: &Path,
        _paths: &[String],
    ) -> Result<UncomparedPaths, VcsError> {
        Ok(UncomparedPaths::default())
    }

    /// The three sides a merge recorded for `path`, left conflicted in
    /// `checkout`; `None` when `path` is not conflicted there.
    ///
    /// For git: the index's stages 1, 2 and 3, stages 2 and 3 swapped in a
    /// rebase, whose stage 2 is the commit replayed onto.
    fn conflict_sides(
        &self,
        checkout: &Path,
        path: &str,
    ) -> Result<Option<ConflictSides>, VcsError>;

    /// Record `sides` as the sides of `path` in `checkout`, leaving it
    /// unmerged; its working file is left as it stands.
    ///
    /// For git: `hash-object -w --path=<path>` for each side, then
    /// `update-index --index-info` at stages 1, 2 and 3, stages 2 and 3
    /// swapped in a rebase.
    fn record_conflict(
        &self,
        checkout: &Path,
        path: &str,
        sides: &ConflictSides,
    ) -> Result<(), VcsError>;

    /// `text`, a working file of `path` the VCS left conflicted in
    /// `checkout`, with every region in conflict taken from yours, then from
    /// incoming; `None` where `text` holds no region in conflict, or one that
    /// is not well formed.
    ///
    /// For git: the halves of its conflict markers, `conflict-marker-size`
    /// long, any base section dropped; HEAD's half is incoming in a rebase.
    fn conflict_picks(
        &self,
        checkout: &Path,
        path: &str,
        text: &str,
    ) -> Result<Option<[String; 2]>, VcsError>;

    /// The staged content of `path` in `checkout`, `None` where nothing is
    /// staged there.
    fn staged_file(&self, checkout: &Path, path: &str) -> Result<Option<String>, VcsError>;

    /// Commit what is staged in `checkout` with `first` then `second` as
    /// parents, `message` and `annotations`, moving the checkout's branch (or
    /// detached HEAD) to it and ending any merge in flight.
    fn commit_merge(
        &self,
        checkout: &Path,
        first: &CommitId,
        second: &CommitId,
        message: &str,
        annotations: &[Annotation],
    ) -> Result<CommitId, VcsError>;

    /// The merge of `second` into `first` as a commit, with no checkout and no
    /// ref moved. `derived` resolves a declared path both sides changed as it
    /// does in [`Vcs::start_merge`], where `incoming`, one of `first` and
    /// `second`, is the side that call brings in and the other is the side held
    /// in the checkout: the same content resolves the same way whether or not a
    /// checkout holds the branch. A backend that cannot apply the policy
    /// without a checkout runs the merge in a scratch one, so a path it would
    /// have resolved never reads as a conflict.
    fn merge_as_refs(
        &self,
        repo: &Path,
        first: &CommitId,
        second: &CommitId,
        message: &str,
        derived: DerivedContentPolicy,
        incoming: &CommitId,
    ) -> Result<RefMerge, VcsError>;

    /// Move the local branch `branch` from `expected` to `to`, a fast-forward,
    /// refusing where the branch is no longer at `expected`.
    fn advance_branch_ref(
        &self,
        repo: &Path,
        branch: &RawRefName,
        expected: &CommitId,
        to: &CommitId,
    ) -> Result<(), VcsError>;

    /// Move the local branch `branch` from `expected` to `to`, which need not
    /// descend from it, after `kept` backed `expected` up.
    fn reset_branch_ref(
        &self,
        repo: &Path,
        branch: &RawRefName,
        expected: &CommitId,
        to: &CommitId,
        kept: &BackupWitness,
    ) -> Result<(), VcsError>;

    /// Put the local branch `branch` back at `original`, where an operation
    /// moved it to `moved_to`, refusing where it has moved since.
    fn restore_branch_ref(
        &self,
        repo: &Path,
        branch: &RawRefName,
        moved_to: &CommitId,
        original: &CommitId,
    ) -> Result<(), VcsError>;

    /// Reset the ref `on` witnesses to `to`, which need not descend from it,
    /// after `kept` backed the tip being left up.
    fn reset_attached_ref_keeping(
        &self,
        on: &AttachedRef,
        to: &ResolvedRevisionId,
        kept: &BackupWitness,
    ) -> Result<(), VcsError> {
        let head = self.head_revision(on.repo())?;
        if kept.repo() != on.repo() || kept.backup().tip != head {
            return Err(VcsError::StaleRefWitness {
                repo: on.repo().to_path_buf(),
                expected: format!("a backup of {}", head.id().short()),
                observed: format!("a backup of {}", kept.backup().tip.id().short()),
            });
        }
        self.verify_attachment(on)?;
        self.hard_reset(on.repo(), to)
    }
}

/// Questions that let a workspace's tracked files be written by something
/// other than the backend's checkout, and then checked.
///
/// A caller creates the workspace without its files, learns what a checkout
/// would write, places what it can, has the backend write the rest, and
/// checks out again every placed path that does not hold exactly what the
/// checkout writes. A backend answers all of these or, through
/// [`Vcs::population`], none.
pub trait WorkspacePopulation: Send + Sync {
    /// [`Vcs::create_worktree_on`], writing no tracked file.
    ///
    /// The same branch handling: says whether this call authored the ref. The
    /// new workspace already records what it is a checkout of, so
    /// [`WorkspacePopulation::tracked_files`] answers for it before anything
    /// is written.
    fn create_unpopulated_worktree_on(
        &self,
        found: &FoundBranch,
        dest: &Path,
    ) -> Result<RefOrigin, VcsError> {
        self.materialize_unpopulated_worktree_on_ref(
            found.store(),
            dest,
            found.name(),
            found.start(),
        )
    }

    /// VCS-specific half of
    /// [`WorkspacePopulation::create_unpopulated_worktree_on`], under the
    /// contract of [`Vcs::materialize_worktree_on_ref`].
    fn materialize_unpopulated_worktree_on_ref(
        &self,
        store: &Path,
        dest: &Path,
        name: &RawRefName,
        start_point: &ResolvedRevisionId,
    ) -> Result<RefOrigin, VcsError>;

    /// [`Vcs::materialize_worktree_detached`], writing no tracked file.
    fn materialize_unpopulated_worktree_detached(
        &self,
        store: &Path,
        dest: &Path,
        at: &ResolvedRevisionId,
    ) -> Result<(), VcsError>;

    /// Every tracked file `workspace` records, with what a checkout writes
    /// for it. A path mid-merge records no single content and is omitted.
    ///
    /// For git: the index's entries. A regular file carries an identity only
    /// where checkout writes the blob's bytes unchanged, which rules out a
    /// `filter`, `ident` or `working-tree-encoding` attribute and conversion
    /// to CRLF. The identity is the blob plus every input to that decision.
    fn tracked_files(&self, workspace: &Path) -> Result<Vec<TrackedFile>, VcsError>;

    /// Of `paths`, the files in `workspace` holding exactly the bytes and the
    /// executable bit a checkout writes for them, right now.
    ///
    /// Byte-exact, never a change-detection shortcut: this is the question
    /// that verifies placed files. A path without an identity is never in the
    /// answer.
    ///
    /// For git: hashes each candidate unfiltered and compares the index's
    /// blob. [`Vcs::tracked_dirty_file_names`] cannot stand in: git compares
    /// after normalizing line endings, so a placed CRLF file in a `text=auto`
    /// path reads as unchanged.
    fn files_holding_recorded_content(
        &self,
        workspace: &Path,
        paths: &[String],
    ) -> Result<Vec<TrackedFile>, VcsError>;

    /// Write every tracked file `workspace` records that is missing, leave
    /// every present one as it is, and leave the workspace as the backend's
    /// own checkout would, hooks included.
    ///
    /// Present files are not checked here; that is
    /// [`WorkspacePopulation::files_holding_recorded_content`].
    ///
    /// For git: a `post-checkout` hook that fails is
    /// [`VcsError::HookRejected`], as it is for `worktree add`.
    fn finish_checkout(&self, workspace: &Path) -> Result<(), VcsError>;

    /// Check out `paths` in `workspace` again from what it records, replacing
    /// what is there. Every path must be tracked.
    fn check_out_paths(&self, workspace: &Path, paths: &[String]) -> Result<(), VcsError>;

    /// Whether, on this platform, an update in one workspace changes neither
    /// the content nor the permissions of a file in another workspace that
    /// shares its storage.
    ///
    /// `true` only where a conformance test proves it; nothing may be shared
    /// by hardlink otherwise.
    fn updates_replace_files(&self) -> bool;

    /// The submodules `workspace` records, each with the commit it is
    /// recorded at. One whose name could address a place outside the store
    /// it belongs in is omitted.
    fn recorded_submodules(&self, workspace: &Path) -> Result<Vec<RecordedSubmodule>, VcsError>;

    /// Whether `module_data` holds a copy of `submodule`'s repository with
    /// its recorded commit, read without reaching any remote.
    /// `module_data` is as [`WorkspacePopulation::create_unpopulated_submodule`]
    /// takes it.
    fn module_data_holds(&self, module_data: &Path, submodule: &RecordedSubmodule) -> bool;

    /// Make `submodule`'s directory in `workspace`, which must be empty, a
    /// checkout of it at its recorded commit with no tracked file written,
    /// from the copy of its repository held under `module_data` and from
    /// nothing else.
    ///
    /// `module_data` is the store a checkout of `workspace`'s repository
    /// keeps its submodules' repositories under: the canonical store for a
    /// member, and the returned [`SubmoduleBirth::Born`] source for a
    /// submodule of a submodule.
    ///
    /// No remote outside this machine is reachable from the new checkout
    /// until [`WorkspacePopulation::connect_submodule_upstream`]: content it
    /// lacks is a failure, never a fetch.
    fn create_unpopulated_submodule(
        &self,
        workspace: &Path,
        submodule: &RecordedSubmodule,
        module_data: &Path,
    ) -> Result<SubmoduleBirth, VcsError>;

    /// Give the submodule checkout in `workspace` the remotes of `source`,
    /// the repository it was created from, so a later fetch there reaches
    /// what one in `source` would.
    fn connect_submodule_upstream(
        &self,
        workspace: &Path,
        submodule: &RecordedSubmodule,
        source: &Path,
    ) -> Result<(), VcsError>;

    /// Undo a [`SubmoduleBirth::Born`]: the directory left empty and the
    /// repository created for it removed.
    fn discard_submodule(
        &self,
        workspace: &Path,
        submodule: &RecordedSubmodule,
    ) -> Result<(), VcsError>;

    /// Check out `submodule`, whose directory in `workspace` is empty, and
    /// every submodule nested in it, from their upstream remotes, keeping
    /// each repository in the store where a checkout of `workspace`'s
    /// repository looks for it. The one population question that reaches
    /// the network.
    fn check_out_submodule_from_upstream(
        &self,
        workspace: &Path,
        submodule: &RecordedSubmodule,
    ) -> Result<(), VcsError>;

    /// Fetch `submodule`'s recorded commit from its upstream remote into the
    /// copy of its repository held under `module_data`, leaving every
    /// checkout as it is.
    fn fetch_into_module_data(
        &self,
        module_data: &Path,
        submodule: &RecordedSubmodule,
    ) -> Result<(), VcsError>;
}

/// A repository a workspace records as checked out at `path`, at `commit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedSubmodule {
    pub name: String,
    pub path: String,
    pub commit: CommitId,
    pub checkout: SubmoduleCheckout,
}

/// Whether the repository's own configuration asks for a submodule to be
/// checked out. A declined one is left empty and is not reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmoduleCheckout {
    Wanted,
    /// git: `submodule.<name>.update = none`.
    Declined,
}

/// What [`WorkspacePopulation::create_unpopulated_submodule`] came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmoduleBirth {
    /// The checkout exists, created from `source`.
    Born { source: PathBuf },
    /// The module data holds no copy of the repository with the recorded
    /// commit.
    NotLocal,
    /// The workspace already keeps a repository for this submodule at
    /// `existing`, which is not replaced.
    RepositoryPresent { existing: PathBuf },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_commit_id_deserializes_only_through_parse() {
        let tag: Result<CommitId, _> = serde_json::from_str("\"v2.0\"");
        assert!(tag.is_err());
        let id: CommitId = serde_json::from_str(&format!("\"{}\"", "c".repeat(40))).unwrap();
        assert_eq!(id.short(), "c".repeat(CommitId::SHORT_WIDTH));
    }

    // -----------------------------------------------------------------------
    // The rules that live in the types rather than in a VCS impl. Each is
    // exercised here because "malformed means absent" and "canonical means
    // checked" have to hold without a repo to consult.
    // -----------------------------------------------------------------------

    #[test]
    fn a_commit_id_is_a_full_width_lowercase_hex_object_name() {
        assert!(CommitId::parse(&"a".repeat(40)).is_ok());
        assert!(CommitId::parse(&"0123456789abcdef".repeat(4)[..64]).is_ok());
        // Abbreviated ids are ambiguous by construction; uppercase means the
        // string did not come from a resolution.
        assert!(CommitId::parse(&"a".repeat(7)).is_err());
        assert!(CommitId::parse(&"a".repeat(39)).is_err());
        assert!(CommitId::parse(&"a".repeat(41)).is_err());
        assert!(CommitId::parse(&"A".repeat(40)).is_err());
        assert!(CommitId::parse(&"g".repeat(40)).is_err());
        assert!(CommitId::parse("").is_err());
    }

    #[test]
    fn operator_text_abbreviates_a_commit_id_and_nothing_else() {
        let sha1 = "a".repeat(40);
        let sha256 = "b".repeat(64);
        assert_eq!(short_revision(&sha1), &sha1[..12]);
        assert_eq!(short_revision(&sha256), &sha256[..12]);
        for name in ["v1.0.0", "main", "aaaaaaa", "A".repeat(40).as_str()] {
            assert_eq!(short_revision(name), name);
        }
    }

    #[test]
    fn the_published_commit_id_pattern_admits_what_parse_admits() {
        let schema = serde_json::to_value(schemars::schema_for!(CommitId)).unwrap();
        let pattern = regex::Regex::new(schema["pattern"].as_str().expect("a pattern")).unwrap();
        let mut candidates = vec!["0123456789abcdef".repeat(4), format!("{} ", "a".repeat(40))];
        for width in [0, 7, 39, 40, 41, 63, 64, 65] {
            for digit in ['0', 'a', 'f', 'A', 'F', 'g'] {
                candidates.push(std::iter::repeat_n(digit, width).collect());
            }
        }
        for candidate in candidates {
            assert_eq!(
                pattern.is_match(&candidate),
                CommitId::parse(&candidate).is_ok(),
                "{candidate:?}"
            );
        }
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

    // -----------------------------------------------------------------------
    // Derived-content policy
    // -----------------------------------------------------------------------

    #[test]
    fn each_constructor_names_the_resolution_it_is_named_for() {
        // The constructors are the entire public vocabulary, so a swapped
        // body would hand every rwv replay the opposite resolution while
        // every call site still read correctly.
        assert_eq!(
            DerivedContentPolicy::keep_target_side().resolution(),
            DerivedContentResolution::KeepTargetSide
        );
        assert_eq!(
            DerivedContentPolicy::vcs_default().resolution(),
            DerivedContentResolution::VcsDefault
        );
    }

    #[test]
    fn the_policies_are_distinguishable() {
        // A parameter with one inhabitant is a token, not a policy: the
        // operations that take one would be free to ignore it and no caller
        // could tell.
        assert_ne!(
            DerivedContentPolicy::keep_target_side(),
            DerivedContentPolicy::vcs_default()
        );
    }
}
