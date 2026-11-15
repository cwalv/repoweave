//! Op-state files: multi-workspace operation tracking for the verbs that stop
//! and resume (`sync`, `land`, `pull`, `update --remote`, `cherry-pick`).
//!
//! # Schema v8 (no back-compat with v7)
//!
//! A record carries its `schema`. One that carries another, or none, or that
//! does not read whole, is refused by every reader, naming the rwv that wrote
//! it: it is finished or aborted with that rwv, or ended by `rwv abort
//! --discard-unreadable-record`. Do not add older-schema parsing.
//!
//! ## Owner record (`.rwv-op`)
//!
//! Written at the **initiating workspace** (the owner). Holds all op parameters
//! plus the current phase. It is the sole copy of mutable op state.
//!
//! `id` is the op id, shared with savepoint refs. `project` is the project
//! both `source` and `target` are resolved under. `verb` names the verb;
//! `history` is the one-run `--history` (`linear`, `merge`,
//! `ff-only`), or `null`; `phase` is `replay`, `relock`,
//! `advance-target`, or `retire`. `converged_tips` is each repo's tip as the
//! replay left it, written at relock completion and empty before. `take_in`
//! is the verb's plan and the journal of what it moved.
//!
//! ## Thin lease (`.rwv-op-lease`)
//!
//! Written at every **other workspace the op mutates** (never at the owner).
//! Immutable once written; a mutex plus a redirect, nothing else.
//!
//! ```json
//! {
//!   "id": "1779769917405921588",
//!   "owner": "/abs/path/to/owner/workspace",
//!   "created_at": "2026-06-10T21:14:03Z"
//! }
//! ```
//!
//! ## Read-only workspaces
//!
//! Not marked. Safe because source reads are snapshots: a read resolves its
//! content at a revision, never from the working tree, so a concurrent write
//! to that workspace cannot tear it.
//!
//! ## Acquisition atomicity
//!
//! The owner record and every touched-workspace lease
//! are acquired **atomically at guard time** via [`acquire_op`]: each file is
//! published with `crate::durable_file::create_new`, and on `AlreadyExists`
//! anywhere the acquisition unwinds any partial state it created and returns
//! the standard in-flight refusal. This closes the guard→mark TOCTOU window
//! that a plain [`check_no_op_in_progress`] would leave: two concurrent ops
//! could otherwise both pass a `check_no_op_in_progress` guard and only collide
//! later at the git layer. The acquired handle carries the touched-workspace
//! set so a precondition refusal after acquisition can call
//! [`release_acquired`] and clear its records per the cleanup table below.
//!
//! ## Cleanup ownership
//!
//! | Exit path | Record + leases |
//! |---|---|
//! | Success / precondition refusal | Cleared everywhere |
//! | Phase failure / crash | Kept everywhere (`--continue` and `abort` remain) |
//! | `abort` | Cleared after restore |
//!
//! ## File locations
//!
//! Both files live at the workspace root (same directory as `.rwv-active`).

use crate::durable_file::CreateNewError;
use crate::manifest::{ProjectName, RepoKey};
use crate::path_spelling::CanonicalPath;
use crate::refusal::RefusalKind;
use crate::state_file::{StateFile, StatePath};
use crate::vcs::{CommitId, RawRefName};
use crate::wire_token::wire_token;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// The schema every record this release writes carries.
pub(crate) const OP_RECORD_SCHEMA: u32 = 8;

/// Name of the owner op-state file at the initiating workspace.
pub(crate) const OP_STATE_FILE: &str = ".rwv-op";

/// Name of the thin-lease file at every other mutated workspace.
pub(crate) const OP_LEASE_FILE: &str = ".rwv-op-lease";

// ---------------------------------------------------------------------------
// OpId — newtype for sync operation identifiers
// ---------------------------------------------------------------------------

/// A nanosecond-resolution identifier for one in-flight sync operation.
///
/// Used to namespace pre-op savepoint refs so concurrent or interleaved
/// sync attempts don't collide.
///
/// Minted by [`OpId::new_now`] or parsed from text by [`FromStr`], which
/// refuses what no op id can be: empty text, a character a ref name cannot
/// carry, and anything ending in `-target`. The last is what keeps a
/// [`SavepointKey`] injective: the target side of an op `x` is spelled
/// `x-target`, so an op id `x-target` would file its own savepoints where
/// another op's target side lives.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OpId(String);

/// What a [`SavepointKey`] appends to an op id to name the target side of a
/// `land`. No [`OpId`] ends with it.
const TARGET_SUFFIX: &str = "-target";

/// What a [`SavepointKey`] appends to an op id to name a checkout a `land`
/// fast-forwards in place, outside both its workspaces. No [`OpId`] ends
/// with it.
const IN_PLACE_SUFFIX: &str = "-in-place";

/// Text that is not an op id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpIdError {
    text: String,
}

impl fmt::Display for OpIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is not an op id: an op id is one or more ASCII letters, digits, `-` or `_`, \
             and does not end in `{TARGET_SUFFIX}` or `{IN_PLACE_SUFFIX}`, which name the other \
             sides of a savepoint",
            self.text
        )
    }
}

impl std::error::Error for OpIdError {}

impl OpId {
    /// Generate a fresh `OpId` from the current wall-clock time.
    ///
    /// Panics if the system clock is before UNIX_EPOCH. The previous
    /// fallback to a literal "0" sentinel masked a clock invariant: every
    /// pre-epoch run would collide on a single `OpId`, and the savepoint
    /// ref scheme this id keys depends on uniqueness. Per FP-in-Rust:
    /// don't silently default away an invariant.
    pub fn new_now() -> Self {
        let s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before UNIX_EPOCH")
            .as_nanos()
            .to_string();
        Self(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for OpId {
    type Err = OpIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let spellable = !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if spellable && !s.ends_with(TARGET_SUFFIX) && !s.ends_with(IN_PLACE_SUFFIX) {
            Ok(Self(s.to_owned()))
        } else {
            Err(OpIdError { text: s.to_owned() })
        }
    }
}

impl TryFrom<String> for OpId {
    type Error = OpIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<OpId> for String {
    fn from(id: OpId) -> Self {
        id.0
    }
}

impl fmt::Display for OpId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The name an op's savepoint is filed under.
///
/// A `land` savepoints two workspaces, and the target's repos may share a
/// ref store with the owner's when both are worktrees of one clone. Filed
/// under the same name, the first restore of `rwv abort` would drop the ref
/// the second needs, so the target side has a namespace of its own. Each
/// variant carries the op, and nothing else can name a namespace.
///
/// Its text form is the ref's leaf: `<op id>` and `<op id>-target`. Parsing
/// reads back exactly what [`fmt::Display`] wrote, which holds because no
/// [`OpId`] ends in the target suffix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SavepointKey {
    /// The workspace the op replayed in.
    Op(OpId),
    /// The workspace a `land` advances.
    LandTarget(OpId),
    /// A checkout outside both of a `land`'s workspaces that holds a
    /// receiving branch, which the landing fast-forwards in place.
    LandInPlace(OpId),
}

impl SavepointKey {
    /// The op this savepoint belongs to, on either side.
    pub fn op(&self) -> &OpId {
        match self {
            Self::Op(op) | Self::LandTarget(op) | Self::LandInPlace(op) => op,
        }
    }
}

impl fmt::Display for SavepointKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Op(op) => write!(f, "{op}"),
            Self::LandTarget(op) => write!(f, "{op}{TARGET_SUFFIX}"),
            Self::LandInPlace(op) => write!(f, "{op}{IN_PLACE_SUFFIX}"),
        }
    }
}

impl FromStr for SavepointKey {
    type Err = OpIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let side = |op: &str, as_key: fn(OpId) -> Self| {
            op.parse()
                .map(as_key)
                .map_err(|_| OpIdError { text: s.to_owned() })
        };
        match (
            s.strip_suffix(TARGET_SUFFIX),
            s.strip_suffix(IN_PLACE_SUFFIX),
        ) {
            (Some(op), _) => side(op, Self::LandTarget),
            (_, Some(op)) => side(op, Self::LandInPlace),
            (None, None) => s.parse().map(Self::Op),
        }
    }
}

/// The operator-facing resume command for a mid-op verb.
///
/// A failed op is resumed with `--continue` from the OWNING workspace, which
/// reads every parameter (source, `--history`, target, retire) back
/// out of op-state — so the resume text is `rwv {verb} --continue`, NEVER a
/// hardcoded `rwv sync {source}`. Hardcoding `rwv sync` was wrong two ways:
/// it named the PULL verb for a `land` (landing) op, and it re-supplied
/// arguments the record already holds. The verb comes from the op's `verb`
/// field ([`OpVerb`]) so the string is derived from op-state, not from a guess
/// made where the message is written.
pub fn resume_command(verb: OpVerb) -> String {
    let words = match verb {
        OpVerb::UpdateRemote => "update --remote".to_owned(),
        other => other.to_string(),
    };
    format!("rwv {words} --continue")
}

/// [`resume_command`] as a line to run against `workspace`, the one the
/// operation is owned by.
pub(crate) fn resume_command_at(verb: OpVerb, workspace: &std::path::Path) -> String {
    crate::workspace::addressed(workspace, &resume_command(verb))
}

/// `rwv abort` as a line to run against `workspace`.
pub(crate) fn abort_command_at(workspace: &std::path::Path) -> String {
    crate::workspace::rwv_line(workspace, "abort")
}

// ---------------------------------------------------------------------------
// OpVerb — which top-level verb started this op
// ---------------------------------------------------------------------------

/// Which top-level verb started this op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OpVerb {
    /// `rwv sync`: a feature takes in its target, or a branch merges `--from`.
    Sync,
    /// `rwv land`: compose in the source, then fast-forward the target.
    Land,
    /// `rwv pull` with `--merge-upstream` or `--reset-to-upstream`.
    Pull,
    /// `rwv update --remote` with `--merge-upstream` or `--reset-to-upstream`.
    UpdateRemote,
    /// `rwv cherry-pick`: copy each member's range of a project commit, then
    /// the project commit with the relock.
    CherryPick,
}

impl std::fmt::Display for OpVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&wire_token(self))
    }
}

// ---------------------------------------------------------------------------
// OpPhase — current phase in the operation lifecycle
// ---------------------------------------------------------------------------

/// Current phase of the in-flight operation.
///
/// Phases are listed in execution order.
///
/// ```text
/// guard → mark → savepoint → replay → relock → advance-target → retire → cleanup
///                                               (land only)   (--retire only)
/// ```
///
/// The persisted phase is always the phase in progress. Every phase is
/// idempotent and re-runnable from the record alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OpPhase {
    /// Project repo, then member repos: Phase 1' and Phase 2.
    Replay,
    /// Regenerate and commit `rwv.lock` (today's Phase 3). On completion,
    /// converged tips are written into the owner record.
    Relock,
    /// FF-advance every target repo to its converged tip (land only).
    AdvanceTarget,
    /// Merged-check then workweave removal (`--retire` only).
    Retire,
}

impl std::fmt::Display for OpPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&wire_token(self))
    }
}

/// What the verb was asked to do and what it has moved, kept so `--continue`
/// re-applies the plan on the tips the first run planned and `rwv abort` puts
/// back what the journal names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TakeInRecord {
    pub merge_upstream: bool,
    /// The repos named by `--reset-to-upstream`, spelled as an operand.
    pub reset_to_upstream: Vec<String>,
    pub roles: Vec<String>,
    pub repos: Vec<String>,
    pub dirty: bool,
    pub commit: bool,
    pub message: Option<String>,
    pub no_materialize: bool,
    /// Each repo's upstream tip as the op's fetch left it.
    pub upstream_tips: BTreeMap<RepoKey, CommitId>,
    /// Each branch the op moved as a ref, where it was and where it went.
    pub ref_moves: Vec<RefMove>,
    /// The branch a `sync` takes in: its target, or `--from`'s branch.
    #[serde(default)]
    pub sync_from: Option<SyncFrom>,
    /// The repos `sync --reset-to-target` names, spelled as an operand.
    #[serde(default)]
    pub reset_to_target: Vec<String>,
    /// The source's project tip a landing composes from.
    #[serde(default)]
    pub source_tip: Option<CommitId>,
    /// The tip of each member the source's lock was behind when a landing
    /// began, which its project commit records: read once, before anything
    /// moved.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub relocked: BTreeMap<crate::manifest::RepoPath, CommitId>,
    /// The repos `land --allow-member-change` names, spelled as an operand.
    #[serde(default)]
    pub allow_member_change: Vec<String>,
    /// A landing's plan: for `T`'s project branch and each planned member,
    /// its receiving branch, that branch's tip as the landing read it, and
    /// how the landing moves it.
    #[serde(default)]
    pub receiving: BTreeMap<RepoKey, Receiving>,
    /// The members of `T`'s workspace a landing moves only by re-pinning
    /// them, which aligning `T`'s workspace then moves.
    #[serde(default)]
    pub realigned: Vec<RepoKey>,
    /// Each checkout of `T`'s workspace a landing moves, by its fast-forward
    /// or by aligning that workspace after it, written before it moves and
    /// taken back where it did not move.
    #[serde(default)]
    pub moved_in_target: Vec<RepoKey>,
    /// The tip each checkout `moved_in_target` names is moved to.
    #[serde(default)]
    pub wrote_in_target: BTreeMap<RepoKey, CommitId>,
    /// Each member checkout of `T`'s workspace a landing makes, by its
    /// fast-forward or by aligning that workspace, written before it is
    /// made.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub made_in_target: Vec<crate::manifest::RepoPath>,
    /// Each checkout a landing fast-forwards in place in a third workspace,
    /// written before it moves and taken back where it did not move.
    #[serde(default)]
    pub moved_in_place: Vec<InPlaceMove>,
    /// Each branch a landing moves as a ref, and the tip it moves it to,
    /// written before it moves.
    #[serde(default)]
    pub moved_refs: Vec<RefWrite>,
    /// Each branch a landing created, and where.
    #[serde(default)]
    pub created_refs: Vec<CreatedRef>,
    /// For each declared-branch change a landing makes, the commits the
    /// target's pin reaches that the new receiving branch lacks.
    #[serde(default)]
    pub left_behind: Vec<LeftBehind>,
    /// What the landing reported as it moved members, so a resumed landing
    /// reports it again.
    #[serde(default)]
    pub reports: Vec<LandingReport>,
    /// What a `cherry-pick` ports, and how far it got.
    #[serde(default)]
    pub pick: Option<PickRecord>,
    /// The repos the op integrates by a merge commit rather than a rebase.
    #[serde(default)]
    pub merged: Vec<RepoKey>,
    /// The backups the op made: `rebased` ones of branches it found
    /// unpublished, and each tip a branch moved to during a stop. They go
    /// when it completes; an abort keeps and names each one the restored
    /// branch does not reach.
    #[serde(default)]
    pub backups: Vec<KeptBackup>,
    /// Each merge the op started, by its repo and the commit it merges in,
    /// written before it starts.
    #[serde(default)]
    pub merges: Vec<Merging>,
    /// `rwv abort` began restoring: written before it moves anything, so a
    /// refused or interrupted abort leaves a record only `rwv abort` takes up.
    #[serde(default)]
    pub abort_begun: bool,
    /// The retire was judged and refused, and the process that ran it ended:
    /// what lets `rwv abort` typed in the target's workspace take up a
    /// landing that released it there. A retire killed leaves it unset.
    #[serde(default)]
    pub retire_stopped: bool,
    /// Each rebase the op starts, written before it starts, so the replay can
    /// be read back from the commits once it ends, by this process or by hand.
    #[serde(default)]
    pub rebases: Vec<Rebasing>,
    /// The project rebase whose replayed tip's `rwv.toml` the operator
    /// settled at the stop where it differed from the merge of the values:
    /// that tip is not judged again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip_settled: Option<Rebasing>,
    /// The stop the op is at with `rwv.toml` unmerged, journaled as it is
    /// entered and cleared once a continue takes it past.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_stop: Option<ManifestStop>,
    /// Each dependent's branch the op carries onto its target's new tip,
    /// written before its replay starts, with the tip the replay composed
    /// once it ends; the branch itself moves only once every carry composed.
    #[serde(default)]
    pub carries: Vec<Carrying>,
}

/// A stop at which `rwv.toml` is unmerged in the project checkout, and its
/// working file as the stop left it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestStop {
    pub kind: ManifestStopKind,
    pub working: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManifestStopKind {
    /// The VCS merged the text with no conflict and the check stopped it.
    Checked,
    /// The VCS left regions in conflict.
    Conflicted,
}

/// A merge of `source` into the checkout of `repo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Merging {
    pub repo: RepoKey,
    pub source: CommitId,
}

/// A rebase of the branch `branch` in `repo`, the commits `base..from`
/// replayed onto `onto`; a branch moved to `onto` with nothing replayed has
/// `base` at `from`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rebasing {
    pub repo: RepoKey,
    pub branch: String,
    pub base: CommitId,
    pub from: CommitId,
    pub onto: CommitId,
}

/// A dependent's branch `branch` in `repo` carried along: the commits
/// `base..from` replayed onto `onto`, composed at `composed` once the replay
/// ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Carrying {
    pub repo: RepoKey,
    pub branch: String,
    /// The branch the checkout the carry replays in was on, which it is put
    /// back on once the replay ends.
    pub checkout_on: String,
    pub base: CommitId,
    pub from: CommitId,
    pub onto: CommitId,
    pub composed: Option<CommitId>,
    /// The pins the carry's project replay moved onto what the member
    /// rebases made of the commits they pinned.
    pub remapped: Vec<CarryRemap>,
}

/// A pin of `member` a carried project commit held on a dropped commit, moved
/// from `was` to `now`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarryRemap {
    pub member: crate::manifest::RepoPath,
    pub was: CommitId,
    pub now: CommitId,
}

impl Carrying {
    /// The carry's replay as a rebase of `branch`.
    pub fn rebasing(&self) -> Rebasing {
        Rebasing {
            repo: self.repo.clone(),
            branch: self.branch.clone(),
            base: self.base.clone(),
            from: self.from.clone(),
            onto: self.onto.clone(),
        }
    }
}

