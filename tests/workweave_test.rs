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
///   {tmp}/ws/projects/{project}/rwv.toml
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
        r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    ws
}

/// Create a workspace where the project directory is itself a git repo.
///
/// Layout:
///   {tmp}/ws/                          -- workspace root
///   {tmp}/ws/github/                   -- registry marker
///   {tmp}/ws/projects/{project}/       -- git repo with commit + rwv.toml
///   {tmp}/ws/github/org/repo/          -- manifest repo
fn make_workspace_with_project_repo(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    init_repo_with_commit(&project_dir);

    let manifest = format!(
        r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    git(&["add", "rwv.toml"], &project_dir);
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // The default container, so the workweave lands here with no override.
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "hotfix"])
        .current_dir(&ws)
        .assert()
        .success();

    // Workweave directory should be under the default container.
    let ww_dir = weaveroot.join("web-app--hotfix");
    assert!(
        ww_dir.exists(),
        "workweave directory web-app--hotfix should exist at {}",
        ww_dir.display()
    );
}

#[test]
fn workweave_create_worktrees_on_ephemeral_branches() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "hotfix"])
        .current_dir(&ws)
        .assert()
        .success();

    // The worktree in the workweave should be on an ephemeral branch
    // named {project}--{workweave-name}/{original-branch}, i.e.
    // "web-app--hotfix".
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
    // Flat (branch-model.md §3.5): `{project}--{workweave}`, no third
    // component. The name is minted from two inputs and nothing observed
    // feeds in, so the source repo's current branch cannot appear here.
    assert_eq!(
        branch, "web-app--hotfix",
        "worktree should be on ephemeral branch web-app--hotfix, got: {branch}"
    );
}

#[test]
fn workweave_create_mirrors_primary_layout() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat-x"])
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
// Workweave create -- project repo worktree
// ============================================================================

#[test]
fn create_workweave_includes_project_repo() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "my-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "my-project", "create", "feat"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "my-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create first.
    rwv()
        .args(["workweave", "my-project", "create", "to-del"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("my-project--to-del");
    assert!(ww_dir.exists(), "workweave should exist before deletion");

    // Delete it. Pass --discard-uncommitted: activation writes generated files
    // into the workweave's project worktree (workspace config, ecosystem
    // outputs) that the dirty check would otherwise treat as untracked
    // changes. This test is verifying worktree cleanup, not dirty-check
    // semantics, so the waiver is incidental.
    rwv()
        .args([
            "workweave",
            "my-project",
            "delete",
            "to-del",
            "--discard-uncommitted",
        ])
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
// Workweave create -- artifact processing
// ============================================================================

#[test]
fn create_workweave_processes_copy_entries() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // Place a .env file in the workspace root.
    std::fs::write(ws.join(".env"), "SECRET=hunter2\n").unwrap();

    // Update the manifest to include workweave.copy.
    let project_dir = ws.join("projects/web-app");
    let manifest = format!(
        r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"

[workweave]
copy = [".env"]
"#,
        repo = ws.join("github/org/repo").display()
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "copy-test"])
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

/// The link source here is a directory, which is the only place the suite
/// exercises a directory-kind link: the Unix call takes no kind, so nothing
/// else can tell the two apart.
#[test]
fn create_workweave_processes_link_entries() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // Create a shared directory to link.
    let shared_dir = ws.join("shared-state");
    std::fs::create_dir_all(&shared_dir).unwrap();
    std::fs::write(shared_dir.join("data.db"), "db content").unwrap();

    // Update manifest with workweave.link.
    let project_dir = ws.join("projects/web-app");
    let manifest = format!(
        r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"

[workweave]
link = ["shared-state"]
"#,
        repo = ws.join("github/org/repo").display()
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "link-test"])
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
// Workweave create -- the marker, and the absence of .rwv-active
// ============================================================================

#[test]
fn create_workweave_writes_marker() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "marker-test"])
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

/// `create` writes the marker and NOT `.rwv-active`.
///
/// This assertion is the inverse of what it used to be. `create` did write a
/// pointer beside the marker, and both named the same project — two copies of
/// the workweave's identity with nothing keeping them in agreement. The two
/// files are now mutually exclusive: a primary root carries the pointer, a
/// workweave root carries the marker, and they occupy one tier of the
/// resolution chain rather than two ranked ones. See
/// `weave_root_identity_test` for the full contract, including the `rwv
/// doctor` arm that clears a pointer left behind by an older build.
#[test]
fn create_workweave_writes_the_marker_and_not_rwv_active() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "active-test"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--active-test");

    let marker = ww_dir.join(".rwv-workweave");
    assert!(
        marker.exists(),
        ".rwv-workweave should exist in workweave at {}",
        marker.display()
    );
    let content = std::fs::read_to_string(&marker).unwrap();
    assert!(
        content.contains("web-app"),
        ".rwv-workweave should name project 'web-app', got: {content}"
    );

    assert!(
        !ww_dir.join(".rwv-active").exists(),
        ".rwv-active must NOT be written into a workweave root: the marker \
         already names the project, and selection is a primary-only concept"
    );
}

// ============================================================================
// Workweave delete -- `rwv workweave PROJECT --delete`
// ============================================================================

#[test]
fn workweave_delete_removes_directory_and_worktrees() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create a workweave first.
    rwv()
        .args(["workweave", "web-app", "create", "to-delete"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--to-delete");
    assert!(ww_dir.exists(), "workweave should exist before deletion");

    // Delete it.
    rwv()
        .args(["workweave", "web-app", "delete", "to-delete"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create two workweaves.
    rwv()
        .args(["workweave", "web-app", "create", "alpha"])
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["workweave", "web-app", "create", "beta"])
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args(["workweave", "web-app", "list"])
        .current_dir(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains("alpha").and(predicate::str::contains("beta")));
}

#[test]
fn workweave_list_empty_when_no_workweaves() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "list"])
        .current_dir(&ws)
        .assert()
        .success();
    // No assertion on content — just that it succeeds with no workweaves.
}

// ============================================================================
// RWV_WORKWEAVE_DIR is inert
// ============================================================================

