//! E2E tests for `rwv add` and `rwv remove`.
//!
//! Tests that require the add/remove commands to be fully implemented are
//! marked `#[ignore]` until spec 6b lands the implementation.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

mod common;

/// Build a `Command` for the `rwv` binary.
fn rwv() -> Command {
    common::rwv()
}

/// Create a bare git repo at `path`.
fn init_bare_repo(path: &Path) {
    let status = common::git()
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
        let status = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("git command failed to start");
        assert!(status.success(), "git {:?} failed", args);
    };

    run(
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
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
///
/// Also writes `.rwv-active` pointing at the project so action verbs
/// resolve cleanly even when CWD is the workspace root. The
/// CWD-inside-projects/<name>/ override is gone, so without an active
/// project most commands would emit a helpful error instead of
/// proceeding.
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
        let status = common::git()
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

    // Make the project active so action-verb tests don't need to pass
    // --project explicitly.
    std::fs::write(workspace.join(".rwv-active"), "test-project\n").unwrap();

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
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
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
    let manifest_content =
        std::fs::read_to_string(&manifest_path).expect("rwv.yaml should exist after add");
    assert!(
        manifest_content.contains(&remote_url) || manifest_content.contains("file://"),
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
    let manifest_content =
        std::fs::read_to_string(&manifest_path).expect("rwv.yaml should exist after add");
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
    let status = common::git()
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
        output.status.success() || combined.contains("already") || combined.contains("exists"),
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
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[(repo_path, &remote_url)]);

    // Clone the repo so it exists on disk.
    let repo_dir = workspace.join(repo_path);
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    let status = common::git()
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
    let manifest_content =
        std::fs::read_to_string(&manifest_path).expect("rwv.yaml should still exist");
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
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[(repo_path, &remote_url)]);

    // Clone the repo so it exists on disk.
    let repo_dir = workspace.join(repo_path);
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    let status = common::git()
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
    let manifest_content =
        std::fs::read_to_string(&manifest_path).expect("rwv.yaml should still exist");
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
    let manifest_content =
        std::fs::read_to_string(&manifest_path).expect("rwv.yaml should exist after add --new");
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
    let manifest_content =
        std::fs::read_to_string(&manifest_path).expect("rwv.yaml should exist after add --new");

    // Find the entry for our repo and verify it has role: owned.
    // The YAML should contain "role: owned" in the newrepo entry.
    assert!(
        manifest_content.contains("role: owned"),
        "new repo should have role owned, got:\n{manifest_content}"
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
    let manifest_content =
        std::fs::read_to_string(&manifest_path).expect("rwv.yaml should exist after add --new");
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

// ============================================================================
// fork role -> remote name convention
// ============================================================================

/// Return the URL of the named remote in `repo`, if it exists.
fn remote_url(repo: &Path, name: &str) -> Option<String> {
    let out = common::git()
        .args(["remote", "get-url", name])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[test]
fn add_fork_clones_with_origin_remote() {
    // Fork now means URL = writable fork; rwv clones to `origin` like every
    // other role. The `upstream` remote convention is gone.
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("fork-src.git");
    init_bare_repo_with_commit(&bare);
    let remote_url_str = format!("file://{}", bare.display());

    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    rwv()
        .args(["add", &remote_url_str, "--role=fork"])
        .current_dir(&workspace)
        .assert()
        .success();

    // Find the cloned repo under the workspace by scanning for one with the
    // matching remote URL — the canonical path depends on URL parsing.
    let cloned = find_cloned_repo(&workspace, &bare);
    let origin = remote_url(&cloned, "origin");
    assert!(
        origin.is_some(),
        "role=fork clone should have an `origin` remote, found none at {}",
        cloned.display()
    );
    assert!(
        remote_url(&cloned, "upstream").is_none(),
        "role=fork clone must NOT have an `upstream` remote (old convention gone)"
    );
}

#[test]
fn add_owned_clones_with_origin_remote() {
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("owned-src.git");
    init_bare_repo_with_commit(&bare);
    let remote_url_str = format!("file://{}", bare.display());

    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    rwv()
        .args(["add", &remote_url_str, "--role=owned"])
        .current_dir(&workspace)
        .assert()
        .success();

    let cloned = find_cloned_repo(&workspace, &bare);
    assert!(
        remote_url(&cloned, "origin").is_some(),
        "role=owned clone should have an `origin` remote"
    );
    assert!(
        remote_url(&cloned, "upstream").is_none(),
        "role=owned clone should NOT have an `upstream` remote"
    );
}

/// The back-compat clap alias on `--role primary` is gone. The CLI now
/// rejects the legacy spelling outright and the error must direct users
/// at `rwv doctor --fix` so the migration path is discoverable from the
/// verb that emitted the error.
#[test]
fn add_primary_cli_alias_no_longer_accepted_with_doctor_hint() {
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("legacy-primary-src.git");
    init_bare_repo_with_commit(&bare);
    let remote_url_str = format!("file://{}", bare.display());

    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, &[]);

    let assertion = rwv()
        .args(["add", &remote_url_str, "--role=primary"])
        .current_dir(&workspace)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).into_owned();
    // clap's enum mismatch error mentions the unknown value; we also want
    // the user to see the doctor-fix migration path. Clap emits the
    // ValueEnum error; we check for the bare-bones signal here.
    assert!(
        stderr.to_lowercase().contains("primary"),
        "error should name the rejected value, got:\n{stderr}"
    );
}