/// One move a landing reports as it makes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum LandingReport {
    /// A receiving branch that existed nowhere, created at `at`.
    Created {
        repo: RepoKey,
        branch: String,
        at: CommitId,
    },
    /// A member holding no new work moved to its receiving branch's tip `to`,
    /// the tip `was` it left kept in `backup`.
    Followed {
        repo: RepoKey,
        to: CommitId,
        was: CommitId,
        backup: String,
    },
    /// A range holding the merge `merge` that a rebasing setting integrated
    /// by a merge commit instead.
    Merged { repo: RepoKey, merge: CommitId },
    /// A branch a rebase rewrote, its tip `was` kept in the `rebased` backup
    /// `backup`.
    Rebased {
        repo: RepoKey,
        was: CommitId,
        backup: String,
    },
    /// A dependent's branch carried along the rebase, from `was` to `now`,
    /// the tip it left kept in the `rebased` backup `backup`, which stays once
    /// the op ends where `outlives_op`.
    Carried {
        repo: RepoKey,
        branch: String,
        was: CommitId,
        now: CommitId,
        backup: String,
        outlives_op: bool,
    },
    /// A pin a replayed project commit held on a commit the rebase dropped,
    /// moved to `now`, what the member's rebase made of the commits before it.
    Remapped {
        repo: RepoKey,
        was: CommitId,
        now: CommitId,
    },
    /// A pin moved backward by name.
    Repinned {
        repo: RepoKey,
        was: CommitId,
        now: CommitId,
    },
    /// A field of the target's entry for `repo`, or of its top-level
    /// settings, changed by name from `was` to `now`.
    EntryChanged {
        repo: RepoKey,
        field: String,
        was: String,
        now: String,
    },
    /// The target's entry for `repo` removed by name.
    EntryRemoved { repo: RepoKey },
    /// `branch`, behind its upstream, taken in from `from` to `to`: the
    /// upstream's commits, and each pin and member entry they changed.
    TakenIn {
        branch: String,
        from: CommitId,
        to: CommitId,
        commits: Vec<crate::vcs::CommitSummary>,
        pins: Vec<PinTakenIn>,
        entries: Vec<EntryTakenIn>,
    },
    /// Shared-ref drift refreshed in the checkout at `checkout` before its
    /// edits were judged: its branch moved from `moved_from` to `moved_to`
    /// outside it, and `files` and `recorded_only` (what the VCS records alone) still held `moved_from`'s
    /// content.
    Refreshed {
        repo: RepoKey,
        checkout: PathBuf,
        moved_from: CommitId,
        moved_to: CommitId,
        files: Vec<String>,
        recorded_only: Vec<String>,
    },
    /// The commits `repo`'s new pin holds beyond the pin `target` held, which
    /// the landing pins into `target` with the feature's work.
    BeyondPin {
        repo: RepoKey,
        target: String,
        commits: Vec<crate::vcs::CommitSummary>,
    },
    /// A pin of `repo` that lock commit `lock` of the target's history held,
    /// which this clone lacked, so it did not decide the base.
    HistoryPinAbsent {
        repo: RepoKey,
        lock: CommitId,
        pin: CommitId,
    },
    /// `branch` holds the landing and its push branch does not, so the
    /// landing is not published until it is pushed from `workspace`, where
    /// the project repo is `label`.
    PushBranchAhead {
        repo: RepoKey,
        label: String,
        branch: String,
        /// The workspace that has the branch checked out, none where it
        /// landed as a ref.
        workspace: Option<PathBuf>,
    },
}

impl TakeInRecord {
    /// Whether the landing has begun writing the target: any checkout, ref
    /// or branch is journaled.
    pub fn wrote_target(&self) -> bool {
        !self.moved_in_target.is_empty()
            || !self.moved_in_place.is_empty()
            || !self.moved_refs.is_empty()
            || !self.created_refs.is_empty()
    }

    /// Each workspace a landing fast-forwards a checkout of in place, outside
    /// both of its own, and leases.
    pub fn in_place_workspaces(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for receiving in self.receiving.values() {
            if let Moves::InPlace { workspace } = &receiving.moves {
                if !out.contains(workspace) {
                    out.push(workspace.clone());
                }
            }
        }
        out
    }

    /// Each checkout the landing moved in place in `workspace`: the repo,
    /// and the tip it wrote there, `None` where it has not moved it yet.
    pub fn in_place_moves<'a>(
        &'a self,
        workspace: &'a Path,
    ) -> impl Iterator<Item = (&'a RepoKey, Option<&'a CommitId>)> + 'a {
        self.receiving
            .iter()
            .filter(move |(_, receiving)| {
                matches!(&receiving.moves, Moves::InPlace { workspace: at } if at == workspace)
            })
            .map(move |(repo, _)| {
                (
                    repo,
                    self.moved_in_place
                        .iter()
                        .find(|moved| moved.workspace == workspace && moved.repo == *repo)
                        .and_then(|moved| moved.wrote.as_ref()),
                )
            })
    }
}

impl LandingReport {
    pub fn is_entry_change(&self) -> bool {
        matches!(self, Self::EntryChanged { .. } | Self::EntryRemoved { .. })
    }
}

/// A member's pin as a landing's take-in of `T`'s upstream changed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinTakenIn {
    pub repo: RepoKey,
    pub was: Option<String>,
    pub now: Option<String>,
    /// `now` does not descend from `was`.
    pub backward: bool,
}

/// A member's entry as a landing's take-in of `T`'s upstream changed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryTakenIn {
    pub repo: RepoKey,
    pub change: crate::pull::EntryChange,
    /// Each field that differs, as `<field>: <was> -> <now>`.
    pub fields: Vec<String>,
}

/// The commits a declared-branch change leaves on the old branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeftBehind {
    pub repo: RepoKey,
    pub vcs_type: crate::manifest::VcsType,
    pub was: String,
    pub now: String,
    /// Whether the commits listed are those the feature's pin reaches, the
    /// landing keeping the target's declared branch; else the target's pin's.
    pub feature_pin: bool,
    pub commits: Vec<crate::vcs::CommitSummary>,
    /// Where the new branch is checked out once the landing aligns the
    /// workspace on the target; none where the target is checked out nowhere.
    pub checkout: Option<PathBuf>,
}

/// The port a `cherry-pick` planned, and each member's copies so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickRecord {
    /// The project commit ported.
    pub rev: CommitId,
    /// The parent it is ported against, from 1, where it is a merge.
    pub mainline: Option<usize>,
    /// The repos `--except` names, spelled as an operand.
    pub except: Vec<String>,
    /// Each member's copies, in the order they run.
    pub members: Vec<MemberPick>,
    /// Each `dependency` or `reference` member's carried pin.
    pub carried: BTreeMap<RepoKey, CommitId>,
    /// Each excepted member's tip, recorded as it stands.
    pub excepted: BTreeMap<RepoKey, CommitId>,
    /// The project repo's tip the port was planned on.
    pub head: CommitId,
    /// The project commit's copy may have begun: recorded before it starts.
    pub project_started: bool,
}

/// One member's copies: the commits to copy, oldest first, each against
/// `mainline` where set, and what each copy came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberPick {
    pub repo: RepoKey,
    pub commits: Vec<CommitId>,
    pub mainline: Option<usize>,
    pub copied: Vec<(CommitId, CommitId)>,
    pub dropped: Vec<CommitId>,
    /// The commits of the range the branch already held, not copied.
    pub left_out: Vec<CommitId>,
    /// The branch the copies go onto.
    pub branch: String,
    /// The checkout's tip the port was planned on.
    pub start: CommitId,
    /// The copy of the first remaining commit may have begun: recorded
    /// before it starts.
    pub copying: bool,
}

impl MemberPick {
    /// The tip the checkout holds once the copies so far are made.
    pub fn expected(&self) -> &CommitId {
        self.copied.last().map_or(&self.start, |(_, to)| to)
    }

    /// The commits not copied or dropped yet.
    pub fn remaining(&self) -> &[CommitId] {
        &self.commits[(self.copied.len() + self.dropped.len()).min(self.commits.len())..]
    }
}

/// A backup an op made, by the repo holding it and the name it is dropped by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeptBackup {
    pub repo: RepoKey,
    pub name: String,
    /// The backup outlives the op's completion, and goes only with an abort,
    /// which puts the branch back at its tip: the branch was published when
    /// it was kept (until the republish replaces the push branch), or the
    /// replay dropped a commit of it, which no rebased commit holds.
    pub outlives_op: bool,
}

/// A receiving branch of a landing: `branch` in `T`'s workspace, its tip as
/// the landing read it, and how the landing moves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receiving {
    pub branch: String,
    pub tip: CommitId,
    pub moves: Moves,
}

/// How a landing moves a receiving branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub enum Moves {
    /// The member is composed onto `tip`, then the branch fast-forwards in
    /// `T`'s workspace's checkout, which must be on it.
    InCheckout,
    /// The member is composed onto `tip`, then the branch fast-forwards in
    /// place in the clean checkout `workspace` has of the member, outside
    /// both of the landing's workspaces, which the landing leases.
    InPlace { workspace: PathBuf },
    /// The member is composed onto `tip`, then the branch, which no checkout
    /// holds, moves as a ref; it is created at `tip` first when `create`.
    AsRef { create: bool },
    /// A member `T` lacks enters at `tip`, its pin in the feature: the branch
    /// is created there, and nothing is composed.
    Enters,
    /// The member takes `tip`, merging nothing; the branch does not move,
    /// and is created at `tip` first when `create`.
    Follows { create: bool },
}

/// A checkout a landing fast-forwards in place in `workspace`, and the tip it
/// writes there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InPlaceMove {
    pub workspace: PathBuf,
    pub repo: RepoKey,
    pub wrote: Option<CommitId>,
}

/// A branch a landing moves as a ref, and the tip it writes there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefWrite {
    pub repo: RepoKey,
    pub branch: String,
    pub to: CommitId,
}

/// A branch the op created, checked out nowhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatedRef {
    pub repo: RepoKey,
    pub branch: String,
    pub at: CommitId,
}

/// What a `sync` takes in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "branch",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum SyncFrom {
    /// The record's target, or `--target`'s.
    Target(String),
    /// `--from`'s branch, merged whatever the setting.
    From(String),
}

/// A branch checked out nowhere that the op moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefMove {
    pub repo: RepoKey,
    pub branch: String,
    pub from: CommitId,
    pub to: CommitId,
}

// ---------------------------------------------------------------------------
// OwnerRecord — the full op record at the initiating workspace
// ---------------------------------------------------------------------------

/// The owner op record written to `.rwv-op` at the initiating workspace.
///
/// All path fields are absolute. `started_at` is RFC3339 UTC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "WireOwnerRecord", into = "WireOwnerRecord")]
pub struct OwnerRecord {
    /// Unique operation identifier (nanosecond wall-clock string). Shared
    /// with savepoint refs and lease files.
    pub id: OpId,
    /// Which verb started this op.
    pub verb: OpVerb,
    /// The `history` the op was run with for one run, which governs a repo
    /// only where the target records none for it.
    pub history: Option<crate::manifest::History>,
    /// The project this op operates on. Both [`Self::source`] and
    /// [`Self::target`] resolve under it and neither one's `.rwv-active` is
    /// consulted: the pointer is ambient state an operator may retarget while
    /// the op is parked, and a workspace pointing elsewhere still holds this
    /// op's repos.
    pub project: ProjectName,
    /// The source workspace.
    pub source: CanonicalPath,
    /// The target workspace (for `sync`: same as the owner workspace; for
    /// `land`: the workspace that has the target checked out, none where it
    /// is checked out nowhere and lands as refs).
    pub target: Option<CanonicalPath>,
    /// Whether `--retire` was passed.
    pub retire: bool,
    /// Current phase.
    pub phase: OpPhase,
    /// Each repo's tip as the replay left it, written at relock; empty
    /// before.
    pub converged_tips: BTreeMap<RepoKey, CommitId>,
    /// RFC3339 UTC timestamp when the op started.
    pub started_at: String,
    /// The verb's plan and the journal of what it moved.
    pub take_in: Option<TakeInRecord>,
}

// ---------------------------------------------------------------------------
// WireOwnerRecord — flat persisted shape for OwnerRecord (serde shim)
// ---------------------------------------------------------------------------

/// On-disk representation of [`OwnerRecord`], which adds the schema and the
/// release that wrote it.
///
/// No field here has a default: every write goes through the full struct, so
/// a record missing a key is malformed, not old, and [`read_owner`] surfaces
/// that as a parse error rather than silently filling it in. A key it does
/// not know is another release's, and is refused rather than dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireOwnerRecord {
    schema: u32,
    release: String,
    id: OpId,
    verb: OpVerb,
    history: Option<crate::manifest::History>,
    project: ProjectName,
    source: CanonicalPath,
    target: Option<CanonicalPath>,
    retire: bool,
    phase: OpPhase,
    converged_tips: BTreeMap<RepoKey, CommitId>,
    started_at: String,
    take_in: Option<TakeInRecord>,
}

impl From<WireOwnerRecord> for OwnerRecord {
    fn from(w: WireOwnerRecord) -> Self {
        Self {
            id: w.id,
            verb: w.verb,
            history: w.history,
            project: w.project,
            source: w.source,
            target: w.target,
            retire: w.retire,
            phase: w.phase,
            converged_tips: w.converged_tips,
            started_at: w.started_at,
            take_in: w.take_in,
        }
    }
}

impl From<OwnerRecord> for WireOwnerRecord {
    fn from(r: OwnerRecord) -> Self {
        Self {
            schema: OP_RECORD_SCHEMA,
            release: crate::rwv_version().to_string(),
            id: r.id,
            verb: r.verb,
            history: r.history,
            project: r.project,
            source: r.source,
            target: r.target,
            retire: r.retire,
            phase: r.phase,
            converged_tips: r.converged_tips,
            started_at: r.started_at,
            take_in: r.take_in,
        }
    }
}

impl OwnerRecord {
    /// Build a new [`OwnerRecord`] for a `rwv sync` invocation.
    ///
    /// Initial phase is `Replay` — the first phase the driver will enter.
    pub fn new_sync(
        op_id: &OpId,
        history: Option<crate::manifest::History>,
        project: ProjectName,
        source_workspace: &Path,
        cwd_workspace: &Path,
    ) -> Self {
        Self {
            id: op_id.clone(),
            verb: OpVerb::Sync,
            history,
            project,
            source: CanonicalPath::of(source_workspace),
            target: Some(CanonicalPath::of(cwd_workspace)),
            retire: false,
            phase: OpPhase::Replay,
            converged_tips: BTreeMap::new(),
            started_at: utc_now_rfc3339(),
            take_in: None,
        }
    }

    /// Whether the op recorded its plan: for a landing, the branch it lands
    /// into and that branch's tip as it read them under its claim.
    pub fn records_a_plan(&self) -> bool {
        self.take_in
            .as_ref()
            .is_some_and(|take_in| take_in.receiving.contains_key(&RepoKey::Project))
    }

    /// Each branch the op's landing claims hold besides the target they are
    /// held on: what a landing lands into and what a carry carries.
    pub fn claimed_branches(&self) -> Vec<(RepoKey, String)> {
        let Some(take_in) = &self.take_in else {
            return Vec::new();
        };
        let target = match (&self.verb, &take_in.sync_from) {
            (OpVerb::Land, Some(SyncFrom::Target(target))) => {
                Some((RepoKey::Project, target.clone()))
            }
            _ => None,
        };
        target
            .into_iter()
            .chain(
                take_in
                    .receiving
                    .iter()
                    .map(|(repo, receiving)| (repo.clone(), receiving.branch.clone())),
            )
            .chain(
                take_in
                    .carries
                    .iter()
                    .map(|carry| (carry.repo.clone(), carry.branch.clone())),
            )
            .collect()
    }

    /// Build a new [`OwnerRecord`] for a `rwv land` invocation.
    ///
    /// Initial phase is `Replay` — the first phase the driver will enter.
    pub fn new_land(
        op_id: &OpId,
        history: Option<crate::manifest::History>,
        project: ProjectName,
        cwd_workspace: &Path,
        target_workspace: Option<&Path>,
        retire: bool,
    ) -> Self {
        Self {
            id: op_id.clone(),
            verb: OpVerb::Land,
            history,
            project,
            source: CanonicalPath::of(cwd_workspace),
            target: target_workspace.map(CanonicalPath::of),
            retire,
            phase: OpPhase::Replay,
            converged_tips: BTreeMap::new(),
            started_at: utc_now_rfc3339(),
            take_in: None,
        }
    }

    /// Build a new [`OwnerRecord`] for a `pull` or `update --remote` that
    /// merges or resets, which acts on one workspace alone.
    pub fn new_take_in(
        op_id: &OpId,
        verb: OpVerb,
        project: ProjectName,
        workspace: &Path,
        take_in: TakeInRecord,
    ) -> Self {
        Self {
            id: op_id.clone(),
            verb,
            history: None,
            project,
            source: CanonicalPath::of(workspace),
            target: Some(CanonicalPath::of(workspace)),
            retire: false,
            phase: OpPhase::Replay,
            converged_tips: BTreeMap::new(),
            started_at: utc_now_rfc3339(),
            take_in: Some(take_in),
        }
    }

    pub fn header(&self) -> OwnerHeader {
        OwnerHeader {
            id: self.id.clone(),
            verb: self.verb,
            project: self.project.clone(),
            source: self.source.clone(),
            target: self.target.clone(),
            started_at: self.started_at.clone(),
        }
    }

    pub fn savepoint_keys(&self) -> Vec<SavepointKey> {
        self.header().savepoint_keys()
    }

    /// The repos of the workspace the op runs in where its own step can
    /// stop in a VCS operation (a rebase, merge or cherry-pick): those its
    /// plan integrates, and each it journaled starting a merge, a rebase or a
    /// carry in. A repo stopped in one anywhere else is someone else's.
    pub fn operates_in(&self) -> std::collections::BTreeSet<RepoKey> {
        let mut repos = std::collections::BTreeSet::new();
        if let Some(take_in) = &self.take_in {
            repos.extend(take_in.merges.iter().map(|m| m.repo.clone()));
            repos.extend(take_in.rebases.iter().map(|r| r.repo.clone()));
            repos.extend(take_in.carries.iter().map(|c| c.repo.clone()));
        }
        if matches!(self.verb, OpVerb::Pull | OpVerb::UpdateRemote) {
            return repos;
        }
        repos.insert(RepoKey::Project);
        if let Some(take_in) = &self.take_in {
            repos.extend(take_in.upstream_tips.keys().cloned());
            repos.extend(
                take_in
                    .receiving
                    .iter()
                    .filter(|(_, r)| {
                        matches!(
                            r.moves,
                            Moves::InCheckout | Moves::AsRef { .. } | Moves::InPlace { .. }
                        )
                    })
                    .map(|(key, _)| key.clone()),
            );
            if let Some(pick) = &take_in.pick {
                repos.extend(
                    pick.members
                        .iter()
                        .filter(|m| !m.commits.is_empty())
                        .map(|m| m.repo.clone()),
                );
            }
        }
        repos
    }

