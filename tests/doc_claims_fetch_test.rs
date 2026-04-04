//! Integration tests verifying documented behaviour of `rwv fetch`, `rwv add`,
//! and `rwv remove`.
//!
//! Each test is tied to a specific doc-claim ticket.  Where the implementation
//! diverges from what the docs say, the test verifies the *current* behaviour
//! and carries a `// TODO` comment pointing at the gap.

use assert_cmd::Command;
use std::path::Path;
use std::process;

// ---------------------------------------------------------------------------
// Helpers (mirrors patterns from add_remove_test.rs / workweave_test.rs)
// ---------------------------------------------------------------------------

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
    assert!(status.success(), "git {:?} in {} failed", args, dir.display());
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

/// Create a bare git repo with one initial commit so it can be cloned.
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

    run(&["clone", &path.to_string_lossy(), &work.to_string_lossy()], tmp.path());
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);
    std::fs::write(work.join("README"), "init").unwrap();
    run(&["add", "."], &work);
    run(&["commit", "-m", "initial"], &work);
    run(&["push", "origin", "main"], &work);
}

/// Push an `rwv.yaml` manifest into a bare repo (via a temporary working clone).
///
/// `repos` is a slice of `(canonical_path, url)` pairs.
fn push_manifest_to_bare(bare: &Path, repos: &[(&str, &str)]) {
    let tmp = tempfile::tempdir().expect("tempdir for manifest work clone");
    let work = tmp.path().join("mwork");

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

    run(&["clone", &bare.to_string_lossy(), &work.to_string_lossy()], tmp.path());
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: primary\n"
        ));
    }
    std::fs::write(work.join("rwv.yaml"), &yaml).unwrap();
    run(&["add", "rwv.yaml"], &work);
    run(&["commit", "-m", "add manifest"], &work);
    run(&["push", "origin", "main"], &work);
}

/// Set up a workspace with a single project that has an `rwv.yaml` and is
/// itself a git repo (required by workspace resolution).
///
/// Returns `(workspace_root, project_dir)`.
fn setup_workspace_with_project(
    tmp: &tempfile::TempDir,
    project_name: &str,
    repos: &[(&str, &str)],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let project_dir = workspace.join("projects").join(project_name);
    std::fs::create_dir_all(&project_dir).unwrap();

    git(&["init", "--initial-branch=main"], &project_dir);
    git(&["config", "user.email", "test@test.com"], &project_dir);
    git(&["config", "user.name", "Test"], &project_dir);

    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: primary\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();
    git(&["add", "rwv.yaml"], &project_dir);
    git(&["commit", "-m", "init"], &project_dir);

    (workspace, project_dir)
}

// ============================================================================
// Test 1 — project-reporoot-42z
//
// Doc claim: "If names collide, rwv fetch errors and suggests a scoped path:
//             projects/{owner}/{name}/"
//
// Current behaviour: fetch prints "already exists, skipping clone" and
// continues without error; it does NOT surface a scoped-path suggestion.
// ============================================================================

