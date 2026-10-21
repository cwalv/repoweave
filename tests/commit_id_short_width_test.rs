//! Every abbreviated commit id an operator reads is `CommitId::short`.
//!
//! Structural, because the property is a prohibition over the abbreviation
//! sites in `src/`: an id abbreviated anywhere else prints at a width of its
//! own, and the surfaces that print one are too many to drive each
//! behaviorally. The behavioral pins sit beside the surfaces that have them
//! (`rwv push`'s lock-mismatch line, `rwv lock --commit`'s message, `rwv
//! abort`'s blocking-commit list).
//!
//! Scope: non-comment production lines of `src/` (see `common::src_scan`).
//! The needles are git's own abbreviation requests and a slice, `min` or
//! `take` at a literal 7 or 12, the two widths the tree used. An abbreviation
//! spelled through another literal, a named constant outside `vcs.rs`, or a
//! helper that hides the width is not this needle's shape.

mod common;

use common::src_scan::{production_lines, SourceLine};

const NEEDLES: &[&str] = &[
    "\"--short\"",
    "%h",
    "--oneline",
    "--abbrev",
    "[..7]",
    "[..12]",
    "min(7)",
    "min(12)",
    "take(7)",
    "take(12)",
];

const OWNER: &str = "vcs.rs";
const OWNER_MINT: &str = "&self.0[..Self::SHORT_WIDTH]";

#[test]
fn the_scan_reaches_the_one_abbreviation() {
    let lines = production_lines();
    assert!(
        lines
            .iter()
            .any(|l| l.file == OWNER && l.text.contains(OWNER_MINT)),
        "src/{OWNER} no longer spells `{OWNER_MINT}` — CommitId::short moved \
         or changed shape, so finding no other abbreviation below proves nothing"
    );
}

#[test]
fn no_commit_id_is_abbreviated_outside_commit_id_short() {
    let strays: Vec<String> = production_lines()
        .iter()
        .filter(|l: &&SourceLine| NEEDLES.iter().any(|n| l.text.contains(n)))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        strays.is_empty(),
        "abbreviate a commit id with `CommitId::short()`, so every surface \
         prints the same width:\n{strays:#?}"
    );
}
