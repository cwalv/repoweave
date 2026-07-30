//! Pins the prohibition this test enforces: `.rwv-workweave` is
//! parsed in exactly one place, `workspace.rs`'s `observe_marker`. Before
//! this, `check.rs`'s `scan_for_legacy_workweave_markers` re-implemented
//! the same classification by hand — a second lenient reader that a grep for
//! the `WORKWEAVE_MARKER_FILE` constant could not catch, because it spelled
//! the filename as the literal `".rwv-workweave"` instead of the constant.
//!
//! The needle is the classification expression duplicated from
//! `observe_marker` (`raw.get("parent").map(|v| v.is_null()).unwrap_or(true)`),
//! which is the tell for a parse rather than for a read.
//!
//! This scan once carried the literal path-join the second reader read
//! through, `.join(".rwv-workweave")`, as a second needle. No module builds
//! the marker path from a literal any more, `workspace.rs` included:
//! `WorkweaveMarker::path_in` is the one construction site, and the constant
//! it joins is module-private. The stronger pin that replaced it —
//! `tests/weave_root_dotfiles_have_one_spelling_test.rs` — holds the literal
//! to exactly one production site in the whole crate, the constant's own
//! declaration, so it catches a re-inlined join wherever it lands rather than
//! only outside this one file. Keeping the needle here would have left this
//! file's vacuity guard asserting the presence of a shape deliberately
//! removed.
//!
//! A bare substring scan for `.rwv-workweave` is unusable here: dozens of
//! `check.rs` messages spell it in backtick-quoted prose ("carries both
//! `.rwv-active` and `.rwv-workweave`") to describe the file to an operator,
//! not to construct a path to read. Those aren't a second reader and would
//! drown a real one.

mod common;

use common::src_scan::production_lines;

const PARSE_NEEDLE: &str = "raw.get(\"parent\").map(|v| v.is_null()).unwrap_or(true)";

#[test]
fn workspace_rs_still_mints_the_needle_this_scan_looks_for() {
    let lines = production_lines();
    let owner_lines: Vec<_> = lines.iter().filter(|l| l.file == "workspace.rs").collect();

    assert!(
        owner_lines.iter().any(|l| l.text.contains(PARSE_NEEDLE)),
        "expected `{PARSE_NEEDLE}` in src/workspace.rs (observe_marker's \
         classification) and found none — the needle no longer matches the \
         source shape, so an empty result under the rest of src/ would prove \
         nothing"
    );
}

#[test]
fn no_module_outside_workspace_parses_the_workweave_marker() {
    let lines = production_lines();
    assert!(
        lines.len() >= 20_000,
        "expected at least 20,000 production lines under src/, got {} — \
         this scan is pointed at the wrong corpus",
        lines.len()
    );

    let parse_hits: Vec<String> = lines
        .iter()
        .filter(|l| l.file != "workspace.rs" && l.text.contains(PARSE_NEEDLE))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        parse_hits.is_empty(),
        "`.rwv-workweave` must be parsed only in src/workspace.rs, through \
         observe_marker (directly, or via legacy_marker_primary, \
         WorkweaveMarker::read, or WorkweaveMarker::migrate_legacy) — a \
         module that reads and classifies the file itself is the bug this \
         test pins the fix for. parse-shape hits: {parse_hits:#?}"
    );
}
