//! Refusal kinds: the stable name a deliberate refusal carries, and the
//! rendering that puts it in front of the operator.
//!
//! A refusal is rwv declining on purpose — a precondition it will not cross,
//! an input it will not accept. Those get a [`RefusalKind`]; an environment
//! failure, an IO error and an internal invariant do not, and an error with no
//! kind anywhere in its chain renders exactly as it did before this module
//! existed.
//!
//! The kind rides the error value rather than its text, so a message can be
//! rewritten without renaming the condition, and a wrapping `.context()` does
//! not lose it.

use std::fmt;

use serde::Serialize;

/// The condition a refusal names.
///
/// Fieldless: a kind identifies a condition, and the detail belongs in the
/// message. Adding a variant here is minting operator-visible surface — the
/// token is versioned, and a rename is a break.
///
/// Some variants deliberately mint a token a `rwv doctor` finding or a
/// `VcsError` already publishes, because a token names one condition wherever
/// it appears and a refusal that reports the state a finding reports is that
/// same state. Those are marked below, and renaming one half of such a pair
/// splits a condition in two under the reader's feet — the spellings move
/// together or not at all. `docs/reference/doctor-findings.md` carries the
/// finding side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RefusalKind {
    /// A sync/sync-to op already holds op-state covering this workspace.
    OpInProgress,
    /// A phase stopped and op-state is held there, awaiting resume or abort.
    OpParked,
    /// `rwv push` was invoked from a workweave.
    PushFromWorkweave,
    /// A name spells a character the flat rendering reserves.
    UnrenderableName,
    /// A name is not usable as a ref-name component.
    InvalidRefName,
    /// `version:` names a revision where it must name a branch.
    VersionIsAPin,

    /// A checkout the verb must move, record or delete carries uncommitted
    /// tracked changes.
    DirtyCheckout,
    /// The dirty-state read failed, so the operation fails closed.
    UnreadableStatus,
    /// The atomic op-state create lost to a peer whose record then vanished.
    OpAcquisitionRaced,
    /// `--continue` or `abort` where no op-state exists.
    NoOpRecorded,
    /// The remote publishes no default branch.
    NoRemoteDefaultBranch,
    /// HEAD is detached, unborn or unreadable where the verb must land or move.
    HeadNotOnBranch,
    /// The project repo is attached to a non-canonical branch.
    ProjectRepoOffCanonicalBranch,
    /// Manifest repos disagree with the committed lock.
    LockStateMismatch,
    /// Repos in the push plan are not on a pushable branch.
    UnpushableRepoBranch,
    /// The manifest push failed, so the lock carrier is deliberately
    /// unpublished.
    ProjectPushWithheld,
    /// Preflight: tips are not in the fast-forward relation the strategy needs.
    NotFastForwardable,
    /// Live worktrees or standing receipts claim the store a delete would
    /// destroy.
    StoreStillClaimed,
    /// The records that would prove a store unclaimed could not be read, so
    /// the delete fails closed.
    StoreClaimsUnreadable,
    /// A checkout dropped from the lock carries commits nothing else reaches.
    DroppedRepoHasUniqueCommits,
    /// A sync-to target's committed lock is behind its HEAD.
    ///
    /// Separate from [`Self::StaleLock`] because `--allow-stale-lock` is
    /// deliberately not offered here, and one page cannot name a remedy that
    /// works at one of the two sites.
    TargetLockBehind,
    /// `--continue` arguments disagree with the recorded op.
    ResumeContradictsRecord,
    /// `--retire`'s merged-check found divergence from the target.
    RetireNotConverged,
    /// Abort found a tip the op does not account for.
    ForeignTip,
    /// At advance time the target carries commits CWD's tip does not.
    ///
    /// Separate from [`Self::NotFastForwardable`]: past replay the exit is
    /// abort rather than a different strategy.
    TargetDivergedMidOp,

    /// A workweave's recorded parent is gone. Shared with the finding.
    DanglingParent,
    /// The replay exclusion a workweave needs is absent or legacy-spelled.
    /// Shared with the finding.
    MissingReplayExclusion,
    /// The canonical clone a checkout is derived from is missing. Shared with
    /// the finding.
    MissingCanonicalClone,
    /// A clone sits in a topology rwv does not maintain. Shared with the
    /// finding.
    CloneTopology,
    /// The repository is mid-rebase, mid-merge or otherwise mid-operation.
    /// Shared with the VCS error.
    MidOperation,
    /// A lock entry names a revision that does not resolve. Shared with the
    /// finding.
    UnresolvableLockEntry,
    /// The lock↔HEAD relation is not `ok`. Shared with the finding.
    StaleLock,
    /// A project directory does not parse as a project. Shared with the
    /// finding.
    UnparseableProject,
    /// An untracked file stands where the operation must write. Shared with
    /// the VCS error.
    UntrackedCollision,
    /// An op-state lease outlived the process that took it. Shared with the
    /// finding.
    DeadOpLease,

    /// No project is selected and none was supplied.
    NoActiveProject,
    /// The containment walk found no weave above CWD.
    NoWeaveRoot,
    /// The bootstrap target is neither a workspace nor empty, without consent.
    OccupiedBootstrapDir,
    /// The named project has no `projects/<name>/` here.
    ProjectDirMissing,
    /// The operation's bound project and the marker's disagree.
    MarkerBindingDisagreement,
    /// A peer process holds the state-file claim past the wait budget.
    StateClaimHeld,
    /// An attested input changed while the file was being generated.
    DerivationInputsMoved,
    /// An ownership receipt's recorded revision is not a commit id.
    ReceiptRevisionUncanonical,
    /// Attested generated files hold content rwv never accepted, no consent
    /// given.
    UnacceptedGeneratedContent,
    /// The verb is defined for the other checkout kind.
    WrongCheckoutKind,
    /// Two projects claim one weave-root surfacing name.
    SharedNameContested,
    /// A repo source parses as neither URL nor shorthand.
    UnresolvableRepoSource,
    /// The address names no recorded workweave.
    NoSuchWorkweave,
    /// A repo the create would fork has no resolvable HEAD.
    RepoWithoutCommits,
    /// The ephemeral name is held by a ref rwv holds no receipt for.
    UnownedBranchInNamespace,
    /// The receipted ref is off its recorded tip.
    OwnedBranchMoved,
    /// The index records this workweave name at another path.
    WorkweaveNameTaken,
    /// The target directory is another workweave's.
    ForeignWorkweaveInTargetDir,
    /// The directory slot is taken by a non-addressable occupant.
    TargetDirOccupied,
    /// The reuse/replace target holds work no flag here can consent to lose.
    ReplaceTargetHoldsWork,
    /// The source project directory is dirty and `--capture-dirty` was not
    /// given.
    DirtyProjectDir,
    /// A workweave holds uncommitted changes and `--discard-uncommitted` was
    /// not given.
    UncommittedWork,
    /// A workweave holds unlanded commits and `--discard-unmerged-commits` was
    /// not given.
    UnmergedCommits,
    /// Two paths both claim to be this workweave.
    RegistryPathDisagreement,

    /// A marker-bearing workweave no registry entry records. Shared with the
    /// finding.
    UnregisteredWorkweave,
    /// A recorded name → path that no longer round-trips. Shared with the
    /// finding.
    StaleRegistryEntry,
    /// `.rwv-active` names a project directory that is not on disk. Shared
    /// with the finding.
    DanglingActiveProject,
    /// A `.rwv-workweave-index` written before ownership receipts existed.
    /// Shared with the finding.
    LegacyWorkweaveIndex,
    /// A `.rwv-workweave` marker in the YAML shape `--fix` migrates. Shared
    /// with the finding.
    LegacyWorkweaveMarker,
    /// A `.rwv-workweave` marker that parses as neither current nor
    /// migratable. Shared with the finding.
    UnreadableMarker,
    /// A marker's `primary:` names no workspace root. Shared with the finding,
    /// which carries this same defect value on its wire.
    DanglingPrimary,
    /// A weave root carries both the marker and the active-project pointer.
    /// Shared with the finding.
    WeaveRootIdentityConflict,
    /// A branch in this workweave's namespace carries the pre-flat shape.
    /// Shared with the finding.
    UnmigratedEphemeralBranch,
    /// A checkout inside a workweave is a canonical store other worktrees link
    /// into. Shared with the finding.
    StandaloneInWorkweave,
    /// No registry recognises the argument, in either direction.
    NoMatchingRegistry,
    /// A repo path is not `registry/owner/repo`.
    MalformedRepoPath,
    /// A repo path spells `\`.
    BackslashInRepoPath,
    /// `--provider` names a registry rwv does not have.
    UnknownRegistry,
    /// `--provider` is not `registry/owner`.
    MalformedProvider,
    /// `rwv remove` names a path the manifest does not carry.
    RepoNotInManifest,
    /// `--frozen` and no lock file.
    MissingLock,
    /// The lock covers fewer repos than the manifest lists.
    IncompleteLock,
    /// The clone `--delete` would destroy is referenced by other projects.
    SharedCloneReferenced,
    /// The local clone has no conventional remote to read a URL from.
    NoRemoteUrl,
    /// The project directory name is taken.
    ProjectDirOccupied,
    /// The name would nest a project inside a project.
    NestedProjectName,
    /// A project directory holds the pre-TOML manifest and nothing rwv reads.
    LegacyManifestFormat,
    /// An addressing flag was given the other flag's argument shape.
    WrongAddressFlag,
    /// `-w` is not `<project>--<name>` with both halves non-empty.
    MalformedWorkweaveAddress,
    /// Two recorded pairs render one `-w` address.
    AmbiguousWorkweaveAddress,
    /// A flag has no meaning in the mode it was passed in.
    InapplicableFlag,
    /// No core verb and no `rwv-<verb>` on `$PATH`.
    UnknownVerb,
    /// `rwv explain` has no page for the name.
    NoExplainEntry,
    /// `--kind` names nothing in the doctor wire vocabulary.
    UnknownFindingKind,
    /// A role value names no role rwv accepts.
    UnknownRole,
    /// `--repo re:` or `--repo glob:` with nothing after the prefix.
    EmptySelectorPattern,
    /// A `--repo` pattern does not compile.
    UncompilableSelector,
    /// The object `--fix` re-observed before acting is gone or moved.
    RepairTargetChanged,
    /// The weave's recorded health floor is below this binary's requirement.
    HealthFloorTooLow,
    /// The settings file `rwv setup claude` edits is absent.
    ClaudeSettingsMissing,
    /// Something rwv did not create sits where a link belongs.
    SurfacingPathOccupied,
    /// Per-item failures stopped a run, and the artifact it would have
    /// written is withheld.
    PartialRunAborted,
}