#[test]
fn workweave_ignores_weaveroot_env() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let decoy_root = tmp.path().join("custom-weaves");
    std::fs::create_dir_all(&decoy_root).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "override-test"])
        .env("RWV_WORKWEAVE_DIR", &decoy_root)
        .current_dir(&ws)
        .assert()
        .success();

    // The workweave must land in the compiled-in default container, not
    // the directory the retired env var names.
    assert!(
        decoy_root.read_dir().unwrap().next().is_none(),
        "RWV_WORKWEAVE_DIR must not steer placement"
    );
    let ww_default = tmp.path().join(".workweaves/web-app--override-test");
    assert!(
        ww_default.exists(),
        "workweave should be created under the default container at {}",
        ww_default.display()
    );
}

// ============================================================================
// Multi-repo workweave structure
// ============================================================================

#[test]
fn workweave_with_multiple_repos_creates_all_worktrees() {
    let tmp = common::tempdir().unwrap();
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
        r#"[repositories."github/org/server"]
type = "git"
url = "file://{server}"
version = "main"
role = "owned"

[repositories."github/org/client"]
type = "git"
url = "file://{client}"
version = "main"
role = "fork"
"#,
        server = repo1.display(),
        client = repo2.display()
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "full-stack", "create", "multi"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let output = rwv()
        .args(["workweave", "web-app", "--hook-mode", "create", "hook-test"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let output = rwv()
        .args(["workweave", "web-app", "--hook-mode", "create", "abs-test"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let output = rwv()
        .args(["workweave", "web-app", "create", "normal-test"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "round-trip-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // --- Create ---
    rwv()
        .args(["workweave", "round-trip-project", "create", "rt"])
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

    // Verify the marker — and that it is the workweave root's ONLY identity
    // file. The two are mutually exclusive; see
    // `create_workweave_writes_the_marker_and_not_rwv_active`.
    assert!(
        ww_dir.join(".rwv-workweave").exists(),
        ".rwv-workweave should exist"
    );
    assert!(
        !ww_dir.join(".rwv-active").exists(),
        ".rwv-active should NOT exist in a workweave root"
    );

    // --- Delete ---
    // Pass --discard-uncommitted: activation generates files in the project
    // worktree (workspace config, ecosystem outputs) that count as untracked
    // changes under the dirty check. The round-trip test isn't about dirty
    // semantics; the waiver is incidental to making delete work after
    // the create-and-activate cycle.
    rwv()
        .args([
            "workweave",
            "round-trip-project",
            "delete",
            "rt",
            "--discard-uncommitted",
        ])
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
        r#"[repositories."github/org/cargo-crate"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    // Trigger-model split: pre-author the integration content
    // in the primary workspace so workweave-create (a context verb)
    // surfaces it via symlinks. Real-world equivalent: an intent verb
    // (rwv add) ran earlier and committed both rwv.toml and Cargo.toml.
    let ctx = repoweave::workspace::WorkspaceContext::resolve(&ws, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        project,
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent-mode activation should pre-author Cargo.toml in project dir");
    ws
}

#[test]
fn create_workweave_generates_ecosystem_files() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_cargo_repo(tmp.path(), "cargo-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "cargo-project", "create", "eco"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create a workweave.
    rwv()
        .args(["workweave", "web-app", "create", "resolve-test"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--resolve-test");

    // Run `rwv resolve` from inside the workweave directory.
    let output = rwv()
        .arg("resolve")
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat_my-feature_v2"])
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Attempt to delete a workweave that was never created.
    let result = rwv()
        .args(["workweave", "web-app", "delete", "ghost"])
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
// Ephemeral branch cleanup
// ============================================================================

/// Helper: list every local branch name in a git repo.
fn branch_names(repo: &Path) -> Vec<String> {
    let output = common::git()
        .args([
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/heads/",
        ])
        .current_dir(repo)
        .output()
        .expect("git for-each-ref should work");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create a workweave — this creates ephemeral branch "web-app--cleanup" in the repo.
    rwv()
        .args(["workweave", "web-app", "create", "cleanup"])
        .current_dir(&ws)
        .assert()
        .success();

    let primary_repo = ws.join("github/org/repo");

    // Confirm the ephemeral branch exists before deletion. Flat (§3.5), so it
    // is the exact name — not a `web-app--cleanup/*` sub-namespace.
    let before = branch_names(&primary_repo);
    assert!(
        before.iter().any(|b| b == "web-app--cleanup"),
        "ephemeral branch web-app--cleanup should exist before delete, got: {before:?}"
    );

    // Delete the workweave.
    rwv()
        .args(["workweave", "web-app", "delete", "cleanup"])
        .current_dir(&ws)
        .assert()
        .success();

    // The ephemeral branch should be gone — destroyed over the RECORDED set,
    // with a Merged warrant against primary's tip (R2 + R3).
    let after = branch_names(&primary_repo);
    assert!(
        !after.iter().any(|b| b == "web-app--cleanup"),
        "delete_workweave should destroy the recorded ephemeral branch \
         'web-app--cleanup', remaining: {after:?}"
    );
}

/// R2 inverted: a branch that merely *looks* like rwv's is not rwv's.
///
/// This is the §2.1 `[S]` scenario the shipped code got backwards. It ran
/// `git branch -D` on "already exists" and retried, so a `web-app--stale-test/main`
/// standing in the way was destroyed with no `--force` and nothing printed —
/// even when it carried a commit reachable from nowhere else. Ownership is by
/// **record**: rwv holds no receipt for a branch it did not create, so the
/// create refuses and the branch survives with its commit intact.
///
/// Break the guard and this fails: restore the force-delete retry and the
/// unique commit is gone.
#[test]
fn create_refuses_and_preserves_a_branch_it_does_not_own() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // A branch in the workweave's namespace that rwv never created, carrying a
    // commit that exists nowhere else.
    let primary_repo = ws.join("github/org/repo");
    git(&["branch", "web-app--stale-test/main"], &primary_repo);
    git(&["checkout", "web-app--stale-test/main"], &primary_repo);
    std::fs::write(primary_repo.join("hand-made.txt"), "operator work").unwrap();
    git(&["add", "-A"], &primary_repo);
    git(
        &["commit", "-m", "work only this branch can reach"],
        &primary_repo,
    );
    let unique_sha = {
        let out = common::git()
            .args(["rev-parse", "web-app--stale-test/main"])
            .current_dir(&primary_repo)
            .output()
            .expect("git rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["checkout", "main"], &primary_repo);

    // The create must refuse rather than clear the name.
    let assert = rwv()
        .args(["workweave", "web-app", "create", "stale-test"])
        .current_dir(&ws)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("web-app--stale-test/main"),
        "the refusal must name the branch that is in the way; got:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv doctor --fix"),
        "the refusal must name the command that migrates rwv's own legacy refs; \
         got:\n{stderr}"
    );

    // The branch and its unique commit survive untouched.
    let branches = branch_names(&primary_repo);
    assert!(
        branches.iter().any(|b| b == "web-app--stale-test/main"),
        "a branch rwv holds no receipt for must survive the create, got: {branches:?}"
    );
    let after = common::git()
        .args(["rev-parse", "web-app--stale-test/main"])
        .current_dir(&primary_repo)
        .output()
        .expect("git rev-parse");
    assert_eq!(
        String::from_utf8_lossy(&after.stdout).trim(),
        unique_sha,
        "the branch must still point at the operator's commit"
    );
}

/// The stale-leftover case the shipped justification only *claimed*: a ref rwv
/// recorded creating, still at exactly the recorded tip, left behind by a
/// create that did not finish. That one is destroyed and recreated, because
/// `DeletionWarrant::unmoved` runs the comparison rather than asserting it.
#[test]
fn create_reuses_its_own_unmoved_leftover() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "leftover"])
        .current_dir(&ws)
        .assert()
        .success();

    // Simulate a create that died after the ref write: drop the directory and
    // the placement entry by hand, leaving the ref AND its receipt standing.
    let ww_dir = weaveroot.join("web-app--leftover");
    let primary_repo = ws.join("github/org/repo");
    git(
        &[
            "worktree",
            "remove",
            "--force",
            ww_dir.join("github/org/repo").to_str().unwrap(),
        ],
        &primary_repo,
    );
    std::fs::remove_dir_all(&ww_dir).unwrap();
    let index = ws.join("projects/web-app/.rwv-workweave-index");
    let mut idx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index).unwrap()).unwrap();
    idx["workweaves"]
        .as_object_mut()
        .unwrap()
        .remove("leftover");
    std::fs::write(&index, serde_json::to_string_pretty(&idx).unwrap()).unwrap();
    assert!(
        branch_names(&primary_repo)
            .iter()
            .any(|b| b == "web-app--leftover"),
        "precondition: the recorded ref must still be standing"
    );

    // Recreating the same workweave proceeds: receipt present, tip unmoved.
    rwv()
        .args(["workweave", "web-app", "create", "leftover"])
        .current_dir(&ws)
        .assert()
        .success();
    assert!(
        ww_dir.join("github/org/repo").exists(),
        "the workweave should be created over its own unmoved leftover"
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create the rig workweave from primary CWD (today's behavior).
    rwv()
        .args(["workweave", "web-app", "create", "rig"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "rig"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let json = worktree_create_json(&ws, "feat/my-branch", "sess-001");

    let output = rwv()
        .args(["workweave", "--claude-hook"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let json = worktree_create_json(&ws, "null", "my-fallback-session");

    let output = rwv()
        .args(["workweave", "--claude-hook"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // First create a workweave the normal way.
    rwv()
        .args(["workweave", "web-app", "create", "to-remove"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--to-remove");
    assert!(ww_dir.exists(), "workweave should exist before removal");

    // Now delete it via --claude-hook WorktreeRemove.
    let json = worktree_remove_json(&ww_dir);

    rwv()
        .args(["workweave", "--claude-hook"])
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
    // primary basename = "ws", project = "web-app". The directory must be
    // `web-app--scratch`, not `ws--scratch`.
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "scratch"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "del-me"])
        .current_dir(&ws)
        .assert()
        .success();
    let ww_dir = weaveroot.join("web-app--del-me");
    assert!(ww_dir.exists());

    rwv()
        .args(["workweave", "web-app", "delete", "del-me"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(!ww_dir.exists(), "workweave dir should be removed");
}

/// `list_workweaves` returns workweaves scoped by project; workweaves of a
/// different project under the same primary are not included.
#[test]
fn list_workweaves_is_scoped_by_project() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "project-a");
    // Add a second project.
    let project_b_dir = ws.join("projects/project-b");
    std::fs::create_dir_all(&project_b_dir).unwrap();
    let repo_path = ws.join("github/org/repo");
    let manifest_b = format!(
        r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_b_dir.join("rwv.toml"), manifest_b).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "project-a", "create", "a-only"])
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["workweave", "project-b", "create", "b-only"])
        .current_dir(&ws)
        .assert()
        .success();

    let out = rwv()
        .args(["workweave", "project-a", "list"])
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

/// Old-form on-disk workweaves without a registry entry are NOT visible in
/// `rwv workweave list` (list is registry-backed since the addressing
/// redesign). Doctor's `unregistered-workweave` finding + `--fix` adoption
/// is the migration surface — silent auto-adoption in read paths is
/// deliberately not provided.
#[test]
fn list_omits_unregistered_workweave_and_doctor_can_adopt_it() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Hand-craft an on-disk workweave with a valid marker but no registry
    // entry (the pre-registry / bootstrap case).
    // The directory name must be `<project>--<name>` for parse to succeed.
    let legacy = weaveroot.join("web-app--from-old");
    std::fs::create_dir_all(&legacy).unwrap();
    let ws_canon = ws.canonicalize().unwrap().display().to_string();
    let marker = format!(
        "{{\"primary\":\"{p}\",\"project\":\"web-app\",\"parent\":\"{p}\"}}",
        p = ws_canon
    );
    std::fs::write(legacy.join(".rwv-workweave"), marker).unwrap();

    // List omits it: no registry entry → not visible.
    let stdout = String::from_utf8(
        rwv()
            .args(["workweave", "web-app", "list"])
            .current_dir(&ws)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        !stdout.contains("from-old"),
        "list must NOT silently surface unregistered workweaves; got:\n{stdout}"
    );

    // Doctor --fix adopts it into the registry.
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .assert()
        .stdout(predicates::str::contains("adopted workweave `from-old`"));

    // After adoption, list surfaces the workweave.
    let stdout2 = String::from_utf8(
        rwv()
            .args(["workweave", "web-app", "list"])
            .current_dir(&ws)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(
        stdout2.contains("from-old"),
        "post-adopt list should surface the workweave; got:\n{stdout2}"
    );
}

// ============================================================================
// §3.5 — nothing observed feeds the ephemeral name
// ============================================================================

/// A detached source repo produces the same flat ephemeral name an attached
/// one does. This is the successor to audit finding A4 (`proj--ww/HEAD`
/// masquerading as a real ref) and its `detached-<shortsha>` workaround: the
/// name has no third component to fill in, so neither shape is representable.
#[test]
fn create_over_a_detached_source_still_mints_the_flat_name() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Detach HEAD in the manifest repo by checking out the commit SHA.
    let repo = ws.join("github/org/repo");
    let head_sha = head_sha(&repo);
    git(&["checkout", "--detach", &head_sha], &repo);

    rwv()
        .args(["workweave", "web-app", "create", "det"])
        .current_dir(&ws)
        .assert()
        .success();

    // §3.5: the name is minted from (project, workweave) and nothing observed
    // feeds into it, so a detached source produces exactly the same flat name
    // an attached one would. The old derivation read the source's `current_ref`
    // and fell back to a `detached-<shortsha>` segment; both are gone. Break
    // that and this fails — the listing grows a `web-app--det/...` entry.
    let branches = branch_names(&repo);
    assert!(
        branches.iter().any(|b| b == "web-app--det"),
        "expected the flat ephemeral branch 'web-app--det', got:\n{branches:?}"
    );
    assert!(
        !branches.iter().any(|b| b.starts_with("web-app--det/")),
        "a detached source must not add a segment to the ephemeral name, got:\n{branches:?}"
    );
}

// ============================================================================
// Pattern B7 — `workweave create` cleans up partial state on bail
// ============================================================================

/// If `create_workweave` fails partway through, it must remove the workweave
/// directory so a clean retry succeeds without `--replace-existing`. Audit
/// finding B7.
#[test]
fn create_workweave_cleans_up_on_bail() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Point the manifest at a non-existent repo path so per-repo worktree
    // creation fails for every repo, causing the loop to bail.
    let project_dir = ws.join("projects/web-app");
    let bad_manifest = r#"[repositories."github/org/missing"]
type = "git"
url = "file:///nonexistent/repo"
version = "main"
role = "owned"
"#;
    std::fs::write(project_dir.join("rwv.toml"), bad_manifest).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "retryme"])
        .current_dir(&ws)
        .assert()
        .failure();

    // The workweave directory must not be left on disk; otherwise the next
    // attempt is forced down the `--replace-existing` path.
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
    let tmp = common::tempdir().unwrap();
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
        r#"[repositories."github/org/repo1"]
type = "git"
url = "file://{repo1}"
version = "main"
role = "owned"

[repositories."github/org/repo2"]
type = "git"
url = "file://{repo2_path}"
version = "main"
role = "owned"
"#,
        repo1 = repo1.display(),
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    ws
}

#[test]
fn create_workweave_rollback_prunes_orphan_worktree_registrations() {
    // When create fails after some repos succeed, the successfully-registered
    // worktrees must be pruned from the primary repos' `.git/worktrees/`
    // metadata. Without this, the primary repo still knows about the partial
    // worktree, producing git warnings and blocking re-creates.
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_two_repos(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "partial"])
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
        r#"[repositories."github/org/repo1"]
type = "git"
url = "file://{repo1}"
version = "main"
role = "owned"

[repositories."github/org/repo2"]
type = "git"
url = "file:///nonexistent/repo2"
version = "main"
role = "owned"

[repositories."github/org/repo3"]
type = "git"
url = "file://{repo3}"
version = "main"
role = "owned"
"#,
        repo1 = repo1.display(),
        repo3 = repo3.display(),
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    ws
}

#[test]
fn create_workweave_rollback_prunes_all_registered_worktrees_not_just_failed() {
    // repo1 and repo3 succeed (both registered); repo2 fails (missing).
    // After rollback, BOTH repo1 and repo3 must have clean worktree metadata.
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_three_repos(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "multi-partial"])
        .current_dir(&ws)
        .assert()
        .failure();

    let ww_dir = weaveroot.join("web-app--multi-partial");
    assert!(
        !ww_dir.exists(),
        "workweave dir must be removed on rollback"
    );

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

/// A clean retry after a failed create must succeed without --replace-existing.
/// This is the end-to-end version of the atomicity contract.
#[test]
fn create_workweave_clean_retry_after_failure_succeeds() {
    let tmp = common::tempdir().unwrap();
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
        r#"[repositories."github/org/repo1"]
type = "git"
url = "file://{repo1}"
version = "main"
role = "owned"

[repositories."github/org/missing"]
type = "git"
url = "file:///nonexistent/missing"
version = "main"
role = "owned"
"#,
        repo1 = repo1.display(),
    );
    std::fs::write(project_dir.join("rwv.toml"), &bad_manifest).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "retry-me"])
        .current_dir(&ws)
        .assert()
        .failure();

    // Fix the manifest — only real repos remain.
    let good_manifest = format!(
        r#"[repositories."github/org/repo1"]
type = "git"
url = "file://{repo1}"
version = "main"
role = "owned"
"#,
        repo1 = repo1.display(),
    );
    std::fs::write(project_dir.join("rwv.toml"), &good_manifest).unwrap();

    // Second create (same name, no --replace-existing): must succeed because rollback
    // cleaned up all state from the first attempt.
    rwv()
        .args(["workweave", "web-app", "create", "retry-me"])
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
/// --replace-existing as the fix. This lets users understand the error without reading
/// source code.
#[test]
fn no_marker_diagnostic_names_partial_create_as_likely_cause() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Simulate a partial workweave: directory exists but no marker file.
    let ww_dir = weaveroot.join("web-app--orphan");
    std::fs::create_dir_all(&ww_dir).unwrap();
    // Deliberately do NOT write .rwv-workweave.

    rwv()
        .args(["workweave", "web-app", "create", "orphan"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("partially created")
                .or(predicate::str::contains("previous failed")),
        )
        .stderr(predicate::str::contains("--replace-existing"));
}

// ============================================================================
// --replace-existing prunes orphan worktree refs from prior partial creates
// ============================================================================

/// `rwv workweave create --replace-existing` must succeed even when the primary repo
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
///   4. `rwv workweave <proj> create <ww> --replace-existing` must:
///      a. Prune the orphan registration before re-creating.
///      b. Succeed — exit 0 and produce a valid workweave with marker.
///      c. Leave no orphan registrations in the primary repo.
#[test]
fn create_workweave_replace_existing_prunes_orphan_worktree_registrations() {
    let tmp = common::tempdir().unwrap();
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
    // repo.  The branch name is incidental to what this test measures and is
    // deliberately OUTSIDE the workweave's `web-app--stale-ww` namespace:
    // a name inside it would make the create refuse on the namespace
    // collision, which is a different test's subject.
    let primary_repo = ws.join("github/org/repo");
    git(
        &[
            "worktree",
            "add",
            "--force",
            wt_dest.to_str().unwrap(),
            "-b",
            "fixture/orphan-registration",
        ],
        &primary_repo,
    );

    // Remove the worktree directory to create an orphan registration —
    // the `.git/worktrees/<name>` entry now points at a missing path.
    std::fs::remove_dir_all(&wt_dest).unwrap();

    // Verify that the primary repo sees the stale registration before the replace.
    let before = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&primary_repo)
        .output()
        .expect("git worktree list should work");
    let before_listing = String::from_utf8_lossy(&before.stdout);
    assert!(
        before_listing.contains("stale-ww"),
        "precondition: orphan registration must be present before the replace; \
         got:\n{before_listing}"
    );

    // ── Step 2: --replace-existing create must succeed ────────────────────
    rwv()
        .args([
            "workweave",
            "web-app",
            "create",
            "stale-ww",
            "--replace-existing",
        ])
        .current_dir(&ws)
        .assert()
        .success();

    // Workweave directory must exist with a marker.
    assert!(
        ww_dir.exists(),
        "workweave dir must exist after --replace-existing create"
    );
    assert!(
        ww_dir.join(".rwv-workweave").exists(),
        "--replace-existing create must write the .rwv-workweave marker"
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
        .filter(|l| {
            l.starts_with("worktree ")
                && l.contains("stale-ww")
                && !l.contains(ww_dir.join("github/org/repo").to_string_lossy().as_ref())
        })
        .count();
    assert_eq!(
        stale_count, 0,
        "--replace-existing must prune orphan worktree registrations; \
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
    // `rwv workweave delete` (no waiver) refuses and names the dirty repo.
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "dirty"])
        .current_dir(&ws)
        .assert()
        .success();

    // Dirty up a tracked file in the manifest-repo worktree.
    let repo_wt = weaveroot.join("web-app--dirty/github/org/repo");
    std::fs::write(repo_wt.join("README"), "DIRTY EDIT\n").unwrap();

    // Plain delete: must refuse, must name the waiver and the dirty repo.
    rwv()
        .args(["workweave", "web-app", "delete", "dirty"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--discard-uncommitted")
                .and(predicate::str::contains("github/org/repo")),
        );

    // Workweave directory must still exist after the refused delete.
    let ww_dir = weaveroot.join("web-app--dirty");
    assert!(
        ww_dir.exists(),
        "refused delete must leave workweave intact"
    );
}

#[test]
fn workweave_delete_discard_uncommitted_proceeds_on_dirty() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "del-force"])
        .current_dir(&ws)
        .assert()
        .success();

    let repo_wt = weaveroot.join("web-app--del-force/github/org/repo");
    std::fs::write(repo_wt.join("README"), "DIRTY\n").unwrap();
    // And an untracked file (to cover that branch of `git status --porcelain`).
    std::fs::write(repo_wt.join("LOCAL_TODO"), "todo\n").unwrap();

    rwv()
        .args([
            "workweave",
            "web-app",
            "delete",
            "del-force",
            "--discard-uncommitted",
        ])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--del-force");
    assert!(
        !ww_dir.exists(),
        "--discard-uncommitted must remove the workweave"
    );
}

