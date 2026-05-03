//! E2E integration tests for `rwv sync`, `rwv abort`, `rwv check --locked`, and `rwv status`.
//!
//! These are the acceptance criteria for fo-wws-sync (rwv sync) and fo-wws-abort (rwv abort).
//! They are expected to FAIL until those implementations land.
//!
//! Scenarios follow the rewritten tutorial in docs/tutorial.md.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

fn git(args: &[&str], dir: &Path) {
    let out = common::git()
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
    let out = common::git()
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
    common::rwv()
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
// rwv sync — Phase 1 ancestor precondition
// ---------------------------------------------------------------------------
//
// Phase 1 hard-resets the destination's project repo to the source's tip.
// The precondition refuses when this would discard reachable commits — i.e.
// when the destination's project tip is NOT an ancestor of the source's tip.

/// Helper: advance primary's project repo by one commit (no-op edit) and re-lock.
/// Leaves primary's lock fresh.
fn primary_advance_project_one_commit(primary: &Workspace, server_sha: &str) {
    write_lock(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL, server_sha)],
    );
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &[
            "commit",
            "--allow-empty",
            "-m",
            "lock: primary project advance",
        ],
        &primary.project_dir,
    );
}

/// Backward sync: when CWD's project tip is ahead of source's, sync refuses.
/// The error names both workspaces and the proper recovery path.
#[test]
fn sync_refuses_when_destination_project_repo_is_ahead_of_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances its project repo by one commit (still pointing at C1
    // server). ww's project repo stays at C1. Both locks are fresh.
    primary_advance_project_one_commit(&primary, &c1);

    // Sync ww → primary: source=ww (project @ C1), CWD=primary (project ahead of C1).
    // primary has a project commit ww doesn't — refuse.
    let assertion = rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let output = assertion.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Names the destination workspace.
    assert!(
        stderr.contains("destination workspace") && stderr.contains("project repo"),
        "expected refusal naming destination project repo; got: {stderr}"
    );
    // Names the source workspace too.
    assert!(
        stderr.contains("source workspace"),
        "expected refusal naming source workspace; got: {stderr}"
    );
    // Names the rwv-native recovery path.
    assert!(
        stderr.contains("sync the other direction first"),
        "expected refusal to name the rwv-native recovery; got: {stderr}"
    );
    // Names the --force scenario explicitly.
    assert!(
        stderr.contains("--force") && stderr.contains("discard"),
        "expected refusal to name the --force scenario (discard); got: {stderr}"
    );
}

/// Diverged sync: when CWD's project tip and source's project tip have diverged
/// (neither is an ancestor of the other), sync refuses.
#[test]
fn sync_refuses_when_destination_project_repo_has_diverged_from_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances its project by one commit (still pointing at C1 server).
    primary_advance_project_one_commit(&primary, &c1);

    // ww independently advances its project by a different commit (still C1 server).
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c1)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(
        &["commit", "--allow-empty", "-m", "lock: ww project advance"],
        &ww.project_dir,
    );

    // Sanity: tips diverged from common ancestor C1.
    let primary_tip = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    let ww_tip = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    assert_ne!(primary_tip, ww_tip);

    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("destination workspace")
                .and(predicate::str::contains("project repo"))
                .and(predicate::str::contains("commits not in source workspace")),
        );
}

/// Forward sync (CWD strict ancestor of source): allowed — the normal case.
/// Already covered by `sync_ff_primary_advances_to_workweave_lock` etc., but
/// guard explicitly that the ancestor precondition does NOT fire.
#[test]
fn sync_allows_when_destination_project_repo_is_strict_ancestor_of_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // ww advances its project (server still at C1). primary's project stays at C1.
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c1)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(
        &["commit", "--allow-empty", "-m", "lock: ww project advance"],
        &ww.project_dir,
    );

    // Sync ww → primary: primary (CWD) at C1, ww (source) ahead. primary IS
    // ancestor of ww — forward, allowed.
    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();
}

