//! Every refusal token reaches an entry, and `rwv explain <token>` serves that
//! entry's own bytes.
//!
//! The property that makes this worth pinning is not "the page exists" but
//! that there is ONE text: a token's entry lives on exactly one published
//! page, and what the terminal prints is a slice of that page rather than a
//! copy of it. Two spellings is the failure D1 forbids, and a copy is how it
//! arrives.

use repoweave::refusal::RefusalKind;

mod common;

/// Every token the register mints, read out of the enum's own declaration.
///
/// Structural, and licensed as a prohibition over an enumerable population: no
/// runtime handle enumerates a fieldless enum without a derive this crate does
/// not carry, and a hand-written list of 97 variants is a second copy of the
/// register that drifts from it silently.
///
/// Scope: the `pub enum RefusalKind` block of `src/refusal.rs`, matched on
/// four-space-indented `Variant,` lines. A variant written on a shared line, or
/// one carrying a `#[serde(rename)]` that overrides the case transform, is
/// invisible here — [`the_case_transform_matches_serde`] is what keeps the
/// transform itself honest.
fn minted_tokens() -> Vec<String> {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/refusal.rs"),
    )
    .expect("the refusal module is readable");
    let body = src
        .split_once("pub enum RefusalKind {")
        .expect("the enum is declared")
        .1;
    let body = body.split_once("\n}").expect("the enum closes").0;

    body.lines()
        .filter_map(|l| {
            let name = l.strip_prefix("    ")?.strip_suffix(',')?;
            (!name.starts_with(char::is_lowercase)
                && !name.starts_with('#')
                && name.chars().all(|c| c.is_ascii_alphanumeric()))
            .then(|| kebab(name))
        })
        .collect()
}

/// The published entry pages, read from disk rather than through the module
/// that serves them.
fn pages_text() -> String {
    let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference");
    let refusals = std::fs::read_to_string(docs.join("refusals.md")).expect("refusals published");
    let findings =
        std::fs::read_to_string(docs.join("doctor-findings.md")).expect("findings published");
    format!("{refusals}\n{findings}")
}

/// serde's `rename_all = "kebab-case"`, reproduced outside serde.
fn kebab(variant: &str) -> String {
    let mut out = String::new();
    for (i, c) in variant.char_indices() {
        if c.is_ascii_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The whole file rests on reproducing serde's transform by hand, so the
/// transform is checked against tokens serde actually produced. `VersionIsAPin`
/// is the shape that breaks a naive splitter — a one-letter word between two
/// longer ones.
#[test]
fn the_case_transform_matches_serde() {
    let ground_truth = [
        (RefusalKind::VersionIsAPin, "VersionIsAPin"),
        (RefusalKind::OpInProgress, "OpInProgress"),
        (RefusalKind::PushFromWorkweave, "PushFromWorkweave"),
        (
            RefusalKind::DroppedRepoHasUniqueCommits,
            "DroppedRepoHasUniqueCommits",
        ),
    ];
    for (kind, variant) in ground_truth {
        assert_eq!(
            kind.token(),
            kebab(variant),
            "transform diverged on {variant}"
        );
    }
}

#[test]
fn every_minted_token_has_an_entry() {
    let tokens = minted_tokens();
    assert!(
        tokens.len() > 90,
        "the register walk yielded {} tokens, too few to be the whole enum",
        tokens.len()
    );

    let missing: Vec<&String> = tokens
        .iter()
        .filter(|t| repoweave::explain::entry_for_token(t).is_none())
        .collect();
    assert!(
        missing.is_empty(),
        "these tokens are printed to operators with no entry to route them to:\n{missing:#?}"
    );
}

/// The reverse direction: an entry whose token no longer exists is a page
/// section nothing can reach, and it outlives the code silently.
#[test]
fn every_entry_names_a_minted_token() {
    let minted: std::collections::BTreeSet<String> = minted_tokens().into_iter().collect();
    let documented = repoweave::explain::documented_tokens();
    assert!(
        documented.len() > 90,
        "the page walk yielded {} tokens, too few to have read both pages",
        documented.len()
    );

    // Doctor findings and issue kinds are documented on the same page and are
    // not refusal tokens; the register is the subset this direction is about.
    let refusals_page = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/refusals.md"),
    )
    .expect("the refusals page is published");
    let orphaned: Vec<&str> = refusals_page
        .lines()
        .filter_map(|l| l.strip_prefix("### `")?.strip_suffix('`'))
        .filter(|t| !minted.contains(*t))
        .collect();
    assert!(
        orphaned.is_empty(),
        "these entries outlived the tokens they document:\n{orphaned:#?}"
    );
}

/// A token whose condition `rwv doctor` also reports keeps its single entry on
/// the findings page. Serving it must not depend on which page carries it —
/// that indifference is what stops a second entry being written here.
#[test]
fn a_shared_token_is_served_from_the_page_that_already_had_it() {
    let entry =
        repoweave::explain::entry_for_token("stale-lock").expect("a shared token still resolves");
    assert!(entry.starts_with("### `stale-lock`"), "got:\n{entry}");

    let refusals_page = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/refusals.md"),
    )
    .expect("the refusals page is published");
    assert!(
        !refusals_page.contains("### `stale-lock`"),
        "a shared condition gained a second entry, which is two spellings of one rule"
    );
}