    /// Each VCS operation the op planned or journaled starting, by repo, with
    /// what it takes in: a merge's source (a planned one merges in the tip
    /// the plan takes in or lands on), a rebase's or carry's range, a copy's
    /// commit. An operation in progress is the op's own only where it takes
    /// in one of these.
    pub fn own_steps(&self) -> std::collections::BTreeMap<RepoKey, Vec<crate::vcs::OwnStep>> {
        use crate::vcs::OwnStep;
        let mut steps: std::collections::BTreeMap<RepoKey, Vec<OwnStep>> = Default::default();
        let Some(take_in) = &self.take_in else {
            return steps;
        };
        let planned_sources = take_in
            .upstream_tips
            .iter()
            .chain(take_in.receiving.iter().map(|(key, r)| (key, &r.tip)));
        for (repo, source) in planned_sources {
            steps.entry(repo.clone()).or_default().push(OwnStep::Merge {
                source: source.clone(),
            });
        }
        for merge in &take_in.merges {
            steps
                .entry(merge.repo.clone())
                .or_default()
                .push(OwnStep::Merge {
                    source: merge.source.clone(),
                });
        }
        for rebase in &take_in.rebases {
            steps
                .entry(rebase.repo.clone())
                .or_default()
                .push(OwnStep::Rebase {
                    base: rebase.base.clone(),
                    from: rebase.from.clone(),
                    onto: rebase.onto.clone(),
                });
        }
        for carry in &take_in.carries {
            steps
                .entry(carry.repo.clone())
                .or_default()
                .push(OwnStep::Rebase {
                    base: carry.base.clone(),
                    from: carry.from.clone(),
                    onto: carry.onto.clone(),
                });
        }
        if let Some(pick) = &take_in.pick {
            for member in &pick.members {
                steps
                    .entry(member.repo.clone())
                    .or_default()
                    .extend(member.commits.iter().map(|commit| OwnStep::Copy {
                        commit: commit.clone(),
                    }));
            }
            if pick.project_started {
                steps
                    .entry(RepoKey::Project)
                    .or_default()
                    .push(OwnStep::Copy {
                        commit: pick.rev.clone(),
                    });
            }
        }
        steps
    }

    /// The owner op-state file within `workspace_dir`.
    pub fn path_in(workspace_dir: &Path) -> StatePath {
        StateFile::OpRecord.in_dir(workspace_dir)
    }
}

/// The fields of an owner record that name its op and the workspaces the op
/// touched. They parse on their own, so a record whose other fields do not
/// still tells `rwv abort` which savepoints to restore from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OwnerHeader {
    pub id: OpId,
    pub verb: OpVerb,
    pub project: ProjectName,
    pub source: CanonicalPath,
    pub target: Option<CanonicalPath>,
    pub started_at: String,
}

impl OwnerHeader {
    /// The keys this op files savepoints under: its own, and for a `land`
    /// the target side's. A savepoint under any of them belongs to the op for
    /// as long as its record exists.
    pub fn savepoint_keys(&self) -> Vec<SavepointKey> {
        match self.verb {
            OpVerb::Sync | OpVerb::Pull | OpVerb::UpdateRemote | OpVerb::CherryPick => {
                vec![SavepointKey::Op(self.id.clone())]
            }
            OpVerb::Land => vec![
                SavepointKey::Op(self.id.clone()),
                SavepointKey::LandTarget(self.id.clone()),
                SavepointKey::LandInPlace(self.id.clone()),
            ],
        }
    }
}

// ---------------------------------------------------------------------------
// LeaseRecord — the thin lease at non-owner mutated workspaces
// ---------------------------------------------------------------------------

/// The thin lease written to `.rwv-op-lease` at every workspace the op
/// mutates other than the owner workspace.
///
/// Immutable once written. Provides mutex semantics and a pointer back
/// to the owner record for `--continue` and `abort`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRecord {
    /// Unique operation identifier (nanosecond wall-clock string). Same as
    /// the owner record's `id`.
    pub id: OpId,
    /// The owner workspace (the workspace holding the full owner record).
    /// Follow this pointer to load op state.
    pub owner: CanonicalPath,
    /// RFC3339 UTC timestamp at which this lease was written.
    ///
    /// Surfaced in `rwv doctor` dead-lease reports as observability-only
    /// context (RFC3339 raw + humanized elapsed). **Never used as a
    /// decision input** — the classification is structural (owner record
    /// absent or op-id mismatch), not elapsed-time based.
    ///
    /// `acquire_op` always populates this. `Option` stays regardless: a
    /// syntactic `Option<T>` field deserializes a missing key as `None`
    /// unconditionally — `#[serde(default)]` has no bearing on it — so this
    /// key is optional on the wire whether or not that reads as intentional,
    /// and `rwv doctor --json`'s dead-lease finding keeps its existing
    /// nullable shape either way.
    pub created_at: Option<String>,
}

impl LeaseRecord {
    /// The lease file within `workspace_dir`.
    pub fn path_in(workspace_dir: &Path) -> StatePath {
        StateFile::OpLease.in_dir(workspace_dir)
    }
}

// ---------------------------------------------------------------------------
// Resolution: owner record or lease → canonical owner record
// ---------------------------------------------------------------------------

/// The result of resolving a workspace's op-state to the canonical owner.
#[derive(Debug, Clone)]
pub struct ResolvedOwner {
    /// The full owner record.
    pub record: OwnerRecord,
    /// The owner workspace (where `.rwv-op` lives).
    pub owner_workspace: CanonicalPath,
}

/// Resolve a workspace's op-state to the canonical owner record.
///
/// - If `workspace_dir` holds an owner record (`.rwv-op`), returns it directly.
/// - If `workspace_dir` holds a thin lease (`.rwv-op-lease`), follows the
///   pointer and reads the owner record from the recorded owner workspace.
/// - Returns `None` if neither file is present.
/// - Returns an error if a present file cannot be parsed, or if the lease
///   pointer leads to a missing owner record.
///
/// This is the entry point for `--continue` and `rwv abort` invoked from any
/// workspace (owner or leased).
pub fn resolve_to_owner(workspace_dir: &Path) -> anyhow::Result<Option<ResolvedOwner>> {
    Ok(
        resolve_with(workspace_dir, read_owner, |record| &record.id)?.map(
            |(record, owner_workspace)| ResolvedOwner {
                record,
                owner_workspace,
            },
        ),
    )
}

fn resolve_with<T>(
    workspace_dir: &Path,
    read: impl Fn(&Path) -> anyhow::Result<Option<T>>,
    id_of: impl Fn(&T) -> &OpId,
) -> anyhow::Result<Option<(T, CanonicalPath)>> {
    if OwnerRecord::path_in(workspace_dir).exists() {
        let record = read(workspace_dir)?.ok_or_else(|| {
            crate::untokened!(
                InternalInvariant,
                "internal: owner record disappeared at {}",
                workspace_dir.display()
            )
        })?;
        return Ok(Some((record, CanonicalPath::of(workspace_dir))));
    }

    let Some(lease) = read_lease(workspace_dir)? else {
        return Ok(None);
    };
    match read(lease.owner.as_path())? {
        Some(record) if *id_of(&record) == lease.id => Ok(Some((record, lease.owner))),
        _ => Err(crate::refusal::refusal(
            RefusalKind::DeadOpLease,
            dead_lease_sentence(workspace_dir, &lease),
        )),
    }
}

/// The advice for a lease whose owner workspace records no op with its id:
/// with no record, nothing can resume or abort the op, so the lease is
/// released on its own.
fn dead_lease_sentence(lease_dir: &Path, lease: &LeaseRecord) -> String {
    format!(
        "{dir} is leased to op {id}, but its owner workspace {owner} records no such op, \
         so nothing can resume or abort it. `rwv doctor --fix` run in {dir} releases \
         the lease.",
        dir = lease_dir.display(),
        id = lease.id,
        owner = lease.owner.as_path().display(),
    )
}

// ---------------------------------------------------------------------------
// Owner record I/O
// ---------------------------------------------------------------------------

/// Write `record` to the `.rwv-op` file in `workspace_dir`.
///
/// This is the **phase-persistence write path**: [`set_phase`] and any
/// caller updating fields on an *already-acquired* owner record (converged
/// tips, `--continue` restarts) call it. It overwrites any existing
/// file.
///
/// **The first write of a fresh op must go through [`acquire_op`]**, not this
/// function — a bare `write_owner` would silently overwrite a peer op's
/// in-flight record. Callers that just need a check without a claim use
/// [`check_no_op_in_progress`].
pub fn write_owner(workspace_dir: &Path, record: &OwnerRecord) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(record).context("failed to serialize owner record")?;
    OwnerRecord::path_in(workspace_dir).publish(json.as_bytes())
}

/// Read the `.rwv-op` file from `workspace_dir`, returning `None` if absent.
///
/// A record that does not read whole refuses as `unreadable-op-record`,
/// whatever failed in it: neither `--continue` nor `rwv abort` acts on part
/// of a record.
pub fn read_owner(workspace_dir: &Path) -> anyhow::Result<Option<OwnerRecord>> {
    let path = OwnerRecord::path_in(workspace_dir);
    if !path.exists() {
        return Ok(None);
    }
    let json = path
        .read_to_string()
        .with_context(|| format!("failed to read owner record from {path}"))?;
    refuse_other_schema(&path, &json)?;
    let whole_error = match serde_json::from_str::<OwnerRecord>(&json) {
        Ok(record) => return Ok(Some(record)),
        Err(e) => e,
    };
    let why = match serde_json::from_str::<OwnerHeader>(&json) {
        Ok(_) => format!(
            "{writer} wrote it in a shape this release does not read ({whole_error:#}), so \
             neither `--continue` nor `rwv abort` acts on it: what the part it cannot read \
             records would not be put back. The rwv that wrote it resumes or aborts it",
            writer = writer_of(&json),
        ),
        Err(_) => format!(
            "it names no operation this rwv can read ({whole_error:#}), so nothing can \
             resume it or restore from it"
        ),
    };
    Err(unreadable_record(&path, &why))
}

/// The verb that ends an operation whose record cannot be read, run in the
/// workspace holding the record.
pub fn discard_unreadable_record_at(workspace: &Path) -> String {
    format!(
        "rwv -C {} abort --discard-unreadable-record",
        workspace.display()
    )
}

fn unreadable_record(path: &StatePath, why: &str) -> anyhow::Error {
    let discard = discard_unreadable_record_at(path.dir());
    let migrations = migrations_before_discard(path.dir());
    let ends = if migrations.is_empty() {
        format!(
            "This ends the operation, removing the record and releasing what the operation \
             holds; it moves nothing back and lists what it leaves:\n  {discard}"
        )
    } else {
        format!(
            "An earlier release left the workspace holding it, so it is migrated first, then \
             the last line ends the operation, removing the record and releasing what the \
             operation holds; it moves nothing back and lists what it leaves:\n  {}\n  \
             {discard}",
            migrations.join("\n  ")
        )
    };
    crate::refusal::refusal(
        RefusalKind::UnreadableOpRecord,
        format!("the operation record at {path} cannot be read: {why}. {ends}"),
    )
}

/// The migrations an earlier release's workspace at `dir` needs before the
/// discard there can run and name every member it leaves: its marker, which
/// the primary migrates, then each manifest of a project repo stopped in no
/// VCS operation, which it migrates itself. A stopped project repo is listed
/// by the discard whatever its manifest holds.
fn migrations_before_discard(dir: &Path) -> Vec<String> {
    let mut verbs = Vec::new();
    if let Some(primary) = crate::workspace::legacy_marker_primary(dir) {
        verbs.push(doctor_fix_in(&primary));
    }
    let vcs = crate::vcs::project_vcs();
    let earlier_manifest = crate::workspace::discover_projects(dir)
        .iter()
        .map(|project| crate::workspace::project_dir(dir, project))
        .filter(|project_dir| vcs.mid_operation(project_dir).is_none())
        .filter_map(|project_dir| {
            std::fs::read_to_string(project_dir.join(crate::manifest::Manifest::FILE_NAME)).ok()
        })
        .any(|text| !crate::manifest::pre_schema_keys(&text).is_empty());
    if earlier_manifest {
        verbs.push(doctor_fix_in(dir));
    }
    verbs
}

fn doctor_fix_in(workspace: &Path) -> String {
    format!("rwv -C {} doctor --fix", workspace.display())
}

/// The release a record names as its writer, as a message names it.
fn writer_of(json: &str) -> String {
    let release = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|record| record.get("release")?.as_str().map(str::to_owned));
    match release {
        Some(release) if release == crate::rwv_version().to_string() => {
            format!("another build of rwv {release}")
        }
        Some(release) => format!("rwv {release}"),
        None => "an rwv release that records no version in it".to_owned(),
    }
}

fn refuse_other_schema(path: &StatePath, json: &str) -> anyhow::Result<()> {
    let Ok(record) = serde_json::from_str::<serde_json::Value>(json) else {
        return Ok(());
    };
    if record.get("schema").and_then(serde_json::Value::as_u64) == Some(u64::from(OP_RECORD_SCHEMA))
    {
        return Ok(());
    }
    let writer = writer_of(json);
    let schema = match record.get("schema") {
        Some(schema) => format!("record schema {schema}"),
        None => "a record with no schema".to_owned(),
    };
    Err(unreadable_record(
        path,
        &format!(
            "{writer} wrote it, in {schema}, and this release reads schema {OP_RECORD_SCHEMA} \
             alone, so neither `--continue` nor `rwv abort` acts on it. The rwv that wrote it \
             resumes or aborts it"
        ),
    ))
}

/// The header of the `.rwv-op` file in `workspace_dir`.
pub fn read_owner_header(workspace_dir: &Path) -> anyhow::Result<Option<OwnerHeader>> {
    Ok(read_owner(workspace_dir)?.map(|record| record.header()))
}

/// The header of the `.rwv-op` file in `workspace_dir` as it reads, whatever
/// the record's schema: a record another release wrote still owns the
/// savepoints that release would abort with.
pub fn read_owner_header_of_any_schema(workspace_dir: &Path) -> Option<OwnerHeader> {
    let json = OwnerRecord::path_in(workspace_dir).read_to_string().ok()?;
    serde_json::from_str::<OwnerHeader>(&json).ok()
}

/// Whether `workspace_dir` records the op `id`: `Some(false)` when it holds
/// no record or one of another op, `None` when its record names no op this
/// rwv reads, so whose it is cannot be told.
pub fn records_op(workspace_dir: &Path, id: &OpId) -> Option<bool> {
    let path = OwnerRecord::path_in(workspace_dir);
    match path.read_to_string() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
        Ok(json) => serde_json::from_str::<OwnerHeader>(&json)
            .ok()
            .map(|header| header.id == *id),
    }
}

/// Record `new_phase` as the phase in progress in the owner record at
/// `workspace_dir`.
///
/// Reads the existing file, updates the phase, writes back. The driver loop's
/// post-transition write is one caller; a resume that re-enters an earlier
/// phase is the other, so this is not ordered.
pub fn set_phase(workspace_dir: &Path, new_phase: OpPhase) -> anyhow::Result<()> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "no owner record found at {} to advance phase",
            workspace_dir.display()
        )
    })?;
    record.phase = new_phase;
    write_owner(workspace_dir, &record)
}

/// Journal that the op is about to start a merge in `repo`, so `rwv abort`
/// knows a merge stopped there may be the op's own.
pub fn journal_merging(
    workspace_dir: &Path,
    repo: RepoKey,
    source: &CommitId,
) -> anyhow::Result<()> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "no owner record at {} to journal in",
            workspace_dir.display()
        )
    })?;
    let merging = Merging {
        repo,
        source: source.clone(),
    };
    if let Some(take_in) = record.take_in.as_mut() {
        if take_in.merges.contains(&merging) {
            return Ok(());
        }
        take_in.merges.push(merging);
    }
    write_owner(workspace_dir, &record)
}

/// Mark the op recorded at `workspace_dir` as one `rwv abort` began rolling
/// back.
pub fn mark_abort_begun(workspace_dir: &Path) -> anyhow::Result<()> {
    let Some(mut record) = read_owner(workspace_dir)? else {
        return Ok(());
    };
    record
        .take_in
        .get_or_insert_with(TakeInRecord::default)
        .abort_begun = true;
    write_owner(workspace_dir, &record)
}

/// Record whether the op's retire stopped, refused, in the process that ran it.
pub fn mark_retire_stopped(workspace_dir: &Path, stopped: bool) -> anyhow::Result<()> {
    let Some(mut record) = read_owner(workspace_dir)? else {
        return Ok(());
    };
    match record.take_in.as_mut() {
        Some(take_in) if take_in.retire_stopped != stopped => take_in.retire_stopped = stopped,
        _ => return Ok(()),
    }
    write_owner(workspace_dir, &record)
}

/// Journal a branch the op moved as a ref, checked out nowhere.
pub fn journal_ref_move(workspace_dir: &Path, moved: RefMove) -> anyhow::Result<()> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "no owner record at {} to journal in",
            workspace_dir.display()
        )
    })?;
    if let Some(take_in) = record.take_in.as_mut() {
        take_in.ref_moves.push(moved);
    }
    write_owner(workspace_dir, &record)
}

/// Journal a branch the op created, checked out nowhere.
pub fn journal_created_ref(workspace_dir: &Path, created: CreatedRef) -> anyhow::Result<()> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "no owner record at {} to journal in",
            workspace_dir.display()
        )
    })?;
    if let Some(take_in) = record.take_in.as_mut() {
        take_in.created_refs.push(created);
    }
    write_owner(workspace_dir, &record)
}

