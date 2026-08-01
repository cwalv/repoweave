//! E2E tests for `rwv init`.
//!
//! These tests exercise the CLI binary via `assert_cmd`. The `Init` subcommand
//! creates a new project directory with `git init` and an empty `rwv.toml`.
//! Optionally, `--provider github/owner` configures a git remote.

use assert_cmd::Command;
use predicates::prelude::*;
use repoweave::manifest::Manifest;
use std::path::Path;
use std::process;

mod common;

/// Build a `Command` for the `rwv` binary.
fn rwv() -> Command {
    common::rwv()
}

/// Run a git command in `dir`, returning its stdout as a String.
fn git_output(args: &[&str], dir: &Path) -> String {
    let output = common::git()
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "my-app"])
        .current_dir(&ws)
        .assert()
        .success();

    let manifest_path = ws.join("projects/my-app/rwv.toml");
    assert!(manifest_path.exists(), "rwv.toml should exist after init");

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

/// After `rwv init`, projects/<name>/rwv.toml must exist, be non-empty,
/// and parse cleanly via the manifest loader.  This is the exact
/// precondition that `rwv workweave create` depends on — the file must
/// be present at the time of the initial `git commit` so the commit
/// captures it.
#[test]
fn init_rwv_yaml_parses_via_manifest_loader() {
    let tmp = common::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "fresh-proj"])
        .current_dir(&ws)
        .assert()
        .success();

    let manifest_path = ws.join("projects/fresh-proj/rwv.toml");

    // 1. File exists.
    assert!(
        manifest_path.exists(),
        "projects/fresh-proj/rwv.toml must exist immediately after `rwv init`"
    );

    // 2. File is non-empty.
    let content = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(
        !content.trim().is_empty(),
        "rwv.toml must not be empty after init"
    );

    // 3. Parses cleanly through the manifest loader (not just as generic YAML).
    let manifest = Manifest::from_path(&manifest_path)
        .expect("rwv.toml written by `rwv init` must parse via Manifest::from_path");

    // 4. Empty repositories map — no repos have been added yet.
    assert!(
        manifest.is_empty(),
        "repositories map should be empty in a freshly initialised project"
    );
}

#[test]
fn init_runs_git_init_in_project_dir() {
    let tmp = common::tempdir().unwrap();
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

#[test]
fn init_writes_gitattributes_with_replay_exclusion() {
    // `rwv init` must seed `.gitattributes` with the
    // `rwv.lock merge=rwv-ours` line so future `rwv sync` rebases keep
    // source's lock through the replay.
    let tmp = common::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    rwv()
        .args(["init", "my-app"])
        .current_dir(&ws)
        .assert()
        .success();

    let attrs = std::fs::read_to_string(ws.join("projects/my-app/.gitattributes"))
        .expect("rwv init should create .gitattributes");
    assert!(
        attrs.contains("rwv.lock merge=rwv-ours"),
        ".gitattributes should contain `rwv.lock merge=rwv-ours`; got: {attrs:?}"
    );
}

// ============================================================================
// Name collision handling
// ============================================================================

#[test]
fn init_rejects_duplicate_project_name() {
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    // Create the project.
    rwv()
        .args(["init", "keep-safe"])
        .current_dir(&ws)
        .assert()
        .success();

    // Write a custom rwv.toml to verify it isn't overwritten.
    let manifest_path = ws.join("projects/keep-safe/rwv.toml");
    let custom_content = "[repositories]\n";
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
        "existing rwv.toml should not be modified on collision"
    );
}

// ============================================================================
// --provider flag -- `rwv init PROJECT --provider github/owner`
// ============================================================================

#[test]
fn init_with_provider_sets_git_remote() {
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let status = common::git()
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
    let status = common::git()
        .args(["clone", bare.to_str().unwrap(), work.to_str().unwrap()])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git clone should succeed");
    assert!(status.success());

    // Configure git user for the commit
    let _ = common::git()
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&work)
        .status();
    let _ = common::git()
        .args(["config", "user.name", "Test"])
        .current_dir(&work)
        .status();

    std::fs::write(work.join("README.md"), "# test\n").unwrap();
    let _ = common::git().args(["add", "."]).current_dir(&work).status();
    let _ = common::git()
        .args(["commit", "-m", "init"])
        .current_dir(&work)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();
    let _ = common::git()
        .args(["push", "origin", "main"])
        .current_dir(&work)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();

    bare
}

#[test]
fn adopt_clones_repo_into_projects() {
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());
    let bare = make_repo_with_commit(tmp.path(), "no-yaml");

    rwv()
        .args(["init", "--adopt", &format!("file://{}", bare.display())])
        .current_dir(&ws)
        .assert()
        .success();

    let manifest_path = ws.join("projects/no-yaml/rwv.toml");
    assert!(
        manifest_path.exists(),
        "rwv.toml should be created for adopted repo"
    );
}

#[test]
fn adopt_preserves_existing_rwv_yaml() {
    let tmp = common::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());

    // Create a bare repo that already has an rwv.toml
    let bare = tmp.path().join("has-yaml.git");
    let work = tmp.path().join("has-yaml-work");

    let _ = common::git()
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&bare)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();
    let _ = common::git()
        .args(["clone", bare.to_str().unwrap(), work.to_str().unwrap()])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();
    let _ = common::git()
        .args(["config", "user.email", "test@test.com"])
        .current_dir(&work)
        .status();
    let _ = common::git()
        .args(["config", "user.name", "Test"])
        .current_dir(&work)
        .status();

    // Write a custom rwv.toml
    let custom = "[repositories]\n";
    std::fs::write(work.join("rwv.toml"), custom).unwrap();
    let _ = common::git().args(["add", "."]).current_dir(&work).status();
    let _ = common::git()
        .args(["commit", "-m", "with rwv.toml"])
        .current_dir(&work)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();
    let _ = common::git()
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

    let content = std::fs::read_to_string(ws.join("projects/has-yaml/rwv.toml")).unwrap();
    assert!(
        content.contains("# custom marker"),
        "existing rwv.toml should be preserved, got: {content}"
    );
}

