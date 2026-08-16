//! The deactivation hook and the trigger that reaches it.
//!
//! `docs/reference/integrations/index.md` publishes when the hook runs. That
//! claim decayed once already — every integration implemented `deactivate()`
//! with tested strip logic and nothing in production called any of them — so
//! what is pinned here is the **call graph**, not the strip bodies. Those have
//! their own unit tests next to each integration.
//!
//! # Why the workweave-deletion pin observes a failure
//!
//! Deletion removes the whole workweave directory (`remove_dir_all` at the end
//! of the delete path), and every file the hook can touch is inside it. So on
//! the success path the strip has, by construction, no observable effect —
//! there is no surviving byte a test could assert on. What IS observable is the
//! hook's report: an integration whose strip fails produces an issue the delete
//! prints. Poisoning one managed file therefore turns "was the hook reached"
//! into a question the operator's own output answers, and removing the call
//! from the delete path makes the assertion fail.
//!
//! That is the honest limit of this file: it pins that deletion **reaches** the
//! hook, and separately that the hook reaches every enabled integration. It
//! does not — and cannot — pin an effect of a successful strip during deletion,
//! because there is none.

use std::path::{Path, PathBuf};

mod common;

use repoweave::manifest::Manifest;

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// A primary weave with one Rust member and an authored, committed project.
fn weave(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("projects/app")).unwrap();

    let member = ws.join("github/acme/lib");
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(member.join("src/lib.rs"), "").unwrap();
    git_init_with_commit(&member);

    std::fs::write(
        ws.join("projects/app/rwv.toml"),
        "[repositories.\"github/acme/lib\"]\ntype = \"git\"\nurl = \"https://github.com/acme/lib.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    git_init_with_commit(&ws.join("projects/app"));
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "app",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation should author the managed files");
    common::git_in(ws.join("projects/app"), &["add", "-A"]);
    common::git_in(ws.join("projects/app"), &["commit", "-m", "authored"]);
    ws
}

/// Published trigger: deleting a workweave runs the deactivation hook in that
/// checkout.
///
/// Read the module docs for why this asserts on the hook's failure report
/// rather than on a stripped file.
#[test]
fn deleting_a_workweave_reaches_the_deactivation_hook() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());

    let out = common::rwv()
        .args(["workweave", "app", "create", "agent-1"])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    assert!(
        out.status.success(),
        "fixture: workweave create failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ww = tmp.path().join(".workweaves/app--agent-1");
    let managed = ww.join("projects/app/Cargo.toml");
    assert!(
        managed.is_file(),
        "fixture: the workweave should carry the managed file"
    );

    // Poison the managed file: a directory where the hook expects a file makes
    // the strip fail, and a failing strip is the one thing a deletion reports.
    // Nothing else in the delete path reads this file, so a report naming
    // cargo-workspace can only have come from the hook.
    std::fs::remove_file(&managed).unwrap();
    std::fs::create_dir(&managed).unwrap();

    let out = common::rwv()
        .args([
            "workweave",
            "app",
            "delete",
            "agent-1",
            "--discard-uncommitted",
        ])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    let report = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        report.contains("cargo-workspace") && report.contains("deactivation failed"),
        "deleting a workweave must run the deactivation hook in that checkout; \
         nothing in the delete output shows it ran:\n{report}"
    );
    assert!(
        !ww.exists(),
        "the deletion should still have completed:\n{report}"
    );
}

