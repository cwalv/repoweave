//! Atomic-acquisition regression tests for `sync` / `sync-to`.
//!
//! The guard→mark TOCTOU closed here:
//!
//! * previously, `check_no_op_in_progress` was a read; two concurrent invocations
//!   both passed it and only collided later at the git layer (R7 root cause).
//! * now, `sync` / `sync-to` write `.rwv-op` + every touched-workspace lease
//!   with `O_CREAT|O_EXCL` at guard time; exactly one caller wins.
//!
//! Coverage in this file:
//!
//! * **In-flight refusal**: a second `rwv sync` against a workweave whose
//!   `.rwv-op` is held by a real parked op gets the in-flight refusal and
//!   leaves the holder's record untouched. Constructed deterministically (a
//!   rebase conflict parks the first op mid-replay); the *concurrent* half of
//!   the mutex — two racers, one winner — is pinned load-invariantly by the
//!   in-process unit tests `op_state::tests::
//!   acquire_op_is_atomic_under_concurrent_racers` and `durable_file::tests::
//!   concurrent_create_new_has_exactly_one_winner`.
//! * **Precondition-refusal cleanup**: acquire → fail a precondition after
//!   acquisition → the acquired records are cleared (cleanup table row
//!   "precondition refusal → cleared everywhere"). Verified by observing that
//!   a follow-up `rwv sync` doesn't get an in-flight refusal.
//! * **Doctor dead-lease detection**: a `.rwv-op-lease` whose recorded owner
//!   has no matching `.rwv-op` is reported by `rwv doctor` and removed by
//!   `rwv doctor --fix`. Structural check — no wall-clock input.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Fixture helpers (adapted from e2e_op_state_test.rs; kept local so this file
// stays self-contained).
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
    // Round-trip through the real parser + `lock::write_lock` (same as
    // e2e_op_state_test.rs). This file's original local copy had drifted to a
    // TOML-ish shape the real JSON parser refuses — every sync in the fixture
    // died at the post-acquire dirt scan ("failed to parse rwv.lock"),
    // release-on-refusal cleared the records, and the racing test this file
    // used to hold was green only when its loser overlapped that brief hold
    // (rwv-g8qb).
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

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

/// Build a workspace holding a server-only project.
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

/// Two workspaces sharing objects via git worktrees (primary + one workweave).
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
// In-flight refusal: a sync against a held workweave refuses and leaves the
// holder intact.
// ---------------------------------------------------------------------------
//
// The property: `.rwv-op` is a mutex — while one sync op holds it, a second
// `rwv sync` at the same workweave must be refused with the operator-facing
// in-flight shape (verb / `--continue` / `rwv abort`), not a raw filesystem
// AlreadyExists and not some downstream git-layer collision, and the holder's
// record must survive the refused attempt byte-for-byte.
//
// The in-flight holder is REAL, not hand-planted JSON: the first `rwv sync`
// is parked mid-replay by a rebase conflict (same recipe as
// e2e_op_state_test.rs test 2), and the cleanup table's "phase failure →
// records kept everywhere" row leaves its `.rwv-op` on disk. The second sync
// then runs against that parked op with no concurrency anywhere — the verdict
// depends only on on-disk artifacts, never on scheduling.
//
// History: this test used to race two `rwv sync` processes released by a
// thread barrier and assert exactly one in-flight refusal. The barrier
// synchronized the parents, not the children's critical sections, so the
// overlap was merely hoped for — under host load the trial collapsed
// (observed both ways: the winner completing and RELEASING before the loser
// arrived, and both racers failing on a torn read) and the assertion went red
// with no defect present (rwv-g8qb). The concurrent half of the property —
// two racers, exactly one winner — is pinned by in-process unit tests whose
// verdicts are load-invariant because the winner's artifact persists (no
// release), so the loser refuses under EVERY interleaving:
// `op_state::tests::acquire_op_is_atomic_under_concurrent_racers` and
// `durable_file::tests::concurrent_create_new_has_exactly_one_winner`.