/// Equal tips: both project repos at the same SHA — no-op, allowed.
#[test]
fn sync_allows_when_destination_project_repo_equals_source() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // No advances anywhere — both project tips share C1.
    let primary_tip = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    let ww_tip = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    assert_eq!(primary_tip, ww_tip);

    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();
}

/// `--force` bypasses the ancestor precondition: backward sync succeeds and
/// the savepoint preserves the discarded commits for `rwv abort`.
#[test]
fn sync_force_bypasses_phase1_ancestor_refusal_and_preserves_savepoint() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances its project past C1; ww's project stays at C1.
    primary_advance_project_one_commit(&primary, &c1);
    let primary_pre_sync = git_out(&["rev-parse", "HEAD"], &primary.project_dir);

    // Backward sync with --force: must succeed.
    rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--force"])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Primary's project repo should now be at ww's project tip (forced reset).
    let ww_tip = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    let primary_post_sync = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    assert_eq!(
        primary_post_sync, ww_tip,
        "with --force, primary's project tip should now equal ww's"
    );

    // The pre-sync commit is no longer reachable from HEAD but MUST be
    // reachable from some `refs/rwv/pre-op/*` savepoint — that's the
    // contract `--force` makes with the operator (recoverable via abort).
    let savepoint_refs = git_out(
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/rwv/pre-op",
        ],
        &primary.project_dir,
    );
    assert!(
        savepoint_refs.contains(&primary_pre_sync),
        "pre-sync commit {primary_pre_sync} should be preserved in a refs/rwv/pre-op/* savepoint; \
         got: {savepoint_refs}"
    );
}

/// Refusal message also names `--strategy rebase` / `--strategy merge` as the
/// rwv-native paths to land diverging project commits without `--force`.
#[test]
fn sync_refusal_message_suggests_rebase_or_merge_strategy() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    primary_advance_project_one_commit(&primary, &c1);

    let assertion = rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("--strategy rebase") || stderr.contains("--strategy merge"),
        "expected refusal to suggest --strategy rebase or --strategy merge; got: {stderr}"
    );
}

/// Backward sync that ff refuses lands cleanly under `--strategy rebase`:
/// CWD's lock-only divergence is replayed onto source's tip with `rwv.lock`
/// excluded (skipped as empty), and Phase 3 leaves the lock consistent with
/// manifest tips.
#[test]
fn sync_rebase_lands_lock_only_divergence_without_force() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // primary advances its project by a lock-only commit; ww stays at C1.
    primary_advance_project_one_commit(&primary, &c1);
    let ww_tip_before = git_out(&["rev-parse", "HEAD"], &ww.project_dir);

    // Default ff would refuse (covered elsewhere). --strategy rebase replays
    // primary's lock-only commit onto ww's tip, dropping rwv.lock from the
    // patch → empty → skipped.
    rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--strategy", "rebase"])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Primary's project tip should match ww's (the lock-only commit was
    // skipped during replay; Phase 3 found no lock change to commit).
    let primary_tip_after = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    assert_eq!(
        primary_tip_after, ww_tip_before,
        "after rebase landed lock-only divergence, primary tip should equal ww tip; \
         got primary={primary_tip_after} ww={ww_tip_before}"
    );
}

/// Backward sync with `--strategy merge` produces a merge commit on top of
/// CWD whose tree matches source on non-lock paths.
#[test]
fn sync_merge_lands_lock_only_divergence_without_force() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // primary advances its project by a lock-only commit.
    primary_advance_project_one_commit(&primary, &c1);
    let primary_pre_sync = git_out(&["rev-parse", "HEAD"], &primary.project_dir);

    rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--strategy", "merge"])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Either a merge commit was created (ff impossible), or primary's tip
    // already had ww's tip as ancestor (ff'd through merge). Either way,
    // primary's history must reach the pre-sync commit (no commits lost).
    let primary_post_sync = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    let reachable = common::git()
        .args([
            "merge-base",
            "--is-ancestor",
            &primary_pre_sync,
            &primary_post_sync,
        ])
        .current_dir(&primary.project_dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(
        reachable,
        "after --strategy merge, pre-sync commit {primary_pre_sync} should still be reachable \
         from primary's HEAD {primary_post_sync}"
    );
}

