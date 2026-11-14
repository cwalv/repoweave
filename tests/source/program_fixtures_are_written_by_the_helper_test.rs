//! A fixture program is written by `common::write_executable`, never by
//! `fs::write` followed by a mode change.
//!
//! Exec fails with ETXTBSY while any process holds the file open for writing,
//! and a write handle this process held is inherited by every child another
//! test thread forks until that child execs. A script written in this
//! process and run at once is refused at random under a parallel runner; the
//! helper writes it through a child process instead.
//!
//! The scan reads `tests/` for a `from_mode` literal with an execute bit. It
//! holds nothing about a program written some other way (a `File::create`
//! with a builder mode), and a mode computed rather than written as a literal
//! is not seen.

use std::path::{Path, PathBuf};

/// Files whose execute-bit literals are not a program this process runs, and
/// why.
const NOT_A_PROGRAM: &[(&str, &str)] = &[
    (
        "tests/process/plugins_dispatch_test.rs",
        "a directory named like a plugin, so the spawn fails",
    ),
    (
        "tests/sync/land_drift_test.rs",
        "sets the mode of tracked data files to test what the VCS reads of it",
    ),
    (
        "tests/weave/workweave_test.rs",
        "restores a directory's mode so the temp directory can be removed",
    ),
    (
        "tests/integrations/read_only_reference_test.rs",
        "the mode a seed repository records for its script; the repository's \
         own checkout is what runs, not this process's write",
    ),
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The lines of `text` that write a `from_mode` literal with an execute bit.
fn executable_mode_lines(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            let Some(at) = line.find("from_mode(0o") else {
                return false;
            };
            let digits: String = line[at + "from_mode(0o".len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits
                .chars()
                .filter_map(|c| c.to_digit(8))
                .any(|d| d & 1 == 1)
        })
        .map(|(n, _)| n + 1)
        .collect()
}

#[test]
fn the_scan_reads_an_execute_bit_literal_and_spares_the_rest() {
    let text =
        "a(from_mode(0o755));\nb(from_mode(0o644));\nc(from_mode(0o000));\nd(from_mode(0o700));";
    assert_eq!(executable_mode_lines(text), vec![1, 4]);
}

#[test]
fn no_test_writes_a_program_with_fs_write_and_a_mode_change() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("tests"), &mut files);
    files.sort();
    assert!(!files.is_empty(), "the scan read no test file");

    let mut found = Vec::new();
    let mut seen_allowed = Vec::new();
    for file in &files {
        let rel = file
            .strip_prefix(&root)
            .expect("under the crate")
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "tests/common/mod.rs"
            || rel.ends_with("program_fixtures_are_written_by_the_helper_test.rs")
        {
            continue;
        }
        let text = std::fs::read_to_string(file).expect("read test source");
        let lines = executable_mode_lines(&text);
        if lines.is_empty() {
            continue;
        }
        if NOT_A_PROGRAM.iter().any(|(allowed, _)| *allowed == rel) {
            seen_allowed.push(rel);
            continue;
        }
        found.extend(lines.into_iter().map(|n| format!("{rel}:{n}")));
    }
    assert!(
        found.is_empty(),
        "write a fixture program with `common::write_executable`; these set an execute \
         bit after `fs::write`:\n{}",
        found.join("\n")
    );
    for (allowed, _) in NOT_A_PROGRAM {
        assert!(
            seen_allowed.iter().any(|seen| seen == allowed),
            "{allowed} no longer sets an execute bit; drop its entry"
        );
    }
}
