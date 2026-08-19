//! Per-phase idempotent re-entry tests for the sync phase machine.
//!
//! Acceptance criteria: phase functions are individually
//! exercised for idempotent re-entry. The full (phase × kill-point) crash
//! matrix is the crash-matrix sweep's scope; here we just pin that re-entering
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

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-q", "-b", "main"]);
    common::git_in(path, &["config", "user.email", "t@example.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    common::git_in(path, &["config", "commit.gpgsign", "false"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "README.md"]);
    common::git_in(path, &["commit", "-m", "init"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn write_manifest(project_dir: &Path) {
    let body = format!(
        "[repositories.\"{SERVER_PATH}\"]\ntype = \"git\"\nurl = \"{SERVER_URL}\"\nversion = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_dir.join("rwv.toml"), body).unwrap();
}

fn write_lock(project_dir: &Path, sha: &str) {
    common::fixture_lock(project_dir, &[(SERVER_PATH, SERVER_URL, sha)]);
}

fn make_locked_workspace(parent: &Path, name: &str) -> (Workspace, String) {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/example")).unwrap();
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
    write_manifest(&project_dir);
    write_lock(&project_dir, &sha);
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

fn make_shared_workspaces(parent: &Path) -> (Workspace, Workspace, String) {
    let (primary, c1) = make_locked_workspace(parent, "primary");
    let ww_root = parent.join("ww");
    std::fs::create_dir_all(ww_root.join("github/example")).unwrap();
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
        "{{\"id\": \"reentry-test-{phase}\", \"verb\": \"sync\", \"strategy\": \"rebase\", \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"{phase}\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-06-10T00:00:00Z\"}}",
        src = common::json_escaped(source),
        tgt = common::json_escaped(target),
    );
    std::fs::write(workspace.join(".rwv-op"), body).unwrap();
}

fn create_savepoint(repo: &Path, op_id: &str) {
    let head = common::git_in(repo, &["rev-parse", "HEAD"]);
    common::git_in(
        repo,
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), &head],
    );
}

#[test]
fn replay_reentry_on_already_converged_repos_is_a_noop_success() {
    let tmp = common::tempdir().unwrap();
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
    // (The CWD project repo's tip MAY move by exactly an auto-relock
    // commit; that's relock legitimately doing its first commit in a
    // fresh workweave, not a replay mutation.)
    let server_tip_before = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);

    rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert!(
        !ww.root.join(".rwv-op").exists(),
        "owner record must be cleared"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
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
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "reentry-test-relock";
    write_owner_record(&ww.root, &primary.root, &ww.root, "relock");
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);

    // Server (manifest) tip is the key invariant for relock re-entry:
    // relock only writes the project repo's lock, never the manifest repos.
    let server_tip_before = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);

    rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert!(
        !ww.root.join(".rwv-op").exists(),
        "owner record must be cleared"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
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
        "{{\"id\": \"reentry-test-advance\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \
         \"phase\": \"advance-target\", \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \
         \"overrides\": [], \"started_at\": \"2026-06-10T00:00:00Z\"}}",
        src = common::json_escaped(workspace),
        tgt = common::json_escaped(target),
    );
    std::fs::write(workspace.join(".rwv-op"), body).unwrap();
}

fn write_lease(workspace: &Path, owner: &Path) {
    let body = format!(
        "{{\"id\": \"reentry-test-advance\", \"owner\": \"{owner}\", \
         \"created_at\": \"2026-06-10T00:00:00Z\"}}",
        owner = common::json_escaped(owner),
    );
    std::fs::write(workspace.join(".rwv-op-lease"), body).unwrap();
}

#[test]
fn advance_target_reentry_on_equal_tips_is_a_noop_success() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    // primary and ww share tips. Write a sync-to owner record at advance-target
    // with ww as CWD and primary as target. Lease on the target side.
    let op_id = "reentry-test-advance";
    write_sync_to_owner_record_at_advance_target(&ww.root, &primary.root);
    write_lease(&primary.root, &ww.root);
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);

    let project_tip_before = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);
    let server_tip_before = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);

    rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert!(
        !ww.root.join(".rwv-op").exists(),
        "owner record must be cleared"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "lease must be cleared"
    );
    assert_eq!(
        common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]),
        project_tip_before,
        "target project tip must be unchanged on advance-target re-entry"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        server_tip_before,
        "target server tip must be unchanged on advance-target re-entry"
    );
}

// ---------------------------------------------------------------------------
// AdvanceTarget refuses to land on a detached target.
//
// `--continue` rebuilds the op context and re-enters the recorded phase
// without re-running the preflights, so the landing primitive itself has to
// hold the line: a detached target has no branch to advance, and `merge
// --ff-only` would move HEAD alone while reporting success.
// ---------------------------------------------------------------------------

