//! The state-to-choices table: every axis a verb judges a repo on, the states
//! of each, the choices each state prints, and the rendering of a refusal that
//! judged repos — its terminal text, its `--json` records and its `rwv
//! explain` page all read this one table.
//!
//! A refusal built here prints one line per repo and axis, then its choices,
//! then one route line per distinct axis. A refusal that names a condition
//! rather than a repo state carries a [`crate::refusal::RefusalKind`] instead.
//!
//! Adding a state is one arm in [`State::row`] and its entry in
//! [`State::ALL`]; shipping a verb is flipping `shipped` on the forms that
//! spell it. A form that is not shipped is never printed, by any state, on any
//! surface.
//!
//! A kind that `rwv doctor` also reports is an axis whose [`Axis::finding`]
//! names that finding. Its one entry is written into the findings page, not
//! the refusals page, and carries the finding's sub-kinds with the severity
//! and `--fix` disposition the doctor register gives them.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::vcs::{Vcs, VcsAction};

/// The verb whose refusals name `rwv activate <project> --update` where other
/// refusals name `rwv update`: a line judged for it is one that switching to
/// the project would have to clear.
pub const ACTIVATE: &str = "activate";

pub const UPDATE_REMOTE: &str = "update --remote";

/// What a refusal judged a repo on. Its token is the kind `rwv explain`
/// serves; the order here is the order a repo's lines print in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Axis {
    /// Uncommitted work, or a VCS operation in progress, in a checkout.
    Edits,
    /// Another checkout derives from what the verb would remove.
    Checkout,
    /// A branch and the branch it takes in from or publishes to.
    PushBranch,
    /// A name the verb was given is unusable.
    Name,
    /// What `rwv push` withheld.
    Publish,
    /// A project whose `rwv.toml` does not load.
    UnparseableProject,
    /// The commit a lock pins for a member, and whether a ref reaches it.
    Pin,
    /// A weave branch's record: how it is used and the branch it lands into.
    Target,
    /// How a branch accepts landings and takes its target's work in.
    History,
    /// Projects sharing a member that record different `history` for it.
    HistoryDisagreement,
    /// Where a workspace's checkouts sit against where the checked-out
    /// branch's record says they should.
    Alignment,
    /// A branch rwv never rewrites, renames or deletes.
    Protected,
    /// A project repo whose lock merge driver is not set up as this release
    /// sets it up.
    MissingLockMergeDriver,
    /// A project commit `rwv cherry-pick` cannot port as it stands.
    Port,
}

/// An axis's page prose: what it judges, why, and where the rule sits.
pub struct KindRow {
    pub condition: &'static str,
    pub why: &'static str,
    pub sits: &'static str,
}

impl Axis {
    pub const ALL: [Axis; 14] = [
        Self::Edits,
        Self::Checkout,
        Self::PushBranch,
        Self::Name,
        Self::Publish,
        Self::UnparseableProject,
        Self::Pin,
        Self::Target,
        Self::History,
        Self::HistoryDisagreement,
        Self::Alignment,
        Self::Protected,
        Self::MissingLockMergeDriver,
        Self::Port,
    ];

    pub fn token(self) -> String {
        crate::wire_token::wire_token(&self)
    }

    pub fn row(self) -> &'static KindRow {
        match self {
            Self::Edits => &KindRow {
                condition: "A checkout the verb must move, rewrite, record or remove holds \
                            uncommitted changes, or is stopped in the middle of a VCS operation.",
                why: "Moving or removing a checkout over uncommitted changes would lose them or \
                      tangle them into a half-applied result, and recording its tip would \
                      describe something other than what is on disk. An operation in progress \
                      holds a partial result that only the tool which started it can finish. \
                      A project that shares a checkout with the project the weave root \
                      presents runs its install hooks against it on activation, and would build \
                      against edits made under the other project.",
                sits: "[Destructive operations](../explanation/destructive-operations.md); \
                       [Sync semantics](../explanation/joints/sync-semantics.md).",
            },
            Self::Checkout => &KindRow {
                condition: "Another checkout derives from the clone a verb would delete: a \
                            workweave's, or a worktree added with the VCS directly.",
                why: "Deleting a clone's store takes every ref and object with it at once, for \
                      every checkout derived from it, so a live one would be left pointing at \
                      nothing.",
                sits: "[Clone topology](../explanation/joints/clone-topology.md).",
            },
            Self::PushBranch => &KindRow {
                condition: "A branch and the remote branch it takes in from or publishes to \
                            have each moved, or the remote has none to give.",
                why: "Taking in a branch that has commits of its own would have to discard \
                      them or tangle the two histories, and only a fast-forward does neither, \
                      so a verb that takes in stops where the histories have diverged instead \
                      of choosing for you.",
                sits: "[Sync semantics](../explanation/joints/sync-semantics.md).",
            },
            Self::Name => &KindRow {
                condition: "A name the verb was given cannot be used: another workweave \
                            already holds it, a repo already has a branch of that name holding \
                            commits, it reads as a revision where a branch belongs, it names \
                            nothing in a repo the verb read, or it is the name rwv keeps its \
                            branch records under.",
                why: "A workweave name maps to one workweave; recording a second would make \
                      every later address ambiguous, and a delete would remove whichever was \
                      found first. A branch that already holds commits under the name a create \
                      would give its checkout is work this create did not make, and starting \
                      the new branch there or somewhere else would hide or lose it. A member entry's `branch` declares the branch to track while \
                      the lock records where you are, so a commit id or release tag there leaves \
                      nothing to track. A name that resolves to nothing has no tip to read, and \
                      reading another branch in its place would answer a question nobody asked. \
                      The branch that holds the branch records is rwv's own: a weave branch of \
                      that name, or below it, would be read as records and written over by \
                      them.",
                sits: "[Alignment](../explanation/joints/alignment.md); \
                       [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md); \
                       [Lock as derived](../explanation/joints/lock-as-derived.md).",
            },
            Self::Publish => &KindRow {
                condition: "`rwv push` published member repos but withheld the project repo.",
                why: "The project repo carries the lock that pins each member's commit. \
                      Publishing it while a member's push failed would advertise a commit \
                      collaborators cannot fetch, so the lock is withheld until every member it \
                      pins is published.",
                sits: "",
            },
            Self::UnparseableProject => &KindRow {
                condition: "A project directory exists but does not load: its `rwv.toml` or \
                            its `rwv.lock` failed to parse.",
                why: "A project rwv cannot see into must not read as a clean project with zero \
                      findings, so `rwv doctor` reports it at error severity. Its `message` \
                      names which of the two files failed and carries that file's remedy, and \
                      `manifest_path` always names the manifest, because it locates the project \
                      rather than the failure. A verb that reads the manifest at the checked-out \
                      tip refuses when it names member-entry keys an earlier release wrote: no \
                      verb reads them, and the repair is mechanical.",
                sits: "[Formats](./formats.md) states the member-entry keys; \
                       [Upgrade rwv](../how-to/upgrade-rwv.md) walks through the rewrite.",
            },
            Self::Target => &KindRow {
                condition: "A weave branch's record cannot be used: the branch has none, two \
                            writers recorded it differently, the entry does not read, its \
                            target chain returns to it, or the branch it lands into is gone; \
                            or other branches' records land into it and a delete would strand \
                            them.",
                why: "A weave branch's record is the one statement of how it is used: as \
                      feature work onto a target, or as declared. Every verb that aligns \
                      members, locks, lands or publishes reads it, and rwv picks none for a \
                      branch that has none, because a guess that is wrong fails later, about \
                      something else. A record two writers set differently keeps both values \
                      until one is chosen, an entry rwv cannot read is refused for that branch \
                      alone, and a target chain that returns to its branch could never land.",
                sits: "[Records and modes](../explanation/joints/branch-records.md); \
                       [Landing](../explanation/joints/landing.md); \
                       [Workweave hierarchy](../explanation/joints/workweave-hierarchy.md); \
                       [Formats](./formats.md).",
            },
            Self::History => &KindRow {
                condition: "Integrating work needs more than a fast-forward, and the branch \
                            it moves into records no `history` for that repo; or a run names a \
                            `history` the branch records otherwise; or the branch records \
                            `ff-only` and only a fast-forward would do.",
                why: "A branch's `rwv.toml` records how it accepts landings: `linear`, \
                      `merge` or `ff-only`, at the top level and per member. No value is the \
                      default and no role implies one, because the choice between rewriting \
                      a feature onto the target and recording a merge is the target's \
                      owners' to make, once, and every later landing follows it. A fast-forward \
                      reads no setting.",
                sits: "[Formats](./formats.md) states the `history` keys; \
                       [History and lineage](../explanation/joints/history-and-lineage.md).",
            },
            Self::HistoryDisagreement => &KindRow {
                condition: "Two projects list the same member, and their live weave branches \
                            record different `history` for it.",
                why: "A member's clone is shared, so one member branch can be landed into by \
                      several projects. Each project's landings follow its own setting, so \
                      the branch takes rebased work from one project and merge commits from \
                      another, and neither project's setting describes its history. Nothing \
                      refuses: each project's setting is its own to keep, and `rwv doctor` \
                      reports the difference.",
                sits: "[Formats](./formats.md) states the `history` keys.",
            },
            Self::Alignment => &KindRow {
                condition: "A member's checkout, or the project repo's, is not where the \
                            checked-out project branch says it should be: on a branch other \
                            than the expected one, detached where it should be attached, or \
                            attached where it should be detached; or the workspace's record \
                            of what it last aligned for no longer matches the branch.",
                why: "The checked-out project branch's record, its member entries and its \
                      lock say which branch each member sits on, and where a member's branch \
                      is checked out in another workspace, or does not reach the commit the \
                      lock pins, that it sits detached at the pin. A checkout anywhere else \
                      builds something other than what the branch describes, and a lock \
                      written from it would record that. `rwv update` moves the members it \
                      can without leaving a commit on no ref, and reports the rest with the \
                      commits that would be left. A member two projects list has one checkout: \
                      activating a project leaves it where it is, so `rwv activate` refuses one \
                      that is not aligned for the incoming project and names `--update`, which \
                      aligns it first.",
                sits: "[Alignment](../explanation/joints/alignment.md); \
                       [Clone topology](../explanation/joints/clone-topology.md); \
                       [Workweave lifecycle](../explanation/joints/workweave-lifecycle.md); \
                       [Shared refs and drift](../explanation/joints/shared-refs-drift.md).",
            },
            Self::Protected => &KindRow {
                condition: "A verb would rewrite, rename or delete a branch that other \
                            branches depend on staying where it is: a branch some live weave \
                            branch declares for a member, or the project branch whose record \
                            is `declared`.",
                why: "A declared branch is what every weave branch that declares it follows, \
                      and what a fresh clone checks out; rewriting, renaming or deleting it \
                      moves them all at once, and a rewrite can strand commits others have \
                      built on. A declared-mode project branch is protected against rewrites \
                      and the deletion of an unlanded push branch alone: a rename moves only \
                      its record, stored entries and the records that land into it, and a \
                      landed push branch deleted loses nothing.",
                sits: "[Workweave hierarchy](../explanation/joints/workweave-hierarchy.md); \
                       [Destructive operations](../explanation/destructive-operations.md).",
            },
            Self::Pin => &KindRow {
                condition: "A commit a lock pins is not there to be at: the lock has no entry \
                            for a member, or pins a commit that no ref in the clone reaches; or \
                            a verb would delete, move or overwrite the last ref that reaches a \
                            commit some project's live lock pins.",
                why: "A lock pins each member at a commit so that a fresh clone can build the \
                      project. With no entry there is nothing to reproduce, and a commit no ref \
                      reaches is one the remotes this clone has fetched no longer hold; the \
                      member is checked out on its declared branch at the tip of the remote's \
                      branch instead, so the weave is usable, and the lock is left as it was \
                      for you to settle. Removing the last ref that reaches a pinned commit \
                      leaves the pin unreachable: no clone can fetch it, and the lock describes \
                      a state nobody can recreate. A pin is live in every project of the weave, \
                      not only the one the verb runs in, because a member's clone is shared.",
                sits: "[Lock as derived](../explanation/joints/lock-as-derived.md); \
                       [Reconcile repos](../how-to/reconcile-repos.md); \
                       [Destructive operations](../explanation/destructive-operations.md).",
            },
            Self::MissingLockMergeDriver => &KindRow {
                condition: "A project repo's lock merge driver is not set up as this release \
                            sets it up: the checked-out branch's committed attribute line for \
                            `rwv.lock` is absent or names a driver an earlier release wrote, or \
                            the clone's config does not define the driver's command or still \
                            defines the earlier one.",
                why: "A merge rwv does not make merges `rwv.lock` through the lock merge \
                      driver: pins merge three-way, and a merge the driver cannot settle is \
                      left conflicted with both pins marked. Without the attribute line git \
                      merges the lock as text; without the config it falls back to text, or \
                      fails into a conflict. The driver an earlier release set up kept one \
                      side of every lock merge silently, so its line and config are migrated, \
                      never honored. Every verb that writes the project repo or its lock \
                      refuses until `rwv doctor --fix` migrates it, and `rwv doctor` reports \
                      it instead.",
                sits: "[Lock as derived](../explanation/joints/lock-as-derived.md); \
                       [Upgrade rwv](../how-to/upgrade-rwv.md).",
            },
            Self::Port => &KindRow {
                condition: "`rwv cherry-pick` cannot port the commit it was given as it stands: \
                            the commit is a merge and no parent was named to pick it against, \
                            or a member's change in it cannot be copied or carried, because it \
                            moves the member's pin backward, the commits it would copy hold a \
                            merge, or this branch does not list the member.",
                why: "A port applies what one project commit introduced, in every repo it \
                      moves, as new commits on this branch, and it ports the commit whole or \
                      not at all. Picking a merge against a parent nobody named would be a \
                      guess; copying a range that holds a merge commit by commit would flatten \
                      it; a pin moved backward is work undone, which only a revert ports; and a \
                      member this branch does not list has nowhere to go. Each line lists what \
                      the member's change holds and its port by hand, then the rerun that \
                      leaves the member out of the pick and records it as it stands.",
                sits: "[Lock as derived](../explanation/joints/lock-as-derived.md).",
            },
        }
    }

    /// The `rwv doctor` finding of the same name, when there is one. Its
    /// entry is then the findings page's, written there from this table.
    pub fn finding(self) -> Option<Finding> {
        match self {
            Self::Edits
            | Self::Checkout
            | Self::PushBranch
            | Self::Name
            | Self::Publish
            | Self::Protected
            | Self::Target
            | Self::History
            | Self::Port => None,
            Self::HistoryDisagreement => Some(Finding::whole(
                crate::check::ViolationKind::HistoryDisagreement,
                FindingMarks {
                    severity: crate::integration::Severity::Warning,
                    fix: crate::check::FixDisposition::ReportOnly,
                    entry: HISTORY_DISAGREEMENT_ENTRY,
                },
            )),
            Self::MissingLockMergeDriver => Some(Finding::of(
                crate::check::ViolationKind::MissingLockMergeDriver,
                crate::check::MissingLockMergeDriverKind::ALL.map(|sub_kind| SubKindRow {
                    token: crate::wire_token::wire_token(&sub_kind),
                    severity: sub_kind.severity(),
                    fix: sub_kind.fix_disposition(),
                    entry: sub_kind.entry(),
                }),
            )),
            Self::Pin => Some(Finding::of(
                crate::check::ViolationKind::Pin,
                crate::check::PinKind::ALL.map(|sub_kind| SubKindRow {
                    token: crate::wire_token::wire_token(&sub_kind),
                    severity: sub_kind.severity(),
                    fix: sub_kind.fix_disposition(),
                    entry: sub_kind.entry(),
                }),
            )),
            Self::Alignment => Some(Finding::of(
                crate::check::ViolationKind::Alignment,
                crate::check::AlignmentKind::ALL.map(|sub_kind| SubKindRow {
                    token: crate::wire_token::wire_token(&sub_kind),
                    severity: sub_kind.severity(),
                    fix: sub_kind.fix_disposition(),
                    entry: sub_kind.entry(),
                }),
            )),
            Self::UnparseableProject => Some(Finding::of(
                crate::check::ViolationKind::UnparseableProject,
                crate::check::UnparseableProjectKind::ALL.map(|sub_kind| SubKindRow {
                    token: crate::wire_token::wire_token(&sub_kind),
                    severity: sub_kind.severity(),
                    fix: sub_kind.fix_disposition(),
                    entry: match sub_kind {
                        crate::check::UnparseableProjectKind::PreSchema => PRE_SCHEMA_ENTRY,
                        crate::check::UnparseableProjectKind::Malformed => MALFORMED_ENTRY,
                    },
                }),
            )),
        }
    }
}

const LOCK_DRIVER_REFUSED_BY: &str = "every verb that writes the project repo or its lock, \
before it writes anything: `rwv lock`, `rwv sync`, `rwv land` (for the project checkout it \
composes onto too), `rwv pull`, `rwv update --remote`, the `rwv member` \
verbs, `rwv branch create`, `delete`, `rename` and `set-mode`, and `rwv workweave create` and \
`delete`; `rwv doctor` reports it instead";

const PRE_SCHEMA_ENTRY: &str = "The project's `rwv.toml` names member-entry keys an earlier \
release wrote: `version` where `branch` is now expected, or `submodules` where \
`recurse-submodules` is. Every verb that reads the manifest at the checked-out tip refuses with \
this kind, and `rwv explain unparseable-project` is its route line.

**What to do:** `rwv doctor --fix`, run in the project's checkout. It renames the keys in place \
— `version` to `branch`, `submodules = \"skip\"` to `recurse-submodules = false`, and drops \
`submodules = \"local\"`, which is what an absent `recurse-submodules` means — keeping the \
file's comments and key order. It writes the rename into the working file and into the project's \
HEAD commit and commits only that, in one commit: any other edit to `rwv.toml`, and any edit to \
another file, stays uncommitted where it was. It declines, changing nothing, while another file \
is marked for the next commit or a rebase, merge or cherry-pick is in progress in the project repo, and says so; run \
it again once that is dealt with. A value of `submodules` other than `\"local\"` or \
`\"skip\"`, or an entry that names both a key and its replacement, is not guessed at: the repair \
names it and leaves the file.

A commit other than the checked-out one that names the old keys is not reported and is not \
refused. It declares nothing to the verbs that read it, and its `rwv.lock` is read as any lock \
is.";

const MALFORMED_ENTRY: &str = "Any other reason the project did not load. The message names which \
file failed and carries that file's remedy — the manifest is yours to edit, at the line the \
parser names; the lock is restored from version control, or written afresh by `rwv lock`, which \
then pins nothing until `rwv member pin` or `rwv update --remote` gives each member its pin. A \
repository entry with no \
`role`, an unknown top-level key, an unknown `[integrations.<name>]` table, an unknown key in \
one, and an `enabled` that is not `true` or `false` are all parse failures of this kind, each \
naming the line and what was expected. `rwv sync` and `rwv land` refuse with it when the \
project's `rwv.lock` does not load after the project repo is replayed, and `rwv abort` rolls \
that operation back; an `rwv.toml` the replay or merge produced that does not parse stops as \
`mid-operation` instead.

`--fix` has no arm for it. Rewriting a manifest rwv could not parse would be guesswork, and \
choosing the commits a lock pins is a decision about state, not a repair.";

const HISTORY_DISAGREEMENT_ENTRY: &str = "`histories` names every live weave branch, of \
every project listing the member, that records a `history` for it, with the value it records: \
the member's own, else the branch's top-level one. Branches of one project that differ among \
themselves are not reported. Doctor reports it from any workspace, for each member a project in \
scope lists.

**What to do:** decide which shape the member's branches take, and give every project that \
setting with `rwv branch set-history <h> --repo <m>` in a workspace on each branch that records \
another, then push it. Leaving it is allowed: each project's landings follow its own setting.";

/// What `rwv doctor` reports under a kind's name: the sub-kinds the finding
/// carries, each with the severity and `--fix` disposition the doctor
/// register gives it, or for a finding with no sub-kinds, those of the
/// finding itself.
pub struct Finding {
    pub kind: crate::check::ViolationKind,
    pub sub_kinds: Vec<SubKindRow>,
    pub whole: Option<FindingMarks>,
}

pub struct FindingMarks {
    pub severity: crate::integration::Severity,
    pub fix: crate::check::FixDisposition,
    pub entry: &'static str,
}

pub struct SubKindRow {
    pub token: String,
    pub severity: crate::integration::Severity,
    pub fix: crate::check::FixDisposition,
    pub entry: &'static str,
}

impl Finding {
    fn of(
        kind: crate::check::ViolationKind,
        sub_kinds: impl IntoIterator<Item = SubKindRow>,
    ) -> Self {
        Self {
            kind,
            sub_kinds: sub_kinds.into_iter().collect(),
            whole: None,
        }
    }

    fn whole(kind: crate::check::ViolationKind, marks: FindingMarks) -> Self {
        Self {
            kind,
            sub_kinds: Vec::new(),
            whole: Some(marks),
        }
    }
}

/// One state a repo can be judged in, on its axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    EditsDirty,
    EditsDriftLeftInPlace,
    EditsMovedAndEdited,
    EditsUnresolved,
    EditsSetBack,
    EditsSubmoduleBehind,
    EditsMidOperation,
    EditsLockFilesUncommitted,
    EditsLockUnresolved,
    EditsCommitMidOperation,
    EditsReplayDiffers,
    CheckoutInAnotherWorkspace,
    CheckoutInRawWorktree,
    PushBranchDiverged,
    PushBranchBehind,
    PushBranchAhead,
    PushBranchGone,
    PushBranchForeignUpstream,
    PushBranchNoPushTarget,
    PushBranchUnconfirmed,
    PinNone,
    PinUnreachable,
    NameTaken,
    NameExists,
    NameARevision,
    NameNoSuchBranch,
    NameReserved,
    NameMinted,
    NameDeclared,
    NameFeature,
    PublishWithheld,
    PublishRecordsRejected,
    PublishNoPin,
    UnparseableProjectHere,
    UnparseableProjectMalformed,
    UnparseableProjectAt,
    UnparseableProjectMidOperation,
    UnparseableProjectCommit,
    MissingLockMergeDriverAttributeAbsent,
    MissingLockMergeDriverAttributeLegacy,
    MissingLockMergeDriverConfigAbsent,
    MissingLockMergeDriverConfigLegacy,
    PinWouldBeUnreachable,
    PinHeldByNoBranch,
    TargetNoTarget,
    TargetUnrecorded,
    TargetConflict,
    TargetDiffers,
    TargetUnreadable,
    TargetCycle,
    TargetGone,
    TargetDependents,
    TargetDependentLacksTip,
    TargetDependentCheckedOut,
    TargetDependentOnAnotherMachine,
    TargetDependentMovedMidOperation,
    TargetRecordChanged,
    TargetModeChanged,
    TargetMemberChanges,
    TargetMovedMidOperation,
    PublishTargetUnpublished,
    AlignmentAligned,
    AlignmentOnBranch,
    AlignmentOnBranchWithStrayCommits,
    AlignmentDetachedElsewhere,
    AlignmentDetachedUnpushed,
    AlignmentUnaligned,
    AlignmentProjectMoved,
    AlignmentProjectUnaligned,
    AlignmentProjectDetached,
    PinNotOnDeclaredBranch,
    PinBehind,
    PinAhead,
    PinUnpinned,
    PinDiverged,
    PinOffLineage,
    PinOffRemote,
    PinBackwards,
    PinUnmerged,
    PinWaived,
    ProtectedDeclared,
    ProtectedDeclaredMode,
    HistoryUnset,
    HistoryContradicted,
    HistoryNotFastForward,
    HistoryDisagreement,
    PortMainline,
    PortMergeInRange,
    PortBackward,
    PortNotListed,
}

/// A state's row: its axis and name, the page prose, and the forms it offers,
/// in the order they print: every refusal's, then the refusing verb's own.
pub struct StateRow {
    pub axis: Axis,
    pub name: &'static str,
    pub means: &'static str,
    pub refused_by: &'static str,
    pub offers: &'static [Form],
    /// Forms only the named verb's refusal offers, because the flag that
    /// spells them belongs to that verb. The name is the verb's subcommand
    /// path, as `rwv explain` and a refusal's `rwv <verb>:` prefix print it.
    pub verb_offers: &'static [(&'static str, Form)],
}

