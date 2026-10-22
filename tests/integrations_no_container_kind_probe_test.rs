//! Pins the prohibition this test enforces: no integration
//! re-derives the container kind from a path. `IntegrationContext` carries
//! `container_kind`, resolved once by whichever verb built the context —
//! an integration testing `workspace_root` for the workweave marker itself
//! would be re-deriving a fact its caller already had.
//!
//! The needles are the routes a path-based re-derivation can take to the
//! marker: `WORKWEAVE_MARKER_FILE`, the constant naming the file;
//! `WorkweaveMarker`, whose `path_in` and `read` locate and parse it and whose
//! name `StateFile::WorkweaveMarker` also carries; and `observe_root`, which
//! reads both identity files at once. Privacy cannot hold this: `workspace.rs`,
//! `state_file.rs` and doctor all need the file, so each route stays public to
//! the crate. Checking for these symbols rather than the literal
//! `.rwv-workweave` string avoids a false hit on the unrelated
//! `.rwv-workweave-index` marker (a different file, read by `check.rs` and
//! `workweave_index.rs`), which shares the shorter string as a prefix.
//!
//! This scan once carried `is_workweave_root` as a second needle. That
//! function no longer exists anywhere in the crate, so the pin that it stays
//! gone is crate-wide and lives in `weave_root_probes_stay_deleted_test.rs`;
//! keeping it here would have left this file's vacuity guard asserting the
//! presence of something deliberately deleted.

mod common;

use common::src_scan::production_lines;

const NEEDLES: [&str; 3] = ["WORKWEAVE_MARKER_FILE", "WorkweaveMarker", "observe_root"];

#[test]
fn workspace_rs_still_mints_the_needles_this_scan_looks_for() {
    let lines = production_lines();
    for needle in NEEDLES {
        let at_owner = lines
            .iter()
            .any(|l| l.file == "workspace.rs" && l.text.contains(needle));
        assert!(
            at_owner,
            "expected `{needle}` in src/workspace.rs and found none — the \
             needle no longer matches the source shape, so an empty result \
             under src/integrations/ would prove nothing"
        );
    }
}

#[test]
fn no_integration_re_derives_the_container_kind_from_a_path() {
    let lines = production_lines();
    let integration_lines: Vec<_> = lines
        .iter()
        .filter(|l| l.file.starts_with("integrations/"))
        .collect();
    assert!(
        integration_lines.len() >= 100,
        "expected at least 100 production lines under src/integrations/, got {} \
         — this scan is pointed at the wrong corpus",
        integration_lines.len()
    );

    for needle in NEEDLES {
        let hits: Vec<String> = integration_lines
            .iter()
            .filter(|l| l.text.contains(needle))
            .map(|l| format!("{} {}", l.site(), l.text.trim()))
            .collect();
        assert!(
            hits.is_empty(),
            "src/integrations/ must not test the container's weave root for \
             the workweave marker itself — `IntegrationContext::container_kind` \
             carries the answer the caller already resolved. Found: {hits:#?}"
        );
    }
}