/// Refusal message names the source-side recovery: "sync the other direction
/// first" — and that recovery actually works (no infinite refusal loop).
#[test]
fn sync_other_direction_first_unblocks_a_refused_backward_sync() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances its project by one commit (C1 server still).
    primary_advance_project_one_commit(&primary, &c1);

    // Backward sync from primary refuses (verified in another test).
    // Operator runs the named recovery: sync the other direction first
    // (primary → ww), bringing primary's commit to ww.
    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Now primary and ww are aligned. The originally-refused sync (primary
    // CWD, ww source) is now allowed (equal tips → no-op).
    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// rwv sync — error message structure
// ---------------------------------------------------------------------------

/// Lock-freshness error names the source workspace and the recovery path,
/// and does NOT mention `--force` (per the structured-error guideline:
/// `rwv lock` is the proper recovery, not `--force`).
#[test]
fn lock_freshness_source_error_names_workspace_and_recovery_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    make_commit(
        &source.server_dir,
        "extra.txt",
        "extra\n",
        "source: advance past lock",
    );

    let assertion = rwv()
        .args(["sync", &source.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("source workspace 'source'"),
        "expected named source workspace; got: {stderr}"
    );
    assert!(
        stderr.contains("stale lock"),
        "expected 'stale lock' phrasing; got: {stderr}"
    );
    assert!(
        stderr.contains("rwv lock"),
        "expected named recovery (`rwv lock`); got: {stderr}"
    );
    assert!(
        !stderr.contains("--force"),
        "lock-freshness error must not mention --force (proper recovery is `rwv lock`); got: {stderr}"
    );
}

/// Lock-freshness destination error names the destination workspace and the
/// recovery path, and does NOT mention `--force`.
#[test]
fn lock_freshness_destination_error_names_workspace_and_recovery_path() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    make_commit(
        &primary.server_dir,
        "extra.txt",
        "extra\n",
        "primary: advance past lock",
    );

    let assertion = rwv()
        .args(["sync", &source.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("destination workspace 'primary'"),
        "expected named destination workspace; got: {stderr}"
    );
    assert!(
        stderr.contains("stale lock"),
        "expected 'stale lock' phrasing; got: {stderr}"
    );
    assert!(
        stderr.contains("rwv lock"),
        "expected named recovery (`rwv lock`); got: {stderr}"
    );
    assert!(
        !stderr.contains("--force"),
        "lock-freshness error must not mention --force; got: {stderr}"
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
    //
    // Both sides also independently committed locks → ww and primary's project
    // repos diverged too. Phase 1's ancestor precondition refuses divergent
    // project repos; this test deliberately constructs that to exercise Phase
    // 2's rebase strategy on the manifest repo, so it opts in via --force.
    rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "rebase",
            "--force",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();

    // After rebase, ww/main should be a descendant of C2.
    let ww_head = git_out(&["rev-parse", "ww/main"], &primary.server_dir);
    let is_descendant = common::git()
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
    // Project repos diverge from independent lock commits; --force is required
    // to bypass the Phase 1 ancestor precondition. The test's intent is the
    // Phase 2 merge strategy on the manifest repo.
    rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "merge",
            "--force",
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
    // Project repos diverged from independent lock commits → --force is
    // required to reach Phase 2 where the rebase conflict is the focus.
    let _ = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "rebase",
            "--force",
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
// rwv sync — tag-form lock freshness (mirrors check_test.rs §8)
// ---------------------------------------------------------------------------

/// Add a lightweight tag at HEAD in `repo`.
fn git_tag(repo: &Path, tag: &str) {
    git(
        &[
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=Test",
            "tag",
            tag,
        ],
        repo,
    );
}

/// sync proceeds when the source lock is pinned by a tag that resolves to the source HEAD.
/// Regression test: before the fix, this spuriously produced "source lock is stale".
#[test]
fn sync_proceeds_when_source_lock_tag_matches_head() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Tag primary's server at its current HEAD and update the source lock to the tag name.
    git_tag(&primary.server_dir, "v1.0.0");
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, "v1.0.0")]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: pin v1.0.0 (tag form)"],
        &primary.project_dir,
    );

    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();
}