impl State {
    pub const ALL: [State; 90] = [
        Self::EditsDirty,
        Self::EditsDriftLeftInPlace,
        Self::EditsMovedAndEdited,
        Self::EditsUnresolved,
        Self::EditsSetBack,
        Self::EditsSubmoduleBehind,
        Self::EditsMidOperation,
        Self::EditsLockFilesUncommitted,
        Self::EditsLockUnresolved,
        Self::EditsCommitMidOperation,
        Self::EditsReplayDiffers,
        Self::CheckoutInAnotherWorkspace,
        Self::CheckoutInRawWorktree,
        Self::PushBranchDiverged,
        Self::PushBranchBehind,
        Self::PushBranchAhead,
        Self::PushBranchGone,
        Self::PushBranchForeignUpstream,
        Self::PushBranchNoPushTarget,
        Self::PushBranchUnconfirmed,
        Self::PinBehind,
        Self::PinAhead,
        Self::PinUnpinned,
        Self::PinNone,
        Self::PinDiverged,
        Self::PinOffLineage,
        Self::PinNotOnDeclaredBranch,
        Self::PinUnreachable,
        Self::PinOffRemote,
        Self::PinBackwards,
        Self::PinUnmerged,
        Self::PinWaived,
        Self::PinWouldBeUnreachable,
        Self::PinHeldByNoBranch,
        Self::ProtectedDeclared,
        Self::ProtectedDeclaredMode,
        Self::NameTaken,
        Self::NameExists,
        Self::NameARevision,
        Self::NameNoSuchBranch,
        Self::NameReserved,
        Self::NameMinted,
        Self::NameDeclared,
        Self::NameFeature,
        Self::PublishWithheld,
        Self::PublishRecordsRejected,
        Self::PublishNoPin,
        Self::UnparseableProjectHere,
        Self::UnparseableProjectMalformed,
        Self::UnparseableProjectAt,
        Self::UnparseableProjectMidOperation,
        Self::UnparseableProjectCommit,
        Self::MissingLockMergeDriverAttributeAbsent,
        Self::MissingLockMergeDriverAttributeLegacy,
        Self::MissingLockMergeDriverConfigAbsent,
        Self::MissingLockMergeDriverConfigLegacy,
        Self::TargetNoTarget,
        Self::TargetUnrecorded,
        Self::TargetConflict,
        Self::TargetDiffers,
        Self::TargetUnreadable,
        Self::TargetCycle,
        Self::TargetGone,
        Self::TargetDependents,
        Self::TargetDependentLacksTip,
        Self::TargetDependentCheckedOut,
        Self::TargetDependentOnAnotherMachine,
        Self::TargetDependentMovedMidOperation,
        Self::HistoryUnset,
        Self::HistoryContradicted,
        Self::HistoryNotFastForward,
        Self::HistoryDisagreement,
        Self::TargetRecordChanged,
        Self::TargetModeChanged,
        Self::TargetMemberChanges,
        Self::TargetMovedMidOperation,
        Self::PublishTargetUnpublished,
        Self::AlignmentAligned,
        Self::AlignmentOnBranch,
        Self::AlignmentOnBranchWithStrayCommits,
        Self::AlignmentDetachedElsewhere,
        Self::AlignmentDetachedUnpushed,
        Self::AlignmentUnaligned,
        Self::AlignmentProjectMoved,
        Self::AlignmentProjectUnaligned,
        Self::AlignmentProjectDetached,
        Self::PortMainline,
        Self::PortMergeInRange,
        Self::PortBackward,
        Self::PortNotListed,
    ];

