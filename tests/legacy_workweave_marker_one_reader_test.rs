//! Pins the prohibition rwv-hdy7s0.12 exists to enforce: `.rwv-workweave` is
//! parsed in exactly one place, `workspace.rs`'s `observe_marker`. Before
//! this bead, `check.rs`'s `scan_for_legacy_workweave_markers` re-implemented
//! the same classification by hand — a second lenient reader that a grep for
//! the `WORKWEAVE_MARKER_FILE` constant could not catch, because it spelled
//! the filename as the literal `".rwv-workweave"` instead of the constant.
//!
//! Two needles, because the bug had two independent tells: the exact
//! classification expression duplicated from `observe_marker`
//! (`raw.get("parent").map(|v| v.is_null()).unwrap_or(true)` — the "parse
//! shape") and the literal path-join it read through
//! (`.join(".rwv-workweave")` — the "literal filename"). Either alone proves
//! less: the expression alone doesn't establish that a file was actually
//! read through it, and the join alone doesn't establish a parse follows it.
//! Together they are what the removed code actually looked like.
//!
//! A bare substring scan for `.rwv-workweave` is unusable here: dozens of
//! `check.rs` messages spell it in backtick-quoted prose ("carries both
//! `.rwv-active` and `.rwv-workweave`") to describe the file to an operator,
//! not to construct a path to read. Those aren't a second reader and would
//! drown a real one. `.join(".rwv-workweave")` is specific to an actual
//! filesystem access; prose never contains it.

mod common;

use common::src_scan::production_lines;

const PARSE_NEEDLE: &str = "raw.get(\"parent\").map(|v| v.is_null()).unwrap_or(true)";
const JOIN_NEEDLE: &str = ".join(\".rwv-workweave\")";

#[test]
fn workspace_rs_still_mints_the_needles_this_scan_looks_for() {
    let lines = production_lines();
    let owner_lines: Vec<_> = lines.iter().filter(|l| l.file == "workspace.rs").collect();

    assert!(
        owner_lines.iter().any(|l| l.text.contains(PARSE_NEEDLE)),
        "expected `{PARSE_NEEDLE}` in src/workspace.rs (observe_marker's \
         classification) and found none — the needle no longer matches the \
         source shape, so an empty result under the rest of src/ would prove \
         nothing"
    );
    assert!(
        owner_lines.iter().any(|l| l.text.contains(JOIN_NEEDLE)),
        "expected `{JOIN_NEEDLE}` in src/workspace.rs and found none — the \
         needle no longer matches the source shape, so an empty result under \
         the rest of src/ would prove nothing"
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

    let outside: Vec<_> = lines.iter().filter(|l| l.file != "workspace.rs").collect();

    let parse_hits: Vec<String> = outside
        .iter()
        .filter(|l| l.text.contains(PARSE_NEEDLE))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    let join_hits: Vec<String> = outside
        .iter()
        .filter(|l| l.text.contains(JOIN_NEEDLE))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        parse_hits.is_empty() && join_hits.is_empty(),
        "`.rwv-workweave` must be parsed only in src/workspace.rs, through \
         observe_marker (directly, or via legacy_marker_primary, \
         WorkweaveMarker::read, or WorkweaveMarker::migrate_legacy) — a \
         module that reads and classifies the file itself is the bug \
         rwv-hdy7s0.12 fixed. parse-shape hits: {parse_hits:#?}, join-shape \
         hits: {join_hits:#?}"
    );
}