#[test]
fn workweave_delete_clean_succeeds_without_waivers() {
    // Make_workspace's project dir is NOT a git repo, so activation can't
    // generate a worktree there. The single manifest repo is clean after a
    // fresh create, so the dirty check should pass and delete should
    // succeed without any waiver.
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "clean"])
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args(["workweave", "web-app", "delete", "clean"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--clean");
    assert!(!ww_dir.exists(), "clean delete should remove the workweave");
}

// ============================================================================
// Workweave create reads working-tree rwv.toml (uncommitted edits)
// ============================================================================

#[test]
fn workweave_create_picks_up_uncommitted_rwv_yaml() {
    // make_workspace_with_project_repo commits the manifest. We then edit
    // the working-tree rwv.toml WITHOUT committing and verify that:
    //   (a) plain `create` refuses and names the dirty file
    //   (b) `create --capture-dirty` succeeds and the workweave sees the edit
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "uncommit-test");

    // Append a comment to rwv.toml in the primary's working tree, without
    // committing. The committed version still says no comment.
    let primary_manifest = ws.join("projects/uncommit-test/rwv.toml");
    let original = std::fs::read_to_string(&primary_manifest).unwrap();
    let edited = format!("{original}# UNCOMMITTED-MARKER\n");
    std::fs::write(&primary_manifest, &edited).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // --- (a) plain create must refuse and name the dirty file ---
    let refuse_assert = rwv()
        .args(["workweave", "uncommit-test", "create", "ww-uncommit"])
        .current_dir(&ws)
        .assert()
        .failure();
    let refuse_stderr = String::from_utf8_lossy(&refuse_assert.get_output().stderr);
    assert!(
        refuse_stderr.contains("uncommitted changes"),
        "expected 'uncommitted changes' in refusal message, got:\n{refuse_stderr}"
    );
    assert!(
        refuse_stderr.contains("rwv.toml"),
        "refusal message should name the dirty file (rwv.toml), got:\n{refuse_stderr}"
    );
    assert!(
        refuse_stderr.contains("--capture-dirty"),
        "refusal message should hint at --capture-dirty, got:\n{refuse_stderr}"
    );

    // The workweave must not be left on disk after the refusal.
    let ww_dir = weaveroot.join("uncommit-test--ww-uncommit");
    assert!(
        !ww_dir.exists(),
        "create must not leave a partial workweave dir on refusal, found: {}",
        ww_dir.display()
    );

    // --- (b) --capture-dirty succeeds and captures the edit ---
    let capture_assert = rwv()
        .args([
            "workweave",
            "uncommit-test",
            "create",
            "ww-uncommit",
            "--capture-dirty",
        ])
        .current_dir(&ws)
        .assert()
        .success();

    // The CLI must emit a warning about dirty state (so the operator
    // notices the working tree was captured).
    let capture_stderr = String::from_utf8_lossy(&capture_assert.get_output().stderr);
    assert!(
        capture_stderr.contains("uncommitted") || capture_stderr.contains("working-tree"),
        "expected dirty-state warning with --capture-dirty, got stderr: {capture_stderr}"
    );

    // The workweave's project worktree must have the UNCOMMITTED marker.
    let ww_manifest = weaveroot.join("uncommit-test--ww-uncommit/projects/uncommit-test/rwv.toml");
    let ww_content = std::fs::read_to_string(&ww_manifest).unwrap();
    assert!(
        ww_content.contains("UNCOMMITTED-MARKER"),
        "workweave's rwv.toml should reflect the primary's uncommitted edit with --capture-dirty, got:\n{ww_content}"
    );
}

