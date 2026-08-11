//! Pins `acquire_origin_dir` as the only reader of the process cwd in `src/`.
//!
//! `workspace::acquire_origin_dir`'s doc comment claims to be "the single
//! sanctioned `std::env::current_dir()` call site in the rwv CLI", and
//! ARCHITECTURE.md §3 repeats it. That is an arity claim with nothing checking
//! it: a handler that reads the process cwd on its own silently un-does `-C`
//! and `-w`, which work by injecting a different origin into the resolver.
//! Such a handler would be correct in a bare invocation and wrong under either
//! addressing flag — the shape a test catches and a reviewer does not.
//!
//! The needle is `std`'s own spelling, so unlike the `*_single_mint_test`
//! family it cannot be derived from a repo symbol. The vacuity guard below is
//! what stands in for that: if the needle stops matching, the sanctioned site
//! stops being found and the test fails rather than passing on an empty scan.
//!
//! Residue: only the `env::current_dir` spelling is matched. A caller that
//! binds `use std::env::current_dir` and calls it bare, or reaches the cwd
//! through `std::env::var("PWD")` or a `Command`'s inherited working
//! directory, is not one of the shapes here.

mod common;

use common::src_scan::{production_lines, SourceLine};

/// The sanctioned site, as `file` and the function that owns it.
const OWNER_FILE: &str = "workspace.rs";
const OWNER_FN: &str = "acquire_origin_dir";

const NEEDLE: &str = "env::current_dir(";

fn lines_reading_the_process_cwd() -> Vec<SourceLine> {
    production_lines()
        .into_iter()
        .filter(|l| l.text.contains(NEEDLE))
        .collect()
}

#[test]
fn the_sanctioned_site_is_still_found() {
    let hits = lines_reading_the_process_cwd();
    let owned: Vec<&SourceLine> = hits.iter().filter(|l| l.file == OWNER_FILE).collect();

    assert!(
        !owned.is_empty(),
        "expected `{NEEDLE}` at the sanctioned site in src/{OWNER_FILE}, found \
         none — the needle no longer matches the source, so an empty result \
         elsewhere would prove nothing. Re-derive the needle before trusting \
         the other test in this file."
    );

    let src = std::fs::read_to_string(common::src_scan::src_dir().join(OWNER_FILE))
        .expect("read workspace.rs");
    assert!(
        src.contains(&format!("fn {OWNER_FN}(")),
        "src/{OWNER_FILE} no longer defines `{OWNER_FN}`; the sanctioned \
         reader moved and this pin names the wrong site."
    );
}

#[test]
fn no_module_outside_the_sanctioned_site_reads_the_process_cwd() {
    let strays: Vec<String> = lines_reading_the_process_cwd()
        .iter()
        .filter(|l| l.file != OWNER_FILE)
        .map(|l| format!("{} {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        strays.is_empty(),
        "the process cwd is read once, by `workspace::{OWNER_FN}`, and every \
         handler receives an already-resolved context. A second reader is \
         correct only when rwv was invoked without `-C` or `-w`, and wrong \
         under either — take the resolved origin as a parameter instead. \
         Found: {strays:#?}"
    );
}
