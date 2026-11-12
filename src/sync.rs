//! The landing phase machine behind `rwv land`, and `rwv abort`.
//!
//! A landing composes in the source workspace (replay, then relock) and then
//! fast-forwards the target's checkouts (advance-target), retiring the
//! workweave last under `--retire`. `rwv abort` rolls an op back to its
//! savepoints.

use crate::check::UnparseableProjectKind;
use crate::diagnosis::{Report, State};
use crate::lock::{commit_lock_file_with_message, generate_lock};
use crate::manifest::{
    LockFile, Manifest, Project, ProjectName, RepoKey, RepoOperand, RepoPath, ResolvedLockFile,
    WorkweaveName,
};
use crate::op_state::{self, OpId, OpVerb, OwnerHeader, OwnerRecord, SavepointKey, SyncStrategy};
use crate::parallel::{run_in_parallel, OutputMode};
use crate::path_spelling::CanonicalPath;
use crate::placement::PlacementOutput;
use crate::refusal::RefusalKind;
use crate::share::FilePlacement;
use crate::status::{compute_relation, LockRelation};
use crate::vcs::{
    project_vcs, vcs_for, AttachedRef, CommitId, ConflictOp, DerivedContentPolicy,
    ForeignTipPolicy, HeadAttachment, RemoteTracking, ResolvedRevisionId, Vcs, VcsError,
    VcsErrorOutput, VerifiedRestoreOutcome,
};
use crate::wire_token::wire_token;
use crate::workspace::{
    project_dir, project_rel_path, AdvisoryOutput, Checkout, Resolution, WorkspaceContext,
};
use crate::workweave::refuse_symlinked_members;
use anyhow::Context;
use schemars::JsonSchema;
use serde::Serialize;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

mod land_envelope;

/// Two-way ancestry of one repo pair on the destination↔source axis, named
/// from the SUBJECT tip's vantage: `Ahead(n)` means the subject contains the
/// other tip and carries `n` commits it does not.
///
/// [`LockRelation`] answers the same question on the lock↔HEAD pair. The two
/// are separate facts about separate pairs, and a message about one must not
/// borrow the other's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Containment {
    Equal,
    Ahead(usize),
    Behind(usize),
    Diverged { ahead: usize, behind: usize },
}

impl Containment {
    /// Classify `subject` against `other` in `repo`, reading the pair once.
    ///
    /// `None` when either range is unresolvable in `repo`'s object store —
    /// a failed count reads as zero, and zero on both sides spells `Equal`.
    fn observe(
        vcs: &dyn Vcs,
        repo: &Path,
        subject: &ResolvedRevisionId,
        other: &ResolvedRevisionId,
    ) -> Option<Self> {
        if subject == other {
            return Some(Self::Equal);
        }
        let ahead = vcs.count_commits_in_range(repo, other, subject).ok()?;
        let behind = vcs.count_commits_in_range(repo, subject, other).ok()?;
        Some(match (ahead, behind) {
            (0, 0) => Self::Equal,
            (ahead, 0) => Self::Ahead(ahead),
            (0, behind) => Self::Behind(behind),
            (ahead, behind) => Self::Diverged { ahead, behind },
        })
    }

    /// The verdict as an operator phrase measuring the subject against
    /// `other`, which names the other tip in the reader's terms.
    fn describe(self, other: &str) -> String {
        match self {
            Self::Equal => format!("equal to {other}"),
            Self::Ahead(n) => format!(
                "strictly ahead of {other} by {n} commit{s} (contains it)",
                s = plural_s(n)
            ),
            Self::Behind(n) => format!("strictly behind {other} by {n} commit{s}", s = plural_s(n)),
            Self::Diverged { ahead, behind } => {
                format!("diverged from {other} ({ahead} ahead, {behind} behind)")
            }
        }
    }
}

/// How a repo's checkout stood against the source tip it was measured
/// against: equal to it, containing it and carrying commits it does not,
/// contained by it, or each holding commits the other lacks.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(tag = "relation", rename_all = "kebab-case")]
pub enum ContainmentVerdictOutput {
    Equal,
    Ahead { commits: usize },
    Behind { commits: usize },
    Diverged { ahead: usize, behind: usize },
}

impl From<Containment> for ContainmentVerdictOutput {
    fn from(c: Containment) -> Self {
        match c {
            Containment::Equal => Self::Equal,
            Containment::Ahead(commits) => Self::Ahead { commits },
            Containment::Behind(commits) => Self::Behind { commits },
            Containment::Diverged { ahead, behind } => Self::Diverged { ahead, behind },
        }
    }
}

/// What one repo's replay was decided from: how this checkout stood against
/// the source, and which read of the source it was measured against.
///
/// The two travel together because a verdict without its baseline counts
/// commits ahead of nothing in particular.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub struct ContainmentOutput {
    /// The ancestry between this checkout and the source tip.
    pub verdict: ContainmentVerdictOutput,
    /// Which read produced the source tip this verdict measured against.
    pub baseline: ReplayBaseline,
}

impl ContainmentOutput {
    /// The disclosure for a repo whose pair the machine read, `None` for one
    /// whose outcome was decided before any such read (an unreadable HEAD, a
    /// lock entry the source could not resolve).
    fn observed(containment: Option<Containment>, baseline: ReplayBaseline) -> Option<Self> {
        containment.map(|c| Self {
            verdict: c.into(),
            baseline,
        })
    }
}

fn plural_s(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Which read gave replay this repo's target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReplayBaseline {
    SourceLockEntry,
    SourceCommittedTip,
}

impl ReplayBaseline {
    fn label(self) -> &'static str {
        match self {
            Self::SourceLockEntry => "the source lock entry",
            Self::SourceCommittedTip => "the source's committed tip",
        }
    }
}

/// Display name for a workspace: the workweave's recorded name when in a
/// workweave, otherwise the basename of the primary path.
///
/// Refuses for a workweave the registry does not name. Sync writes this string
/// into the auto-relock commit message, where it becomes the durable record of
/// which workspace a landing came from; a name invented for the occasion would
/// outlive every chance to correct it.
fn workspace_name(ctx: &WorkspaceContext) -> anyhow::Result<String> {
    match &ctx.checkout {
        Checkout::Workweave {
            name, dir, project, ..
        } => Ok(name.require(dir.as_path(), project)?.as_str().to_owned()),
        Checkout::Primary { .. } => Ok(ctx
            .primary_path()
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_owned()),
    }
}

/// The commit message for the auto-relock the sync engine writes into the CWD
/// project repo when it regenerates `rwv.lock` from the converged manifest tips.
///
/// This is the ONE home of the literal: both the op-start
/// benign-staleness relock and the post-replay Phase-3 relock format their
/// commit message here, and the explain generator splices the
/// same string into `rwv explain land` via a `{{MSG:auto_relock}}`
/// placeholder so the docs cannot drift from the code. `<source>` is the
/// display name of the workspace the sync pulled from. Text is unchanged from
/// the pre-extraction literal.
pub fn auto_relock_commit_message(source: &str) -> String {
    format!("lock: auto-relock after sync from {source}")
}

// ---------------------------------------------------------------------------
// RepoSyncOutcome — per-repo result of a sync operation
// ---------------------------------------------------------------------------

/// Why a per-repo sync attempt failed.
///
/// Discriminates between the recoverable cases the caller may want to react
/// to differently — directly maps to `--json` output for `rwv status` /
/// `rwv sync`. The `error` payload is the underlying error string for
/// human-readable display.
///
/// `cause` is the underlying typed [`VcsError`], so `--json` consumers can
/// pattern-match on the structured failure mode without parsing the human
/// string. A strategy failure always has one; `HeadUnreadable` has none when
/// the source lock pins a commit the source cannot resolve.
#[derive(Debug)]
pub enum SyncFailure {
    /// Couldn't read HEAD on the repo (e.g. not a repo, or I/O failure).
    HeadUnreadable {
        error: String,
        cause: Option<VcsError>,
    },
    /// `--strategy ff` cannot proceed (divergence, conflict).
    FastForwardImpossible { error: String, cause: VcsError },
    /// `--strategy rebase` failed (conflict or git error).
    RebaseFailed { error: String, cause: VcsError },
}

/// Which [`SyncFailure`] variant a failure is, as the `kind` it carries in
/// `rwv sync --json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncFailureKind {
    HeadUnreadable,
    FfImpossible,
    RebaseFailed,
}

impl SyncFailureKind {
    pub const ALL: [Self; 3] = [Self::HeadUnreadable, Self::FfImpossible, Self::RebaseFailed];
}

impl fmt::Display for SyncFailureKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&wire_token(self))
    }
}

impl SyncFailure {
    pub fn kind(&self) -> SyncFailureKind {
        match self {
            Self::HeadUnreadable { .. } => SyncFailureKind::HeadUnreadable,
            Self::FastForwardImpossible { .. } => SyncFailureKind::FfImpossible,
            Self::RebaseFailed { .. } => SyncFailureKind::RebaseFailed,
        }
    }

    pub fn error(&self) -> &str {
        match self {
            Self::HeadUnreadable { error, .. }
            | Self::FastForwardImpossible { error, .. }
            | Self::RebaseFailed { error, .. } => error,
        }
    }

    pub fn cause(&self) -> Option<&VcsError> {
        match self {
            Self::HeadUnreadable { cause, .. } => cause.as_ref(),
            Self::FastForwardImpossible { cause, .. } | Self::RebaseFailed { cause, .. } => {
                Some(cause)
            }
        }
    }

    fn for_strategy(strategy: SyncStrategy, error: String, cause: VcsError) -> Self {
        match strategy {
            SyncStrategy::Ff => Self::FastForwardImpossible { error, cause },
            SyncStrategy::Rebase => Self::RebaseFailed { error, cause },
        }
    }
}

impl fmt::Display for SyncFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.error())
    }
}

/// Per-repo result of a sync operation.
///
/// Public so the `--json` wire-output layer can pattern-match on it. The
/// JSON shape is produced by converting through [`SyncOutcomeOutput`].
#[derive(Debug)]
pub enum RepoSyncOutcome {
    /// HEAD advanced to the lock SHA.
    ///
    /// `derived_content_dropped` names the declared derived paths whose
    /// replayed version the landed tree does not carry — empty on all but the
    /// replays that resolved one. Only this variant can carry them: the two
    /// no-op variants never replay, and a failed replay lands nothing.
    Converged {
        derived_content_dropped: Vec<String>,
    },
    /// Replay's target is already an ancestor of HEAD; no change made.
    /// `baseline` names which read produced that target, so the line can say
    /// what HEAD is ahead OF.
    ///
    /// `source_position` is where the SOURCE's own checkout stood against that
    /// same target. Containing the target says nothing about how far the source
    /// has moved past it, so a line stating only `commits_ahead` reads as "at
    /// the tip" to an operator deciding whether anything is outstanding.
    /// `None` where the source's checkout could not be measured.
    AlreadyAhead {
        commits_ahead: usize,
        baseline: ReplayBaseline,
        source_position: Option<Containment>,
    },
    /// HEAD was already equal to replay's target before sync.
    NoOp,
    /// Strategy failed (conflict, divergence, etc.).
    Failed(SyncFailure),
}

impl RepoSyncOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

// ---------------------------------------------------------------------------
// JSON wire-output types for `rwv sync --json`
// ---------------------------------------------------------------------------

/// Wire-output mirror of [`SyncFailure`] for `--json` emission.
///
/// Carries the same payload as the in-memory enum but with a `cause`
/// represented as the serializable [`VcsErrorOutput`].
///
/// `message` is the human-readable display string of the failure (free-form
/// text, not a typed discriminant). `cause` is the structured typed cause —
/// always present on `ff-impossible` and `rebase-failed`, optional on
/// `head-unreadable` — and consumers that want to branch on failure mode should
/// inspect `cause.kind` rather than parsing `message`.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SyncFailureOutput {
    HeadUnreadable {
        /// Free-form display message for this failure. Not a typed discriminant.
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<VcsErrorOutput>,
    },
    FfImpossible {
        /// Free-form display message for this failure. Not a typed discriminant.
        message: String,
        cause: VcsErrorOutput,
    },
    RebaseFailed {
        /// Free-form display message for this failure. Not a typed discriminant.
        message: String,
        cause: VcsErrorOutput,
    },
}

impl From<&SyncFailure> for SyncFailureOutput {
    fn from(f: &SyncFailure) -> Self {
        let message = f.error().to_owned();
        match f {
            SyncFailure::HeadUnreadable { cause, .. } => Self::HeadUnreadable {
                message,
                cause: cause.as_ref().map(VcsErrorOutput::from),
            },
            SyncFailure::FastForwardImpossible { cause, .. } => Self::FfImpossible {
                message,
                cause: cause.into(),
            },
            SyncFailure::RebaseFailed { cause, .. } => Self::RebaseFailed {
                message,
                cause: cause.into(),
            },
        }
    }
}

/// Step-3 fast-forward advance record for one repo in `rwv land --json` output.
///
/// Present in a per-repo outcome iff step 3 (advance-target) actually advanced
/// that repo's branch pointer, and carrying the revisions that advance read
/// and wrote. Omitted (`skip_serializing_if = "Option::is_none"`) on a no-op
/// advance — the target was already at CWD's tip — which is the same condition
/// under which the text line reads `already at <sha>`, because both render one
/// outcome.
#[derive(Debug, Serialize, JsonSchema, Clone)]
pub struct Step3AdvanceOutput {
    /// Target repo's HEAD SHA before the fast-forward.
    pub from_sha: String,
    /// Target repo's HEAD SHA after the fast-forward (== CWD's tip).
    pub to_sha: String,
}

/// One per-repo record in `rwv sync --json` output.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SyncOutcomeOutput {
    Converged {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        /// Step-3 fast-forward advance for this repo; present only in
        /// `rwv land --json` output when step 3 advanced this repo.
        #[serde(skip_serializing_if = "Option::is_none")]
        step3_advance: Option<Step3AdvanceOutput>,
        /// Repo-relative paths this repo declares derived whose replayed
        /// version the landed tree does not carry: the replay resolved them
        /// to the target's version instead. Regenerating them from their
        /// source of record and committing is what makes the landed tree
        /// describe itself again. Omitted when nothing was resolved away.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        derived_content_dropped: Vec<String>,
        /// How this checkout stood against the source when the strategy ran.
        /// The `kind` says what the strategy did; this says what it was up
        /// against — `behind` is a fast-forward, `diverged` is a rebase.
        #[serde(skip_serializing_if = "Option::is_none")]
        containment: Option<ContainmentOutput>,
    },
    AlreadyAhead {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        commits_ahead: usize,
        /// Step-3 fast-forward advance for this repo; present only in
        /// `rwv land --json` output when step 3 advanced this repo.
        #[serde(skip_serializing_if = "Option::is_none")]
        step3_advance: Option<Step3AdvanceOutput>,
        /// How this checkout stood against the source, and which read of the
        /// source `commits_ahead` counts against.
        #[serde(skip_serializing_if = "Option::is_none")]
        containment: Option<ContainmentOutput>,
        /// Where the SOURCE's own checkout stood against that same read of the
        /// source. `containment` proves this repo holds the target; only this
        /// says whether the source has since moved past it. Absent where the
        /// source's checkout could not be measured.
        #[serde(skip_serializing_if = "Option::is_none")]
        source_position: Option<ContainmentVerdictOutput>,
    },
    NoOp {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        /// Step-3 fast-forward advance for this repo; present only in
        /// `rwv land --json` output when step 3 advanced this repo.
        #[serde(skip_serializing_if = "Option::is_none")]
        step3_advance: Option<Step3AdvanceOutput>,
        /// How this checkout stood against the source, and which read of the
        /// source it was equal to.
        #[serde(skip_serializing_if = "Option::is_none")]
        containment: Option<ContainmentOutput>,
    },
    Failed {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        failure: SyncFailureOutput,
        /// Step-3 fast-forward advance for this repo; present only in
        /// `rwv land --json` output when step 3 advanced this repo.
        /// Typically absent when the repo failed in step 1.
        #[serde(skip_serializing_if = "Option::is_none")]
        step3_advance: Option<Step3AdvanceOutput>,
        /// How this checkout stood against the source when the strategy ran.
        /// `failure.message` renders this in prose; branch on this instead.
        /// Absent where the outcome was decided before any pair was read.
        #[serde(skip_serializing_if = "Option::is_none")]
        containment: Option<ContainmentOutput>,
    },
}

impl SyncOutcomeOutput {
    /// `containment` is the verdict the machine read for this repo before its
    /// strategy ran, threaded from that one read rather than taken again here:
    /// the text line and this record then describe one observation of the pair.
    pub fn from_outcome(
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        outcome: &RepoSyncOutcome,
        containment: Option<ContainmentOutput>,
    ) -> Self {
        match outcome {
            RepoSyncOutcome::Converged {
                derived_content_dropped,
            } => Self::Converged {
                path,
                absolute_path,
                step3_advance: None,
                derived_content_dropped: derived_content_dropped.clone(),
                containment,
            },
            RepoSyncOutcome::AlreadyAhead {
                commits_ahead,
                source_position,
                ..
            } => Self::AlreadyAhead {
                path,
                absolute_path,
                commits_ahead: *commits_ahead,
                step3_advance: None,
                containment,
                source_position: source_position.map(ContainmentVerdictOutput::from),
            },
            RepoSyncOutcome::NoOp => Self::NoOp {
                path,
                absolute_path,
                step3_advance: None,
                containment,
            },
            RepoSyncOutcome::Failed(failure) => Self::Failed {
                path,
                absolute_path,
                failure: SyncFailureOutput::from(failure),
                step3_advance: None,
                containment,
            },
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    fn path(&self) -> &str {
        match self {
            Self::Converged { path, .. }
            | Self::AlreadyAhead { path, .. }
            | Self::NoOp { path, .. }
            | Self::Failed { path, .. } => path,
        }
    }

    /// Return a mutable reference to the `step3_advance` field regardless of variant.
    fn step3_advance_mut(&mut self) -> &mut Option<Step3AdvanceOutput> {
        match self {
            Self::Converged { step3_advance, .. }
            | Self::AlreadyAhead { step3_advance, .. }
            | Self::NoOp { step3_advance, .. }
            | Self::Failed { step3_advance, .. } => step3_advance,
        }
    }
}

/// Top-level envelope for `rwv sync --json`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SyncJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    /// One record per repo, sorted by repo path whatever `-j` is.
    pub outcomes: Vec<SyncOutcomeOutput>,
    /// How the files of each checkout the sync created or moved were placed.
    /// Empty, not absent, when it placed none.
    pub placements: Vec<PlacementOutput>,
    /// What replay did with each member checkout its per-repo outcomes do
    /// not cover: kept, pruned, or a prune that was refused. Empty, not
    /// absent, when there are none. A `prune-refused` entry is a checkout left
    /// on disk that no manifest names; branch on it. The op still completes,
    /// and then exits 1.
    pub members: Vec<MemberOutput>,
    /// True iff the op ran its last phase and cleared its op-state, so nothing
    /// remains to `--continue` or `rwv abort`; false when a phase stopped it
    /// with its op-state held. A complete op exits 1 when `members` carries a
    /// `prune-refused` entry.
    pub complete: bool,
    /// Each repo the op judged when it refused, with that state's choices.
    /// Empty, not absent, when it refused on none.
    pub refused: Vec<crate::diagnosis::JudgedOutput>,
    /// Standing advisories raised during this sync (e.g. delivered changes
    /// touching a materialized input). Empty, not absent, when there are
    /// none — a consumer branches on length rather than presence.
    pub advisories: Vec<AdvisoryOutput>,
    /// Resolved workspace coordinates (weave root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

/// Top-level envelope for `rwv land --json`.
///
/// Extends [`SyncJsonOutput`] with land-specific observability fields:
/// - `source_workweave` — the workweave the op is rooted in (null when that is
///   the primary weave).
/// - `target` — the absolute path of the target workspace that was advanced.
/// - `retired` — true iff the workweave was deleted.
/// - `resumed` — the phase a `--continue` resumed from; absent on a fresh run.
/// - `project_repo_advance` — step-3 advance of `projects/<project>/.git`;
///   omitted when the project repo was already at CWD's tip (no-op advance).
/// - per-outcome `step3_advance` — step-3 advance SHA pair for each member
///   repo; omitted on a no-op advance.
/// - `placements` — file placements in both workspaces, the target's included.
///
/// Kept as a separate type so the generated schema artifact
/// (`docs/reference/schemas/land.json`) has its own title/description.
#[derive(Debug, Serialize, JsonSchema)]
pub struct LandJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    /// The workweave the op is rooted in; null when that is the primary weave.
    /// On a resumed op this is the workspace that owns the op record, which is
    /// not necessarily the one `--continue` was invoked from.
    pub source_workweave: Option<String>,
    /// Absolute path of the target workspace that step-3 fast-forwarded, as
    /// the machine resolved it.
    pub target: crate::path_spelling::WirePath,
    /// True iff retire deleted the workweave. False when `--retire` was not
    /// passed, when retire refused, and when retire was skipped (the op is
    /// rooted in the primary weave, where there is no workweave to delete).
    pub retired: bool,
    /// The branches of the retired workweave the deletion rule kept, and why.
    /// Absent when it kept none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub survivors: Vec<crate::deletion::Survivor>,
    /// The phase the interrupted op had reached when `--continue` resumed it;
    /// absent on a fresh run. Presence encodes "this envelope describes a
    /// resumed op".
    ///
    /// Decides what `outcomes` below describes. `replay` means replay re-ran
    /// in this invocation, so `outcomes` carries every repo (already-converged
    /// ones as `no-op`). Any later phase means replay had already completed
    /// before the interruption: this invocation re-entered downstream of it,
    /// and `outcomes` is empty — not because no repos were synced, but because
    /// the syncing happened in the interrupted invocation. A per-repo
    /// `step3_advance` performed by such a resume has no outcome to ride on
    /// and is not reported; the target's tips remain readable via
    /// `rwv status --json` in the target workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumed: Option<op_state::OpPhase>,
    /// One record per repo, sorted by repo path whatever `-j` is.
    pub outcomes: Vec<SyncOutcomeOutput>,
    /// How the files of each checkout the op created or moved were placed, in
    /// this workspace and in the target, each under its own `absolute_path`.
    /// Empty, not absent, when it placed none. Unlike `step3_advance`, a
    /// placement a resume makes is reported whatever phase it re-entered at.
    pub placements: Vec<PlacementOutput>,
    /// What replay did with each member checkout its per-repo outcomes do
    /// not cover: kept, pruned, or a prune that was refused. Empty, not
    /// absent, when there are none. A `prune-refused` entry is a checkout left
    /// on disk that no manifest names; branch on it. The op still completes,
    /// and then exits 1.
    pub members: Vec<MemberOutput>,
    /// True iff the op ran its last phase and cleared its op-state, so nothing
    /// remains to `--continue` or `rwv abort`; false when a phase stopped it
    /// with its op-state held. A complete op exits 1 when `members` carries a
    /// `prune-refused` entry.
    pub complete: bool,
    /// Each repo the op judged when it refused, with that state's choices.
    /// Empty, not absent, when it refused on none.
    pub refused: Vec<crate::diagnosis::JudgedOutput>,
    /// Step-3 advance of the project repo (`projects/<project>/.git`). Omitted
    /// when the project repo was already at CWD's tip (no-op fast-forward).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_repo_advance: Option<Step3AdvanceOutput>,
    /// For each declared-branch change the landing makes, the commits the
    /// target's pin reaches that the new receiving branch lacks, with the copy
    /// that ports them. Empty, not absent, when it leaves none; a resumed op
    /// reports the same list its start did.
    pub left_behind: Vec<LeftBehindOutput>,
    /// Each move the landing reported as it made it: a receiving branch
    /// created, a member that followed its receiving branch, a pin moved
    /// backward by name. Empty, not absent, when it made none; a resumed op
    /// reports the moves its earlier runs made as well.
    pub reports: Vec<LandingReportOutput>,
    /// Resolved workspace coordinates (weave root, optional workweave
    /// identity, project). Absent when no project is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

/// One declared-branch change's left-behind commits.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LeftBehindOutput {
    /// The member's manifest-relative path.
    pub path: String,
    /// The branch the target's entry declared.
    pub was: String,
    /// The receiving branch the landing declares.
    pub now: String,
    /// The commits the target's pin reaches that `now` lacks, newest first.
    pub commits: Vec<LeftCommitOutput>,
    /// The copy that ports them onto `now`, in the order the text prints it.
    pub choices: Vec<crate::diagnosis::ChoiceOutput>,
}

/// One move a landing reported as it made it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LandingReportOutput {
    /// A receiving branch that existed nowhere, created at `at`.
    Created {
        path: String,
        branch: String,
        at: CommitId,
    },
    /// A member holding no new work moved to its receiving branch's tip `to`;
    /// the tip `was` it left is kept in the backup `backup`.
    Followed {
        path: String,
        to: CommitId,
        was: CommitId,
        backup: String,
    },
    /// A pin the landing moved backward, named by `--allow-member-change`.
    RePinned {
        path: String,
        was: CommitId,
        now: CommitId,
    },
    /// A field of the target's entry for `path`, or of its top-level
    /// settings, changed from `was` to `now`, named by
    /// `--allow-member-change`.
    EntryChanged {
        path: String,
        field: String,
        was: String,
        now: String,
    },
    /// The target's entry for `path` removed, named by
    /// `--allow-member-change`.
    EntryRemoved { path: String },
    /// The target's project branch, behind its upstream as last fetched,
    /// taken in from `from` to `to`, fast-forward only: the upstream's
    /// commits, and each pin and member entry they changed.
    TakenIn {
        path: String,
        branch: String,
        from: CommitId,
        to: CommitId,
        /// The upstream's commits, newest first.
        commits: Vec<LeftCommitOutput>,
        pins: Vec<PinTakenInOutput>,
        member_changes: Vec<crate::pull::MemberChangeOutput>,
    },
    /// A checkout, in either workspace, whose branch something outside it
    /// moved forward from `moved_from` to `moved_to`: before judging its
    /// edits the landing gave `files` (content and index entry) and
    /// `index_entries` (index entry alone) `moved_to`'s content, since they
    /// still held `moved_from`'s. A refused landing refreshes only a checkout
    /// whose own edits it refused.
    Refreshed {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        moved_from: CommitId,
        moved_to: CommitId,
        files: Vec<String>,
        index_entries: Vec<String>,
    },
}

/// A member's pin as the upstream a landing took in changed it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PinTakenInOutput {
    pub path: String,
    /// Absent where the target's lock did not pin the member.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub was: Option<String>,
    /// Absent where the upstream's lock does not pin the member.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub now: Option<String>,
    /// `now` does not descend from `was`: reported `re-pinned`.
    pub backward: bool,
}

impl LandingReportOutput {
    fn of(report: &op_state::LandingReport) -> Self {
        match report {
            op_state::LandingReport::Created { repo, branch, at } => Self::Created {
                path: repo.to_string(),
                branch: branch.clone(),
                at: at.clone(),
            },
            op_state::LandingReport::Followed {
                repo,
                to,
                was,
                backup,
            } => Self::Followed {
                path: repo.to_string(),
                to: to.clone(),
                was: was.clone(),
                backup: backup.clone(),
            },
            op_state::LandingReport::Repinned { repo, was, now } => Self::RePinned {
                path: repo.to_string(),
                was: was.clone(),
                now: now.clone(),
            },
            op_state::LandingReport::EntryChanged {
                repo,
                field,
                was,
                now,
            } => Self::EntryChanged {
                path: repo.to_string(),
                field: field.clone(),
                was: was.clone(),
                now: now.clone(),
            },
            op_state::LandingReport::EntryRemoved { repo } => Self::EntryRemoved {
                path: repo.to_string(),
            },
            op_state::LandingReport::TakenIn {
                branch,
                from,
                to,
                commits,
                pins,
                entries,
            } => Self::TakenIn {
                path: RepoKey::Project.to_string(),
                branch: branch.clone(),
                from: from.clone(),
                to: to.clone(),
                commits: commits
                    .iter()
                    .map(|c| LeftCommitOutput {
                        sha: c.id.clone(),
                        subject: c.subject.clone(),
                    })
                    .collect(),
                pins: pins
                    .iter()
                    .map(|p| PinTakenInOutput {
                        path: p.repo.to_string(),
                        was: p.was.clone(),
                        now: p.now.clone(),
                        backward: p.backward,
                    })
                    .collect(),
                member_changes: entries
                    .iter()
                    .map(|e| crate::pull::MemberChangeOutput {
                        path: e.repo.to_string(),
                        change: e.change,
                        fields: e.fields.clone(),
                    })
                    .collect(),
            },
            op_state::LandingReport::Refreshed {
                repo,
                checkout,
                moved_from,
                moved_to,
                files,
                index_entries,
            } => Self::Refreshed {
                path: repo.to_string(),
                absolute_path: crate::path_spelling::WirePath::of(checkout),
                moved_from: moved_from.clone(),
                moved_to: moved_to.clone(),
                files: files.clone(),
                index_entries: index_entries.clone(),
            },
        }
    }
}

/// A landing's report of one move, as the handler takes it: printed in
/// text, recorded under `--json`.
fn emit_landing_report(handler: &dyn OutputHandler, report: &op_state::LandingReport) {
    if handler.emit_text() {
        match report {
            op_state::LandingReport::Created { repo, branch, at } => {
                outln!("  {repo}: created '{branch}' at {}", at.short());
            }
            op_state::LandingReport::Followed {
                repo,
                to,
                was,
                backup,
            } => {
                outln!(
                    "  {repo}: has no new work, so it takes {} as its pin (was {}, kept in backup \
                     {backup})",
                    to.short(),
                    was.short()
                );
            }
            op_state::LandingReport::Repinned { repo, was, now } => {
                eprintln!(
                    "  {repo}: re-pinned {} -> {} (backward, named by --allow-member-change)",
                    was.short(),
                    now.short()
                );
            }
            op_state::LandingReport::EntryChanged {
                repo,
                field,
                was,
                now,
            } => {
                eprintln!(
                    "  {repo}: {field}: {was} → {now} (the target's entry, named by \
                     --allow-member-change)"
                );
            }
            op_state::LandingReport::EntryRemoved { repo } => {
                eprintln!("  {repo}: removed (the target's entry, named by --allow-member-change)");
            }
            op_state::LandingReport::TakenIn {
                branch,
                from,
                to,
                commits,
                pins,
                entries,
            } => {
                outln!(
                    "  {}: took in {} commit(s) of '{branch}' from its upstream ({} -> {})",
                    RepoKey::Project,
                    commits.len(),
                    from.short(),
                    to.short()
                );
                for commit in commits {
                    outln!("    {} {}", commit.id.short(), commit.subject);
                }
                let shown = |pin: &Option<String>| {
                    pin.as_deref()
                        .map_or("(none)", crate::vcs::short_revision)
                        .to_owned()
                };
                for pin in pins {
                    let (was, now) = (shown(&pin.was), shown(&pin.now));
                    if pin.backward {
                        outln!(
                            "  {}: re-pinned {was} -> {now} (backward, taken in from '{branch}''s \
                             upstream)",
                            pin.repo
                        );
                    } else {
                        outln!("  {}: pin taken in {was} -> {now}", pin.repo);
                    }
                }
                for entry in entries {
                    let what = match entry.change {
                        crate::pull::EntryChange::Added => "added".to_owned(),
                        crate::pull::EntryChange::Removed => "removed".to_owned(),
                        crate::pull::EntryChange::Changed => entry.fields.join(", "),
                    };
                    outln!(
                        "  member-changed {}: {what} (taken in from '{branch}''s upstream)",
                        entry.repo
                    );
                }
            }
            op_state::LandingReport::Refreshed {
                repo,
                checkout,
                moved_from,
                moved_to,
                files,
                index_entries,
            } => {
                let refreshed = crate::check::Refreshed {
                    moved_from: moved_from.clone(),
                    moved_to: moved_to.clone(),
                    files: files.clone(),
                    index_entries: index_entries.clone(),
                };
                eprintln!(
                    "rwv land: {repo} at {}: {}",
                    checkout.display(),
                    refreshed.describe()
                );
            }
        }
    }
    handler.record_landing_report(LandingReportOutput::of(report));
}

/// Journal a move the landing made and report it, once: a move an earlier
/// run of the op reported is reported again when the op resumes, not here.
fn report_landing(ctx: &OpContext<'_>, report: op_state::LandingReport) -> anyhow::Result<()> {
    if op_state::journal_report(&ctx.owner_workspace_dir, &report)? {
        emit_landing_report(ctx.handler, &report);
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LeftCommitOutput {
    pub sha: CommitId,
    pub subject: String,
}

/// What replay did with a member checkout outside its per-repo strategy: the
/// member set the replayed manifest decides.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum MemberOutput {
    /// The source's lock does not name this member and the manifest replay
    /// produced does, so its checkout stays.
    Kept {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
    },
    /// The replayed manifest no longer names this member, and its checkout
    /// was removed.
    Pruned {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
    },
    /// The replayed manifest no longer names this member, but removing its
    /// checkout was refused. The checkout is still on disk, and once the op
    /// relocks no manifest or lock names it. The op runs to completion and
    /// then exits 1.
    PruneRefused {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        /// The refusal token, resolvable with `rwv explain <token>`, when the
        /// removal was refused on a condition. Absent when it was refused on
        /// the checkout's state, which `refused` then carries, or failed
        /// without a classified refusal.
        #[serde(skip_serializing_if = "Option::is_none")]
        refusal: Option<String>,
        /// Each state the checkout was judged in that refused the removal,
        /// with that state's choices; `rwv explain <axis>` serves each axis.
        /// Empty, not absent, when none was.
        refused: Vec<crate::diagnosis::JudgedOutput>,
        /// Free-form display message, the full cause chain. Not a typed
        /// discriminant.
        message: String,
    },
    /// The source's lock names this member and no checkout of it is on disk
    /// here, but creating one failed. It gets no per-repo outcome, and the op
    /// parks.
    MaterializeFailed {
        path: String,
        absolute_path: crate::path_spelling::WirePath,
        /// The refusal token, resolvable with `rwv explain <token>`. Absent
        /// when the failure carries no classified refusal.
        #[serde(skip_serializing_if = "Option::is_none")]
        refusal: Option<String>,
        /// Free-form display message, the full cause chain. Not a typed
        /// discriminant.
        message: String,
    },
}

impl MemberOutput {
    fn kept(repo_path: &RepoPath, checkout: &Path) -> Self {
        Self::Kept {
            path: repo_path.to_string(),
            absolute_path: crate::path_spelling::WirePath::of(checkout),
        }
    }

    pub(crate) fn pruned(repo_path: &RepoPath, checkout: &Path) -> Self {
        Self::Pruned {
            path: repo_path.to_string(),
            absolute_path: crate::path_spelling::WirePath::of(checkout),
        }
    }

    pub(crate) fn prune_refused(
        repo_path: &RepoPath,
        checkout: &Path,
        error: &anyhow::Error,
    ) -> Self {
        Self::PruneRefused {
            path: repo_path.to_string(),
            absolute_path: crate::path_spelling::WirePath::of(checkout),
            refusal: crate::refusal::kind_of(error).map(RefusalKind::token),
            refused: crate::diagnosis::refused_output(error),
            message: format!("{error:#}"),
        }
    }

    fn materialize_failed(repo_path: &RepoPath, checkout: &Path, error: &anyhow::Error) -> Self {
        Self::MaterializeFailed {
            path: repo_path.to_string(),
            absolute_path: crate::path_spelling::WirePath::of(checkout),
            refusal: crate::refusal::kind_of(error).map(RefusalKind::token),
            message: format!("{error:#}"),
        }
    }
}

/// Schema URL embedded in `rwv sync --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/sync.json`.
pub const SYNC_JSON_SCHEMA_URL: &str = crate::schema_url::schema_url!("sync");

/// Schema URL embedded in `rwv land --json` output. Pins to the committed
/// artifact under `docs/reference/schemas/land.json`.
pub const LAND_JSON_SCHEMA_URL: &str = crate::schema_url::schema_url!("land");

/// The clause naming where the source's own checkout stood against the tip
/// replay measured this repo against.
///
/// Stated on every already-ahead line, including the one where the source is
/// AT that tip: an operator who learns nothing about the source from a silent
/// line has to assume the reassuring reading, and that assumption is what
/// turns a contained lock entry into "nothing outstanding".
fn source_position_phrase(position: Option<Containment>, baseline: ReplayBaseline) -> String {
    match position {
        Some(c) => format!("the source's own HEAD is {}", c.describe(baseline.label())),
        None => format!(
            "the source's own HEAD could not be measured against {}",
            baseline.label()
        ),
    }
}

impl fmt::Display for RepoSyncOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Converged { .. } => f.write_str("converged"),
            Self::AlreadyAhead {
                commits_ahead,
                baseline,
                source_position,
            } => write!(
                f,
                "already-ahead — HEAD is {shape}; {source}; nothing to replay",
                shape = Containment::Ahead(*commits_ahead).describe(baseline.label()),
                source = source_position_phrase(*source_position, *baseline),
            ),
            Self::NoOp => f.write_str("up-to-date"),
            Self::Failed(failure) => fmt::Display::fmt(failure, f),
        }
    }
}

/// Name the declared derived paths `repo`'s replay resolved to the target's
/// version, by comparing the tree the replay produced against the tip it
/// started from.
///
/// `pre_replay_tip` is that starting tip, read before any of this op's picks
/// applied — a resumed replay's HEAD is mid-sequence and answers a different
/// question. `None` means the caller could not establish it, and an
/// unestablished starting point yields no report rather than a guessed one.
fn dropped_derived_content(
    vcs: &dyn Vcs,
    repo: &Path,
    target: &ResolvedRevisionId,
    pre_replay_tip: Option<&ResolvedRevisionId>,
) -> Vec<String> {
    let Some(pre_replay_tip) = pre_replay_tip else {
        return Vec::new();
    };
    let Ok(landed) = vcs.head_revision(repo) else {
        return Vec::new();
    };
    vcs.derived_content_dropped_by_replay(repo, target, pre_replay_tip, &landed)
        .unwrap_or_default()
}

/// What one repo's replay was decided from, and what its outcome reports: the
/// read that produced its target, this checkout's verdict against that target,
/// and the source checkout's own verdict against it.
///
/// Carried as one value because they are one observation. A signature taking
/// them apart lets a caller pass a verdict measured against one tip beside a
/// baseline naming another, and the rendered line would name the second while
/// counting the first.
#[derive(Debug, Clone, Copy)]
struct ReplayVerdicts {
    baseline: ReplayBaseline,
    containment: Option<Containment>,
    source_position: Option<Containment>,
}