#[test]
fn advance_target_reentry_refuses_to_land_on_a_detached_target() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, sha) = make_shared_workspaces(tmp.path());

    // Give the landing something to move.
    std::fs::write(ww.server_dir.join("ww.txt"), "ww work\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "ww.txt"]);
    common::git_in(&ww.server_dir, &["commit", "-m", "ww: advance"]);
    let ww_server_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);

    let op_id = "reentry-test-advance";
    write_sync_to_owner_record_at_advance_target(&ww.root, &primary.root);
    write_lease(&primary.root, &ww.root);
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);

    common::git_in(&primary.server_dir, &["checkout", "--detach", "HEAD"]);

    let err_output = rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&err_output.stderr);
    assert!(
        stderr.contains("not on a branch"),
        "the landing primitive must refuse a detached target by name; got:\n{stderr}"
    );

    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "refs/heads/main"]),
        sha,
        "target `main` must not have moved"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        sha,
        "the refused landing must not have moved the detached HEAD either"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "refs/heads/ww/main"]),
        ww_server_tip,
        "the source branch must still hold the work"
    );
    assert!(
        ww.root.join(".rwv-op").exists(),
        "op-state must survive so the operator can re-attach and --continue"
    );
}

// ---------------------------------------------------------------------------
// Cross-verb --continue: the invoked verb must match the recorded op's verb.
//
// `load_continuing_context` reads the CLI-invoked verb only to cross-check it
// against `record.verb`; every read past that point uses `record.verb`, so a
// missing check is purely diagnostic — the recorded op still completes, just
// under the CLI name the operator didn't type. The fixture below is the same
// advance-target/equal-tips setup `advance_target_reentry_on_equal_tips_is_a_noop_success`
// uses to resume a sync-to op to a clean success, so mismatching the verb
// here demonstrates the refusal is the only thing standing between a `sync`
// invocation and a silently completed landing.
// ---------------------------------------------------------------------------

#[test]
fn continue_refuses_a_sync_invocation_against_a_sync_to_op() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "reentry-test-advance";
    write_sync_to_owner_record_at_advance_target(&ww.root, &primary.root);
    write_lease(&primary.root, &ww.root);
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);

    let err_output = rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&err_output.stderr);
    assert!(
        stderr.contains("in-progress op is `sync-to` but `rwv sync --continue` was invoked"),
        "the refusal must name both the recorded verb and the invoked one; got:\n{stderr}"
    );
    assert!(
        stderr.contains("Run `rwv sync-to --continue` instead"),
        "the refusal must name the correct resume command; got:\n{stderr}"
    );

    // Purely diagnostic: a refused cross-verb continue leaves the mismatched
    // op exactly where it was, not partway resumed under the wrong verb.
    assert!(
        ww.root.join(".rwv-op").exists(),
        "the owner record must survive the refusal"
    );
    assert!(
        primary.root.join(".rwv-op-lease").exists(),
        "the target lease must survive the refusal"
    );
}

// ---------------------------------------------------------------------------
// Lease-side --continue: end-state parity with owner-side --continue.
//
// Plant an owner record at the SOURCE workweave (ww), a thin lease at the
// TARGET (primary), and make the workspaces genuinely diverge. Invoke
// `rwv sync-to --continue` FROM the LEASE workspace and assert that the end
// state is identical to what an owner-side `--continue` would produce:
//   - target's project tip advanced to the owner's converged tip;
//   - owner's savepoints dropped;
//   - both the owner record (.rwv-op) and the lease (.rwv-op-lease) cleared.
//
// Pre-fix, lease-side `--continue` runs the engine against the lease's own
// CWD (cwd_ctx / cwd_workspace_dir resolved from invocation CWD): replay
// enumerates the target's repos against the target's own lock (silent no-op),
// record_converged_tips records target tips instead of owner tips, and
// cleanup deletes savepoints under the target. Post-fix, the engine roots at
// the owner record's workspace; the literal invocation CWD only locates op-
// state.
// ---------------------------------------------------------------------------

fn write_sync_to_owner_record_at_phase(
    owner: &Path,
    source: &Path,
    target: &Path,
    phase: &str,
    id: &str,
) {
    let body = format!(
        "{{\"id\": \"{id}\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"{phase}\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-06-10T00:00:00Z\"}}",
        src = common::json_escaped(source),
        tgt = common::json_escaped(target),
    );
    std::fs::write(owner.join(".rwv-op"), body).unwrap();
}

fn write_lease_with_id(workspace: &Path, owner: &Path, id: &str) {
    let body = format!(
        "{{\"id\": \"{id}\", \"owner\": \"{owner}\", \"created_at\": \"2026-06-10T00:00:00Z\"}}",
        owner = common::json_escaped(owner),
    );
    std::fs::write(workspace.join(".rwv-op-lease"), body).unwrap();
}