/// Locate the directory rwv cloned a given bare source into by scanning the
/// workspace for git repos whose `origin` matches.
fn find_cloned_repo(workspace: &Path, bare: &Path) -> std::path::PathBuf {
    let want = format!("file://{}", bare.display());
    let want_alt = bare.to_string_lossy().into_owned();
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if p.join(".git").exists() {
                        out.push(p.clone());
                    }
                    // Skip recursing into .git dirs.
                    if p.file_name().and_then(|n| n.to_str()) != Some(".git") {
                        walk(&p, out);
                    }
                }
            }
        }
    }
    let mut repos = Vec::new();
    walk(workspace, &mut repos);
    for r in &repos {
        for remote in ["origin", "upstream"] {
            if let Some(u) = remote_url(r, remote) {
                if u == want || u == want_alt {
                    return r.clone();
                }
            }
        }
    }
    panic!(
        "could not find a cloned repo under {} pointing at {}; found repos: {:?}",
        workspace.display(),
        bare.display(),
        repos
    );
}

// ============================================================================
// `rwv add` must target CWD's workspace's rwv.yaml
//
// The bug: `rwv add` always wrote to primary's manifest, even when
// invoked from inside a workweave. Per-workspace ownership extends from
// rwv.lock to rwv.yaml — both are tracked files in the project repo and
// follow the same `active_path()` resolution rule `rwv lock` already
// uses. Conceptual reference:
// `docs/explanation/joints/lock-as-derived.md`.
// ============================================================================

