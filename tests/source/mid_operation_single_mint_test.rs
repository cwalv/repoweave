//! `MidOperation` in `src/vcs.rs` spells the `mid-<operation>` tokens, and no
//! other production file writes one.
//!
//! Scope: production lines under `src/` outside `#[cfg(test)]` and comments,
//! read for a string literal that begins `"mid-`. A token embedded in the
//! middle of a longer literal is invisible to this scan, and so is one built by
//! concatenation. The `mid-operation` name — the VCS condition in `vcs.rs`,
//! the `edits` state in `diagnosis.rs` — begins the same way and is no
//! operation's token, so that one literal is not a hit, and `vcs.rs` is the
//! one file not read for hits.

use crate::common;

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
        .filter(|l| l.file != "vcs.rs" && spells_an_operation_token(&l.text))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "only MidOperation in src/vcs.rs spells the mid-operation tokens: {hits:#?}"
    );
}

/// Whether `text` holds a `"mid-` literal other than the `"mid-operation"`
/// name.
fn spells_an_operation_token(text: &str) -> bool {
    text.match_indices(NEEDLE)
        .any(|(at, _)| !text[at..].starts_with("\"mid-operation\""))
}

#[test]
fn the_condition_name_is_not_an_operation_token_but_an_operation_is() {
    assert!(!spells_an_operation_token(r#"name: "mid-operation","#));
    assert!(spells_an_operation_token(r#"let t = "mid-rebase";"#));
    assert!(spells_an_operation_token(
        r#"("mid-operation", "mid-merge")"#
    ));
}
