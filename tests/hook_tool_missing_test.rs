//! A hook whose tool is not on `PATH` is skipped with a warning that names the
//! tool its integration declares, for every integration whose hook shells out.
//!
//! The regression it closes: a hook returned `ToolMissing` with a string it
//! typed itself, so its warning named whatever that string said, and only
//! `npm-workspaces`' hook had a test that read the warning back.
//!
//! The expectation is the table below, what each ecosystem's install tool is
//! called, not the declaration under test. Each fixture is a weave whose member
//! carries the ecosystem's manifest, activated with an empty `PATH`, so the
//! hook runs, finds nothing, and the verb must still succeed.

#[cfg(unix)]
mod common;

#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
struct Case {
    integration: &'static str,
    tool: &'static str,
    member_file: &'static str,
    member_text: &'static str,
    enabled_by_default: bool,
}

#[cfg(unix)]
const CASES: &[Case] = &[
    Case {
        integration: "npm-workspaces",
        tool: "npm",
        member_file: "package.json",
        member_text: "{\"name\": \"m\", \"version\": \"1.0.0\"}\n",
        enabled_by_default: true,
    },
    Case {
        integration: "pnpm-workspaces",
        tool: "pnpm",
        member_file: "package.json",
        member_text: "{\"name\": \"m\", \"version\": \"1.0.0\"}\n",
        enabled_by_default: false,
    },
    Case {
        integration: "uv-workspace",
        tool: "uv",
        member_file: "pyproject.toml",
        member_text: "[project]\nname = \"m\"\nversion = \"0.1.0\"\n",
        enabled_by_default: true,
    },
    Case {
        integration: "cargo-workspace",
        tool: "cargo",
        member_file: "Cargo.toml",
        member_text: "[package]\nname = \"m\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        enabled_by_default: true,
    },
];

#[cfg(unix)]
fn weave(root: &Path, case: &Case) -> std::path::PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let member = ws.join("github/org/m");
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(member.join(case.member_file), case.member_text).unwrap();
    std::fs::write(member.join("src/lib.rs"), "").unwrap();

    let enable = if case.enabled_by_default {
        String::new()
    } else {
        format!("\n[integrations.{}]\nenabled = true\n", case.integration)
    };
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/m\"]\ntype = \"git\"\nurl = \"https://github.com/org/m.git\"\nversion = \"main\"\nrole = \"owned\"\n{enable}"
        ),
    )
    .unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation authors the integration's files");
    ws
}

#[cfg(unix)]
#[test]
fn a_hook_that_cannot_find_its_tool_names_the_tool_its_integration_declares() {
    let mut failures = Vec::new();

    for case in CASES {
        let tmp = common::tempdir().unwrap();
        let ws = weave(tmp.path(), case);
        let empty_bin = tmp.path().join("empty-bin");
        std::fs::create_dir_all(&empty_bin).unwrap();

        let output = common::rwv()
            .args(["activate", "app"])
            .env("PATH", &empty_bin)
            .current_dir(&ws)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);

        let expected = format!(
            "[warning] {}: {} is not on PATH, so this integration's install hook did not run",
            case.integration, case.tool
        );
        if !output.status.success() {
            failures.push(format!(
                "{}: a missing tool is a warning, not a failure; got:\n{stderr}",
                case.integration
            ));
        } else if !stderr.contains(&expected) {
            failures.push(format!(
                "{}: expected `{expected}`; got:\n{stderr}",
                case.integration
            ));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
