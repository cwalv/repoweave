//! `rwv status --role` / `--repo`: the shared selector grammar narrows which
//! manifest repos the report lists, in the table and in the `--json` `repos`
//! array alike.

use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

const APP: &str = "github/org/app";
const LIB: &str = "github/org/lib";
const TOOL: &str = "github/vendor/tool";

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "--initial-branch=main"]);
    std::fs::write(path.join("README"), "init").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
}

fn make_workspace(parent: &Path) -> PathBuf {
    let ws = parent.join("ws");
    let project_dir = ws.join("projects").join("alpha");
    std::fs::create_dir_all(&project_dir).unwrap();
    let mut manifest = String::new();
    for (path, role) in [(APP, "owned"), (LIB, "dependency"), (TOOL, "reference")] {
        let repo = ws.join(path);
        init_repo_with_commit(&repo);
        let url = common::file_url(&repo);
        manifest.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"{role}\"\n\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    std::fs::write(ws.join(".rwv-active"), "alpha\n").unwrap();
    ws
}

fn status_json_paths(ws: &Path, selector_args: &[&str]) -> Vec<String> {
    let output = common::rwv()
        .arg("status")
        .arg("--json")
        .args(selector_args)
        .current_dir(ws)
        .output()
        .expect("rwv status --json");
    assert!(
        output.status.success(),
        "status --json {selector_args:?} failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
    let mut paths: Vec<String> = parsed["repos"]
        .as_array()
        .expect("repos array")
        .iter()
        .map(|r| r["path"].as_str().expect("path").to_owned())
        .collect();
    paths.sort();
    paths
}

#[test]
fn no_selector_reports_every_repo() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    assert_eq!(status_json_paths(&ws, &[]), [APP, LIB, TOOL]);
}

#[test]
fn bare_repo_selector_matches_one_manifest_path_exactly() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    assert_eq!(status_json_paths(&ws, &["--repo", LIB]), [LIB]);
    assert!(status_json_paths(&ws, &["--repo", "github/org"]).is_empty());
}

#[test]
fn regex_repo_selector_matches_by_pattern() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    assert_eq!(
        status_json_paths(&ws, &["--repo", "re:/(app|tool)$"]),
        [APP, TOOL]
    );
}

#[test]
fn glob_repo_selector_matches_by_pattern() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    assert_eq!(
        status_json_paths(&ws, &["--repo", "glob:github/org/*"]),
        [APP, LIB]
    );
}

#[test]
fn role_selector_matches_by_role_and_repeats_as_union() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    assert_eq!(status_json_paths(&ws, &["--role", "reference"]), [TOOL]);
    assert_eq!(
        status_json_paths(&ws, &["--role", "owned", "--role", "reference"]),
        [APP, TOOL]
    );
}

#[test]
fn role_and_repo_selectors_combine_as_union() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    assert_eq!(
        status_json_paths(&ws, &["--role", "owned", "--repo", TOOL]),
        [APP, TOOL]
    );
}

#[test]
fn text_table_lists_only_matching_repos() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let output = common::rwv()
        .args(["status", "--repo", "glob:github/vendor/*"])
        .current_dir(&ws)
        .output()
        .expect("rwv status");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<&str> = stdout.lines().collect();
    assert_eq!(rows.len(), 1, "one row expected, got:\n{stdout}");
    assert!(rows[0].starts_with(TOOL), "got:\n{stdout}");
}

#[test]
fn a_filter_matching_nothing_reports_nothing_and_succeeds() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    assert!(status_json_paths(&ws, &["--repo", "glob:nowhere/*"]).is_empty());

    let output = common::rwv()
        .args(["status", "--role", "fork"])
        .current_dir(&ws)
        .output()
        .expect("rwv status");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stdout.is_empty(),
        "no rows expected, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn an_invalid_selector_is_refused_before_any_report() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let output = common::rwv()
        .args(["status", "--json", "--repo", "re:["])
        .current_dir(&ws)
        .output()
        .expect("rwv status");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("re:["), "got: {stderr}");
}