/// Journal a report a landing made; `false` when the record holds it already,
/// so it was reported before.
pub fn journal_report(workspace_dir: &Path, report: &LandingReport) -> anyhow::Result<bool> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "no owner record at {} to journal in",
            workspace_dir.display()
        )
    })?;
    let Some(take_in) = record.take_in.as_mut() else {
        return Ok(true);
    };
    if take_in.reports.contains(report) {
        return Ok(false);
    }
    take_in.reports.push(report.clone());
    write_owner(workspace_dir, &record)?;
    Ok(true)
}

/// Whether the op recorded at `workspace_dir` journaled creating `branch` in
/// `repo`.
pub fn created_by(
    workspace_dir: &Path,
    repo: &RepoKey,
    branch: &RawRefName,
) -> anyhow::Result<bool> {
    Ok(read_owner(workspace_dir)?
        .and_then(|record| record.take_in)
        .is_some_and(|take_in| {
            take_in
                .created_refs
                .iter()
                .any(|made| &made.repo == repo && made.branch == branch.as_str())
        }))
}

/// Record a backup the op made, so it goes when the op completes.
pub fn journal_backup(workspace_dir: &Path, kept: KeptBackup) -> anyhow::Result<()> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "no owner record at {} to journal in",
            workspace_dir.display()
        )
    })?;
    if let Some(take_in) = record.take_in.as_mut() {
        if !take_in.backups.contains(&kept) {
            take_in.backups.push(kept);
        }
    }
    write_owner(workspace_dir, &record)
}

/// Journal `rebasing` on the op at `workspace_dir` before the rebase starts,
/// replacing an earlier rebase of the same branch of the same repo.
pub fn journal_rebasing(workspace_dir: &Path, rebasing: Rebasing) -> anyhow::Result<()> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "no owner record at {} to journal in",
            workspace_dir.display()
        )
    })?;
    if let Some(take_in) = record.take_in.as_mut() {
        take_in
            .rebases
            .retain(|r| !(r.repo == rebasing.repo && r.branch == rebasing.branch));
        take_in.rebases.push(rebasing);
    }
    write_owner(workspace_dir, &record)
}

/// Journal that the operator settled the `rwv.toml` of the tip `rebasing`
/// replayed, on the op at `workspace_dir`.
pub fn journal_tip_settled(workspace_dir: &Path, rebasing: Rebasing) -> anyhow::Result<()> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "no owner record at {} to journal in",
            workspace_dir.display()
        )
    })?;
    if let Some(take_in) = record.take_in.as_mut() {
        take_in.tip_settled = Some(rebasing);
    }
    write_owner(workspace_dir, &record)
}

/// Journal the `rwv.toml` stop the op at `workspace_dir` is at, or, with
/// `None`, that it is past it.
pub fn journal_manifest_stop(
    workspace_dir: &Path,
    stop: Option<ManifestStop>,
) -> anyhow::Result<()> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "no owner record at {} to journal in",
            workspace_dir.display()
        )
    })?;
    if let Some(take_in) = record.take_in.as_mut() {
        take_in.manifest_stop = stop;
    }
    write_owner(workspace_dir, &record)
}

/// Journal `carrying` on the op at `workspace_dir`, replacing the record of
/// the same branch of the same repo. A member's carry makes the same
/// branch's project carry, composed over the member tips it pinned, one to
/// compose again, unless that carry already moved its branch.
pub fn journal_carrying(workspace_dir: &Path, carrying: Carrying) -> anyhow::Result<()> {
    let mut record = read_owner(workspace_dir)?.ok_or_else(|| {
        crate::untokened!(
            InternalInvariant,
            "no owner record at {} to journal in",
            workspace_dir.display()
        )
    })?;
    if let Some(take_in) = record.take_in.as_mut() {
        if carrying.repo != RepoKey::Project {
            let moves = &take_in.ref_moves;
            for project in take_in.carries.iter_mut().filter(|c| {
                c.repo == RepoKey::Project
                    && c.branch == carrying.branch
                    && !moves.iter().any(|m| {
                        m.repo == RepoKey::Project
                            && m.branch == c.branch
                            && Some(&m.to) == c.composed.as_ref()
                    })
            }) {
                project.composed = None;
            }
        }
        match take_in
            .carries
            .iter_mut()
            .find(|c| c.repo == carrying.repo && c.branch == carrying.branch)
        {
            Some(held) => *held = carrying,
            None => take_in.carries.push(carrying),
        }
    }
    write_owner(workspace_dir, &record)
}

/// Remove the `.rwv-op` owner record from `workspace_dir`. No-op if absent.
pub fn clear_owner(workspace_dir: &Path) {
    let _ = OwnerRecord::path_in(workspace_dir).remove();
}

/// Remove the owner record from `workspace_dir` only while it records `id`.
pub fn clear_owner_of(workspace_dir: &Path, id: &OpId) {
    if records_op(workspace_dir, id) == Some(true) {
        clear_owner(workspace_dir);
    }
}

// ---------------------------------------------------------------------------
// Lease I/O
// ---------------------------------------------------------------------------

/// Read the `.rwv-op-lease` file from `workspace_dir`, returning `None` if absent.
///
/// Returns an error if the file exists but cannot be parsed.
pub fn read_lease(workspace_dir: &Path) -> anyhow::Result<Option<LeaseRecord>> {
    let path = LeaseRecord::path_in(workspace_dir);
    if !path.exists() {
        return Ok(None);
    }
    let json = path
        .read_to_string()
        .with_context(|| format!("failed to read lease from {path}"))?;
    match serde_json::from_str::<LeaseRecord>(&json) {
        Ok(lease) => Ok(Some(lease)),
        Err(e) => crate::refuse!(
            RefusalKind::UnreadableOpLease,
            "the lease at {path} cannot be read ({e:#}), so which operation holds this \
             workspace cannot be told. This removes it, and nothing else; an operation that \
             still runs loses its hold on this workspace:\n  {discard}",
            discard = discard_unreadable_record_at(path.dir()),
        ),
    }
}

/// Remove the `.rwv-op-lease` lease file from `workspace_dir`. No-op if absent.
pub fn clear_lease(workspace_dir: &Path) {
    let _ = LeaseRecord::path_in(workspace_dir).remove();
}

/// Remove both op-state files (owner record + lease) from `workspace_dir`.
/// No-op for files that are absent.
pub fn clear_all_at(workspace_dir: &Path) {
    clear_owner(workspace_dir);
    clear_lease(workspace_dir);
}

// ---------------------------------------------------------------------------
// Concurrency guard
// ---------------------------------------------------------------------------

/// Check that no op-state file (owner record or lease) exists in any of the
/// given workspace directories.
///
/// This is the cross-verb advisory refusal, never an exclusion: it reads, and
/// the window between the read and what the caller does next stays open. A
/// caller here declines to START while an op involves the workspace and holds
/// nothing afterwards; a caller that needs the exclusion itself takes it
/// through [`acquire_op`] instead, which is `sync` and `land` alone. Which
/// verbs sit on each side is enumerated where a new one reddens the
/// enumeration, in `tests/process/concurrency_boundaries_test.rs`, rather than in this
/// sentence, where the last enumeration went false without anything noticing.
///
/// Returns an error if any workspace carries an owner record or a lease. Both
/// branches name the op (verb), its age, the phase it stalled in, and the two
/// exits — `--continue` from the OWNING workspace, or `rwv abort` — so the
/// operator knows exactly what they interrupted and how to clear it. When the
/// workspace only holds a thin lease, the pointer is followed to the owner
/// record so the message carries the same detail (naming the owner workspace
/// where `--continue` must run). A crashed op leaves a stale record on purpose:
/// there is no auto-expiry; `rwv abort`'s verified restore is the way out.
///
/// For non-mutating callers this is enough — they don't write op state, so
/// there is no TOCTOU exposure. For `sync` / `land`, which do write op
/// state, use [`acquire_op`] instead: it performs the same refusal shape but
/// via an atomic create so two concurrent ops cannot both pass and only
/// collide later at the git layer.
pub fn check_no_op_in_progress(workspace_dirs: &[&Path]) -> anyhow::Result<()> {
    for &dir in workspace_dirs {
        if let Some(err) = in_flight_refusal_for(dir)? {
            return Err(err);
        }
    }
    Ok(())
}

/// Build the standard in-flight-op refusal for `dir` if it carries an op-state
/// file, or `Ok(None)` if the directory is clean.
///
/// Split out of [`check_no_op_in_progress`] so [`acquire_op`] can emit the
/// identical refusal shape on an atomic-create `AlreadyExists`.
fn in_flight_refusal_for(dir: &Path) -> anyhow::Result<Option<anyhow::Error>> {
    Ok(match in_flight_sentence_for(dir)? {
        None => None,
        Some(InFlight::Op(sentence)) => {
            Some(crate::refusal::refusal(RefusalKind::OpInProgress, sentence))
        }
        Some(InFlight::DeadLease(sentence)) => {
            Some(crate::refusal::refusal(RefusalKind::DeadOpLease, sentence))
        }
    })
}

enum InFlight {
    Op(String),
    DeadLease(String),
}

/// What `rwv abort` does to a stopped op, as every surface naming it says.
pub const ABORT_ROLLS_BACK: &str = "`rwv abort` rolls it back whole, or refuses before \
     anything moves, naming what is in the way and the command that ends it";

/// [`ABORT_ROLLS_BACK`] with its abort spelled to run against `workspace`,
/// the one the operation is owned by.
pub(crate) fn abort_rolls_back_at(workspace: &Path) -> String {
    ABORT_ROLLS_BACK.replacen(
        "`rwv abort`",
        &format!("`{}`", abort_command_at(workspace)),
        1,
    )
}

fn in_flight_sentence_for(dir: &Path) -> anyhow::Result<Option<InFlight>> {
    // Owner record: full detail is right here.
    if let Some(record) = read_owner(dir)? {
        let elapsed = elapsed_since(&record.started_at);
        return Ok(Some(InFlight::Op(format!(
            "{verb} in progress (started {elapsed} ago, mid `{phase}`) at {dir}.\n\
             Rerun with `{resume}` after resolving; or {abort}.",
            verb = record.verb,
            phase = record.phase,
            dir = dir.display(),
            resume = resume_command_at(record.verb, dir),
            abort = abort_rolls_back_at(dir),
        ))));
    }
    // Lease: follow the pointer to the owner record so the refusal carries
    // the same op / age / phase detail and names where `--continue` runs.
    if let Some(lease) = read_lease(dir)? {
        match read_owner(lease.owner.as_path())? {
            Some(record) if record.id == lease.id => {
                let elapsed = elapsed_since(&record.started_at);
                return Ok(Some(InFlight::Op(format!(
                    "{verb} in progress (started {elapsed} ago, mid `{phase}`); this \
                     workspace ({dir}) is leased to it. Owner workspace: {owner}.\n\
                     Rerun with `{resume}` after resolving; or {abort}.",
                    verb = record.verb,
                    phase = record.phase,
                    dir = dir.display(),
                    owner = lease.owner.as_path().display(),
                    resume = resume_command_at(record.verb, lease.owner.as_path()),
                    abort = abort_rolls_back_at(lease.owner.as_path()),
                ))));
            }
            _ => {
                return Ok(Some(InFlight::DeadLease(dead_lease_sentence(dir, &lease))));
            }
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Atomic acquisition (guard→mark TOCTOU fix)
// ---------------------------------------------------------------------------

/// Handle returned by a successful [`acquire_op`] — records which files were
/// atomically created so a subsequent precondition refusal can undo them.
///
/// The plain "concurrency guard" (`check_no_op_in_progress`) is a check, not a
/// claim: two ops can both pass it and collide later at the git layer (R7 root
/// cause). Atomic acquisition writes each file with `O_CREAT|O_EXCL`
/// so the OS refuses the second creator; the handle carries the created set so
/// the caller can [`release_acquired`] on a downstream precondition refusal
/// (per the cleanup table's "refusal → cleared everywhere" row).
///
/// Cleanup ownership after success:
///
/// - Op runs to success / precondition-refusal AFTER acquisition → caller calls
///   [`release_acquired`] to clear the acquired records.
/// - Op crashes after acquisition → records remain on disk; `--continue` /
///   `rwv abort` are the exits (structural cleanup by the dead-lease doctor
///   check when leases become provably-dead is a hygiene backstop, not a
///   policy).
#[must_use = "an acquired op handle must either be committed to the running op \
              or released via release_acquired on refusal"]
#[derive(Debug)]
pub struct AcquiredOp {
    touched: TouchedWorkspaces,
}

impl AcquiredOp {
    /// The acquisition of `touched` whose files the caller has written, by
    /// [`acquire_op`] and [`acquire_lease`] in turn.
    pub fn of(touched: TouchedWorkspaces) -> Self {
        Self { touched }
    }

    /// The owner workspace this acquisition writes into.
    pub fn owner_workspace(&self) -> &Path {
        self.touched.owner()
    }
}

/// The workspaces one op writes op-state into: the owner, which holds
/// `.rwv-op`, and every workspace that holds a `.rwv-op-lease` pointing back
/// at it.
///
/// `sync` mutates only the workspace it runs in, so it has no leases;
/// `land` also mutates the target and leases it. Acquisition and cleanup
/// both take the set from here. Spelled at each site instead, the two drift
/// apart the moment a verb starts touching another workspace, and the leases
/// cleanup then stops clearing surface only as dead-lease doctor findings.
#[derive(Debug, Clone)]
pub struct TouchedWorkspaces {
    owner: PathBuf,
    leases: Vec<PathBuf>,
}

impl TouchedWorkspaces {
    pub fn of(verb: OpVerb, owner_workspace_dir: &Path, dest_workspace_dir: Option<&Path>) -> Self {
        let leases = match verb {
            OpVerb::Sync | OpVerb::Pull | OpVerb::UpdateRemote | OpVerb::CherryPick => Vec::new(),
            OpVerb::Land => dest_workspace_dir
                .map(Path::to_path_buf)
                .into_iter()
                .collect(),
        };
        Self {
            owner: owner_workspace_dir.to_path_buf(),
            leases,
        }
    }

    /// The same set, leasing as well each of `more` it does not hold yet.
    pub fn leasing(mut self, more: impl IntoIterator<Item = PathBuf>) -> Self {
        for ws in more {
            let at = CanonicalPath::of(&ws);
            if CanonicalPath::of(&self.owner) != at
                && !self.leases.iter().any(|held| CanonicalPath::of(held) == at)
            {
                self.leases.push(ws);
            }
        }
        self
    }

    pub fn owner(&self) -> &Path {
        &self.owner
    }

    /// Remove every op-state file this op wrote. A lease that points at
    /// another owner by now belongs to another op, and stays.
    ///
    /// Leases go first so an interruption part-way leaves an owner record with
    /// no leases — a valid resume target — rather than a lease pointing at a
    /// record that is already gone.
    pub fn clear(&self) {
        for ws in &self.leases {
            clear_lease_owned_by(ws, &self.owner);
        }
        clear_owner(&self.owner);
    }
}

/// Remove the lease in `workspace_dir` if it points at `owner`.
pub fn clear_lease_owned_by(workspace_dir: &Path, owner: &Path) {
    if read_lease(workspace_dir)
        .ok()
        .flatten()
        .is_some_and(|lease| lease.owner == CanonicalPath::of(owner))
    {
        clear_lease(workspace_dir);
    }
}

/// Whether a lease [`acquire_lease`] returned was there before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leased {
    Before,
    Now,
}

/// Lease `workspace_dir` to the op `record` is the owner record of, which
/// `owner_workspace_dir` holds: done when the lease is this op's already,
/// refused with the standard in-flight refusal when another op's state is
/// there.
pub fn acquire_lease(
    workspace_dir: &Path,
    owner_workspace_dir: &Path,
    id: &OpId,
) -> anyhow::Result<Leased> {
    if read_lease(workspace_dir)?.is_some_and(|lease| lease.id == *id) {
        return Ok(Leased::Before);
    }
    if let Some(err) = in_flight_refusal_for(workspace_dir)? {
        return Err(err);
    }
    let lease = LeaseRecord {
        id: id.clone(),
        owner: CanonicalPath::of(owner_workspace_dir),
        created_at: Some(utc_now_rfc3339()),
    };
    let json = serde_json::to_string_pretty(&lease).context("failed to serialize lease record")?;
    let path = LeaseRecord::path_in(workspace_dir);
    match path.create(json.as_bytes()) {
        Ok(()) => Ok(Leased::Now),
        Err(CreateNewError::AlreadyExists) => Err(in_flight_refusal_for(workspace_dir)?
            .unwrap_or_else(|| {
                crate::refusal::refusal(
                    RefusalKind::OpAcquisitionRaced,
                    format!(
                        "raced with a concurrent op at {}; retry",
                        workspace_dir.display()
                    ),
                )
            })),
        Err(CreateNewError::Io(e)) => {
            Err(anyhow::Error::new(e).context(format!("failed to acquire lease at {path}")))
        }
        Err(CreateNewError::NoExclusivePublish(e)) => Err(e),
    }
}

/// Atomically acquire an op across `touched` — the owner workspace plus every
/// additional workspace the op will mutate.
///
/// Semantics:
///
/// - The owner workspace gets `.rwv-op` written with the full `owner_record`.
/// - Every lease workspace gets `.rwv-op-lease` written pointing back at the
///   owner.
/// - Every file is created with `O_CREAT|O_EXCL`. On `AlreadyExists` anywhere,
///   any files already created by *this* call are removed and the returned
///   error is the standard [`check_no_op_in_progress`]-shape refusal reading
///   the *pre-existing* holder (name, age, phase, `--continue` / `abort`
///   exits). This preserves the cleanup-table row that says a refusal leaves
///   no trace.
/// - A crash between successful acquisition and the caller's next persistent
///   write (savepoints) leaves records with no
///   savepoints. That partial state is what the dead-lease doctor check
///   diagnoses (a lease whose recorded owner workspace has no matching
///   `.rwv-op` with the same op id is structurally dead).
///
/// Ordering: the owner record is written first, then leases. This makes the
/// crash-partial case symmetric to the abort/rollback path — an owner record
/// with no leases is a valid resume target; a lease with no owner record is
/// the dead-lease case doctor auto-fixes.
pub fn acquire_op(
    touched: &TouchedWorkspaces,
    owner_record: &OwnerRecord,
) -> anyhow::Result<AcquiredOp> {
    let owner_workspace_dir = touched.owner();
    let lease_workspaces = &touched.leases;
    // Acquisition dominates every other refusal, so if any touched workspace
    // already carries op-state we must emit the rich in-flight refusal from IT
    // rather than a raw AlreadyExists context. We check first
    // (cheap) and then atomic-create; a losing racer whose file lands between
    // check and create still gets the correct shape via the AlreadyExists
    // branch below (which re-reads and re-derives the same message).
    if let Some(err) = in_flight_refusal_for(owner_workspace_dir)? {
        return Err(err);
    }
    for ws in lease_workspaces {
        if let Some(err) = in_flight_refusal_for(ws)? {
            return Err(err);
        }
    }

    crate::durable_file::refuse_without_exclusive_publish(owner_workspace_dir)?;
    for ws in lease_workspaces {
        crate::durable_file::refuse_without_exclusive_publish(ws)?;
    }

    // Owner first, then leases.
    let owner_path = OwnerRecord::path_in(owner_workspace_dir);
    let owner_json = serde_json::to_string_pretty(owner_record)
        .context("failed to serialize owner record for acquisition")?;
    match owner_path.create(owner_json.as_bytes()) {
        Ok(()) => {}
        Err(CreateNewError::AlreadyExists) => {
            // Race: another op landed here. Read it back for the standard
            // refusal shape. If it disappeared between EEXIST and re-read
            // (the winner completed successfully already), fall through with a
            // generic refusal — the operator can retry.
            return Err(
                in_flight_refusal_for(owner_workspace_dir)?.unwrap_or_else(|| {
                    crate::refusal::refusal(
                        RefusalKind::OpAcquisitionRaced,
                        format!(
                            "raced with a concurrent op at {}; retry",
                            owner_workspace_dir.display()
                        ),
                    )
                }),
            );
        }
        Err(CreateNewError::Io(e)) => {
            return Err(anyhow::Error::new(e)
                .context(format!("failed to acquire owner record at {owner_path}")));
        }
        Err(CreateNewError::NoExclusivePublish(e)) => return Err(e),
    }

    let mut acquired_leases: Vec<PathBuf> = Vec::with_capacity(lease_workspaces.len());
    for ws in lease_workspaces {
        let lease_path = LeaseRecord::path_in(ws);
        let lease = LeaseRecord {
            id: owner_record.id.clone(),
            owner: CanonicalPath::of(owner_workspace_dir),
            created_at: Some(utc_now_rfc3339()),
        };
        let lease_json = match serde_json::to_string_pretty(&lease) {
            Ok(s) => s,
            Err(e) => {
                // Serialize failure is not I/O contention but still needs to
                // roll back the owner + any leases we already wrote so refusal
                // leaves no trace.
                rollback_acquired(owner_workspace_dir, &acquired_leases);
                return Err(anyhow::Error::new(e).context("failed to serialize lease record"));
            }
        };
        match lease_path.create(lease_json.as_bytes()) {
            Ok(()) => acquired_leases.push(ws.clone()),
            Err(CreateNewError::AlreadyExists) => {
                // Race on a lease: undo owner + any leases we already wrote,
                // then emit the in-flight refusal reading the *existing*
                // lease (following its pointer for the rich message shape,
                // matching what a plain check_no_op_in_progress would emit).
                rollback_acquired(owner_workspace_dir, &acquired_leases);
                return Err(in_flight_refusal_for(ws)?.unwrap_or_else(|| {
                    crate::refusal::refusal(
                        RefusalKind::OpAcquisitionRaced,
                        format!("raced with a concurrent op at {}; retry", ws.display()),
                    )
                }));
            }
            Err(CreateNewError::Io(e)) => {
                rollback_acquired(owner_workspace_dir, &acquired_leases);
                return Err(anyhow::Error::new(e)
                    .context(format!("failed to acquire lease at {lease_path}")));
            }
            Err(CreateNewError::NoExclusivePublish(e)) => {
                rollback_acquired(owner_workspace_dir, &acquired_leases);
                return Err(e);
            }
        }
    }

    Ok(AcquiredOp {
        touched: touched.clone(),
    })
}

/// Lease each of `more` to the op `acquired` holds, as [`acquire_op`] leases:
/// a workspace another op holds refuses, and the leases this call wrote are
/// taken back. Those already held are left as they are.
pub fn acquire_more_leases(
    acquired: &mut AcquiredOp,
    more: &[PathBuf],
    owner_record: &OwnerRecord,
) -> anyhow::Result<()> {
    let widened = acquired.touched.clone().leasing(more.iter().cloned());
    let fresh: Vec<PathBuf> = widened.leases[acquired.touched.leases.len()..].to_vec();
    let mut written: Vec<PathBuf> = Vec::with_capacity(fresh.len());
    for ws in &fresh {
        let lease_one = || -> anyhow::Result<Option<anyhow::Error>> {
            if let Some(err) = in_flight_refusal_for(ws)? {
                return Ok(Some(err));
            }
            let lease = LeaseRecord {
                id: owner_record.id.clone(),
                owner: CanonicalPath::of(acquired.touched.owner()),
                created_at: Some(utc_now_rfc3339()),
            };
            let json =
                serde_json::to_string_pretty(&lease).context("failed to serialize lease record")?;
            Ok(match LeaseRecord::path_in(ws).create(json.as_bytes()) {
                Ok(()) => None,
                Err(CreateNewError::AlreadyExists) => {
                    Some(in_flight_refusal_for(ws)?.unwrap_or_else(|| {
                        crate::refusal::refusal(
                            RefusalKind::OpAcquisitionRaced,
                            format!("raced with a concurrent op at {}; retry", ws.display()),
                        )
                    }))
                }
                Err(CreateNewError::Io(e)) => Some(
                    anyhow::Error::new(e)
                        .context(format!("failed to acquire a lease at {}", ws.display())),
                ),
                Err(CreateNewError::NoExclusivePublish(e)) => Some(e),
            })
        };
        let refused = lease_one().unwrap_or_else(Some);
        if let Some(err) = refused {
            for taken in &written {
                clear_lease(taken);
            }
            return Err(err);
        }
        written.push(ws.clone());
    }
    acquired.touched = widened;
    Ok(())
}

/// Undo an acquisition performed by [`acquire_op`] — clear the owner record and
/// every acquired lease. Idempotent; missing files are ignored.
///
/// Called when a precondition refuses AFTER a successful acquisition (the
/// cleanup table's "precondition refusal → cleared everywhere" row), and by
/// [`acquire_op`] itself when a partial acquisition hits `AlreadyExists` on a
/// later file.
pub fn release_acquired(acquired: &AcquiredOp) {
    acquired.touched.clear();
}

fn rollback_acquired(owner_workspace_dir: &Path, acquired_leases: &[PathBuf]) {
    for ws in acquired_leases {
        clear_lease(ws);
    }
    clear_owner(owner_workspace_dir);
}

// ---------------------------------------------------------------------------
// Dead-lease detection (structural, no wall-clock policy)
// ---------------------------------------------------------------------------

/// A lease whose recorded owner workspace has no matching `.rwv-op` with the
/// same op id — the structural dead-lease case.
///
/// Structural because it is derived from ancestry-of-state, not from elapsed
/// time: the operator's precedent (matching the stale-op-state
/// doctor check) is that time may be *surfaced* to the operator, never
/// consumed as policy. A dead lease arises from:
///
/// - crash between owner-record acquisition and the caller writing the lease
///   (this concrete window is why we detect it — the crash pattern the
///   acquire→mark ordering leaves behind);
/// - owner workspace deleted / moved out from under the lease;
/// - manual `rm .rwv-op` without also clearing the lease.
///
/// The reverse pattern (owner record with no matching lease anywhere) is
/// caught by the existing `StaleOpState` doctor finding — that scan already
/// walks every workspace and reports every `.rwv-op` it sees. So the *only*
/// gap this new check covers is the lease-side dangling-pointer case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLease {
    /// Absolute path to the workspace directory holding the dangling lease.
    pub workspace_dir: PathBuf,
    /// Op id recorded in the lease.
    pub op_id: OpId,
    /// Owner workspace the lease pointed at.
    pub recorded_owner: PathBuf,
    /// Why the lease is dead (owner missing vs owner op id mismatch), for the
    /// human-facing message.
    pub reason: DeadLeaseReason,
    /// RFC3339 UTC timestamp at which the lease was written, carried through
    /// from [`LeaseRecord::created_at`]. Observability-only: surfaced in
    /// doctor reports, never a decision input.
    pub created_at: Option<String>,
}

/// Discriminator for why a lease is structurally dead. Reported to the
/// operator so `doctor` can name the specific shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeadLeaseReason {
    /// The recorded owner workspace has no `.rwv-op` at all.
    OwnerRecordAbsent,
    /// The recorded owner workspace has an `.rwv-op` but with a *different*
    /// op id — the lease references an op the owner has since moved past
    /// (owner cleared and a new op started, but this stale lease survived).
    OwnerOpIdMismatch { owner_op_id: OpId },
}

