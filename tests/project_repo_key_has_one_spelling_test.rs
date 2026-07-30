//! The key standing for the project repo in a per-repo map or JSON record is
//! written once in `src/`, at the constant that declares it.
//!
//! `manifest.rs` answers for it: `project_repo_key` hands back the one
//! spelling, and the constant behind it is module-private, so the compiler
//! refuses a foreign module that names it. What the compiler has nothing to
//! say about is a second module writing the literal again — which is how
//! eleven copies arrived across `sync.rs` and `workweave.rs`, keying op-state
//! tip tables, `rwv sync-to --json` records and `rwv workweave log --json`
//! records with a string each site spelled for itself.
//!
//! The needle is the closed string literal, quotes included. That is what a
//! map key, a `get`, a `remove` and a serialized field value all have to
//! spell, and it discriminates against the shape that must survive: the name
//! quoted inside a larger string, which is text an operator reads rather than
//! a key this code composes.
//!
//! Doc comments and `#[cfg(test)]` bodies are exempt, and deliberately.
//! `src_scan` drops both: a comment documenting the convention to an
//! implementor is prose about the contract, not a participant in it, and a
//! test asserting on the literal output is the test doing its job — a
//! fixture that reaches the owner instead can no longer catch the owner
//! changing under it.
//!
//! Residue, for anyone extending this. The scan is blind to the key written
//! into a longer string, which `sync.rs` does twice in dirt-report text
//! (`format!("(project): {files}")` and an `eprintln!` prefix) — separating a
//! re-introduced key from those would take a hard-coded site list, and a list
//! its author typed drifts. It is likewise blind to a key assembled rather
//! than written; it inherits `src_scan`'s line-leading `//` comment filter, so
//! a closed literal in a trailing comment reads as absent; and it says nothing
//! about `tests/`, an external crate where fixtures spell the key by design.

mod common;

use common::src_scan::{production_lines, SourceLine};

/// The sentinel, and the constant that is allowed to be its one site.
const SENTINEL: &str = "\"(project)\"";
const DECLARED_AS: &str = "PROJECT_REPO_KEY";

fn sites<'a>(lines: &'a [SourceLine], needle: &str) -> Vec<&'a SourceLine> {
    lines.iter().filter(|l| l.text.contains(needle)).collect()
}

#[test]
fn the_scan_is_pointed_at_a_whole_source_tree() {
    let lines = production_lines();
    assert!(
        lines.len() >= 10_000,
        "expected at least 10000 production lines under src/, got {} — a \
         one-site result below would be measuring the corpus, not the source",
        lines.len()
    );
}

#[test]
fn the_project_repo_key_is_spelled_once_and_at_its_declaration() {
    let lines = production_lines();
    let found = sites(&lines, SENTINEL);

    assert_eq!(
        found.len(),
        1,
        "`{SENTINEL}` must be written at exactly one production site. A second \
         one is either a map keyed past `manifest.rs` or a constant re-minting \
         a sentinel that already has an owner. Found: {:?}",
        found.iter().map(|l| l.site()).collect::<Vec<_>>()
    );

    let site = found[0];
    assert_eq!(
        site.file,
        "manifest.rs",
        "`{SENTINEL}`'s one site must be manifest.rs, beside the `RepoPath` \
         keyspace it is the exception to; found {}",
        site.site()
    );
    assert!(
        site.text.contains("const") && site.text.contains(DECLARED_AS),
        "`{SENTINEL}` survives at {} but not as `{DECLARED_AS}`'s declaration \
         — the name moved, so this file's one-site count no longer says what \
         it claims: {}",
        site.site(),
        site.text.trim()
    );
}

#[test]
fn a_key_written_outside_the_owner_is_what_this_reports() {
    let planted = |file: &str, text: &str| SourceLine {
        file: file.to_string(),
        line: 1,
        text: text.to_string(),
    };
    let corpus = vec![
        planted(
            "manifest.rs",
            "const PROJECT_REPO_KEY: &str = \"(project)\";",
        ),
        planted(
            "sync.rs",
            "    converged.insert(\"(project)\".to_owned(), rev);",
        ),
        planted(
            "sync.rs",
            "    eprintln!(\"  (project): ff-advance failed\");",
        ),
    ];

    let found = sites(&corpus, SENTINEL);
    assert_eq!(
        found.len(),
        2,
        "the seeded insert must be reported alongside the declaration; got {:?}",
        found.iter().map(|l| l.text.trim()).collect::<Vec<_>>()
    );
    assert!(
        found.iter().any(|l| l.text.contains("insert")),
        "the seeded insert is the one this scan exists for and it went unseen"
    );
    assert!(
        found.iter().all(|l| !l.text.contains("eprintln")),
        "the key named inside a longer operator line must not register as a \
         key this code composes — it is what the operator is shown"
    );
}
