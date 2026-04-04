//! E2E tests for `rwv add` and `rwv remove`.
//!
//! Tests that require the add/remove commands to be fully implemented are
//! marked `#[ignore]` until bead 6b lands the implementation.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

/// Build a `Command` for the `rwv` binary.
fn rwv() -> Command {
    Command::cargo_bin("rwv").expect("rwv binary should be buildable")
}

/// Create a bare git repo at `path`.
fn init_bare_repo(path: &Path) {
    let status = process::Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(path)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git init --bare failed");
}

/// Create a bare git repo with an initial commit so it can be cloned.
fn init_bare_repo_with_commit(path: &Path) {
    init_bare_repo(path);

    let tmp = tempfile::tempdir().expect("tempdir for working clone");
    let work = tmp.path().join("work");

    let run = |args: &[&str], cwd: &Path| {
        let status = process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("git command failed to start");
        assert!(status.success(), "git {:?} failed", args);
    };

    run(
        &[
            "clone",
            &path.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);
    std::fs::write(work.join("README"), "init").unwrap();
    run(&["add", "."], &work);
    run(&["commit", "-m", "initial"], &work);
    run(&["push", "origin", "main"], &work);
}

/// Set up a workspace with a project directory containing an rwv.yaml manifest.
/// Returns (workspace_dir, project_dir).
fn setup_workspace_with_project(
    tmp: &tempfile::TempDir,
    repos: &[(&str, &str)],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let project_dir = workspace.join("projects").join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    // Initialize the project dir as a git repo so workspace resolution works.
    let run = |args: &[&str], cwd: &Path| {
        let status = process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("git command failed");
        assert!(status.success(), "git {:?} failed", args);
    };
    run(&["init", "--initial-branch=main"], &project_dir);
    run(&["config", "user.email", "test@test.com"], &project_dir);
    run(&["config", "user.name", "Test"], &project_dir);

    write_manifest(&project_dir, repos);
    run(&["add", "rwv.yaml"], &project_dir);
    run(&["commit", "-m", "init"], &project_dir);

    (workspace, project_dir)
}

/// Write an `rwv.yaml` manifest pointing repos at the given URLs.
fn write_manifest(dir: &Path, repos: &[(&str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    if repos.is_empty() {
        yaml.push_str("  {}\n");
    }
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: primary\n"
        ));
    }
    std::fs::write(dir.join("rwv.yaml"), &yaml).unwrap();
}

// ============================================================================
// Smoke tests — command recognition (these pass now)
// ============================================================================

