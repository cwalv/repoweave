//! What is a legal rwv name, and how does one render as a single path
//! segment.
//!
//! This module has no crate-internal dependencies, deliberately. Everything
//! here is a rule about text, and every consumer — the manifest format, the
//! workspace layout, the VCS seam, the CLI address parser — sits above it and
//! reaches down.
//!
//! # The flat address, and why it is injective
//!
//! A workweave directory is named `<project>--<workweave>`. A project name may
//! be `/`-segmented (`chatly/web-app`), and the projects tree nests to match,
//! but everything downstream needs one segment instead: a directory name, a
//! `-w` address, a ref component, a `.code-workspace` filename. So `/` is
//! written as `+` on the way out.
//!
//! The rendering is injective only because two rules hold together, and
//! they are enforced in different places:
//!
//! - the renderer writes `+` for `/`, and nothing else writes `+`;
//! - the validators reject a name that already spells `+`, or that spells the
//!   `--` the two halves are joined with.
//!
//! Drop either half and two distinct addresses collapse onto one spelling: a
//! project genuinely named `a+b` and the nested `a/b` render alike, and a
//! project named `x--y` splits at its own middle. The rules ship together or
//! the rendering stops being a bijection, which is why the validators and the
//! renderer live in one file rather than agreeing across two.
//!
//! # Nothing reads a flat name back into an identity
//!
//! There is no inverse of [`weave_dir_name`] taking a string alone, and the
//! absence is the design. A caller holding the project — a `.rwv-workweave`
//! marker, a registry entry — recovers the name half exactly with
//! [`workweave_name_in`], because the split point is that project's own
//! rendered length rather than the first `--` in the string. A caller holding
//! only what an operator typed — the `-w` address — resolves it with
//! [`resolve_flat_address`], which renders what is recorded and compares.
//! Neither can invent an identity the records do not already hold, so the
//! injectivity above is what makes an address unambiguous, not what makes a
//! decode safe.

use serde::Serialize;
use std::fmt;

// ---------------------------------------------------------------------------
// The grammar, at the level of `&str`
// ---------------------------------------------------------------------------

/// Split `s` at the first workweave separator, without validating either
/// half. A caller that only wants to test for the separator's presence, or
/// that needs its own error text for an empty half, calls this instead of
/// writing `"--"` again. The halves it returns say where the separator is,
/// not whose workweave the string names.
pub fn split_at_weave_separator(s: &str) -> Option<(&str, &str)> {
    s.split_once("--")
}

/// Join an already-encoded project segment to a workweave name.
pub fn join_flat(project_segment: &str, workweave: &str) -> String {
    format!("{project_segment}--{workweave}")
}

/// Write `/` as `+`, rendering a possibly-nested name as one segment.
pub fn encode_segment(s: &str) -> String {
    s.replace('/', "+")
}

/// The inverse of [`encode_segment`].
pub fn decode_segment(s: &str) -> String {
    s.replace('+', "/")
}

/// Would `s` be ambiguous against the separator [`join_flat`] writes?
///
/// A leading or trailing `-` counts: joined to a neighbour it produces a
/// third `-` run that splits at a different offset than the one intended.
///
/// [`ProjectName::new`] and [`WorkweaveName::new`] are the callers that turn
/// this into a rejection; the encoding is a bijection only while they do.
pub fn collides_with_separator(s: &str) -> bool {
    split_at_weave_separator(s).is_some() || s.starts_with('-') || s.ends_with('-')
}

/// Does `s` spell the character [`encode_segment`] writes for `/`?
pub fn spells_segment_escape(s: &str) -> bool {
    s.contains('+')
}

// ---------------------------------------------------------------------------
// Ref-name shape
// ---------------------------------------------------------------------------

/// Why a name could not be used as a ref name.
///
/// Each variant is a distinct rejection rule so callers can report which
/// one fired instead of re-deriving it from message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefNameError {
    /// The name is empty.
    Empty,
    /// The name is commit-id shaped. `version:` declares what to TRACK;
    /// the lock records where you ARE — a pin needs a different
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

