//! Integration tests for working-tree drift detection (`rwv doctor`) and
//! auto-fix (`rwv doctor --fix`).
//!
//! These tests exercise the seven acceptance scenarios plus the missing-
//! canonical-clone re-attribution:
//!   1. Stale-working-tree detection
//!   2. Stale-working-tree auto-fix
//!   3. Live edit NOT fixed
//!   4. Sync post-refresh (working-tree version)
//!   5. 3+ worktrees
//!   6. Reachable-blob guard
//!   7. Composition with index fix (the real 2026-04-23 production case)
//!   8. Missing canonical clone — re-attributed finding (not "live edits")
//!   9. Missing canonical clone — primary intact regression
//!  10. Missing canonical clone — DanglingReference fires alongside
//!
//! # How working-tree drift is simulated
//!
//! The actual drift condition (working tree behind HEAD) arises when:
//! - Worktree B is on branch `ww/main`
//! - Another process advances `refs/heads/ww/main`
//! - B's HEAD (via symbolic ref) now points at the new commit
//! - B's index and on-disk files still hold the old tip's content
//!
//! Tests simulate this with `git update-ref` run in the primary's checkout,
//! which leaves B's own `HEAD` reflog where B last put it. A `git reset` run
//! in B would record the move as B's own, and what it leaves is then an edit
//! returning files to earlier content, not drift.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

use crate::common;

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

/// Initialize a git repo with one commit. Returns HEAD SHA.
fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

/// Commit a file change. Returns new HEAD SHA.
fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// Write rwv.toml.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
}

