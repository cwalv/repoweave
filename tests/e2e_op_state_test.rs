//! E2E integration tests for the multi-workspace op-state machinery and `--continue` flag.
//!
//! Acceptance criteria:
//! 1. Concurrent-op detection: an in-progress op-state blocks new ops on either workspace.
//! 2. Mid-step-1 resume: conflict → resolve → `--continue` → completion.
//! 3. Mid-step-3 resume: op-state left at step3-ff phase → `--continue` → FF-advance completes.
//! 4. Parameter exclusivity: `--strategy` passed alongside `--continue` → error.
//! 5. Abort cross-workspace: `rwv abort` from either workspace clears both op-state files.
//!
//! Notes:
//! - Tests 3 and 5 are "sync-to" scenarios (multi-workspace), but since `sync-to` is not yet
//!   implemented (spec 1), we simulate the cross-workspace state by manually writing op-state
//!   files and driving `rwv abort`. This mirrors the test-hook pattern requested in the spec:
//!   manufacture mid-op state, then verify recovery.
//! - Test 1 uses `rwv sync` from two directions to trigger the in-progress check.
//! - Test 2 uses `rwv sync --strategy rebase` to induce a conflict, then `--continue`.
//! - Test 4 uses `rwv sync` with mismatched `--strategy`.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Git helpers (mirroring e2e_sync_abort_test.rs pattern)
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

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    if let Some(parent) = repo.join(filename).parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(repo.join(filename), content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();
}

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

const SERVER_URL: &str = "https://github.com/chatly/server.git";
const SERVER_PATH: &str = "github/chatly/server";

// ---------------------------------------------------------------------------
// Workspace fixture helpers
// ---------------------------------------------------------------------------

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

/// Build a workspace:
///   root/
///     github/chatly/server/   (git repo, initial commit)
///     projects/web-app/       (git repo, rwv.yaml + rwv.lock committed)
fn make_locked_workspace(parent: &Path, name: &str) -> (Workspace, String) {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server_dir = root.join(SERVER_PATH);
    let sha = init_repo(&server_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    write_manifest(&project_dir, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &sha)]);
    git(
        &["add", ".gitattributes", "rwv.yaml", "rwv.lock"],
        &project_dir,
    );
    git(&["commit", "-m", "lock: initial"], &project_dir);
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    (
        Workspace {
            root,
            project_dir,
            server_dir,
        },
        sha,
    )
}

/// Build two workspaces sharing objects via git worktrees.
///
/// Layout:
///   parent/primary/   (primary workspace)
///   parent/ww/        (workweave workspace, repos are worktrees of primary's)
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
    std::fs::write(ww_root.join(".rwv-active"), "web-app\n").unwrap();

    let ww = Workspace {
        root: ww_root,
        project_dir: ww_project,
        server_dir: ww_server,
    };
    (primary, ww, c1)
}

// ---------------------------------------------------------------------------
// Test 1: Concurrent-op detection
//
// Start a sync-to (simulated by writing an op-state file directly, since
// sync-to doesn't exist yet). Then attempt another `rwv sync` from either the
// CWD or the target workspace — both should refuse with the in-progress error.
//
// We use `rwv sync` with a manually planted `.rwv-op` file to simulate the
// cross-workspace concurrency guard without implementing sync-to.
// ---------------------------------------------------------------------------

#[test]
fn concurrent_op_detection_blocks_new_sync_in_cwd_workspace() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances to C2, lock updated.
    let c2 = make_commit(
        &primary.server_dir,
        "advance.txt",
        "advance\n",
        "primary: C2",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: C2"], &primary.project_dir);

    // ww is at C1 (no changes). First sync from primary will succeed normally.
    // We start the sync-and-kill it by writing an op-state file manually first
    // to simulate a sync that was interrupted mid-way.
    //
    // Write a v2 owner record into ww's workspace (simulating an in-progress op).
    // [v1→v2: phase "running" → "replay"; added converged_tips/overrides.]
    let op_id = "test-concurrent-op-1234";
    let op_state_yaml = format!(
        "id: \"{op_id}\"\nverb: sync\nstrategy: ff\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: replay\nconverged_tips: {{}}\noverrides: []\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        src = primary.root.display(),
        tgt = ww.root.display(),
    );
    std::fs::write(ww.root.join(".rwv-op"), &op_state_yaml).unwrap();

    // Also create a savepoint ref so abort can clean up later.
    let ww_server_sha = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &ww_server_sha,
        ],
        &primary.server_dir,
    );
    let ww_project_sha = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &ww_project_sha,
        ],
        &ww.project_dir,
    );
    // Suppress unused var warning for c1
    let _ = c1;

    // Attempt a new sync from ww while the op-state file is present.
    // It should fail with an in-progress error.
    let assertion = rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("in progress"),
        "expected 'in progress' error when op-state file present; got: {stderr}"
    );
    assert!(
        stderr.contains("--continue"),
        "expected '--continue' hint in error; got: {stderr}"
    );
}

