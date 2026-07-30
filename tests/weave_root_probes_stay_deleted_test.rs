//! `is_workweave_root` and `read_weave_root_project` do not come back.
//!
//! Weave-root identity was three separate probes over the same two files: a
//! marker existence test, a project read that checked the marker then fell
//! back to the pointer, and the pointer read itself. Each answered part of
//! one question, and a caller wanting the whole answer asked two of them and
//! joined the results at its own site. `observe_root` answers it once —
//! `presented_project` for the project, `container_kind` for the kind — so
//! the two collapsed probes are deleted rather than deprecated.
//!
//! A scan for names that are supposed to appear nowhere cannot use the usual
//! vacuity guard, which asserts the needle still exists at its owner: there
//! is no owner left. The guard here is a positive control on the replacement
//! instead. `observe_root` must appear in both the module that defines it and
//! the module whose callers were converted to call it, so an empty result for
//! the deleted names is evidence of deletion and not of a scan pointed at an
//! empty corpus, a renamed file, or a broken filter.

mod common;

use common::src_scan::production_lines;

/// The two collapsed probes, by the names they were called.
const DELETED: [&str; 2] = ["is_workweave_root", "read_weave_root_project"];

/// Where the answer lives now, and the two files that have to show it.
const REPLACEMENT: &str = "observe_root";
const REPLACEMENT_SITES: [&str; 2] = ["workspace.rs", "activate.rs"];

#[test]
fn the_scan_can_see_the_reader_that_replaced_them() {
    let lines = production_lines();
    assert!(
        lines.len() >= 10_000,
        "expected at least 10000 production lines under src/, got {} — this \
         scan is pointed at the wrong corpus, so an empty result below would \
         prove nothing",
        lines.len()
    );

    for file in REPLACEMENT_SITES {
        assert!(
            lines
                .iter()
                .any(|l| l.file == file && l.text.contains(REPLACEMENT)),
            "expected `{REPLACEMENT}` in src/{file} and found none — the \
             reader moved or was renamed, so this file's absence assertions \
             are no longer evidence of anything"
        );
    }
}

#[test]
fn neither_collapsed_probe_is_back() {
    let lines = production_lines();

    for needle in DELETED {
        let hits: Vec<String> = lines
            .iter()
            .filter(|l| l.text.contains(needle))
            .map(|l| format!("{} {}", l.site(), l.text.trim()))
            .collect();
        assert!(
            hits.is_empty(),
            "`{needle}` is a weave-root probe this design collapsed into \
             `observe_root`. A caller needing the project asks \
             `RootObservation::presented_project`; one needing the kind asks \
             `container_kind`; one needing to write the pointer asks for a \
             `PrimaryIdentity`. Found: {hits:#?}"
        );
    }
}