fn rwv() -> AssertCommand {
    common::rwv()
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
    common::fixture_lock(&project_primary, &[(SERVER_PATH, SERVER_URL, &c1)]);
    common::git_in(&project_primary, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&project_primary, &["commit", "-m", "lock: initial"]);
    common::record_trunk(&project_primary);

    let ww_root = parent.join(".workweaves").join("ws--ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let server_ww = ww_root.join(SERVER_PATH);
    common::git_in(
        &server_primary,
        &[
            "worktree",
            "add",
            "-b",
            "ww/main",
            &server_ww.to_string_lossy(),
        ],
    );

    let project_ww = ww_root.join("projects/web-app");
    common::git_in(
        &project_primary,
        &[
            "worktree",
            "add",
            "-b",
            "ww/project",
            &project_ww.to_string_lossy(),
        ],
    );
    common::record_feature(&project_ww, "ww/project", "main");

    let primary_canon = repoweave::path_spelling::resolve(&primary_root)
        .unwrap()
        .display()
        .to_string();
    let marker_content = common::workweave_marker(&primary_canon, "web-app", &primary_canon);
    std::fs::write(ww_root.join(".rwv-workweave"), marker_content).unwrap();
    common::register_workweave(&primary_canon, "web-app", "ww", &ww_root);

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
    common::git_in(
        server_primary,
        &["update-ref", &format!("refs/heads/{branch}"), new_sha],
    );
}

/// Simulate working-tree drift: advance the ww branch ref to `new_sha` from
/// outside the workweave's checkout.
///
/// After this call: HEAD = new_sha, index and working tree = the old tip's.
fn make_working_tree_stale(server_primary: &Path, new_sha: &str) {
    advance_ww_branch(server_primary, "ww/main", new_sha);
}

// ---------------------------------------------------------------------------
// Test 1: Stale-working-tree detection
// ---------------------------------------------------------------------------

#[test]
fn doctor_detects_stale_working_tree_in_workweave() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");
    make_working_tree_stale(&ws.server_primary, &c2);

    // Verify working tree IS stale before doctor.
    let wt_diff = common::git()
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
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Write an UNTRACKED file — must survive the fix.
    std::fs::write(ws.server_ww.join("untracked.txt"), "untouched\n").unwrap();

    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");
    make_working_tree_stale(&ws.server_primary, &c2);

    // Confirm working tree is stale.
    let diff_before = common::git()
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
    let diff_after = common::git()
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
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // C2 on primary — we'll advance the ww branch ref to it.
    let c2 = make_commit(&ws.server_primary, "unrelated.txt", "x\n", "primary: C2");
    advance_ww_branch(&ws.server_primary, "ww/main", &c2);

    // Now write a live edit to a tracked file with UNIQUE content (never committed).
    std::fs::write(
        ws.server_ww.join("README.md"),
        "my unique edit — never committed\n",
    )
    .unwrap();

    // doctor --fix must NOT touch the live edit.
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .assert()
        .stdout(
            predicate::str::contains("live edits").or(predicate::str::contains("manual review")),
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
    let tmp = common::tempdir().unwrap();

    let primary_root = tmp.path().join("primary");
    std::fs::create_dir_all(primary_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(primary_root.join("projects")).unwrap();

    let server_primary = primary_root.join(SERVER_PATH);
    let c1 = init_repo(&server_primary);
    let origin = tmp.path().join("server-origin.git");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            "--bare",
            &server_primary.to_string_lossy(),
            &origin.to_string_lossy(),
        ],
    );
    let server_url = common::file_url(&origin);

    let project_primary = primary_root.join("projects/web-app");
    init_repo(&project_primary);
    write_manifest(&project_primary, &[(SERVER_PATH, &server_url)]);
    common::fixture_lock(&project_primary, &[(SERVER_PATH, &server_url, &c1)]);
    common::git_in(&project_primary, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&project_primary, &["commit", "-m", "lock: initial"]);
    common::record_trunk(&project_primary);

    let ww_root = tmp.path().join(".workweaves").join("primary--ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let server_ww = ww_root.join(SERVER_PATH);
    common::git_in(
        &server_primary,
        &[
            "worktree",
            "add",
            "-b",
            "ww/project",
            &server_ww.to_string_lossy(),
        ],
    );

    let project_ww = ww_root.join("projects/web-app");
    common::git_in(
        &project_primary,
        &[
            "worktree",
            "add",
            "-b",
            "ww/project",
            &project_ww.to_string_lossy(),
        ],
    );
    common::record_feature(&project_ww, "ww/project", "main");

    let primary_canon = repoweave::path_spelling::resolve(&primary_root)
        .unwrap()
        .display()
        .to_string();
    let marker_content = common::workweave_marker(&primary_canon, "web-app", &primary_canon);
    std::fs::write(ww_root.join(".rwv-workweave"), marker_content).unwrap();
    common::register_workweave(&primary_canon, "web-app", "ww", &ww_root);
    // Action verbs need the primary's pointer (or --project); in the
    // workweave the marker names the project, and a root carrying both files
    // is refused at resolution.
    std::fs::write(primary_root.join(".rwv-active"), "web-app\n").unwrap();

    // Primary commits C2 and updates lock.
    let c2 = make_commit(&server_primary, "advance.txt", "new\n", "primary: C2");
    common::fixture_lock(&project_primary, &[(SERVER_PATH, &server_url, &c2)]);
    common::git_in(&project_primary, &["add", "rwv.lock"]);
    common::git_in(&project_primary, &["commit", "-m", "lock: advance"]);

    // Mirror the lock advance in the workweave's own project worktree so
    // that sync's CWD-lock-freshness precondition passes once the server
    // ref is advanced below. (The workweave's freshness check reads its
    // own committed lock, not primary's.)
    common::fixture_lock(&project_ww, &[(SERVER_PATH, &server_url, &c2)]);
    common::git_in(&project_ww, &["add", "rwv.lock"]);
    common::git_in(&project_ww, &["commit", "-m", "lock: ww advance"]);

    // Set up working-tree drift in the ww server repo.
    advance_ww_branch(&server_primary, "ww/project", &c2);

    let diff_before = common::git()
        .args(["diff-index", "--name-only", "HEAD"])
        .current_dir(&server_ww)
        .output()
        .unwrap();
    assert!(
        !diff_before.stdout.is_empty(),
        "working tree should be stale before sync"
    );

    rwv()
        .args(["sync", "--history", "linear"])
        .current_dir(&ww_root)
        .assert()
        .success();

    // After sync: working tree must match HEAD.
    let diff_after = common::git()
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

/// As above, with a revert touching nothing the move changed, so no path the
/// move changed holds HEAD's entry and only the operation in progress keeps
/// the stale path from being refreshed.
#[test]
fn doctor_fix_leaves_drift_beside_an_unrelated_revert() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());
    make_commit(&ws.server_ww, "added.txt", "added\n", "ww: add");
    common::git_in(&ws.server_ww, &["revert", "--no-commit", "HEAD"]);
    let scratch = tmp.path().join("scratch");
    common::git_in(
        &ws.server_primary,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &scratch.to_string_lossy(),
            "ww/main",
        ],
    );
    let moved = make_commit(&scratch, "change.txt", "new\n", "moved");
    common::git_in(
        &ws.server_primary,
        &["worktree", "remove", &scratch.to_string_lossy()],
    );
    advance_ww_branch(&ws.server_primary, "ww/main", &moved);
    let before = common::git_in(&ws.server_ww, &["ls-files", "-s"]);

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("drift in"),
        "nothing is refreshed: {stdout}"
    );
    assert_eq!(common::git_in(&ws.server_ww, &["ls-files", "-s"]), before);
    assert!(!ws.server_ww.join("change.txt").exists());
}

