//! E2E tests for `rwv fetch`.
//!
//! These tests exercise the CLI binary via `assert_cmd`. Tests that require
//! the fetch command to be fully implemented are marked `#[ignore]`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

mod common;

/// Build a `Command` for the `rwv` binary.
fn rwv() -> Command {
    common::rwv()
}

/// Create a bare git repo at `path` to serve as a local "remote".
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

    // Create a temporary working clone, make a commit, push to the bare repo.
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

/// Write an `rwv.yaml` manifest pointing repos at the given bare repo URL.
fn write_manifest(dir: &Path, repos: &[(&str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: primary\n"
        ));
    }
    std::fs::write(dir.join("rwv.yaml"), &yaml).unwrap();
}

// ============================================================================
// Basic CLI plumbing
// ============================================================================

#[test]
fn fetch_requires_source_argument() {
    rwv()
        .arg("fetch")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

#[test]
fn fetch_accepts_source_argument() {
    // With a non-existent source, fetch should fail with a clear error.
    rwv()
        .args(["fetch", "some-source"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("error")
                .or(predicate::str::contains("Error"))
                .or(predicate::str::contains("not found")),
        );
}

// ============================================================================
// Fetch with a valid project source — clones project repo and listed repos
// ============================================================================

#[test]
fn fetch_clones_project_and_repos() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // Set up two bare repos to act as remotes.
    let bare_server = tmp.path().join("bare_server.git");
    let bare_client = tmp.path().join("bare_client.git");
    init_bare_repo_with_commit(&bare_server);
    init_bare_repo_with_commit(&bare_client);

    // Set up the "project source" — a bare repo containing rwv.yaml.
    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    // Clone the project bare repo, add an rwv.yaml, push.
    let project_work = tmp.path().join("project_work");
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

    run(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &project_work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &project_work);
    run(&["config", "user.name", "Test"], &project_work);

    // Use file:// URLs so no network access is needed.
    let server_url = format!("file://{}", bare_server.display());
    let client_url = format!("file://{}", bare_client.display());
    write_manifest(
        &project_work,
        &[
            ("local/org/server", &server_url),
            ("local/org/client", &client_url),
        ],
    );
    run(&["add", "rwv.yaml"], &project_work);
    run(&["commit", "-m", "add manifest"], &project_work);
    run(&["push", "origin", "main"], &project_work);

    // Run `rwv fetch` pointing at the project bare repo.
    let project_source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &project_source])
        .current_dir(&workspace)
        .assert()
        .success();

    // Verify the repos were cloned to canonical paths.
    assert!(
        workspace.join("local/org/server").exists(),
        "server repo should be cloned"
    );
    assert!(
        workspace.join("local/org/client").exists(),
        "client repo should be cloned"
    );
}

// ============================================================================
// Directory structure after fetch
// ============================================================================

#[test]
fn fetch_creates_project_dir_with_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // Set up a bare repo for the project source with an rwv.yaml.
    let bare_repo = tmp.path().join("remote.git");
    init_bare_repo(&bare_repo);

    let work = tmp.path().join("work");
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

    run(
        &[
            "clone",
            &bare_repo.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);
    write_manifest(&work, &[]);
    run(&["add", "rwv.yaml"], &work);
    run(&["commit", "-m", "manifest"], &work);
    run(&["push", "origin", "main"], &work);

    let source = format!("file://{}", bare_repo.display());
    rwv()
        .args(["fetch", &source])
        .current_dir(&workspace)
        .assert()
        .success();

    // The project directory should exist under projects/ and contain rwv.yaml.
    let projects_dir = workspace.join("projects");
    assert!(projects_dir.exists(), "projects/ directory should exist");

    // Find the project subdirectory (name derived from source).
    let entries: Vec<_> = std::fs::read_dir(&projects_dir)
        .expect("should be able to read projects/")
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "projects/ should contain at least one project"
    );

    let project_dir = entries[0].path();
    assert!(
        project_dir.join("rwv.yaml").exists(),
        "project directory should contain rwv.yaml"
    );
}