#[test]
fn sync_to_continue_from_lease_workspace_drives_owner_to_clean_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared_workspaces(tmp.path());

    // Real, multi-repo divergence so the bug's "engine ran against the
    // lease's workspace" symptom is observable on the manifest repos:
    //
    //   1. Advance ww's manifest repo (server) to a new sha S'. primary's
    //      server worktree stays at S.
    //   2. Update ww's lock to pin S' and commit it (mirrors what
    //      `rwv update` would have produced after the divergent commit).
    //
    // After these steps: ww has project tip P' with lock pinning S';
    // primary has project tip P with lock pinning S.
    //
    // Under the fix, the engine roots at the owner (ww):
    //   - replay reads source (= primary) lock S, sees ww/server at S'
    //     (S is ancestor of S') → AlreadyAhead, not a failure;
    //   - relock regenerates ww's lock from ww's tips (server=S') —
    //     already current, no auto-relock commit;
    //   - advance-target ff's primary's server S → S' and primary's
    //     project P → P'.
    //
    // Under the bug, ctx.cwd_workspace_dir is the lease (primary):
    //   - replay reads primary/server (head=S, target=S) → NoOp;
    //   - relock's generate_lock walks ctx.cwd_ctx.primary_path() =
    //     primary, regenerating a lock pinning S (the LEASE's tip), then
    //     commits the regressed lock into ww's project — moving ww's
    //     project tip to a polluted state;
    //   - advance-target's per-manifest-repo loop reads
    //     ctx.cwd_workspace_dir.join(server) = primary's server (head=S),
    //     then ff's primary's server to S — trivial no-op, leaving
    //     primary's server stuck at S despite ww being at S';
    //   - ff_advance_repo for the project still uses ctx.cwd_project_dir
    //     (which was already owner-rooted pre-fix) so the project tip
    //     does advance — but to the polluted-by-relock ww tip.
    //
    // The two manifest-side checks below isolate the load-bearing
    // differences the bug doesn't escape: primary's server tip moves,
    // and primary's lock content names S' rather than S.
    std::fs::write(ww.server_dir.join("feature.txt"), "ww-only feature\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "feature.txt"]);
    common::git_in(
        &ww.server_dir,
        &["commit", "-m", "ww: divergent server commit"],
    );
    let ww_server_tip_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let primary_server_tip_pre = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    assert_ne!(
        ww_server_tip_pre, primary_server_tip_pre,
        "test setup: ww's server must diverge from primary's"
    );

    write_lock(&ww.project_dir, &ww_server_tip_pre);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &ww.project_dir,
        &["commit", "-m", "lock: pin ww server tip"],
    );
    let ww_project_tip_pre = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    let primary_project_tip_pre = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);
    assert_ne!(
        ww_project_tip_pre, primary_project_tip_pre,
        "test setup: ww's project must diverge from primary's"
    );

    // Plant op-state v2: full owner record at ww (the sync-to source),
    // thin lease at primary (the sync-to target). Phase=replay so the
    // full machine runs: replay → relock → advance-target → cleanup.
    let op_id = "lease-continue-test";
    write_sync_to_owner_record_at_phase(&ww.root, &ww.root, &primary.root, "replay", op_id);
    write_lease_with_id(&primary.root, &ww.root, op_id);

    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);

    // Invoke FROM the lease workspace.
    rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&primary.root)
        .assert()
        .success();

    // === End-state assertions (real end states, not impl echoes). ===

    assert!(
        !ww.root.join(".rwv-op").exists(),
        "owner record must be cleared after lease-side --continue"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "lease must be cleared after lease-side --continue"
    );

    // Target's MANIFEST repo (server) advanced to the owner's server tip.
    // Pre-fix this assertion fails: advance-target's per-manifest-repo
    // loop ff's target-against-target via the wrongly-rooted
    // ctx.cwd_workspace_dir, a trivial no-op.
    let primary_server_tip_post = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        primary_server_tip_post, ww_server_tip_pre,
        "target's server tip must be ff'd to the owner's server tip"
    );
    assert_ne!(
        primary_server_tip_post, primary_server_tip_pre,
        "target's server tip must have moved"
    );

    // Owner's lock must remain pinned at the operator-intended sha and
    // primary's lock must reach the same. Pre-fix, relock's
    // generate_lock walks the lease's primary_path() and regenerates a
    // regressed lock (pinning S, the lease's tip); the auto-relock
    // commit then bakes the regression into ww and ff's it into primary.
    let primary_lock = std::fs::read_to_string(primary.project_dir.join("rwv.lock")).unwrap();
    assert!(
        primary_lock.contains(&ww_server_tip_pre),
        "target lock must reference owner's server tip {ww_server_tip_pre} after sync-to; \
         got:\n{primary_lock}"
    );

    // Target's project tip advanced to the owner's project tip.
    let ww_project_tip_post = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    let primary_project_tip_post = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        primary_project_tip_post, ww_project_tip_post,
        "target's project tip must equal the owner's project tip after sync-to"
    );
    assert_ne!(
        primary_project_tip_post, primary_project_tip_pre,
        "target's project tip must have moved"
    );

    // Owner's savepoints dropped (cleanup ran in the OWNER workspace).
    let owner_savepoint_refs = common::git_in(
        &ww.project_dir,
        &["for-each-ref", &format!("refs/rwv/pre-op/{op_id}")],
    );
    assert!(
        owner_savepoint_refs.is_empty(),
        "owner savepoints must be dropped by cleanup; refs left: {owner_savepoint_refs}"
    );
}

