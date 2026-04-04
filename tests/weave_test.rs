//! E2E tests for `rwv weave`.
//!
//! These tests exercise the CLI binary via `assert_cmd`. Tests that require
//! the weave command to be fully implemented (bead 9b) are marked `#[ignore]`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

/// Build a `Command` for the `rwv` binary.
fn rwv() -> Command {
    Command::cargo_bin("rwv").expect("rwv binary should be buildable")
}

/// Run a git command in `dir`, panicking on failure.
fn git(args: &[&str], dir: &Path) {
    let status = process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {:?} in {} failed", args, dir.display());
}

/// Initialise a normal (non-bare) git repo at `path` with one commit on `main`.
fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Create a minimal workspace structure with one project and one repo.
///
/// Layout:
///   {tmp}/ws/                  -- workspace root
///   {tmp}/ws/github/           -- registry marker (makes it a workspace root)
///   {tmp}/ws/projects/{project}/rwv.yaml
///   {tmp}/ws/github/org/repo/  -- a real git repo with a commit
///
/// Returns the workspace root path.
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

// ============================================================================
// Smoke tests -- command recognition (can pass now)
// ============================================================================

#[test]
fn weave_subcommand_is_recognised() {
    // `rwv weave` with a project name should not produce "unrecognized subcommand".
    let assert = rwv().args(["weave", "my-project"]).assert();
    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "weave should be a recognised subcommand, got stderr: {stderr}"
    );
}

#[test]
fn weave_requires_project_argument() {
    rwv()
        .arg("weave")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn weave_accepts_project_and_name() {
    // `rwv weave my-project hotfix` should be accepted by the CLI parser.
    let assert = rwv().args(["weave", "my-project", "hotfix"]).assert();
    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should not fail with a clap parse error.
    assert!(
        !stderr.contains("unexpected argument"),
        "weave should accept project + name, got stderr: {stderr}"
    );
}

// ============================================================================
// Weave create -- `rwv weave PROJECT NAME`
// ============================================================================

#[test]

fn weave_create_makes_sibling_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    rwv()
        .args(["weave", "web-app", "hotfix"])
        .current_dir(&ws)
        .assert()
        .success();

    // Weave directory should be a sibling of the workspace root.
    let weave_dir = tmp.path().join("ws--hotfix");
    assert!(
        weave_dir.exists(),
        "weave sibling directory ws--hotfix should exist at {}",
        weave_dir.display()
    );
}

#[test]

fn weave_create_worktrees_on_ephemeral_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    rwv()
        .args(["weave", "web-app", "hotfix"])
        .current_dir(&ws)
        .assert()
        .success();

    // The worktree in the weave should be on an ephemeral branch
    // named {weave-name}/{original-branch}, i.e. "hotfix/main".
    let weave_repo = tmp.path().join("ws--hotfix/github/org/repo");
    assert!(
        weave_repo.exists(),
        "weave should contain worktree at github/org/repo"
    );

    let output = process::Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(&weave_repo)
        .output()
        .expect("git should work");
    let branch = String::from_utf8(output.stdout)
        .expect("valid UTF-8")
        .trim()
        .to_string();
    assert_eq!(
        branch, "hotfix/main",
        "worktree should be on ephemeral branch hotfix/main, got: {branch}"
    );
}

#[test]

fn weave_create_mirrors_primary_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    rwv()
        .args(["weave", "web-app", "feat-x"])
        .current_dir(&ws)
        .assert()
        .success();

    let weave_dir = tmp.path().join("ws--feat-x");
    // The weave should mirror the primary layout: github/org/repo should exist.
    assert!(
        weave_dir.join("github/org/repo").exists(),
        "weave should mirror primary directory structure"
    );
    // The repo inside the weave should be a git worktree (has .git file, not dir).
    let dot_git = weave_dir.join("github/org/repo/.git");
    assert!(
        dot_git.exists(),
        ".git should exist in the weave repo (as a file for worktrees)"
    );
}

// ============================================================================
// Weave delete -- `rwv weave PROJECT --delete`
// ============================================================================

#[test]

