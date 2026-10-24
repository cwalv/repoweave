//! A member the manifest names is found on disk wherever it sits, whether or
//! not its top-level directory is a registry segment and whatever its depth.

use std::path::{Path, PathBuf};

mod common;

fn weave(tmp: &tempfile::TempDir) -> PathBuf {
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    common::rwv()
        .args(["init", "demo"])
        .current_dir(&ws)
        .assert()
        .success();
    ws
}

fn clone_into(tmp: &tempfile::TempDir, dest: &Path) {
    let origin = tmp.path().join("origins").join("other.git");
    std::fs::create_dir_all(origin.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&origin);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    common::git_in(
        dest.parent().unwrap(),
        &["clone", origin.to_str().unwrap(), dest.to_str().unwrap()],
    );
}

fn adopt(ws: &Path, member: &str) {
    common::rwv()
        .args(["add", "--adopt", member])
        .current_dir(ws)
        .assert()
        .success();
}

/// `(kind, path)` for every violation `rwv doctor --all --json` reports.
fn doctor_findings(ws: &Path) -> Vec<(String, String)> {
    let out = common::rwv()
        .args(["doctor", "--all", "--json"])
        .current_dir(ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{stdout}"));
    report["violations"]
        .as_array()
        .expect("the report carries a violations array")
        .iter()
        .map(|v| {
            (
                v["kind"].as_str().unwrap_or_default().to_owned(),
                v["path"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// The URL places this member under `local/`, so the one finding an adopt
/// at `vendor/` leaves is the report-only placement disagreement.
fn assert_only_placement_disagreement(ws: &Path, member: &str) {
    let findings = doctor_findings(ws);
    assert_eq!(
        findings,
        [("provenance".to_owned(), member.to_owned())],
        "an adopted clone at a non-registry path is a member on disk"
    );
    common::rwv()
        .args(["doctor", "--all"])
        .current_dir(ws)
        .assert()
        .success();
}

#[test]
fn a_clone_adopted_under_a_non_registry_directory_is_not_dangling() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(&tmp);
    clone_into(&tmp, &ws.join("vendor/other"));
    adopt(&ws, "vendor/other");

    assert_only_placement_disagreement(&ws, "vendor/other");
}

#[test]
fn a_clone_adopted_three_deep_under_a_non_registry_directory_is_not_dangling() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(&tmp);
    clone_into(&tmp, &ws.join("vendor/a/b/other"));
    adopt(&ws, "vendor/a/b/other");

    assert_only_placement_disagreement(&ws, "vendor/a/b/other");
}

#[test]
fn a_missing_member_under_a_non_registry_directory_is_dangling_until_fetch_restores_it() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(&tmp);
    let member = ws.join("vendor/other");
    clone_into(&tmp, &member);
    adopt(&ws, "vendor/other");
    std::fs::remove_dir_all(&member).unwrap();

    let findings = doctor_findings(&ws);
    assert!(
        findings.contains(&("dangling-reference".to_owned(), "vendor/other".to_owned())),
        "a named member absent from disk is dangling: {findings:?}"
    );

    common::rwv()
        .arg("fetch")
        .current_dir(&ws)
        .assert()
        .success();
    assert!(member.join(".git").exists(), "fetch re-clones the member");
    assert_only_placement_disagreement(&ws, "vendor/other");
}