/// Inspect the lease at `workspace_dir` and classify it as dead if its owner
/// pointer is dangling. Returns `Ok(None)` when there is no lease, when the
/// lease reads as invalid (surfaced separately by the parse error), or when
/// the lease resolves to an owner record with the matching op id (the live
/// case).
///
/// The classification is purely structural: existence of the owner file and
/// id equality. No elapsed-time input; no filesystem timestamps.
pub fn detect_dead_lease(workspace_dir: &Path) -> anyhow::Result<Option<DeadLease>> {
    let Some(lease) = read_lease(workspace_dir)? else {
        return Ok(None);
    };
    let created_at = lease.created_at.clone();
    let owner_path = OwnerRecord::path_in(lease.owner.as_path());
    if !owner_path.exists() {
        return Ok(Some(DeadLease {
            workspace_dir: workspace_dir.to_path_buf(),
            op_id: lease.id,
            recorded_owner: lease.owner.into_path_buf(),
            reason: DeadLeaseReason::OwnerRecordAbsent,
            created_at,
        }));
    }
    // An owner record whose header does not parse names no op to compare
    // against, so the lease is left undecided rather than called dead.
    if let Ok(Some(owner_header)) = read_owner_header(lease.owner.as_path()) {
        if owner_header.id != lease.id {
            let owner_op_id = owner_header.id.clone();
            return Ok(Some(DeadLease {
                workspace_dir: workspace_dir.to_path_buf(),
                op_id: lease.id,
                recorded_owner: lease.owner.into_path_buf(),
                reason: DeadLeaseReason::OwnerOpIdMismatch { owner_op_id },
                created_at,
            }));
        }
    }
    Ok(None)
}

/// Remove the lease file at `workspace_dir`. Used by `doctor --fix` on a
/// dead-lease finding — safe because the classification proved the lease is
/// no longer paired with a live owner record.
pub fn fix_dead_lease(workspace_dir: &Path) {
    clear_lease(workspace_dir);
}

// ---------------------------------------------------------------------------
// --continue: read op-state for resume
// ---------------------------------------------------------------------------

