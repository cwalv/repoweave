//! Every refusal site in `src/` says what it is, and the census's two walks
//! see what they claim to.
//!
//! **The question the census asks is not "is this site classified".** A site
//! that carries a valid token, and the wrong one, is classified, its token
//! exists and its entry exists; only a test that drives *that site* and
//! asserts *its* token would notice. So `docs/internals/refusal-census.tsv`
//! records, per site, the test that reads it — keyed on the full token, a
//! judged state included, so a test that only reaches its axis reads nothing —
//! and `-` where none does. That file is generated and drift-gated; the unread
//! rows are the census's finding, and a change to them shows in its diff.
//!
//! Structural, under docs/internals/testing.md's license 2 — a prohibition over
//! an enumerable population. What a site and a read are, and what neither
//! sees, is stated in `tests/common/refusal_census.rs`.

use crate::common;

use common::refusal_census::{
    bare_constructor_on, derive, judged_state_on, sites, tokens_read_in, untokened_class_on, BARE,
};
use repoweave::diagnosis::State;
use std::path::{Path, PathBuf};

fn member() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Every error rwv mints says at its site which it is: a refusal with a
/// token, a judged state, or `untokened!` naming why it has neither. A bare
/// anyhow constructor is a judgment nobody made.
#[test]
fn no_error_is_minted_without_its_class() {
    let bare: Vec<String> = sites(&member())
        .into_iter()
        .filter(|(_, _, token)| token == BARE)
        .map(|(file, line, _)| format!("{file}:{line}"))
        .collect();
    assert!(
        bare.is_empty(),
        "these sites mint an error with anyhow's own constructor. A refusal rwv makes on \
         purpose carries a token (`refuse!`) or a judged state; anything else goes through \
         `crate::untokened!(<Class>, …)`, naming why it has no token:\n{}",
        bare.join("\n")
    );
}

/// The walk reaches each kind of site in the real tree, and the read scan
/// reaches each kind of read.
#[test]
fn the_census_walks_see_the_tree() {
    let census = derive(&member());
    let row = |file: &str, token: &str| {
        census
            .iter()
            .find(|((f, t, _), _)| f == file && t == token)
            .map(|((_, _, instrument), _)| instrument.clone())
    };
    assert_eq!(
        row("src/branch.rs", "target cycle").as_deref(),
        Some("tests/weave/branch_create_switch_test.rs"),
        "a judged site and the test that refuses with its state"
    );
    assert_eq!(
        row("src/add_remove.rs", "no-remote-url").as_deref(),
        Some("tests/docs/refusal_arrival_test.rs"),
        "a token site and the test that routes to it"
    );
    assert_eq!(
        row("src/lock.rs", "(untokened:environment-passthrough)").as_deref(),
        Some("-"),
        "an untokened site"
    );
}

/// A read is keyed on the token a site prints. For a judged state that is
/// `<axis> <state>`: asserting the axis, or another state of it, reads
/// nothing.
#[test]
fn a_judged_state_is_read_only_through_its_own_state() {
    let test = "let stderr = common::cli::refused(\n    &out,\n    State::PinNone,\n);\n\
                assert_routes_to(&stderr, \"pin\");\n\
                // refused(&out, State::PinAhead)\n\
                fn refused(out: &Output, expected: impl Into<Refusal>) {}\n\
                not_refused(&out, State::PinBehind);\n";
    assert_eq!(tokens_read_in(test), ["pin none", "pin"]);
}

#[test]
fn a_refusal_quoted_in_a_string_is_not_a_read() {
    let test = "let fixture = \"refused(&out, State::PinNone)\";\n\
                let c = '\"'; refused(&out, State::PinAhead);\n";
    assert_eq!(tokens_read_in(test), ["pin ahead"]);
}

#[test]
fn a_token_is_read_through_refused_or_its_route() {
    let test = "refused(&out, RefusalKind::NoRemoteUrl);\n\
                assert_routes_to(&stderr, \"op-in-progress\");\n";
    assert_eq!(tokens_read_in(test), ["no-remote-url", "op-in-progress"]);
}

#[test]
fn a_judged_state_is_read_by_its_own_name_only() {
    assert_eq!(
        judged_state_on("    crate::diagnosis::State::EditsDirty"),
        Some(State::EditsDirty)
    );
    assert_eq!(judged_state_on("let s = OpState::EditsDirty;"), None);
    assert_eq!(judged_state_on("State::EditsDirtyish"), None);
}

#[test]
fn an_untokened_class_is_read_across_lines() {
    assert_eq!(
        untokened_class_on("return Err(crate::untokened!(   InternalInvariant, \"x\"))").as_deref(),
        Some("internal-invariant")
    );
    assert_eq!(
        untokened_class_on("crate::untokened!( Owed,").as_deref(),
        Some("owed")
    );
    assert_eq!(untokened_class_on("refuse!(RefusalKind::X, \"y\")"), None);
}

#[test]
fn every_anyhow_constructor_spelling_is_a_bare_site() {
    for minted in [
        "anyhow::bail!(\"x\")",
        "    bail!(",
        "_ => anyhow::anyhow!(\"x\")",
        ".ok_or_else(|| anyhow!(\"x\"))",
        "::anyhow::ensure!(ok, \"x\");",
        "ensure!(ok, \"x\");",
        "anyhow::Error::msg(text)",
        "Err(Error::msg(text))",
    ] {
        assert!(bare_constructor_on(minted), "{minted}");
    }
    for not_minted in [
        "crate::untokened!(Tally, \"x\")",
        "crate::refuse!(RefusalKind::X, \"x\")",
        "my_bail!(\"x\")",
        "other::bail!(\"x\")",
        "io::Error::msg(\"x\")",
        "anyhow::Error::new(e)",
    ] {
        assert!(!bare_constructor_on(not_minted), "{not_minted}");
    }
}
