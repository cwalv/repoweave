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
    if let Some(e) = link.downcast_ref::<crate::naming::RefNameError>() {
        return Some(ref_name_kind(e));
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
        use crate::naming::{RefNameError, WorkweaveNameError};

        let slash = anyhow::Error::new(WorkweaveNameError::Slash("a/b".into()));
        assert_eq!(kind_of(&slash), Some(RefusalKind::UnrenderableName));

        let pinned = anyhow::Error::new(WorkweaveNameError::InvalidRef(RefNameError::TagShaped(
            "v1.2.3".into(),
        )));
        assert_eq!(kind_of(&pinned), Some(RefusalKind::VersionIsAPin));

        let empty = anyhow::Error::new(RefNameError::Empty);
        assert_eq!(kind_of(&empty), Some(RefusalKind::InvalidRefName));
    }
}