impl RefusalKind {
    /// The token an operator reads and `rwv explain` resolves.
    ///
    /// Derived through `Serialize` so the `rename_all` above is the only place
    /// a variant becomes a string. A second spelling — a `match` arm, a table,
    /// a doc heading typed by hand — is a thing to keep in sync rather than a
    /// convenience.
    pub fn token(self) -> String {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(token)) => token,
            other => panic!("a fieldless enum must serialize to a string, got {other:?}"),
        }
    }
}

/// A kind attached to an error, rendering as the error it wraps.
///
/// `Display` forwards to the wrapped headline and `source` skips straight to
/// the wrapped error's own source, so this link is invisible to every
/// formatter: an `anyhow::Error` carrying one prints what it printed before it
/// was tagged. Give it a `Display` of its own and every tagged refusal grows a
/// spurious line.
#[derive(Debug)]
struct Refusal {
    kind: RefusalKind,
    inner: anyhow::Error,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.inner, f)
    }
}

impl std::error::Error for Refusal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

/// Tag `err` as a refusal of `kind`, changing nothing about what it renders.
pub fn refusing(kind: RefusalKind, err: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(Refusal { kind, inner: err })
}

/// A refusal of `kind` whose message is `message`.
pub fn refusal(kind: RefusalKind, message: impl fmt::Display) -> anyhow::Error {
    refusing(kind, anyhow::Error::msg(message.to_string()))
}