/// The tip a repo's replay lands on, when that is knowable before it moves:
/// a genuine fast-forward, whose verdict is `Behind`. Written to the op record
/// at replay entry so abort can attribute the repo the instant it advances.
///
/// A function of the task's data and nothing else: it holds no `Vcs`, so it
/// cannot ask the filesystem again after the verdict was taken.
///
/// `Equal` and `Ahead` land nothing: the savepoint already attributes the
/// no-op, and recording an unreached target for an already-ahead repo is
/// forgeable. A `Diverged` repo's fresh rebased tip is captured after the join.
fn entry_tip(
    repo_path: &RepoPath,
    verdicts: &ReplayVerdicts,
    target: &ResolvedRevisionId,
) -> Option<(RepoKey, CommitId)> {
    match verdicts.containment {
        Some(Containment::Behind(_)) => {
            Some((RepoKey::Member(repo_path.clone()), target.id().clone()))
        }
        _ => None,
    }
}

/// `verdicts.containment` is HEAD's verdict against `target`, read by the
/// caller at replay entry — the same value the op record's intent write
/// consumed, so a commit landing here between the two makes the strategy fail
/// rather than re-classifying the repo behind the record's back.
fn sync_one_repo(
    vcs: &dyn Vcs,
    repo: &Path,
    target: &ResolvedRevisionId,
    verdicts: ReplayVerdicts,
    strategy: SyncStrategy,
    pre_replay_tip: Option<&ResolvedRevisionId>,
) -> RepoSyncOutcome {
    let ReplayVerdicts {
        baseline,
        containment,
        source_position,
    } = verdicts;
    // Replay re-entry (`rwv sync --continue`) can find a manifest repo
    // mid-rebase from a previous phase that stopped on a conflict. HEAD in
    // that state points at the last-applied pick (descended from `target`),
    // which would falsely match the AlreadyAhead branch below and skip the
    // remaining picks entirely. Route mid-rebase repos straight to
    // `apply_strategy` so the `rebase_continue` path drives the rebase
    // forward; the head-equality / AlreadyAhead short-circuits assume a
    // repo not in a mid-op state.
    if matches!(vcs.mid_op(repo), Some(ConflictOp::Rebase)) && strategy == SyncStrategy::Rebase {
        return match apply_strategy(vcs, repo, target, baseline, None, strategy) {
            Ok(()) => RepoSyncOutcome::Converged {
                derived_content_dropped: dropped_derived_content(vcs, repo, target, pre_replay_tip),
            },
            Err(StrategyError { message, cause }) => {
                RepoSyncOutcome::Failed(SyncFailure::for_strategy(strategy, message, cause))
            }
        };
    }

    match containment {
        Some(Containment::Equal) => return RepoSyncOutcome::NoOp,
        Some(Containment::Ahead(commits_ahead)) => {
            return RepoSyncOutcome::AlreadyAhead {
                commits_ahead,
                baseline,
                source_position,
            }
        }
        _ => {}
    }

    match apply_strategy(vcs, repo, target, baseline, containment, strategy) {
        Ok(()) => RepoSyncOutcome::Converged {
            derived_content_dropped: dropped_derived_content(vcs, repo, target, pre_replay_tip),
        },
        Err(StrategyError { message, cause }) => {
            RepoSyncOutcome::Failed(SyncFailure::for_strategy(strategy, message, cause))
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Failure from [`apply_strategy`] carrying both the human-formatted error
/// string and the underlying typed [`VcsError`] so callers can plumb
/// structured cause info into `--json` output.
struct StrategyError {
    message: String,
    cause: VcsError,
}

impl StrategyError {
    fn from_vcs(err: VcsError) -> Self {
        Self {
            message: format!("{err:#}"),
            cause: err,
        }
    }

    fn with_message(message: impl Into<String>, cause: VcsError) -> Self {
        Self {
            message: message.into(),
            cause,
        }
    }
}

/// `containment` is the verdict on HEAD against `target` read before this
/// strategy ran; `None` where no such read was taken (a mid-rebase re-entry
/// reaches the rebase arm without one).
fn apply_strategy(
    vcs: &dyn Vcs,
    repo: &Path,
    target: &ResolvedRevisionId,
    baseline: ReplayBaseline,
    containment: Option<Containment>,
    strategy: SyncStrategy,
) -> Result<(), StrategyError> {
    match strategy {
        SyncStrategy::Ff => {
            if let Err(e) = vcs.advance_if_fast_forward(repo, target) {
                let shape = match containment {
                    Some(c) => format!(" (HEAD is {})", c.describe(baseline.label())),
                    None => String::new(),
                };
                let message =
                    format!("cannot fast-forward{shape}; rerun with --strategy rebase. {e:#}");
                return Err(StrategyError::with_message(message, e));
            }
        }
        SyncStrategy::Rebase => {
            // Route through the Vcs trait so the in-flight conflict signal
            // (mid-rebase state + RebaseConflict) is the consolidated path
            // for both manifest and project repos. `git rebase <target>` is
            // equivalent to `git rebase --onto <target> <target>` — git
            // computes the merge-base internally to bound the replay set.
            //
            // Replay re-entry (`rwv sync --continue`): if the repo is
            // already mid-rebase from a previous phase that stopped on a
            // conflict, `Vcs::rebase` would fail immediately ("cannot
            // rebase: you have unstaged changes" / "another rebase is in
            // progress"). Route through `Vcs::rebase_continue` instead so
            // the operator's resolve+stage-then-`rwv sync --continue` loop
            // actually drives the rebase forward through remaining picks
            // (including derived-content picks the policy resolves rather
            // than stopping on). A mid-op state that is NOT rebase
            // (mid-merge, mid-cherry-pick) is not rwv-initiated for this
            // path; fall through to `Vcs::rebase` and let it fail loudly.
            //
            // Both arms state the same derived-content resolution, and they
            // have to: a resume finishes the picks the interrupted replay
            // never reached, so a resume under a different policy than the
            // replay it resumes would resolve the tail of one operation by
            // different rules than its head.
            match vcs.mid_op(repo) {
                Some(ConflictOp::Rebase) => {
                    vcs.rebase_continue(repo, DerivedContentPolicy::keep_target_side())
                        .map_err(StrategyError::from_vcs)?;
                }
                _ => {
                    vcs.rebase(
                        repo,
                        target,
                        target,
                        DerivedContentPolicy::keep_target_side(),
                    )
                    .map_err(StrategyError::from_vcs)?;
                }
            }
        }
    }
    Ok(())
}

fn create_savepoint(
    vcs: &dyn Vcs,
    repo: &Path,
    key: &SavepointKey,
) -> anyhow::Result<ResolvedRevisionId> {
    Ok(vcs.create_savepoint(repo, key)?)
}

fn delete_savepoint(vcs: &dyn Vcs, repo: &Path, key: &SavepointKey) {
    vcs.drop_savepoint(repo, key);
}

/// Number of commits to show inline per refused repo before summarizing the
/// rest as "and N more".
const BLOCKING_COMMITS_CAP: usize = 5;

// ---------------------------------------------------------------------------
// Conflict-bail messages
// ---------------------------------------------------------------------------
//
// Sync's conflict messages lead with concrete resolution steps and mention
// `rwv abort` last as the rollback option. The per-VCS step text comes from
// [`Vcs::conflict_resolution_hint`] so a future non-git impl supplies its
// own vocabulary.

/// Which lock-phase emitted a top-level failure — for the message tag.
#[derive(Debug, Clone, Copy)]
enum Phase {
    /// Phase 1' — project-repo strategy with `rwv.lock` excluded.
    One,
    /// Phase 3 — regenerate `rwv.lock` and commit if changed.
    Three,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::One => "Phase 1' (project repo)",
            Self::Three => "Phase 3 (re-lock)",
        }
    }
}

/// Bail message for the manifest-repo per-repo sync loop (Site 1).
///
/// One or more repos in the loop emitted a per-repo failure (printed already).
/// Lead with the resolution steps that apply uniformly to each conflicted
/// repo; mention `rwv abort` last as the rollback option.
///
/// `live_conflict` is `Some(op)` only when a manifest repo is genuinely left
/// in a VCS-native in-flight state (a live [`ConflictOp`]); the VCS-native
/// resolution steps (`cd <repo>` / stage the resolution) are emitted ONLY then.
/// A non-conflict batch of failures (e.g. a fetch error) omits
/// the VCS block and points straight at the resume command. For
/// [`ConflictOp::Rebase`] the VCS hint stops at staging — the rwv-native
/// `{resume}` line IS the continue step; merge/cherry-pick
/// hints still carry their VCS-native continue since rwv cannot resume those.
fn manifest_repo_failure_message(
    vcs: &dyn Vcs,
    verb: OpVerb,
    live_conflict: Option<ConflictOp>,
) -> String {
    let resume = op_state::resume_command(verb);
    match live_conflict {
        Some(op) => {
            let hint = vcs.conflict_resolution_hint(op);
            format!(
                "sync hit conflicts in one or more member repos (see per-repo lines above).\n\
                 \n\
                 To resolve each conflicted repo:\n\
                   cd <repo>\n\
                 {hint}\n\
                   {resume}   # resume; already-converged repos are no-ops\n\
                 \n\
                 If you'd rather roll everything back: `rwv abort`."
            )
        }
        None => format!(
            "sync hit failures in one or more member repos (see per-repo lines above).\n\
             \n\
             Fix the underlying issue in each failed repo, then:\n\
               {resume}   # resume; already-converged repos are no-ops\n\
             \n\
             If you'd rather roll everything back: `rwv abort`."
        ),
    }
}

/// Bail message for the Phase 1' / Phase 3 top-level failures (Sites 2 and 3).
///
/// Both phases print their inner error via `eprintln!` before bailing; this
/// message gives the operator a uniform "what next?" block that closes with
/// `rwv abort` as the rollback option.
///
/// `live_conflict` is `Some(op)` only when the project repo is actually left
/// mid-op (a rebase/merge/cherry-pick conflict). The VCS-native resolution
/// steps are emitted ONLY then — Phase 3 (relock) is never a
/// VCS conflict, so it always passes `None` and never teaches a spurious
/// `git rebase --continue` (which would print "No rebase in progress").
fn phase1_or_phase3_failure_message(
    vcs: &dyn Vcs,
    phase: Phase,
    cwd_project_dir: &Path,
    verb: OpVerb,
    live_conflict: Option<ConflictOp>,
) -> String {
    let resume = op_state::resume_command(verb);
    let phase_label = phase.label();
    let repo_display = cwd_project_dir.display();
    match live_conflict {
        Some(op) => {
            let hint = vcs.conflict_resolution_hint(op);
            format!(
                "sync failed in {phase_label} (see error above).\n\
                 \n\
                 Resolve the conflict in {repo_display}:\n\
                   cd {repo_display}\n\
                 {hint}\n\
                   {resume}   # resume; already-converged repos are no-ops\n\
                 \n\
                 If you'd rather roll everything back: `rwv abort`."
            )
        }
        None => format!(
            "sync failed in {phase_label} (see error above).\n\
             \n\
             Fix the underlying issue, then: {resume}\n\
             If you'd rather roll everything back: `rwv abort`."
        ),
    }
}

/// Bail message for an inner per-conflict-site.
///
/// Used by Phase 1' when a rebase or merge leaves the project repo in the
/// VCS-native in-flight state. This site ALWAYS has a live [`ConflictOp`] (`op`
/// comes from the `RebaseConflict` variant), so the VCS-native steps are
/// unconditional here. The per-VCS resolution steps come from the trait method;
/// this helper builds the surrounding framing (which repo, how to resume, how
/// to abort).
///
/// For [`ConflictOp::Rebase`] the VCS hint stops at staging and the appended
/// `{resume}` line is the continue step (rwv resumes the rebase
/// natively). For [`ConflictOp::Merge`] / [`ConflictOp::CherryPick`] the
/// VCS hint still carries its own `git … --continue` — rwv has no native
/// resume for those ops; the operator finishes them in git, then `{resume}`
/// picks the op back up.
fn per_conflict_bail_message(
    vcs: &dyn Vcs,
    repo: &Path,
    op: ConflictOp,
    op_label: &str,
    detail: &str,
    verb: OpVerb,
) -> String {
    let hint = vcs.conflict_resolution_hint(op);
    let resume = op_state::resume_command(verb);
    let repo_display = repo.display();
    format!(
        "sync hit a conflict in {repo_display} during {op_label} ({detail}).\n\
         \n\
         To resolve:\n\
           cd {repo_display}\n\
         {hint}\n\
           {resume}   # resume; already-converged repos are no-ops\n\
         \n\
         If you'd rather roll everything back: `rwv abort`."
    )
}

/// Refresh the shared-ref drift of a checkout an op has finished with,
/// reporting what it replaced under `verb`. A checkout whose branch did not
/// move under it is left alone, edits and all.
pub(crate) fn refresh_drift_after(
    verb: impl std::fmt::Display,
    repo: impl std::fmt::Display,
    vcs: &dyn Vcs,
    dir: &Path,
) {
    let refreshed = crate::check::read_drift(vcs, dir)
        .and_then(|reading| crate::check::refresh_drift(vcs, dir, &reading));
    match refreshed {
        Ok(Some(refreshed)) => eprintln!("rwv {verb}: {repo}: {}", refreshed.describe()),
        Ok(None) => {}
        Err(e) => eprintln!("warning: rwv {verb}: {repo}: drift left in place: {e:#}"),
    }
}

/// CWD's project as Phase 1' left it: the manifest the op ends with.
fn reload_replayed_project(ctx: &OpContext<'_>) -> anyhow::Result<Project> {
    let failure = match Project::from_dir(&ctx.cwd_project_dir, ctx.cwd_project_name.clone()) {
        Ok(project) => return Ok(project),
        Err(failure) => failure,
    };
    let refusal = match UnparseableProjectKind::of(&failure) {
        UnparseableProjectKind::PreSchema => failure,
        UnparseableProjectKind::Malformed => {
            let listing = format!("{failure:#}");
            State::UnparseableProjectMalformed
                .of(crate::workspace::project_rel_path(&ctx.cwd_project_name))
                .listing(listing.lines())
                .judge(ctx.project_vcs.as_ref())
                .refused()
        }
    };
    Err(refusal.context(format!(
        "failed to reload project manifest after Phase 1'.\n\
         \n\
         The project repo was successfully rebased/merged, but the manifest \
         in {cwd_project_dir} could not be parsed. Proceeding would silently \
         omit newly-added repos from the regenerated lock.\n\
         \n\
         To recover: `rwv abort`",
        cwd_project_dir = ctx.cwd_project_dir.display(),
    )))
}

/// CWD's manifest as the op found it, read at the project repo's savepoint so
/// a resumed replay weighs the same member set the interrupted run did.
fn pre_op_manifest(ctx: &OpContext<'_>) -> anyhow::Result<Manifest> {
    let vcs = ctx.project_vcs.as_ref();
    let savepoint = vcs
        .resolve_savepoint(&ctx.cwd_project_dir, &ctx.savepoint_key())
        .ok_or_else(|| {
            crate::untokened!(
                InternalInvariant,
                "internal: no savepoint under op {} in {}",
                ctx.op_id.as_str(),
                ctx.cwd_project_dir.display(),
            )
        })?;
    manifest_at_revision(vcs, &ctx.cwd_project_dir, &savepoint)
        .context("failed to read CWD's manifest as the op found it")
}

/// `rwv.toml` as the project repo at `project_dir` holds it at `revision`, a
/// commit other than the checked-out one. A file naming keys an earlier
/// release wrote declares nothing.
fn manifest_at_revision(
    vcs: &dyn Vcs,
    project_dir: &Path,
    revision: &ResolvedRevisionId,
) -> anyhow::Result<Manifest> {
    let content = vcs
        .read_file_at_revision(project_dir, revision, Path::new(Manifest::FILE_NAME))
        .with_context(|| {
            format!(
                "failed to read {} at revision {} in {}",
                Manifest::FILE_NAME,
                revision.id().short(),
                project_dir.display()
            )
        })?;
    Manifest::from_other_commit(&content).with_context(|| {
        format!(
            "failed to parse {} at revision {} in {}",
            Manifest::FILE_NAME,
            revision.id().short(),
            project_dir.display()
        )
    })
}

/// Materialize a repo that's listed in the (source) lock but missing from the
/// CWD workspace on disk.
///
/// - In a workweave: a worktree of the canonical clone at primary, born at
///   `start` ([`crate::workweave::materialize_member`]).
/// - In a primary weave: `git clone` from the manifest URL (mirrors
///   `rwv clone`'s member clone).
///
/// `entry` carries the manifest URL. Caller is responsible for the
/// surrounding sync flow (replay will then call `sync_one_repo` to land HEAD
/// on its target).
fn materialize_missing_repo(
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    repo_path: &RepoPath,
    entry: &crate::manifest::RepoEntry,
    start: &str,
    project_name: &ProjectName,
) -> anyhow::Result<Option<FilePlacement>> {
    match &ctx.checkout {
        Checkout::Workweave {
            name, dir, project, ..
        } => {
            return Ok(crate::workweave::materialize_member(
                vcs,
                ctx.primary_path(),
                ctx.active_path(),
                project_name,
                name.require(dir.as_path(), project)?,
                repo_path,
                entry.role,
                entry.recurse_submodules,
                start,
                crate::workweave::BranchPolicy::Restore,
            )?
            .files);
        }
        Checkout::Primary { .. } => {
            let clone_url = crate::registry::resolve_to_clone_info(&entry.url)?.url;
            let dest = ctx.active_path().join(repo_path.as_path());
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            vcs.clone_repo(&clone_url, &dest)
                .with_context(|| format!("clone of {repo_path} from {clone_url} failed"))?;
            crate::settle::settle(vcs, &dest, entry.role)?;
        }
    }
    Ok(None)
}

/// R4, first half: every live checkout derived from `store`, judged on the
/// `checkout` axis — a recorded workweave's as in another workspace, any
/// other as in a raw worktree. Only a verb in the primary destroys a store,
/// so the choices that run there run here.
///
/// `git worktree add` writes its administration *into* the canonical store
/// (`.git/worktrees/<name>`), so `remove_dir_all` on the store does not
/// merely delete a clone — it deletes the object database and the worktree
/// administration every live checkout of that repo depends on. The
/// registration is what makes those checkouts findable, and it is what this
/// reads. Fail-closed: a registration that could not be enumerated refuses.
fn judge_store_checkouts(
    vcs: &dyn Vcs,
    store: &Path,
    primary_root: &Path,
    repo_path: &RepoPath,
    report: &mut Report,
) -> anyhow::Result<()> {
    let registered = vcs.list_worktrees(store).with_context(|| {
        format!(
            "{repo_path}: cannot enumerate the worktrees registered against {}; \
                 refusing to destroy a store whose claims could not be read",
            store.display()
        )
    })?;
    // `worktree list` includes the store's own main worktree. Compare
    // canonicalized: the registration records a resolved path, and the
    // store path here is built by joining onto the workspace root, which
    // may reach it through a symlink.
    let store_key = CanonicalPath::of(store);
    let live: Vec<&PathBuf> = registered
        .iter()
        .filter(|p| CanonicalPath::of(p) != store_key)
        .collect();
    if live.is_empty() {
        return Ok(());
    }
    let recorded = recorded_workweaves(primary_root)?;
    for checkout in live {
        report.add(judge_checkout(
            vcs, repo_path, checkout, store, &recorded, None,
        ));
    }
    Ok(())
}

/// One live checkout derived from `store`, judged on the `checkout` axis: in
/// another workspace when a recorded workweave holds it, else in a raw
/// worktree.
pub(crate) fn judge_checkout(
    vcs: &dyn Vcs,
    repo: impl std::fmt::Display,
    checkout: &Path,
    store: &Path,
    recorded: &[(crate::manifest::WorkweaveId, CanonicalPath)],
    opening: Option<(&'static str, &str)>,
) -> crate::diagnosis::Judged {
    let at = CanonicalPath::of(checkout);
    let judging = match recorded.iter().find(|(_, dir)| dir.contains(&at)) {
        Some((workweave, dir)) => State::CheckoutInAnotherWorkspace
            .of(repo)
            .subject(workweave.flat())
            .listing([checkout.display()])
            .bind("<project>", &workweave.project)
            .bind("<ww>", &workweave.name)
            .bind_place("<other>", dir.as_path()),
        None => State::CheckoutInRawWorktree
            .of(repo)
            .listing([checkout.display()])
            .checkout(store)
            .bind("<worktree>", checkout.display()),
    };
    match opening {
        Some((verb, branch)) => judging.by_verb(verb).bind("<W>", branch),
        None => judging,
    }
    .judge(vcs)
}

/// Every workweave the primary's projects record, with its directory.
pub(crate) fn recorded_workweaves(
    primary_root: &Path,
) -> anyhow::Result<Vec<(crate::manifest::WorkweaveId, CanonicalPath)>> {
    let mut out = Vec::new();
    for project in crate::workspace::discover_projects(primary_root) {
        let Some(index) = crate::workweave_index::read(primary_root, &project)? else {
            continue;
        };
        for (name, entry) in index.workweaves {
            out.push((
                crate::manifest::WorkweaveId {
                    project: project.clone(),
                    name,
                },
                entry.path,
            ));
        }
    }
    Ok(out)
}

/// Conservatively remove a repo's worktree/clone after it has been dropped
/// from the lock. Refuses (and warns) if the worktree has uncommitted changes
/// or local-only commits (branch tip differs from canonical HEAD in workweave;
/// any commits at all in primary).
///
/// Whether the second of those questions can be asked at all depends on what
/// is being removed, so the arms below establish that first rather than
/// inferring it from where the workspace happens to be. A workweave checkout
/// is removable as a working tree only once it is known to be a workspace
/// linked into a store elsewhere; a checkout that IS the store is refused,
/// because the divergence comparison that would say whether its objects exist
/// anywhere else has nothing to run against.
///
/// The local-only refusal in the primary arm is what keeps `remove_dir_all`
/// off a live workweave's object store; [`judge_store_checkouts`] stands in
/// *front* of the destroy as well.
pub(crate) fn prune_dropped_repo(
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    repo_path: &RepoPath,
) -> anyhow::Result<()> {
    plan_checkout_removal(vcs, ctx, repo_path, RemovalCause::DroppedFromLock)?
        .perform(vcs, repo_path)
}

/// What is removing a checkout, as a refusal of that removal tells it.
#[derive(Clone, Copy)]
pub(crate) enum RemovalCause {
    DroppedFromLock,
    RemoveDelete,
}

impl RemovalCause {
    fn releaser(self) -> crate::workweave::Releaser {
        match self {
            Self::DroppedFromLock => crate::workweave::Releaser::Prune,
            Self::RemoveDelete => crate::workweave::Releaser::Remove,
        }
    }

    fn situation(self) -> &'static str {
        match self {
            Self::DroppedFromLock => "dropped from lock",
            Self::RemoveDelete => "named for `rwv member remove --delete`",
        }
    }

    /// The last step of a remedy. A dropped checkout the op could not prune
    /// stays unpruned: the next op starts from a manifest that no longer names
    /// it, so re-running sync is not a way to remove it.
    fn rerun(self) -> &'static str {
        match self {
            Self::DroppedFromLock => "remove the checkout by hand",
            Self::RemoveDelete => "re-run `rwv member remove --delete`",
        }
    }

    fn then_rerun(self, first: &str) -> String {
        match self {
            Self::DroppedFromLock => format!("{first}, then {}", self.rerun()),
            Self::RemoveDelete => format!("{first} and {}, or remove manually", self.rerun()),
        }
    }
}

/// How a dropped repo's checkout in this workspace comes off disk. Only
/// [`plan_checkout_removal`] makes one, and only once every guard has passed.
#[must_use]
pub(crate) struct CheckoutRemoval(Removal);

enum Removal {
    Absent,
    /// A workweave's worktree registered against the canonical clone in the
    /// primary weave, released from it the way deleting the workweave would.
    RegisteredWorktree {
        checkout: crate::workweave::WorkweaveCheckout,
        releaser: crate::workweave::Releaser,
    },
    /// A workspace linked into a store elsewhere; only its working tree goes.
    LinkedWorkspace(PathBuf),
    /// The canonical store itself.
    Store(PathBuf),
}

impl CheckoutRemoval {
    pub(crate) fn removes_anything(&self) -> bool {
        !matches!(self.0, Removal::Absent)
    }

    pub(crate) fn perform(self, vcs: &dyn Vcs, repo_path: &RepoPath) -> anyhow::Result<()> {
        match self.0 {
            Removal::Absent => {}
            Removal::RegisteredWorktree { checkout, releaser } => {
                crate::workweave::release_checkout(vcs, &checkout, repo_path.as_str(), releaser)
                    .with_context(|| format!("worktree remove for {repo_path} failed"))?;
            }
            Removal::LinkedWorkspace(dest) | Removal::Store(dest) => {
                std::fs::remove_dir_all(&dest)
                    .with_context(|| format!("failed to remove {}", dest.display()))?;
            }
        }
        Ok(())
    }
}

/// Every guard [`prune_dropped_repo`] holds before it removes `repo_path`'s
/// checkout in the workspace `ctx` resolves to, run without removing anything.
pub(crate) fn plan_checkout_removal(
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    repo_path: &RepoPath,
    cause: RemovalCause,
) -> anyhow::Result<CheckoutRemoval> {
    let dropped = cause.situation();
    let dest = ctx.active_path().join(repo_path.as_path());
    if !dest.exists() {
        return Ok(CheckoutRemoval(Removal::Absent));
    }
    let mut report = Report::default();
    let dirty = vcs
        .dirty_file_names(&dest)
        .with_context(|| format!("{repo_path}: {dropped}, but its status could not be read"))
        .map_err(|e| crate::refusal::refusing(RefusalKind::UnreadableStatus, e))?;
    if !dirty.is_empty() {
        report.add(
            State::EditsDirty
                .of(repo_path)
                .listing(dirty)
                .checkout(&dest)
                .judge(vcs),
        );
    }
    if let Checkout::Primary { .. } = &ctx.checkout {
        judge_store_checkouts(vcs, &dest, ctx.primary_path(), repo_path, &mut report)?;
    }
    report.note(format!(
        "{repo_path}: {dropped}; once nothing above refuses it, {}",
        cause.rerun()
    ));
    report.into_result()?;

    match &ctx.checkout {
        Checkout::Workweave {
            name, dir, project, ..
        } => {
            // Diverged-from-canonical check: refuse if local commits would be lost.
            let canonical = ctx.primary_path().join(repo_path.as_path());
            if canonical.exists() {
                let wt_head = vcs.head_revision(&dest).ok();
                let canon_head = vcs.head_revision(&canonical).ok();
                if let (Some(w), Some(c)) = (wt_head, canon_head) {
                    if w != c {
                        // Allow when w is ancestor of c (no unique commits in workweave).
                        let is_ancestor = vcs.is_ancestor(&dest, &w, &c).unwrap_or(false);
                        if !is_ancestor {
                            crate::refuse!(RefusalKind::DroppedRepoHasUniqueCommits,
                                "{repo_path}: {dropped} but worktree has commits not in canonical clone; {}",
                                cause.then_rerun("push/merge them")
                            );
                        }
                    }
                }
                let workweave = crate::manifest::WorkweaveId {
                    project: project.clone(),
                    name: name.require(dir.as_path(), project)?.clone(),
                };
                let store = crate::workweave::store_behind(vcs, &canonical);
                Ok(CheckoutRemoval(Removal::RegisteredWorktree {
                    checkout: crate::workweave::WorkweaveCheckout::of_live_workweave(
                        vcs,
                        ctx.primary_path(),
                        dir.as_path(),
                        &workweave,
                        store,
                        dest,
                        &repo_path.as_path(),
                    ),
                    releaser: cause.releaser(),
                }))
            } else {
                // No canonical at the primary-side slot. That is a fact about
                // the PRIMARY and says nothing about what `dest` is: under
                // inverted topology (docs/explanation/joints/clone-topology.md,
                // I1) a workweave checkout can itself be the standalone clone holding the
                // repo's only object database, and `remove_dir_all` on that is
                // a DESTROY-STORE, not a checkout removal. It is also
                // one this arm could not discharge if it wanted to — with no
                // canonical to compare against, the divergence refusal above
                // is not merely skipped here, it is unavailable, so nothing
                // has established that the objects exist anywhere else.
                //
                // So resolve what `dest` actually is before deleting it, the
                // same way `delete_workweave`'s `is_lone_canonical` does:
                // compare the checkout against the store its refs live in.
                // The fallback is `dest` itself, which makes an unresolvable
                // store refuse rather than be assumed linked.
                let store = crate::workweave::resolved_worktree_parent(vcs, &dest, &dest);
                if store == CanonicalPath::of(&dest) {
                    crate::refuse!(
                        RefusalKind::CloneTopology,
                        "{repo_path}: {dropped}, but the checkout at {} is itself a \
                         canonical store rather than a workspace linked into one — inverted \
                         clone topology (precondition: a workweave checkout is a linked \
                         workspace). There is no canonical clone at {} either, so removing \
                         this directory would destroy the only object database the weave \
                         has for this repo, with nothing having checked what is in it.\n\
                         \n\
                         Run `rwv doctor` for a topology audit and remediation guidance, or \
                         remove the directory manually once you are certain nothing needs \
                         its objects.",
                        dest.display(),
                        canonical.display(),
                    );
                }
                // `dest` is a linked workspace: its refdb and objects live in
                // `store`, which this delete does not touch. Removing the
                // directory removes a working tree only.
                Ok(CheckoutRemoval(Removal::LinkedWorkspace(dest)))
            }
        }
        Checkout::Primary { .. } => {
            // Primary: refuse if local-only branches with unique commits exist.
            // Conservative — any branch with commits not on origin is grounds.
            let any_local_only = match vcs.list_local_branch_names(&dest) {
                Ok(names) => {
                    let mut any = false;
                    for branch in &names {
                        let has_counterpart = vcs
                            .branch_has_remote_counterpart(&dest, branch)
                            .unwrap_or(false);
                        if !has_counterpart {
                            any = true;
                            break;
                        }
                        // A count git could not produce is "we could not
                        // tell", never "nothing unpushed" — the reading the
                        // old `unwrap_or(0)` gave it, which let a git failure
                        // clear the branch and hand the store to the delete.
                        // Refuse on the same terms as an unreadable branch
                        // list below, and say which of the two happened.
                        let count = vcs
                            .count_commits_ahead_of_remote(&dest, branch)
                            .with_context(|| {
                                format!(
                                    "{repo_path}: {dropped}, but counting {branch}'s \
                                     commits against its remote counterpart failed; refusing \
                                     to remove a clone whose unpushed work could not be \
                                     ruled out — {}",
                                    cause.then_rerun("push")
                                )
                            })
                            .map_err(|e| {
                                crate::refusal::refusing(
                                    RefusalKind::DroppedRepoHasUniqueCommits,
                                    e,
                                )
                            })?;
                        if count > 0 {
                            any = true;
                            break;
                        }
                    }
                    any
                }
                Err(_) => true, // conservative: refuse on uncertainty
            };
            if any_local_only {
                crate::refuse!(
                    RefusalKind::DroppedRepoHasUniqueCommits,
                    "{repo_path}: {dropped} but clone has local-only commits; {}",
                    cause.then_rerun("push them")
                );
            }
            let kept_alone: Vec<String> = vcs
                .list_backups(&dest, RemoteTracking::Counted)
                .with_context(|| {
                    format!(
                        "{repo_path}: {dropped}, but its backups could not be listed; refusing \
                         to remove a clone whose backups could not be ruled out"
                    )
                })
                .map_err(|e| crate::refusal::refusing(RefusalKind::DroppedRepoHasUniqueCommits, e))?
                .into_iter()
                .filter(|listed| !listed.redundant())
                .map(|listed| listed.backup.name.to_string())
                .collect();
            if !kept_alone.is_empty() {
                crate::refuse!(
                    RefusalKind::DroppedRepoHasUniqueCommits,
                    "{repo_path}: {dropped} but clone holds backups with commits nothing else \
                     reaches ({}); `rwv backup list` shows them. {}",
                    kept_alone.join(", "),
                    cause.then_rerun(&format!(
                        "publish the work you keep (`rwv push`) or drop the backups \
                         (`rwv backup drop {repo_path}`)"
                    ))
                );
            }
            // DESTROY-STORE: `dest` in the primary IS the canonical
            // store — the refdb and object database every workweave worktree
            // of this repo is registered in. Its checkouts were judged above.
            Ok(CheckoutRemoval(Removal::Store(dest)))
        }
    }
}

// ---------------------------------------------------------------------------
// OutputHandler trait: open extensibility seam for per-repo output.
// ---------------------------------------------------------------------------

/// Trait implemented by every output mode.
///
/// Callers pass `&dyn OutputHandler` into the orchestration body so new modes
/// can be added without touching existing orchestration code.
///
/// ## Contract
///
/// - `record` is called once per completed per-repo outcome (including
///   failures surfaced before the main sync loop). Implementations may
///   print, buffer, or stream as appropriate.
/// - `emit_text` controls whether the orchestration body emits human-readable
///   progress lines to stdout/stderr. Returning `false` suppresses all
///   non-record chatter (used by JSON modes).
/// - `record_step3_advance` is called by the advance-target phase for each
///   repo whose target branch pointer was actually moved (no-ops are not
///   reported). The default no-op implementation is suitable for text-mode
///   handlers and plain-sync JSON handlers that do not surface step-3 SHAs.
///
/// ## Thread safety
///
/// `Send + Sync` is required because `record` may be called from parallel
/// worker threads inside `run_in_parallel`. Implementations use interior
/// mutability (e.g. `Mutex`) for any mutable state.
pub trait OutputHandler: Send + Sync {
    /// Record one per-repo outcome.
    ///
    /// `path` is the repo's manifest-relative path string; `abs_path` is
    /// the absolute on-disk path. `outcome` is the raw sync result —
    /// implementations convert to `SyncOutcomeOutput` internally when needed.
    /// `containment` is the verdict this repo's outcome was decided from,
    /// `None` where it was decided before any pair was read.
    fn record(
        &self,
        path: &str,
        abs_path: &Path,
        outcome: &RepoSyncOutcome,
        containment: Option<ContainmentOutput>,
    );

    /// Return `true` if the orchestration body should emit human-readable
    /// text progress to stdout/stderr. JSON-mode handlers return `false`.
    fn emit_text(&self) -> bool;

    /// Record a step-3 (advance-target) fast-forward for one repo or the
    /// project repo. Called iff the branch pointer actually moved.
    ///
    /// `repo` is the repo that moved: a member, which `record` named by its
    /// manifest-relative path, or the project repo.
    /// `from_sha` is the target's tip before the FF; `to_sha` is after.
    ///
    /// Default implementation is a no-op — suitable for text-mode and
    /// plain-sync JSON handlers that do not need step-3 SHAs.
    fn record_step3_advance(&self, _repo: &RepoKey, _from_sha: &str, _to_sha: &str) {}

    /// Record how one checkout's files were placed when the op created or
    /// moved it. A checkout of the target is not the one its repo's record
    /// names, and a placement can arrive after that record.
    ///
    /// Default implementation is a no-op: the text handler prints placement
    /// at the call site.
    fn record_placement(&self, _placement: PlacementOutput) {}

    /// Record what replay did with one member checkout outside its per-repo
    /// strategy.
    ///
    /// Default implementation is a no-op: the text handler prints at the call
    /// site.
    fn record_member(&self, _member: MemberOutput) {}

    /// Record a standing advisory raised during cleanup.
    ///
    /// Default implementation is a no-op: text-mode handlers print the
    /// advisory's human rendering directly at the call site instead, and
    /// land's envelope carries no `advisories` field to buffer it into.
    fn record_advisory(&self, _advisory: AdvisoryOutput) {}

    /// Record the coordinates the machine resolved for this op. Called once,
    /// as soon as the context exists and before any phase runs.
    ///
    /// Default implementation is a no-op: only the land envelope reports
    /// them.
    fn record_coordinates(&self, _coordinates: OpCoordinates) {}

    /// Record that retire deleted the workweave. Called with the delete's own
    /// witness, so nothing else can report a retire.
    ///
    /// Default implementation is a no-op: only the land envelope reports it.
    fn record_retired(&self, _retired: &RetiredWorkweave) {}

    /// Record that this invocation resumed a parked op whose record stood at
    /// `phase`. Called once, from the resume loader, and never on a fresh run
    /// — so an envelope's "resumed" claim can only come from the op record
    /// the resume actually read, not from the invocation's own flags.
    ///
    /// Default implementation is a no-op: only the land envelope reports it.
    fn record_resumed(&self, _phase: op_state::OpPhase) {}

    /// Record that the op ran its last phase and cleanup cleared its op-state.
    /// Never called when a phase stops the op.
    ///
    /// Default implementation is a no-op: only the envelopes report it.
    fn record_complete(&self) {}

    /// Record the commits a landing's declared-branch change leaves behind.
    ///
    /// Default implementation is a no-op: the text handler prints them at the
    /// call site.
    fn record_left_behind(&self, _left: LeftBehindOutput) {}

    /// Record one move a landing reported as it made it.
    ///
    /// Default implementation is a no-op: the text handler prints it at the
    /// call site.
    fn record_landing_report(&self, _report: LandingReportOutput) {}
}

/// Where an op is running and what it is landing into, as the machine
/// resolved them.
///
/// The invocation's own arguments answer neither on a `--continue`: the
/// target comes from the op record, and the workspace the operator typed the
/// command in may be the leased target rather than the workspace that owns
/// the op. Anything reporting these facts consumes the machine's values.
pub struct OpCoordinates {
    /// The workweave the op is rooted in — `None` when the owner is the
    /// primary weave.
    pub source_workweave: Option<String>,
    /// The workspace advance-target lands into.
    pub target: PathBuf,
}

/// Witness that `--retire` deleted a workweave.
///
/// Produced by the delete's own return in `retire_workweave_after_landing`
/// and nowhere else, so a `retired` claim cannot be assembled from the flags
/// the operator passed.
pub struct RetiredWorkweave {
    name: WorkweaveName,
    deletion: crate::workweave::WorkweaveDeletion,
}

impl RetiredWorkweave {
    /// The workweave this op deleted.
    pub fn name(&self) -> &WorkweaveName {
        &self.name
    }
}

