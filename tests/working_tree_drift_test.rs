//! Integration tests for working-tree drift detection (`rwv doctor`) and
//! auto-fix (`rwv doctor --fix`).
//!
//! These tests exercise the seven acceptance scenarios described in fo-rwv-worktree-drift:
//!   1. Stale-working-tree detection
//!   2. Stale-working-tree auto-fix
//!   3. Live edit NOT fixed
//!   4. Sync post-refresh (working-tree version)
//!   5. 3+ worktrees
//!   6. Reachable-blob guard
//!   7. Composition with index fix (the real fo-city 2026-04-23 case)
//!
//! # How working-tree drift is simulated
//!
//! The actual drift condition (working tree behind HEAD) arises when:
//! - Worktree B is on branch `ww/main`
//! - Another process advances `refs/heads/ww/main`
//! - B's HEAD (via symbolic ref) now points at the new commit
//! - B's index is then reset to HEAD (`git reset`) but the on-disk files are not updated
//!
//! Tests simulate this by combining `git update-ref` with `git reset` in the
//! worktree, leaving files on disk at their pre-advance content.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

fn git(args: &[&str], dir: &Path) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_out(args: &[&str], dir: &Path) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Initialise a git repo with one commit. Returns HEAD SHA.
fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

/// Commit a file change. Returns new HEAD SHA.
fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
}

/// Write rwv.yaml.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: primary\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();
}

/// Write rwv.lock.
fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url, sha) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: {sha}\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), &yaml).unwrap();
}

fn rwv() -> AssertCommand {
    AssertCommand::cargo_bin("rwv").expect("rwv binary should be buildable")
}

// ---------------------------------------------------------------------------
// Shared fixture
// ---------------------------------------------------------------------------

const SERVER_PATH: &str = "github/chatly/server";
const SERVER_URL: &str = "https://github.com/chatly/server.git";

struct Workspace {
    primary_root: PathBuf,
    server_primary: PathBuf,
    server_ww: PathBuf,
}

/// Create a primary workspace and a workweave whose server repo is a worktree
/// on branch `ww/main` (initially at C1).
fn make_workspace_with_ww(parent: &Path) -> (Workspace, String) {
    let primary_root = parent.join("ws");
    std::fs::create_dir_all(primary_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(primary_root.join("projects")).unwrap();

    let server_primary = primary_root.join(SERVER_PATH);
    let c1 = init_repo(&server_primary);

    let project_primary = primary_root.join("projects/web-app");
    init_repo(&project_primary);
    write_manifest(&project_primary, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&project_primary, &[(SERVER_PATH, SERVER_URL, &c1)]);
    git(&["add", "rwv.yaml", "rwv.lock"], &project_primary);
    git(&["commit", "-m", "lock: initial"], &project_primary);

    let ww_root = parent.join(".workweaves").join("ws--ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let server_ww = ww_root.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            "-b",
            "ww/main",
            &server_ww.to_string_lossy(),
        ],
        &server_primary,
    );

    let project_ww = ww_root.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            "-b",
            "ww/project",
            &project_ww.to_string_lossy(),
        ],
        &project_primary,
    );

    let marker_content = format!(
        "primary: {}\nproject: web-app\n",
        primary_root.canonicalize().unwrap().display()
    );
    std::fs::write(ww_root.join(".rwv-workweave"), marker_content).unwrap();

    (
        Workspace {
            primary_root,
            server_primary,
            server_ww,
        },
        c1,
    )
}

/// Advance `refs/heads/<branch>` to `new_sha` from the primary server repo.
fn advance_ww_branch(server_primary: &Path, branch: &str, new_sha: &str) {
    git(
        &["update-ref", &format!("refs/heads/{branch}"), new_sha],
        server_primary,
    );
}

/// Simulate working-tree drift: advance the ww branch ref to `new_sha` and
/// reset the index to HEAD, leaving working-tree files at their old content.
///
/// After this call: HEAD = new_sha, index = new_sha's tree, working tree = old.
fn make_working_tree_stale(server_ww: &Path, server_primary: &Path, new_sha: &str) {
    advance_ww_branch(server_primary, "ww/main", new_sha);
    // `git reset` (mixed) aligns the index to HEAD without touching the working tree.
    git(&["reset"], server_ww);
}

// ---------------------------------------------------------------------------
// Test 1: Stale-working-tree detection
// ---------------------------------------------------------------------------

#[test]
fn doctor_detects_stale_working_tree_in_workweave() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");
    make_working_tree_stale(&ws.server_ww, &ws.server_primary, &c2);

    // Verify working tree IS stale before doctor.
    let wt_diff = Command::new("git")
        .args(["diff-index", "--name-only", "HEAD"])
        .current_dir(&ws.server_ww)
        .output()
        .unwrap();
    assert!(
        !wt_diff.stdout.is_empty(),
        "working tree should be stale before doctor"
    );

    rwv()
        .args(["doctor"])
        .current_dir(&ws.primary_root)
        .assert()
        .stdout(
            predicate::str::contains("working tree stale")
                .or(predicate::str::contains("safe to --fix")),
        );
}

