//! Refusal kinds: the stable name a deliberate refusal carries, and the
//! rendering that puts it in front of the operator.
//!
//! A refusal is rwv declining on purpose — a precondition it will not cross,
//! an input it will not accept. One that judged repo states carries a
//! [`crate::diagnosis::Report`]; one that names a condition carries a
//! [`RefusalKind`]. An environment failure, an IO error and an internal
//! invariant carry neither, and an error with no kind anywhere in its chain
//! renders exactly as it did before this module existed.
//!
//! The kind rides the error value rather than its text, so a message can be
//! rewritten without renaming the condition, and a wrapping `.context()` does
//! not lose it.

use std::fmt;

use schemars::JsonSchema;
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
/// same state. Renaming one half of such a pair splits a condition in two
/// under the reader's feet — the spellings move together or not at all.
/// `tests/source/token_register_agreement_test.rs` asserts which variants they are.
///
/// The derived schema is the register's enumeration: it lists every variant
/// under the token serde writes for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RefusalKind {
    /// A sync/land op already holds op-state covering this workspace.
    OpInProgress,
    /// A phase stopped and op-state is held there, awaiting resume or abort.
    OpParked,
    /// A name spells a character the flat rendering reserves.
    UnrenderableName,
    /// A name is not usable as a ref-name component.
    InvalidRefName,

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
    /// A checkout dropped from the lock carries commits nothing else reaches.
    DroppedRepoHasUniqueCommits,
    /// `--continue` arguments disagree with the recorded op.
    ResumeContradictsRecord,
    /// Abort found a tip the op does not account for.
    ForeignTip,
    /// No record of a rebase the op ran says which commit became which.
    ReplayUnrecorded,
    /// A savepoint the operation needs could not be written, so it did not
    /// start.
    SavepointUnfiled,

    /// The canonical clone a checkout is derived from is missing.
    MissingCanonicalClone,
    /// A clone sits in a topology rwv does not maintain.
    CloneTopology,
    /// An untracked file stands where the operation must write.
    UntrackedCollision,
    /// An op-state lease outlived the process that took it.
    DeadOpLease,
    /// A claim's owner workspace is not here to read, so whether its op
    /// stands cannot be told.
    ClaimOwnerUnreachable,
    /// A workweave lacks the worktree a member needs; a symlink in the slot
    /// counts.
    WorkweaveDrift,

    /// No project is selected and none was supplied.
    NoActiveProject,
    /// CWD is inside one project's directory while `.rwv-active` selects another.
    CwdInInactiveProject,
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
    /// The directory slot is taken by a non-addressable occupant.
    TargetDirOccupied,
    /// Two paths both claim to be this workweave.
    RegistryPathDisagreement,

    /// A marker-bearing workweave no registry entry records.
    UnregisteredWorkweave,
    /// A recorded name → path that no longer round-trips.
    StaleRegistryEntry,
    /// `.rwv-active` names a project directory that is not on disk.
    DanglingActiveProject,
    /// `.rwv-active` is empty, unreadable, or holds text that is not a project
    /// name.
    UnreadableActiveProject,
    /// A `.rwv-workweave` marker in the YAML shape `--fix` migrates.
    LegacyWorkweaveMarker,
    /// A `.rwv-workweave` marker that parses as neither current nor
    /// migratable.
    UnreadableMarker,
    /// A marker's `primary:` names no workspace root.
    DanglingPrimary,
    /// A weave root carries both the marker and the active-project pointer.
    WeaveRootIdentityConflict,
    /// A checkout inside a workweave is a full clone rather than a worktree of
    /// the canonical clone, and the verb will not remove it.
    StandaloneInWorkweave,
    /// No registry recognizes the argument, in either direction.
    NoMatchingRegistry,
    /// A repo path is not `registry/owner/repo`.
    MalformedRepoPath,
    /// A repo path spells `\`.
    BackslashInRepoPath,
    /// A repo path spells the project repo's key, `(project)`.
    ReservedRepoPath,
    /// `--provider` names a registry rwv does not have.
    UnknownRegistry,
    /// `--provider` is not `registry/owner`.
    MalformedProvider,
    /// `rwv member remove` names a path the manifest does not carry.
    RepoNotInManifest,
    /// The clone `--delete` would destroy is referenced by other projects.
    SharedCloneReferenced,
    /// The local clone has no remote, or several, or one that records a
    /// relative path: no one URL to read.
    NoRemoteUrl,
    /// `--adopt` names a path that holds no clone.
    NoCloneToAdopt,
    /// `--adopt` names a location inside neither the primary weave root nor
    /// the workweave it runs in.
    AdoptPathOutsideWeave,
    /// `--adopt` names a clone inside a workweave that is not a worktree of
    /// the primary's clone at the same member path.
    ForeignCloneInWorkweave,
    /// `--adopt` names a clone below the weave root by a directory name no
    /// member path can spell: one holding a `\`, or one that is not Unicode.
    UnspellableDirectoryName,
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
    /// `rwv explain` was given a sub-kind that heads an entry under more than
    /// one finding kind.
    AmbiguousExplainToken,
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
    /// The installed VCS tool is older than its backend's minimum release, or
    /// names no release.
    UnsupportedVcsTool,
    /// The VCS tool a backend drives cannot be started.
    MissingVcsTool,
    /// The settings file `rwv setup claude` edits is absent.
    ClaudeSettingsMissing,
    /// Something rwv did not create sits where a link belongs.
    SurfacingPathOccupied,
    /// A switch away from a project whose tool write at the weave root
    /// conflicts with that project's own copy.
    ConflictingRootWrite,
    /// Per-item failures stopped a run, and the artifact it would have
    /// written is withheld.
    PartialRunAborted,

    /// A creation address's registry declares a required parameter the
    /// supplied map has no entry for.
    MissingCreationParam,
    /// A creation parameter's value cannot be used as given — a `root` that
    /// is not an absolute path, does not exist, one inside the weave, a name
    /// the registry does not declare, or one supplied through more than one spelling (the address
    /// shorthand, `--param`, `--params-json`).
    UnusableCreationParam,
    /// A creation address's placement already holds a different URL.
    OccupiedPlacement,
    /// The placement already holds a clone whose origin names another repo.
    ForeignCloneAtPlacement,
    /// The placement holds a directory or file that is not a clone.
    NonCloneAtPlacement,
    /// `--provider` names a registry that cannot mint a clone URL from an
    /// owner and a project name alone.
    ProviderCannotMintUrl,
}