#[test]
fn add_subcommand_is_recognized() {
    // `rwv add` without arguments should fail because URL is required.
    rwv()
        .arg("add")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn remove_subcommand_is_recognized() {
    // `rwv remove` without arguments should fail because PATH is required.
    rwv()
        .arg("remove")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn add_accepts_url_argument() {
    // CLI parses the URL argument successfully. Fails at workspace resolution
    // (not argument parsing) because we run from an empty temp dir.
    let tmp = tempfile::tempdir().unwrap();
    rwv()
        .args(["add", "https://example.com/org/repo.git"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("workspace").or(predicate::str::contains("project")));
}

#[test]
fn remove_accepts_path_argument() {
    // CLI parses the path argument successfully. Fails at workspace resolution
    // (not argument parsing) because we run from an empty temp dir.
    let tmp = tempfile::tempdir().unwrap();
    rwv()
        .args(["remove", "github/example/repo"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("workspace").or(predicate::str::contains("project")));
}

// ============================================================================
// rwv add URL — clones repo and updates manifest
// ============================================================================

#[test]

fn add_clones_repo_to_canonical_path() {
    let tmp = tempfile::tempdir().unwrap();

    // Create a bare repo to serve as the "remote".
    let bare = tmp.path().join("remote.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    rwv()
        .args(["add", &remote_url])
        .current_dir(&workspace)
        .assert()
        .success();

    // The repo should be cloned to a canonical path under the workspace.
    // For a file:// URL the exact path depends on registry resolution,
    // but the manifest should have a new entry.
    let manifest_path = workspace.join("projects/test-project/rwv.yaml");
    let manifest_content = std::fs::read_to_string(&manifest_path)
        .expect("rwv.yaml should exist after add");
    assert!(
        manifest_content.contains(&remote_url)
            || manifest_content.contains("file://"),
        "manifest should contain the added repo URL, got:\n{manifest_content}"
    );
}

#[test]

fn add_with_role_flag_sets_annotation() {
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("fork-remote.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    rwv()
        .args(["add", &remote_url, "--role=fork"])
        .current_dir(&workspace)
        .assert()
        .success();

    let manifest_path = workspace.join("projects/test-project/rwv.yaml");
    let manifest_content = std::fs::read_to_string(&manifest_path)
        .expect("rwv.yaml should exist after add");
    assert!(
        manifest_content.contains("role: fork"),
        "manifest should have role set to fork, got:\n{manifest_content}"
    );
}

#[test]

fn add_existing_repo_handles_gracefully() {
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("existing.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    // Start with the repo already in the manifest.
    let (workspace, _project_dir) =
        setup_workspace_with_project(&tmp, &[("local/org/existing", &remote_url)]);

    // Pre-clone the repo so it already exists on disk.
    let repo_dir = workspace.join("local/org/existing");
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    let status = process::Command::new("git")
        .args([
            "clone",
            &bare.to_string_lossy(),
            &repo_dir.to_string_lossy(),
        ])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git clone failed");
    assert!(status.success());

    // Adding the same URL again should handle gracefully.
    let result = rwv()
        .args(["add", &remote_url])
        .current_dir(&workspace)
        .assert();

    let output = result.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    assert!(
        output.status.success()
            || combined.contains("already")
            || combined.contains("exists"),
        "adding an existing repo should succeed or give a clear message, got: {combined}"
    );
}

#[test]

fn add_invalid_url_errors_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    rwv()
        .args(["add", "not-a-valid-url-at-all"])
        .current_dir(&workspace)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("error")
                .or(predicate::str::contains("Error"))
                .or(predicate::str::contains("invalid"))
                .or(predicate::str::contains("Invalid"))
                .or(predicate::str::contains("unrecognized"))
                .or(predicate::str::contains("failed")),
        );
}

// ============================================================================
// rwv remove PATH — removes entry from manifest
// ============================================================================

#[test]

fn remove_path_removes_manifest_entry() {
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("to-remove.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    let repo_path = "local/org/to-remove";
    let (workspace, _project_dir) =
        setup_workspace_with_project(&tmp, &[(repo_path, &remote_url)]);

    // Clone the repo so it exists on disk.
    let repo_dir = workspace.join(repo_path);
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    let status = process::Command::new("git")
        .args([
            "clone",
            &bare.to_string_lossy(),
            &repo_dir.to_string_lossy(),
        ])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git clone failed");
    assert!(status.success());

    rwv()
        .args(["remove", repo_path])
        .current_dir(&workspace)
        .assert()
        .success();

    // The manifest should no longer contain the removed path.
    let manifest_path = workspace.join("projects/test-project/rwv.yaml");
    let manifest_content = std::fs::read_to_string(&manifest_path)
        .expect("rwv.yaml should still exist");
    assert!(
        !manifest_content.contains(repo_path),
        "manifest should not contain the removed repo path, got:\n{manifest_content}"
    );

    // The repo should still exist on disk (remove without --delete keeps files).
    assert!(
        repo_dir.exists(),
        "repo directory should still exist after remove (no --delete)"
    );
}

#[test]

fn remove_with_delete_flag_removes_clone() {
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("delete-me.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    let repo_path = "local/org/delete-me";
    let (workspace, _project_dir) =
        setup_workspace_with_project(&tmp, &[(repo_path, &remote_url)]);

    // Clone the repo so it exists on disk.
    let repo_dir = workspace.join(repo_path);
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    let status = process::Command::new("git")
        .args([
            "clone",
            &bare.to_string_lossy(),
            &repo_dir.to_string_lossy(),
        ])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git clone failed");
    assert!(status.success());

    rwv()
        .args(["remove", repo_path, "--delete"])
        .current_dir(&workspace)
        .assert()
        .success();

    // The manifest should no longer contain the removed path.
    let manifest_path = workspace.join("projects/test-project/rwv.yaml");
    let manifest_content = std::fs::read_to_string(&manifest_path)
        .expect("rwv.yaml should still exist");
    assert!(
        !manifest_content.contains(repo_path),
        "manifest should not contain the removed repo path, got:\n{manifest_content}"
    );

    // The repo directory should be deleted.
    assert!(
        !repo_dir.exists(),
        "repo directory should be deleted after remove --delete"
    );
}

#[test]

fn remove_nonexistent_path_errors_clearly() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    rwv()
        .args(["remove", "nonexistent/path/repo"])
        .current_dir(&workspace)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("error")
                .or(predicate::str::contains("Error"))
                .or(predicate::str::contains("not found"))
                .or(predicate::str::contains("does not exist"))
                .or(predicate::str::contains("no such")),
        );
}

// ============================================================================
// rwv add PATH --new — creates new repo via git init
// ============================================================================

#[test]
fn add_new_creates_git_repo_at_canonical_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    rwv()
        .args(["add", "github/myorg/newrepo", "--new"])
        .current_dir(&workspace)
        .assert()
        .success();

    // The directory should exist and be a git repo.
    let repo_dir = workspace.join("github/myorg/newrepo");
    assert!(
        repo_dir.exists(),
        "repo directory should be created at canonical path"
    );
    assert!(
        repo_dir.join(".git").exists(),
        "repo should be initialized as a git repo"
    );
}

#[test]
fn add_new_updates_manifest_with_inferred_url() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    rwv()
        .args(["add", "github/myorg/newrepo", "--new"])
        .current_dir(&workspace)
        .assert()
        .success();

    // The manifest should contain the new entry with an inferred URL.
    let manifest_path = workspace.join("projects/test-project/rwv.yaml");
    let manifest_content = std::fs::read_to_string(&manifest_path)
        .expect("rwv.yaml should exist after add --new");
    assert!(
        manifest_content.contains("github/myorg/newrepo"),
        "manifest should contain the repo path, got:\n{manifest_content}"
    );
    assert!(
        manifest_content.contains("https://github.com/myorg/newrepo.git"),
        "manifest should contain the inferred GitHub URL, got:\n{manifest_content}"
    );
}

#[test]
fn add_new_sets_role_to_primary() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    rwv()
        .args(["add", "github/myorg/newrepo", "--new"])
        .current_dir(&workspace)
        .assert()
        .success();

    let manifest_path = workspace.join("projects/test-project/rwv.yaml");
    let manifest_content = std::fs::read_to_string(&manifest_path)
        .expect("rwv.yaml should exist after add --new");

    // Find the entry for our repo and verify it has role: primary.
    // The YAML should contain "role: primary" in the newrepo entry.
    assert!(
        manifest_content.contains("role: primary"),
        "new repo should have role primary, got:\n{manifest_content}"
    );
}

#[test]
fn add_new_infers_url_for_github_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    rwv()
        .args(["add", "github/cwalv/repoweave", "--new"])
        .current_dir(&workspace)
        .assert()
        .success();

    let manifest_path = workspace.join("projects/test-project/rwv.yaml");
    let manifest_content = std::fs::read_to_string(&manifest_path)
        .expect("rwv.yaml should exist after add --new");
    assert!(
        manifest_content.contains("https://github.com/cwalv/repoweave.git"),
        "should infer GitHub HTTPS URL from path convention, got:\n{manifest_content}"
    );
}

#[test]
fn add_new_without_path_like_argument_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    // A bare name without slashes is not a valid path.
    rwv()
        .args(["add", "not-a-path", "--new"])
        .current_dir(&workspace)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("does not look like")
                .or(predicate::str::contains("Error"))
                .or(predicate::str::contains("error")),
        );
}

#[test]
fn add_new_with_two_segment_path_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    // Two segments (owner/repo) without registry prefix is not enough.
    rwv()
        .args(["add", "owner/repo", "--new"])
        .current_dir(&workspace)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("does not look like")
                .or(predicate::str::contains("Error"))
                .or(predicate::str::contains("error")),
        );
}

#[test]
fn add_new_with_unknown_registry_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    // A three-segment path with an unknown registry prefix should fail.
    rwv()
        .args(["add", "unknownhost/owner/repo", "--new"])
        .current_dir(&workspace)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("could not infer")
                .or(predicate::str::contains("Error"))
                .or(predicate::str::contains("error")),
        );
}

#[test]
fn add_new_existing_repo_in_manifest_handles_gracefully() {
    let tmp = tempfile::tempdir().unwrap();

    let repo_path = "github/myorg/existing";
    let (workspace, _project_dir) = setup_workspace_with_project(
        &tmp,
        &[(repo_path, "https://github.com/myorg/existing.git")],
    );

    // The repo is already in the manifest — adding with --new should not fail.
    rwv()
        .args(["add", repo_path, "--new"])
        .current_dir(&workspace)
        .assert()
        .success();
}