// ---------------------------------------------------------------------------
// Test 2: Stale-working-tree auto-fix
// ---------------------------------------------------------------------------

#[test]
fn doctor_fix_restores_stale_working_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Write an UNTRACKED file — must survive the fix.
    std::fs::write(ws.server_ww.join("untracked.txt"), "untouched\n").unwrap();

    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");
    make_working_tree_stale(&ws.server_ww, &ws.server_primary, &c2);

    // Confirm working tree is stale.
    let diff_before = Command::new("git")
        .args(["diff-index", "--name-only", "HEAD"])
        .current_dir(&ws.server_ww)
        .output()
        .unwrap();
    assert!(
        !diff_before.stdout.is_empty(),
        "working tree should be stale before --fix"
    );

    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .assert()
        .stdout(predicate::str::contains("fixed").or(predicate::str::contains("refreshed")));

    // After fix: working tree must match HEAD.
    let diff_after = Command::new("git")
        .args(["diff-index", "--exit-code", "HEAD"])
        .current_dir(&ws.server_ww)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        diff_after.success(),
        "working tree should match HEAD after --fix"
    );

    // Untracked file must still exist.
    assert!(
        ws.server_ww.join("untracked.txt").exists(),
        "untracked file should survive --fix"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Live edit NOT fixed
// ---------------------------------------------------------------------------

#[test]
fn doctor_fix_does_not_clobber_live_working_tree_edits() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // C2 on primary — we'll advance the ww branch ref to it.
    let c2 = make_commit(&ws.server_primary, "unrelated.txt", "x\n", "primary: C2");
    advance_ww_branch(&ws.server_primary, "ww/main", &c2);
    // Reset index to HEAD so the index reflects C2.
    git(&["reset"], &ws.server_ww);

    // Now write a live edit to a tracked file with UNIQUE content (never committed).
    std::fs::write(ws.server_ww.join("README.md"), "my unique edit — never committed\n")
        .unwrap();

    // doctor --fix must NOT touch the live edit.
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .assert()
        .stdout(
            predicate::str::contains("live edits")
                .or(predicate::str::contains("manual review")),
        );

    // The live edit must still be present.
    let content = std::fs::read_to_string(ws.server_ww.join("README.md")).unwrap();
    assert!(
        content.contains("my unique edit"),
        "live edit must survive doctor --fix; got: {content}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Sync post-refresh
// ---------------------------------------------------------------------------

#[test]
fn sync_post_refresh_clears_stale_working_tree() {
    let tmp = tempfile::tempdir().unwrap();

    let primary_root = tmp.path().join("primary");
    std::fs::create_dir_all(primary_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(primary_root.join("projects")).unwrap();

    let server_primary = primary_root.join(SERVER_PATH);
    let c1 = init_repo(&server_primary);

    let project_primary = primary_root.join("projects/web-app");
    init_repo(&project_primary);
    write_manifest(&project_primary, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&project_primary, &[(SERVER_PATH, SERVER_URL, &c1)]);
    git(&["add", "rwv.yaml", "rwv.lock"], &project_primary);
    git(&["commit", "-m", "lock: initial"], &project_primary);

    let ww_root = tmp.path().join(".workweaves").join("primary--ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let server_ww = ww_root.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            "-b",
            "ww/main",
            &server_ww.to_string_lossy(),
        ],
        &server_primary,
    );

    let project_ww = ww_root.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            "-b",
            "ww/project",
            &project_ww.to_string_lossy(),
        ],
        &project_primary,
    );

    let marker_content = format!(
        "primary: {}\nproject: web-app\n",
        primary_root.canonicalize().unwrap().display()
    );
    std::fs::write(ww_root.join(".rwv-workweave"), marker_content).unwrap();

    // Primary commits C2 and updates lock.
    let c2 = make_commit(&server_primary, "advance.txt", "new\n", "primary: C2");
    write_lock(&project_primary, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &project_primary);
    git(&["commit", "-m", "lock: advance"], &project_primary);

    // Set up working-tree drift in the ww server repo.
    make_working_tree_stale(&server_ww, &server_primary, &c2);

    let diff_before = Command::new("git")
        .args(["diff-index", "--name-only", "HEAD"])
        .current_dir(&server_ww)
        .output()
        .unwrap();
    assert!(
        !diff_before.stdout.is_empty(),
        "working tree should be stale before sync"
    );

    rwv()
        .args(["sync", &primary_root.to_string_lossy()])
        .current_dir(&ww_root)
        .assert()
        .success();

    // After sync: working tree must match HEAD.
    let diff_after = Command::new("git")
        .args(["diff-index", "--exit-code", "HEAD"])
        .current_dir(&server_ww)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        diff_after.success(),
        "working tree should match HEAD after sync"
    );
}