#[test]
fn workweave_create_with_clean_committed_manifest_emits_no_dirty_warning() {
    // Sanity counterpart to the above: a clean workspace must NOT trigger
    // the dirty-state warning, so users don't get noise on every create.
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "clean-test");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let assert = rwv()
        .args(["workweave", "clean-test", "create", "ww-clean"])
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
// --capture-dirty: refuse dirty primary by default
// ============================================================================

/// Default `create` refuses when the project dir has uncommitted changes,
/// names the dirty files, and hints at all three remediation options.
#[test]
fn workweave_create_refuses_dirty_primary_by_default() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "dirty-proj");

    // Write an uncommitted file in the project dir.
    let project_dir = ws.join("projects/dirty-proj");
    std::fs::write(project_dir.join("in-progress.txt"), "work in progress").unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let assert = rwv()
        .args(["workweave", "dirty-proj", "create", "blocked"])
        .current_dir(&ws)
        .assert()
        .failure();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);

    // Must name the dirty file.
    assert!(
        stderr.contains("in-progress.txt"),
        "error must name the dirty file, got:\n{stderr}"
    );
    // Must hint at all three remediation options.
    assert!(
        stderr.contains("commit"),
        "error must hint at committing, got:\n{stderr}"
    );
    assert!(
        stderr.contains("stash"),
        "error must hint at stashing, got:\n{stderr}"
    );
    assert!(
        stderr.contains("--capture-dirty"),
        "error must hint at --capture-dirty, got:\n{stderr}"
    );

    // No partial workweave dir must be left behind.
    let ww_dir = weaveroot.join("dirty-proj--blocked");
    assert!(
        !ww_dir.exists(),
        "create must not leave a partial workweave dir after refusal, found: {}",
        ww_dir.display()
    );
}

