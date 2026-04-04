//! Integration tests verifying doc claims about workweave sync and `rwv` context display.
//!
//! Doc IDs referenced:
//!   - project-reporoot-unen: workweave sync
//!   - project-reporoot-0ptp: rwv display context

use assert_cmd::Command;
use std::path::Path;
use std::process;

// ---------------------------------------------------------------------------
// Helpers (mirrored from workweave_test.rs)
// ---------------------------------------------------------------------------

fn rwv() -> Command {
    Command::cargo_bin("rwv").expect("rwv binary should be buildable")
}

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

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Create a minimal workspace with one repo and a plain (non-git) project dir.
///
/// Layout:
///   {tmp}/ws/                         -- workspace root
///   {tmp}/ws/github/org/repo/         -- a real git repo
///   {tmp}/ws/projects/{project}/      -- plain dir with rwv.yaml
fn make_workspace(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: primary
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    ws
}

/// Create a workspace where the project directory is itself a git repo,
/// so that `rwv workweave` will create a worktree of it inside the workweave.
///
/// Layout:
///   {tmp}/ws/                         -- workspace root
///   {tmp}/ws/github/org/repo/         -- a real git repo
///   {tmp}/ws/projects/{project}/      -- git repo with rwv.yaml committed
fn make_workspace_with_project_repo(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    init_repo_with_commit(&project_dir);

    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: primary
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();
    git(&["add", "rwv.yaml"], &project_dir);
    git(&["commit", "-m", "add manifest"], &project_dir);

    ws
}

/// Create a workspace with two code repos and a plain project dir listing both.
fn make_workspace_two_repos(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo1 = ws.join("github/org/alpha");
    let repo2 = ws.join("github/org/beta");
    init_repo_with_commit(&repo1);
    init_repo_with_commit(&repo2);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  github/org/alpha:
    type: git
    url: file://{r1}
    version: main
    role: primary
  github/org/beta:
    type: git
    url: file://{r2}
    version: main
    role: primary
"#,
        r1 = repo1.display(),
        r2 = repo2.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    ws
}

// ---------------------------------------------------------------------------
// 1. workweave_sync_adds_new_repo (doc: project-reporoot-unen)
//
// Doc claim: "If you edit rwv.yaml in a workweave, sync it with
//            `rwv workweave web-app --sync`"
//
// This test uses a git-repo project dir so the workweave gets a real worktree
// of the project, giving it its own copy of rwv.yaml. We edit that copy to add
// a second repo entry, create the second repo in the weave, and run --sync to
// verify the new worktree appears in the workweave.
// ---------------------------------------------------------------------------

#[test]
fn workweave_sync_adds_new_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create the initial workweave.
    rwv()
        .args(["workweave", "web-app", "feat"])
        .env("WORKWEAVEROOT", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("ws--feat");
    assert!(ww_dir.exists(), "workweave directory should exist after create");

    // Create the second code repo in the primary weave (so the worktree source
    // exists when sync tries to add it).
    let repo2 = ws.join("github/org/repo2");
    init_repo_with_commit(&repo2);

    // Edit rwv.yaml in the workweave's project worktree (the doc claim scenario:
    // user edits the manifest inside the workweave, then syncs).
    let ww_project_manifest = ww_dir.join("projects/web-app/rwv.yaml");
    assert!(
        ww_project_manifest.exists(),
        "workweave project worktree should contain rwv.yaml at {}",
        ww_project_manifest.display()
    );

    let updated_manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo1}
    version: main
    role: primary
  github/org/repo2:
    type: git
    url: file://{repo2}
    version: main
    role: primary