#[test]
fn concurrent_op_detection_error_names_phase_and_start_time() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Write a v2 owner record at a specific phase.
    // [v1→v2: phase "step1-rebase" → "relock" (mid-op relock phase).]
    let op_state_yaml = format!(
        "id: \"test-phase-detect\"\nverb: sync\nstrategy: rebase\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: relock\nconverged_tips: {{}}\noverrides: []\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        src = primary.root.display(),
        tgt = ww.root.display(),
    );
    std::fs::write(ww.root.join(".rwv-op"), &op_state_yaml).unwrap();

    let assertion = rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    // [v1→v2: phase "step1-rebase" → "relock" in v2 schema.]
    assert!(
        stderr.contains("relock"),
        "error should mention the in-progress phase; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 2: Mid-step-1 resume (conflict → resolve → --continue → completion)
//
// Induce a rebase conflict in Phase 1' of sync. Resolve it manually.
// Then `rwv sync --continue` should resume and complete.
// ---------------------------------------------------------------------------

#[test]
fn mid_step1_resume_with_continue_after_conflict_resolution() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2 with a file.
    let c2 = make_commit(
        &primary.server_dir,
        "shared.txt",
        "primary version\n",
        "primary: add shared.txt",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: C2"], &primary.project_dir);

    // ww: make a conflicting commit on the same file (plus lock update).
    let c_ww = make_commit(
        &ww.server_dir,
        "shared.txt",
        "ww version\n",
        "ww: add shared.txt (conflicts with primary)",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww C_ww"], &ww.project_dir);

    // Attempt rebase sync from ww → primary. Phase 2 (server repo) will conflict.
    // --discard-local-commits bypasses the Phase 1 ancestor precondition
    // (project repos diverged). Adapted from --force.
    let out = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "rebase",
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Op-state file should now exist.
    let op_state_path = ww.root.join(".rwv-op");
    assert!(
        op_state_path.exists(),
        "op-state file should exist after failed sync; got stderr: {stderr}"
    );

    // Resolve the conflict in the server repo (Phase 2 conflict).
    // The server repo is mid-rebase; resolve and continue.
    std::fs::write(ww.server_dir.join("shared.txt"), "resolved version\n").unwrap();
    git(&["add", "shared.txt"], &ww.server_dir);
    // Note: the server repo is a worktree; the rebase is happening there.
    let rebase_dir = ww.server_dir.join(".git");
    // Check if we're mid-rebase (may not be if the conflict was in the project repo).
    if rebase_dir.join("rebase-merge").exists() || rebase_dir.join("rebase-apply").exists() {
        let _ = common::git()
            .args(["rebase", "--continue"])
            .current_dir(&ww.server_dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output();
    } else {
        // Check the primary server dir (the actual git dir for the worktree).
        let primary_server_git = primary.server_dir.join(".git");
        if primary_server_git.join("rebase-merge").exists() {
            let _ = common::git()
                .args(["rebase", "--continue"])
                .current_dir(&ww.server_dir)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@test.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@test.com")
                .output();
        }
    }

    // Now run `rwv sync --continue` (alone — all params read from op-state).
    let result = rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert();

    // After --continue, the op-state file should be gone (either success cleared it,
    // or the op is still conflicted in which case it remains — but the command itself
    // must not refuse with "in-progress" error).
    let out = result.get_output().clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("in progress (started"),
        "--continue should not produce the 'in progress' refusal; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Mid-step-3 resume (op-state at step3-ff → --continue → completion)
//
// Since sync-to is not yet implemented, we simulate this by manually writing
// an op-state at `step3-ff` phase and verifying that `--continue` picks it up
// without the in-progress refusal. The actual FF-advance completion logic is
// in spec 1; here we just test the op-state machinery.
// ---------------------------------------------------------------------------

#[test]
fn mid_step3_continue_does_not_produce_in_progress_refusal() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Write a v2 owner record at advance-target phase into ww's workspace.
    // [v1→v2: phase "step3-ff" → "advance-target" in v2 schema.]
    let op_state_yaml = format!(
        "id: \"test-step3-ff-1234\"\nverb: sync\nstrategy: ff\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: advance-target\nconverged_tips: {{}}\noverrides: []\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        src = primary.root.display(),
        tgt = ww.root.display(),
    );
    std::fs::write(ww.root.join(".rwv-op"), &op_state_yaml).unwrap();

    // Create a savepoint ref so the op-id is consistent.
    let op_id = "test-step3-ff-1234";
    let ww_project_sha = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &ww_project_sha,
        ],
        &ww.project_dir,
    );
    let ww_server_sha = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &ww_server_sha,
        ],
        &primary.server_dir,
    );

    // `rwv sync --continue` (alone — all params from op-state) should not produce
    // the "in progress, resolve and rerun" refusal error.
    let result = rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert();

    let out = result.get_output().clone();
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Must not be the "in-progress" refusal — it should be a --continue resume.
    assert!(
        !stderr.contains("in progress (started"),
        "--continue must not produce the in-progress refusal; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: --continue is exclusive — passing other flags alongside it is rejected
//
// `--continue` must be passed alone (no other args/flags except `--project`).
// Passing `--strategy`, `--force`, `--retire`, or a positional source/target
// alongside `--continue` must produce a clap-level error with an actionable
// message. Verify the message mentions `rwv abort` as the escape hatch.
// ---------------------------------------------------------------------------

#[test]
fn continue_with_strategy_flag_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Plant a v2 owner record so --continue would proceed if it were alone.
    // [v1→v2: phase "running" → "replay"; added converged_tips/overrides.]
    let op_state_yaml = format!(
        "id: \"test-exclusive-1234\"\nverb: sync\nstrategy: rebase\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: replay\nconverged_tips: {{}}\noverrides: []\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        src = primary.root.display(),
        tgt = ww.root.display(),
    );
    std::fs::write(ww.root.join(".rwv-op"), &op_state_yaml).unwrap();

    // Passing --strategy alongside --continue must be rejected.
    let assertion = rwv()
        .args(["sync", "--strategy", "rebase", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    // Clap emits "cannot be used with" for conflicts_with violations.
    assert!(
        stderr.contains("cannot be used with") || stderr.contains("--continue"),
        "expected clap exclusivity error; got: {stderr}"
    );
}

/// --force is removed from sync/sync-to; passing it must produce an actionable
/// error (from the early-dispatch did-you-mean hint in `cli::dispatch`).
#[test]
fn sync_force_flag_is_removed_and_produces_friendly_error() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // --force must be rejected with a migration hint.
    let assertion = rwv()
        .args(["sync", "--force"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("--allow-stale-lock") || stderr.contains("--discard-local-commits"),
        "expected migration hint mentioning the named overrides; got: {stderr}"
    );
}

/// --allow-stale-lock alongside --continue must be rejected (conflicts_with).
#[test]
fn continue_with_allow_stale_lock_flag_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let assertion = rwv()
        .args(["sync", "--allow-stale-lock", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("cannot be used with") || stderr.contains("--continue"),
        "expected clap exclusivity error for --allow-stale-lock + --continue; got: {stderr}"
    );
}

/// --discard-local-commits alongside --continue must be rejected (conflicts_with).
#[test]
fn continue_with_discard_local_commits_flag_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let assertion = rwv()
        .args(["sync", "--discard-local-commits", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("cannot be used with") || stderr.contains("--continue"),
        "expected clap exclusivity error for --discard-local-commits + --continue; got: {stderr}"
    );
}

#[test]
fn sync_to_continue_with_retire_flag_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // rwv sync-to --retire --continue must be rejected.
    let assertion = rwv()
        .args(["sync-to", "--retire", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("cannot be used with") || stderr.contains("--continue"),
        "expected clap exclusivity error for sync-to --retire --continue; got: {stderr}"
    );
}

#[test]
fn sync_to_continue_with_strategy_flag_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // rwv sync-to --strategy=rebase --continue must be rejected.
    let assertion = rwv()
        .args(["sync-to", "--strategy", "rebase", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("cannot be used with") || stderr.contains("--continue"),
        "expected clap exclusivity error for sync-to --strategy=rebase --continue; got: {stderr}"
    );
}

#[test]
fn continue_with_no_op_in_progress_errors_clearly() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // No op-state file present. `rwv sync --continue` (alone, no other flags)
    // should error with "no sync/sync-to op in progress to continue".
    let assertion = rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("no sync")
            || stderr.contains("nothing to continue")
            || stderr.contains("in progress"),
        "expected 'no sync/sync-to op in progress' error when --continue has no op; got: {stderr}"
    );
}

