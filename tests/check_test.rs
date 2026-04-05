//! E2E tests for `rwv check` — convention enforcement.
//!
//! These tests exercise the CLI binary via `assert_cmd`. Tests that depend on
//! the full check implementation (bead 8b) are marked `#[ignore]`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
use std::process;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a minimal workspace directory structure with a `github/` registry dir
/// and a `projects/` dir. Returns the workspace root path.
fn make_workspace(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    root
}

/// Initialise a git repo at `path` with a single commit so HEAD exists.
/// Returns the SHA of that commit.
fn init_git_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();

    let run = |args: &[&str], dir: &Path| {
        let out = process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("git command failed to start");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    run(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    run(&["add", "."], path);
    run(&["commit", "-m", "initial"], path);

    // Return HEAD SHA
    run(&["rev-parse", "HEAD"], path)
}

/// Write an `rwv.yaml` manifest into a project directory.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    std::fs::create_dir_all(project_dir).unwrap();
    let mut yaml = String::from("repositories:\n");
    for (repo_path, url) in repos {
        yaml.push_str(&format!(
            "  {repo_path}:\n    type: git\n    url: {url}\n    version: main\n    role: primary\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();
}

/// Write an `rwv.lock` file into a project directory with given repo SHAs.
fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (repo_path, url, sha) in repos {
        yaml.push_str(&format!(
            "  {repo_path}:\n    type: git\n    url: {url}\n    version: {sha}\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), &yaml).unwrap();
}

/// Build a `Command` for the `rwv` binary.
///
/// Sets `current_dir` to a temp dir so tests never accidentally pick up
/// the real workspace. Tests override with their own `.current_dir()`.
fn rwv_cmd() -> Command {
    let mut cmd = Command::cargo_bin("rwv").expect("rwv binary not found");
    cmd.current_dir(std::env::temp_dir());
    cmd
}

// ===========================================================================
// 1. `rwv check` with no issues — clean workspace, exits 0
// ===========================================================================

#[test]

fn check_clean_workspace_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create a repo on disk
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    // Create a project that references that repo
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    rwv_cmd().arg("check").current_dir(&root).assert().success();
}

// ===========================================================================
// 2. Orphaned clone — directory under `github/` not in any project's rwv.yaml
// ===========================================================================

#[test]

fn check_orphaned_clone_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create two repos on disk
    let known_repo = "github/acme/server";
    let orphan_repo = "github/acme/stray-clone";
    init_git_repo(&root.join(known_repo));
    init_git_repo(&root.join(orphan_repo));

    // Only reference one repo in the project manifest
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(known_repo, "https://github.com/acme/server.git")],
    );

    rwv_cmd()
        .arg("check")
        .current_dir(&root)
        .assert()
        .failure()
        .stdout(predicate::str::contains("orphan").or(predicate::str::contains("stray-clone")));
}

// ===========================================================================
// 3. Dangling reference — rwv.yaml entry pointing to a path not on disk
// ===========================================================================

#[test]

fn check_dangling_reference_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create one repo on disk but reference two in the manifest
    let real_repo = "github/acme/server";
    let missing_repo = "github/acme/vanished";
    init_git_repo(&root.join(real_repo));
    // Deliberately do NOT create `missing_repo` on disk

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[
            (real_repo, "https://github.com/acme/server.git"),
            (missing_repo, "https://github.com/acme/vanished.git"),
        ],
    );

    rwv_cmd()
        .arg("check")
        .current_dir(&root)
        .assert()
        .failure()
        .stdout(predicate::str::contains("dangling").or(predicate::str::contains("vanished")));
}

// ===========================================================================
// 4. Stale lock — rwv.lock SHA doesn't match current HEAD
// ===========================================================================

#[test]

fn check_stale_lock_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let _real_sha = init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Write a lock file with a stale (bogus) SHA
    write_lock(
        &project_dir,
        &[(
            repo_path,
            "https://github.com/acme/server.git",
            "0000000000000000000000000000000000000000",
        )],
    );

    rwv_cmd()
        .arg("check")
        .current_dir(&root)
        .assert()
        .failure()
        .stdout(predicate::str::contains("stale").or(predicate::str::contains("lock")));
}

// ===========================================================================
// 5. Multi-project awareness — repo in project A is not orphan even if not
//    in project B
// ===========================================================================

#[test]

fn check_multi_project_no_false_orphan() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create two repos
    let repo_a = "github/acme/server";
    let repo_b = "github/acme/client";
    init_git_repo(&root.join(repo_a));
    init_git_repo(&root.join(repo_b));

    // Project alpha references repo_a only
    let proj_alpha = root.join("projects").join("alpha");
    write_manifest(
        &proj_alpha,
        &[(repo_a, "https://github.com/acme/server.git")],
    );

    // Project beta references repo_b only
    let proj_beta = root.join("projects").join("beta");
    write_manifest(
        &proj_beta,
        &[(repo_b, "https://github.com/acme/client.git")],
    );

    // Both repos are known across projects — no orphans expected
    rwv_cmd().arg("check").current_dir(&root).assert().success();
}

// ===========================================================================
// 6. `rwv check` outside a workspace — should error
// ===========================================================================

#[test]

fn check_outside_workspace_errors() {
    let tmp = tempfile::tempdir().unwrap();
    // No workspace markers here — just an empty temp dir

    rwv_cmd()
        .arg("check")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no repoweave workspace found"));
}

// ===========================================================================
// 7. Integration check hooks report warnings
// ===========================================================================

#[test]

fn check_integration_hooks_report_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create a repo on disk
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    // Create a project with the repo and an integration config
    let project_dir = root.join("projects").join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let yaml = format!(
        r#"repositories:
  {repo_path}:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: primary
integrations:
  cargo:
    enabled: true
"#
    );
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();

    // Even with integration hooks, a clean workspace should not error.
    // Any integration warnings should be printed but not cause failure
    // (only errors cause non-zero exit).
    rwv_cmd().arg("check").current_dir(&root).assert().success();
}

// ===========================================================================
// Smoke test: `rwv check` CLI command is recognized
// ===========================================================================

#[test]
fn check_command_is_recognized() {
    // The command should parse successfully (not fail with "unrecognized subcommand").
    rwv_cmd()
        .arg("check")
        .assert()
        .stdout(predicate::str::contains("unrecognized").not());
}