/// `--capture-dirty` opts in to the old behavior: create succeeds even when
/// the project dir has uncommitted changes.
#[test]
fn workweave_create_capture_dirty_allows_dirty_primary() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "dirty-ok");

    // Write an uncommitted file in the project dir.
    let project_dir = ws.join("projects/dirty-ok");
    std::fs::write(project_dir.join("draft.txt"), "draft content").unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args([
            "workweave",
            "dirty-ok",
            "create",
            "allowed",
            "--capture-dirty",
        ])
        .current_dir(&ws)
        .assert()
        .success();

    // The workweave directory must exist.
    let ww_dir = weaveroot.join("dirty-ok--allowed");
    assert!(
        ww_dir.exists(),
        "--capture-dirty create must succeed and leave the workweave dir at {}",
        ww_dir.display()
    );
}

/// A workspace without a git-backed project dir (plain directory) is not
/// subject to the dirty check — there is nothing to check. Create must
/// succeed without `--capture-dirty`.
#[test]
fn workweave_create_no_project_repo_skips_dirty_check() {
    // `make_workspace` creates a plain (non-git) project dir.
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "plain-proj");

    // Write an arbitrary file into the plain project dir to confirm it's
    // not git-tracked (and thus the dirty check should not fire).
    let project_dir = ws.join("projects/plain-proj");
    std::fs::write(project_dir.join("extra.txt"), "extra").unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "plain-proj", "create", "no-git-check"])
        .current_dir(&ws)
        .assert()
        .success();
}