#[test]
fn fetch_repos_at_canonical_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare_repo = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare_repo);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
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

    run(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    let dep_url = format!("file://{}", bare_repo.display());
    write_manifest(&work, &[("local/team/dep", &dep_url)]);
    run(&["add", "rwv.yaml"], &work);
    run(&["commit", "-m", "manifest"], &work);
    run(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &source])
        .current_dir(&workspace)
        .assert()
        .success();

    // Repo should be at {registry}/{owner}/{repo}/ relative to workspace root.
    let repo_dir = workspace.join("local/team/dep");
    assert!(
        repo_dir.exists(),
        "repo should be at canonical path local/team/dep"
    );
    assert!(
        repo_dir.join(".git").exists() || repo_dir.join("HEAD").exists(),
        "canonical path should contain a git repository"
    );
}

// ============================================================================
// Fetch with already-existing workspace — graceful handling
// ============================================================================

#[test]
fn fetch_existing_workspace_handles_gracefully() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare_repo = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare_repo);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
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

    run(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    let dep_url = format!("file://{}", bare_repo.display());
    write_manifest(&work, &[("local/team/dep", &dep_url)]);
    run(&["add", "rwv.yaml"], &work);
    run(&["commit", "-m", "manifest"], &work);
    run(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());

    // First fetch — should succeed.
    rwv()
        .args(["fetch", &source])
        .current_dir(&workspace)
        .assert()
        .success();

    // Second fetch of the same source — the project directory already exists,
    // so rwv fetch must exit non-zero with an "already exists" error and a
    // scoped-path hint.
    let output = rwv()
        .args(["fetch", &source])
        .current_dir(&workspace)
        .output()
        .expect("rwv fetch should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");

    assert!(
        !output.status.success(),
        "second fetch of same source should fail (project already exists), got: {combined}"
    );
    assert!(
        combined.contains("already exists"),
        "second fetch should report 'already exists', got: {combined}"
    );
}

// ============================================================================
// Fetch with invalid/nonexistent source — clear error
// ============================================================================

#[test]
fn fetch_invalid_source_errors_clearly() {
    let tmp = tempfile::tempdir().unwrap();

    rwv()
        .args(["fetch", "file:///nonexistent/path/to/repo.git"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("error")
                .or(predicate::str::contains("Error"))
                .or(predicate::str::contains("not found")
                    .or(predicate::str::contains("failed"))
                    .or(predicate::str::contains("could not"))),
        );
}

#[test]
fn fetch_garbage_source_errors_clearly() {
    let tmp = tempfile::tempdir().unwrap();

    rwv()
        .args(["fetch", "not-a-valid-source-at-all"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("error")
                .or(predicate::str::contains("Error"))
                .or(predicate::str::contains("invalid"))
                .or(predicate::str::contains("Invalid"))
                .or(predicate::str::contains("not found"))
                .or(predicate::str::contains("failed")),
        );
}

// ============================================================================
// FetchMode enum tests (V6a)
// ============================================================================
//
// FetchMode controls how `rwv fetch` resolves repo versions:
//   - Default: fetch branch HEAD from `rwv.yaml`, update `rwv.lock` with SHAs
//   - Locked:  check out exact revisions from `rwv.lock`
//   - Frozen:  like Locked, but error if lock is missing or stale

#[test]
fn fetch_mode_default_updates_lock() {
    // Default fetch should clone repos at branch HEAD from rwv.yaml and then
    // write/update rwv.lock with the resolved SHAs.
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare_repo = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare_repo);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
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

    run(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    let dep_url = format!("file://{}", bare_repo.display());
    write_manifest(&work, &[("local/team/dep", &dep_url)]);
    run(&["add", "rwv.yaml"], &work);
    run(&["commit", "-m", "manifest"], &work);
    run(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &source])
        .current_dir(&workspace)
        .assert()
        .success();

    // After default fetch, rwv.lock should exist in the project directory.
    let lock_path = workspace.join("projects/project/rwv.lock");
    assert!(
        lock_path.exists(),
        "default fetch should create/update rwv.lock"
    );

    let lock_content = std::fs::read_to_string(&lock_path).unwrap();
    assert!(
        lock_content.contains("local/team/dep"),
        "lock should reference the fetched repo"
    );
    assert!(
        lock_content.contains("version:"),
        "lock should contain pinned versions"
    );
}

#[test]
fn fetch_default_auto_activates_project() {
    // Default fetch should auto-activate the project (write .rwv-active,
    // generate ecosystem files, create symlinks).
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare_repo = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare_repo);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
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

    run(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    let dep_url = format!("file://{}", bare_repo.display());
    write_manifest(&work, &[("local/team/dep", &dep_url)]);
    run(&["add", "rwv.yaml"], &work);
    run(&["commit", "-m", "manifest"], &work);
    run(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &source])
        .current_dir(&workspace)
        .assert()
        .success();

    // .rwv-active should be written at workspace root.
    let active_path = workspace.join(".rwv-active");
    assert!(
        active_path.exists(),
        "fetch should auto-activate the project (write .rwv-active)"
    );
    let active_content = std::fs::read_to_string(&active_path).unwrap();
    assert!(
        active_content.contains("project"),
        ".rwv-active should reference the fetched project name"
    );
}

