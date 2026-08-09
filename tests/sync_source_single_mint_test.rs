//! Pins the `SyncSource` "primary" token to `SyncSource` in `src/sync.rs`.
//!
//! `cli/dispatch.rs` used to match `Some("primary")` by hand while resolving
//! `rwv workweave create --from`, re-minting the disambiguation
//! `SyncSource::from_str` already performs.
//!
//! The needle is `SyncSource::Primary`'s own `Display` spelling, so a rename
//! moves it with the type. It is scoped to comparison and match-arm shapes
//! rather than the bare word: `"primary"` also spells two unrelated
//! vocabularies in production code — `Role::LEGACY_SPELLING` (manifest.rs)
//! and the `.rwv-workweave` marker's `primary:` field name (workspace.rs,
//! read via `field("primary")` / `.as_mapping_get("primary")`) — both
//! call-argument or assignment shapes, never a comparison or match arm, so
//! scoping the needle this way drops both known collisions for free.
//!
//! Residue: a hand-rolled dispatch spelled as `matches!(s, "primary")` or a
//! bare `match` guard is not one of the shapes below and would not be caught.

mod common;

use common::src_scan::{production_lines, SourceLine};
use repoweave::sync::SyncSource;

/// The textual shapes a hand-rolled `"primary"` comparison takes — the ways
/// a caller decides "this string means `SyncSource::Primary`" without going
/// through `SyncSource::from_str`.
fn primary_comparison_needles() -> Vec<String> {
    let s = SyncSource::Primary.to_string();
    vec![
        format!("Some(\"{s}\")"),
        format!("\"{s}\" =>"),
        format!("== \"{s}\""),
        format!("\"{s}\" =="),
        format!(".eq(\"{s}\")"),
    ]
}

fn lines_quoting_a_needle(lines: &[SourceLine], file: &str) -> Vec<SourceLine> {
    let needles = primary_comparison_needles();
    lines
        .iter()
        .filter(|l| l.file == file)
        .filter(|l| needles.iter().any(|n| l.text.contains(n.as_str())))
        .cloned()
        .collect()
}

#[test]
fn the_primary_spelling_round_trips() {
    let s = SyncSource::Primary.to_string();
    assert_eq!(
        s.parse::<SyncSource>().unwrap(),
        SyncSource::Primary,
        "the spelling this test's needles are derived from must be the one \
         SyncSource::from_str actually accepts"
    );
}

#[test]
fn no_module_outside_sync_spells_the_primary_sync_source() {
    let lines = production_lines();
    let owner = "sync.rs";

    let minted = lines_quoting_a_needle(&lines, owner);
    assert!(
        !minted.is_empty(),
        "expected SyncSource::from_str's comparison quoted at its mint in \
         src/{owner}, found none — the needles no longer match the source \
         shape, so emptiness elsewhere would prove nothing."
    );

    let hits: Vec<String> = lines
        .iter()
        .filter(|l| l.file != owner)
        .filter(|l| {
            primary_comparison_needles()
                .iter()
                .any(|n| l.text.contains(n.as_str()))
        })
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        hits.is_empty(),
        "the `primary` SyncSource token is minted by SyncSource::from_str in \
         src/{owner}; a module that compares a string against it by hand is a \
         second parse path waiting to disagree. Route it through \
         SyncSource::from_str. Found: {hits:#?}"
    );
}
