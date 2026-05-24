//! End-to-end tests for the `--project` flag and CWD-vs-active divergence
//! handling introduced by fo-h9prh.
//!
//! Pre fo-h9prh: `rwv lock`, `rwv add`, etc. silently substituted the
//! CWD-inferred project for `.rwv-active`, letting symlinks and manifest
//! diverge. Post fo-h9prh: `.rwv-active` is the single source of truth;
//! `--project <name>` is the one-shot escape hatch; a divergence emits a
//! helpful error or warning depending on whether the divergent project is
//! active at all.

use std::path::{Path, PathBuf};
use std::process;

mod common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git command failed");
    assert!(
        status.success(),
        "git {:?} failed in {}",
        args,
        dir.display()
    );
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init\n").unwrap();
    git(&["add", "README"], path);
    git(&["commit", "-m", "initial"], path);
    let out = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn write_manifest_only(project_dir: &Path, repos: &[(&str, &str)]) {
    std::fs::create_dir_all(project_dir).unwrap();
    let mut yaml = String::from("repositories:\n");
    if repos.is_empty() {
        yaml.push_str("  {}\n");
    }
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();
}

/// Build a workspace with two projects; no `.rwv-active` set.
///
/// Returns (workspace_root, project_a_dir, project_b_dir, server_sha).
fn make_two_project_workspace(parent: &Path) -> (PathBuf, PathBuf, PathBuf, String) {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github/acme")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    let server = ws.join("github/acme/server");
    let sha = init_repo(&server);
    let url = format!("file://{}", server.display());

    let proj_a = ws.join("projects/proj-a");
    init_repo(&proj_a);
    write_manifest_only(&proj_a, &[("github/acme/server", &url)]);
    git(&["add", "rwv.yaml"], &proj_a);
    git(&["commit", "-m", "init"], &proj_a);

    let proj_b = ws.join("projects/proj-b");
    init_repo(&proj_b);
    write_manifest_only(&proj_b, &[("github/acme/server", &url)]);
    git(&["add", "rwv.yaml"], &proj_b);
    git(&["commit", "-m", "init"], &proj_b);

    (ws, proj_a, proj_b, sha)
}

// ---------------------------------------------------------------------------
// Helpful error when CWD is in a project but `.rwv-active` is unset
// ---------------------------------------------------------------------------

#[test]
fn action_verb_in_project_dir_without_active_errors_helpfully() {
    let tmp = tempfile::tempdir().unwrap();
    let (_ws, proj_a, _proj_b, _sha) = make_two_project_workspace(tmp.path());

    let out = rwv().args(["lock"]).current_dir(&proj_a).assert().failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();

    assert!(
        stderr.contains("projects/proj-a"),
        "error should name the CWD project, got: {stderr}"
    );
    assert!(
        stderr.contains("--project proj-a"),
        "error should suggest --project, got: {stderr}"
    );
    assert!(
        stderr.contains("rwv activate proj-a"),
        "error should suggest rwv activate, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// --project lets a verb act on a non-active project without changing .rwv-active
// ---------------------------------------------------------------------------

#[test]
fn project_override_runs_without_changing_active() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _proj_a, _proj_b, _sha) = make_two_project_workspace(tmp.path());

    // Make proj-a active.
    std::fs::write(ws.join(".rwv-active"), "proj-a\n").unwrap();

    // Lock proj-b via --project, from the workspace root.
    rwv()
        .args(["lock", "--project", "proj-b"])
        .current_dir(&ws)
        .assert()
        .success();

    // .rwv-active must still be proj-a.
    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(
        active.trim(),
        "proj-a",
        "--project must not mutate .rwv-active"
    );

    // The lock should have landed in proj-b's project dir.
    assert!(
        ws.join("projects/proj-b/rwv.lock").exists(),
        "lock should be written for the --project target"
    );
    // ... and proj-a's project dir should NOT have a lock from this run.
    assert!(
        !ws.join("projects/proj-a/rwv.lock").exists(),
        "lock should not be written for the non-target project"
    );
}

// ---------------------------------------------------------------------------
// Divergence warning when CWD project != active project
// ---------------------------------------------------------------------------

#[test]
fn divergence_warning_when_cwd_project_differs_from_active() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _proj_a, proj_b, _sha) = make_two_project_workspace(tmp.path());

    // proj-a is active; CWD is proj-b — divergence.
    std::fs::write(ws.join(".rwv-active"), "proj-a\n").unwrap();

    let out = rwv().args(["lock"]).current_dir(&proj_b).assert().success();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).into_owned();

    assert!(
        stderr.contains("projects/proj-b/")
            && stderr.contains("proj-a")
            && stderr.contains("--project proj-b"),
        "warning should mention CWD project, active project, and recommend --project; got: {stderr}"
    );

    // The lock should be in proj-a (the active project), NOT proj-b — the
    // CWD does not override anymore.
    assert!(
        ws.join("projects/proj-a/rwv.lock").exists(),
        "lock should be written for the ACTIVE project"
    );
    assert!(
        !ws.join("projects/proj-b/rwv.lock").exists(),
        "lock must not be written for the CWD-project when CWD is not active"
    );
}