// ============================================================================
// Default fetch reads rwv.lock (pins to lock SHA, does NOT bump to HEAD)
// ============================================================================

#[test]
fn fetch_default_reads_lock_and_does_not_bump_it() {
    // Default `rwv fetch` reads rwv.lock and aligns each repo to the lock
    // SHA, NOT the manifest's branch HEAD. The lock file itself must not
    // be mutated when the lock already covers every manifest entry —
    // that is the headline guarantee of fo-zvxff.
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare_repo = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare_repo);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
    let run = |args: &[&str], cwd: &Path| -> String {
        let out = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::piped())
            .stderr(process::Stdio::null())
            .output()
            .expect("git command failed");
        assert!(out.status.success(), "git {:?} failed", args);
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    let run_quiet = |args: &[&str], cwd: &Path| {
        let status = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("git command failed");
        assert!(status.success(), "git {:?} failed", args);
    };

    run_quiet(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run_quiet(&["config", "user.email", "test@test.com"], &work);
    run_quiet(&["config", "user.name", "Test"], &work);

    let dep_url = format!("file://{}", bare_repo.display());
    write_manifest(&work, &[("local/team/dep", &dep_url)]);
    run_quiet(&["add", "rwv.yaml"], &work);
    run_quiet(&["commit", "-m", "manifest"], &work);

    // Get the first commit SHA from the bare repo (before any new commits).
    let dep_clone = tmp.path().join("dep_clone");
    run_quiet(
        &[
            "clone",
            &bare_repo.to_string_lossy(),
            &dep_clone.to_string_lossy(),
        ],
        tmp.path(),
    );
    let first_sha = run(&["rev-parse", "HEAD"], &dep_clone);

    // Write a lock file pinning the dep at first_sha.
    let lock_yaml = format!(
        "repositories:\n  local/team/dep:\n    type: git\n    url: {dep_url}\n    version: {first_sha}\n"
    );
    std::fs::write(work.join("rwv.lock"), &lock_yaml).unwrap();
    run_quiet(&["add", "rwv.lock"], &work);
    run_quiet(&["commit", "-m", "add lock"], &work);

    // Push a second commit to the bare repo so HEAD advances past the lock.
    run_quiet(&["config", "user.email", "test@test.com"], &dep_clone);
    run_quiet(&["config", "user.name", "Test"], &dep_clone);
    std::fs::write(dep_clone.join("second.txt"), "change").unwrap();
    run_quiet(&["add", "."], &dep_clone);
    run_quiet(&["commit", "-m", "second"], &dep_clone);
    run_quiet(&["push", "origin", "main"], &dep_clone);

    run_quiet(&["push", "origin", "main"], &work);

    // Capture the committed lock content so we can verify fetch did not
    // bump it to branch HEAD.
    let lock_before = std::fs::read_to_string(work.join("rwv.lock")).unwrap();

    // Default fetch: should check out dep at first_sha (the lock value),
    // not HEAD (the branch tip, which is now ahead).
    let source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &source])
        .current_dir(&workspace)
        .assert()
        .success();

    // Verify the cloned repo is at the locked SHA.
    let dep_dir = workspace.join("local/team/dep");
    assert!(dep_dir.exists(), "dep should be cloned");
    let checked_out_sha = run(&["rev-parse", "HEAD"], &dep_dir);
    assert_eq!(
        checked_out_sha, first_sha,
        "default fetch should check out the exact revision from rwv.lock"
    );

    // The committed lock fully covered the manifest, so fetch had nothing
    // to bootstrap or additively add — lock content must be byte-identical.
    let lock_after = std::fs::read_to_string(workspace.join("projects/project/rwv.lock")).unwrap();
    assert_eq!(
        lock_before, lock_after,
        "default fetch must not mutate rwv.lock when it already covers every manifest entry"
    );
}

