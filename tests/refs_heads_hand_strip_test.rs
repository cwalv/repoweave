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
//! Scanned via `common::src_scan::production_lines`, whose own residue
//! applies here too: comment lines are excluded — occurrence outside a
//! comment is the test, per the stale-symbol rule in CLAUDE.md, so this
//! file's own header is not itself a violation — but only a line-leading
//! `//` counts as one; a `/* … */` block or a trailing `// …` after code is
//! read as production text. `#[cfg(test)]`-gated code is skipped too, so a
//! test fixture hand-rolling the same strip for its own oracle is invisible
//! here — it is not a caller of the seam.

mod common;

use common::src_scan::{production_lines, SourceLine};

const SEAM_FILE: &str = "git.rs";
const PATTERNS: &[&str] = &[
    "trim_start_matches(\"refs/heads/\"",
    "strip_prefix(\"refs/heads/\"",
];

#[test]
fn only_the_git_seam_hand_strips_refs_heads() {
    let lines = production_lines();
    assert!(!lines.is_empty(), "no production lines found under src/");

    let matches = |l: &SourceLine| PATTERNS.iter().any(|p| l.text.contains(p));

    let mut offenders = Vec::new();
    let mut seam_saw_the_pattern = false;
    for line in &lines {
        if !matches(line) {
            continue;
        }
        if line.file == SEAM_FILE {
            seam_saw_the_pattern = true;
        } else {
            offenders.push(format!("{} — {}", line.site(), line.text.trim()));
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