fn weave_delete_removes_directory_and_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // Create a weave first.
    rwv()
        .args(["weave", "web-app", "to-delete"])
        .current_dir(&ws)
        .assert()
        .success();

    let weave_dir = tmp.path().join("ws--to-delete");
    assert!(weave_dir.exists(), "weave should exist before deletion");

    // Delete it.
    rwv()
        .args(["weave", "web-app", "to-delete", "--delete"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        !weave_dir.exists(),
        "weave directory should be removed after --delete"
    );

    // The primary repo should not list the weave worktree any more.
    let output = process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(ws.join("github/org/repo"))
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.contains("ws--to-delete"),
        "worktree should be cleaned up from primary repo, got: {listing}"
    );
}

// ============================================================================
// Weave list -- `rwv weave PROJECT --list`
// ============================================================================

#[test]

fn weave_list_shows_existing_weaves() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // Create two weaves.
    rwv()
        .args(["weave", "web-app", "alpha"])
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["weave", "web-app", "beta"])
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args(["weave", "web-app", "--list"])
        .current_dir(&ws)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("alpha").and(predicate::str::contains("beta")),
        );
}

#[test]

fn weave_list_empty_when_no_weaves() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    rwv()
        .args(["weave", "web-app", "--list"])
        .current_dir(&ws)
        .assert()
        .success();
    // No assertion on content — just that it succeeds with no weaves.
}

// ============================================================================
// Weave sync -- `rwv weave PROJECT --sync`
// ============================================================================

#[test]

fn weave_sync_reconciles_with_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // Create a weave.
    rwv()
        .args(["weave", "web-app", "sync-test"])
        .current_dir(&ws)
        .assert()
        .success();

    // Add a second repo to the manifest.
    let repo2 = ws.join("github/org/repo2");
    init_repo_with_commit(&repo2);

    let project_dir = ws.join("projects/web-app");
    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo1}
    version: main
    role: primary
  github/org/repo2:
    type: git
    url: file://{repo2}
    version: main
    role: primary
"#,
        repo1 = ws.join("github/org/repo").display(),
        repo2 = repo2.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    // Sync should add the new repo's worktree to the weave.
    rwv()
        .args(["weave", "web-app", "sync-test", "--sync"])
        .current_dir(&ws)
        .assert()
        .success();

    let weave_repo2 = tmp.path().join("ws--sync-test/github/org/repo2");
    assert!(
        weave_repo2.exists(),
        "sync should add newly-listed repo worktree to the weave"
    );
}

// ============================================================================
// WEAVEROOT override
// ============================================================================

#[test]

fn weave_respects_weaveroot_env() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let custom_root = tmp.path().join("custom-weaves");
    std::fs::create_dir_all(&custom_root).unwrap();

    rwv()
        .args(["weave", "web-app", "override-test"])
        .env("WEAVEROOT", &custom_root)
        .current_dir(&ws)
        .assert()
        .success();

    // The weave should be created under the custom root, not as a sibling.
    let weave_in_custom = custom_root.join("ws--override-test");
    assert!(
        weave_in_custom.exists(),
        "weave should be created under WEAVEROOT at {}",
        weave_in_custom.display()
    );

    // It should NOT be a sibling of the workspace root.
    let default_sibling = tmp.path().join("ws--override-test");
    assert!(
        !default_sibling.exists(),
        "weave should NOT be at the default sibling location when WEAVEROOT is set"
    );
}

// ============================================================================
// Multi-repo weave structure
// ============================================================================

#[test]

fn weave_with_multiple_repos_creates_all_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");

    // Create two repos.
    let repo1 = ws.join("github/org/server");
    let repo2 = ws.join("github/org/client");
    init_repo_with_commit(&repo1);
    init_repo_with_commit(&repo2);

    // Create project with both repos.
    let project_dir = ws.join("projects/full-stack");
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest = format!(
        r#"repositories:
  github/org/server:
    type: git
    url: file://{server}
    version: main
    role: primary
  github/org/client:
    type: git
    url: file://{client}
    version: main
    role: fork
"#,
        server = repo1.display(),
        client = repo2.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    rwv()
        .args(["weave", "full-stack", "multi"])
        .current_dir(&ws)
        .assert()
        .success();

    let weave_dir = tmp.path().join("ws--multi");
    assert!(
        weave_dir.join("github/org/server").exists(),
        "server worktree should exist in weave"
    );
    assert!(
        weave_dir.join("github/org/client").exists(),
        "client worktree should exist in weave"
    );
}