// ============================================================================
// --frozen mode: error on missing or stale lock
// ============================================================================

#[test]
fn fetch_frozen_errors_on_missing_lock() {
    // --frozen should error if rwv.lock does not exist.
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare_repo = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare_repo);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
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

    run(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    let dep_url = format!("file://{}", bare_repo.display());
    write_manifest(&work, &[("local/team/dep", &dep_url)]);
    // Deliberately do NOT create rwv.lock.
    run(&["add", "rwv.yaml"], &work);
    run(&["commit", "-m", "manifest without lock"], &work);
    run(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &source, "--frozen"])
        .current_dir(&workspace)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("lock").and(
                predicate::str::contains("missing")
                    .or(predicate::str::contains("not found"))
                    .or(predicate::str::contains("does not exist")),
            ),
        );
}

#[test]
fn fetch_frozen_errors_on_stale_lock() {
    // --frozen should error if rwv.lock exists but doesn't match the manifest
    // (e.g., manifest has a repo that the lock file doesn't cover).
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare_repo = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare_repo);

    let bare_repo2 = tmp.path().join("dep2.git");
    init_bare_repo_with_commit(&bare_repo2);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
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

    run(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    let dep_url = format!("file://{}", bare_repo.display());
    let dep2_url = format!("file://{}", bare_repo2.display());

    // Manifest lists TWO repos.
    write_manifest(
        &work,
        &[("local/team/dep", &dep_url), ("local/team/dep2", &dep2_url)],
    );

    // Lock only covers ONE repo — stale.
    let lock_yaml = format!(
        "repositories:\n  local/team/dep:\n    type: git\n    url: {dep_url}\n    version: {}\n",
        "a".repeat(40)
    );
    std::fs::write(work.join("rwv.lock"), &lock_yaml).unwrap();

    run(&["add", "."], &work);
    run(&["commit", "-m", "stale lock"], &work);
    run(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &source, "--frozen"])
        .current_dir(&workspace)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("stale")
                .or(predicate::str::contains("mismatch"))
                .or(predicate::str::contains("does not match"))
                .or(predicate::str::contains("out of date")),
        );
}

#[test]
fn fetch_frozen_succeeds_with_valid_lock() {
    // --frozen should succeed when rwv.lock exists and covers all manifest repos.
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare_repo = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare_repo);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
    let run = |args: &[&str], cwd: &Path| -> String {
        let out = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::piped())
            .stderr(process::Stdio::null())
            .output()
            .expect("git command failed");
        assert!(out.status.success(), "git {:?} failed", args);
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let run_quiet = |args: &[&str], cwd: &Path| {
        let status = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("git command failed");
        assert!(status.success(), "git {:?} failed", args);
    };

    run_quiet(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run_quiet(&["config", "user.email", "test@test.com"], &work);
    run_quiet(&["config", "user.name", "Test"], &work);

    // Get the dep's HEAD SHA for the lock file.
    let dep_clone = tmp.path().join("dep_clone");
    run_quiet(
        &[
            "clone",
            &bare_repo.to_string_lossy(),
            &dep_clone.to_string_lossy(),
        ],
        tmp.path(),
    );
    let dep_sha = run(&["rev-parse", "HEAD"], &dep_clone);

    let dep_url = format!("file://{}", bare_repo.display());
    write_manifest(&work, &[("local/team/dep", &dep_url)]);

    // Write a valid lock that matches the manifest.
    let lock_yaml = format!(
        "repositories:\n  local/team/dep:\n    type: git\n    url: {dep_url}\n    version: {dep_sha}\n"
    );
    std::fs::write(work.join("rwv.lock"), &lock_yaml).unwrap();

    run_quiet(&["add", "."], &work);
    run_quiet(&["commit", "-m", "manifest+lock"], &work);
    run_quiet(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &source, "--frozen"])
        .current_dir(&workspace)
        .assert()
        .success();

    let dep_dir = workspace.join("local/team/dep");
    assert!(dep_dir.exists(), "dep should be cloned with --frozen");
}

