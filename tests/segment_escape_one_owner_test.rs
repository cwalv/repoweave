//! Pins the prohibition this test enforces: `+`, the character a project
//! name's `/` is written as when the name is rendered as one path segment, is
//! spelled only in `naming.rs`. Everywhere else routes through
//! `encode_segment`, `decode_segment`, `flat_project_segment`,
//! `parse_weave_dir_name`, or the validator that rejects a name already
//! containing one.
//!
//! The `--` half of the same grammar is pinned by
//! `tests/weave_separator_one_owner_test.rs`. Both metacharacters have the
//! same owner, and that is the point: the flat address round-trips only
//! because the renderer writes `+` for `/` and the validators refuse a name
//! that already spells one. A module that invents its own handling of either
//! breaks the bijection without breaking a behavioural test, because the
//! corpus those tests range over is defined by the validators themselves.
//!
//! **The needles are three specific spellings, not the character.** A bare
//! scan for `'+'` is unusable: `src/vcs.rs`'s release-shape predicate splits a
//! version string on `['.', '-', '+']` — correct code, nothing to do with this
//! encoding — and a matcher that reports correct code is a matcher someone
//! turns off. Measured across `src/` at the time of writing, the three needles
//! below hit exactly one line each and all three are in the owner.
//!
//! Residue. A fourth spelling reached some other way — `char::from_u32(43)`,
//! a byte literal, a `+` inside a larger `replace` chain, a format string
//! writing it positionally — is not their shape and this scan will not see it.
//! `src_scan`'s comment filter is line-leading `//` only, so a needle in a
//! trailing or block comment reads as a live use.

mod common;

use common::src_scan::production_lines;

const OWNER: &str = "naming.rs";

/// The three spellings that write, read, or test for the segment escape.
const ESCAPE_NEEDLES: &[&str] = &[
    "replace('/', \"+\")",
    "replace('+', \"/\")",
    "contains('+')",
];

#[test]
fn naming_rs_still_spells_every_needle_this_scan_looks_for() {
    let lines = production_lines();
    let owner_lines: Vec<_> = lines.iter().filter(|l| l.file == OWNER).collect();

    assert!(
        !owner_lines.is_empty(),
        "no production lines scanned from src/{OWNER} — the owner moved or was \
         renamed, and an empty result under the rest of src/ would prove nothing"
    );

    for needle in ESCAPE_NEEDLES {
        assert!(
            owner_lines.iter().any(|l| l.text.contains(needle)),
            "expected `{needle}` in src/{OWNER} and found none — the needle no \
             longer matches the source shape, so an empty result under the rest \
             of src/ would prove nothing"
        );
    }
}

#[test]
fn no_module_outside_the_owner_spells_the_segment_escape() {
    let lines = production_lines();
    assert!(
        lines.len() >= 20_000,
        "expected at least 20,000 production lines under src/, got {} — \
         this scan is pointed at the wrong corpus",
        lines.len()
    );

    let hits: Vec<String> = lines
        .iter()
        .filter(|l| l.file != OWNER)
        .filter(|l| ESCAPE_NEEDLES.iter().any(|n| l.text.contains(n)))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        hits.is_empty(),
        "the `+` segment escape must be spelled only in src/{OWNER} — encode \
         via encode_segment or flat_project_segment, decode via decode_segment \
         or parse_weave_dir_name, and test for it via spells_segment_escape. A \
         second site writing or reading the escape can disagree with the \
         validators about which names are representable, and the flat address \
         stops being a bijection. Found: {hits:#?}"
    );
}
