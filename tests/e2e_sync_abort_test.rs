//! E2E integration tests for `rwv sync`, `rwv abort`, `rwv check --locked`, and `rwv status`.
//!
//! These are the acceptance criteria for fo-wws-sync (rwv sync) and fo-wws-abort (rwv abort).
//! They are expected to FAIL until those implementations land.
//!
//! Scenarios follow the rewritten tutorial in docs/tutorial.md.

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

/// Init a git repo with one commit. Returns HEAD SHA.
fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

/// Write a file, stage, commit. Returns new HEAD SHA.
fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
}

/// Write an rwv.yaml manifest into `project_dir`.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: primary\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();
}

/// Write an rwv.lock file into `project_dir`.
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
// Fixture
// ---------------------------------------------------------------------------

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

const SERVER_URL: &str = "https://github.com/chatly/server.git";
const SERVER_PATH: &str = "github/chatly/server";

/// Build a workspace:
///   root/
///     github/chatly/server/   (git repo, initial commit)
///     projects/web-app/       (git repo, rwv.yaml + rwv.lock committed)
///
/// Both workspaces share no objects — independent repos. Good for precondition
/// tests where the error fires before any cross-workspace object access.
fn make_locked_workspace(parent: &Path, name: &str) -> (Workspace, String) {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server_dir = root.join(SERVER_PATH);
    let sha = init_repo(&server_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    write_manifest(&project_dir, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &sha)]);
    git(&["add", "rwv.yaml", "rwv.lock"], &project_dir);
    git(&["commit", "-m", "lock: initial"], &project_dir);

    (
        Workspace {
            root,
            project_dir,
            server_dir,
        },
        sha,
    )
}

/// Build two workspaces whose server repos share objects via a git worktree.
///
/// Layout:
///   parent/primary/                          (primary workspace)
///     github/chatly/server/                  (git repo, initial commit C1)
///     projects/web-app/                      (git repo, lock@C1 committed)
///   parent/ww/                               (workweave workspace)
///     github/chatly/server/                  (git worktree of primary's server, on ww/main@C1)
///     projects/web-app/                      (git worktree of primary's project, on ww/project@lock@C1)
///
/// Returns (primary, workweave, shared_c1_sha).
fn make_shared_workspaces(parent: &Path) -> (Workspace, Workspace, String) {
    let (primary, c1) = make_locked_workspace(parent, "primary");

    let ww_root = parent.join("ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let ww_server = ww_root.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/main",
        ],
        &primary.server_dir,
    );

    let ww_project = ww_root.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
        &primary.project_dir,
    );
    // The worktree inherits primary's already-committed rwv.lock (same C1 SHA).
    // No additional commit needed.

    let ww = Workspace {
        root: ww_root,
        project_dir: ww_project,
        server_dir: ww_server,
    };
    (primary, ww, c1)
}

// ---------------------------------------------------------------------------
// Smoke tests — command recognition
// ---------------------------------------------------------------------------

#[test]
fn sync_subcommand_is_recognized() {
    let out = rwv().args(["sync", "--help"]).assert();
    let output = out.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unrecognized subcommand") && !stderr.contains("unexpected argument"),
        "`rwv sync --help` should be recognized; got stderr: {stderr}"
    );
}

#[test]
fn abort_subcommand_is_recognized() {
    let out = rwv().args(["abort", "--help"]).assert();
    let output = out.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unrecognized subcommand") && !stderr.contains("unexpected argument"),
        "`rwv abort --help` should be recognized; got stderr: {stderr}"
    );
}

#[test]
fn sync_requires_source_argument() {
    let tmp = tempfile::tempdir().unwrap();
    rwv()
        .arg("sync")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// ---------------------------------------------------------------------------
// rwv check --locked
// ---------------------------------------------------------------------------

#[test]
fn check_locked_passes_when_lock_matches_head() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _) = make_locked_workspace(tmp.path(), "primary");
    rwv()
        .args(["check", "--locked"])
        .current_dir(&ws.root)
        .assert()
        .success();
}

#[test]
fn check_locked_fails_when_repo_has_advanced_past_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _) = make_locked_workspace(tmp.path(), "primary");

    // Advance server past the locked SHA without updating rwv.lock.
    make_commit(&ws.server_dir, "extra.txt", "extra\n", "advance past lock");

    rwv()
        .args(["check", "--locked"])
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stdout(predicate::str::contains(SERVER_PATH));
}