    pub fn row(self) -> &'static StateRow {
        match self {
            Self::EditsDirty => &StateRow {
                axis: Axis::Edits,
                name: "dirty",
                means: "the checkout has uncommitted changes, listed by file",
                refused_by: "`rwv sync` and `rwv land`, for a repo they would rebase or \
                             fast-forward (tracked changes only); `rwv lock` without `--dirty`, \
                             for a member (tracked changes only); `rwv pull`, for project-repo \
                             files its fast-forward would overwrite; `rwv member \
                             set-role <repo> reference`; `rwv member remove --delete` and \
                             replay's prune of a dropped member; `rwv workweave delete` and \
                             `rwv land --retire`, for every checkout in the workweave; \
                             `rwv activate`, for a member the project shares with the project \
                             the weave root presents; `rwv update` and `rwv pull`, for such a \
                             member that is not aligned for a project the weave root does not \
                             present",
                offers: &[Form::C13],
                verb_offers: &[
                    ("workweave delete", Form::C13WorkweaveDelete),
                    ("lock", Form::LockDirty),
                    ("update --remote", Form::UpdateRemoteDirty),
                ],
            },
            Self::EditsDriftLeftInPlace => &StateRow {
                axis: Axis::Edits,
                name: "drift left in place",
                means: "the checkout's branch moved under it from another checkout, and the \
                        refresh that brings it to its HEAD wrote none of these paths, listed, \
                        for the reason named: a path changed after it was read or something \
                        on disk was in the way, the branch moved again, the checkout holds \
                        unresolved conflicts, or the write failed. They still hold the old \
                        tip's content, so committing them would record the move's reverse. \
                        The checkout's other edits are judged once the drift is refreshed; a \
                        landing lists them beside it, with no choice of their own until then. \
                        Where its drift cannot be read at all (no record of where the checkout \
                        stood, its branch moved to a commit that does not contain where it \
                        stood, or part of the move taken in a way its history does not show), \
                        nothing it holds is judged an edit and the one choice sets aside every \
                        difference. Where the checkout is stopped in a merge or copy, nothing \
                        is refreshed, since concluding it would record the move's reverse, and \
                        the one choice abandons the operation",
                refused_by: "`rwv land`, for a checkout whose edits it judges, in either \
                             workspace, and every other verb that judges a checkout's edits",
                offers: &[
                    Form::DriftLandAgain,
                    Form::DriftResolveThenLand,
                    Form::DriftRefreshAgain,
                    Form::StashUnreadDrift,
                    Form::UnmarkThenStashUnreadDrift,
                    Form::AbortOperation,
                ],
                verb_offers: &[],
            },
            Self::EditsMovedAndEdited => &StateRow {
                axis: Axis::Edits,
                name: "moved and edited",
                means: "the checkout's branch moved under it from another checkout, and each \
                        listed file was changed both by that move and by an uncommitted edit \
                        here, or stands where the move puts a file or directory of its own. \
                        Nothing in the checkout is refreshed or recorded while such a file \
                        stands, so the move's other files still hold the old tip's content and a \
                        commit there would record the move's reverse; the checkout's other edits \
                        get no choice of their own. The merge and the set-aside each hand the \
                        edits to the VCS against the tip the files hold, so what is merged or \
                        set aside is the edits alone",
                refused_by: "every verb that judges a checkout's edits, `rwv doctor` with or \
                             without `--fix` included, for a checkout in either workspace; \
                             conflicted files are judged `unresolved` first",
                offers: &[
                    Form::MergeEditedDrift,
                    Form::SetEditsAside,
                    Form::ReplaceThenMerge,
                    Form::CompareMarked,
                    Form::CompareSkipped,
                    Form::ClearRefusalThenMerge,
                ],
                verb_offers: &[],
            },
            Self::EditsUnresolved => &StateRow {
                axis: Axis::Edits,
                name: "unresolved",
                means: "the checkout holds conflicted files, listed, with no operation in \
                        progress, as a merge of edits onto a moved tip leaves the files it \
                        could not resolve: each holds both versions between conflict markers, \
                        and no commit is possible until each is resolved and marked resolved",
                refused_by: "every verb that refuses a `dirty` checkout, in its place",
                offers: &[Form::ResolveConflicts],
                verb_offers: &[],
            },
            Self::EditsSetBack => &StateRow {
                axis: Axis::Edits,
                name: "set back",
                means: "the merge or set-aside of edits onto a branch's moved tip pointed the \
                        checkout at the tip its files hold, named in the subject with the branch \
                        it left, and did not finish: the VCS refused the next step, its reason \
                        quoted, or the run was stopped. HEAD names what the files hold, so the \
                        listed edits are the only difference from it and a commit or a \
                        set-aside made here records them and nothing of the move. Running the merge or \
                        the set-aside again carries them onto wherever the branch is then",
                refused_by: "every verb that judges a checkout's edits, `rwv doctor --fix` \
                             included",
                offers: &[
                    Form::MergeEditedDrift,
                    Form::SetEditsAside,
                    Form::UnstageThenMerge,
                    Form::ReplaceThenMerge,
                    Form::RemoveLockThenMerge,
                    Form::FreeBranchThenMerge,
                    Form::RecreateBranchThenMerge,
                    Form::ClearRefusalThenMerge,
                    Form::CompareMarked,
                    Form::CompareSkipped,
                    Form::TakeBackSetAside,
                ],
                verb_offers: &[],
            },
            Self::EditsSubmoduleBehind => &StateRow {
                axis: Axis::Edits,
                name: "submodule behind",
                means: "each listed submodule's checkout stands at a commit other than the one \
                        its member records, and holds no edits of its own: the member's \
                        history moved the submodule and its checkout was not moved with it, as \
                        for a member that does not recurse into its submodules. Committing it \
                        would record the commit the checkout stands at, undoing the move",
                refused_by: "every verb that judges a checkout's edits",
                offers: &[Form::FollowSubmodule],
                verb_offers: &[("workweave delete", Form::C13WorkweaveDelete)],
            },
            Self::EditsMidOperation => &StateRow {
                axis: Axis::Edits,
                name: "mid-operation",
                means: "a rebase, merge, cherry-pick, revert or bisect is in progress, \
                        named. It is the checkout's only line: where its HEAD stands and \
                        what it holds are the operation's until it ends",
                refused_by: "`rwv sync` and `rwv land`, for a repo they would rebase or \
                             fast-forward; `rwv workweave delete` and `rwv land --retire`, for \
                             every checkout in the workweave, whatever their discard flags; \
                             `rwv switch`, `rwv member set-branch`, `rwv member pin`, `rwv lock` \
                             and `rwv push`, for a member not where they would put it or read \
                             it; `rwv update` reports it for a member it would move, and moves \
                             the others; `rwv activate`, `rwv update` and `rwv pull`, for a \
                             shared member as `dirty` is",
                offers: &[Form::C32],
                verb_offers: &[],
            },
            Self::EditsLockUnresolved => &StateRow {
                axis: Axis::Edits,
                name: "lock unresolved",
                means: "the project repo's `rwv.lock` is conflicted: a merge, rebase or copy \
                        the lock merge driver could not settle left it conflicted with both \
                        pins marked, or a driver that could not run left one side in the \
                        working file with the conflict recorded only by the VCS. Resolve it \
                        to the pins you mean and mark it resolved",
                refused_by: "every verb resolved for a project, until it is resolved; `rwv \
                             doctor` and `rwv abort` run",
                offers: &[
                    Form::MarkResolved,
                    Form::ResumeOperation,
                    Form::AbortOperation,
                    Form::C32,
                ],
                verb_offers: &[],
            },
            Self::EditsCommitMidOperation => &StateRow {
                axis: Axis::Edits,
                name: "commit mid-operation",
                means: "the project repo is in the middle of a merge, rebase, cherry-pick or \
                        revert, named, with each path still conflicted listed. A commit made \
                        there now would become that operation's commit, concluding it with \
                        only what the commit carries. The operation ends by its own continue, \
                        which commits what you marked resolved, or by its abort",
                refused_by: "every verb that commits the project repo: the `rwv member` and \
                             `rwv branch` setters, `rwv lock --commit`, `rwv doctor --fix`, and \
                             `rwv sync`, `rwv land`, `rwv pull`, `rwv cherry-pick` and \
                             `rwv update --remote` as they start; each operation's own \
                             `--continue` and `rwv abort` run",
                offers: &[
                    Form::MarkResolved,
                    Form::KeepYours,
                    Form::KeepIncoming,
                    Form::ResumeOperation,
                    Form::AbortOperation,
                    Form::C32,
                ],
                verb_offers: &[],
            },
            Self::EditsLockFilesUncommitted => &StateRow {
                axis: Axis::Edits,
                name: "rwv.toml or rwv.lock uncommitted",
                means: "the project repo's only uncommitted changes are to `rwv.toml` or \
                        `rwv.lock`, or either is not yet tracked",
                refused_by: "`rwv push`; `rwv switch`, and `rwv branch create` for an `rwv.lock` \
                             where the branch starts at another commit than the checked-out \
                             one. In a detached project repo the choice is `rwv branch create`, \
                             which carries both files",
                offers: &[Form::C5Lock],
                verb_offers: &[
                    ("switch", Form::C30BranchCreate),
                    ("branch create", Form::C30BranchCreate),
                ],
            },
            Self::CheckoutInAnotherWorkspace => &StateRow {
                axis: Axis::Checkout,
                name: "in another workspace",
                means: "a workweave's checkout derives from the clone, named; or the \
                        branch is checked out there, the primary's own checkout included",
                refused_by: "`rwv member remove --delete` in the primary, and replay's prune \
                             of a dropped member there; `rwv branch delete`, `rwv switch` and \
                             `rwv workweave create --branch`, for a project branch checked out \
                             in another workspace",
                offers: &[Form::C16MemberRemove, Form::C16WorkweaveDelete],
                verb_offers: &[
                    ("switch", Form::C16Detach),
                    ("workweave create", Form::C16Detach),
                ],
            },
            Self::CheckoutInRawWorktree => &StateRow {
                axis: Axis::Checkout,
                name: "in a raw worktree",
                means: "a worktree no workweave records derives from the clone, named by \
                        path; or the branch is checked out there",
                refused_by: "`rwv member remove --delete` in the primary, and replay's prune \
                             of a dropped member there; `rwv branch delete`, for a branch \
                             checked out in a raw worktree",
                offers: &[Form::C16Raw],
                verb_offers: &[],
            },
            Self::PushBranchDiverged => &StateRow {
                axis: Axis::PushBranch,
                name: "diverged",
                means: "the branch and its upstream each hold commits the other lacks, counted \
                        on the line",
                refused_by: "`rwv pull`, for the project branch; `rwv update --remote`, for a \
                             member, and with `--merge-upstream` for a member branch a commit \
                             of the unpushed project range declares, checked out nowhere, whose \
                             merge as refs would conflict; `rwv branch create` and `rwv \
                             workweave create`, for a branch that would start at the tip of a \
                             branch that has diverged from its upstream",
                offers: &[
                    Form::C2Pull,
                    Form::C3Pull,
                    Form::C2UpdateRemote,
                    Form::C3UpdateRemote,
                ],
                verb_offers: &[
                    ("branch create", Form::C34),
                    ("workweave create", Form::C34),
                    ("push", Form::Republish),
                    ("sync", Form::Republish),
                    (UPDATE_REMOTE, Form::C3HeldByDeclared),
                    (UPDATE_REMOTE, Form::C15ThenReset),
                    (UPDATE_REMOTE, Form::C27ThenReset),
                ],
            },
            Self::PushBranchBehind => &StateRow {
                axis: Axis::PushBranch,
                name: "behind",
                means: "the branch is an ancestor of its push branch, as last fetched: the \
                        remote holds commits the branch lacks",
                refused_by: "`rwv push` skips it and reports it; `rwv status` reports it",
                offers: &[Form::C1Pull, Form::C1UpdateRemote],
                verb_offers: &[],
            },
            Self::PushBranchAhead => &StateRow {
                axis: Axis::PushBranch,
                name: "ahead",
                means: "its push branch is an ancestor of the branch, as last fetched: the \
                        branch holds commits the remote lacks",
                refused_by: "`rwv status` reports it, and `rwv land` for the branch it \
                             landed into; `rwv push` publishes it",
                offers: &[Form::C7Push, Form::C7PushRepo],
                verb_offers: &[("land", Form::C7PushBranch)],
            },
            Self::PushBranchGone => &StateRow {
                axis: Axis::PushBranch,
                name: "gone",
                means: "the branch takes in from a branch its remote no longer has, as last \
                        fetched, and holds commits no remote-tracking branch reaches, listed",
                refused_by: "`rwv push` skips it unless `--repo` names it; `rwv status` \
                             reports it",
                offers: &[
                    Form::C10Adopt,
                    Form::C11SetBranch,
                    Form::C22,
                    Form::C9BranchDeleteDiscardUnlanded,
                    Form::C7PushRepo,
                ],
                verb_offers: &[],
            },
            Self::PushBranchForeignUpstream => &StateRow {
                axis: Axis::PushBranch,
                name: "foreign upstream",
                means: "the branch takes in from a branch of another name, which the subject \
                        names; a branch publishes to the branch of its own name",
                refused_by: "`rwv push`; `rwv status` reports it",
                offers: &[Form::StopTakingIn],
                verb_offers: &[],
            },
            Self::PushBranchNoPushTarget => &StateRow {
                axis: Axis::PushBranch,
                name: "no push target",
                means: "no remote is configured for the branch to push to, and the repo has no \
                        url remote to fall back on",
                refused_by: "`rwv push`; `rwv status` reports it",
                offers: &[Form::SetPublishRemote, Form::AddRemote],
                verb_offers: &[],
            },
            Self::PushBranchUnconfirmed => &StateRow {
                axis: Axis::PushBranch,
                name: "unconfirmed",
                means: "the last pruning fetch of the remote the branch publishes to could not \
                        complete, the cause listed; the verb needs the remote's answer and \
                        judges nothing from refs as last fetched",
                refused_by: "`rwv push`",
                offers: &[Form::C33],
                verb_offers: &[],
            },
            Self::PinNone => &StateRow {
                axis: Axis::Pin,
                name: "none",
                means: "the lock has no entry for the member, or there is no lock file; the \
                        checkout is on the declared branch at the tip of the remote's branch, \
                        or where it was found",
                refused_by: "`rwv push`; `rwv land` and `rwv sync`, for a member of the \
                             workspace they run in; `rwv clone`, which does not refuse: it \
                             completes, lists the member and exits with status 3; `rwv lock`, `rwv \
                             update` and `rwv status` report it",
                offers: &[Form::C25, Form::C1UpdateRemoteCommit],
                verb_offers: &[],
            },
            Self::PinUnreachable => &StateRow {
                axis: Axis::Pin,
                name: "unreachable",
                means: "the lock pins a commit that no ref in the clone reaches; the checkout \
                        is on the declared branch at the tip of the remote's branch",
                refused_by: "`rwv push`; `rwv land`, for a pin of the target's lock as last \
                             fetched, naming `rwv fetch`; `rwv clone`, which does not refuse: it \
                             completes, lists the member and exits with status 3; `rwv \
                             cherry-pick`, for a pin the ported commit or its parent holds; `rwv \
                             status` reports it",
                offers: &[Form::AddRemote, Form::C25, Form::C19],
                verb_offers: &[("land", Form::C33)],
            },
            Self::NameTaken => &StateRow {
                axis: Axis::Name,
                name: "taken",
                means: "the project already records a workweave by this name at another path, \
                        or the directory the workweave would take is another workweave's, or \
                        holds a workweave that is not clean, was made from another parent, or \
                        holds something other than what this spelling would make; or a \
                        member's clone has a remote holding the name its url remote would take, \
                        with another url",
                refused_by: "`rwv workweave create`; for a member's url remote, `rwv fetch`, \
                             `rwv pull`, `rwv update`, `rwv update --remote`, \
                             `rwv backup prune`, `rwv init` and `rwv member add`",
                offers: &[],
                verb_offers: &[
                    ("workweave create", Form::AnotherName),
                    ("workweave create", Form::C9WorkweaveDelete),
                    ("fetch", Form::AddRemote),
                    ("pull", Form::AddRemote),
                    ("backup prune", Form::AddRemote),
                    ("member add", Form::AddRemote),
                    ("update", Form::AddRemote),
                    ("init", Form::AddRemote),
                ],
            },
            Self::NameExists => &StateRow {
                axis: Axis::Name,
                name: "exists",
                means: "a repo already has a branch of the name the verb would give its \
                        checkout, standing at a revision other than the one the verb would \
                        start it at, which the line lists; it holds commits the verb did not \
                        put there. For a rename, the new name is a local branch of the project \
                        repo or of a member the rename would move, or a branch a remote has, \
                        as last fetched; `--adopt` takes up the remote's",
                refused_by: "`rwv workweave create`, for the branch it mints in the project \
                             repo and in each owned or fork member; `rwv branch rename`; \
                             `rwv member set-branch --new`, for a branch the member's repo \
                             already has; `rwv branch create`, for a name any repo of the \
                             workspace has, locally or on a remote as last fetched",
                offers: &[],
                verb_offers: &[
                    ("workweave create", Form::AnotherName),
                    ("branch create", Form::AnotherName),
                    ("branch create", Form::OpenSwitch),
                    ("workweave create", Form::OpenWorkweave),
                    ("branch rename", Form::C10),
                    ("branch rename", Form::C10Adopt),
                    ("member set-branch", Form::C11SetBranch),
                ],
            },
            Self::NameARevision => &StateRow {
                axis: Axis::Name,
                name: "a revision",
                means: "the branch `rwv member add` would declare reads as a commit id (seven \
                        or more hex digits) or a release tag (`v2.0`, `v0.3.4-rc1`)",
                refused_by: "`rwv member add`, for the remote's default branch or, with \
                             `--new`, the branch the new repo is on; `rwv member set-branch`",
                offers: &[Form::C11SetBranch],
                verb_offers: &[],
            },
            Self::NameNoSuchBranch => &StateRow {
                axis: Axis::Name,
                name: "no-such-branch",
                means: "a branch or revision the verb was asked to read names nothing in this \
                        repo, spelled as the repo's VCS resolves it; the name is listed with the \
                        repo it is missing from, and the repos that have it are still read",
                refused_by: "`rwv log`, for a range endpoint a member lacks, and for the weave \
                             branch `--against` names; `rwv branch set-mode`, for a branch the \
                             project repo has neither locally nor on a remote as last fetched; \
                             `rwv member set-branch`, for a branch the member's repo has neither \
                             locally nor on a remote as last fetched; `rwv lock --commit` and `rwv \
                             update --remote`, for a declared branch a hand edit named that \
                             exists nowhere; `rwv switch`, \
                             for a branch the project repo lacks, and `rwv branch create`, for a \
                             `--target` or `--at` that names nothing here; `rwv cherry-pick`, \
                             for a `<rev>` the project repo cannot resolve",
                offers: &[Form::C33, Form::C11SetBranchNew, Form::C18],
                verb_offers: &[
                    ("switch", Form::C24),
                    ("switch", Form::C14Keep),
                ],
            },
            Self::NameReserved => &StateRow {
                axis: Axis::Name,
                name: "reserved",
                means: "the name is the one rwv keeps its branch records under, or is below it \
                        (`<records>/<name>`); no weave branch can take it",
                refused_by: "`rwv branch set-mode`, `rwv branch create`, `rwv switch`, `rwv init`, \
                             `rwv clone`, and the verbs that align, lock, land or publish while \
                             the project repo is on a branch of that name; `rwv member set-branch`",
                offers: &[Form::C10],
                verb_offers: &[
                    ("member set-branch", Form::C11SetBranch),
                    ("branch create", Form::AnotherName),
                    ("switch", Form::AnotherName),
                ],
            },
            Self::NameMinted => &StateRow {
                axis: Axis::Name,
                name: "minted",
                means: "the project branch is the name `rwv workweave create` gave a workweave \
                        that was not given one, `<project>--<workweave>`, which is never \
                        published, and neither is its record",
                refused_by: "`rwv push`",
                offers: &[Form::C10],
                verb_offers: &[],
            },
            Self::NameDeclared => &StateRow {
                axis: Axis::Name,
                name: "declared",
                means: "a feature would take a name that a live weave branch declares as a \
                        member's branch, the member and the declaring branches listed: the \
                        member's branch of that name is the declared branch, and a feature's \
                        own member branch cannot be it",
                refused_by: "the verbs that write, open, align or publish a feature",
                offers: &[Form::C10, Form::C12Declared],
                verb_offers: &[("branch create", Form::AnotherName)],
            },
            Self::NameFeature => &StateRow {
                axis: Axis::Name,
                name: "feature",
                means: "a feature would take a name that another project's feature of that \
                        name already uses for a member both list, the project and the member \
                        named: one member branch would serve two features",
                refused_by: "the verbs that write, open, align or publish a feature; `rwv member \
                             set-branch`, for a branch a feature of that name uses",
                offers: &[Form::C10, Form::C12Declared],
                verb_offers: &[
                    ("member set-branch", Form::C11SetBranch),
                    ("branch create", Form::AnotherName),
                ],
            },
            Self::PublishWithheld => &StateRow {
                axis: Axis::Publish,
                name: "withheld",
                means: "the project repo was not pushed: a pin the project branch's lock \
                        holds is not reached by the remote-tracking branches that must reach \
                        it, or the remote rejected a member's push; the branches that do reach \
                        each pin, and what the remote answered, are listed",
                refused_by: "`rwv push`",
                offers: &[
                    Form::C7Push,
                    Form::C7PushRepo,
                    Form::C7PushTarget,
                    Form::C27,
                    Form::SetPublishRemote,
                ],
                verb_offers: &[],
            },
            Self::PublishRecordsRejected => &StateRow {
                axis: Axis::Publish,
                name: "records rejected",
                means: "a fact: the remote the project branch publishes to refused the branch \
                        that holds the weave branches' records, and the project branch was \
                        published without its record there. What the remote answered is \
                        listed. Another machine opening the branch finds it unrecorded and is \
                        asked once how to treat it (`target` `unrecorded`)",
                refused_by: "nothing: `rwv push` reports it and publishes the branch",
                offers: &[],
                verb_offers: &[],
            },
            Self::PublishNoPin => &StateRow {
                axis: Axis::Publish,
                name: "no pin",
                means: "the project branch's lock has no entry for a member the push does not \
                        act on, so the project repo was not pushed with a lock that pins \
                        nothing there",
                refused_by: "`rwv push`",
                offers: &[Form::C25],
                verb_offers: &[],
            },
            Self::UnparseableProjectHere => &StateRow {
                axis: Axis::UnparseableProject,
                name: "here",
                means: "the project's `rwv.toml` at the checked-out tip names member-entry keys \
                        an earlier release wrote, each listed with the key that replaces it",
                refused_by: "every verb that reads the project's `rwv.toml`; `rwv doctor` \
                             reports it instead, and `rwv status` and `rwv prime` skip the \
                             project with a warning",
                offers: &[Form::DoctorFix],
                verb_offers: &[],
            },
            Self::UnparseableProjectMalformed => &StateRow {
                axis: Axis::UnparseableProject,
                name: "malformed",
                means: "the project's `rwv.toml` or `rwv.lock` does not parse, listed with the \
                        file that failed and the parser's error",
                refused_by: "`rwv sync` and `rwv land`, when the project's `rwv.lock` does not \
                             load after the project repo is replayed; `rwv doctor` reports it \
                             for every project",
                offers: &[],
                verb_offers: &[],
            },
            Self::UnparseableProjectAt => &StateRow {
                axis: Axis::UnparseableProject,
                name: "at",
                means: "the `rwv.toml` at the tip of a live branch the verb acts on, not the \
                        checked-out one, names member-entry keys an earlier release wrote or \
                        does not parse, with the parser's error. `rwv doctor --fix` is offered \
                        for the keys an earlier release wrote; any other cause is fixed by a \
                        commit on that branch",
                refused_by: "the verbs that act on that branch",
                offers: &[Form::DoctorFixOnBranch],
                verb_offers: &[],
            },
            Self::UnparseableProjectMidOperation => &StateRow {
                axis: Axis::UnparseableProject,
                name: "mid-operation",
                means: "the `rwv.toml` the operation's merge or replay of the project repo \
                        produced does not parse, listed with the parser's error: the two sides \
                        merged as text without a conflict into a file that is not a manifest, \
                        or a conflict in it was resolved into one. Under merge history the merge is in progress and the merged file is \
                        in the working tree, marked resolved; continuing commits it in the \
                        merge. Under linear history the replay finished and the replayed tip \
                        holds the file; continuing makes the resolved file part of that \
                        commit, which the replay wrote and nothing has published. A resolved \
                        file that parses under a working file that differs and does not parse \
                        stops here too, before anything is committed",
                refused_by: "`rwv sync`, `rwv land` and `rwv pull --merge-upstream` when they \
                             first read what the merge or replay produced, and their \
                             `--continue` while the resolved `rwv.toml` does not parse; `rwv \
                             abort` runs",
                offers: &[
                    Form::StageParsingManifest,
                    Form::ResumeWithManifest,
                    Form::AbortOperation,
                ],
                verb_offers: &[],
            },
            Self::EditsReplayDiffers => &StateRow {
                axis: Axis::Edits,
                name: "replay differs",
                means: "under linear history the replay finished and the replayed tip's \
                        `rwv.toml` parses, but differs from the merge of the values of the \
                        merge base, the commit replayed onto and the branch's tip before the \
                        replay, each entry that differs listed; an entry both sides changed \
                        differently keeps the tip's value. Nothing is committed until the \
                        file is settled",
                refused_by: "`rwv sync`, `rwv land` and `rwv pull --merge-upstream` when they \
                             first read what the replay produced; `rwv abort` runs",
                offers: &[
                    Form::KeepTextMerge,
                    Form::StageOwnManifest,
                    Form::AbortOperation,
                ],
                verb_offers: &[],
            },
            Self::UnparseableProjectCommit => &StateRow {
                axis: Axis::UnparseableProject,
                name: "commit",
                means: "the `rwv.toml` of a commit the verb reads by name does not parse, listed \
                        with the commit and the parser's error: one it copies from or compares \
                        with, or one it would check out or start a workweave at. The verb cannot \
                        tell which members the commit declares, so it moves and makes nothing; \
                        a commit is never rewritten, so nothing makes its `rwv.toml` parse",
                refused_by: "`rwv cherry-pick`, `rwv switch` to a commit, `rwv workweave create \
                             --at`",
                offers: &[],
                verb_offers: &[],
            },
            Self::MissingLockMergeDriverAttributeAbsent => &StateRow {
                axis: Axis::MissingLockMergeDriver,
                name: "attribute absent",
                means: "the checked-out branch's committed `.gitattributes` assigns `rwv.lock` \
                        no merge driver, so a merge rwv does not make merges the lock as text",
                refused_by: LOCK_DRIVER_REFUSED_BY,
                offers: &[Form::MigrateLockDriver],
                verb_offers: &[],
            },
            Self::MissingLockMergeDriverAttributeLegacy => &StateRow {
                axis: Axis::MissingLockMergeDriver,
                name: "attribute legacy",
                means: "the checked-out branch's committed `.gitattributes` assigns `rwv.lock` \
                        `merge=rwv-ours` or `merge=ours`, a driver an earlier release wrote \
                        that kept one side of every lock merge",
                refused_by: LOCK_DRIVER_REFUSED_BY,
                offers: &[Form::MigrateLockDriver],
                verb_offers: &[],
            },
            Self::MissingLockMergeDriverConfigAbsent => &StateRow {
                axis: Axis::MissingLockMergeDriver,
                name: "config absent",
                means: "the clone's config does not define the lock merge driver's command, \
                        so git merges the lock as text",
                refused_by: LOCK_DRIVER_REFUSED_BY,
                offers: &[Form::MigrateLockDriver],
                verb_offers: &[],
            },
            Self::MissingLockMergeDriverConfigLegacy => &StateRow {
                axis: Axis::MissingLockMergeDriver,
                name: "config legacy",
                means: "the clone's config defines a driver that keeps one side: the \
                        `merge.rwv-ours` entry an earlier release wrote, or `merge.rwv-lock` \
                        naming a command other than `rwv lock --merge-driver %O %A %B`",
                refused_by: LOCK_DRIVER_REFUSED_BY,
                offers: &[Form::MigrateLockDriver],
                verb_offers: &[],
            },
            Self::PinWouldBeUnreachable => &StateRow {
                axis: Axis::Pin,
                name: "would be unreachable",
                means: "a live lock pins a commit that only the backups being dropped reach, \
                        listed with the locks that pin it",
                refused_by: "`rwv backup drop` and `rwv backup prune`",
                offers: &[Form::C7Push, Form::C27],
                verb_offers: &[],
            },
            Self::PinHeldByNoBranch => &StateRow {
                axis: Axis::Pin,
                name: "held by no branch",
                means: "a project commit of the range being pushed pins the commit named, which \
                        no branch or remote-tracking branch reaches, listed with the project \
                        commits pinning it, the backups holding it, and the pushed tip's pin \
                        where that records a copy of it. A pin no backup holds is not listed \
                        where a commit of the range re-pinned the member by name off it or off \
                        a commit containing it. Only the pushed tip's pins must be reached, so \
                        the push publishes the range",
                refused_by: "nothing refuses; `rwv push` reports it",
                offers: &[Form::C27],
                verb_offers: &[],
            },
            Self::TargetNoTarget => &StateRow {
                axis: Axis::Target,
                name: "no target",
                means: "the branch's record is `declared`, so it names no branch the branch \
                        lands into",
                refused_by: "`rwv branch delete`, which needs the branch whose commits \
                             should have landed, named with `--target`; `rwv cherry-pick`, \
                             which ports into a feature onto the branch",
                offers: &[Form::C12Feature],
                verb_offers: &[
                    ("cherry-pick", Form::C37BranchCreate),
                    ("cherry-pick", Form::C37WorkweaveCreate),
                ],
            },
            Self::TargetUnrecorded => &StateRow {
                axis: Axis::Target,
                name: "unrecorded",
                means: "the branch has no record here: a raw switch or rename made it, its \
                        record was never published, or it came from a remote that carries none. \
                        The branches that could be its target are listed, nearest first",
                refused_by: "`rwv lock`, `rwv update`, `rwv pull`, `rwv sync`, `rwv land` and \
                             `rwv push`, for the project repo's checked-out branch; `rwv \
                             workweave create`, for the branch it forks from; `rwv branch \
                             create` with no spelling, for the branch it starts at, and `--target`, \
                             for a target that has no record; `rwv switch` and `rwv workweave \
                             create --branch`, for a branch with no record; `rwv branch \
                             set-mode`, for a `--target` that has no record",
                offers: &[Form::C33, Form::C12Feature, Form::C12Declared],
                verb_offers: &[],
            },
            Self::TargetConflict => &StateRow {
                axis: Axis::Target,
                name: "conflict",
                means: "two writers recorded the branch differently and a merge kept both \
                        values, listed; the conflict stays on the machine whose merge met it",
                refused_by: "every verb that reads the branch's record on this machine",
                offers: &[Form::C12Feature, Form::C12Declared],
                verb_offers: &[],
            },
            Self::TargetDiffers => &StateRow {
                axis: Axis::Target,
                name: "differs",
                means: "the record a verb would give the branch differs from the one it holds, \
                        both listed: a record is replaced only by `rwv branch set-mode`; \
                        nothing on the records branch is conflicted",
                refused_by: "`rwv branch rename --adopt` onto a name recorded otherwise",
                offers: &[Form::C12Feature, Form::C12Declared],
                verb_offers: &[],
            },
            Self::TargetUnreadable => &StateRow {
                axis: Axis::Target,
                name: "unreadable",
                means: "the branch's entry on the records branch does not read as a record, \
                        with why and the rwv release that wrote it when its commit names one; \
                        other branches' entries read as usual",
                refused_by: "every verb that reads the branch's record",
                offers: &[Form::C12Feature, Form::C12Declared],
                verb_offers: &[],
            },
            Self::TargetCycle => &StateRow {
                axis: Axis::Target,
                name: "cycle",
                means: "the branch's target chain returns to it, listed from the branch",
                refused_by: "`rwv branch set-mode`, for a `--target` that would close one, and \
                             every verb that reads the record of a branch a merge of records \
                             put on one",
                offers: &[Form::C12Feature, Form::C12Declared],
                verb_offers: &[],
            },
            Self::AlignmentAligned => &StateRow {
                axis: Axis::Alignment,
                name: "aligned",
                means: "the checkout is in its expected state; the state is printed only when \
                        it carries a fact: `checkout`, the member's branch being checked out \
                        in another workspace (named), so the member sits detached at its pin \
                        and nothing moves it",
                refused_by: "`rwv status` reports it with the fact; `rwv update` reports the \
                             fact for each member it detaches",
                offers: &[],
                verb_offers: &[],
            },
            Self::AlignmentOnBranch => &StateRow {
                axis: Axis::Alignment,
                name: "on branch",
                means: "the checkout is on a branch other than the expected one, and no \
                        commit it holds would be left on no other branch or backup if it \
                        moved off",
                refused_by: "`rwv update` reports it for a checkout it did not move, one with \
                             uncommitted changes or an operation in progress; `rwv status` \
                             reports it; `rwv activate` refuses it for a member shared with \
                             the project the weave root presents, and `rwv update` and `rwv \
                             pull` for one of a project the weave root does not present",
                offers: &[Form::C6],
                verb_offers: &[(ACTIVATE, Form::C6Activate)],
            },
            Self::AlignmentOnBranchWithStrayCommits => &StateRow {
                axis: Axis::Alignment,
                name: "on branch with stray commits",
                means: "the checkout is on a branch other than the expected one, which is \
                        neither a declared branch nor named after a recorded weave branch, and \
                        holds the commits listed that no other branch, remote-tracking branch \
                        or backup reaches",
                refused_by: "`rwv update` reports it and leaves the checkout; `rwv status` \
                             reports it; `rwv activate`, `rwv update` and `rwv pull` refuse it \
                             as `on branch` is refused",
                offers: &[Form::C15OntoCheckedOut, Form::C14Keep],
                verb_offers: &[],
            },
            Self::AlignmentDetachedElsewhere => &StateRow {
                axis: Axis::Alignment,
                name: "detached",
                means: "the checkout is detached where it is not expected to be, and no commit \
                        reaches it that a branch lacks",
                refused_by: "`rwv update` reports it for a checkout it did not move; `rwv \
                             status` reports it; `rwv activate`, `rwv update` and `rwv pull` \
                             refuse it as `on branch` is refused",
                offers: &[Form::C6],
                verb_offers: &[(ACTIVATE, Form::C6Activate)],
            },
            Self::AlignmentDetachedUnpushed => &StateRow {
                axis: Axis::Alignment,
                name: "detached with unpushed commits",
                means: "the checkout is detached and holds the commits listed, which no branch \
                        or remote-tracking branch reaches",
                refused_by: "`rwv update` reports it and leaves the checkout; `rwv status` \
                             reports it; `rwv activate`, `rwv update` and `rwv pull` refuse it \
                             as `on branch` is refused",
                offers: &[Form::C14Keep, Form::C15OntoCheckedOut],
                verb_offers: &[],
            },
            Self::AlignmentUnaligned => &StateRow {
                axis: Axis::Alignment,
                name: "unaligned",
                means: "the lock has no entry for the member and its expected branch is found \
                        nowhere, so it stays where it is and its pin stays out of the lock",
                refused_by: "`rwv update` and `rwv status` report it",
                offers: &[Form::C25, Form::C18],
                verb_offers: &[],
            },
            Self::AlignmentProjectMoved => &StateRow {
                axis: Axis::Alignment,
                name: "project moved",
                means: "the project repo is on another branch than the one the workspace last \
                        aligned its members for, which still exists, named in the subject",
                refused_by: "nothing refuses at the project level; `rwv status` reports it as \
                             a fact, and each member not aligned for the checked-out branch \
                             reports its own line",
                offers: &[Form::C30Switch, Form::C6],
                verb_offers: &[],
            },
            Self::AlignmentProjectUnaligned => &StateRow {
                axis: Axis::Alignment,
                name: "project unaligned",
                means: "the workspace has no record of the branch it last aligned its members \
                        for, or that branch is gone",
                refused_by: "`rwv status` reports it; `rwv update` records it",
                offers: &[Form::C6],
                verb_offers: &[],
            },
            Self::AlignmentProjectDetached => &StateRow {
                axis: Axis::Alignment,
                name: "project detached",
                means: "the project repo's HEAD is on no branch and the workspace was not made \
                        detached; the commits it holds that no branch reaches are listed",
                refused_by: "`rwv update`; `rwv branch create` with no spelling, `rwv branch \
                             rename` and `rwv workweave create`, which need a checked-out \
                             branch; `rwv status` reports it",
                offers: &[Form::C14KeepThenSwitch, Form::C30Switch, Form::C30BranchCreate],
                verb_offers: &[],
            },
            Self::PinNotOnDeclaredBranch => &StateRow {
                axis: Axis::Pin,
                name: "not on declared branch",
                means: "the lock pins a commit that neither the member's declared branch nor \
                        its upstream reaches, so the checkout sits detached at the pin",
                refused_by: "`rwv member pin`, for a commit that neither reaches; `rwv \
                             cherry-pick`, for a `dependency` or `reference` member's pin it \
                             would carry; `rwv update` and `rwv status` report it",
                offers: &[Form::C19, Form::C15, Form::C25, Form::C11SetBranch],
                verb_offers: &[("cherry-pick", Form::C25AtCherryPick)],
            },
            Self::PinBehind => &StateRow {
                axis: Axis::Pin,
                name: "behind",
                means: "the checkout's commit is an ancestor of the commit the lock pins: the \
                        lock records commits the checkout lacks",
                refused_by: "`rwv push`; `rwv status` reports it",
                offers: &[Form::C6, Form::C1UpdateRemote],
                verb_offers: &[],
            },
            Self::PinAhead => &StateRow {
                axis: Axis::Pin,
                name: "ahead",
                means: "the checkout holds commits beyond the commit the lock pins, and each is \
                        pushed or pinned by another live lock, which the facts name; the \
                        commits are published with `rwv push` only where none is pushed yet",
                refused_by: "`rwv status` reports it; `rwv push` reports it and goes on; `rwv \
                             cherry-pick` refuses it for a member it would copy onto",
                offers: &[Form::C5Lock, Form::C7PushRepo],
                verb_offers: &[],
            },
            Self::PinUnpinned => &StateRow {
                axis: Axis::Pin,
                name: "unpinned",
                means: "the checkout holds the commits listed beyond the commit the lock pins, \
                        which no remote-tracking branch and no live lock reaches",
                refused_by: "`rwv push`; `rwv cherry-pick`, for a member it would copy onto; \
                             `rwv land`, in the target's workspace, or for a branch the target \
                             declares and nothing has checked out; `rwv status` reports it",
                offers: &[Form::C5Lock, Form::C25, Form::C3UpdateRemote],
                verb_offers: &[("land", Form::OpenWorkweave)],
            },
            Self::PinDiverged => &StateRow {
                axis: Axis::Pin,
                name: "diverged",
                means: "neither the checkout's commit nor the commit the lock pins is an \
                        ancestor of the other",
                refused_by: "`rwv push`; `rwv status` reports it",
                offers: &[Form::C25, Form::C3UpdateRemote, Form::C22],
                verb_offers: &[],
            },
            Self::PinOffLineage => &StateRow {
                axis: Axis::Pin,
                name: "off lineage",
                means: "the member's work is built on a commit the branch it would move into \
                        lacks: its base, the newest commit of its first-parent chain that a \
                        lock in the target's history pins or built on, listed with the \
                        receiving branch",
                refused_by: "`rwv sync`, for a member it would integrate, or that would follow \
                             while the branch has dependents; `rwv update --remote \
                             --merge-upstream` and `rwv pull --merge-upstream`, for a \
                             feature's member",
                offers: &[Form::C22, Form::C11SetBranch],
                verb_offers: &[
                    ("update", Form::C3UpdateRemote),
                    ("update", Form::C27),
                    ("pull", Form::C3Pull),
                    ("pull", Form::C27),
                ],
            },
            Self::PinOffRemote => &StateRow {
                axis: Axis::Pin,
                name: "off remote",
                means: "a live lock pins a commit that no remote-tracking branch of the member \
                        reaches after the pruning fetch, listed with the locks that pin it. \
                        Choices print only in a workspace whose checked-out branch's lock holds \
                        the pin: a feature's offers publishing it, a declared branch's offers \
                        publishing it where the member's checkout holds the pin, else \
                        pinning another commit; elsewhere the state is a fact and offers \
                        waiting",
                refused_by: "nothing refuses; `rwv fetch` and `rwv pull` report it",
                offers: &[
                    Form::C22,
                    Form::C7PushRepo,
                    Form::C27,
                    Form::C25,
                    Form::C19,
                ],
                verb_offers: &[],
            },
            Self::PinUnmerged => &StateRow {
                axis: Axis::Pin,
                name: "unmerged",
                means: "a merge commit in the range being pushed joined two pins of one \
                        member that its parents changed, each under one declared branch, and \
                        the pushed tip pins neither merged with the other: a commit holding \
                        both. The merge and the pins are listed. A member the parents declare \
                        differently is listed and not refused. A commit at or after the merge \
                        that records re-pinning the member backward by name, to a commit \
                        reaching neither pin the merge left unmerged, waives it (`pin` \
                        waived); a commit made outside rwv records none, and a record whose old \
                        pin no parent's lock holds, or whose new pin its own lock does not, \
                        waives nothing and is listed. At a landing: the feature and the target each changed a \
                        `dependency` or `reference` member's pin since their merge base, and \
                        neither pin contains the other",
                refused_by: "`rwv push`; `rwv land`, before anything moves",
                offers: &[Form::C26, Form::C25],
                verb_offers: &[],
            },
            Self::ProtectedDeclared => &StateRow {
                axis: Axis::Protected,
                name: "declared",
                means: "a member's branch that some live weave branch declares, listed with \
                        the weave branches declaring it; rwv never rewrites, renames or \
                        deletes it",
                refused_by: "`rwv push --overwrite`, for a member; `rwv branch \
                             delete` and `rwv workweave delete` with `--delete-remote` or \
                             `--delete-remote-unlanded`, for a member branch of the deleted \
                             name; `rwv sync` where the target's `history` rebases a member \
                             on the branch that has commits to replay; `rwv sync` and `rwv \
                             land` where carrying a dependent along would rewrite its member \
                             branch",
                offers: &[Form::C2UpdateRemote, Form::C12Declared],
                verb_offers: &[],
            },
            Self::ProtectedDeclaredMode => &StateRow {
                axis: Axis::Protected,
                name: "declared mode",
                means: "a project branch whose record is `declared`; rwv never rewrites it, \
                        and never deletes its unlanded push branch. Renaming it, and deleting \
                        its push branch once landed, run",
                refused_by: "`rwv push --overwrite`; `rwv branch delete \
                             --delete-remote-unlanded`, for the project repo",
                offers: &[Form::C2Pull],
                verb_offers: &[("branch delete", Form::RetireDeclared)],
            },
            Self::PinWaived => &StateRow {
                axis: Axis::Pin,
                name: "waived",
                means: "a merge in the range being pushed left one member's pins unmerged, and \
                        a commit in the range at or after it, the merge's own included, \
                        records re-pinning the member backward by name to a commit that does \
                        not reach the pin the merge left. The merge, the re-pin that waived it \
                        and the verb that made it are listed. A landing that rebased the member \
                        records that re-pin in its project commit, from the commit the feature \
                        pinned to the one the rebase made of it, and is listed as such",
                refused_by: "nothing refuses; `rwv push` reports it and goes on",
                offers: &[],
                verb_offers: &[],
            },
            Self::PinBackwards => &StateRow {
                axis: Axis::Pin,
                name: "backwards",
                means: "recording the commit the checkout is at would move the pin to one that \
                        does not descend from the pinned commit: it is behind the pin or has \
                        diverged from it, or the pinned commit is in no ref here",
                refused_by: "`rwv lock`, with or without `--repo`; `rwv update --remote`; \
                             `rwv cherry-pick`, for a member `--except` names; `rwv land` and \
                             `rwv sync`, for a member of the workspace they run in, before \
                             anything moves. A pin moves backward only by name: `rwv member pin`, a setter, \
                             or a hand edit of the member's entry that `rwv lock --commit` \
                             commits",
                offers: &[Form::C25],
                verb_offers: &[("update", Form::C3UpdateRemote)],
            },
            Self::TargetDependents => &StateRow {
                axis: Axis::Target,
                name: "dependents",
                means: "another branch's record names this branch as the one it lands into, \
                        one line for each such branch; deleting this one would leave that \
                        record with no target",
                refused_by: "`rwv branch delete`, `rwv workweave delete`, `rwv land \
                             --retire`, and `--reset-to-target` or `--reset-to-upstream` on \
                             this branch; a rebase carries dependents along instead",
                offers: &[Form::C35, Form::C35Retire, Form::C12Feature],
                verb_offers: &[],
            },
            Self::TargetDependentLacksTip => &StateRow {
                axis: Axis::Target,
                name: "dependent lacks the tip",
                means: "a branch whose record names this branch as the one it lands into was \
                        cut from an older commit of it: in the repos listed it lacks this \
                        branch's tip, so a rewrite of this branch cannot carry it along",
                refused_by: "`rwv sync` and `rwv land` where they would rebase this branch; \
                             nothing moves",
                offers: &[Form::C35, Form::C12Feature, Form::C36Dependent],
                verb_offers: &[],
            },
            Self::TargetDependentCheckedOut => &StateRow {
                axis: Axis::Target,
                name: "dependent checked out elsewhere",
                means: "a branch whose record names this branch as the one it lands into is \
                        checked out in another workspace, where a rewrite of this branch \
                        cannot move it",
                refused_by: "`rwv sync` and `rwv land` where they would rebase this branch; \
                             nothing moves",
                offers: &[
                    Form::C16WorkweaveDelete,
                    Form::C35,
                    Form::C35Retire,
                    Form::C12Feature,
                    Form::LostRace,
                ],
                verb_offers: &[],
            },
            Self::TargetDependentOnAnotherMachine => &StateRow {
                axis: Axis::Target,
                name: "dependent on another machine",
                means: "a branch whose record names this branch as the one it lands into is \
                        here only as a remote's copy, published from another machine with \
                        its record, holding commits this branch's tip lacks: no verb here \
                        moves it. A copy this branch's tip already holds has landed: it is \
                        reported, not refused",
                refused_by: "`rwv sync` and `rwv land` where they would rebase this branch; \
                             nothing moves",
                offers: &[Form::C35, Form::C12Feature],
                verb_offers: &[],
            },
            Self::TargetDependentMovedMidOperation => &StateRow {
                axis: Axis::Target,
                name: "dependent moved mid-operation",
                means: "a dependent's branch a carry composed moved after the carry read it, \
                        so what the carry composed no longer holds it: each branch and where \
                        it now stands, listed. The operation stopped before moving any \
                        dependent's branch it had not moved already",
                refused_by: "`rwv sync` and `rwv land` as a carry moves the dependents' \
                             branches",
                offers: &[Form::CarryAgain, Form::LostRace],
                verb_offers: &[],
            },
            Self::HistoryUnset => &StateRow {
                axis: Axis::History,
                name: "unset",
                means: "integrating this repo needs more than a fast-forward, and the branch it \
                        moves into records no `history` for it: neither the member's own nor \
                        the top-level one",
                refused_by: "a verb that takes a target's work in or lands work into it, \
                             where a fast-forward does not do it",
                offers: &[Form::C23SetHistory, Form::C23SetHistoryRepo, Form::C23Rerun],
                verb_offers: &[],
            },
            Self::HistoryContradicted => &StateRow {
                axis: Axis::History,
                name: "contradicted",
                means: "the run names a `history` for one run, and the branch it moves into \
                        records another for this repo",
                refused_by: "a verb that takes a target's work in or lands work into it, run \
                             with a `history` for one run",
                offers: &[
                    Form::C23SetHistory,
                    Form::C23SetHistoryRepo,
                    Form::KeepRecordedHistory,
                ],
                verb_offers: &[],
            },
            Self::HistoryNotFastForward => &StateRow {
                axis: Axis::History,
                name: "not fast-forward",
                means: "the branch records `ff-only` for this repo, and the landing is not a \
                        fast-forward: the target has work the source has not taken in",
                refused_by: "a landing into a branch that records `ff-only`",
                offers: &[Form::C36],
                verb_offers: &[],
            },
            Self::HistoryDisagreement => &StateRow {
                axis: Axis::HistoryDisagreement,
                name: "disagreement",
                means: "projects listing this member record different `history` for it on \
                        their live weave branches, each branch listed with its value",
                refused_by: "nothing refuses; `rwv doctor` reports it",
                offers: &[Form::C11SetHistory],
                verb_offers: &[],
            },
            Self::TargetModeChanged => &StateRow {
                axis: Axis::Target,
                name: "mode changed",
                means: "the branch the feature lands into is used another way than when the \
                        feature's record was written: its mode then and now, listed. A landing \
                        never redirects into it",
                refused_by: "`rwv land`, before anything moves, until the feature is rebound or \
                             the target's mode restored",
                offers: &[Form::C12Feature, Form::C12RestoreTarget],
                verb_offers: &[],
            },
            Self::TargetMemberChanges => &StateRow {
                axis: Axis::Target,
                name: "member changes",
                means: "the landing would change the target's entry for a member, each changed \
                        field listed with the target's value and the feature's, remove the \
                        member, move a `dependency` or `reference` member's pin backward, or \
                        change the target's top-level `history`; a landing changes the \
                        target's entries only where they are named. Each live weave branch \
                        whose entries match the feature's is listed as a `--target` candidate",
                refused_by: "`rwv land`, before anything moves, unless `--allow-member-change` \
                             names the repo: the member's path, or `projects/<p>` for the \
                             top-level `history`; and its `--continue`, judging `rwv.toml` \
                             as the stop's resolution leaves it, unless the landing's start or \
                             the `--continue` names the repo",
                offers: &[
                    Form::C12Feature,
                    Form::C11SetBranch,
                    Form::C11SetUrl,
                    Form::C11SetRole,
                    Form::C11SetRecurseSubmodules,
                    Form::C11SetHistory,
                    Form::C11UnsetHistory,
                    Form::C11SetTopHistory,
                    Form::C11UnsetTopHistory,
                    Form::C11MemberAdd,
                    Form::C25,
                    Form::C31,
                    Form::C31Continue,
                    Form::LostRace,
                ],
                verb_offers: &[],
            },
            Self::TargetMovedMidOperation => &StateRow {
                axis: Axis::Target,
                name: "moved mid-operation",
                means: "the target, or a branch the landing fast-forwards, moved after the \
                        landing read it, or the checkout there is on another branch, so the \
                        work composed in the source no longer fast-forwards onto it: the repo \
                        and where it now stands, listed. The landing stopped with its op \
                        record kept and the refs it already moved left as moved. Once the \
                        landing has written its target, abort cannot put the target back over \
                        what moved it, so the choice ends the landing leaving the target as it \
                        stands",
                refused_by: "`rwv land`, at the fast-forward, and its `--continue`, since \
                             the plan no longer describes the landing",
                offers: &[Form::LostRaceLanding, Form::LeaveTarget],
                verb_offers: &[],
            },
            Self::PublishTargetUnpublished => &StateRow {
                axis: Axis::Publish,
                name: "target unpublished",
                means: "the feature's record names a target with no push branch on the remote \
                        the feature publishes to, so another machine reading the record would \
                        resolve the target to a branch of that name it holds, or to none; the \
                        project branch and its record were not pushed",
                refused_by: "`rwv push`, for a feature",
                offers: &[Form::C7PushTarget, Form::C10, Form::C12Feature],
                verb_offers: &[],
            },
            Self::TargetRecordChanged => &StateRow {
                axis: Axis::Target,
                name: "record changed",
                means: "the checked-out branch's record differs from the one this workspace \
                        last saw it hold, and this workspace did not write the change: the \
                        old and the new value, listed, and whether the change arrived by \
                        fetch or was written in another workspace of this clone",
                refused_by: "`rwv sync` and `rwv land`, before anything is fetched or moved, \
                             until the change is accepted or the old value restored; `rwv \
                             status`, `rwv update`, `rwv pull`, `rwv switch` and `rwv push` \
                             report it",
                offers: &[Form::C6, Form::C12Feature, Form::C12Declared],
                verb_offers: &[],
            },
            Self::TargetGone => &StateRow {
                axis: Axis::Target,
                name: "gone",
                means: "the branch the record lands into exists nowhere: not as a local branch, \
                        nor on a remote as last fetched; or the record was published on a \
                        remote where that branch is not, so the local branch of that name is \
                        another branch",
                refused_by: "`rwv land` and `rwv sync`; `rwv branch list` reports it",
                offers: &[Form::C12Feature, Form::C12Declared],
                verb_offers: &[],
            },
            Self::PortMainline => &StateRow {
                axis: Axis::Port,
                name: "mainline",
                means: "the commit is a merge and no `-m` names the parent to pick it against, \
                        or `-m` names a parent it does not have, or names one for a commit that \
                        is no merge; its parents are listed, numbered from 1",
                refused_by: "`rwv cherry-pick`, before anything moves",
                offers: &[Form::C38, Form::C38NoMainline],
                verb_offers: &[],
            },
            Self::PortMergeInRange => &StateRow {
                axis: Axis::Port,
                name: "merge in range",
                means: "the commits the member's copy would make hold a merge commit, listed \
                        with the whole range: copied commit by commit it would be flattened. \
                        A member whose new pin is a merge on the parent's pin, picked against \
                        the parent `-m` names, ports as that one merge and is not refused",
                refused_by: "`rwv cherry-pick`, before anything moves, unless `--except` names \
                             the member",
                offers: &[Form::C15AtCherryPick, Form::C26AtCherryPick],
                verb_offers: &[],
            },
            Self::PortBackward => &StateRow {
                axis: Axis::Port,
                name: "backward",
                means: "the commit moves the member's pin backward: the parent's pin is not an \
                        ancestor of the commit's, a diverged change included, and the commits \
                        it drops are listed",
                refused_by: "`rwv cherry-pick`, before anything moves, unless `--except` names \
                             the member",
                offers: &[Form::C39, Form::C25AtCherryPick],
                verb_offers: &[],
            },
            Self::PortNotListed => &StateRow {
                axis: Axis::Port,
                name: "not listed",
                means: "the commit moves the pin of a member this branch does not list",
                refused_by: "`rwv cherry-pick`, before anything moves, unless `--except` names \
                             the member",
                offers: &[Form::C11MemberAdd, Form::C40],
                verb_offers: &[],
            },
        }
    }

    /// Start judging `repo` in this state.
    pub fn of(self, repo: impl fmt::Display) -> Judging {
        Judging {
            repo: repo.to_string(),
            scope: None,
            state: self,
            subject: None,
            listed: Vec::new(),
            facts: Vec::new(),
            workspace: None,
            dependents_workspace: None,
            target_workspace: None,
            untracked: false,
            removes_store: false,
            without_at: Vec::new(),
            verb: None,
            only: None,
            only_forms: None,
            without: Vec::new(),
            bindings: BTreeMap::new(),
            places: BTreeMap::new(),
        }
    }

    pub fn axis(self) -> Axis {
        self.row().axis
    }

    pub fn name(self) -> &'static str {
        self.row().name
    }
}

