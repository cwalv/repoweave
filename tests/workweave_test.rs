//! E2E tests for `rwv workweave`.
//!
//! These tests exercise the CLI binary via `assert_cmd`. Tests that require
//! the workweave command to be fully implemented are marked `#[ignore]` where
//! appropriate.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

mod common;

/// Build a `Command` for the `rwv` binary.
fn rwv() -> Command {
    common::rwv()
}

/// Run a git command in `dir`, panicking on failure.
fn git(args: &[&str], dir: &Path) {
    let status = common::git()
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
    role: owned
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    ws
}

/// Create a workspace where the project directory is itself a git repo.
///
/// Layout:
///   {tmp}/ws/                          -- workspace root
///   {tmp}/ws/github/                   -- registry marker
///   {tmp}/ws/projects/{project}/       -- git repo with commit + rwv.yaml
///   {tmp}/ws/github/org/repo/          -- manifest repo
fn make_workspace_with_project_repo(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    init_repo_with_commit(&project_dir);

    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: owned
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();
    git(&["add", "rwv.yaml"], &project_dir);
    git(&["commit", "-m", "add manifest"], &project_dir);

    ws
}

// ============================================================================
// Smoke tests -- command recognition (can pass now)
// ============================================================================

#[test]
fn workweave_subcommand_is_recognised() {
    // `rwv workweave` with a project name should not produce "unrecognized subcommand".
    let assert = rwv().args(["workweave", "my-project", "list"]).assert();
    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "workweave should be a recognised subcommand, got stderr: {stderr}"
    );
}

#[test]
fn workweave_requires_project_argument() {
    rwv()
        .arg("workweave")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn workweave_accepts_project_and_name() {
    // `rwv workweave my-project create hotfix` should be accepted by the CLI parser.
    let assert = rwv()
        .args(["workweave", "my-project", "create", "hotfix"])
        .assert();
    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Should not fail with a clap parse error.
    assert!(
        !stderr.contains("unexpected argument"),
        "workweave should accept project + create + name, got stderr: {stderr}"
    );
}

// ============================================================================
// Workweave create -- `rwv workweave PROJECT NAME`
// ============================================================================

#[test]
fn workweave_create_makes_directory_under_weaveroot() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // Use RWV_WORKWEAVE_DIR so the workweave goes to a known location.
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "hotfix"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Workweave directory should be under RWV_WORKWEAVE_DIR.
    let ww_dir = weaveroot.join("web-app--hotfix");
    assert!(
        ww_dir.exists(),
        "workweave directory web-app--hotfix should exist at {}",
        ww_dir.display()
    );
}

#[test]
fn workweave_create_worktrees_on_ephemeral_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "hotfix"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // The worktree in the workweave should be on an ephemeral branch
    // named {project}--{workweave-name}/{original-branch}, i.e.
    // "web-app--hotfix/main".
    let weave_repo = weaveroot.join("web-app--hotfix/github/org/repo");
    assert!(
        weave_repo.exists(),
        "workweave should contain worktree at github/org/repo"
    );

    let output = common::git()
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(&weave_repo)
        .output()
        .expect("git should work");
    let branch = String::from_utf8(output.stdout)
        .expect("valid UTF-8")
        .trim()
        .to_string();
    assert_eq!(
        branch, "web-app--hotfix/main",
        "worktree should be on ephemeral branch web-app--hotfix/main, got: {branch}"
    );
}

#[test]
fn workweave_create_mirrors_primary_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat-x"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--feat-x");
    // The workweave should mirror the primary layout: github/org/repo should exist.
    assert!(
        ww_dir.join("github/org/repo").exists(),
        "workweave should mirror primary directory structure"
    );
    // The repo inside the workweave should be a git worktree (has .git file, not dir).
    let dot_git = ww_dir.join("github/org/repo/.git");
    assert!(
        dot_git.exists(),
        ".git should exist in the workweave repo (as a file for worktrees)"
    );
}

// ============================================================================
// Workweave create -- project repo worktree (new in rwv-2h1)
// ============================================================================

#[test]
fn create_workweave_includes_project_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "my-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "my-project", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // projects/my-project/ should exist in the workweave as a worktree.
    let project_wt = weaveroot.join("my-project--feat/projects/my-project");
    assert!(
        project_wt.exists(),
        "workweave should contain project worktree at projects/my-project, expected at {}",
        project_wt.display()
    );

    // Confirm it's a git worktree (has .git file, not directory).
    let dot_git = project_wt.join(".git");
    assert!(
        dot_git.exists(),
        ".git should exist in the project worktree"
    );
    let meta = std::fs::symlink_metadata(&dot_git).unwrap();
    assert!(
        meta.file_type().is_file(),
        ".git should be a file (worktree), not a directory"
    );
}

#[test]
fn delete_workweave_removes_project_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "my-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create first.
    rwv()
        .args(["workweave", "my-project", "create", "to-del"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("my-project--to-del");
    assert!(ww_dir.exists(), "workweave should exist before deletion");

    // Delete it. Pass --force: activation writes generated files into the
    // workweave's project worktree (workspace config, ecosystem outputs)
    // that the dirty check would otherwise treat as untracked changes.
    // This test is verifying worktree cleanup, not dirty-check
    // semantics, so the --force is incidental.
    rwv()
        .args(["workweave", "my-project", "delete", "to-del", "--force"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        !ww_dir.exists(),
        "workweave directory should be removed after --delete"
    );

    // The primary project repo should not list the workweave worktree any more.
    let primary_project = ws.join("projects/my-project");
    let output = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&primary_project)
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.contains("my-project--to-del"),
        "project worktree should be cleaned up from primary repo, got: {listing}"
    );
}

// ============================================================================
// Workweave create -- artifact processing (new in rwv-2h1)
// ============================================================================

#[test]
fn create_workweave_processes_copy_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // Place a .env file in the workspace root.
    std::fs::write(ws.join(".env"), "SECRET=hunter2\n").unwrap();

    // Update the manifest to include workweave.copy.
    let project_dir = ws.join("projects/web-app");
    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: owned
workweave:
  copy:
    - .env
"#,
        repo = ws.join("github/org/repo").display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "copy-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--copy-test");
    let copied_env = ww_dir.join(".env");
    assert!(
        copied_env.exists(),
        ".env should be copied into workweave at {}",
        copied_env.display()
    );

    // Should be a regular file, not a symlink.
    let meta = std::fs::symlink_metadata(&copied_env).unwrap();
    assert!(
        meta.file_type().is_file(),
        ".env copy should be a regular file, not a symlink"
    );

    // Content should match.
    let content = std::fs::read_to_string(&copied_env).unwrap();
    assert_eq!(content, "SECRET=hunter2\n");
}

#[test]
#[cfg(unix)]
fn create_workweave_processes_link_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // Create a shared directory to link.
    let shared_dir = ws.join("shared-state");
    std::fs::create_dir_all(&shared_dir).unwrap();
    std::fs::write(shared_dir.join("data.db"), "db content").unwrap();

    // Update manifest with workweave.link.
    let project_dir = ws.join("projects/web-app");
    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: owned
workweave:
  link:
    - shared-state