/// What `rwv explain <token>` prints is the page's own bytes.
///
/// Driven through the binary rather than the library: the claim is about what
/// an operator reads, and a library-only assertion would hold even if dispatch
/// never reached the entry.
#[test]
fn explain_serves_the_page_section_verbatim() {
    // The last entry on a page is the one case with no following heading to
    // stop at, so it takes a different branch of the slicer. Derived rather
    // than named: an entry appended after it would otherwise silently take
    // that coverage away.
    let refusals_page = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/refusals.md"),
    )
    .expect("the refusals page is published");
    let last_entry = refusals_page
        .lines()
        .filter_map(|l| l.strip_prefix("### `")?.strip_suffix('`'))
        .next_back()
        .expect("the page has entries")
        .to_owned();

    for token in [
        "push-from-workweave",
        "stale-lock",
        "dangling-parent",
        &last_entry,
    ] {
        let out = common::rwv()
            .args(["explain", token])
            .output()
            .expect("rwv should run");
        assert!(out.status.success(), "rwv explain {token} failed");
        let printed = String::from_utf8_lossy(&out.stdout).into_owned();
        let printed = printed.trim_end_matches('\n');

        // Compared against the PAGE, not against `entry_for_token` — that is
        // the function under test, and asserting it against itself passes
        // whatever it becomes. What must hold is that the bytes an operator
        // reads occur verbatim in what the site publishes.
        let published = pages_text();
        assert!(
            !printed.is_empty() && published.contains(printed),
            "`rwv explain {token}` printed text that is not in any published page:\n{printed}"
        );
        assert!(
            printed.starts_with(&format!("### `{token}`"))
                || printed.starts_with(&format!("#### `{token}`")),
            "`rwv explain {token}` opened with something other than that token's heading:\n{printed}"
        );
    }
}

/// The typo hint spans tokens, not only verbs — a mistyped token is the input
/// this arm exists for now that tokens outnumber verbs five to one.
#[test]
fn a_mistyped_token_is_suggested() {
    let out = common::rwv()
        .args(["explain", "stale-lok"])
        .output()
        .expect("rwv should run");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("did you mean: stale-lock"),
        "expected a token suggestion, got:\n{stderr}"
    );
}

/// A verb and a token could collide; the verb wins, because that is the name
/// the reader just ran a command with.
#[test]
fn a_verb_outranks_a_token_of_the_same_name() {
    let out = common::rwv()
        .args(["explain", "lock"])
        .output()
        .expect("rwv should run");
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(
        printed.starts_with("# rwv lock"),
        "the verb bundle must win, got:\n{}",
        &printed[..printed.len().min(120)]
    );
}