// ============================================================================
// Workweave parent tracking (the edge a bare `rwv sync-to` lands along)
// ============================================================================

#[test]
fn workweave_create_records_primary_as_parent() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "parented"])
        .current_dir(&ws)
        .assert()
        .success();

    let marker = weaveroot.join("web-app--parented/.rwv-workweave");
    let content = std::fs::read_to_string(&marker).unwrap();
    assert!(
        content.contains("\"parent\""),
        "marker must include a `parent` field, got:\n{content}"
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Step 1: create ww1 forked from primary.
    rwv()
        .args(["workweave", "web-app", "create", "ww1"])
        .current_dir(&ws)
        .assert()
        .success();

    // Step 2: from inside ww1, create ww2 — should fork from ww1.
    let ww1 = weaveroot.join("web-app--ww1");
    rwv()
        .args(["workweave", "web-app", "create", "ww2"])
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
    // refuse because `source` is now a required argument. The error must
    // be non-zero and mention source or SOURCE (clap capitalizes args).
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "p");

    rwv()
        .args(["sync"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("SOURCE").or(predicate::str::contains("source")));
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
        r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    // NOTE: rwv.toml is NOT committed — no commit exists yet.

    ws
}

/// Build a workspace where one manifest repo has been git-init'd but has no
/// commits yet. The project repo is fine; this exercises the manifest-repo
/// preflight path.
fn make_workspace_with_uncommitted_manifest_repo(tmp: &Path, project: &str) -> std::path::PathBuf {
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
        r#"[repositories."github/org/good"]
type = "git"
url = "file://{good}"
version = "main"
role = "owned"

[repositories."github/org/empty"]
type = "git"
url = "file://{bad}"
version = "main"
role = "owned"
"#,
        good = good_repo.display(),
        bad = bad_repo.display()
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

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
    let tmp = common::tempdir().unwrap();
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
    // Verify the exact shape of the error message matches the spec:
    //   "project <name> has no commits yet — run "git -C projects/<name> commit" ..."
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_uncommitted_project(tmp.path(), "myproj");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let output = rwv()
        .args(["workweave", "myproj", "create", "check-msg"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_uncommitted_manifest_repo(tmp.path(), "multiproj");

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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "good-project");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "good-project", "create", "preflight-ok"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("good-project--preflight-ok");
    assert!(
        ww_dir.exists(),
        "workweave should be created successfully when all repos have commits"
    );
}

// ===========================================================================
// R25 rollback: failed create leaves canonical repos exactly as before
// ===========================================================================

/// Plant a failing `post-checkout` hook in `repo`.
///
/// `git worktree add` runs `post-checkout` after materializing the new tree;
/// a non-zero exit causes the add to fail with output containing "hook".
/// The returned hook path is the installed executable so the test can remove
/// or modify it later if needed.
#[cfg(unix)]
fn plant_failing_post_checkout_hook(repo: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let hooks_dir = repo.join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join("post-checkout");
    std::fs::write(&hook_path, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    hook_path
}

/// Verify that `repo` has no local branches matching `prefix/*`.
///
/// Used after a rollback to assert that no ephemeral branch was left behind.
fn assert_no_branches_with_prefix(repo: &Path, prefix: &str) {
    let branches = branches_with_prefix(repo, prefix);
    assert!(
        branches.is_empty(),
        "rollback must delete all ephemeral branches with prefix '{prefix}/*'; \
         found: {branches:?} in {}",
        repo.display()
    );
}

/// After a hook-rejected create the primary repo must be exactly as before:
/// no prunable worktree registration AND no ephemeral branch.
///
/// This is the end-to-end test of the R25 rollback contract: the operator
/// should be able to fix the hook, rerun `rwv workweave create`, and succeed
/// without any manual `git worktree prune` or `git branch -D` steps.
///
/// Not gated because its subject is Unix — the rollback contract is portable.
/// Gated because whether Git for Windows executes a `#!/bin/sh` post-checkout
/// hook is unestablished, and this test needs the hook to FIRE. If it does not,
/// the create succeeds and every residue assertion below passes against a
/// rollback that never ran. The `.failure()` assertion on the create is what
/// makes the rest non-vacuous, and it is exactly what would stop holding.
#[test]
#[cfg(unix)]
fn hook_rejected_create_leaves_no_registration_and_no_branch() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproject");
    let repo_path = ws.join("github/org/repo");

    plant_failing_post_checkout_hook(&repo_path);

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "myproject", "create", "ww-hook"])
        .current_dir(&ws)
        .assert()
        .failure();

    // 1. Workweave directory must not exist.
    let ww_dir = weaveroot.join("myproject--ww-hook");
    assert!(
        !ww_dir.exists(),
        "workweave dir must not exist after hook-rejected create: {}",
        ww_dir.display()
    );

    // 2. No orphan worktree registration in the primary repo.
    let listing_out = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo_path)
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&listing_out.stdout);
    assert!(
        !listing.contains("ww-hook"),
        "rollback must prune orphan worktree registration; git worktree list still shows it:\n{listing}"
    );

    // 3. No ephemeral branch left behind in the primary repo.
    assert_no_branches_with_prefix(&repo_path, "myproject--ww-hook");
}