// ============================================================================
// Second fetch of same project: collision error
// ============================================================================

#[test]
fn fetch_second_invocation_is_idempotent() {
    // Fetching the same project source twice collides on the project directory.
    // The second fetch must exit non-zero with an "already exists" error.
    // The first fetch activates the project; the active file must remain after
    // the failed second fetch.
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare_repo = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&bare_repo);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
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

    run(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    let dep_url = format!("file://{}", bare_repo.display());
    write_manifest(&work, &[("local/team/dep", &dep_url)]);
    run(&["add", "rwv.yaml"], &work);
    run(&["commit", "-m", "manifest"], &work);
    run(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());

    // First fetch — succeeds and activates the project.
    rwv()
        .args(["fetch", &source])
        .current_dir(&workspace)
        .assert()
        .success();

    assert!(
        workspace.join(".rwv-active").exists(),
        "project should be activated after first fetch"
    );

    // Second fetch of the same source — must fail with collision error.
    let output = rwv()
        .args(["fetch", &source])
        .current_dir(&workspace)
        .output()
        .expect("rwv fetch should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");

    assert!(
        !output.status.success(),
        "second fetch of same source should fail (project already exists), got: {combined}"
    );
    assert!(
        combined.contains("already exists"),
        "second fetch should report 'already exists', got: {combined}"
    );

    // .rwv-active must still exist after the failed second fetch.
    assert!(
        workspace.join(".rwv-active").exists(),
        "project should remain activated after failed second fetch"
    );
}

// ============================================================================
// CLI flag plumbing: --frozen is recognized
// ============================================================================

#[test]
fn fetch_frozen_flag_is_recognized() {
    // `rwv fetch <source> --frozen` should not fail with "unrecognized argument".
    rwv()
        .args(["fetch", "some-source", "--frozen"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized").not());
}

// ============================================================================
// --no-reference: skip reference-role repos
// ============================================================================

/// Write a manifest with mixed primary/reference roles.
fn write_manifest_with_roles(dir: &Path, repos: &[(&str, &str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url, role) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: {role}\n"
        ));
    }
    std::fs::write(dir.join("rwv.yaml"), &yaml).unwrap();
}

#[test]
fn fetch_no_reference_skips_reference_role_repos() {
    // `rwv fetch <source> --no-reference` should clone primary/fork/dependency
    // repos but skip any repo with `role: reference`.
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let primary_bare = tmp.path().join("primary.git");
    init_bare_repo_with_commit(&primary_bare);
    let reference_bare = tmp.path().join("reference.git");
    init_bare_repo_with_commit(&reference_bare);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
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

    run(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    let primary_url = format!("file://{}", primary_bare.display());
    let reference_url = format!("file://{}", reference_bare.display());
    write_manifest_with_roles(
        &work,
        &[
            ("local/team/primary", &primary_url, "primary"),
            ("local/team/reference", &reference_url, "reference"),
        ],
    );
    run(&["add", "rwv.yaml"], &work);
    run(&["commit", "-m", "manifest"], &work);
    run(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &source, "--no-reference"])
        .current_dir(&workspace)
        .assert()
        .success();

    assert!(
        workspace.join("local/team/primary").exists(),
        "primary repo should be cloned"
    );
    assert!(
        !workspace.join("local/team/reference").exists(),
        "reference repo should be skipped with --no-reference"
    );
}

