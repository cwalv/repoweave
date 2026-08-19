//! Integration tests for index-drift detection (`rwv doctor`) and
//! auto-fix (`rwv doctor --fix`).
//!
//! These tests exercise the six acceptance scenarios:
//!   1. Stale-index detection (safe class)
//!   2. Stale-index auto-fix
//!   3. Live staged changes not fixed
//!   4. Sync post-condition refresh
//!   5. Reachable-tree guard (non-reachable tree not touched)
//!   6. Tag-form lock pin (sync precondition tag-deref regression guard)
//!
//! # How stale-index drift is simulated
//!
//! The actual drift condition (HEAD ahead of index) arises when:
//! - Worktree B is on branch `ww/main`
//! - Another process (worktree A or `git update-ref`) advances `refs/heads/ww/main`
//!   from outside worktree B
//! - Worktree B's HEAD (via the symbolic ref) now resolves to the new commit,
//!   but its index file was never updated
//!
//! Tests simulate this via `git update-ref refs/heads/ww/main <new-sha>` on the
//! primary repo after committing C2.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

fn git_tag(repo: &Path, tag: &str) {
    let out = common::git()
        .args(["tag", tag])
        .current_dir(repo)
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git tag failed to start");
    assert!(
        out.status.success(),
        "git tag {} failed: {}",
        tag,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Initialise a git repo with one commit. Returns HEAD SHA.
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
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
}

fn rwv() -> AssertCommand {
    common::rwv()
}

// ---------------------------------------------------------------------------
// Shared fixture
//
// Creates a primary weave and a workweave whose server repo is on a separate
// branch (`ww/main`) starting from C1.  Tests advance the branch ref via
// `git update-ref` (from the primary server repo) to simulate the
// shared-ref-advance mechanism without triggering git's branch-lock check.
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
///
/// To simulate shared-ref drift, callers can advance `refs/heads/ww/main`
/// inside `server_primary` using `git update-ref`.
fn make_workspace_with_ww(parent: &Path) -> (Workspace, String) {
    // --- Primary weave ---
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

    // --- Workweave ---
    // Place under .workweaves/ws--ww/ (the naming convention rwv uses)
    let ww_root = parent.join(".workweaves").join("ws--ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    // Worktree on a NEW branch `ww/main` (not the same as primary's `main`).
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

    // Project repo worktree on a new branch `ww/project`.
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

    // .rwv-workweave marker so `rwv` resolves the workweave correctly.
    let primary_canon = primary_root.canonicalize().unwrap().display().to_string();
    let marker_content = common::workweave_marker(&primary_canon, "web-app", &primary_canon);
    std::fs::write(ww_root.join(".rwv-workweave"), marker_content).unwrap();
    common::register_workweave(&primary_canon, "web-app", "ww", &ww_root);
    // The pointer is primary-only: the workweave's marker already names its
    // project, and a root carrying both files is refused at resolution.
    std::fs::write(primary_root.join(".rwv-active"), "web-app\n").unwrap();

    (
        Workspace {
            primary_root,
            server_primary,
            server_ww,
        },
        c1,
    )
}

/// Advance `refs/heads/ww/main` (the branch the workweave server repo is on)
/// to `new_sha` from the primary server repo.
///
/// This simulates the shared-ref-advance mechanism: the branch ref moves but
/// the worktree's index is not updated, producing the stale-index condition.
fn advance_ww_branch(server_primary: &Path, branch: &str, new_sha: &str) {
    common::git_in(
        server_primary,
        &["update-ref", &format!("refs/heads/{branch}"), new_sha],
    );
}

// ---------------------------------------------------------------------------
// Test 1: Stale-index detection
// ---------------------------------------------------------------------------

/// Commit C2 in the primary; advance the workweave's branch ref to C2 (via
/// `git update-ref`). Assert `rwv doctor` from the primary detects the stale
/// index in the workweave and reports "index stale (safe to --fix)".
#[test]
fn doctor_detects_stale_index_in_workweave() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Commit C2 on primary's `main`.
    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");

    // Advance workweave's branch ref to C2 — simulates shared-ref advance.
    advance_ww_branch(&ws.server_primary, "ww/main", &c2);

    // Verify the workweave's index IS stale (HEAD=C2, index=C1-tree).
    let diff = common::git()
        .args(["diff-index", "--cached", "--name-only", "HEAD"])
        .current_dir(&ws.server_ww)
        .output()
        .unwrap();
    assert!(
        !diff.stdout.is_empty(),
        "workweave index should be stale before doctor"
    );

    // Running doctor from the primary should report the workweave's stale index.
    rwv()
        .args(["doctor"])
        .current_dir(&ws.primary_root)
        .assert()
        .stdout(
            predicate::str::contains("index stale").or(predicate::str::contains("safe to --fix")),
        );
}

// ---------------------------------------------------------------------------
// Test 2: Stale-index auto-fix
// ---------------------------------------------------------------------------

/// Same drift setup as test 1. After `rwv doctor --fix`:
/// - The workweave's index matches HEAD
/// - An unstaged working-tree file is untouched
#[test]
fn doctor_fix_refreshes_stale_index_without_touching_working_tree() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Write an UNSTAGED file in the workweave working tree.
    std::fs::write(ws.server_ww.join("unstaged.txt"), "working tree file\n").unwrap();

    // Commit C2 and advance workweave's branch → stale index.
    let c2 = make_commit(&ws.server_primary, "change.txt", "new\n", "primary: C2");
    advance_ww_branch(&ws.server_primary, "ww/main", &c2);

    // Confirm stale before fix.
    let diff_before = common::git()
        .args(["diff-index", "--cached", "--name-only", "HEAD"])
        .current_dir(&ws.server_ww)
        .output()
        .unwrap();
    assert!(
        !diff_before.stdout.is_empty(),
        "index should be stale before --fix"
    );

    // Run doctor --fix from the primary.
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .assert()
        .stdout(predicate::str::contains("fixed").or(predicate::str::contains("refreshed")));

    // After fix: index must match HEAD.
    let diff_after = common::git()
        .args(["diff-index", "--cached", "--exit-code", "HEAD"])
        .current_dir(&ws.server_ww)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(diff_after.success(), "index should match HEAD after --fix");

    // Working tree file must still exist.
    assert!(
        ws.server_ww.join("unstaged.txt").exists(),
        "unstaged working tree file should survive --fix"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Live staged changes not fixed
// ---------------------------------------------------------------------------

/// Workweave has live staged content (new file, never committed). After the
/// branch ref is advanced (simulating drift), `rwv doctor --fix` must NOT
/// discard the staged content.
#[test]
fn doctor_fix_does_not_clobber_live_staged_changes() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Stage new content in the workweave — this file has NEVER been committed.
    std::fs::write(ws.server_ww.join("new_feature.rs"), "fn foo() {}\n").unwrap();
    common::git_in(&ws.server_ww, &["add", "new_feature.rs"]);

    // Commit C2 and advance the ww branch ref (but the index now also has
    // new_feature.rs staged, so the index tree != any ancestor tree).
    let c2 = make_commit(&ws.server_primary, "unrelated.txt", "x\n", "primary: C2");
    advance_ww_branch(&ws.server_primary, "ww/main", &c2);

    // doctor --fix should report "manual review" / "live staged", not "[fixed]".
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .assert()
        .stdout(
            predicate::str::contains("manual review").or(predicate::str::contains("live staged")),
        );

    // The staged file must still be staged.
    let staged = common::git_in(&ws.server_ww, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.contains("new_feature.rs"),
        "new_feature.rs should still be staged after doctor --fix; got: {staged}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Sync post-condition refresh
// ---------------------------------------------------------------------------

/// After `rwv sync`, if the workweave's server repo had a stale index before
/// the sync (from a shared-ref advance), the index should be clean afterward.
#[test]
fn sync_post_refresh_clears_stale_index() {
    let tmp = common::tempdir().unwrap();

    // --- Primary workspace ---
    let primary_root = tmp.path().join("primary");
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

    // --- Workweave ---
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
    // The ww project worktree inherits the already-committed rwv.lock@c1 from
    // project_primary. No additional commit needed — the lock already matches HEAD.

    let primary_canon = primary_root.canonicalize().unwrap().display().to_string();
    let marker_content = common::workweave_marker(&primary_canon, "web-app", &primary_canon);
    std::fs::write(ww_root.join(".rwv-workweave"), marker_content).unwrap();
    common::register_workweave(&primary_canon, "web-app", "ww", &ww_root);
    // Action verbs need the primary's pointer (or --project); in the
    // workweave the marker names the project, and a root carrying both files
    // is refused at resolution.
    std::fs::write(primary_root.join(".rwv-active"), "web-app\n").unwrap();

    // --- Primary commits C2, updates lock ---
    let c2 = make_commit(&server_primary, "advance.txt", "new\n", "primary: C2");
    common::fixture_lock(&project_primary, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&project_primary, &["add", "rwv.lock"]);
    common::git_in(&project_primary, &["commit", "-m", "lock: advance"]);

    // Mirror the lock advance into the workweave's own project worktree so
    // that the workweave's lock matches the workweave's tip after we advance
    // the server ref below. Without this, sync's CWD-lock-freshness
    // precondition (which reads the workweave's own committed lock)
    // would fail before reaching the
    // post-refresh path this test is exercising.
    common::fixture_lock(&project_ww, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&project_ww, &["add", "rwv.lock"]);
    common::git_in(&project_ww, &["commit", "-m", "lock: ww advance"]);

    // Advance the workweave's server branch ref to C2 (stale-index setup).
    advance_ww_branch(&server_primary, "ww/main", &c2);

    // Verify stale before sync.
    let diff_before = common::git()
        .args(["diff-index", "--cached", "--name-only", "HEAD"])
        .current_dir(&server_ww)
        .output()
        .unwrap();
    assert!(
        !diff_before.stdout.is_empty(),
        "workweave index should be stale before sync"
    );

    // Sync from primary to workweave. Both sides committed independent lock
    // advances above (so each side's lock-freshness check passes), which
    // diverges the project repos; --discard-local-commits is required to
    // bypass the Phase 1 ancestor precondition. The test's focus is the
    // Phase 2 post-refresh.
    rwv()
        .args([
            "sync",
            &primary_root.to_string_lossy(),
            "--discard-local-commits",
        ])
        .current_dir(&ww_root)
        .assert()
        .success();

    // After sync: index should match HEAD.
    let diff_after = common::git()
        .args(["diff-index", "--cached", "--exit-code", "HEAD"])
        .current_dir(&server_ww)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        diff_after.success(),
        "workweave index should match HEAD after sync"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Reachable-tree guard
// ---------------------------------------------------------------------------

/// When the workweave's index holds a tree that is NOT reachable from HEAD
/// (the user has staged new-never-committed content), `--fix` must refuse
/// and the staged content must survive.
#[test]
fn doctor_fix_refuses_non_reachable_index_tree() {
    let tmp = common::tempdir().unwrap();
    let (ws, _c1) = make_workspace_with_ww(tmp.path());

    // Stage content that has never been committed (tree will not be found
    // in any ancestor commit).
    std::fs::write(ws.server_ww.join("brand_new.rs"), "// never committed\n").unwrap();
    common::git_in(&ws.server_ww, &["add", "brand_new.rs"]);

    // Also advance the branch to make HEAD != index (so doctor sees drift).
    let c2 = make_commit(&ws.server_primary, "other.txt", "x\n", "primary: C2");
    advance_ww_branch(&ws.server_primary, "ww/main", &c2);

    // doctor --fix must NOT clear the staged content.
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws.primary_root)
        .assert()
        .stdout(
            predicate::str::contains("manual review").or(predicate::str::contains("live staged")),
        );

    // The staged file must still be there.
    let staged = common::git_in(&ws.server_ww, &["diff", "--cached", "--name-only"]);
    assert!(
        staged.contains("brand_new.rs"),
        "staged file should survive --fix on non-reachable index tree; got: {staged}"
    );
}

// ---------------------------------------------------------------------------
// Test 6: Tag-form lock pin (sync precondition tag-deref regression guard)
// ---------------------------------------------------------------------------

/// Source lock is pinned by a tag name (e.g., `v1.0.0`) whose commit matches
/// HEAD. `rwv sync` must NOT falsely report "source lock is stale".
/// This is the regression guard for the sync tag-drift companion fix.
#[test]
fn sync_precondition_accepts_tag_form_lock_entry() {
    let tmp = common::tempdir().unwrap();

    // Source workspace: server at tag v1.0.0, lock records the tag name.
    let source_root = tmp.path().join("source");
    std::fs::create_dir_all(source_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(source_root.join("projects")).unwrap();

    let server_source = source_root.join(SERVER_PATH);
    let sha = init_repo(&server_source);
    git_tag(&server_source, "v1.0.0");

    let project_source = source_root.join("projects/web-app");
    init_repo(&project_source);
    write_manifest(&project_source, &[(SERVER_PATH, SERVER_URL)]);
    // Lock pins the TAG name, not the SHA.
    common::fixture_lock(&project_source, &[(SERVER_PATH, SERVER_URL, "v1.0.0")]);
    common::git_in(&project_source, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&project_source, &["commit", "-m", "lock: v1.0.0"]);

    // CWD workspace: also at the same commit (SHA lock entry).
    let cwd_root = tmp.path().join("cwd");
    std::fs::create_dir_all(cwd_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(cwd_root.join("projects")).unwrap();

    let server_cwd = cwd_root.join(SERVER_PATH);
    // Share objects via a worktree (different branch).
    common::git_in(
        &server_source,
        &[
            "worktree",
            "add",
            "-b",
            "cwd/main",
            &server_cwd.to_string_lossy(),
        ],
    );

    let project_cwd = cwd_root.join("projects/web-app");
    common::git_in(
        &project_source,
        &[
            "worktree",
            "add",
            "-b",
            "cwd/project",
            &project_cwd.to_string_lossy(),
        ],
    );
    // CWD lock uses the SHA form.
    common::fixture_lock(&project_cwd, &[(SERVER_PATH, SERVER_URL, &sha)]);
    common::git_in(&project_cwd, &["add", "rwv.lock"]);
    common::git_in(&project_cwd, &["commit", "-m", "lock: sha form"]);

    // rwv sync from source (tag-pinned lock) must not emit "source lock is stale".
    let out = rwv()
        .args(["sync", &source_root.to_string_lossy()])
        .current_dir(&cwd_root)
        .assert();

    let output = out.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("lock is stale"),
        "rwv sync must not falsely report 'lock is stale' for a tag-pinned source lock; \
         stderr: {stderr}"
    );
}
