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
}

/// An axis's page prose: what it judges, why, and where the rule sits.
pub struct KindRow {
    pub condition: &'static str,
    pub why: &'static str,
    pub sits: &'static str,
}

impl Axis {
    pub const ALL: [Axis; 7] = [
        Self::Edits,
        Self::Checkout,
        Self::PushBranch,
        Self::Name,
        Self::Publish,
        Self::UnparseableProject,
        Self::Pin,
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
                      holds a partial result that only the tool which started it can finish.",
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
                            already holds it, it reads as a revision where a branch belongs, or \
                            it names nothing in a repo the verb read.",
                why: "A workweave name maps to one workweave; recording a second would make \
                      every later address ambiguous, and a delete would remove whichever was \
                      found first. A member entry's `branch` declares the branch to track while \
                      the lock records where you are, so a commit id or release tag there leaves \
                      nothing to track. A name that resolves to nothing has no tip to read, and \
                      reading another branch in its place would answer a question nobody asked.",
                sits: "[Workweave hierarchy](../explanation/joints/workweave-hierarchy.md); \
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
            | Self::Pin => None,
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
is staged or a rebase, merge or cherry-pick is in progress in the project repo, and says so; run \
it again once that is dealt with. A value of `submodules` other than `\"local\"` or \
`\"skip\"`, or an entry that names both a key and its replacement, is not guessed at: the repair \
names it and leaves the file.

A commit other than the checked-out one that names the old keys is not reported and is not \
refused. It declares nothing to the verbs that read it, and its `rwv.lock` is read as any lock \
is.";

const MALFORMED_ENTRY: &str = "Any other reason the project did not load. The message names which \
file failed and carries that file's remedy — the manifest is yours to edit, at the line the \
parser names; the lock is generated, so `rwv lock` rewrites it. A repository entry with no \
`role`, an unknown top-level key, an unknown `[integrations.<name>]` table, an unknown key in \
one, and an `enabled` that is not `true` or `false` are all parse failures of this kind, each \
naming the line and what was expected. `rwv sync` and `rwv sync-to` refuse with it when the \
project does not load after the project repo is replayed, and `rwv abort` rolls that operation \
back.

`--fix` has no arm for it. Rewriting a manifest rwv could not parse would be guesswork, and \
regenerating a lock re-pins the workspace to whatever the working tips happen to be — a decision \
about state, not a repair.";

/// What `rwv doctor` reports under a kind's name: the sub-kinds the finding
/// carries, each with the severity and `--fix` disposition the doctor
/// register gives it.
pub struct Finding {
    pub kind: crate::check::ViolationKind,
    pub sub_kinds: Vec<SubKindRow>,
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
        }
    }
}

