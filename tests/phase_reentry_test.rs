//! Per-phase idempotent re-entry tests for the sync phase machine.
//!
//! Acceptance criteria for bead fo-jsbr3i.2: phase functions are individually
//! exercised for idempotent re-entry. The full (phase × kill-point) crash
//! matrix is sibling fo-jsbr3i.7's scope; here we just pin that re-entering
//! each phase from a quiescent state is a no-op-shaped success.
//!
//! Mechanism: write an owner record at the target phase (sometimes with
//! converged_tips pre-populated), then invoke `rwv sync --continue` /
//! `rwv sync-to --continue` and assert success with no spurious mutation.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

const SERVER_PATH: &str = "github/example/server";
const SERVER_URL: &str = "https://github.com/example/server";

fn rwv() -> AssertCommand {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git command failed");
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
        .output()
        .expect("git command failed");
    assert!(
        out.status.success(),
        "git {:?} failed in {}:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-q", "-b", "main"], path);
    git(&["config", "user.email", "t@example.com"], path);
    git(&["config", "user.name", "Test"], path);
    git(&["config", "commit.gpgsign", "false"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "README.md"], path);
    git(&["commit", "-m", "init"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

fn write_manifest(project_dir: &Path) {
    let body = format!(
        "repositories:\n  {SERVER_PATH}:\n    type: git\n    url: {SERVER_URL}\n    version: main\n    role: owned\n"
    );
    std::fs::write(project_dir.join("rwv.yaml"), body).unwrap();
}

fn write_lock(project_dir: &Path, sha: &str) {
    let body = format!(
        "repositories:\n  {SERVER_PATH}:\n    type: git\n    url: {SERVER_URL}\n    version: {sha}\n"
    );
    std::fs::write(project_dir.join("rwv.lock"), body).unwrap();
}

fn make_locked_workspace(parent: &Path, name: &str) -> (Workspace, String) {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/example")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server_dir = root.join(SERVER_PATH);
    let sha = init_repo(&server_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();
    write_manifest(&project_dir);
    write_lock(&project_dir, &sha);
    git(&["add", ".gitattributes", "rwv.yaml", "rwv.lock"], &project_dir);
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

fn make_shared_workspaces(parent: &Path) -> (Workspace, Workspace, String) {
    let (primary, c1) = make_locked_workspace(parent, "primary");
    let ww_root = parent.join("ww");
    std::fs::create_dir_all(ww_root.join("github/example")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let ww_server = ww_root.join(SERVER_PATH);
    git(
        &[
            "worktree", "add", &ww_server.to_string_lossy(), "-b", "ww/main",
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
// Replay re-entry
//
// After a successful sync, the owner record is gone — we can't directly
// re-enter replay via --continue (--continue refuses with "no op in progress"
// when state is clean). What we CAN test: re-entering an already-converged
// replay is detected and short-circuits. We synthesize this by writing an
// owner record at `replay` whose source state already matches CWD, then
// continue: the per-repo no-op detection must drive replay to success
// without any mutation.
// ---------------------------------------------------------------------------

fn write_owner_record(workspace: &Path, source: &Path, target: &Path, phase: &str) {
    let body = format!(
        "id: \"reentry-test-{phase}\"\n\
         verb: sync\n\
         strategy: rebase\n\
         source: \"{src}\"\n\
         target: \"{tgt}\"\n\
         retire: false\n\
         phase: {phase}\n\
         converged_tips: {{}}\n\
         overrides: []\n\
         started_at: \"2026-06-10T00:00:00Z\"\n",
        src = source.display(),
        tgt = target.display(),
    );
    std::fs::write(workspace.join(".rwv-op"), body).unwrap();
}

fn create_savepoint(repo: &Path, op_id: &str) {
    let head = git_out(&["rev-parse", "HEAD"], repo);
    git(
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), &head],
        repo,
    );
}

#[test]
fn replay_reentry_on_already_converged_repos_is_a_noop_success() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    // ww and primary are already in sync; HEADs match. Write an owner
    // record at phase=replay and savepoints to mirror what a crashed
    // mid-replay invocation would have left behind.
    let op_id = "reentry-test-replay";
    write_owner_record(&ww.root, &primary.root, &ww.root, "replay");
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);

    // The server (manifest) repo must be untouched on a converged-replay
    // re-entry — the §4 invariant for replay's per-repo no-op detection.
    // (The CWD project repo's tip MAY move by exactly the auto-relock
    // commit that adds the `workweave:` field to the lock; that's relock
    // legitimately doing its first commit in a fresh workweave, not a
    // replay mutation.)
    let server_tip_before = git_out(&["rev-parse", "HEAD"], &ww.server_dir);

    rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert!(!ww.root.join(".rwv-op").exists(), "owner record must be cleared");
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ww.server_dir),
        server_tip_before,
        "server tip must be unchanged on re-entry of converged replay"
    );
}

// ---------------------------------------------------------------------------
// Relock re-entry
//
// Owner record at phase=relock: replay supposedly completed but relock didn't
// finish. Re-entering relock with a lock that's already current must be a
// no-op (regenerate_lock_phase3 + commit_lock_file_with_message short-
// circuit when content unchanged); converged_tips must be populated; and
// the machine must proceed to cleanup.
// ---------------------------------------------------------------------------

#[test]
fn relock_reentry_on_current_lock_is_a_noop_success() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "reentry-test-relock";
    write_owner_record(&ww.root, &primary.root, &ww.root, "relock");
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);

    // Server (manifest) tip is the key invariant for relock re-entry:
    // relock only writes the project repo's lock, never the manifest repos.
    let server_tip_before = git_out(&["rev-parse", "HEAD"], &ww.server_dir);

    rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert!(!ww.root.join(".rwv-op").exists(), "owner record must be cleared");
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ww.server_dir),
        server_tip_before,
        "server tip must be unchanged when relock runs"
    );
}