/// Multi-repo partial failure: when repo1 succeeds and repo2 fails (via a
/// hook), rollback must delete the ephemeral branch that was created in repo1,
/// not only prune the worktree registration.
///
/// This exercises the "earlier repos' state cleaned up too" requirement.
///
/// Not gated because its subject is Unix — the rollback contract is portable.
/// Gated because whether Git for Windows executes a `#!/bin/sh` post-checkout
/// hook is unestablished, and this test needs the hook to FIRE. If it does not,
/// the create succeeds and every residue assertion below passes against a
/// rollback that never ran. The `.failure()` assertion on the create is what
/// makes the rest non-vacuous, and it is exactly what would stop holding.
#[test]
#[cfg(unix)]
fn partial_create_failure_rolls_back_branches_of_earlier_repos() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");

    // repo1 succeeds; repo2 has a failing hook so it fails.
    let repo1 = ws.join("github/org/repo1");
    let repo2 = ws.join("github/org/repo2");
    init_repo_with_commit(&repo1);
    init_repo_with_commit(&repo2);

    // Plant a failing hook in repo2 only.
    plant_failing_post_checkout_hook(&repo2);

    let project_dir = ws.join("projects/multi-hook");
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest = format!(
        r#"[repositories."github/org/repo1"]
type = "git"
url = "file://{r1}"
version = "main"
role = "owned"

[repositories."github/org/repo2"]
type = "git"
url = "file://{r2}"
version = "main"
role = "owned"
"#,
        r1 = repo1.display(),
        r2 = repo2.display(),
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "multi-hook", "create", "partial-hook"])
        .current_dir(&ws)
        .assert()
        .failure();

    // Workweave dir must not exist.
    let ww_dir = weaveroot.join("multi-hook--partial-hook");
    assert!(
        !ww_dir.exists(),
        "workweave dir must be removed on rollback: {}",
        ww_dir.display()
    );

    // repo1 must have no orphan registration.
    let listing_out = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo1)
        .output()
        .expect("git worktree list should work");
    let listing = String::from_utf8_lossy(&listing_out.stdout);
    assert!(
        !listing.contains("partial-hook"),
        "rollback must prune repo1 worktree registration; still shows:\n{listing}"
    );

    // repo1 must have no ephemeral branch left over.
    assert_no_branches_with_prefix(&repo1, "multi-hook--partial-hook");

    // repo2's failed worktree should also leave no stale branch (it was never
    // created because the hook fired before the branch was committed).
    assert_no_branches_with_prefix(&repo2, "multi-hook--partial-hook");
}

