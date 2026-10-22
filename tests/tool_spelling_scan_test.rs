//! No integration spells a tool as a string literal at the place it looks the
//! tool up, spawns it, or reports it missing: each reads the one declaration
//! (`DrivesTool::TOOL`).
//!
//! The regression it closes: `which::which("npm")` in `check()`, `node_tool("npm")`
//! in the hook and `HookOutcome::ToolMissing("npm")` beside it spelled one tool
//! three times in one file, and nothing tied the copies together, so a rename
//! made in one left the others probing for a program that no longer ran.
//!
//! **Structural**, because the property is a prohibition over the call sites in
//! one directory. **Scope:** production lines under `src/integrations/`
//! (`common::src_scan` drops comment lines and `#[cfg(test)]` items), matched
//! on the four call shapes below with a string literal as the first argument. A
//! lookup spelled through a binding, a `format!`, or a helper outside that
//! directory is invisible to it, which is the intent: those read something
//! other than a literal. The second test requires the scan to see the
//! declarations it lets stand.
//!
//! A second pair of tests holds the other half: no lookup or spawn of a tool,
//! literal or not, is made outside `ToolProgram`. Their scope is production
//! lines of `src/integrations/` and `src/plugins.rs`; version control spawns
//! through its own seam and is not read.

mod common;

use common::src_scan::{production_lines, SourceLine};

const LITERAL_CALLS: &[&str] = &[
    "which::which(\"",
    "run_install_tool(\"",
    "node_tool(\"",
    "Command::new(\"",
    "ToolMissing(\"",
];

fn spells_a_tool(text: &str) -> Option<&'static str> {
    LITERAL_CALLS
        .iter()
        .copied()
        .find(|call| text.contains(call))
}

fn in_integrations(line: &SourceLine) -> bool {
    line.file.starts_with("integrations/")
}

#[test]
fn no_integration_spells_a_tool_where_it_probes_spawns_or_reports_it() {
    let lines = production_lines();
    let scanned: Vec<&SourceLine> = lines.iter().filter(|l| in_integrations(l)).collect();
    assert!(
        scanned.len() > 1000,
        "the scan must reach the integrations, saw {} lines",
        scanned.len()
    );

    let spelled: Vec<String> = scanned
        .iter()
        .filter_map(|l| {
            spells_a_tool(&l.text)
                .map(|call| format!("{}: `{call}…` in {}", l.site(), l.text.trim()))
        })
        .collect();
    assert!(
        spelled.is_empty(),
        "a tool is spelled once, in its `DrivesTool::TOOL`; read that instead:\n{}",
        spelled.join("\n")
    );
}

#[test]
fn the_scan_sees_the_declarations_it_exempts() {
    let lines = production_lines();
    let declarations = lines
        .iter()
        .filter(|l| in_integrations(l) && l.text.contains("const TOOL: Tool = Tool::new(\""))
        .count();
    assert!(
        declarations >= 2,
        "the scan must see the declarations the other test lets stand, saw {declarations}"
    );
}

#[test]
fn the_scan_flags_each_call_shape_it_claims_to_read() {
    for (call, line) in [
        (
            "which::which(\"",
            "        if which::which(\"npm\").is_err() {",
        ),
        (
            "run_install_tool(\"",
            "super::run_install_tool(\"uv\", &[\"sync\"], root)?",
        ),
        ("node_tool(\"", "super::node_tool(\"pnpm\")"),
        ("Command::new(\"", "let status = Command::new(\"go\")"),
        (
            "ToolMissing(\"",
            "return Ok(HookOutcome::ToolMissing(\"cargo\"));",
        ),
    ] {
        assert_eq!(spells_a_tool(line), Some(call), "seeded line: {line}");
    }
    assert_eq!(
        spells_a_tool("which::which(I::TOOL.name()).is_ok()"),
        None,
        "a lookup through the declaration is the intended spelling"
    );
}

const LOOKUPS_AND_SPAWNS: &[&str] = &[
    "which::which(",
    "which::which_in(",
    "Command::new(",
    "node_tool",
];

fn looks_up_or_spawns(text: &str) -> Option<&'static str> {
    LOOKUPS_AND_SPAWNS
        .iter()
        .copied()
        .find(|needle| text.contains(needle))
}

#[test]
fn the_second_scan_flags_each_lookup_and_spawn_it_claims_to_read() {
    for (needle, line) in [
        ("which::which(", "let found = which::which(name).ok()?;"),
        ("which::which_in(", "which::which_in(name, Some(path), cwd)"),
        ("Command::new(", "let mut cmd = Command::new(binary);"),
        ("node_tool", "super::node_tool(Self::TOOL.name())"),
    ] {
        assert_eq!(
            looks_up_or_spawns(line),
            Some(needle),
            "seeded line: {line}"
        );
    }
    assert_eq!(looks_up_or_spawns("program.command().args(args)"), None);
}

/// Production lines where the library looks a tool up or spawns one: the
/// integrations and plugin dispatch. Version control has its own seam.
fn tool_users(line: &SourceLine) -> bool {
    line.file.starts_with("integrations/") || line.file == "plugins.rs"
}

#[test]
fn every_lookup_and_spawn_of_a_tool_goes_through_tool_program() {
    let lines = production_lines();
    let scanned: Vec<&SourceLine> = lines.iter().filter(|l| tool_users(l)).collect();
    assert!(
        scanned.len() > 1000 && scanned.iter().any(|l| l.file == "plugins.rs"),
        "the scan must reach the integrations and plugin dispatch, saw {} lines",
        scanned.len()
    );

    let strays: Vec<String> = scanned
        .iter()
        .filter_map(|l| {
            looks_up_or_spawns(&l.text)
                .map(|needle| format!("{}: `{needle}` in {}", l.site(), l.text.trim()))
        })
        .collect();
    assert!(
        strays.is_empty(),
        "a tool is looked up and spawned through `ToolProgram`, one lookup whose answer is \
         the spawn target:\n{}",
        strays.join("\n")
    );
}