/// A choice: one rwv verb, or one VCS action, that a state can print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChoiceId {
    C1,
    C2,
    C3,
    C5,
    C6,
    C7,
    C9,
    C10,
    C11,
    C12,
    RetireDeclared,
    GiveUpUnlanded,
    C13,
    C14,
    C15,
    C16,
    StopTakingIn,
    SetPublishRemote,
    AddRemote,
    C18,
    C19,
    C22,
    C23,
    C24,
    C25,
    C26,
    C27,
    C30,
    C31,
    C32,
    C33,
    C34,
    C35,
    C36,
    C37,
    C38,
    C39,
    C40,
    AnotherName,
    OpenIt,
    DoctorFix,
    RecordCommitted,
    KeepRecordedHistory,
    MigrateLockDriver,
    LostRace,
    LeaveTarget,
    CarryAgain,
    Republish,
    LandAgain,
    MarkResolved,
    KeepSide,
    ResumeOperation,
    AbortOperation,
    StageParsingManifest,
    ResumeWithManifest,
    KeepTextMerge,
    StageOwnManifest,
    RefreshAgain,
    MergeEditedDrift,
    SetEditsAside,
    ResolveConflicts,
    ReplaceThenMerge,
    CompareMarked,
    CompareSkipped,
    UnstageThenMerge,
    RemoveLockThenMerge,
    StashUnreadDrift,
    UnmarkThenStashUnreadDrift,
    FreeBranchThenMerge,
    RecreateBranchThenMerge,
    ClearRefusalThenMerge,
    TakeBackSetAside,
    FollowSubmodule,
    CommitEdits,
}

/// A choice's id, as `--json` and the page print it (none for a choice the
/// table does not number), what it does, and why it keeps work.
pub struct ChoiceRow {
    pub id: Option<&'static str>,
    pub phrase: &'static str,
    pub keeps_work: &'static str,
}

impl ChoiceId {
    pub fn row(self) -> &'static ChoiceRow {
        match self {
            Self::C1 => &ChoiceRow {
                id: Some("C1"),
                phrase: "take in theirs",
                keeps_work: "fast-forward only",
            },
            Self::C2 => &ChoiceRow {
                id: Some("C2"),
                phrase: "merge yours and theirs, theirs first (commits they dropped come back, \
                         unless only their rewrite held them)",
                keeps_work: "a merge discards nothing; never across lineages; `rwv abort` undoes \
                             it",
            },
            Self::C3 => &ChoiceRow {
                id: Some("C3"),
                phrase: "take theirs; yours is kept in a backup",
                keeps_work: "backup first",
            },
            Self::C5 => &ChoiceRow {
                id: Some("C5"),
                phrase: "record it: the lock, and `rwv.toml` as it stands",
                keeps_work: "records only",
            },
            Self::C6 => &ChoiceRow {
                id: Some("C6"),
                phrase: "align members for the checked-out branch",
                keeps_work: "moves only what alignment moves",
            },
            Self::C7 => &ChoiceRow {
                id: Some("C7"),
                phrase: "publish it",
                keeps_work: "adds only",
            },
            Self::C9 => &ChoiceRow {
                id: Some("C9"),
                phrase: "delete what is landed",
                keeps_work: "deletes only what is landed elsewhere; local only",
            },
            Self::C10 => &ChoiceRow {
                id: Some("C10"),
                phrase: "name it, or adopt the remote branch you name",
                keeps_work: "renames or links only",
            },
            Self::C11 => &ChoiceRow {
                id: Some("C11"),
                phrase: "declare a branch for it, or keep, restore or port the entry's value",
                keeps_work: "re-pins by name, backup kept",
            },
            Self::GiveUpUnlanded => &ChoiceRow {
                id: None,
                phrase: "give up what has not landed: delete the branch, a backup keeping what it \
                         held",
                keeps_work: "backup kept",
            },
            Self::RetireDeclared => &ChoiceRow {
                id: None,
                phrase: "give the line up unlanded: record it as a feature onto `<T>`, then \
                         delete it and its published copy, a backup keeping what it held, then publish the dropped record",
                keeps_work: "backup kept",
            },
            Self::C12 => &ChoiceRow {
                id: Some("C12"),
                phrase: "say how this branch is used: feature work onto `<T>`, or declared",
                keeps_work: "records only",
            },
            Self::C13 => &ChoiceRow {
                id: Some("C13"),
                phrase: "commit your edits, or set them aside",
                keeps_work: "both keep edits",
            },
            Self::CommitEdits => &ChoiceRow {
                id: None,
                phrase: "commit your edits: removing the clone removes anything set aside in it",
                keeps_work: "the commit keeps them; the removal refuses a commit no remote has",
            },
            Self::C14 => &ChoiceRow {
                id: Some("C14"),
                phrase: "keep this work on a branch, then return to the expected branch",
                keeps_work: "the branch holds the work before anything moves",
            },
            Self::C15 => &ChoiceRow {
                id: Some("C15"),
                phrase: "copy these commits onto `<b>`",
                keeps_work: "a copy adds only",
            },
            Self::C16 => &ChoiceRow {
                id: Some("C16"),
                phrase: "free the other checkout",
                keeps_work: "removal refuses over edits",
            },
            Self::StopTakingIn => &ChoiceRow {
                id: None,
                phrase: "stop `<b>` taking in from the branch of another name",
                keeps_work: "config only",
            },
            Self::SetPublishRemote => &ChoiceRow {
                id: None,
                phrase: "set the remote this repo publishes to",
                keeps_work: "config only",
            },
            Self::AddRemote => &ChoiceRow {
                id: None,
                phrase: "add a remote for `<url>`, under a name no remote has",
                keeps_work: "config only",
            },
            Self::C18 => &ChoiceRow {
                id: Some("C18"),
                phrase: "start `<m>`'s `<b>` at a commit you choose",
                keeps_work: "creates only",
            },
            Self::C19 => &ChoiceRow {
                id: Some("C19"),
                phrase: "wait until the PR lands or its branch is deleted, then rerun",
                keeps_work: "nothing changes",
            },
            Self::C22 => &ChoiceRow {
                id: Some("C22"),
                phrase: "squashed, rebase-merged or on another lineage: reset yours to the \
                         target's here",
                keeps_work: "backup kept; local only",
            },
            Self::C23 => &ChoiceRow {
                id: Some("C23"),
                phrase: "say how `<T>` accepts landings, `<h>` being `linear`, `merge` or `ff-only`",
                keeps_work: "records only",
            },
            Self::C24 => &ChoiceRow {
                id: Some("C24"),
                phrase: "look at it without a weave branch",
                keeps_work: "the left HEAD holds nothing unpreserved",
            },
            Self::C25 => &ChoiceRow {
                id: Some("C25"),
                phrase: "pin it at a commit you choose on its declared branch",
                keeps_work: "re-pins by name; backup for a detached HEAD",
            },
            Self::C26 => &ChoiceRow {
                id: Some("C26"),
                phrase: "merge `<pin>` in as a merge commit",
                keeps_work: "a merge adds only",
            },
            Self::C27 => &ChoiceRow {
                id: Some("C27"),
                phrase: "publish this commit under a branch name you choose",
                keeps_work: "creates or fast-forwards",
            },
            Self::C30 => &ChoiceRow {
                id: Some("C30"),
                phrase: "return the project repo to a branch, or start one carrying the edits",
                keeps_work: "moves only what alignment moves",
            },
            Self::C31 => &ChoiceRow {
                id: Some("C31"),
                phrase: "land these changes too (this changes the target's entries, or its \
                         top-level setting)",
                keeps_work: "only named entries change; the lineage rule still judges",
            },
            Self::C32 => &ChoiceRow {
                id: Some("C32"),
                phrase: "finish or abandon the operation in progress first",
                keeps_work: "rwv moves nothing until it ends",
            },
            Self::C33 => &ChoiceRow {
                id: Some("C33"),
                phrase: "look for what may be published but not yet fetched",
                keeps_work: "downloads only",
            },
            Self::C34 => &ChoiceRow {
                id: Some("C34"),
                phrase: "start from this clone's `<T>`",
                keeps_work: "starts only",
            },
            Self::C35 => &ChoiceRow {
                id: Some("C35"),
                phrase: "land `<dep>` into its target first (a landed one: retire it)",
                keeps_work: "a landing only fast-forwards",
            },
            Self::C36 => &ChoiceRow {
                id: Some("C36"),
                phrase: "take the target's new work in first",
                keeps_work: "by the target's setting; `rwv abort` undoes it",
            },
            Self::C37 => &ChoiceRow {
                id: Some("C37"),
                phrase: "port it in a feature onto this branch",
                keeps_work: "creates only",
            },
            Self::C38 => &ChoiceRow {
                id: Some("C38"),
                phrase: "pick it against the parent you name, or with no `-m` where it is no \
                         merge",
                keeps_work: "ports only",
            },
            Self::C39 => &ChoiceRow {
                id: Some("C39"),
                phrase: "undo these commits on `<b>` by new commits",
                keeps_work: "a revert adds only",
            },
            Self::C40 => &ChoiceRow {
                id: Some("C40"),
                phrase: "leave `<m>` out of the pick, recording it as it stands",
                keeps_work: "records its tip forward only",
            },
            Self::AnotherName => &ChoiceRow {
                id: None,
                phrase: "use another name",
                keeps_work: "nothing changes",
            },
            Self::OpenIt => &ChoiceRow {
                id: None,
                phrase: "open it",
                keeps_work: "moves only what alignment moves",
            },
            Self::DoctorFix => &ChoiceRow {
                id: None,
                phrase: "rewrite the keys an earlier release wrote, committing only that",
                keeps_work: "commits only the rewrite; other edits stay uncommitted",
            },
            Self::RecordCommitted => &ChoiceRow {
                id: None,
                phrase: "record the committed HEAD, leaving these uncommitted edits out of the \
                         lock",
                keeps_work: "the edits stay in the checkout, uncommitted",
            },
            Self::KeepRecordedHistory => &ChoiceRow {
                id: None,
                phrase: "integrate by the setting the branch records",
                keeps_work: "nothing changes",
            },
            Self::CarryAgain => &ChoiceRow {
                id: None,
                phrase: "carry each dependent again from where it now stands, its new commits \
                         with it",
                keeps_work: "the dependents are judged again before any moves; a replay that \
                             conflicts stops as the first did",
            },
            Self::LostRace => &ChoiceRow {
                id: None,
                phrase: "put back every ref the operation moved, then run it again from the new \
                         tips",
                keeps_work: "`rwv abort` moves only what the operation moved, from the tips it \
                             wrote; a branch someone else moved stays where they left it",
            },
            Self::LeaveTarget => &ChoiceRow {
                id: None,
                phrase: "end the landing, putting back the source and leaving the target as it \
                         stands, the landing's tips in it included",
                keeps_work: "each tip the landing wrote in the target is kept as a backup first \
                             where no branch reaches it",
            },
            Self::MarkResolved => &ChoiceRow {
                id: None,
                phrase: "mark each conflicted path resolved once it holds what you mean",
                keeps_work: "marks the working files resolved as they stand; the operation is still \
                             in progress",
            },
            Self::KeepSide => &ChoiceRow {
                id: None,
                phrase: "take one side for every `rwv.toml` entry both sides changed \
                         differently, and write the merge of both sides, marked resolved",
                keeps_work: "it writes only over `rwv.toml` as the stop left it; one edited \
                             since, or reading as another manifest, is refused, not replaced",
            },
            Self::ResumeOperation => &ChoiceRow {
                id: None,
                phrase: "let the operation that stopped go on, committing what you marked resolved",
                keeps_work: "it stops again while any path is conflicted",
            },
            Self::AbortOperation => &ChoiceRow {
                id: None,
                phrase: "abandon the operation that stopped",
                keeps_work: "`rwv abort` moves only what the operation moved, back to where it \
                             found it",
            },
            Self::StageParsingManifest => &ChoiceRow {
                id: None,
                phrase: "edit rwv.toml until it parses, then mark it resolved",
                keeps_work: "marks the working file resolved as it stands; the operation is still in \
                             progress",
            },
            Self::ResumeWithManifest => &ChoiceRow {
                id: None,
                phrase: "let the operation go on with the resolved rwv.toml",
                keeps_work: "a merge commits it in the merge; a replay makes it part of the \
                             replayed tip; it stops again while the resolved file does not parse",
            },
            Self::KeepTextMerge => &ChoiceRow {
                id: None,
                phrase: crate::manifest_conflict::KEEP_TEXT_MERGE,
                keeps_work: "writes nothing; the text merge is not judged again in this \
                             operation",
            },
            Self::StageOwnManifest => &ChoiceRow {
                id: None,
                phrase: crate::manifest_conflict::STAGE_OWN,
                keeps_work: "the resolved file is taken whole and not judged again",
            },
            Self::MergeEditedDrift => &ChoiceRow {
                id: None,
                phrase: "merge each listed file's edit onto the moved tip, refreshing the \
                         move's other files",
                keeps_work: "the working copy and the VCS's record of it are kept as a backup before anything \
                             is written; the VCS merges the edits as it merges local changes on \
                             a checkout, leaving a file it does not resolve conflicted",
            },
            Self::SetEditsAside => &ChoiceRow {
                id: None,
                phrase: "set the listed files' edits aside with the VCS, each taking the moved \
                         tip's content, refreshing the move's other files",
                keeps_work: "the VCS keeps the edits, each version it records included, against the tip \
                             the files held; the report prints the command that merges them \
                             back onto the moved tip",
            },
            Self::ResolveConflicts => &ChoiceRow {
                id: None,
                phrase: "resolve each conflicted file, then mark it resolved",
                keeps_work: "nothing is written but what you write",
            },
            Self::CompareMarked => &ChoiceRow {
                id: None,
                phrase: "clear the mark that tells the VCS to treat each file listed in the command \
                         as unchanged, so the VCS compares it and a backup keeps it, then merge or set aside \
                         again; the mark stays cleared",
                keeps_work: "nothing is written but the VCS's mark on each file",
            },
            Self::CompareSkipped => &ChoiceRow {
                id: None,
                phrase: "clear the mark that tells the VCS to treat each file listed in the command \
                         as outside the working copy, so the VCS compares it and a backup keeps it, then merge or set aside \
                         again; the mark stays cleared",
                keeps_work: "nothing is written but the VCS's mark on each file",
            },
            Self::ReplaceThenMerge => &ChoiceRow {
                id: None,
                phrase: "move out of the way what stands where the moved tip puts each listed \
                         path: a link or file where its directory goes, a directory or an \
                         untracked file, ignored or not, where its file goes; then merge each \
                         edit onto the moved tip",
                keeps_work: "the working copy and the VCS's record of it are kept as a backup before the \
                             merge writes anything",
            },
            Self::UnstageThenMerge => &ChoiceRow {
                id: None,
                phrase: "drop what the VCS records for each listed path, each staying in the \
                         working copy, then merge again",
                keeps_work: "the backup taken before the merge holds each recorded version; \
                             dropping the record writes nothing to the working copy",
            },
            Self::TakeBackSetAside => &ChoiceRow {
                id: None,
                phrase: "merge back the edits the newest set-aside kept, named last in the \
                         subject, once the checkout stands on its branch; any earlier one named \
                         there is taken back the same way, with its own id, once what the one \
                         taken back before it brought back is recorded",
                keeps_work: "the set-aside record stays until you drop it",
            },
            Self::FollowSubmodule => &ChoiceRow {
                id: None,
                phrase: "move each listed submodule to the commit its member records",
                keeps_work: "the commit it stood at stays in its store",
            },
            Self::FreeBranchThenMerge => &ChoiceRow {
                id: None,
                phrase: "check the branch it left, named in the subject, out of the other \
                         checkout the subject names, then merge again",
                keeps_work: "nothing here is written until the merge, which keeps a backup first",
            },
            Self::RecreateBranchThenMerge => &ChoiceRow {
                id: None,
                phrase: "create the branch it left, named in the subject, again at the tip it \
                         should hold, then merge again",
                keeps_work: "nothing here is written until the merge, which keeps a backup first",
            },
            Self::ClearRefusalThenMerge => &ChoiceRow {
                id: None,
                phrase: "clear what the VCS says stopped it, quoted in the subject, then merge \
                         again",
                keeps_work: "the working copy and the VCS's record of it are kept as a backup before the merge \
                             writes anything",
            },
            Self::StashUnreadDrift => &ChoiceRow {
                id: None,
                phrase: "set aside every difference from HEAD, the edits with the move's reverse, \
                         so the checkout stands at HEAD; take back only your own files from \
                         what was set aside afterwards",
                keeps_work: "what is set aside keeps every difference, edits included",
            },
            Self::UnmarkThenStashUnreadDrift => &ChoiceRow {
                id: None,
                phrase: "unmark the listed paths marked to be added with no content recorded, \
                         which leaves each an untracked file, then set aside every difference \
                         from HEAD, the edits with the move's reverse, so the checkout stands at \
                         HEAD; take back only your own files from what was set aside afterwards",
                keeps_work: "unmarking leaves each file on disk; what is set aside keeps every \
                             other difference, edits included",
            },
            Self::RemoveLockThenMerge => &ChoiceRow {
                id: None,
                phrase: "once no VCS process is running in the checkout, remove the lock on the VCS's record of the checkout \
                         a killed one left, named in the subject, then merge again",
                keeps_work: "the lock holds no work; the backup taken before the merge holds \
                             every edit",
            },
            Self::LandAgain => &ChoiceRow {
                id: None,
                phrase: "land again once nothing else is changing the checkout: the landing \
                         reads it afresh and refreshes what still holds the old tip",
                keeps_work: "a refresh writes only paths that still hold the old tip's content",
            },
            Self::RefreshAgain => &ChoiceRow {
                id: None,
                phrase: "refresh again once nothing else is changing the checkout: doctor \
                         reads it afresh and refreshes what still holds the old tip",
                keeps_work: "a refresh writes only paths that still hold the old tip's content",
            },
            Self::Republish => &ChoiceRow {
                id: None,
                phrase: "republish rwv's own rebase over the push branch it rewrote",
                keeps_work: "leased to the push branch's tip, which the `rebased` backup holds; \
                             the backup goes once the push lands",
            },
            Self::MigrateLockDriver => &ChoiceRow {
                id: None,
                phrase: "set up the lock merge driver: rewrite the attribute line, committing \
                         only that, and replace the config",
                keeps_work: "commits only the rewritten line; other edits stay uncommitted",
            },
        }
    }
}

/// Where a form runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Here,
    /// The workspace holding the judged checkout: here, or the one an `rwv`
    /// step names with `-C` where the checkout was judged in another.
    CheckoutsWorkspace,
    Any,
    Checkout,
    Primary,
    OtherCheckout,
    WithBranch,
    WhereBranchCheckedOut,
    BranchNotCheckedOut,
    OnTarget,
    WithFeature,
    DependentsWorkspace,
    Nowhere,
}

impl Where {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Here => "here",
            Self::CheckoutsWorkspace => "the checkout's workspace",
            Self::Any => "any workspace",
            Self::Checkout => "the checkout",
            Self::Primary => "the primary",
            Self::OtherCheckout => "the other checkout",
            Self::WithBranch => "a workspace with the branch checked out",
            Self::WhereBranchCheckedOut => "where `<b>` is checked out",
            Self::BranchNotCheckedOut => "a workspace where `<W>` is not checked out",
            Self::OnTarget => "a workspace on `<T>`",
            Self::WithFeature => "the workspace with that feature",
            Self::DependentsWorkspace => "the dependent's workspace, on its machine",
            Self::Nowhere => "nowhere",
        }
    }

    /// The directory a printed form names because it does not run here, or
    /// `None` where it runs here or locates itself.
    fn at(self, places: &BTreeMap<&'static str, PathBuf>) -> Option<PathBuf> {
        let placeholder = match self {
            Self::Primary => "<primary>",
            Self::WhereBranchCheckedOut => "<b-checkout>",
            _ => return None,
        };
        places.get(placeholder).cloned()
    }
}

/// Which kind of repo a form is spelled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Project,
    Member,
}