impl RefusalKind {
    /// The token an operator reads and `rwv explain` resolves.
    pub fn token(self) -> String {
        crate::wire_token::wire_token(&self)
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

/// Why an error carries no [`RefusalKind`]. Every error rwv mints outside the
/// refusal constructors names one, through [`untokened!`](crate::untokened).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Untokened {
    /// A VCS, filesystem or tool call failed: rwv was stopped, it did not
    /// decline.
    EnvironmentPassthrough,
    /// An integration's own failure, which the operator reads as that
    /// integration's report.
    IntegrationChannel,
    /// Reaching it is a bug; there is no operator exit to document.
    InternalInvariant,
    /// A count of failures already reported one by one; nothing was withheld.
    Tally,
    /// Malformed input from the harness that invoked a hook, not from an
    /// operator.
    HookInput,
    /// An argument rejected before any verb runs.
    ArgvShim,
    /// rwv declined on purpose and the refusal has no token yet. Giving it one
    /// is owed.
    Owed,
}

/// `anyhow!` for an error outside the refusal class, naming its
/// [`Untokened`] class. It renders exactly as `anyhow!` does.
#[macro_export]
macro_rules! untokened {
    ($class:ident, $($arg:tt)+) => {{
        let _: $crate::refusal::Untokened = $crate::refusal::Untokened::$class;
        ::anyhow::anyhow!($($arg)+)
    }};
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
        return workweave_name_kind(e);
    }
    if let Some(e) = link.downcast_ref::<crate::naming::ProjectNameError>() {
        return project_name_kind(e);
    }
    if let Some(e) = link.downcast_ref::<crate::naming::RefNameError>() {
        return ref_name_kind(e);
    }
    if let Some(e) = link.downcast_ref::<crate::manifest::RepoPathError>() {
        return Some(repo_path_kind(e));
    }
    if let Some(e) = link.downcast_ref::<crate::selector::FilterError>() {
        return Some(filter_kind(e));
    }
    None
}

fn workweave_name_kind(e: &crate::naming::WorkweaveNameError) -> Option<RefusalKind> {
    use crate::naming::WorkweaveNameError as E;
    match e {
        E::Slash(_) | E::AmbiguousDelimiter(_) => Some(RefusalKind::UnrenderableName),
        E::InvalidRef(inner) => ref_name_kind(inner),
    }
}

