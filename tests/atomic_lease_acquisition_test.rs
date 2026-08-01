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
//! * **Race**: two concurrent `rwv sync` invocations against the same workweave
//!   — exactly one succeeds, the loser gets the in-flight refusal.
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

/// Two workspaces sharing objects via git worktrees (primary + one workweave).
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
// Race: two concurrent syncs — exactly one wins, the other sees in-flight.
// ---------------------------------------------------------------------------
//
// Runs both `rwv sync` invocations from separate threads coordinated by a
// barrier. The winner may complete successfully OR fail on some downstream
// git conflict (fine); the loser MUST see the in-flight refusal from the
// atomic-acquisition path — not a raw AlreadyExists, not a lock-relation
// refusal, not a savepoint-collision. Trials are repeated to expose
// ordering flakiness that would otherwise slip past.

#[test]
fn concurrent_sync_atomic_acquire_yields_exactly_one_in_flight_refusal() {
    for trial in 0..3 {
        let tmp = common::tempdir().unwrap();
        let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

        // Give primary an advance so `sync` has real work to do (Phase 2).
        let c2 = make_commit(
            &primary.server_dir,
            &format!("advance-{trial}.txt"),
            "advance\n",
            "primary: advance",
        );
        write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
        git(&["add", "rwv.lock"], &primary.project_dir);
        git(&["commit", "-m", "lock: advance"], &primary.project_dir);

        let ww_root_a = ww.root.clone();
        let primary_root_a = primary.root.clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let barrier_a = barrier.clone();

        let h1 = std::thread::spawn(move || {
            barrier_a.wait();
            rwv()
                .args(["sync", &primary_root_a.to_string_lossy()])
                .current_dir(&ww_root_a)
                .assert()
                .try_success()
                .map(|a| String::from_utf8_lossy(&a.get_output().stderr).to_string())
                .map_err(|e| e.to_string())
        });

        let ww_root_b = ww.root.clone();
        let primary_root_b = primary.root.clone();
        let barrier_b = barrier.clone();
        let h2 = std::thread::spawn(move || {
            barrier_b.wait();
            rwv()
                .args(["sync", &primary_root_b.to_string_lossy()])
                .current_dir(&ww_root_b)
                .assert()
                .try_success()
                .map(|a| String::from_utf8_lossy(&a.get_output().stderr).to_string())
                .map_err(|e| e.to_string())
        });

        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();

        // Collect stderrs: successful runs surface via Ok(stderr); failing runs
        // via Err(assertion display which includes stderr). We only need to
        // check the failure carries the in-flight refusal shape.
        let stderrs = [
            match &r1 {
                Ok(s) => s.clone(),
                Err(e) => e.clone(),
            },
            match &r2 {
                Ok(s) => s.clone(),
                Err(e) => e.clone(),
            },
        ];

        let in_flight_hits = stderrs
            .iter()
            .filter(|s| {
                s.contains("in progress") && s.contains("--continue") && s.contains("rwv abort")
            })
            .count();

        // Exactly one of the two racers must have hit the in-flight refusal.
        // (Zero would mean the TOCTOU is back; both would mean somehow both
        // lost, which shouldn't happen because there is nothing else to race
        // with here.)
        assert_eq!(
            in_flight_hits, 1,
            "trial {trial}: exactly one racer must see the in-flight refusal.\n\
             r1: {r1:?}\nr2: {r2:?}"
        );

        // The refusal MUST NOT surface as a raw AlreadyExists / a git-level
        // collision — those would show that acquisition happened too late.
        for s in &stderrs {
            assert!(
                !s.contains("AlreadyExists") && !s.contains("File exists"),
                "trial {trial}: refusal must be the in-flight shape, not raw \
                 filesystem AlreadyExists; got: {s}"
            );
        }
    }
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
    git(&["add", "dirty-file.txt"], &ww.project_dir);
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
        owner = ghost_owner.display(),
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
    assert!(
        combined.contains("dead-op-9999"),
        "doctor must include the op id; got:\n{combined}"
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
        owner = ghost_owner.display(),
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
        "{{\"id\": \"{op_id}\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"replay\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        src = owner_ws.display(),
        tgt = ww.root.display(),
    );
    std::fs::write(owner_ws.join(".rwv-op"), &owner_json).unwrap();

    // Lease at ww with the SAME id → live pairing → dead-lease check must
    // return None.
    let lease_json = format!(
        "{{\"id\": \"{op_id}\", \"owner\": \"{owner}\", \
         \"created_at\": \"2026-05-27T10:00:00Z\"}}",
        owner = owner_ws.display(),
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
        "{{\"id\": \"fresh-op\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"replay\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        src = owner_ws.display(),
        tgt = ww.root.display(),
    );
    std::fs::write(owner_ws.join(".rwv-op"), &fresh_json).unwrap();

    // Lease at ww references an OLDER, unrelated op id. Follow the pointer
    // → the owner file exists but has id "fresh-op" → dead by structural
    // mismatch.
    let stale_lease_json = format!(
        "{{\"id\": \"old-op-stranded\", \"owner\": \"{owner}\", \
         \"created_at\": \"2026-05-27T10:00:00Z\"}}",
        owner = owner_ws.display(),
    );
    std::fs::write(ww.root.join(".rwv-op-lease"), &stale_lease_json).unwrap();

    let assertion = rwv().arg("doctor").current_dir(&ww.root).assert();
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");

    assert!(
        combined.contains("dead-op-lease") && combined.contains("old-op-stranded"),
        "doctor must report the id-mismatch dead-op-lease shape; got:\n{combined}"
    );
}