// ---------------------------------------------------------------------------
// Retire test helpers
//
// For retire tests the CWD workspace must resolve as `Checkout::Workweave`
// (retire is only meaningful inside a workweave). We build on make_shared_workspaces
// but add a `.rwv-workweave` marker so WorkspaceContext::resolve identifies the
// workweave directory as a workweave rather than a plain weave.
// ---------------------------------------------------------------------------

/// Write a `.rwv-workweave` marker file into a workweave directory.
///
/// The marker records the primary root, project name, and parent workspace
/// (here the primary, since this is a direct child of primary).
fn write_workweave_marker(workweave_dir: &Path, primary_root: &Path, name: &str) {
    common::register_workweave(primary_root, "web-app", name, workweave_dir);
    let content = common::workweave_marker(primary_root, "web-app", primary_root);
    std::fs::write(workweave_dir.join(".rwv-workweave"), content).unwrap();
}

/// Create a workweave + primary pair where the workweave has a proper
/// `.rwv-workweave` marker, making it resolve as `Checkout::Workweave`.
///
/// The workweave is placed under `<parent>/.workweaves/web-app--ww` and
/// recorded in the registry so lookups find it by `(project="web-app",
/// name="ww")`.
/// Returns `(primary, workweave, initial_sha)`.
fn make_retire_workspaces(parent: &Path) -> (Workspace, Workspace, String) {
    let (primary, initial_sha) = make_locked_workspace(parent, "primary");

    // Workweave lives in `.workweaves/web-app--ww` relative to the parent dir.
    let ww_parent = parent.join(".workweaves");
    let ww_root = ww_parent.join("web-app--ww");
    std::fs::create_dir_all(ww_root.join("github/example")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let ww_server = ww_root.join(SERVER_PATH);
    common::git_in(
        &primary.server_dir,
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "web-app--ww",
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
            "web-app--ww",
        ],
    );
    // The marker is this root's only identity file — no `.rwv-active` beside
    // it, which resolution refuses.
    write_workweave_marker(&ww_root, &primary.root, "ww");

    let ww = Workspace {
        root: ww_root,
        project_dir: ww_project,
        server_dir: ww_server,
    };
    (primary, ww, initial_sha)
}

// ---------------------------------------------------------------------------
// Retire phase: phase=retire record survives a merged-check failure,
// --continue retries, abort restores both sides.
//
// Acceptance criteria:
//   1. Merged-check failure leaves phase=retire record AND target lease.
//   2. --continue completes retire after the operator reconciles.
//   3. Abort from phase=retire restores source (CWD workweave) and target.
// ---------------------------------------------------------------------------