/// Reject strings that cannot name a ref.
///
/// These rules are the conservative intersection of what VCSes accept as a
/// ref name; git's `check-ref-format` is the strictest of the ones rwv
/// targets and is what this mirrors. Validating here rather than in the git
/// impl means a manifest carrying `feat/../../etc` is refused once, at parse
/// time, instead of once per VCS.
///
/// Also the ref-name-shape half of [`ProjectName::new`] and
/// [`WorkweaveName::new`], which layer their own delimiter rules on top.
pub fn validate_ref_name(s: &str) -> Result<(), RefNameError> {
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

// ---------------------------------------------------------------------------
// ProjectName
// ---------------------------------------------------------------------------

/// A project name, possibly multi-segment (e.g., `web-app` or `chatly/web-app`).
///
/// # Construction cannot be treated as infallible
///
/// ```compile_fail
/// use repoweave::manifest::ProjectName;
/// fn take(_: ProjectName) {}
/// fn f(s: String) {
///     take(ProjectName::new(s)); // E0308: expected ProjectName, found Result<ProjectName, ProjectNameError>
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectName(String);

/// Typed error returned by [`ProjectName::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectNameError {
    /// Contains `--`, or starts/ends with `-`: any of these could be
    /// confused with the `--` [`join_flat`] joins project to workweave with,
    /// letting two distinct (project, workweave) pairs render the same name.
    AmbiguousDelimiter(String),
    /// Contains `+`, which [`encode_segment`] writes in place of `/` when it
    /// renders this name as one path segment. A `+` in the name itself would
    /// decode back as a segment boundary the name never had, so two distinct
    /// projects could render the same segment.
    EncodedSeparator(String),
    /// Not usable as a (possibly `/`-segmented) ref-name component.
    InvalidRef(RefNameError),
}

impl fmt::Display for ProjectNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AmbiguousDelimiter(s) => write!(
                f,
                "'{s}' is not a valid project name: contains `--` or starts/ends \
                 with `-`, ambiguous against the `--` that joins project to workweave. \
                 See docs/reference/formats.md, \"Names, and the characters they exclude\"."
            ),
            Self::EncodedSeparator(s) => write!(
                f,
                "'{s}' is not a valid project name: contains `+`, which rwv writes in \
                 place of `/` when it renders a project name as one path segment \
                 (a workweave directory, a `-w` address, a branch name). Choose a \
                 name without `+`. See docs/reference/formats.md, \"Names, and the \
                 characters they exclude\"."
            ),
            Self::InvalidRef(e) => write!(f, "not a valid project name: {e}"),
        }
    }
}

impl std::error::Error for ProjectNameError {}

fn validate_project_name(s: &str) -> Result<(), ProjectNameError> {
    if collides_with_separator(s) {
        return Err(ProjectNameError::AmbiguousDelimiter(s.to_owned()));
    }
    if spells_segment_escape(s) {
        return Err(ProjectNameError::EncodedSeparator(s.to_owned()));
    }
    validate_ref_name(s).map_err(ProjectNameError::InvalidRef)
}

impl ProjectName {
    /// Construct a `ProjectName`, returning a [`ProjectNameError`] if `s` fails validation.
    pub fn new(s: impl Into<String>) -> Result<Self, ProjectNameError> {
        let s = s.into();
        validate_project_name(&s)?;
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for ProjectName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        validate_project_name(&s).map_err(serde::de::Error::custom)?;
        Ok(ProjectName(s))
    }
}

// ---------------------------------------------------------------------------
// WorkweaveName
// ---------------------------------------------------------------------------

/// A workweave name (e.g., `agent-42`, `hotfix`).
///
/// # Construction cannot be treated as infallible
///
/// ```compile_fail
/// use repoweave::manifest::WorkweaveName;
/// fn take(_: WorkweaveName) {}
/// fn f(s: String) {
///     take(WorkweaveName::new(s)); // E0308: expected WorkweaveName, found Result<WorkweaveName, WorkweaveNameError>
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct WorkweaveName(String);

/// Typed error returned by [`WorkweaveName::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkweaveNameError {
    /// Contains `/`. Unlike [`ProjectName`], a workweave name is never
    /// `/`-segmented, because the minted ephemeral ref name would then read
    /// back as the pre-flat segmented shape for a *different*, entirely
    /// valid, live workweave.
    Slash(String),
    /// Contains `--`, or starts/ends with `-`: any of these could be
    /// confused with the `--` that joins project to workweave, letting two
    /// distinct (project, workweave) pairs render the same name.
    AmbiguousDelimiter(String),
    /// Not usable as a ref-name component.
    InvalidRef(RefNameError),
}

impl fmt::Display for WorkweaveNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Slash(s) => write!(
                f,
                "'{s}' is not a valid workweave name: contains `/`, which would \
                 make a minted ephemeral ref name masquerade as the pre-flat \
                 segmented shape"
            ),
            Self::AmbiguousDelimiter(s) => write!(
                f,
                "'{s}' is not a valid workweave name: contains `--` or starts/ends \
                 with `-`, ambiguous against the `--` that joins project to workweave. \
                 See docs/reference/formats.md, \"Names, and the characters they exclude\"."
            ),
            Self::InvalidRef(e) => write!(f, "not a valid workweave name: {e}"),
        }
    }
}

impl std::error::Error for WorkweaveNameError {}

