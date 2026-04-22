//! Integration tests verifying doc claims about `rwv` context display.
//!
//! Doc IDs referenced:
//!   - project-reporoot-0ptp: rwv display context

use assert_cmd::Command;
use std::path::Path;
use std::process;

// ---------------------------------------------------------------------------
// Helpers (mirrored from workweave_test.rs)
// ---------------------------------------------------------------------------

fn rwv() -> Command {
    Command::cargo_bin("rwv").expect("rwv binary should be buildable")
}

fn git(args: &[&str], dir: &Path) {
    let status = process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Create a minimal workspace with one repo and a plain (non-git) project dir.
///
/// Layout:
///   {tmp}/ws/                         -- workspace root
///   {tmp}/ws/github/org/repo/         -- a real git repo
///   {tmp}/ws/projects/{project}/      -- plain dir with rwv.yaml
fn make_workspace(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: primary
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    ws
}

/// Create a workspace with two code repos and a plain project dir listing both.
fn make_workspace_two_repos(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo1 = ws.join("github/org/alpha");
    let repo2 = ws.join("github/org/beta");
    init_repo_with_commit(&repo1);
    init_repo_with_commit(&repo2);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  github/org/alpha:
    type: git
    url: file://{r1}
    version: main
    role: primary
  github/org/beta:
    type: git
    url: file://{r2}
    version: main
    role: primary
"#,
        r1 = repo1.display(),
        r2 = repo2.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    ws
}

// ---------------------------------------------------------------------------
// 1. rwv_display_shows_repos (doc: project-reporoot-0ptp)
//
// Doc claim: "`rwv` (no subcommand) shows root, project, workweave, repos"
//
// With an active project, the output must contain the weave root path, the
// active project name, and evidence of the repos from the manifest.
// ---------------------------------------------------------------------------

#[test]
fn rwv_display_shows_repos() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_two_repos(tmp.path(), "web-app");

    // Activate the project by writing .rwv-active.
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    let output = rwv()
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");

    // Root path must appear.
    let canonical_root = ws.canonicalize().unwrap();
    assert!(
        stdout.contains(canonical_root.to_str().unwrap()),
        "output should contain the weave root path {}, got:\n{stdout}",
        canonical_root.display()
    );

    // Active project name must appear.
    assert!(
        stdout.contains("web-app"),
        "output should contain the active project name 'web-app', got:\n{stdout}"
    );

    // The manifest has 2 repos; the display reports the repo count.
    assert!(
        stdout.contains('2') || stdout.contains("alpha") || stdout.contains("beta"),
        "output should reference the repos from the manifest, got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// 3. rwv_display_no_active_project (doc: project-reporoot-0ptp)
//
// With no .rwv-active file, the output should still show the root and list
// available projects, but not claim any project is active.
// ---------------------------------------------------------------------------

#[test]
fn rwv_display_no_active_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "my-project");

    // Deliberately do NOT write .rwv-active — no project is active.

    let output = rwv()
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");

    // Root path must appear.
    let canonical_root = ws.canonicalize().unwrap();
    assert!(
        stdout.contains(canonical_root.to_str().unwrap()),
        "output should contain the weave root path, got:\n{stdout}"
    );

    // The available project should be listed (the projects/ dir is scanned).
    assert!(
        stdout.contains("my-project"),
        "output should list available projects (found 'my-project'), got:\n{stdout}"
    );

    // No "Project:" line should appear when nothing is active.
    assert!(
        !stdout.contains("Project:"),
        "output should NOT show 'Active project:' when no project is active, got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// 4. rwv_display_in_workweave (doc: project-reporoot-0ptp)
//
// Doc claim: running `rwv` from inside a workweave shows "workweave" location
// and the workweave name.
// ---------------------------------------------------------------------------

#[test]
fn rwv_display_in_workweave() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create the workweave.
    rwv()
        .args(["workweave", "web-app", "create", "display-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("ws--display-test");
    assert!(ww_dir.exists(), "workweave directory should exist");

    // Run `rwv` (no subcommand) from inside the workweave directory.
    let output = rwv()
        .current_dir(&ww_dir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");

    // Output must mention "workweave" (case-insensitive).
    assert!(
        stdout.to_lowercase().contains("workweave"),
        "output should contain 'workweave' when run from inside a workweave, got:\n{stdout}"
    );

    // Output must contain the workweave name.
    assert!(
        stdout.contains("display-test"),
        "output should contain the workweave name 'display-test', got:\n{stdout}"
    );

    // Root path (primary weave root) must appear.
    let canonical_root = ws.canonicalize().unwrap();
    assert!(
        stdout.contains(canonical_root.to_str().unwrap()),
        "output should contain the primary root path {}, got:\n{stdout}",
        canonical_root.display()
    );
}