/// Resume a `--continue` attempt by resolving op-state at `workspace_dir`.
///
/// Follows a lease pointer if the workspace holds a lease.
///
/// Returns an error if no op-state is present.
pub fn resume(workspace_dir: &Path) -> anyhow::Result<ResolvedOwner> {
    match resolve_to_owner(workspace_dir)? {
        Some(resolved)
            if resolved
                .record
                .take_in
                .as_ref()
                .is_some_and(|take_in| take_in.abort_begun) =>
        {
            crate::refuse!(
                RefusalKind::ResumeContradictsRecord,
                "`rwv abort` began rolling this operation back at {}, so it cannot be continued; \
                 run `rwv abort` to finish rolling it back",
                resolved.owner_workspace.as_path().display()
            );
        }
        Some(resolved) => Ok(resolved),
        None => {
            crate::refuse!(
                RefusalKind::NoOpRecorded,
                "no operation in progress to continue at {}. \
                 If you meant to start a new one, omit `--continue`.",
                workspace_dir.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// RFC3339 UTC timestamp for "right now", display-only, never policy.
pub(crate) fn utc_now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let (year, month, day, hour, min, sec) = unix_secs_to_ymd_hms(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Decompose Unix seconds into (year, month, day, hour, min, sec) UTC.
///
/// Uses the algorithm from Richards (2013) — exact for all dates in the
/// range 1970–2399. No overflow for u64 timestamps within that range.
#[allow(clippy::many_single_char_names)]
fn unix_secs_to_ymd_hms(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let hour = (secs / 3600) % 24;
    let min = (secs / 60) % 60;
    let sec = secs % 60;
    let days = secs / 86400;

    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, hour, min, sec)
}

/// Human-readable elapsed time since `started_at` (RFC3339).
///
/// Used by doctor reports for both `StaleOpState` and `DeadOpLease` findings.
/// Observability only — never a decision input.
pub(crate) fn elapsed_since(started_at: &str) -> String {
    if let Some(elapsed_secs) = parse_rfc3339_to_unix(started_at).map(|start| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(start)
    }) {
        if elapsed_secs < 60 {
            return format!("{elapsed_secs}s");
        } else if elapsed_secs < 3600 {
            return format!("{}m", elapsed_secs / 60);
        } else {
            return format!("{}h", elapsed_secs / 3600);
        }
    }
    started_at.to_owned()
}

/// Parse a `YYYY-MM-DDTHH:MM:SSZ` string to Unix seconds. Returns `None` on
/// any parse failure.
fn parse_rfc3339_to_unix(s: &str) -> Option<u64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: u64 = date_parts.next()?.parse().ok()?;
    let month: u64 = date_parts.next()?.parse().ok()?;
    let day: u64 = date_parts.next()?.parse().ok()?;
    let mut time_parts = time.split(':');
    let hour: u64 = time_parts.next()?.parse().ok()?;
    let min: u64 = time_parts.next()?.parse().ok()?;
    let sec: u64 = time_parts.next()?.parse().ok()?;

    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y / 400;
    let yoe = y % 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let z = era * 146097 + doe;
    let days = z.checked_sub(719468)?;

    Some(days * 86400 + hour * 3600 + min * 60 + sec)
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn a_record_in(dir: &Path, schema_fields: &str) {
        let record = format!(
            "{{{schema_fields}\"id\": \"1\", \"verb\": \"sync\", \"history\": null, \
             \"project\": \"p\", \"source\": \"{d}\", \"target\": \"{d}\", \
             \"retire\": false, \"phase\": \"replay\", \
             \"converged_tips\": {{}}, \"started_at\": \"2026-01-01T00:00:00Z\", \
             \"take_in\": null}}",
            d = dir.display()
        );
        std::fs::write(dir.join(OP_STATE_FILE), record).unwrap();
    }

    /// The field a retired consent list left in records of this schema number
    /// is one this release does not read, so such a record is unreadable and
    /// refused with the exit every unreadable record gets.
    #[test]
    fn a_record_still_carrying_a_retired_field_is_unreadable() {
        for retired in ["\"overrides\": [], ", "\"advanced_tips\": {}, "] {
            let tmp = crate::tempdir().unwrap();
            a_record_in(
                tmp.path(),
                &format!("\"schema\": {OP_RECORD_SCHEMA}, \"release\": \"0.25.0\", {retired}"),
            );
            let refused = read_owner(tmp.path()).unwrap_err();
            assert_eq!(
                crate::refusal::kind_of(&refused),
                Some(RefusalKind::UnreadableOpRecord),
                "{retired}"
            );
            assert!(
                format!("{refused:#}").contains(&discard_unreadable_record_at(tmp.path())),
                "{refused:#}"
            );
        }
    }

    #[test]
    fn a_record_of_an_older_schema_is_refused_by_every_reader_naming_its_release() {
        let tmp = crate::tempdir().unwrap();
        a_record_in(tmp.path(), "");
        for refused in [
            read_owner(tmp.path()).map(|_| ()),
            read_owner_header(tmp.path()).map(|_| ()),
        ] {
            let refused = refused.unwrap_err();
            assert_eq!(
                crate::refusal::kind_of(&refused),
                Some(RefusalKind::UnreadableOpRecord)
            );
            let message = format!("{refused:#}");
            assert!(
                message.contains("an rwv release that records no version")
                    && message.contains("resumes or aborts it")
                    && message.contains(&discard_unreadable_record_at(tmp.path())),
                "{message}"
            );
        }
    }

    /// The record's wire shape at [`OP_RECORD_SCHEMA`], as this file declares
    /// it: a field added, removed, renamed, retyped or given a serde
    /// attribute in any type the record reaches changes the digest, and the
    /// change ships with a new schema number and the digest it has there.
    ///
    /// Structural, since no record either release writes tells the two
    /// apart. It reads the declarations reachable from `WireOwnerRecord`,
    /// attributes and fields, comments and blank lines left out, in this
    /// file, then `vcs.rs`, then `manifest.rs`; a type declared in another
    /// module is read by its name alone, and a hand-written serde impl is not
    /// read at all, so a change inside one is not seen here.
    const PINNED_SHAPE: (u32, &str) = (8, "db353d6ba9f401db");

    fn declaration_of<'a>(source: &'a [&'a str], name: &str) -> Option<Vec<&'a str>> {
        let start = source.iter().position(|line| {
            let line = line.trim_start().trim_start_matches("pub ");
            ["struct ", "enum "].iter().any(|kw| {
                line.strip_prefix(kw).is_some_and(|rest| {
                    rest.strip_prefix(name)
                        .is_some_and(|after| after.starts_with([' ', '{', '(', '<']))
                })
            })
        })?;
        let first = (0..start)
            .rev()
            .take_while(|&i| source[i].trim_start().starts_with("#["))
            .last()
            .unwrap_or(start);
        let mut depth = 0i32;
        let mut lines = Vec::new();
        for line in &source[first..] {
            let code = line.trim();
            if code.is_empty() || code.starts_with("//") {
                continue;
            }
            lines.push(code);
            depth += code.matches('{').count() as i32 - code.matches('}').count() as i32;
            if depth == 0 && (code.ends_with('}') || code.ends_with(';')) && lines.len() > 1 {
                break;
            }
        }
        Some(lines)
    }

    fn shape_digest() -> String {
        let sources: Vec<Vec<&str>> = [
            include_str!("op_state.rs"),
            include_str!("vcs.rs"),
            include_str!("manifest.rs"),
        ]
        .iter()
        .map(|file| {
            file.split("\n#[cfg(test)]\nmod ")
                .next()
                .unwrap()
                .lines()
                .collect()
        })
        .collect();
        let mut seen = std::collections::BTreeMap::new();
        let mut pending = vec!["WireOwnerRecord".to_owned()];
        while let Some(name) = pending.pop() {
            if seen.contains_key(&name) {
                continue;
            }
            let Some(lines) = sources
                .iter()
                .find_map(|source| declaration_of(source, &name))
            else {
                continue;
            };
            for word in lines
                .iter()
                .flat_map(|line| line.split(|c: char| !c.is_ascii_alphanumeric() && c != '_'))
                .filter(|word| word.starts_with(|c: char| c.is_ascii_uppercase()))
            {
                pending.push(word.to_owned());
            }
            seen.insert(name, lines.join("\n"));
        }
        assert!(["TakeInRecord", "Carrying", "CommitSummary", "History"]
            .iter()
            .all(|name| seen.contains_key(*name)));
        let shape = seen.into_values().collect::<Vec<_>>().join("\n");
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in shape.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{hash:016x}")
    }

    #[test]
    fn the_record_schema_number_moves_with_its_shape() {
        assert_eq!(
            (OP_RECORD_SCHEMA, shape_digest().as_str()),
            PINNED_SHAPE,
            "the op record's shape and its schema number move together: a shape change \
             bumps OP_RECORD_SCHEMA and pins the new digest beside it"
        );
    }

    #[test]
    fn a_field_this_release_does_not_read_is_refused_naming_its_writer() {
        let tmp = crate::tempdir().unwrap();
        let record = OwnerRecord::new_take_in(
            &"1".parse().unwrap(),
            OpVerb::Pull,
            test_project(),
            tmp.path(),
            TakeInRecord::default(),
        );
        write_owner(tmp.path(), &record).unwrap();
        let path = OwnerRecord::path_in(tmp.path());
        let mut json: serde_json::Value =
            serde_json::from_str(&path.read_to_string().unwrap()).unwrap();
        json["release"] = "9.9.9".into();
        json["take_in"]["moved_somewhere_new"] = serde_json::json!(["x"]);
        path.publish(json.to_string().as_bytes()).unwrap();
        let refused = read_owner(tmp.path()).unwrap_err();
        assert_eq!(
            crate::refusal::kind_of(&refused),
            Some(RefusalKind::UnreadableOpRecord)
        );
        let message = format!("{refused:#}");
        assert!(
            message.contains("rwv 9.9.9 wrote it in a shape")
                && message.contains("moved_somewhere_new"),
            "{message}"
        );
    }

    #[test]
    fn a_field_this_release_does_not_read_inside_an_enum_of_the_record_is_refused() {
        let tmp = crate::tempdir().unwrap();
        let tip = CommitId::parse(&"a".repeat(40)).unwrap();
        let take_in = TakeInRecord {
            sync_from: Some(SyncFrom::Target("main".into())),
            receiving: BTreeMap::from([(
                RepoKey::Project,
                Receiving {
                    branch: "main".into(),
                    tip: tip.clone(),
                    moves: Moves::Follows { create: false },
                },
            )]),
            reports: vec![LandingReport::BeyondPin {
                repo: RepoKey::Project,
                target: "main".into(),
                commits: vec![crate::vcs::CommitSummary {
                    id: tip,
                    subject: "s".into(),
                }],
            }],
            ..TakeInRecord::default()
        };
        let record = OwnerRecord::new_take_in(
            &"1".parse().unwrap(),
            OpVerb::Land,
            test_project(),
            tmp.path(),
            take_in,
        );
        write_owner(tmp.path(), &record).unwrap();
        let path = OwnerRecord::path_in(tmp.path());
        let written: serde_json::Value =
            serde_json::from_str(&path.read_to_string().unwrap()).unwrap();
        assert!(read_owner(tmp.path()).unwrap().is_some());
        type Plant = fn(&mut serde_json::Value);
        let plants: [(&str, Plant); 4] = [
            ("Moves", |r| {
                r["take_in"]["receiving"]["(project)"]["moves"]["follows"]["beyond"] = 1.into()
            }),
            ("LandingReport", |r| {
                r["take_in"]["reports"][0]["beyond"] = 1.into()
            }),
            ("SyncFrom", |r| {
                r["take_in"]["sync_from"]["beyond"] = 1.into()
            }),
            ("CommitSummary", |r| {
                r["take_in"]["reports"][0]["commits"][0]["beyond"] = 1.into()
            }),
        ];
        for (inside, plant) in plants {
            let mut json = written.clone();
            plant(&mut json);
            path.publish(json.to_string().as_bytes()).unwrap();
            let refused = read_owner(tmp.path()).unwrap_err();
            assert_eq!(
                crate::refusal::kind_of(&refused),
                Some(RefusalKind::UnreadableOpRecord),
                "{inside}: {refused:#}"
            );
            assert!(
                format!("{refused:#}").contains("\"beyond\"")
                    || format!("{refused:#}").contains("`beyond`"),
                "{inside}: {refused:#}"
            );
        }
    }

    #[test]
    fn a_record_of_the_current_schema_reads_whole() {
        let tmp = crate::tempdir().unwrap();
        a_record_in(
            tmp.path(),
            &format!("\"schema\": {OP_RECORD_SCHEMA}, \"release\": \"1.2.3\", "),
        );
        assert!(read_owner(tmp.path()).unwrap().is_some());
    }

    fn tip(hex: char) -> CommitId {
        CommitId::parse(&hex.to_string().repeat(40)).unwrap()
    }

    fn member(path: &str) -> RepoKey {
        RepoKey::Member(crate::manifest::RepoPath::new(path).unwrap())
    }

    fn test_project() -> ProjectName {
        ProjectName::new("web-app").expect("valid project name")
    }

    /// The resume verb is derived from the op's `verb` field, never hardcoded
    /// `rwv sync`: naming the pull verb for a `land` op sends the operator
    /// into the verb-mismatch refusal.
    #[test]
    fn resume_command_derives_from_op_verb() {
        assert_eq!(resume_command(OpVerb::Sync), "rwv sync --continue");
        assert_eq!(resume_command(OpVerb::Land), "rwv land --continue");
    }

    // -----------------------------------------------------------------------
    // OpId and SavepointKey
    // -----------------------------------------------------------------------

    #[test]
    fn a_record_with_its_tips_is_rewritten_byte_for_byte() {
        let tip = |c: char| c.to_string().repeat(40);
        let tmp = crate::tempdir().unwrap();
        let place = |name: &str| {
            let dir = tmp.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            serde_json::to_string(&crate::path_spelling::resolve(&dir).unwrap()).unwrap()
        };
        let (source, target) = (place("src"), place("tgt"));
        let record = format!(
            r#"{{
  "schema": 8,
  "release": "{release}",
  "id": "1779769917405921588",
  "verb": "land",
  "history": null,
  "project": "web-app",
  "source": {source},
  "target": {target},
  "retire": false,
  "phase": "relock",
  "converged_tips": {{
    "(project)": "{d}",
    "github/acme/api": "{a}",
    "github/acme/web": "{b}"
  }},
  "started_at": "2026-06-10T21:14:03Z",
  "take_in": null
}}"#,
            a = tip('a'),
            b = tip('b'),
            d = tip('d'),
            release = crate::rwv_version(),
        );

        let read: OwnerRecord = serde_json::from_str(&record).unwrap();
        assert_eq!(read.converged_tips.len(), 3);
        assert!(read.converged_tips.contains_key(&RepoKey::Project));
        assert!(read.converged_tips.contains_key(&member("github/acme/api")));
        assert_eq!(serde_json::to_string_pretty(&read).unwrap(), record);
    }

    #[test]
    fn an_op_id_refuses_what_no_op_id_can_be() {
        for text in [
            "",
            "a b",
            "a/b",
            "a.b",
            "1779769917405921588-target",
            "-target",
            "1779769917405921588-in-place",
        ] {
            assert!(text.parse::<OpId>().is_err(), "{text:?} is not an op id");
        }
    }

    #[test]
    fn an_op_id_accepts_the_clock_and_the_planted_spellings() {
        let minted = OpId::new_now();
        assert_eq!(minted.as_str().parse::<OpId>().unwrap(), minted);
        for text in [
            "1779769917405921588",
            "20991231T000001Z",
            "crash-matrix-e_1",
            "target",
        ] {
            assert_eq!(text.parse::<OpId>().unwrap().as_str(), text);
        }
    }

    #[test]
    fn a_savepoint_key_reads_back_as_the_key_it_spells() {
        let op: OpId = "1779769917405921588".parse().unwrap();
        for key in [
            SavepointKey::Op(op.clone()),
            SavepointKey::LandTarget(op.clone()),
        ] {
            assert_eq!(key.to_string().parse::<SavepointKey>().unwrap(), key);
            assert_eq!(key.op(), &op);
        }
        assert_eq!(
            SavepointKey::Op(op.clone()).to_string(),
            "1779769917405921588"
        );
        assert_eq!(
            SavepointKey::LandTarget(op).to_string(),
            "1779769917405921588-target"
        );
    }

    #[test]
    fn a_landing_files_savepoints_on_both_sides_and_a_sync_on_one() {
        let op = OpId::new_now();
        let sync = make_sync_record(&op, Path::new("/own"), Path::new("/src"));
        let landing = make_land_record(&op, Path::new("/own"), Path::new("/tgt"));

        assert_eq!(sync.savepoint_keys(), vec![SavepointKey::Op(op.clone())]);
        assert_eq!(
            landing.savepoint_keys(),
            vec![
                SavepointKey::Op(op.clone()),
                SavepointKey::LandTarget(op.clone()),
                SavepointKey::LandInPlace(op)
            ]
        );
    }

    #[test]
    fn a_savepoint_key_refuses_text_that_names_no_op() {
        for text in ["", "-target", "a-target-target", "a b-target"] {
            assert!(
                text.parse::<SavepointKey>().is_err(),
                "{text:?} names no key"
            );
        }
    }

    // -----------------------------------------------------------------------
    // The one-run history on the wire
    // -----------------------------------------------------------------------

    #[test]
    fn the_one_run_history_is_written_as_its_token() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        for history in crate::manifest::History::ALL {
            let record = OwnerRecord::new_sync(
                &OpId::new_now(),
                Some(history),
                test_project(),
                Path::new("/src/ws"),
                Path::new("/cwd/ws"),
            );
            write_owner(dir, &record).unwrap();
            let json = std::fs::read_to_string(dir.join(OP_STATE_FILE)).unwrap();
            assert!(
                json.contains(&format!("\"history\": \"{history}\"")),
                "the history must be written as `{history}`; got:\n{json}"
            );
            assert_eq!(read_owner(dir).unwrap().unwrap().history, Some(history));
        }
    }

    #[test]
    fn utc_roundtrip_is_plausible() {
        let ts = utc_now_rfc3339();
        assert!(
            ts.len() == 20 && ts.ends_with('Z') && ts.contains('T'),
            "unexpected timestamp format: {ts}"
        );
        assert!(
            parse_rfc3339_to_unix(&ts).is_some(),
            "timestamp {ts} failed to round-trip"
        );
    }

    #[test]
    fn elapsed_since_returns_secs_for_recent() {
        let ts = utc_now_rfc3339();
        let elapsed = elapsed_since(&ts);
        assert!(
            elapsed.ends_with('s'),
            "expected seconds-suffix for fresh timestamp; got {elapsed}"
        );
    }

    // -----------------------------------------------------------------------
    // Owner record round-trip
    // -----------------------------------------------------------------------

    #[test]
    fn owner_write_read_roundtrip_sync() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src/ws"),
            Path::new("/cwd/ws"),
        );
        write_owner(dir, &record).unwrap();
        let read_back = read_owner(dir).unwrap().unwrap();
        assert_eq!(read_back.id, record.id);
        assert_eq!(read_back.verb, OpVerb::Sync);
        assert_eq!(read_back.history, None);
        assert_eq!(read_back.project, test_project());
        assert_eq!(read_back.phase, OpPhase::Replay);
        assert!(!read_back.retire);
        assert!(read_back.converged_tips.is_empty());
    }

    #[test]
    fn wire_record_missing_project_key_is_rejected() {
        // The op's project is what abort resolves both of the op's workspaces
        // under. A record without it has no answer that is not a guess at
        // ambient state, so it is malformed rather than defaulted.
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{
  "id": "9999999999999999999",
  "verb": "sync",
  "history": null,
  "source": "/src/ws",
  "target": "/cwd/ws",
  "retire": false,
  "phase": "replay",
  "converged_tips": {},
  "started_at": "2026-06-01T00:00:00Z", "schema": 8, "release": "0.0.0", "take_in": null
}
"#;
        std::fs::write(dir.join(OP_STATE_FILE), json).unwrap();
        let err = read_owner(dir).unwrap_err();
        assert!(
            crate::refusal::kind_of(&err) == Some(RefusalKind::UnreadableOpRecord),
            "a record missing project must fail to parse; got: {err}"
        );
    }

    #[test]
    fn wire_record_rejects_a_project_name_the_type_refuses() {
        // The wire field is the validated newtype, not a String the reader
        // widens back into one: a name no `ProjectName` could be built from
        // is a parse error at the boundary rather than a path component
        // assembled later.
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{
  "id": "9999999999999999999",
  "verb": "sync",
  "history": null,
  "project": "web--app",
  "source": "/src/ws",
  "target": "/cwd/ws",
  "retire": false,
  "phase": "replay",
  "converged_tips": {},
  "started_at": "2026-06-01T00:00:00Z", "schema": 8, "release": "0.0.0", "take_in": null
}
"#;
        std::fs::write(dir.join(OP_STATE_FILE), json).unwrap();
        let err = read_owner(dir).unwrap_err();
        assert!(
            crate::refusal::kind_of(&err) == Some(RefusalKind::UnreadableOpRecord),
            "a record whose project name fails validation must fail to parse; got: {err}"
        );
    }

    fn record_json(id: &str, history: &str) -> String {
        format!(
            r#"{{
  "id": "{id}",
  "verb": "sync",
  "history": {history},
  "project": "web-app",
  "source": "/src/ws",
  "target": "/cwd/ws",
  "retire": false,
  "phase": "replay",
  "converged_tips": {{}},
  "started_at": "2026-06-01T00:00:00Z", "schema": 8, "release": "0.0.0", "take_in": null
}}
"#
        )
    }

    #[test]
    fn wire_record_rejects_an_id_in_the_target_namespace() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(
            dir.join(OP_STATE_FILE),
            record_json("1779769917405921588-target", "null"),
        )
        .unwrap();
        let err = read_owner(dir).unwrap_err();
        assert!(
            format!("{err:#}").contains("1779769917405921588-target"),
            "an op id carrying the target-side suffix must fail to parse, naming it; got: {err:#}"
        );
    }

    #[test]
    fn wire_record_rejects_a_history_no_variant_names() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(
            dir.join(OP_STATE_FILE),
            record_json("1779769917405921588", "\"rebase\""),
        )
        .unwrap();
        let err = read_owner(dir).unwrap_err();
        let shown = format!("{err:#}");
        assert!(
            shown.contains("rebase") && shown.contains("linear") && shown.contains("ff-only"),
            "a history the enum does not have must fail at read, naming what is accepted; \
             got: {shown}"
        );
    }

    #[test]
    fn wire_record_missing_converged_tips_key_is_rejected() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{
  "id": "9999999999999999999",
  "verb": "sync",
  "history": null,
  "project": "web-app",
  "source": "/src/ws",
  "target": "/cwd/ws",
  "retire": false,
  "phase": "replay",
  "started_at": "2026-06-01T00:00:00Z", "schema": 8, "release": "0.0.0", "take_in": null
}
"#;
        std::fs::write(dir.join(OP_STATE_FILE), json).unwrap();
        let err = read_owner(dir).unwrap_err();
        assert_eq!(
            crate::refusal::kind_of(&err),
            Some(RefusalKind::UnreadableOpRecord),
            "a record missing converged_tips must fail to parse; got: {err:#}"
        );
        assert!(format!("{err:#}").contains("converged_tips"), "{err:#}");
    }

    #[test]
    fn converged_tips_wire_roundtrip() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let mut record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src/ws"),
            Path::new("/cwd/ws"),
        );
        record
            .converged_tips
            .insert(member("github/foo/bar"), tip('c'));
        record.converged_tips.insert(RepoKey::Project, tip('d'));
        record.phase = OpPhase::Relock;

        write_owner(dir, &record).unwrap();
        let raw = std::fs::read_to_string(dir.join(OP_STATE_FILE)).unwrap();
        assert!(
            raw.contains("\"converged_tips\""),
            "expected converged_tips key, got:\n{raw}"
        );

        let read_back = read_owner(dir).unwrap().unwrap();
        assert_eq!(read_back, record);
        let converged = &read_back.converged_tips;
        assert_eq!(converged.len(), 2);
        assert_eq!(
            converged.get(&member("github/foo/bar")).cloned(),
            Some(tip('c')),
        );
    }

    #[test]
    fn a_tag_name_as_a_tip_is_refused_at_load() {
        let converged = r#"{"github/a/b": "v2.0"}"#;
        {
            let tmp = crate::tempdir().unwrap();
            let dir = tmp.path();
            let json = format!(
                r#"{{"id": "1", "verb": "sync", "history": null, "project": "web-app",
                "source": "/src/ws", "target": "/cwd/ws", "retire": false, "phase": "replay",
                "converged_tips": {converged},
                "started_at": "2026-06-01T00:00:00Z", "schema": 8, "release": "0.0.0", "take_in": null}}"#
            );
            std::fs::write(dir.join(OP_STATE_FILE), json).unwrap();
            let err = read_owner(dir).expect_err("a tag name is not a commit id");
            assert!(
                format!("{err:#}").contains("'v2.0' is not a commit id"),
                "converged {converged}: {err:#}"
            );
        }
    }

    #[test]
    fn owner_write_read_roundtrip_land() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_land(
            &op_id,
            None,
            test_project(),
            Path::new("/cwd/ws"),
            Some(Path::new("/tgt/ws")),
            true,
        );
        write_owner(dir, &record).unwrap();
        let read_back = read_owner(dir).unwrap().unwrap();
        assert_eq!(read_back.verb, OpVerb::Land);
        assert_eq!(read_back.history, None);
        assert_eq!(read_back.phase, OpPhase::Replay);
        assert!(read_back.retire);
    }

    #[test]
    fn read_owner_returns_none_when_absent() {
        let tmp = crate::tempdir().unwrap();
        assert!(read_owner(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn clear_owner_removes_file() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src"),
            Path::new("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        assert!(read_owner(dir).unwrap().is_some());
        clear_owner(dir);
        assert!(read_owner(dir).unwrap().is_none());
    }

    #[test]
    fn an_ending_op_clears_only_its_own_record_and_leases() {
        let tmp = crate::tempdir().unwrap();
        let (owner, other, leased) = (
            tmp.path().join("owner"),
            tmp.path().join("other"),
            tmp.path().join("leased"),
        );
        for dir in [&owner, &other, &leased] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let (ended, next) = (OpId::new_now(), "1".parse::<OpId>().unwrap());
        let record_of = |id: &OpId, at: &Path| {
            OwnerRecord::new_sync(id, None, test_project(), at, Path::new("/tgt"))
        };
        write_owner(&owner, &record_of(&next, &owner)).unwrap();
        write_owner(&other, &record_of(&next, &other)).unwrap();
        acquire_lease(&leased, &other, &next).unwrap();
        clear_owner_of(&owner, &ended);
        clear_lease_owned_by(&leased, &owner);
        assert!(
            read_owner(&owner).unwrap().is_some(),
            "another op's record stays"
        );
        assert!(
            read_lease(&leased).unwrap().is_some(),
            "another op's lease stays"
        );
        clear_lease_owned_by(&leased, &other);
        assert!(read_lease(&leased).unwrap().is_none());
        clear_owner_of(&owner, &next);
        assert!(read_owner(&owner).unwrap().is_none());
    }

    #[test]
    fn set_phase_updates_owner_file() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src"),
            Path::new("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        set_phase(dir, OpPhase::Relock).unwrap();
        let updated = read_owner(dir).unwrap().unwrap();
        assert_eq!(updated.phase, OpPhase::Relock);
    }

    // -----------------------------------------------------------------------
    // write_owner durability — crash recovery via the durable_file rename
    // -----------------------------------------------------------------------

    /// Atomicity, pinned by the mechanism rather than by hoping: an overwrite
    /// replaces `.rwv-op` by `rename(2)`, so the target's inode changes and no
    /// reader can ever catch a partial write. An in-place rewrite (the pre-fix
    /// `std::fs::write`) would keep the inode — and would be observable
    /// half-written.
    ///
    /// Not gated because its subject is Unix — replace-by-rename is the invariant
    /// the whole owned-file discipline rests on, and it holds on any platform.
    /// Gated because the INSTRUMENT is unverified: this proves the replacement by
    /// watching the inode change, and whether NTFS gives a renamed-over file a new
    /// file index is not something this repository can check. Ported on the
    /// assumption that it does, it would go red against correct code.
    #[test]
    #[cfg(unix)]
    fn write_owner_replaces_by_rename_not_in_place() {
        use std::os::unix::fs::MetadataExt;

        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let mut record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src"),
            Path::new("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        let path = OwnerRecord::path_in(dir);
        let first_inode = path.symlink_metadata().unwrap().ino();

        record.phase = OpPhase::Relock;
        write_owner(dir, &record).unwrap();
        assert_ne!(
            path.symlink_metadata().unwrap().ino(),
            first_inode,
            "the owner record must be replaced by rename, not rewritten in place"
        );

        let leftovers: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    /// A write that fails partway must not touch `.rwv-op` at all: the prior
    /// record has to survive exactly as it was. A read-only directory forces
    /// the failure at the earliest point `durable_file::replace` can hit one
    /// — its temp file needs `O_CREAT` on a fresh name, which the directory
    /// refuses — while leaving the existing `.rwv-op` itself writable. That's
    /// the same asymmetry a crash mid-write exploits: truncating an
    /// already-existing, already-permitted file needs no directory
    /// permission at all, so the pre-fix `std::fs::write` would sail through
    /// this and clobber the record instead of failing.
    ///
    /// Not gated because its subject is Unix — a failed write must not damage
    /// the prior record on any platform. Gated on the obstruction: a Windows
    /// read-only directory attribute does not stop a file being created inside
    /// it, so the write would succeed, and this would go red against correct
    /// code rather than pass vacuously. Denying that creation takes an ACL deny
    /// entry, which is a different fixture, not a different spelling.
    #[test]
    #[cfg(unix)]
    fn write_owner_failure_leaves_prior_record_recoverable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let original = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src"),
            Path::new("/tgt"),
        );
        write_owner(dir, &original).unwrap();

        let writable_perms = std::fs::metadata(dir).unwrap().permissions();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let mut next = original.clone();
        next.phase = OpPhase::Relock;
        let result = write_owner(dir, &next);

        std::fs::set_permissions(dir, writable_perms).unwrap();

        assert!(
            result.is_err(),
            "a write into a read-only directory must fail, not silently succeed"
        );
        assert_eq!(
            read_owner(dir).unwrap().unwrap(),
            original,
            "a failed write must leave the prior record exactly as it was"
        );
    }

    /// `read_owner` must never mis-parse a torn record as something else, and
    /// must never panic on one: no prefix of a valid record's bytes shorter
    /// than the whole thing parses, since a truncated JSON object is always
    /// missing its closing brace. This pins the read side's half of crash
    /// safety; the write side — `write_owner_replaces_by_rename_not_in_place`
    /// and `write_owner_failure_leaves_prior_record_recoverable` — is what
    /// keeps a torn file from ever landing at `.rwv-op` through `write_owner`
    /// itself. This test only covers a torn file that got there some other
    /// way (a full disk mid-rename, a hand-edited file).
    #[test]
    fn read_owner_rejects_every_truncated_prefix() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src/ws"),
            Path::new("/cwd/ws"),
        );
        let json = serde_json::to_string_pretty(&record).unwrap();
        let path = OwnerRecord::path_in(dir);

        for len in 0..json.len() {
            path.publish(&json.as_bytes()[..len]).unwrap();
            let result = read_owner(dir);
            assert!(
                result.is_err(),
                "a {len}-byte prefix of a valid record must not parse; got {result:?}"
            );
        }

        path.publish(json.as_bytes()).unwrap();
        assert_eq!(read_owner(dir).unwrap().unwrap(), record);
    }

    // -----------------------------------------------------------------------
    // Lease round-trip
    // -----------------------------------------------------------------------

    /// Publish `lease` at `workspace_dir` by replacement — the shape
    /// `acquire_op` forbids production from using, needed here because
    /// several fixtures below plant lease states `acquire_op` cannot produce
    /// directly: a lease with no owner record, or one whose id doesn't match
    /// the owner's. Private to this module so the replacement shape can't
    /// reach a caller outside tests.
    fn overwrite_lease_in_test(workspace_dir: &Path, lease: &LeaseRecord) -> anyhow::Result<()> {
        let path = LeaseRecord::path_in(workspace_dir);
        let json =
            serde_json::to_string_pretty(lease).context("failed to serialize lease record")?;
        path.publish(json.as_bytes())
            .with_context(|| format!("failed to write lease to {path}"))
    }

    #[test]
    fn lease_write_read_roundtrip() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let lease = LeaseRecord {
            id: "1234567890".parse().unwrap(),
            owner: CanonicalPath::of(Path::new("/owner/ws")),
            created_at: None,
        };
        overwrite_lease_in_test(dir, &lease).unwrap();
        let read_back = read_lease(dir).unwrap().unwrap();
        assert_eq!(read_back.id.as_str(), "1234567890");
        assert_eq!(read_back.owner, CanonicalPath::of(Path::new("/owner/ws")));
    }

    #[test]
    fn read_lease_returns_none_when_absent() {
        let tmp = crate::tempdir().unwrap();
        assert!(read_lease(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn lease_missing_created_at_key_parses_as_none() {
        // Unlike WireOwnerRecord's fields, `created_at`'s missing-key
        // tolerance isn't something `#[serde(default)]` controls: serde's
        // derive treats a syntactic `Option<T>` field as optional-on-the-wire
        // unconditionally, so this parses whether or not the attribute is
        // present.
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let json = r#"{"id": "1234567890", "owner": "/owner/ws"}"#;
        std::fs::write(dir.join(OP_LEASE_FILE), json).unwrap();
        let lease = read_lease(dir).unwrap().unwrap();
        assert_eq!(lease.created_at, None);
    }

    #[test]
    fn clear_lease_removes_file() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let lease = LeaseRecord {
            id: "abc".parse().unwrap(),
            owner: CanonicalPath::of(Path::new("/owner")),
            created_at: None,
        };
        overwrite_lease_in_test(dir, &lease).unwrap();
        assert!(read_lease(dir).unwrap().is_some());
        clear_lease(dir);
        assert!(read_lease(dir).unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // resolve_to_owner (pointer-follow)
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_owner_from_owner_workspace() {
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        std::fs::create_dir_all(&owner_dir).unwrap();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src"),
            Path::new("/tgt"),
        );
        write_owner(&owner_dir, &record).unwrap();

        let resolved = resolve_to_owner(&owner_dir).unwrap().unwrap();
        assert_eq!(resolved.owner_workspace, CanonicalPath::of(&owner_dir));
        assert_eq!(resolved.record.id, record.id);
    }

    #[test]
    fn resolve_owner_from_lease_workspace_follows_pointer() {
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();

        let op_id = OpId::new_now();
        let record = OwnerRecord::new_land(
            &op_id,
            None,
            test_project(),
            Path::new("/cwd"),
            Some(Path::new("/tgt")),
            false,
        );
        write_owner(&owner_dir, &record).unwrap();

        let lease = LeaseRecord {
            id: op_id.clone(),
            owner: CanonicalPath::of(&owner_dir),
            created_at: None,
        };
        overwrite_lease_in_test(&lease_dir, &lease).unwrap();

        let resolved = resolve_to_owner(&lease_dir).unwrap().unwrap();
        assert_eq!(resolved.owner_workspace, CanonicalPath::of(&owner_dir));
        assert_eq!(resolved.record.id, record.id);
        assert_eq!(resolved.record.verb, OpVerb::Land);
    }

    #[test]
    fn resolve_to_owner_returns_none_when_absent() {
        let tmp = crate::tempdir().unwrap();
        assert!(resolve_to_owner(tmp.path()).unwrap().is_none());
    }

    // -----------------------------------------------------------------------
    // check_no_op_in_progress
    // -----------------------------------------------------------------------

    #[test]
    fn check_no_op_passes_when_clean() {
        let tmp = crate::tempdir().unwrap();
        assert!(check_no_op_in_progress(&[tmp.path()]).is_ok());
    }

    #[test]
    fn check_no_op_fails_when_owner_record_present() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src"),
            Path::new("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        let err = check_no_op_in_progress(&[dir]).unwrap_err().to_string();
        assert!(
            err.contains("in progress"),
            "expected 'in progress' in error: {err}"
        );
        assert!(
            err.contains("--continue"),
            "expected '--continue' hint in error: {err}"
        );
    }

    #[test]
    fn check_no_op_fails_when_lease_present() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path().join("leased");
        let owner_dir = tmp.path().join("owner");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&owner_dir).unwrap();
        let op_id = OpId::new_now();
        write_owner(&owner_dir, &make_land_record(&op_id, &owner_dir, &dir)).unwrap();
        let lease = LeaseRecord {
            id: op_id,
            owner: CanonicalPath::of(&owner_dir),
            created_at: None,
        };
        overwrite_lease_in_test(&dir, &lease).unwrap();
        let err = check_no_op_in_progress(&[&dir]).unwrap_err().to_string();
        assert!(
            err.contains("in progress"),
            "expected 'in progress' in error: {err}"
        );
    }

    /// The three in-flight refusals below are pinned whole rather than by
    /// token. Containment is the right instrument where the advice IS the
    /// token, which is most of this crate. These three carry it in a clause —
    /// which command to rerun, and from WHICH workspace — and a clause dropped
    /// between the tokens changes what the operator does while every
    /// containment assertion over the same message stays green.
    ///
    /// `started_at` is set far enough back to land in the hours bucket, so the
    /// one interpolated value is stable across the two reads.
    fn stale_started_at() -> String {
        "2020-01-01T00:00:00Z".to_owned()
    }

    #[test]
    fn check_no_op_owner_refusal_is_the_whole_template() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let mut record = OwnerRecord::new_land(
            &op_id,
            None,
            test_project(),
            Path::new("/cwd"),
            Some(Path::new("/tgt")),
            false,
        );
        record.started_at = stale_started_at();
        write_owner(dir, &record).unwrap();
        let err = check_no_op_in_progress(&[dir]).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "land in progress (started {elapsed} ago, mid `replay`) at {dir}.\n\
                 Rerun with `rwv land --continue` after resolving; or {ABORT_ROLLS_BACK}.",
                elapsed = elapsed_since(&record.started_at),
                dir = dir.display(),
            )
        );
    }

    /// A workspace holding only a thin lease still gets the full op / age /
    /// phase / owner detail — the guard follows the lease pointer to the owner
    /// record, and every field below arrives from there.
    #[test]
    fn check_no_op_lease_refusal_is_the_whole_template() {
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();

        let op_id = OpId::new_now();
        let mut record = OwnerRecord::new_land(
            &op_id,
            None,
            test_project(),
            &owner_dir,
            Some(Path::new("/tgt")),
            false,
        );
        record.started_at = stale_started_at();
        write_owner(&owner_dir, &record).unwrap();
        let lease = LeaseRecord {
            id: op_id.clone(),
            owner: CanonicalPath::of(&owner_dir),
            created_at: None,
        };
        overwrite_lease_in_test(&lease_dir, &lease).unwrap();

        let err = check_no_op_in_progress(&[lease_dir.as_path()])
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            format!(
                "land in progress (started {elapsed} ago, mid `replay`); this \
                 workspace ({dir}) is leased to it. Owner workspace: {owner}.\n\
                 Rerun with `rwv land --continue` after resolving; or {ABORT_ROLLS_BACK}.",
                elapsed = elapsed_since(&record.started_at),
                dir = lease_dir.display(),
                owner = owner_dir.display(),
            )
        );
    }

    /// A lease whose owner records no op cannot be resumed or aborted, so its
    /// refusal names the repair that releases it rather than resume-or-abort,
    /// which would send the operator to a verb that finds nothing.
    #[test]
    fn check_no_op_dangling_lease_refusal_is_the_whole_template() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let owner = tmp.path().join("nonexistent-owner");
        let lease = LeaseRecord {
            id: "dangling-op".parse().unwrap(),
            owner: CanonicalPath::of(&owner),
            created_at: None,
        };
        overwrite_lease_in_test(dir, &lease).unwrap();
        let err = check_no_op_in_progress(&[dir]).unwrap_err();
        assert_eq!(
            crate::refusal::kind_of(&err),
            Some(RefusalKind::DeadOpLease)
        );
        assert_eq!(
            err.to_string(),
            format!(
                "{dir} is leased to op dangling-op, but its owner workspace {owner} records \
                 no such op, so nothing can resume or abort it. `rwv doctor --fix` run in \
                 {dir} releases the lease.",
                dir = dir.display(),
                owner = owner.display(),
            )
        );
    }

    #[test]
    fn a_lease_whose_owner_records_another_op_is_refused_as_dead() {
        let tmp = crate::tempdir().unwrap();
        let lease_dir = tmp.path().join("leased");
        let owner_dir = tmp.path().join("owner");
        std::fs::create_dir_all(&lease_dir).unwrap();
        std::fs::create_dir_all(&owner_dir).unwrap();
        let record = OwnerRecord::new_sync(
            &OpId::new_now(),
            None,
            test_project(),
            &owner_dir,
            &owner_dir,
        );
        write_owner(&owner_dir, &record).unwrap();
        let lease = LeaseRecord {
            id: "finished-op".parse().unwrap(),
            owner: CanonicalPath::of(&owner_dir),
            created_at: None,
        };
        overwrite_lease_in_test(&lease_dir, &lease).unwrap();

        let err = check_no_op_in_progress(&[&lease_dir]).unwrap_err();
        assert_eq!(
            crate::refusal::kind_of(&err),
            Some(RefusalKind::DeadOpLease)
        );
        let err = resolve_to_owner(&lease_dir).unwrap_err();
        assert_eq!(
            crate::refusal::kind_of(&err),
            Some(RefusalKind::DeadOpLease)
        );
    }

    #[test]
    fn a_record_whose_header_reads_and_whose_tips_do_not_is_refused_whole() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_land(
            &op_id,
            None,
            test_project(),
            Path::new("/src"),
            Some(Path::new("/tgt")),
            false,
        );
        write_owner(dir, &record).unwrap();
        let path = OwnerRecord::path_in(dir);
        let mut json: serde_json::Value =
            serde_json::from_str(&path.read_to_string().unwrap()).unwrap();
        json["converged_tips"]["(project)"] = "v1.0".into();
        path.publish(json.to_string().as_bytes()).unwrap();

        for refused in [
            read_owner(dir).map(|_| ()),
            read_owner_header(dir).map(|_| ()),
        ] {
            let refused = refused.unwrap_err();
            assert_eq!(
                crate::refusal::kind_of(&refused),
                Some(RefusalKind::UnreadableOpRecord)
            );
            let message = format!("{refused:#}");
            assert!(
                message.contains("'v1.0' is not a commit id")
                    && message.contains(&discard_unreadable_record_at(dir)),
                "{message}"
            );
        }
    }

    #[test]
    fn check_no_op_fails_across_multiple_dirs() {
        let tmp = crate::tempdir().unwrap();
        let empty_dir = tmp.path().join("empty");
        let dirty_dir = tmp.path().join("dirty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        std::fs::create_dir_all(&dirty_dir).unwrap();

        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src"),
            Path::new("/tgt"),
        );
        write_owner(&dirty_dir, &record).unwrap();

        let err = check_no_op_in_progress(&[empty_dir.as_path(), dirty_dir.as_path()])
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("in progress"),
            "expected in-progress error: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // resume (--continue)
    // -----------------------------------------------------------------------

    #[test]
    fn resume_returns_record_for_owner_workspace() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src"),
            Path::new("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        let ResolvedOwner {
            record: resumed,
            owner_workspace: owner_ws,
        } = resume(dir).unwrap();
        assert_eq!(resumed.id, record.id);
        assert_eq!(owner_ws, CanonicalPath::of(dir));
    }

    #[test]
    fn resume_follows_lease_pointer() {
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();

        let op_id = OpId::new_now();
        let record = OwnerRecord::new_land(
            &op_id,
            None,
            test_project(),
            Path::new("/cwd"),
            Some(Path::new("/tgt")),
            false,
        );
        write_owner(&owner_dir, &record).unwrap();
        let lease = LeaseRecord {
            id: op_id.clone(),
            owner: CanonicalPath::of(&owner_dir),
            created_at: None,
        };
        overwrite_lease_in_test(&lease_dir, &lease).unwrap();

        let ResolvedOwner {
            record: resumed,
            owner_workspace: owner_ws,
        } = resume(&lease_dir).unwrap();
        assert_eq!(resumed.id, record.id);
        assert_eq!(owner_ws, CanonicalPath::of(&owner_dir));
    }

    #[test]
    fn resume_errors_when_no_op_present() {
        let tmp = crate::tempdir().unwrap();
        let err = resume(tmp.path()).unwrap_err().to_string();
        assert!(
            err.contains("no operation in progress"),
            "expected 'no operation in progress' error; got: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Cleanup paths
    // -----------------------------------------------------------------------

    #[test]
    fn clear_all_at_removes_both_files() {
        let tmp = crate::tempdir().unwrap();
        let dir = tmp.path();
        let op_id = OpId::new_now();
        let record = OwnerRecord::new_sync(
            &op_id,
            None,
            test_project(),
            Path::new("/src"),
            Path::new("/tgt"),
        );
        write_owner(dir, &record).unwrap();
        let lease = LeaseRecord {
            id: op_id.clone(),
            owner: CanonicalPath::of(Path::new("/owner")),
            created_at: None,
        };
        overwrite_lease_in_test(dir, &lease).unwrap();
        assert!(OwnerRecord::path_in(dir).exists());
        assert!(LeaseRecord::path_in(dir).exists());
        clear_all_at(dir);
        assert!(!OwnerRecord::path_in(dir).exists());
        assert!(!LeaseRecord::path_in(dir).exists());
    }

    // -----------------------------------------------------------------------
    // Phase enum serialization
    // -----------------------------------------------------------------------

    #[test]
    fn phase_serializes_to_kebab_case() {
        let cases = [
            (OpPhase::Replay, "replay"),
            (OpPhase::Relock, "relock"),
            (OpPhase::AdvanceTarget, "advance-target"),
            (OpPhase::Retire, "retire"),
        ];
        for (phase, expected) in cases {
            let display = phase.to_string();
            assert_eq!(display, expected, "Display mismatch for {phase:?}");
        }
    }

    // -----------------------------------------------------------------------
    // Atomic acquisition (guard→mark TOCTOU fix)
    // -----------------------------------------------------------------------

    fn sync_touched(owner: &Path) -> TouchedWorkspaces {
        TouchedWorkspaces::of(OpVerb::Sync, owner, Some(owner))
    }

    fn land_touched(owner: &Path, target: &Path) -> TouchedWorkspaces {
        TouchedWorkspaces::of(OpVerb::Land, owner, Some(target))
    }

    fn make_sync_record(op_id: &OpId, owner: &Path, source: &Path) -> OwnerRecord {
        OwnerRecord::new_sync(op_id, None, test_project(), source, owner)
    }

    fn make_land_record(op_id: &OpId, owner: &Path, target: &Path) -> OwnerRecord {
        OwnerRecord::new_land(op_id, None, test_project(), owner, Some(target), false)
    }

    #[test]
    fn in_place_moves_come_from_the_in_place_journal_alone() {
        let workspace = PathBuf::from("/third");
        let repo = RepoKey::Member(crate::manifest::RepoPath::new("app").unwrap());
        let tip = CommitId::parse(&"a".repeat(40)).unwrap();
        let mut take_in = TakeInRecord::default();
        take_in.receiving.insert(
            repo.clone(),
            Receiving {
                branch: "main".to_owned(),
                tip: tip.clone(),
                moves: Moves::InPlace {
                    workspace: workspace.clone(),
                },
            },
        );
        take_in.moved_in_target.push(repo.clone());
        take_in.wrote_in_target.insert(repo.clone(), tip.clone());

        assert_eq!(
            take_in.in_place_moves(&workspace).collect::<Vec<_>>(),
            vec![(&repo, None)],
            "a checkout of the target's workspace is not a move in place"
        );

        take_in.moved_in_place.push(InPlaceMove {
            workspace: workspace.clone(),
            repo: repo.clone(),
            wrote: Some(tip.clone()),
        });
        assert_eq!(
            take_in.in_place_moves(&workspace).collect::<Vec<_>>(),
            vec![(&repo, Some(&tip))]
        );
        assert_eq!(take_in.in_place_moves(Path::new("/elsewhere")).count(), 0);
    }

    #[test]
    fn more_leases_that_fail_to_read_take_back_every_lease_they_wrote() {
        let tmp = crate::tempdir().unwrap();
        let [owner_dir, target_dir, third, fourth] =
            ["owner", "target", "third", "fourth"].map(|name| tmp.path().join(name));
        for dir in [&owner_dir, &target_dir, &third, &fourth] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let record = make_land_record(&OpId::new_now(), &owner_dir, &target_dir);
        let mut acquired = acquire_op(&land_touched(&owner_dir, &target_dir), &record).unwrap();
        let unreadable = LeaseRecord::path_in(&fourth);
        std::fs::create_dir_all(unreadable.dir().join(unreadable.file().file_name())).unwrap();

        assert!(acquire_more_leases(&mut acquired, &[third.clone(), fourth], &record).is_err());

        assert!(
            !LeaseRecord::path_in(&third)
                .dir()
                .join(LeaseRecord::path_in(&third).file().file_name())
                .exists(),
            "the lease written before the failing one is taken back"
        );
        assert!(LeaseRecord::path_in(&target_dir)
            .dir()
            .join(LeaseRecord::path_in(&target_dir).file().file_name())
            .exists());
    }

    #[test]
    fn more_leases_refused_on_a_workspace_another_op_holds_take_back_every_lease_they_wrote() {
        let tmp = crate::tempdir().unwrap();
        let [owner_dir, target_dir, third, fourth, other_owner] =
            ["owner", "target", "third", "fourth", "other"].map(|name| tmp.path().join(name));
        for dir in [&owner_dir, &target_dir, &third, &fourth, &other_owner] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let record = make_land_record(&OpId::new_now(), &owner_dir, &target_dir);
        let mut acquired = acquire_op(&land_touched(&owner_dir, &target_dir), &record).unwrap();
        let other = make_sync_record(&OpId::new_now(), &other_owner, &fourth);
        write_owner(&other_owner, &other).unwrap();
        acquire_lease(&fourth, &other_owner, &other.id).unwrap();

        assert!(acquire_more_leases(&mut acquired, &[third.clone(), fourth], &record).is_err());

        assert!(
            read_lease(&third).unwrap().is_none(),
            "the lease written before the refused one is taken back"
        );
        assert!(read_lease(&target_dir).unwrap().is_some());
    }

    #[test]
    fn acquire_op_writes_owner_only_for_plain_sync() {
        // Plain `sync` has no lease workspaces — only the owner record is
        // written. This is the minimal-acquisition case.
        let tmp = crate::tempdir().unwrap();
        let owner = tmp.path();
        let op_id = OpId::new_now();
        let record = make_sync_record(&op_id, owner, &PathBuf::from("/src"));

        let acquired = acquire_op(&sync_touched(owner), &record).unwrap();

        assert!(OwnerRecord::path_in(owner).exists());
        assert_eq!(acquired.owner_workspace(), owner);
        // Round-trip preserves the record.
        let read_back = read_owner(owner).unwrap().unwrap();
        assert_eq!(read_back.id, record.id);
    }

    #[test]
    fn acquire_op_writes_owner_plus_lease_for_land() {
        // `land` writes an owner at CWD and a lease at the target.
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let target_dir = tmp.path().join("target");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        let op_id = OpId::new_now();
        let record = make_land_record(&op_id, &owner_dir, &target_dir);

        let acquired = acquire_op(&land_touched(&owner_dir, &target_dir), &record).unwrap();

        // Owner record + lease both present.
        assert!(OwnerRecord::path_in(&owner_dir).exists());
        assert!(LeaseRecord::path_in(&target_dir).exists());
        // Lease points back at the owner.
        let lease = read_lease(&target_dir).unwrap().unwrap();
        assert_eq!(lease.id, op_id);
        assert_eq!(lease.owner, CanonicalPath::of(&owner_dir));
        assert_eq!(acquired.owner_workspace(), owner_dir);
    }

    /// The handle is `#[must_use]`, and once the preconditions have run the
    /// sync engine discards it rather than releasing it: from that point the
    /// records belong to the phase driver and `--continue` / `abort` are the
    /// exits. Clearing on drop would delete the running op's own claim.
    #[test]
    fn dropping_an_acquired_handle_leaves_the_records_on_disk() {
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let target_dir = tmp.path().join("target");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        let op_id = OpId::new_now();
        let record = make_land_record(&op_id, &owner_dir, &target_dir);

        drop(acquire_op(&land_touched(&owner_dir, &target_dir), &record).unwrap());

        assert!(
            OwnerRecord::path_in(&owner_dir).exists(),
            "the owner record must outlive the handle"
        );
        assert!(
            LeaseRecord::path_in(&target_dir).exists(),
            "the lease must outlive the handle"
        );
    }

    #[test]
    fn acquire_op_refuses_when_owner_workspace_already_has_op() {
        // If a prior op left an owner record, acquisition must refuse with the
        // standard in-flight-op refusal (verb / age / phase / both exits) —
        // NOT a raw "AlreadyExists" I/O error.
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path();
        // Plant a prior op.
        let prior_id = OpId::new_now();
        let prior = make_land_record(&prior_id, owner_dir, &PathBuf::from("/prior/target"));
        write_owner(owner_dir, &prior).unwrap();

        // Second acquisition attempt.
        let new_id = OpId::new_now();
        let new_record = make_sync_record(&new_id, owner_dir, &PathBuf::from("/new/src"));
        let err = acquire_op(&sync_touched(owner_dir), &new_record)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("in progress"),
            "refusal must name in-flight state; got: {err}"
        );
        assert!(
            err.contains("land in progress"),
            "refusal must name the prior op's verb: {err}"
        );
        assert!(
            err.contains("--continue") && err.contains("rwv abort"),
            "refusal must offer both exits: {err}"
        );
        // The prior op's record must survive (no clobbering).
        let after = read_owner(owner_dir).unwrap().unwrap();
        assert_eq!(after.id, prior_id);
    }

    #[test]
    fn acquire_op_rolls_back_owner_when_lease_workspace_taken() {
        // land acquires owner then lease. If the lease workspace already
        // holds an op-state file, the just-written owner MUST be rolled back
        // so a refusal leaves no trace (cleanup table).
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let target_dir = tmp.path().join("target");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        // Plant a lease at the target (as if a prior land already claimed it).
        let prior_lease = LeaseRecord {
            id: "prior-op".parse().unwrap(),
            owner: CanonicalPath::of(Path::new("/some/prior/owner")),
            created_at: None,
        };
        overwrite_lease_in_test(&target_dir, &prior_lease).unwrap();

        let op_id = OpId::new_now();
        let record = make_land_record(&op_id, &owner_dir, &target_dir);
        let err = acquire_op(&land_touched(&owner_dir, &target_dir), &record)
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("is leased to op prior-op"),
            "lease-side refusal must name the lease it met; got: {err}"
        );
        // The owner file we would have written must NOT persist — refusal
        // leaves no trace.
        assert!(
            !OwnerRecord::path_in(&owner_dir).exists(),
            "owner record must be rolled back on lease-side EEXIST; still present"
        );
        // The prior lease must survive untouched.
        let after = read_lease(&target_dir).unwrap().unwrap();
        assert_eq!(after.id.as_str(), "prior-op");
    }

    #[test]
    fn release_acquired_clears_owner_and_leases() {
        // After a successful acquisition, `release_acquired` must clear every
        // file it created — this is the cleanup-table row for a precondition
        // refusal AFTER acquisition.
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let target_dir = tmp.path().join("target");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&target_dir).unwrap();
        let op_id = OpId::new_now();
        let record = make_land_record(&op_id, &owner_dir, &target_dir);
        let acquired = acquire_op(&land_touched(&owner_dir, &target_dir), &record).unwrap();
        assert!(OwnerRecord::path_in(&owner_dir).exists());
        assert!(LeaseRecord::path_in(&target_dir).exists());

        release_acquired(&acquired);

        assert!(
            !OwnerRecord::path_in(&owner_dir).exists(),
            "owner cleared on release"
        );
        assert!(
            !LeaseRecord::path_in(&target_dir).exists(),
            "lease cleared on release"
        );
    }

    #[test]
    fn acquire_op_is_atomic_under_concurrent_racers() {
        // Two threads race to acquire the same owner workspace: exactly one
        // succeeds, the other gets an in-flight refusal.
        //
        // This is the core TOCTOU regression test — a check-then-write guard
        // would let both racers pass; atomic O_CREAT|O_EXCL forces a serial
        // winner. Repeated across trials to expose ordering flakiness.
        for _ in 0..20 {
            let tmp = crate::tempdir().unwrap();
            let owner_dir = tmp.path().to_path_buf();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

            let owner_dir_a = owner_dir.clone();
            let barrier_a = barrier.clone();
            let h1 = std::thread::spawn(move || {
                let op = OpId::new_now();
                let rec = make_sync_record(&op, &owner_dir_a, &PathBuf::from("/src"));
                barrier_a.wait();
                acquire_op(&sync_touched(&owner_dir_a), &rec)
            });

            let owner_dir_b = owner_dir.clone();
            let barrier_b = barrier.clone();
            let h2 = std::thread::spawn(move || {
                let op = OpId::new_now();
                let rec = make_sync_record(&op, &owner_dir_b, &PathBuf::from("/src2"));
                barrier_b.wait();
                acquire_op(&sync_touched(&owner_dir_b), &rec)
            });

            let r1 = h1.join().unwrap();
            let r2 = h2.join().unwrap();

            let (ok, err) = match (r1, r2) {
                (Ok(a), Err(e)) => (a, e),
                (Err(e), Ok(a)) => (a, e),
                (Ok(_), Ok(_)) => panic!("both racers acquired — TOCTOU regression"),
                (Err(e1), Err(e2)) => panic!("both racers failed: {e1} / {e2}"),
            };

            assert_eq!(ok.owner_workspace(), owner_dir);
            let err_s = err.to_string();
            assert!(
                err_s.contains("in progress"),
                "loser must see in-flight refusal; got: {err_s}"
            );
            assert!(
                err_s.contains("--continue") && err_s.contains("rwv abort"),
                "loser must see both exits: {err_s}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Dead-lease detection
    // -----------------------------------------------------------------------

    #[test]
    fn detect_dead_lease_returns_none_when_absent() {
        let tmp = crate::tempdir().unwrap();
        assert!(detect_dead_lease(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn detect_dead_lease_returns_none_when_owner_matches() {
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();
        let op_id = OpId::new_now();
        let record = make_land_record(&op_id, &owner_dir, &lease_dir);
        write_owner(&owner_dir, &record).unwrap();
        overwrite_lease_in_test(
            &lease_dir,
            &LeaseRecord {
                id: op_id.clone(),
                owner: CanonicalPath::of(&owner_dir),
                created_at: None,
            },
        )
        .unwrap();

        assert!(detect_dead_lease(&lease_dir).unwrap().is_none());
    }

    #[test]
    fn detect_dead_lease_flags_missing_owner_record() {
        // The concrete crash pattern: a lease whose owner workspace has no
        // `.rwv-op` — either the owner file was hand-removed or the workspace
        // was deleted out-of-band. Doctor auto-fixes this.
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();
        overwrite_lease_in_test(
            &lease_dir,
            &LeaseRecord {
                id: "dangling-op".parse().unwrap(),
                owner: CanonicalPath::of(&owner_dir),
                created_at: None,
            },
        )
        .unwrap();

        let dead = detect_dead_lease(&lease_dir).unwrap().unwrap();
        assert_eq!(dead.workspace_dir, lease_dir);
        assert_eq!(dead.op_id.as_str(), "dangling-op");
        assert_eq!(dead.recorded_owner, owner_dir);
        assert_eq!(dead.reason, DeadLeaseReason::OwnerRecordAbsent);
    }

    #[test]
    fn detect_dead_lease_flags_owner_op_id_mismatch() {
        // A stale lease survived past its op — the owner is now on a fresh op
        // with a different id. The lease is dead by structural comparison of
        // op ids, not by any time input.
        let tmp = crate::tempdir().unwrap();
        let owner_dir = tmp.path().join("owner");
        let lease_dir = tmp.path().join("lease");
        std::fs::create_dir_all(&owner_dir).unwrap();
        std::fs::create_dir_all(&lease_dir).unwrap();
        let fresh_op_id = OpId::new_now();
        let owner_record = make_land_record(&fresh_op_id, &owner_dir, &lease_dir);
        write_owner(&owner_dir, &owner_record).unwrap();
        // Lease references an OLDER op id — stale carry-over.
        overwrite_lease_in_test(
            &lease_dir,
            &LeaseRecord {
                id: "old-op-id".parse().unwrap(),
                owner: CanonicalPath::of(&owner_dir),
                created_at: None,
            },
        )
        .unwrap();

        let dead = detect_dead_lease(&lease_dir).unwrap().unwrap();
        assert_eq!(dead.op_id.as_str(), "old-op-id");
        assert!(matches!(
            dead.reason,
            DeadLeaseReason::OwnerOpIdMismatch { .. }
        ));
        if let DeadLeaseReason::OwnerOpIdMismatch { owner_op_id } = dead.reason {
            assert_eq!(owner_op_id, fresh_op_id);
        }
    }

    #[test]
    fn fix_dead_lease_removes_lease_file() {
        let tmp = crate::tempdir().unwrap();
        let lease_dir = tmp.path();
        overwrite_lease_in_test(
            lease_dir,
            &LeaseRecord {
                id: "dangling-op".parse().unwrap(),
                owner: CanonicalPath::of(Path::new("/gone")),
                created_at: None,
            },
        )
        .unwrap();
        assert!(LeaseRecord::path_in(lease_dir).exists());

        fix_dead_lease(lease_dir);

        assert!(
            !LeaseRecord::path_in(lease_dir).exists(),
            "fix_dead_lease removes the lease file"
        );
    }
}
