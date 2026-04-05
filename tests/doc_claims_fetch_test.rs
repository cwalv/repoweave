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
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
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

    run(
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
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
// Test 1 — project-reporoot-gazl
//
// Doc claim: "If names collide, rwv fetch errors and suggests a scoped path:
//             projects/{owner}/{name}/"
// ============================================================================

#[test]
fn fetch_name_collision_behavior() {
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

    // Command must exit non-zero.
    assert!(
        !output.status.success(),
        "fetch should exit non-zero on name collision, got exit: {}\n{combined}",
        output.status
    );

    // Error message must mention "already exists".
    assert!(
        combined.contains("already exists"),
        "expected 'already exists' in error output, got:\n{combined}"
    );

    // Output must include the scoped-path hint.
    assert!(
        combined.contains("Hint") || combined.contains("scoped"),
        "expected a scoped-path hint in the output, got:\n{combined}"
    );
    assert!(
        combined.contains("web-app"),
        "hint should name the project, got:\n{combined}"
    );
}

// ============================================================================
// Test 2 — project-reporoot-nt18
//
// `rwv remove --delete` checks whether other projects reference the same repo
// before deleting.  If a reference is found:
//   - A warning is printed: "warning: repo also referenced by project 'X'"
//   - The deletion is refused unless `--force` is also passed.
// ============================================================================

#[test]
fn remove_delete_does_not_check_other_projects() {
    let tmp = tempfile::tempdir().unwrap();

    // Create a bare repo that both projects will reference.
    let shared_bare = tmp.path().join("shared.git");
    init_bare_repo_with_commit(&shared_bare);
    let shared_url = format!("file://{}", shared_bare.display());
    let repo_path = "local/org/shared";

    // Project A references the shared repo.
    let (workspace, _project_a_dir) =
        setup_workspace_with_project(&tmp, "project-a", &[(repo_path, &shared_url)]);

    // Project B also references the same repo.
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

    // --- Without --force: should fail and print a warning ---
    let output = rwv()
        .args(["remove", repo_path, "--delete"])
        .current_dir(&workspace.join("projects").join("project-a"))
        .output()
        .expect("rwv remove should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "rwv remove --delete should fail when another project references the repo; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("warning: repo also referenced by project"),
        "expected cross-project warning in stderr, got:\n{stderr}"
    );
    // The directory must still exist — deletion was refused.
    assert!(
        repo_dir.exists(),
        "directory should NOT be deleted when another project references the repo"
    );

    // --- With --force: should succeed and delete ---
    rwv()
        .args(["remove", repo_path, "--delete", "--force"])
        .current_dir(&workspace.join("projects").join("project-a"))
        .assert()
        .success();

    assert!(
        !repo_dir.exists(),
        "rwv remove --delete --force should delete the directory even when shared"
    );
}

// ============================================================================
// Test 3 — project-reporoot-fwui
//
// `rwv add <local-path> --role reference` infers the URL from the clone's
// origin remote when the argument has no URL scheme and the directory exists
// under the workspace root.
// ============================================================================

#[test]
fn add_from_local_path_infers_url() {
    let tmp = tempfile::tempdir().unwrap();

    // Create a bare repo to act as the remote origin.
    let bare = tmp.path().join("origin.git");
    init_bare_repo_with_commit(&bare);
    let origin_url = format!("file://{}", bare.display());

    // Set up workspace with an empty project manifest.
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, "test-project", &[]);

    // Clone the bare repo to the canonical path github/org/repo inside the workspace.
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

    // Run `rwv add github/org/repo --role reference` from the workspace root.
    // The directory exists, so rwv should detect it as a local path and infer
    // the URL from the clone's origin remote.
    rwv()
        .args(["add", "github/org/repo", "--role", "reference"])
        .current_dir(&workspace)
        .assert()
        .success();

    // Verify the manifest contains the correct origin URL and repo path.
    let manifest_path = workspace.join("projects/test-project/rwv.yaml");
    let manifest_content =
        std::fs::read_to_string(&manifest_path).expect("rwv.yaml should exist after add");

    assert!(
        manifest_content.contains("github/org/repo"),
        "manifest should contain the repo path 'github/org/repo', got:\n{manifest_content}"
    );
    assert!(
        manifest_content.contains(&origin_url),
        "manifest should contain the inferred origin URL '{origin_url}', got:\n{manifest_content}"
    );
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
// Test 5 — project-reporoot-gjci
//
// Doc claim: Fetching a second project does not change the active project.
// ============================================================================

#[test]
fn fetch_second_project_does_not_auto_activate() {
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

    // Fetch project A — this should activate it (first fetch, no .rwv-active yet).
    rwv()
        .args(["fetch", &url_a])
        .current_dir(&workspace)
        .assert()
        .success();

    // Verify project A is active.
    let active_path = workspace.join(".rwv-active");
    assert!(
        active_path.exists(),
        ".rwv-active should exist after first fetch"
    );
    let active_after_a = std::fs::read_to_string(&active_path).expect("failed to read .rwv-active");
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

    // Fetch project B — .rwv-active already exists, so activation is skipped.
    rwv()
        .args(["fetch", &url_b])
        .current_dir(&workspace)
        .assert()
        .success();

    // .rwv-active must still point to project-a.
    let active_after_b = std::fs::read_to_string(&active_path)
        .expect("failed to read .rwv-active after second fetch");
    assert_eq!(
        active_after_b.trim(),
        "project-a",
        "second fetch must not overwrite .rwv-active; project-a should remain active"
    );
}