/// Build a workspace plus a workweave directory ready for `rwv add` testing.
///
/// Layout produced (mirroring what `rwv workweave create` would build):
///   {tmp}/ws/                                  -- primary workspace root
///   {tmp}/ws/github/                           -- registry marker
///   {tmp}/ws/projects/test-project/rwv.yaml    -- primary's manifest (initially empty)
///   {tmp}/ws/.rwv-active                       -- "test-project"
///   {tmp}/.workweaves/test-project--feat/
///     .rwv-workweave                           -- marker pointing at primary
///     .rwv-active                              -- "test-project"
///     projects/test-project/rwv.yaml           -- workweave's manifest (initially empty)
///     github/                                  -- registry marker (so workweave resolves)
///
/// The project dir in primary is a git repo so workspace resolution succeeds
/// and the activation step can find rwv.yaml committed.
///
/// Returns (primary_root, workweave_dir).
fn setup_workweave_for_add_tests(
    tmp: &tempfile::TempDir,
) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::process::Stdio;
    let primary = tmp.path().join("ws");
    std::fs::create_dir_all(primary.join("github")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let primary_project_dir = primary.join("projects").join("test-project");
    std::fs::create_dir_all(&primary_project_dir).unwrap();

    let git_run = |args: &[&str], cwd: &Path| {
        let status = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git command failed");
        assert!(status.success(), "git {:?} failed", args);
    };
    git_run(&["init", "--initial-branch=main"], &primary_project_dir);
    git_run(
        &["config", "user.email", "test@test.com"],
        &primary_project_dir,
    );
    git_run(&["config", "user.name", "Test"], &primary_project_dir);
    write_manifest(&primary_project_dir, &[]);
    git_run(&["add", "rwv.yaml"], &primary_project_dir);
    git_run(&["commit", "-m", "init"], &primary_project_dir);

    std::fs::write(primary.join(".rwv-active"), "test-project\n").unwrap();

    // Workweave directory — mirror what `rwv workweave create` produces.
    let workweaves_parent = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&workweaves_parent).unwrap();
    let workweave_dir = workweaves_parent.join("test-project--feat");
    std::fs::create_dir_all(workweave_dir.join("github")).unwrap();

    // Marker file pointing at primary (canonical, so resolve matches).
    let primary_canonical = primary.canonicalize().unwrap();
    let marker = format!(
        "primary: {p}\nproject: test-project\nparent: {p}\n",
        p = primary_canonical.display()
    );
    std::fs::write(workweave_dir.join(".rwv-workweave"), marker).unwrap();
    std::fs::write(workweave_dir.join(".rwv-active"), "test-project\n").unwrap();

    // Workweave's own copy of the project dir (its own git repo to mirror
    // the worktree contract — a worktree of primary's project repo. A plain
    // copy with `git init` here is enough: the workweave's project repo
    // just needs to be writable independently of primary's, and the test
    // observes the rwv.yaml file directly).
    let workweave_project_dir = workweave_dir.join("projects").join("test-project");
    std::fs::create_dir_all(&workweave_project_dir).unwrap();
    git_run(&["init", "--initial-branch=main"], &workweave_project_dir);
    git_run(
        &["config", "user.email", "test@test.com"],
        &workweave_project_dir,
    );
    git_run(&["config", "user.name", "Test"], &workweave_project_dir);
    write_manifest(&workweave_project_dir, &[]);
    git_run(&["add", "rwv.yaml"], &workweave_project_dir);
    git_run(&["commit", "-m", "init"], &workweave_project_dir);

    (primary, workweave_dir)
}