fn validate_workweave_name(s: &str) -> Result<(), WorkweaveNameError> {
    if s.contains('/') {
        return Err(WorkweaveNameError::Slash(s.to_owned()));
    }
    if collides_with_separator(s) {
        return Err(WorkweaveNameError::AmbiguousDelimiter(s.to_owned()));
    }
    validate_ref_name(s).map_err(WorkweaveNameError::InvalidRef)
}

impl WorkweaveName {
    /// Construct a `WorkweaveName`, returning a [`WorkweaveNameError`] if `s` fails validation.
    pub fn new(s: impl Into<String>) -> Result<Self, WorkweaveNameError> {
        let s = s.into();
        validate_workweave_name(&s)?;
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkweaveName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for WorkweaveName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        validate_workweave_name(&s).map_err(serde::de::Error::custom)?;
        Ok(WorkweaveName(s))
    }
}

// ---------------------------------------------------------------------------
// The flat address, typed
// ---------------------------------------------------------------------------

/// Render a project name as one path segment, writing `/` as `+`.
///
/// A project name may be `/`-segmented, and the projects tree nests to match.
/// Everything downstream of this function needs one segment instead: a
/// workweave directory name, the `-w` address, a ref component, a
/// `.code-workspace` filename. Rendering the `/` through unchanged makes each
/// of those a path — a nested directory, an address `-w` refuses, a ref
/// directory git will not also let be a ref file.
pub fn flat_project_segment(project: &ProjectName) -> String {
    encode_segment(project.as_str())
}

/// Build a workweave directory name using the `{project}--{name}` convention,
/// with the project half rendered as one segment.
///
/// Workweaves are keyed by the project they're created for so that the directory
/// layout makes the project explicit and `<project>--<name>` is stable across
/// fork sources.
pub fn weave_dir_name(project: &ProjectName, workweave_name: &WorkweaveName) -> String {
    join_flat(&flat_project_segment(project), workweave_name.as_str())
}

/// The workweave name half of `dir_name`, for a caller that already knows
/// whose workweave the directory is.
///
/// The project is a parameter because it is the split point: `dir_name` must
/// begin with exactly that project's own rendering followed by the separator,
/// so a project whose name contains the separator would still split at its own
/// end rather than at the first `--` in the string. A name that does not carry
/// that prefix is not this project's workweave directory and answers `None`,
/// as does one whose remainder is not a legal workweave name.
///
/// Callers hold the project as a record — a `.rwv-workweave` marker, a
/// registry entry — so the answer is what the records say the directory is
/// called, never a reading of the string on its own.
///
/// # A flat name on its own names nobody
///
/// The project parameter is the prohibition. Reading a directory name without
/// one is the shape that let `a--b--c` be read as project `a`, workweave
/// `b--c` — a guess, and at a caller that writes its answer into a record, a
/// durable one. There is no overload that omits it:
///
/// ```
/// use repoweave::manifest::ProjectName;
/// use repoweave::naming::workweave_name_in;
/// let project = ProjectName::new("chatly/web-app").unwrap();
/// let name = workweave_name_in(&project, "chatly+web-app--wtest").unwrap();
/// assert_eq!(name.as_str(), "wtest");
/// ```
///
/// ```compile_fail
/// use repoweave::manifest::WorkweaveName;
/// use repoweave::naming::workweave_name_in;
/// fn f(dir_name: &str) -> Option<WorkweaveName> {
///     workweave_name_in(dir_name) // E0061: this function takes 2 arguments
/// }
/// ```
pub fn workweave_name_in(project: &ProjectName, dir_name: &str) -> Option<WorkweaveName> {
    let prefix = join_flat(&flat_project_segment(project), "");
    WorkweaveName::new(dir_name.strip_prefix(&prefix)?).ok()
}

/// Every recorded `(project, workweave)` pair that renders `address`.
///
/// This is how a `-w` address becomes an identity: rwv renders what it has
/// recorded and keeps the pairs that match, so an address nothing recorded
/// resolves to nothing instead of minting a project name out of a string an
/// operator typed.
///
/// More than one match is a rendering collision — two recorded pairs spelled
/// alike. [`validate_project_name`] and [`validate_workweave_name`] make that
/// unreachable today, which is why the caller reports it rather than choosing:
/// picking one would be picking which of two live workweaves an operator meant.
pub fn resolve_flat_address(
    address: &str,
    recorded: &[(ProjectName, WorkweaveName)],
) -> Vec<(ProjectName, WorkweaveName)> {
    recorded
        .iter()
        .filter(|(project, name)| weave_dir_name(project, name) == address)
        .cloned()
        .collect()
}

