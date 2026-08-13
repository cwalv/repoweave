//! Pins zero `head_revision` reads inside the entry-tips filter (advanced_tips
//! write 1) in `run_replay`.
//!
//! `SyncTask.containment` is read once, in the task-build loop, from
//! `vcs.head_revision`. Both consumers — `sync_one_repo` and the entry-tips
//! filter that pre-writes `advanced_tips` for genuine fast-forwards — read
//! that one field back rather than asking again. A `head_revision` call
//! inside the filter re-asks the filesystem after the task-build loop already
//! asked it once, so a commit landing between the two reads would let the
//! written intent describe a HEAD the task's containment verdict was never
//! computed against.
//!
//! The fix is structural, so the pin is too: the window between the two reads
//! is microseconds with no seam to land a commit inside it, so a behavioural
//! test would need a sleep, and a sleep-timed test pins the sleep.
//!
//! Residue: the block is found by two production-code string anchors, not by
//! brace matching, so a rewrite of either anchor line fails the vacuity guard
//! below rather than letting the main assertion pass on an empty slice. A
//! `head_revision` read added between the anchors under a different spelling
//! (an alias import, a wrapper method) is not this needle's shape.

mod common;

use common::src_scan::{production_lines, SourceLine};

const OWNER: &str = "sync.rs";
const NEEDLE: &str = "head_revision(";
const BLOCK_START: &str = "let entry_tips: std::collections::BTreeMap<String, String> = sync_tasks";
const BLOCK_END: &str = ".context(\"failed to write advanced_tips at replay entry\")?;";

/// The entry-tips filter's production lines, in file order: the slice of
/// `sync.rs` from the block's opening `let` through its trailing
/// `.context(...)?;`, both inclusive. `None` when either anchor is missing,
/// or the end anchor sorts before the start.
fn entry_tips_write_lines(lines: &[SourceLine]) -> Option<Vec<&SourceLine>> {
    let owned: Vec<&SourceLine> = lines.iter().filter(|l| l.file == OWNER).collect();
    let start = owned.iter().position(|l| l.text.contains(BLOCK_START))?;
    let end = owned.iter().position(|l| l.text.contains(BLOCK_END))?;
    if end < start {
        return None;
    }
    Some(owned[start..=end].to_vec())
}

#[test]
fn the_scan_reaches_the_pinned_block() {
    let lines = production_lines();
    assert!(
        lines.iter().any(|l| l.file == OWNER),
        "no production lines scanned from src/{OWNER} — every assertion below \
         would hold vacuously"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.file == OWNER && l.text.contains(NEEDLE)),
        "src/{OWNER} no longer spells `{NEEDLE}` anywhere — the needle moved \
         or was renamed, so finding none inside the block below would prove \
         nothing."
    );
    assert!(
        entry_tips_write_lines(&lines).is_some(),
        "src/{OWNER} no longer contains both `{BLOCK_START}` and `{BLOCK_END}` \
         in that order — the entry-tips write block moved or was rewritten, \
         and this pin names the wrong lines."
    );
}

#[test]
fn the_entry_tips_filter_reads_containment_not_head_revision() {
    let lines = production_lines();
    let block = entry_tips_write_lines(&lines).expect("checked by the scan-reach test above");

    let strays: Vec<String> = block
        .iter()
        .filter(|l| l.text.contains(NEEDLE))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        strays.is_empty(),
        "the entry-tips filter that pre-writes advanced_tips reads \
         `task.containment`, computed once in the task-build loop above it — \
         it does not call `{NEEDLE}` itself. A call here re-asks the \
         filesystem after that loop already asked it, so a commit landing \
         between the two reads lets the written intent describe a HEAD the \
         containment verdict was never computed against. Thread the field \
         instead.\n\
         \n\
         Found: {strays:#?}"
    );
}