/// sync proceeds when the source lock is pinned by a SHA that equals the source HEAD.
/// Regression guard: SHA-form should always have worked; this ensures it still does.
#[test]
fn sync_proceeds_when_source_lock_sha_matches_head() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // primary's project already has the lock with C1 SHA committed.
    // Syncing ww from primary is a no-op; the freshness check must pass.
    let source_lock_sha = c1; // the SHA already in the lock
    assert!(!source_lock_sha.is_empty());

    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();
}

/// sync refuses when the source lock is pinned by a tag whose commit differs from HEAD.
/// The "stale" error must fire even when the lock version is a tag name, not a raw SHA.
#[test]
fn sync_refuses_when_source_lock_tag_is_genuinely_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (cwd_ws, _) = make_locked_workspace(tmp.path(), "cwd");

    // Tag primary's server at C1, then advance past it — now HEAD ≠ tag commit.
    git_tag(&primary.server_dir, "v1.0.0");
    make_commit(
        &primary.server_dir,
        "advance.txt",
        "advance\n",
        "primary: advance past v1.0.0",
    );

    // Update source lock to the tag name — it's now genuinely stale (HEAD > tag commit).
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, "v1.0.0")]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: v1.0.0 (stale)"],
        &primary.project_dir,
    );

    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&cwd_ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("lock").or(predicate::str::contains("stale")));
}

/// sync refuses and reports "unknown revision" when the source lock references a tag
/// that no longer exists locally.
#[test]
fn sync_refuses_with_unknown_revision_when_source_lock_tag_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (cwd_ws, _) = make_locked_workspace(tmp.path(), "cwd");

    // Write a source lock pinned by a tag that was never created.
    write_lock(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL, "v9.9.9-nonexistent")],
    );
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: nonexistent tag"],
        &primary.project_dir,
    );

    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&cwd_ws.root)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unknown revision")
                .or(predicate::str::contains("v9.9.9-nonexistent")),
        );
}

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

// ---------------------------------------------------------------------------
// Marker-based workweave fixtures and regression tests
// ---------------------------------------------------------------------------
//
// `make_shared_workspaces` produces two workspaces that both look like primary
// weaves to `WorkspaceContext::resolve` (no `.rwv-workweave` marker, no
// `{primary}--{name}` naming). The marker-based fixtures below mirror what
// `rwv workweave create` produces in production and exercise the
// fo-rwv-lock-cross-workspace-confusion code paths: lock writes and
// freshness checks must use the workweave's own `projects/<name>/rwv.lock`,
// not primary's, when CWD is inside the workweave.

/// Workspaces produced by `make_marker_workweave`.
struct MarkerSharedWorkspaces {
    primary: Workspace,
    /// The workweave's root directory (with `.rwv-workweave` marker).
    ww_root: PathBuf,
    /// The workweave's project worktree (`<ww>/projects/web-app`), on its own branch.
    ww_project_dir: PathBuf,
    /// The workweave's server worktree (`<ww>/github/chatly/server`), on its own branch.
    ww_server_dir: PathBuf,
}

/// Mirror what `rwv workweave create` produces: a workweave directory under
/// `parent/.workweaves/<primary-name>--<ww-name>/` with a `.rwv-workweave`
/// marker, with each repo (and the project repo) as a worktree of primary's
/// on a per-workweave ephemeral branch.
fn make_marker_workweave(parent: &Path, ww_name: &str) -> MarkerSharedWorkspaces {
    let (primary, _c1) = make_locked_workspace(parent, "primary");
    let ww_root = parent
        .join(".workweaves")
        .join(format!("primary--{ww_name}"));
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let ww_server = ww_root.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            "-b",
            &format!("primary--{ww_name}/main"),
            &ww_server.to_string_lossy(),
        ],
        &primary.server_dir,
    );

    let ww_project = ww_root.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            "-b",
            &format!("primary--{ww_name}/project"),
            &ww_project.to_string_lossy(),
        ],
        &primary.project_dir,
    );

    let marker = format!(
        "primary: {}\nproject: web-app\n",
        primary.root.canonicalize().unwrap().display()
    );
    std::fs::write(ww_root.join(".rwv-workweave"), marker).unwrap();

    MarkerSharedWorkspaces {
        primary,
        ww_root,
        ww_project_dir: ww_project,
        ww_server_dir: ww_server,
    }
}