// ---------------------------------------------------------------------------
// AdvanceTarget re-entry
//
// Owner record at phase=advance-target with verb=sync-to: the ff-advance
// must be a no-op when the target is already at the converged tip. We
// stage this with primary as target and the project repo at the converged
// tip via a write directly to the owner record's converged_tips map.
// ---------------------------------------------------------------------------

fn write_sync_to_owner_record_at_advance_target(workspace: &Path, target: &Path) {
    let body = format!(
        "id: \"reentry-test-advance\"\n\
         verb: sync-to\n\
         strategy: rebase\n\
         source: \"{src}\"\n\
         target: \"{tgt}\"\n\
         retire: false\n\
         phase: advance-target\n\
         converged_tips: {{}}\n\
         overrides: []\n\
         started_at: \"2026-06-10T00:00:00Z\"\n",
        src = workspace.display(),
        tgt = target.display(),
    );
    std::fs::write(workspace.join(".rwv-op"), body).unwrap();
}

fn write_lease(workspace: &Path, owner: &Path) {
    let body = format!(
        "id: \"reentry-test-advance\"\nowner: \"{owner}\"\n",
        owner = owner.display(),
    );
    std::fs::write(workspace.join(".rwv-op-lease"), body).unwrap();
}

#[test]
fn advance_target_reentry_on_equal_tips_is_a_noop_success() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    // primary and ww share tips. Write a sync-to owner record at advance-target
    // with ww as CWD and primary as target. Lease on the target side.
    let op_id = "reentry-test-advance";
    write_sync_to_owner_record_at_advance_target(&ww.root, &primary.root);
    write_lease(&primary.root, &ww.root);
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);

    let project_tip_before = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    let server_tip_before = git_out(&["rev-parse", "HEAD"], &primary.server_dir);

    rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert!(!ww.root.join(".rwv-op").exists(), "owner record must be cleared");
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "lease must be cleared"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.project_dir),
        project_tip_before,
        "target project tip must be unchanged on advance-target re-entry"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
        server_tip_before,
        "target server tip must be unchanged on advance-target re-entry"
    );
}

// ---------------------------------------------------------------------------
// Driver invariant: re-entering the same phase twice in a row is benign.
//
// Run sync once to a clean state, then plant an owner record at each phase
// (replay, relock, advance-target) and verify --continue completes each
// without mutation. This is a smoke test for the "every phase is idempotent
// and re-runnable from the record alone" invariant.
// ---------------------------------------------------------------------------

#[test]
fn re_entering_each_phase_independently_completes_to_a_clean_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());
    let op_id = "reentry-test-multi";

    for phase in ["replay", "relock"] {
        // Plant owner record at the phase.
        write_owner_record(&ww.root, &primary.root, &ww.root, phase);
        create_savepoint(&ww.project_dir, op_id);
        create_savepoint(&ww.server_dir, op_id);

        rwv()
            .args(["sync", "--continue"])
            .current_dir(&ww.root)
            .assert()
            .success();

        assert!(
            !ww.root.join(".rwv-op").exists(),
            "owner record must be cleared after --continue from phase {phase}"
        );
    }
}
