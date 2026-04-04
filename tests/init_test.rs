//! E2E tests for `rwv init`.
//!
//! These tests exercise the CLI binary via `assert_cmd`. The `Init` subcommand
//! creates a new project directory with `git init` and an empty `rwv.yaml`.
//! Optionally, `--provider github/owner` configures a git remote.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

/// Build a `Command` for the `rwv` binary.
fn rwv() -> Command {
    Command::cargo_bin("rwv").expect("rwv binary should be buildable")
}

/// Run a git command in `dir`, returning its stdout as a String.
fn git_output(args: &[&str], dir: &Path) -> String {
    let output = process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should be available");
    assert!(
        output.status.success(),
        "git {:?} in {} failed: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("valid UTF-8")
        .trim()
        .to_string()
}

/// Create a minimal workspace structure (no projects yet).
///
/// Layout:
///   {tmp}/ws/            -- workspace root
///   {tmp}/ws/github/     -- registry marker
///   {tmp}/ws/projects/   -- projects directory (empty)
///
/// Returns the workspace root path.
fn make_empty_workspace(tmp: &Path) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

// ============================================================================
// Smoke tests -- command recognition
// ============================================================================

#[test]
fn init_subcommand_is_recognised() {
    // `rwv init` should not produce "unrecognized subcommand".
    let assert = rwv().arg("init").assert();
    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "init should be a recognised subcommand, got stderr: {stderr}"
    );
}

#[test]
fn init_requires_project_argument() {
    rwv()
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// ============================================================================
// Basic init -- `rwv init PROJECT`
// ============================================================================

#[test]
fn init_creates_project_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "my-app"])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/my-app");
    assert!(
        project_dir.exists(),
        "projects/my-app/ should exist after init"
    );
}

#[test]
fn init_creates_empty_rwv_yaml() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "my-app"])
        .current_dir(&ws)
        .assert()
        .success();

    let manifest_path = ws.join("projects/my-app/rwv.yaml");
    assert!(manifest_path.exists(), "rwv.yaml should exist after init");

    let content = std::fs::read_to_string(&manifest_path).unwrap();
    // The manifest should parse as valid YAML with an empty repositories map.
    let manifest: serde_yaml::Value = serde_yaml::from_str(&content).unwrap();
    let repos = manifest.get("repositories").expect("should have repositories key");
    // Empty map can be represented as Mapping with 0 entries or as Null.
    match repos {
        serde_yaml::Value::Mapping(m) => assert!(m.is_empty(), "repositories should be empty"),
        serde_yaml::Value::Null => {} // `repositories:` with no value is fine for empty
        other => panic!("repositories should be empty map or null, got: {:?}", other),
    }
}

#[test]
fn init_runs_git_init_in_project_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "my-app"])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/my-app");
    // Verify it is a git repo by running git rev-parse.
    let toplevel = git_output(&["rev-parse", "--git-dir"], &project_dir);
    assert!(
        toplevel.contains(".git"),
        "project dir should be a git repo, got: {toplevel}"
    );
}

// ============================================================================
// Name collision handling
// ============================================================================

#[test]
fn init_rejects_duplicate_project_name() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    // First init should succeed.
    rwv()
        .args(["init", "collision"])
        .current_dir(&ws)
        .assert()
        .success();

    // Second init with same name should fail.
    rwv()
        .args(["init", "collision"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists").or(predicate::str::contains("exists")));
}

#[test]
fn init_collision_does_not_modify_existing_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    // Create the project.
    rwv()
        .args(["init", "keep-safe"])
        .current_dir(&ws)
        .assert()
        .success();

    // Write a custom rwv.yaml to verify it isn't overwritten.
    let manifest_path = ws.join("projects/keep-safe/rwv.yaml");
    let custom_content = "repositories: {}\n# custom marker\n";
    std::fs::write(&manifest_path, custom_content).unwrap();

    // Attempt duplicate init.
    rwv()
        .args(["init", "keep-safe"])
        .current_dir(&ws)
        .assert()
        .failure();

    // Original content should be preserved.
    let after = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(
        after.contains("# custom marker"),
        "existing rwv.yaml should not be modified on collision"
    );
}

// ============================================================================
// --provider flag -- `rwv init PROJECT --provider github/owner`
// ============================================================================

#[test]
fn init_with_provider_sets_git_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "my-lib", "--provider", "github/acme"])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/my-lib");
    assert!(project_dir.exists(), "project dir should be created");

    // Check that a git remote was configured.
    let remotes = git_output(&["remote", "-v"], &project_dir);
    assert!(
        !remotes.is_empty(),
        "git remote should be configured when --provider is given"
    );
    // The remote URL should reference the provider host and owner.
    assert!(
        remotes.contains("github.com") && remotes.contains("acme"),
        "remote should reference the provider, got: {remotes}"
    );
}

#[test]
fn init_with_provider_remote_contains_project_name() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "cool-tool", "--provider", "github/myorg"])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/cool-tool");
    let remotes = git_output(&["remote", "-v"], &project_dir);
    // The remote URL should include the project name as the repo name.
    assert!(
        remotes.contains("cool-tool"),
        "remote URL should include project name as repo name, got: {remotes}"
    );
}

#[test]
fn init_without_provider_has_no_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "local-only"])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/local-only");
    let remotes = git_output(&["remote"], &project_dir);
    assert!(
        remotes.is_empty(),
        "no remote should be configured without --provider, got: {remotes}"
    );
}

// ============================================================================
// Init does not activate
// ============================================================================

#[test]
fn init_does_not_activate_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    // Create a first project so we have something to compare against.
    rwv()
        .args(["init", "first-proj"])
        .current_dir(&ws)
        .assert()
        .success();

    // Init a second project.
    rwv()
        .args(["init", "second-proj"])
        .current_dir(&ws)
        .assert()
        .success();

    // Running `rwv` (no subcommand) from the workspace root should NOT show
    // second-proj as the active project (no project should be active when
    // running from root).
    let output = rwv()
        .current_dir(&ws)
        .output()
        .expect("rwv should run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Active project: second-proj"),
        "init should not activate the project, got: {stdout}"
    );
}

// ============================================================================
// Init from subdirectory -- should still find workspace root
// ============================================================================

#[test]
fn init_works_from_workspace_subdirectory() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    // Run init from within the github/ subdirectory.
    rwv()
        .args(["init", "from-subdir"])
        .current_dir(ws.join("github"))
        .assert()
        .success();

    let project_dir = ws.join("projects/from-subdir");
    assert!(
        project_dir.exists(),
        "init from a subdirectory should still create the project under projects/"
    );
}