"#,
        repo = ws.join("github/org/repo").display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "link-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--link-test");
    let linked = ww_dir.join("shared-state");
    assert!(
        linked.exists(),
        "shared-state should exist in workweave at {}",
        linked.display()
    );

    // Should be a symlink.
    let meta = std::fs::symlink_metadata(&linked).unwrap();
    assert!(
        meta.file_type().is_symlink(),
        "shared-state should be a symlink in workweave"
    );

    // The symlink target should be an absolute path pointing to the primary.
    let target = std::fs::read_link(&linked).unwrap();
    assert!(
        target.is_absolute(),
        "symlink target should be absolute, got: {}",
        target.display()
    );
    assert!(
        target.ends_with("shared-state"),
        "symlink target should end with shared-state, got: {}",
        target.display()
    );
}

// ============================================================================
// Workweave create -- marker and .rwv-active (new in rwv-2h1)
// ============================================================================

#[test]
fn create_workweave_writes_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "marker-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--marker-test");
    let marker_file = ww_dir.join(".rwv-workweave");
    assert!(
        marker_file.exists(),
        ".rwv-workweave marker should exist at {}",
        marker_file.display()
    );

    // Parse and verify contents.
    let content = std::fs::read_to_string(&marker_file).unwrap();
    // primary should contain the workspace root path.
    let ws_canonical = ws.canonicalize().unwrap();
    assert!(
        content.contains(ws_canonical.to_str().unwrap()),
        ".rwv-workweave should contain primary path {}, got:\n{content}",
        ws_canonical.display()
    );
    // project should be "web-app".
    assert!(
        content.contains("web-app"),
        ".rwv-workweave should contain project name, got:\n{content}"
    );
}

#[test]
fn create_workweave_writes_rwv_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "active-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--active-test");
    let active_file = ww_dir.join(".rwv-active");
    assert!(
        active_file.exists(),
        ".rwv-active should exist in workweave at {}",
        active_file.display()
    );

    let content = std::fs::read_to_string(&active_file).unwrap();
    assert_eq!(
        content.trim(),
        "web-app",
        ".rwv-active should contain project name 'web-app', got: {content}"
    );
}

// ============================================================================
// Workweave delete -- `rwv workweave PROJECT --delete`
// ============================================================================

#[test]
fn workweave_delete_removes_directory_and_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create a workweave first.
    rwv()
        .args(["workweave", "web-app", "create", "to-delete"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--to-delete");
    assert!(ww_dir.exists(), "workweave should exist before deletion");

    // Delete it.
    rwv()
        .args(["workweave", "web-app", "delete", "to-delete"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        !ww_dir.exists(),
        "workweave directory should be removed after --delete"
    );

    // The primary repo should not list the workweave worktree any more.
    let output = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(ws.join("github/org/repo"))
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.contains("web-app--to-delete"),
        "worktree should be cleaned up from primary repo, got: {listing}"
    );
}

// ============================================================================
// Workweave list -- `rwv workweave PROJECT --list`
// ============================================================================

#[test]
fn workweave_list_shows_existing_workweaves() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create two workweaves.
    rwv()
        .args(["workweave", "web-app", "create", "alpha"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["workweave", "web-app", "create", "beta"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args(["workweave", "web-app", "list"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains("alpha").and(predicate::str::contains("beta")));
}

#[test]
fn workweave_list_empty_when_no_workweaves() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "list"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    // No assertion on content — just that it succeeds with no workweaves.
}

// ============================================================================
// RWV_WORKWEAVE_DIR override
// ============================================================================

#[test]
fn workweave_respects_weaveroot_env() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let custom_root = tmp.path().join("custom-weaves");
    std::fs::create_dir_all(&custom_root).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "override-test"])
        .env("RWV_WORKWEAVE_DIR", &custom_root)
        .current_dir(&ws)
        .assert()
        .success();

    // The workweave should be created under the custom root.
    let ww_in_custom = custom_root.join("web-app--override-test");
    assert!(
        ww_in_custom.exists(),
        "workweave should be created under RWV_WORKWEAVE_DIR at {}",
        ww_in_custom.display()
    );
}

// ============================================================================
// Multi-repo workweave structure
// ============================================================================

#[test]
fn workweave_with_multiple_repos_creates_all_worktrees() {
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
    role: owned
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

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "full-stack", "create", "multi"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("full-stack--multi");
    assert!(
        ww_dir.join("github/org/server").exists(),
        "server worktree should exist in workweave"
    );
    assert!(
        ww_dir.join("github/org/client").exists(),
        "client worktree should exist in workweave"
    );
}

// ============================================================================
// --hook-mode flag
// ============================================================================

#[test]
fn cli_workweave_hook_mode_outputs_path() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let output = rwv()
        .args(["workweave", "web-app", "--hook-mode", "create", "hook-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");
    let stdout = stdout.trim();

    // stdout should be a single line: the workweave path
    assert_eq!(
        stdout.lines().count(),
        1,
        "--hook-mode stdout should be exactly one line, got: {stdout:?}"
    );

    // The path should end with the workweave directory name
    assert!(
        stdout.ends_with("web-app--hook-test"),
        "--hook-mode stdout should end with the workweave dir name 'web-app--hook-test', got: {stdout}"
    );
}

#[test]
fn cli_workweave_hook_mode_path_is_absolute() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let output = rwv()
        .args(["workweave", "web-app", "--hook-mode", "create", "abs-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");
    let path_str = stdout.trim();
    let path = std::path::Path::new(path_str);

    assert!(
        path.is_absolute(),
        "--hook-mode should print an absolute path, got: {path_str}"
    );
}