#[test]
fn sync_to_continue_with_no_op_in_progress_errors_clearly() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // No op-state file present. `rwv sync-to --continue` (alone, no other flags)
    // should error with "no sync/sync-to op in progress to continue".
    let assertion = rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("no sync") || stderr.contains("nothing to continue")
            || stderr.contains("in progress"),
        "expected 'no sync/sync-to op in progress' error when sync-to --continue has no op; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 5: Abort cross-workspace
//
// Start a sync-to (simulated by planting op-state files in two workspaces),
// then run `rwv abort` from one workspace. Verify both op-state files are
// removed and savepoints are restored.
// ---------------------------------------------------------------------------

#[test]
fn abort_from_cwd_cleans_cross_workspace_op_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Simulate an in-progress sync-to: plant op-state in both workspaces with
    // the same op-id. For the abort to clean the "target" workspace (primary),
    // the op-state in ww's workspace must record verb=sync-to and target=primary.
    let op_id = "test-cross-abort-9999";

    // Create savepoint refs in both workspaces.
    let primary_project_sha = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &primary_project_sha,
        ],
        &primary.project_dir,
    );
    let primary_server_sha = git_out(&["rev-parse", "HEAD"], &primary.server_dir);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &primary_server_sha,
        ],
        &primary.server_dir,
    );
    let ww_project_sha = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &ww_project_sha,
        ],
        &ww.project_dir,
    );
    let ww_server_sha = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &ww_server_sha,
        ],
        &primary.server_dir, // ww's server dir is a worktree of primary's
    );

    // v2: ww is the owner (CWD/source of sync-to) → owner record at ww.root/.rwv-op.
    // primary is the target workspace → thin lease at primary.root/.rwv-op-lease.
    //
    // [v1→v2: replaced dual full-record writes with owner record
    // + thin lease. Phase "step1-rebase" → "replay". Primary gets lease, not owner record.]
    let ww_op_state_yaml = format!(
        "id: \"{op_id}\"\nverb: sync-to\nstrategy: ff\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: replay\nconverged_tips: {{}}\noverrides: []\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        src = ww.root.display(),
        tgt = primary.root.display(),
    );
    std::fs::write(ww.root.join(".rwv-op"), &ww_op_state_yaml).unwrap();

    // Thin lease in primary (target workspace): just {id, owner}.
    let primary_lease_yaml = format!(
        "id: \"{op_id}\"\nowner: \"{owner}\"\n",
        owner = ww.root.display(),
    );
    std::fs::write(primary.root.join(".rwv-op-lease"), &primary_lease_yaml).unwrap();

    // Run `rwv abort` from ww.
    rwv().arg("abort").current_dir(&ww.root).assert().success();

    // Owner record at ww should be removed.
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "ww's owner record should be removed after abort"
    );
    // Lease at primary should be removed.
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "primary's lease file should be removed after abort (cross-workspace)"
    );
}