/// `bail!` for a refusal: same formatting arguments, plus the kind it carries.
#[macro_export]
macro_rules! refuse {
    ($kind:expr, $($arg:tt)*) => {
        return Err($crate::refusal::refusal($kind, format!($($arg)*)))
    };
}

/// The kind this error carries, outermost first.
///
/// A wrapping `.context()` sits above the tag rather than replacing it, so a
/// refusal wrapped by its caller still answers with the condition that
/// actually fired. Where two tags are nested, the outer one is the one the
/// operator is being routed to.
pub fn kind_of(err: &anyhow::Error) -> Option<RefusalKind> {
    err.chain().find_map(kind_of_link)
}

/// The kinds carried by typed errors, which reach the terminal through `?`
/// rather than through a tagging call.
///
/// Exhaustive per type: adding a variant to one of these stops this compiling
/// until the new condition is classified.
fn kind_of_link(link: &(dyn std::error::Error + 'static)) -> Option<RefusalKind> {
    if let Some(refusal) = link.downcast_ref::<Refusal>() {
        return Some(refusal.kind);
    }
    if let Some(e) = link.downcast_ref::<crate::naming::WorkweaveNameError>() {
        return Some(workweave_name_kind(e));
    }
    if let Some(e) = link.downcast_ref::<crate::naming::ProjectNameError>() {
        return Some(project_name_kind(e));
    }
    if let Some(e) = link.downcast_ref::<crate::naming::RefNameError>() {
        return Some(ref_name_kind(e));
    }
    if let Some(e) = link.downcast_ref::<crate::manifest::RepoPathError>() {
        return Some(repo_path_kind(e));
    }
    if let Some(e) = link.downcast_ref::<crate::selector::FilterError>() {
        return Some(filter_kind(e));
    }
    None
}

fn workweave_name_kind(e: &crate::naming::WorkweaveNameError) -> RefusalKind {
    use crate::naming::WorkweaveNameError as E;
    match e {
        E::Slash(_) | E::AmbiguousDelimiter(_) => RefusalKind::UnrenderableName,
        E::InvalidRef(inner) => ref_name_kind(inner),
    }
}

fn project_name_kind(e: &crate::naming::ProjectNameError) -> RefusalKind {
    use crate::naming::ProjectNameError as E;
    match e {
        E::AmbiguousDelimiter(_) | E::EncodedSeparator(_) => RefusalKind::UnrenderableName,
        E::InvalidRef(inner) => ref_name_kind(inner),
    }
}

fn repo_path_kind(e: &crate::manifest::RepoPathError) -> RefusalKind {
    use crate::manifest::RepoPathError as E;
    match e {
        E::Backslash(_) => RefusalKind::BackslashInRepoPath,
    }
}

/// A `--role` value reaches the operator only through here: the one production
/// producer of a bare [`crate::manifest::RoleParseError`] wraps it in
/// [`crate::selector::FilterError::UnknownRole`] on the spot, and a manifest's
/// own role value is a serde string by the time it surfaces.
fn filter_kind(e: &crate::selector::FilterError) -> RefusalKind {
    use crate::selector::FilterError as E;
    match e {
        E::UnknownRole(_) => RefusalKind::UnknownRole,
        E::EmptyPattern { .. } => RefusalKind::EmptySelectorPattern,
        E::InvalidRegex { .. } | E::InvalidGlob { .. } => RefusalKind::UncompilableSelector,
    }
}

fn ref_name_kind(e: &crate::naming::RefNameError) -> RefusalKind {
    use crate::naming::RefNameError as E;
    match e {
        E::ShaShaped(_) | E::TagShaped(_) => RefusalKind::VersionIsAPin,
        E::Empty | E::Malformed { .. } => RefusalKind::InvalidRefName,
    }
}

/// Everything a refused run writes to stderr, as bytes.
///
/// The headline and chain go through `Debug`, which is the formatter the
/// runtime's own reporter uses, so an error carrying no kind produces the same
/// bytes here as it did when `main` returned a `Result`. The route line is a
/// bare command and nothing else: a reader who has learned to skip it must be
/// able to skip it on shape alone.
pub fn render(err: &anyhow::Error) -> String {
    let mut out = format!("Error: {err:?}\n");
    if let Some(kind) = kind_of(err) {
        out.push_str(&format!("\nrwv explain {}\n", kind.token()));
    }
    out
}

/// [`render`], to stderr.
pub fn report(err: &anyhow::Error) {
    eprint!("{}", render(err));
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn the_token_is_the_kebab_case_variant_name() {
        assert_eq!(RefusalKind::OpInProgress.token(), "op-in-progress");
        assert_eq!(
            RefusalKind::PushFromWorkweave.token(),
            "push-from-workweave"
        );
        assert_eq!(RefusalKind::VersionIsAPin.token(), "version-is-a-pin");
    }

    /// Where a refusal names a state doctor already names, the two spell it
    /// the same. Compared against `wire_kind` rather than a literal: a literal
    /// agrees with whatever it was typed as, and the pair drifting apart is
    /// exactly the failure this exists to catch.
    #[test]
    fn a_shared_condition_spells_one_token_in_both_registers() {
        use crate::check::CheckViolation;
        use crate::manifest::{ProjectName, RepoPath};

        let project = ProjectName::new("proj").expect("fixture name must validate");
        let repo = RepoPath::new("github/acme/server").expect("fixture path must validate");

        let pairs: [(RefusalKind, CheckViolation); 2] = [
            (
                RefusalKind::LegacyManifestFormat,
                CheckViolation::LegacyManifestFormat {
                    project: project.clone(),
                    legacy_path: std::path::PathBuf::from("/ws/projects/proj/rwv.yaml"),
                },
            ),
            (
                RefusalKind::IncompleteLock,
                CheckViolation::IncompleteLock {
                    project,
                    repo: repo.clone(),
                },
            ),
        ];

        for (kind, finding) in pairs {
            assert_eq!(kind.token(), finding.wire_kind());
        }
    }

    #[test]
    fn tagging_does_not_change_what_an_error_renders() {
        let plain = anyhow::Error::msg("the root sentence")
            .context("the middle")
            .context("the headline");
        let expected = format!("{plain:?}");

        let tagged = refusing(RefusalKind::OpInProgress, plain);
        assert_eq!(format!("{tagged:?}"), expected);
    }

    /// The bytes, spelled out. Comparing against `format!("Error: {err:?}")`
    /// would restate the implementation and pass whatever it became; what an
    /// operator's terminal held before this module existed is a literal.
    #[test]
    fn an_untagged_error_renders_the_bytes_the_runtime_used_to_print() {
        let err = anyhow::Error::msg("the deepest cause")
            .context("the middle")
            .context("the headline");

        assert_eq!(
            render(&err),
            "Error: the headline\n\
             \n\
             Caused by:\n    \
             0: the middle\n    \
             1: the deepest cause\n"
        );
    }

    /// A headline of several lines, which is the shape a refusal takes when it
    /// carries its own advice. Nothing indents it and nothing separates its
    /// lines, where a cause's continuation lines are indented to match the
    /// first — so the blank line before `Caused by:` is the only structural
    /// break in the output, and it is what a route line would later sit after.
    #[test]
    fn a_multi_line_headline_renders_unindented_and_unbroken() {
        let err = anyhow::Error::msg("the deepest cause\nwith a second line")
            .context("the headline\nHint: try a scoped path: projects/{owner}/web-app/");

        assert_eq!(
            render(&err),
            "Error: the headline\n\
             Hint: try a scoped path: projects/{owner}/web-app/\n\
             \n\
             Caused by:\n    \
             the deepest cause\n    \
             with a second line\n"
        );
    }

    #[test]
    fn an_untagged_error_gets_no_route_line() {
        let err = anyhow::Error::msg("nothing was declined here").context("wrapped");
        assert!(!render(&err).contains("rwv explain"));
    }

    #[test]
    fn a_wrapped_refusal_routes_once_to_the_inner_kind() {
        let err = Err::<(), _>(refusal(RefusalKind::OpInProgress, "an op is in flight"))
            .context("materialize does not start while an operation is in flight")
            .unwrap_err();

        assert_eq!(
            render(&err),
            "Error: materialize does not start while an operation is in flight\n\
             \n\
             Caused by:\n    \
             an op is in flight\n\
             \n\
             rwv explain op-in-progress\n"
        );
    }

    /// A chain really carrying two kinds. A tag placed directly on a tagged
    /// error is not one: the wrapper's `source` reaches past the error it
    /// holds, so the lower tag never appears in the chain at all and nothing
    /// downstream could tell one kind from two. A `.context()` between the two
    /// is what puts both in reach — and it is the shape the resume path
    /// produces, where a decorator tags a gate error its caller already
    /// wrapped.
    fn two_kinds_in_one_chain() -> anyhow::Error {
        let wrapped = Err::<(), _>(refusal(RefusalKind::OpInProgress, "an op is in flight"))
            .context("the resumed phase re-gated its source")
            .unwrap_err();
        let doubled = refusing(RefusalKind::OpParked, wrapped);
        assert_eq!(
            doubled.chain().filter_map(kind_of_link).count(),
            2,
            "the fixture must really carry two kinds, or what follows proves nothing"
        );
        doubled
    }

    /// Two kinds in one chain still route once. A rendering that walked the
    /// chain and printed what it found would put two commands in front of a
    /// reader who can only run one.
    #[test]
    fn two_kinds_render_one_route_line() {
        let rendered = render(&two_kinds_in_one_chain());
        assert_eq!(rendered.matches("rwv explain").count(), 1);
        assert!(rendered.ends_with("\nrwv explain op-parked\n"));
    }

    #[test]
    fn the_outer_tag_wins_over_a_tag_beneath_it() {
        assert_eq!(
            kind_of(&two_kinds_in_one_chain()),
            Some(RefusalKind::OpParked)
        );
    }

    #[test]
    fn a_typed_name_error_names_its_own_condition() {
        use crate::naming::{ProjectNameError, RefNameError, WorkweaveNameError};

        let slash = anyhow::Error::new(WorkweaveNameError::Slash("a/b".into()));
        assert_eq!(kind_of(&slash), Some(RefusalKind::UnrenderableName));

        let pinned = anyhow::Error::new(WorkweaveNameError::InvalidRef(RefNameError::TagShaped(
            "v1.2.3".into(),
        )));
        assert_eq!(kind_of(&pinned), Some(RefusalKind::VersionIsAPin));

        let empty = anyhow::Error::new(RefNameError::Empty);
        assert_eq!(kind_of(&empty), Some(RefusalKind::InvalidRefName));

        let plus = anyhow::Error::new(ProjectNameError::EncodedSeparator("a+b".into()));
        assert_eq!(kind_of(&plus), Some(RefusalKind::UnrenderableName));

        let malformed = anyhow::Error::new(ProjectNameError::InvalidRef(RefNameError::Malformed {
            name: "a..b".into(),
            reason: "contains `..`",
        }));
        assert_eq!(kind_of(&malformed), Some(RefusalKind::InvalidRefName));
    }

    /// A selector error carries its own condition, and it carries it from the
    /// outermost link: the role arm holds the parse error it wraps, so a walk
    /// that read the innermost link instead would answer from a type the
    /// operator never sees named.
    ///
    /// Each value comes back from the parser rather than being assembled here,
    /// so a variant this stops constructing is a variant the parser stopped
    /// producing.
    #[test]
    fn a_selector_error_names_the_condition_of_its_own_variant() {
        use crate::selector::RepoFilter;

        let refused = |roles: &[&str], selectors: &[&str]| {
            let roles: Vec<String> = roles.iter().map(|s| (*s).to_owned()).collect();
            let selectors: Vec<String> = selectors.iter().map(|s| (*s).to_owned()).collect();
            anyhow::Error::new(
                RepoFilter::parse(&roles, &selectors).expect_err("the fixture must be refused"),
            )
        };

        let role = refused(&["bogus"], &[]);
        assert_eq!(role.chain().count(), 2, "the wrapped error is in the chain");
        assert_eq!(kind_of(&role), Some(RefusalKind::UnknownRole));

        let empty = refused(&[], &["re:"]);
        assert_eq!(kind_of(&empty), Some(RefusalKind::EmptySelectorPattern));

        let bad = refused(&[], &["re:["]);
        assert_eq!(kind_of(&bad), Some(RefusalKind::UncompilableSelector));

        let glob = refused(&[], &["glob:{"]);
        assert_eq!(kind_of(&glob), Some(RefusalKind::UncompilableSelector));
    }

    #[test]
    fn a_typed_repo_path_error_names_its_own_condition() {
        use crate::manifest::RepoPathError;

        let back = anyhow::Error::new(RepoPathError::Backslash("a\\b".into()));
        assert_eq!(kind_of(&back), Some(RefusalKind::BackslashInRepoPath));
    }
}
