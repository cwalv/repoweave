//! The weave's layout — that a project's files live at `projects/<name>` —
//! is written once in `src/`, at the constant that declares the segment.
//!
//! `workspace.rs` answers for it: `projects_dir` and `project_dir` build the
//! absolute paths, `project_rel_dir` and `project_rel_path` the weave-relative
//! ones, `strip_projects_prefix` and `project_name_from_dir` read a project
//! back out of a path. The constant behind them is module-private, so the
//! compiler refuses a foreign module that names it — but it has nothing to say
//! about `root.join("projects")`, which is how the sprawl arrived in the first
//! place, and nothing to say about a second constant re-minting the same
//! string under a different name in another module.
//!
//! The needle is the closed string literal, quotes included. That is what a
//! path join, a `push`, a `strip_prefix` and a component comparison all have
//! to spell, and it discriminates against the shape that must survive: text
//! that names the directory inside a larger string, which is documentation of
//! the on-disk contract rather than a path being built.
//!
//! Residue, for anyone extending this. The scan is blind to the interpolated
//! form, `format!("projects/{}", name)` — a different literal, and one
//! `fetch.rs` legitimately writes twice in the hint it prints when a project
//! name is taken. Discriminating a re-introduced path build from those two
//! operator strings would take a hard-coded site list, which is a list its
//! author typed and would drift; the honest statement is that this scan does
//! not cover them. It is likewise blind to a segment assembled rather than
//! written, it inherits `src_scan`'s line-leading `//` comment filter and its
//! `#[cfg(test)]` skip, so an in-`src` fixture spelling the literal reads as
//! absent, and it says nothing about `tests/`, an external crate where
//! fixtures spell the layout by design.

mod common;

use common::src_scan::{production_lines, SourceLine};

/// The layout segment, and the constant that is allowed to be its one site.
const SEGMENT: &str = "\"projects\"";
const DECLARED_AS: &str = "PROJECTS_DIR";

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
fn the_layout_segment_is_spelled_once_and_at_its_declaration() {
    let lines = production_lines();
    let found = sites(&lines, SEGMENT);

    assert_eq!(
        found.len(),
        1,
        "`{SEGMENT}` must be written at exactly one production site. A second \
         one is either a path built past `workspace.rs` or a constant \
         re-minting a segment that already has an owner. Found: {:?}",
        found.iter().map(|l| l.site()).collect::<Vec<_>>()
    );

    let site = found[0];
    assert_eq!(
        site.file,
        "workspace.rs",
        "`{SEGMENT}`'s one site must be workspace.rs; found {}",
        site.site()
    );
    assert!(
        site.text.contains("const") && site.text.contains(DECLARED_AS),
        "`{SEGMENT}` survives at {} but not as `{DECLARED_AS}`'s declaration — \
         the name moved, so this file's one-site count no longer says what it \
         claims: {}",
        site.site(),
        site.text.trim()
    );
}

#[test]
fn a_join_outside_the_owner_is_what_this_reports() {
    let planted = |file: &str, text: &str| SourceLine {
        file: file.to_string(),
        line: 1,
        text: text.to_string(),
    };
    let corpus = vec![
        planted("workspace.rs", "const PROJECTS_DIR: &str = \"projects\";"),
        planted(
            "check.rs",
            "    let dir = root.join(\"projects\").join(name);",
        ),
        planted("prime.rs", "    out.push_str(\"  projects/\\n\");"),
    ];

    let found = sites(&corpus, SEGMENT);
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
    assert!(
        found.iter().all(|l| l.file != "prime.rs"),
        "a rendered directory listing naming the directory inside a larger \
         string must not register as a path build — it is what the operator \
         is shown, not a path this code composes"
    );
}
