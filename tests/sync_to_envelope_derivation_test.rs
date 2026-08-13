//! Pins where the `rwv sync-to --json` envelope's machine facts come from.
//!
//! `source_workweave`, `target`, `retired` and each `step3_advance` describe
//! what the machine DID. Every one of them used to be recomputed at envelope
//! assembly from what the invocation ASKED FOR — the flags on the command
//! line, the workspace the operator typed it in, a second HEAD read taken
//! beside the fast-forward. Those answers agree with the machine's only
//! because of conditions held elsewhere: `--json` and `--continue` are
//! mutually exclusive at the CLI, so the invocation happens to be the op; and
//! the second HEAD read happens microseconds from the first. Neither is a
//! property of this code, and the failure mode when one lapses is an envelope
//! that reports a retire that did not happen, or an empty target, in a
//! machine-readable field a consumer cannot second-guess.
//!
//! Both pins are structural because the defect is: with the guards standing,
//! recomputation and threading produce identical bytes, so no fixture
//! distinguishes them. What a fixture would have to arrange — a resumed op
//! emitting JSON, or a repo whose HEAD moves between two adjacent reads — is
//! respectively unreachable and a race.
//!
//! Residue: these are token scans, and a token scan catches the recomputation
//! only in the spelling it left in. Reading the checkout off some other
//! context, re-resolving the target through a helper, taking the target's HEAD
//! through a binding named something else — each passes both scans.
//!
//! The three identity fields have one path where behaviour separates the two
//! derivations, and the two `resumed_sync_to_json_run_*` tests in
//! `tests/sync_to_json_test.rs` take it: a stranded op resumed through the
//! library, from each of its two sides, where what the invocation carries and
//! what the machine resolved are different values. Those survive a rename.
//! Nothing equivalent exists for `step3_advance`, whose two candidate reads
//! are separated only by a race, so the second scan below stands alone.
//! Neither scan says anything about `run_sync_json` or about any file but
//! `src/sync.rs`.

mod common;

use common::src_scan::{production_lines, SourceLine};

const OWNER: &str = "sync.rs";

/// The name of the item on a top-level `fn` line, `None` for anything else.
/// Indented lines are skipped, so a nested closure never re-attributes a site.
fn top_level_fn_name(text: &str) -> Option<&str> {
    if text.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = text
        .strip_prefix("pub(crate) ")
        .or_else(|| text.strip_prefix("pub "))
        .unwrap_or(text);
    let rest = rest.strip_prefix("async ").unwrap_or(rest);
    let rest = rest.strip_prefix("fn ")?;
    rest.split(['(', '<', ' ']).next()
}

/// The production lines of `src/sync.rs` inside top-level `fn name`.
fn body_of(lines: &[SourceLine], name: &str) -> Vec<SourceLine> {
    let mut inside = false;
    let mut out = Vec::new();
    for l in lines.iter().filter(|l| l.file == OWNER) {
        if let Some(found) = top_level_fn_name(&l.text) {
            inside = found == name;
            continue;
        }
        if inside {
            out.push(l.clone());
        }
    }
    out
}

/// Sites in `name`'s body mentioning any of `needles`.
fn sites_naming(lines: &[SourceLine], name: &str, needles: &[&str]) -> Vec<String> {
    body_of(lines, name)
        .iter()
        .filter(|l| needles.iter().any(|n| l.text.contains(n)))
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect()
}

/// The functions that may not read the invocation's own arguments: the one
/// that assembles the envelope and the wrapper that prints it. Both, because
/// assembly moving back into the wrapper is a refactor, not a change of
/// subject — a scan naming only one of them goes quiet the moment the code
/// crosses that line.
const ENVELOPE_ASSEMBLY: [&str; 2] = ["sync_to_json_run", "run_sync_to_json"];

#[test]
fn the_scan_reaches_the_functions_it_pins() {
    let lines = production_lines();
    let pinned = ENVELOPE_ASSEMBLY
        .iter()
        .copied()
        .chain(["run_advance_target"]);
    for name in pinned {
        assert!(
            lines
                .iter()
                .any(|l| top_level_fn_name(&l.text) == Some(name)),
            "src/{OWNER} no longer declares `{name}` at top level — the scan below \
             would read an empty body and pass on nothing"
        );
        assert!(
            !body_of(&lines, name).is_empty(),
            "the body scan for `{name}` came back empty, so every assertion \
             against it holds vacuously"
        );
    }
}

#[test]
fn the_envelope_reports_the_op_the_machine_ran_not_the_one_invoked() {
    let lines = production_lines();
    // The three invocation-derived inputs the envelope used to recompute from:
    // the retire flag as passed, the target as the operator spelled it, and
    // the checkout the command was typed in. On a resumed op the machine reads
    // all three from the op record instead, and they are the op's answer.
    let found: Vec<String> = ENVELOPE_ASSEMBLY
        .iter()
        .flat_map(|name| {
            sites_naming(
                &lines,
                name,
                &["request.retire", "request.source", "ctx.checkout"],
            )
        })
        .collect();
    assert!(
        found.is_empty(),
        "the `rwv sync-to --json` envelope is assembled from what the machine \
         reported — the coordinates it recorded when its context resolved, and \
         the witness retire's delete returned. Reading the invocation's own \
         arguments here answers a different question: what was asked for, not \
         what happened.\n\
         \n\
         Found: {found:#?}"
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