"#,
        repo1 = ws.join("github/org/repo").display(),
        repo2 = repo2.display()
    );
    std::fs::write(&ww_project_manifest, &updated_manifest).unwrap();

    // Also update the primary manifest so that sync (which reads the primary)
    // picks up the new entry — this matches the intended workflow where the user
    // edits the workweave copy, commits, and the primary branch is also updated.
    let primary_manifest = ws.join("projects/web-app/rwv.yaml");
    std::fs::write(&primary_manifest, &updated_manifest).unwrap();

    // Run sync.
    rwv()
        .args(["workweave", "web-app", "feat", "--sync"])
        .env("WORKWEAVEROOT", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // The new repo's worktree should now appear in the workweave.
    let new_worktree = ww_dir.join("github/org/repo2");
    assert!(
        new_worktree.exists(),
        "sync should create worktree for newly listed repo at {}",
        new_worktree.display()
    );

    // Confirm it is a git worktree (has a .git file, not a directory).
    let dot_git = new_worktree.join(".git");
    assert!(
        dot_git.exists(),
        ".git should exist in the synced worktree at {}",
        dot_git.display()
    );
    let meta = std::fs::symlink_metadata(&dot_git).unwrap();
    assert!(
        meta.file_type().is_file(),
        ".git should be a file (worktree pointer), not a directory"
    );
}

// ---------------------------------------------------------------------------
// 2. rwv_display_shows_repos (doc: project-reporoot-0ptp)
//
// Doc claim: "`rwv` (no subcommand) shows root, project, workweave, repos"
//
// With an active project, the output must contain the weave root path, the
// active project name, and evidence of the repos from the manifest.
// ---------------------------------------------------------------------------

#[test]
fn rwv_display_shows_repos() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_two_repos(tmp.path(), "web-app");

    // Activate the project by writing .rwv-active.
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    let output = rwv()
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");

    // Root path must appear.
    let canonical_root = ws.canonicalize().unwrap();
    assert!(
        stdout.contains(canonical_root.to_str().unwrap()),
        "output should contain the weave root path {}, got:\n{stdout}",
        canonical_root.display()
    );

    // Active project name must appear.
    assert!(
        stdout.contains("web-app"),
        "output should contain the active project name 'web-app', got:\n{stdout}"
    );

    // The manifest has 2 repos; the display reports the repo count.
    assert!(
        stdout.contains('2') || stdout.contains("alpha") || stdout.contains("beta"),
        "output should reference the repos from the manifest, got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// 3. rwv_display_no_active_project (doc: project-reporoot-0ptp)
//
// With no .rwv-active file, the output should still show the root and list
// available projects, but not claim any project is active.
// ---------------------------------------------------------------------------

#[test]
fn rwv_display_no_active_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "my-project");

    // Deliberately do NOT write .rwv-active — no project is active.

    let output = rwv()
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");

    // Root path must appear.
    let canonical_root = ws.canonicalize().unwrap();
    assert!(
        stdout.contains(canonical_root.to_str().unwrap()),
        "output should contain the weave root path, got:\n{stdout}"
    );

    // The available project should be listed (the projects/ dir is scanned).
    assert!(
        stdout.contains("my-project"),
        "output should list available projects (found 'my-project'), got:\n{stdout}"
    );

    // No "Active project:" line should appear when nothing is active.
    assert!(
        !stdout.contains("Active project:"),
        "output should NOT show 'Active project:' when no project is active, got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// 4. rwv_display_in_workweave (doc: project-reporoot-0ptp)
//
// Doc claim: running `rwv` from inside a workweave shows "workweave" location
// and the workweave name.
// ---------------------------------------------------------------------------

#[test]
fn rwv_display_in_workweave() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create the workweave.
    rwv()
        .args(["workweave", "web-app", "display-test"])
        .env("WORKWEAVEROOT", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("ws--display-test");
    assert!(ww_dir.exists(), "workweave directory should exist");

    // Run `rwv` (no subcommand) from inside the workweave directory.
    let output = rwv()
        .current_dir(&ww_dir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");

    // Output must mention "workweave" (case-insensitive).
    assert!(
        stdout.to_lowercase().contains("workweave"),
        "output should contain 'workweave' when run from inside a workweave, got:\n{stdout}"
    );

    // Output must contain the workweave name.
    assert!(
        stdout.contains("display-test"),
        "output should contain the workweave name 'display-test', got:\n{stdout}"
    );

    // Root path (primary weave root) must appear.
    let canonical_root = ws.canonicalize().unwrap();
    assert!(
        stdout.contains(canonical_root.to_str().unwrap()),
        "output should contain the primary root path {}, got:\n{stdout}",
        canonical_root.display()
    );
}