#[test]
fn add_from_primary_cwd_writes_to_primary_rwv_yaml() {
    // Regression: `rwv add` from primary's CWD still writes to primary's
    // manifest (the unchanged baseline behaviour).
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("primary-add.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    let (primary, _workweave_dir) = setup_workweave_for_add_tests(&tmp);

    rwv()
        .args(["add", &remote_url])
        .current_dir(&primary)
        .assert()
        .success();

    // Primary's rwv.yaml must contain the new entry.
    let primary_manifest =
        std::fs::read_to_string(primary.join("projects/test-project/rwv.yaml")).unwrap();
    assert!(
        primary_manifest.contains("file://") || primary_manifest.contains(&remote_url),
        "primary's rwv.yaml should contain the added repo url, got:\n{primary_manifest}"
    );
}

#[test]
fn add_from_workweave_cwd_writes_to_workweave_rwv_yaml_not_primary() {
    // `rwv add` from a workweave's CWD must mutate the workweave's own
    // rwv.yaml, leaving primary's unchanged. This mirrors `rwv lock`'s
    // existing per-workspace resolution; manifest and lock are siblings
    // in the project repo and follow the same `active_path()` rule.
    // See `docs/explanation/joints/lock-as-derived.md`.
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("workweave-add.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    let (primary, workweave_dir) = setup_workweave_for_add_tests(&tmp);

    // Snapshot primary's manifest before — it must not be mutated below.
    let primary_manifest_path = primary.join("projects/test-project/rwv.yaml");
    let primary_before = std::fs::read_to_string(&primary_manifest_path).unwrap();

    rwv()
        .args(["add", &remote_url])
        .current_dir(&workweave_dir)
        .assert()
        .success();

    // Workweave's rwv.yaml must contain the new entry.
    let workweave_manifest_path = workweave_dir.join("projects/test-project/rwv.yaml");
    let workweave_manifest = std::fs::read_to_string(&workweave_manifest_path).unwrap();
    assert!(
        workweave_manifest.contains("file://") || workweave_manifest.contains(&remote_url),
        "workweave's rwv.yaml should contain the added repo url after add-from-workweave, got:\n{workweave_manifest}"
    );

    // Primary's rwv.yaml must NOT have been touched.
    let primary_after = std::fs::read_to_string(&primary_manifest_path).unwrap();
    assert_eq!(
        primary_before, primary_after,
        "primary's rwv.yaml must be untouched by `rwv add` from a workweave; \
         before:\n{primary_before}\nafter:\n{primary_after}"
    );
    assert!(
        !primary_after.contains(&remote_url) && !primary_after.contains("file://"),
        "primary's rwv.yaml must not contain the added repo url, got:\n{primary_after}"
    );
}

#[test]
fn add_from_workweave_clones_to_primary_canonical_path() {
    // The clone destination stays at primary's `github/<owner>/<repo>/`
    // even when add runs from a workweave: the canonical store is the
    // primary-side artifact; workweaves link into it via git worktree.
    // See `docs/explanation/joints/clone-topology.md` (invariant I1).
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("clone-target.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    let (primary, workweave_dir) = setup_workweave_for_add_tests(&tmp);

    rwv()
        .args(["add", &remote_url])
        .current_dir(&workweave_dir)
        .assert()
        .success();

    // The canonical clone exists under primary, matching its remote.
    let cloned = find_cloned_repo(&primary, &bare);
    assert!(
        cloned.starts_with(&primary),
        "clone must live under primary's path, got {} (primary: {})",
        cloned.display(),
        primary.display()
    );
}

#[test]
fn add_from_workweave_creates_worktree_at_workweave() {
    // Acceptance #4: from a workweave, `rwv add` must materialize the new
    // repo as a worktree at the workweave so the operator's CWD sees the
    // repo immediately (no separate sync step required for the add-from-
    // workweave flow).
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("wt-target.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    let (primary, workweave_dir) = setup_workweave_for_add_tests(&tmp);

    rwv()
        .args(["add", &remote_url])
        .current_dir(&workweave_dir)
        .assert()
        .success();

    // Locate the cloned repo at primary so we know the canonical path.
    let canonical = find_cloned_repo(&primary, &bare);
    let rel = canonical
        .strip_prefix(&primary)
        .expect("canonical clone lives under primary");

    // The same relative path should exist as a worktree at the workweave.
    let workweave_repo = workweave_dir.join(rel);
    assert!(
        workweave_repo.exists(),
        "after add-from-workweave, the new repo must exist at {} (workweave's worktree path)",
        workweave_repo.display()
    );
    // It must be a git worktree, not just a directory. Worktrees have a
    // `.git` *file* (the gitdir pointer) rather than a directory.
    let dot_git = workweave_repo.join(".git");
    assert!(
        dot_git.exists(),
        "workweave's add-materialized path must contain .git; got missing at {}",
        dot_git.display()
    );
    assert!(
        dot_git.is_file(),
        "workweave's add-materialized path must be a worktree (.git as file), \
         not an independent clone (.git as directory): {}",
        dot_git.display()
    );
}

#[test]
fn add_from_workweave_does_not_modify_primary_rwv_active() {
    // Side-effect regression: when running `rwv add` from a workweave, the
    // activation step must not clobber primary's .rwv-active (or its
    // ecosystem symlinks). The activation pass operates on the workweave's
    // own .rwv-active when CWD is in a workweave.
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("active-target.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    let (primary, workweave_dir) = setup_workweave_for_add_tests(&tmp);

    let primary_active_before = std::fs::read_to_string(primary.join(".rwv-active")).unwrap();

    rwv()
        .args(["add", &remote_url])
        .current_dir(&workweave_dir)
        .assert()
        .success();

    // Primary's .rwv-active should be untouched.
    let primary_active_after = std::fs::read_to_string(primary.join(".rwv-active")).unwrap();
    assert_eq!(
        primary_active_before, primary_active_after,
        "primary's .rwv-active must be untouched by `rwv add` from a workweave"
    );

    // Workweave's own .rwv-active still names the right project.
    let workweave_active = std::fs::read_to_string(workweave_dir.join(".rwv-active")).unwrap();
    assert_eq!(workweave_active.trim(), "test-project");
}

// ============================================================================
// fo-hycb06.7 — clone placement: git-common-dir + ephemeral branch
//
// These tests are the acceptance criteria for the fo-a0spgj regression
// scenario: canonical clone lives under the primary weave (not under
// .workweaves/), the workweave's copy is a linked worktree whose
// git-common-dir resolves back to the canonical clone, and the worktree
// is on the expected ephemeral branch `{project}--{weave}/{branch}`.
//
// Covered arms:
//   URL arm:        `rwv add <file://…>` from a workweave
//   local-path arm: `rwv add <github/owner/repo>` (path-as-arg to an
//                   existing clone) from a workweave
// ============================================================================

/// Read `git rev-parse --git-common-dir` in `repo`, returning the
/// canonical (resolved) path it points at.  For a linked worktree the
/// git-common-dir is inside the canonical clone's `.git/worktrees/…`
/// directory; canonicalizing and then stripping that suffix gives us the
/// canonical clone root.  For a plain clone `--git-common-dir` is just
/// `.git` relative to the repo directory, which resolves to the same place.
///
/// Returns the canonical path of the *git object store root* — i.e. the
/// `.git` directory of the canonical clone.
fn git_common_dir(repo: &std::path::Path) -> std::path::PathBuf {
    let out = common::git()
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(repo)
        .output()
        .expect("git rev-parse --git-common-dir should run");
    assert!(
        out.status.success(),
        "git rev-parse --git-common-dir failed in {}: {}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8(out.stdout)
        .expect("valid UTF-8")
        .trim()
        .to_string();
    // --git-common-dir can be relative to the repo directory; canonicalize
    // resolves that, plus any symlinks.
    let joined = if std::path::Path::new(&raw).is_absolute() {
        std::path::PathBuf::from(&raw)
    } else {
        repo.join(&raw)
    };
    joined.canonicalize().unwrap_or_else(|_| joined.clone())
}

/// Read the current branch name of `repo` via `git symbolic-ref --short HEAD`.
/// Returns `None` when HEAD is detached.
fn current_branch(repo: &std::path::Path) -> Option<String> {
    let out = common::git()
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(repo)
        .output()
        .expect("git symbolic-ref should run");
    if !out.status.success() {
        return None; // detached HEAD
    }
    Some(
        String::from_utf8(out.stdout)
            .expect("valid UTF-8")
            .trim()
            .to_string(),
    )
}

#[test]
fn add_url_arm_from_workweave_git_common_dir_points_to_primary_clone() {
    // Acceptance criterion (fo-hycb06.7):
    // After `rwv add <url>` from a workweave:
    //   1. The canonical clone lives under primary (not .workweaves/).
    //   2. The workweave's copy has git-common-dir pointing into the
    //      canonical clone's .git directory — confirming it is a linked
    //      worktree, not an independent clone.
    //   3. The worktree is on the ephemeral branch
    //      `test-project--feat/main`.
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("url-gcdir.git");
    init_bare_repo_with_commit(&bare);
    let remote_url = format!("file://{}", bare.display());

    let (primary, workweave_dir) = setup_workweave_for_add_tests(&tmp);

    rwv()
        .args(["add", &remote_url])
        .current_dir(&workweave_dir)
        .assert()
        .success();

    // Locate the canonical clone under primary.
    let canonical = find_cloned_repo(&primary, &bare);
    assert!(
        canonical.starts_with(&primary),
        "canonical clone must be under primary ({}), got {}",
        primary.display(),
        canonical.display()
    );

    // The relative path of the clone within primary.
    let rel = canonical.strip_prefix(&primary).expect("under primary");

    // The same relative path should exist inside the workweave.
    let workweave_repo = workweave_dir.join(rel);
    assert!(
        workweave_repo.exists(),
        "worktree must exist in workweave at {}",
        workweave_repo.display()
    );

    // 1. .git must be a file (linked worktree), not a directory (clone).
    let dot_git = workweave_repo.join(".git");
    assert!(
        dot_git.is_file(),
        ".git in workweave must be a file (linked worktree), got: {}",
        dot_git.display()
    );

    // 2. git-common-dir must be inside the canonical clone's .git, not
    //    inside the workweave directory.
    let common_dir = git_common_dir(&workweave_repo);
    let canonical_git = canonical.join(".git").canonicalize().unwrap();
    assert!(
        common_dir.starts_with(&canonical_git),
        "git-common-dir ({}) must be inside the canonical clone's .git ({})",
        common_dir.display(),
        canonical_git.display()
    );

    // 3. Ephemeral branch must follow the {project}--{weave}/{branch} pattern.
    // Workweave name is "feat", project is "test-project", branch from bare is "main".
    let branch =
        current_branch(&workweave_repo).expect("worktree should have a branch (not detached HEAD)");
    assert_eq!(
        branch, "test-project--feat/main",
        "worktree in workweave must be on ephemeral branch test-project--feat/main, got: {branch}"
    );

    // 4. No repo was cloned under the workweave directory.
    // The workweave's github/ entry must be a worktree file, not a .git dir.
    let workweave_github = workweave_dir.join("github");
    if workweave_github.exists() {
        // Any repo found under workweave/github/ must be a linked worktree,
        // not an independent clone — validate by checking .git is a file.
        fn assert_no_independent_clones(dir: &std::path::Path) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        let dg = p.join(".git");
                        if dg.exists() {
                            assert!(
                                dg.is_file(),
                                "expected linked worktree (.git as file) under workweave, \
                                 found independent clone (.git as dir) at {}",
                                p.display()
                            );
                        }
                        assert_no_independent_clones(&p);
                    }
                }
            }
        }
        assert_no_independent_clones(&workweave_github);
    }
}

#[test]
fn add_local_path_arm_from_workweave_git_common_dir_points_to_primary_clone() {
    // Acceptance criterion (fo-hycb06.7), local-path arm:
    // `rwv add <github/owner/repo>` (path to an existing clone under primary)
    // from a workweave must produce the same placement guarantee as the URL
    // arm — canonical clone stays in primary, workweave gets a linked
    // worktree with git-common-dir pointing to the canonical clone.
    let tmp = tempfile::tempdir().unwrap();

    let bare = tmp.path().join("localpath-gcdir.git");
    init_bare_repo_with_commit(&bare);

    let (primary, workweave_dir) = setup_workweave_for_add_tests(&tmp);

    // Pre-clone the repo into primary at the canonical path so the
    // local-path arm triggers (the condition is "!url.contains("://") &&
    // ctx.primary_path().join(url) exists as a directory").
    // Use two-segment path so it lands under primary/bar/repo/.
    let canonical_rel = "bar/localpath-gcdir";
    let canonical = primary.join(canonical_rel);
    std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
    let status = common::git()
        .args([
            "clone",
            &bare.to_string_lossy(),
            &canonical.to_string_lossy(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git clone failed");
    assert!(status.success(), "pre-clone of bare repo failed");

    // Run `rwv add <canonical_rel>` from the workweave (local-path arm).
    rwv()
        .args(["add", canonical_rel])
        .current_dir(&workweave_dir)
        .assert()
        .success();

    // The canonical clone must still be at primary/bar/localpath-gcdir/.
    assert!(
        canonical.exists(),
        "canonical clone must exist at primary/{canonical_rel}"
    );

    // The same relative path must exist in the workweave.
    let workweave_repo = workweave_dir.join(canonical_rel);
    assert!(
        workweave_repo.exists(),
        "worktree must exist in workweave at {}",
        workweave_repo.display()
    );

    // .git must be a file (linked worktree).
    let dot_git = workweave_repo.join(".git");
    assert!(
        dot_git.is_file(),
        ".git in workweave (local-path arm) must be a file (linked worktree), \
         found a directory (independent clone) at {}",
        dot_git.display()
    );

    // git-common-dir must be inside the canonical clone's .git directory.
    let common_dir = git_common_dir(&workweave_repo);
    let canonical_git = canonical.join(".git").canonicalize().unwrap();
    assert!(
        common_dir.starts_with(&canonical_git),
        "git-common-dir ({}) must be inside the canonical clone's .git ({}) [local-path arm]",
        common_dir.display(),
        canonical_git.display()
    );

    // Ephemeral branch: {project}--{weave}/{branch}.
    let branch = current_branch(&workweave_repo).expect("worktree should have a branch");
    assert_eq!(
        branch, "test-project--feat/main",
        "worktree (local-path arm) must be on ephemeral branch test-project--feat/main, got: {branch}"
    );

    // No independent clone materialized under .workweaves/ (the workweave
    // directory). Check that bar/ under workweave only contains a worktree.
    let workweave_bar = workweave_dir.join("bar");
    if workweave_bar.exists() {
        let ww_repo_dg = workweave_bar.join("localpath-gcdir/.git");
        if ww_repo_dg.exists() {
            assert!(
                ww_repo_dg.is_file(),
                "no independent clone should exist under workweave bar/; \
                 .git should be a file (worktree), got dir: {}",
                ww_repo_dg.display()
            );
        }
    }
}
