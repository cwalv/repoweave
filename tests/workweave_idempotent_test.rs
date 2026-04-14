//! Idempotency regression test for `rwv workweave <project> create <name>`.
//!
//! Gas City's pool-worker resume contract (`gc runtime request-restart`)
//! relies on re-invoking `rwv workweave` leaving non-git state in the
//! workweave untouched. If a future refactor makes workweave creation
//! destructive on the second call, `.runtime/`, `.claude/`, and similar
//! non-git scratch state get wiped — silently breaking session resume.
//!
//! This test locks the contract: create a workweave, drop sentinel files
//! into it (mimicking agent runtime state), re-invoke `rwv workweave ...
//! create ...`, and assert that the workweave directory, the sentinel
//! files, the `.rwv-workweave` marker, and the per-repo worktree branches
//! are all preserved.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

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

fn current_branch(dir: &Path) -> String {
    let output = process::Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git symbolic-ref should run");
    assert!(
        output.status.success(),
        "git symbolic-ref in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("branch name should be valid UTF-8")
        .trim()
        .to_string()
}

/// Re-invoking `rwv workweave PROJECT create NAME` on an already-created
/// workweave must not destroy non-git state inside the workweave.
///
/// Rationale: Gas City's `gc runtime request-restart` flow re-creates the
/// session inside the same workweave path. The pool-worker contract
/// assumes non-git files written by agents (sentinel state under
/// `.runtime/`, agent scratch under `.claude/`) survive a re-invocation
/// of the same `rwv workweave ... create ...` command.
#[test]
fn workweave_recreate_preserves_non_git_state() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // ---- First invocation: create the workweave fresh. ----
    rwv()
        .args(["workweave", "web-app", "create", "resume"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("ws--resume");
    assert!(ww_dir.exists(), "workweave should exist after first create");

    // ---- Drop non-git state into the workweave. ----
    //
    // Mirrors what Gas City pool-workers actually write: a sentinel under
    // .runtime/ and an agent-scratch file under .claude/.
    let runtime_dir = ww_dir.join(".runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let sentinel_path = runtime_dir.join("sentinel.txt");
    let sentinel_content = "pool-worker session state\n";
    std::fs::write(&sentinel_path, sentinel_content).unwrap();

    let claude_dir = ww_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let claude_state_path = claude_dir.join("agent-scratch.txt");
    let claude_state_content = "claude agent ephemeral state\n";
    std::fs::write(&claude_state_path, claude_state_content).unwrap();

    // Snapshot the marker and the repo's worktree branch so we can
    // assert they are unchanged after re-invocation.
    let marker_path = ww_dir.join(".rwv-workweave");
    assert!(marker_path.exists(), ".rwv-workweave should exist after create");
    let marker_before = std::fs::read_to_string(&marker_path).unwrap();

    let weave_repo = ww_dir.join("github/org/repo");
    let branch_before = current_branch(&weave_repo);
    assert_eq!(
        branch_before, "resume/main",
        "worktree should be on ephemeral branch resume/main before re-invocation"
    );

    // ---- Second invocation: re-create the same workweave. ----
    //
    // The assertion is that this succeeds AND leaves non-git state
    // intact. If this fails, it confirms fo-bsd's premise: rwv
    // workweave create is not idempotent on re-invocation and needs a
    // fix to support the pool-worker resume contract.
    rwv()
        .args(["workweave", "web-app", "create", "resume"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // ---- Assert: workweave directory still at the same path. ----
    assert!(
        ww_dir.exists(),
        "workweave directory should still exist after re-invocation at {}",
        ww_dir.display()
    );

    // ---- Assert: sentinel files survived unchanged. ----
    assert!(
        sentinel_path.exists(),
        ".runtime/sentinel.txt should survive re-invocation"
    );
    let sentinel_after = std::fs::read_to_string(&sentinel_path).unwrap();
    assert_eq!(
        sentinel_after, sentinel_content,
        ".runtime/sentinel.txt content should be unchanged after re-invocation"
    );

    assert!(
        claude_state_path.exists(),
        ".claude/agent-scratch.txt should survive re-invocation"
    );
    let claude_state_after = std::fs::read_to_string(&claude_state_path).unwrap();
    assert_eq!(
        claude_state_after, claude_state_content,
        ".claude/agent-scratch.txt content should be unchanged after re-invocation"
    );

    // ---- Assert: marker still points at the same primary + project. ----
    //
    // We compare the content directly: the marker is derived from the
    // workspace root and project name, both of which are unchanged, so
    // a byte-identical result is the strongest assertion we can make.
    assert!(marker_path.exists(), ".rwv-workweave marker should still exist");
    let marker_after = std::fs::read_to_string(&marker_path).unwrap();
    assert_eq!(
        marker_after, marker_before,
        ".rwv-workweave marker should be unchanged after re-invocation"
    );

    // ---- Assert: worktree still on the same ephemeral branch. ----
    assert!(
        weave_repo.exists(),
        "per-repo worktree should still exist after re-invocation"
    );
    let branch_after = current_branch(&weave_repo);
    assert_eq!(
        branch_after, branch_before,
        "worktree ephemeral branch should be unchanged after re-invocation"
    );
}

/// Re-invoking `rwv workweave PROJECT create NAME` on a workweave that has
/// local modifications (uncommitted changes OR commits on the ephemeral
/// branch) must refuse without `--force`, preserving the user's work.
///
/// This protects against silent loss when a user has done work inside a
/// workweave and then accidentally (or a tool) re-issues the create
/// command — a failed idempotency check here would clobber that work.
#[test]
fn workweave_recreate_refuses_on_local_modifications() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // ---- Case A: uncommitted changes in the worktree. ----
    rwv()
        .args(["workweave", "web-app", "create", "dirty"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("ws--dirty");
    let weave_repo = ww_dir.join("github/org/repo");
    let head_before_dirty = head_sha(&weave_repo);

    // Introduce an uncommitted change (new file).
    std::fs::write(weave_repo.join("scratch.txt"), "untracked edit\n").unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "dirty"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted changes"));

    // Dirty file must still be there.
    assert!(
        weave_repo.join("scratch.txt").exists(),
        "scratch.txt should survive a refused re-invocation"
    );
    assert_eq!(
        head_sha(&weave_repo),
        head_before_dirty,
        "worktree HEAD should be unchanged after refused re-invocation"
    );

    // ---- Case B: a new commit on the ephemeral branch. ----
    rwv()
        .args(["workweave", "web-app", "create", "advanced"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww2_dir = weaveroot.join("ws--advanced");
    let weave2_repo = ww2_dir.join("github/org/repo");

    std::fs::write(weave2_repo.join("new-file.txt"), "content\n").unwrap();
    git(&["add", "."], &weave2_repo);
    git(&["commit", "-m", "work in progress"], &weave2_repo);
    let advanced_head = head_sha(&weave2_repo);

    rwv()
        .args(["workweave", "web-app", "create", "advanced"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("diverged from primary"));

    // Commit must still be there.
    assert_eq!(
        head_sha(&weave2_repo),
        advanced_head,
        "ephemeral-branch commit should survive a refused re-invocation"
    );
    assert!(
        weave2_repo.join("new-file.txt").exists(),
        "committed file should still be on disk"
    );
}

/// Re-invoking `rwv workweave` with a project that does NOT match the
/// existing marker in the target directory must refuse (without `--force`).
///
/// Without this check, a user who typoed the project name or reused a
/// name across projects would silently clobber the existing workweave.
#[test]
fn workweave_recreate_refuses_on_wrong_project_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "project-a");
    // Add a second project manifest pointing at the same repo.
    let project_b_dir = ws.join("projects/project-b");
    std::fs::create_dir_all(&project_b_dir).unwrap();
    let repo_path = ws.join("github/org/repo");
    let manifest_b = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: primary
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_b_dir.join("rwv.yaml"), manifest_b).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create workweave "shared" for project-a.
    rwv()
        .args(["workweave", "project-a", "create", "shared"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("ws--shared");
    assert!(ww_dir.exists());
    let marker_before = std::fs::read_to_string(ww_dir.join(".rwv-workweave")).unwrap();

    // Attempt to recreate the same workweave for project-b — must refuse.
    rwv()
        .args(["workweave", "project-b", "create", "shared"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("project-a"));

    // Marker unchanged: the existing workweave was not touched.
    let marker_after = std::fs::read_to_string(ww_dir.join(".rwv-workweave")).unwrap();
    assert_eq!(
        marker_after, marker_before,
        ".rwv-workweave marker should be unchanged after refused cross-project recreate"
    );
}

/// `--force` must destroy and recreate the workweave even when the
/// existing state has local modifications. This is the explicit rebuild
/// path (corruption recovery, reusing a slot for a new purpose).
#[test]
fn workweave_recreate_with_force_destroys_and_recreates() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "reset"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("ws--reset");
    let weave_repo = ww_dir.join("github/org/repo");

    // Dirty it — a refused-recreate would have fired here without --force.
    std::fs::write(weave_repo.join("scratch.txt"), "local work\n").unwrap();
    let sentinel = ww_dir.join(".runtime/sentinel.txt");
    std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    std::fs::write(&sentinel, "will be wiped\n").unwrap();
    let head_before = head_sha(&weave_repo);

    // Re-invoke with --force: should destroy and recreate.
    rwv()
        .args(["workweave", "web-app", "create", "reset", "--force"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Dirty file and sentinel are gone — --force is destructive.
    assert!(
        !weave_repo.join("scratch.txt").exists(),
        "scratch.txt should be wiped by --force recreate"
    );
    assert!(
        !sentinel.exists(),
        "sentinel file should be wiped by --force recreate"
    );

    // Workweave is rebuilt: marker present, worktree on expected branch,
    // HEAD matching primary's current branch.
    assert!(ww_dir.join(".rwv-workweave").exists());
    assert_eq!(current_branch(&weave_repo), "reset/main");
    assert_eq!(
        head_sha(&weave_repo),
        head_before,
        "rebuilt worktree HEAD should match primary's current-branch HEAD"
    );
}

fn head_sha(dir: &Path) -> String {
    let output = process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git rev-parse should run");
    assert!(
        output.status.success(),
        "git rev-parse HEAD in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("sha should be valid UTF-8")
        .trim()
        .to_string()
}
