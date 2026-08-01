//! Pins the prohibition this test enforces: `.rwv-workweave` is
//! parsed in exactly one place, `workspace.rs`'s `observe_marker`. Before
//! this, `check.rs`'s `scan_for_legacy_workweave_markers` re-implemented
//! the same classification by hand — a second lenient reader that a grep for
//! the `WORKWEAVE_MARKER_FILE` constant could not catch, because it spelled
//! the filename as the literal `".rwv-workweave"` instead of the constant.
//!
//! The needle is `as_mapping_get("primary")`, the field extraction
//! `observe_marker` performs on the parsed document. Reading a marker's
//! required field is the tell for a parse rather than for a read.
//!
//! It replaced `raw.get("parent").map(|v| v.is_null()).unwrap_or(true)`, the
//! classification expression that served while the marker was parsed through
//! serde_yaml. That expression no longer exists: markers are written as JSON
//! and the legacy YAML shape is read through saphyr, which reaches its fields
//! by name rather than by mutating a parsed tree. The vacuity guard below is
//! what forced this needle to be re-minted rather than silently matching
//! nothing.
//!
//! `YamlOwned::load_from_str` would be the more obvious tell and is the wrong
//! one: `integrations/pnpm_workspaces.rs` parses `pnpm-workspace.yaml` through
//! the same call, and that is a different file, not a second reader of this
//! one. The needle has to name a field only this marker has.
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

const PARSE_NEEDLE: &str = "as_mapping_get(\"primary\")";

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