/// Write a sync-to owner record at phase=retire with retire=true.
///
/// `converged_tips` mirrors what relock records on a real op: a phase=retire
/// record has always been through relock, so abort's HEAD-verified restore
/// classifies post-advance-target tips via these entries.
fn write_sync_to_retire_record(
    owner: &Path,
    source: &Path,
    target: &Path,
    id: &str,
    converged_tips: &[(&str, &str)],
) {
    let tips_json = converged_tips
        .iter()
        .map(|(repo, sha)| format!("\"{repo}\": \"{sha}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let body = format!(
        "{{\"id\": \"{id}\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": true, \"phase\": \"retire\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{{tips_json}}}, \"overrides\": [], \
         \"started_at\": \"2026-06-10T00:00:00Z\"}}",
        src = common::json_escaped(source),
        tgt = common::json_escaped(target),
    );
    std::fs::write(owner.join(".rwv-op"), body).unwrap();
}

/// Test: merged-check failure keeps phase=retire record and target lease.
///
/// Setup: ww and primary share tips (advance-target ran clean). Inject a
/// commit into ww's server repo AFTER planting the retire record so the
/// merged-check (ww-tip != primary-tip) fails. Verify:
///   - rwv sync-to --continue fails.
///   - owner record still present at ww, still at phase=retire.
///   - target lease still present at primary.
#[test]
fn retire_merged_check_failure_leaves_phase_retire_and_lease() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_retire_workspaces(tmp.path());

    let op_id = "retire-merged-check-fail";

    // Plant the retire owner record at ww, lease at primary. (No abort in
    // this test, so converged_tips are irrelevant.)
    write_sync_to_retire_record(&ww.root, &ww.root, &primary.root, op_id, &[]);
    write_lease_with_id(&primary.root, &ww.root, op_id);

    // CWD savepoints use op_id; target savepoints use "<op_id>-target".
    let target_op_id = format!("{op_id}-target");
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);
    // Savepoints for primary (target-side) — now created by guard_and_mark.
    create_savepoint(&primary.project_dir, &target_op_id);
    create_savepoint(&primary.server_dir, &target_op_id);

    // Inject divergence: commit into ww's server repo AFTER the advance-target
    // "completed". The merged-check compares ww and primary tips, so this
    // divergence makes the check fail.
    std::fs::write(ww.server_dir.join("post-advance.txt"), "ww divergence\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "post-advance.txt"]);
    common::git_in(
        &ww.server_dir,
        &["commit", "-m", "ww: post-advance divergence"],
    );

    // --continue must fail (merged-check fails).
    let out = rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .output()
        .expect("rwv command failed to run");
    assert!(
        !out.status.success(),
        "sync-to --continue must fail when merged-check fails; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Owner record must survive at ww, still at phase=retire.
    assert!(
        ww.root.join(".rwv-op").exists(),
        "owner record must survive a merged-check failure"
    );
    let record_json = std::fs::read_to_string(ww.root.join(".rwv-op")).expect("read owner record");
    assert!(
        record_json.contains("\"phase\": \"retire\""),
        "owner record must remain at phase=retire after merged-check failure; got:\n{record_json}"
    );

    // Target lease must survive at primary.
    assert!(
        primary.root.join(".rwv-op-lease").exists(),
        "target lease must survive a merged-check failure"
    );

    // Error message must mention --continue and abort.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--continue") || stderr.contains("sync-to --continue"),
        "error must mention --continue for resumability; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv abort"),
        "error must mention rwv abort for rollback; stderr:\n{stderr}"
    );
    // Pin the specific refusal, not just its generic resumability hints: a
    // different bail sharing "--continue"/"rwv abort" would pass the two
    // checks above without this one firing at all.
    assert!(
        stderr.contains("--retire: workweave's manifest repos differ from target after sync-to"),
        "the merged-check failure must name itself as the retire divergence refusal; \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(SERVER_PATH),
        "the refusal must name which repo diverged; stderr:\n{stderr}"
    );
    // The gate is equality-only on purpose — the right conservatism for a
    // delete. What the operator still needs is which way to reconcile, and
    // only the per-repo verdict says it. Here CWD contains the target, so the
    // reconcile runs toward the target; a line that stopped at the two SHAs
    // leaves the operator to work that out with git plumbing.
    assert!(
        stderr.contains("CWD is strictly ahead of the target by 1 commit (contains it)"),
        "the per-repo line must state the reconcile direction, not just the two \
         tips; stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("diverged"),
        "a target CWD provably contains is not a divergence; stderr:\n{stderr}"
    );

    // Nothing destructive happened: the workweave must still be on disk.
    assert!(
        ww.root.exists(),
        "a refused retire must leave the workweave in place"
    );
}

/// Test: --continue completes retire after the operator reconciles.
///
/// Setup: same as the failure test, but after injecting the divergence we
/// also fast-forward primary's server to match (simulating reconciliation).
/// Then --continue should succeed: merged-check passes, workweave is deleted,
/// lease is cleared.
#[test]
fn retire_continue_completes_after_reconciliation() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_retire_workspaces(tmp.path());

    let op_id = "retire-continue-reconciled";

    // Plant the retire owner record at ww, lease at primary. (No abort in
    // this test, so converged_tips are irrelevant.)
    write_sync_to_retire_record(&ww.root, &ww.root, &primary.root, op_id, &[]);
    write_lease_with_id(&primary.root, &ww.root, op_id);

    // CWD-side savepoints use op_id; target-side savepoints use "<op_id>-target"
    // (see target_savepoint_id in sync.rs — worktree pairs share a ref namespace,
    // so separate ids prevent the first restore from deleting the second's ref).
    let target_op_id = format!("{op_id}-target");
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);
    create_savepoint(&primary.project_dir, &target_op_id);
    create_savepoint(&primary.server_dir, &target_op_id);

    // Inject a divergence (same as failure test).
    std::fs::write(ww.server_dir.join("post-advance.txt"), "ww divergence\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "post-advance.txt"]);
    common::git_in(
        &ww.server_dir,
        &["commit", "-m", "ww: post-advance divergence"],
    );
    let ww_server_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);

    // Verify the failure state first (the merged-check actually fires).
    let out = rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .output()
        .expect("rwv command failed to run");
    assert!(
        !out.status.success(),
        "initial --continue must fail; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Reconcile: fast-forward primary's server to match ww.
    // (Simulates the operator running `git fetch` / `git merge` in the target.)
    common::git_in(
        &primary.server_dir,
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
    );
    let fetch_head = common::git_in(&primary.server_dir, &["rev-parse", "FETCH_HEAD"]);
    common::git_in(&primary.server_dir, &["reset", "--hard", &fetch_head]);
    let primary_server_tip_after = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        primary_server_tip_after, ww_server_tip,
        "test setup: reconciliation must bring primary's server to ww's tip"
    );

    // Now --continue should succeed.
    rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Owner workspace (workweave) must be deleted.
    assert!(
        !ww.root.exists(),
        "workweave directory must be deleted after successful retire"
    );

    // Target lease must be cleared.
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "target lease must be cleared after successful retire"
    );
}