/// A drifted checkout stopped in a revert is left as it stands by the sync's
/// post-refresh: the revert's staged state and the stale path alike.
#[test]
fn sync_leaves_drift_in_a_checkout_stopped_in_a_revert() {
    let tmp = common::tempdir().unwrap();

    let primary_root = tmp.path().join("primary");
    std::fs::create_dir_all(primary_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(primary_root.join("projects")).unwrap();

    let server_primary = primary_root.join(SERVER_PATH);
    let c1 = init_repo(&server_primary);
    let origin = tmp.path().join("server-origin.git");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            "--bare",
            &server_primary.to_string_lossy(),
            &origin.to_string_lossy(),
        ],
    );
    let server_url = common::file_url(&origin);

    let project_primary = primary_root.join("projects/web-app");
    init_repo(&project_primary);
    write_manifest(&project_primary, &[(SERVER_PATH, &server_url)]);
    common::fixture_lock(&project_primary, &[(SERVER_PATH, &server_url, &c1)]);
    common::git_in(&project_primary, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&project_primary, &["commit", "-m", "lock: initial"]);
    common::record_trunk(&project_primary);

    let ww_root = tmp.path().join(".workweaves").join("primary--ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let server_ww = ww_root.join(SERVER_PATH);
    common::git_in(
        &server_primary,
        &[
            "worktree",
            "add",
            "-b",
            "ww/project",
            &server_ww.to_string_lossy(),
        ],
    );

    let project_ww = ww_root.join("projects/web-app");
    common::git_in(
        &project_primary,
        &[
            "worktree",
            "add",
            "-b",
            "ww/project",
            &project_ww.to_string_lossy(),
        ],
    );
    common::record_feature(&project_ww, "ww/project", "main");

    let primary_canon = repoweave::path_spelling::resolve(&primary_root)
        .unwrap()
        .display()
        .to_string();
    let marker_content = common::workweave_marker(&primary_canon, "web-app", &primary_canon);
    std::fs::write(ww_root.join(".rwv-workweave"), marker_content).unwrap();
    common::register_workweave(&primary_canon, "web-app", "ww", &ww_root);
    // Action verbs need the primary's pointer (or --project); in the
    // workweave the marker names the project, and a root carrying both files
    // is refused at resolution.
    std::fs::write(primary_root.join(".rwv-active"), "web-app\n").unwrap();

    // Primary commits C2 and updates lock.
    let c2 = make_commit(&server_primary, "advance.txt", "new\n", "primary: C2");
    common::fixture_lock(&project_primary, &[(SERVER_PATH, &server_url, &c2)]);
    common::git_in(&project_primary, &["add", "rwv.lock"]);
    common::git_in(&project_primary, &["commit", "-m", "lock: advance"]);

    // Mirror the lock advance in the workweave's own project worktree so
    // that sync's CWD-lock-freshness precondition passes once the server
    // ref is advanced below. (The workweave's freshness check reads its
    // own committed lock, not primary's.)
    common::fixture_lock(&project_ww, &[(SERVER_PATH, &server_url, &c2)]);
    common::git_in(&project_ww, &["add", "rwv.lock"]);
    common::git_in(&project_ww, &["commit", "-m", "lock: ww advance"]);

    common::git_in(&server_ww, &["revert", "--no-commit", "HEAD"]);
    advance_ww_branch(&server_primary, "ww/project", &c2);
    let state = |repo: &Path| {
        format!(
            "{}\n{}\n{:?}\n{:?}",
            common::git_in(repo, &["ls-files", "-s"]),
            common::git_in(repo, &["status", "--porcelain"]),
            std::fs::read(repo.join("README.md")).ok(),
            std::fs::read(repo.join("advance.txt")).ok(),
        )
    };
    let before = state(&server_ww);

    let out = rwv().args(["sync"]).current_dir(&ww_root).output().unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("refreshed drift"), "{stderr}");
    assert_eq!(state(&server_ww), before, "{stderr}");
    assert!(
        !common::git_in(&server_ww, &["rev-parse", "-q", "--verify", "REVERT_HEAD"]).is_empty(),
        "the revert is still in progress:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 5: 3+ worktrees
// ---------------------------------------------------------------------------

#[test]
fn doctor_detects_working_tree_drift_in_three_worktrees() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Create a second workweave.
    let ww2_root = tmp.path().join(".workweaves").join("ws--ww2");
    std::fs::create_dir_all(ww2_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww2_root.join("projects")).unwrap();

    let server_ww2 = ww2_root.join(SERVER_PATH);
    common::git_in(
        &ws.server_primary,
        &[
            "worktree",
            "add",
            "-b",
            "ww2/main",
            &server_ww2.to_string_lossy(),
        ],
    );

    let project_primary = ws.primary_root.join("projects/web-app");
    let project_ww2 = ww2_root.join("projects/web-app");
    common::git_in(
        &project_primary,
        &[
            "worktree",
            "add",
            "-b",
            "ww2/project",
            &project_ww2.to_string_lossy(),
        ],
    );

    let primary_canon2 = repoweave::path_spelling::resolve(&ws.primary_root)
        .unwrap()
        .display()
        .to_string();
    let marker2 = common::workweave_marker(&primary_canon2, "web-app", &primary_canon2);
    std::fs::write(ww2_root.join(".rwv-workweave"), marker2).unwrap();
    common::register_workweave(&primary_canon2, "web-app", "ww2", &ww2_root);

    // Commit C2 and make BOTH workweaves stale.
    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");
    make_working_tree_stale(&ws.server_primary, &c2);
    advance_ww_branch(&ws.server_primary, "ww2/main", &c2);

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
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Advance the ww branch and reset index (working-tree drift setup).
    let c2 = make_commit(&ws.server_primary, "other.txt", "x\n", "primary: C2");
    make_working_tree_stale(&ws.server_primary, &c2);

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
            predicate::str::contains("live edits").or(predicate::str::contains("manual review")),
        );

    // Unique content must still be intact.
    let content = std::fs::read_to_string(ws.server_ww.join("README.md")).unwrap();
    assert!(
        content.contains("content that has never been committed anywhere"),
        "unreachable content must survive --fix; got: {content}"
    );
}