#[test]
fn fetch_frozen_no_reference_tolerates_reference_missing_from_lock() {
    // Regression test for find_stale_repos: with --frozen --no-reference,
    // a reference repo present in the manifest but missing from rwv.lock
    // must NOT cause failure — the user has explicitly opted out of
    // fetching reference repos, so missing lock entries for them are fine.
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let primary_bare = tmp.path().join("primary.git");
    init_bare_repo_with_commit(&primary_bare);
    let reference_bare = tmp.path().join("reference.git");
    init_bare_repo_with_commit(&reference_bare);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
    let run = |args: &[&str], cwd: &Path| -> String {
        let out = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::piped())
            .stderr(process::Stdio::null())
            .output()
            .expect("git command failed");
        assert!(out.status.success(), "git {:?} failed", args);
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let run_quiet = |args: &[&str], cwd: &Path| {
        let status = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("git command failed");
        assert!(status.success(), "git {:?} failed", args);
    };

    run_quiet(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run_quiet(&["config", "user.email", "test@test.com"], &work);
    run_quiet(&["config", "user.name", "Test"], &work);

    // Get the primary HEAD SHA so we can write a valid lock for it.
    let primary_clone = tmp.path().join("primary_clone");
    run_quiet(
        &[
            "clone",
            &primary_bare.to_string_lossy(),
            &primary_clone.to_string_lossy(),
        ],
        tmp.path(),
    );
    let primary_sha = run(&["rev-parse", "HEAD"], &primary_clone);

    let primary_url = format!("file://{}", primary_bare.display());
    let reference_url = format!("file://{}", reference_bare.display());
    write_manifest_with_roles(
        &work,
        &[
            ("local/team/primary", &primary_url, "primary"),
            ("local/team/reference", &reference_url, "reference"),
        ],
    );

    // Lock file covers ONLY the primary — reference is intentionally absent.
    // Without --no-reference this would be "stale"; with --no-reference,
    // find_stale_repos must skip the reference entry.
    let lock_yaml = format!(
        "repositories:\n  local/team/primary:\n    type: git\n    url: {primary_url}\n    version: {primary_sha}\n"
    );
    std::fs::write(work.join("rwv.lock"), &lock_yaml).unwrap();

    run_quiet(&["add", "."], &work);
    run_quiet(&["commit", "-m", "manifest+partial-lock"], &work);
    run_quiet(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &source, "--frozen", "--no-reference"])
        .current_dir(&workspace)
        .assert()
        .success();

    assert!(
        workspace.join("local/team/primary").exists(),
        "primary repo should be cloned with --frozen --no-reference"
    );
    assert!(
        !workspace.join("local/team/reference").exists(),
        "reference repo should be skipped"
    );
}

#[test]
fn fetch_frozen_without_no_reference_errors_when_reference_missing_from_lock() {
    // Sibling check for the test above: WITHOUT --no-reference, a reference
    // repo missing from the lock must still trigger the stale-lock error.
    // This guards against the find_stale_repos fix accidentally exempting
    // reference repos unconditionally.
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let primary_bare = tmp.path().join("primary.git");
    init_bare_repo_with_commit(&primary_bare);
    let reference_bare = tmp.path().join("reference.git");
    init_bare_repo_with_commit(&reference_bare);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);

    let work = tmp.path().join("work");
    let run_quiet = |args: &[&str], cwd: &Path| {
        let status = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("git command failed");
        assert!(status.success(), "git {:?} failed", args);
    };

    run_quiet(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp.path(),
    );
    run_quiet(&["config", "user.email", "test@test.com"], &work);
    run_quiet(&["config", "user.name", "Test"], &work);

    let primary_url = format!("file://{}", primary_bare.display());
    let reference_url = format!("file://{}", reference_bare.display());
    write_manifest_with_roles(
        &work,
        &[
            ("local/team/primary", &primary_url, "primary"),
            ("local/team/reference", &reference_url, "reference"),
        ],
    );

    // Lock covers only primary; reference absent.
    let lock_yaml = format!(
        "repositories:\n  local/team/primary:\n    type: git\n    url: {primary_url}\n    version: {}\n",
        "a".repeat(40)
    );
    std::fs::write(work.join("rwv.lock"), &lock_yaml).unwrap();

    run_quiet(&["add", "."], &work);
    run_quiet(&["commit", "-m", "manifest+partial-lock"], &work);
    run_quiet(&["push", "origin", "main"], &work);

    let source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &source, "--frozen"])
        .current_dir(&workspace)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("stale")
                .or(predicate::str::contains("mismatch"))
                .or(predicate::str::contains("does not match"))
                .or(predicate::str::contains("out of date"))
                .or(predicate::str::contains("missing")),
        );
}
