//! Pins the on-disk file names `rwv.yaml` and `rwv.lock` to the types that
//! own those formats, `Manifest` and `LockFile` in `src/manifest.rs`.
//!
//! The names are a published interface — operators type them, `.gitattributes`
//! merge rules name them — so the constants are `pub` and this test does not
//! ask anyone to hide them. It asks that they be written once. Before this
//! pin, thirty-three production sites spelled them in path joins and sync.rs
//! carried a private `const RWV_LOCK_FILE` of its own, one duplicate away from
//! a rename that moved most of the tree and left the rest reading a file that
//! no longer existed.
//!
//! The needle is the constant's own value in quotes, so a rename moves the
//! needle with it.
//!
//! Residue: this scans for the *quoted* form, which is how a path is built.
//! The names also appear unquoted inside operator sentences ("uncommitted
//! changes outside rwv.lock") and in doc comments. Those are prose about a
//! file, not a second mint of its name, and are deliberately not covered — a
//! rename has to sweep them by hand.

mod common;

use common::src_scan::{production_lines, SourceLine};
use repoweave::manifest::{LockFile, Manifest};

/// `("owner file", "\"<name>\"")` for each format whose name is pinned.
fn pinned_names() -> Vec<(&'static str, String)> {
    vec![
        ("manifest.rs", format!("\"{}\"", Manifest::FILE_NAME)),
        ("manifest.rs", format!("\"{}\"", LockFile::FILE_NAME)),
    ]
}

fn lines_quoting(lines: &[SourceLine], needle: &str) -> Vec<SourceLine> {
    lines
        .iter()
        .filter(|l| l.text.contains(needle))
        .cloned()
        .collect()
}

#[test]
fn the_two_file_names_are_distinct() {
    assert_ne!(
        Manifest::FILE_NAME,
        LockFile::FILE_NAME,
        "one needle standing in for both would make this scan blind to a \
         mint of the other"
    );
}

#[test]
fn no_module_outside_the_format_owner_spells_the_file_names() {
    let lines = production_lines();
    assert!(
        !lines.is_empty(),
        "the source scan yielded nothing — it is pointed at the wrong corpus"
    );

    for (owner, needle) in pinned_names() {
        let all = lines_quoting(&lines, &needle);
        let at_owner: Vec<&SourceLine> = all.iter().filter(|l| l.file == owner).collect();
        assert!(
            !at_owner.is_empty(),
            "expected {needle} at its mint in src/{owner} and found none — the \
             needle no longer matches the source shape, so emptiness elsewhere \
             would prove nothing"
        );

        let elsewhere: Vec<String> = all
            .iter()
            .filter(|l| l.file != owner)
            .map(|l| format!("{} {}", l.site(), l.text.trim()))
            .collect();
        assert!(
            elsewhere.is_empty(),
            "{needle} is minted by a FILE_NAME constant in src/{owner}; building \
             a path from a re-typed literal is what lets a rename move most of \
             the tree and leave the rest reading a file that is gone. \
             Found: {elsewhere:#?}"
        );
    }
}
