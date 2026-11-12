//! Structural pins over who writes a re-pin record.
//!
//! Licensed as prohibitions over an enumerable population: the call sites of
//! the seam's commit operations, and of the shared report of a pin moved
//! backward by name. Both read the production lines of every file under
//! `src/` (comment lines and `#[cfg(test)]` items dropped), so a site in a
//! trailing comment or a block comment is read as a use; nothing outside
//! `src/` is read.

use crate::common;

use common::src_scan::{production_lines, SourceLine};
use std::collections::BTreeSet;

fn in_seam(file: &str) -> bool {
    file == "git.rs" || file.starts_with("git/") || file == "vcs.rs" || file.starts_with("vcs/")
}

fn sites(needle: &str) -> Vec<SourceLine> {
    production_lines()
        .into_iter()
        .filter(|line| line.text.contains(needle))
        .collect()
}

/// A commit outside `src/repin.rs` could carry a lock change with no record
/// of the members its verb named: every commit the seam makes for a caller
/// goes through it.
#[test]
fn every_commit_of_a_staged_change_goes_through_the_record_writer() {
    for needle in [".commit_only(", ".commit_merge("] {
        let found = sites(needle);
        assert!(
            found.iter().any(|line| line.file == "repin.rs"),
            "the scan no longer sees src/repin.rs call {needle}"
        );
        let outside: Vec<String> = found
            .iter()
            .filter(|line| line.file != "repin.rs" && !in_seam(&line.file))
            .map(SourceLine::site)
            .collect();
        assert!(
            outside.is_empty(),
            "{needle} called outside src/repin.rs: {outside:?}"
        );
    }
}

/// A verb that reports a pin moved backward by name moved it by name, so
/// its lock commit names the members it moved: every file calling the report
/// also builds a by-name commit.
#[test]
fn every_file_reporting_a_backward_re_pin_by_name_commits_it_by_name() {
    let reporting: BTreeSet<String> = sites("report_backward_repin(")
        .into_iter()
        .filter(|line| !line.text.contains("fn report_backward_repin("))
        .map(|line| line.file)
        .collect();
    assert!(
        reporting.contains("member_pin.rs"),
        "the scan no longer sees member pin's report: {reporting:?}"
    );
    let naming: BTreeSet<String> = sites("by_name(")
        .into_iter()
        .map(|line| line.file)
        .collect();
    let unnamed: Vec<&String> = reporting.difference(&naming).collect();
    assert!(
        unnamed.is_empty(),
        "these report a backward re-pin by name but commit nothing by name: {unnamed:?}"
    );
}