#[test]
fn adopt_activates_project() {
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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

// ============================================================================
// Empty-directory bootstrap
// ============================================================================

/// `rwv init` in a completely empty directory must succeed and leave a valid
/// workspace skeleton so that the caller can immediately use other rwv verbs.
#[test]
fn init_bootstraps_empty_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("fresh");
    std::fs::create_dir_all(&ws).unwrap();

    // Directory is empty — no projects/, no registry dirs.
    assert!(
        std::fs::read_dir(&ws).unwrap().next().is_none(),
        "precondition: fresh/ must be empty"
    );

    // `rwv init` should bootstrap the workspace and create the project.
    rwv()
        .args(["init", "my-app"])
        .current_dir(&ws)
        .assert()
        .success();

    // projects/ skeleton was created.
    assert!(
        ws.join("projects").is_dir(),
        "projects/ must exist after bootstrap"
    );

    // The project itself was created.
    assert!(
        ws.join("projects/my-app").is_dir(),
        "projects/my-app/ must exist after init in empty dir"
    );

    // The rwv.toml is present and valid.
    let manifest_path = ws.join("projects/my-app/rwv.toml");
    assert!(manifest_path.exists(), "rwv.toml must exist");
    let manifest = repoweave::manifest::Manifest::from_path(&manifest_path)
        .expect("rwv.toml from bootstrapped init must parse cleanly");
    assert!(manifest.is_empty(), "repositories map must be empty");
}

/// After an empty-dir bootstrap, workspace context resolves (i.e. rwv verbs
/// that need a workspace work immediately without any extra steps).
#[test]
fn init_empty_dir_workspace_context_resolves_after() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ctx-fresh");
    std::fs::create_dir_all(&ws).unwrap();

    rwv()
        .args(["init", "proj"])
        .current_dir(&ws)
        .assert()
        .success();

    // Running any workspace-context verb (e.g. bare `rwv` status) must not
    // fail with "no repoweave workspace found". Use `rwv init` with a second
    // project as the proxy: it needs a workspace context and it would fail if
    // the bootstrap left the workspace in an unresolvable state.
    rwv()
        .args(["init", "proj2"])
        .current_dir(&ws)
        .assert()
        .success();

    // Both projects exist.
    assert!(ws.join("projects/proj").is_dir());
    assert!(ws.join("projects/proj2").is_dir());
}

/// `rwv init` in a non-empty, non-workspace directory must refuse with a
/// clear message — naming the state and the next step.
#[test]
fn init_refuses_non_empty_non_workspace_directory() {
    let tmp = common::tempdir().unwrap();
    let noisy = tmp.path().join("noisy");
    std::fs::create_dir_all(&noisy).unwrap();
    // Seed with an unrelated file so it is non-empty.
    std::fs::write(noisy.join("some-file.txt"), "random content").unwrap();

    rwv()
        .args(["init", "proj"])
        .current_dir(&noisy)
        .assert()
        .failure()
        .stderr(
            // Must name the state — "not a workspace" and "not empty".
            predicate::str::contains("not a workspace")
                .and(predicate::str::contains("not empty"))
                // Must name the next step — use an empty dir or existing workspace.
                .and(
                    predicate::str::contains("empty directory")
                        .or(predicate::str::contains("existing workspace")),
                ),
        );
}

/// Existing-workspace behavior of `init` must be unchanged after the
/// empty-dir bootstrap path was added.  Running init in an already-valid
/// workspace (one with a registry dir marker) must succeed without touching
/// anything outside the new project.
#[test]
fn init_existing_workspace_unaffected_by_bootstrap_path() {
    let tmp = common::tempdir().unwrap();
    // Use make_empty_workspace which creates github/ + projects/.
    let ws = make_empty_workspace(tmp.path());

    // First project — creates projects/first/.
    rwv()
        .args(["init", "first"])
        .current_dir(&ws)
        .assert()
        .success();

    // Second project — reuses the existing workspace without any re-bootstrap.
    rwv()
        .args(["init", "second"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(ws.join("projects/first").is_dir());
    assert!(ws.join("projects/second").is_dir());
    // .rwv-active was updated by the second init.
    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(active.trim(), "second");
}

/// `rwv init --adopt` in an empty directory must also bootstrap the workspace
/// skeleton before cloning.
#[test]
fn adopt_bootstraps_empty_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("adopt-fresh");
    std::fs::create_dir_all(&ws).unwrap();
    let bare = make_repo_with_commit(tmp.path(), "adopted-proj");

    rwv()
        .args(["init", "--adopt", &format!("file://{}", bare.display())])
        .current_dir(&ws)
        .assert()
        .success();

    // projects/ skeleton was created and the adopted project landed inside it.
    assert!(
        ws.join("projects").is_dir(),
        "projects/ must exist after adopt bootstrap"
    );
    assert!(
        ws.join("projects/adopted-proj").is_dir(),
        "projects/adopted-proj/ must exist after adopt in empty dir"
    );
    // Must be a real git repo (cloned, not init'd).
    let toplevel = git_output(
        &["rev-parse", "--git-dir"],
        &ws.join("projects/adopted-proj"),
    );
    assert!(
        toplevel.contains(".git"),
        "adopted project must be a git repo"
    );
}
