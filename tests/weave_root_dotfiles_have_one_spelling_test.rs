//! Each weave-root dotfile name is written once in `src/`, at the constant
//! that declares it.
//!
//! `.rwv-active` and `.rwv-workweave` are answered for by `workspace.rs`:
//! `observe_root` and `WorkweaveMarker::path_in` build the paths,
//! `read_active_project` and `clear_active_project` read and remove the
//! pointer. Both constants are module-private, so the compiler refuses a
//! foreign module that names one — but it has nothing to say about
//! `root.join(".rwv-active")`, which is how the sprawl arrived in the first
//! place, and nothing to say about a second constant re-minting the same
//! string under a different name in another module.
//!
//! The needle is the closed string literal, quotes included. That is what a
//! path join has to spell, and it discriminates against the two shapes that
//! must survive: doctor's operator-facing prose, which quotes the names inside
//! a larger sentence, and `.rwv-workweave-index`, a different file whose
//! literal shares the shorter one as a prefix.
//!
//! Residue, for anyone extending this. The scan is blind to a name assembled
//! rather than written — `format!(".rwv-{}", suffix)`, or a `&str` bound in
//! one module and joined in another. It inherits `src_scan`'s line-leading
//! `//` comment filter, so a closed literal in a trailing comment reads as
//! production text. And it says nothing about `tests/`, an external crate
//! where fixtures spell the names by design.

mod common;

use common::src_scan::{production_lines, SourceLine};

/// Each name, and the constant that is allowed to be the one site spelling it.
const SPELLINGS: [(&str, &str); 2] = [
    ("ACTIVE_PROJECT_FILE", "\".rwv-active\""),
    ("WORKWEAVE_MARKER_FILE", "\".rwv-workweave\""),
];

fn sites<'a>(lines: &'a [SourceLine], needle: &str) -> Vec<&'a SourceLine> {
    lines.iter().filter(|l| l.text.contains(needle)).collect()
}

#[test]
fn the_scan_is_pointed_at_a_whole_source_tree() {
    let lines = production_lines();
    assert!(
        lines.len() >= 10_000,
        "expected at least 10000 production lines under src/, got {} — a \
         one-site result below would be measuring the corpus, not the source",
        lines.len()
    );
}

#[test]
fn each_dotfile_name_is_spelled_once_and_at_its_declaration() {
    let lines = production_lines();

    for (constant, literal) in SPELLINGS {
        let found = sites(&lines, literal);
        assert_eq!(
            found.len(),
            1,
            "`{literal}` must be written at exactly one production site. A \
             second one is either a path join reaching past `workspace.rs` or \
             a constant re-minting a name that already has an owner. Found: {:?}",
            found.iter().map(|l| l.site()).collect::<Vec<_>>()
        );

        let site = found[0];
        assert_eq!(
            site.file,
            "workspace.rs",
            "`{literal}`'s one site must be workspace.rs; found {}",
            site.site()
        );
        assert!(
            site.text.contains("const") && site.text.contains(constant),
            "`{literal}` survives at {} but not as `{constant}`'s declaration \
             — the name moved, so this file's one-site count no longer says \
             what it claims: {}",
            site.site(),
            site.text.trim()
        );
    }
}

#[test]
fn a_join_outside_the_owner_is_what_this_reports() {
    let planted = |file: &str, text: &str| SourceLine {
        file: file.to_string(),
        line: 1,
        text: text.to_string(),
    };
    let corpus = vec![
        planted(
            "workspace.rs",
            "const ACTIVE_PROJECT_FILE: &str = \".rwv-active\";",
        ),
        planted("check.rs", "    if root.join(\".rwv-active\").exists() {"),
        planted(
            "check.rs",
            "  `.rwv-active` and `.rwv-workweave` are exclusive",
        ),
    ];

    let found = sites(&corpus, "\".rwv-active\"");
    assert_eq!(
        found.len(),
        2,
        "the seeded join must be reported alongside the declaration; got {:?}",
        found.iter().map(|l| l.text.trim()).collect::<Vec<_>>()
    );
    assert!(
        found.iter().any(|l| l.file == "check.rs"),
        "the seeded join is the one this scan exists for and it went unseen"
    );
    assert_eq!(
        sites(&corpus, "\".rwv-workweave\"").len(),
        0,
        "operator prose quoting the names in backticks must not register as a \
         path join — it is documentation of the on-disk contract"
    );
}