/// One state a repo can be judged in, on its axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    EditsDirty,
    EditsMidOperation,
    EditsLockFilesUncommitted,
    CheckoutInAnotherWorkspace,
    CheckoutInRawWorktree,
    PushBranchDiverged,
    PinNone,
    PinUnreachable,
    NameTaken,
    NameARevision,
    NameNoSuchBranch,
    PublishWithheld,
    UnparseableProjectHere,
    UnparseableProjectMalformed,
    PinWouldBeUnreachable,
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
    pub const ALL: [State; 15] = [
        Self::EditsDirty,
        Self::EditsMidOperation,
        Self::EditsLockFilesUncommitted,
        Self::CheckoutInAnotherWorkspace,
        Self::CheckoutInRawWorktree,
        Self::PushBranchDiverged,
        Self::PinNone,
        Self::PinUnreachable,
        Self::NameTaken,
        Self::NameARevision,
        Self::NameNoSuchBranch,
        Self::PublishWithheld,
        Self::UnparseableProjectHere,
        Self::UnparseableProjectMalformed,
        Self::PinWouldBeUnreachable,
    ];

    pub fn row(self) -> &'static StateRow {
        match self {
            Self::EditsDirty => &StateRow {
                axis: Axis::Edits,
                name: "dirty",
                means: "the checkout has uncommitted changes, listed by file",
                refused_by: "`rwv sync` and `rwv sync-to`, for a repo they would rebase or \
                             fast-forward (tracked changes only); `rwv lock` without `--dirty`, \
                             for a member (tracked changes only); `rwv lock --commit` and `rwv \
                             update --commit`, for project-repo changes outside `rwv.lock` and \
                             `rwv.toml`; `rwv push`, for the project repo; `rwv member \
                             set-role <repo> reference`; `rwv member remove --delete` and \
                             replay's prune of a dropped member; `rwv workweave delete` and \
                             `rwv sync-to --retire`, for every checkout in the workweave",
                offers: &[Form::C13],
                verb_offers: &[("workweave delete", Form::C13WorkweaveDelete)],
            },
            Self::EditsMidOperation => &StateRow {
                axis: Axis::Edits,
                name: "mid-operation",
                means: "a rebase, merge, cherry-pick or bisect is in progress, named",
                refused_by: "`rwv sync` and `rwv sync-to`, for a repo they would rebase or \
                             fast-forward",
                offers: &[Form::C32],
                verb_offers: &[],
            },
            Self::EditsLockFilesUncommitted => &StateRow {
                axis: Axis::Edits,
                name: "rwv.toml or rwv.lock uncommitted",
                means: "the project repo's only uncommitted changes are to `rwv.toml` or \
                        `rwv.lock`, or either is not yet tracked",
                refused_by: "`rwv push`",
                offers: &[Form::C5Lock],
                verb_offers: &[],
            },
            Self::CheckoutInAnotherWorkspace => &StateRow {
                axis: Axis::Checkout,
                name: "in another workspace",
                means: "a workweave's checkout derives from the clone, named",
                refused_by: "`rwv member remove --delete` in the primary, and replay's prune \
                             of a dropped member there",
                offers: &[Form::C16Switch, Form::C16WorkweaveDelete],
                verb_offers: &[],
            },
            Self::CheckoutInRawWorktree => &StateRow {
                axis: Axis::Checkout,
                name: "in a raw worktree",
                means: "a worktree no workweave records derives from the clone, named by path",
                refused_by: "`rwv member remove --delete` in the primary, and replay's prune \
                             of a dropped member there",
                offers: &[Form::C16Raw],
                verb_offers: &[],
            },
            Self::PushBranchDiverged => &StateRow {
                axis: Axis::PushBranch,
                name: "diverged",
                means: "the branch and its upstream each hold commits the other lacks, counted \
                        on the line",
                refused_by: "`rwv pull`, for the project branch",
                offers: &[Form::C2Pull, Form::C3Pull],
                verb_offers: &[],
            },
            Self::PinNone => &StateRow {
                axis: Axis::Pin,
                name: "none",
                means: "the lock has no entry for the member, or there is no lock file; the \
                        checkout is on the declared branch at the tip of the remote's branch",
                refused_by: "`rwv clone`, which does not refuse: it completes, lists the member \
                             and exits with status 3",
                offers: &[Form::C25, Form::C1UpdateRemote],
                verb_offers: &[],
            },
            Self::PinUnreachable => &StateRow {
                axis: Axis::Pin,
                name: "unreachable",
                means: "the lock pins a commit that no ref in the clone reaches; the checkout \
                        is on the declared branch at the tip of the remote's branch",
                refused_by: "`rwv clone`, which does not refuse: it completes, lists the member \
                             and exits with status 3",
                offers: &[Form::C17, Form::C25, Form::C19],
                verb_offers: &[],
            },
            Self::NameTaken => &StateRow {
                axis: Axis::Name,
                name: "taken",
                means: "the project already records a workweave by this name at another path, \
                        or the directory the workweave would take is another workweave's",
                refused_by: "`rwv workweave create`",
                offers: &[Form::AnotherName, Form::C9WorkweaveDelete],
                verb_offers: &[],
            },
            Self::NameARevision => &StateRow {
                axis: Axis::Name,
                name: "a revision",
                means: "the branch `rwv member add` would declare reads as a commit id (seven \
                        or more hex digits) or a release tag (`v2.0`, `v0.3.4-rc1`)",
                refused_by: "`rwv member add`, for the remote's default branch or, with \
                             `--new`, the branch the new repo is on",
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
                             branch `--against` names",
                offers: &[Form::C33],
                verb_offers: &[],
            },
            Self::PublishWithheld => &StateRow {
                axis: Axis::Publish,
                name: "withheld",
                means: "the remote rejected this member's push, so the project repo was not \
                        pushed; what the remote answered is listed",
                refused_by: "`rwv push`",
                offers: &[Form::C7Push, Form::C17],
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
                refused_by: "`rwv sync` and `rwv sync-to`, when the project does not load after \
                             the project repo is replayed; `rwv doctor` reports it for every \
                             project",
                offers: &[],
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
        }
    }

    /// Start judging `repo` in this state.
    pub fn of(self, repo: impl fmt::Display) -> Judging {
        Judging {
            repo: repo.to_string(),
            state: self,
            subject: None,
            listed: Vec::new(),
            facts: Vec::new(),
            workspace: None,
            verb: None,
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
    C13,
    C14,
    C15,
    C16,
    C17,
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
    DoctorFix,
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
                phrase: "publish it (again)",
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
            Self::C17 => &ChoiceRow {
                id: Some("C17"),
                phrase: "choose where it pushes and pulls",
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
                phrase: "say how `<T>` accepts landings",
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
                phrase: "merge `<rev>` in as a merge commit",
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
                phrase: "land these changes too",
                keeps_work: "only named entries change",
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
                phrase: "land the dependent into `<W>` first (a landed one: retire it)",
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
                phrase: "pick it against parent `<n>`",
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
            Self::DoctorFix => &ChoiceRow {
                id: None,
                phrase: "rewrite the keys an earlier release wrote, committing only that",
                keeps_work: "commits only the rewrite; other edits stay uncommitted",
            },
        }
    }
}

