//! `MidOperation` in `src/vcs.rs` spells the `mid-<operation>` tokens, and no
//! other production file writes one.
//!
//! Scope: production lines under `src/` outside `#[cfg(test)]` and comments,
//! read for a string literal that begins `"mid-`. A token embedded in the
//! middle of a longer literal is invisible to this scan, and so is one built by
//! concatenation. The `mid-operation` condition name in `vcs.rs` begins the
//! same way and is that file's, so `vcs.rs` is the one file not read for hits.

mod common;

use common::src_scan::production_lines;

const NEEDLE: &str = "\"mid-";

#[test]
fn only_vcs_rs_spells_a_mid_operation_token() {
    let lines = production_lines();

    assert!(
        lines
            .iter()
            .any(|l| l.file == "vcs.rs" && l.text.contains(NEEDLE)),
        "src/vcs.rs no longer quotes the prefix — the needle no longer matches the source \
         shape, so emptiness elsewhere would prove nothing"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.file == "git.rs" && l.text.contains("MidOperation")),
        "src/git.rs no longer mentions MidOperation — this scan is pointed at the wrong corpus"
    );

    let hits: Vec<String> = lines
        .iter()
        .filter(|l| l.file != "vcs.rs" && l.text.contains(NEEDLE))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "only MidOperation in src/vcs.rs spells the mid-operation tokens: {hits:#?}"
    );
}
