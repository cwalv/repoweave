//! Integration tests for the `rwv sync` pre-flight dirt scan.
//!
//! Covers four adversarial scenarios:
//!
//! 1. Dirty tracked file in the destination CWD manifest repo → eager refusal
//!    BEFORE any mutation (no op-state, no savepoints, tips untouched).
//! 2. Dirty tracked file in the destination project repo → same.
//! 3. Untracked-only file in the destination repos → sync proceeds (untracked
//!    files survive rebase/ff untouched; refusing would block normal work).
//! 4. Clean workweave → sync proceeds normally.
//!
//! Each scenario additionally verifies the no-trace postcondition: no `.rwv-op`
//! file is left behind, no `refs/rwv/pre-op/*` savepoints are created, and the
//! repos' HEADs are unchanged from before the refused invocation.
//!
//! ## Fixture topology
//!
//! Source (primary workspace) + destination (workweave-style: project repo and
//! manifest repo are git worktrees sharing history with the source). This gives
//! the two workspaces a shared commit ancestry so the `check_phase1_ancestor`
//! ff-precondition passes, and lets us meaningfully test the dirt scan as the
//! blocking precondition.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

fn rwv() -> AssertCommand {
    common::rwv()
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------
//
// PRIMARY workspace: the source of `rwv sync <source>`. Holds the canonical
// git object stores for both the project repo and the manifest repo.
//
// WORKWEAVE destination: a worktree pair sharing history with the primary.
// The destination gets `rwv sync <primary>` run against it.
//
// The worktree topology (shared history) is what lets the sync proceed past
// the `check_phase1_ancestor` ff-precondition; without shared history the two
// independent `git init` trees diverge and sync refuses on the ancestry gate
// before the dirt scan even runs.

const REPO_PATH: &str = "github/acme/lib";
const PROJECT: &str = "app";

struct Primary {
    root: PathBuf,
    project_dir: PathBuf,
    repo_dir: PathBuf,
}

struct Workweave {
    root: PathBuf,
    project_dir: PathBuf,
    repo_dir: PathBuf,
}

struct Fixture {
    _tmp: tempfile::TempDir,
    primary: Primary,
    ww: Workweave,
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    if let Some(parent) = repo.join(filename).parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(repo.join(filename), content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn head(repo: &Path) -> String {
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn has_op_state(workspace_root: &Path) -> bool {
    workspace_root.join(".rwv-op").exists()
}

/// Build the fixture: primary + one workweave sharing history.
fn fixture() -> Fixture {
    let tmp = common::tempdir().unwrap();

    // -----------------------------------------------------------------------
    // Primary workspace
    // -----------------------------------------------------------------------
    let primary_root = tmp.path().join("primary");
    let primary_repo = primary_root.join(REPO_PATH);
    let initial_sha = init_repo(&primary_repo);

    let primary_project = primary_root.join("projects").join(PROJECT);
    init_repo(&primary_project);
    std::fs::write(
        primary_project.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    let url = common::file_url(&primary_repo);
    let manifest = format!(
        "[repositories.\"{REPO_PATH}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(primary_project.join("rwv.toml"), manifest).unwrap();
    // Round-trips through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let raw_lock = format!(
        "{{\"repositories\": {{{REPO_PATH:?}: {{\"type\": \"git\", \"url\": {url:?}, \"version\": {initial_sha:?}}}}}}}"
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &primary_project.join("rwv.lock")).unwrap();
    common::git_in(
        &primary_project,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&primary_project, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary_root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    // -----------------------------------------------------------------------
    // Workweave (destination): worktrees of the primary repos.
    //
    // Using git worktrees gives the workweave's repos shared history with
    // the primary, so `rwv sync <primary>` can ff-advance the workweave's
    // branch when the primary has newer commits. Without shared history,
    // sync refuses on the ancestry gate before the dirt scan runs.
    // -----------------------------------------------------------------------
    let ww_root = tmp.path().join("ww");
    std::fs::create_dir_all(
        ww_root.join(
            primary_repo
                .parent()
                .unwrap()
                .strip_prefix(&primary_root)
                .unwrap(),
        ),
    )
    .unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let ww_repo = ww_root.join(REPO_PATH);
    common::git_in(
        &primary_repo,
        &[
            "worktree",
            "add",
            &ww_repo.to_string_lossy(),
            "-b",
            "ww/main",
        ],
    );

    let ww_project = ww_root.join("projects").join(PROJECT);
    common::git_in(
        &primary_project,
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );
    std::fs::write(ww_root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    Fixture {
        _tmp: tmp,
        primary: Primary {
            root: primary_root,
            project_dir: primary_project,
            repo_dir: primary_repo,
        },
        ww: Workweave {
            root: ww_root,
            project_dir: ww_project,
            repo_dir: ww_repo,
        },
    }
}

/// Advance the primary workspace: commit to the manifest repo AND update+commit
/// the lock so the workweave has something to pull.
fn advance_primary(f: &Fixture) -> String {
    let new_sha = make_commit(
        &f.primary.repo_dir,
        "advance.txt",
        "advance\n",
        "primary: advance",
    );
    let url = common::file_url(&f.primary.repo_dir);
    let raw_lock = format!(
        "{{\"repositories\": {{{REPO_PATH:?}: {{\"type\": \"git\", \"url\": {url:?}, \"version\": {new_sha:?}}}}}}}"
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &f.primary.project_dir.join("rwv.lock")).unwrap();
    common::git_in(&f.primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&f.primary.project_dir, &["commit", "-m", "lock: advance"]);
    new_sha
}

// ===========================================================================
// 1. Dirty tracked file in destination manifest repo → eager refusal, no trace
// ===========================================================================

/// CWD (destination) has a tracked modification in a manifest repo.
/// Sync must refuse immediately, name the dirty repo, and leave no op-state
/// or savepoint refs behind.
#[test]
fn sync_dirty_tracked_manifest_repo_refuses_before_mutation() {
    let f = fixture();

    // Primary advances so the sync would have real work to do.
    advance_primary(&f);

    // Record the pre-refusal HEADs so we can assert they are untouched.
    let ww_project_head_before = head(&f.ww.project_dir);
    let ww_repo_head_before = head(&f.ww.repo_dir);

    // Dirty a TRACKED file in the destination (ww) manifest repo. README.md
    // was committed during init, so it is a tracked file.
    std::fs::write(f.ww.repo_dir.join("README.md"), "dirty edit\n").unwrap();
    // Confirm the test precondition: the file is actually tracked-dirty.
    let porcelain = common::git_in(
        &f.ww.repo_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    );
    assert!(
        !porcelain.is_empty(),
        "test setup: README.md must be tracked-dirty; porcelain:\n{porcelain}"
    );

    let assert = rwv()
        .args(["sync", &f.primary.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    // Must name the dirty repo and the kind of dirt.
    assert!(
        stderr.contains("sync precondition failed")
            && stderr.contains("uncommitted tracked changes"),
        "refusal must name the sync precondition and kind of dirt; got:\n{stderr}"
    );
    assert!(
        stderr.contains(REPO_PATH),
        "refusal must name the dirty repo ({REPO_PATH}); got:\n{stderr}"
    );
    // Must mention remediation verbs (commit / stash).
    assert!(
        stderr.contains("commit") && stderr.contains("stash"),
        "refusal must suggest commit or stash; got:\n{stderr}"
    );

    // No op-state left behind (cleanup table: "precondition refusal → cleared").
    assert!(
        !has_op_state(&f.ww.root),
        "refusal must leave no .rwv-op file behind"
    );

    // No savepoint refs created in the project repo.
    let proj_savepoints = common::git_in(
        &f.ww.project_dir,
        &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-op/"],
    );
    assert!(
        proj_savepoints.is_empty(),
        "refusal must not create savepoint refs in the project repo; got:\n{proj_savepoints}"
    );
    // No savepoint refs created in the manifest repo.
    let repo_savepoints = common::git_in(
        &f.primary.repo_dir,
        &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-op/"],
    );
    assert!(
        repo_savepoints.is_empty(),
        "refusal must not create savepoint refs in the manifest repo; got:\n{repo_savepoints}"
    );

    // HEADs untouched.
    assert_eq!(
        head(&f.ww.project_dir),
        ww_project_head_before,
        "ww project repo HEAD must be unchanged after refusal"
    );
    assert_eq!(
        head(&f.ww.repo_dir),
        ww_repo_head_before,
        "ww manifest repo HEAD must be unchanged after refusal"
    );

    // The dirty content must survive unharmed.
    let content = std::fs::read_to_string(f.ww.repo_dir.join("README.md")).unwrap();
    assert_eq!(
        content, "dirty edit\n",
        "uncommitted content must survive the refusal byte-for-byte"
    );
}

// ===========================================================================
// 2. Dirty tracked file in destination project repo → eager refusal, no trace
// ===========================================================================

/// Destination has a tracked modification in the project repo (not a manifest
/// repo). Sync must refuse and name the project repo with the dirty file.
#[test]
fn sync_dirty_tracked_project_repo_refuses_before_mutation() {
    let f = fixture();
    advance_primary(&f);

    let ww_project_head_before = head(&f.ww.project_dir);

    // Dirty a tracked NON-lock file in the destination project repo (rwv.toml).
    let yaml_path = f.ww.project_dir.join("rwv.toml");
    let mut y = std::fs::read_to_string(&yaml_path).unwrap();
    y.push_str("# scratch\n");
    std::fs::write(&yaml_path, y).unwrap();

    // Confirm it's tracked-dirty.
    let porcelain = common::git_in(
        &f.ww.project_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    );
    assert!(
        !porcelain.is_empty(),
        "test setup: rwv.toml must be tracked-dirty; porcelain:\n{porcelain}"
    );

    let assert = rwv()
        .args(["sync", &f.primary.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("sync precondition failed")
            && stderr.contains("uncommitted tracked changes"),
        "refusal must name the sync precondition and kind of dirt; got:\n{stderr}"
    );
    assert!(
        stderr.contains("(project)") && stderr.contains("rwv.toml"),
        "refusal must name the project repo and the dirty file; got:\n{stderr}"
    );

    assert!(
        !has_op_state(&f.ww.root),
        "refusal must leave no .rwv-op file behind"
    );
    assert_eq!(
        head(&f.ww.project_dir),
        ww_project_head_before,
        "ww project repo HEAD must be unchanged after refusal"
    );
}

// ===========================================================================
// 3. Multiple dirty repos → all named in one refusal
// ===========================================================================

/// Both the manifest repo AND the project repo are dirty. The refusal must
/// list ALL dirty repos in a single message (no fail-on-first-drip).
#[test]
fn sync_multiple_dirty_repos_all_named_in_one_refusal() {
    let f = fixture();
    advance_primary(&f);

    // Dirty both the manifest repo and the project repo.
    std::fs::write(f.ww.repo_dir.join("README.md"), "dirty1\n").unwrap();
    let yaml_path = f.ww.project_dir.join("rwv.toml");
    let mut y = std::fs::read_to_string(&yaml_path).unwrap();
    y.push_str("# scratch\n");
    std::fs::write(&yaml_path, y).unwrap();

    let assert = rwv()
        .args(["sync", &f.primary.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains(REPO_PATH),
        "refusal must name the dirty manifest repo; got:\n{stderr}"
    );
    assert!(
        stderr.contains("(project)"),
        "refusal must name the dirty project repo; got:\n{stderr}"
    );

    assert!(
        !has_op_state(&f.ww.root),
        "multi-dirty refusal must leave no .rwv-op file behind"
    );
}

// ===========================================================================
// 4. Untracked-only file in destination → sync proceeds
// ===========================================================================

/// A destination manifest repo has an UNTRACKED file. Sync must NOT refuse.
/// Untracked files survive rebase and fast-forward untouched; blocking on
/// them would prevent normal in-progress work.
///
/// The untracked file must also still be present after the sync completes.
#[test]
fn sync_untracked_only_destination_proceeds() {
    let f = fixture();
    let new_sha = advance_primary(&f);

    // Place an UNTRACKED file in the destination manifest repo.
    std::fs::write(f.ww.repo_dir.join("scratch.tmp"), "untracked content\n").unwrap();
    // Confirm it's untracked (not in the tracked-porcelain output).
    let tracked = common::git_in(
        &f.ww.repo_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    );
    assert!(
        tracked.is_empty(),
        "test setup: scratch.tmp must be untracked; got:\n{tracked}"
    );

    rwv()
        .args(["sync", &f.primary.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .success();

    // Destination repo must have advanced to the new SHA.
    assert_eq!(
        head(&f.ww.repo_dir),
        new_sha,
        "destination manifest repo must have synced to the primary's tip"
    );

    // Untracked file must still be there, unharmed.
    let content = std::fs::read_to_string(f.ww.repo_dir.join("scratch.tmp")).unwrap();
    assert_eq!(
        content, "untracked content\n",
        "untracked file must survive the sync byte-for-byte"
    );
}

// ===========================================================================
// 5. Dirty rwv.lock in destination project → refused (no carve-out for sync)
// ===========================================================================

/// Unlike the sync-to source-side scan, `rwv sync` does NOT carve out a dirty
/// `rwv.lock`. The distinction:
///
/// - sync-to: the project repo is never ff'd during replay; Phase 3 regenerates
///   and commits the lock. A dirty lock is the auto-relock's input.
/// - sync (pull): Phase 1' fast-forwards or rebases the project repo to the
///   source's tip. `git merge --ff-only` and `git rebase` both fail on a dirty
///   tracked `rwv.lock`. The operator must stash or commit it.
///
/// This test verifies that a destination with a dirty-only rwv.lock is refused
/// (not silently passed through to a mid-op git failure).
#[test]
fn sync_dirty_lock_in_destination_project_refuses() {
    let f = fixture();
    advance_primary(&f);

    let ww_project_head_before = head(&f.ww.project_dir);

    // Hand-edit the destination's rwv.lock so it shows as tracked-dirty.
    // Trailing whitespace is the only append JSON tolerates without
    // becoming unparseable — a comment line (the YAML-era trick) is
    // trailing *content* and fails to parse.
    let lock_path = f.ww.project_dir.join("rwv.lock");
    let mut lock = std::fs::read_to_string(&lock_path).unwrap();
    lock.push('\n');
    std::fs::write(&lock_path, lock).unwrap();

    // Confirm the test precondition: rwv.lock is tracked-dirty.
    let porcelain = common::git_in(
        &f.ww.project_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    );
    assert!(
        porcelain.contains("rwv.lock"),
        "test setup: rwv.lock must be tracked-dirty; porcelain:\n{porcelain}"
    );

    // Sync must refuse (no carve-out for sync; git would fail mid-op anyway).
    let assert = rwv()
        .args(["sync", &f.primary.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("sync precondition failed")
            && stderr.contains("uncommitted tracked changes"),
        "dirty rwv.lock must trigger the sync dirt refusal; got:\n{stderr}"
    );
    assert!(
        stderr.contains("(project)") && stderr.contains("rwv.lock"),
        "refusal must name the project repo and the dirty file; got:\n{stderr}"
    );
    assert!(
        !has_op_state(&f.ww.root),
        "refusal must leave no .rwv-op file behind"
    );
    assert_eq!(
        head(&f.ww.project_dir),
        ww_project_head_before,
        "ww project repo HEAD must be unchanged after refusal"
    );
}

// ===========================================================================
// 6. Clean workweave → sync proceeds normally (positive baseline)
// ===========================================================================

/// Both source and destination are clean — sync must succeed.
/// This is the positive baseline confirming the dirt scan does not regress
/// the normal path.
#[test]
fn sync_clean_workweave_proceeds() {
    let f = fixture();
    let new_sha = advance_primary(&f);

    rwv()
        .args(["sync", &f.primary.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .success();

    assert_eq!(
        head(&f.ww.repo_dir),
        new_sha,
        "clean destination must sync to source's tip"
    );
}

// ===========================================================================
// 7. Staged (indexed) change in destination manifest repo → refuses
// ===========================================================================

/// A staged (added-but-not-committed) change in the destination manifest repo
/// must also refuse. `git rebase` fails on staged changes just as on unstaged.
#[test]
fn sync_staged_tracked_change_in_destination_refuses() {
    let f = fixture();
    advance_primary(&f);

    // Write AND stage a new file in the destination manifest repo.
    std::fs::write(f.ww.repo_dir.join("staged.txt"), "staged content\n").unwrap();
    common::git_in(&f.ww.repo_dir, &["add", "staged.txt"]);

    // Confirm it's tracked-dirty (staged).
    let porcelain = common::git_in(
        &f.ww.repo_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    );
    assert!(
        !porcelain.is_empty(),
        "test setup: staged.txt must show as tracked-dirty; porcelain:\n{porcelain}"
    );

    let assert = rwv()
        .args(["sync", &f.primary.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("sync precondition failed"),
        "staged tracked change must refuse the sync; got:\n{stderr}"
    );
    assert!(
        stderr.contains(REPO_PATH),
        "refusal must name the dirty repo; got:\n{stderr}"
    );
    assert!(
        !has_op_state(&f.ww.root),
        "refusal must leave no .rwv-op file behind"
    );
}

// ===========================================================================
// 8. Retry after dirt refusal succeeds (no stranded in-flight op)
// ===========================================================================

/// After a dirty-destination refusal, the operator fixes the dirt and retries.
/// The retry must succeed — the refusal must have left no stranded op-state
/// that would produce an in-flight refusal on the second attempt.
#[test]
fn sync_retry_after_dirt_refusal_succeeds() {
    let f = fixture();
    advance_primary(&f);

    // First attempt: dirty destination → refusal.
    std::fs::write(f.ww.repo_dir.join("README.md"), "dirty\n").unwrap();
    rwv()
        .args(["sync", &f.primary.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .failure();

    // Fix the dirt.
    common::git_in(&f.ww.repo_dir, &["restore", "README.md"]);
    let clean = common::git_in(
        &f.ww.repo_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    );
    assert!(
        clean.is_empty(),
        "test setup: ww repo must be clean before retry; porcelain:\n{clean}"
    );

    // Second attempt: clean destination → must succeed without an in-flight refusal.
    let assert = rwv()
        .args(["sync", &f.primary.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        !stderr.contains("in progress"),
        "retry must not see a stranded in-flight refusal; got:\n{stderr}"
    );
    assert.success();
}

// ===========================================================================
// 9. Attributable drift is excluded; mixed state names only user dirt
// ===========================================================================

/// A manifest repo whose branch ref was advanced externally (shared-ref
/// advance — index/working tree lag the moved tip; rwv-attributable drift)
/// plus a project repo with a genuine user edit. The refusal must name ONLY
/// the user dirt: the drifted repo is excluded because sync's replay loop
/// self-heals attributable drift (the pure-drift self-healing spec lives in
/// index_drift_test.rs / working_tree_drift_test.rs), and advising the
/// operator to commit/stash a moved-branch diff they never authored would be
/// harmful.
#[test]
fn sync_attributable_drift_excluded_from_refusal_mixed_with_user_dirt() {
    let f = fixture();
    let new_sha = advance_primary(&f);

    // Shared-ref advance: move ww's branch to the new tip from the canonical
    // store, leaving ww's index/working tree at the old state. Structurally
    // attributable: the lagging index tree is an ancestor commit's tree and
    // the "missing" file exists in HEAD (D entry — restorable from the DAG).
    common::git_in(
        &f.primary.repo_dir,
        &["update-ref", "refs/heads/ww/main", &new_sha],
    );
    // Sanity: the drifted repo DOES show tracked differences to git status —
    // without attribution this would have refused.
    let porcelain = common::git_in(
        &f.ww.repo_dir,
        &["status", "--porcelain", "--untracked-files=no"],
    );
    assert!(
        !porcelain.is_empty(),
        "test setup: shared-ref advance must show as tracked differences; got:\n{porcelain}"
    );

    // Genuine user dirt in the project repo (never-committed content).
    let yaml_path = f.ww.project_dir.join("rwv.toml");
    let mut y = std::fs::read_to_string(&yaml_path).unwrap();
    y.push_str("# scratch\n");
    std::fs::write(&yaml_path, y).unwrap();

    let assert = rwv()
        .args(["sync", &f.primary.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("(project)") && stderr.contains("rwv.toml"),
        "refusal must name the genuine user dirt; got:\n{stderr}"
    );
    assert!(
        !stderr.contains(REPO_PATH),
        "refusal must NOT name the drifted repo ({REPO_PATH}) — its differences are \
         rwv-attributable and self-heal during replay; got:\n{stderr}"
    );
    assert!(
        !has_op_state(&f.ww.root),
        "refusal must leave no .rwv-op file behind"
    );
}
