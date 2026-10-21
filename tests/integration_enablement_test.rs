//! Enabling an integration in `rwv.toml` and running `rwv doctor --fix` authors
//! it.
//!
//! `--fix` regenerates only on a fixable finding, so an enabled integration
//! with nothing on disk has to report one. gita authors two whole files and
//! is the case driven here: before it reported anything, `doctor` was clean
//! over a gita that had never been written and `--fix` wrote nothing.

use std::path::{Path, PathBuf};

mod common;

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

fn doctor_fix(ws: &Path) -> String {
    let output = common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(ws)
        .output()
        .expect("rwv should run");
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn doctor_json(ws: &Path) -> serde_json::Value {
    let output = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(ws)
        .output()
        .expect("rwv should run");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "`rwv doctor --json` must emit JSON ({e}):\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn gita_issues(report: &serde_json::Value) -> Vec<serde_json::Value> {
    report["issues"]
        .as_array()
        .expect("`issues` must be an array")
        .iter()
        .filter(|issue| issue["integration"] == "gita" && issue["kind"] != "tool-missing")
        .cloned()
        .collect()
}

/// An activated weave whose one member has no ecosystem manifest, with gita
/// enabled in `rwv.toml` after the activation — so nothing has authored it.
fn weave_with_gita_newly_enabled(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    let project_dir = ws.join("projects/app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let member = ws.join("github/acme/lib");
    std::fs::create_dir_all(&member).unwrap();
    std::fs::write(member.join("README"), "lib\n").unwrap();
    git_init_with_commit(&member);

    let manifest = "[repositories.\"github/acme/lib\"]\n\
                    type = \"git\"\n\
                    url = \"https://github.com/acme/lib.git\"\n\
                    version = \"main\"\n\
                    role = \"owned\"\n";
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    git_init_with_commit(&project_dir);
    std::fs::write(ws.join(".rwv-active"), "app\n").unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        &repoweave::manifest::ProjectName::new("app").unwrap(),
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent activation should author the managed files");

    std::fs::write(
        project_dir.join("rwv.toml"),
        format!("{manifest}\n[integrations.gita]\nenabled = true\n"),
    )
    .unwrap();
    assert!(
        !project_dir.join("gita").exists(),
        "fixture: gita must not have been authored yet"
    );
    ws
}

#[test]
fn doctor_reports_a_newly_enabled_gita_missing_and_fix_authors_it() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_gita_newly_enabled(tmp.path());
    let project_dir = ws.join("projects/app");

    let report = doctor_json(&ws);
    let issues = gita_issues(&report);
    let missing: Vec<&str> = issues
        .iter()
        .filter(|i| i["kind"] == "managed-file-missing" && i["safe_to_fix"] == true)
        .filter_map(|i| i["message"].as_str())
        .collect();
    assert_eq!(
        missing.len(),
        2,
        "each unauthored CSV must be a fixable managed-file-missing finding:\n{issues:#?}"
    );
    assert!(
        missing.iter().any(|m| m.contains("repos.csv"))
            && missing.iter().any(|m| m.contains("groups.csv")),
        "each finding names its file:\n{missing:#?}"
    );

    let fixed = doctor_fix(&ws);
    for csv in ["gita/repos.csv", "gita/groups.csv"] {
        assert!(
            project_dir.join(csv).is_file(),
            "`doctor --fix` must author {csv}:\n{fixed}"
        );
        assert!(
            ws.join(csv).symlink_metadata().is_ok(),
            "`doctor --fix` must surface {csv} at the weave root"
        );
    }
    assert!(
        std::fs::read_to_string(project_dir.join("gita/groups.csv"))
            .unwrap()
            .starts_with("owned:lib:"),
        "the authored groups.csv is in gita's `name:repos:path` format"
    );

    let after = gita_issues(&doctor_json(&ws));
    assert!(
        after.is_empty(),
        "the authored CSVs clear the findings that named them:\n{after:#?}"
    );
}

/// A CSV in a format gita does not read — what an earlier rwv wrote — is
/// reported, and `--fix` rewrites it.
#[test]
fn doctor_reports_a_csv_in_another_format_as_drift_and_fix_rewrites_it() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_gita_newly_enabled(tmp.path());
    let project_dir = ws.join("projects/app");
    doctor_fix(&ws);

    let stale = "group,repos\nowned,lib\n";
    std::fs::write(project_dir.join("gita/groups.csv"), stale).unwrap();

    let issues = gita_issues(&doctor_json(&ws));
    assert!(
        issues.iter().any(|i| i["kind"] == "managed-file-drift"
            && i["safe_to_fix"] == true
            && i["message"]
                .as_str()
                .is_some_and(|m| m.contains("groups.csv"))),
        "a CSV rwv would not author is fixable drift:\n{issues:#?}"
    );

    let fixed = doctor_fix(&ws);
    assert_eq!(
        std::fs::read_to_string(project_dir.join("gita/groups.csv")).unwrap(),
        "owned:lib:\n",
        "`doctor --fix` must rewrite the CSV in gita's format:\n{fixed}"
    );
}