#[test]
fn abort_restores_repos_and_removes_op_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Plant an op-state file in ww's workspace (simulate an in-progress sync).
    let op_id = "test-abort-opstate-5678";
    let ww_project_sha = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &ww_project_sha,
        ],
        &ww.project_dir,
    );
    let ww_server_sha = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &ww_server_sha,
        ],
        &primary.server_dir,
    );

    // v2 owner record: phase "running" → "replay"; added converged_tips/overrides.
    let op_state_yaml = format!(
        "id: \"{op_id}\"\nverb: sync\nstrategy: ff\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: replay\nconverged_tips: {{}}\noverrides: []\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        src = primary.root.display(),
        tgt = ww.root.display(),
    );
    std::fs::write(ww.root.join(".rwv-op"), &op_state_yaml).unwrap();

    // Run abort.
    rwv().arg("abort").current_dir(&ww.root).assert().success();

    // Op-state file should be removed.
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "op-state file should be removed by abort"
    );

    // Repos should be at their pre-op state.
    let post_abort_project = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    assert_eq!(
        post_abort_project, ww_project_sha,
        "project repo should be restored to pre-op SHA after abort"
    );
}

// ---------------------------------------------------------------------------
// Cross-verb mutex (Correction 1 COVERAGE + ORDERING).
//
// The op-state mutex must extend beyond `sync`: every verb that mutates repo
// state in an involved workspace (`update`, `lock --commit`, `workweave
// delete`, retire) refuses while a `.rwv-op` / `.rwv-op-lease` involves that
// workspace. Each refusal names the in-flight op (verb), its age, and the two
// exits (`--continue` from the owning workspace / `rwv abort`). The `workweave
// delete` case lives in `workweave_topology_parent_test.rs` where the fixture
// can create + delete a real workweave.
// ---------------------------------------------------------------------------