#[test]
fn sync_against_in_flight_op_refuses_and_leaves_the_holder_intact() {
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

    // ww: a conflicting commit on the same file (plus lock update), so the
    // replay rebase must stop on a conflict.
    let c_ww = make_commit(
        &ww.server_dir,
        "shared.txt",
        "ww version\n",
        "ww: add shared.txt (conflicts with primary)",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww C_ww"]);

    // First sync: acquires `.rwv-op`, then parks on the replay conflict.
    // A phase failure keeps the records (cleanup table), so the op is now
    // in flight on disk — a real holder produced by the production acquire
    // path, exactly what a crashed/conflicted peer leaves behind.
    let assertion = rwv()
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
    let parked_stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    // The park must be the conflict, not an in-flight refusal — nothing was
    // in flight when it started.
    assert!(
        !parked_stderr.contains("in progress (started"),
        "first sync must park on the conflict, not refuse as in-flight; \
         got: {parked_stderr}"
    );
    let op_path = ww.root.join(".rwv-op");
    assert!(
        op_path.exists(),
        "a mid-replay phase failure must keep `.rwv-op` (cleanup table); \
         first sync stderr: {parked_stderr}"
    );
    let holder_record = std::fs::read(&op_path).unwrap();

    // Second sync against the held workweave. Acquisition dominates every
    // other refusal (Correction-1 ordering), so regardless of what state the
    // parked op left the repos in, the refusal MUST be the in-flight shape.
    let assertion = rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let refusal = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert!(
        refusal.contains("in progress")
            && refusal.contains("--continue")
            && refusal.contains("rwv abort"),
        "second sync must see the in-flight refusal with both exits; \
         got: {refusal}"
    );
    // The refusal MUST NOT surface as a raw AlreadyExists / a git-level
    // collision — those would show that acquisition happened too late.
    assert!(
        !refusal.contains("AlreadyExists") && !refusal.contains("File exists"),
        "refusal must be the in-flight shape, not raw filesystem \
         AlreadyExists; got: {refusal}"
    );

    // At most one holder: the refused attempt must not have clobbered,
    // rewritten, or released the parked op's record.
    let after = std::fs::read(&op_path).unwrap();
    assert_eq!(
        holder_record, after,
        "the holder's `.rwv-op` must survive a refused sync byte-for-byte"
    );
}

// ---------------------------------------------------------------------------
// Precondition-refusal cleanup: acquire → precondition refuses → records
// cleared. Verified by re-running sync and observing NO in-flight refusal.
// ---------------------------------------------------------------------------
//
// Scenario: dirty CWD project so that `sync-to`'s dirty-source preflight
// refuses AFTER acquisition (the preflight runs post-acquisition in the new
// ordering). If the release path is broken, the second sync-to attempt would
// see stale op-state and refuse with "in progress" — proving the cleanup
// table's "precondition refusal → cleared everywhere" row is enforced.

#[test]
fn precondition_refusal_after_acquire_clears_op_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Dirty the ww project repo so sync-to's dirty-source preflight refuses.
    std::fs::write(
        ww.project_dir.join("dirty-file.txt"),
        "uncommitted content\n",
    )
    .unwrap();
    common::git_in(&ww.project_dir, &["add", "dirty-file.txt"]);
    // Staged but not committed — dirty-source preflight refuses on tracked dirt.

    // First attempt: sync-to must refuse via the dirty-source preflight, NOT
    // via the in-flight refusal (nothing is in flight yet).
    let assertion = rwv()
        .args(["sync-to", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert!(
        !stderr.contains("in progress (started"),
        "first attempt: refusal must be the dirty-source shape, not in-flight; \
         got: {stderr}"
    );
    // Verify no records were left behind.
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "owner record must not persist after precondition refusal at ww; got: {stderr}"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "target lease must not persist after precondition refusal at primary; \
         got: {stderr}"
    );

    // Second attempt: same shape, same refusal — proves the release path
    // was idempotent and the first attempt didn't strand any records that
    // would now surface as an "in progress" error.
    let assertion = rwv()
        .args(["sync-to", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();
    assert!(
        !stderr.contains("in progress (started"),
        "second attempt: still the dirty-source refusal, no stranded in-flight \
         state; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Doctor: dead-lease detection + --fix.
// ---------------------------------------------------------------------------
//
// Plant a `.rwv-op-lease` at ww whose recorded owner is a workspace with no
// `.rwv-op`. Doctor reports it; `doctor --fix` removes it. Structural check —
// no wall-clock input, no timeout.

#[test]
fn doctor_reports_dead_op_lease_with_missing_owner_record() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // The recorded owner path exists as a directory but has no .rwv-op file.
    // This is the classical crash-between-acquire-and-mark shape (or an owner
    // deletion out-of-band).
    let ghost_owner = tmp.path().join("ghost-owner");
    std::fs::create_dir_all(&ghost_owner).unwrap();

    let lease_json = format!(
        "{{\"id\": \"dead-op-9999\", \"owner\": \"{owner}\", \
         \"created_at\": \"2026-05-27T10:00:00Z\"}}",
        owner = common::json_escaped(&ghost_owner),
    );
    std::fs::write(ww.root.join(".rwv-op-lease"), &lease_json).unwrap();

    let assertion = rwv().arg("doctor").current_dir(&ww.root).assert();
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");

    assert!(
        combined.contains("dead-op-lease"),
        "doctor must report the dead-op-lease finding; got stdout:\n{stdout}\n\
         stderr:\n{stderr}"
    );
    // Per-item detail (the op id) lives in `--json` — the text report
    // collapses reclamation classes to a per-class count line.
    let json_assert = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ww.root)
        .assert();
    let json_stdout = String::from_utf8_lossy(&json_assert.get_output().stdout).to_string();
    assert!(
        json_stdout.contains("dead-op-9999"),
        "doctor --json must carry the op id; got:\n{json_stdout}"
    );
}

#[test]
fn doctor_fix_removes_dead_op_lease() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let ghost_owner = tmp.path().join("ghost-owner-2");
    std::fs::create_dir_all(&ghost_owner).unwrap();
    let lease_json = format!(
        "{{\"id\": \"dead-op-fix\", \"owner\": \"{owner}\", \
         \"created_at\": \"2026-05-27T10:00:00Z\"}}",
        owner = common::json_escaped(&ghost_owner),
    );
    let lease_path = ww.root.join(".rwv-op-lease");
    std::fs::write(&lease_path, &lease_json).unwrap();

    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ww.root)
        .assert();

    assert!(
        !lease_path.exists(),
        "doctor --fix must remove the dead lease at {}",
        lease_path.display()
    );
}

#[test]
fn doctor_leaves_live_lease_alone() {
    // Sanity: a lease PAIRED with a real owner record for the same op id is
    // NOT reported as dead-op-lease (that would be a false positive that
    // could tear down a live in-flight sync-to).
    //
    // The lease sits at the scanned workspace (ww) and points at a separate
    // directory that carries the matching owner record — the doctor
    // resolves the pointer, finds a matching op id, and reports nothing.
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let op_id = "live-op-1234";
    let owner_ws = tmp.path().join("live-owner");
    std::fs::create_dir_all(&owner_ws).unwrap();
    let owner_json = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"replay\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        src = common::json_escaped(&owner_ws),
        tgt = common::json_escaped(&ww.root),
    );
    std::fs::write(owner_ws.join(".rwv-op"), &owner_json).unwrap();

    // Lease at ww with the SAME id → live pairing → dead-lease check must
    // return None.
    let lease_json = format!(
        "{{\"id\": \"{op_id}\", \"owner\": \"{owner}\", \
         \"created_at\": \"2026-05-27T10:00:00Z\"}}",
        owner = common::json_escaped(&owner_ws),
    );
    std::fs::write(ww.root.join(".rwv-op-lease"), &lease_json).unwrap();

    let assertion = rwv().arg("doctor").current_dir(&ww.root).assert();
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");

    assert!(
        !combined.contains("dead-op-lease"),
        "live-paired lease must not be reported as dead; got:\n{combined}"
    );
}