#[test]
fn cli_workweave_create_without_hook_mode() {
    // Without --hook-mode, normal create should succeed but stdout should NOT
    // be just a bare path (it may be empty or contain human-friendly output).
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let output = rwv()
        .args(["workweave", "web-app", "create", "normal-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");

    // Without hook mode, stdout should be empty (no path printed)
    assert!(
        stdout.trim().is_empty(),
        "without --hook-mode stdout should be empty, got: {stdout:?}"
    );
}

#[test]
fn cli_workweave_help_says_workweave() {
    // Verify help text uses "workweave" terminology (not "weave")
    rwv()
        .args(["workweave", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("workweave"))
        .stdout(predicate::str::contains("hook-mode"));
}

// ============================================================================
// Full round-trip: create → verify layout → delete → verify clean
// ============================================================================

#[test]
fn workweave_full_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "round-trip-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // --- Create ---
    rwv()
        .args(["workweave", "round-trip-project", "create", "rt"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("round-trip-project--rt");

    // Verify repo worktree exists.
    let repo_wt = ww_dir.join("github/org/repo");
    assert!(repo_wt.exists(), "repo worktree should exist after create");
    let dot_git = repo_wt.join(".git");
    assert!(
        dot_git.exists() && dot_git.is_file(),
        "repo .git should be a worktree file"
    );

    // Verify project worktree exists.
    let project_wt = ww_dir.join("projects/round-trip-project");
    assert!(
        project_wt.exists(),
        "project worktree should exist after create"
    );
    let project_dot_git = project_wt.join(".git");
    assert!(
        project_dot_git.exists() && project_dot_git.is_file(),
        "project .git should be a worktree file"
    );

    // Verify marker and .rwv-active.
    assert!(
        ww_dir.join(".rwv-workweave").exists(),
        ".rwv-workweave should exist"
    );
    assert!(
        ww_dir.join(".rwv-active").exists(),
        ".rwv-active should exist"
    );

    // --- Delete ---
    // Pass --force: activation generates files in the project worktree
    // (workspace config, ecosystem outputs) that count as untracked changes
    // under the dirty check. The round-trip test isn't about dirty
    // semantics; the --force is incidental to making delete work after
    // the create-and-activate cycle.
    rwv()
        .args(["workweave", "round-trip-project", "delete", "rt", "--force"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Verify workweave directory is gone.
    assert!(
        !ww_dir.exists(),
        "workweave directory should be removed after delete"
    );

    // Verify repo worktree is cleaned up from primary.
    let output = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(ws.join("github/org/repo"))
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.contains("round-trip-project--rt"),
        "repo worktree should be cleaned up from primary, got: {listing}"
    );

    // Verify project worktree is cleaned up from primary.
    let output = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(ws.join("projects/round-trip-project"))
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.contains("round-trip-project--rt"),
        "project worktree should be cleaned up from primary, got: {listing}"
    );
}

// ============================================================================
// Ecosystem files generated by integrations in a workweave
// ============================================================================

/// Create a workspace where the primary repo contains a Cargo.toml so the
/// cargo-workspace integration will generate a workspace Cargo.toml.
fn make_workspace_with_cargo_repo(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/cargo-crate");
    init_repo_with_commit(&repo_path);

    // Add a Cargo.toml to the repo so the integration detects it.
    std::fs::write(
        repo_path.join("Cargo.toml"),
        "[package]\nname = \"cargo-crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    git(&["add", "Cargo.toml"], &repo_path);
    git(&["commit", "-m", "add Cargo.toml"], &repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  github/org/cargo-crate:
    type: git
    url: file://{repo}
    version: main
    role: owned
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();
    ws
}

#[test]
fn create_workweave_generates_ecosystem_files() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_cargo_repo(tmp.path(), "cargo-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "cargo-project", "create", "eco"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // The cargo-workspace integration should have generated Cargo.toml
    // in the workweave's project directory.
    let ww_dir = weaveroot.join("cargo-project--eco");
    let generated_cargo = ww_dir.join("projects/cargo-project/Cargo.toml");
    assert!(
        generated_cargo.exists(),
        "cargo-workspace integration should generate Cargo.toml in the workweave project dir at {}",
        generated_cargo.display()
    );

    let content = std::fs::read_to_string(&generated_cargo).unwrap();
    assert!(
        content.contains("[workspace]"),
        "generated Cargo.toml should contain [workspace], got:\n{content}"
    );
    assert!(
        content.contains("cargo-crate"),
        "generated Cargo.toml should list the repo member, got:\n{content}"
    );
}

// ============================================================================
// rwv resolve from inside a workweave
// ============================================================================

#[test]
fn resolve_from_inside_workweave_returns_workweave_path() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create a workweave.
    rwv()
        .args(["workweave", "web-app", "create", "resolve-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--resolve-test");

    // Run `rwv resolve` from inside the workweave directory.
    let output = rwv()
        .arg("resolve")
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ww_dir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");
    let resolved = stdout.trim();

    // The resolved path should be the workweave directory (not the primary).
    let ww_canonical = ww_dir.canonicalize().unwrap();
    let resolved_path = std::path::Path::new(resolved);
    let resolved_canonical = resolved_path
        .canonicalize()
        .unwrap_or_else(|_| resolved_path.to_path_buf());

    assert_eq!(
        resolved_canonical, ww_canonical,
        "rwv resolve from workweave should return the workweave path, got: {resolved}"
    );
}

// ============================================================================
// Edge cases
// ============================================================================

#[test]
fn workweave_name_with_hyphens_and_underscores() {
    // Workweave names may contain hyphens and underscores.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat_my-feature_v2"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--feat_my-feature_v2");
    assert!(
        ww_dir.exists(),
        "workweave with hyphen/underscore name should be created at {}",
        ww_dir.display()
    );
    assert!(
        ww_dir.join(".rwv-workweave").exists(),
        ".rwv-workweave marker should exist in hyphen/underscore-named workweave"
    );
}

#[test]
fn workweave_create_without_rwv_active_in_primary() {
    // Creating a workweave does not require .rwv-active in the primary workspace
    // because the project name is passed explicitly as an argument.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "my-proj");

    // Explicitly ensure .rwv-active does NOT exist in the workspace.
    let active_file = ws.join(".rwv-active");
    if active_file.exists() {
        std::fs::remove_file(&active_file).unwrap();
    }

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Should succeed: project name is provided as CLI argument.
    rwv()
        .args(["workweave", "my-proj", "create", "no-active"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("my-proj--no-active");
    assert!(
        ww_dir.exists(),
        "workweave should be created even when primary has no .rwv-active"
    );
}

#[test]
fn delete_nonexistent_workweave_errors_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Attempt to delete a workweave that was never created.
    let result = rwv()
        .args(["workweave", "web-app", "delete", "ghost"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert();

    // The command should either succeed (nothing to do) or fail with a clear
    // error — it must not panic or produce an unhandled error.
    // We accept both outcomes but verify no panic occurred (exit code checked).
    let output = result.get_output();
    let exit_code = output.status.code().unwrap_or(-1);

    // Exit code 0 (graceful no-op) or non-zero (error message) are both acceptable.
    // What is NOT acceptable is a process crash (signal termination, no exit code).
    assert!(
        output.status.code().is_some(),
        "delete of non-existent workweave should exit cleanly (not crash), got exit status: {}",
        output.status
    );
    let _ = exit_code; // silence unused warning
}

// ============================================================================
// Ephemeral branch cleanup (rwv-9mp)
// ============================================================================

/// Helper: list local branches in a git repo whose names start with `prefix/`.
fn branches_with_prefix(repo: &Path, prefix: &str) -> Vec<String> {
    let output = common::git()
        .args(["branch", "--list", &format!("{prefix}/*")])
        .current_dir(repo)
        .output()
        .expect("git branch --list should work");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim_start_matches('*').trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

#[test]
fn delete_workweave_cleans_up_ephemeral_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create a workweave — this creates ephemeral branch "web-app--cleanup/main" in the repo.
    rwv()
        .args(["workweave", "web-app", "create", "cleanup"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let primary_repo = ws.join("github/org/repo");

    // Confirm the ephemeral branch exists before deletion.
    let before = branches_with_prefix(&primary_repo, "web-app--cleanup");
    assert!(
        !before.is_empty(),
        "ephemeral branch web-app--cleanup/main should exist before delete, got: {before:?}"
    );

    // Delete the workweave.
    rwv()
        .args(["workweave", "web-app", "delete", "cleanup"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // The ephemeral branch should be gone.
    let after = branches_with_prefix(&primary_repo, "web-app--cleanup");
    assert!(
        after.is_empty(),
        "delete_workweave should remove ephemeral branches with prefix 'web-app--cleanup/', remaining: {after:?}"
    );
}

#[test]
fn create_workweave_handles_stale_branches() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create a workweave, then delete it normally (branches cleaned up).
    rwv()
        .args(["workweave", "web-app", "create", "stale-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args(["workweave", "web-app", "delete", "stale-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Manually re-create the stale ephemeral branch to simulate a failed cleanup.
    let primary_repo = ws.join("github/org/repo");
    let head = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(&primary_repo)
        .output()
        .expect("git rev-parse HEAD");
    let head_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let status = common::git()
        .args(["branch", "web-app--stale-test/main", &head_sha])
        .current_dir(&primary_repo)
        .status()
        .expect("git branch web-app--stale-test/main");
    assert!(status.success(), "should be able to create stale branch");

    // Creating the workweave again with the same name should succeed despite
    // the stale ephemeral branch.
    rwv()
        .args(["workweave", "web-app", "create", "stale-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Verify the workweave was actually created.
    let ww_dir = weaveroot.join("web-app--stale-test");
    assert!(
        ww_dir.join("github/org/repo").exists(),
        "workweave should be created successfully even with pre-existing stale branch"
    );
}

// ============================================================================
// Fork source — peer workweaves fork from CWD's active workspace
// ============================================================================

/// Helper: read HEAD revision SHA from a git repo or worktree.
fn head_sha(repo: &Path) -> String {
    let output = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .expect("git rev-parse HEAD");
    assert!(
        output.status.success(),
        "git rev-parse HEAD failed in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn workweave_create_from_workweave_cwd_forks_from_workweave() {
    // When `rwv workweave create` is invoked from inside a workweave (no
    // `--from` flag), the new peer should fork from that workweave's HEAD,
    // not from primary's. Establishes the rig=workweave model: peer rooted
    // in rig means rig→peer is a fast-forward and `rwv sync` works without
    // ancestor divergence.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create the rig workweave from primary CWD (today's behavior).
    rwv()
        .args(["workweave", "web-app", "create", "rig"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let rig_dir = weaveroot.join("web-app--rig");
    let rig_repo = rig_dir.join("github/org/repo");
    let primary_repo = ws.join("github/org/repo");

    // Advance the rig's repo so its HEAD diverges from primary's. The
    // commit lands on the rig's ephemeral branch only.
    std::fs::write(rig_repo.join("rig-marker.txt"), "advanced in rig\n").unwrap();
    git(&["add", "rig-marker.txt"], &rig_repo);
    git(&["commit", "-m", "advance rig"], &rig_repo);

    let primary_head = head_sha(&primary_repo);
    let rig_head = head_sha(&rig_repo);
    assert_ne!(
        primary_head, rig_head,
        "rig should have advanced past primary; primary={primary_head}, rig={rig_head}"
    );

    // Create a peer workweave from inside the rig. Default (no --from)
    // should fork from rig.
    rwv()
        .args(["workweave", "web-app", "create", "peer"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&rig_dir)
        .assert()
        .success();

    // Peer should land in the same .workweaves/ as rig, not nested under
    // rig — workweaves are flat under primary's parent.
    let peer_dir = weaveroot.join("web-app--peer");
    assert!(
        peer_dir.exists(),
        "peer workweave should live alongside rig at {}, not nested under rig",
        peer_dir.display()
    );
    assert!(
        !rig_dir.join(".workweaves").exists(),
        "peer should not be created nested under rig at {}/.workweaves",
        rig_dir.display()
    );

    let peer_repo = peer_dir.join("github/org/repo");
    let peer_head = head_sha(&peer_repo);
    assert_eq!(
        peer_head, rig_head,
        "peer forked from rig CWD should start at rig's HEAD ({rig_head}), got peer={peer_head}, primary={primary_head}"
    );

    // Marker still points at primary, regardless of source.
    let marker = std::fs::read_to_string(peer_dir.join(".rwv-workweave")).unwrap();
    let ws_canonical = ws.canonicalize().unwrap();
    assert!(
        marker.contains(ws_canonical.to_str().unwrap()),
        "peer marker should record primary {}, got:\n{marker}",
        ws_canonical.display()
    );
}

#[test]
fn workweave_create_from_primary_flag_overrides_active_path() {
    // When invoked from inside a workweave with `--from primary`, the new
    // peer should fork from primary's HEAD even though CWD's active
    // workspace is the rig. Escape hatch for operators who want the old
    // behavior explicitly.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "rig"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let rig_dir = weaveroot.join("web-app--rig");
    let rig_repo = rig_dir.join("github/org/repo");
    let primary_repo = ws.join("github/org/repo");

    std::fs::write(rig_repo.join("rig-marker.txt"), "advanced\n").unwrap();
    git(&["add", "rig-marker.txt"], &rig_repo);
    git(&["commit", "-m", "advance rig"], &rig_repo);

    let primary_head = head_sha(&primary_repo);
    let rig_head = head_sha(&rig_repo);
    assert_ne!(primary_head, rig_head);

    rwv()
        .args([
            "workweave",
            "web-app",
            "create",
            "peer",
            "--from",
            "primary",
        ])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&rig_dir)
        .assert()
        .success();

    let peer_repo = weaveroot.join("web-app--peer/github/org/repo");
    let peer_head = head_sha(&peer_repo);
    assert_eq!(
        peer_head, primary_head,
        "peer with --from primary should start at primary's HEAD ({primary_head}), got peer={peer_head}, rig={rig_head}"
    );
}

// ============================================================================
// --claude-hook flag
// ============================================================================

/// Helper: build WorktreeCreate JSON for a workspace cwd.
fn worktree_create_json(cwd: &std::path::Path, branch: &str, session: &str) -> String {
    serde_json::json!({
        "hook_event_name": "WorktreeCreate",
        "cwd": cwd.to_string_lossy(),
        "branch_name": branch,
        "session_id": session,
    })
    .to_string()
}

/// Helper: build WorktreeRemove JSON for a workweave path.
fn worktree_remove_json(worktree_path: &std::path::Path) -> String {
    serde_json::json!({
        "hook_event_name": "WorktreeRemove",
        "worktree_path": worktree_path.to_string_lossy(),
    })
    .to_string()
}

#[test]
fn claude_hook_create_produces_path() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let json = worktree_create_json(&ws, "feat/my-branch", "sess-001");

    let output = rwv()
        .args(["workweave", "--claude-hook"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .write_stdin(json)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let path_str = stdout.trim();
    assert!(
        !path_str.is_empty(),
        "should print workweave path to stdout"
    );

    let ww_path = std::path::Path::new(path_str);
    assert!(
        ww_path.exists(),
        "workweave directory should exist at {path_str}"
    );
}

#[test]
fn claude_hook_null_branch_fallback() {
    // When branch_name is "null", should generate a timestamp-based name
    // (session_id is ignored — it's constant within a session, causing collisions).
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let json = worktree_create_json(&ws, "null", "my-fallback-session");

    let output = rwv()
        .args(["workweave", "--claude-hook"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .write_stdin(json)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let ww_path_str = stdout.trim();
    // Should use timestamp fallback, not session_id
    assert!(
        ww_path_str.contains("workweave-"),
        "workweave path should use generated name (workweave-*), got: {ww_path_str}"
    );
    assert!(
        std::path::Path::new(ww_path_str).exists(),
        "workweave directory should exist"
    );
}

#[test]
fn claude_hook_remove_cleans_up() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // First create a workweave the normal way.
    rwv()
        .args(["workweave", "web-app", "create", "to-remove"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--to-remove");
    assert!(ww_dir.exists(), "workweave should exist before removal");

    // Now delete it via --claude-hook WorktreeRemove.
    let json = worktree_remove_json(&ww_dir);

    rwv()
        .args(["workweave", "--claude-hook"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .write_stdin(json)
        .assert()
        .success();

    assert!(
        !ww_dir.exists(),
        "workweave directory should be removed after WorktreeRemove hook"
    );
}

#[test]
fn claude_hook_unknown_event_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let json = serde_json::json!({
        "hook_event_name": "SomeUnknownEvent",
        "cwd": ws.to_string_lossy(),
    })
    .to_string();

    rwv()
        .args(["workweave", "--claude-hook"])
        .write_stdin(json)
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown hook_event_name"));
}

#[test]
fn claude_hook_conflicts_with_hook_mode_flag() {
    // --claude-hook should conflict with --hook-mode.
    rwv()
        .args(["workweave", "--claude-hook", "--hook-mode"])
        .write_stdin(r#"{}"#)
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

// ============================================================================
// `<project>--<name>` directory convention
// ============================================================================

/// `create_workweave` builds `<project>--<name>` when the project differs
/// from the primary weave's directory basename.
#[test]
fn create_workweave_dir_name_uses_project_not_primary_basename() {
    let tmp = tempfile::tempdir().unwrap();
    // primary basename = "ws", project = "web-app". The directory must be
    // `web-app--scratch`, not `ws--scratch`.
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "scratch"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        weaveroot.join("web-app--scratch").exists(),
        "workweave should land at .workweaves/web-app--scratch"
    );
    assert!(
        !weaveroot.join("ws--scratch").exists(),
        "must not use the legacy <primary>--<name> form"
    );
}

/// `delete_workweave` finds and removes the workweave under the new convention.
#[test]
fn delete_workweave_resolves_project_form() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "del-me"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    let ww_dir = weaveroot.join("web-app--del-me");
    assert!(ww_dir.exists());

    rwv()
        .args(["workweave", "web-app", "delete", "del-me"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    assert!(!ww_dir.exists(), "workweave dir should be removed");
}

/// `list_workweaves` returns workweaves scoped by project; workweaves of a
/// different project under the same primary are not included.
#[test]
fn list_workweaves_is_scoped_by_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "project-a");
    // Add a second project.
    let project_b_dir = ws.join("projects/project-b");
    std::fs::create_dir_all(&project_b_dir).unwrap();
    let repo_path = ws.join("github/org/repo");
    let manifest_b = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: owned
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_b_dir.join("rwv.yaml"), manifest_b).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "project-a", "create", "a-only"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["workweave", "project-b", "create", "b-only"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let out = rwv()
        .args(["workweave", "project-a", "list"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(out).unwrap();
    assert!(
        stdout.contains("a-only"),
        "project-a list should include 'a-only', got:\n{stdout}"
    );
    assert!(
        !stdout.contains("b-only"),
        "project-a list must not include 'b-only', got:\n{stdout}"
    );
}

/// Old-form workweaves (legacy `<primary>--<name>`, marker recorded) are
/// resolved by `workweave_path_for` via the marker scan, not the directory
/// name. Verified through `rwv workweave list`, which is one of the surfaces
/// that must work for legacy on-disk layouts.
#[test]
fn list_workweaves_includes_legacy_form_via_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Hand-craft an old-form workweave dir with a marker. No worktrees needed
    // to test the listing surface — `list_workweaves` only scans markers.
    let legacy = weaveroot.join("ws--from-old");
    std::fs::create_dir_all(&legacy).unwrap();
    let marker = format!(
        "primary: {}\nproject: web-app\n",
        ws.canonicalize().unwrap().display()
    );
    std::fs::write(legacy.join(".rwv-workweave"), marker).unwrap();

    let out = rwv()
        .args(["workweave", "web-app", "list"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(out).unwrap();
    assert!(
        stdout.contains("from-old"),
        "list should include legacy-form workweave via marker, got:\n{stdout}"
    );
}

// ============================================================================
// Pattern A4 — detached HEAD does not produce an ephemeral branch named "HEAD"
// ============================================================================

/// When the source repo is in detached-HEAD state, the workweave's ephemeral
/// branch must not be `proj--ww/HEAD` (which masquerades as a real ref).
/// Audit finding A4: emit `detached-<shortsha>` instead.
#[test]
fn create_workweave_detached_head_uses_detached_branch_name() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Detach HEAD in the manifest repo by checking out the commit SHA.
    let repo = ws.join("github/org/repo");
    let head_sha = head_sha(&repo);
    git(&["checkout", "--detach", &head_sha], &repo);

    rwv()
        .args(["workweave", "web-app", "create", "det"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // List branches on the source repo and confirm we have a
    // `web-app--det/detached-<shortsha>` branch — not `web-app--det/HEAD`.
    let branches = common::git()
        .args(["branch", "--list"])
        .current_dir(&repo)
        .output()
        .expect("git branch --list");
    let listing = String::from_utf8_lossy(&branches.stdout).to_string();
    assert!(
        !listing.contains("web-app--det/HEAD"),
        "detached HEAD must not be encoded as a branch named '/HEAD': {listing}"
    );
    assert!(
        listing.contains("web-app--det/detached-"),
        "expected a 'web-app--det/detached-<sha>' ephemeral branch, got:\n{listing}"
    );
}

// ============================================================================
// Pattern B7 — `workweave create` cleans up partial state on bail
// ============================================================================

/// If `create_workweave` fails partway through, it must remove the workweave
/// directory so a clean retry succeeds without `--force`. Audit finding B7.
#[test]
fn create_workweave_cleans_up_on_bail() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Point the manifest at a non-existent repo path so per-repo worktree
    // creation fails for every repo, causing the loop to bail.
    let project_dir = ws.join("projects/web-app");
    let bad_manifest = r#"repositories:
  github/org/missing:
    type: git
    url: file:///nonexistent/repo
    version: main
    role: owned
"#;
    std::fs::write(project_dir.join("rwv.yaml"), bad_manifest).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "retryme"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure();

    // The workweave directory must not be left on disk; otherwise the next
    // attempt is forced down the `--force` path.
    let ww_dir = weaveroot.join("web-app--retryme");
    assert!(
        !ww_dir.exists(),
        "create_workweave must clean up partial workweave dir on bail, found: {}",
        ww_dir.display()
    );
}

// ============================================================================
// Pattern B8 — project-repo worktree creation failure is fatal, not silent
// ============================================================================

/// When the project directory is a git repo but the worktree create itself
/// fails (e.g. branch conflict), the call must bail with a clear error rather
/// than silently fall through to `copy_dir_recursive` producing a static copy
/// that is not a worktree. Audit finding B8.
#[test]
fn create_workweave_bails_on_project_worktree_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Pre-populate the workweave's project-worktree destination with a
    // non-empty directory so `git worktree add` refuses. This also blocks
    // any silent `copy_dir_recursive` fallback from producing a fake
    // worktree — which is exactly what B8 is about.
    let ww_dir = weaveroot.join("web-app--conflict");
    let project_dest = ww_dir.join("projects/web-app");
    std::fs::create_dir_all(&project_dest).unwrap();
    std::fs::write(project_dest.join("squat"), "blocker").unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "conflict"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure();

    // And the partial workweave dir is cleaned up (B7 generalised).
    assert!(
        !ww_dir.exists() || !project_dest.join(".git").exists(),
        "no silent static copy should land in place of a worktree"
    );
}

// ============================================================================
// Atomic rollback: orphan worktree pruning on mid-create failure
// ============================================================================

/// Create a workspace with TWO repos in the manifest.  The first repo is real;
/// the second points at a nonexistent path so worktree creation fails for it.
/// After the failed create:
///   1. The workweave directory must not exist.
///   2. The primary's first repo must have no orphan worktree registration
///      (the `.git/worktrees/<name>` entry created when the first worktree
///      succeeded must have been pruned).
fn make_workspace_two_repos(tmp: &std::path::Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo1 = ws.join("github/org/repo1");
    let repo2_path = "/nonexistent/repo2"; // will cause worktree-add to fail

    init_repo_with_commit(&repo1);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    // Manifest: repo1 (real) + repo2 (missing) — forces a partial create.
    let manifest = format!(
        r#"repositories:
  github/org/repo1:
    type: git
    url: file://{repo1}
    version: main
    role: owned
  github/org/repo2:
    type: git
    url: file://{repo2_path}
    version: main
    role: owned
"#,
        repo1 = repo1.display(),
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();
    ws
}

#[test]
fn create_workweave_rollback_prunes_orphan_worktree_registrations() {
    // When create fails after some repos succeed, the successfully-registered
    // worktrees must be pruned from the primary repos' `.git/worktrees/`
    // metadata. Without this, the primary repo still knows about the partial
    // worktree, producing git warnings and blocking re-creates.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_two_repos(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "partial"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure();

    // 1. Workweave directory must not be left on disk.
    let ww_dir = weaveroot.join("web-app--partial");
    assert!(
        !ww_dir.exists(),
        "workweave dir must be removed on rollback, found: {}",
        ww_dir.display()
    );

    // 2. repo1's `.git/worktrees/` must have no entry referencing this workweave.
    //    `git worktree list --porcelain` shows only live registrations; a stale
    //    registration will show up here until pruned.
    let primary_repo1 = ws.join("github/org/repo1");
    let output = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&primary_repo1)
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.contains("web-app--partial"),
        "rollback must prune orphan worktree registration from repo1; \
         git worktree list still shows it:\n{listing}"
    );
}

/// When create fails with multiple manifest repos (some succeeding before the
/// failure), ALL successfully-registered worktrees must be pruned — not just
/// the one that failed.
///
/// This test exercises the "multi-repo partial success" path: repo1 and repo3
/// succeed (registered), repo2 fails. All three repos must end up with clean
/// worktree metadata after rollback.
fn make_workspace_three_repos(tmp: &std::path::Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo1 = ws.join("github/org/repo1");
    let repo3 = ws.join("github/org/repo3");

    init_repo_with_commit(&repo1);
    init_repo_with_commit(&repo3);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    // BTreeMap ordering: repo1 < repo2 < repo3 alphabetically.
    // repo1 succeeds, repo2 fails (missing), repo3 is never attempted
    // (once errors is non-empty, bail fires). Actually, the code collects
    // ALL errors and bails at the end. So repo3 IS attempted but also fails
    // because it doesn't exist... wait, repo3 DOES exist. Let me re-read.
    //
    // The manifest loop continues even on per-repo failure (collects errors),
    // bailing only after the full loop. So:
    //   repo1 → success (registered)
    //   repo2 → failure (missing, skipped)
    //   repo3 → success (registered)
    // Then bail fires, rollback must prune BOTH repo1 and repo3.
    let manifest = format!(
        r#"repositories:
  github/org/repo1:
    type: git
    url: file://{repo1}
    version: main
    role: owned
  github/org/repo2:
    type: git
    url: file:///nonexistent/repo2
    version: main
    role: owned
  github/org/repo3:
    type: git
    url: file://{repo3}
    version: main
    role: owned
"#,
        repo1 = repo1.display(),
        repo3 = repo3.display(),
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();
    ws
}

#[test]
fn create_workweave_rollback_prunes_all_registered_worktrees_not_just_failed() {
    // repo1 and repo3 succeed (both registered); repo2 fails (missing).
    // After rollback, BOTH repo1 and repo3 must have clean worktree metadata.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_three_repos(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "multi-partial"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure();

    let ww_dir = weaveroot.join("web-app--multi-partial");
    assert!(!ww_dir.exists(), "workweave dir must be removed on rollback");

    // Both real repos must have their orphan registrations pruned.
    for repo_name in &["repo1", "repo3"] {
        let repo = ws.join(format!("github/org/{repo_name}"));
        let output = common::git()
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&repo)
            .output()
            .expect("git worktree list should work");
        let listing = String::from_utf8_lossy(&output.stdout);
        assert!(
            !listing.contains("web-app--multi-partial"),
            "rollback must prune orphan registration from {repo_name}; \
             git worktree list still shows it:\n{listing}"
        );
    }
}

/// A clean retry after a failed create must succeed without --force.
/// This is the end-to-end version of the atomicity contract.
#[test]
fn create_workweave_clean_retry_after_failure_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");

    // Set up a real repo at repo1.
    let repo1 = ws.join("github/org/repo1");
    init_repo_with_commit(&repo1);

    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // First create: manifest with a bad repo — fails.
    let bad_manifest = format!(
        r#"repositories:
  github/org/repo1:
    type: git
    url: file://{repo1}
    version: main
    role: owned
  github/org/missing:
    type: git
    url: file:///nonexistent/missing
    version: main
    role: owned
"#,
        repo1 = repo1.display(),
    );
    std::fs::write(project_dir.join("rwv.yaml"), &bad_manifest).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "retry-me"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure();

    // Fix the manifest — only real repos remain.
    let good_manifest = format!(
        r#"repositories:
  github/org/repo1:
    type: git
    url: file://{repo1}
    version: main
    role: owned
"#,
        repo1 = repo1.display(),
    );
    std::fs::write(project_dir.join("rwv.yaml"), &good_manifest).unwrap();

    // Second create (same name, no --force): must succeed because rollback
    // cleaned up all state from the first attempt.
    rwv()
        .args(["workweave", "web-app", "create", "retry-me"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--retry-me");
    assert!(
        ww_dir.exists(),
        "workweave must exist after successful retry at {}",
        ww_dir.display()
    );
    assert!(
        ww_dir.join(".rwv-workweave").exists(),
        "marker must be written on successful retry"
    );
}

/// When the no-marker detection fires (workweave dir exists, marker absent),
/// the diagnostic must name the likely cause — partial create — and recommend
/// --force as the fix. This lets users understand the error without reading
/// source code.
#[test]
fn no_marker_diagnostic_names_partial_create_as_likely_cause() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Simulate a partial workweave: directory exists but no marker file.
    let ww_dir = weaveroot.join("web-app--orphan");
    std::fs::create_dir_all(&ww_dir).unwrap();
    // Deliberately do NOT write .rwv-workweave.

    rwv()
        .args(["workweave", "web-app", "create", "orphan"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("partially created")
                .or(predicate::str::contains("previous failed")),
        )
        .stderr(predicate::str::contains("--force"));
}

// ============================================================================
// --force prunes orphan worktree refs from prior partial creates
// ============================================================================

/// `rwv workweave create --force` must succeed even when the primary repo
/// already has a stale `.git/worktrees/<name>` registration pointing at the
/// (now-absent) workweave worktree path.
///
/// Scenario:
///   1. Workspace with one real repo.
///   2. Workweave directory exists on disk but has NO `.rwv-workweave` marker
///      (simulates a partial create that survived after interruption).
///   3. Primary repo has an orphan `.git/worktrees/<name>` registration pointing
///      at a path inside the workweave dir — created by a prior `git worktree add`
///      whose directory was subsequently removed.
///   4. `rwv workweave <proj> create <ww> --force` must:
///      a. Prune the orphan registration before re-creating.
///      b. Succeed — exit 0 and produce a valid workweave with marker.
///      c. Leave no orphan registrations in the primary repo.
#[test]
fn create_workweave_force_prunes_orphan_worktree_registrations() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // ── Step 1: manufacture the "stale partial create" state ──────────────
    // Create the workweave dir without a marker.
    let ww_dir = weaveroot.join("web-app--stale-ww");
    let wt_dest = ww_dir.join("github/org/repo");
    std::fs::create_dir_all(&wt_dest).unwrap();

    // Run `git worktree add` in the primary repo, pointing into the workweave
    // dir.  This writes a `.git/worktrees/<name>` registration in the primary
    // repo.  Use a fresh branch name so it does not conflict with main.
    let primary_repo = ws.join("github/org/repo");
    git(
        &[
            "worktree",
            "add",
            "--force",
            wt_dest.to_str().unwrap(),
            "-b",
            "web-app--stale-ww/main",
        ],
        &primary_repo,
    );

    // Remove the worktree directory to create an orphan registration —
    // the `.git/worktrees/<name>` entry now points at a missing path.
    std::fs::remove_dir_all(&wt_dest).unwrap();

    // Verify that the primary repo sees the stale registration before --force.
    let before = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&primary_repo)
        .output()
        .expect("git worktree list should work");
    let before_listing = String::from_utf8_lossy(&before.stdout);
    assert!(
        before_listing.contains("stale-ww"),
        "precondition: orphan registration must be present before --force; \
         got:\n{before_listing}"
    );

    // ── Step 2: --force create must succeed ───────────────────────────────
    rwv()
        .args(["workweave", "web-app", "create", "stale-ww", "--force"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Workweave directory must exist with a marker.
    assert!(
        ww_dir.exists(),
        "workweave dir must exist after --force create"
    );
    assert!(
        ww_dir.join(".rwv-workweave").exists(),
        "--force create must write the .rwv-workweave marker"
    );

    // ── Step 3: primary repo must have no leftover orphan registrations ───
    let after = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&primary_repo)
        .output()
        .expect("git worktree list should work");
    let after_listing = String::from_utf8_lossy(&after.stdout);

    // The only worktree entry remaining should be the newly-created one inside
    // the fresh workweave — not a duplicate stale entry.
    let stale_count = after_listing
        .lines()
        .filter(|l| l.starts_with("worktree ") && l.contains("stale-ww") && !l.contains(&ww_dir.join("github/org/repo").to_string_lossy().as_ref()))
        .count();
    assert_eq!(
        stale_count,
        0,
        "--force must prune orphan worktree registrations; \
         git worktree list after:\n{after_listing}"
    );
}

#[test]
fn claude_hook_no_project_arg_needed() {
    // --claude-hook should work without a project argument (derived from .rwv-active).
    let assert = rwv()
        .args(["workweave", "--claude-hook"])
        .write_stdin(r#"{"hook_event_name":"WorktreeCreate","cwd":"/nonexistent/path"}"#)
        .assert();
    // It will fail because the path doesn't exist — but the important thing is
    // that it doesn't fail with a clap "required argument" error.
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        !stderr.contains("required arguments"),
        "should not require project arg with --claude-hook, got: {stderr}"
    );
}

// ============================================================================
// workweave delete dirty-worktree safety
// ============================================================================

#[test]
fn workweave_delete_refuses_dirty_manifest_repo() {
    // Create a workweave, dirty up a manifest-repo worktree, verify that
    // `rwv workweave delete` (no --force) refuses and names the dirty repo.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "dirty"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Dirty up a tracked file in the manifest-repo worktree.
    let repo_wt = weaveroot.join("web-app--dirty/github/org/repo");
    std::fs::write(repo_wt.join("README"), "DIRTY EDIT\n").unwrap();

    // Plain delete: must refuse, must mention --force, must name the dirty repo.
    rwv()
        .args(["workweave", "web-app", "delete", "dirty"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--force").and(predicate::str::contains("github/org/repo")),
        );

    // Workweave directory must still exist after the refused delete.
    let ww_dir = weaveroot.join("web-app--dirty");
    assert!(
        ww_dir.exists(),
        "refused delete must leave workweave intact"
    );
}

#[test]
fn workweave_delete_force_proceeds_on_dirty() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "del-force"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let repo_wt = weaveroot.join("web-app--del-force/github/org/repo");
    std::fs::write(repo_wt.join("README"), "DIRTY\n").unwrap();
    // And an untracked file (to cover that branch of `git status --porcelain`).
    std::fs::write(repo_wt.join("LOCAL_TODO"), "todo\n").unwrap();

    rwv()
        .args(["workweave", "web-app", "delete", "del-force", "--force"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--del-force");
    assert!(!ww_dir.exists(), "--force must remove the workweave");
}

#[test]
fn workweave_delete_clean_succeeds_without_force() {
    // Make_workspace's project dir is NOT a git repo, so activation can't
    // generate a worktree there. The single manifest repo is clean after a
    // fresh create, so the dirty check should pass and delete should
    // succeed without --force.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "clean"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args(["workweave", "web-app", "delete", "clean"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--clean");
    assert!(!ww_dir.exists(), "clean delete should remove the workweave");
}

// ============================================================================
// rwv-c7h: workweave create reads working-tree rwv.yaml (uncommitted edits)
// ============================================================================

#[test]
fn workweave_create_picks_up_uncommitted_rwv_yaml() {
    // make_workspace_with_project_repo commits the manifest. We then edit
    // the working-tree rwv.yaml WITHOUT committing, create a workweave, and
    // verify the workweave's project worktree sees the edited (working-tree)
    // version — not the last committed one.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "uncommit-test");

    // Append a comment to rwv.yaml in the primary's working tree, without
    // committing. The committed version still says no comment.
    let primary_manifest = ws.join("projects/uncommit-test/rwv.yaml");
    let original = std::fs::read_to_string(&primary_manifest).unwrap();
    let edited = format!("{original}# UNCOMMITTED-MARKER\n");
    std::fs::write(&primary_manifest, &edited).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let assert = rwv()
        .args(["workweave", "uncommit-test", "create", "ww-uncommit"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // The CLI must emit a warning about dirty state (so the operator
    // notices the working tree was captured).
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("uncommitted") || stderr.contains("working-tree"),
        "expected dirty-state warning, got stderr: {stderr}"
    );

    // The workweave's project worktree must have the UNCOMMITTED marker.
    let ww_manifest = weaveroot.join("uncommit-test--ww-uncommit/projects/uncommit-test/rwv.yaml");
    let ww_content = std::fs::read_to_string(&ww_manifest).unwrap();
    assert!(
        ww_content.contains("UNCOMMITTED-MARKER"),
        "workweave's rwv.yaml should reflect the primary's uncommitted edit, got:\n{ww_content}"
    );
}

#[test]
fn workweave_create_with_clean_committed_manifest_emits_no_dirty_warning() {
    // Sanity counterpart to the above: a clean workspace must NOT trigger
    // the dirty-state warning, so users don't get noise on every create.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "clean-test");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let assert = rwv()
        .args(["workweave", "clean-test", "create", "ww-clean"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        !stderr.contains("uncommitted") && !stderr.contains("dirty state"),
        "clean workspace must not warn about uncommitted state, got stderr: {stderr}"
    );
}

// ============================================================================
// Workweave parent tracking + bare sync follows parent
// ============================================================================

#[test]
fn workweave_create_records_primary_as_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "parented"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let marker = weaveroot.join("web-app--parented/.rwv-workweave");
    let content = std::fs::read_to_string(&marker).unwrap();
    assert!(
        content.contains("parent:"),
        "marker must include `parent:` field, got:\n{content}"
    );
    // For a workweave forked from primary, parent should resolve to the
    // canonicalised primary path.
    let ws_canonical = ws.canonicalize().unwrap();
    assert!(
        content.contains(ws_canonical.to_str().unwrap()),
        "parent must equal primary path {} for primary-forked workweave, got:\n{content}",
        ws_canonical.display()
    );
}

#[test]
fn workweave_forked_from_other_workweave_records_that_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Step 1: create ww1 forked from primary.
    rwv()
        .args(["workweave", "web-app", "create", "ww1"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Step 2: from inside ww1, create ww2 — should fork from ww1.
    let ww1 = weaveroot.join("web-app--ww1");
    rwv()
        .args(["workweave", "web-app", "create", "ww2"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ww1)
        .assert()
        .success();

    let ww2_marker = weaveroot.join("web-app--ww2/.rwv-workweave");
    let content = std::fs::read_to_string(&ww2_marker).unwrap();
    let ww1_canonical = ww1.canonicalize().unwrap();
    assert!(
        content.contains(ww1_canonical.to_str().unwrap()),
        "ww2's parent must be ww1's path (forked from ww1's CWD), got:\n{content}"
    );
}

#[test]
fn bare_sync_outside_workweave_errors_clearly() {
    // make_workspace_with_project_repo gives us a primary weave with a git
    // project repo. Running `rwv sync` (no source) from primary should
    // refuse with a helpful message (parent-tracking only applies inside a
    // workweave).
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "p");

    rwv()
        .args(["sync"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("workweave").or(predicate::str::contains("source")));
}

// ============================================================================
// Pre-flight HEAD check
// ============================================================================

/// Build a workspace where the project directory is a git-init'd repo with
/// NO commits yet. This mirrors the state after `rwv init <project>` before
/// any activation artifacts are committed.
///
/// Layout:
///   {tmp}/ws/                          -- workspace root
///   {tmp}/ws/github/                   -- registry marker
///   {tmp}/ws/projects/{project}/       -- git-init'd but NO commits
///   {tmp}/ws/github/org/repo/          -- manifest repo (has commits)
fn make_workspace_with_uncommitted_project(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();
    // git init — no commit.
    let status = common::git()
        .args(["init", "--initial-branch=main"])
        .current_dir(&project_dir)
        .status()
        .expect("git init should work");
    assert!(status.success(), "git init in project dir should succeed");
    common::git()
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&project_dir)
        .status()
        .ok();
    common::git()
        .args(["config", "user.name", "Test"])
        .current_dir(&project_dir)
        .status()
        .ok();

    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: owned
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();
    // NOTE: rwv.yaml is NOT committed — no commit exists yet.

    ws
}

/// Build a workspace where one manifest repo has been git-init'd but has no
/// commits yet. The project repo is fine; this exercises the manifest-repo
/// preflight path.
fn make_workspace_with_uncommitted_manifest_repo(
    tmp: &Path,
    project: &str,
) -> std::path::PathBuf {
    let ws = tmp.join("ws");

    // Good manifest repo.
    let good_repo = ws.join("github/org/good");
    init_repo_with_commit(&good_repo);

    // Bad manifest repo — git-init'd, no commits.
    let bad_repo = ws.join("github/org/empty");
    std::fs::create_dir_all(&bad_repo).unwrap();
    let status = common::git()
        .args(["init", "--initial-branch=main"])
        .current_dir(&bad_repo)
        .status()
        .expect("git init should work");
    assert!(status.success());
    common::git()
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&bad_repo)
        .status()
        .ok();
    common::git()
        .args(["config", "user.name", "Test"])
        .current_dir(&bad_repo)
        .status()
        .ok();

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  github/org/good:
    type: git
    url: file://{good}
    version: main
    role: owned
  github/org/empty:
    type: git
    url: file://{bad}
    version: main
    role: owned
"#,
        good = good_repo.display(),
        bad = bad_repo.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    ws
}

/// Core assertion: create fails, error names the path, no workweave dir left.
fn assert_preflight_fails_with_actionable_message(
    ws: &std::path::Path,
    weaveroot: &std::path::Path,
    project: &str,
    ww_name: &str,
    expected_path_fragment: &str,
) {
    let assert = rwv()
        .args(["workweave", project, "create", ww_name])
        .env("RWV_WORKWEAVE_DIR", weaveroot)
        .current_dir(ws)
        .assert()
        .failure();

    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Error must name the specific path.
    assert!(
        stderr.contains(expected_path_fragment),
        "preflight error should name the path '{expected_path_fragment}', got:\n{stderr}"
    );

    // Error must mention "no commits" or "commit" so the user knows what to do.
    assert!(
        stderr.contains("commit"),
        "preflight error should mention 'commit' as the fix, got:\n{stderr}"
    );

    // No partial workweave directory should be left on disk.
    let ww_dir = weaveroot.join(format!("{project}--{ww_name}"));
    assert!(
        !ww_dir.exists(),
        "preflight failure should leave no partial workweave directory at {}",
        ww_dir.display()
    );
}

#[test]
fn create_workweave_fails_actionably_when_project_repo_has_no_commits() {
    // Regression: project git-init'd but not committed. The pre-flight
    // check must fire before any disk mutation and produce an error that
    // names projects/<project> and tells the user to commit.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_uncommitted_project(tmp.path(), "fresh-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    assert_preflight_fails_with_actionable_message(
        &ws,
        &weaveroot,
        "fresh-project",
        "preflight-check",
        // Error must name the project.
        "fresh-project",
    );
}

#[test]
fn create_workweave_preflight_error_names_project_path() {
    // Verify the exact shape of the error message matches the bead spec:
    //   "project <name> has no commits yet — run "git -C projects/<name> commit" ..."
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_uncommitted_project(tmp.path(), "myproj");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let output = rwv()
        .args(["workweave", "myproj", "create", "check-msg"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();

    let stderr = String::from_utf8_lossy(&output);

    // Must name the project by name.
    assert!(
        stderr.contains("myproj"),
        "error must name the project 'myproj', got:\n{stderr}"
    );

    // Must tell the user to commit.
    assert!(
        stderr.contains("commit"),
        "error must suggest running a commit, got:\n{stderr}"
    );

    // Must not leave a workweave directory.
    let ww_dir = weaveroot.join("myproj--check-msg");
    assert!(
        !ww_dir.exists(),
        "no workweave directory should exist after preflight failure"
    );
}

#[test]
fn create_workweave_fails_actionably_when_manifest_repo_has_no_commits() {
    // Verify that the preflight check also fires for manifest repos (not just
    // the project repo). The error must name the specific repo path.
    let tmp = tempfile::tempdir().unwrap();
    let ws =
        make_workspace_with_uncommitted_manifest_repo(tmp.path(), "multiproj");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    assert_preflight_fails_with_actionable_message(
        &ws,
        &weaveroot,
        "multiproj",
        "manifest-preflight",
        // Error must name the specific repo path.
        "github/org/empty",
    );
}

#[test]
fn create_workweave_succeeds_when_all_repos_have_commits() {
    // Positive control: a well-formed workspace (all repos have commits) should
    // sail through the preflight check without error.
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "good-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "good-project", "create", "preflight-ok"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("good-project--preflight-ok");
    assert!(
        ww_dir.exists(),
        "workweave should be created successfully when all repos have commits"
    );
}