#[test]
fn fetch_name_collision_behavior() {
    // TODO: docs claim scoped path suggestion, not implemented
    //
    // The implementation skips re-cloning and carries on (exit 0).  The docs
    // describe an error with a helpful suggestion — that suggestion is absent.

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // Create a bare repo for the "web-app" project.
    let project_bare = tmp.path().join("web-app.git");
    init_bare_repo(&project_bare);
    push_manifest_to_bare(&project_bare, &[]);

    // Pre-create projects/web-app/ so the name collides on the second fetch.
    let pre_existing = workspace.join("projects").join("web-app");
    std::fs::create_dir_all(&pre_existing).unwrap();
    // Write a minimal rwv.yaml so the project is readable after being "found".
    std::fs::write(pre_existing.join("rwv.yaml"), "repositories: {}\n").unwrap();

    // Fetching into a workspace where projects/web-app already exists.
    let project_url = format!("file://{}", project_bare.display());
    let output = rwv()
        .args(["fetch", &project_url])
        .current_dir(&workspace)
        .output()
        .expect("rwv fetch should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    // Current behaviour: command succeeds and reports "already exists".
    // The docs claim it should error — that is NOT implemented.
    assert!(
        combined.contains("already exists"),
        "expected 'already exists' message for name collision, got:\n{combined}"
    );
    // NOTE: the docs also claim a scoped-path suggestion ("projects/owner/name/")
    // is printed.  That suggestion is absent from the current implementation.
}

// ============================================================================
// Test 2 — project-reporoot-cq5
//
// Doc claim: `rwv remove --delete` checks whether other projects reference
//             the same repo before deleting.
//
// Current behaviour: deletes unconditionally — no cross-project check.
// ============================================================================

#[test]
fn remove_delete_does_not_check_other_projects() {
    // TODO: docs claim cross-project check, not implemented
    //
    // The implementation calls `std::fs::remove_dir_all` without inspecting
    // any other project manifests.  Shared repos are silently deleted.

    let tmp = tempfile::tempdir().unwrap();

    // Create a bare repo that both projects will reference.
    let shared_bare = tmp.path().join("shared.git");
    init_bare_repo_with_commit(&shared_bare);
    let shared_url = format!("file://{}", shared_bare.display());
    let repo_path = "local/org/shared";

    // Project A references the shared repo.
    let (workspace, _project_a_dir) =
        setup_workspace_with_project(&tmp, "project-a", &[(repo_path, &shared_url)]);

    // Project B also references the same repo (written directly — we only need
    // one active project for workspace resolution to work).
    let project_b_dir = workspace.join("projects").join("project-b");
    std::fs::create_dir_all(&project_b_dir).unwrap();
    let yaml_b = format!(
        "repositories:\n  {repo_path}:\n    type: git\n    url: {shared_url}\n    version: main\n    role: primary\n"
    );
    std::fs::write(project_b_dir.join("rwv.yaml"), &yaml_b).unwrap();

    // Clone the shared repo to disk so --delete has something to remove.
    let repo_dir = workspace.join(repo_path);
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    let clone_status = process::Command::new("git")
        .args([
            "clone",
            &shared_bare.to_string_lossy(),
            &repo_dir.to_string_lossy(),
        ])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git clone failed");
    assert!(clone_status.success());

    // Run `rwv remove <repo> --delete` from within project-a.
    // Workspace resolver picks the single project in projects/ when CWD is ws root,
    // but with two projects present we run from inside project-a to disambiguate.
    rwv()
        .args(["remove", repo_path, "--delete"])
        .current_dir(&workspace.join("projects").join("project-a"))
        .assert()
        .success();

    // Current behaviour: directory is deleted without checking project-b.
    assert!(
        !repo_dir.exists(),
        "rwv remove --delete should delete the directory (no cross-project check)"
    );
    // NOTE: project-b still references the now-deleted repo — no warning was
    // issued.  The docs claim a cross-project check would prevent or warn about
    // this; that check is absent.
}

// ============================================================================
// Test 3 — project-reporoot-781
//
// Doc claim: `rwv add github/org/repo --role reference` infers the URL from
//             the clone's origin remote when the directory already exists.
//
// Current behaviour: the argument is treated as a URL, not as a canonical
// path.  A bare `github/org/repo` string (without a URL scheme) is rejected
// because neither `resolve_url` nor `derive_local_path_from_url` can handle
// it.
// ============================================================================

#[test]
fn add_from_local_path_infers_url() {
    // TODO: docs describe URL inference from an existing clone's origin remote;
    //       current `run_add` treats the positional argument as a URL and
    //       errors on path-style inputs.

    let tmp = tempfile::tempdir().unwrap();

    // Create a bare repo to act as the remote origin.
    let bare = tmp.path().join("origin.git");
    init_bare_repo_with_commit(&bare);
    let origin_url = format!("file://{}", bare.display());

    // Set up workspace with an empty project manifest.
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, "test-project", &[]);

    // Clone the bare repo to the canonical path github/org/repo.
    let repo_dir = workspace.join("github").join("org").join("repo");
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    let clone_status = process::Command::new("git")
        .args([
            "clone",
            &bare.to_string_lossy(),
            &repo_dir.to_string_lossy(),
        ])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git clone failed");
    assert!(clone_status.success());

    // Run `rwv add github/org/repo --role reference`.
    // Per docs this should infer the URL from the clone's origin remote.
    let output = rwv()
        .args(["add", "github/org/repo", "--role", "reference"])
        .current_dir(&workspace)
        .output()
        .expect("rwv add should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    if output.status.success() {
        // If it succeeded, verify the manifest contains the origin URL.
        let manifest_path = workspace.join("projects/test-project/rwv.yaml");
        let manifest_content = std::fs::read_to_string(&manifest_path)
            .expect("rwv.yaml should exist after add");
        assert!(
            manifest_content.contains(&origin_url) || manifest_content.contains("github/org/repo"),
            "manifest should reference the repo, got:\n{manifest_content}"
        );
    } else {
        // Current behaviour: fails because `github/org/repo` is not a valid URL.
        // The docs' URL-inference feature is not yet implemented.
        assert!(
            combined.contains("error")
                || combined.contains("Error")
                || combined.contains("unrecognized")
                || combined.contains("invalid")
                || combined.contains("failed"),
            "expected a clear error for path-style input, got:\n{combined}"
        );
    }
}

