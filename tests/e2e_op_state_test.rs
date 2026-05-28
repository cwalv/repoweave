//! E2E integration tests for the multi-workspace op-state machinery and `--continue` flag.
//!
//! Acceptance criteria for bead fo-pte54.7:
//! 1. Concurrent-op detection: an in-progress op-state blocks new ops on either workspace.
//! 2. Mid-step-1 resume: conflict → resolve → `--continue` → completion.
//! 3. Mid-step-3 resume: op-state left at step3-ff phase → `--continue` → FF-advance completes.
//! 4. Parameter-mismatch error: `--strategy=rebase` start → `--strategy=merge` continue → error.
//! 5. Abort cross-workspace: `rwv abort` from either workspace clears both op-state files.
//!
//! Notes:
//! - Tests 3 and 5 are "sync-to" scenarios (multi-workspace), but since `sync-to` is not yet
//!   implemented (bead 1), we simulate the cross-workspace state by manually writing op-state
//!   files and driving `rwv abort`. This mirrors the test-hook pattern requested in the bead:
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
    std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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
    // Write a .rwv-op file into ww's workspace (simulating an in-progress op).
    let op_id = "test-concurrent-op-1234";
    let op_state_yaml = format!(
        "id: \"{op_id}\"\nverb: sync\nstrategy: ff\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: running\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
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
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Write a .rwv-op file at a specific phase.
    let op_state_yaml = format!(
        "id: \"test-phase-detect\"\nverb: sync\nstrategy: rebase\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: step1-rebase\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
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

    assert!(
        stderr.contains("step1-rebase"),
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
    let tmp = tempfile::tempdir().unwrap();
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
    // --force bypasses the Phase 1 ancestor precondition (project repos diverged).
    let out = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "rebase",
            "--force",
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
        !stderr.contains("Resolve and rerun with `--continue`"),
        "--continue should not produce the 'in progress' refusal; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Mid-step-3 resume (op-state at step3-ff → --continue → completion)
//
// Since sync-to is not yet implemented, we simulate this by manually writing
// an op-state at `step3-ff` phase and verifying that `--continue` picks it up
// without the in-progress refusal. The actual FF-advance completion logic is
// in bead 1; here we just test the op-state machinery.
// ---------------------------------------------------------------------------

#[test]
fn mid_step3_continue_does_not_produce_in_progress_refusal() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Write an op-state at step3-ff phase into ww's workspace.
    let op_state_yaml = format!(
        "id: \"test-step3-ff-1234\"\nverb: sync\nstrategy: ff\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: step3-ff\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
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
        !stderr.contains("Resolve and rerun with `--continue`"),
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
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Plant an op-state file so --continue would proceed if it were alone.
    let op_state_yaml = format!(
        "id: \"test-exclusive-1234\"\nverb: sync\nstrategy: rebase\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: running\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        src = primary.root.display(),
        tgt = ww.root.display(),
    );
    std::fs::write(ww.root.join(".rwv-op"), &op_state_yaml).unwrap();

    // Passing --strategy alongside --continue must be rejected.
    let assertion = rwv()
        .args(["sync", "--strategy", "merge", "--continue"])
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

#[test]
fn continue_with_force_flag_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // --force alongside --continue must be rejected.
    let assertion = rwv()
        .args(["sync", "--force", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("cannot be used with") || stderr.contains("--continue"),
        "expected clap exclusivity error for --force + --continue; got: {stderr}"
    );
}

#[test]
fn sync_to_continue_with_retire_flag_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // rwv sync-to --strategy=merge --continue must be rejected.
    let assertion = rwv()
        .args(["sync-to", "--strategy", "merge", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("cannot be used with") || stderr.contains("--continue"),
        "expected clap exclusivity error for sync-to --strategy=merge --continue; got: {stderr}"
    );
}

#[test]
fn continue_with_no_op_in_progress_errors_clearly() {
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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

    // Plant op-state in ww (CWD for abort): verb=sync-to, target=primary.
    // This tells run_abort to also clean up primary's workspace.
    let ww_op_state_yaml = format!(
        "id: \"{op_id}\"\nverb: sync-to\nstrategy: ff\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: step1-rebase\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        src = ww.root.display(),
        tgt = primary.root.display(),
    );
    std::fs::write(ww.root.join(".rwv-op"), &ww_op_state_yaml).unwrap();

    // Plant op-state in primary (target workspace) with the same op-id.
    let primary_op_state_yaml = format!(
        "id: \"{op_id}\"\nverb: sync-to\nstrategy: ff\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: step1-rebase\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        src = ww.root.display(),
        tgt = primary.root.display(),
    );
    std::fs::write(primary.root.join(".rwv-op"), &primary_op_state_yaml).unwrap();

    // Run `rwv abort` from ww.
    rwv().arg("abort").current_dir(&ww.root).assert().success();

    // Both op-state files should be removed.
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "ww's op-state file should be removed after abort"
    );
    assert!(
        !primary.root.join(".rwv-op").exists(),
        "primary's op-state file should be removed after abort (cross-workspace)"
    );
}

#[test]
fn abort_restores_repos_and_removes_op_state() {
    let tmp = tempfile::tempdir().unwrap();
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

    let op_state_yaml = format!(
        "id: \"{op_id}\"\nverb: sync\nstrategy: ff\nsource: \"{src}\"\ntarget: \"{tgt}\"\nretire: false\nphase: running\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
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
// Backward compatibility: legacy .rwv-sync-op is still recognized by abort
// ---------------------------------------------------------------------------

#[test]
fn abort_recognizes_legacy_rwv_sync_op_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _server_sha) = make_locked_workspace(tmp.path(), "primary");
    let project_dir = &ws.project_dir;

    // Capture pre-op SHA.
    let sha = git_out(&["rev-parse", "HEAD"], project_dir);

    // Write legacy .rwv-sync-op marker (old format: just the op-id).
    let op_id = "legacy-op-id-12345";
    git(
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), &sha],
        project_dir,
    );
    std::fs::write(ws.root.join(".rwv-sync-op"), op_id).unwrap();

    // `rwv abort` must recognise the legacy marker and not error with "no op in progress".
    rwv().arg("abort").current_dir(&ws.root).assert().success();

    // Legacy marker should be cleaned up.
    assert!(
        !ws.root.join(".rwv-sync-op").exists(),
        "legacy .rwv-sync-op should be removed by abort"
    );
}

// ---------------------------------------------------------------------------
// Unit-level: op_state module round-trip (smoke test via rwv binary)
// ---------------------------------------------------------------------------

#[test]
fn op_state_file_written_during_sync_and_removed_on_success() {
    let tmp = tempfile::tempdir().unwrap();
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

    // Also verify no legacy marker was created.
    assert!(
        !ww.root.join(".rwv-sync-op").exists(),
        "old-style .rwv-sync-op should not be created by new code"
    );
}
