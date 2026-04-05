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
    let repos = manifest
        .get("repositories")
        .expect("should have repositories key");
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
// Init auto-activates the project
// ============================================================================

#[test]
fn init_activates_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "my-proj"])
        .current_dir(&ws)
        .assert()
        .success();

    // .rwv-active should be written with the new project name.
    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(
        active.trim(),
        "my-proj",
        ".rwv-active should contain the newly initialised project name"
    );
}

#[test]
fn init_last_project_wins_activation() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "first-proj"])
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args(["init", "second-proj"])
        .current_dir(&ws)
        .assert()
        .success();

    // The second init should have activated second-proj, overwriting first-proj.
    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(
        active.trim(),
        "second-proj",
        ".rwv-active should reflect the last project initialised"
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

// ============================================================================
// --adopt flag tests
// ============================================================================

/// Create a bare git repo that can serve as a clone source for --adopt tests.
/// Returns the path to the bare repo.
fn make_bare_repo(parent: &Path, name: &str) -> std::path::PathBuf {
    let bare = parent.join(format!("{}.git", name));
    let status = process::Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&bare)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git init --bare should succeed");
    assert!(status.success());
    bare
}

/// Create a non-bare repo with an initial commit and push to a bare remote.
/// This gives us a clone source that has a valid HEAD.
fn make_repo_with_commit(parent: &Path, name: &str) -> std::path::PathBuf {
    let bare = make_bare_repo(parent, name);

    // Clone, commit, push
    let work = parent.join(format!("{}-work", name));
    let status = process::Command::new("git")
        .args(["clone", bare.to_str().unwrap(), work.to_str().unwrap()])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git clone should succeed");
    assert!(status.success());

    // Configure git user for the commit
    let _ = process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&work)
        .status();
    let _ = process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&work)
        .status();

    std::fs::write(work.join("README.md"), "# test\n").unwrap();
    let _ = process::Command::new("git")
        .args(["add", "."])
        .current_dir(&work)
        .status();
    let _ = process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&work)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();
    let _ = process::Command::new("git")
        .args(["push", "origin", "main"])
        .current_dir(&work)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();

    bare
}

#[test]
fn adopt_clones_repo_into_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());
    let bare = make_repo_with_commit(tmp.path(), "my-app");

    rwv()
        .args(["init", "--adopt", &format!("file://{}", bare.display())])
        .current_dir(&ws)
        .assert()
        .success();

    let project_dir = ws.join("projects/my-app");
    assert!(
        project_dir.exists(),
        "projects/my-app/ should exist after adopt"
    );
    // Should be a git repo (cloned, not git-init'd)
    let toplevel = git_output(&["rev-parse", "--git-dir"], &project_dir);
    assert!(
        toplevel.contains(".git"),
        "adopted project should be a git repo"
    );
}

#[test]
fn adopt_writes_rwv_yaml_when_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());
    let bare = make_repo_with_commit(tmp.path(), "no-yaml");

    rwv()
        .args(["init", "--adopt", &format!("file://{}", bare.display())])
        .current_dir(&ws)
        .assert()
        .success();

    let manifest_path = ws.join("projects/no-yaml/rwv.yaml");
    assert!(
        manifest_path.exists(),
        "rwv.yaml should be created for adopted repo"
    );
}

#[test]
fn adopt_preserves_existing_rwv_yaml() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    // Create a bare repo that already has an rwv.yaml
    let bare = tmp.path().join("has-yaml.git");
    let work = tmp.path().join("has-yaml-work");

    let _ = process::Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&bare)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();
    let _ = process::Command::new("git")
        .args(["clone", bare.to_str().unwrap(), work.to_str().unwrap()])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();
    let _ = process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&work)
        .status();
    let _ = process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(&work)
        .status();

    // Write a custom rwv.yaml
    let custom = "repositories: {}\n# custom marker\n";
    std::fs::write(work.join("rwv.yaml"), custom).unwrap();
    let _ = process::Command::new("git")
        .args(["add", "."])
        .current_dir(&work)
        .status();
    let _ = process::Command::new("git")
        .args(["commit", "-m", "with rwv.yaml"])
        .current_dir(&work)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();
    let _ = process::Command::new("git")
        .args(["push", "origin", "main"])
        .current_dir(&work)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();

    rwv()
        .args(["init", "--adopt", &format!("file://{}", bare.display())])
        .current_dir(&ws)
        .assert()
        .success();

    let content = std::fs::read_to_string(ws.join("projects/has-yaml/rwv.yaml")).unwrap();
    assert!(
        content.contains("# custom marker"),
        "existing rwv.yaml should be preserved, got: {content}"
    );
}

#[test]
fn adopt_activates_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());
    let bare = make_repo_with_commit(tmp.path(), "activated");

    rwv()
        .args(["init", "--adopt", &format!("file://{}", bare.display())])
        .current_dir(&ws)
        .assert()
        .success();

    // Check that .rwv-active was written
    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(
        active.trim(),
        "activated",
        ".rwv-active should contain the adopted project name"
    );
}

#[test]
fn adopt_rejects_duplicate_project_name() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());
    let bare = make_repo_with_commit(tmp.path(), "dup");

    // First adopt succeeds.
    rwv()
        .args(["init", "--adopt", &format!("file://{}", bare.display())])
        .current_dir(&ws)
        .assert()
        .success();

    // Second adopt with same source should fail.
    rwv()
        .args(["init", "--adopt", &format!("file://{}", bare.display())])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn adopt_conflicts_with_provider() {
    // --adopt and --provider are mutually exclusive.
    rwv()
        .args(["init", "--adopt", "--provider", "github/owner", "foo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}
