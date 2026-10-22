//! Pins where each `rwv sync-to --json` `step3_advance` comes from.
//!
//! The envelope's identity fields — `source_workweave`, `target`, `retired`,
//! `resumed` — are built from what the machine reported alone, by a type: the
//! outcome the envelope is built from lives in `src/sync/sync_to_envelope.rs`,
//! where only the handler sets its fields. The resumed-op tests in
//! `tests/sync_to_json_test.rs` separate the machine's answer from the
//! invocation's behaviorally.
//!
//! A `step3_advance` record is another matter. It reports the tip the
//! fast-forward decided against, and a second read of the target's HEAD beside
//! the fast-forward is a second instant: the two candidates are separated only
//! by a repo's HEAD moving between two adjacent reads, a race no fixture can
//! arrange and no type holds. So this pin is structural.
//!
//! Residue: a token scan catches the second read only in the spelling it
//! names. Taking the target's HEAD through a binding named something else
//! passes.

mod common;

use common::src_scan::{body_of, production_lines, top_level_fn_name, SourceLine};

const OWNER: &str = "sync.rs";

/// Sites in `name`'s body mentioning any of `needles`.
fn sites_naming(lines: &[SourceLine], name: &str, needles: &[&str]) -> Vec<String> {
    body_of(lines, OWNER, name)
        .iter()
        .filter(|l| needles.iter().any(|n| l.text.contains(n)))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect()
}

#[test]
fn the_scan_reaches_the_functions_it_pins() {
    let lines = production_lines();
    let name = "run_advance_target";
    assert!(
        lines
            .iter()
            .any(|l| top_level_fn_name(&l.text) == Some(name)),
        "src/{OWNER} no longer declares `{name}` at top level — the scan below \
         would read an empty body and pass on nothing"
    );
    assert!(
        !body_of(&lines, OWNER, name).is_empty(),
        "the body scan for `{name}` came back empty, so every assertion \
         against it holds vacuously"
    );
}

#[test]
fn advance_target_reports_the_tip_its_own_fast_forward_read() {
    let lines = production_lines();
    let found = sites_naming(
        &lines,
        "run_advance_target",
        &[
            "head_revision(&target_repo)",
            "head_revision(&ctx.dest_project_dir)",
        ],
    );
    assert!(
        found.is_empty(),
        "the `from_sha` in a `step3_advance` record is the tip `ff_advance_repo` \
         decided against, carried out on its `FfAdvance` outcome. A second read \
         of the target's HEAD here is a second instant: the text line and the \
         JSON record then answer `did this repo advance, and from where` from \
         two different observations of the same repo.\n\
         \n\
         Found: {found:#?}"
    );
}
