//! Pins one `classify_lock_relations` call per workspace side on the sync path.
//!
//! `classify_lock_relations` reads each checkout's live HEAD, so two calls
//! against the same workspace read it at two instants. The source's relations
//! feed two consumers — the tips-as-truth note and replay's targets — and when
//! those came from separate calls a member that was `Ok` at the first read and
//! `Ahead` at the second pinned no tip while the note announced one: the pull
//! took the lock and said it took the tips.
//!
//! The fix is structural, so the pin is too. A behavioural test here would have
//! to widen the window with a sleep, and a sleep-timed test pins the sleep.
//!
//! Residue: this counts calls per enclosing function, so it cannot tell a
//! second call against an already-classified workspace from a first call
//! against a new one. Both land as a count change that has to be justified.

mod common;

use common::src_scan::{production_lines, SourceLine};
use std::collections::BTreeMap;

const OWNER: &str = "sync.rs";
const PREDICATE: &str = "classify_lock_relations";

/// The call shape of [`PREDICATE`], so the guard below and the scan below
/// cannot name different functions.
fn needle() -> String {
    format!("{PREDICATE}(")
}

/// The name of the item on a top-level `fn` line, `None` for anything else.
/// Indented lines are skipped, so a nested closure never re-attributes a site.
fn top_level_fn_name(text: &str) -> Option<&str> {
    if text.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = text
        .strip_prefix("pub(crate) ")
        .or_else(|| text.strip_prefix("pub "))
        .unwrap_or(text);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("fn ")?;
    rest.split(['(', '<', ' ']).next()
}

/// Call sites of [`needle`] in `src/sync.rs`, keyed by enclosing function.
/// The definition's own `fn` line is not a call site.
fn call_sites_by_function(lines: &[SourceLine]) -> BTreeMap<String, Vec<String>> {
    let needle = needle();
    let mut enclosing: Option<String> = None;
    let mut sites: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for l in lines.iter().filter(|l| l.file == OWNER) {
        if let Some(name) = top_level_fn_name(&l.text) {
            enclosing = Some(name.to_string());
            continue;
        }
        if !l.text.contains(needle.as_str()) {
            continue;
        }
        let owner = enclosing
            .clone()
            .unwrap_or_else(|| "(no enclosing fn)".to_string());
        sites
            .entry(owner)
            .or_default()
            .push(format!("{} {}", l.site(), l.text.trim()));
    }
    sites
}

#[test]
fn the_scan_reaches_the_predicate_it_pins() {
    let lines = production_lines();
    assert!(
        lines.iter().any(|l| l.file == OWNER),
        "no production lines scanned from src/{OWNER} — every assertion below \
         would hold vacuously"
    );
    assert!(
        lines
            .iter()
            .any(|l| top_level_fn_name(&l.text) == Some(PREDICATE)),
        "src/{OWNER} no longer declares `{PREDICATE}` at top level. The needle \
         names a function that moved or was renamed, so finding no extra call \
         sites would prove nothing."
    );
}

#[test]
fn each_workspace_side_is_classified_once_on_the_sync_path() {
    let lines = production_lines();
    let sites = call_sites_by_function(&lines);

    let actual: BTreeMap<&str, usize> = sites.iter().map(|(k, v)| (k.as_str(), v.len())).collect();
    let expected: BTreeMap<&str, usize> = BTreeMap::from([
        ("pin_source_snapshot", 1),
        ("run_preconditions_after_acquire", 1),
    ]);

    assert_eq!(
        actual, expected,
        "each side of a sync is classified exactly once: the source in \
         `pin_source_snapshot`, whose result the whole path reads back off \
         `SourceSnapshot::source_class`, and the destination in \
         `run_preconditions_after_acquire`.\n\
         \n\
         A second call inside a function that already has one re-reads a \
         workspace this op has already classified, at a later instant — the \
         note and replay's targets then describe two different states of the \
         same checkouts. Thread the existing value instead.\n\
         \n\
         A call in a new function is a new workspace or a new op path; say \
         which in the assertion above.\n\
         \n\
         Found: {sites:#?}"
    );
}