/// Where a form runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Here,
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
            Self::OtherCheckout => "<other>",
            _ => return None,
        };
        places.get(placeholder).cloned()
    }
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
}

/// One spelling of a choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Form {
    C1Pull,
    C1UpdateRemote,
    C2Pull,
    C2UpdateRemote,
    C3Pull,
    C3UpdateRemote,
    C5Lock,
    C5LockRepo,
    C6,
    C7Push,
    C7PushRepo,
    C7PushBranch,
    C9BranchDelete,
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
    C12Feature,
    C12Declared,
    C13,
    C13WorkweaveDelete,
    C14Keep,
    C14Return,
    C15,
    C15AtCherryPick,
    C16Switch,
    C16WorkweaveDelete,
    C16Raw,
    C17,
    C18,
    C19,
    C22,
    C23SetHistory,
    C23Rerun,
    C24,
    C25,
    C26,
    C27,
    C30Switch,
    C30BranchCreate,
    C31,
    C32,
    C33,
    C34,
    C35,
    C35Retire,
    C36,
    C37BranchCreate,
    C37WorkweaveCreate,
    C38,
    C39,
    C40,
    AnotherName,
    DoctorFix,
}

/// A form's choice, steps and Where, and whether its verb has shipped.
pub struct FormRow {
    pub choice: ChoiceId,
    pub steps: &'static [Step],
    pub at: Where,
    pub shipped: bool,
}

