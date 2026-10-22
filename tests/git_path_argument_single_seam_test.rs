//! Pins that the one conversion from a path to a `git` argument strips the
//! Windows extended-length prefix.
//!
//! A weave root rwv reads back from a record can carry the Windows `\\?\`
//! form, and git refuses an argument spelled that way — `git worktree add`
//! rejects a destination of `//?/C:/…` outright.
//!
//! That a path reaches argv through that one conversion and no other is held
//! by the types, not by this file: `tests/git_arg_seal_test.rs` pins that a
//! `&Path` fits no slot of an argv and that only `GitArg::from_path` makes a
//! `GitArg`. What a type cannot say is what the conversion does to the text.
//!
//! The pin is structural because the defect cannot be observed here. No path
//! on this platform carries the prefix, `dunce::simplified` is a documented
//! no-op off Windows, and the whole suite is green with the strip removed —
//! measured, not assumed. The source, unlike the run, is the same on both
//! platforms.
//!
//! **What this does not buy.** It says the strip is spelled. It does not say
//! git accepts the result on Windows; only a run there says that, and the
//! advisory workflow is the one instrument that performs it.

mod common;

use common::src_scan::{production_lines, SourceLine};

const OWNER: &str = "git/argv.rs";
const SEAM: &str = "fn from_path";
const STRIP: &str = "dunce::simplified";

/// The seam's declaration line through its closing brace: production lines of
/// `src/git/argv.rs` from the `fn` line to the first line that is exactly a
/// method's closing brace. `None` when the declaration is gone or nothing
/// closes it.
fn seam_body(lines: &[SourceLine]) -> Option<Vec<&SourceLine>> {
    let owned: Vec<&SourceLine> = lines.iter().filter(|l| l.file == OWNER).collect();
    let start = owned.iter().position(|l| l.text.contains(SEAM))?;
    let close = owned[start + 1..].iter().position(|l| l.text == "    }")?;
    Some(owned[start..=start + 1 + close].to_vec())
}

#[test]
fn the_scan_reaches_the_seam_it_pins() {
    let lines = production_lines();
    assert!(
        lines.iter().any(|l| l.file == OWNER),
        "no production lines scanned from src/{OWNER} — the assertion below \
         would hold vacuously"
    );

    let body = seam_body(&lines).unwrap_or_else(|| {
        panic!(
            "src/{OWNER} no longer declares `{SEAM}` with a body this scan \
             can bound — it was renamed, moved or rewritten, and this pin names \
             lines that are not it"
        )
    });
    assert!(
        body.len() > 1,
        "the scanned body of `{SEAM}` is its declaration line alone, so the \
         assertion below would prove nothing"
    );
}

#[test]
fn the_seam_strips_the_prefix_git_refuses() {
    let lines = production_lines();
    let body = seam_body(&lines).expect("checked by the scan-reach test above");

    assert!(
        body.iter().any(|l| l.text.contains(STRIP)),
        "`{SEAM}` no longer spells `{STRIP}`, so a path reaches git argv in \
         whatever form `resolve` produced it. On Windows that can be the \
         `\\\\?\\` extended-length form and git refuses it. Making every path \
         pass through one function is worth nothing if the function stopped \
         normalizing.\n\
         \n\
         Body scanned: {body:#?}"
    );
}