/// One step of a form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// An rwv command, `<placeholder>` words filled from what was judged.
    Rwv(&'static str),
    /// A VCS action, spelled by the backend.
    Vcs(VcsAction),
    /// Another form, run after the steps before it.
    Then(Form),
    /// A flag to rerun the refused verb with.
    Rerun(&'static str),
    /// A flag to rerun the refused verb without.
    RerunWithout(&'static str),
    /// The continue of an operation of this verb.
    Resume(crate::op_state::OpVerb),
    /// A line of the operation the op record names, built on `<resume>`, its
    /// continue, or `<abort>`, its abort, each spelled to run against the
    /// workspace it is recorded in.
    Recorded(&'static str),
}

/// One spelling of a choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Form {
    C1Pull,
    C1UpdateRemote,
    C1UpdateRemoteCommit,
    C2Pull,
    C2UpdateRemote,
    C3Pull,
    C3UpdateRemote,
    C3HeldByDeclared,
    C15ThenReset,
    C27ThenReset,
    C5Lock,
    C6,
    C6Activate,
    C7Push,
    C7PushRepo,
    C7PushBranch,
    C7PushTarget,
    C9BranchDelete,
    C9BranchDeleteDiscardUnlanded,
    C9WorkweaveDelete,
    C10,
    C10Adopt,
    C11SetBranch,
    C11SetBranchNew,
    C11SetUrl,
    C11SetRole,
    C11SetRecurseSubmodules,
    C11SetHistory,
    C11UnsetHistory,
    C11MemberAdd,
    C11MemberRemove,
    C11SetTopHistory,
    C11UnsetTopHistory,
    C12Feature,
    RetireDeclared,
    GiveUpUnlanded,
    C12Declared,
    C12RestoreTarget,
    C13,
    C13Untracked,
    C13Commit,
    C13CommitUntracked,
    C13WorkweaveDelete,
    C14Keep,
    C14Return,
    C15,
    C15AtCherryPick,
    C15OntoCheckedOut,
    C16MemberRemove,
    C16WorkweaveDelete,
    C16Raw,
    C16Detach,
    StopTakingIn,
    SetPublishRemote,
    AddRemote,
    C18,
    C19,
    C22,
    C23SetHistory,
    C23SetHistoryRepo,
    C23Rerun,
    C24,
    C25,
    C25AtCherryPick,
    C26,
    C26AtCherryPick,
    C27,
    C30Switch,
    C30BranchCreate,
    C31,
    C31Continue,
    C32,
    C33,
    C34,
    C35,
    C35Retire,
    C36,
    C36Dependent,
    C37BranchCreate,
    C37WorkweaveCreate,
    C38,
    C38NoMainline,
    C39,
    C40,
    AnotherName,
    OpenSwitch,
    OpenWorkweave,
    DoctorFix,
    DoctorFixOnBranch,
    LockDirty,
    UpdateRemoteDirty,
    KeepRecordedHistory,
    MigrateLockDriver,
    LostRace,
    LostRaceSync,
    LostRaceLanding,
    C14KeepThenSwitch,
    LeaveTarget,
    CarryAgain,
    CarryAgainLand,
    Republish,
    DriftLandAgain,
    DriftResolveThenLand,
    MarkResolved,
    KeepYours,
    KeepIncoming,
    ResumeOperation,
    AbortOperation,
    StageParsingManifest,
    ResumeWithManifest,
    KeepTextMerge,
    StageOwnManifest,
    DriftRefreshAgain,
    MergeEditedDrift,
    SetEditsAside,
    ResolveConflicts,
    ReplaceThenMerge,
    CompareMarked,
    CompareSkipped,
    UnstageThenMerge,
    RemoveLockThenMerge,
    StashUnreadDrift,
    UnmarkThenStashUnreadDrift,
    FreeBranchThenMerge,
    RecreateBranchThenMerge,
    ClearRefusalThenMerge,
    TakeBackSetAside,
    FollowSubmodule,
}

/// A form's choice, steps and Where, and whether its verb has shipped.
pub struct FormRow {
    pub choice: ChoiceId,
    pub steps: &'static [Step],
    pub at: Where,
    pub shipped: bool,
}

impl FormRow {
    /// Shipped, and every form it runs after prints by `printable`.
    fn printable_where(&self, printable: impl Fn(Form) -> bool) -> bool {
        self.shipped
            && self.steps.iter().all(|step| match step {
                Step::Then(next) => printable(*next),
                Step::Rwv(_)
                | Step::Vcs(_)
                | Step::Rerun(_)
                | Step::RerunWithout(_)
                | Step::Resume(_)
                | Step::Recorded(_) => true,
            })
    }
}

impl Form {
    pub const ALL: [Form; 128] = [
        Self::C1Pull,
        Self::C1UpdateRemote,
        Self::C1UpdateRemoteCommit,
        Self::C2Pull,
        Self::C2UpdateRemote,
        Self::C3Pull,
        Self::C3UpdateRemote,
        Self::C3HeldByDeclared,
        Self::C15ThenReset,
        Self::C27ThenReset,
        Self::C5Lock,
        Self::C6,
        Self::C6Activate,
        Self::C7Push,
        Self::C7PushRepo,
        Self::C7PushBranch,
        Self::C7PushTarget,
        Self::C9BranchDelete,
        Self::C9BranchDeleteDiscardUnlanded,
        Self::C9WorkweaveDelete,
        Self::C10,
        Self::C10Adopt,
        Self::C11SetBranch,
        Self::C11SetBranchNew,
        Self::C11SetUrl,
        Self::C11SetRole,
        Self::C11SetRecurseSubmodules,
        Self::C11SetHistory,
        Self::C11UnsetHistory,
        Self::C11MemberAdd,
        Self::C11MemberRemove,
        Self::C11SetTopHistory,
        Self::C11UnsetTopHistory,
        Self::C12Feature,
        Self::RetireDeclared,
        Self::GiveUpUnlanded,
        Self::C12Declared,
        Self::C12RestoreTarget,
        Self::C13,
        Self::C13Untracked,
        Self::C13Commit,
        Self::C13CommitUntracked,
        Self::C13WorkweaveDelete,
        Self::C14Keep,
        Self::C14Return,
        Self::C15,
        Self::C15AtCherryPick,
        Self::C15OntoCheckedOut,
        Self::C16MemberRemove,
        Self::C16WorkweaveDelete,
        Self::C16Raw,
        Self::C16Detach,
        Self::StopTakingIn,
        Self::SetPublishRemote,
        Self::AddRemote,
        Self::C18,
        Self::C19,
        Self::C22,
        Self::C23SetHistory,
        Self::C23SetHistoryRepo,
        Self::C23Rerun,
        Self::C24,
        Self::C25,
        Self::C25AtCherryPick,
        Self::C26,
        Self::C26AtCherryPick,
        Self::C27,
        Self::C30Switch,
        Self::C30BranchCreate,
        Self::C31,
        Self::C31Continue,
        Self::C32,
        Self::C33,
        Self::C34,
        Self::C35,
        Self::C35Retire,
        Self::C36,
        Self::C36Dependent,
        Self::C37BranchCreate,
        Self::C37WorkweaveCreate,
        Self::C38,
        Self::C38NoMainline,
        Self::C39,
        Self::C40,
        Self::AnotherName,
        Self::OpenSwitch,
        Self::OpenWorkweave,
        Self::DoctorFix,
        Self::DoctorFixOnBranch,
        Self::LockDirty,
        Self::UpdateRemoteDirty,
        Self::KeepRecordedHistory,
        Self::MigrateLockDriver,
        Self::LostRace,
        Self::LostRaceSync,
        Self::LostRaceLanding,
        Self::C14KeepThenSwitch,
        Self::LeaveTarget,
        Self::CarryAgain,
        Self::CarryAgainLand,
        Self::Republish,
        Self::DriftLandAgain,
        Self::DriftResolveThenLand,
        Self::MarkResolved,
        Self::KeepYours,
        Self::KeepIncoming,
        Self::ResumeOperation,
        Self::AbortOperation,
        Self::StageParsingManifest,
        Self::ResumeWithManifest,
        Self::KeepTextMerge,
        Self::StageOwnManifest,
        Self::DriftRefreshAgain,
        Self::MergeEditedDrift,
        Self::SetEditsAside,
        Self::ResolveConflicts,
        Self::ReplaceThenMerge,
        Self::CompareMarked,
        Self::CompareSkipped,
        Self::UnstageThenMerge,
        Self::RemoveLockThenMerge,
        Self::TakeBackSetAside,
        Self::FollowSubmodule,
        Self::StashUnreadDrift,
        Self::UnmarkThenStashUnreadDrift,
        Self::FreeBranchThenMerge,
        Self::RecreateBranchThenMerge,
        Self::ClearRefusalThenMerge,
    ];

    pub fn row(self) -> &'static FormRow {
        use ChoiceId as C;
        use Step::{Rerun, RerunWithout, Resume, Rwv, Then, Vcs};
        use VcsAction as A;
        use Where as W;
        match self {
            Self::C1Pull => &FormRow {
                choice: C::C1,
                steps: &[Rwv("rwv pull")],
                at: W::Any,
                shipped: true,
            },
            Self::C1UpdateRemote => &FormRow {
                choice: C::C1,
                steps: &[Rwv("rwv update --remote --repo <m>")],
                at: W::Any,
                shipped: true,
            },
            Self::C1UpdateRemoteCommit => &FormRow {
                choice: C::C1,
                steps: &[Rwv("rwv update --remote --repo <m> --commit")],
                at: W::Any,
                shipped: true,
            },
            Self::C2Pull => &FormRow {
                choice: C::C2,
                steps: &[Rwv("rwv pull --merge-upstream")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C2UpdateRemote => &FormRow {
                choice: C::C2,
                steps: &[Rwv(
                    "rwv update --remote --merge-upstream --repo <m> --commit",
                )],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C3Pull => &FormRow {
                choice: C::C3,
                steps: &[Rwv("rwv pull --reset-to-upstream")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C3UpdateRemote => &FormRow {
                choice: C::C3,
                steps: &[Rwv(
                    "rwv update --remote --reset-to-upstream <repo> --commit",
                )],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C3HeldByDeclared => &FormRow {
                choice: C::C3,
                steps: &[Then(Self::C3UpdateRemote)],
                at: W::Any,
                shipped: true,
            },
            Self::C15ThenReset => &FormRow {
                choice: C::C15,
                steps: &[
                    Vcs(A::CherryPickOntoCheckedOut),
                    Then(Self::C5Lock),
                    Then(Self::C3UpdateRemote),
                ],
                at: W::WhereBranchCheckedOut,
                shipped: true,
            },
            Self::C27ThenReset => &FormRow {
                choice: C::C27,
                steps: &[Vcs(A::PushCommitAs), Then(Self::C3UpdateRemote)],
                at: W::Any,
                shipped: true,
            },
            Self::C5Lock => &FormRow {
                choice: C::C5,
                steps: &[Rwv("rwv lock --commit")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C6 => &FormRow {
                choice: C::C6,
                steps: &[Rwv("rwv update")],
                at: W::Here,
                shipped: true,
            },
            Self::C6Activate => &FormRow {
                choice: C::C6,
                steps: &[Rwv("rwv activate <project> --update")],
                at: W::Here,
                shipped: true,
            },
            Self::C7Push => &FormRow {
                choice: C::C7,
                steps: &[Rwv("rwv push")],
                at: W::Here,
                shipped: true,
            },
            Self::C7PushRepo => &FormRow {
                choice: C::C7,
                steps: &[Rwv("rwv push --repo <m>")],
                at: W::Here,
                shipped: true,
            },
            Self::C7PushBranch => &FormRow {
                choice: C::C7,
                steps: &[Rwv("rwv push --branch <W>")],
                at: W::Any,
                shipped: true,
            },
            Self::C7PushTarget => &FormRow {
                choice: C::C7,
                steps: &[Rwv("rwv push --branch <target>")],
                at: W::Any,
                shipped: true,
            },
            Self::C9BranchDelete => &FormRow {
                choice: C::C9,
                steps: &[Rwv("rwv branch delete <W>")],
                at: W::BranchNotCheckedOut,
                shipped: true,
            },
            Self::C9BranchDeleteDiscardUnlanded => &FormRow {
                choice: C::C9,
                steps: &[Rwv("rwv branch delete <W> --discard-unlanded")],
                at: W::BranchNotCheckedOut,
                shipped: true,
            },
            Self::C9WorkweaveDelete => &FormRow {
                choice: C::C9,
                steps: &[Rwv("rwv workweave delete <ww> --project <project>")],
                at: W::Primary,
                shipped: true,
            },
            Self::C10 => &FormRow {
                choice: C::C10,
                steps: &[Rwv("rwv branch rename <new>")],
                at: W::Here,
                shipped: true,
            },
            Self::C10Adopt => &FormRow {
                choice: C::C10,
                steps: &[Rwv("rwv branch rename <new> --adopt")],
                at: W::Here,
                shipped: true,
            },
            Self::C11SetBranch => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member set-branch <m> <b>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11SetBranchNew => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member set-branch <m> <b> --new")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11SetUrl => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member set-url <m> <url>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11SetRole => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member set-role <m> <role>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11SetRecurseSubmodules => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member set-recurse-submodules <m> <bool>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11SetHistory => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv branch set-history <h> --repo <m>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11UnsetHistory => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv branch set-history --unset --repo <m>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11MemberAdd => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member add <url> --role <role>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11MemberRemove => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member remove <m>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11SetTopHistory => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv branch set-history <h>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11UnsetTopHistory => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv branch set-history --unset")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::GiveUpUnlanded => &FormRow {
                choice: C::GiveUpUnlanded,
                steps: &[Rwv("rwv branch delete <W> --target <T> --discard-unlanded")],
                at: W::Any,
                shipped: true,
            },
            Self::RetireDeclared => &FormRow {
                choice: C::RetireDeclared,
                steps: &[
                    Rwv("rwv branch set-mode <W> feature --target <T>"),
                    Rwv("rwv branch delete <W> --discard-unlanded --delete-remote-unlanded <repo>"),
                    Rwv("rwv push"),
                ],
                at: W::Any,
                shipped: true,
            },
            Self::C12Feature => &FormRow {
                choice: C::C12,
                steps: &[Rwv("rwv branch set-mode <W> feature --target <T>")],
                at: W::Any,
                shipped: true,
            },
            Self::C12Declared => &FormRow {
                choice: C::C12,
                steps: &[Rwv("rwv branch set-mode <W> declared")],
                at: W::Any,
                shipped: true,
            },
            Self::C12RestoreTarget => &FormRow {
                choice: C::C12,
                steps: &[Rwv("rwv branch set-mode <T> <as>")],
                at: W::Any,
                shipped: true,
            },
            Self::C13 => &FormRow {
                choice: C::C13,
                steps: &[Vcs(A::Commit), Vcs(A::Stash)],
                at: W::Checkout,
                shipped: true,
            },
            Self::C13Untracked => &FormRow {
                choice: C::C13,
                steps: &[Vcs(A::AddAll), Vcs(A::Commit), Vcs(A::StashUntracked)],
                at: W::Checkout,
                shipped: true,
            },
            Self::C13Commit => &FormRow {
                choice: C::CommitEdits,
                steps: &[Vcs(A::Commit)],
                at: W::Checkout,
                shipped: true,
            },
            Self::C13CommitUntracked => &FormRow {
                choice: C::CommitEdits,
                steps: &[Vcs(A::AddAll), Vcs(A::Commit)],
                at: W::Checkout,
                shipped: true,
            },
            Self::C13WorkweaveDelete => &FormRow {
                choice: C::C13,
                steps: &[Rwv("rwv workweave delete <ww> --discard-uncommitted")],
                at: W::Primary,
                shipped: true,
            },
            Self::C14Keep => &FormRow {
                choice: C::C14,
                steps: &[Vcs(A::BranchAt), Then(Self::C6)],
                at: W::Any,
                shipped: true,
            },
            Self::C14Return => &FormRow {
                choice: C::C14,
                steps: &[Vcs(A::SwitchTo)],
                at: W::Any,
                shipped: true,
            },
            Self::C15 => &FormRow {
                choice: C::C15,
                steps: &[Vcs(A::CherryPick), Then(Self::C5Lock)],
                at: W::WhereBranchCheckedOut,
                shipped: true,
            },
            Self::C15AtCherryPick => &FormRow {
                choice: C::C15,
                steps: &[Vcs(A::CherryPickMainline), Then(Self::C40)],
                at: W::WhereBranchCheckedOut,
                shipped: true,
            },
            Self::C15OntoCheckedOut => &FormRow {
                choice: C::C15,
                steps: &[Vcs(A::CherryPickOntoCheckedOut), Then(Self::C5Lock)],
                at: W::WhereBranchCheckedOut,
                shipped: true,
            },
            Self::C16MemberRemove => &FormRow {
                choice: C::C16,
                steps: &[Rwv("rwv member remove <repo> --delete")],
                at: W::OtherCheckout,
                shipped: true,
            },
            Self::C16WorkweaveDelete => &FormRow {
                choice: C::C16,
                steps: &[Rwv("rwv workweave delete <ww> --project <project>")],
                at: W::Primary,
                shipped: true,
            },
            Self::C16Raw => &FormRow {
                choice: C::C16,
                steps: &[Vcs(A::RemoveWorktree)],
                at: W::Checkout,
                shipped: true,
            },
            Self::C16Detach => &FormRow {
                choice: C::C16,
                steps: &[Rwv("rwv workweave create <new> --detach <W>")],
                at: W::Here,
                shipped: true,
            },
            Self::StopTakingIn => &FormRow {
                choice: C::StopTakingIn,
                steps: &[Vcs(A::UnsetUpstream)],
                at: W::Any,
                shipped: true,
            },
            Self::SetPublishRemote => &FormRow {
                choice: C::SetPublishRemote,
                steps: &[Vcs(A::SetPublishRemote)],
                at: W::Any,
                shipped: true,
            },
            Self::AddRemote => &FormRow {
                choice: C::AddRemote,
                steps: &[Vcs(A::AddRemote)],
                at: W::Any,
                shipped: true,
            },
            Self::C18 => &FormRow {
                choice: C::C18,
                steps: &[Vcs(A::BranchAt), Then(Self::C6)],
                at: W::Any,
                shipped: true,
            },
            Self::C19 => &FormRow {
                choice: C::C19,
                steps: &[],
                at: W::Nowhere,
                shipped: true,
            },
            Self::C22 => &FormRow {
                choice: C::C22,
                steps: &[Rwv("rwv sync --reset-to-target <m>")],
                at: W::WithFeature,
                shipped: true,
            },
            Self::C23SetHistory => &FormRow {
                choice: C::C23,
                steps: &[Rwv("rwv branch set-history <h>")],
                at: W::OnTarget,
                shipped: true,
            },
            Self::C23SetHistoryRepo => &FormRow {
                choice: C::C23,
                steps: &[Rwv("rwv branch set-history <h> --repo <m>")],
                at: W::OnTarget,
                shipped: true,
            },
            Self::C23Rerun => &FormRow {
                choice: C::C23,
                steps: &[Rerun("--history <h>")],
                at: W::Here,
                shipped: true,
            },
            Self::C24 => &FormRow {
                choice: C::C24,
                steps: &[Vcs(A::SwitchDetached)],
                at: W::Any,
                shipped: true,
            },
            Self::C25 => &FormRow {
                choice: C::C25,
                steps: &[Rwv("rwv member pin <m> <rev> --commit")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C25AtCherryPick => &FormRow {
                choice: C::C25,
                steps: &[Rwv("rwv member pin <m> <rev> --commit"), Then(Self::C40)],
                at: W::Here,
                shipped: true,
            },
            Self::C26 => &FormRow {
                choice: C::C26,
                steps: &[Vcs(A::MergeNoFastForward)],
                at: W::Checkout,
                shipped: true,
            },
            Self::C26AtCherryPick => &FormRow {
                choice: C::C26,
                steps: &[Vcs(A::MergeNoFastForward), Then(Self::C40)],
                at: W::Checkout,
                shipped: true,
            },
            Self::C27 => &FormRow {
                choice: C::C27,
                steps: &[Vcs(A::PushCommitAs)],
                at: W::Any,
                shipped: true,
            },
            Self::C30Switch => &FormRow {
                choice: C::C30,
                steps: &[Rwv("rwv switch <b>")],
                at: W::Here,
                shipped: true,
            },
            Self::C30BranchCreate => &FormRow {
                choice: C::C30,
                steps: &[
                    Rwv("rwv branch create <W> --target <T>"),
                    Rwv("rwv branch create <W> --declared"),
                ],
                at: W::Here,
                shipped: true,
            },
            Self::C31 => &FormRow {
                choice: C::C31,
                steps: &[Step::Recorded("<landing> --allow-member-change <changed>")],
                at: W::WithFeature,
                shipped: true,
            },
            Self::C31Continue => &FormRow {
                choice: C::C31,
                steps: &[Step::Recorded("<resume> --allow-member-change <m>")],
                at: W::WithFeature,
                shipped: true,
            },
            Self::C32 => &FormRow {
                choice: C::C32,
                steps: &[Vcs(A::Status)],
                at: W::Checkout,
                shipped: true,
            },
            Self::C33 => &FormRow {
                choice: C::C33,
                steps: &[Rwv("rwv fetch")],
                at: W::Any,
                shipped: true,
            },
            Self::C34 => &FormRow {
                choice: C::C34,
                steps: &[Rerun("--at <T>")],
                at: W::Here,
                shipped: true,
            },
            Self::C35 => &FormRow {
                choice: C::C35,
                steps: &[Rwv("rwv land")],
                at: W::DependentsWorkspace,
                shipped: true,
            },
            Self::C35Retire => &FormRow {
                choice: C::C35,
                steps: &[Rwv("rwv land --retire")],
                at: W::DependentsWorkspace,
                shipped: true,
            },
            Self::C36 => &FormRow {
                choice: C::C36,
                steps: &[Rwv("rwv sync")],
                at: W::WithFeature,
                shipped: true,
            },
            Self::C36Dependent => &FormRow {
                choice: C::C36,
                steps: &[Rwv("rwv sync")],
                at: W::DependentsWorkspace,
                shipped: true,
            },
            Self::C37BranchCreate => &FormRow {
                choice: C::C37,
                steps: &[Rwv("rwv branch create <W>")],
                at: W::Here,
                shipped: true,
            },
            Self::C37WorkweaveCreate => &FormRow {
                choice: C::C37,
                steps: &[Rwv("rwv workweave create <ww>")],
                at: W::Here,
                shipped: true,
            },
            Self::C38 => &FormRow {
                choice: C::C38,
                steps: &[Rwv("rwv cherry-pick <rev> -m <n>")],
                at: W::Here,
                shipped: true,
            },
            Self::C38NoMainline => &FormRow {
                choice: C::C38,
                steps: &[Rwv("rwv cherry-pick <rev>")],
                at: W::Here,
                shipped: true,
            },
            Self::C39 => &FormRow {
                choice: C::C39,
                steps: &[Vcs(A::Revert), Then(Self::C40)],
                at: W::WhereBranchCheckedOut,
                shipped: true,
            },
            Self::C40 => &FormRow {
                choice: C::C40,
                steps: &[Rerun("--except <m>")],
                at: W::Here,
                shipped: true,
            },
            Self::AnotherName => &FormRow {
                choice: C::AnotherName,
                steps: &[],
                at: W::Here,
                shipped: true,
            },
            Self::OpenSwitch => &FormRow {
                choice: C::OpenIt,
                steps: &[Rwv("rwv switch <W>")],
                at: W::Here,
                shipped: true,
            },
            Self::OpenWorkweave => &FormRow {
                choice: C::OpenIt,
                steps: &[Rwv("rwv workweave create <ww> --branch <W>")],
                at: W::Here,
                shipped: true,
            },
            Self::DoctorFix => &FormRow {
                choice: C::DoctorFix,
                steps: &[Rwv("rwv doctor --fix")],
                at: W::Here,
                shipped: true,
            },
            Self::DoctorFixOnBranch => &FormRow {
                choice: C::DoctorFix,
                steps: &[Rwv("rwv doctor --fix")],
                at: W::WhereBranchCheckedOut,
                shipped: true,
            },
            Self::LockDirty => &FormRow {
                choice: C::RecordCommitted,
                steps: &[Rwv("rwv lock --dirty")],
                at: W::Here,
                shipped: true,
            },
            Self::UpdateRemoteDirty => &FormRow {
                choice: C::RecordCommitted,
                steps: &[Rwv("rwv update --remote --dirty")],
                at: W::Here,
                shipped: true,
            },
            Self::KeepRecordedHistory => &FormRow {
                choice: C::KeepRecordedHistory,
                steps: &[RerunWithout("--history")],
                at: W::Here,
                shipped: true,
            },
            Self::MigrateLockDriver => &FormRow {
                choice: C::MigrateLockDriver,
                steps: &[Rwv("rwv doctor --fix")],
                at: W::Checkout,
                shipped: true,
            },
            Self::MergeEditedDrift => &FormRow {
                choice: C::MergeEditedDrift,
                steps: &[Rwv("rwv doctor --fix --merge-edited-drift")],
                at: W::CheckoutsWorkspace,
                shipped: true,
            },
            Self::SetEditsAside => &FormRow {
                choice: C::SetEditsAside,
                steps: &[Rwv("rwv doctor --fix --set-aside-edited-drift")],
                at: W::CheckoutsWorkspace,
                shipped: true,
            },
            Self::ResolveConflicts => &FormRow {
                choice: C::ResolveConflicts,
                steps: &[Vcs(A::MarkResolved)],
                at: W::Checkout,
                shipped: true,
            },
            Self::CompareMarked => &FormRow {
                choice: C::CompareMarked,
                steps: &[Vcs(A::CompareMarked)],
                at: W::Checkout,
                shipped: true,
            },
            Self::CompareSkipped => &FormRow {
                choice: C::CompareSkipped,
                steps: &[Vcs(A::CompareOutside)],
                at: W::Checkout,
                shipped: true,
            },
            Self::UnstageThenMerge => &FormRow {
                choice: C::UnstageThenMerge,
                steps: &[
                    Vcs(A::Unstage),
                    Rwv("rwv doctor --fix --merge-edited-drift"),
                ],
                at: W::CheckoutsWorkspace,
                shipped: true,
            },
            Self::TakeBackSetAside => &FormRow {
                choice: C::TakeBackSetAside,
                steps: &[Vcs(A::TakeBackSetAside)],
                at: W::Checkout,
                shipped: true,
            },
            Self::FollowSubmodule => &FormRow {
                choice: C::FollowSubmodule,
                steps: &[Vcs(A::FollowSubmodule)],
                at: W::Checkout,
                shipped: true,
            },
            Self::FreeBranchThenMerge => &FormRow {
                choice: C::FreeBranchThenMerge,
                steps: &[Rwv("rwv doctor --fix --merge-edited-drift")],
                at: W::CheckoutsWorkspace,
                shipped: true,
            },
            Self::RecreateBranchThenMerge => &FormRow {
                choice: C::RecreateBranchThenMerge,
                steps: &[Rwv("rwv doctor --fix --merge-edited-drift")],
                at: W::CheckoutsWorkspace,
                shipped: true,
            },
            Self::ClearRefusalThenMerge => &FormRow {
                choice: C::ClearRefusalThenMerge,
                steps: &[Rwv("rwv doctor --fix --merge-edited-drift")],
                at: W::CheckoutsWorkspace,
                shipped: true,
            },
            Self::StashUnreadDrift => &FormRow {
                choice: C::StashUnreadDrift,
                steps: &[Vcs(A::Stash)],
                at: W::Checkout,
                shipped: true,
            },
            Self::UnmarkThenStashUnreadDrift => &FormRow {
                choice: C::UnmarkThenStashUnreadDrift,
                steps: &[Vcs(A::Unstage), Vcs(A::Stash)],
                at: W::Checkout,
                shipped: true,
            },
            Self::RemoveLockThenMerge => &FormRow {
                choice: C::RemoveLockThenMerge,
                steps: &[Rwv("rwv doctor --fix --merge-edited-drift")],
                at: W::CheckoutsWorkspace,
                shipped: true,
            },
            Self::ReplaceThenMerge => &FormRow {
                choice: C::ReplaceThenMerge,
                steps: &[Rwv("rwv doctor --fix --merge-edited-drift")],
                at: W::CheckoutsWorkspace,
                shipped: true,
            },
            Self::DriftLandAgain => &FormRow {
                choice: C::LandAgain,
                steps: &[Rwv("rwv land")],
                at: W::Here,
                shipped: true,
            },
            Self::DriftResolveThenLand => &FormRow {
                choice: C::LandAgain,
                steps: &[Vcs(A::Status), Rwv("rwv land")],
                at: W::Here,
                shipped: true,
            },
            Self::MarkResolved => &FormRow {
                choice: C::MarkResolved,
                steps: &[Vcs(A::MarkResolved)],
                at: W::Checkout,
                shipped: true,
            },
            Self::KeepYours => &FormRow {
                choice: C::KeepSide,
                steps: &[Step::Recorded("<resume> --keep yours")],
                at: W::Here,
                shipped: true,
            },
            Self::KeepIncoming => &FormRow {
                choice: C::KeepSide,
                steps: &[Step::Recorded("<resume> --keep incoming")],
                at: W::Here,
                shipped: true,
            },
            Self::ResumeOperation => &FormRow {
                choice: C::ResumeOperation,
                steps: &[Step::Recorded("<resume>")],
                at: W::Here,
                shipped: true,
            },
            Self::AbortOperation => &FormRow {
                choice: C::AbortOperation,
                steps: &[Step::Recorded("<abort>")],
                at: W::Here,
                shipped: true,
            },
            Self::StageParsingManifest => &FormRow {
                choice: C::StageParsingManifest,
                steps: &[Vcs(A::MarkResolved)],
                at: W::Checkout,
                shipped: true,
            },
            Self::ResumeWithManifest => &FormRow {
                choice: C::ResumeWithManifest,
                steps: &[Step::Recorded("<resume>")],
                at: W::Here,
                shipped: true,
            },
            Self::KeepTextMerge => &FormRow {
                choice: C::KeepTextMerge,
                steps: &[Step::Recorded("<resume> --keep text-merge")],
                at: W::Here,
                shipped: true,
            },
            Self::StageOwnManifest => &FormRow {
                choice: C::StageOwnManifest,
                steps: &[Vcs(A::MarkResolved), Step::Recorded("<resume>")],
                at: W::Checkout,
                shipped: true,
            },
            Self::DriftRefreshAgain => &FormRow {
                choice: C::RefreshAgain,
                steps: &[Rwv("rwv doctor --fix")],
                at: W::CheckoutsWorkspace,
                shipped: true,
            },
            Self::LostRace => &FormRow {
                choice: C::LostRace,
                steps: &[Rwv("rwv abort"), Rwv("rwv land")],
                at: W::Here,
                shipped: true,
            },
            Self::CarryAgain => &FormRow {
                choice: C::CarryAgain,
                steps: &[Resume(crate::op_state::OpVerb::Sync)],
                at: W::Here,
                shipped: true,
            },
            Self::CarryAgainLand => &FormRow {
                choice: C::CarryAgain,
                steps: &[Resume(crate::op_state::OpVerb::Land)],
                at: W::Here,
                shipped: true,
            },
            Self::LeaveTarget => &FormRow {
                choice: C::LeaveTarget,
                steps: &[Rwv("rwv abort --leave-target")],
                at: W::Here,
                shipped: true,
            },
            Self::LostRaceSync => &FormRow {
                choice: C::LostRace,
                steps: &[Rwv("rwv abort"), Rwv("rwv sync")],
                at: W::Here,
                shipped: true,
            },
            Self::C14KeepThenSwitch => &FormRow {
                choice: C::C14,
                steps: &[Vcs(A::BranchAt), Then(Self::C30Switch)],
                at: W::Here,
                shipped: true,
            },
            Self::LostRaceLanding => &FormRow {
                choice: C::LostRace,
                steps: &[Rwv("rwv abort"), Step::Recorded("<landing>")],
                at: W::Here,
                shipped: true,
            },
            Self::Republish => &FormRow {
                choice: C::Republish,
                steps: &[Rwv("rwv push --overwrite <repo>")],
                at: W::Here,
                shipped: true,
            },
        }
    }

    /// Whether the form belongs to the project repo or to a member, where it
    /// is spelled for one of them alone.
    fn scope(self) -> Option<Scope> {
        match self {
            Self::C1Pull | Self::C2Pull | Self::C3Pull | Self::C7Push | Self::C10Adopt => {
                Some(Scope::Project)
            }
            Self::C1UpdateRemote
            | Self::C1UpdateRemoteCommit
            | Self::C2UpdateRemote
            | Self::C3UpdateRemote
            | Self::C3HeldByDeclared
            | Self::C15ThenReset
            | Self::C27ThenReset
            | Self::C7PushRepo
            | Self::C11SetBranchNew
            | Self::C18
            | Self::LockDirty
            | Self::UpdateRemoteDirty => Some(Scope::Member),
            _ => None,
        }
    }

    /// Whether this form may be printed: its verb has shipped, and so has
    /// every form it runs after.
    pub fn printable(self) -> bool {
        self.row().printable_where(Form::printable)
    }

    /// Every rwv command this form prints, `then` forms included, with its
    /// placeholders unfilled.
    pub fn rwv_commands(self) -> Vec<&'static str> {
        self.row()
            .steps
            .iter()
            .flat_map(|step| match step {
                Step::Rwv(command) => vec![*command],
                Step::Then(next) => next.rwv_commands(),
                Step::Vcs(_)
                | Step::Rerun(_)
                | Step::RerunWithout(_)
                | Step::Resume(_)
                | Step::Recorded(_) => Vec::new(),
            })
            .collect()
    }

    /// The form a refusal judged for `verb` prints where the table lists
    /// this one: the way back to alignment that a switch of project offers is
    /// not the one a run in place offers.
    fn for_verb(self, verb: Option<&str>) -> Self {
        match (self, verb) {
            (Self::C6, Some(ACTIVATE)) => Self::C6Activate,
            (Self::LostRace, Some("sync")) => Self::LostRaceSync,
            (Self::CarryAgain, Some("land")) => Self::CarryAgainLand,
            _ => self,
        }
    }

    /// The form a refusal of a removal that takes the repo's only store
    /// prints where the table lists this one: what is set aside there goes
    /// with the store.
    fn for_store_removal(self) -> Self {
        match self {
            Self::C13 => Self::C13Commit,
            Self::C13Untracked => Self::C13CommitUntracked,
            _ => self,
        }
    }

    /// What the route this form spells keeps, where its choice's phrase
    /// alone does not say; written to follow the phrase.
    fn because(self) -> Option<&'static str> {
        match self {
            Self::C3HeldByDeclared => Some("; `<b>` already holds each commit only yours has"),
            Self::C15ThenReset => Some(
                ", then take theirs: `<b>` keeps a recorded copy of each, the originals only a \
                 backup",
            ),
            Self::C27ThenReset => Some(
                ", then take theirs: the remote keeps each original under that name, so every \
                 project commit being pushed stays checkoutable",
            ),
            _ => None,
        }
    }

    fn phrase(self, bindings: &BTreeMap<&'static str, String>) -> String {
        let phrase = self.row().choice.row().phrase;
        fill(
            &format!("{phrase}{}", self.because().unwrap_or_default()),
            bindings,
        )
    }

    fn lines(self, vcs: &dyn Vcs, verb: Option<&str>) -> Vec<String> {
        self.row()
            .steps
            .iter()
            .flat_map(|step| match step {
                Step::Rwv(command) => vec![(*command).to_owned()],
                Step::Vcs(action) => vec![vcs.hint(*action).to_owned()],
                Step::Then(next) => next.for_verb(verb).lines(vcs, verb),
                Step::Rerun(flag) => vec![format!("rerun with {flag}")],
                Step::RerunWithout(flag) => vec![format!("rerun without {flag}")],
                Step::Resume(verb) => vec![crate::op_state::resume_command(*verb)],
                Step::Recorded(line) => vec![(*line).to_owned()],
            })
            .collect()
    }
}

/// A fact about a judged repo. Facts are reported; they never select a
/// choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FactKind {
    /// A superseded tip was recorded for the branch.
    Rewritten,
    /// The branch is an ancestor of where it lands.
    Landed,
    /// Another project's live lock pins the commits.
    PinnedBy,
    /// Other live weave branches declare the branch.
    Shared,
    /// The pin is on the branch it lands into.
    PinLanded,
    /// The member was left detached because its branch is checked out
    /// elsewhere.
    Checkout,
    /// The unlanded commits carry changes equivalent to commits where they
    /// land.
    PatchEquivalent,
    /// How many commits the branch holds that its push branch lacks.
    Ahead,
    /// How many commits the push branch holds that the branch lacks.
    Behind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub kind: FactKind,
    pub about: Option<String>,
}

/// A fact as a line prints it: its token, then what it is about.
pub fn fact_text(fact: &Fact) -> String {
    let token = crate::wire_token::wire_token(&fact.kind);
    match &fact.about {
        Some(about) => format!("{token}: {about}"),
        None => token,
    }
}

/// A repo being judged: its state and what the state names, and the values
/// its choices' placeholders take.
#[must_use]
pub struct Judging {
    repo: String,
    scope: Option<Scope>,
    state: State,
    subject: Option<String>,
    listed: Vec<String>,
    facts: Vec<Fact>,
    workspace: Option<PathBuf>,
    dependents_workspace: Option<PathBuf>,
    target_workspace: Option<PathBuf>,
    untracked: bool,
    removes_store: bool,
    without_at: Vec<Where>,
    verb: Option<&'static str>,
    only: Option<Vec<ChoiceId>>,
    only_forms: Option<Vec<Form>>,
    without: Vec<ChoiceId>,
    bindings: BTreeMap<&'static str, String>,
    places: BTreeMap<&'static str, PathBuf>,
}

impl Judging {
    /// The state being judged.
    pub fn judged_state(&self) -> State {
        self.state
    }

    /// What the state names: the operation in progress, the name taken.
    pub fn subject(mut self, subject: impl fmt::Display) -> Self {
        self.subject = Some(subject.to_string());
        self
    }

    /// The repo is the project repo: the forms spelled for a member alone are
    /// left out.
    pub fn project_repo(mut self) -> Self {
        self.scope = Some(Scope::Project);
        self
    }

    /// The repo is a member: the forms spelled for the project repo alone are
    /// left out.
    pub fn member(mut self) -> Self {
        self.scope = Some(Scope::Member);
        self
    }

    /// The verb refusing, so the state's forms that belong to that verb alone
    /// are offered too.
    pub fn by_verb(mut self, verb: &'static str) -> Self {
        self.verb = Some(verb);
        self
    }

    /// Print only the forms of these choices. The state's row says what it can
    /// offer; this narrows it where the judging knows which of them hold.
    pub fn only(mut self, choices: &[ChoiceId]) -> Self {
        self.only = Some(choices.to_vec());
        self
    }

    /// Leave out the forms of these choices, where the judging knows they
    /// cannot run: the state's row says what it can offer.
    /// `rwv doctor --fix` only where `text`, the `rwv.toml` that was read,
    /// names member-entry keys an earlier release wrote, the one thing it
    /// rewrites; otherwise a line says a commit on `branch` fixes it.
    pub fn doctor_fix_for(self, text: Option<&str>, branch: impl fmt::Display) -> Self {
        let rewrites = text.is_some_and(|text| {
            matches!(crate::manifest::rewrite_pre_schema_keys(text), Ok(Some(_)))
        });
        if rewrites {
            return self;
        }
        self.listing([format!(
            "a commit on {branch} that makes its rwv.toml parse fixes it"
        )])
        .without(&[ChoiceId::DoctorFix])
    }

    /// The refusal stands in front of removing the repo's only store, so no
    /// choice keeps work by setting it aside there.
    pub fn removing_store(mut self) -> Self {
        self.removes_store = true;
        self
    }

    pub fn without(mut self, choices: &[ChoiceId]) -> Self {
        self.without.extend_from_slice(choices);
        self
    }

    /// Print only these forms, where a choice's spellings differ in which of
    /// them holds.
    pub fn only_forms(mut self, forms: &[Form]) -> Self {
        self.only_forms = Some(forms.to_vec());
        self
    }

    /// The files, paths or commits the state lists beneath its line.
    pub fn listing<I>(mut self, items: I) -> Self
    where
        I: IntoIterator,
        I::Item: fmt::Display,
    {
        self.listed.extend(items.into_iter().map(|i| i.to_string()));
        self
    }

    pub fn fact(mut self, fact: Fact) -> Self {
        self.facts.push(fact);
        self
    }

    /// The value `placeholder` takes in this repo's choices.
    pub fn bind(mut self, placeholder: &'static str, value: impl fmt::Display) -> Self {
        self.bindings.insert(placeholder, value.to_string());
        self
    }

    /// Several values for `placeholder`: the flag the template writes
    /// before it is repeated, once with each.
    pub fn bind_each<V: fmt::Display>(
        mut self,
        placeholder: &'static str,
        values: impl IntoIterator<Item = V>,
    ) -> Self {
        let joined = values
            .into_iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(EACH);
        self.bindings.insert(placeholder, joined);
        self
    }

    /// Several values for `placeholder`, each its own argument where it
    /// stands.
    pub fn bind_list<V: fmt::Display>(
        mut self,
        placeholder: &'static str,
        values: impl IntoIterator<Item = V>,
    ) -> Self {
        let joined = values
            .into_iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(LIST);
        self.bindings.insert(placeholder, joined);
        self
    }

    pub fn checkout(self, dir: &Path) -> Self {
        self.bind("<checkout>", dir.display())
    }

    /// The directory `placeholder` names, where a choice that runs there is
    /// printed as running.
    pub fn bind_place(mut self, placeholder: &'static str, dir: &Path) -> Self {
        self.places.insert(placeholder, dir.to_path_buf());
        self.bind(placeholder, dir.display())
    }

    /// The workspace on the branch `<T>` names, where the choices that belong
    /// in one run. Where none has it checked out they are not offered.
    pub fn on_target(mut self, vcs: &dyn Vcs) -> Self {
        let held = self
            .bindings
            .get("<checkout>")
            .zip(self.bindings.get("<T>"))
            .and_then(|(dir, target)| {
                vcs.checkouts_of_branch(Path::new(dir), &crate::vcs::RawRefName::new(target))
                    .ok()
            })
            .and_then(|held| held.into_iter().next())
            .and_then(|checkout| {
                crate::workspace::WorkspaceContext::resolve_unbound(&checkout)
                    .ok()
                    .map(|ctx| ctx.active_path().to_path_buf())
            });
        match held {
            Some(workspace) => self.target_workspace = Some(workspace),
            None => self.without_at.push(Where::OnTarget),
        }
        self
    }

    /// The workspace holding the dependent the judged line names, where its
    /// choices run.
    pub fn dependents_at(mut self, workspace: &Path) -> Self {
        self.dependents_workspace = Some(workspace.to_path_buf());
        self
    }

    /// A branch whose record lands into the judged one: its bindings, and the
    /// workspace holding it where its choices run. One checked out nowhere has
    /// no workspace for them to run in, so they are left out.
    pub fn dependent(
        mut self,
        vcs: &dyn Vcs,
        project_dir: &Path,
        dependent: &str,
        target: Option<&str>,
    ) -> Self {
        self = self.bind("<dep>", dependent).bind("<W>", dependent);
        match target {
            Some(target) => self = self.bind("<T>", target),
            None => self.without.push(ChoiceId::C12),
        }
        let workspace = vcs
            .checkouts_of_branch(project_dir, &crate::vcs::RawRefName::new(dependent))
            .ok()
            .and_then(|held| held.into_iter().next())
            .and_then(|checkout| Some(checkout.parent()?.parent()?.to_path_buf()));
        match workspace {
            Some(workspace) => self.dependents_workspace = Some(workspace),
            None => self.without.push(ChoiceId::C35),
        }
        self
    }

    /// The repo was judged in `workspace`, not the one the verb runs in.
    pub fn elsewhere(mut self, workspace: &Path) -> Self {
        self.workspace = Some(workspace.to_path_buf());
        self
    }

    /// Name `primary` as where its choices run, unless that is `here`, where
    /// the verb runs.
    pub fn in_primary(self, primary: &Path, here: &Path) -> Self {
        if crate::path_spelling::CanonicalPath::of(primary)
            == crate::path_spelling::CanonicalPath::of(here)
        {
            self
        } else {
            self.bind_place("<primary>", primary)
        }
    }

    /// The workspace a form that runs `at` is spelled to run in, where the
    /// judgment knows one and it is not the workspace the verb runs in.
    fn addressed_to(&self, form: Form) -> Option<&Path> {
        if form.row().choice == ChoiceId::C12 {
            return self.dependents_workspace.as_deref();
        }
        match form.row().at {
            Where::OtherCheckout => self.places.get("<other>").map(PathBuf::as_path),
            Where::Here | Where::CheckoutsWorkspace | Where::WithBranch => {
                self.workspace.as_deref()
            }
            Where::OnTarget => self
                .target_workspace
                .as_deref()
                .or(self.workspace.as_deref()),
            Where::DependentsWorkspace => self.dependents_workspace.as_deref(),
            _ => None,
        }
    }

    /// The judged line, its choices spelled by `vcs`.
    pub fn judge(self, vcs: &dyn Vcs) -> Judged {
        let row = self.state.row();
        let verb_offers = row
            .verb_offers
            .iter()
            .filter(|(verb, _)| Some(*verb) == self.verb)
            .map(|(_, form)| form);
        let mut forms: Vec<Form> = Vec::new();
        for form in row
            .offers
            .iter()
            .map(|form| form.for_verb(self.verb))
            .chain(verb_offers.copied())
        {
            let form = match (self.untracked, form) {
                (true, Form::C13) => Form::C13Untracked,
                _ => form,
            };
            let form = match self.removes_store {
                true => form.for_store_removal(),
                false => form,
            };
            if !forms.contains(&form) {
                forms.push(form);
            }
        }
        let choices = forms
            .iter()
            .filter(|form| form.printable())
            .filter(|form| {
                self.only
                    .as_ref()
                    .is_none_or(|only| only.contains(&form.row().choice))
            })
            .filter(|form| !self.without.contains(&form.row().choice))
            .filter(|form| !self.without_at.contains(&form.row().at))
            .filter(|form| {
                let phrase = form.phrase(&self.bindings);
                !JUDGED_PLACEHOLDERS.iter().any(|p| phrase.contains(p))
            })
            .filter(|form| {
                form.rwv_commands().iter().all(|command| {
                    JUDGED_PLACEHOLDERS.iter().all(|placeholder| {
                        !command.contains(placeholder) || self.bindings.contains_key(placeholder)
                    })
                })
            })
            .filter(|form| {
                self.only_forms
                    .as_ref()
                    .is_none_or(|only| only.iter().any(|f| f.for_verb(self.verb) == **form))
            })
            .filter(|form| match (self.scope, form.scope()) {
                (Some(want), Some(have)) => want == have,
                _ => true,
            })
            .map(|form| Offered {
                choice: form.row().choice,
                phrase: form.phrase(&self.bindings),
                at: form.row().at.at(&self.places),
                run: form
                    .lines(vcs, self.verb)
                    .iter()
                    .map(|line| for_spelled_project(fill(line, &self.bindings)))
                    .map(|line| match self.addressed_to(*form) {
                        Some(workspace) => in_workspace(&line, workspace),
                        None => line,
                    })
                    .collect(),
            })
            .collect();
        Judged {
            repo: self.repo,
            workspace: self.workspace,
            state: self.state,
            subject: self.subject,
            listed: self.listed,
            facts: self.facts,
            choices,
        }
    }
}

/// `line`, an `rwv` command, spelled to run in `workspace` from anywhere.
fn in_workspace(line: &str, workspace: &Path) -> String {
    match line.strip_prefix("rwv ") {
        Some(rest) => {
            let shown = workspace.display().to_string();
            let quoted = match shown.contains(char::is_whitespace) {
                true => format!("\"{shown}\""),
                false => shown,
            };
            format!("rwv -C {quoted} {rest}")
        }
        None => line.to_owned(),
    }
}

/// The placeholders only a judgment can fill, from the repo, branch or
/// workspace it judged: a form that spells one the judgment did not bind is
/// not offered, where the operator-chosen ones (`<T>`, `<rev>`, `<h>`) print
/// for the operator to fill.
const JUDGED_PLACEHOLDERS: &[&str] = &["<m>", "<target>", "<dep>", "<repo>"];

/// What separates the values [`Judging::bind_each`] binds.
const EACH: &str = "\u{1f}";

/// What separates the values [`Judging::bind_list`] binds.
const LIST: &str = "\u{1e}";

/// `template` with every bound `<placeholder>` replaced by its value, quoted
/// where it holds whitespace. An unbound one stays for the operator to fill.
fn fill(template: &str, bindings: &BTreeMap<&'static str, String>) -> String {
    let quoted = |value: &str| {
        if value.contains(char::is_whitespace) {
            format!("\"{value}\"")
        } else {
            value.to_owned()
        }
    };
    bindings
        .iter()
        .fold(template.to_owned(), |text, (placeholder, value)| {
            if value.contains(LIST) {
                let words: Vec<String> = value.split(LIST).map(quoted).collect();
                return text.replace(placeholder, &words.join(" "));
            }
            if !value.contains(EACH) {
                return text.replace(placeholder, &quoted(value));
            }
            let Some(at) = text.find(placeholder) else {
                return text;
            };
            let flag_at = text[..at].trim_end().rfind(' ').map_or(0, |i| i + 1);
            let flag = text[flag_at..at].trim_end();
            let repeated = value
                .split(EACH)
                .map(|v| format!("{flag} {}", quoted(v)))
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "{}{repeated}{}",
                &text[..flag_at],
                &text[at + placeholder.len()..]
            )
        })
}

/// `line`, an `rwv` command, with `--project <project>` added where its verb
/// takes the flag and the line does not give it.
/// `line`, spelled for the project this process spells its choices for.
fn for_spelled_project(line: String) -> String {
    crate::refusal::choices_spelled_for()
        .and_then(|project| spelled_for_project(&line, project))
        .unwrap_or(line)
}

fn spelled_for_project(line: &str, project: &crate::manifest::ProjectName) -> Option<String> {
    use clap::CommandFactory;
    let words = line.strip_prefix("rwv ")?;
    if line.split_whitespace().any(|w| w == "--project") {
        return None;
    }
    let mut command = crate::cli::Cli::command();
    for word in words.split_whitespace() {
        match command.find_subcommand(word) {
            Some(sub) => command = sub.clone(),
            None => break,
        }
    }
    let takes_project = command
        .get_arguments()
        .any(|arg| arg.get_long() == Some("project"));
    takes_project.then(|| format!("{line} --project {project}"))
}

/// One repo judged in one state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judged {
    repo: String,
    workspace: Option<PathBuf>,
    state: State,
    subject: Option<String>,
    listed: Vec<String>,
    facts: Vec<Fact>,
    choices: Vec<Offered>,
}

/// A choice as one judged repo prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Offered {
    choice: ChoiceId,
    phrase: String,
    at: Option<PathBuf>,
    run: Vec<String>,
}

impl Judged {
    /// The refusal of this one line.
    pub fn refused(self) -> anyhow::Error {
        let mut report = Report::default();
        report.add(self);
        anyhow::Error::new(Refused(report))
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Each `rwv` choice spelled for `project`, so it runs as printed in a
    /// workspace that presents another: a verb taking `--project` and not
    /// given one is given it.
    pub fn for_project(mut self, project: &crate::manifest::ProjectName) -> Self {
        for offered in &mut self.choices {
            for line in &mut offered.run {
                if let Some(spelled) = spelled_for_project(line, project) {
                    *line = spelled;
                }
            }
        }
        self
    }

    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    pub fn listed(&self) -> &[String] {
        &self.listed
    }

    pub fn state(&self) -> State {
        self.state
    }

    fn headline(&self) -> String {
        let mut line = self.repo.clone();
        if let Some(workspace) = &self.workspace {
            line.push_str(&format!(" in {}", workspace.display()));
        }
        line.push_str(&format!(": {}", self.state_line()));
        line
    }

    /// The axis, the state and what it names, as the line prints them after
    /// the repo: `name no-such-branch (feat-x)`.
    pub fn state_line(&self) -> String {
        let mut line = format!("{} {}", self.state.axis().token(), self.state.name());
        if let Some(subject) = &self.subject {
            line.push_str(&format!(" ({subject})"));
        }
        line
    }

    fn details(&self) -> Vec<String> {
        let facts = self.facts.iter().map(fact_text);
        facts.chain(self.listed.iter().cloned()).collect()
    }

    pub fn output(&self) -> JudgedOutput {
        JudgedOutput {
            path: self.repo.clone(),
            workspace: self
                .workspace
                .as_deref()
                .map(crate::path_spelling::WirePath::of),
            axis: self.state.axis(),
            state: self.state.name().to_owned(),
            subject: self.subject.clone(),
            listed: self.listed.clone(),
            facts: self
                .facts
                .iter()
                .map(|fact| FactOutput {
                    fact: fact.kind,
                    about: fact.about.clone(),
                })
                .collect(),
            choices: self
                .choices
                .iter()
                .map(|offered| {
                    let row = offered.choice.row();
                    ChoiceOutput {
                        id: row.id.map(str::to_owned),
                        phrase: offered.phrase.clone(),
                        at: offered
                            .at
                            .as_deref()
                            .map(crate::path_spelling::WirePath::of),
                        run: offered.run.clone(),
                    }
                })
                .collect(),
        }
    }
}

/// Every repo a verb judged before refusing, with any notes the refusal adds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    lines: Vec<Judged>,
    notes: Vec<String>,
}

impl Report {
    /// Add `judged`, unless the report already holds that same line. A
    /// checkout stopped in a VCS operation is judged on that alone: where its
    /// HEAD stands and what it holds are the operation's until it ends, so its
    /// other `alignment`, `edits` and `pin` lines give way.
    pub fn add(&mut self, judged: Judged) {
        let held = |line: &Judged| {
            line.repo == judged.repo
                && line.workspace == judged.workspace
                && matches!(line.state.axis(), Axis::Alignment | Axis::Edits | Axis::Pin)
        };
        if judged.state == State::EditsMidOperation {
            self.lines.retain(|line| !held(line));
        } else if held(&judged)
            && self
                .lines
                .iter()
                .any(|line| line.state == State::EditsMidOperation && held(line))
        {
            return;
        }
        if !self.lines.contains(&judged) {
            self.lines.push(judged);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Every line as a fact alone: where a stopped operation holds the
    /// workspace, its own resume and abort are the only way out, and no
    /// choice a line offers can run.
    pub fn without_choices(mut self) -> Self {
        for line in &mut self.lines {
            line.choices.clear();
        }
        self
    }

    /// Add `judged` when there is one.
    /// Every line's `rwv` choices spelled for `project`
    /// ([`Judged::for_project`]).
    pub fn for_project(self, project: &crate::manifest::ProjectName) -> Self {
        Self {
            lines: self
                .lines
                .into_iter()
                .map(|line| line.for_project(project))
                .collect(),
            notes: self.notes,
        }
    }

    pub fn extend(&mut self, judged: Option<Judged>) {
        if let Some(judged) = judged {
            self.add(judged);
        }
    }

    pub fn lines(&self) -> &[Judged] {
        &self.lines
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Print `note` after the judged lines.
    pub fn note(&mut self, note: impl fmt::Display) {
        self.notes.push(note.to_string());
    }

    /// Take `result`'s judged lines into this report, so a judged refusal
    /// is reported with the rest; any other error comes back as it was.
    pub fn absorb(&mut self, result: anyhow::Result<()>) -> anyhow::Result<()> {
        let Err(err) = result else {
            return Ok(());
        };
        match err.downcast::<Refused>() {
            Ok(Refused(report)) => {
                for judged in report.lines {
                    self.add(judged);
                }
                self.notes.extend(report.notes);
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    /// `Ok` when nothing was judged, else the refusal carrying every line.
    pub fn into_result(self) -> anyhow::Result<()> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(anyhow::Error::new(Refused(self)))
        }
    }

    /// The distinct axes judged, in the order their first line prints.
    pub fn kinds(&self) -> Vec<Axis> {
        let mut kinds: Vec<Axis> = Vec::new();
        for judged in self.ordered() {
            let axis = judged.state.axis();
            if !kinds.contains(&axis) {
                kinds.push(axis);
            }
        }
        kinds
    }

    /// Lines grouped by repo in the order repos were judged, each repo's
    /// lines in axis order.
    fn ordered(&self) -> Vec<&Judged> {
        let mut repos: Vec<&str> = Vec::new();
        for judged in &self.lines {
            if !repos.contains(&judged.repo.as_str()) {
                repos.push(&judged.repo);
            }
        }
        let mut out: Vec<&Judged> = Vec::new();
        for repo in repos {
            let mut of_repo: Vec<&Judged> = self.lines.iter().filter(|j| j.repo == repo).collect();
            of_repo.sort_by_key(|j| j.state.axis());
            out.extend(of_repo);
        }
        out
    }

    /// The choices of every line, phrase and place once each, their lines in
    /// the order the judged lines offered them.
    fn choices(&self) -> Vec<(&str, Option<PathBuf>, Vec<String>)> {
        let mut out: Vec<(&str, Option<PathBuf>, Vec<String>)> = Vec::new();
        for judged in self.ordered() {
            for offered in &judged.choices {
                let phrase = offered.phrase.as_str();
                let at = offered.at.clone();
                let slot = match out.iter().position(|(p, a, _)| *p == phrase && *a == at) {
                    Some(i) => i,
                    None => {
                        out.push((phrase, at, Vec::new()));
                        out.len() - 1
                    }
                };
                for line in &offered.run {
                    if !out[slot].2.contains(line) {
                        out[slot].2.push(line.clone());
                    }
                }
            }
        }
        out
    }

    fn body(&self, prefix: &str) -> String {
        self.body_listing(prefix, true)
    }

    fn body_listing(&self, prefix: &str, list_choices: bool) -> String {
        let mut out = String::new();
        for judged in self.ordered() {
            out.push_str(&format!("{prefix}{}\n", judged.headline()));
            for detail in judged.details() {
                out.push_str(&format!("    {detail}\n"));
            }
        }
        for note in &self.notes {
            out.push_str(&format!("{note}\n"));
        }
        let choices = self.choices();
        if !list_choices {
            let next = choices
                .iter()
                .find_map(|(phrase, _, lines)| lines.first().map(|line| (phrase, line)));
            if let Some((phrase, line)) = next {
                out.push_str(&format!("next: {phrase}: {line}\n"));
            }
        } else if !choices.is_empty() {
            out.push_str("choices:\n");
            for (phrase, at, lines) in choices {
                match at {
                    Some(at) => out.push_str(&format!("  {phrase}, in {}\n", at.display())),
                    None => out.push_str(&format!("  {phrase}\n")),
                }
                for line in lines {
                    out.push_str(&format!("    {line}\n"));
                }
            }
        }
        out
    }

    /// What a refused `rwv <verb>` prints: each judged line, its facts and
    /// listing, the choices, then one route line per distinct axis.
    pub fn render(&self, verb: &str) -> String {
        let prefix = if verb.is_empty() {
            "rwv: ".to_owned()
        } else {
            format!("rwv {verb}: ")
        };
        let mut out = self.body(&prefix);
        out.push('\n');
        for kind in self.kinds() {
            out.push_str(&format!("rwv explain {}\n", kind.token()));
        }
        out
    }

    /// What a verb that completed prints for its judged lines: no choices
    /// block, and the first choice's command as one `next:` line.
    pub fn render_completed(&self, verb: &str) -> String {
        let prefix = if verb.is_empty() {
            "rwv: ".to_owned()
        } else {
            format!("rwv {verb}: ")
        };
        let mut out = self.body_listing(&prefix, false);
        out.push('\n');
        for kind in self.kinds() {
            out.push_str(&format!("rwv explain {}\n", kind.token()));
        }
        out
    }

    pub fn output(&self) -> Vec<JudgedOutput> {
        self.ordered().into_iter().map(Judged::output).collect()
    }
}

/// The choices `forms` print as a report's tail, in the shape a refusal prints
/// them: each phrase once, its lines beneath. Forms whose verb has not shipped
/// are left out, as everywhere.
pub fn choices_text(
    vcs: &dyn Vcs,
    forms: &[Form],
    bindings: &BTreeMap<&'static str, String>,
) -> String {
    let mut out = String::new();
    for choice in choices(vcs, forms, bindings) {
        out.push_str(&format!("  {}\n", choice.phrase));
        for line in &choice.run {
            out.push_str(&format!("    {line}\n"));
        }
    }
    if out.is_empty() {
        return out;
    }
    format!("choices:\n{out}")
}

/// What a verb that completed prints for `forms`: the first command as one
/// `next:` line, or nothing where no form has shipped.
pub fn next_text(
    vcs: &dyn Vcs,
    forms: &[Form],
    bindings: &BTreeMap<&'static str, String>,
) -> String {
    choices(vcs, forms, bindings)
        .iter()
        .find_map(|choice| choice.run.first().map(|line| (&choice.phrase, line)))
        .map(|(phrase, line)| format!("next: {phrase}: {line}\n"))
        .unwrap_or_default()
}

/// The choices `forms` print, as a `--json` record carries them.
pub fn choices(
    vcs: &dyn Vcs,
    forms: &[Form],
    bindings: &BTreeMap<&'static str, String>,
) -> Vec<ChoiceOutput> {
    forms
        .iter()
        .filter(|form| form.printable())
        .map(|form| {
            let row = form.row().choice.row();
            ChoiceOutput {
                id: row.id.map(str::to_owned),
                phrase: form.phrase(bindings),
                at: None,
                run: form
                    .lines(vcs, None)
                    .iter()
                    .map(|line| for_spelled_project(fill(line, bindings)))
                    .collect(),
            }
        })
        .collect()
}

/// `dir`'s edits, judged: an operation in progress is the state whether or
/// not it left files dirty, since only its own continue or abort ends it;
/// otherwise `dirty`'s files, when there are any. `elsewhere` is the
/// workspace `dir` is in when the verb runs in another.
pub fn judge_edits(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    dirty: Vec<String>,
    elsewhere: Option<&Path>,
) -> Option<Judged> {
    judge_edits_settling(vcs, repo, dir, dirty, elsewhere, true)
}

/// [`judge_edits`], with any drift judged as the dirt it looks like and
/// nothing refreshed, for a landing that settles drift itself.
pub fn judge_edits_as_found(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    dirty: Vec<String>,
    elsewhere: Option<&Path>,
) -> Option<Judged> {
    judge_edits_settling(vcs, repo, dir, dirty, elsewhere, false)
}

fn judge_edits_settling(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    dirty: Vec<String>,
    elsewhere: Option<&Path>,
    settle: bool,
) -> Option<Judged> {
    let mut judging = match vcs.mid_operation(dir) {
        Some(op) => State::EditsMidOperation.of(repo).subject(op.operation()),
        None => match vcs.set_back_from(dir) {
            Ok(Some(back)) => set_back(vcs, repo, dir, &back, None, None),
            _ if dirty.is_empty() => return None,
            _ if settle => dirty_beside_drift(vcs, repo, dir, dirty)?,
            _ => self::dirty(vcs, repo, dir, dirty),
        },
    };
    if let Some(workspace) = elsewhere {
        judging = judging.elsewhere(workspace);
    }
    Some(judging.checkout(dir).judge(vcs))
}

/// `dir`'s uncommitted `files`, judged by what else the checkout holds,
/// since no commit clears any of these: a merge of edits onto a moved tip
/// left set back, conflicted files, or files both a move of its branch
/// under it and an edit here changed. Otherwise `dirty`, listing `files`.
pub fn dirty(vcs: &dyn Vcs, repo: impl fmt::Display, dir: &Path, files: Vec<String>) -> Judging {
    if let Ok(Some(back)) = vcs.set_back_from(dir) {
        return set_back(vcs, repo, dir, &back, None, None);
    }
    if let Ok(conflicted) = vcs.unmerged_paths(dir) {
        if !conflicted.is_empty() {
            return unresolved(repo, conflicted);
        }
    }
    if let Ok(reading) = crate::check::read_drift(vcs, dir) {
        if !reading.moved_and_edited().is_empty() {
            return held(vcs, repo, dir, &reading);
        }
    }
    let behind = submodules_behind(vcs, dir);
    if !files.is_empty() && files.iter().all(|file| behind.contains(file)) {
        return State::EditsSubmoduleBehind
            .of(repo)
            .bind_list("<paths>", &files)
            .listing(files);
    }
    let tracked = vcs.tracked_dirty_file_names(dir).unwrap_or_default();
    let mut judging = State::EditsDirty.of(repo);
    judging.untracked = files.iter().any(|file| !tracked.contains(file));
    judging.listing(files)
}

/// The paths of `dir`'s submodules whose checkout stands at a commit other
/// than the one `dir` records and holds no edits of its own.
fn submodules_behind(vcs: &dyn Vcs, dir: &Path) -> Vec<String> {
    let Some(population) = vcs.population() else {
        return Vec::new();
    };
    population
        .recorded_submodules(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|submodule| {
            let checkout = dir.join(&submodule.path);
            vcs.is_repo(&checkout)
                && vcs
                    .head_revision(&checkout)
                    .is_ok_and(|head| *head.id() != submodule.commit)
                && vcs
                    .dirty_file_names(&checkout)
                    .is_ok_and(|edits| edits.is_empty())
        })
        .map(|submodule| submodule.path)
        .collect()
}

/// [`dirty`] for a verb about to refuse on or write to `dir`: drift its
/// branch's move left there is refreshed first, replacing only what still
/// holds the old tip's content, and what is left of `files` is judged; none
/// when nothing is. A refresh left in place is judged as that, whose one
/// choice reads the checkout afresh.
pub fn dirty_beside_drift(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    files: Vec<String>,
) -> Option<Judging> {
    let repo = repo.to_string();
    let held = matches!(vcs.set_back_from(dir), Ok(Some(_)))
        || vcs.unmerged_paths(dir).is_ok_and(|u| !u.is_empty());
    let reading = match crate::check::read_drift(vcs, dir) {
        Ok(reading) if !held && reading.moved_and_edited().is_empty() => reading,
        Err(e) if !held && vcs.is_repo(dir) => return Some(drift_unread(vcs, repo, dir, &e)),
        _ => return Some(dirty(vcs, repo, dir, files)),
    };
    if !reading.is_refreshable() {
        return (!files.is_empty()).then(|| dirty(vcs, repo, dir, files));
    }
    let outcome = match crate::check::refresh_drift(vcs, dir, &reading) {
        Ok(outcome) => outcome,
        Err(e) => crate::check::DriftRefresh {
            left_in_place: Some(crate::check::LeftInPlace {
                paths: reading
                    .stale_files()
                    .into_iter()
                    .chain(reading.stale_records())
                    .collect(),
                why: crate::check::LeftWhy::Failed(format!("{e:#}")),
            }),
            ..crate::check::DriftRefresh::default()
        },
    };
    if let Some(refreshed) = &outcome.refreshed {
        eprintln!("rwv: {repo}: {}", refreshed.describe());
    }
    if let Some(unrecorded) = outcome.describe_unrecorded() {
        eprintln!("warning: rwv: {repo}: {unrecorded}");
    }
    if let Some(left) = &outcome.left_in_place {
        return Some(
            State::EditsDriftLeftInPlace
                .of(repo)
                .subject(left.describe())
                .listing(&left.paths)
                .only_forms(&[Form::DriftRefreshAgain]),
        );
    }
    if !outcome.held.is_empty() {
        return Some(dirty(vcs, repo, dir, files));
    }
    let left: Vec<String> = files
        .into_iter()
        .filter(|file| {
            !outcome
                .refreshed
                .as_ref()
                .is_some_and(|refreshed| refreshed.replaced(file))
        })
        .collect();
    (!left.is_empty()).then(|| dirty(vcs, repo, dir, left))
}

/// `dir` given [`dirty_beside_drift`]'s judgment before a verb reads, stages,
/// commits or moves anything there: drift its branch's move left is refreshed,
/// and the checkout is held where that move's content cannot be told from what
/// the verb would carry or commit, or where a file is conflicted. Edits of the
/// operator's own are the verb's to handle.
pub fn drift_held(vcs: &dyn Vcs, repo: impl fmt::Display, dir: &Path) -> Option<Judging> {
    if !vcs.is_repo(dir) || vcs.head_revision(dir).is_err() {
        return None;
    }
    let files = vcs.tracked_dirty_file_names(dir).unwrap_or_default();
    dirty_beside_drift(vcs, repo, dir, files).filter(|judging| {
        matches!(
            judging.judged_state(),
            State::EditsMovedAndEdited
                | State::EditsDriftLeftInPlace
                | State::EditsSetBack
                | State::EditsUnresolved
        )
    })
}

/// [`drift_held`] for the project repo at `dir`, refused.
pub fn settle_project_drift(
    project: &crate::manifest::ProjectName,
    dir: &Path,
) -> anyhow::Result<()> {
    let vcs = crate::vcs::project_vcs();
    let repo = crate::workspace::project_rel_path(project);
    let Some(judging) = drift_held(vcs.as_ref(), &repo, dir) else {
        return Ok(());
    };
    let mut report = Report::default();
    report.add(judging.project_repo().checkout(dir).judge(vcs.as_ref()));
    report.into_result()
}

/// A checkout whose drift could not be read: nothing it holds is judged an
/// edit, since any of it may be the move's reverse, so the one choice is a
/// set-aside, which keeps every difference without recording any. The VCS
/// refuses to set aside a path marked to be added with no content, so those
/// are unmarked first.
pub fn drift_unread(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    e: &crate::vcs::VcsError,
) -> Judging {
    let judging = State::EditsDriftLeftInPlace
        .of(repo)
        .subject(format!("its drift could not be read: {e:#}"));
    let marked = marked_to_add(vcs, dir);
    if marked.is_empty() {
        return judging.only_forms(&[Form::StashUnreadDrift]);
    }
    judging
        .listing(&marked)
        .bind_list("<paths>", &marked)
        .only_forms(&[Form::UnmarkThenStashUnreadDrift])
}

/// The paths `dir`'s index marks to be added with no content recorded.
pub fn marked_to_add(vcs: &dyn Vcs, dir: &Path) -> Vec<String> {
    let versions = vcs.staged_versions_against_head(dir).unwrap_or_default();
    vcs.staged_against_head(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|path| !versions.contains(path))
        .collect()
}

/// Edits beside drift a verb refuses before refreshing: each listed, with
/// the refresh as the one choice, after which they are judged as edits.
pub fn edits_beside_unrefreshed_drift(
    repo: impl fmt::Display,
    reading: &crate::check::DriftReading,
    edits: &[String],
) -> Judging {
    State::EditsDriftLeftInPlace
        .of(repo)
        .subject(format!(
            "{}; it is not refreshed before this refusal, so these edits beside it get no \
             choice of their own until it is",
            reading.moved()
        ))
        .listing(edits)
        .only_forms(&[Form::DriftRefreshAgain])
}

fn unresolved(repo: impl fmt::Display, conflicted: Vec<String>) -> Judging {
    State::EditsUnresolved
        .of(repo)
        .bind_list("<paths>", &conflicted)
        .listing(conflicted)
}

/// `dir`'s conflicted files, judged `unresolved` whatever else it holds.
pub fn judge_unresolved(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    elsewhere: Option<&Path>,
) -> Judged {
    let mut judging = unresolved(repo, vcs.unmerged_paths(dir).unwrap_or_default());
    if let Some(workspace) = elsewhere {
        judging = judging.elsewhere(workspace);
    }
    judging.checkout(dir).judge(vcs)
}

/// A checkout holding files both its branch's move and an edit changed,
/// listed, whose merge is not refused by what stands above them.
pub fn moved_and_edited(repo: impl fmt::Display) -> Judging {
    State::EditsMovedAndEdited.of(repo).without(&[
        ChoiceId::ReplaceThenMerge,
        ChoiceId::CompareMarked,
        ChoiceId::CompareSkipped,
        ChoiceId::ClearRefusalThenMerge,
    ])
}

/// `dir`'s files both moved and edited, as `reading` found them, judged.
/// Where the VCS does not compare some of them, so no backup would keep
/// them, the choices are those that have it compare them.
pub fn held(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    reading: &crate::check::DriftReading,
) -> Judging {
    let held = reading.moved_and_edited();
    let judging = State::EditsMovedAndEdited
        .of(repo)
        .subject(reading.moved())
        .listing(&held);
    if !reading.in_the_way(dir).is_empty() {
        return judging.only_forms(&[Form::ReplaceThenMerge]);
    }
    let uncompared = match vcs.uncompared_paths(dir, &held) {
        Ok(uncompared) => uncompared,
        Err(e) => {
            return judging
                .subject(format!(
                    "{}; the files the VCS does not compare could not be listed, so no \
                     backup is known to keep them: {e:#}",
                    reading.moved()
                ))
                .only_forms(&[Form::ClearRefusalThenMerge])
        }
    };
    if uncompared.is_empty() {
        return judging.without(&[
            ChoiceId::ReplaceThenMerge,
            ChoiceId::CompareMarked,
            ChoiceId::CompareSkipped,
            ChoiceId::ClearRefusalThenMerge,
        ]);
    }
    let mut forms = Vec::new();
    let mut judging = judging;
    if !uncompared.ignored.is_empty() {
        judging = judging.subject(format!(
            "{}; ignored files no backup keeps stand where the moved tip puts its own: {}",
            reading.moved(),
            uncompared.ignored.join(", ")
        ));
        forms.push(Form::ReplaceThenMerge);
    }
    if !uncompared.assumed.is_empty() {
        forms.push(Form::CompareMarked);
    }
    if !uncompared.skipped.is_empty() {
        forms.push(Form::CompareSkipped);
    }
    judging
        .bind_list("<assumed>", &uncompared.assumed)
        .bind_list("<skipped>", &uncompared.skipped)
        .only_forms(&forms)
}

/// A backup kept before a merge of edits, and its tip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptBackup {
    pub name: String,
    pub tip: String,
}

/// A checkout the merge or set-aside of edits left set back, judged: its
/// edits listed, and the choices that carry them on from what stands there.
/// `refused` is the VCS's own account of the step it refused.
pub fn set_back(
    vcs: &dyn Vcs,
    repo: impl fmt::Display,
    dir: &Path,
    back: &crate::vcs::SetBack,
    refused: Option<&str>,
    backup: Option<&KeptBackup>,
) -> Judging {
    let mut subject = format!(
        "pointed at {}, the tip its files hold, off branch {}",
        back.at.short(),
        back.branch
    );
    if let Some(backup) = backup {
        subject.push_str(&format!(
            "; the edits are kept as {}",
            crate::check::backup_named(Some(&backup.name))
        ));
    }
    if let Some(said) = refused {
        let said: Vec<&str> = said
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        subject.push_str(&format!("; the VCS refused: {}", said.join(" ")));
    }
    let gone = vcs.resolve_revision(dir, &back.branch).is_err();
    let elsewhere: Vec<PathBuf> = vcs
        .checkouts_of_branch(dir, &crate::vcs::RawRefName::new(back.branch.clone()))
        .unwrap_or_default()
        .into_iter()
        .filter(|other| {
            crate::path_spelling::CanonicalPath::of(other)
                != crate::path_spelling::CanonicalPath::of(dir)
        })
        .collect();
    if gone {
        subject.push_str(&format!("; branch {} no longer exists", back.branch));
    }
    if let Some(other) = elsewhere.first() {
        subject.push_str(&format!(
            "; branch {} is checked out at {}",
            back.branch,
            other.display()
        ));
    }
    let lock = vcs.index_lock(dir).ok().flatten();
    if let Some(lock) = &lock {
        if refused.is_none() {
            subject.push_str(&format!(
                "; the VCS's record of the checkout is locked by {}",
                lock.display()
            ));
        }
    }
    let mut edits: Vec<String> = vcs.staged_paths(dir).unwrap_or_default();
    for path in vcs.unstaged_paths(dir).unwrap_or_default() {
        if !edits.contains(&path) {
            edits.push(path);
        }
    }
    edits.sort();
    let staged = vcs.staged_against_head(dir).unwrap_or_default();
    let all_kept = vcs
        .set_aside_from(dir, &back.at, &back.branch)
        .unwrap_or_default();
    if !all_kept.is_empty() {
        subject.push_str(&format!(
            "; edits set aside here are kept as {}",
            all_kept
                .iter()
                .map(crate::vcs::CommitId::short)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let kept = all_kept.last();
    let carried = match gone {
        true => Ok(crate::vcs::UncomparedPaths::default()),
        false => carried_over(vcs, dir, back, &edits),
    };
    if let Ok(uncompared) = &carried {
        if !uncompared.ignored.is_empty() {
            subject.push_str(&format!(
                "; ignored files no backup keeps stand where the branch's tip puts its own: {}",
                uncompared.ignored.join(", ")
            ));
        }
    }
    if let Err(e) = &carried {
        subject.push_str(&format!(
            "; the files the VCS does not compare could not be listed, so no backup is known \
             to keep them: {e:#}"
        ));
    }
    let judging = State::EditsSetBack
        .of(repo)
        .subject(subject)
        .listing(&edits)
        .bind(
            "<set-aside>",
            kept.map(crate::vcs::CommitId::as_str).unwrap_or_default(),
        );
    let forms = |first: &[Form]| -> Vec<Form> {
        first
            .iter()
            .copied()
            .chain(kept.map(|_| Form::TakeBackSetAside))
            .collect()
    };
    if lock.is_some() {
        return judging.only_forms(&forms(&[Form::RemoveLockThenMerge]));
    }
    if !staged.is_empty() {
        return judging
            .bind_list("<paths>", &staged)
            .only_forms(&forms(&[Form::UnstageThenMerge]));
    }
    match carried {
        Err(_) => return judging.only_forms(&forms(&[Form::ClearRefusalThenMerge])),
        Ok(uncompared) if !uncompared.is_empty() => {
            let mut held = Vec::new();
            if !uncompared.ignored.is_empty() {
                held.push(Form::ReplaceThenMerge);
            }
            if !uncompared.assumed.is_empty() {
                held.push(Form::CompareMarked);
            }
            if !uncompared.skipped.is_empty() {
                held.push(Form::CompareSkipped);
            }
            return judging
                .listing(uncompared.ignored.iter().filter(|p| !edits.contains(p)))
                .bind_list("<assumed>", &uncompared.assumed)
                .bind_list("<skipped>", &uncompared.skipped)
                .only_forms(&forms(&held));
        }
        Ok(_) => {}
    }
    if gone {
        return judging.only_forms(&forms(&[Form::RecreateBranchThenMerge]));
    }
    if !elsewhere.is_empty() {
        return judging.only_forms(&forms(&[Form::FreeBranchThenMerge]));
    }
    match refused {
        Some(_) => judging.only_forms(&forms(&[Form::ClearRefusalThenMerge])),
        None => judging.only_forms(&forms(&[Form::MergeEditedDrift, Form::SetEditsAside])),
    }
}

/// Of the paths a rerun from `back` writes, those the VCS does not compare
/// with what it records, so no backup would keep them: each the branch's
/// tip changes from the tip the files hold, and each edit standing there.
pub fn carried_over(
    vcs: &dyn Vcs,
    dir: &Path,
    back: &crate::vcs::SetBack,
    edits: &[String],
) -> Result<crate::vcs::UncomparedPaths, crate::vcs::VcsError> {
    let tip = vcs.resolve_revision(dir, &back.branch)?;
    let mut paths = vcs.changed_paths_between(
        dir,
        &crate::vcs::ResolvedRevisionId::from(back.at.clone()),
        &tip,
    )?;
    paths.extend(edits.iter().cloned());
    paths.sort();
    paths.dedup();
    vcs.uncompared_paths(dir, &paths)
}

/// The error a [`Report`] refuses with.
#[derive(Debug)]
struct Refused(Report);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.body("").trim_end_matches('\n'))
    }
}

impl std::error::Error for Refused {}

/// The report one link of an error chain refuses with: a judged refusal, or
/// a typed error that is a state of the table and reaches the terminal
/// through `?`.
fn report_at<'a>(link: &'a (dyn std::error::Error + 'static)) -> Option<Cow<'a, Report>> {
    if let Some(refused) = link.downcast_ref::<Refused>() {
        return Some(Cow::Borrowed(&refused.0));
    }
    let pre_schema = link.downcast_ref::<crate::manifest::PreSchemaManifest>()?;
    let repo = crate::workspace::project_rel_path_of_manifest(pre_schema.path.as_deref()?)?;
    let mut report = Report::default();
    report.add(
        State::UnparseableProjectHere
            .of(repo)
            .listing(pre_schema.listing())
            .judge(crate::vcs::vcs_for(crate::manifest::VcsType::Git).as_ref()),
    );
    Some(Cow::Owned(report))
}

/// The report `err` refuses with, wherever in its chain it sits.
pub fn report_of(err: &anyhow::Error) -> Option<Cow<'_, Report>> {
    err.chain().find_map(report_at)
}

/// The judged lines `err` refuses with, for a `--json` record; empty when it
/// judged none.
pub fn refused_output(err: &anyhow::Error) -> Vec<JudgedOutput> {
    report_of(err)
        .map(|report| report.output())
        .unwrap_or_default()
}

/// The context lines wrapped above `err`'s report, outermost first.
pub fn contexts_above_report(err: &anyhow::Error) -> Vec<String> {
    err.chain()
        .take_while(|link| report_at(*link).is_none())
        .map(ToString::to_string)
        .collect()
}

/// Schema URL of the envelope a refused verb prints under `--json` when it
/// printed no envelope of its own.
pub const REFUSAL_SCHEMA_URL: &str = crate::schema_url::schema_url!("refusal");

/// What a refused `rwv <verb> --json` prints when the verb printed nothing
/// before refusing: every repo it judged.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RefusalJsonOutput {
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Each repo the verb judged, with that state's choices.
    pub refused: Vec<JudgedOutput>,
}

/// One judged repo, as `--json` carries it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct JudgedOutput {
    /// The repo's path, `projects/<name>` for the project repo.
    pub path: String,
    /// The workspace the repo was judged in, when that is not the one the
    /// verb ran in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<crate::path_spelling::WirePath>,
    /// What it was judged on; `rwv explain <axis>` serves the axis's page.
    pub axis: Axis,
    /// The state it was judged in, as that page names it.
    pub state: String,
    /// What the state names, when it names something.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The files, paths or commits the state lists.
    pub listed: Vec<String>,
    pub facts: Vec<FactOutput>,
    /// What this state can run, in the order the page lists it.
    pub choices: Vec<ChoiceOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FactOutput {
    pub fact: FactKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChoiceOutput {
    /// The choice's id on the axis's page; null for one it does not number.
    pub id: Option<String>,
    pub phrase: String,
    /// The directory it must run in; null where it runs here or names its
    /// own directory.
    #[serde(rename = "where")]
    pub at: Option<crate::path_spelling::WirePath>,
    /// The commands, one per line, as the terminal prints them.
    pub run: Vec<String>,
}

/// The entries of the refusals page: what each axis judges, its states, and
/// each state's printable choices, VCS actions spelled by `vcs_type`'s backend.
/// An axis with a finding has its one entry on the findings page instead
/// ([`finding_entries_markdown`]).
pub fn kind_entries_markdown(vcs_type: crate::manifest::VcsType) -> String {
    let vcs = crate::vcs::vcs_for(vcs_type);
    let vcs_name = crate::wire_token::wire_token(&vcs_type);
    Axis::ALL
        .into_iter()
        .filter(|axis| axis.finding().is_none())
        .map(|axis| {
            format!(
                "### `{}`\n\n**Condition.** {}\n\n{}",
                axis.token(),
                axis.row().condition,
                kind_sections(axis, vcs.as_ref(), &vcs_name)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The refusals page's pointers to the kinds whose entry is on the findings
/// page, one bullet each.
pub fn finding_kind_links_markdown() -> String {
    Axis::ALL
        .into_iter()
        .filter(|axis| axis.finding().is_some())
        .map(|axis| {
            let token = axis.token();
            format!("- [`{token}`](./doctor-findings.md#{token})\n")
        })
        .collect()
}

/// The entry of each axis that has a finding, as the findings page carries
/// it: the axis's prose, its states and choices, then one section per
/// sub-kind opening with the severity and `--fix` disposition the doctor
/// register gives it.
pub fn finding_entries_markdown(vcs_type: crate::manifest::VcsType) -> Vec<(Axis, String)> {
    let vcs = crate::vcs::vcs_for(vcs_type);
    let vcs_name = crate::wire_token::wire_token(&vcs_type);
    Axis::ALL
        .into_iter()
        .filter_map(|axis| Some((axis, axis.finding()?)))
        .map(|(axis, finding)| {
            let marks = finding.whole.as_ref().map_or(String::new(), |whole| {
                format!(
                    "**{}. {}** ",
                    severity_text(whole.severity),
                    fix_text(whole.fix)
                )
            });
            let mut out = format!(
                "### `{}`\n\n{marks}{}\n\n{}",
                axis.token(),
                axis.row().condition,
                kind_sections(axis, vcs.as_ref(), &vcs_name)
            );
            if let Some(whole) = &finding.whole {
                out.push_str(&format!("\n{}\n", consent_note(whole.fix, whole.entry)));
            }
            for sub_kind in &finding.sub_kinds {
                out.push_str(&format!(
                    "\n#### `{}`\n\n**{}. {}** {}\n",
                    sub_kind.token,
                    severity_text(sub_kind.severity),
                    fix_text(sub_kind.fix),
                    consent_note(sub_kind.fix, sub_kind.entry)
                ));
            }
            (axis, out)
        })
        .collect()
}

fn severity_text(severity: crate::integration::Severity) -> &'static str {
    match severity {
        crate::integration::Severity::Warning => "Warning",
        crate::integration::Severity::Error => "Error",
    }
}

fn fix_text(fix: crate::check::FixDisposition) -> &'static str {
    match fix {
        crate::check::FixDisposition::Auto => "Auto-fixable.",
        crate::check::FixDisposition::ReportOnly => "Report-only.",
        crate::check::FixDisposition::Consented(_) => "Report-only by default.",
    }
}

fn consent_note(fix: crate::check::FixDisposition, entry: &str) -> String {
    match fix {
        crate::check::FixDisposition::Consented(flag) => {
            format!("Bare `rwv doctor --fix` leaves it; `{flag}` repairs it. {entry}")
        }
        crate::check::FixDisposition::Auto | crate::check::FixDisposition::ReportOnly => {
            entry.to_owned()
        }
    }
}

/// The part of an axis's entry that comes from the table: why the rule
/// exists, the states, their choices, and where the rule sits.
fn kind_sections(axis: Axis, vcs: &dyn Vcs, vcs_name: &str) -> String {
    let mut out = String::new();
    let kind = axis.row();
    let states: Vec<State> = State::ALL
        .into_iter()
        .filter(|s| s.axis() == axis)
        .collect();
    out.push_str(&format!("**Why.** {}\n\n", kind.why));
    out.push_str(&format!(
        // rwv-advice: not-an-invocation
        "**States.** A verb that judges a repo in one of these prints `rwv <verb>: <repo>: {} \
         <state>`, with what the state lists beneath it, and refuses unless its row below \
         says it completes.\n\n",
        axis.token()
    ));
    out.push_str("| State | Means | Refused by |\n|---|---|---|\n");
    for state in &states {
        let row = state.row();
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            row.name, row.means, row.refused_by
        ));
    }
    out.push_str(
        "\n**Choices.** Each state's choices print under `choices:` in this order, with a \
         directory named where a choice does not run where the verb did. A state with no \
         row here prints none.\n\n",
    );
    out.push_str(
        "| State | Choice | Spelled | Where | Keeps work because |\n|---|---|---|---|---|\n",
    );
    for state in &states {
        let state_row = state.row();
        let every = state_row.offers.iter().map(|form| (None, form));
        let by_verb = state_row
            .verb_offers
            .iter()
            .map(|(verb, form)| (Some(*verb), *form));
        let by_store_removal = state_row
            .offers
            .iter()
            .filter(|form| form.for_store_removal() != **form)
            .map(|form| (Some(STORE_REMOVAL), form.for_store_removal()));
        for (verb, form) in every
            .map(|(verb, form)| (verb, *form))
            .chain(by_verb)
            .chain(by_store_removal)
            .filter(|(_, f)| f.printable())
        {
            let row = form.row();
            let choice = row.choice.row();
            let label = match choice.id {
                Some(id) => format!("{id}: {}", choice.phrase),
                None => choice.phrase.to_owned(),
            };
            let state_cell = match verb {
                None => format!("`{}`", state.name()),
                Some(STORE_REMOVAL) => format!(
                    "`{}`, for a removal of a clone that is the repo's only store",
                    state.name()
                ),
                Some(verb) => format!("`{}`, for `rwv {verb}`", state.name()),
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                state_cell,
                label,
                spelled(form, vcs, vcs_name),
                row.at.describe(),
                choice.keeps_work
            ));
        }
    }
    if !kind.sits.is_empty() {
        out.push_str(&format!("\n**Where this sits.** {}\n", kind.sits));
    }
    out
}

const STORE_REMOVAL: &str = "";

fn spelled(form: Form, vcs: &dyn Vcs, vcs_name: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut actions: Vec<String> = Vec::new();
    let flush = |actions: &mut Vec<String>, parts: &mut Vec<String>| {
        if !actions.is_empty() {
            parts.push(format!("VCS action ({vcs_name}: {})", actions.join(", ")));
            actions.clear();
        }
    };
    for step in form.row().steps {
        match step {
            Step::Vcs(action) => actions.push(format!("`{}`", vcs.hint(*action))),
            Step::Rwv(command) => {
                flush(&mut actions, &mut parts);
                parts.push(format!("`{command}`"));
            }
            Step::Then(next) => {
                flush(&mut actions, &mut parts);
                parts.push(format!("then {}", spelled(*next, vcs, vcs_name)));
            }
            Step::Rerun(flag) => {
                flush(&mut actions, &mut parts);
                parts.push(format!("rerun with `{flag}`"));
            }
            Step::RerunWithout(flag) => {
                flush(&mut actions, &mut parts);
                parts.push(format!("rerun without `{flag}`"));
            }
            Step::Resume(verb) => {
                flush(&mut actions, &mut parts);
                parts.push(format!("`{}`", crate::op_state::resume_command(*verb)));
            }
            Step::Recorded(line) => {
                flush(&mut actions, &mut parts);
                parts.push(match line.strip_prefix("<resume>") {
                    Some(flags) => format!("the stopped operation's `--continue{flags}`"),
                    None if *line == "<landing>" => {
                        "`rwv land` with the flags the stopped landing was started with".to_owned()
                    }
                    None if line.starts_with("<landing> ") => format!(
                        "`rwv land` with the flags it was run with, and `{}`",
                        &line["<landing> ".len()..]
                    ),
                    None => format!(
                        "`{}`, against the stopped operation's workspace",
                        line.replace("<abort>", "rwv abort")
                    ),
                });
            }
        }
    }
    flush(&mut actions, &mut parts);
    if parts.is_empty() {
        "—".to_owned()
    } else {
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git() -> Box<dyn Vcs> {
        crate::vcs::vcs_for(crate::manifest::VcsType::Git)
    }

    #[test]
    fn every_state_row_names_its_own_axis_and_a_unique_name() {
        let mut seen = std::collections::BTreeSet::new();
        let mut offering_nothing = Vec::new();
        for state in State::ALL {
            assert!(
                seen.insert((state.axis(), state.name())),
                "{state:?} repeats a name on its axis"
            );
            if state.row().offers.is_empty() && state.row().verb_offers.is_empty() {
                offering_nothing.push(state);
            }
        }
        assert_eq!(
            offering_nothing,
            [
                State::PinWaived,
                State::PublishRecordsRejected,
                State::UnparseableProjectMalformed,
                State::UnparseableProjectCommit,
                State::AlignmentAligned
            ],
            "a state that offers nothing is a decision the table records, not an omission"
        );
        for axis in Axis::ALL {
            assert!(
                State::ALL.iter().any(|s| s.axis() == axis),
                "{axis:?} has no state"
            );
        }
    }

    #[test]
    fn a_form_runs_after_another_only_once_that_one_prints() {
        for form in Form::ALL {
            let after_unprintable = form.row().steps.iter().any(|step| match step {
                Step::Then(next) => !next.printable(),
                _ => false,
            });
            assert_eq!(
                form.printable(),
                form.row().shipped && !after_unprintable,
                "{form:?}"
            );
        }
        assert!(Form::C15AtCherryPick.printable());
        assert!(Form::C14Keep.printable());
        let after_c40 = FormRow {
            steps: &[Step::Rwv("rwv lock --commit"), Step::Then(Form::C40)],
            ..*Form::C15AtCherryPick.row()
        };
        assert!(after_c40.printable_where(|_| true));
        assert!(!after_c40.printable_where(|form| form != Form::C40));
    }

    #[test]
    fn a_checkout_mid_operation_is_judged_on_that_alone() {
        let vcs = git();
        let line = |state: State, repo: &str, elsewhere: Option<&str>| {
            let judging = state.of(repo).subject("x");
            match elsewhere {
                Some(at) => judging.elsewhere(Path::new(at)),
                None => judging,
            }
            .judge(vcs.as_ref())
        };
        let mut report = Report::default();
        report.add(line(State::PinBackwards, "github/o/lib", None));
        report.add(line(State::EditsDirty, "github/o/lib", None));
        report.add(line(State::PinBackwards, "github/o/lib", Some("/t")));
        report.add(line(State::PinBackwards, "github/o/app", None));
        report.add(line(State::EditsMidOperation, "github/o/lib", None));
        report.add(line(State::AlignmentOnBranch, "github/o/lib", None));
        report.add(line(State::PushBranchDiverged, "github/o/lib", None));
        let kept: Vec<(State, &str, bool)> = report
            .lines()
            .iter()
            .map(|l| (l.state(), l.repo(), l.workspace.is_some()))
            .collect();
        assert_eq!(
            kept,
            vec![
                (State::PinBackwards, "github/o/lib", true),
                (State::PinBackwards, "github/o/app", false),
                (State::EditsMidOperation, "github/o/lib", false),
                (State::PushBranchDiverged, "github/o/lib", false),
            ]
        );
    }

    #[test]
    fn a_line_judged_twice_is_printed_once() {
        let vcs = git();
        let busy = || {
            State::EditsMidOperation
                .of("github/o/lib")
                .subject("rebase")
                .checkout(Path::new("/w/github/o/lib"))
                .judge(vcs.as_ref())
        };
        let mut report = Report::default();
        report.add(busy());
        report.extend(Some(busy()));
        assert!(report.absorb(Err(busy().refused())).is_ok());
        assert_eq!(
            report.render("land"),
            "rwv land: github/o/lib: edits mid-operation (rebase)\n\
             choices:\n\
             \x20 finish or abandon the operation in progress first\n\
             \x20   git -C /w/github/o/lib status\n\
             \n\
             rwv explain edits\n"
        );
    }

    #[test]
    fn a_report_prints_lines_then_choices_then_one_route_line_per_axis() {
        let vcs = git();
        let mut report = Report::default();
        report.add(
            State::EditsDirty
                .of("github/o/lib")
                .listing(["a.txt"])
                .checkout(Path::new("/w/github/o/lib"))
                .judge(vcs.as_ref()),
        );
        report.add(
            State::CheckoutInRawWorktree
                .of("github/o/lib")
                .listing(["/x/wt"])
                .checkout(Path::new("/w/github/o/lib"))
                .bind("<worktree>", "/x/wt")
                .judge(vcs.as_ref()),
        );
        report.add(
            State::EditsDirty
                .of("github/o/app")
                .listing(["b.txt"])
                .checkout(Path::new("/w/github/o/app"))
                .judge(vcs.as_ref()),
        );
        assert_eq!(
            report.render("remove"),
            "rwv remove: github/o/lib: edits dirty\n\
             \x20   a.txt\n\
             rwv remove: github/o/lib: checkout in a raw worktree\n\
             \x20   /x/wt\n\
             rwv remove: github/o/app: edits dirty\n\
             \x20   b.txt\n\
             choices:\n\
             \x20 commit your edits, or set them aside\n\
             \x20   git -C /w/github/o/lib commit\n\
             \x20   git -C /w/github/o/lib stash push\n\
             \x20   git -C /w/github/o/app commit\n\
             \x20   git -C /w/github/o/app stash push\n\
             \x20 free the other checkout\n\
             \x20   git -C /w/github/o/lib worktree remove /x/wt\n\
             \n\
             rwv explain edits\n\
             rwv explain checkout\n"
        );
    }

    #[test]
    fn a_place_elsewhere_is_named_and_a_value_with_a_space_is_quoted() {
        let vcs = git();
        let judged = State::NameTaken
            .of("projects/proj")
            .subject("seat")
            .bind("<project>", "proj")
            .bind("<ww>", "seat")
            .bind_place("<primary>", Path::new("/my weave"))
            .by_verb("workweave create")
            .judge(vcs.as_ref());
        let out = judged.output();
        assert_eq!(out.axis, Axis::Name);
        assert_eq!(out.state, "taken");
        let ids: Vec<Option<&str>> = out.choices.iter().map(|c| c.id.as_deref()).collect();
        assert_eq!(ids, [None, Some("C9")]);
        assert_eq!(
            out.choices[1].at,
            Some(crate::path_spelling::WirePath::of(Path::new("/my weave")))
        );
        assert_eq!(
            out.choices[1].run,
            ["rwv workweave delete seat --project proj"]
        );
        assert_eq!(
            fill("cd <dir>", &BTreeMap::from([("<dir>", "a b".into())])),
            "cd \"a b\""
        );
        assert_eq!(
            fill(
                "rwv land --allow-member-change <m> --retire",
                &BTreeMap::from([("<m>", ["a", "b c"].join(EACH))])
            ),
            "rwv land --allow-member-change a --allow-member-change \"b c\" --retire"
        );
    }

    #[test]
    fn an_axis_with_a_finding_spells_its_token_and_carries_every_sub_kind() {
        let finding_kinds: std::collections::BTreeSet<String> = crate::check::ViolationKind::ALL
            .iter()
            .map(ToString::to_string)
            .collect();
        let shared: Vec<Axis> = Axis::ALL
            .into_iter()
            .filter(|axis| finding_kinds.contains(&axis.token()))
            .collect();
        let with_finding: Vec<Axis> = Axis::ALL
            .into_iter()
            .filter(|axis| axis.finding().is_some())
            .collect();
        assert!(!shared.is_empty());
        assert_eq!(
            with_finding, shared,
            "an axis names its doctor finding exactly when a finding kind spells its token"
        );
        for axis in Axis::ALL {
            let Some(finding) = axis.finding() else {
                continue;
            };
            assert_eq!(finding.kind.to_string(), axis.token());
            let carried = crate::check::finding_sub_kinds()
                .remove(&axis.token())
                .unwrap_or_default();
            let listed: std::collections::BTreeSet<String> =
                finding.sub_kinds.iter().map(|s| s.token.clone()).collect();
            assert_eq!(listed, carried, "{axis:?}");
        }
    }

    #[test]
    fn a_findings_entry_opens_each_sub_kind_with_the_registers_marks() {
        let entries = finding_entries_markdown(crate::manifest::VcsType::Git);
        let [(Axis::UnparseableProject, entry), (Axis::Pin, pin), (Axis::HistoryDisagreement, history), (Axis::Alignment, alignment), (Axis::MissingLockMergeDriver, driver)] =
            &entries[..]
        else {
            panic!("each axis with a finding yields its entry, in order: {entries:?}");
        };
        assert!(
            history.starts_with("### `history-disagreement`\n"),
            "{history}"
        );
        assert!(
            history.starts_with("### `history-disagreement`\n\n**Warning. Report-only.** Two"),
            "{history}"
        );
        assert!(!history.contains("\n#### "), "{history}");
        assert!(
            driver.contains("\n#### `config-legacy`\n\n**Warning. Auto-fixable.** "),
            "{driver}"
        );
        assert!(pin.starts_with("### `pin`\n"), "{pin}");
        assert!(
            pin.contains("\n#### `behind`\n\n**Error. Report-only.** "),
            "{pin}"
        );
        assert!(alignment.starts_with("### `alignment`\n"), "{alignment}");
        assert!(
            alignment.contains("\n#### `on-branch`\n\n**Warning. Report-only.** "),
            "{alignment}"
        );
        assert!(entry.starts_with("### `unparseable-project`\n"), "{entry}");
        assert!(
            entry.contains("\n#### `pre-schema`\n\n**Error. Auto-fixable.** "),
            "{entry}"
        );
        assert!(
            entry.contains("\n#### `malformed`\n\n**Error. Report-only.** "),
            "{entry}"
        );
        let refusals = kind_entries_markdown(crate::manifest::VcsType::Git);
        for token in ["unparseable-project", "### `pin`", "### `alignment`"] {
            assert!(
                !refusals.contains(token),
                "the refusals page holds no entry for a kind the findings page holds: {token}"
            );
        }
    }

    #[test]
    fn a_state_that_offers_nothing_prints_no_choices_block_and_still_routes() {
        let mut report = Report::default();
        report.add(
            State::UnparseableProjectMalformed
                .of("projects/p")
                .listing(["failed to parse rwv.toml at /w/projects/p/rwv.toml"])
                .judge(git().as_ref()),
        );
        assert_eq!(
            report.render("sync"),
            "rwv sync: projects/p: unparseable-project malformed\n\
             \x20   failed to parse rwv.toml at /w/projects/p/rwv.toml\n\
             \n\
             rwv explain unparseable-project\n"
        );
    }

    #[test]
    fn a_diverged_branch_offers_the_merge_and_the_reset_spelled_for_its_repo() {
        let judged = |scope: Scope| {
            let judging = State::PushBranchDiverged
                .of("github/o/lib")
                .subject("1 ahead, 1 behind")
                .bind("<m>", "github/o/lib")
                .bind("<repo>", "github/o/lib");
            let judging = match scope {
                Scope::Member => judging.member(),
                Scope::Project => judging.project_repo(),
            };
            let mut report = Report::default();
            report.add(judging.judge(git().as_ref()));
            report.render("update")
        };
        let member = judged(Scope::Member);
        assert!(
            member.contains("rwv update --remote --merge-upstream --repo github/o/lib")
                && member.contains("rwv update --remote --reset-to-upstream github/o/lib"),
            "{member}"
        );
        assert!(!member.contains("rwv pull"), "{member}");
        let project = judged(Scope::Project);
        assert!(
            project.contains("rwv pull --merge-upstream")
                && project.contains("rwv pull --reset-to-upstream\n"),
            "{project}"
        );
    }

    #[test]
    fn a_fact_prints_before_the_listing() {
        let mut report = Report::default();
        report.add(
            State::PublishWithheld
                .of("github/o/lib")
                .listing(["rejected"])
                .fact(Fact {
                    kind: FactKind::PinnedBy,
                    about: Some("other".into()),
                })
                .judge(git().as_ref()),
        );
        assert!(report.render("push").starts_with(
            "rwv push: github/o/lib: publish withheld\n    pinned-by: other\n    rejected\n"
        ));
    }

    #[test]
    fn the_report_survives_a_context_wrap() {
        use anyhow::Context;
        let mut report = Report::default();
        report.add(
            State::EditsMidOperation
                .of("projects/p")
                .subject("rebase")
                .judge(git().as_ref()),
        );
        let err = report.into_result().context("while landing").unwrap_err();
        assert_eq!(report_of(&err).map(|r| r.kinds()), Some(vec![Axis::Edits]));
        assert_eq!(contexts_above_report(&err), ["while landing"]);
    }
}
