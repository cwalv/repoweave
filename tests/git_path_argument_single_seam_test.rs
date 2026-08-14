//! Pins that a path becomes a `git` argument in exactly one place inside
//! `src/git.rs`, and that the place strips the Windows extended-length prefix.
//!
//! `std::fs::canonicalize` on Windows always answers in the `\\?\` form, rwv
//! walks to a canonicalized weave root, and git refuses an argument spelled
//! that way — `git worktree add` rejects a destination of `//?/C:/…` outright.
//! So a path has to lose the prefix before it reaches argv, and the cheap way
//! for that to regress is one more conversion written the way the others used
//! to be written.
//!
//! `git_command` is private to `src/git.rs`, so no frame outside that file can
//! assemble git argv at all. The compiler holds that half and this test does
//! not repeat it; what it holds is the half inside the file.
//!
//! The pin is structural because the defect cannot be observed here. No path
//! on this platform carries the prefix, `dunce::simplified` is a documented
//! no-op off Windows, and the whole suite is green with the strip removed —
//! measured, not assumed. The source, unlike the run, is the same on both
//! platforms.
//!
//! **What this does not buy.** It says the conversion is routed and the strip
//! is spelled. It does not say git accepts the result on Windows; only a run
//! there says that, and the advisory workflow is the one instrument that
//! performs it.
//!
//! Residue. The needles name two spellings of "render a path as text". A
//! `&Path` handed straight to `Command::arg` is neither, and no text scan can
//! separate that from a `&str` argument — the private-`git_command` closure
//! does not cover it either, since that one is about who may assemble argv
//! rather than what goes into it. A `to_str()` on something that is not a path
//! reads here as a conversion and would be a false positive. And `src_scan`'s
//! comment filter is line-leading `//` only, so a needle in a trailing or
//! block comment reads as a live use.

mod common;

use common::src_scan::{production_lines, SourceLine};

const OWNER: &str = "git.rs";
const SEAM: &str = "path_as_git_arg";
const STRIP: &str = "dunce::simplified";

/// The spellings that turn a `Path` into text a git argument can be built
/// from. Each must still occur somewhere in `src/`, which the vacuity guard
/// asserts — a spelling this codebase no longer writes would leave the absence
/// assertion below holding over a needle nothing can trip.
const CONVERSION_NEEDLES: &[&str] = &["to_str()", "to_string_lossy"];

/// The seam's declaration line through its closing brace: production lines of
/// `src/git.rs` from the `fn` line to the first bare `}` at column zero, which
/// is the only unindented line a function body can end on. `None` when the
/// declaration is gone or nothing closes it.
fn seam_body(lines: &[SourceLine]) -> Option<Vec<&SourceLine>> {
    let owned: Vec<&SourceLine> = lines.iter().filter(|l| l.file == OWNER).collect();
    let decl = format!("fn {SEAM}");
    let start = owned.iter().position(|l| l.text.contains(&decl))?;
    let close = owned[start + 1..].iter().position(|l| l.text == "}")?;
    Some(owned[start..=start + 1 + close].to_vec())
}

/// Call sites of the seam in `src/git.rs`, outside the seam's own body.
fn call_sites(lines: &[SourceLine], body: &[&SourceLine]) -> Vec<String> {
    let (first, last) = (body[0].line, body[body.len() - 1].line);
    let needle = format!("{SEAM}(");
    lines
        .iter()
        .filter(|l| l.file == OWNER && (l.line < first || l.line > last))
        .filter(|l| l.text.contains(&needle))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect()
}

#[test]
fn the_scan_reaches_the_seam_it_pins() {
    let lines = production_lines();
    assert!(
        lines.iter().any(|l| l.file == OWNER),
        "no production lines scanned from src/{OWNER} — every assertion below \
         would hold vacuously"
    );

    let body = seam_body(&lines).unwrap_or_else(|| {
        panic!(
            "src/{OWNER} no longer declares `fn {SEAM}` with a body this scan \
             can bound — it was renamed, moved or rewritten, and this pin names \
             lines that are not it"
        )
    });
    assert!(
        body.len() > 1,
        "the scanned body of `{SEAM}` is its declaration line alone, so the \
         assertions below would prove nothing"
    );

    for needle in CONVERSION_NEEDLES {
        assert!(
            lines.iter().any(|l| l.text.contains(needle)),
            "`{needle}` no longer occurs anywhere in src/ — the spelling was \
             renamed or dropped, so finding none of it outside the seam below \
             would prove nothing"
        );
    }
}

#[test]
fn the_seam_strips_the_prefix_git_refuses() {
    let lines = production_lines();
    let body = seam_body(&lines).expect("checked by the scan-reach test above");

    assert!(
        body.iter().any(|l| l.text.contains(STRIP)),
        "`{SEAM}` no longer spells `{STRIP}`, so a path reaches git argv in \
         whatever form `canonicalize` produced it. On Windows that is the \
         `\\\\?\\` extended-length form and git refuses it. Routing every \
         conversion through one function is worth nothing if the function \
         stopped normalizing.\n\
         \n\
         Body scanned: {body:#?}"
    );
}

#[test]
fn every_path_to_git_argument_conversion_is_in_the_seam() {
    let lines = production_lines();
    let body = seam_body(&lines).expect("checked by the scan-reach test above");
    let (first, last) = (body[0].line, body[body.len() - 1].line);

    let strays: Vec<String> = lines
        .iter()
        .filter(|l| l.file == OWNER && (l.line < first || l.line > last))
        .filter(|l| CONVERSION_NEEDLES.iter().any(|n| l.text.contains(n)))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        strays.is_empty(),
        "src/{OWNER} turns a path into text outside `{SEAM}`. Every git \
         argument built from a path is assembled in this file, and on Windows \
         every one of those paths descends from a canonicalized weave root and \
         carries the `\\\\?\\` extended-length prefix that git refuses. A \
         conversion that does not pass through `{SEAM}` hands that prefix \
         straight to argv, and no run on any other platform can tell.\n\
         \n\
         Found: {strays:#?}"
    );
}

#[test]
fn the_seam_is_the_route_and_not_a_dead_helper() {
    let lines = production_lines();
    let body = seam_body(&lines).expect("checked by the scan-reach test above");
    let calls = call_sites(&lines, &body);

    assert!(
        !calls.is_empty(),
        "`{SEAM}` has no call sites in src/{OWNER}. The absence assertion above \
         is satisfied by a file that converts no paths at all, so without a \
         live caller it reports the seam is unbypassed and means the seam is \
         unused."
    );
}
