//! `src/sync.rs` composes a `mid-<op>` refusal from `ConflictOp`'s `Display`
//! and spells none of the mid-op labels (`rebase`, `merge`, `cherry-pick`)
//! itself. Two tables agree only until someone edits one of them, and the one
//! in sync is the one an operator reads.
//!
//! Neither side of the comparison is typed into this file: the labels come out
//! of serializing the enum, whose kebab-case wire spelling is the vocabulary
//! the display text uses, and the corpus is `src/` itself.

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
        .expect("ConflictOp serializes")
        .as_str()
        .expect("ConflictOp serializes to a plain string")
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
fn sync_does_not_re_mint_the_conflict_op_labels() {
    let lines = production_lines();

    let spelled_to_git = lines_quoting_a_label(&lines, "git.rs");
    for op in all_conflict_ops() {
        let needle = format!("\"{}\"", wire_label(op));
        assert!(
            spelled_to_git.iter().any(|l| l.text.contains(&needle)),
            "expected {needle} quoted where src/git.rs names the git subcommand — the \
             needles no longer match the source shape, so emptiness elsewhere would \
             prove nothing"
        );
    }

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
