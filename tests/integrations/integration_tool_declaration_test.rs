//! Each built-in integration declares the CLI it shells out to once, and the
//! declaration is what its `PATH` probe, its install hook and the
//! `tool-missing` finding and skip name.
//!
//! The table is the independent oracle: what each integration drives is a fact
//! about the ecosystem, not something to read back off the declaration under
//! test. It is held to the population in both directions, so a built-in
//! integration added without a row, or a row naming none, fails here instead
//! of being skipped.
//!
//! The behavioral half lives where the binary is driven with the tool off
//! `PATH`: `tests/integrations_test/*` raise `tool-missing` per integration
//! and clear it with the tool present, which fails the day a probe reads a
//! different spelling than the one the table names.

use repoweave::integrations::builtin_integrations;

const DRIVES: &[(&str, Option<&str>)] = &[
    ("npm-workspaces", Some("npm")),
    ("pnpm-workspaces", Some("pnpm")),
    ("go-work", Some("go")),
    ("uv-workspace", Some("uv")),
    ("cargo-workspace", Some("cargo")),
    ("gita", Some("gita")),
    ("vscode-workspace", None),
    ("static-files", None),
];

#[test]
fn every_builtin_integration_declares_the_tool_it_drives_or_none() {
    let builtin = builtin_integrations();
    let mut disagreements = Vec::new();

    for integration in &builtin {
        let declared = integration.tool().map(|t| t.name());
        match DRIVES.iter().find(|(name, _)| *name == integration.name()) {
            None => disagreements.push(format!(
                "{} has no row in the table of what each integration drives",
                integration.name()
            )),
            Some((_, expected)) if *expected != declared => disagreements.push(format!(
                "{} declares {declared:?}, but drives {expected:?}",
                integration.name()
            )),
            Some(_) => {}
        }
    }
    for (name, _) in DRIVES {
        if !builtin.iter().any(|i| i.name() == *name) {
            disagreements.push(format!("the table names `{name}`, which is not built in"));
        }
    }

    assert!(
        disagreements.is_empty(),
        "the declared tools disagree with the table:\n{}",
        disagreements.join("\n")
    );
    assert!(
        DRIVES.iter().any(|(_, tool)| tool.is_some())
            && DRIVES.iter().any(|(_, tool)| tool.is_none()),
        "the table must hold integrations that drive a tool and ones that drive none"
    );
}