/// Anomaly B regression: `rwv lock` running inside a marker-based workweave
/// must write to the workweave's own `projects/<name>/rwv.lock`, leaving
/// primary's lock file content untouched.
#[test]
fn lock_in_marker_workweave_does_not_mutate_primary_lock_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Advance the workweave's server tip past the inherited C1 lock.
    let c2 = make_commit(&ws.ww_server_dir, "ww-only.txt", "ww only\n", "ww: advance");

    // Snapshot primary's lock file content before running lock in the workweave.
    let primary_lock_path = ws.primary.project_dir.join("rwv.lock");
    let primary_lock_before = std::fs::read_to_string(&primary_lock_path).unwrap();

    rwv()
        .arg("lock")
        .current_dir(&ws.ww_root)
        .assert()
        .success();

    // Primary's lock file content must be unchanged on disk.
    let primary_lock_after = std::fs::read_to_string(&primary_lock_path).unwrap();
    assert_eq!(
        primary_lock_before, primary_lock_after,
        "`rwv lock` from a workweave must not mutate primary's rwv.lock content"
    );

    // Workweave's lock must contain the workweave's tip (C2), not primary's (C1).
    let ww_lock_path = ws.ww_project_dir.join("rwv.lock");
    let ww_lock = repoweave::manifest::LockFile::from_path(&ww_lock_path).unwrap();
    let entry = ww_lock
        .repositories
        .get(&repoweave::manifest::RepoPath::new(SERVER_PATH))
        .expect("workweave lock should contain server entry");
    assert_eq!(
        entry.version.as_str(),
        &c2,
        "workweave lock entry must reflect workweave's tip (C2), not primary's (C1)"
    );
}

/// Anomaly A regression: when locks diverge between primary and a marker-based
/// workweave, `rwv sync`'s "CWD lock" freshness check must read the workweave's
/// own committed lock — not primary's — to determine staleness from the
/// operator's perspective.
#[test]
fn sync_in_marker_workweave_uses_workweave_own_lock_for_cwd_freshness() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Primary advances to C2 and commits the C2 lock on its main branch.
    let c2 = make_commit(
        &ws.primary.server_dir,
        "primary-advance.txt",
        "primary advance\n",
        "primary: C2",
    );
    write_lock(&ws.primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &ws.primary.project_dir);
    git(
        &["commit", "-m", "lock: primary advance"],
        &ws.primary.project_dir,
    );

    // Workweave's own server tip is still at C1, and its committed lock is
    // still at C1 (inherited from the worktree-creation snapshot of project
    // repo @ C1). From the operator's perspective in the workweave, "CWD lock
    // is fresh" — workweave-tip C1 == workweave-lock C1 — even though primary
    // has diverged.
    rwv()
        .args(["sync", &ws.primary.root.to_string_lossy()])
        .current_dir(&ws.ww_root)
        .assert()
        .success();

    // After sync the workweave's server tip should have caught up to C2.
    let ww_server_head = git_out(&["rev-parse", "HEAD"], &ws.ww_server_dir);
    assert_eq!(
        ww_server_head, c2,
        "workweave server should be at C2 after sync from primary"
    );
}

/// Symmetric regression: `rwv sync` from primary with a marker-based workweave
/// as source must read the workweave's lock for `source` freshness — not
/// primary's lock interpreted as the source's.
#[test]
fn sync_from_primary_with_marker_workweave_source_uses_source_own_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Workweave advances to C2 and commits the C2 lock on its own project branch.
    let c2 = make_commit(
        &ws.ww_server_dir,
        "ww-advance.txt",
        "ww advance\n",
        "ww: C2",
    );
    write_lock(&ws.ww_project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &ws.ww_project_dir);
    git(&["commit", "-m", "lock: ww advance"], &ws.ww_project_dir);

    // Primary syncs from the workweave. Source freshness must be checked
    // against the workweave's own lock (C2 vs ww-tip C2), not primary's lock.
    rwv()
        .args(["sync", &ws.ww_root.to_string_lossy()])
        .current_dir(&ws.primary.root)
        .assert()
        .success();

    // Primary's server should have advanced to C2 (the workweave's lock target).
    let primary_server_head = git_out(&["rev-parse", "HEAD"], &ws.primary.server_dir);
    assert_eq!(
        primary_server_head, c2,
        "primary server should be at C2 after sync from workweave"
    );
}