// ---------------------------------------------------------------------------
// rwv status
// ---------------------------------------------------------------------------

#[test]
fn status_shows_per_repo_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _) = make_locked_workspace(tmp.path(), "primary");
    rwv()
        .arg("status")
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains(SERVER_PATH));
}

#[test]
fn status_json_flag_produces_machine_readable_output() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _) = make_locked_workspace(tmp.path(), "primary");
    rwv()
        .args(["status", "--json"])
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::starts_with("{").or(predicate::str::starts_with("[")));
}

// ---------------------------------------------------------------------------
// rwv sync — fast-forward path (shared object store via worktrees)
// ---------------------------------------------------------------------------

/// Tutorial scenario: workweave finishes work → `rwv lock` → from primary `rwv sync <ww>`.
#[test]
fn sync_ff_primary_advances_to_workweave_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Workweave: make commit C2, update lock.
    let c2 = make_commit(
        &ww.server_dir,
        "change.txt",
        "workweave change\n",
        "ww: add change",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww change"], &ww.project_dir);

    // From primary: sync from the workweave.
    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Primary's server `main` branch should now be at C2.
    let primary_head = git_out(&["rev-parse", "main"], &primary.server_dir);
    assert_eq!(
        primary_head, c2,
        "primary server/main should be at C2 after sync from workweave"
    );
}

/// Tutorial scenario: primary has advanced → from workweave `rwv sync primary` catches up.
#[test]
fn sync_ff_is_symmetric_workweave_catches_up_to_primary() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2, update lock.
    let c2 = make_commit(
        &primary.server_dir,
        "upstream.txt",
        "upstream change\n",
        "primary: upstream advance",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: upstream advance"],
        &primary.project_dir,
    );

    // From workweave: sync to primary.
    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Workweave's ww/main branch (inside the shared clone) should be at C2.
    let ww_head = git_out(&["rev-parse", "ww/main"], &primary.server_dir);
    assert_eq!(
        ww_head, c2,
        "ww/main should be at C2 after syncing workweave from primary"
    );
}

// ---------------------------------------------------------------------------
// rwv sync — precondition enforcement
// ---------------------------------------------------------------------------

/// sync refuses when the source workspace's lock is stale (source HEAD ≠ source lock).
#[test]
fn sync_refuses_when_source_lock_is_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    // Advance source repo past its lock without updating the lock.
    make_commit(
        &source.server_dir,
        "extra.txt",
        "extra\n",
        "source: advance past lock",
    );

    rwv()
        .args(["sync", &source.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("lock").or(predicate::str::contains("stale")));
}

/// sync refuses when the CWD workspace's lock is stale (CWD HEAD ≠ CWD lock).
#[test]
fn sync_refuses_when_cwd_lock_is_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    // Advance primary past its lock without updating the lock.
    make_commit(
        &primary.server_dir,
        "extra.txt",
        "extra\n",
        "primary: advance past lock",
    );

    rwv()
        .args(["sync", &source.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("lock").or(predicate::str::contains("stale")));
}

/// --force bypasses the lock-freshness precondition; the specific "stale lock" error
/// must not appear even when CWD lock is stale.
#[test]
fn sync_force_bypasses_lock_freshness_precondition() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    // Advance primary past its lock — this would normally trigger a precondition refusal.
    make_commit(
        &primary.server_dir,
        "extra.txt",
        "extra\n",
        "advance past lock",
    );

    let out = rwv()
        .args(["sync", &source.root.to_string_lossy(), "--force"])
        .current_dir(&primary.root)
        .assert();

    // With --force the lock-staleness precondition is bypassed.
    // The op may fail for other reasons (diverged repos, missing objects) but NOT
    // with the lock-freshness message.
    let output = out.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let is_lock_freshness_error =
        (stderr.contains("lock") || stderr.contains("stale")) && stderr.contains("precondition");
    assert!(
        !is_lock_freshness_error,
        "--force should bypass the lock-freshness precondition; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// rwv sync — non-ff strategies
// ---------------------------------------------------------------------------

/// When CWD has local commits on top of an older base, --strategy rebase replays them.
#[test]
fn sync_rebase_replays_local_commits_on_source_tip() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2, update lock.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: advance",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: primary advance"],
        &primary.project_dir,
    );

    // Workweave: add a commit C_ww on top of C1 (before primary's C2).
    let c_ww = make_commit(
        &ww.server_dir,
        "ww_feature.txt",
        "ww feature\n",
        "ww: local commit",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww feature"], &ww.project_dir);

    // ww/main (C_ww) and primary main (C2) have both diverged from C1.
    // --strategy rebase should replay C_ww onto C2.
    rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "rebase",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();

    // After rebase, ww/main should be a descendant of C2.
    let ww_head = git_out(&["rev-parse", "ww/main"], &primary.server_dir);
    let is_descendant = Command::new("git")
        .args(["merge-base", "--is-ancestor", &c2, &ww_head])
        .current_dir(&primary.server_dir)
        .status()
        .unwrap()
        .success();
    assert!(
        is_descendant,
        "after rebase, ww/main ({ww_head}) should be a descendant of primary C2 ({c2})"
    );
}

