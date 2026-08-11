//! Tripwire: no caller outside the git seam hand-strips a `refs/heads/`
//! prefix to turn a qualified ref into a bare branch name.
//!
//! `list_local_branches` used to return `Vec<RefName>` fully qualified via
//! `%(refname)`, and its one caller in `sync.rs` hand-stripped the prefix
//! before it could pass a bare name to `branch_has_remote_counterpart`.
//! It is gone now: `list_local_branch_names` (`Vec<RawRefName>`, bare via
//! `%(refname:lstrip=2)`) is the only local-branch listing left, so a
//! caller has nothing qualified to strip. This pins that a future
//! qualified listing does not reintroduce the pattern — only `git.rs`,
//! which owns git ref-name parsing, may convert `refs/heads/<name>` to
//! `<name>` by hand.
//!
//! Comment lines are excluded — occurrence outside a comment is the test,
//! per the stale-symbol rule in CLAUDE.md — so this file's own header is
//! not itself a violation.

use std::path::{Path, PathBuf};

const SEAM_FILE: &str = "git.rs";
const PATTERNS: &[&str] = &[
    "trim_start_matches(\"refs/heads/\"",
    "strip_prefix(\"refs/heads/\"",
];

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_the_git_seam_hand_strips_refs_heads() {
    let src = src_dir();
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(!files.is_empty(), "no source files found under {src:?}");

    let mut offenders = Vec::new();
    let mut seam_saw_the_pattern = false;
    for file in &files {
        let text = std::fs::read_to_string(file).expect("readable source file");
        let rel = file
            .strip_prefix(&src)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let is_seam = rel == SEAM_FILE;
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if PATTERNS.iter().any(|p| line.contains(p)) {
                if is_seam {
                    seam_saw_the_pattern = true;
                } else {
                    offenders.push(format!("{rel}:{} — {}", n + 1, line.trim()));
                }
            }
        }
    }

    // A pattern that matches nowhere — because the seam's own parse moved
    // or was rewritten — would make the offenders check below pass
    // vacuously. Require the seam to still exhibit the shape this test
    // pins against, so a drifted pattern fails loudly instead of going
    // quiet.
    assert!(
        seam_saw_the_pattern,
        "expected {SEAM_FILE} to still contain a refs/heads/ prefix strip; \
         if the seam's ref parsing was rewritten, update this test's \
         PATTERNS rather than deleting the guard"
    );
    assert!(
        offenders.is_empty(),
        "a caller outside {SEAM_FILE} hand-stripped a refs/heads/ prefix; \
         a VCS listing should return names in the shape callers need \
         (see `list_local_branch_names`) rather than forcing the caller to \
         convert:\n{}",
        offenders.join("\n")
    );
}