fn project_name_kind(e: &crate::naming::ProjectNameError) -> Option<RefusalKind> {
    use crate::naming::ProjectNameError as E;
    match e {
        E::AmbiguousDelimiter(_) | E::EncodedSeparator(_) => Some(RefusalKind::UnrenderableName),
        E::InvalidRef(inner) => ref_name_kind(inner),
    }
}

fn repo_path_kind(e: &crate::manifest::RepoPathError) -> RefusalKind {
    use crate::manifest::RepoPathError as E;
    match e {
        E::Backslash(_) => RefusalKind::BackslashInRepoPath,
        E::ProjectRepo => RefusalKind::ReservedRepoPath,
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

/// A revision-shaped name carries no kind: it is refused where it is read, as
/// the `name` state of the repo it would have named, and a site that lets it
/// reach the terminal raw has dropped that repo.
fn ref_name_kind(e: &crate::naming::RefNameError) -> Option<RefusalKind> {
    use crate::naming::RefNameError as E;
    match e {
        E::ShaShaped(_) | E::TagShaped(_) => None,
        E::Empty | E::Malformed { .. } => Some(RefusalKind::InvalidRefName),
    }
}

/// The tokens `rwv explain` serves for `err`: each distinct axis a report
/// judged, else the one kind it carries, else none.
pub fn routes(err: &anyhow::Error) -> Vec<String> {
    match crate::diagnosis::report_of(err) {
        Some(report) => report.kinds().into_iter().map(|k| k.token()).collect(),
        None => kind_of(err).map(RefusalKind::token).into_iter().collect(),
    }
}

/// Everything a refused `rwv <verb>` writes to stderr, as bytes.
///
/// A refusal that judged repos prints each judged line under `rwv <verb>:`,
/// any context wrapped above it first, then its choices and a route line per
/// axis. Otherwise an error carrying no kind goes through `Debug`, which is
/// the formatter the runtime's own reporter uses, so it produces the same
/// bytes here as it did when `main` returned a `Result` — the backtrace
/// `RUST_BACKTRACE` asks for included. A refusal prints the same headline and
/// chain without one: it was declined on purpose, and where in rwv it was
/// declined is no help to the operator it routes. The route line is a bare
/// command and nothing else: a reader who has learned to skip it must be able
/// to skip it on shape alone.
pub fn render(err: &anyhow::Error, verb: &str) -> String {
    render_for(err, verb, None)
}

/// [`render`], each printed choice spelled for `project` where its verb
/// takes `--project`.
pub fn render_for(
    err: &anyhow::Error,
    verb: &str,
    project: Option<&crate::manifest::ProjectName>,
) -> String {
    if let Some(report) = spelled_report(err, project) {
        let prefix = if verb.is_empty() {
            "rwv: ".to_owned()
        } else {
            format!("rwv {verb}: ")
        };
        let mut out: String = crate::diagnosis::contexts_above_report(err)
            .into_iter()
            .map(|context| format!("{prefix}{context}\n"))
            .collect();
        out.push_str(&report.render(verb));
        return out;
    }
    let kind = kind_of(err);
    let body = match kind {
        Some(_) => headline_and_causes(err),
        None => format!("{err:?}"),
    };
    let mut out = format!("Error: {body}\n");
    if let Some(kind) = kind {
        out.push_str(&format!("\nrwv explain {}\n", kind.token()));
    }
    out
}

/// `err`'s `Debug` rendering up to where anyhow appends a captured backtrace:
/// the headline, then each cause under `Caused by:`, numbered when there are
/// several, every line of a cause indented under its first.
fn headline_and_causes(err: &anyhow::Error) -> String {
    let mut links = err.chain().map(ToString::to_string);
    let mut out = links.next().unwrap_or_default();
    let causes: Vec<String> = links.collect();
    if causes.is_empty() {
        return out;
    }
    out.push_str("\n\nCaused by:");
    let numbered = causes.len() > 1;
    for (n, cause) in causes.iter().enumerate() {
        let (first, rest) = if numbered {
            (format!("{n: >5}: "), "       ")
        } else {
            ("    ".to_owned(), "    ")
        };
        for (i, line) in cause.split('\n').enumerate() {
            out.push('\n');
            out.push_str(if i == 0 { &first } else { rest });
            out.push_str(line);
        }
    }
    out
}

/// [`render`], to stderr; and, for a `--json` invocation refused on the
/// repos it judged before it printed anything, those repos to stdout.
fn spelled_report<'e>(
    err: &'e anyhow::Error,
    project: Option<&crate::manifest::ProjectName>,
) -> Option<std::borrow::Cow<'e, crate::diagnosis::Report>> {
    let report = crate::diagnosis::report_of(err)?;
    Some(match project {
        Some(project) => std::borrow::Cow::Owned(report.into_owned().for_project(project)),
        None => report,
    })
}