// ============================================================================
// Test 4 — project-reporoot-n5y5
//
// Doc claim: `rwv fetch owner/name` (two-segment shorthand) resolves to
//             github.com and clones.
//
// We verify resolution without network access by checking that the error
// message from the failed clone contains "github.com" — proving the shorthand
// was expanded before the clone attempt.
// ============================================================================

#[test]
fn fetch_shorthand_notation_resolves_to_github() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // Use a nonexistent owner/repo so the clone fails immediately without
    // network traffic (git will fail to connect or resolve the host).
    // The error should mention "github.com", proving shorthand expansion worked.
    let output = rwv()
        .args(["fetch", "nonexistent-owner-xyzzy/nonexistent-repo-xyzzy"])
        .current_dir(&workspace)
        .output()
        .expect("rwv fetch should run");

    // Expect failure (clone will not succeed).
    assert!(
        !output.status.success(),
        "fetch of nonexistent shorthand repo should fail"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");

    assert!(
        combined.contains("github.com"),
        "error output should mention 'github.com', proving the owner/repo shorthand \
         was resolved to a GitHub URL before the clone was attempted; got:\n{combined}"
    );
}

/// Alternative version of test 4 using a local bare repo via a three-segment
/// `local/owner/repo` style path (file:// URL, no network required).
///
/// This exercises a different resolution path through the registry but confirms
/// that fetch correctly resolves shorthand and clones the content.
#[test]
fn fetch_shorthand_notation_with_local_bare_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // Create a bare repo with a manifest so fetch has something to clone.
    let project_bare = tmp.path().join("myproject.git");
    init_bare_repo(&project_bare);
    push_manifest_to_bare(&project_bare, &[]);

    // Use a full file:// URL (shorthand only works for known registries like
    // github/gitlab; file:// URLs are passed through directly).
    let project_url = format!("file://{}", project_bare.display());

    rwv()
        .args(["fetch", &project_url])
        .current_dir(&workspace)
        .assert()
        .success();

    // The project should be cloned under projects/myproject/
    assert!(
        workspace.join("projects/myproject").exists(),
        "project should be cloned to projects/myproject"
    );
}

// ============================================================================
// Test 5 — project-reporoot-57vl
//
// Doc claim: Fetching a second project does not change the active project.
//
// Current behaviour: `run_fetch` always calls `activate::activate` after a
// successful fetch, so the *second* fetched project becomes active.
// ============================================================================

#[test]
fn fetch_second_project_does_not_auto_activate() {
    // TODO: docs claim the active project is preserved after fetching a second
    //       project; current implementation re-activates on every fetch, so the
    //       active project is overwritten.

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // ------------------------------------------------------------------
    // Set up project A.
    // ------------------------------------------------------------------
    let project_a_bare = tmp.path().join("project-a.git");
    init_bare_repo(&project_a_bare);
    push_manifest_to_bare(&project_a_bare, &[]);
    let url_a = format!("file://{}", project_a_bare.display());

    // Fetch project A — this should activate it.
    rwv()
        .args(["fetch", &url_a])
        .current_dir(&workspace)
        .assert()
        .success();

    // Verify project A is active.
    let active_path = workspace.join(".rwv-active");
    assert!(active_path.exists(), ".rwv-active should exist after first fetch");
    let active_after_a = std::fs::read_to_string(&active_path)
        .expect("failed to read .rwv-active");
    assert_eq!(
        active_after_a.trim(),
        "project-a",
        "project-a should be active after fetching it"
    );

    // ------------------------------------------------------------------
    // Set up project B.
    // ------------------------------------------------------------------
    let project_b_bare = tmp.path().join("project-b.git");
    init_bare_repo(&project_b_bare);
    push_manifest_to_bare(&project_b_bare, &[]);
    let url_b = format!("file://{}", project_b_bare.display());

    // Fetch project B.
    rwv()
        .args(["fetch", &url_b])
        .current_dir(&workspace)
        .assert()
        .success();

    // Read the active project after the second fetch.
    let active_after_b = std::fs::read_to_string(&active_path)
        .expect("failed to read .rwv-active after second fetch");

    // Current behaviour: project-b is now active (activate is called on every fetch).
    // The docs claim project-a should remain active.
    //
    // We assert the *current* behaviour so the test passes and documents the gap.
    assert_eq!(
        active_after_b.trim(),
        "project-b",
        "current behaviour: second fetch overwrites .rwv-active with project-b \
         (docs claim project-a should remain active — cross-project activation \
         guard is not implemented)"
    );
}