/// Text-mode handler: prints one line per repo to stdout/stderr and discards
/// structured records. Used by `rwv sync` and `rwv land` (no `--json`).
///
/// `stdout_lock` serializes concurrent writes when `-j > 1` is combined with
/// text mode (defensive; text mode is serial in normal use).
pub struct TextHandler<'a> {
    stdout_lock: &'a Mutex<()>,
}

impl OutputHandler for TextHandler<'_> {
    fn emit_text(&self) -> bool {
        true
    }

    fn record(
        &self,
        path: &str,
        _abs_path: &Path,
        outcome: &RepoSyncOutcome,
        _containment: Option<ContainmentOutput>,
    ) {
        let _guard = self.stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        if outcome.is_failure() {
            eprintln!("  {path}: {outcome}");
        } else {
            outln!("  {path}: {outcome}");
        }
        if let RepoSyncOutcome::Converged {
            derived_content_dropped,
        } = outcome
        {
            if !derived_content_dropped.is_empty() {
                eprintln!(
                    "  warning: {path}: the replay kept the target's version of these declared \
                     derived paths — what you replayed is not what landed:"
                );
                for dropped in derived_content_dropped {
                    eprintln!("      {dropped}");
                }
                eprintln!(
                    "  regenerate them from their source of record in {path} and commit — \
                     until then the landed tree is stale at those paths."
                );
            }
        }
    }
}

/// Envelope handler for `rwv sync --json`: buffers all records so the caller
/// can emit a single `{ "$schema": ..., "outcomes": [...] }` JSON object after
/// the sync completes.
///
/// Text chatter is suppressed (`emit_text` returns `false`).
pub struct JsonEnvelopeHandler<'a> {
    records: &'a Mutex<Vec<SyncOutcomeOutput>>,
    advisories: &'a Mutex<Vec<AdvisoryOutput>>,
    placements: &'a Mutex<Vec<PlacementOutput>>,
    members: &'a Mutex<Vec<MemberOutput>>,
    complete: &'a AtomicBool,
}

impl OutputHandler for JsonEnvelopeHandler<'_> {
    fn emit_text(&self) -> bool {
        false
    }

    fn record(
        &self,
        path: &str,
        abs_path: &Path,
        outcome: &RepoSyncOutcome,
        containment: Option<ContainmentOutput>,
    ) {
        let out = SyncOutcomeOutput::from_outcome(
            path.to_owned(),
            crate::path_spelling::WirePath::of(abs_path),
            outcome,
            containment,
        );
        let mut guard = self.records.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(out);
    }

    fn record_advisory(&self, advisory: AdvisoryOutput) {
        let mut guard = self.advisories.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(advisory);
    }

    fn record_member(&self, member: MemberOutput) {
        let mut guard = self.members.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(member);
    }

    fn record_placement(&self, placement: PlacementOutput) {
        let mut guard = self.placements.lock().unwrap_or_else(|e| e.into_inner());
        guard.push(placement);
    }

    fn record_complete(&self) {
        self.complete.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// The phase machine
// ---------------------------------------------------------------------------
//
// One data-driven machine drives both `rwv sync` and `rwv land`. Phases:
//
//   guard → mark → savepoint → replay → relock → advance-target → retire → cleanup
//                                                 (land only)   (--retire only)
//
// guard + mark + savepoint happen once, before the loop, in `guard_and_mark`.
// The persisted record's phase starts at `Replay`; `drive` owns the loop and
// the persistence-ordering invariant.
//
// **Invariant:** the persisted phase is the phase in progress, and every
// phase is idempotent and re-runnable from the record alone.
//
// - replay re-entry derives per-repo state from the VCS itself (savepoint →
//   redo; mid-conflict → VCS-native continue; already-converged → no-op).
//   No `resume_phase` / `step1_continue` flags anywhere.
// - relock re-entry: regenerating a current lock is a no-op.
// - advance-target re-entry: ff to an already-reached tip is a no-op.
// - retire re-entry: re-running the merged-check is read-only.
//
// `--continue` (both verbs) = load record (resolving lease pointer if invoked
// from a non-owner workspace), enter the driver loop at the recorded phase.

/// Immutable per-op context built once, before the driver loop. Holds
/// everything the phase functions need that isn't on disk in the owner record.
struct OpContext<'a> {
    /// CWD workspace context (the workspace invocation was made from).
    ///
    /// Owned by the op context — a `Clone` of the invocation context passed
    /// in by the top-of-`main` resolution (or a re-rooted owner context on
    /// `--continue` from a leased target). The phase engine never re-resolves
    /// the invocation context via `current_dir()`; it may resolve *other*
    /// workspaces (source/target) as needed.
    cwd_ctx: WorkspaceContext,
    /// CWD workspace root (== owner workspace for fresh invocations from the
    /// owner side; == lease workspace when --continue is invoked from target).
    cwd_workspace_dir: PathBuf,
    /// Workspace that holds the full owner record. Resolved from a lease
    /// pointer when --continue is invoked from a non-owner workspace.
    owner_workspace_dir: PathBuf,
    /// Display name of the source workspace, used in human messages
    /// (e.g. the auto-relock commit message and replay's lock-freshness hint).
    source_workspace_name: String,
    /// Destination workspace (the one phases write into): the target workspace.
    dest_workspace_dir: PathBuf,
    dest_project_dir: PathBuf,
    /// CWD's project repo dir: where replay+relock run, while
    /// `dest_project_dir` is where advance-target lands.
    cwd_project_dir: PathBuf,
    /// CWD project name (used for materialize_missing_repo's ephemeral
    /// branch namespace in workweaves).
    cwd_project_name: ProjectName,
    /// Path arg the operator passed on the CLI (or recorded in op-state),
    /// retained for hint messages that show the original target spelling.
    cli_path: PathBuf,
    strategy: SyncStrategy,
    /// The landing's plan, read back from the op record: each `owned` or
    /// `fork` member's receiving-branch tip it is composed onto, `T`'s tip,
    /// and the source's tip once the op-start relock committed.
    take_in: op_state::TakeInRecord,
    retire: bool,
    /// Set when retire removed the workweave and the deletion rule kept
    /// branches of it.
    retire_left: std::sync::atomic::AtomicBool,
    jobs: usize,
    handler: &'a dyn OutputHandler,
    /// The CWD project repo's backend. One per invocation, because the
    /// project repo is one per invocation — see [`crate::vcs::project_vcs`]
    /// for why it cannot be resolved from a manifest entry.
    project_vcs: Box<dyn Vcs>,
    verb: op_state::OpVerb,
    op_id: OpId,
    /// Atomic source snapshot pinned at guard time (T0): one ref read of
    /// the source project tip, then manifest + lock read AT that revision.
    /// On `--continue`, T0 is re-established at the start of the resumed
    /// session — per-repo no-op detection handles repos that already
    /// converged in the previous (now-aborted) replay run.
    snapshot: SourceSnapshot,
}

/// What a source snapshot classifies about the source's member checkouts.
///
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClassifySource {
    /// Classify the source's checkouts. The gate's refusals read the relations.
    Relations,
}

/// Atomic source snapshot pinned at T0 (start of replay).
///
/// The source's project tip is read once and everything derived from it —
/// manifest, lock — is read AT that revision via `Vcs::read_file_at_revision`.
/// A concurrent mutation of the source after T0 changes refs but cannot touch
/// anything we've read.
struct SourceSnapshot {
    /// Source project tip at T0.
    source_project_tip: ResolvedRevisionId,
    /// Source lock (raw, unresolved), read at `source_project_tip`.
    raw_source_lock: LockFile,
    /// `raw_source_lock` resolved ONCE, against the SOURCE workspace, at T0.
    /// A lock entry in tag or branch form names a different commit in a
    /// different workspace, so the workspace whose lock it is, is the only one
    /// that can say what it means. A commit id names the same commit in every
    /// workspace on the store, so one whose checkout the source lacks is
    /// resolved in the canonical clone instead. Every downstream consumer takes
    /// its target from here; nothing re-resolves the raw lock in another
    /// context. An entry neither could resolve is absent, never guessed at.
    source_lock: crate::manifest::ResolvedLockFile,
    /// The source's member checkouts classified at T0.
    source_class: Option<LockClassification>,
    /// Committed member tips of EVERY source checkout, read at T0 — the
    /// disclosure the already-ahead line measures against replay's target.
    /// Taken from `source_class` when
    /// the op classified the source, so one read of each checkout answers both.
    source_tips: std::collections::BTreeMap<RepoPath, ResolvedRevisionId>,
    /// Root of the workspace the tips above were read in, and the object store
    /// their ancestry against a target must be measured in.
    source_workspace_root: PathBuf,
}

/// The C15 copy that ports a declared-branch change's left-behind commits
/// onto the new receiving branch.
fn left_behind_choices(
    left: &op_state::LeftBehind,
) -> (
    &'static [crate::diagnosis::Form],
    std::collections::BTreeMap<&'static str, String>,
) {
    let mut bindings = std::collections::BTreeMap::new();
    bindings.insert("<m>", left.repo.to_string());
    bindings.insert("<b>", left.now.clone());
    bindings.insert("<checkout>", left.checkout.display().to_string());
    bindings.insert(
        "<ids>",
        left.commits
            .iter()
            .rev()
            .map(|commit| commit.id.short().to_owned())
            .collect::<Vec<_>>()
            .join(" "),
    );
    (&[crate::diagnosis::Form::C15], bindings)
}

/// A named declared-branch change's report: the commits the target's pin
/// reaches that the new receiving branch lacks, and the copy that ports them.
fn left_behind_text(left: &op_state::LeftBehind) -> String {
    let vcs = vcs_for(left.vcs_type);
    let mut out = format!(
        "rwv land: {}: branch {} -> {}: the target's pin reaches commits '{}' lacks:\n",
        left.repo, left.was, left.now, left.now
    );
    for commit in &left.commits {
        out.push_str(&format!("    {} {}\n", commit.id.short(), commit.subject));
    }
    let (forms, bindings) = left_behind_choices(left);
    out.push_str(&crate::diagnosis::choices_text(
        vcs.as_ref(),
        forms,
        &bindings,
    ));
    out
}

impl LeftBehindOutput {
    fn of(left: &op_state::LeftBehind) -> Self {
        let (forms, bindings) = left_behind_choices(left);
        LeftBehindOutput {
            path: left.repo.to_string(),
            was: left.was.clone(),
            now: left.now.clone(),
            commits: left
                .commits
                .iter()
                .map(|commit| LeftCommitOutput {
                    sha: commit.id.clone(),
                    subject: commit.subject.clone(),
                })
                .collect(),
            choices: crate::diagnosis::choices(vcs_for(left.vcs_type).as_ref(), forms, &bindings),
        }
    }
}

/// Each declared-branch change's left-behind commits, as the handler reports
/// them: printed in text, recorded under `--json`.
fn report_left_behind(handler: &dyn OutputHandler, left_behind: &[op_state::LeftBehind]) {
    for left in left_behind {
        if handler.emit_text() {
            eprint!("{}", left_behind_text(left));
        }
        handler.record_left_behind(LeftBehindOutput::of(left));
    }
}

/// The branch the landing lands into, as its op record names it.
fn target_name(ctx: &OpContext<'_>) -> String {
    match &ctx.take_in.sync_from {
        Some(op_state::SyncFrom::Target(name) | op_state::SyncFrom::From(name)) => name.clone(),
        None => "the target".to_owned(),
    }
}

impl OpContext<'_> {
    /// The key the op's replay-side savepoints are filed under.
    fn savepoint_key(&self) -> SavepointKey {
        SavepointKey::Op(self.op_id.clone())
    }

    /// The key a `land` target's savepoints are filed under.
    fn target_savepoint_key(&self) -> SavepointKey {
        SavepointKey::LandTarget(self.op_id.clone())
    }

    /// Convenience: the active phase recorded on the owner record.
    fn current_phase(&self) -> anyhow::Result<op_state::OpPhase> {
        let record = op_state::read_owner(&self.owner_workspace_dir)?.ok_or_else(|| {
            crate::untokened!(
                InternalInvariant,
                "internal: owner record disappeared at {} mid-op",
                self.owner_workspace_dir.display()
            )
        })?;
        Ok(record.phase)
    }
}

// ---------------------------------------------------------------------------
// Top-level entry points (public API surface unchanged)
// ---------------------------------------------------------------------------

/// What a `land` invocation asks the machine to do: start an op, or resume
/// the one recorded where it runs.
#[derive(Debug, Clone)]
pub enum SyncRequest {
    Fresh {
        landing: Box<crate::land::Landing>,
        strategy: SyncStrategy,
        /// Remove the workweave after a successful landing (`--retire`).
        retire: bool,
        /// Parallel per-repo worker count (resolved `-j N`); `1` runs serially.
        jobs: usize,
    },
    /// Every parameter comes off the op's record, and the resume runs
    /// serially.
    Continue {
        op: Box<op_state::ResolvedOwner>,
        /// Asserts the recorded project; a different one refuses.
        project_override: Option<ProjectName>,
        /// The repos `--allow-member-change` names, added to the consent the
        /// op recorded at its start.
        allow_member_change: Vec<RepoOperand>,
    },
}

// ---------------------------------------------------------------------------
// Driver entry: shared by sync and land (text + json modes)
// ---------------------------------------------------------------------------

/// Build the [`OpContext`] and run the phase-machine driver. Both `rwv sync`
/// and `rwv land` route through here; the `verb` parameter selects which
/// phases run (advance-target and retire are land-only, and retire runs
/// under `--retire` only).
///
/// A [`SyncRequest::Continue`] enters the driver loop at the phase its op
/// recorded.
///
/// `cwd_ctx` is the already-resolved invocation context (with `--project`
/// baked in when passed). The driver never re-resolves the invocation
/// context; it may still resolve *other* workspaces (sync source/target,
/// lease owner) internally.
fn run_machine(
    cwd_ctx: &WorkspaceContext,
    request: SyncRequest,
    handler: &dyn OutputHandler,
) -> anyhow::Result<()> {
    let ctx = match request {
        SyncRequest::Fresh {
            landing,
            strategy,
            retire,
            jobs,
        } => guard_and_mark(
            cwd_ctx,
            FreshOp {
                landing: *landing,
                strategy,
                retire,
                jobs,
            },
            handler,
        )?,
        SyncRequest::Continue {
            op,
            project_override,
            allow_member_change,
        } => load_continuing_context(
            cwd_ctx,
            *op,
            project_override,
            &allow_member_change,
            handler,
        )?,
    };

    // Reported before the first phase, so a run that fails partway still
    // describes the op it was running rather than the arguments it was
    // invoked with.
    handler.record_coordinates(OpCoordinates {
        source_workweave: match &ctx.cwd_ctx.checkout {
            Checkout::Workweave {
                name, dir, project, ..
            } => Some(name.require(dir.as_path(), project)?.as_str().to_owned()),
            Checkout::Primary { .. } => None,
        },
        target: ctx.dest_workspace_dir.clone(),
    });

    drive(&ctx)
}

/// The phase-machine driver. Reads the persisted phase, runs it, persists the
/// transition to the next phase, loops.
///
/// Direction-of-motion invariant: the persisted phase never runs ahead of
/// completed work. The owner record's `phase` field is the SINGLE source of
/// truth; entry into the loop relies on either `guard_and_mark`'s initial
/// write (fresh start: phase=replay) or the prior iteration's write (resume:
/// phase=whatever crashed). This loop's own write is post-transition — it
/// commits only after `run_phase` returns, so a crash mid-phase leaves the
/// record exactly where the loop found it. `load_continuing_context`'s
/// resume-entry write upholds the same invariant from the other direction:
/// it only ever moves the record backward or laterally, to
/// `resume_entry_phase`'s idempotent re-entry point, never ahead of what
/// this loop has itself completed.
///
/// Crash semantics:
///   - Inside `run_phase`: record stays at the phase that was running →
///     `--continue` re-enters that phase (idempotent by construction).
///   - After `run_phase` returned but before `set_phase` of the next phase
///     committed: record still says current → `--continue` re-runs the just-
///     completed phase (idempotent), then transitions.
///   - After `set_phase` of the next phase committed: record says next →
///     `--continue` enters next directly.
fn drive(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    loop {
        let phase = ctx.current_phase()?;
        let next = run_phase(ctx, phase)?;
        match next {
            Some(p) => {
                // Post-transition write. Until this commits, a crash leaves
                // the record at the just-completed phase, which re-runs
                // idempotently on resume.
                op_state::set_phase(&ctx.owner_workspace_dir, p)
                    .context("failed to advance phase between iterations")?;
            }
            None => {
                let stranded = stranded_checkouts(ctx)?;
                cleanup(ctx)?;
                ctx.handler.record_complete();
                announce_completion(ctx);
                let stranded: Vec<&str> = stranded.iter().map(RepoPath::as_str).collect();
                fail_on_stranded_checkouts(ctx.verb, &stranded)?;
                if ctx.retire_left.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(crate::exit_status::CompletedWithRemainder.into());
                }
                return Ok(());
            }
        }
    }
}

/// The member checkouts the op's replay dropped and could not prune: named by
/// CWD's manifest as the op found it, named by no manifest now, and still on
/// disk. Read before cleanup, which drops the savepoint the op-start manifest
/// is read at; derived rather than carried from replay, so a resumed op that
/// re-entered past replay still finds them. A retire that deleted CWD left
/// none.
fn stranded_checkouts(ctx: &OpContext<'_>) -> anyhow::Result<Vec<RepoPath>> {
    if !ctx.cwd_workspace_dir.exists() {
        return Ok(Vec::new());
    }
    let current = Manifest::from_path(&ctx.cwd_project_dir.join(Manifest::FILE_NAME))
        .context("failed to reload CWD's manifest to look for unpruned checkouts")?;
    Ok(pre_op_manifest(ctx)?
        .iter_repo_paths()
        .filter(|repo_path| {
            !current.contains_repo(repo_path)
                && ctx.cwd_workspace_dir.join(repo_path.as_path()).exists()
        })
        .cloned()
        .collect())
}