// ---------------------------------------------------------------------------
// Test 5: 3+ worktrees
// ---------------------------------------------------------------------------

#[test]
fn doctor_detects_working_tree_drift_in_three_worktrees() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Create a second workweave.
    let ww2_root = tmp.path().join(".workweaves").join("ws--ww2");
    std::fs::create_dir_all(ww2_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww2_root.join("projects")).unwrap();

    let server_ww2 = ww2_root.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            "-b",
            "ww2/main",
            &server_ww2.to_string_lossy(),
        ],
        &ws.server_primary,
    );

    let project_primary = ws.primary_root.join("projects/web-app");
    let project_ww2 = ww2_root.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            "-b",
            "ww2/project",
            &project_ww2.to_string_lossy(),
        ],
        &project_primary,
    );

    let marker2 = format!(
        "primary: {}\nproject: web-app\n",
        ws.primary_root.canonicalize().unwrap().display()
    );
    std::fs::write(ww2_root.join(".rwv-workweave"), marker2).unwrap();

    // Commit C2 and make BOTH workweaves stale.
    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");
    make_working_tree_stale(&ws.server_ww, &ws.server_primary, &c2);
    advance_ww_branch(&ws.server_primary, "ww2/main", &c2);
    git(&["reset"], &server_ww2);

    // Doctor from primary should detect drift in both workweaves.
    let out = rwv()
        .args(["doctor"])
        .current_dir(&ws.primary_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stale_count = stdout
        .lines()
        .filter(|l| l.contains("working tree stale") || l.contains("safe to --fix"))
        .count();
    assert!(
        stale_count >= 2,
        "doctor should report stale working tree for both workweaves; got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Test 6: Reachable-blob guard
// ---------------------------------------------------------------------------

#[test]
fn doctor_fix_refuses_when_working_tree_has_unreachable_content() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Advance the ww branch and reset index (working-tree drift setup).
    let c2 = make_commit(&ws.server_primary, "other.txt", "x\n", "primary: C2");
    make_working_tree_stale(&ws.server_ww, &ws.server_primary, &c2);

    // Write UNIQUE content to a tracked file — content not in any committed blob.
    std::fs::write(
        ws.server_ww.join("README.md"),
        "content that has never been committed anywhere\n",
    )
    .unwrap();

    // doctor --fix must refuse (live edits present).
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .assert()
        .stdout(
            predicate::str::contains("live edits")
                .or(predicate::str::contains("manual review")),
        );

    // Unique content must still be intact.
    let content = std::fs::read_to_string(ws.server_ww.join("README.md")).unwrap();
    assert!(
        content.contains("content that has never been committed anywhere"),
        "unreachable content must survive --fix; got: {content}"
    );
}

// ---------------------------------------------------------------------------
// Test 7: Composition with index fix (the real fo-city 2026-04-23 case)
// ---------------------------------------------------------------------------

/// Combined scenario: BOTH index and working tree are stale.
/// `rwv doctor --fix` must clean both, leaving no residual diffs.
#[test]
fn doctor_fix_clears_both_index_and_working_tree_drift() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");

    // Advance the ww branch ref WITHOUT resetting the index.
    // This leaves: HEAD = C2, index = C1 tree, working tree = C1 files.
    // Both index drift AND working-tree drift are present simultaneously.
    advance_ww_branch(&ws.server_primary, "ww/main", &c2);

    // Confirm both drifts are present.
    let idx_diff = Command::new("git")
        .args(["diff-index", "--cached", "--name-only", "HEAD"])
        .current_dir(&ws.server_ww)
        .output()
        .unwrap();
    assert!(
        !idx_diff.stdout.is_empty(),
        "index should be stale before --fix"
    );
    let wt_diff = Command::new("git")
        .args(["diff-index", "--name-only", "HEAD"])
        .current_dir(&ws.server_ww)
        .output()
        .unwrap();
    assert!(
        !wt_diff.stdout.is_empty(),
        "working tree should be stale before --fix"
    );

    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .assert()
        .stdout(predicate::str::contains("fixed").or(predicate::str::contains("refreshed")));

    // After --fix: index must match HEAD.
    let idx_after = Command::new("git")
        .args(["diff-index", "--cached", "--exit-code", "HEAD"])
        .current_dir(&ws.server_ww)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        idx_after.success(),
        "index should match HEAD after --fix (no residual index diff)"
    );

    // After --fix: working tree must match HEAD.
    let wt_after = Command::new("git")
        .args(["diff-index", "--exit-code", "HEAD"])
        .current_dir(&ws.server_ww)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        wt_after.success(),
        "working tree should match HEAD after --fix (no residual working-tree diff)"
    );

    // `git status` should report a clean working directory — no confusion.
    let status_out = git_out(&["status", "--porcelain"], &ws.server_ww);
    assert!(
        status_out.is_empty(),
        "git status should be clean after fixing both drifts; got:\n{status_out}"
    );
}
