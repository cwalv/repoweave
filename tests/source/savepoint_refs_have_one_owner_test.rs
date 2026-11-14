//! The reference namespace rwv keeps its savepoints and backups under is
//! spelled only in `git/rwv_ref.rs`, the registry of the backend whose layout
//! it is.
//!
//! The registry's `RwvRef` mints the names, and
//! the `Vcs` trait relays them out: a `Backup` carries its operator-spellable
//! `BackupName`, `savepoint_label` and `savepoint_namespace` do the same for
//! the savepoint side. Callers hold an opaque op-id and an opaque label. The trait says so in as many words, and
//! the claim was false where it stood — `sync.rs` composed the namespace three
//! times, once into a runnable `git reset --hard` an operator is invited to
//! paste. A second backend relaying a different layout would leave that
//! command pointing at nothing.
//!
//! The needle is the namespace root, so it covers both the savepoint and the
//! backup halves. What it asserts is a module boundary rather than a single
//! site: the layout is spelled by the registry alone, and every other place in
//! the backend, the recognizer of rwv's own refs among foreign tag names
//! included, reads it from there. The prohibition is on composing the
//! namespace outside its owner.
//!
//! Doc comments are exempt, and deliberately — `src_scan` drops them. That is
//! a real hole and worth naming: the `--discard-local-commits` help text in
//! `cli.rs` is a doc comment clap lifts onto `rwv sync --help`, so an operator
//! surface spelling the namespace sits outside this scan. Closing it would
//! mean rendering help text at runtime, which clap's derive does not offer.
//!
//! Residue, for anyone extending this. The scan is blind to a namespace
//! assembled rather than written, and to the recovery *command* wrapped around
//! a label: `sync.rs` still writes `git reset --hard` and `git update-ref` in
//! backend-agnostic code, which is git command vocabulary rather than rwv's
//! own layout and is not what this pins. It inherits `src_scan`'s line-leading
//! `//` comment filter, so the namespace in a trailing comment reads as
//! absent, and it says nothing about `tests/`, an external crate where
//! fixtures spell refs by design.

use crate::common;

use common::src_scan::{production_lines, SourceLine};

/// The namespace root, the file that owns it, and the spelling of each half.
const NAMESPACE: &str = "refs/rwv";
const OWNER: &str = "git/rwv_ref.rs";
const DECLARED_AS: &str = "\"refs/rwv/pre-op\"";
const BACKUP_DECLARED_AS: &str = "\"refs/rwv/backup\"";

fn sites<'a>(lines: &'a [SourceLine], needle: &str) -> Vec<&'a SourceLine> {
    lines.iter().filter(|l| l.text.contains(needle)).collect()
}

#[test]
fn the_scan_is_pointed_at_a_whole_source_tree() {
    let lines = production_lines();
    assert!(
        lines.len() >= 10_000,
        "expected at least 10000 production lines under src/, got {} — an \
         all-sites-in-one-file result below would be measuring the corpus, \
         not the source",
        lines.len()
    );
}

#[test]
fn the_ref_namespace_is_spelled_only_in_its_backend() {
    let lines = production_lines();
    let found = sites(&lines, NAMESPACE);

    assert!(
        !found.is_empty(),
        "`{NAMESPACE}` was not found anywhere under src/. The mints have to \
         exist — a zero-site scan means the needle stopped matching, and every \
         assertion below would pass on an empty set"
    );

    let strays: Vec<String> = found
        .iter()
        .filter(|l| l.file != OWNER)
        .map(|l| format!("{}: {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        strays.is_empty(),
        "`{NAMESPACE}` must be spelled only in {OWNER}, which owns the layout. \
         A site elsewhere is a caller composing a name it should have taken \
         from `Vcs::savepoint_label`, `Vcs::savepoint_namespace` or a returned \
         `Backup`. Found: {strays:?}"
    );

    let declarations: Vec<&str> = found
        .iter()
        .filter(|l| l.text.contains(DECLARED_AS))
        .map(|l| l.text.trim())
        .collect();
    assert_eq!(
        declarations.len(),
        1,
        "the savepoint namespace must be spelled once, as {DECLARED_AS}; \
         without it this file's boundary check no longer says what it \
         claims. Found: {declarations:?}"
    );

    let backup_declarations: Vec<&str> = found
        .iter()
        .filter(|l| l.text.contains(BACKUP_DECLARED_AS))
        .map(|l| l.text.trim())
        .collect();
    assert_eq!(
        backup_declarations.len(),
        1,
        "the backup namespace must be spelled at exactly one site, as \
         {BACKUP_DECLARED_AS}. Found: {backup_declarations:?}"
    );
}

#[test]
fn a_namespace_composed_outside_the_backend_is_what_this_reports() {
    let planted = |file: &str, text: &str| SourceLine {
        file: file.to_string(),
        line: 1,
        text: text.to_string(),
    };
    let corpus = vec![
        planted(
            "git/rwv_ref.rs",
            "            Self::PreOp => \"refs/rwv/pre-op\",",
        ),
        planted(
            "sync.rs",
            "         refs/rwv/pre-op/{op_id} (recover with `git reset --hard`",
        ),
        planted("sync.rs", "    let label = vcs.savepoint_label(op_id);"),
    ];

    let found = sites(&corpus, NAMESPACE);
    assert_eq!(
        found.len(),
        2,
        "the seeded composition must be reported alongside the declaration; \
         got {:?}",
        found.iter().map(|l| l.text.trim()).collect::<Vec<_>>()
    );
    assert!(
        found.iter().any(|l| l.file == "sync.rs"),
        "the seeded composition is the one this scan exists for and it went \
         unseen"
    );
    assert!(
        found.iter().all(|l| !l.text.contains("savepoint_label")),
        "a caller taking the name from the relay must not register — reaching \
         the owner is exactly what this scan is asking for"
    );
}