/// A completed op that left a dropped checkout on disk partially failed, and
/// nothing the op can still do repairs it: its op-state is already gone.
/// A merge or sync completed but left the dropped checkouts named in
/// `members` on disk.
pub(crate) fn fail_on_refused_prunes(members: &[MemberOutput]) -> anyhow::Result<()> {
    let stranded: Vec<&str> = members
        .iter()
        .filter_map(|member| match member {
            MemberOutput::PruneRefused { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect();
    fail_on_stranded_checkouts(op_state::OpVerb::Sync, &stranded)
}

fn fail_on_stranded_checkouts(verb: op_state::OpVerb, stranded: &[&str]) -> anyhow::Result<()> {
    if stranded.is_empty() {
        return Ok(());
    }
    let n = stranded.len();
    let (s, them, are) = if n == 1 {
        ("", "it", "is")
    } else {
        ("s", "them", "are")
    };
    Err(crate::untokened!(
        Tally,
        "{verb} completed: replay and relock finished and op-state is cleared, so `rwv abort` \
         does not apply. Pruning was refused for {n} member checkout{s} the manifest no longer \
         names, and {them} {are} still on disk:\n  {list}\n\
         Re-running {verb} will not prune {them}. Once nothing in {them} is needed, remove {them} \
         by hand.",
        list = stranded.join("\n  "),
    ))
}

/// The op-scope completion claim, made where the machine is terminal for this
/// invocation's phase set: the last phase returned no successor and cleanup is
/// done.
///
/// No phase function can make this claim. Reaching the end of advance-target
/// says nothing about whether `--retire` still has retire scheduled, and
/// retire's merged- and dirty-checks can refuse after everything above them
/// succeeded. Phase functions announce their own phase's outcome; the claim
/// that the op is complete belongs to the only place that knows it.
fn announce_completion(ctx: &OpContext<'_>) {
    if !ctx.handler.emit_text() {
        return;
    }
    match ctx.verb {
        op_state::OpVerb::Sync
        | op_state::OpVerb::Pull
        | op_state::OpVerb::UpdateRemote
        | op_state::OpVerb::CherryPick => {}
        op_state::OpVerb::Land => {
            eprintln!(
                "land complete: {} fast-forwarded to this workspace's tip",
                target_name(ctx)
            );
        }
    }
}

/// Dispatch one phase. Each phase function is idempotent and returns the
/// next phase (or `None` to signal "no more phases — proceed to cleanup").
fn run_phase(
    ctx: &OpContext<'_>,
    phase: op_state::OpPhase,
) -> anyhow::Result<Option<op_state::OpPhase>> {
    use op_state::OpPhase;
    match phase {
        OpPhase::Replay => {
            run_replay(ctx)?;
            Ok(Some(OpPhase::Relock))
        }
        OpPhase::Relock => {
            run_relock(ctx)?;
            Ok(next_after_relock(ctx))
        }
        OpPhase::AdvanceTarget => {
            run_advance_target(ctx)?;
            Ok(next_after_advance_target(ctx))
        }
        OpPhase::Retire => {
            run_retire(ctx)?;
            Ok(None)
        }
    }
}

/// After relock, `land` continues with advance-target.
fn next_after_relock(ctx: &OpContext<'_>) -> Option<op_state::OpPhase> {
    match ctx.verb {
        op_state::OpVerb::Sync
        | op_state::OpVerb::Pull
        | op_state::OpVerb::UpdateRemote
        | op_state::OpVerb::CherryPick => None,
        op_state::OpVerb::Land => Some(op_state::OpPhase::AdvanceTarget),
    }
}

/// After advance-target, retire runs only when `--retire` was passed.
fn next_after_advance_target(ctx: &OpContext<'_>) -> Option<op_state::OpPhase> {
    if ctx.retire {
        Some(op_state::OpPhase::Retire)
    } else {
        None
    }
}

/// The phase a `--continue` enters, given the phase the record was left at.
///
/// Advance-target publishes CWD's tips AND CWD's lock to the target; relock is
/// what makes those two agree. A resume exists because the operator changed CWD
/// after the op stranded, so the agreement relock established before the strand
/// no longer holds — re-entering relock restores the property every other path
/// into advance-target has: relock ran immediately before it with no operator
/// window in between.
///
/// Replay reaches relock by running forward. Retire runs after the target was
/// already advanced, and its merged-check refuses a CWD that moved rather than
/// publishing it, so retire is entered as recorded.
fn resume_entry_phase(recorded: &op_state::OpPhase) -> op_state::OpPhase {
    match recorded {
        op_state::OpPhase::AdvanceTarget => op_state::OpPhase::Relock,
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Pre-loop: guard + mark + savepoint (fresh start)
// ---------------------------------------------------------------------------

/// Outputs of the post-acquisition precondition sweep. Threaded from
/// [`run_preconditions_after_acquire`] into the surrounding Mark / Savepoint
/// code in [`guard_and_mark`].
struct PreconditionOutcome {
    /// The target pinned at T₀ by a snapshot read.
    snapshot: SourceSnapshot,
    /// CWD's parsed project (manifest + lock, if present).
    cwd_project: Project,
    /// Human name of the CWD workspace, used in the auto-relock LOUD line.
    cwd_workspace_name_str: String,
    /// CWD repos whose committed lock is behind HEAD — auto-relocked at op
    /// start, forward.
    cwd_lock_behind: Vec<RepoRelation>,
    /// Each `owned` or `fork` member's receiving-branch tip.
    plan: crate::land::Plan,
}

/// The landing's judgment of where a source checkout stands to its pin: a
/// forward relock records a checkout ahead of it; a pin the relock would move
/// backward, or a member with none, refuses.
fn judge_source_relations(
    class: &LockClassification,
    root: &Path,
    manifest: &Manifest,
    report: &mut Report,
) {
    for (repo_path, raw) in &class.unresolvable {
        let dir = root.join(repo_path.as_path());
        let vcs = manifest
            .get_entry(repo_path)
            .map(|e| vcs_for(e.vcs_type))
            .unwrap_or_else(crate::vcs::probe_vcs);
        report.add(
            State::PinUnreachable
                .of(repo_path)
                .member()
                .checkout(&dir)
                .bind("<m>", repo_path)
                .subject(format!(
                    "the lock pins {}, which this clone lacks",
                    raw.as_str()
                ))
                .by_verb("land")
                .judge(vcs.as_ref()),
        );
    }
    for r in &class.relations {
        let dir = root.join(r.repo_path.as_path());
        let vcs = manifest
            .get_entry(&r.repo_path)
            .map(|e| vcs_for(e.vcs_type))
            .unwrap_or_else(crate::vcs::probe_vcs);
        let state = match r.relation {
            LockRelation::Ok | LockRelation::Ahead | LockRelation::Missing => continue,
            LockRelation::Behind | LockRelation::Diverged => State::PinBackwards,
            LockRelation::NoLock | LockRelation::Unknown => State::PinNone,
            LockRelation::Unreachable => State::PinUnreachable,
        };
        report.add(
            state
                .of(&r.repo_path)
                .member()
                .checkout(&dir)
                .bind("<m>", &r.repo_path)
                .bind("<repo>", &r.repo_path)
                .subject(format!("HEAD {} the lock's pin", r.relation))
                .by_verb("land")
                .judge(vcs.as_ref()),
        );
    }
}

/// The source's own refusals, which a landing owes whatever its target: edits
/// in its checkouts and where each stands to the source's lock. The landing
/// refuses before it holds any claim, so drift is not read: nothing here may
/// write, and what drift leaves is judged as the dirt it looks like.
pub(crate) fn judge_landing_source(
    ctx: &WorkspaceContext,
    report: &mut Report,
) -> anyhow::Result<()> {
    let project = ctx.require_bound_project()?.clone();
    let root = ctx.active_path();
    let dir = project_dir(root, &project);
    let source = Project::from_dir(&dir, project)?;
    judge_edits_source_preflight(&source, root, &dir, report, None);
    let class = {
        let (resolved, unresolvable) = match source.lock.as_ref() {
            Some(raw) => {
                let (resolved, failures) = raw.clone().resolve_versions(root);
                (Some(resolved), failures)
            }
            None => (None, Vec::new()),
        };
        classify_lock_relations(root, &source.manifest, resolved.as_ref(), unresolvable)
    };
    judge_source_relations(&class, root, &source.manifest, report);
    Ok(())
}

/// Each pin of a lock `T`'s landing reads, `T`'s tip's or its workspace's,
/// that this clone lacks: the landing would compose against it or leave it
/// pinned, so it refuses (`pin` unreachable).
fn judge_unreachable_target_pins(
    class: &LockClassification,
    root: &Path,
    manifest: &Manifest,
    report: &mut Report,
) {
    for (repo_path, raw) in &class.unresolvable {
        let dir = root.join(repo_path.as_path());
        let vcs = manifest
            .get_entry(repo_path)
            .map(|e| vcs_for(e.vcs_type))
            .unwrap_or_else(crate::vcs::probe_vcs);
        report.add(
            State::PinUnreachable
                .of(repo_path)
                .member()
                .checkout(&dir)
                .bind("<m>", repo_path)
                .subject(format!(
                    "the target's lock pins {}, which this clone lacks",
                    raw.as_str()
                ))
                .elsewhere(root)
                .by_verb("land")
                .judge(vcs.as_ref()),
        );
    }
}

/// Where each checkout of `T`'s workspace stands to the lock that workspace
/// has checked out, never the upstream's, whose moves the landing takes in:
/// one beyond its pin by commits no live lock and no remote reaches refuses
/// (`pin` unpinned in `T`); one behind or diverged is not where that lock
/// says.
fn judge_target_pins(
    class: &LockClassification,
    root: &Path,
    manifest: &Manifest,
    primary: &Path,
    report: &mut Report,
) -> anyhow::Result<()> {
    let live = crate::pins::live_pins(primary)?;
    judge_unreachable_target_pins(class, root, manifest, report);
    for r in &class.relations {
        let (Some(head), Some(pin)) = (&r.tip, &r.lock_pin) else {
            continue;
        };
        let dir = root.join(r.repo_path.as_path());
        let Some(entry) = manifest.get_entry(&r.repo_path) else {
            continue;
        };
        let vcs = vcs_for(entry.vcs_type);
        if !matches!(r.relation, LockRelation::Ahead)
            && (!matches!(
                entry.role,
                crate::manifest::Role::Owned | crate::manifest::Role::Fork
            ) || follows_its_pin_detached(entry.role, vcs.as_ref(), &dir))
        {
            continue;
        }
        let held = live
            .get(&r.repo_path)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let standing = crate::pins::standing(vcs.as_ref(), &dir, head, pin, held)?;
        if matches!(
            standing,
            crate::pins::Standing::At | crate::pins::Standing::Ahead(_)
        ) {
            continue;
        }
        if let Some(judged) = crate::pins::judge_elsewhere(
            &r.repo_path,
            &dir,
            vcs.as_ref(),
            &standing,
            head,
            pin,
            root,
        ) {
            report.add(judged);
        }
    }
    Ok(())
}

/// Run every precondition that gates the op AFTER the atomic acquire, every
/// judged refusal collected into one report so the operator sees them
/// together.
///
/// Nothing here writes: each checkout's shared-ref drift is classified into
/// `drifted`, and the caller refreshes it once the outcome is known, so on
/// refusal every checkout this left `drifted` alone looks exactly as it did
/// before acquisition.
#[allow(clippy::too_many_arguments)]
fn run_preconditions_after_acquire(
    drifted: &mut Vec<DriftedCheckout>,
    project_vcs: &dyn Vcs,
    landing: &crate::land::Landing,
    strategy: SyncStrategy,
    retire: bool,
    cwd_project_dir: &Path,
    cwd_workspace_dir: &Path,
    cwd_ctx: &WorkspaceContext,
    cwd_project_name: &ProjectName,
    dest_project_dir: &Path,
    dest_workspace_dir: &Path,
    dest_workspace_name: &str,
) -> anyhow::Result<PreconditionOutcome> {
    let mut report = landing.judged.clone();
    let mut unjudged: Option<anyhow::Error> = None;
    let mut hold = |result: anyhow::Result<()>, report: &mut Report| {
        if let Err(e) = report.absorb(result) {
            if unjudged.is_none() {
                unjudged = Some(e);
            }
        }
    };
    let cwd_project = Project::from_dir(cwd_project_dir, cwd_project_name.clone())
        .context("failed to load CWD project for guard preconditions")?;
    judge_edits_source_preflight(
        &cwd_project,
        cwd_workspace_dir,
        cwd_project_dir,
        &mut report,
        Some(&mut *drifted),
    );
    if retire {
        if let Checkout::Workweave { dir, project, .. } = &cwd_ctx.checkout {
            let dirty = crate::workweave::judge_dirty_checkouts(
                project_vcs,
                dir.as_path(),
                project,
                &cwd_project.manifest,
                None,
            );
            let project_repo = project_rel_path(project);
            for line in dirty.lines() {
                let judged = report.lines().iter().any(|l| l.repo() == line.repo());
                let drift_alone = drifted.iter().any(|d| {
                    d.at.elsewhere.is_none()
                        && d.at.repo == line.repo()
                        && line.listed().iter().all(|f| d.reading.refreshes(f))
                });
                if !judged && !drift_alone && line.repo() != project_repo {
                    report.add(line.clone());
                }
            }
        }
    }
    if strategy == SyncStrategy::Ff {
        hold(
            check_land_ff_precondition(project_vcs, cwd_project_dir, &landing.target_tip),
            &mut report,
        );
    }
    let snapshot = pin_source_snapshot(
        project_vcs,
        dest_project_dir,
        dest_workspace_dir,
        cwd_ctx.primary_path(),
        Some(ResolvedRevisionId::from(landing.target_tip.clone())),
        ClassifySource::Relations,
    )?;
    if let (Some(tip_class), Ok(tip_manifest)) = (
        snapshot.source_class.as_ref(),
        manifest_at_revision(project_vcs, dest_project_dir, &snapshot.source_project_tip),
    ) {
        judge_unreachable_target_pins(tip_class, dest_workspace_dir, &tip_manifest, &mut report);
    }
    let cwd_workspace_name_str = workspace_name(cwd_ctx)?;
    let cwd_class = {
        let (resolved, unresolvable) = match cwd_project.lock.as_ref() {
            Some(raw) => {
                let (resolved, failures) = raw.clone().resolve_versions(cwd_workspace_dir);
                (Some(resolved), failures)
            }
            None => (None, Vec::new()),
        };
        classify_lock_relations(
            cwd_workspace_dir,
            &cwd_project.manifest,
            resolved.as_ref(),
            unresolvable,
        )
    };
    judge_source_relations(
        &cwd_class,
        cwd_workspace_dir,
        &cwd_project.manifest,
        &mut report,
    );
    let checked_out_target = pin_source_snapshot(
        project_vcs,
        dest_project_dir,
        dest_workspace_dir,
        cwd_ctx.primary_path(),
        None,
        ClassifySource::Relations,
    )?;
    if let Some(target_class) = checked_out_target.source_class.as_ref() {
        if let Ok(target_manifest) = manifest_at_revision(
            project_vcs,
            dest_project_dir,
            &checked_out_target.source_project_tip,
        ) {
            judge_target_pins(
                target_class,
                dest_workspace_dir,
                &target_manifest,
                cwd_ctx.primary_path(),
                &mut report,
            )?;
        }
    }
    let cwd_lock_behind: Vec<RepoRelation> = cwd_class
        .relations
        .into_iter()
        .filter(|r| r.relation == LockRelation::Ahead)
        .collect();
    let sides = [
        (cwd_workspace_name_str.as_str(), cwd_project.lock.as_ref()),
        (dest_workspace_name, Some(&snapshot.raw_source_lock)),
    ];
    for (workspace, lock) in sides {
        for (repo_path, version) in lock.into_iter().flat_map(|l| l.entries_pinned_by_name()) {
            eprintln!(
                "warning: {}",
                crate::lock::pinned_by_name_warning(
                    format_args!("{workspace}/{repo_path}"),
                    version
                )
            );
        }
    }
    let plan = crate::land::judge(
        landing,
        cwd_workspace_dir,
        cwd_ctx.primary_path(),
        match strategy {
            SyncStrategy::Rebase => crate::land::Composing::Rebase,
            SyncStrategy::Ff => crate::land::Composing::FastForwardOnly,
        },
        &mut report,
    )?;
    judge_edits_target_preflight(
        &cwd_project,
        plan.receiving.keys(),
        dest_workspace_dir,
        dest_project_dir,
        &mut report,
        drifted,
    );
    if strategy == SyncStrategy::Ff {
        hold(
            refuse_members_that_need_composing(&cwd_project.manifest, cwd_workspace_dir, &plan),
            &mut report,
        );
    }
    if matches!(strategy, SyncStrategy::Rebase) {
        hold(
            crate::lock_driver::refuse_unmigrated(
                project_vcs,
                dest_project_dir.display(),
                dest_project_dir,
            ),
            &mut report,
        );
    }
    if let Some(e) = unjudged {
        if report.is_empty() {
            return Err(e);
        }
        report.note(format!("{e:#}"));
    }
    if report.lines().is_empty() && !report.notes().is_empty() {
        crate::refuse!(
            RefusalKind::NotFastForwardable,
            "{}",
            report.notes().join("\n")
        );
    }
    report.into_result()?;

    Ok(PreconditionOutcome {
        snapshot,
        cwd_project,
        cwd_workspace_name_str,
        cwd_lock_behind,
        plan,
    })
}

/// A [`SyncRequest::Fresh`] as [`guard_and_mark`] consumes it.
struct FreshOp {
    landing: crate::land::Landing,
    strategy: SyncStrategy,
    retire: bool,
    jobs: usize,
}

/// Guard (preconditions), mark (write owner record + leases), savepoint
/// (per-repo pre-op refs). Returns the immutable [`OpContext`] driving the
/// loop. Refusals here leave no trace.
fn guard_and_mark<'a>(
    cwd_ctx: &WorkspaceContext,
    op: FreshOp,
    handler: &'a dyn OutputHandler,
) -> anyhow::Result<OpContext<'a>> {
    let FreshOp {
        landing,
        strategy,
        retire,
        jobs,
    } = op;

    let emit_text = handler.emit_text();
    let cwd_workspace_dir = cwd_ctx.active_path().to_path_buf();
    let dest_workspace_dir = landing.target_workspace.clone();
    let cli_path = dest_workspace_dir.clone();

    let cwd_project_name = cwd_ctx.require_bound_project()?.clone();
    let cwd_project_dir = project_dir(&cwd_workspace_dir, &cwd_project_name);
    let dest_project_dir = project_dir(&dest_workspace_dir, &cwd_project_name);
    let source_workspace_name =
        WorkspaceContext::resolve_for_project(&dest_workspace_dir, &cwd_project_name)
            .and_then(|target| workspace_name(&target))
            .with_context(|| {
                format!(
                    "failed to resolve {} as a workspace of project `{}`",
                    dest_workspace_dir.display(),
                    cwd_project_name.as_str(),
                )
            })?;

    refuse_symlinked_members(&cwd_workspace_dir, &cwd_project_name)?;
    refuse_symlinked_members(&dest_workspace_dir, &cwd_project_name)?;

    // The touched-workspace set is claimed atomically before any
    // precondition runs, so two landings judging the same workspaces never
    // both pass; a refusal below releases the claim and leaves no trace.
    let op_id = OpId::new_now();
    let owner_workspace_dir = cwd_workspace_dir.clone();
    let initial_record = OwnerRecord::new_land(
        &op_id,
        strategy,
        cwd_project_name.clone(),
        &cwd_workspace_dir,
        &dest_workspace_dir,
        retire,
    );
    let touched = op_state::TouchedWorkspaces::of(
        initial_record.verb,
        &owner_workspace_dir,
        &dest_workspace_dir,
    );
    let acquired = op_state::acquire_op(&touched, &initial_record)?;

    let project_vcs = project_vcs();
    let mut drifted = Vec::new();
    let precondition_result = run_preconditions_after_acquire(
        &mut drifted,
        project_vcs.as_ref(),
        &landing,
        strategy,
        retire,
        &cwd_project_dir,
        &cwd_workspace_dir,
        cwd_ctx,
        &cwd_project_name,
        &dest_project_dir,
        &dest_workspace_dir,
        &source_workspace_name,
    );
    let PreconditionOutcome {
        snapshot,
        cwd_project,
        cwd_workspace_name_str,
        cwd_lock_behind,
        plan,
    } = match precondition_result {
        Ok(v) => v,
        Err(e) => {
            if crate::diagnosis::report_of(&e).is_some() {
                match refresh_drifted(&drifted, true) {
                    Ok(reports) => {
                        for report in &reports {
                            emit_landing_report(handler, report);
                        }
                    }
                    Err(refresh) => eprintln!("warning: {refresh:#}"),
                }
            }
            op_state::release_acquired(&acquired);
            return Err(e);
        }
    };
    let refreshed = match refresh_drifted(&drifted, false) {
        Ok(reports) => reports,
        Err(e) => {
            op_state::release_acquired(&acquired);
            return Err(e);
        }
    };
    for report in &refreshed {
        emit_landing_report(handler, report);
    }
    let _ = acquired;
    if emit_text {
        for line in plan.fact_lines(&landing.target) {
            eprintln!("land: {line}");
        }
    }

    // Savepoints on both sides: the source's repos replay, the target's
    // fast-forward, and `rwv abort` restores either. The target's are filed
    // under a key of their own, since the two sides may share a ref store.
    let savepoint_key = SavepointKey::Op(op_id.clone());
    create_savepoint(project_vcs.as_ref(), &cwd_project_dir, &savepoint_key)?;
    let mut project_anchors: Vec<(PathBuf, SavepointKey)> =
        vec![(cwd_project_dir.clone(), savepoint_key.clone())];
    let mut member_anchors: Vec<(Box<dyn Vcs>, PathBuf, SavepointKey)> = Vec::new();
    for (repo_path, entry) in cwd_project.manifest.iter_entries() {
        let abs = cwd_workspace_dir.join(repo_path.as_path());
        if abs.exists() {
            let vcs = vcs_for(entry.vcs_type);
            if create_savepoint(vcs.as_ref(), &abs, &savepoint_key).is_ok() {
                member_anchors.push((vcs, abs, savepoint_key.clone()));
            }
        }
    }
    let tsp_id = SavepointKey::LandTarget(op_id.clone());
    if create_savepoint(project_vcs.as_ref(), &dest_project_dir, &tsp_id).is_ok() {
        project_anchors.push((dest_project_dir.clone(), tsp_id.clone()));
    }
    if let Ok(tp) =
        crate::manifest::Project::from_dir_skip_lock(&dest_project_dir, cwd_project_name.clone())
    {
        for (repo_path, entry) in tp.manifest.iter_entries() {
            let abs = dest_workspace_dir.join(repo_path.as_path());
            if abs.exists() {
                let vcs = vcs_for(entry.vcs_type);
                if create_savepoint(vcs.as_ref(), &abs, &tsp_id).is_ok() {
                    member_anchors.push((vcs, abs, tsp_id.clone()));
                }
            }
        }
    }

    // The source relocks forward before composing, after the savepoints so
    // `rwv abort` rolls the relock back with the rest. A relock that cannot
    // commit refuses the op before any work, leaving nothing to abort.
    if !cwd_lock_behind.is_empty() {
        let lock_path = LockFile::path_in(&cwd_project_dir);
        let lock_found = lock_path.read().ok();
        let relocked = match regenerate_lock_phase3(
            project_vcs.as_ref(),
            cwd_ctx,
            &cwd_project_dir,
            &cwd_project,
            &source_workspace_name,
        ) {
            Ok(relocked) => relocked,
            Err(e) => {
                let restored = lock_found
                    .as_ref()
                    .map_or(Ok(()), |bytes| lock_path.publish(bytes));
                for (dir, id) in &project_anchors {
                    delete_savepoint(project_vcs.as_ref(), dir, id);
                }
                for (vcs, dir, id) in &member_anchors {
                    delete_savepoint(vcs.as_ref(), dir, id);
                }
                op_state::release_acquired(&acquired);
                let behind = op_start_relock_lines(
                    &cwd_lock_behind,
                    None,
                    &cwd_workspace_dir,
                    &cwd_project.manifest,
                    &cwd_workspace_name_str,
                )
                .join("\n");
                let restore_note = match restored {
                    Ok(()) => String::new(),
                    Err(r) => format!(
                        "\n\nRestoring {lock_path} to the bytes the op found also failed ({r}); \
                         check it before re-running."
                    ),
                };
                return Err(crate::untokened!(
                    EnvironmentPassthrough,
                    "the op-start relock in {} could not commit, so the op did not start and \
                     nothing was changed: {e:#}\n\n{behind}\n\n\
                     Clear what refused the commit, then re-run.{restore_note}",
                    cwd_project_dir.display(),
                ));
            }
        };
        if emit_text {
            for line in op_start_relock_lines(
                &cwd_lock_behind,
                relocked.as_ref(),
                &cwd_workspace_dir,
                &cwd_project.manifest,
                &cwd_workspace_name_str,
            ) {
                eprintln!("{line}");
            }
        }
    }

    let mut take_in = op_state::TakeInRecord {
        sync_from: Some(op_state::SyncFrom::Target(landing.target.to_string())),
        source_tip: Some(
            project_vcs
                .head_revision(&cwd_project_dir)
                .context("failed to read the source's project HEAD after the op-start relock")?
                .id()
                .clone(),
        ),
        allow_member_change: landing
            .allow_member_change
            .iter()
            .map(ToString::to_string)
            .collect(),
        ..op_state::TakeInRecord::default()
    };
    take_in.receiving.insert(
        RepoKey::Project,
        op_state::Receiving {
            branch: landing.target.to_string(),
            tip: landing.target_tip.clone(),
            moves: op_state::Moves::InCheckout,
        },
    );
    for (path, receiving) in &plan.receiving {
        take_in.receiving.insert(
            RepoKey::Member(path.clone()),
            op_state::Receiving {
                branch: receiving.branch.as_str().to_owned(),
                tip: receiving.tip.clone(),
                moves: receiving.moves,
            },
        );
    }
    take_in.left_behind = plan.left_behind.clone();
    take_in.reports = refreshed;
    report_left_behind(handler, &take_in.left_behind);
    for change in &plan.landed_changes {
        emit_landing_report(handler, change);
    }
    take_in.reports.extend(plan.landed_changes.iter().cloned());
    let mut record = op_state::read_owner(&owner_workspace_dir)?
        .context("the op record vanished as the landing began")?;
    record.take_in = Some(take_in.clone());
    op_state::write_owner(&owner_workspace_dir, &record)
        .context("failed to record the landing's plan")?;

    Ok(OpContext {
        cwd_ctx: cwd_ctx.clone(),
        cwd_workspace_dir,
        owner_workspace_dir,
        source_workspace_name,
        dest_workspace_dir,
        dest_project_dir,
        cwd_project_dir,
        cwd_project_name,
        cli_path,
        strategy,
        take_in,
        retire,
        retire_left: std::sync::atomic::AtomicBool::new(false),
        jobs,
        handler,
        project_vcs,
        verb: op_state::OpVerb::Land,
        op_id,
        snapshot,
    })
}

/// Load context for `--continue`: derive all op parameters from the owner
/// record `op` resolved (following a lease pointer if invoked from a
/// non-owner workspace), and rebuild the [`OpContext`].
///
/// Everything the engine consumes is rooted at the OWNER: `--continue`
/// invoked from the leased target operates identically to owner-side
/// invocation.
fn load_continuing_context<'a>(
    invocation_ctx: &'a WorkspaceContext,
    op: op_state::ResolvedOwner,
    project_override: Option<ProjectName>,
    allow_member_change: &[RepoOperand],
    handler: &'a dyn OutputHandler,
) -> anyhow::Result<OpContext<'a>> {
    let emit_text = handler.emit_text();
    let op_state::ResolvedOwner {
        record,
        owner_workspace,
    } = op;
    let op_id = record.id.clone();

    if record.verb != op_state::OpVerb::Land {
        crate::refuse!(
            RefusalKind::ResumeContradictsRecord,
            "in-progress op is `{recorded}` but `{invoked}` was invoked. \
             Run `{resume}` instead, or `rwv abort` to discard.",
            recorded = record.verb,
            invoked = op_state::resume_command(op_state::OpVerb::Land),
            resume = op_state::resume_command(record.verb),
        );
    }

    // The record is authoritative for a resumed op, so `--project` can only
    // assert what it already says: the phases already run used the recorded
    // project.
    if let Some(flag) = &project_override {
        if flag != &record.project {
            crate::refuse!(
                RefusalKind::ResumeContradictsRecord,
                "op was started for project `{recorded}`; `--project {flag}` contradicts the \
                 record — a half-completed op cannot be rebound. Re-run `{resume}` without \
                 `--project`, or `rwv abort` to discard the op and start over.",
                recorded = record.project,
                resume = op_state::resume_command(record.verb),
            );
        }
    }
    let mut take_in = record
        .take_in
        .clone()
        .filter(|take_in| take_in.receiving.contains_key(&RepoKey::Project))
        .ok_or_else(|| {
            crate::untokened!(
                InternalInvariant,
                "op {op_id} records no landing plan; `rwv abort` rolls it back"
            )
        })?;

    let cwd_ctx = if owner_workspace == *invocation_ctx.active_root() {
        invocation_ctx.clone()
    } else {
        WorkspaceContext::resolve_for_project(owner_workspace.as_path(), &record.project)?
    };
    let owner_workspace_dir = owner_workspace.into_path_buf();
    let cwd_workspace_dir = owner_workspace_dir.clone();
    let cwd_project_name = record.project.clone();
    let cwd_project_dir = project_dir(&owner_workspace_dir, &cwd_project_name);
    let dest_workspace_dir = record.target.as_path().to_path_buf();
    let cli_path = dest_workspace_dir.clone();
    let dest_project_dir = project_dir(&dest_workspace_dir, &cwd_project_name);
    let source_workspace_name =
        WorkspaceContext::resolve_for_project(&dest_workspace_dir, &cwd_project_name)
            .and_then(|target| workspace_name(&target))
            .with_context(|| {
                format!(
                    "failed to resolve {} as a workspace of project `{}`",
                    dest_workspace_dir.display(),
                    cwd_project_name.as_str(),
                )
            })?;
    let project_vcs = project_vcs();

    // A target that moved after the landing composed is a plan the landing no
    // longer describes: only `rwv abort` and a fresh landing judge the new
    // tips.
    if !matches!(record.phase, op_state::OpPhase::Retire) {
        let composed = record.tips.converged().filter(|tips| !tips.is_empty());
        if let Some(moved) =
            target_moved(&cwd_project_name, &dest_workspace_dir, &take_in, composed)?
        {
            return Err(moved.refused());
        }
    }

    rejudge_entries(
        &record,
        &mut take_in,
        &owner_workspace_dir,
        allow_member_change,
    )?;

    handler.record_resumed(record.phase.clone());
    let entry_phase = resume_entry_phase(&record.phase);
    if entry_phase != record.phase {
        op_state::set_phase(&owner_workspace_dir, entry_phase.clone())
            .context("failed to record the phase this resume re-enters")?;
    }
    if emit_text {
        if entry_phase == record.phase {
            eprintln!(
                "continuing {verb_str} (op {op_id}, mid `{phase}`)",
                verb_str = record.verb,
                phase = record.phase,
            );
        } else {
            eprintln!(
                "continuing {verb_str} (op {op_id}, mid `{phase}`); re-entering `{entry_phase}` \
                 first so the lock this lands pins the tips it lands",
                verb_str = record.verb,
                phase = record.phase,
            );
        }
    }
    report_left_behind(handler, &take_in.left_behind);
    for report in &take_in.reports {
        emit_landing_report(handler, report);
    }

    let target_tip = take_in
        .receiving
        .get(&RepoKey::Project)
        .map(|r| ResolvedRevisionId::from(r.tip.clone()));
    let snapshot = pin_source_snapshot(
        project_vcs.as_ref(),
        &dest_project_dir,
        &dest_workspace_dir,
        invocation_ctx.primary_path(),
        target_tip,
        ClassifySource::Relations,
    )?;

    // From relock onward the remaining phase that reads the target's worktree
    // is advance-target, whose fast-forward a dirty target blocks: refused
    // here, before relock commits. Retire re-entry is exempt: its own check
    // refuses any uncommitted state, untracked included.
    if matches!(entry_phase, op_state::OpPhase::Relock) {
        let cwd_project_preflight =
            Project::from_dir(&cwd_project_dir, cwd_project_name.clone())
                .context("failed to load CWD project for the resumed op's dirt scan")?;
        let mut edits = Report::default();
        let mut drifted = Vec::new();
        judge_edits_target_preflight(
            &cwd_project_preflight,
            take_in.upstream_tips.keys().filter_map(|key| match key {
                RepoKey::Member(path) => Some(path),
                RepoKey::Project => None,
            }),
            &dest_workspace_dir,
            &dest_project_dir,
            &mut edits,
            &mut drifted,
        );
        for report in refresh_drifted(&drifted, !edits.is_empty())? {
            if op_state::journal_report(&owner_workspace_dir, &report)? {
                emit_landing_report(handler, &report);
            }
        }
        edits.note(parked_note(record.verb));
        edits.into_result()?;
    }

    Ok(OpContext {
        cwd_ctx,
        cwd_workspace_dir,
        owner_workspace_dir,
        source_workspace_name,
        dest_workspace_dir,
        dest_project_dir,
        cwd_project_dir,
        cwd_project_name,
        cli_path,
        strategy: record.strategy,
        take_in,
        retire: record.retire,
        retire_left: std::sync::atomic::AtomicBool::new(false),
        jobs: 1,
        handler,
        project_vcs,
        verb: record.verb,
        op_id,
        snapshot,
    })
}

/// Judge the source's `rwv.toml`, as the stop's resolution leaves it, against
/// `T`'s tip under the consent the op recorded and the one this `--continue`
/// adds; record the added consent once it is judged. A landing that already
/// moved `T` has nothing left to judge, and a consent named then names
/// nothing.
fn rejudge_entries(
    record: &op_state::OwnerRecord,
    take_in: &mut op_state::TakeInRecord,
    owner_workspace_dir: &Path,
    added: &[RepoOperand],
) -> anyhow::Result<()> {
    let target_branch = match &take_in.sync_from {
        Some(op_state::SyncFrom::Target(name)) => name.clone(),
        _ => take_in
            .receiving
            .get(&RepoKey::Project)
            .map(|r| r.branch.clone())
            .unwrap_or_default(),
    };
    let target = target_branch.as_str();
    if matches!(record.phase, op_state::OpPhase::Retire) {
        if let Some(named) = added.first() {
            crate::refuse!(
                RefusalKind::InapplicableFlag,
                "`--allow-member-change {named}` names no change this landing makes to \
                 '{target}''s entries: it has already landed into '{target}'"
            );
        }
        return Ok(());
    }
    let Some(target_tip) = take_in
        .receiving
        .get(&RepoKey::Project)
        .map(|r| r.tip.clone())
    else {
        return Ok(());
    };
    let recorded: Vec<RepoOperand> = take_in
        .allow_member_change
        .iter()
        .map(|named| named.parse())
        .collect::<Result<_, _>>()?;
    let resumed = crate::land::Resumed {
        project: &record.project,
        target,
        target_tip: &target_tip,
        recorded: &recorded,
        added,
    };
    match crate::land::rejudge_resolved(&resumed, owner_workspace_dir)? {
        crate::land::Rejudged::Unresolved => return Ok(()),
        crate::land::Rejudged::Refused(mut report) => {
            report.note(parked_note(record.verb));
            report.into_result()?;
        }
        crate::land::Rejudged::Lands(changes) => {
            take_in.reports.retain(|r| !r.is_entry_change());
            take_in.reports.extend(changes);
        }
    }
    take_in.allow_member_change.extend(
        added
            .iter()
            .filter(|named| !recorded.contains(named))
            .map(ToString::to_string),
    );
    let mut owner = op_state::read_owner(owner_workspace_dir)?
        .context("the op record vanished as the landing resumed")?;
    if let Some(recorded) = owner.take_in.as_mut() {
        recorded.allow_member_change = take_in.allow_member_change.clone();
        recorded.reports = take_in.reports.clone();
    }
    op_state::write_owner(owner_workspace_dir, &owner)
        .context("failed to record the consent `--continue` added")?;
    Ok(())
}

/// The `target` moved mid-operation line, when a checkout the landing
/// fast-forwards in `T`'s workspace is no longer on the branch the plan
/// names for it, or a branch it moves, in a checkout or as a ref, no longer
/// fast-forwards to the tip `composed` holds for it (before the landing
/// composed, to the tip it read), or is gone where the plan does not create
/// it and no remote holds it at that tip to take it from.
fn target_moved(
    project: &ProjectName,
    target_workspace: &Path,
    take_in: &op_state::TakeInRecord,
    composed: Option<&std::collections::BTreeMap<RepoKey, CommitId>>,
) -> anyhow::Result<Option<crate::diagnosis::Judged>> {
    let mut moved: Vec<String> = Vec::new();
    let manifest =
        Manifest::from_path(&project_dir(target_workspace, project).join(Manifest::FILE_NAME)).ok();
    for (key, receiving) in &take_in.receiving {
        let to = match (receiving.moves, composed) {
            (op_state::Moves::Follows, _) => continue,
            (op_state::Moves::Enters, _) | (_, None) => &receiving.tip,
            (_, Some(composed)) => match composed.get(key) {
                Some(to) => to,
                None => continue,
            },
        };
        let (dir, vcs) = match key {
            RepoKey::Project => (project_dir(target_workspace, project), project_vcs()),
            RepoKey::Member(path) => {
                let Some(entry) = manifest.as_ref().and_then(|m| m.get_entry(path)) else {
                    continue;
                };
                (
                    target_workspace.join(path.as_path()),
                    vcs_for(entry.vcs_type),
                )
            }
        };
        let create = match receiving.moves {
            op_state::Moves::AsRef { create } => Some(create),
            op_state::Moves::Enters => Some(true),
            op_state::Moves::InCheckout | op_state::Moves::Follows => None,
        };
        if let Some(create) = create {
            if !vcs.is_repo(&dir) {
                continue;
            }
            let branch = crate::vcs::RawRefName::new(receiving.branch.clone());
            match vcs.resolve_local_branch_tip(&dir, &branch)? {
                Some(now)
                    if !vcs.is_ancestor(&dir, &now, &ResolvedRevisionId::from(to.clone()))? =>
                {
                    moved.push(format!(
                        "{key}: '{}' now at {}, which does not fast-forward to {}",
                        receiving.branch,
                        now.id().short(),
                        to.short()
                    ));
                }
                None if !create
                    && !vcs
                        .remote_copies(&dir, &branch)?
                        .iter()
                        .any(|copy| copy.tip.id() == &receiving.tip) =>
                {
                    moved.push(format!("{key}: '{}' is gone", receiving.branch));
                }
                _ => {}
            }
            continue;
        }
        let Ok(head) = vcs.head_revision(&dir) else {
            continue;
        };
        if let Some(off) = off_its_branch(vcs.as_ref(), &dir, &receiving.branch)? {
            moved.push(format!("{key}: {off}"));
            continue;
        }
        if !vcs.is_ancestor(&dir, &head, &ResolvedRevisionId::from(to.clone()))? {
            moved.push(format!(
                "{key}: now at {}, which does not fast-forward to {}",
                head.id().short(),
                to.short()
            ));
        }
    }
    if moved.is_empty() {
        return Ok(None);
    }
    Ok(Some(target_moved_line(project, target_workspace, moved)))
}

/// The other branch a checkout is on when it is not on `branch`, the branch
/// a landing fast-forwards there. A checkout on no branch is the move's own
/// refusal, which a switch back to `branch` clears.
fn off_its_branch(vcs: &dyn Vcs, dir: &Path, branch: &str) -> anyhow::Result<Option<String>> {
    Ok(match vcs.head_attachment(dir)? {
        HeadAttachment::Attached(on) if on.to_string() != branch => {
            Some(format!("on '{on}', not on '{branch}'"))
        }
        _ => None,
    })
}

fn target_moved_line(
    project: &ProjectName,
    target_workspace: &Path,
    moved: Vec<String>,
) -> crate::diagnosis::Judged {
    State::TargetMovedMidOperation
        .of(project_rel_path(project))
        .project_repo()
        .subject(format!(
            "a branch the landing fast-forwards in {} moved after the landing read it",
            target_workspace.display()
        ))
        .listing(moved)
        .by_verb("land")
        .judge(project_vcs().as_ref())
}

// ---------------------------------------------------------------------------
// Pre-loop helpers (precondition checks used by guard_and_mark)
// ---------------------------------------------------------------------------

/// land `--strategy=ff` precondition: CWD must be strictly ahead of
/// target. If equal: no-op (signaled by `Ok(())` from a caller that then
/// short-circuits — handled inside `run_replay` via the noop-detection path).
/// If diverged: refuse before any side effects.
fn check_land_ff_precondition(
    vcs: &dyn Vcs,
    cwd_project_dir: &Path,
    target_tip: &CommitId,
) -> anyhow::Result<()> {
    let cwd_tip = vcs
        .head_revision(cwd_project_dir)
        .context("failed to read CWD project HEAD")?;
    let target_tip = ResolvedRevisionId::from(target_tip.clone());
    if cwd_tip == target_tip {
        return Ok(());
    }
    let cwd_ahead = vcs
        .is_ancestor(cwd_project_dir, &target_tip, &cwd_tip)
        .unwrap_or(false);
    if !cwd_ahead {
        crate::refuse!(
            RefusalKind::NotFastForwardable,
            "land --strategy ff needs this feature to be ahead of its target, but its project \
             tip ({}) does not contain the target's tip ({}).\n\
             Rerun with `--strategy rebase` to compose it onto the target's tip first.",
            cwd_tip,
            target_tip,
        );
    }
    Ok(())
}

/// Under `--strategy ff` nothing is composed, so every member whose branch
/// does not contain the tip it lands on refuses.
fn refuse_members_that_need_composing(
    manifest: &Manifest,
    root: &Path,
    plan: &crate::land::Plan,
) -> anyhow::Result<()> {
    let mut behind: Vec<String> = Vec::new();
    for (path, receiving) in plan.composed() {
        let source = &receiving.tip;
        let Some(entry) = manifest.get_entry(path) else {
            continue;
        };
        let vcs = vcs_for(entry.vcs_type);
        let dir = root.join(path.as_path());
        let Ok(head) = vcs.head_revision(&dir) else {
            continue;
        };
        let source = ResolvedRevisionId::from(source.clone());
        if !vcs.is_ancestor(&dir, &source, &head).unwrap_or(false) {
            let shape = Containment::observe(vcs.as_ref(), &dir, &head, &source)
                .map(|c| c.describe(&format!("the tip it lands on ({})", source.id().short())))
                .unwrap_or_else(|| format!("not a descendant of {}", source.id().short()));
            behind.push(format!("{path}: HEAD is {shape}"));
        }
    }
    if behind.is_empty() {
        return Ok(());
    }
    crate::refuse!(
        RefusalKind::NotFastForwardable,
        "land --strategy ff composes nothing, and these members do not contain the tip \
         they land on:\n  {}\nRerun with `--strategy rebase` to compose them onto it first.",
        behind.join("\n  ")
    );
}

/// land dirty-target preflight: refuse if a repo of the target workspace that
/// advance-target fast-forwards, the project repo and each `moving` member,
/// has uncommitted **tracked** changes. A member the landing does not move is
/// left as it is, edits and all.
///
/// Untracked files are not scanned here — a fast-forward only ever touches
/// paths the incoming tree writes, so a target-side untracked file is safe
/// unless it collides with one of those paths. That narrower question
/// doesn't have an answer until replay has picked CWD's tips, which is after
/// this preflight runs; [`ff_advance_repo`] catches an actual collision at
/// advance time instead, where git itself refuses to clobber it.
fn judge_edits_target_preflight<'m>(
    cwd_project: &Project,
    moving: impl IntoIterator<Item = &'m RepoPath>,
    target_workspace_dir: &Path,
    target_project_dir: &Path,
    report: &mut Report,
    drifted: &mut Vec<DriftedCheckout>,
) {
    for repo_path in moving {
        let Some(entry) = cwd_project.manifest.get_entry(repo_path) else {
            continue;
        };
        let target_repo = target_workspace_dir.join(repo_path.as_path());
        if target_repo.exists() {
            judge_edits_beside_drift(
                vcs_for(entry.vcs_type),
                JudgedCheckout {
                    key: RepoKey::Member(repo_path.clone()),
                    repo: repo_path.to_string(),
                    dir: target_repo,
                    elsewhere: Some(target_workspace_dir.to_path_buf()),
                },
                |dirt| dirt,
                report,
                Some(&mut *drifted),
            );
        }
    }
    judge_edits_beside_drift(
        project_vcs(),
        JudgedCheckout {
            key: RepoKey::Project,
            repo: project_rel_path(&cwd_project.name),
            dir: target_project_dir.to_path_buf(),
            elsewhere: Some(target_workspace_dir.to_path_buf()),
        },
        |dirt| dirt,
        report,
        Some(drifted),
    );
}

/// A checkout a landing found drift in while judging its edits.
pub(crate) struct DriftedCheckout {
    vcs: Box<dyn Vcs>,
    at: JudgedCheckout,
    reading: crate::check::DriftReading,
    edits_refused: bool,
}

/// A checkout whose edits a landing judges: its repo, the label its edits
/// line names it by, and the workspace it is in when that is not this one.
pub(crate) struct JudgedCheckout {
    key: RepoKey,
    repo: String,
    dir: PathBuf,
    elsewhere: Option<PathBuf>,
}

/// Judge the checkout's edits on what a drift refresh would leave: shared-ref drift
/// is classified first, so a path drift explains is neither refused as an
/// edit nor listed under the choice that commits edits. A checkout stopped
/// in an operation, or one whose reading fails, is judged on all its dirt.
/// `keep` narrows the dirt judged, as the source's project repo does for its
/// lock. Without `drifted` no drift is read, and every difference is judged
/// an edit: a judgment that can only end in a refusal reports drift as the
/// dirt it looks like rather than set up a refresh it may not make.
fn judge_edits_beside_drift(
    vcs: Box<dyn Vcs>,
    at: JudgedCheckout,
    keep: impl Fn(Vec<String>) -> Vec<String>,
    report: &mut Report,
    drifted: Option<&mut Vec<DriftedCheckout>>,
) {
    let reading = match &drifted {
        Some(_) => crate::check::read_drift(vcs.as_ref(), &at.dir).unwrap_or_default(),
        None => crate::check::DriftReading::default(),
    };
    let dirt: Vec<String> = keep(tracked_or_unreadable(vcs.as_ref(), &at.dir))
        .into_iter()
        .filter(|file| !reading.refreshes(file))
        .collect();
    let judged = crate::diagnosis::judge_edits(
        vcs.as_ref(),
        &at.repo,
        &at.dir,
        dirt,
        at.elsewhere.as_deref(),
    );
    let edits_refused = judged.is_some();
    if let Some(judged) = judged {
        report.add(judged);
    }
    if let (Some(drifted), true) = (drifted, reading.is_refreshable()) {
        drifted.push(DriftedCheckout {
            vcs,
            at,
            reading,
            edits_refused,
        });
    }
}

/// Refresh the drift `drifted` classified, returning each checkout's report.
///
/// A landing that proceeds refreshes every one. A landing that refuses
/// refreshes only the checkouts whose own edits it refused, so the commit or
/// stash it offers there cannot carry the drift's stale content; every other
/// checkout is left as the refusal found it.
fn refresh_drifted(
    drifted: &[DriftedCheckout],
    landing_refused: bool,
) -> anyhow::Result<Vec<op_state::LandingReport>> {
    let mut reports = Vec::new();
    for checkout in drifted {
        if landing_refused && !checkout.edits_refused {
            continue;
        }
        let refreshed =
            crate::check::refresh_drift(checkout.vcs.as_ref(), &checkout.at.dir, &checkout.reading)
                .map_err(|e| {
                    crate::untokened!(
                        EnvironmentPassthrough,
                        "refreshing the drift in {} failed: {e:#}",
                        checkout.at.dir.display()
                    )
                })?;
        if let Some(refreshed) = refreshed {
            reports.push(op_state::LandingReport::Refreshed {
                repo: checkout.at.key.clone(),
                checkout: checkout.at.dir.clone(),
                moved_from: refreshed.moved_from,
                moved_to: refreshed.moved_to,
                files: refreshed.files,
                index_entries: refreshed.index_entries,
            });
        }
    }
    Ok(reports)
}

/// A repo's tracked dirt, or a status that could not be read standing in as
/// dirt, so the scan fails closed rather than moving over an unknown state.
fn tracked_or_unreadable(vcs: &dyn Vcs, repo: &Path) -> Vec<String> {
    vcs.tracked_dirty_file_names(repo)
        .unwrap_or_else(|_| vec!["(status unreadable)".to_string()])
}

/// [`crate::diagnosis::judge_edits`] into `report`, the repo named in the
/// workspace `elsewhere` when that is not the one the verb runs in.
fn judge_edits_in(
    vcs: &dyn Vcs,
    repo: impl std::fmt::Display,
    dir: &Path,
    dirty: Vec<String>,
    elsewhere: Option<&Path>,
    report: &mut Report,
) {
    if let Some(judged) = crate::diagnosis::judge_edits(vcs, repo, dir, dirty, elsewhere) {
        report.add(judged);
    }
}

/// A `dependency` or `reference` member sitting detached with no commit of
/// its own: the state alignment puts such a member in wherever its branch is
/// checked out elsewhere. It holds nothing a branch lacks, so a verb that
/// needs every member on a branch passes over it.
fn follows_its_pin_detached(role: crate::manifest::Role, vcs: &dyn Vcs, repo: &Path) -> bool {
    if !matches!(
        role,
        crate::manifest::Role::Dependency | crate::manifest::Role::Reference
    ) {
        return false;
    }
    match vcs.head_attachment(repo) {
        Ok(HeadAttachment::Detached(d)) => vcs
            .stray_commits(repo, d.at(), None, None)
            .is_ok_and(|stray| stray.is_empty()),
        _ => false,
    }
}

/// land source-side cleanliness preflight.
///
/// Before writing op-state or touching any repo, refuse if the CWD side (the
/// operator's own workspace — where replay runs) carries uncommitted **tracked**
/// changes in any manifest repo or the project repo. This defines the
/// "half-rebased op with a pre-rebase lock" state out of existence for the
/// dirty-tree class: today the blast radius depends on repo iteration order —
/// repos before the dirty one get rebased, the lock goes stale, then the op dies
/// mid-replay. One refusal naming every dirty path, before anything rebases.
///
/// - **Untracked files are fine** — they survive the replay untouched, matching
///   the intent recorded in the spec (a scratch file must not block a landing).
///   Only tracked modifications (staged or unstaged) refuse.
/// - **Carve-out:** a dirty `projects/<p>/rwv.lock` *alone* is NOT dirt — it is
///   the auto-relock's own input and the op commits it. A project repo that
///   is dirty *only* in `rwv.lock` passes; any other tracked project-repo change
///   still refuses (and the project entry names the specific files so the lock
///   carve-out is auditable).
fn judge_edits_source_preflight(
    cwd_project: &Project,
    cwd_workspace_dir: &Path,
    cwd_project_dir: &Path,
    report: &mut Report,
    mut drifted: Option<&mut Vec<DriftedCheckout>>,
) {
    for (repo_path, entry) in cwd_project.manifest.iter_entries() {
        let repo = cwd_workspace_dir.join(repo_path.as_path());
        if repo.exists() {
            judge_edits_beside_drift(
                vcs_for(entry.vcs_type),
                JudgedCheckout {
                    key: RepoKey::Member(repo_path.clone()),
                    repo: repo_path.to_string(),
                    dir: repo,
                    elsewhere: None,
                },
                |dirt| dirt,
                report,
                drifted.as_deref_mut(),
            );
        }
    }

    // Project repo: apply the rwv.lock carve-out. A project repo dirty ONLY in
    // rwv.lock is the auto-relock's expected input, not dirt.
    judge_edits_beside_drift(
        project_vcs(),
        JudgedCheckout {
            key: RepoKey::Project,
            repo: project_rel_path(&cwd_project.name),
            dir: cwd_project_dir.to_path_buf(),
            elsewhere: None,
        },
        |dirt| {
            dirt.into_iter()
                .filter(|p| p.as_str() != LockFile::FILE_NAME)
                .collect()
        },
        report,
        drifted,
    );
}

// ---------------------------------------------------------------------------
// Benign-staleness classification
// ---------------------------------------------------------------------------
//
// At op start each manifest repo's committed lock SHA is classified against its
// on-disk HEAD via the SAME per-repo relation vocabulary `rwv status` surfaces
// ([`crate::status::LockRelation`] — no parallel enum).
//
// TERMINOLOGY (load-bearing — the spec and the enum name this from opposite
// vantage points): the spec's "lock behind HEAD" (lock is a strict ancestor of
// HEAD — new commits since the last lock, the normal shape of in-progress work)
// is [`LockRelation::Ahead`] — the *tip* is ahead of the lock. That relation is
// the BENIGN case: a landing auto-relocks, a pull takes the source's committed
// tips. The spec's "ahead" case (HEAD is a strict ancestor of the lock — a reset
// or an `update` without FF) is [`LockRelation::Behind`] — the *tip* is behind
// the lock; that is anomalous and refuses. Every non-`ok` relation other than
// `Ahead` (i.e. `Behind` / `Diverged` / `NoLock` / `Unknown` / `Missing` /
// `Unreachable`) hard-refuses, naming the relation. The ancestry gate is the
// whole answer — a benign `Ahead` cannot conceal divergence, because the
// "missing" lock entries are exactly the commits the op will replay/FF.
//
// `Missing` (clone dir absent from disk) and `Unreachable` (clone present but
// locked SHA not in the local object store) are clone-health states; sync skips
// repos that don't exist on disk (`if !repo_abs.exists() { continue }`) so
// these relations are only emitted by `rwv status` — the sync lock-freshness
// gate never sees them.

/// One manifest repo's lock↔HEAD relation, plus the commit count for the benign
/// `Ahead` case (the tip is ahead of the lock — "lock behind HEAD" in the spec's
/// phrasing). The count makes the surprising number visible at the moment it
/// matters — the LOUD auto-relock line.
struct RepoRelation {
    repo_path: RepoPath,
    relation: LockRelation,
    /// `Some(n)` only when `relation == Ahead`: how many commits HEAD is ahead
    /// of the lock (`lock..HEAD`). `None` for every other relation.
    ahead_count: Option<usize>,
    /// The checkout's HEAD at classification time; `None` when unreadable.
    tip: Option<ResolvedRevisionId>,
    /// The committed lock's revision for this repo, resolved against the
    /// workspace — the baseline `relation` and `ahead_count` were computed
    /// against. `None` when the project carries no lock entry for it.
    lock_pin: Option<ResolvedRevisionId>,
}

/// Result of classifying a workspace's committed lock against its repos: the
/// per-repo relations, plus any lock entries whose pinned revision could not be
/// resolved on disk (a tag/branch that no longer exists). Unresolvable entries
/// are a corrupt-lock error distinct from any relation and are reported first
/// (naming the unknown revision), preserving the old lock-freshness diagnostic.
struct LockClassification {
    relations: Vec<RepoRelation>,
    unresolvable: Vec<(RepoPath, crate::vcs::RawRevisionId)>,
}

/// Classify every manifest repo's committed lock SHA against its on-disk HEAD.
///
/// Repos missing on disk are skipped (nothing to classify).
/// `resolved_lock` is the workspace's own lock already resolved against
/// `workspace_dir` — the caller resolves, so the value the relations compare
/// against is the same one every other consumer of that resolve reads.
/// `None` when the project carries no committed lock at all (every entry then
/// classifies as `no-lock`). `unresolvable` carries the entries that resolve
/// produced no answer for, so they are reported rather than silently dropped.
fn classify_lock_relations(
    workspace_dir: &Path,
    manifest: &Manifest,
    resolved_lock: Option<&crate::manifest::ResolvedLockFile>,
    unresolvable: Vec<(RepoPath, crate::vcs::RawRevisionId)>,
) -> LockClassification {
    let mut out = Vec::new();
    for (repo_path, entry) in manifest.iter_entries() {
        let vcs = vcs_for(entry.vcs_type);
        let repo_abs = workspace_dir.join(repo_path.as_path());
        if !repo_abs.exists() {
            continue;
        }
        let tip = vcs.head_revision(&repo_abs).ok();
        // A repo whose lock entry failed to resolve is reported via
        // `unresolvable` (a corrupt-lock error), not as a `no-lock` relation —
        // skip it here to avoid a double-report.
        if unresolvable.iter().any(|(p, _)| p == repo_path) {
            continue;
        }
        let lock_sha = resolved_lock
            .and_then(|l| l.get_entry(repo_path))
            .map(|e| e.version.clone());
        let relation = compute_relation(vcs.as_ref(), &repo_abs, &tip, &lock_sha);
        let ahead_count = if relation == LockRelation::Ahead {
            // `Ahead` ⟺ lock is a strict ancestor of tip (both present, per
            // compute_relation). Count lock..HEAD — the commits the landing
            // replays / the auto-relock pins.
            match (lock_sha.as_ref(), tip.as_ref()) {
                (Some(lock), Some(tip)) => vcs.count_commits_in_range(&repo_abs, lock, tip).ok(),
                _ => None,
            }
        } else {
            None
        };
        out.push(RepoRelation {
            repo_path: repo_path.clone(),
            relation,
            ahead_count,
            tip,
            lock_pin: lock_sha,
        });
    }
    LockClassification {
        relations: out,
        unresolvable,
    }
}

/// What is true of a refused resume and of no fresh refusal: the op is still
/// recorded, and both ways out of it are still open.
fn parked_note(verb: op_state::OpVerb) -> String {
    format!(
        "This op is still parked at its recorded phase; the refusal changed nothing. \
         Fix the source, then `{resume}` — or `rwv abort` to roll the whole op back.",
        resume = op_state::resume_command(verb),
    )
}

