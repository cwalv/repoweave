//! Pins the mid-op label vocabulary (`rebase`, `merge`, `cherry-pick`) to
//! `ConflictOp` in `src/vcs.rs`.
//!
//! sync used to carry its own table mapping each variant to its hyphen
//! spelling. Two tables agree only until someone edits one of them, and the
//! one in sync is the one an operator reads in a `mid-<op>` refusal.
//!
//! The labels are not typed into this file: they come out of serialising the
//! enum, whose kebab-case wire spelling is the same vocabulary the display
//! text uses.

use repoweave::vcs::ConflictOp;
use std::path::{Path, PathBuf};

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

/// `"<label>"` for every variant — the shape a re-mint would take in source.
fn quoted_labels() -> Vec<String> {
    all_conflict_ops()
        .into_iter()
        .map(|op| format!("\"{}\"", wire_label(op)))
        .collect()
}

fn sync_source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sync.rs")
}

/// Lines of `text` quoting one of `needles`, skipping comment lines and —
/// when `skip_test_items` — whatever each `#[cfg(test)]` gates. A test
/// fixture quoting a label is not a mint.
fn quoting_lines(text: &str, needles: &[String], skip_test_items: bool) -> Vec<String> {
    let mut hits = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if skip_test_items && trimmed.starts_with("#[cfg(test)]") {
            skip_gated_item(&mut lines);
            continue;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        if needles.iter().any(|n| trimmed.contains(n.as_str())) {
            hits.push(trimmed.to_owned());
        }
    }
    hits
}

/// Consume lines through the end of whatever `#[cfg(test)]` gates: a brace
/// block tracked by depth, or a brace-less item's terminating `;`.
fn skip_gated_item(lines: &mut std::str::Lines) {
    let mut depth: i32 = 0;
    let mut seen_open = false;
    for line in lines {
        seen_open |= line.contains('{');
        depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
        if seen_open && depth <= 0 {
            return;
        }
        if !seen_open && line.trim_end().ends_with(';') {
            return;
        }
    }
}

/// The scanner's own fixture: one production mint, one comment, one fixture
/// inside a gated module. A scanner that reports none of these — or all of
/// them — cannot be trusted with the real file below.
#[test]
fn the_scanner_reports_a_mint_and_ignores_comments_and_test_items() {
    let needles = quoted_labels();
    let fixture = "fn label(op: ConflictOp) -> &'static str {\n\
                   \x20   match op {\n\
                   \x20       ConflictOp::Merge => \"merge\",\n\
                   \x20   }\n\
                   }\n\
                   // a comment naming \"rebase\" is not a mint\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                   \x20   let fixture = \"cherry-pick\";\n\
                   }\n";

    let hits = quoting_lines(fixture, &needles, true);
    assert_eq!(
        hits,
        vec!["ConflictOp::Merge => \"merge\",".to_string()],
        "the scanner must report the production mint and nothing else"
    );
    assert_eq!(
        quoting_lines(fixture, &needles, false).len(),
        2,
        "with the gated item left in, the same fixture yields the mint and the \
         fixture quote; a smaller count means the needles no longer match the \
         source shape and the emptiness above proves nothing"
    );
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
    let text = std::fs::read_to_string(sync_source()).expect("read src/sync.rs");
    assert!(
        text.contains("ConflictOp"),
        "src/sync.rs no longer mentions ConflictOp — this scan is pointed at \
         the wrong corpus"
    );

    let hits = quoting_lines(&text, &quoted_labels(), true);
    assert!(
        hits.is_empty(),
        "src/sync.rs must not spell the VCS's mid-op vocabulary — \
         ConflictOp's Display is where those labels are minted. Found: {hits:#?}"
    );
}