/// Edits whose content a commit already holds, in a checkout whose branch did
/// not move under it: a deletion, and a staged revert, which leaves the index
/// at the parent's tree exactly as drift from the parent would. Neither is
/// drift, so `--fix` reports both as live and leaves them as they are.
#[test]
fn doctor_fix_leaves_edits_that_hold_committed_content() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());
    make_commit(&ws.server_ww, "added.txt", "added\n", "ww: add");
    common::git_in(&ws.server_ww, &["revert", "--no-commit", "HEAD"]);
    std::fs::remove_file(ws.server_ww.join("README.md")).unwrap();
    let before = common::git_in(&ws.server_ww, &["status", "--porcelain"]);

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("marked for commit") && stdout.contains("live edits"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("drift in"),
        "nothing is refreshed: {stdout}"
    );
    assert_eq!(
        common::git_in(&ws.server_ww, &["status", "--porcelain"]),
        before
    );
    assert!(!ws.server_ww.join("README.md").exists());
    assert!(!ws.server_ww.join("added.txt").exists());
}

/// A drifted checkout stopped in a revert is left as it stands: refreshing
/// an index entry would end the revert.
#[test]
fn doctor_fix_leaves_drift_in_a_checkout_stopped_in_a_revert() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());
    make_commit(&ws.server_ww, "added.txt", "added\n", "ww: add");
    common::git_in(&ws.server_ww, &["revert", "--no-commit", "HEAD"]);
    let ww_tip = common::git_in(&ws.server_ww, &["rev-parse", "HEAD"]);
    let moved = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");
    let moved = common::git_in(
        &ws.server_primary,
        &[
            "commit-tree",
            &format!("{moved}^{{tree}}"),
            "-p",
            &ww_tip,
            "-m",
            "moved",
        ],
    );
    advance_ww_branch(&ws.server_primary, "ww/main", &moved);
    let before = common::git_in(&ws.server_ww, &["status", "--porcelain"]);

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("drift in"),
        "nothing is refreshed: {stdout}"
    );
    assert!(
        common::git()
            .args(["rev-parse", "-q", "--verify", "REVERT_HEAD"])
            .current_dir(&ws.server_ww)
            .status()
            .unwrap()
            .success(),
        "the revert is still in progress"
    );
    assert_eq!(
        common::git_in(&ws.server_ww, &["status", "--porcelain"]),
        before
    );
}

// ---------------------------------------------------------------------------
// Test 7: Composition with index fix (the real 2026-04-23 production case)
// ---------------------------------------------------------------------------