impl Form {
    pub const ALL: [Form; 62] = [
        Self::C1Pull,
        Self::C1UpdateRemote,
        Self::C2Pull,
        Self::C2UpdateRemote,
        Self::C3Pull,
        Self::C3UpdateRemote,
        Self::C5Lock,
        Self::C5LockRepo,
        Self::C6,
        Self::C7Push,
        Self::C7PushRepo,
        Self::C7PushBranch,
        Self::C9BranchDelete,
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
        Self::C12Feature,
        Self::C12Declared,
        Self::C13,
        Self::C13WorkweaveDelete,
        Self::C14Keep,
        Self::C14Return,
        Self::C15,
        Self::C15AtCherryPick,
        Self::C16Switch,
        Self::C16WorkweaveDelete,
        Self::C16Raw,
        Self::C17,
        Self::C18,
        Self::C19,
        Self::C22,
        Self::C23SetHistory,
        Self::C23Rerun,
        Self::C24,
        Self::C25,
        Self::C26,
        Self::C27,
        Self::C30Switch,
        Self::C30BranchCreate,
        Self::C31,
        Self::C32,
        Self::C33,
        Self::C34,
        Self::C35,
        Self::C35Retire,
        Self::C36,
        Self::C37BranchCreate,
        Self::C37WorkweaveCreate,
        Self::C38,
        Self::C39,
        Self::C40,
        Self::AnotherName,
        Self::DoctorFix,
    ];