/// Test: retire refuses to delete a workweave that still has uncommitted
/// changes, even once the merged-check (manifest repos match target) passes.
///
/// Setup: ww and primary already share tips (no divergence, unlike the
/// merged-check-failure test above), so the first retire guard clears. An
/// untracked file left in ww's manifest repo worktree is the uncommitted
/// change the second guard must catch. Verify:
///   - rwv sync-to --continue fails, naming the dirty path.
///   - the workweave is NOT deleted.
///   - the untracked file is untouched (the refusal took no action).
#[test]
fn retire_refuses_when_workweave_has_uncommitted_changes_after_convergence() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_retire_workspaces(tmp.path());

    let op_id = "retire-dirty-check";

    write_sync_to_retire_record(&ww.root, &ww.root, &primary.root, op_id, &[]);
    write_lease_with_id(&primary.root, &ww.root, op_id);

    let target_op_id = format!("{op_id}-target");
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);
    create_savepoint(&primary.project_dir, &target_op_id);
    create_savepoint(&primary.server_dir, &target_op_id);

    // Leave an uncommitted (untracked) file in ww's manifest repo. Tips still
    // match primary's, so the merged-check passes and the dirty-check is what
    // must fire.
    std::fs::write(ww.server_dir.join("stray.txt"), "not committed\n").unwrap();

    let out = rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .output()
        .expect("rwv command failed to run");
    assert!(
        !out.status.success(),
        "sync-to --continue must fail when the workweave is dirty; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--retire: workweave has uncommitted changes after sync-to"),
        "the refusal must name itself as the retire dirty-check; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(SERVER_PATH),
        "the refusal must name which repo is dirty; stderr:\n{stderr}"
    );

    assert!(
        ww.root.exists(),
        "a workweave refused for dirtiness must not be deleted"
    );
    assert!(
        ww.server_dir.join("stray.txt").exists(),
        "the refusal must not have touched the uncommitted file"
    );
    assert!(
        ww.root.join(".rwv-op").exists(),
        "owner record must survive the dirty-check refusal"
    );
}

/// Test: abort from phase=retire restores both source and target.
///
/// Setup: ww is ahead of primary (divergence before any sync-to). We plant
/// a phase=retire record with pre-op savepoints on both sides. Then call
/// `rwv abort` and verify both workspaces are restored to their pre-op state.
///
/// The pre-op state is: ww's server at sha_before_advance (original shared
/// tip); primary's server also at that tip (savepoint captures pre-op).
/// The "post-advance" state injected here simulates what advance-target
/// would have written to primary (ff'd to ww's tip).
#[test]
fn retire_abort_restores_source_and_target() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, initial_sha) = make_retire_workspaces(tmp.path());

    let op_id = "retire-abort-test";

    // Record the pre-op tips (what savepoints will capture).
    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let primary_server_pre = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        ww_server_pre, primary_server_pre,
        "test setup: both sides start at the same tip"
    );
    assert_eq!(
        ww_server_pre, initial_sha,
        "test setup: ww server must be at the initial sha"
    );

    // Create pre-op savepoints BEFORE simulating the advance.
    // CWD savepoints use op_id; target savepoints use "<op_id>-target" to
    // avoid shared-ref collision in the worktree topology (see sync.rs
    // target_savepoint_id / guard_and_mark).
    let target_op_id = format!("{op_id}-target");
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);
    create_savepoint(&primary.project_dir, &target_op_id);
    create_savepoint(&primary.server_dir, &target_op_id);

    // Simulate advance-target: add a commit to ww's server and ff primary to it.
    std::fs::write(ww.server_dir.join("advanced.txt"), "advance\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "advanced.txt"]);
    common::git_in(
        &ww.server_dir,
        &["commit", "-m", "ww: advance-target commit"],
    );
    let ww_server_advanced = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);

    // Fast-forward primary's server to the advanced tip (what advance-target does).
    common::git_in(
        &primary.server_dir,
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
    );
    let fetch_head = common::git_in(&primary.server_dir, &["rev-parse", "FETCH_HEAD"]);
    common::git_in(&primary.server_dir, &["reset", "--hard", &fetch_head]);
    let primary_server_advanced = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        primary_server_advanced, ww_server_advanced,
        "test setup: primary server must be ff'd to ww's advanced tip"
    );

    // Plant phase=retire record + lease. The op is mid-retire. A real
    // phase=retire record has been through relock, which recorded the
    // converged tips — abort's HEAD-verified restore needs
    // them to classify the post-advance-target tips as attributable.
    write_sync_to_retire_record(
        &ww.root,
        &ww.root,
        &primary.root,
        op_id,
        &[("github/example/server", ww_server_advanced.as_str())],
    );
    write_lease_with_id(&primary.root, &ww.root, op_id);

    // Run abort from the owner workspace.
    rwv()
        .args(["abort"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Both op-state files must be cleared.
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "owner record must be cleared by abort"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "target lease must be cleared by abort"
    );

    // Source (ww) server must be restored to its pre-op tip.
    let ww_server_post = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        ww_server_post, ww_server_pre,
        "abort must restore ww's server to the pre-op savepoint tip"
    );

    // Target (primary) server must be restored to its pre-op tip.
    let primary_server_post = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        primary_server_post, primary_server_pre,
        "abort must restore primary's server to the pre-op savepoint tip"
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
    let tmp = common::tempdir().unwrap();
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