/// When both sides have diverged, --strategy merge creates a merge commit.
#[test]
fn sync_merge_creates_merge_commit_from_diverged_sides() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2 on a different file.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: advance",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: primary advance"],
        &primary.project_dir,
    );

    // Workweave: advance to C_ww on a different file (no conflict).
    let c_ww = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: advance");
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww advance"], &ww.project_dir);

    // --strategy merge should create a merge commit on ww/main.
    rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "merge",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();

    // The merge commit should have both C2 and C_ww as parents.
    let ww_head = git_out(&["rev-parse", "ww/main"], &primary.server_dir);
    let parents = git_out(&["log", "--pretty=%P", "-1", &ww_head], &primary.server_dir);
    assert!(
        parents.contains(&c2) || parents.contains(&c_ww),
        "merge commit parents should include both sides; got: {parents}"
    );
}

// ---------------------------------------------------------------------------
// rwv abort
// ---------------------------------------------------------------------------

/// abort fails with a clear message when no sync operation is in progress.
#[test]
fn abort_fails_gracefully_when_no_op_in_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _) = make_locked_workspace(tmp.path(), "primary");
    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("no operation in progress")
                .or(predicate::str::contains("nothing to abort")),
        );
}

/// After a conflicted rebase, abort restores every repo to its pre-sync state.
#[test]
fn abort_restores_repos_to_pre_op_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Both sides make conflicting changes to the same file.
    let c_primary = make_commit(
        &primary.server_dir,
        "shared.txt",
        "primary version\n",
        "primary: conflict candidate",
    );
    write_lock(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL, &c_primary)],
    );
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: primary"], &primary.project_dir);

    let c_ww = make_commit(
        &ww.server_dir,
        "shared.txt",
        "ww version\n",
        "ww: conflict candidate",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww"], &ww.project_dir);

    // Record ww's server tip before the attempted sync.
    let pre_op_sha = git_out(&["rev-parse", "ww/main"], &primary.server_dir);
    assert_eq!(pre_op_sha, c_ww);

    // Attempt rebase sync — should hit a conflict and leave repos mid-op.
    let _ = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "rebase",
        ])
        .current_dir(&ww.root)
        .assert();

    // Abort should restore ww/main back to pre-op state.
    rwv().arg("abort").current_dir(&ww.root).assert().success();

    let post_abort_sha = git_out(&["rev-parse", "ww/main"], &primary.server_dir);
    assert_eq!(
        post_abort_sha, pre_op_sha,
        "abort should restore ww/main to pre-op SHA {pre_op_sha}; got {post_abort_sha}"
    );
}

// ---------------------------------------------------------------------------
// Round-trip convergence
// ---------------------------------------------------------------------------

/// sync A→B then B→A should be a no-op on B (project repo must not grow unbounded).
#[test]
fn sync_roundtrip_converges_without_project_repo_growth() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Advance primary to C2, update lock.
    let c2 = make_commit(
        &primary.server_dir,
        "advance.txt",
        "advance\n",
        "primary: advance",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: advance"], &primary.project_dir);

    let count_before: usize = git_out(&["rev-list", "--count", "HEAD"], &primary.project_dir)
        .parse()
        .unwrap();

    // Sync primary → workweave (workweave catches up to C2).
    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Sync workweave → primary (now a no-op: ww is at C2, primary is already at C2).
    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Primary's project repo commit count must not have grown.
    let count_after: usize = git_out(&["rev-list", "--count", "HEAD"], &primary.project_dir)
        .parse()
        .unwrap();
    assert_eq!(
        count_after, count_before,
        "no-op round-trip sync must not add commits to the project repo (auto-relock idempotence)"
    );
}