static SPELLED_FOR: std::sync::OnceLock<crate::manifest::ProjectName> = std::sync::OnceLock::new();

/// Spell every choice this process prints for `project`: the verb acts for a
/// project the workspace its choices run in does not present.
pub(crate) fn spell_choices_for(project: crate::manifest::ProjectName) {
    let _ = SPELLED_FOR.set(project);
}

pub(crate) fn choices_spelled_for() -> Option<&'static crate::manifest::ProjectName> {
    SPELLED_FOR.get()
}

pub fn report(
    err: &anyhow::Error,
    verb: &str,
    output: crate::parallel::OutputMode,
    project: Option<&crate::manifest::ProjectName>,
) {
    if let Some(report) = spelled_report(err, project) {
        if output.is_json() && !crate::stdout::written() {
            let envelope = crate::diagnosis::RefusalJsonOutput {
                schema: crate::diagnosis::REFUSAL_SCHEMA_URL.to_owned(),
                refused: report.output(),
            };
            if let Ok(out) = serde_json::to_string_pretty(&envelope) {
                outln!("{out}");
            }
        }
    }
    eprint!("{}", render_for(err, verb, project));
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn the_token_is_the_kebab_case_variant_name() {
        assert_eq!(RefusalKind::OpInProgress.token(), "op-in-progress");
        assert_eq!(RefusalKind::HeadNotOnBranch.token(), "head-not-on-branch");
        assert_eq!(RefusalKind::InvalidRefName.token(), "invalid-ref-name");
    }

    /// Where a refusal names a state doctor already names, the two spell it
    /// the same. Compared against `kind` rather than a literal: a literal
    /// agrees with whatever it was typed as, and the pair drifting apart is
    /// exactly the failure this exists to catch.
    #[test]
    fn a_shared_condition_spells_one_token_in_both_registers() {
        use crate::check::CheckViolation;
        use crate::manifest::ProjectName;

        let project = ProjectName::new("proj").expect("fixture name must validate");

        let pairs: [(RefusalKind, CheckViolation); 1] = [(
            RefusalKind::LegacyManifestFormat,
            CheckViolation::LegacyManifestFormat {
                project,
                legacy_path: std::path::PathBuf::from("/ws/projects/proj/rwv.yaml"),
            },
        )];

        for (kind, finding) in pairs {
            assert_eq!(kind.token(), finding.kind().to_string());
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
            render(&err, ""),
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
            render(&err, ""),
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
        assert!(!render(&err, "").contains("rwv explain"));
    }

    #[test]
    fn a_wrapped_refusal_routes_once_to_the_inner_kind() {
        let err = Err::<(), _>(refusal(RefusalKind::OpInProgress, "an op is in flight"))
            .context("materialize does not start while an operation is in flight")
            .unwrap_err();

        assert_eq!(
            render(&err, ""),
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
        let rendered = render(&two_kinds_in_one_chain(), "");
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
        assert_eq!(kind_of(&pinned), None);

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

        let reserved = anyhow::Error::new(RepoPathError::ProjectRepo);
        assert_eq!(kind_of(&reserved), Some(RefusalKind::ReservedRepoPath));
    }

    /// Against anyhow's own `Debug`, which is what an unkinded error prints:
    /// the two agree on every byte before the backtrace anyhow may append.
    #[test]
    fn a_refusal_renders_anyhows_headline_and_chain() {
        let none = anyhow::anyhow!("one\ntwo");
        let one = Err::<(), _>(anyhow::anyhow!("root\ncause"))
            .context("outer")
            .unwrap_err();
        let many = Err::<(), _>(anyhow::anyhow!("root\nsecond line"))
            .context("middle")
            .context("outer\nheadline")
            .unwrap_err();
        for err in [none, one, many] {
            let debug = format!("{err:?}");
            let ours = headline_and_causes(&err);
            let rest = debug
                .strip_prefix(&ours)
                .unwrap_or_else(|| panic!("{ours:?} is not a prefix of {debug:?}"));
            assert!(
                rest.is_empty() || rest.starts_with("\n\nStack backtrace:"),
                "{rest:?}"
            );
        }
    }
}