/// When the rollback's ref DESTROY fails, the error returned from
/// `create_workweave` must:
/// 1. Preserve the original root-cause error as the primary message.
/// 2. Append a "manual cleanup" note with the exact `git branch -D` command.
///
/// The obstruction is arranged by a `post-checkout` hook in repo1 that makes
/// the shared store's `refs/heads` read-only and then exits 0: `git worktree
/// add` has already written the ephemeral ref by the time the hook runs, so
/// the birth succeeds and the branch exists at exactly the revision the
/// receipt records — the rollback's `DeletionWarrant::unmoved` is satisfied
/// and it is `git branch -D` itself that then fails on EACCES. repo2 is
/// missing, which is what triggers the rollback in the first place.
///
/// NOTE: `CreateRollbackGuard` is private; the test drives `create_workweave`
/// via the public API and inspects the returned error string.
///
/// Not gated because its subject is Unix — the rollback contract is portable.
/// Gated because whether Git for Windows executes a `#!/bin/sh` post-checkout
/// hook is unestablished, and this test needs the hook to FIRE. If it does not,
/// the create succeeds and every residue assertion below passes against a
/// rollback that never ran. The `.failure()` assertion on the create is what
/// makes the rest non-vacuous, and it is exactly what would stop holding.
/// It also drops a directory's write permission to force the cleanup failure,
/// which a Windows read-only attribute does not do — that half needs an ACL.
#[test]
#[cfg(unix)]
fn cleanup_failure_preserves_original_error_with_manual_note() {
    use repoweave::manifest::{ProjectName, WorkweaveName};
    use repoweave::workweave::create_workweave;
    use std::os::unix::fs::PermissionsExt;

    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");

    // repo1 will succeed; repo2 is intentionally missing to trigger rollback.
    let repo1 = ws.join("github/org/repo1");
    init_repo_with_commit(&repo1);

    let project_dir = ws.join("projects/locked-branch");
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest_content = format!(
        r#"[repositories."github/org/repo1"]
type = "git"
url = "file://{r1}"
version = "main"
role = "owned"

[repositories."github/org/repo2"]
type = "git"
url = "file:///nonexistent/repo2"
version = "main"
role = "owned"
"#,
        r1 = repo1.display(),
    );
    std::fs::write(project_dir.join("rwv.toml"), &manifest_content).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Post-checkout hook: seal `refs/heads` AFTER the branch is written.
    let hooks_dir = repo1.join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join("post-checkout");
    std::fs::write(
        &hook_path,
        "#!/bin/sh\n\
         common=$(git rev-parse --path-format=absolute --git-common-dir)\n\
         chmod 500 \"$common/refs/heads\"\n\
         exit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let refs_heads = repo1.join(".git/refs/heads");
    let project = ProjectName::new("locked-branch".to_string()).unwrap();
    let ww_name = WorkweaveName::new("stuck".to_string()).unwrap();
    let err = create_workweave(&ws, &ws, &project, &ww_name, false, false, false, None);

    // Restore permissions so tempdir cleanup doesn't fail.
    let _ = std::fs::set_permissions(&refs_heads, std::fs::Permissions::from_mode(0o755));

    let err_msg = err
        .expect_err("create must fail (repo2 missing + branch undeletable)")
        .to_string();

    // Primary error is the root cause, not the cleanup failure.
    assert!(
        err_msg.contains("workweave create completed with"),
        "primary error must be the root-cause 'workweave create completed with N failure(s)'; \
         got:\n{err_msg}"
    );

    // Cleanup failure note must name the manual command.
    assert!(
        err_msg.contains("manual cleanup") || err_msg.contains("manually"),
        "error must include a manual-cleanup note; got:\n{err_msg}"
    );
    assert!(
        err_msg.contains("branch -D"),
        "manual-cleanup note must name the git branch -D command; got:\n{err_msg}"
    );
}

// ===========================================================================
// R25: workweave create failure caused by a git hook names the hook
// (repair-verb audit)
// ===========================================================================

/// When `git worktree add` fails because a git repository hook (e.g.
/// `post-checkout`) exits non-zero, the error message must attribute the
/// failure to the hook so the operator knows where to look — not just report
/// a bare "git command failed" with opaque stderr.
#[test]
fn create_names_hook_config_when_git_hook_rejects_worktree() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "hook-test");
    let repo_path = ws.join("github/org/repo");

    // Install a failing post-checkout hook in the repo.  `git worktree add`
    // runs this hook after checking out the new worktree; a non-zero exit
    // causes the add to fail.  We use `#!/bin/sh\nexit 1` — minimal,
    // portable, and deterministic.
    let hooks_dir = repo_path.join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join("post-checkout");
    std::fs::write(&hook_path, "#!/bin/sh\nexit 1\n").unwrap();
    // Make the hook executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let output = rwv()
        .args(["workweave", "hook-test", "create", "hook-ww"])
        .current_dir(&ws)
        .output()
        .expect("failed to spawn rwv workweave create");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // The command must fail (the hook rejects the worktree).
    assert!(
        !output.status.success(),
        "workweave create should fail when a git hook exits non-zero"
    );

    // R25: the error must attribute the failure to a hook and name
    // `.git/hooks/` or `core.hooksPath` so the operator knows where to look.
    assert!(
        combined.contains("hook"),
        "error must name 'hook' when git worktree add fails due to a hook; \
         got:\n{combined}"
    );
    assert!(
        combined.contains(".git/hooks") || combined.contains("hooksPath"),
        "error must point at `.git/hooks/` or core.hooksPath as the config location; \
         got:\n{combined}"
    );
}
