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
