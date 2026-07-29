//! Pins the mid-op label vocabulary (`rebase`, `merge`, `cherry-pick`) to
//! `ConflictOp` in `src/vcs.rs`.
//!
//! sync used to carry its own table mapping each variant to its hyphen
//! spelling. Two tables agree only until someone edits one of them, and the
//! one in sync is the one an operator reads in a `mid-<op>` refusal.
//!
//! Neither side of the comparison is typed into this file: the labels come out
//! of serialising the enum, whose kebab-case wire spelling is the same
//! vocabulary the display text uses, and the corpus is `src/` itself.

mod common;

use common::src_scan::{production_lines, SourceLine};
use repoweave::vcs::ConflictOp;

/// Every `ConflictOp`, with a match that stops compiling when a variant is
/// added — the list below is what the assertions iterate.
fn all_conflict_ops() -> Vec<ConflictOp> {
    let all = vec![
        ConflictOp::Rebase,
        ConflictOp::Merge,
        ConflictOp::CherryPick,
    ];
    for op in &all {
        match op {
            ConflictOp::Rebase | ConflictOp::Merge | ConflictOp::CherryPick => {}
        }
    }
    all
}

fn wire_label(op: ConflictOp) -> String {
    serde_json::to_value(op)
        .expect("ConflictOp serialises")
        .as_str()
        .expect("ConflictOp serialises to a plain string")
        .to_owned()
}

/// `"<label>"` for every variant — the shape a mint takes in source.
fn quoted_labels() -> Vec<String> {
    all_conflict_ops()
        .into_iter()
        .map(|op| format!("\"{}\"", wire_label(op)))
        .collect()
}

/// The production lines of `file` that quote one of the labels.
fn lines_quoting_a_label(lines: &[SourceLine], file: &str) -> Vec<SourceLine> {
    let needles = quoted_labels();
    lines
        .iter()
        .filter(|l| l.file == file)
        .filter(|l| needles.iter().any(|n| l.text.contains(n.as_str())))
        .cloned()
        .collect()
}

#[test]
fn the_display_label_is_the_wire_spelling() {
    let ops = all_conflict_ops();
    assert!(!ops.is_empty(), "no variants to check");
    for op in ops {
        assert_eq!(
            op.to_string(),
            wire_label(op),
            "ConflictOp's display label and its serialised form are one \
             vocabulary; they must not drift apart"
        );
    }
}

#[test]
fn sync_does_not_re_mint_the_conflict_op_labels() {
    let lines = production_lines();

    let minted = lines_quoting_a_label(&lines, "vcs.rs");
    assert!(
        minted.len() >= all_conflict_ops().len(),
        "expected every label quoted at its mint in src/vcs.rs, found {} of {} \
         — the needles no longer match the source shape, so emptiness \
         elsewhere would prove nothing. Found: {minted:#?}",
        minted.len(),
        all_conflict_ops().len()
    );

    assert!(
        lines
            .iter()
            .any(|l| l.file == "sync.rs" && l.text.contains("ConflictOp")),
        "src/sync.rs no longer mentions ConflictOp — this scan is pointed at \
         the wrong corpus"
    );

    let hits: Vec<String> = lines_quoting_a_label(&lines, "sync.rs")
        .iter()
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "src/sync.rs must not spell the VCS's mid-op vocabulary — \
         ConflictOp's Display is where those labels are minted. Found: {hits:#?}"
    );
}