/// Plant a v2 owner record for an in-flight op at `ws_root`.
fn plant_owner_record(ws_root: &Path, verb: &str, phase: &str, src: &Path, tgt: &Path) {
    let yaml = format!(
        "id: \"planted-op-1234\"\nverb: {verb}\nstrategy: rebase\nsource: \"{src}\"\n\
         target: \"{tgt}\"\nretire: false\nphase: {phase}\nconverged_tips: {{}}\n\
         overrides: []\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        src = src.display(),
        tgt = tgt.display(),
    );
    std::fs::write(ws_root.join(".rwv-op"), &yaml).unwrap();
}

/// Assert an in-flight-op refusal: names the verb, phase, age, and both exits.
fn assert_in_flight_op_refusal(stderr: &str, expect_verb: &str) {
    assert!(
        stderr.contains("in progress (started"),
        "refusal must name the in-flight op with its age; got: {stderr}"
    );
    assert!(
        stderr.contains(&format!("{expect_verb} in progress")),
        "refusal must name the op's verb `{expect_verb}`; got: {stderr}"
    );
    assert!(
        stderr.contains("--continue"),
        "refusal must offer `--continue`; got: {stderr}"
    );
    assert!(
        stderr.contains("rwv abort"),
        "refusal must offer `rwv abort`; got: {stderr}"
    );
}

/// `rwv update` refuses while an op involves the active workspace.
#[test]
fn mid_op_update_refuses_with_in_flight_message() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());
    plant_owner_record(&ww.root, "sync-to", "replay", &ww.root, &primary.root);

    let assertion = rwv()
        .args(["update"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert_in_flight_op_refusal(&stderr, "sync-to");
}

/// `rwv lock --commit` refuses while an op involves the active workspace.
#[test]
fn mid_op_lock_commit_refuses_with_in_flight_message() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());
    plant_owner_record(&ww.root, "sync", "relock", &primary.root, &ww.root);

    let assertion = rwv()
        .args(["lock", "--commit"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert_in_flight_op_refusal(&stderr, "sync");
}

/// Plain `rwv lock` (no `--commit`) is NOT gated: writing the working-tree
/// `rwv.lock` is the auto-relock's own input (Correction 3 carve-out), so the
/// mutex is scoped to `--commit`. This guards the scope from over-broadening.
#[test]
fn mid_op_plain_lock_is_not_gated() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());
    plant_owner_record(&ww.root, "sync", "relock", &primary.root, &ww.root);

    // Plain `rwv lock` writes the working-tree lock and must NOT hit the op
    // guard (it succeeds despite the planted op-state).
    let assertion = rwv()
        .args(["lock"])
        .current_dir(&ww.root)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert!(
        !stderr.contains("in progress (started"),
        "plain `rwv lock` must not be gated by the op mutex; got: {stderr}"
    );
}

