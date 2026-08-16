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
//! - Tests 3 and 5 are `sync-to` scenarios (multi-workspace); they drive a real `sync-to`
//!   into a parked state via a target-side collision and recover it via `--continue` /
//!   `rwv abort`.
//! - Test 1 uses `rwv sync` from two directions to trigger the in-progress check.
//! - Test 2 uses `rwv sync --strategy rebase` to induce a conflict, then `--continue`.
//! - Test 4 uses `rwv sync` with mismatched `--strategy`.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Git helpers (mirroring e2e_sync_abort_test.rs pattern)
// ---------------------------------------------------------------------------

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
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

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
}

fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    // Round-trip through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let entries: Vec<String> = repos
        .iter()
        .map(|(path, url, sha)| {
            format!("{path:?}: {{\"type\": \"git\", \"url\": {url:?}, \"version\": {sha:?}}}")
        })
        .collect();
    let raw = format!("{{\"repositories\": {{{}}}}}", entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
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
///     projects/web-app/       (git repo, rwv.toml + rwv.lock committed)
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
    common::git_in(
        &project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
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
    common::git_in(
        &primary.server_dir,
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/main",
        ],
    );

    let ww_project = ww_root.join("projects/web-app");
    common::git_in(
        &primary.project_dir,
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
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
// An in-progress op-state file blocks a new `rwv sync` from either the CWD
// or the target workspace. The first test parks a real op via a genuine
// rebase conflict; the second constructs an owner record directly at the
// `relock` phase, which that recipe cannot reach organically.
// ---------------------------------------------------------------------------

#[test]
fn concurrent_op_detection_blocks_new_sync_in_cwd_workspace() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary and ww each commit conflicting content to the same file, so a
    // rebase sync from ww genuinely conflicts and parks — a real in-flight
    // op left on disk by the production acquire path (same recipe as
    // atomic_lease_acquisition_test.rs), not hand-planted JSON.
    let c2 = make_commit(
        &primary.server_dir,
        "shared.txt",
        "primary version\n",
        "primary: shared.txt",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: primary C2"]);

    let c_ww = make_commit(
        &ww.server_dir,
        "shared.txt",
        "ww version\n",
        "ww: shared.txt (conflicts with primary)",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww C_ww"]);

    // First sync parks mid-replay on the real conflict.
    rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "rebase",
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert()
        .failure();
    assert!(
        ww.root.join(".rwv-op").exists(),
        "the parked op must leave the owner record at ww"
    );

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

const STALE_STARTED_AT: &str = "2020-01-01T00:00:00Z";
const STALE_STARTED_AT_UNIX: u64 = 1_577_836_800;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn concurrent_op_detection_error_names_phase_and_start_time() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let op_id = repoweave::op_state::OpId::new_now();
    let mut owner = repoweave::op_state::OwnerRecord::new_sync(
        &op_id,
        repoweave::op_state::SyncStrategy::Rebase,
        repoweave::manifest::ProjectName::new("web-app").unwrap(),
        primary.root.clone(),
        ww.root.clone(),
    );
    owner.phase = repoweave::op_state::OpPhase::Relock;
    owner.started_at = STALE_STARTED_AT.to_owned();
    repoweave::op_state::write_owner(&ww.root, &owner).unwrap();

    let before = unix_now();
    let assertion = rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let after = unix_now();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("relock"),
        "error should mention the in-progress phase; got: {stderr}"
    );

    // started_at is fixed decades in the past, so the elapsed hours the
    // refusal reports are pinned to this record's own started_at and not
    // satisfiable by an arbitrary or defaulted timestamp. The window between
    // `before` and `after` covers the rare case where the real clock crosses
    // an hour boundary mid-run.
    let hours_lo = (before - STALE_STARTED_AT_UNIX) / 3600;
    let hours_hi = (after - STALE_STARTED_AT_UNIX) / 3600;
    let candidates: Vec<String> = (hours_lo..=hours_hi)
        .map(|h| format!("started {h}h ago"))
        .collect();
    assert!(
        candidates.iter().any(|c| stderr.contains(c.as_str())),
        "error should report elapsed time computed from this op's own started_at \
         ({STALE_STARTED_AT}); expected one of {candidates:?}; got: {stderr}"
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
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: C2"]);

    // ww: make a conflicting commit on the same file (plus lock update).
    let c_ww = make_commit(
        &ww.server_dir,
        "shared.txt",
        "ww version\n",
        "ww: add shared.txt (conflicts with primary)",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww C_ww"]);

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

    // Resolve the conflict in the server repo (Phase 2 conflict). The server
    // repo is a linked worktree; git resolves the rebase-in-progress state
    // via the gitlink transparently when invoked with `ww.server_dir` as its
    // working directory, so no path probing is needed.
    std::fs::write(ww.server_dir.join("shared.txt"), "resolved version\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "shared.txt"]);
    let continue_rebase = common::git()
        .args(["rebase", "--continue"])
        .current_dir(&ww.server_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();
    assert!(
        continue_rebase.status.success(),
        "rebase --continue failed: {}",
        String::from_utf8_lossy(&continue_rebase.stderr)
    );

    // `rwv sync --continue` should now resume and complete.
    rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert!(
        !op_state_path.exists(),
        "op-state file must be removed once the resumed sync completes"
    );
    assert_eq!(
        std::fs::read_to_string(ww.server_dir.join("shared.txt")).unwrap(),
        "resolved version\n",
        "the resolved content must survive the completed rebase"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Mid-step-3 resume (op-state at advance-target → --continue → completion)
//
// AdvanceTarget is a sync-to-only phase (`OpPhase` doc), so the real fixture
// is a `sync-to` parked there: the target holds an untracked file that
// collides with a path the incoming project commit writes, so every manifest
// repo lands but the project repo's ff-advance blocks (same recipe
// resume_project_binding_test.rs uses to reach this phase for real).
// ---------------------------------------------------------------------------

#[test]
fn mid_step3_continue_does_not_produce_in_progress_refusal() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let landed = make_commit(&ww.server_dir, "advance.txt", "advance\n", "ww: advance");
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &landed)]);
    std::fs::write(ww.project_dir.join("notes.txt"), "ww notes\n").unwrap();
    common::git_in(&ww.project_dir, &["add", "rwv.lock", "notes.txt"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    // The target holds an untracked file where the incoming project commit
    // writes one, so the manifest repo lands but the project repo blocks —
    // the op parks genuinely at advance-target.
    std::fs::write(primary.project_dir.join("notes.txt"), "primary scratch\n").unwrap();

    rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    assert!(
        ww.root.join(".rwv-op").exists(),
        "the parked op must leave the owner record in CWD"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        landed,
        "the parked op must have landed the target's manifest repo already"
    );

    // Clear the collision so the resume has a path forward.
    std::fs::remove_file(primary.project_dir.join("notes.txt")).unwrap();

    // `rwv sync-to --continue` (alone — all params from op-state) should not
    // produce the "in progress, resolve and rerun" refusal error.
    let result = rwv()
        .args(["sync-to", "--continue"])
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
    let op_state_json = format!(
        "{{\"id\": \"test-exclusive-1234\", \"verb\": \"sync\", \"strategy\": \"rebase\", \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"replay\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        src = common::json_escaped(&primary.root),
        tgt = common::json_escaped(&ww.root),
    );
    std::fs::write(ww.root.join(".rwv-op"), &op_state_json).unwrap();

    // Passing --strategy alongside --continue must be rejected.
    let assertion = rwv()
        .args(["sync", "--strategy", "rebase", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    // Clap's conflicts_with wording names both the flag and --continue.
    assert!(
        stderr.contains("cannot be used with '--continue'"),
        "expected clap exclusivity error naming --continue as the conflict; got: {stderr}"
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

/// Plain `sync`'s `--json` still conflicts with `--continue`. The sync-to
/// side relaxed this (its envelope reports the machine's own coordinates and
/// a `resumed` disclosure); the sync envelope carries no such disclosure, so
/// its conflict stands until it grows one. This pin is what a relaxer must
/// consciously remove.
#[test]
fn sync_continue_with_json_flag_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let assertion = rwv()
        .args(["sync", "--json", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("cannot be used with '--continue'"),
        "expected clap exclusivity error for sync --json + --continue; got: {stderr}"
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
        stderr.contains("cannot be used with '--continue'"),
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
        stderr.contains("cannot be used with '--continue'"),
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
        stderr.contains("cannot be used with '--continue'"),
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
        stderr.contains("cannot be used with '--continue'"),
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
// Drive a real `sync-to` into a parked state (owner record at ww, thin lease
// at primary — same target-side collision recipe test 3 above uses), then
// run `rwv abort` from ww. Verify both op-state files are removed.
// ---------------------------------------------------------------------------

#[test]
fn abort_from_cwd_cleans_cross_workspace_op_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let landed = make_commit(&ww.server_dir, "advance.txt", "advance\n", "ww: advance");
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &landed)]);
    std::fs::write(ww.project_dir.join("notes.txt"), "ww notes\n").unwrap();
    common::git_in(&ww.project_dir, &["add", "rwv.lock", "notes.txt"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    // The target holds an untracked file where the incoming project commit
    // writes one, so the manifest repo lands but the project repo blocks —
    // ww ends up the owner (CWD/source of sync-to), primary the target with
    // the thin lease.
    std::fs::write(primary.project_dir.join("notes.txt"), "primary scratch\n").unwrap();

    rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    assert!(
        ww.root.join(".rwv-op").exists(),
        "the parked op must leave the owner record in CWD"
    );
    assert!(
        primary.root.join(".rwv-op-lease").exists(),
        "the parked op must leave the lease at the target"
    );

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
    let ww_project_sha = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    common::git_in(
        &ww.project_dir,
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &ww_project_sha,
        ],
    );
    let ww_server_sha = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    common::git_in(
        &primary.server_dir,
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{op_id}"),
            &ww_server_sha,
        ],
    );

    let op_state_json = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"sync\", \"strategy\": \"ff\", \"project\": \"web-app\", \"source\": \"{src}\", \
         \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"replay\", \"advanced_tips\": {{}}, \
         \"converged_tips\": {{}}, \"overrides\": [], \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        src = common::json_escaped(&primary.root),
        tgt = common::json_escaped(&ww.root),
    );
    std::fs::write(ww.root.join(".rwv-op"), &op_state_json).unwrap();

    // Run abort.
    rwv().arg("abort").current_dir(&ww.root).assert().success();

    // Op-state file should be removed.
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "op-state file should be removed by abort"
    );

    // Repos should be at their pre-op state.
    let post_abort_project = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
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
    let json = format!(
        "{{\"id\": \"planted-op-1234\", \"verb\": \"{verb}\", \"strategy\": \"rebase\", \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"{phase}\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        src = common::json_escaped(src),
        tgt = common::json_escaped(tgt),
    );
    std::fs::write(ws_root.join(".rwv-op"), &json).unwrap();
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
    let lease_json = format!(
        "{{\"id\": \"planted-op-1234\", \"owner\": \"{owner}\", \
         \"created_at\": \"2026-05-27T10:00:00Z\"}}",
        owner = common::json_escaped(&ww.root),
    );
    std::fs::write(primary.root.join(".rwv-op-lease"), &lease_json).unwrap();

    // From the LEASED workspace (primary), `rwv update` must refuse, following
    // the lease pointer to the owner record for the rich message.
    let assertion = rwv()
        .args(["update"])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert_in_flight_op_refusal(&stderr, "sync-to");
    common::assert_names_operator_path(&stderr, &ww.root);
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
    common::git_in(&ww.server_dir, &["checkout", "-b", "throwaway"]);
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

/// A parked op is the deterministic fixture for observing mid-flight
/// presence: a live process exits before a test could race-read its
/// filesystem state, so the file has to be caught while a real op sits
/// stalled on a genuine conflict, not while one is merely running.
#[test]
fn op_state_file_written_during_sync_and_removed_on_success() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Both sides make conflicting changes to the same file, so the first
    // attempt parks mid-replay with the op-state file left on disk.
    let c2 = make_commit(
        &primary.server_dir,
        "shared.txt",
        "primary version\n",
        "primary: add shared.txt",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: C2"]);

    let c_ww = make_commit(
        &ww.server_dir,
        "shared.txt",
        "ww version\n",
        "ww: add shared.txt (conflicts with primary)",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww C_ww"]);

    rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "rebase",
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert()
        .failure();

    // Mid-flight: the parked replay conflict leaves the op-state file on
    // disk — the observation this test's name promises.
    assert!(
        ww.root.join(".rwv-op").exists(),
        "op-state file must be present while the op is parked mid-sync"
    );

    // Resolve the conflict. `ww.server_dir` is a linked worktree, so its
    // rebase-in-progress state lives under the primary checkout's
    // `.git/worktrees/`, not under `ww.server_dir/.git` (a gitlink file) —
    // git resolves that transparently when invoked with `ww.server_dir` as
    // its working directory.
    std::fs::write(ww.server_dir.join("shared.txt"), "resolved version\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "shared.txt"]);
    let continue_out = common::git()
        .args(["rebase", "--continue"])
        .current_dir(&ww.server_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();
    assert!(
        continue_out.status.success(),
        "rebase --continue failed: {}",
        String::from_utf8_lossy(&continue_out.stderr)
    );

    rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // On success, the op-state file must be removed.
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "op-state file must be removed after successful sync"
    );
}