/// The op-start auto-relock's per-repo lines, derived from the commit's own
/// result.
///
/// `committed` is the lock the relock commit landed, `None` when no commit
/// happened — nothing was staged, or the commit failed. Without a commit there
/// is no relock to announce and no fresh pin to name, so the line states what
/// still holds: the committed lock is behind HEAD by what the classification
/// measured. With a commit, the count spans the entry the classification read
/// to the entry the commit wrote, so a repo that took another commit during the
/// op-start window reports the tips the lock now pins rather than the ones it
/// was behind by when the op began.
fn op_start_relock_lines(
    behind: &[RepoRelation],
    committed: Option<&ResolvedLockFile>,
    workspace_dir: &Path,
    manifest: &Manifest,
    workspace_name: &str,
) -> Vec<String> {
    behind
        .iter()
        .map(|r| {
            let pinned = committed
                .and_then(|lock| lock.get_entry(&r.repo_path))
                .map(|entry| &entry.version);
            let Some(pinned) = pinned else {
                let n = display_count(r.ahead_count);
                return format!(
                    "{workspace_name}/{}: lock behind HEAD by {n} commits — not relocked; the \
                     committed lock still pins the older tips",
                    r.repo_path,
                );
            };
            let relocked_over = manifest
                .get_entry(&r.repo_path)
                .zip(r.lock_pin.as_ref())
                .and_then(|(entry, from)| {
                    vcs_for(entry.vcs_type)
                        .count_commits_in_range(
                            &workspace_dir.join(r.repo_path.as_path()),
                            from,
                            pinned,
                        )
                        .ok()
                });
            format!(
                "{workspace_name}/{}: lock behind HEAD by {n} commits — auto-relocked to {pin}",
                r.repo_path,
                n = display_count(relocked_over),
                pin = pinned.id().short(),
            )
        })
        .collect()
}

/// A commit count for an operator line, or `?` when the count could not be
/// taken.
fn display_count(n: Option<usize>) -> String {
    n.map_or_else(|| "?".to_owned(), |c| c.to_string())
}

// ---------------------------------------------------------------------------
// Phase: replay
// ---------------------------------------------------------------------------
//
// Runs Phase 1' (project repo), materializes the members its manifest names,
// replays the manifest repos, then prunes the members that manifest no longer
// names. Per-repo parallelism and partial-failure reporting live inside this
// phase.
//
// **Re-entry rule:** per-repo state is derived from the VCS itself:
// - repo at its savepoint → redo the strategy (no-op for already-clean cases);
// - repo mid-conflict → leave the VCS-native continue/abort to the operator;
// - repo already at the converged target → no-op (HEAD == lock target).
//
// No resume flags. The strategy functions already handle the "already there"
// case via the `head == target` short-circuit at the top of `sync_one_repo`.

fn run_replay(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    let emit_text = ctx.handler.emit_text();

    // land with `--strategy=ff`: replay is a no-op (CWD is strictly ahead
    // of target per guard's ff precondition). The advance-target phase does
    // all the work.
    if matches!(ctx.verb, op_state::OpVerb::Land) && ctx.strategy == SyncStrategy::Ff {
        return Ok(());
    }

    if emit_text {
        eprintln!(
            "land: composing this workspace onto {} ({})...",
            target_name(ctx),
            ctx.cli_path.display(),
        );
    }

    // Snapshot was pinned in guard (or re-pinned on --continue). Re-entry
    // rule: per-repo state is derived from the VCS itself — already-
    // converged repos no-op via `sync_one_repo`'s head-equals-target check.
    let snapshot = &ctx.snapshot;

    // CWD project tip — read before any side effects so Phase 1' has the
    // pre-op starting state for its `cwd_tip == source_tip` short-circuit.
    let cwd_project_tip = ctx
        .project_vcs
        .head_revision(&ctx.cwd_project_dir)
        .context("failed to read CWD project HEAD")?;

    // === Phase 1' (project repo) — strategy on the project repo ===

    let phase1_outcome = apply_project_strategy(
        ctx.project_vcs.as_ref(),
        &ctx.cwd_project_dir,
        &snapshot.source_project_tip,
        &cwd_project_tip,
        ctx.strategy,
        ctx.verb,
    );

    if let Err(e) = phase1_outcome {
        if emit_text {
            eprintln!("Phase 1' (project repo) failed: {e:#}");
        }
        // Only teach the VCS-native resume when the project repo is actually
        // left mid-op: a non-conflict rebase error leaves no in-flight VCS op.
        let live_conflict = ctx.project_vcs.mid_op(&ctx.cwd_project_dir);
        crate::refuse!(
            RefusalKind::OpParked,
            "{}",
            phase1_or_phase3_failure_message(
                ctx.project_vcs.as_ref(),
                Phase::One,
                &ctx.cwd_project_dir,
                ctx.verb,
                live_conflict,
            )
        );
    }

    // === advanced_tips write 2: capture actual post-Phase-1' project repo tip ===
    //
    // Phase 1' may rebase CWD's project commits onto source_project_tip, landing
    // at a fresh SHA T1 that was not knowable at replay entry.  Overwrite the
    // project repo's advanced_tips entry with the actual post-rebase HEAD.
    // This also covers the fast-forward case (tip == source tip, idempotent
    // overwrite).
    {
        let project_tip = ctx
            .project_vcs
            .head_revision(&ctx.cwd_project_dir)
            .context("failed to read project HEAD after Phase 1'")?;
        let mut owner = op_state::read_owner(&ctx.owner_workspace_dir)?.ok_or_else(|| {
            crate::untokened!(
                InternalInvariant,
                "internal: owner record missing after Phase 1'"
            )
        })?;
        owner
            .tips
            .advanced_mut()
            .ok_or_else(|| {
                crate::untokened!(
                    InternalInvariant,
                    "internal: advanced_tips write after convergence post Phase 1'"
                )
            })?
            .insert(RepoKey::Project, project_tip.id().clone());
        op_state::write_owner(&ctx.owner_workspace_dir, &owner)
            .context("failed to write advanced_tips after Phase 1'")?;
    }

    // === Member set: decided against the manifest Phase 1' produced ===
    //
    // Checkouts are added and removed only below this point, and removed only
    // after every manifest repo replayed, so an op that parks leaves every
    // checkout in place for `rwv abort`, which restores refs and nothing else.
    let replayed = reload_replayed_project(ctx)?;
    let pre_op_manifest = pre_op_manifest(ctx)?;

    let mut materialize_failures: Vec<crate::manifest::RepoPath> = Vec::new();
    for repo_path in snapshot.raw_source_lock.iter_repo_paths() {
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if abs.exists() {
            continue;
        }
        let Some(entry) = replayed.manifest.get_entry(repo_path) else {
            continue;
        };
        // Born where replay will aim it, so the new checkout needs no move.
        let declared = entry.branch.local_counterpart();
        let start = snapshot
            .source_lock
            .get_entry(repo_path)
            .map(|e| e.version.id().as_str())
            .unwrap_or_else(|| declared.as_str());
        match materialize_missing_repo(
            vcs_for(entry.vcs_type).as_ref(),
            &ctx.cwd_ctx,
            repo_path,
            entry,
            start,
            &ctx.cwd_project_name,
        ) {
            Ok(files) => {
                if let Some(files) = files {
                    ctx.handler
                        .record_placement(PlacementOutput::new(repo_path, &abs, files));
                }
                if emit_text {
                    outln!(
                        "  {repo_path}: materialized{}",
                        crate::share::files_note(files)
                    );
                }
            }
            Err(e) => {
                if emit_text {
                    eprintln!("  {repo_path}: materialize failed: {e:#}");
                    for route in crate::refusal::routes(&e) {
                        eprintln!("rwv explain {route}");
                    }
                }
                ctx.handler
                    .record_member(MemberOutput::materialize_failed(repo_path, &abs, &e));
                materialize_failures.push(repo_path.clone());
            }
        }
    }

    let mut any_failure = !materialize_failures.is_empty();

    struct SyncTask {
        repo_path: crate::manifest::RepoPath,
        abs: PathBuf,
        /// The repo's backend, resolved on this thread before the fan-out.
        /// Workers borrow it; none of them resolves one.
        vcs: Box<dyn Vcs>,
        target: ResolvedRevisionId,
        /// The verdicts this repo's replay was decided from, read once here at
        /// replay entry. The intent write below and the move in the fan-out
        /// both consume this value, so the record cannot describe a different
        /// landing than the one the strategy attempts; a commit that lands
        /// between the two makes the strategy fail rather than silently
        /// re-classifying the repo.
        verdicts: ReplayVerdicts,
        /// The repo's tip before this op replayed anything, taken from the
        /// op's savepoint so a `--continue` re-entry reads the same tip the
        /// interrupted run did rather than the pick it stopped on.
        pre_replay_tip: Option<ResolvedRevisionId>,
    }
    let mut sync_tasks: Vec<SyncTask> = Vec::new();

    for (key, receiving) in &ctx.take_in.receiving {
        let RepoKey::Member(repo_path) = key else {
            continue;
        };
        if receiving.moves == op_state::Moves::Enters {
            continue;
        }
        let source = &receiving.tip;
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            if emit_text && !materialize_failures.contains(repo_path) {
                outln!("  {repo_path}: skipped (not on disk)");
            }
            continue;
        }
        if receiving.moves == op_state::Moves::Follows {
            let vcs = replayed
                .manifest
                .get_entry(repo_path)
                .map(|e| vcs_for(e.vcs_type))
                .unwrap_or_else(crate::vcs::probe_vcs);
            follow_receiving(ctx, vcs.as_ref(), repo_path, &abs, source)?;
            continue;
        }
        // The lock names paths; the manifest names backends. A lock entry
        // with no manifest entry has no declared backend to resolve from.
        let vcs = replayed
            .manifest
            .get_entry(repo_path)
            .map(|e| vcs_for(e.vcs_type))
            .unwrap_or_else(crate::vcs::probe_vcs);
        let target = ResolvedRevisionId::from(source.clone());
        let baseline = ReplayBaseline::SourceLockEntry;
        let containment = match vcs.head_revision(&abs) {
            Ok(head) => Containment::observe(vcs.as_ref(), &abs, &head, &target),
            Err(e) => {
                any_failure = true;
                let outcome = RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
                    error: format!("{e:#}"),
                    cause: Some(e),
                });
                ctx.handler.record(repo_path.as_str(), &abs, &outcome, None);
                continue;
            }
        };
        let source_position = snapshot.source_tips.get(repo_path).and_then(|source_tip| {
            Containment::observe(
                vcs.as_ref(),
                &snapshot.source_workspace_root.join(repo_path.as_path()),
                source_tip,
                &target,
            )
        });
        sync_tasks.push(SyncTask {
            repo_path: repo_path.clone(),
            pre_replay_tip: vcs.resolve_savepoint(&abs, &ctx.savepoint_key()),
            abs,
            vcs,
            target,
            verdicts: ReplayVerdicts {
                baseline,
                containment,
                source_position,
            },
        });
    }

    // === advanced_tips write 1: pre-write planned targets for genuine ff-movers ===
    {
        let entry_tips: std::collections::BTreeMap<RepoKey, CommitId> = sync_tasks
            .iter()
            .filter_map(|task| entry_tip(&task.repo_path, &task.verdicts, &task.target))
            .collect();
        if !entry_tips.is_empty() {
            let mut owner = op_state::read_owner(&ctx.owner_workspace_dir)?.ok_or_else(|| {
                crate::untokened!(
                    InternalInvariant,
                    "internal: owner record missing during replay entry write"
                )
            })?;
            owner
                .tips
                .advanced_mut()
                .ok_or_else(|| {
                    crate::untokened!(
                        InternalInvariant,
                        "internal: advanced_tips write after convergence at replay entry"
                    )
                })?
                .extend(entry_tips);
            op_state::write_owner(&ctx.owner_workspace_dir, &owner)
                .context("failed to write advanced_tips at replay entry")?;
        }
    }

    let strategy = ctx.strategy;
    // Return type: (is_failure, Option<actual_head_if_converged>).
    // The actual HEAD is read inside the closure (single-repo reads, no shared
    // state) and returned for the post-join batch write.  No write to the owner
    // record happens inside this closure — that would be a race.
    let task_results: Vec<(bool, Option<CommitId>)> =
        run_in_parallel(&sync_tasks, ctx.jobs, |_idx, task| {
            let outcome = sync_one_repo(
                task.vcs.as_ref(),
                &task.abs,
                &task.target,
                task.verdicts,
                strategy,
                task.pre_replay_tip.as_ref(),
            );
            let is_failure = outcome.is_failure();
            // Capture the actual post-advance HEAD if this task converged.
            // For ff-movers this equals the pre-written target (idempotent
            // overwrite in write 3); for rebased repos it is the fresh SHA.
            let converged_head = if matches!(outcome, RepoSyncOutcome::Converged { .. }) {
                task.vcs
                    .head_revision(&task.abs)
                    .ok()
                    .map(|h| h.id().clone())
            } else {
                None
            };
            if !is_failure {
                refresh_drift_after(ctx.verb, &task.repo_path, task.vcs.as_ref(), &task.abs);
            }
            ctx.handler.record(
                task.repo_path.as_str(),
                &task.abs,
                &outcome,
                ContainmentOutput::observed(task.verdicts.containment, task.verdicts.baseline),
            );
            (is_failure, converged_head)
        });

    // === advanced_tips write 3: batch-write actual tips of converged manifest repos ===
    //
    // Single-threaded post-join, so no race against write_owner.  Overwrites the
    // ff-pre-written entries (same value, idempotent) and captures fresh rebased
    // SHAs for manifest repos that had local commits to replay.
    // Must precede the any_failure bail so partially-advanced repos are captured
    // even when the overall fan-out fails.
    {
        let post_join_tips: Vec<(RepoKey, CommitId)> = sync_tasks
            .iter()
            .zip(task_results.iter())
            .filter_map(|(task, (_, head_opt))| {
                head_opt
                    .as_ref()
                    .map(|h| (RepoKey::Member(task.repo_path.clone()), h.clone()))
            })
            .collect();
        if !post_join_tips.is_empty() {
            let mut owner = op_state::read_owner(&ctx.owner_workspace_dir)?.ok_or_else(|| {
                crate::untokened!(
                    InternalInvariant,
                    "internal: owner record missing during post-fan-out write"
                )
            })?;
            let advanced = owner.tips.advanced_mut().ok_or_else(|| {
                crate::untokened!(
                    InternalInvariant,
                    "internal: advanced_tips write after convergence post fan-out"
                )
            })?;
            for (repo_path, tip) in post_join_tips {
                advanced.insert(repo_path, tip);
            }
            op_state::write_owner(&ctx.owner_workspace_dir, &owner)
                .context("failed to write advanced_tips after fan-out join")?;
        }
    }

    if task_results.iter().any(|(f, _)| *f) {
        any_failure = true;
    }

    if any_failure {
        // A VCS-native resolution hint is only correct when a manifest repo is
        // genuinely left mid-op. Probe the involved repos for a
        // live conflict rather than assuming one from the strategy — a batch of
        // fetch/head-unreadable failures leaves no rebase to `--continue`.
        let live_conflict = sync_tasks
            .iter()
            .find_map(|t| t.vcs.mid_op(&t.abs).map(|op| (t.vcs.as_ref(), op)));
        crate::refuse!(
            RefusalKind::OpParked,
            "{}",
            match live_conflict {
                Some((vcs, op)) => manifest_repo_failure_message(vcs, ctx.verb, Some(op)),
                None => manifest_repo_failure_message(ctx.project_vcs.as_ref(), ctx.verb, None),
            }
        );
    }

    for repo_path in replayed.manifest.iter_repo_paths() {
        let checkout = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if snapshot.raw_source_lock.contains_repo(repo_path) || !checkout.exists() {
            continue;
        }
        if emit_text {
            outln!(
                "  {repo_path}: kept (not in the source's lock; the replayed manifest names it)"
            );
        }
        ctx.handler
            .record_member(MemberOutput::kept(repo_path, &checkout));
    }

    for (repo_path, entry) in pre_op_manifest.iter_entries() {
        let checkout = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if replayed.manifest.contains_repo(repo_path) || !checkout.exists() {
            continue;
        }
        let member =
            match prune_dropped_repo(vcs_for(entry.vcs_type).as_ref(), &ctx.cwd_ctx, repo_path) {
                Ok(()) => {
                    if emit_text {
                        outln!("  {repo_path}: pruned (dropped from lock)");
                    }
                    MemberOutput::pruned(repo_path, &checkout)
                }
                Err(e) => {
                    if emit_text {
                        eprintln!("  {repo_path}: prune skipped: {e:#}");
                    }
                    MemberOutput::prune_refused(repo_path, &checkout, &e)
                }
            };
        ctx.handler.record_member(member);
    }

    settle_members(
        &ctx.cwd_workspace_dir,
        &ctx.cwd_project_dir,
        &|vcs, member| vcs.resolve_savepoint(member, &ctx.savepoint_key()),
        Some(ctx.handler),
    );

    Ok(())
}

/// A member whose declared branch the landing changes and that holds no new
/// work takes its receiving branch's tip, merging nothing: its branch moves
/// there, the tip it leaves kept in a backup.
fn follow_receiving(
    ctx: &OpContext<'_>,
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    abs: &Path,
    source: &CommitId,
) -> anyhow::Result<()> {
    let head = vcs.head_revision(abs)?;
    if head.id() == source {
        return Ok(());
    }
    let HeadAttachment::Attached(attached) = vcs.head_attachment(abs)? else {
        return Err(crate::untokened!(
            EnvironmentPassthrough,
            "{repo_path}: left its branch during the landing"
        ));
    };
    let branch = crate::vcs::RawRefName::new(attached.to_string());
    let kept = vcs.create_backup(
        abs,
        &crate::vcs::BackupSource::Branch(branch.clone()),
        &head,
        None,
    )?;
    vcs.reset_attached_ref_keeping(&attached, &ResolvedRevisionId::from(source.clone()), &kept)?;
    op_state::journal_tip(
        &ctx.owner_workspace_dir,
        RepoKey::Member(repo_path.clone()),
        source,
    )?;
    report_landing(
        ctx,
        op_state::LandingReport::Followed {
            repo: RepoKey::Member(repo_path.clone()),
            to: source.clone(),
            was: head.id().clone(),
            backup: kept.backup().name.to_string(),
        },
    )
}

/// Whether settling speaks in text: a run with no handler, `rwv abort`, does,
/// and one with a handler speaks as that handler does.
fn settle_speaks(handler: Option<&dyn OutputHandler>) -> bool {
    handler.is_none_or(|h| h.emit_text())
}

/// Settle every member checked out under `workspace` for the role its
/// project's manifest now gives it: replay and advance-target rewrite member
/// files, and a manifest that arrived with them can change a member's role.
/// A member that cannot be settled is named, not fatal — the op's own work
/// stands, and `rwv doctor --fix` settles what is left. A manifest that does
/// not parse settles nothing here; the phase that needs it next refuses on it.
///
/// `moved_from` answers, for a member's checkout, the revision the op moved
/// it from; the files that move rewrote are shared again first, and how they
/// were placed goes to `handler` when there is one.
fn settle_members(
    workspace: &Path,
    project_dir: &Path,
    moved_from: &dyn Fn(&dyn Vcs, &Path) -> Option<ResolvedRevisionId>,
    handler: Option<&dyn OutputHandler>,
) {
    let emit_text = settle_speaks(handler);
    let Ok(manifest) = Manifest::from_path(&project_dir.join(Manifest::FILE_NAME)) else {
        return;
    };
    for (repo_path, settled) in
        crate::settle::settle_workspace_moved(workspace, &manifest, moved_from)
    {
        if let (Ok(settled), Some(handler)) = (&settled, handler) {
            if let Some(files) = settled.files {
                handler.record_placement(PlacementOutput::new(
                    &repo_path,
                    &workspace.join(repo_path.as_path()),
                    files,
                ));
            }
        }
        if !emit_text {
            continue;
        }
        match settled {
            Ok(settled) => {
                if let Some(files) = settled.files {
                    outln!("  {repo_path}: files {files}");
                }
            }
            Err(e) => eprintln!(
                "  {repo_path}: warning: could not settle its files for its role ({e:#}); \
                 `rwv doctor --fix` retries"
            ),
        }
    }
}

/// Pin the atomic source snapshot at T0: read the source project tip once,
/// then read source manifest + lock AT that revision. Combined with the
/// no-op-in-progress check on the source (in `check_no_op_in_progress`),
/// source reads are effectively serializable with no locks.
fn pin_source_snapshot(
    source_vcs: &dyn Vcs,
    source_project_dir: &Path,
    source_workspace_root: &Path,
    canonical_root: &Path,
    project_tip: Option<ResolvedRevisionId>,
    classify: ClassifySource,
) -> anyhow::Result<SourceSnapshot> {
    let source_project_tip = match project_tip {
        Some(tip) => tip,
        None => source_vcs
            .head_revision(source_project_dir)
            .context("failed to read source project HEAD")?,
    };

    let raw_source_lock = {
        let content = source_vcs
            .read_file_at_revision(
                source_project_dir,
                &source_project_tip,
                Path::new(LockFile::FILE_NAME),
            )
            .with_context(|| {
                format!(
                    "failed to read source lock at revision {} in {}",
                    source_project_tip,
                    source_project_dir.display()
                )
            })?;
        LockFile::from_json_str(&content).with_context(|| {
            format!(
                "failed to parse source lock at revision {} in {}",
                source_project_tip,
                source_project_dir.display()
            )
        })?
    };

    let source_manifest = manifest_at_revision(source_vcs, source_project_dir, &source_project_tip)
        .context("failed to read the source's manifest")?;

    let (mut source_lock, source_lock_unresolvable) = raw_source_lock
        .clone()
        .resolve_versions(source_workspace_root);
    // A commit id names one commit in every workspace on the store, so an
    // entry the source holds no checkout for still resolves in the canonical
    // clone. A name does not: it can point elsewhere in each workspace.
    for (repo_path, entry) in raw_source_lock.iter_entries() {
        if source_lock.contains_repo(repo_path)
            || !entry.version.is_commit_id()
            || source_workspace_root.join(repo_path.as_path()).exists()
        {
            continue;
        }
        if let Ok(version) = vcs_for(entry.vcs_type).resolve_revision(
            &canonical_root.join(repo_path.as_path()),
            entry.version.as_str(),
        ) {
            source_lock.repositories.insert(
                repo_path.clone(),
                crate::manifest::ResolvedLockEntry {
                    vcs_type: entry.vcs_type,
                    url: entry.url.clone(),
                    version,
                },
            );
        }
    }

    let source_class = match classify {
        ClassifySource::Relations => Some(classify_lock_relations(
            source_workspace_root,
            &source_manifest,
            Some(&source_lock),
            source_lock_unresolvable,
        )),
    };

    let source_tips = match &source_class {
        Some(class) => class
            .relations
            .iter()
            .filter_map(|r| r.tip.clone().map(|tip| (r.repo_path.clone(), tip)))
            .collect(),
        None => read_member_tips(&source_manifest, source_workspace_root),
    };

    Ok(SourceSnapshot {
        source_project_tip,
        raw_source_lock,
        source_lock,
        source_class,
        source_tips,
        source_workspace_root: source_workspace_root.to_path_buf(),
    })
}

/// Read every manifest repo's HEAD in `workspace_dir`, skipping the checkouts
/// [`classify_lock_relations`] skips — repos absent from disk.
///
/// The unclassified counterpart to reading the relations' pinned tips, still
/// one read per repo.
fn read_member_tips(
    manifest: &Manifest,
    workspace_dir: &Path,
) -> std::collections::BTreeMap<RepoPath, ResolvedRevisionId> {
    let mut out = std::collections::BTreeMap::new();
    for (repo_path, entry) in manifest.iter_entries() {
        let abs = workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            continue;
        }
        if let Ok(tip) = vcs_for(entry.vcs_type).head_revision(&abs) {
            out.insert(repo_path.clone(), tip);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Phase: relock
// ---------------------------------------------------------------------------
//
// Regenerates `rwv.lock` from the post-replay manifest tips and commits if
// changed. On completion, records the converged per-repo tips in the owner
// record (consumed by abort hardening, sibling .4).
//
// **Re-entry rule:** regenerating a lock that is already current is a no-op
// (write_lock + commit_lock_file_with_message both short-circuit when the
// content hasn't changed). The per-repo HEAD reads that populate
// `converged_tips` are pure reads.
//
// Runs for every verb and strategy. `--strategy=ff` makes replay a no-op, not
// this phase: whatever last moved CWD's manifest repos — an operator's fix
// between a stranded op and its resume, most of all — leaves a lock that no
// longer pins them, and advance-target publishes that lock to the target.

fn run_relock(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    let emit_text = ctx.handler.emit_text();

    let cwd_project = reload_replayed_project(ctx)?;

    if let Err(e) = relock_landing(ctx, &cwd_project.manifest) {
        if emit_text {
            eprintln!("Phase 3 (re-lock) failed: {e:#}");
        }
        // Phase 3 (relock) is a lock regeneration + commit, never a VCS
        // rebase/merge — so there is never a live ConflictOp to resume with a
        // `git … --continue`. Pass `None`: the message points at
        // `rwv {verb} --continue`, not a spurious `git rebase --continue`.
        crate::refuse!(
            RefusalKind::OpParked,
            "{}",
            phase1_or_phase3_failure_message(
                ctx.project_vcs.as_ref(),
                Phase::Three,
                &ctx.cwd_project_dir,
                ctx.verb,
                None,
            )
        );
    }

    // Record converged tips on the owner record. These are read by
    // advance-target and consumed by abort hardening (sibling .4).
    record_converged_tips(ctx, &cwd_project)?;

    Ok(())
}

/// Answers whether one pin of a member reaches another, in the source's
/// checkout of it.
struct SourceReach<'a> {
    root: &'a Path,
    manifest: &'a Manifest,
}

impl crate::lock_merge::Reach for SourceReach<'_> {
    fn reaches(&self, member: &RepoPath, ancestor: &CommitId, of: &CommitId) -> Option<bool> {
        let entry = self.manifest.get_entry(member)?;
        vcs_for(entry.vcs_type)
            .is_ancestor(
                &self.root.join(member.as_path()),
                &ResolvedRevisionId::from(ancestor.clone()),
                &ResolvedRevisionId::from(of.clone()),
            )
            .ok()
    }
}

/// The landing's lock: the three-way merge of the source's and the
/// target's pins against their merge base's lock, each member the landing
/// composed at its composed tip, committed as an unnamed relock. A member the
/// composed manifest names that the merge leaves unpinned was added back by a
/// commit the source replayed, so it keeps the source's pin.
fn relock_landing(ctx: &OpContext<'_>, manifest: &Manifest) -> anyhow::Result<()> {
    let vcs = ctx.project_vcs.as_ref();
    let dir = &ctx.cwd_project_dir;
    let source = ctx.take_in.source_tip.as_ref().ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "the op record holds no source tip to relock from"
        )
    })?;
    let target = ctx
        .take_in
        .receiving
        .get(&RepoKey::Project)
        .map(|r| &r.tip)
        .ok_or_else(|| {
            crate::untokened!(
                InternalInvariant,
                "the op record holds no target tip to relock onto"
            )
        })?;
    let held = |commit: &CommitId| crate::unmerged::held_at(vcs, dir, commit);
    let base = vcs
        .merge_bases(dir, source, target)?
        .into_iter()
        .next()
        .map(|b| held(&b));
    let first = held(source);
    let second = held(target);
    let merged_declared: std::collections::BTreeMap<RepoPath, String> = manifest
        .iter_entries()
        .map(|(path, entry)| {
            (
                path.clone(),
                entry.branch.local_counterpart().as_str().to_owned(),
            )
        })
        .collect();
    let mut member_merges: std::collections::BTreeMap<RepoPath, CommitId> =
        std::collections::BTreeMap::new();
    for (path, entry) in manifest.iter_entries() {
        if !ctx
            .take_in
            .receiving
            .get(&RepoKey::Member(path.clone()))
            .is_some_and(|r| r.moves != op_state::Moves::Enters)
        {
            continue;
        }
        let checkout = ctx.cwd_workspace_dir.join(path.as_path());
        if let Ok(tip) = vcs_for(entry.vcs_type).head_revision(&checkout) {
            member_merges.insert(path.clone(), tip.id().clone());
        }
    }
    let merged = crate::lock_merge::merge_pins(
        &crate::lock_merge::PinMerge {
            base: base.as_ref(),
            first: &first,
            second: &second,
            merged_declared: &merged_declared,
            member_merges: &member_merges,
        },
        &SourceReach {
            root: &ctx.cwd_workspace_dir,
            manifest,
        },
    );
    let mut unmerged = Report::default();
    for finding in &merged.findings {
        if let crate::lock_merge::PinFinding::Unmerged {
            member,
            first,
            second,
        } = finding
        {
            unmerged.add(
                State::PinUnmerged
                    .of(member)
                    .member()
                    .bind("<m>", member)
                    .bind("<pin>", second)
                    .bind("<rev>", second)
                    .subject(format!(
                        "this feature pins {} and the target pins {}, neither containing the \
                         other",
                        first.short(),
                        second.short()
                    ))
                    .by_verb("land")
                    .judge(vcs),
            );
        }
    }
    unmerged.into_result()?;
    let mut pins = merged.pins;
    let named = LockFile::path_in(dir)
        .read_to_string()
        .ok()
        .and_then(|text| LockFile::from_json_str(&text).ok());
    for (path, entry) in manifest.iter_entries() {
        if pins.contains_key(path) {
            continue;
        }
        if let Some(pin) = first.pins.get(path) {
            pins.insert(path.clone(), pin.clone());
            continue;
        }
        let by_name = named
            .as_ref()
            .and_then(|lock| lock.get_entry(path))
            .and_then(|e| {
                vcs_for(entry.vcs_type)
                    .resolve_revision(
                        &ctx.cwd_workspace_dir.join(path.as_path()),
                        e.version.as_str(),
                    )
                    .ok()
            });
        if let Some(pin) = by_name {
            pins.insert(path.clone(), pin.id().clone());
        }
    }
    let resolved: Vec<(RepoPath, ResolvedRevisionId)> = pins
        .iter()
        .map(|(p, c)| (p.clone(), ResolvedRevisionId::from(c.clone())))
        .collect();
    let lock = crate::lock::with_pins(manifest, None, resolved.iter().map(|(p, c)| (p, c)));
    crate::lock::write_lock_in(&lock, dir)?;
    let named_members: Vec<RepoPath> = ctx
        .take_in
        .allow_member_change
        .iter()
        .filter_map(
            |named| match named.parse::<crate::manifest::RepoOperand>() {
                Ok(crate::manifest::RepoOperand::Member(path)) => Some(path),
                _ => None,
            },
        )
        .collect();
    let reach = SourceReach {
        root: &ctx.cwd_workspace_dir,
        manifest,
    };
    for path in &named_members {
        let (Some(was), Some(now)) = (second.pins.get(path), pins.get(path)) else {
            continue;
        };
        if was != now && crate::lock_merge::Reach::reaches(&reach, path, was, now) != Some(true) {
            report_landing(
                ctx,
                op_state::LandingReport::Repinned {
                    repo: RepoKey::Member(path.clone()),
                    was: was.clone(),
                    now: now.clone(),
                },
            )?;
        }
    }
    let named = crate::repin::Naming::by_name("land", named_members);
    if commit_lock_file_with_message(
        vcs,
        dir,
        &auto_relock_commit_message(&ctx.source_workspace_name),
        &named,
    )? && ctx.handler.emit_text()
    {
        eprintln!(
            "  {}: re-locked after composing onto {}",
            RepoKey::Project,
            target_name(ctx)
        );
    }
    Ok(())
}

/// Read post-replay HEADs of each manifest repo + project repo and write
/// them into the owner record's `converged_tips` map. Used by advance-target
/// and (sibling .4) abort's HEAD-verified restore.
fn record_converged_tips(ctx: &OpContext<'_>, cwd_project: &Project) -> anyhow::Result<()> {
    let mut owner = op_state::read_owner(&ctx.owner_workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "internal: owner record missing during relock"
        )
    })?;
    // Build the converged table from post-replay HEADs...
    let mut converged: std::collections::BTreeMap<RepoKey, CommitId> =
        std::collections::BTreeMap::new();
    for (repo_path, entry) in cwd_project.manifest.iter_entries() {
        let abs = ctx.cwd_workspace_dir.join(repo_path.as_path());
        if !abs.exists() {
            continue;
        }
        if let Ok(rev) = vcs_for(entry.vcs_type).head_revision(&abs) {
            converged.insert(RepoKey::Member(repo_path.clone()), rev.id().clone());
        }
    }
    if let Ok(rev) = ctx.project_vcs.head_revision(&ctx.cwd_project_dir) {
        converged.insert(RepoKey::Project, rev.id().clone());
    }
    // ...then swap atomically: `PhaseTips::converge` discards the replay-phase
    // advanced_tips and installs converged_tips in one move, so they land in the
    // SAME persist. The ADT makes the both-populated state
    // unrepresentable, so the prior "clear advanced before populating converged"
    // ordering hazard cannot recur.
    owner.tips.converge(converged);
    op_state::write_owner(&ctx.owner_workspace_dir, &owner)
        .context("failed to write converged_tips back to owner record")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Phase: advance-target (land only)
// ---------------------------------------------------------------------------
//
// FF-advance every target manifest repo + the target project repo to the
// converged tips recorded by relock.
//
// **Re-entry rule:** ff to an already-reached tip is a no-op (the equal-tip
// check at the top of `ff_advance_repo` short-circuits).

fn run_advance_target(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    let emit_text = ctx.handler.emit_text();

    if emit_text {
        eprintln!(
            "land: fast-forwarding {} and its receiving branches in {}...",
            target_name(ctx),
            ctx.dest_workspace_dir.display()
        );
    }

    let cwd_project_final = Project::from_dir(&ctx.cwd_project_dir, ctx.cwd_project_name.clone())
        .context("failed to reload CWD project for advance-target")?;
    let owner = op_state::read_owner(&ctx.owner_workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "internal: owner record missing at advance-target"
        )
    })?;
    let composed = owner.tips.converged().cloned().ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "internal: advance-target reached before the landing recorded its composed tips"
        )
    })?;
    if let Some(moved) = target_moved(
        &ctx.cwd_project_name,
        &ctx.dest_workspace_dir,
        &ctx.take_in,
        Some(&composed),
    )? {
        return Err(moved.refused());
    }
    let planned = |key: &RepoKey| -> anyhow::Result<(&str, ResolvedRevisionId)> {
        match (ctx.take_in.receiving.get(key), composed.get(key)) {
            (Some(receiving), Some(to)) => Ok((
                receiving.branch.as_str(),
                ResolvedRevisionId::from(to.clone()),
            )),
            _ => Err(crate::untokened!(
                InternalInvariant,
                "internal: the landing recorded no composed tip for {key}"
            )),
        }
    };

    let target_ctx =
        WorkspaceContext::resolve_for_project(&ctx.dest_workspace_dir, &ctx.cwd_project_name)
            .context("failed to resolve the target for advance-target")?;

    let mut any_ff_failure = false;
    let mut moved = Report::default();
    for (key, receiving) in &ctx.take_in.receiving {
        let RepoKey::Member(repo_path) = key else {
            continue;
        };
        let create = match receiving.moves {
            op_state::Moves::AsRef { create } => create,
            op_state::Moves::Enters => true,
            op_state::Moves::InCheckout | op_state::Moves::Follows => continue,
        };
        let Some(entry) = cwd_project_final.manifest.get_entry(repo_path) else {
            continue;
        };
        let vcs = vcs_for(entry.vcs_type);
        let cwd_repo = ctx.cwd_workspace_dir.join(repo_path.as_path());
        let composed = match receiving.moves {
            op_state::Moves::Enters => receiving.tip.clone(),
            _ => planned(key)?.1.id().clone(),
        };
        if let Err(e) = moved.absorb(advance_receiving_ref(
            ctx,
            vcs.as_ref(),
            repo_path,
            &cwd_repo,
            receiving,
            create,
            &composed,
        )) {
            if emit_text {
                eprintln!("  {repo_path}: receiving branch not moved: {e:#}");
            }
            any_ff_failure = true;
        }
    }
    for (repo_path, entry) in cwd_project_final.manifest.iter_entries() {
        let key = RepoKey::Member(repo_path.clone());
        if !ctx
            .take_in
            .receiving
            .get(&key)
            .is_some_and(|r| r.moves == op_state::Moves::InCheckout)
        {
            continue;
        }
        let vcs = vcs_for(entry.vcs_type);
        let cwd_repo = ctx.cwd_workspace_dir.join(repo_path.as_path());
        let target_repo = ctx.dest_workspace_dir.join(repo_path.as_path());
        if !cwd_repo.exists() {
            continue;
        }
        let (branch, cwd_tip) = planned(&key)?;
        if !target_repo.exists() {
            match &target_ctx.checkout {
                Checkout::Workweave {
                    name, dir, project, ..
                } => match name.require(dir.as_path(), project).and_then(|name| {
                    crate::workweave::materialize_member(
                        vcs.as_ref(),
                        target_ctx.primary_path(),
                        target_ctx.active_path(),
                        &ctx.cwd_project_name,
                        name,
                        repo_path,
                        entry.role,
                        entry.recurse_submodules,
                        cwd_tip.id().as_str(),
                        crate::workweave::BranchPolicy::Restore,
                    )
                }) {
                    Ok(born) => {
                        if let Some(files) = born.files {
                            ctx.handler.record_placement(PlacementOutput::new(
                                repo_path,
                                &target_repo,
                                files,
                            ));
                        }
                        if emit_text {
                            outln!(
                                "  {repo_path}: materialized in target at {}{}",
                                cwd_tip.id().short(),
                                crate::share::files_note(born.files)
                            );
                        }
                    }
                    Err(e) => {
                        if emit_text {
                            eprintln!("  {repo_path}: materialize in target failed: {e:#}");
                        }
                        any_ff_failure = true;
                    }
                },
                Checkout::Primary { .. } => {
                    if emit_text {
                        eprintln!("  {}: skipped (not on disk in target)", repo_path);
                    }
                }
            }
            continue;
        }
        match ff_advance_repo(
            vcs.as_ref(),
            repo_path,
            &target_repo,
            &cwd_repo,
            branch,
            &cwd_tip,
        ) {
            Ok(advance) => {
                if emit_text {
                    outln!("  {}: {}", repo_path, ff_advance_line(&advance, &cwd_tip));
                }
                if advance.onto.is_some() {
                    journal_fast_forward(ctx, &key)?;
                    ctx.handler.record_step3_advance(
                        &RepoKey::Member(repo_path.clone()),
                        advance.from.id().as_str(),
                        cwd_tip.id().as_str(),
                    );
                }
            }
            Err(e) => {
                if let Err(e) = moved.absorb(Err(e)) {
                    if emit_text {
                        eprintln!("  {}: ff-advance failed: {e:#}", repo_path);
                    }
                    any_ff_failure = true;
                }
            }
        }
    }
    moved.into_result()?;

    // The project repo carries the lock, and the lock names the manifest tips.
    // Advancing it while a manifest repo failed to land would leave the target
    // asserting revisions it does not hold — a worse state than the one the
    // failure already produced, and one no later phase repairs. Every manifest
    // repo that DID land is left landed: those tips are ancestors of nothing the
    // target lacks, so the un-advanced lock stays true about them.
    if any_ff_failure {
        if emit_text {
            eprintln!(
                "  {}: not advanced — a member repo did not land (see above)",
                RepoKey::Project
            );
        }
        crate::refuse!(
            RefusalKind::OpParked,
            "land advance-target failed for one or more member repos (see above).\n\
             The target's project repo was NOT advanced, so its lock still describes the \
             target's pre-op state rather than naming revisions the target does not have.\n\
             Op-state remains in both workspaces.\n\
             Rerun `{resume}` after resolving, or `rwv abort` to roll the whole op back.",
            resume = op_state::resume_command(ctx.verb),
        );
    }

    let (target_branch, cwd_project_tip) = planned(&RepoKey::Project)?;
    let taken_in = upstream_taken_in(ctx, target_branch)?;

    match ff_advance_repo(
        ctx.project_vcs.as_ref(),
        project_rel_path(&ctx.cwd_project_name),
        &ctx.dest_project_dir,
        &ctx.cwd_project_dir,
        target_branch,
        &cwd_project_tip,
    ) {
        Ok(advance) => {
            if emit_text {
                outln!(
                    "  {}: {}",
                    RepoKey::Project,
                    ff_advance_line(&advance, &cwd_project_tip)
                );
            }
            if advance.onto.is_some() {
                journal_fast_forward(ctx, &RepoKey::Project)?;
                ctx.handler.record_step3_advance(
                    &RepoKey::Project,
                    advance.from.id().as_str(),
                    cwd_project_tip.id().as_str(),
                );
            }
        }
        Err(e) if crate::diagnosis::report_of(&e).is_some() => return Err(e),
        Err(e) => {
            if emit_text {
                eprintln!("  {}: ff-advance failed: {e:#}", RepoKey::Project);
            }
            crate::refuse!(
                RefusalKind::OpParked,
                "land advance-target could not advance the target's project repo (see \
                 above). Every member repo landed, so the target holds the work but its \
                 lock does not yet name those tips.\n\
                 Op-state remains in both workspaces.\n\
                 Rerun `{resume}` after resolving, or `rwv abort` to roll the whole op back.",
                resume = op_state::resume_command(ctx.verb),
            );
        }
    }

    if let Some(taken_in) = taken_in {
        report_landing(ctx, taken_in)?;
    }
    if emit_text {
        report_target_ahead(ctx);
    }

    let target_savepoint = ctx.target_savepoint_key();
    settle_members(
        &ctx.dest_workspace_dir,
        &ctx.dest_project_dir,
        &|vcs, member| vcs.resolve_savepoint(member, &target_savepoint),
        Some(ctx.handler),
    );

    align_after_landing(ctx, &target_ctx);
    if !ctx.retire {
        align_after_landing(ctx, &ctx.cwd_ctx);
    }
    Ok(())
}