/// The hook reaches every enabled integration, and stops at rwv's own marker.
///
/// This is the half that CAN be observed on the success path, because the
/// directory it strips is not being deleted.
#[test]
fn the_hook_strips_every_enabled_integration_and_keeps_operator_content() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let project_dir = ws.join("projects/app");

    // Operator content in the two hybrid files rwv shares with a user.
    let cargo_toml = project_dir.join("Cargo.toml");
    let authored = std::fs::read_to_string(&cargo_toml).unwrap();
    std::fs::write(
        &cargo_toml,
        format!("{authored}\n[profile.release]\nlto = true\n"),
    )
    .unwrap();

    let code_workspace = project_dir.join("app.code-workspace");
    let ws_json = std::fs::read_to_string(&code_workspace).unwrap();
    std::fs::write(
        &code_workspace,
        ws_json.replace(
            "\"settings\": {",
            "\"extensions\": { \"recommendations\": [\"rust-lang.rust-analyzer\"] },\n  \"settings\": {",
        ),
    )
    .unwrap();

    assert!(
        std::fs::read_to_string(&cargo_toml)
            .unwrap()
            .contains("managed by rwv"),
        "precondition: rwv's marker is on the file before the strip"
    );

    let manifest = Manifest::from_path(&project_dir.join("rwv.toml")).unwrap();
    let issues = repoweave::activate::strip_project_regions(&project_dir, &manifest);
    assert!(
        issues.is_empty(),
        "the strip should not report on a healthy project: {:?}",
        issues.iter().map(|i| &i.message).collect::<Vec<_>>()
    );

    let stripped = std::fs::read_to_string(&cargo_toml).unwrap();
    assert!(
        !stripped.contains("managed by rwv") && !stripped.contains("members"),
        "cargo-workspace's region should be gone:\n{stripped}"
    );
    assert!(
        stripped.contains("[profile.release]") && stripped.contains("lto = true"),
        "the operator's profile must survive the strip:\n{stripped}"
    );

    let stripped_ws = std::fs::read_to_string(&code_workspace).unwrap();
    assert!(
        !stripped_ws.contains("rwv.generated"),
        "vscode-workspace's region should be gone:\n{stripped_ws}"
    );
    assert!(
        stripped_ws.contains("rust-lang.rust-analyzer"),
        "the operator's extension list must survive the strip:\n{stripped_ws}"
    );
}

/// Selection is not deactivation. A project that is merely no longer the
/// selected one keeps the ecosystem files committed in its own repo; only its
/// weave-root surfacing goes.
///
/// This is a prohibition, and the thing it prohibits is not hypothetical: the
/// hook strips a project directory's committed content, so aiming it at a
/// project switch converts the outgoing project's managed file into a
/// hand-held one and leaves its repo dirty.
#[test]
fn switching_projects_leaves_the_outgoing_project_intact() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());

    // A second project, sharing the member so both have Rust work to manage.
    std::fs::create_dir_all(ws.join("projects/other")).unwrap();
    std::fs::write(
        ws.join("projects/other/rwv.toml"),
        "[repositories.\"github/acme/lib\"]\ntype = \"git\"\nurl = \"https://github.com/acme/lib.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    git_init_with_commit(&ws.join("projects/other"));

    let outgoing = ws.join("projects/app");
    let before = std::fs::read_to_string(outgoing.join("Cargo.toml")).unwrap();

    let out = common::rwv()
        .args(["activate", "other", "--no-materialize"])
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    let report = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "activate should succeed:\n{report}");

    assert_eq!(
        std::fs::read_to_string(outgoing.join("Cargo.toml")).unwrap(),
        before,
        "activating another project must not rewrite the outgoing project's \
         committed manifest:\n{report}"
    );
    assert!(
        outgoing.join("app.code-workspace").is_file(),
        "activating another project must not delete the outgoing project's \
         committed workspace file:\n{report}"
    );

    // The repo it belongs to is the surface an operator would notice this on.
    let status = common::git()
        .args(["status", "--porcelain"])
        .current_dir(&outgoing)
        .output()
        .expect("git should run");
    assert!(
        String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "a project switch left the outgoing project's repo dirty:\n{}\n{report}",
        String::from_utf8_lossy(&status.stdout)
    );

    // What DOES go is the weave-root surfacing, which is the framework's job.
    assert!(
        !ws.join("app.code-workspace").exists(),
        "the outgoing project's weave-root surfacing should not survive the switch"
    );
}