    pub fn row(self) -> &'static FormRow {
        use ChoiceId as C;
        use Step::{Rerun, Rwv, Then, Vcs};
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
            Self::C2Pull => &FormRow {
                choice: C::C2,
                steps: &[Rwv("rwv pull --merge-upstream")],
                at: W::WithBranch,
                shipped: false,
            },
            Self::C2UpdateRemote => &FormRow {
                choice: C::C2,
                steps: &[Rwv("rwv update --remote --merge-upstream --repo <m>")],
                at: W::WithBranch,
                shipped: false,
            },
            Self::C3Pull => &FormRow {
                choice: C::C3,
                steps: &[Rwv("rwv pull --reset-to-upstream <repo>")],
                at: W::WithBranch,
                shipped: false,
            },
            Self::C3UpdateRemote => &FormRow {
                choice: C::C3,
                steps: &[Rwv("rwv update --remote --reset-to-upstream <repo>")],
                at: W::WithBranch,
                shipped: false,
            },
            Self::C5Lock => &FormRow {
                choice: C::C5,
                steps: &[Rwv("rwv lock --commit")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C5LockRepo => &FormRow {
                choice: C::C5,
                steps: &[Rwv("rwv lock --repo <m> --commit")],
                at: W::WithBranch,
                shipped: false,
            },
            Self::C6 => &FormRow {
                choice: C::C6,
                steps: &[Rwv("rwv update")],
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
                shipped: false,
            },
            Self::C9BranchDelete => &FormRow {
                choice: C::C9,
                steps: &[Rwv("rwv branch delete <W>")],
                at: W::BranchNotCheckedOut,
                shipped: false,
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
                shipped: false,
            },
            Self::C10Adopt => &FormRow {
                choice: C::C10,
                steps: &[Rwv("rwv branch rename <new> --adopt")],
                at: W::Here,
                shipped: false,
            },
            Self::C11SetBranch => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member set-branch <m> <b>")],
                at: W::WithBranch,
                shipped: false,
            },
            Self::C11SetBranchNew => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member set-branch <m> <b> --new")],
                at: W::WithBranch,
                shipped: false,
            },
            Self::C11SetUrl => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member set-url <m> <url>")],
                at: W::WithBranch,
                shipped: false,
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
                shipped: false,
            },
            Self::C11UnsetHistory => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv branch set-history --unset --repo <m>")],
                at: W::WithBranch,
                shipped: false,
            },
            Self::C11MemberAdd => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member add <url>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C11MemberRemove => &FormRow {
                choice: C::C11,
                steps: &[Rwv("rwv member remove <m>")],
                at: W::WithBranch,
                shipped: true,
            },
            Self::C12Feature => &FormRow {
                choice: C::C12,
                steps: &[Rwv("rwv branch set-mode <W> feature --target <T>")],
                at: W::Any,
                shipped: false,
            },
            Self::C12Declared => &FormRow {
                choice: C::C12,
                steps: &[Rwv("rwv branch set-mode <W> declared")],
                at: W::Any,
                shipped: false,
            },
            Self::C13 => &FormRow {
                choice: C::C13,
                steps: &[Vcs(A::Commit), Vcs(A::Stash)],
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
                steps: &[Vcs(A::CherryPick), Then(Self::C40)],
                at: W::WhereBranchCheckedOut,
                shipped: true,
            },
            Self::C16Switch => &FormRow {
                choice: C::C16,
                steps: &[Rwv("rwv switch <other>")],
                at: W::OtherCheckout,
                shipped: false,
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
            Self::C17 => &FormRow {
                choice: C::C17,
                steps: &[
                    Vcs(A::SetPushRemote),
                    Vcs(A::SetPushDefault),
                    Vcs(A::SetDefaultRemote),
                    Vcs(A::AddRemote),
                    Vcs(A::UnsetUpstream),
                ],
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
                shipped: false,
            },
            Self::C23SetHistory => &FormRow {
                choice: C::C23,
                steps: &[Rwv("rwv branch set-history <h>")],
                at: W::OnTarget,
                shipped: false,
            },
            Self::C23Rerun => &FormRow {
                choice: C::C23,
                steps: &[Rerun("--history <h>")],
                at: W::Here,
                shipped: false,
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
                shipped: false,
            },
            Self::C26 => &FormRow {
                choice: C::C26,
                steps: &[Vcs(A::MergeNoFastForward)],
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
                shipped: false,
            },
            Self::C30BranchCreate => &FormRow {
                choice: C::C30,
                steps: &[Rwv("rwv branch create <W>")],
                at: W::Here,
                shipped: false,
            },
            Self::C31 => &FormRow {
                choice: C::C31,
                steps: &[Rwv("rwv land --allow-member-change <m>")],
                at: W::WithFeature,
                shipped: false,
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
                shipped: false,
            },
            Self::C35 => &FormRow {
                choice: C::C35,
                steps: &[Rwv("rwv land")],
                at: W::DependentsWorkspace,
                shipped: false,
            },
            Self::C35Retire => &FormRow {
                choice: C::C35,
                steps: &[Rwv("rwv land --retire")],
                at: W::DependentsWorkspace,
                shipped: false,
            },
            Self::C36 => &FormRow {
                choice: C::C36,
                steps: &[Rwv("rwv sync")],
                at: W::WithFeature,
                shipped: false,
            },
            Self::C37BranchCreate => &FormRow {
                choice: C::C37,
                steps: &[Rwv("rwv branch create <W>")],
                at: W::Here,
                shipped: false,
            },
            Self::C37WorkweaveCreate => &FormRow {
                choice: C::C37,
                steps: &[Rwv("rwv workweave create <ww>")],
                at: W::Here,
                shipped: false,
            },
            Self::C38 => &FormRow {
                choice: C::C38,
                steps: &[Rwv("rwv cherry-pick <rev> -m <n>")],
                at: W::Here,
                shipped: false,
            },
            Self::C39 => &FormRow {
                choice: C::C39,
                steps: &[Vcs(A::Revert), Then(Self::C40)],
                at: W::WhereBranchCheckedOut,
                shipped: true,
            },
            Self::C40 => &FormRow {
                choice: C::C40,
                steps: &[Rwv("rwv cherry-pick <rev> --except <m>")],
                at: W::Here,
                shipped: false,
            },
            Self::AnotherName => &FormRow {
                choice: C::AnotherName,
                steps: &[],
                at: W::Here,
                shipped: true,
            },
            Self::DoctorFix => &FormRow {
                choice: C::DoctorFix,
                steps: &[Rwv("rwv doctor --fix")],
                at: W::Here,
                shipped: true,
            },
        }
    }

    /// Whether this form may be printed: its verb has shipped, and so has
    /// every form it runs after.
    pub fn printable(self) -> bool {
        let row = self.row();
        row.shipped
            && row.steps.iter().all(|step| match step {
                Step::Then(next) => next.printable(),
                Step::Rwv(_) | Step::Vcs(_) | Step::Rerun(_) => true,
            })
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
                Step::Vcs(_) | Step::Rerun(_) => Vec::new(),
            })
            .collect()
    }

    fn lines(self, vcs: &dyn Vcs) -> Vec<String> {
        self.row()
            .steps
            .iter()
            .flat_map(|step| match step {
                Step::Rwv(command) => vec![(*command).to_owned()],
                Step::Vcs(action) => vec![vcs.hint(*action).to_owned()],
                Step::Then(next) => next.lines(vcs),
                Step::Rerun(flag) => vec![format!("rerun with {flag}")],
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub kind: FactKind,
    pub about: Option<String>,
}

/// A repo being judged: its state and what the state names, and the values
/// its choices' placeholders take.
#[must_use]
pub struct Judging {
    repo: String,
    state: State,
    subject: Option<String>,
    listed: Vec<String>,
    facts: Vec<Fact>,
    workspace: Option<PathBuf>,
    verb: Option<&'static str>,
    bindings: BTreeMap<&'static str, String>,
    places: BTreeMap<&'static str, PathBuf>,
}

impl Judging {
    /// What the state names: the operation in progress, the name taken.
    pub fn subject(mut self, subject: impl fmt::Display) -> Self {
        self.subject = Some(subject.to_string());
        self
    }

    /// The verb refusing, so the state's forms that belong to that verb alone
    /// are offered too.
    pub fn by_verb(mut self, verb: &'static str) -> Self {
        self.verb = Some(verb);
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

    pub fn checkout(self, dir: &Path) -> Self {
        self.bind("<checkout>", dir.display())
    }

    /// The directory `placeholder` names, where a choice that runs there is
    /// printed as running.
    pub fn bind_place(mut self, placeholder: &'static str, dir: &Path) -> Self {
        self.places.insert(placeholder, dir.to_path_buf());
        self.bind(placeholder, dir.display())
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

    /// The judged line, its choices spelled by `vcs`.
    pub fn judge(self, vcs: &dyn Vcs) -> Judged {
        let row = self.state.row();
        let verb_offers = row
            .verb_offers
            .iter()
            .filter(|(verb, _)| Some(*verb) == self.verb)
            .map(|(_, form)| form);
        let choices = row
            .offers
            .iter()
            .chain(verb_offers)
            .filter(|form| form.printable())
            .map(|form| Offered {
                choice: form.row().choice,
                at: form.row().at.at(&self.places),
                run: form
                    .lines(vcs)
                    .iter()
                    .map(|line| fill(line, &self.bindings))
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

/// `template` with every bound `<placeholder>` replaced by its value, quoted
/// where it holds whitespace. An unbound one stays for the operator to fill.
fn fill(template: &str, bindings: &BTreeMap<&'static str, String>) -> String {
    bindings
        .iter()
        .fold(template.to_owned(), |text, (placeholder, value)| {
            let value = if value.contains(char::is_whitespace) {
                format!("\"{value}\"")
            } else {
                value.clone()
            };
            text.replace(placeholder, &value)
        })
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
        let facts = self.facts.iter().map(|fact| {
            let token = crate::wire_token::wire_token(&fact.kind);
            match &fact.about {
                Some(about) => format!("{token}: {about}"),
                None => token,
            }
        });
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
                        phrase: row.phrase.to_owned(),
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
    pub fn add(&mut self, judged: Judged) {
        self.lines.push(judged);
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn lines(&self) -> &[Judged] {
        &self.lines
    }

    /// Print `note` after the judged lines.
    pub fn note(&mut self, note: impl fmt::Display) {
        self.notes.push(note.to_string());
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
    fn choices(&self) -> Vec<(&'static str, Option<PathBuf>, Vec<String>)> {
        let mut out: Vec<(&'static str, Option<PathBuf>, Vec<String>)> = Vec::new();
        for judged in self.ordered() {
            for offered in &judged.choices {
                let phrase = offered.choice.row().phrase;
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
        if !choices.is_empty() {
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

    pub fn output(&self) -> Vec<JudgedOutput> {
        self.ordered().into_iter().map(Judged::output).collect()
    }
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
    let mut judging = match vcs.mid_operation(dir) {
        Some(op) => State::EditsMidOperation.of(repo).subject(op.operation()),
        None if dirty.is_empty() => return None,
        None => State::EditsDirty.of(repo).listing(dirty),
    };
    if let Some(workspace) = elsewhere {
        judging = judging.elsewhere(workspace);
    }
    Some(judging.checkout(dir).judge(vcs))
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
            let mut out = format!(
                "### `{}`\n\n{}\n\n{}",
                axis.token(),
                axis.row().condition,
                kind_sections(axis, vcs.as_ref(), &vcs_name)
            );
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
            .map(|(verb, form)| (Some(*verb), form));
        for (verb, form) in every.chain(by_verb).filter(|(_, f)| f.printable()) {
            let row = form.row();
            let choice = row.choice.row();
            let label = match choice.id {
                Some(id) => format!("{id}: {}", choice.phrase),
                None => choice.phrase.to_owned(),
            };
            let state_cell = match verb {
                None => format!("`{}`", state.name()),
                Some(verb) => format!("`{}`, for `rwv {verb}`", state.name()),
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                state_cell,
                label,
                spelled(*form, vcs, vcs_name),
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
            if state.row().offers.is_empty() {
                offering_nothing.push(state);
            }
        }
        assert_eq!(
            offering_nothing,
            [State::UnparseableProjectMalformed],
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
        assert!(Form::C15AtCherryPick.row().shipped);
        assert!(!Form::C40.printable());
        assert!(!Form::C15AtCherryPick.printable());
        assert!(Form::C15.printable());
        assert!(Form::C14Keep.printable());
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
    }

    #[test]
    fn an_axis_with_a_finding_spells_its_token_and_carries_every_sub_kind() {
        let mut with_finding = 0;
        for axis in Axis::ALL {
            let Some(finding) = axis.finding() else {
                continue;
            };
            with_finding += 1;
            assert_eq!(finding.kind.to_string(), axis.token());
            let carried = crate::check::finding_sub_kinds()
                .remove(&axis.token())
                .unwrap_or_default();
            let listed: std::collections::BTreeSet<String> =
                finding.sub_kinds.iter().map(|s| s.token.clone()).collect();
            assert_eq!(listed, carried, "{axis:?}");
        }
        assert_eq!(with_finding, 1);
    }

    #[test]
    fn a_findings_entry_opens_each_sub_kind_with_the_registers_marks() {
        let entries = finding_entries_markdown(crate::manifest::VcsType::Git);
        let [(Axis::UnparseableProject, entry)] = &entries[..] else {
            panic!("the one axis with a finding yields the one entry: {entries:?}");
        };
        assert!(entry.starts_with("### `unparseable-project`\n"), "{entry}");
        assert!(
            entry.contains("\n#### `pre-schema`\n\n**Error. Auto-fixable.** "),
            "{entry}"
        );
        assert!(
            entry.contains("\n#### `malformed`\n\n**Error. Report-only.** "),
            "{entry}"
        );
        assert!(
            !kind_entries_markdown(crate::manifest::VcsType::Git).contains("unparseable-project"),
            "the refusals page holds no entry for a kind the findings page holds"
        );
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
    fn a_state_with_nothing_shipped_prints_no_choices_block() {
        let mut report = Report::default();
        report.add(
            State::NameARevision
                .of("github/o/lib")
                .subject("v2.0")
                .judge(git().as_ref()),
        );
        let text = report.render("add");
        assert!(!text.contains("choices:"), "{text}");
        assert!(text.ends_with("\nrwv explain name\n"), "{text}");
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
