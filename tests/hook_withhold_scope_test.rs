//! Pins the UNIT of the drift withholding: the workspace, not the integration
//! whose file drifted.
//!
//! One drifted `Cargo.lock` withholds `npm install` too. That is a chosen cost
//! rather than an oversight, and the asymmetry is exactly what a later reader
//! tidies: moving the question inside the per-integration loop looks like
//! precision, and what it buys is a workspace where some ecosystems regenerated
//! against a new membership and others did not.
//!
//! Membership is why the coarse unit is the right one. The verbs that reach
//! this gate are the ones that CHANGE membership, and every integration derives
//! its files from that same membership — so scoping the withholding to the
//! integration whose file drifted does not confine the damage, it distributes
//! it. Nothing afterwards can name the result: each ecosystem is internally
//! consistent, so every per-integration check passes and `rwv doctor` reports a
//! healthy workspace that no single member set explains. Withholding everything
//! keeps one fact true of the whole workspace instead — nothing has regenerated
//! since the drift arrived — and that is a state a verb can both describe and
//! repair.
//!
//! The claim is an arity, so the pin is one: the question is asked once, and
//! the hooks are run once. Narrowing the scope means calling the predicate from
//! inside the runner, which moves or multiplies one of the two sites and fails
//! here. That the ask guards the run is the types' job: `run_activate_hooks`
//! takes a `HooksPermit`, and the gate that asks is the permit's one mint,
//! which the last test here holds to one construction site.
//!
//! Residue: this reads the call graph's shape, not its behavior. A gate that
//! kept both arities and computed the wrong answer is not a shape this sees —
//! `tests/arrived_drift_consent_scope_test.rs` is where the answer is checked.
//!
//! A second, by-construction reason per-integration scoping cannot arrive
//! silently: the gate's own input carries no integration to scope by.
//! `drifted_attested_owned_files` returns `DriftedOwnedFile { name, content }`
//! — a filename and bytes, nothing recording which integration produced
//! either. Narrowing the withhold to the drifted file's own integration is not
//! a rewrite of the `if` this file pins; it needs a new column on the
//! owned-digest ledger first, because nothing on this path has one to read.

mod common;

use common::src_scan::{body_of, production_lines, SourceLine};

/// The file that must own both sites, and the predicate that must guard the
/// run.
const OWNER_FILE: &str = "activate.rs";
const GATE_FN: &str = "withhold_hooks_over_unsettled_drift";

/// The permit's mint, and the literal only it may write.
const MINT_FN: &str = "permit_hooks";
const PERMIT_LITERAL: &str = "HooksPermit(())";

/// The call form, not the declaration: `run_activate_hooks` is declared in
/// `integration_runner.rs` and that line must not count as a use. Matched
/// without the arguments, so a caller holding its inputs by reference or by
/// value is counted the same.
const HOOK_CALL: &str = "run_activate_hooks(";

fn production_lines_containing(needle: &str) -> Vec<SourceLine> {
    production_lines()
        .into_iter()
        .filter(|l| l.text.contains(needle))
        .collect()
}

fn hook_calls() -> Vec<SourceLine> {
    production_lines_containing(HOOK_CALL)
        .into_iter()
        .filter(|l| !l.text.contains(&format!("fn {HOOK_CALL}")))
        .collect()
}

/// Vacuity guard. Both needles are repo symbols, so a rename makes them stop
/// matching and every emptiness below would then be evidence of nothing.
#[test]
fn both_needles_still_match_the_source() {
    let hooks = hook_calls();
    assert!(
        !hooks.is_empty(),
        "`{HOOK_CALL}` matches no production line — the hook runner was \
         renamed or its call form changed, so the arity tests in this file \
         would pass on an empty scan. Re-derive the needle."
    );
    let gate = production_lines_containing(GATE_FN);
    assert!(
        gate.len() >= 2,
        "expected `{GATE_FN}` at a definition and a call, found {} line(s). \
         The gate was renamed or removed; this file pins nothing until the \
         needle is re-derived: {gate:#?}",
        gate.len()
    );
}

/// The hooks are run from one place, so there is one place for the question to
/// be asked.
#[test]
fn the_hooks_are_run_from_exactly_one_site() {
    let sites: Vec<String> = hook_calls()
        .iter()
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert_eq!(
        sites.len(),
        1,
        "the install hooks must run from exactly one site, because that site \
         is where the drift question is asked. A second caller is a second \
         answer, and the two can differ. Found: {sites:#?}"
    );
    assert!(
        sites[0].starts_with(OWNER_FILE),
        "and that site belongs in src/{OWNER_FILE}, beside the mode dispatch \
         it serves. Found: {sites:#?}"
    );
}

/// The question is asked once, inside the permit's mint — not per integration
/// inside the runner.
#[test]
fn the_drift_question_is_asked_once_by_the_permit_mint() {
    let calls: Vec<SourceLine> = production_lines_containing(GATE_FN)
        .into_iter()
        .filter(|l| !l.text.contains(&format!("fn {GATE_FN}")))
        .collect();

    let sites: Vec<String> = calls
        .iter()
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "the drift question must be asked once per run. More than one asker \
         is per-something scoping arriving by accident — decide it and say so \
         rather than letting the call graph decide. Found: {sites:#?}"
    );

    let mint = body_of(&production_lines(), OWNER_FILE, MINT_FN);
    assert!(
        mint.iter()
            .any(|l| l.file == calls[0].file && l.line == calls[0].line),
        "the question must be asked by `{MINT_FN}`, so its answer is what decides \
         whether a permit exists. Found it at {sites:#?}"
    );
}

/// The permit is written in one place, the mint that asks the gate's
/// questions; a second construction is a run nobody gated.
#[test]
fn the_permit_is_constructed_only_by_its_mint() {
    let lines = production_lines();
    let constructions: Vec<&SourceLine> = lines
        .iter()
        .filter(|l| l.text.contains(PERMIT_LITERAL) && !l.text.contains("struct HooksPermit"))
        .collect();
    let mint = body_of(&lines, OWNER_FILE, MINT_FN);
    assert!(
        !mint.is_empty(),
        "`{MINT_FN}` has no body in src/{OWNER_FILE}; the mint was renamed or moved"
    );
    let sites: Vec<String> = constructions
        .iter()
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();
    assert_eq!(sites.len(), 1, "one construction of the permit: {sites:#?}");
    assert!(
        mint.iter()
            .any(|l| l.file == constructions[0].file && l.line == constructions[0].line),
        "the permit must be constructed inside `{MINT_FN}`: {sites:#?}"
    );
}
