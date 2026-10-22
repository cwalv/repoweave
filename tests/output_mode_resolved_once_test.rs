//! The output mode and the fetch's member selection are each resolved in one
//! place and passed down.
//!
//! Three prohibitions over `src/`, which no behavior distinguishes while the
//! copies agree, and which is why they are read as source:
//!
//! 1. No function outside the clap definitions takes `json: bool` or
//!    `ndjson: bool`. Each re-derived the shape of the output from the flag,
//!    and 5c8ad7e is the one that drifted.
//! 2. `OutputMode::resolve` is called by the dispatcher alone.
//! 3. The `reference` role is tested in one place in `fetch.rs`,
//!    `MemberSelection::leaves_out`; the test used to be re-derived at four
//!    sites below dispatch.
//!
//! **Scope**, which is also the coverage boundary: production lines of `src/`
//! (`common::src_scan` drops comment lines and `#[cfg(test)]` items). Prohibition
//! 1 reads parameters spelled `json: bool` or `ndjson: bool`, so a flag renamed
//! or carried in a struct field is invisible to it, and `src/cli.rs`, whose
//! clap arguments are those flags, and `src/bin/`, whose plugin is not this
//! library, are not read. Prohibition 3 reads `Role::Reference` in `fetch.rs`
//! only.

mod common;

use common::src_scan::{production_lines, SourceLine};

/// The resolver is the one function that takes the flag: it is what turns it
/// into the mode.
const THE_RESOLVER: &str = "pub fn resolve(json: bool, jobs: usize) -> Self {";

fn takes_the_flag(text: &str) -> Option<&'static str> {
    if text.contains(THE_RESOLVER) {
        return None;
    }
    ["ndjson: bool", "json: bool"]
        .into_iter()
        .find(|needle| text.contains(needle))
}

fn calls_resolve(text: &str) -> bool {
    text.contains("OutputMode::resolve(")
}

fn tests_the_reference_role(text: &str) -> bool {
    text.contains("Role::Reference")
}

fn library(line: &&SourceLine) -> bool {
    line.file != "cli.rs" && !line.file.starts_with("bin/")
}

#[test]
fn no_function_takes_the_json_flag() {
    let lines = production_lines();
    let read: Vec<&SourceLine> = lines.iter().filter(library).collect();
    assert!(
        read.len() > 20_000,
        "the scan must reach the library, saw {} lines",
        read.len()
    );

    let strays: Vec<String> = read
        .iter()
        .filter_map(|l| {
            takes_the_flag(&l.text)
                .map(|needle| format!("{}: `{needle}` in {}", l.site(), l.text.trim()))
        })
        .collect();
    assert!(
        strays.is_empty(),
        "a verb takes the `OutputMode` the dispatcher resolved, or a `JsonShape` when it has no \
         text mode; it never takes the flag:\n{}",
        strays.join("\n")
    );
}

#[test]
fn only_the_dispatcher_resolves_the_output_mode() {
    let lines = production_lines();
    let callers: Vec<&SourceLine> = lines.iter().filter(|l| calls_resolve(&l.text)).collect();
    assert!(
        callers.iter().any(|l| l.file == "cli/dispatch.rs"),
        "the dispatcher must be where the mode is resolved; found {:?}",
        callers.iter().map(|l| l.site()).collect::<Vec<_>>()
    );

    let strays: Vec<String> = callers
        .iter()
        .filter(|l| l.file != "cli/dispatch.rs")
        .map(|l| format!("{}: {}", l.site(), l.text.trim()))
        .collect();
    assert!(
        strays.is_empty(),
        "the mode is resolved once, in `cli/dispatch.rs`, and passed down:\n{}",
        strays.join("\n")
    );
}

#[test]
fn the_reference_role_is_tested_once_in_fetch() {
    let lines = production_lines();
    let tests: Vec<String> = lines
        .iter()
        .filter(|l| l.file == "fetch.rs" && tests_the_reference_role(&l.text))
        .map(|l| format!("{}: {}", l.site(), l.text.trim()))
        .collect();
    assert_eq!(
        tests.len(),
        1,
        "`MemberSelection::leaves_out` is the one test of the reference role, found:\n{}",
        tests.join("\n")
    );
}

#[test]
fn the_scans_flag_each_shape_they_claim_to_read() {
    assert_eq!(
        takes_the_flag("pub fn run_status(ctx: &WorkspaceContext, json: bool) {"),
        Some("json: bool")
    );
    assert_eq!(takes_the_flag("    ndjson: bool,"), Some("ndjson: bool"));
    assert_eq!(takes_the_flag("    output: OutputMode,"), None);
    assert_eq!(takes_the_flag(&format!("    {THE_RESOLVER}")), None);
    assert!(calls_resolve(
        "let o = crate::parallel::OutputMode::resolve(json, jobs);"
    ));
    assert!(!calls_resolve(
        "pub fn resolve(json: bool, jobs: usize) -> Self {"
    ));
    assert!(tests_the_reference_role(
        "self.skip_reference && role == Role::Reference"
    ));
}