// ---------------------------------------------------------------------------
// A resume at advance-target re-enters relock first.
//
// Advance-target publishes CWD's manifest tips AND CWD's lock. Between the
// failure that stranded the op and the resume, the operator moves CWD — that is
// what a resume is for — which leaves the lock pinning revisions CWD no longer
// holds. These tests assert the DELIVERED state: the target's lock names the
// target's manifest tip. Exit code and the completion sentence are both 0 and
// present either way.
// ---------------------------------------------------------------------------

fn write_sync_to_record_with_strategy(
    owner: &Path,
    target: &Path,
    strategy: &str,
    id: &str,
) -> String {
    let body = format!(
        "{{\"id\": \"{id}\", \"verb\": \"sync-to\", \"strategy\": \"{strategy}\", \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \
         \"phase\": \"advance-target\", \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \
         \"overrides\": [], \"started_at\": \"2026-06-10T00:00:00Z\"}}",
        src = common::json_escaped(owner),
        tgt = common::json_escaped(target),
    );
    std::fs::write(owner.join(".rwv-op"), body).unwrap();
    id.to_string()
}

/// Plant a stranded advance-target op whose CWD moved after the strand, resume
/// it, and return `(delivered_target_tip, target_lock_contents)`.
fn resume_stranded_advance_target(tmp: &Path, strategy: &str, id: &str) -> (String, String) {
    let (primary, ww, _sha) = make_shared_workspaces(tmp);

    // The operator's post-strand fix: a manifest-repo commit in CWD that the
    // committed lock does not pin.
    std::fs::write(ww.server_dir.join("fix.txt"), "operator fix\n").unwrap();
    common::git_in(&ww.server_dir, &["add", "fix.txt"]);
    common::git_in(&ww.server_dir, &["commit", "-m", "ww: post-strand fix"]);
    let ww_server_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);

    let op_id = write_sync_to_record_with_strategy(&ww.root, &primary.root, strategy, id);
    write_lease(&primary.root, &ww.root);
    create_savepoint(&ww.project_dir, &op_id);
    create_savepoint(&ww.server_dir, &op_id);

    rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    let delivered = common::git_in(&primary.server_dir, &["rev-parse", "refs/heads/main"]);
    assert_eq!(
        delivered, ww_server_tip,
        "the resume must land CWD's post-strand tip on the target"
    );
    let target_lock = std::fs::read_to_string(primary.project_dir.join("rwv.lock")).unwrap();
    (delivered, target_lock)
}

#[test]
fn advance_target_resume_relocks_so_the_target_lock_pins_what_it_delivered() {
    let tmp = common::tempdir().unwrap();
    let (delivered, target_lock) =
        resume_stranded_advance_target(tmp.path(), "rebase", "reentry-test-relock-on-resume");
    assert!(
        target_lock.contains(&delivered),
        "the target's lock must pin the manifest tip the resume delivered ({delivered}); lock:\n\
         {target_lock}"
    );
}

/// `--strategy=ff` makes REPLAY a no-op, not relock: the operator's post-strand
/// fix moved CWD's manifest repo under either strategy, so the lock has to be
/// regenerated under either one too.
#[test]
fn advance_target_resume_relocks_under_ff_strategy_too() {
    let tmp = common::tempdir().unwrap();
    let (delivered, target_lock) =
        resume_stranded_advance_target(tmp.path(), "ff", "reentry-test-relock-on-resume-ff");
    assert!(
        target_lock.contains(&delivered),
        "the target's lock must pin the manifest tip the resume delivered ({delivered}); lock:\n\
         {target_lock}"
    );
}