#[test]
fn doctor_reports_dead_op_lease_on_op_id_mismatch() {
    // Owner exists with a DIFFERENT op id — the stale-carry-over case that
    // structurally distinguishes an old lease from an in-flight one.
    //
    // We plant the lease at ww (the workspace doctor scans) and let its
    // owner-pointer reference a separate directory that carries a
    // fresh-op `.rwv-op`. The doctor scan reads the lease, follows the
    // pointer, sees the id mismatch, and reports.
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // A separate directory acting as the "owner workspace" that the lease
    // points at. Not part of the scanned workspace tree — doctor only needs
    // to open its `.rwv-op` file via the pointer, not scan it as a
    // hygiene target.
    let owner_ws = tmp.path().join("other-owner");
    std::fs::create_dir_all(&owner_ws).unwrap();
    let fresh_json = format!(
        "{{\"id\": \"fresh-op\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"replay\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        src = common::json_escaped(&owner_ws),
        tgt = common::json_escaped(&ww.root),
    );
    std::fs::write(owner_ws.join(".rwv-op"), &fresh_json).unwrap();

    // Lease at ww references an OLDER, unrelated op id. Follow the pointer
    // → the owner file exists but has id "fresh-op" → dead by structural
    // mismatch.
    let stale_lease_json = format!(
        "{{\"id\": \"old-op-stranded\", \"owner\": \"{owner}\", \
         \"created_at\": \"2026-05-27T10:00:00Z\"}}",
        owner = common::json_escaped(&owner_ws),
    );
    std::fs::write(ww.root.join(".rwv-op-lease"), &stale_lease_json).unwrap();

    let assertion = rwv().arg("doctor").current_dir(&ww.root).assert();
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");

    assert!(
        combined.contains("dead-op-lease"),
        "doctor must report the id-mismatch dead-op-lease shape; got:\n{combined}"
    );
    // The mismatched op id is per-item detail, carried by `--json` — the
    // text report collapses reclamation classes to a per-class count line.
    let json_assert = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ww.root)
        .assert();
    let json_stdout = String::from_utf8_lossy(&json_assert.get_output().stdout).to_string();
    assert!(
        json_stdout.contains("old-op-stranded"),
        "doctor --json must carry the mismatched op id; got:\n{json_stdout}"
    );
}
