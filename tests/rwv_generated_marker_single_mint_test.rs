//! Pins the single-mint invariant for the `rwv.generated` JSON marker key:
//! `RwvGeneratedMarker::KEY` in `src/integrations/merge.rs` is the sole
//! definition, and every consumer (vscode-workspace) references that
//! constant rather than re-minting the string literal. A second mint drifts
//! silently — the two only stay in sync as long as nobody edits one and
//! forgets the other.
//!
//! Residue: the scan drops `//`-led comment lines, so a mint written only in
//! a comment is invisible here; it reads `src/` only, so a re-mint under
//! `tests/` is invisible too; and the needle is the literal `"rwv.generated"`
//! substring, so a mint assembled by concatenation rather than typed as one
//! literal would not be caught.

mod common;

use common::src_scan::production_lines;

#[test]
fn rwv_generated_marker_is_minted_exactly_once() {
    let lines = production_lines();
    let mints: Vec<String> = lines
        .iter()
        .filter(|l| l.text.contains("\"rwv.generated\""))
        .map(|l| l.file.clone())
        .collect();

    assert!(
        mints.iter().any(|f| f == "integrations/merge.rs"),
        "expected \"rwv.generated\" quoted at its mint in \
         src/integrations/merge.rs (RwvGeneratedMarker::KEY) and found none \
         — the needle no longer matches the source shape, so emptiness \
         elsewhere would prove nothing. Found mints at: {mints:?}"
    );

    assert_eq!(
        mints,
        vec!["integrations/merge.rs".to_string()],
        "\"rwv.generated\" must be minted in exactly one place \
         (RwvGeneratedMarker::KEY in merge.rs) — every other site \
         references that constant. Found mints at: {mints:?}"
    );
}