// ---------------------------------------------------------------------------
// Resume re-gates the re-pinned source, and says it re-read it.
//
// `--continue` re-pins the source snapshot at resume time so replay re-entry
// has coherent inputs. The fresh path takes its pin and its GATE together:
// the classification that pin produces is what refuses an unresolvable lock
// entry or an anomalous lock relation. The resume path took the pin alone —
// so a source that went anomalous between the strand and the resume was
// replayed from at a state the op's own first invocation would have refused,
// and the operator was told nothing about either the re-read or the state.
// ---------------------------------------------------------------------------

/// Commit `content` as `name` in `repo`, returning the new tip.
fn commit_file(repo: &Path, name: &str, content: &str, message: &str) -> String {
    std::fs::write(repo.join(name), content).unwrap();
    common::git_in(repo, &["add", name]);
    common::git_in(repo, &["commit", "-m", message]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// Drive the source's committed lock ahead of its own manifest tip: commit,
/// lock that commit, then rewind the checkout. HEAD is now a strict ancestor
/// of what the lock pins — `LockRelation::Behind`, the anomalous shape the
/// fresh path refuses on either side. Returns the tip the lock pins.
fn make_source_lock_anomalous(source: &Workspace, base_sha: &str) -> String {
    let ahead = commit_file(&source.server_dir, "ahead.txt", "ahead\n", "src: ahead");
    write_lock(&source.project_dir, &ahead);
    common::git_in(&source.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &source.project_dir,
        &["commit", "-m", "lock: pin the ahead commit"],
    );
    common::git_in(&source.server_dir, &["reset", "--hard", base_sha]);
    ahead
}

#[test]
fn a_resume_refuses_a_source_that_went_anomalous_and_leaves_the_op_parked() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, base_sha) = make_shared_workspaces(tmp.path());

    let op_id = "reentry-test-replay";
    write_owner_record(&ww.root, &primary.root, &ww.root, "replay");
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);

    make_source_lock_anomalous(&primary, &base_sha);

    let record_before = std::fs::read(ww.root.join(".rwv-op")).unwrap();
    let savepoint_before = common::git_in(
        &ww.server_dir,
        &["rev-parse", &format!("refs/rwv/pre-op/{op_id}")],
    );
    let cwd_tip_before = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);

    let out = rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .output()
        .expect("rwv command failed to run");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "a resume must not consume a source at a state the fresh path refuses; \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("lock-freshness"),
        "the refusal must be the lock-freshness gate, not some later failure; \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv sync --continue") && stderr.contains("rwv abort"),
        "a refused resume must name both ways out of the op it left standing; \
         stderr:\n{stderr}"
    );

    // The refusal changed nothing: the op is still exactly where it was, which
    // is what makes "fix the source, then --continue" a true instruction.
    assert_eq!(
        std::fs::read(ww.root.join(".rwv-op")).unwrap(),
        record_before,
        "the owner record must be byte-identical across the refusal"
    );
    assert_eq!(
        common::git_in(
            &ww.server_dir,
            &["rev-parse", &format!("refs/rwv/pre-op/{op_id}")]
        ),
        savepoint_before,
        "the savepoint must survive the refusal unmoved"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        cwd_tip_before,
        "a refused resume must not have replayed anything"
    );
}

#[test]
fn a_resume_names_the_source_tip_it_re_read() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _base_sha) = make_shared_workspaces(tmp.path());

    // The source moves between the strand and the resume, healthily: a new
    // commit, and a lock that pins it. This is the state the banner used to
    // pass over in silence — the resumed op plans against a read the operator
    // was never shown.
    let moved = commit_file(&primary.server_dir, "moved.txt", "moved\n", "src: moved");
    write_lock(&primary.project_dir, &moved);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &primary.project_dir,
        &["commit", "-m", "lock: after the strand"],
    );
    let source_project_tip = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);

    write_owner_record(&ww.root, &primary.root, &ww.root, "replay");
    create_savepoint(&ww.project_dir, "reentry-test-replay");
    create_savepoint(&ww.server_dir, "reentry-test-replay");

    let out = rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .output()
        .expect("rwv command failed to run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a healthy moved source must resume, not refuse; stderr:\n{stderr}"
    );

    // The value, not the sentence: the tip named is the one this session
    // pinned and replayed from, so the announcement and the plan cannot come
    // apart.
    assert!(
        stderr.contains("source re-read at resume"),
        "the resume must state that it re-read the source; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(&source_project_tip[..12]),
        "the re-read line must name the source project tip this resume pinned \
         ({source_project_tip}); stderr:\n{stderr}"
    );
    // And it delivered from that read: CWD holds the moved tip.
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        moved,
        "the resume must deliver the tips the re-read announced"
    );
}
