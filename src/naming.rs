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
//! written as `+` on the way out and read back as `/` on the way in.
//!
//! That round trip is injective only because two rules hold together, and
//! they are enforced in different places:
//!
//! - the renderer writes `+` for `/`, and nothing else writes `+`;
//! - the validators reject a name that already spells `+`, or that spells the
//!   `--` the two halves are joined with.
//!
//! Drop either half and two distinct addresses collapse onto one spelling: a
//! project genuinely named `a+b` would decode as the nested `a/b`, and a
//! project named `x--y` would split at its own middle. The rules ship
//! together or the encoding stops being a bijection, which is why the
//! validators and the renderer live in one file rather than agreeing across
//! two.

use serde::Serialize;
use std::fmt;

// ---------------------------------------------------------------------------
// The grammar, at the level of `&str`
// ---------------------------------------------------------------------------

/// Split `s` at the first workweave separator, without validating either
/// half. [`parse_weave_dir_name`] layers validation on top; a caller that
/// needs its own error text for an invalid half, or that only wants to test
/// for the separator's presence, calls this instead of writing `"--"` again.
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

/// The inverse of [`flat_project_segment`], for a segment already split off a
/// flat address.
fn nested_project_name(segment: &str) -> String {
    decode_segment(segment)
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

/// Parse a directory name into `(project, workweave_name)` if it matches the
/// `{left}--{name}` shape, decoding the left half back through
/// [`flat_project_segment`].
///
/// A discovery aid: it answers what a directory's own name says, which is a
/// weaker claim than what the records say. A resolution takes the workweave's
/// name from the registry entry recording the directory, so a decode that
/// disagrees misdirects a scan and cannot corrupt an identity.
pub fn parse_weave_dir_name(dir_name: &str) -> Option<(String, WorkweaveName)> {
    let (left, workweave) = split_at_weave_separator(dir_name)?;
    if left.is_empty() || workweave.is_empty() {
        return None;
    }
    Some((
        nested_project_name(left),
        WorkweaveName::new(workweave).ok()?,
    ))
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
