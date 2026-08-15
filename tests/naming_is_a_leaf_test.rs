//! Pins the one property that makes `naming.rs` worth having: it names no
//! other module in this crate.
//!
//! The grammar and the name types moved there so that `manifest`, `workspace`
//! and `vcs` could stop reaching sideways and upward for them. That only holds
//! while the leaf stays a leaf. Nothing in the language enforces it — Rust
//! resolves intra-crate cycles silently, which is how the triangle this module
//! was extracted to break went unreported for as long as it did. A single
//! `use crate::…` added here re-creates it, and every other test in the suite
//! stays green.
//!
//! The needle is the `crate::` path prefix, in any position: a `use` line, a
//! fully-qualified call, a type in a signature, an intra-doc link in code.
//! Comment lines are dropped by `src_scan` before this sees them, so the
//! module doc may discuss its consumers by name.
//!
//! Residue. `super::` and `self::` are not the needle and do not need to be:
//! they cannot leave the module. A dependency introduced through a macro that
//! expands to a `crate::` path is not visible in the pre-expansion source this
//! reads.

mod common;

use common::src_scan::production_lines;

const LEAF: &str = "naming.rs";
const CRATE_PATH: &str = "crate::";

#[test]
fn the_scan_reaches_the_leaf() {
    let lines = production_lines();
    let leaf_lines = lines.iter().filter(|l| l.file == LEAF).count();
    assert!(
        leaf_lines >= 100,
        "expected at least 100 production lines from src/{LEAF}, got {leaf_lines} \
         — the file was renamed, moved, or gutted, and the assertion below would \
         hold over nothing"
    );
}

#[test]
fn the_name_grammar_names_no_other_module() {
    let lines = production_lines();

    let deps: Vec<String> = lines
        .iter()
        .filter(|l| l.file == LEAF)
        .filter(|l| l.text.contains(CRATE_PATH))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        deps.is_empty(),
        "src/{LEAF} must not name another module in this crate. It holds the \
         name grammar and the types that grammar constrains precisely so that \
         `manifest`, `workspace` and `vcs` can depend on it without depending \
         on each other; a dependency pointing back out of it restores the \
         cycle those modules were untangled from, and the compiler will not \
         say so. If the grammar genuinely needs something from above, the \
         thing it needs belongs here or the caller should pass it in. \
         Found: {deps:#?}"
    );
}
