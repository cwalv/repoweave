//! E2E tests for `rwv workweave`.
//!
//! These tests exercise the CLI binary via `assert_cmd`. Tests that require
//! the workweave command to be fully implemented are marked `#[ignore]` where
//! appropriate.

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
    role: primary
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
    role: primary
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
    let ww_dir = weaveroot.join("ws--hotfix");
    assert!(
        ww_dir.exists(),
        "workweave directory ws--hotfix should exist at {}",
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
    // named {workweave-name}/{original-branch}, i.e. "hotfix/main".
    let weave_repo = weaveroot.join("ws--hotfix/github/org/repo");
    assert!(
        weave_repo.exists(),
        "workweave should contain worktree at github/org/repo"
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

    let ww_dir = weaveroot.join("ws--feat-x");
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
    let project_wt = weaveroot.join("ws--feat/projects/my-project");
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

    let ww_dir = weaveroot.join("ws--to-del");
    assert!(ww_dir.exists(), "workweave should exist before deletion");

    // Delete it.
    rwv()
        .args(["workweave", "my-project", "delete", "to-del"])
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
    let output = process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&primary_project)
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.contains("ws--to-del"),
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
    role: primary
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

    let ww_dir = weaveroot.join("ws--copy-test");
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
    role: primary
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

    let ww_dir = weaveroot.join("ws--link-test");
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

    let ww_dir = weaveroot.join("ws--marker-test");
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

    let ww_dir = weaveroot.join("ws--active-test");
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

    let ww_dir = weaveroot.join("ws--to-delete");
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
// Workweave sync -- `rwv workweave PROJECT --sync`
// ============================================================================

#[test]
fn workweave_sync_reconciles_with_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create a workweave.
    rwv()
        .args(["workweave", "web-app", "create", "sync-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
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

    // Sync should add the new repo's worktree to the workweave.
    rwv()
        .args(["workweave", "web-app", "sync", "sync-test"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let weave_repo2 = weaveroot.join("ws--sync-test/github/org/repo2");
    assert!(
        weave_repo2.exists(),
        "sync should add newly-listed repo worktree to the workweave"
    );
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
    let ww_in_custom = custom_root.join("ws--override-test");
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

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "full-stack", "create", "multi"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("ws--multi");
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
        stdout.ends_with("ws--hook-test"),
        "--hook-mode stdout should end with the workweave dir name 'ws--hook-test', got: {stdout}"
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

    let ww_dir = weaveroot.join("ws--rt");

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
    rwv()
        .args(["workweave", "round-trip-project", "delete", "rt"])
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
    let output = process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(ws.join("github/org/repo"))
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.contains("ws--rt"),
        "repo worktree should be cleaned up from primary, got: {listing}"
    );

    // Verify project worktree is cleaned up from primary.
    let output = process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(ws.join("projects/round-trip-project"))
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        !listing.contains("ws--rt"),
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
    role: primary
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
    let ww_dir = weaveroot.join("ws--eco");
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

    let ww_dir = weaveroot.join("ws--resolve-test");

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

    let ww_dir = weaveroot.join("ws--feat_my-feature_v2");
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

    let ww_dir = weaveroot.join("ws--no-active");
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
    let output = process::Command::new("git")
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

    // Create a workweave — this creates ephemeral branch "cleanup/main" in the repo.
    rwv()
        .args(["workweave", "web-app", "create", "cleanup"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let primary_repo = ws.join("github/org/repo");

    // Confirm the ephemeral branch exists before deletion.
    let before = branches_with_prefix(&primary_repo, "cleanup");
    assert!(
        !before.is_empty(),
        "ephemeral branch cleanup/main should exist before delete, got: {before:?}"
    );

    // Delete the workweave.
    rwv()
        .args(["workweave", "web-app", "delete", "cleanup"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // The ephemeral branch should be gone.
    let after = branches_with_prefix(&primary_repo, "cleanup");
    assert!(
        after.is_empty(),
        "delete_workweave should remove ephemeral branches with prefix 'cleanup/', remaining: {after:?}"
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
    let head = process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&primary_repo)
        .output()
        .expect("git rev-parse HEAD");
    let head_sha = String::from_utf8_lossy(&head.stdout).trim().to_string();

    let status = process::Command::new("git")
        .args(["branch", "stale-test/main", &head_sha])
        .current_dir(&primary_repo)
        .status()
        .expect("git branch stale-test/main");
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
    let ww_dir = weaveroot.join("ws--stale-test");
    assert!(
        ww_dir.join("github/org/repo").exists(),
        "workweave should be created successfully even with pre-existing stale branch"
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

    let ww_dir = weaveroot.join("ws--to-remove");
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