/// Combined scenario: BOTH index and working tree are stale.
/// `rwv doctor --fix` must clean both, leaving no residual diffs.
#[test]
fn doctor_fix_clears_both_index_and_working_tree_drift() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");

    // Advance the ww branch ref WITHOUT resetting the index.
    // This leaves: HEAD = C2, index = C1 tree, working tree = C1 files.
    // Both index drift AND working-tree drift are present simultaneously.
    advance_ww_branch(&ws.server_primary, "ww/main", &c2);

    // Confirm both drifts are present.
    let idx_diff = common::git()
        .args(["diff-index", "--cached", "--name-only", "HEAD"])
        .current_dir(&ws.server_ww)
        .output()
        .unwrap();
    assert!(
        !idx_diff.stdout.is_empty(),
        "index should be stale before --fix"
    );
    let wt_diff = common::git()
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
    let idx_after = common::git()
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
    let wt_after = common::git()
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
    let status_out = common::git_in(&ws.server_ww, &["status", "--porcelain"]);
    assert!(
        status_out.is_empty(),
        "git status should be clean after fixing both drifts; got:\n{status_out}"
    );
}

// ---------------------------------------------------------------------------
// Test 8: Missing canonical clone — re-attributed finding (not "live edits")
// ---------------------------------------------------------------------------

/// When the primary clone dir is removed out-of-band the linked worktree in
/// the workweave must be reported as "canonical clone … absent" (not "live
/// edits"). This is the core re-attribution scenario.
#[test]
fn doctor_reports_missing_canonical_not_live_edits() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Remove the primary clone out-of-band, as if the user ran `rm -rf`.
    std::fs::remove_dir_all(&ws.server_primary)
        .expect("failed to remove primary clone for test setup");

    // The workweave worktree still exists on disk; its .git file points at
    // the now-gone primary.
    assert!(
        ws.server_ww.exists(),
        "workweave worktree must still exist on disk after primary removal"
    );

    let out = rwv()
        .args(["doctor"])
        .current_dir(&ws.primary_root)
        .output()
        .expect("rwv doctor failed to start");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Must name the canonical-clone-missing root cause.
    assert!(
        stdout.contains("canonical clone")
            || stdout.contains("absent")
            || stdout.contains("cannot be classified"),
        "doctor must report canonical-clone-missing finding; got:\n{stdout}"
    );

    // Must NOT misattribute as live edits.
    assert!(
        !stdout.contains("live edits"),
        "doctor must NOT report 'live edits' when canonical is absent; got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Test 9: Missing canonical clone — primary intact regression
// ---------------------------------------------------------------------------

/// When the canonical clone is intact the existing drift classification must
/// be unchanged. This is a regression guard: the pre-flight detection must
/// not suppress legitimate drift findings.
#[test]
fn doctor_canonical_intact_classification_unchanged() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Introduce real working-tree drift (stale, not live edits).
    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");
    make_working_tree_stale(&ws.server_primary, &c2);

    // canonical is intact — classification must proceed normally.
    let out = rwv()
        .args(["doctor"])
        .current_dir(&ws.primary_root)
        .output()
        .expect("rwv doctor failed to start");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Must detect legitimate working-tree drift.
    assert!(
        stdout.contains("working tree stale") || stdout.contains("safe to --fix"),
        "doctor must still report working-tree drift when canonical is intact; got:\n{stdout}"
    );

    // Must NOT emit the canonical-missing finding (canonical is fine).
    assert!(
        !stdout.contains("canonical clone"),
        "doctor must NOT emit canonical-missing when canonical is present; got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// Test 10: Missing canonical clone — DanglingReference fires alongside
// ---------------------------------------------------------------------------

/// When the primary clone is removed the doctor should also emit a
/// DanglingReference for the primary-weave entry (since the repo is listed
/// in rwv.toml but no longer on disk). Both findings must appear together.
#[test]
fn doctor_dangling_reference_fires_alongside_missing_canonical() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Remove the primary clone — makes the primary weave entry dangling.
    std::fs::remove_dir_all(&ws.server_primary)
        .expect("failed to remove primary clone for test setup");

    let out = rwv()
        .args(["doctor"])
        .current_dir(&ws.primary_root)
        .output()
        .expect("rwv doctor failed to start");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // DanglingReference must appear for the missing primary-weave clone.
    assert!(
        stdout.contains("dangling reference") || stdout.contains("not cloned on disk"),
        "doctor must emit DanglingReference for the missing primary clone; got:\n{stdout}"
    );

    // The canonical-missing re-attribution must appear for the workweave worktree.
    assert!(
        stdout.contains("canonical clone")
            || stdout.contains("absent")
            || stdout.contains("cannot be classified"),
        "doctor must emit canonical-clone-missing for the workweave worktree; got:\n{stdout}"
    );
}