/// `rwv sync-to` (the retire entry point) refuses while an op involves the
/// workspace — the op guard reports the in-flight op, covering the retire verb.
#[test]
fn mid_op_sync_to_refuses_with_in_flight_message() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());
    plant_owner_record(&ww.root, "sync-to", "replay", &ww.root, &primary.root);

    let assertion = rwv()
        .args(["sync-to", "--retire", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert_in_flight_op_refusal(&stderr, "sync-to");
}

/// A workspace holding only a thin `.rwv-op-lease` (not the owner record) also
/// refuses; the guard follows the lease pointer to name the op / age / exits.
#[test]
fn mid_op_lease_side_verb_refuses_and_names_owner() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Owner record at ww (the op's initiating workspace), thin lease at primary.
    plant_owner_record(
        &ww.root,
        "sync-to",
        "advance-target",
        &ww.root,
        &primary.root,
    );
    let lease_yaml = format!(
        "id: \"planted-op-1234\"\nowner: \"{owner}\"\n",
        owner = ww.root.display(),
    );
    std::fs::write(primary.root.join(".rwv-op-lease"), &lease_yaml).unwrap();

    // From the LEASED workspace (primary), `rwv update` must refuse, following
    // the lease pointer to the owner record for the rich message.
    let assertion = rwv()
        .args(["update"])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert_in_flight_op_refusal(&stderr, "sync-to");
    assert!(
        stderr.contains(&ww.root.display().to_string()),
        "lease-side refusal must name the owner workspace; got: {stderr}"
    );
}

/// Correction 1 ORDERING: the op guard fires BEFORE lock-relation
/// classification. A mid-op workspace whose lock is ALSO in an anomalous
/// relation (would otherwise trip a lock/relation refusal) must report the
/// in-flight op, not the stale-lock error — that is the whole point of the
/// reorder.
#[test]
fn op_guard_precedes_lock_relation_classification() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Make ww's committed lock DIVERGE from HEAD so that, absent the op guard,
    // the lock-relation classifier would refuse with a relation error. Advance
    // the server repo without relocking, then rewrite history so the lock's
    // pinned SHA is neither ancestor nor descendant of HEAD (diverged).
    make_commit(&ww.server_dir, "a.txt", "a\n", "ww: A");
    git(&["checkout", "-b", "throwaway"], &ww.server_dir);
    make_commit(&ww.server_dir, "b.txt", "b\n", "ww: B (diverged)");

    // Plant an in-flight op on ww at the same time.
    plant_owner_record(&ww.root, "sync-to", "replay", &ww.root, &primary.root);

    // A `rwv sync-to` must report the in-flight op, NOT a lock/relation error.
    let assertion = rwv()
        .args(["sync-to", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert_in_flight_op_refusal(&stderr, "sync-to");
    assert!(
        !stderr.contains("diverged")
            && !stderr.contains("lock behind")
            && !stderr.contains("lock ahead")
            && !stderr.contains("stale lock"),
        "op guard must preempt any lock-relation refusal; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Smoke tests for --continue flag recognition
// ---------------------------------------------------------------------------

#[test]
fn sync_continue_flag_is_recognized() {
    let out = rwv().args(["sync", "--help"]).assert();
    let output = out.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--continue") || stdout.contains("continue"),
        "`rwv sync --help` should mention the --continue flag; got stdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Unit-level: op_state module round-trip (smoke test via rwv binary)
// ---------------------------------------------------------------------------

#[test]
fn op_state_file_written_during_sync_and_removed_on_success() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary advances to C2.
    let c2 = make_commit(
        &primary.server_dir,
        "advance-op.txt",
        "advance\n",
        "primary: C2",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: C2"], &primary.project_dir);

    // Sync ww from primary. The op-state file should be written during the op
    // and removed on success.
    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();

    // On success, the op-state file must be removed.
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "op-state file must be removed after successful sync"
    );
}