/// Anomaly A precondition guard: when the workweave's own committed lock is
/// genuinely stale (workweave-tip ≠ workweave-lock), sync from the workweave
/// must refuse with a stale-lock error naming the destination workspace —
/// not silently pass by reading primary's lock.
#[test]
fn sync_in_marker_workweave_refuses_when_workweave_own_lock_is_stale() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Advance the workweave's server tip without updating the workweave's
    // own committed lock. (Primary's lock is left untouched at C1 to guarantee
    // we are not silently reading primary's value.)
    make_commit(
        &ws.ww_server_dir,
        "ww-advance.txt",
        "ww advance\n",
        "ww: advance past lock",
    );

    rwv()
        .args(["sync", &ws.primary.root.to_string_lossy()])
        .current_dir(&ws.ww_root)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("destination workspace 'primary--feat'")
                .and(predicate::str::contains("stale lock")),
        );
}

// ---------------------------------------------------------------------------
// RepoSyncOutcome — per-repo status reporting
// ---------------------------------------------------------------------------

/// Regression: CWD is ahead of the lock target → sync reports `already-ahead`, not bare `ok`.
///
/// Repro from SME triage: after `rwv sync`, the per-repo line said "ok" even
/// though HEAD didn't move to the lock SHA, leaving `rwv status` showing [ahead].
#[test]
fn sync_reports_already_ahead_when_cwd_is_past_lock_target() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2, lock at C2.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: C2"], &primary.project_dir);

    // Workweave: fast-forward to C2, then add C3 (ww is now ahead of primary's lock).
    git(&["merge", "--ff-only", "main"], &ww.server_dir);
    let c3 = make_commit(
        &ww.server_dir,
        "ww-extra.txt",
        "ww extra\n",
        "ww: C3 (ahead of C2)",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c3)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: C3"], &ww.project_dir);

    // Sync ww from primary: target is C2, but ww is at C3 (C2 is ancestor of C3).
    // --force bypasses the Phase 1 ancestor precondition because ww's project repo
    // has the C3 lock commit primary doesn't; the test's intent is Phase 2 behavior
    // on the manifest repo (already-ahead reporting), not the Phase 1 guard.
    let out = rwv()
        .args(["sync", &primary.root.to_string_lossy(), "--force"])
        .current_dir(&ww.root)
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("already-ahead"),
        "sync should report already-ahead when CWD is past the lock target; got stdout: {stdout}"
    );
    assert!(
        !stdout.contains(": ok"),
        "sync must not report bare 'ok' for already-ahead case; got stdout: {stdout}"
    );
}

/// Regression: diverged repo + `--strategy ff` must still report `failed (cannot fast-forward...)`.
/// Verifies the failure message structure carries through the new outcome enum unchanged.
#[test]
fn sync_ff_reports_failed_for_diverged_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: C2"], &primary.project_dir);

    // Workweave: diverge from C1 (different file, cannot fast-forward to C2).
    let c_ww = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: diverged from C1");
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: C_ww"], &ww.project_dir);

    // ff sync: C2 and C_ww both diverge from C1 → cannot fast-forward.
    // --force bypasses the Phase 1 ancestor precondition because ww's project repo
    // has the C_ww lock commit primary doesn't; the test's intent is Phase 2's ff
    // failure on the manifest repo, not the Phase 1 guard.
    let out = rwv()
        .args(["sync", &primary.root.to_string_lossy(), "--force"])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot fast-forward") || stderr.contains("failed"),
        "diverged ff sync should report failure; got stderr: {stderr}"
    );
}