/// Has `s` the shape of a bare flat address — no path separator, a workweave
/// separator, and a legal workweave name on its right?
///
/// A `bool`, deliberately. This screens strings an operator typed where a path
/// was expected, or a path where an address was; nothing downstream may learn
/// which project or workweave the string mentions, because the shape does not
/// say and only [`resolve_flat_address`] can.
pub fn has_flat_address_shape(s: &str) -> bool {
    if s.contains('/') || s.contains('\\') {
        return false;
    }
    split_at_weave_separator(s)
        .is_some_and(|(project, name)| !project.is_empty() && WorkweaveName::new(name).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_and_decode_round_trip_a_nested_name() {
        assert_eq!(encode_segment("chatly/web-app"), "chatly+web-app");
        assert_eq!(decode_segment("chatly+web-app"), "chatly/web-app");
    }

    #[test]
    fn the_two_metacharacters_are_rejected_by_the_validators_that_make_the_encoding_injective() {
        assert!(matches!(
            ProjectName::new("a+b"),
            Err(ProjectNameError::EncodedSeparator(_))
        ));
        assert!(matches!(
            ProjectName::new("a--b"),
            Err(ProjectNameError::AmbiguousDelimiter(_))
        ));
        assert!(matches!(
            WorkweaveName::new("a--b"),
            Err(WorkweaveNameError::AmbiguousDelimiter(_))
        ));
    }

    #[test]
    fn a_leading_or_trailing_dash_collides_with_the_separator() {
        assert!(collides_with_separator("-lead"));
        assert!(collides_with_separator("trail-"));
        assert!(!collides_with_separator("mid-dle"));
    }

    fn project(s: &str) -> ProjectName {
        ProjectName::new(s).expect("fixture project name must validate")
    }

    fn workweave(s: &str) -> WorkweaveName {
        WorkweaveName::new(s).expect("fixture workweave name must validate")
    }

    #[test]
    fn the_name_half_is_taken_at_the_projects_own_length_not_the_first_separator() {
        let nested = project("chatly/web-app");
        assert_eq!(
            workweave_name_in(&nested, "chatly+web-app--wtest"),
            Some(workweave("wtest"))
        );
        assert_eq!(
            workweave_name_in(&project("chatly"), "chatly+web-app--wtest"),
            None
        );
        assert_eq!(workweave_name_in(&project("a"), "b--seat"), None);
    }

    /// A directory whose name half is not a legal workweave name has no name
    /// half at all. `a--b--c` is the case: read without a project it splits at
    /// the first separator and offers `b--c`, which is exactly the identity
    /// `fix_unregistered_workweave` would have written into the registry.
    #[test]
    fn a_remainder_that_is_not_a_legal_workweave_name_is_not_one() {
        assert_eq!(workweave_name_in(&project("a"), "a--b--c"), None);
        assert_eq!(workweave_name_in(&project("a"), "a--"), None);
    }

    #[test]
    fn an_address_resolves_to_the_recorded_pair_that_renders_it() {
        let recorded = vec![
            (project("chatly/web-app"), workweave("wtest")),
            (project("chatly"), workweave("wtest")),
        ];
        assert_eq!(
            resolve_flat_address("chatly+web-app--wtest", &recorded),
            vec![(project("chatly/web-app"), workweave("wtest"))]
        );
        assert!(resolve_flat_address("chatly+web-app--other", &recorded).is_empty());
        assert!(resolve_flat_address("no-separator", &recorded).is_empty());
    }

    /// Two recorded pairs spelled alike are both returned, so the caller can
    /// refuse instead of picking.
    ///
    /// The pair is built past [`ProjectName::new`] on purpose: `a+b` is what
    /// [`validate_project_name`] refuses, and refusing it is the only reason
    /// the collision cannot occur. This asserts the resolver does not lean on
    /// that refusal — it reports what it is given, so the arm that reports the
    /// collision is live code and not a comment.
    #[test]
    fn two_pairs_that_render_alike_both_come_back() {
        assert!(ProjectName::new("a+b").is_err());
        let recorded = vec![
            (project("a/b"), workweave("seat")),
            (ProjectName("a+b".to_owned()), workweave("seat")),
        ];
        assert_eq!(weave_dir_name(&recorded[0].0, &recorded[0].1), "a+b--seat");
        assert_eq!(resolve_flat_address("a+b--seat", &recorded).len(), 2);
    }

    #[test]
    fn the_address_shape_predicate_answers_for_bare_names_only() {
        assert!(has_flat_address_shape("proj--seat"));
        assert!(has_flat_address_shape("chatly+web-app--seat"));
        assert!(!has_flat_address_shape("proj/seat--x"));
        assert!(!has_flat_address_shape("proj\\seat--x"));
        assert!(!has_flat_address_shape("bareword"));
        assert!(!has_flat_address_shape("--seat"));
        assert!(!has_flat_address_shape("proj--"));
        assert!(!has_flat_address_shape("a--b--c"));
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
}
