//! Pins the single-mint invariant for the `rwv.generated` JSON marker key:
//! `RwvGeneratedMarker::KEY` in `src/integrations/merge.rs` is the sole
//! definition, and every consumer (vscode-workspace) references that
//! constant rather than re-minting the string literal. A second mint drifts
//! silently — the two only stay in sync as long as nobody edits one and
//! forgets the other.

mod common;

use common::src_scan::production_lines;

#[test]
fn rwv_generated_marker_is_minted_exactly_once() {
    let mints: Vec<String> = production_lines()
        .into_iter()
        .filter(|l| l.text.contains("\"rwv.generated\""))
        .map(|l| l.file)
        .collect();

    assert_eq!(
        mints,
        vec!["integrations/merge.rs".to_string()],
        "\"rwv.generated\" must be minted in exactly one place \
         (RwvGeneratedMarker::KEY in merge.rs) — every other site \
         references that constant. Found mints at: {mints:?}"
    );
}
