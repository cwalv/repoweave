//! A `.rwv-active` that holds no project name selects nothing, so a verb that
//! takes its project from the pointer refuses on it instead of acting as if no
//! project were selected. `rwv doctor` reports it and `--fix` removes it;
//! `rwv activate` replaces it; a verb that names its project, or selects none,
//! never reads it.

use std::path::{Path, PathBuf};

mod common;

const TOKEN: &str = "unreadable-active-project";

/// A primary with one project, `web-app`, and `.rwv-active` holding `pointer`.
fn weave(parent: &Path, pointer: &str) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    let project = ws.join("projects").join("web-app");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("rwv.toml"), "[repositories]\n").unwrap();
    std::fs::write(ws.join(".rwv-active"), pointer).unwrap();
    ws
}

fn run(ws: &Path, args: &[&str]) -> std::process::Output {
    common::rwv_std()
        .args(args)
        .current_dir(ws)
        .output()
        .unwrap()
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, vec![token], "stderr was:\n{stderr}");
}

fn assert_refused_naming_the_repairs(ws: &Path, args: &[&str]) {
    let out = run(ws, args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{args:?} must refuse: {stderr}");
    assert_routes_to(&stderr, "unreadable-active-project");
    for needle in [".rwv-active", "rwv activate <project>", "rwv doctor --fix"] {
        assert!(
            stderr.contains(needle),
            "{args:?}: missing `{needle}`: {stderr}"
        );
    }
}

#[test]
fn a_verb_reading_the_pointer_refuses_one_that_holds_no_project_name() {
    for pointer in ["two words\n", "", "  \n", "-leading-dash\n"] {
        let tmp = common::tempdir().unwrap();
        let ws = weave(tmp.path(), pointer);
        assert_refused_naming_the_repairs(&ws, &["lock"]);
        assert_refused_naming_the_repairs(&ws, &["status"]);
    }
}

#[test]
fn a_verb_given_its_project_does_not_read_the_pointer() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path(), "two words\n");
    let out = run(&ws, &["status", "--project", "web-app"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn verbs_that_select_no_project_do_not_read_the_pointer() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path(), "two words\n");
    let out = run(&ws, &["resolve"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn doctor_reports_it_under_the_same_token() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path(), "two words\n");

    let text = run(&ws, &["doctor"]);
    assert!(!text.status.success());
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(
        stdout.contains("names no project rwv can select") && stdout.contains("rwv activate"),
        "{stdout}"
    );

    let json = run(&ws, &["doctor", "--json"]);
    let doc: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let finding = doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == TOKEN)
        .unwrap_or_else(|| panic!("no {TOKEN} finding: {doc:#}"));
    assert!(
        finding["path"].as_str().unwrap().ends_with(".rwv-active"),
        "{finding}"
    );
    assert!(
        finding["detail"].as_str().unwrap().contains("two words"),
        "{finding}"
    );
}

#[test]
fn doctor_fix_removes_it() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path(), "two words\n");
    let out = run(&ws, &["doctor", "--fix"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("it named no project rwv can select"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(!ws.join(".rwv-active").exists());
    let status = run(&ws, &["status", "--project", "web-app"]);
    assert!(status.status.success());
}

#[test]
fn activate_replaces_it() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path(), "two words\n");
    let out = run(&ws, &["activate", "web-app", "--no-materialize"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(ws.join(".rwv-active")).unwrap(),
        "web-app\n"
    );
}