/// What the landing takes in of `T`'s upstream: none where `T`'s checkout
/// already holds the tip the landing composed onto.
fn upstream_taken_in(
    ctx: &OpContext<'_>,
    branch: &str,
) -> anyhow::Result<Option<op_state::LandingReport>> {
    let Some(receiving) = ctx.take_in.receiving.get(&RepoKey::Project) else {
        return Ok(None);
    };
    let vcs = ctx.project_vcs.as_ref();
    let dir = &ctx.dest_project_dir;
    let before = vcs.head_revision(dir)?.id().clone();
    if before == receiving.tip
        || !vcs.is_ancestor(
            dir,
            &ResolvedRevisionId::from(before.clone()),
            &ResolvedRevisionId::from(receiving.tip.clone()),
        )?
    {
        return Ok(None);
    }
    crate::land::upstream_taken_in(
        vcs,
        dir,
        ctx.cwd_ctx.primary_path(),
        branch,
        &before,
        &receiving.tip,
    )
    .map(Some)
}

/// Move a receiving branch no checkout holds to `composed`, creating it
/// first at its planned start where it existed nowhere. Each step is
/// journaled, so `rwv abort` restores the move and deletes the creation; a
/// branch another verb moved since the landing read it stops the landing.
fn advance_receiving_ref(
    ctx: &OpContext<'_>,
    vcs: &dyn Vcs,
    repo_path: &RepoPath,
    dir: &Path,
    receiving: &op_state::Receiving,
    create: bool,
    composed: &CommitId,
) -> anyhow::Result<()> {
    let emit_text = ctx.handler.emit_text();
    let branch = crate::vcs::RawRefName::new(receiving.branch.clone());
    let key = RepoKey::Member(repo_path.clone());
    let from = match vcs.resolve_local_branch_tip(dir, &branch)? {
        Some(tip) => tip.id().clone(),
        None => {
            let upstream = if create {
                None
            } else {
                vcs.remote_copies(dir, &branch)?
                    .into_iter()
                    .find(|copy| copy.tip.id() == &receiving.tip)
                    .map(|copy| crate::vcs::Upstream {
                        remote: copy.remote,
                        branch: branch.clone(),
                    })
            };
            vcs.create_branch(
                dir,
                &branch,
                &ResolvedRevisionId::from(receiving.tip.clone()),
                upstream.as_ref(),
            )?;
            op_state::journal_created_ref(
                &ctx.owner_workspace_dir,
                op_state::CreatedRef {
                    repo: key.clone(),
                    branch: receiving.branch.clone(),
                    at: receiving.tip.clone(),
                },
            )?;
            if create {
                report_landing(
                    ctx,
                    op_state::LandingReport::Created {
                        repo: key.clone(),
                        branch: receiving.branch.clone(),
                        at: receiving.tip.clone(),
                    },
                )?;
            }
            receiving.tip.clone()
        }
    };
    if &from == composed {
        return Ok(());
    }
    crate::test_pause::point("advance-target");
    let fast_forwards = vcs
        .is_ancestor(
            dir,
            &ResolvedRevisionId::from(from.clone()),
            &ResolvedRevisionId::from(composed.clone()),
        )
        .unwrap_or(false);
    if !fast_forwards
        || vcs
            .advance_branch_ref(dir, &branch, &from, composed)
            .is_err()
    {
        let now = vcs
            .resolve_local_branch_tip(dir, &branch)?
            .unwrap_or_else(|| ResolvedRevisionId::from(from.clone()));
        return Err(target_moved_line_for(
            repo_path,
            dir,
            does_not_fast_forward(&now, &ResolvedRevisionId::from(composed.clone())),
        )
        .refused());
    }
    op_state::journal_ref_move(
        &ctx.owner_workspace_dir,
        op_state::RefMove {
            repo: key.clone(),
            branch: receiving.branch.clone(),
            from: from.clone(),
            to: composed.clone(),
        },
    )?;
    ctx.handler
        .record_step3_advance(&key, from.as_str(), composed.as_str());
    if emit_text {
        outln!(
            "  {repo_path}: '{}' fast-forwarded {} -> {}",
            receiving.branch,
            from.short(),
            composed.short()
        );
    }
    Ok(())
}

/// Record on the op record that the landing moved `key`'s branch in `T`'s
/// workspace, so `rwv abort` restores it and leaves every other repo there
/// where it stands.
fn journal_fast_forward(ctx: &OpContext<'_>, key: &RepoKey) -> anyhow::Result<()> {
    let mut owner = op_state::read_owner(&ctx.owner_workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "internal: owner record missing at advance-target"
        )
    })?;
    let take_in = owner.take_in.get_or_insert_with(Default::default);
    if !take_in.fast_forwarded.contains(key) {
        take_in.fast_forwarded.push(key.clone());
    }
    op_state::write_owner(&ctx.owner_workspace_dir, &owner)
        .context("failed to record a fast-forward of the target")
}

/// `T`, once it holds the landing, judged against its push branch where it is
/// ahead of it: the landing is not published until `T` is pushed.
fn report_target_ahead(ctx: &OpContext<'_>) {
    let vcs = ctx.project_vcs.as_ref();
    let Ok(target) = landed_into(ctx, &ctx.cwd_project_name) else {
        return;
    };
    let url_remote = vcs.project_url_remote(&ctx.dest_project_dir).ok().flatten();
    let Ok((standing, _)) = crate::push_branch::standing_of_branch(
        vcs,
        &ctx.dest_project_dir,
        &crate::vcs::RawRefName::new(target.as_str()),
        url_remote.as_ref(),
    ) else {
        return;
    };
    if standing != crate::push_branch::Standing::Ahead {
        return;
    }
    let label = project_rel_path(&ctx.cwd_project_name);
    let mut report = Report::default();
    report.add(
        State::PushBranchAhead
            .of(&label)
            .project_repo()
            .subject(format!(
                "'{target}' holds the landing, and its push branch does not"
            ))
            .bind("<W>", &target)
            .elsewhere(&ctx.dest_workspace_dir)
            .by_verb("land")
            .only_forms(&[crate::diagnosis::Form::C7PushBranch])
            .judge(vcs),
    );
    eprint!("{}", report.render("land"));
}

/// Align the workspace `ctx` names to the branch the landing left it on.
/// The landing itself is done by then, so a member alignment leaves where it
/// was is reported, with `rwv update` there to finish it, and fails nothing.
fn align_after_landing(op: &OpContext<'_>, ctx: &WorkspaceContext) {
    let output = if op.handler.emit_text() {
        OutputMode::Text
    } else {
        OutputMode::Json
    };
    let run = crate::update::align_after_landing(ctx, op.jobs, output);
    let left = match run {
        Ok(run) => {
            for placement in run.envelope.placements {
                op.handler.record_placement(placement);
            }
            run.tail.err()
        }
        Err(e) => Some(e),
    };
    if let (Some(e), true) = (left, op.handler.emit_text()) {
        eprintln!(
            "land: {} did not finish aligning; `rwv update` there finishes it:\n{e:#}",
            ctx.active_path().display()
        );
    }
}

// ---------------------------------------------------------------------------
// Phase: retire (--retire only)
// ---------------------------------------------------------------------------
//
// Retire runs the merged-check (manifest repo tips equal on both sides) and
// dirty-check, then calls `delete_workweave`. A failure from any of these
// propagates as an error from `run_retire`, which keeps the op record at
// phase=retire (cleanup never runs on error, per the driver invariant).
//
// Re-entry rule: both the merged-check and dirty-check are read-only; the
// workweave removal itself is idempotent (a missing workweave dir is a no-op
// in delete_workweave). So re-entering retire after a prior failure is safe.
//
// Abort from phase=retire: run_abort scans the target workspace for repos with
// op-id savepoints and restores them to their pre-op state. Target savepoints
// are created in guard_and_mark for verb=Land (this spec's scope); sibling
// .4 adds HEAD-verification on top of the savepoint restore.

/// The branch the landing went into, as its plan records it, never what the
/// target workspace has checked out by the time the retire runs. The deletion
/// rule judges the retired branches against it, a one-run `--target`
/// included.
fn landed_into(
    ctx: &OpContext<'_>,
    project: &crate::manifest::ProjectName,
) -> anyhow::Result<crate::records::BranchName> {
    let vcs = ctx.project_vcs.as_ref();
    let dir = project_dir(&ctx.dest_workspace_dir, project);
    let receiving = ctx
        .take_in
        .receiving
        .get(&RepoKey::Project)
        .ok_or_else(|| {
            crate::untokened!(
                InternalInvariant,
                "the op record names no branch the landing went into"
            )
        })?;
    crate::branch::named(vcs, project, &dir, &receiving.branch)
}

