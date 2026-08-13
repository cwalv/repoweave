//! Pins one `classify_checkout` read per checkout decision on the sync path.
//!
//! `classify_checkout` reads the filesystem, so two calls against the same
//! path answer at two instants. The replay loop both SKIPS a checkout and
//! PRINTS why, and when the label came from a second call the printed reason
//! could describe a state the skip decision was never made on. Routing every
//! sync-side question through `checkout_syncability` means the decision and
//! its label are the same value.
//!
//! The fix is structural, so the pin is too: the disagreement needs the
//! filesystem to change inside a window measured in microseconds, and a test
//! that widens the window with a sleep pins the sleep.
//!
//! Residue: this reads `src/sync.rs` only. Every other caller of
//! `classify_checkout` — `workweave.rs`, `check.rs` — is unexamined here, and
//! a second read introduced in one of them is invisible to this test. It also
//! counts calls per enclosing function, so a function holding one call that
//! moves to a different path within it still reads as one.

mod common;

use common::src_scan::{production_lines, SourceLine};
use std::collections::BTreeMap;

const OWNER: &str = "sync.rs";
const PREDICATE: &str = "classify_checkout";
const CHOKEPOINT: &str = "checkout_syncability";

/// The call shape of [`PREDICATE`], so the guards and the scan below cannot
/// name different functions.
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
fn the_scan_reaches_the_functions_it_pins() {
    let lines = production_lines();
    assert!(
        lines.iter().any(|l| l.file == OWNER),
        "no production lines scanned from src/{OWNER} — every assertion below \
         would hold vacuously"
    );
    assert!(
        lines
            .iter()
            .any(|l| top_level_fn_name(&l.text) == Some(CHOKEPOINT)),
        "src/{OWNER} no longer declares `{CHOKEPOINT}` at top level. The sync \
         path's checkout questions are supposed to route through it, so finding \
         no extra `{PREDICATE}` call sites would prove nothing."
    );
}

#[test]
fn the_sync_path_classifies_a_checkout_through_one_chokepoint() {
    let lines = production_lines();
    let sites = call_sites_by_function(&lines);

    let actual: BTreeMap<&str, usize> = sites.iter().map(|(k, v)| (k.as_str(), v.len())).collect();
    let expected: BTreeMap<&str, usize> = BTreeMap::from([(CHOKEPOINT, 1)]);

    assert_eq!(
        actual, expected,
        "`{PREDICATE}` is called once in src/{OWNER}, inside `{CHOKEPOINT}`, whose \
         result every sync-side consumer reads.\n\
         \n\
         A call anywhere else asks the filesystem a second time about a checkout \
         this op has already classified. Where the two answers feed a decision and \
         the sentence explaining it, the explanation can name a state the decision \
         was not made on. Consume `{CHOKEPOINT}`'s value instead.\n\
         \n\
         Found: {sites:#?}"
    );
}
