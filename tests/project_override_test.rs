//! End-to-end tests for the `--project` flag and CWD-vs-active
//! divergence handling.
//!
//! Previously `rwv lock`, `rwv add`, etc. silently substituted the
//! CWD-inferred project for `.rwv-active`, letting symlinks and manifest
//! diverge. Now `.rwv-active` is the single source of truth;
//! `--project <name>` is the one-shot escape hatch; a divergence emits a
//! helpful error or warning depending on whether the divergent project
//! is active at all.

use std::path::{Path, PathBuf};

mod common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "--initial-branch=main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README"), "init\n").unwrap();
    common::git_in(path, &["add", "README"]);
    common::git_in(path, &["commit", "-m", "initial"]);
    let out = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn write_manifest_only(project_dir: &Path, repos: &[(&str, &str)]) {
    std::fs::create_dir_all(project_dir).unwrap();
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
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
    // Give the member a bare `origin` so `rwv update` can resolve
    // `origin/main`; without a remote the advance step has nothing to
    // resolve against.
    let bare = parent.join("server.git");
    common::git_in(
        parent,
        &[
            "clone",
            "--bare",
            server.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let url = common::file_url(&bare);
    common::git_in(&server, &["remote", "add", "origin", &url]);
    common::git_in(&server, &["fetch", "origin"]);

    let proj_a = ws.join("projects/proj-a");
    init_repo(&proj_a);
    write_manifest_only(&proj_a, &[("github/acme/server", &url)]);
    common::git_in(&proj_a, &["add", "rwv.toml"]);
    common::git_in(&proj_a, &["commit", "-m", "init"]);

    let proj_b = ws.join("projects/proj-b");
    init_repo(&proj_b);
    write_manifest_only(&proj_b, &[("github/acme/server", &url)]);
    common::git_in(&proj_b, &["add", "rwv.toml"]);
    common::git_in(&proj_b, &["commit", "-m", "init"]);

    (ws, proj_a, proj_b, sha)
}

// ---------------------------------------------------------------------------
// Helpful error when CWD is in a project but `.rwv-active` is unset
// ---------------------------------------------------------------------------

#[test]
fn action_verb_in_project_dir_without_active_errors_helpfully() {
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
// The intent verbs (add / remove / update) regenerate the --project target
// without selecting it: `.rwv-active` is untouched and the weave root keeps
// surfacing the project it already surfaced.
// ---------------------------------------------------------------------------

/// Select `proj-a` for real (symlinks + `.rwv-active`) so a later
/// `--project proj-b` run has an established selection to disturb.
///
/// `activate` is a context verb and authors nothing, so the repair verb runs
/// after it to write the `.code-workspace` this fixture then requires to be
/// surfaced. Without that step the weave root holds no link for it at all:
/// a managed file written at its source is not surfaced until it exists, so
/// what this helper asserts would otherwise be a link resolving to nothing —
/// which is not a selection anything downstream could disturb.
fn select_proj_a(ws: &Path) {
    rwv()
        .args(["activate", "proj-a"])
        .current_dir(ws)
        .assert()
        .success();
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(ws)
        .output()
        .unwrap();
    assert!(
        ws.join("projects/proj-a/proj-a.code-workspace").is_file(),
        "fixture: proj-a's managed file must exist before it can be surfaced"
    );
    assert_eq!(
        std::fs::read_link(ws.join("proj-a.code-workspace")).unwrap(),
        Path::new("projects/proj-a/proj-a.code-workspace"),
        "activate should surface proj-a at the weave root"
    );
}

/// Assert that a `--project proj-b` intent verb regenerated proj-b's content
/// in proj-b's own directory and left the weave root selecting proj-a.
fn assert_regenerated_without_selecting(ws: &Path) {
    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(
        active.trim(),
        "proj-a",
        "--project must not mutate .rwv-active"
    );
    assert!(
        ws.join("projects/proj-b/proj-b.code-workspace").exists(),
        "the target project's content must be regenerated in its own directory"
    );
    assert!(
        !ws.join("proj-b.code-workspace").exists(),
        "the non-selected target must not be surfaced at the weave root"
    );
    assert_eq!(
        std::fs::read_link(ws.join("proj-a.code-workspace")).unwrap(),
        Path::new("projects/proj-a/proj-a.code-workspace"),
        "the selected project's surfacing must survive a --project run"
    );
}

#[test]
fn add_with_project_override_regenerates_without_selecting() {
    let tmp = common::tempdir().unwrap();
    let (ws, _proj_a, _proj_b, _sha) = make_two_project_workspace(tmp.path());
    select_proj_a(&ws);

    // A second on-disk repo, added by local path so no clone is needed.
    // Local-path add reads the repo's `origin` to record a URL — the
    // origin is unreachable on purpose, so origin/HEAD is planted by hand
    // rather than fetched, the same local-only state `rwv add` reads.
    let client = ws.join("github/acme/client");
    init_repo(&client);
    common::git_in(
        &client,
        &["remote", "add", "origin", "file:///nowhere/client.git"],
    );
    common::git_in(
        &client,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );

    rwv()
        .args(["add", "github/acme/client", "--project", "proj-b"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        std::fs::read_to_string(ws.join("projects/proj-b/rwv.toml"))
            .unwrap()
            .contains("github/acme/client"),
        "add should have landed in the --project target's manifest"
    );
    assert_regenerated_without_selecting(&ws);
}

#[test]
fn remove_with_project_override_regenerates_without_selecting() {
    let tmp = common::tempdir().unwrap();
    let (ws, _proj_a, _proj_b, _sha) = make_two_project_workspace(tmp.path());
    select_proj_a(&ws);

    rwv()
        .args(["remove", "github/acme/server", "--project", "proj-b"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        !std::fs::read_to_string(ws.join("projects/proj-b/rwv.toml"))
            .unwrap()
            .contains("github/acme/server"),
        "remove should have landed in the --project target's manifest"
    );
    assert!(
        std::fs::read_to_string(ws.join("projects/proj-a/rwv.toml"))
            .unwrap()
            .contains("github/acme/server"),
        "remove must not touch the active project's manifest"
    );
    assert_regenerated_without_selecting(&ws);
}

#[test]
fn update_with_project_override_regenerates_without_selecting() {
    let tmp = common::tempdir().unwrap();
    let (ws, _proj_a, _proj_b, sha) = make_two_project_workspace(tmp.path());
    select_proj_a(&ws);

    rwv()
        .args(["update", "--project", "proj-b"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        std::fs::read_to_string(ws.join("projects/proj-b/rwv.lock"))
            .unwrap()
            .contains(&sha),
        "update should have re-snapshotted the --project target's lock"
    );
    assert!(
        !ws.join("projects/proj-a/rwv.lock").exists(),
        "update must not write a lock for the active project"
    );
    assert_regenerated_without_selecting(&ws);
}

// ---------------------------------------------------------------------------
// Divergence warning when CWD project != active project
// ---------------------------------------------------------------------------

#[test]
fn divergence_warning_when_cwd_project_differs_from_active() {
    let tmp = common::tempdir().unwrap();
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