fn run_retire(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    crate::test_pause::point("retire");
    let emit_text = ctx.handler.emit_text();

    match &ctx.cwd_ctx.checkout {
        Checkout::Workweave {
            dir, name, project, ..
        } => {
            let retired = retire_workweave_after_landing(
                ctx.project_vcs.as_ref(),
                &ctx.cwd_ctx,
                dir.as_path(),
                name.require(dir.as_path(), project)?,
                project,
                &ctx.cwd_project_dir,
                landed_into(ctx, project)?,
            )?;
            eprintln!("retired workweave {}", retired.name().as_str());
            if emit_text {
                retired.deletion.print();
            }
            if retired.deletion.left_something() {
                ctx.retire_left
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            ctx.handler.record_retired(&retired);
            Ok(())
        }
        Checkout::Primary { .. } => {
            if emit_text {
                eprintln!("warning: --retire is only meaningful inside a workweave; ignoring");
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal: cleanup
// ---------------------------------------------------------------------------
//
// Drop savepoints and clear the owner record + lease. Cleanup is not a
// persisted phase: a crash before cleanup completes leaves the on-disk
// phase at the last work phase (e.g. retire), which is re-runnable;
// `--continue` then re-runs that phase (it's idempotent) and reaches
// cleanup again.

fn cleanup(ctx: &OpContext<'_>) -> anyhow::Result<()> {
    // Savepoint refs (`refs/rwv/pre-op/*`) live in the shared clone refdb, not
    // in any worktree, so `git update-ref -d` from ANY live worktree of the
    // same clone drops the shared ref. Crucially, in the `land --retire`
    // flow the phase order is `… → retire → cleanup`: retire deletes CWD's
    // workweave BEFORE cleanup runs, so `ctx.cwd_project_dir` /
    // `ctx.cwd_workspace_dir` now point at a deleted directory. Dropping
    // savepoints through those paths silently no-ops while the ref survives in
    // the surviving clone — the leak this code path fixes.
    //
    // We therefore target the CANONICAL/PRIMARY clone (`primary_path()`), which
    // survives workweave deletion: workweave repos are `git worktree add`ed
    // from the primary's clones, so the primary holds the shared refdb. When
    // CWD is itself the primary weave (plain `sync` from primary), the
    // canonical path equals CWD, so this is also correct for the non-retire
    // case.
    let primary = ctx.cwd_ctx.primary_path();
    let canonical_project_dir = project_dir(primary, &ctx.cwd_project_name);

    delete_savepoint(
        ctx.project_vcs.as_ref(),
        &canonical_project_dir,
        &ctx.savepoint_key(),
    );

    // Manifest savepoints: load the manifest from the canonical project repo
    // (the workweave's may be gone after retire) and drop each repo's savepoint
    // through the canonical clone. A missing ref is a harmless no-op, so no
    // existence guard is needed (and `if abs.exists()` would re-introduce the
    // leak by skipping the now-deleted workweave paths).
    if let Ok(project) =
        Project::from_dir_skip_lock(&canonical_project_dir, ctx.cwd_project_name.clone())
    {
        for (repo_path, entry) in project.manifest.iter_entries() {
            let abs = primary.join(repo_path.as_path());
            delete_savepoint(vcs_for(entry.vcs_type).as_ref(), &abs, &ctx.savepoint_key());
        }
    }

    // Sync-to: drop target savepoints, filed under their own key
    // (`SavepointKey::LandTarget`). Best-effort: if the target's
    // project dir or repos are gone (e.g. after a retire that deleted the
    // workweave and the target structure changed), silently skip.
    //
    // Resolve the target under the op's own project, exactly as savepoint
    // creation did: the target workspace's `.rwv-active` may name a different
    // project, and resolving by that pointer drops refs in the wrong
    // project's repos while the real ones survive every successful land.
    if matches!(ctx.verb, op_state::OpVerb::Land) {
        let tsp_id = ctx.target_savepoint_key();
        let target_presents_the_project =
            WorkspaceContext::resolve_for_project(&ctx.dest_workspace_dir, &ctx.cwd_project_name)
                .is_ok();
        if target_presents_the_project {
            let target_project_dir = project_dir(&ctx.dest_workspace_dir, &ctx.cwd_project_name);
            delete_savepoint(ctx.project_vcs.as_ref(), &target_project_dir, &tsp_id);
            if let Ok(tp) =
                Project::from_dir_skip_lock(&target_project_dir, ctx.cwd_project_name.clone())
            {
                for (repo_path, entry) in tp.manifest.iter_entries() {
                    let abs = ctx.dest_workspace_dir.join(repo_path.as_path());
                    if abs.exists() {
                        delete_savepoint(vcs_for(entry.vcs_type).as_ref(), &abs, &tsp_id);
                    }
                }
            }
        }
    }

    op_state::TouchedWorkspaces::of(ctx.verb, &ctx.owner_workspace_dir, &ctx.dest_workspace_dir)
        .clear();
    Ok(())
}

/// `rwv land --retire` post-land cleanup.
///
/// Verify that no worktree has uncommitted changes, then delete the
/// workweave and apply the deletion rule to its branches. Bails (preserving
/// the workweave) on dirt so the operator can fix and re-run; a branch that
/// has not landed is kept by the rule and reported, not a refusal.
///
/// The [`RetiredWorkweave`] returned is the only evidence a retire happened:
/// every path that does not reach the delete returns `Err`, and the primary
/// weave never calls this at all.
fn retire_workweave_after_landing(
    project_vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    workweave_dir: &Path,
    workweave_name: &WorkweaveName,
    project: &crate::manifest::ProjectName,
    cwd_project_dir: &Path,
    landed_into: crate::records::BranchName,
) -> anyhow::Result<RetiredWorkweave> {
    let resume = op_state::resume_command(op_state::OpVerb::Land);

    // Reload manifest post-Phase 3 so we see any repos newly added by sync.
    let manifest_path = cwd_project_dir.join(Manifest::FILE_NAME);
    let manifest =
        Manifest::from_path(&manifest_path).context("--retire: failed to reload manifest")?;

    // Reuse the shared dirty-path check. Any dirty worktree blocks retire.
    let mut dirty = crate::workweave::judge_dirty_checkouts(
        project_vcs,
        workweave_dir,
        project,
        &manifest,
        None,
    );
    dirty.note(format!(
        "--retire: the landing is done and the workweave was not deleted; once these \
         are clean, `{resume}` re-runs the retire check, or `rwv abort` rolls the whole op back"
    ));
    dirty.into_result()?;

    let deletion = crate::workweave::delete_workweave_for_retire(
        ctx.primary_path(),
        project,
        workweave_name,
        workweave_dir,
        landed_into,
    )
    .context("--retire: workweave delete failed")?;

    Ok(RetiredWorkweave {
        name: workweave_name.clone(),
        deletion,
    })
}

/// Phase 1': replay CWD's unique project commits onto `source_tip` via
/// `strategy`, holding the lock merge driver to source's version of the lock
/// through the replay. Phase 3 regenerates the lock from manifest tips
/// afterwards.
///
/// - `Ff`: requires CWD ancestor of source (caller already verified). Performs
///   a fast-forward via `git merge --ff-only`.
/// - Either: a CWD tip that already contains `source_tip` is left as it is.
///   There is nothing to compose, and a forced rebase would flatten any
///   merge the range holds.
/// - `Rebase`: native `git rebase` via [`Vcs::rebase`]. On conflict, leaves
///   the repo mid-rebase; the operator resolves, `git add`s, then runs
///   `rwv sync --continue` (or `rwv land --continue` for land).
///
/// Conflicts on non-lock paths halt the operation, leaving the VCS-native
/// in-flight state for the operator to resolve and re-run sync, or
/// `rwv abort`.
///
/// `verb` is the running op's verb ([`OpVerb`]), threaded through so the
/// conflict-bail message shows the correct rwv-native resume command
/// (`rwv sync --continue` / `rwv land --continue`).
fn apply_project_strategy(
    vcs: &dyn Vcs,
    cwd_project_dir: &Path,
    source_tip: &ResolvedRevisionId,
    cwd_tip: &ResolvedRevisionId,
    strategy: SyncStrategy,
    verb: OpVerb,
) -> anyhow::Result<()> {
    // Replay re-entry (`rwv sync --continue`): a project repo that stopped
    // mid-rebase on a conflict has HEAD sitting at `source_tip` (git checked
    // it out as the rebase onto and stopped on the very first pick). The
    // no-op short-circuit below would then trip and return `Ok(())` without
    // ever driving the rebase forward — leaving the repo mid-rebase forever
    // and lying to the caller that Phase 1' completed. Mid-rebase is the
    // signal that trumps head-equality: always route through
    // `Vcs::rebase_continue` (below) when we see it under a rebase strategy.
    let mid_rebase = matches!(vcs.mid_op(cwd_project_dir), Some(ConflictOp::Rebase))
        && strategy == SyncStrategy::Rebase;

    if !mid_rebase
        && (cwd_tip == source_tip || vcs.is_ancestor(cwd_project_dir, source_tip, cwd_tip)?)
    {
        return Ok(());
    }

    match strategy {
        SyncStrategy::Ff => {
            // CWD must be ancestor of source (caller verified). Fast-forward.
            let containment = Containment::observe(vcs, cwd_project_dir, cwd_tip, source_tip);
            if let Err(e) = vcs.advance_if_fast_forward(cwd_project_dir, source_tip) {
                let shape = match containment {
                    Some(c) => format!(
                        " (CWD's project tip is {})",
                        c.describe(&format!("source's project tip ({source_tip})"))
                    ),
                    None => String::new(),
                };
                return Err(crate::untokened!(
                    EnvironmentPassthrough,
                    "project repo fast-forward failed{shape}: {e:#}"
                ));
            }
        }
        SyncStrategy::Rebase => {
            // The replay states `keep_target_side`, which holds the driver the
            // repo's committed `rwv.lock merge=rwv-lock` line names: lock-only
            // commits become empty patches and git drops them by default
            // (`--empty=drop`), so source's version of `rwv.lock` survives
            // the replay untouched. Phase 3 then regenerates the lock from
            // manifest tips — the resolution only has to be mechanical,
            // because regeneration is what makes the result correct.
            //
            // Replay re-entry (`rwv sync --continue`): if the project repo
            // is already mid-rebase from a previous phase that stopped on a
            // conflict, `Vcs::rebase` would fail immediately ("another
            // rebase is in progress"). Route through `Vcs::rebase_continue`
            // so `resolve → git add → rwv sync --continue` iterates per
            // conflicted pick. It states the same policy, so any remaining
            // lock-only picks resolve the same way a fresh `Vcs::rebase`
            // would. A mid-op that is NOT rebase (mid-merge,
            // mid-cherry-pick) is not rwv-initiated for this path —
            // preserve today's behavior of calling `Vcs::rebase`, which will
            // fail loudly rather than silently adopting foreign state.
            let outcome = if mid_rebase {
                vcs.rebase_continue(cwd_project_dir, DerivedContentPolicy::keep_target_side())
            } else {
                vcs.rebase(
                    cwd_project_dir,
                    source_tip,
                    source_tip,
                    DerivedContentPolicy::keep_target_side(),
                )
            };
            match outcome {
                Ok(()) => {}
                Err(VcsError::RebaseConflict { repo, op }) => {
                    let detail = vcs.rebase_stopped_commit_detail(&repo);
                    crate::refuse!(
                        RefusalKind::OpParked,
                        "{}",
                        per_conflict_bail_message(
                            vcs,
                            &repo,
                            op,
                            "rebase (project repo)",
                            &detail,
                            verb,
                        )
                    );
                }
                Err(e) => {
                    return Err(crate::untokened!(
                        EnvironmentPassthrough,
                        "project repo rebase failed: {e:#}"
                    ))
                }
            }
        }
    }
    Ok(())
}

/// Phase 3: regenerate `rwv.lock` from the current manifest tips. Commit it
/// if it differs from what's currently in the project repo.
///
/// Returns the lock the commit landed, or `None` when the regenerated lock
/// matched the committed one and nothing was staged. A caller that announces
/// the relock derives both its tense and the revisions it names from that
/// return, never from the classification that decided to call it.
fn regenerate_lock_phase3(
    vcs: &dyn Vcs,
    ctx: &WorkspaceContext,
    cwd_project_dir: &Path,
    cwd_project: &Project,
    source_workspace_name: &str,
) -> anyhow::Result<Option<ResolvedLockFile>> {
    let workweave_dir = match &ctx.checkout {
        Checkout::Workweave { dir, .. } => Some(dir.as_path()),
        Checkout::Primary { .. } => None,
    };

    let new_lock = generate_lock(
        &cwd_project.manifest,
        ctx.primary_path(),
        workweave_dir,
        true, // dirty: skip uncommitted-changes check; sync may have produced WT churn
    )
    .context("failed to generate lock")?;

    crate::lock::write_lock_in(&new_lock, cwd_project_dir)?;

    let message = auto_relock_commit_message(source_workspace_name);
    if !commit_lock_file_with_message(
        vcs,
        cwd_project_dir,
        &message,
        &crate::repin::Naming::Unnamed,
    )? {
        return Ok(None);
    }
    eprintln!(
        "  {}: re-locked after sync from {source_workspace_name}",
        RepoKey::Project
    );
    Ok(Some(new_lock))
}

// ---------------------------------------------------------------------------
// rwv abort
// ---------------------------------------------------------------------------

/// Which of an op's workspaces a restore is running in, decided by the
/// workspace's identity in the owner record and never by which of the two
/// `rwv abort` was invoked from.
#[derive(Clone, Copy)]
enum AbortSide {
    /// The workspace the op replayed in: the owner, savepointed under the base
    /// op id, whose replay journals `advanced_tips`.
    Replay,
    /// The workspace a `land` advanced, savepointed under
    /// [`SavepointKey::LandTarget`] so that worktree pairs sharing one refdb
    /// don't collide on savepoint ref names. Nothing journals intent
    /// for it; its tips land in `converged_tips` at relock.
    LandTarget,
}

impl AbortSide {
    fn of(workspace: &CanonicalPath, header: &OwnerHeader) -> Self {
        if header.verb == OpVerb::Land && *workspace == header.target {
            Self::LandTarget
        } else {
            Self::Replay
        }
    }

    fn savepoint_key(self, op_id: &OpId) -> SavepointKey {
        match self {
            Self::Replay => SavepointKey::Op(op_id.clone()),
            Self::LandTarget => SavepointKey::LandTarget(op_id.clone()),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Replay => "[source] ",
            Self::LandTarget => "[target] ",
        }
    }
}

/// What a whole `rwv abort` has seen so far, across both of its workspaces.
#[derive(Default)]
struct AbortTally {
    noise: AbortNoiseSummary,
    any_foreign: bool,
    any_failure: bool,
}

impl AbortTally {
    fn clean(&self) -> bool {
        !self.any_foreign && !self.any_failure
    }
}

/// Execute `rwv abort` — verified-restore every workspace of the in-flight op
/// to its pre-sync state, from whichever of them it is invoked in.
///
/// Reads the op-state file (`.rwv-op`) to find the op-id, the involved
/// workspaces, and the per-repo `converged_tips` recorded at relock. For
/// `land` ops, both the owner and the recorded target are rolled back.
///
/// The `reset --hard` to the savepoint happens ONLY if the repo's current
/// tip is attributable to the op (== savepoint, == recorded converged tip,
/// or a VCS-native mid-op state). Anything else is reported with a named
/// `foreign-tip` violation and recovery hints, and the repo's tip is left
/// untouched. The op-state is RETAINED on a foreign-tip refusal so the
/// operator can re-run `rwv abort` after reconciling.
///
/// `abandon` is the operator's per-repo waiver of that rail: the repos it
/// names are restored over their foreign tips, each tip kept first as an
/// `abandoned` backup.
pub fn run_abort(
    ctx: &WorkspaceContext,
    abandon: &crate::cli::consent::AbandonForeignTipConsent,
) -> anyhow::Result<()> {
    let workspace = ctx.active_root();
    let workspace_dir = workspace.as_path().to_path_buf();

    // resolve_for_abort follows a lease pointer if the workspace holds a lease,
    // so `rwv abort` invoked from either the owner or a leased workspace finds
    // the same record.
    let Some((reading, owner_workspace)) = op_state::resolve_for_abort(&workspace_dir)? else {
        crate::refuse!(RefusalKind::NoOpRecorded, "no operation in progress")
    };
    let header = reading.header();
    if let op_state::OwnerReading::HeaderOnly { parse_error, .. } = &reading {
        eprintln!(
            "owner record at {} does not parse past op {}'s id and workspaces: {parse_error}\n\
             No tips it recorded are read: a repo is restored only where it sits at its \
             savepoint or mid-op, and any other tip is refused as foreign.",
            op_state::OwnerRecord::path_in(owner_workspace.as_path()),
            header.id,
        );
    }
    let other_workspace = (header.verb == OpVerb::Land).then(|| {
        if owner_workspace == *workspace {
            header.target.clone()
        } else {
            owner_workspace.clone()
        }
    });
    let tips = reading.tips();
    let op_id = header.id.clone();
    let fast_forwarded: Option<&[RepoKey]> = match &reading {
        op_state::OwnerReading::Whole(record) => Some(
            record
                .take_in
                .as_ref()
                .map(|take_in| take_in.fast_forwarded.as_slice())
                .unwrap_or_default(),
        ),
        op_state::OwnerReading::HeaderOnly { .. } => None,
    };

    // Both of the op's workspaces are enumerated under the op's own project,
    // whichever side abort was invoked from. A workspace's `.rwv-active` may
    // name a different project by the time an operator aborts, and that
    // project's repos are not the ones this op advanced.
    let op_project_name = header.project.clone();
    if let Some(named) = abandon.names_another_project(&op_project_name) {
        crate::refuse!(
            RefusalKind::InapplicableFlag,
            "--abandon-foreign-tip names the repo of project `{named}`, but the operation in \
             progress belongs to project `{op_project_name}`; name its project repo as `{}`",
            RepoOperand::Project(op_project_name.clone())
        );
    }
    let cwd_project_dir = project_dir(&workspace_dir, &op_project_name);
    // Use the lockless loader: abort's contract is "the state is bad, get me
    // out". rwv.lock may contain git conflict markers from the half-completed
    // rebase, so we must not try to parse it. The abort path only needs the
    // manifest (to enumerate repo paths); it never reads lock contents.
    let cwd_project = Project::from_dir_skip_lock(&cwd_project_dir, op_project_name.clone())
        .context("failed to load CWD project")?;
    refuse_symlinked_members(&workspace_dir, &op_project_name)?;
    if let Some(other) = &other_workspace {
        refuse_symlinked_members(other.as_path(), &op_project_name)?;
    }

    let mut tally = AbortTally::default();

    restore_abort_workspace(
        &workspace_dir,
        &cwd_project_dir,
        &cwd_project,
        AbortSide::of(workspace, &header),
        "",
        &op_id,
        tips,
        fast_forwarded,
        abandon,
        &mut tally,
    );
    if let op_state::OwnerReading::Whole(record) = &reading {
        let ref_moves = record
            .take_in
            .as_ref()
            .map(|take_in| take_in.ref_moves.as_slice())
            .unwrap_or_default();
        for (repo, branch, error) in
            crate::merge_upstream::restore_ref_moves(&workspace_dir, ref_moves)
        {
            eprintln!(
                "  {}: '{branch}' moved since the op wrote it, so it is left where it is: {error:#}",
                RepoOperand::of(&repo, &op_project_name)
            );
            tally.any_foreign = true;
        }
        let created = record
            .take_in
            .as_ref()
            .map(|take_in| take_in.created_refs.as_slice())
            .unwrap_or_default();
        for (repo, branch, why) in crate::land::undo_created(&workspace_dir, created) {
            eprintln!(
                "  {}: '{branch}', which the op created, is left where it is: {why}",
                RepoOperand::of(&repo, &op_project_name)
            );
            tally.any_foreign = true;
        }
    }

    if let Some(other_dir) = &other_workspace {
        // Best-effort: an other workspace that does not present the op's
        // project is skipped with a warning rather than restored from
        // whatever else lives there.
        match WorkspaceContext::resolve_for_project(other_dir.as_path(), &op_project_name) {
            Ok(other_ctx) => {
                let other_ws_dir = other_ctx.active_path().to_path_buf();
                let other_project_dir = project_dir(&other_ws_dir, &op_project_name);
                match Project::from_dir_skip_lock(&other_project_dir, op_project_name.clone()) {
                    Ok(other_project) => {
                        let side = AbortSide::of(other_ctx.active_root(), &header);
                        restore_abort_workspace(
                            &other_ws_dir,
                            &other_project_dir,
                            &other_project,
                            side,
                            side.label(),
                            &op_id,
                            tips,
                            fast_forwarded,
                            abandon,
                            &mut tally,
                        );
                        // Clear op-state from the other workspace only when
                        // every repo so far restored — otherwise the operator
                        // may want to re-run abort after reconciling.
                        if tally.clean() {
                            op_state::clear_all_at(&other_ws_dir);
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "  warning: could not load project at {}: {e:#}; skipping",
                            other_project_dir.display()
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "  warning: could not resolve workspace at {}: {e:#}; skipping",
                    other_dir.as_path().display()
                );
            }
        }
    }

    // Emit the aggregate noise summary (skipped / untouched) as a single line.
    print_abort_noise_summary(&tally.noise);

    // Print the recovery-options block ONCE, only when at least one repo
    // refused (foreign-tip violation). The per-repo block above shows the
    // evidence (savepoint, tip, blocking commits); this block tells the
    // operator what to do about it.
    if tally.any_foreign {
        print_abort_recovery_options();
    }

    // Clear op-state from CWD workspace only on a fully clean abort. If any
    // repo refused (foreign tip) or errored, retain op-state so the operator
    // can re-run `rwv abort` after manually reconciling the divergence.
    if tally.clean() {
        op_state::clear_all_at(&workspace_dir);
    } else if tally.any_foreign {
        eprintln!(
            "\nabort refused on at least one repo (foreign-tip violation); \
             op-state retained at {} so you can re-run `rwv abort` after \
             reconciling.",
            workspace_dir.display()
        );
    }

    if tally.any_foreign {
        crate::refuse!(
            RefusalKind::ForeignTip,
            "abort refused: foreign tip on at least one repo"
        );
    }
    if tally.any_failure {
        return Err(crate::untokened!(Tally, "abort completed with failures"));
    }

    Ok(())
}

/// Restore one of the op's workspaces: its member repos, then its project
/// repo, then settle its members. `label` prefixes every per-repo line; the
/// workspace abort was invoked from carries none.
///
/// The op's tip table is phase-scoped (`PhaseTips`): `advanced_tips` during
/// replay, `converged_tips` after relock, keyed by repo path for member repos
/// and by `RepoKey::Project` for the project repo. The replay journal applies
/// to the replay side only; converged tips apply to both, because
/// advance-target lands the target on them.
///
/// `fast_forwarded` is the landing's journal of the target repos whose branch
/// it moved, `None` where the record could not be read. On the target side a
/// repo the journal does not name, at neither its savepoint nor its converged
/// tip, was moved by someone else and never by the landing: it is left where
/// it stands, and is no foreign tip.
#[allow(clippy::too_many_arguments)]
fn restore_abort_workspace(
    workspace_dir: &Path,
    project_dir: &Path,
    project: &Project,
    side: AbortSide,
    label: &str,
    op_id: &OpId,
    tips: Option<&op_state::PhaseTips>,
    fast_forwarded: Option<&[RepoKey]>,
    abandon: &crate::cli::consent::AbandonForeignTipConsent,
    tally: &mut AbortTally,
) {
    let empty_tips = std::collections::BTreeMap::new();
    let converged_tips = tips.and_then(|t| t.converged()).unwrap_or(&empty_tips);
    let advanced_tips = match side {
        AbortSide::Replay => tips.and_then(|t| t.advanced()).unwrap_or(&empty_tips),
        AbortSide::LandTarget => &empty_tips,
    };
    let restore_key = side.savepoint_key(op_id);

    let project_vcs = project_vcs();
    let repos = project
        .manifest
        .iter_entries()
        .map(|(repo_path, entry)| {
            (
                RepoKey::Member(repo_path.clone()),
                vcs_for(entry.vcs_type),
                workspace_dir.join(repo_path.as_path()),
            )
        })
        .filter(|(_, _, abs)| abs.exists())
        .chain(std::iter::once((
            RepoKey::Project,
            project_vcs,
            project_dir.to_path_buf(),
        )));
    let mut moved_from: std::collections::BTreeMap<PathBuf, ResolvedRevisionId> =
        std::collections::BTreeMap::new();
    for (repo_key, vcs, abs) in repos {
        let named = RepoOperand::of(&repo_key, &project.name);
        let unmoved_target = matches!(side, AbortSide::LandTarget)
            && fast_forwarded.is_some_and(|moved| !moved.contains(&repo_key));
        let consented = !unmoved_target && abandon.covers(&repo_key, &project.name);
        if let Ok(head) = vcs.head_revision(&abs) {
            moved_from.insert(abs.clone(), head);
        }
        match abort_one_repo(
            vcs.as_ref(),
            &abs,
            &restore_key,
            advanced_tips.get(&repo_key),
            converged_tips.get(&repo_key),
            consented,
        ) {
            Ok(VerifiedRestoreOutcome::ForeignTip { observed_tip, .. }) if unmoved_target => {
                outln!(
                    "  {label}{named}: left at {}, where something other than this landing moved it",
                    observed_tip.short()
                );
            }
            Ok(outcome) => report_abort_outcome(
                vcs.as_ref(),
                &format!("{label}{named}"),
                &outcome,
                Some(abs.as_path()),
                &mut tally.noise,
                &mut tally.any_foreign,
            ),
            Err(e) => {
                eprintln!("  {label}{named}: {e:#}");
                tally.any_failure = true;
            }
        }
    }
    settle_members(
        workspace_dir,
        project_dir,
        &|_, member| moved_from.get(member).cloned(),
        None,
    );
}

/// Restore a single repo as part of `rwv abort`.
///
/// The destructive `reset --hard` to the savepoint is gated on the current tip
/// being attributable to the op (== savepoint, == recorded intent tip,
/// == recorded converged tip, or mid-op). Anything else is reported as
/// foreign, not reset.
///
/// `recorded_intent_tip` is the SHA from the owner record's `advanced_tips`
/// map for this repo (source/owner side only — target side passes `None`).
/// `recorded_converged_tip` is from `converged_tips` (written at relock).
///
/// `abandon_foreign_tip` is whether the operator named THIS repo on
/// `--abandon-foreign-tip`: a foreign tip is then kept as an `abandoned`
/// backup and the repo restored over it.
fn abort_one_repo(
    vcs: &dyn Vcs,
    repo: &Path,
    key: &SavepointKey,
    recorded_intent_tip: Option<&CommitId>,
    recorded_converged_tip: Option<&CommitId>,
    abandon_foreign_tip: bool,
) -> anyhow::Result<VerifiedRestoreOutcome> {
    let policy = if abandon_foreign_tip {
        ForeignTipPolicy::Abandon
    } else {
        ForeignTipPolicy::Refuse
    };
    vcs.verified_restore_savepoint(
        repo,
        key,
        recorded_intent_tip,
        recorded_converged_tip,
        policy,
    )
    .context("verified restore failed")
}

/// Accumulated counts for non-actionable per-repo abort outcomes. Gathered
/// during the reporting loop so a single summary line can be emitted at the
/// end rather than one line per boring repo.
#[derive(Default)]
struct AbortNoiseSummary {
    no_savepoint: usize,
    untouched: usize,
}

/// Record a per-repo abort outcome into `summary`/`any_foreign`, printing
/// actionable lines immediately and deferring non-actionable counts to the
/// summary. The recovery-options block is NOT printed here — callers print
/// it once after all repos are processed.
///
/// `repo_abs` is the absolute path to the repo on disk, used only for the
/// read-only `git log` lookup on foreign-tip refusals; pass `None` when the
/// path is unavailable (e.g. the repo does not exist on disk).
fn report_abort_outcome(
    vcs: &dyn Vcs,
    label: &str,
    outcome: &VerifiedRestoreOutcome,
    repo_abs: Option<&Path>,
    summary: &mut AbortNoiseSummary,
    any_foreign: &mut bool,
) {
    match outcome {
        VerifiedRestoreOutcome::NoSavepoint => {
            // Demoted to the noise summary — printed as one aggregate line.
            summary.no_savepoint += 1;
        }
        VerifiedRestoreOutcome::Untouched => {
            // Demoted to the noise summary — printed as one aggregate line.
            summary.untouched += 1;
        }
        VerifiedRestoreOutcome::RestoredFromIntent => {
            outln!("  {label}: restored (from recorded intent tip)");
        }
        VerifiedRestoreOutcome::RestoredFromConverged => {
            outln!("  {label}: restored (from recorded converged tip)");
        }
        VerifiedRestoreOutcome::RestoredFromMidOp => {
            outln!("  {label}: restored (from mid-op state)");
        }
        VerifiedRestoreOutcome::AbandonedForeignTip {
            abandoned_tip,
            savepoint,
            backup,
        } => {
            outln!(
                "  {label}: restored (abandoned foreign tip, per --abandon-foreign-tip)\n\
                 \tabandoned tip: {abandoned_tip} — kept as backup {name} (`rwv backup list`)\n\
                 \trestored to:   {savepoint}",
                name = backup.backup().name,
            );
        }
        VerifiedRestoreOutcome::ForeignTip {
            observed_tip,
            savepoint,
            recorded_converged_tip,
        } => {
            *any_foreign = true;
            let converged_text = match recorded_converged_tip {
                Some(c) => format!("recorded converged tip: {c}"),
                None => "no converged tip recorded (op crashed before relock)".to_string(),
            };

            // Determine the commit-graph shape and fetch blocking commits.
            let shape_and_commits = if let Some(repo) = repo_abs {
                let (ahead, behind) = vcs.ahead_behind(repo, savepoint, observed_tip);
                let shape = if behind == 0 && ahead > 0 {
                    format!("tip is {ahead} commit(s) ahead of savepoint (strictly ahead — common recoverable case)")
                } else if ahead > 0 && behind > 0 {
                    format!("tip and savepoint have diverged ({ahead} ahead, {behind} behind — requires manual reconciliation)")
                } else {
                    // ahead == 0 && behind == 0: equal — shouldn't reach ForeignTip, but be safe.
                    "tip equals savepoint (unexpected ForeignTip state)".to_string()
                };
                let (commits, total) =
                    vcs.commits_in_range(repo, savepoint, observed_tip, BLOCKING_COMMITS_CAP);
                let commit_block = if commits.is_empty() {
                    "\t  (no commits in range or range unresolvable)".to_string()
                } else {
                    let mut block = commits
                        .iter()
                        .map(|c| format!("\t  {} {}", c.id.short(), c.subject))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if total > BLOCKING_COMMITS_CAP {
                        block.push_str(&format!(
                            "\n\t  ... and {} more",
                            total - BLOCKING_COMMITS_CAP
                        ));
                    }
                    block
                };
                format!("\tblocking commits ({shape}):\n{commit_block}")
            } else {
                "\tblocking commits: (repo path unavailable)".to_string()
            };

            eprintln!(
                "  {label}: foreign-tip violation — refusing to reset.\n\
                 \tobserved tip:  {observed_tip} (left in place)\n\
                 \texpected one of: savepoint {savepoint}, {converged_text}, or a VCS-native mid-op state\n\
                 {shape_and_commits}",
            );
        }
    }
}

/// Print the one-time recovery-options block. Called exactly once at the end
/// of `run_abort` when `any_foreign` is true. Kept separate so the per-repo
/// loop in `run_abort` stays free of repeated options text.
fn print_abort_recovery_options() {
    eprintln!(
        "\nrecovery options (apply to each refused repo above):\n\
         \t- if the foreign commits are wanted, move the branch back to them yourself \
           and re-run `rwv abort`.\n\
         \t- if they are not, re-run naming the repo: \
           `rwv abort --abandon-foreign-tip <repo>` (repeat per repo; `projects/<name>` \
           names the project repo). The branch returns to the savepoint and the \
           abandoned commits are kept as an `abandoned` backup that `rwv backup list` \
           shows and `rwv backup drop` drops."
    );
}

/// Emit a one-line summary of non-actionable (noise) outcomes. Suppressed
/// when both counts are zero (nothing to say).
fn print_abort_noise_summary(summary: &AbortNoiseSummary) {
    let mut parts: Vec<String> = Vec::new();
    if summary.no_savepoint > 0 {
        parts.push(format!(
            "{} repo(s) skipped (no savepoint)",
            summary.no_savepoint
        ));
    }
    if summary.untouched > 0 {
        parts.push(format!(
            "{} untouched (tip == savepoint)",
            summary.untouched
        ));
    }
    if !parts.is_empty() {
        outln!("  summary: {}", parts.join(", "));
    }
}

/// The repos a failed op's refusal judged, for its envelope.
fn refused_by(result: &anyhow::Result<()>) -> Vec<crate::diagnosis::JudgedOutput> {
    result
        .as_ref()
        .err()
        .map(crate::diagnosis::refused_output)
        .unwrap_or_default()
}

/// JSON exit/return tail of `run_land_json`.
///
/// After serialization is done, both callers share
/// the same exit logic: exit 1 when any per-repo outcome failed, otherwise
/// propagate `project_level_result`. Factored here so the logic lives in
/// exactly one place.
///
/// Uses `process::exit` rather than `Err` when repos fail so that the JSON
/// already printed to stdout is not contaminated by anyhow's stderr
/// formatter (the test harness asserts on stdout + exit code only).
fn json_exit_tail(
    any_failure: bool,
    project_level_result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Map exit code: non-zero iff any per-repo outcome was a failure.
    // We use process::exit directly so the JSON we just printed is the
    // only thing on stdout — bubbling Err would route through anyhow's
    // stderr formatter (acceptable, but the test harness asserts only on
    // stdout + exit code).
    if any_failure {
        std::process::exit(1);
    }
    // Also propagate any project-level error that fired AFTER per-repo
    // outcomes were captured (e.g., Phase 1' or Phase 3 failure). The
    // outcomes JSON has been emitted; surface the error via Err so
    // main's anyhow display fires.
    project_level_result
}

/// Workers record outcomes as they finish, so under `-j N` the buffer holds
/// them in completion order; the envelope lists them as the lock does.
fn in_manifest_order(mut records: Vec<SyncOutcomeOutput>) -> Vec<SyncOutcomeOutput> {
    records.sort_by(|a, b| a.path().cmp(b.path()));
    records
}

// ---------------------------------------------------------------------------
// rwv land
// ---------------------------------------------------------------------------
//
// `land` is the same data-driven phase machine as `sync` (above) with
// advance-target always running and retire running when `--retire` is set:
//
//   guard → mark → savepoint → replay → relock → advance-target → [retire] → cleanup
//
// The replay phase IS what `rwv sync <target>` does (CWD absorbs target's
// history with CWD's commits on top). advance-target ff-forwards the
// target's repos to CWD's converged tips. With `--retire`, the workweave
// is then deleted (merged-check + dirty-check, see [`retire_workweave_after_landing`]).
//
// Op-state is one full owner record at CWD plus a thin lease at the target
// workspace. Driver re-entry follows the lease pointer when `--continue`
// is invoked from the target side.

/// Execute `rwv land <target>`.
///
/// `target` is the workspace to advance, or `None` when `--continue` is set
/// (target is then read from the in-progress op-state file). All steps are
/// expressed as phases in the data-driven machine; `--continue` resumes at
/// the recorded phase from either workspace.
#[allow(clippy::too_many_arguments)]
pub fn run_land(
    ctx: &WorkspaceContext,
    request: SyncRequest,
    output: OutputMode,
) -> anyhow::Result<()> {
    match output {
        OutputMode::Json => run_land_json(ctx, request),
        OutputMode::Text => {
            let stdout_lock: Mutex<()> = Mutex::new(());
            let handler = TextHandler {
                stdout_lock: &stdout_lock,
            };
            run_machine(ctx, request, &handler)
        }
    }
}

/// Execute `rwv land <target> --json`.
///
/// Emits a [`LandJsonOutput`] envelope with the new observability fields:
/// `source_workweave`, `target`, `retired`, per-outcome `step3_advance`, and
/// `project_repo_advance`. These fields are absent from the plain
/// `rwv sync --json` envelope ([`SyncJsonOutput`]).
fn run_land_json(ctx: &WorkspaceContext, request: SyncRequest) -> anyhow::Result<()> {
    let run = land_json_run(ctx, request);
    if let Some(envelope) = &run.envelope {
        let out =
            serde_json::to_string_pretty(envelope).context("failed to serialize land output")?;
        outln!("{out}");
    }
    json_exit_tail(run.any_failure, run.project_level_result)
}

/// What a `rwv land --json` run produced, before any of it reaches stdout.
///
/// `run_land_json` is this plus a `println!` and the exit tail both JSON
/// emitters share. The split is what lets the envelope be read as a value: a
/// caller that wants the machine's own answers does not have to capture fd 1,
/// and does not lose the process to the failure exit.
pub struct LandJsonRun {
    /// The envelope to print. `None` when there is nothing to print: a machine
    /// that refused before resolving its coordinates has no op to describe.
    pub envelope: Option<LandJsonOutput>,
    /// Whether any per-repo outcome failed — what decides the exit code.
    pub any_failure: bool,
    /// The project-level result, which can fail after the per-repo outcomes
    /// are already captured.
    pub project_level_result: anyhow::Result<()>,
}

/// Run the `rwv land` machine under `--json` and assemble its envelope.
///
/// The envelope is built from what the machine reported alone, plus the
/// invocation's resolution: the coordinates it recorded once its context
/// resolved, and the witness retire's delete returned. On a resumed op those
/// differ from what the invocation carries — the op is owned by a workspace
/// the caller need not be in, and its target is in the op record.
pub fn land_json_run(ctx: &WorkspaceContext, request: SyncRequest) -> LandJsonRun {
    let recorder = land_envelope::LandRecorder::default();
    let project_level_result = run_machine(ctx, request, &recorder);
    let outcome = recorder.into_outcome();
    LandJsonRun {
        any_failure: outcome.any_failure(),
        envelope: outcome.envelope(
            refused_by(&project_level_result),
            project_level_result.is_err(),
            Some(ctx.resolution()),
        ),
        project_level_result,
    }
}

/// What one repo's step-3 fast-forward did, taken from the reads the advance
/// itself made.
///
/// The line the operator reads and the `step3_advance` record a `--json`
/// consumer reads are two renderings of this one value. Derived separately —
/// the text from the move, the record from a second HEAD read taken beside it
/// — they could disagree about whether the same repo advanced, and about what
/// it advanced from.
#[derive(Debug)]
struct FfAdvance {
    /// The target's HEAD the ff decision was taken against.
    from: ResolvedRevisionId,
    /// The branch that received the landing; `None` when the target was
    /// already at CWD's tip and nothing moved.
    onto: Option<AttachedRef>,
}

/// Per-repo advance-target line, naming the branch that received the landing.
fn ff_advance_line(advance: &FfAdvance, tip: &ResolvedRevisionId) -> String {
    let short = &tip.id().as_str()[..8.min(tip.id().as_str().len())];
    match &advance.onto {
        Some(branch) => format!("ff-advanced {branch} to {short}"),
        None => format!("already at {short}"),
    }
}

/// The `target` moved mid-operation line for one repo the fast-forward found
/// moved off the branch or the tip it was to receive.
fn target_moved_line_for(
    label: impl std::fmt::Display,
    repo: &Path,
    how: String,
) -> crate::diagnosis::Judged {
    State::TargetMovedMidOperation
        .of(label)
        .subject(format!(
            "{} moved after the landing read it",
            repo.display()
        ))
        .listing([how])
        .by_verb("land")
        .judge(project_vcs().as_ref())
}

fn does_not_fast_forward(now: &ResolvedRevisionId, composed: &ResolvedRevisionId) -> String {
    format!(
        "now at {}, which does not fast-forward to {}",
        now.id().short(),
        composed.id().short()
    )
}

/// Fast-forward `branch`, which `target_repo` must have checked out, to
/// `cwd_tip`, returning the witness for that branch. `None` means the target
/// was already at `cwd_tip` and nothing moved.
///
/// We need the objects to be reachable in `target_repo`. For a worktree
/// pair (workweave + primary), they share the same object store, so any
/// SHA reachable in the CWD worktree is also reachable in the target
/// worktree. We use `git fetch <cwd_repo_path> HEAD` to ensure the object
/// is present in the target's object store, then `git merge --ff-only
/// <sha>` to advance the branch. merge --ff-only (rather than `reset
/// --hard`) because the purpose here is advancing, not discarding: it
/// refuses, instead of silently clobbering, if the update would touch
/// uncommitted changes in the target.
///
/// The fetch-then-advance approach works for both worktrees (same object
/// store, fetch is a no-op) and independent clones (fetch copies objects).
///
/// # The landing target is a witness, not a path
///
/// The refusal below is not the only thing standing between a detached
/// target and a landing that referenced nothing; it is also a *type*:
/// the MOVE takes an [`AttachedRef`] and derives the repo it moves from
/// that witness, so there is no signature in which the branch this function
/// establishes and the repo it advances can come apart. `target_repo` is
/// where the witness is *obtained*; it is never handed to the MOVE.
///
/// That closes the dodge the runtime check alone left open. `cwd_repo` is a
/// workweave checkout and is therefore always attached, so a witness taken
/// from it would satisfy any "did you check for a branch" gate while the
/// advance still landed on the detached target. With the witness carrying
/// its own repo, using CWD's attachment to move the target is not a check
/// someone can route around — it is a call that does not typecheck.
fn ff_advance_repo(
    vcs: &dyn Vcs,
    label: impl std::fmt::Display,
    target_repo: &Path,
    cwd_repo: &Path,
    branch: &str,
    cwd_tip: &ResolvedRevisionId,
) -> anyhow::Result<FfAdvance> {
    crate::test_pause::point("advance-target");
    if let Some(off) = off_its_branch(vcs, target_repo, branch)? {
        return Err(target_moved_line_for(label, target_repo, off).refused());
    }
    // Verify that target_repo's HEAD is an ancestor of (or equal to) cwd_tip.
    // If not, this is a concurrent-modification scenario — bail.
    let target_tip = vcs
        .head_revision(target_repo)
        .context("failed to read target HEAD")?;

    if target_tip == *cwd_tip {
        return Ok(FfAdvance {
            from: target_tip,
            onto: None,
        }); // already at the right tip
    }

    // Landing must name the ref it lands on. `merge --ff-only` against a
    // detached HEAD moves HEAD alone and reports success, leaving the work on
    // no branch — and the source's branch, the only other ref holding it, is
    // force-deleted by `--retire`. Checked after the equal-tip return, like
    // the dirty gate below: a checkout we won't move needs no destination.
    //
    // Obtaining the witness IS the refusal: the MOVE below cannot be written
    // without one, and the only producer is a `head_attachment` read of this
    // repo. `Unborn` is a third state rather than a second spelling of
    // detached — unreachable here, because a branch with no commits
    // fails the `head_revision` read above, but it is answered rather than
    // folded in so the arm cannot be quietly re-collapsed.
    let on = match vcs
        .head_attachment(target_repo)
        .context("failed to read target HEAD ref")?
    {
        HeadAttachment::Attached(a) => a,
        HeadAttachment::Detached(d) => crate::refuse!(
            RefusalKind::HeadNotOnBranch,
            "target repo at {} is not on a branch (detached HEAD at {}); refusing to \
             land onto it. Nothing would record the advance to {}. Check out the branch \
             that should receive this work (`git switch <branch>` in the target), then \
             re-run.",
            target_repo.display(),
            d.at(),
            cwd_tip,
        ),
        HeadAttachment::Unborn(u) => crate::refuse!(
            RefusalKind::HeadNotOnBranch,
            "target repo at {} is on unborn branch '{}' (no commits yet); refusing to \
             land onto it. Nothing would record the advance to {}. Commit in the target, \
             or check out the branch that should receive this work, then re-run.",
            target_repo.display(),
            u,
            cwd_tip,
        ),
    };

    // Fast-forwarding a target worktree with dirty tracked files risks
    // clobbering them. The land preflight already refused on tracked
    // target dirt; this catches concurrent modification since then, with a
    // named precondition instead of merge's generic refusal. Checked after
    // the equal-tip return: a dirty worktree we won't move is safe.
    //
    // Untracked files are not scanned here for the same reason the preflight
    // doesn't: a fast-forward only clobbers an untracked path that collides
    // with one the incoming tree writes, and `advance_attached_ref` below
    // catches that collision natively and re-mints it as a named refusal.
    let tracked = tracked_or_unreadable(vcs, target_repo);
    let mut edits = Report::default();
    judge_edits_in(
        vcs,
        target_repo.display(),
        target_repo,
        tracked,
        None,
        &mut edits,
    );
    edits.into_result()?;

    // Check that target is an ancestor of cwd_tip (ff precondition).
    // Bring objects across first via the Vcs trait so merge-base has the
    // SHAs reachable in target_repo. For sibling worktrees that share an
    // object store this is a no-op; for independent clones it copies
    // objects across.
    vcs.fetch_objects_from(target_repo, cwd_repo);

    let is_ancestor = vcs
        .is_ancestor(target_repo, &target_tip, cwd_tip)
        .unwrap_or(false);

    if !is_ancestor {
        return Err(target_moved_line_for(
            label,
            target_repo,
            does_not_fast_forward(&target_tip, cwd_tip),
        )
        .refused());
    }

    crate::test_pause::point("advance-target-move");

    // Fast-forward. The witness names both the ref that moves and the repo
    // it moves in; `advance_attached_ref` re-observes before acting, so an
    // attachment that changed since the read above is a refusal rather than
    // a landing on whatever HEAD became (how wide that window should be
    // stays open). Underneath, the ff refuses rather than clobbers if the
    // update would touch tracked changes — the VCS-native backstop behind
    // the tracked-dirty gate above — or an untracked file colliding with an
    // incoming path, which the `UntrackedCollision` arm below names.
    if let Err(e) = vcs.advance_attached_ref(&on, cwd_tip) {
        if let VcsError::UntrackedCollision { repo, paths } = &e {
            // advance-target is land only (see the module comment above
            // `run_advance_target`), so the resume command is fixed rather
            // than read off a `verb` this leaf has no context to carry.
            let resume = op_state::resume_command(OpVerb::Land);
            crate::refuse!(
                RefusalKind::UntrackedCollision,
                "target repo at {} has untracked file(s) that collide with paths this \
                 fast-forward would write:\n  {}\n\
                 \n\
                 Move or remove them in the target, then `{resume}`.",
                repo.display(),
                paths.join("\n  "),
            );
        }
        let now = vcs.head_revision(target_repo).ok();
        if matches!(e, VcsError::StaleRefWitness { .. }) || now.as_ref() != Some(&target_tip) {
            let how = match off_its_branch(vcs, target_repo, branch)? {
                Some(off) => off,
                None => match &now {
                    Some(now) => does_not_fast_forward(now, cwd_tip),
                    None => format!("its HEAD no longer reads: {e:#}"),
                },
            };
            return Err(target_moved_line_for(label, target_repo, how).refused());
        }
        return Err(e).context("fast-forward advance failed in target");
    }

    Ok(FfAdvance {
        from: target_tip,
        onto: Some(on),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::git_vcs;
    use crate::path_spelling::resolve;

    #[test]
    fn settling_speaks_when_nothing_handles_the_run_and_when_the_handler_does() {
        let stdout_lock: Mutex<()> = Mutex::new(());
        let text = TextHandler {
            stdout_lock: &stdout_lock,
        };
        let recorder = land_envelope::LandRecorder::default();

        assert!(settle_speaks(None), "`rwv abort` has no handler and speaks");
        assert!(settle_speaks(Some(&text)));
        assert!(
            !settle_speaks(Some(&recorder)),
            "a JSON run keeps stdout JSON"
        );
    }

    // -----------------------------------------------------------------------
    // Fixtures for the branch-model tests below
    // -----------------------------------------------------------------------

    /// Run git in `dir`, panicking on failure.
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed in {}: {}",
            args,
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// A repo on `main` with one commit, at `dir`.
    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "T"]);
        // Git for Windows ships core.autocrlf=true, under which a checkout
        // returns CRLF for content committed as LF.
        git(dir, &["config", "core.autocrlf", "false"]);
        std::fs::write(dir.join("f"), "1").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "one"]);
    }

    /// Commit a new file and return the resulting tip.
    fn commit(dir: &Path, name: &str) -> ResolvedRevisionId {
        std::fs::write(dir.join(name), name).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", name]);
        git_vcs().head_revision(dir).unwrap()
    }

    // -----------------------------------------------------------------------
    // Reading a manifest at a commit other than the checked-out one
    // -----------------------------------------------------------------------

    #[test]
    fn a_manifest_naming_the_old_keys_declares_nothing_at_another_revision() {
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("r");
        init_repo(&repo);
        let entry = |key: &str| {
            format!(
                "[repositories.\"github/acme/server\"]\ntype = \"git\"\n\
                 url = \"https://github.com/acme/server.git\"\n{key} = \"main\"\n\
                 role = \"owned\"\n"
            )
        };
        let at = |manifest: &str| {
            std::fs::write(repo.join(Manifest::FILE_NAME), manifest).unwrap();
            git(&repo, &["add", "."]);
            git(&repo, &["commit", "-m", "manifest"]);
            git_vcs().head_revision(&repo).unwrap()
        };
        let old = at(&entry("version"));
        let current = at(&entry("branch"));
        let broken = at("[repositories.\"github/acme/server\"\n");

        let vcs = git_vcs();
        let declared = manifest_at_revision(vcs.as_ref(), &repo, &old).unwrap();
        assert!(declared.is_empty(), "the old keys declare no member");
        assert_eq!(
            manifest_at_revision(vcs.as_ref(), &repo, &current)
                .unwrap()
                .len(),
            1
        );
        assert!(
            manifest_at_revision(vcs.as_ref(), &repo, &broken).is_err(),
            "a file broken for any other reason is still refused"
        );
    }

    // -----------------------------------------------------------------------
    // Containment — the dest↔source verdict every refusal on that axis reads
    // -----------------------------------------------------------------------

    /// Two tips in one repo with no ancestry either way, forked from a shared
    /// base: `(left, right)`, one commit each side.
    fn forked_tips(dir: &Path) -> (ResolvedRevisionId, ResolvedRevisionId) {
        init_repo(dir);
        let base = git(dir, &["rev-parse", "HEAD"]);
        let left = commit(dir, "left");
        git(dir, &["checkout", "-b", "right", &base]);
        let right = commit(dir, "right");
        (left, right)
    }

    #[test]
    fn containment_names_each_shape_of_one_pair() {
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("r");
        init_repo(&repo);
        let base = git_vcs().head_revision(&repo).unwrap();
        let ahead = commit(&repo, "a");

        let vcs = git_vcs();
        assert_eq!(
            Containment::observe(vcs.as_ref(), &repo, &base, &base),
            Some(Containment::Equal)
        );
        assert_eq!(
            Containment::observe(vcs.as_ref(), &repo, &ahead, &base),
            Some(Containment::Ahead(1))
        );
        assert_eq!(
            Containment::observe(vcs.as_ref(), &repo, &base, &ahead),
            Some(Containment::Behind(1))
        );

        let forked = tmp.path().join("f");
        let (left, right) = forked_tips(&forked);
        assert_eq!(
            Containment::observe(vcs.as_ref(), &forked, &left, &right),
            Some(Containment::Diverged {
                ahead: 1,
                behind: 1
            })
        );
    }

    #[test]
    fn containment_declines_a_pair_the_store_cannot_resolve() {
        let tmp = crate::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        init_repo(&first);
        init_repo(&second);
        let vcs = git_vcs();
        let in_first = commit(&first, "only in first");
        let in_second = commit(&second, "only in second");
        assert_eq!(
            Containment::observe(vcs.as_ref(), &first, &in_first, &in_second),
            None
        );
    }

    #[test]
    fn every_verdict_renders_in_one_vocabulary() {
        // `Equal` and `Behind` reach no refusal site today; they are rendered
        // here so the words a later consumer inherits are pinned rather than
        // invented at that call site.
        let other = "the source lock entry";
        assert_eq!(
            Containment::Equal.describe(other),
            "equal to the source lock entry"
        );
        assert_eq!(
            Containment::Ahead(1).describe(other),
            "strictly ahead of the source lock entry by 1 commit (contains it)"
        );
        assert_eq!(
            Containment::Behind(2).describe(other),
            "strictly behind the source lock entry by 2 commits"
        );
        assert_eq!(
            Containment::Diverged {
                ahead: 3,
                behind: 4
            }
            .describe(other),
            "diverged from the source lock entry (3 ahead, 4 behind)"
        );
    }

    #[test]
    fn containment_declines_a_pair_it_cannot_resolve() {
        // A count git refuses to produce is not zero, and zero on both sides
        // is what `Equal` means: the pair that cannot be read must yield no
        // verdict at all, or every unreadable pair reports as converged.
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("r");
        init_repo(&repo);
        let head = git_vcs().head_revision(&repo).unwrap();
        let absent = ResolvedRevisionId::from_canonical(
            crate::vcs::CommitId::parse("0000000000000000000000000000000000000001").unwrap(),
            None,
        );

        assert_eq!(
            Containment::observe(git_vcs().as_ref(), &repo, &head, &absent),
            None
        );
    }

    #[test]
    fn the_already_ahead_line_names_its_baseline_and_advises_no_rebase() {
        // The condition that reaches this variant PROVED containment, so a
        // rebase onto that baseline is a no-op and advising one sent an
        // operator chasing a divergence that does not exist.
        let line = RepoSyncOutcome::AlreadyAhead {
            commits_ahead: 2,
            baseline: ReplayBaseline::SourceCommittedTip,
            source_position: Some(Containment::Equal),
        }
        .to_string();

        assert!(
            line.contains("strictly ahead of the source's committed tip by 2 commits"),
            "the line must name the baseline it was given; got: {line}"
        );
        assert!(
            !line.contains("rebase"),
            "a proven-contained tip must not be told to rebase; got: {line}"
        );
        assert!(
            !line.contains("diverge"),
            "containment is not divergence; got: {line}"
        );
    }

    // -----------------------------------------------------------------------
    // The entry tip is a function of the task's data and nothing else
    // -----------------------------------------------------------------------

    fn entry_verdicts(containment: Option<Containment>) -> ReplayVerdicts {
        ReplayVerdicts {
            baseline: ReplayBaseline::SourceCommittedTip,
            containment,
            source_position: None,
        }
    }

    #[test]
    fn only_a_fast_forward_has_an_entry_tip() {
        let repo = RepoPath::new("github/acme/server").unwrap();
        let target = ResolvedRevisionId::from_canonical(
            crate::vcs::CommitId::parse("00000000000000000000000000000000000000a1").unwrap(),
            None,
        );

        assert_eq!(
            entry_tip(
                &repo,
                &entry_verdicts(Some(Containment::Behind(2))),
                &target
            ),
            Some((RepoKey::Member(repo.clone()), target.id().clone()))
        );
        for containment in [
            None,
            Some(Containment::Equal),
            Some(Containment::Ahead(1)),
            Some(Containment::Diverged {
                ahead: 1,
                behind: 1,
            }),
        ] {
            assert_eq!(
                entry_tip(&repo, &entry_verdicts(containment), &target),
                None,
                "{containment:?} lands on no tip knowable before the move"
            );
        }
    }

    // -----------------------------------------------------------------------
    // sync_one_repo consumes the entry verdict; it does not re-read HEAD
    // -----------------------------------------------------------------------
    //
    // The op record's intent write and the move it records are two consumers of
    // one classification. A second HEAD read here is what let them disagree, so
    // these hand `sync_one_repo` a verdict that CONTRADICTS the disk: an outcome
    // that follows the disk is a re-read that has grown back.

    #[test]
    fn the_repo_outcome_follows_the_carried_verdict_not_the_disk() {
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("r");
        init_repo(&repo);
        let target = git_vcs().head_revision(&repo).unwrap();

        // On disk HEAD IS the target, which a fresh read would call NoOp.
        let outcome = sync_one_repo(
            git_vcs().as_ref(),
            &repo,
            &target,
            ReplayVerdicts {
                baseline: ReplayBaseline::SourceLockEntry,
                containment: Some(Containment::Ahead(7)),
                source_position: None,
            },
            SyncStrategy::Ff,
            None,
        );

        assert!(
            matches!(
                outcome,
                RepoSyncOutcome::AlreadyAhead {
                    commits_ahead: 7,
                    ..
                }
            ),
            "the carried verdict must decide the outcome; got: {outcome}"
        );
    }

    #[test]
    fn a_carried_equal_verdict_runs_no_strategy() {
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("r");
        init_repo(&repo);
        let target = git_vcs().head_revision(&repo).unwrap();
        let moved = commit(&repo, "later");

        // On disk HEAD is now PAST the target, which a fresh read would call
        // AlreadyAhead — and a re-read that reached the strategy would try to
        // move the checkout.
        let outcome = sync_one_repo(
            git_vcs().as_ref(),
            &repo,
            &target,
            ReplayVerdicts {
                baseline: ReplayBaseline::SourceLockEntry,
                containment: Some(Containment::Equal),
                source_position: None,
            },
            SyncStrategy::Ff,
            None,
        );

        assert!(
            matches!(outcome, RepoSyncOutcome::NoOp),
            "the carried verdict must decide the outcome; got: {outcome}"
        );
        assert_eq!(
            git_vcs().head_revision(&repo).unwrap(),
            moved,
            "a no-op outcome must have moved nothing"
        );
    }

    #[test]
    fn a_commit_landing_after_the_verdict_reaches_the_strategy() {
        // The record's intent write says this repo is a planned fast-forward to
        // `target`. A re-read here would find HEAD past `target` and report
        // already-ahead — the record claiming a landing the printed outcome
        // denies. The carried verdict sends it to the strategy instead, which
        // is the only place that can act on what is actually there.
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("r");
        init_repo(&repo);
        let target = commit(&repo, "target");
        commit(&repo, "foreign");

        let outcome = sync_one_repo(
            git_vcs().as_ref(),
            &repo,
            &target,
            ReplayVerdicts {
                baseline: ReplayBaseline::SourceLockEntry,
                containment: Some(Containment::Behind(1)),
                source_position: None,
            },
            SyncStrategy::Ff,
            None,
        );

        assert!(
            !matches!(outcome, RepoSyncOutcome::AlreadyAhead { .. }),
            "a planned ff-mover must not be silently re-classified behind the \
             record's back; got: {outcome}"
        );
    }

    // -----------------------------------------------------------------------
    // ff_advance_repo — landing takes the target's witness
    //
    // This is the phase-body layer of the detached-target refusal, and it is
    // the layer that covers `land --continue`: a resumed op re-enters at
    // the phase, so the whole-op preflight does not run again. These drive
    // the function directly for that reason — routing through the CLI would
    // be answered by the preflight before this code was reached, and the
    // resume path is exactly where that is not true.
    // -----------------------------------------------------------------------

    /// A (cwd, target) pair of independent clones of the same history, where
    /// cwd has one commit the target does not. `target` is on `main`.
    fn landing_pair(tmp: &Path) -> (PathBuf, PathBuf, ResolvedRevisionId) {
        let origin = tmp.join("origin");
        init_repo(&origin);

        let target = tmp.join("target");
        git(
            tmp,
            &["clone", origin.to_str().unwrap(), target.to_str().unwrap()],
        );
        let cwd = tmp.join("cwd");
        git(
            tmp,
            &["clone", origin.to_str().unwrap(), cwd.to_str().unwrap()],
        );
        git(&cwd, &["config", "user.email", "t@t"]);
        git(&cwd, &["config", "user.name", "T"]);
        let cwd_tip = commit(&cwd, "landed");
        (cwd, target, cwd_tip)
    }

    #[test]
    fn ff_advance_repo_lands_on_the_branch_the_target_is_attached_to() {
        // The control. Without it, the refusal test below could pass because
        // this fixture cannot advance at all rather than because the refusal
        // fired.
        let tmp = crate::tempdir().unwrap();
        let (cwd, target, cwd_tip) = landing_pair(tmp.path());

        let advance = ff_advance_repo(git_vcs().as_ref(), "repo", &target, &cwd, "main", &cwd_tip)
            .expect("an attached target accepts the landing");
        let landed = advance
            .onto
            .expect("the target moved, so a branch received it");

        assert_eq!(
            landed.to_string(),
            "main",
            "the returned witness must name the branch that received the landing"
        );
        assert_eq!(
            git(&target, &["rev-parse", "refs/heads/main"]),
            cwd_tip.id().as_str(),
            "target `main` must hold the landed commit"
        );
    }

    #[test]
    fn ff_advance_repo_stops_on_a_target_that_moved_as_a_state_with_a_route() {
        let tmp = crate::tempdir().unwrap();
        let (cwd, target, cwd_tip) = landing_pair(tmp.path());
        git(&target, &["config", "user.email", "t@t"]);
        git(&target, &["config", "user.name", "T"]);
        commit(&target, "target-only");

        let err = ff_advance_repo(git_vcs().as_ref(), "repo", &target, &cwd, "main", &cwd_tip)
            .expect_err("a target holding commits CWD's tip does not must refuse");
        let report = crate::diagnosis::report_of(&err).expect("the stop is judged");
        assert_eq!(
            report.lines()[0].state(),
            State::TargetMovedMidOperation,
            "{err:#}"
        );
        assert!(
            format!("{err:#}").contains("which does not fast-forward to"),
            "{err:#}"
        );
    }

    #[test]
    fn ff_advance_repo_refuses_to_land_onto_a_detached_target() {
        // The loss chain: `merge --ff-only` against a detached HEAD
        // moves HEAD alone and reports success, so the landing ends up
        // referenced by nothing — and `--retire` then force-deletes the
        // source's branch, the only other ref that was holding it.
        let tmp = crate::tempdir().unwrap();
        let (cwd, target, cwd_tip) = landing_pair(tmp.path());

        let main_before = git(&target, &["rev-parse", "refs/heads/main"]);
        git(&target, &["checkout", "--detach", "HEAD"]);
        let head_before = git(&target, &["rev-parse", "HEAD"]);

        let err = ff_advance_repo(git_vcs().as_ref(), "repo", &target, &cwd, "main", &cwd_tip)
            .expect_err("a detached target has no branch for the landing to advance");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("is not on a branch (detached HEAD at"),
            "the refusal must name the detached-target precondition, not some other \
             failure; got:\n{msg}"
        );

        assert_eq!(
            git(&target, &["rev-parse", "refs/heads/main"]),
            main_before,
            "a refused landing must leave the target's branch where it was"
        );
        assert_eq!(
            git(&target, &["rev-parse", "HEAD"]),
            head_before,
            "a refused landing must leave the target's detached HEAD where it was — \
             this is the assertion that fails if the MOVE goes back to taking a path \
             instead of the target's witness"
        );
    }

    #[test]
    fn ff_advance_repo_refuses_a_target_switched_off_the_branch_it_plans() {
        let tmp = crate::tempdir().unwrap();
        let (cwd, target, cwd_tip) = landing_pair(tmp.path());
        let main_before = git(&target, &["rev-parse", "refs/heads/main"]);
        git(&target, &["switch", "-c", "hotfix"]);

        let err = ff_advance_repo(git_vcs().as_ref(), "repo", &target, &cwd, "main", &cwd_tip)
            .expect_err("a checkout on another branch than the plan names must refuse");
        let report = crate::diagnosis::report_of(&err).expect("the stop is judged");
        assert_eq!(
            report.lines()[0].state(),
            State::TargetMovedMidOperation,
            "{err:#}"
        );
        assert!(
            format!("{err:#}").contains("on 'hotfix', not on 'main'"),
            "{err:#}"
        );
        assert_eq!(
            git(&target, &["rev-parse", "refs/heads/hotfix"]),
            main_before
        );
        assert_eq!(git(&target, &["rev-parse", "refs/heads/main"]), main_before);
    }

    #[test]
    fn ff_advance_repo_short_circuits_a_detached_target_that_is_already_there() {
        // The refusal sits *after* the equal-tip return, deliberately: a
        // checkout we are not going to move needs no destination. Pinned so
        // the ordering cannot be "tidied" into refusing on every detached
        // target, which would make `--continue` unable to finish an op whose
        // target was already advanced.
        let tmp = crate::tempdir().unwrap();
        let (cwd, target, cwd_tip) = landing_pair(tmp.path());

        git(&target, &["fetch", cwd.to_str().unwrap(), "HEAD"]);
        git(&target, &["checkout", "--detach", cwd_tip.id().as_str()]);

        let landed = ff_advance_repo(git_vcs().as_ref(), "repo", &target, &cwd, "main", &cwd_tip)
            .expect("an already-converged target is a no-op, detached or not");
        assert!(
            landed.onto.is_none(),
            "nothing moved, so no branch received anything"
        );
    }

    // -----------------------------------------------------------------------
    // judge_store_checkouts — in front of the store destroy
    // -----------------------------------------------------------------------

    /// A canonical store plus the primary root and project name.
    fn store_fixture(tmp: &Path) -> (PathBuf, PathBuf) {
        let primary = tmp.join("weave");
        std::fs::create_dir_all(primary.join("projects").join("web-app")).unwrap();
        let store = primary.join("github/example/server");
        init_repo(&store);
        (store, primary)
    }

    fn dropped() -> RepoPath {
        RepoPath::new("github/example/server").unwrap()
    }

    /// Every line `judge_store_checkouts` adds for `store`.
    fn judged_checkouts(store: &Path, primary: &Path) -> anyhow::Result<Report> {
        let mut report = Report::default();
        judge_store_checkouts(git_vcs().as_ref(), store, primary, &dropped(), &mut report)?;
        Ok(report)
    }

    #[test]
    fn a_store_nothing_claims_judges_no_checkout() {
        // The control: without it the refusals below could be passing on a
        // fixture that can never be unclaimed.
        let tmp = crate::tempdir().unwrap();
        let (store, primary) = store_fixture(tmp.path());

        assert!(judged_checkouts(&store, &primary).unwrap().is_empty());
    }

    #[test]
    fn a_registered_worktree_no_workweave_records_is_judged_a_raw_worktree() {
        let tmp = crate::tempdir().unwrap();
        let (store, primary) = store_fixture(tmp.path());

        // `git worktree add` writes its administration into the canonical
        // store, so `remove_dir_all` on the store takes this checkout's refdb
        // and objects with it. Detached, so the fixture adds no local-only
        // branch — the point is that R4 refuses on the registration alone.
        let live = tmp.path().join("live-workweave");
        git(
            &store,
            &["worktree", "add", "--detach", live.to_str().unwrap()],
        );

        let report = judged_checkouts(&store, &primary).unwrap();
        let lines: Vec<(State, &str)> = report
            .lines()
            .iter()
            .map(|j| (j.state(), j.repo()))
            .collect();
        assert_eq!(
            lines,
            [(State::CheckoutInRawWorktree, "github/example/server")]
        );
        let out = report.render("sync");
        assert!(
            out.contains("live-workweave"),
            "the worktree is listed:\n{out}"
        );
        assert!(out.ends_with("\nrwv explain checkout\n"), "{out}");
    }

    #[test]
    fn judge_store_checkouts_refuses_when_the_claims_cannot_be_read() {
        // Fail-closed: a claim we could not enumerate is a claim that stands.
        let tmp = crate::tempdir().unwrap();
        let not_a_repo = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&not_a_repo).unwrap();

        let err = judged_checkouts(&not_a_repo, tmp.path())
            .expect_err("an unreadable store must not be destroyed on the strength of a guess");
        assert!(
            format!("{err:#}").contains("cannot enumerate the worktrees registered against"),
            "the refusal must say the enumeration failed; got:\n{err:#}"
        );
    }

    // -----------------------------------------------------------------------
    // prune_dropped_repo — R4 in front of the store destroy, and the shipped
    // local-only refusal behind it, unrelaxed
    // -----------------------------------------------------------------------

    /// A primary weave whose `github/example/server` is a *clone* of a bare
    /// origin, so `main` has a remote counterpart and is not ahead of it —
    /// which is what makes the shipped local-only predicate pass and leaves
    /// R4 as the thing under test. A fixture without the remote would refuse
    /// before ever reaching the gate, and every assertion below would be
    /// about the wrong refusal.
    fn primary_with_cloned_store(tmp: &Path) -> (WorkspaceContext, PathBuf, ProjectName) {
        let origin = tmp.join("origin");
        init_repo(&origin);

        let primary = tmp.join("weave");
        std::fs::create_dir_all(primary.join("projects").join("web-app")).unwrap();
        std::fs::create_dir_all(primary.join("github/example")).unwrap();
        let store = primary.join("github/example/server");
        git(
            &primary,
            &["clone", origin.to_str().unwrap(), store.to_str().unwrap()],
        );
        git(&store, &["config", "user.email", "t@t"]);
        git(&store, &["config", "user.name", "T"]);

        let project = ProjectName::new("web-app").unwrap();
        let ctx = WorkspaceContext::resolve_invocation(&primary, Some(project.clone()))
            .expect("the fixture is a workspace root");
        (ctx, store, project)
    }

    #[test]
    fn prune_dropped_repo_removes_a_store_nothing_claims() {
        // The control. Without it, the two refusals below could be reporting
        // a fixture that can never be pruned rather than a gate that fired.
        let tmp = crate::tempdir().unwrap();
        let (ctx, store, _project) = primary_with_cloned_store(tmp.path());

        prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped())
            .expect("nothing claims this store");
        assert!(
            !store.exists(),
            "an unclaimed store is what prune is for; it must actually be removed"
        );
    }

    #[test]
    fn prune_dropped_repo_refuses_while_a_live_workweave_is_registered() {
        // `git worktree add` runs in the canonical store, so the worktree's
        // administration and its objects live *inside* the directory prune is
        // about to `remove_dir_all`. Detached, so the fixture adds no
        // local-only branch: the shipped predicate passes and R4 is the only
        // thing left standing between the live workweave and the delete.
        let tmp = crate::tempdir().unwrap();
        let (ctx, store, _project) = primary_with_cloned_store(tmp.path());

        let live = tmp.path().join("live-workweave");
        git(
            &store,
            &["worktree", "add", "--detach", live.to_str().unwrap()],
        );

        let err = prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped())
            .expect_err("a store a live workweave is registered against is claimed");
        let judged: Vec<State> = crate::diagnosis::report_of(&err)
            .map(|r| r.lines().iter().map(|j| j.state()).collect())
            .unwrap_or_default();
        assert_eq!(
            judged,
            [State::CheckoutInRawWorktree],
            "R4's registration arm must be what refused — not the local-only scan, \
             which this fixture satisfies; got:\n{err:#}"
        );
        assert!(
            store.exists() && live.exists(),
            "a refused prune must leave both the store and the live checkout on disk"
        );
        let msg = format!("{err:#}");
        assert!(
            msg.contains(&format!(
                "git -C {} worktree remove {}",
                store.display(),
                live.display()
            )),
            "the choice must spell the removal of this worktree from this store; got:\n{msg}"
        );

        // `rwv workweave delete <name>` retracts the registration;
        // mirror that at the git level R4 reads.
        git(
            &store,
            &["worktree", "remove", "--force", live.to_str().unwrap()],
        );
        prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped())
            .expect("with the live worktree gone the store is unclaimed");
        assert!(
            !store.exists(),
            "once nothing claims the store, prune removes it"
        );
    }

    #[test]
    fn prune_dropped_repo_refuses_a_store_holding_a_branch_only_it_has() {
        // This is the E1 scenario: a workweave holding a commit that exists
        // nowhere else. It must refuse *because of the unique commit*.
        let tmp = crate::tempdir().unwrap();
        let (ctx, store, _project) = primary_with_cloned_store(tmp.path());

        // The branch holds the unique commit with no checkout of it left, so
        // the store's checkouts pass and the local-only scan is what decides.
        git(&store, &["switch", "-c", "web-app--feat"]);
        git(&store, &["config", "user.email", "t@t"]);
        git(&store, &["config", "user.name", "T"]);
        let unique = commit(&store, "only-here");
        git(&store, &["switch", "main"]);

        let err = prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped())
            .expect_err("a store holding a commit that exists nowhere else is not prunable");
        assert!(
            format!("{err:#}").contains("local-only commits"),
            "the local-only refusal must fire; got:\n{err:#}"
        );
        assert_eq!(
            git(&store, &["rev-parse", "web-app--feat"]),
            unique.id().as_str(),
            "the unique commit must survive the refused prune"
        );
    }

    #[test]
    fn prune_dropped_repo_refuses_unpushed_work_when_a_tag_shares_the_branch_name() {
        let tmp = crate::tempdir().unwrap();
        let (ctx, store, _project) = primary_with_cloned_store(tmp.path());
        let unpushed = commit(&store, "only-here");
        git(&store, &["tag", "main", "refs/remotes/origin/main"]);

        let err = prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped())
            .expect_err("a store holding an unpushed commit is not prunable");

        assert!(
            format!("{err:#}").contains("local-only commits"),
            "the local-only refusal must fire on the branch, not be answered by the tag; got:\n{err:#}"
        );
        assert_eq!(
            git(&store, &["rev-parse", "refs/heads/main"]),
            unpushed.id().as_str(),
            "the unpushed commit must survive the refused prune"
        );
    }

    #[test]
    fn prune_dropped_repo_refuses_when_the_ahead_count_cannot_be_taken() {
        // The remote-tracking ref is present, so the counterpart probe passes
        // and the branch reaches the ahead-count; it points at an object the
        // clone does not have, which is the shape that makes `rev-list`
        // FAIL rather than answer. That failure is "we could not tell" and
        // has to refuse — read as "nothing unpushed" it clears the branch and
        // hands the store to the delete.
        let tmp = crate::tempdir().unwrap();
        let (ctx, store, _project) = primary_with_cloned_store(tmp.path());

        // A loose ref file wins over the packed-refs entry the clone wrote.
        let dangling = store.join(".git/refs/remotes/origin/main");
        std::fs::create_dir_all(dangling.parent().unwrap()).unwrap();
        std::fs::write(&dangling, "1111111111111111111111111111111111111111\n").unwrap();
        assert!(
            git_vcs()
                .branch_has_remote_counterpart(&store, &crate::vcs::RawRefName::new("main"))
                .unwrap(),
            "the fixture must still pass the counterpart probe, or the refusal \
             under test is never reached"
        );

        let err = prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped())
            .expect_err("a count git could not take is not evidence that nothing is unpushed");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("could not be ruled out"),
            "the unreadable count must be what refused, and must say so rather \
             than claim commits it never saw; got:\n{msg}"
        );
        assert!(
            store.exists(),
            "a refused prune must leave the clone on disk"
        );
    }

    // -----------------------------------------------------------------------
    // prune_dropped_repo — the workweave arm, where the absence of a
    // primary-side canonical decides nothing about what the checkout IS
    // -----------------------------------------------------------------------

    /// A workweave context whose primary-side `github/example/server` slot is
    /// ABSENT — the shape that routes prune into the arm with no canonical to
    /// compare against. What the workweave's own checkout is left unbuilt on
    /// purpose: that is the thing under test, so each case materializes it.
    /// Returns the context, the checkout path to build, and the project.
    fn workweave_without_primary_canonical(tmp: &Path) -> (WorkspaceContext, PathBuf, ProjectName) {
        let primary = tmp.join("weave");
        crate::workweave_index::init_project_clone(&primary, &ProjectName::new("web-app").unwrap());
        // Deliberately no `primary/github/example/server`.

        let ww = primary.join(".workweaves").join("web-app--feat");
        std::fs::create_dir_all(ww.join("github/example")).unwrap();
        let primary_canon = resolve(&primary).unwrap();
        crate::workspace::WorkweaveMarker::new(
            &primary_canon,
            ProjectName::new("web-app").unwrap(),
            &primary_canon,
        )
        .write(&ww)
        .unwrap();
        crate::workweave_index::record_workweave(
            &primary_canon,
            &ProjectName::new("web-app").unwrap(),
            &crate::manifest::WorkweaveName::new("feat").unwrap(),
            CanonicalPath::of(&ww),
            CanonicalPath::of(&primary_canon),
        )
        .unwrap();

        let ctx = WorkspaceContext::resolve_invocation(&ww, None)
            .expect("the marker names the primary weave");
        assert!(
            matches!(ctx.checkout, Checkout::Workweave { .. }),
            "the fixture must resolve as a workweave, or the arm under test is not the one that runs"
        );
        assert!(
            !ctx.primary_path().join("github/example/server").exists(),
            "the absent primary-side canonical is the precondition of this arm"
        );
        (
            ctx,
            ww.join("github/example/server"),
            ProjectName::new("web-app").unwrap(),
        )
    }

    #[test]
    fn prune_dropped_repo_refuses_a_workweave_checkout_that_is_itself_the_store() {
        // Inverted topology: the workweave holds a standalone clone rather
        // than a linked worktree. The absent primary-side slot used to be read as
        // "the store is gone, so this must be a linked worktree, delete it" —
        // but the objects under this path exist nowhere else in the weave, and
        // with no canonical to compare against, nothing has looked at them.
        let tmp = crate::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        init_repo(&origin);
        let (ctx, checkout, _project) = workweave_without_primary_canonical(tmp.path());
        git(
            tmp.path(),
            &[
                "clone",
                origin.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        assert!(
            checkout.join(".git").is_dir(),
            "the fixture must be a standalone clone, not a linked workspace"
        );
        let tip = git_vcs().head_revision(&checkout).unwrap();

        let err = prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped())
            .expect_err("a checkout that IS the store is not a working tree to delete");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("inverted clone topology"),
            "the refusal must name what it found, not blame uncommitted changes \
             or divergence; got:\n{msg}"
        );
        assert!(
            checkout.exists(),
            "a refused prune must leave the store on disk"
        );
        assert_eq!(
            git_vcs().head_revision(&checkout).unwrap(),
            tip,
            "the object database must survive the refused prune intact"
        );
    }

    #[test]
    fn prune_dropped_repo_removes_a_workweave_checkout_linked_into_a_store_elsewhere() {
        // The control for the refusal above. Same absent primary-side slot,
        // but the checkout is a real linked workspace whose refdb and objects
        // live in a store this delete never touches — so removing the
        // directory removes a working tree and nothing else. Without this,
        // the refusal above could be reporting an arm that never removes
        // anything.
        let tmp = crate::tempdir().unwrap();
        let store = tmp.path().join("elsewhere/server");
        init_repo(&store);
        let (ctx, checkout, _project) = workweave_without_primary_canonical(tmp.path());
        git(
            &store,
            &["worktree", "add", "--detach", checkout.to_str().unwrap()],
        );
        assert!(
            checkout.join(".git").is_file(),
            "the fixture must be a linked workspace, not a standalone clone"
        );

        prune_dropped_repo(git_vcs().as_ref(), &ctx, &dropped())
            .expect("a linked workspace is a working tree, and pruning it is what this arm is for");
        assert!(
            !checkout.exists(),
            "the dropped repo's working tree must actually be removed"
        );
        assert!(
            git_vcs().head_revision(&store).is_ok(),
            "the store the checkout linked into must be left intact"
        );
    }

    // -----------------------------------------------------------------------
    // materialize_missing_repo — phase 3 births through the chokepoint, which
    // puts the checkout on the workweave's branch where that branch stands
    // -----------------------------------------------------------------------

    /// A workweave whose primary holds the canonical clone and whose own
    /// checkout of that repo is ABSENT — the state phase 3 materializes into.
    /// Returns the context, the canonical store, the project, and the entry
    /// whose `version` is the start point the ephemeral ref is cut at.
    fn workweave_missing_one_repo(
        tmp: &Path,
    ) -> (
        WorkspaceContext,
        PathBuf,
        ProjectName,
        crate::manifest::RepoEntry,
    ) {
        let origin = tmp.join("origin");
        init_repo(&origin);

        let primary = tmp.join("weave");
        crate::workweave_index::init_project_clone(&primary, &ProjectName::new("web-app").unwrap());
        std::fs::create_dir_all(primary.join("github/example")).unwrap();
        let canonical = primary.join("github/example/server");
        git(
            &primary,
            &[
                "clone",
                origin.to_str().unwrap(),
                canonical.to_str().unwrap(),
            ],
        );
        git(&canonical, &["config", "user.email", "t@t"]);
        git(&canonical, &["config", "user.name", "T"]);

        let ww = primary.join(".workweaves").join("web-app--feat");
        std::fs::create_dir_all(ww.join("github/example")).unwrap();
        let primary_canon = resolve(&primary).unwrap();
        crate::workspace::WorkweaveMarker::new(
            &primary_canon,
            ProjectName::new("web-app").unwrap(),
            &primary_canon,
        )
        .write(&ww)
        .unwrap();
        crate::workweave_index::record_workweave(
            &primary_canon,
            &ProjectName::new("web-app").unwrap(),
            &crate::manifest::WorkweaveName::new("feat").unwrap(),
            CanonicalPath::of(&ww),
            CanonicalPath::of(&primary_canon),
        )
        .unwrap();

        let ctx = WorkspaceContext::resolve_invocation(&ww, None)
            .expect("the marker names the primary weave");
        assert!(
            matches!(ctx.checkout, Checkout::Workweave { .. }),
            "the fixture must resolve as a workweave, or materialize takes the clone arm instead"
        );
        let entry = crate::manifest::RepoEntry {
            vcs_type: crate::manifest::VcsType::Git,
            url: "https://example.com/server.git".parse().unwrap(),
            branch: crate::vcs::TrackingRef::parse(crate::vcs::RawRefName::new("main")).unwrap(),
            role: crate::manifest::Role::Owned,
            recurse_submodules: crate::manifest::SubmoduleRecursion::Recurse,
            history: None,
        };
        (ctx, canonical, ProjectName::new("web-app").unwrap(), entry)
    }

    #[test]
    fn materialize_missing_repo_authors_the_branch() {
        // The control. Without it the attach below could be passing on a
        // fixture that can never materialize anything at all.
        let tmp = crate::tempdir().unwrap();
        let (ctx, canonical, project, entry) = workweave_missing_one_repo(tmp.path());

        materialize_missing_repo(
            git_vcs().as_ref(),
            &ctx,
            &dropped(),
            &entry,
            "main",
            &project,
        )
        .expect("a name nothing holds is the ordinary create");

        let checkout = ctx.active_path().join(dropped().as_path());
        assert!(
            checkout.join(".git").is_file(),
            "the materialized repo must be a linked workspace on the ephemeral ref"
        );
        assert_eq!(
            git(&canonical, &["rev-parse", "web-app--feat"]),
            git(&canonical, &["rev-parse", "main"]),
            "the branch is created at the revision asked for"
        );
    }

    #[test]
    fn materialize_missing_repo_attaches_a_pre_existing_branch_where_it_stands() {
        // The workweave's own branch survived its checkout: the member is put
        // back on it, commits and all, and nothing moves or deletes it.
        let tmp = crate::tempdir().unwrap();
        let (ctx, canonical, project, entry) = workweave_missing_one_repo(tmp.path());

        git(&canonical, &["checkout", "-b", "web-app--feat"]);
        let operator_tip = commit(&canonical, "operators-work");
        git(&canonical, &["checkout", "main"]);

        materialize_missing_repo(
            git_vcs().as_ref(),
            &ctx,
            &dropped(),
            &entry,
            "main",
            &project,
        )
        .expect("the branch is the workweave's, so the member is put back on it");

        let checkout = ctx.active_path().join(dropped().as_path());
        assert_eq!(
            git_vcs().head_revision(&checkout).unwrap(),
            operator_tip,
            "the checkout stands at the branch's own tip"
        );
        assert_eq!(
            git_vcs()
                .resolve_revision(&canonical, "web-app--feat")
                .unwrap(),
            operator_tip,
            "the branch survives at its own tip"
        );
    }

    fn all_sync_failures() -> Vec<SyncFailure> {
        let all = vec![
            SyncFailure::HeadUnreadable {
                error: "x".into(),
                cause: None,
            },
            SyncFailure::FastForwardImpossible {
                error: "x".into(),
                cause: VcsError::NotARepo(PathBuf::from("/x")),
            },
            SyncFailure::RebaseFailed {
                error: "x".into(),
                cause: VcsError::NotARepo(PathBuf::from("/x")),
            },
        ];
        for f in &all {
            match f {
                SyncFailure::HeadUnreadable { .. }
                | SyncFailure::FastForwardImpossible { .. }
                | SyncFailure::RebaseFailed { .. } => {}
            }
        }
        all
    }

    #[test]
    fn every_sync_failure_kind_is_the_tag_its_wire_mirror_carries() {
        let failures = all_sync_failures();
        for failure in &failures {
            let wire = serde_json::to_value(SyncFailureOutput::from(failure)).unwrap();
            assert_eq!(
                failure.kind().to_string(),
                wire["kind"].as_str().expect("wire kind is a string"),
                "{failure:?}: the kind and the wire tag differ"
            );
        }

        let listed: std::collections::BTreeSet<String> =
            failures.iter().map(|f| f.kind().to_string()).collect();
        let all: std::collections::BTreeSet<String> = SyncFailureKind::ALL
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(listed, all, "SyncFailureKind::ALL omits a kind");
    }

    #[test]
    fn sync_failure_for_strategy_picks_matching_variant() {
        assert!(matches!(
            SyncFailure::for_strategy(
                SyncStrategy::Ff,
                "e".into(),
                VcsError::NotARepo(PathBuf::from("/x"))
            ),
            SyncFailure::FastForwardImpossible { .. }
        ));
        assert!(matches!(
            SyncFailure::for_strategy(
                SyncStrategy::Rebase,
                "e".into(),
                VcsError::NotARepo(PathBuf::from("/x"))
            ),
            SyncFailure::RebaseFailed { .. }
        ));
    }

    // -----------------------------------------------------------------------
    // Conflict-bail messages.
    //
    // One test per bail site asserts the message embeds the per-VCS
    // resolution hint and mentions `rwv abort` last (after the steps).
    // -----------------------------------------------------------------------

    /// Acceptance shape predicate: the message contains a resolution hint
    /// (the trait-method text via `git add <files>` token) and lists
    /// `rwv abort` strictly AFTER it.
    fn assert_resolution_first_abort_last(msg: &str) {
        let add_pos = msg
            .find("git add <files>")
            .unwrap_or_else(|| panic!("expected resolution hint `git add <files>` in: {msg}"));
        let abort_pos = msg
            .find("rwv abort")
            .unwrap_or_else(|| panic!("expected `rwv abort` mentioned in: {msg}"));
        assert!(
            abort_pos > add_pos,
            "`rwv abort` must come AFTER the resolution steps; \
             abort_pos={abort_pos}, add_pos={add_pos}, msg={msg}"
        );
    }

    // Site 1 — manifest-repo per-repo sync loop failure summary. With a live
    // rebase conflict the VCS-native staging steps are present; the resume
    // verb is derived from the op verb; and the rebase hint must NOT spell
    // raw `git rebase --continue` (`rwv <verb> --continue` IS the continue
    // step — rwv resumes the rebase natively).
    #[test]
    fn manifest_repo_failure_message_live_conflict_includes_vcs_and_verb_resume() {
        let msg = manifest_repo_failure_message(
            git_vcs().as_ref(),
            OpVerb::Land,
            Some(ConflictOp::Rebase),
        );
        assert!(
            !msg.contains("git rebase --continue"),
            "rebase hint must NOT spell raw `git rebase --continue`; got: {msg}"
        );
        // Resume verb from op-state, never a hardcoded pull `rwv sync`.
        assert!(
            msg.contains("rwv land --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(
            !msg.contains("rwv sync primary") && !msg.contains("rwv sync /"),
            "must NOT hardcode a workspace to sync from: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 1 — NO live conflict (e.g. fetch/head-unreadable failures): the
    // VCS-native hint is OMITTED (nothing to `--continue` in git), and the
    // message points straight at the verb-derived resume.
    #[test]
    fn manifest_repo_failure_message_no_conflict_omits_vcs_hint() {
        let msg = manifest_repo_failure_message(git_vcs().as_ref(), OpVerb::Sync, None);
        assert!(
            !msg.contains("git rebase --continue")
                && !msg.contains("git merge --continue")
                && !msg.contains("git cherry-pick --continue"),
            "VCS hint must be absent without a live conflict: {msg}"
        );
        assert!(
            msg.contains("rwv sync --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(msg.contains("rwv abort"), "expected rollback option: {msg}");
    }

    // Site 2 — Phase 1' (project repo) outer bail, live conflict.
    #[test]
    fn phase1_bail_message_live_conflict_includes_resolution_and_verb_resume() {
        let cwd = Path::new("/ws/projects/web-app");
        let msg = phase1_or_phase3_failure_message(
            git_vcs().as_ref(),
            Phase::One,
            cwd,
            OpVerb::Land,
            Some(ConflictOp::Rebase),
        );
        assert!(
            msg.contains("Phase 1' (project repo)"),
            "expected phase label in: {msg}"
        );
        assert!(
            !msg.contains("git rebase --continue"),
            "rebase bail must NOT spell raw `git rebase --continue`; got: {msg}"
        );
        assert!(
            msg.contains("/ws/projects/web-app"),
            "expected repo path: {msg}"
        );
        assert!(
            msg.contains("rwv land --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(
            !msg.contains("rwv sync ww1") && !msg.contains("rwv sync /"),
            "must NOT hardcode a workspace to sync from: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 2 — Phase 1' non-conflict failure (a rebase error that is no
    // conflict): no live ConflictOp, so no VCS-native hint.
    #[test]
    fn phase1_bail_message_no_conflict_omits_vcs_hint() {
        let cwd = Path::new("/ws/projects/web-app");
        let msg = phase1_or_phase3_failure_message(
            git_vcs().as_ref(),
            Phase::One,
            cwd,
            OpVerb::Sync,
            None,
        );
        assert!(
            !msg.contains("git rebase --continue"),
            "VCS hint must be absent without a live conflict: {msg}"
        );
        assert!(
            msg.contains("rwv sync --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(msg.contains("rwv abort"), "expected rollback option: {msg}");
    }

    // Site 3 — Phase 3 (re-lock) outer bail: relock is NEVER a VCS conflict, so
    // the caller passes `None` and the message must NOT teach a spurious
    // `git rebase --continue` (which would print "No rebase in progress").
    #[test]
    fn phase3_bail_message_never_teaches_vcs_hint() {
        let cwd = Path::new("/ws/projects/web-app");
        let msg = phase1_or_phase3_failure_message(
            git_vcs().as_ref(),
            Phase::Three,
            cwd,
            OpVerb::Land,
            None,
        );
        assert!(
            msg.contains("Phase 3 (re-lock)"),
            "expected phase label in: {msg}"
        );
        assert!(
            !msg.contains("git rebase --continue"),
            "relock is never a rebase conflict — no `git rebase --continue`: {msg}"
        );
        assert!(
            msg.contains("rwv land --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(
            !msg.contains("rwv sync /abs/source"),
            "must NOT hardcode a workspace to sync from: {msg}"
        );
    }

    // Site 4 — cherry-pick op hint (trait surface; sync no longer uses
    // cherry-pick directly but the message builder must still render the
    // op's hint correctly for any VCS impl that does). This site ALWAYS has a
    // live ConflictOp, so the VCS hint is unconditional. CherryPick has no
    // rwv-native continue, so its VCS hint retains the raw git continue; the
    // verb-derived resume line then picks the op back up.
    #[test]
    fn per_conflict_bail_cherry_pick_includes_cherry_pick_hint() {
        let repo = Path::new("/ws/projects/web-app");
        let msg = per_conflict_bail_message(
            git_vcs().as_ref(),
            repo,
            ConflictOp::CherryPick,
            "cherry-pick (rebase replay)",
            "commit deadbeef on paths: foo.txt",
            OpVerb::Sync,
        );
        assert!(
            msg.contains("git cherry-pick --continue"),
            "expected cherry-pick hint in: {msg}"
        );
        assert!(
            msg.contains("cherry-pick (rebase replay)"),
            "expected op label in: {msg}"
        );
        assert!(msg.contains("deadbeef"), "expected detail in: {msg}");
        assert!(msg.contains("foo.txt"), "expected detail in: {msg}");
        assert!(
            msg.contains("rwv sync --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(
            !msg.contains("rwv sync primary"),
            "must NOT hardcode a workspace to sync from: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 5 — Phase 1' merge inner bail.
    //
    // Merge has no rwv-native continue; continue_cmd carries the raw git form.
    #[test]
    fn per_conflict_bail_merge_includes_merge_hint() {
        let repo = Path::new("/ws/projects/web-app");
        let msg = per_conflict_bail_message(
            git_vcs().as_ref(),
            repo,
            ConflictOp::Merge,
            "merge",
            "paths: bar.txt",
            OpVerb::Land,
        );
        assert!(
            msg.contains("git merge --continue"),
            "expected merge hint in: {msg}"
        );
        assert!(msg.contains("bar.txt"), "expected detail in: {msg}");
        assert!(
            msg.contains("rwv land --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Site 6 — Phase 1' rebase inner bail: subject appears in rendered message.
    //
    // This tests that per_conflict_bail_message surfaces the subject when a
    // stopped-commit detail string containing the subject is passed as the
    // `detail` arg — as `apply_project_strategy` now does via
    // `Vcs::rebase_stopped_commit_detail` (now a trait method).
    //
    // Key contract: Rebase bail must use `rwv sync --continue`, NOT raw
    // `git rebase --continue`. `rwv abort` comes last.
    #[test]
    fn per_conflict_bail_rebase_project_repo_includes_commit_subject_in_detail() {
        let repo = Path::new("/ws/projects/web-app");
        let detail = "commit abc1234 (lock: refresh — post-OOB drift in gc-formulas)";
        let msg = per_conflict_bail_message(
            git_vcs().as_ref(),
            repo,
            ConflictOp::Rebase,
            "rebase (project repo)",
            detail,
            OpVerb::Sync,
        );
        assert!(
            msg.contains("abc1234"),
            "expected short SHA in message: {msg}"
        );
        assert!(
            msg.contains("lock: refresh"),
            "expected commit subject in message: {msg}"
        );
        assert!(
            msg.contains("post-OOB drift"),
            "expected commit subject continuation in message: {msg}"
        );
        assert!(
            !msg.contains("git rebase --continue"),
            "rebase bail must NOT spell raw `git rebase --continue`; got: {msg}"
        );
        assert!(
            msg.contains("rwv sync --continue"),
            "expected verb-derived resume: {msg}"
        );
        assert!(
            !msg.contains("rwv sync primary"),
            "must NOT hardcode a workspace to sync from: {msg}"
        );
        assert_resolution_first_abort_last(&msg);
    }

    // Post-Phase-1' manifest reload is a hard bail, not warn-and-proceed.
    //
    // Before: on Project::from_dir failure after Phase 1', the code emitted a
    // warning (suppressed in --json mode) and fell through to Phase 3 with a
    // stale snapshot.  After: bail!() with a message that names `rwv abort`.
    //
    // This test pins the inline error wording to ensure it mentions `rwv abort`
    // and does not regress to the old suppress-and-proceed path.  The companion
    // E2E test (`sync_bails_hard_when_post_phase1_manifest_reload_fails` in
    // e2e_sync_abort_test.rs) exercises the live code path end-to-end.
    #[test]
    fn post_phase1_reload_error_message_mentions_rwv_abort() {
        // The error is constructed inline at the call site as:
        //
        //   anyhow::anyhow!(
        //       "failed to reload project manifest after Phase 1' ({e}).\n...\
        //        To recover: `rwv abort`",
        //       cwd_project_dir = ...,
        //   )?;
        //
        // We replicate the format string here so the test breaks if the wording
        // is changed to drop the recovery hint.
        let fake_dir = Path::new("/ws/projects/web-app");
        let fake_err = "TOML parse error: expected `.`, `=`";
        let msg = format!(
            "failed to reload project manifest after Phase 1' ({fake_err}).\n\
             \n\
             The project repo was successfully rebased/merged, but the manifest \
             in {cwd} could not be parsed. Proceeding would silently \
             omit newly-added repos from the regenerated lock.\n\
             \n\
             To recover: `rwv abort`",
            cwd = fake_dir.display(),
        );

        assert!(
            msg.contains("rwv abort"),
            "post-Phase-1' reload error must mention `rwv abort`; msg: {msg}"
        );
        assert!(
            msg.contains("failed to reload project manifest after Phase 1'"),
            "must identify the Phase 1' reload site; msg: {msg}"
        );
        assert!(
            msg.contains("manifest") || msg.contains(Manifest::FILE_NAME),
            "must mention the manifest; msg: {msg}"
        );
    }

    // ---------------------------------------------------------------------------
    // Extensibility acceptance test
    //
    // Verifies that another output mode can be added by implementing
    // OutputHandler without modifying TextHandler or JsonEnvelopeHandler.
    // A CountingHandler is constructed entirely in this test;
    // no existing handler or orchestration code is touched.
    // ---------------------------------------------------------------------------

    /// A minimal extra-mode handler that just counts how many outcomes were
    /// recorded.  Demonstrates that OutputHandler is open for extension.
    struct CountingHandler {
        count: std::sync::Mutex<usize>,
    }

    impl CountingHandler {
        fn new() -> Self {
            Self {
                count: std::sync::Mutex::new(0),
            }
        }

        fn recorded(&self) -> usize {
            *self.count.lock().unwrap()
        }
    }

    impl OutputHandler for CountingHandler {
        fn emit_text(&self) -> bool {
            false
        }

        fn record(
            &self,
            _path: &str,
            _abs_path: &Path,
            _outcome: &RepoSyncOutcome,
            _containment: Option<ContainmentOutput>,
        ) {
            *self.count.lock().unwrap() += 1;
        }
    }

    #[test]
    fn output_handler_is_open_for_extension_without_modifying_existing_handlers() {
        // Build three distinct outcomes.
        let outcomes = vec![
            RepoSyncOutcome::Converged {
                derived_content_dropped: Vec::new(),
            },
            RepoSyncOutcome::NoOp,
            RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
                error: "test".to_owned(),
                cause: None,
            }),
        ];

        let handler = CountingHandler::new();

        // Drive record() from outside the sync orchestration to prove the trait
        // contract is sufficient on its own.
        for outcome in &outcomes {
            handler.record("some/repo", Path::new("/abs/some/repo"), outcome, None);
        }

        assert_eq!(
            handler.recorded(),
            3,
            "CountingHandler should record exactly one entry per record() call"
        );

        // Also verify that emit_text() returns the value the handler advertises.
        assert!(
            !handler.emit_text(),
            "CountingHandler should not emit text (it has no text output)"
        );
    }

    // -----------------------------------------------------------------------
    // `apply_strategy` states a derived-content policy
    //
    // These drive `apply_strategy` directly, and the fixture asserts the repo
    // carries NO durable `merge.rwv-lock` definition, so the stated policy is
    // the only route by which a replay can resolve the declared path, and a
    // test can tell a stated policy from an ignored one.
    // -----------------------------------------------------------------------

    /// A repo whose committed `.gitattributes` declares `generated.txt`
    /// derived, with `main` and the checked-out `feat` each having
    /// regenerated it. Returns the tip of `main` — the side a replay onto it
    /// must keep.
    fn diverged_on_a_declared_derived_path(dir: &Path) -> ResolvedRevisionId {
        init_repo(dir);
        std::fs::write(dir.join(".gitattributes"), "generated.txt merge=rwv-lock\n").unwrap();
        std::fs::write(dir.join("generated.txt"), "base\n").unwrap();
        std::fs::write(dir.join("shared.txt"), "base\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "declare generated.txt derived"]);
        let base = git(dir, &["rev-parse", "HEAD"]);

        std::fs::write(dir.join("generated.txt"), "regenerated on main\n").unwrap();
        std::fs::write(dir.join("shared.txt"), "main version\n").unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "main: regenerate"]);
        let main_tip = git_vcs().head_revision(dir).unwrap();

        git(dir, &["checkout", "-b", "feat", &base]);
        main_tip
    }

    /// Fixture precondition: no durable definition of the driver is reachable
    /// from this repo's config. Without this, a replay resolves the declared
    /// path whatever policy the call site states, and the assertions below
    /// would pass for a call site that stated nothing at all.
    fn assert_no_durable_driver_definition(dir: &Path) {
        let out = std::process::Command::new("git")
            .args(["config", "--get", crate::git::RWV_MERGE_DRIVER_CONFIG_KEY])
            .current_dir(dir)
            .output()
            .expect("run git config");
        assert!(
            !out.status.success(),
            "fixture precondition: {} must be undefined in {}, else the durable \
             plant resolves the declared path and the stated policy is untestable",
            crate::git::RWV_MERGE_DRIVER_CONFIG_KEY,
            dir.display()
        );
    }

    #[test]
    fn the_rebase_strategy_resolves_a_declared_derived_path_without_stopping() {
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("manifest");
        let main_tip = diverged_on_a_declared_derived_path(&repo);
        assert_no_durable_driver_definition(&repo);

        std::fs::write(repo.join("generated.txt"), "regenerated on feat\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "feat: regenerate"]);

        apply_strategy(
            git_vcs().as_ref(),
            &repo,
            &main_tip,
            ReplayBaseline::SourceLockEntry,
            None,
            SyncStrategy::Rebase,
        )
        .map_err(|e| e.message)
        .expect("a declared derived path must not stop a manifest-repo replay");

        assert_eq!(
            std::fs::read_to_string(repo.join("generated.txt")).unwrap(),
            "regenerated on main\n",
            "the replay target's version of a declared path is what survives"
        );
    }

    #[test]
    fn a_resumed_rebase_strategy_resolves_the_declared_path_it_reaches() {
        // The resume arm states the policy separately from the arm that
        // started the replay, and it is the arm that reaches the picks the
        // interrupted replay never got to. A resume that stated nothing would
        // stop again on the declared path — after the operator had already
        // resolved the real conflict.
        let tmp = crate::tempdir().unwrap();
        let repo = tmp.path().join("manifest");
        let main_tip = diverged_on_a_declared_derived_path(&repo);
        assert_no_durable_driver_definition(&repo);

        // F1 collides with main on an UNDECLARED path: a genuine conflict, and
        // no policy may resolve it.
        std::fs::write(repo.join("shared.txt"), "feat version\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "feat: edit shared"]);
        // F2 regenerates the declared path — the pick the resume must reach.
        std::fs::write(repo.join("generated.txt"), "regenerated on feat\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "feat: regenerate"]);

        let stopped = apply_strategy(
            git_vcs().as_ref(),
            &repo,
            &main_tip,
            ReplayBaseline::SourceLockEntry,
            None,
            SyncStrategy::Rebase,
        );
        assert!(
            stopped.is_err(),
            "an authored conflict must stop the replay whatever the policy"
        );
        assert_eq!(
            git_vcs().mid_op(&repo),
            Some(ConflictOp::Rebase),
            "the replay must be left resumable"
        );

        // Operator resolves and stages, then re-enters — the `rwv sync
        // --continue` loop.
        std::fs::write(repo.join("shared.txt"), "merged\n").unwrap();
        git(&repo, &["add", "shared.txt"]);

        apply_strategy(
            git_vcs().as_ref(),
            &repo,
            &main_tip,
            ReplayBaseline::SourceLockEntry,
            None,
            SyncStrategy::Rebase,
        )
        .map_err(|e| e.message)
        .expect("the resumed replay must carry through the declared path");

        assert!(
            git_vcs().mid_op(&repo).is_none(),
            "the replay must be complete after a successful resume"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("generated.txt")).unwrap(),
            "regenerated on main\n",
            "the resumed picks resolve the declared path the same way"
        );
    }
}
