//! E2E acceptance tests for `rwv abort`'s two hardening rails
//! (design § 5, fo-jsbr3i.4):
//!
//! 1. **Pre-abort reference**: a durable `refs/rwv/pre-abort/<op-id>` ref
//!    is written at every restored repo's tip BEFORE the restore happens,
//!    and is NOT removed by abort's cleanup. Information-preserving:
//!    abort itself is undoable.
//! 2. **HEAD-verified restore**: `reset --hard` is gated on the tip being
//!    attributable to the op — equal to the savepoint, equal to the
//!    recorded converged tip, or the repo is in a VCS-native mid-op
//!    state. Anything else (foreign commits landed after a crash, or
//!    another agent built on an advanced target) is REPORTED with a named
//!    `foreign-tip` violation and recovery hints, never reset.
//!
//! Each case manufactures the op-state file and savepoint by hand (the
//! same shape `phase_reentry_test.rs` and the conflict-marker abort test
//! use) so the unit under test is the abort path itself, not whatever
//! the surrounding phase machine happened to produce.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Helpers (kept local — mirroring e2e_sync_abort_test.rs style)
// ---------------------------------------------------------------------------

fn rwv() -> AssertCommand {
    common::rwv()
}

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

fn try_git(args: &[&str], dir: &Path) -> bool {
    common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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

const SERVER_URL: &str = "https://github.com/chatly/server.git";
const SERVER_PATH: &str = "github/chatly/server";

/// A workspace usable for in-place abort fixtures: one manifest repo
/// (`github/chatly/server`) plus a project repo with `rwv.yaml`/`rwv.lock`
/// committed. Mirrors `make_locked_workspace` in `e2e_sync_abort_test.rs`
/// but kept local so this file's contract is self-contained.
struct Fixture {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

fn make_fixture(parent: &Path, name: &str) -> Fixture {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server_dir = root.join(SERVER_PATH);
    let server_sha = init_repo(&server_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();
    write_manifest(&project_dir, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &server_sha)]);
    git(
        &["add", ".gitattributes", "rwv.yaml", "rwv.lock"],
        &project_dir,
    );
    git(&["commit", "-m", "lock: initial"], &project_dir);

    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    Fixture {
        root,
        project_dir,
        server_dir,
    }
}

/// Plant a v2 owner record at the workspace root with the given op-id,
/// phase, and per-repo converged_tips map (key → SHA). Mirrors the YAML
/// `rwv abort` reads.
fn plant_owner_record(workspace: &Path, op_id: &str, phase: &str, converged_tips: &[(&str, &str)]) {
    let tips_yaml = if converged_tips.is_empty() {
        "{}\n".to_string()
    } else {
        let mut s = String::from("\n");
        for (key, sha) in converged_tips {
            s.push_str(&format!("  \"{key}\": \"{sha}\"\n"));
        }
        s
    };
    let yaml = format!(
        "id: \"{op_id}\"\nverb: sync\nstrategy: rebase\nsource: \"{root}\"\ntarget: \"{root}\"\nretire: false\nphase: {phase}\nconverged_tips: {tips_yaml}overrides: []\nstarted_at: \"2026-05-27T10:00:00Z\"\n",
        root = workspace.display(),
    );
    std::fs::write(workspace.join(".rwv-op"), &yaml).unwrap();
}

/// Create a `refs/rwv/pre-op/<op-id>` savepoint pointing at `sha` in `repo`.
fn plant_savepoint(repo: &Path, op_id: &str, sha: &str) {
    git(
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), sha],
        repo,
    );
}

fn pre_abort_ref_path(op_id: &str) -> String {
    format!("refs/rwv/pre-abort/{op_id}")
}

fn pre_abort_ref_sha(repo: &Path, op_id: &str) -> Option<String> {
    let out = common::git()
        .args(["rev-parse", "--verify", &pre_abort_ref_path(op_id)])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

fn savepoint_sha(repo: &Path, op_id: &str) -> Option<String> {
    let out = common::git()
        .args(["rev-parse", "--verify", &format!("refs/rwv/pre-op/{op_id}")])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

// ---------------------------------------------------------------------------
// Case 1: Untouched — repo tip == savepoint
// ---------------------------------------------------------------------------

/// When the repo's tip never moved, abort succeeds and the pre-abort ref
/// records the (unchanged) tip. Verifies the information-preserving rail
/// runs even on the no-op restore path.
#[test]
fn abort_untouched_is_noop_and_writes_pre_abort_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000001Z";
    let server_pre = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    let project_pre = git_out(&["rev-parse", "HEAD"], &ws.project_dir);

    plant_savepoint(&ws.server_dir, op_id, &server_pre);
    plant_savepoint(&ws.project_dir, op_id, &project_pre);
    plant_owner_record(&ws.root, op_id, "replay", &[]);

    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("untouched"));

    // Tips unchanged.
    assert_eq!(git_out(&["rev-parse", "HEAD"], &ws.server_dir), server_pre);
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ws.project_dir),
        project_pre
    );
    // Pre-abort refs written for both repos at the (unchanged) tip.
    assert_eq!(
        pre_abort_ref_sha(&ws.server_dir, op_id).as_deref(),
        Some(server_pre.as_str()),
        "pre-abort ref should record server's pre-abort tip"
    );
    assert_eq!(
        pre_abort_ref_sha(&ws.project_dir, op_id).as_deref(),
        Some(project_pre.as_str()),
        "pre-abort ref should record project's pre-abort tip"
    );
    // Op-state cleared on clean abort.
    assert!(!ws.root.join(".rwv-op").exists());
}

// ---------------------------------------------------------------------------
// Case 2: Converged — repo tip == recorded converged tip
// ---------------------------------------------------------------------------

/// When the repo's tip is the recorded converged tip (relock completed),
/// abort resets back to the savepoint and the pre-abort ref preserves the
/// converged tip. This is the "post-replay crash" path.
#[test]
fn abort_converged_resets_to_savepoint_and_preserves_converged_tip() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000002Z";

    // Savepoint = original tip; advance the repo to a "converged" tip.
    let server_savepoint = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    let server_converged = make_commit(
        &ws.server_dir,
        "converged.txt",
        "converged\n",
        "server: converged",
    );

    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);

    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(
        &ws.root,
        op_id,
        "relock",
        &[(SERVER_PATH, &server_converged)],
    );

    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("restored"));

    // Server is back at the savepoint.
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ws.server_dir),
        server_savepoint
    );
    // Project never moved; abort is a no-op there.
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ws.project_dir),
        project_savepoint
    );
    // Pre-abort ref preserved the converged tip on the server repo.
    assert_eq!(
        pre_abort_ref_sha(&ws.server_dir, op_id).as_deref(),
        Some(server_converged.as_str()),
        "pre-abort ref should preserve the converged tip on the restored repo"
    );
    // Savepoint dropped after restore (matches restore_savepoint contract).
    assert!(
        savepoint_sha(&ws.server_dir, op_id).is_none(),
        "savepoint should be dropped after a successful restore"
    );
}

// ---------------------------------------------------------------------------
// Case 3: Mid-op — repo is in a VCS-native rebase
// ---------------------------------------------------------------------------

/// When the repo is mid-rebase (VCS-native wreckage), abort cancels the
/// rebase + resets to the savepoint. The pre-abort ref captures the tip
/// observed at abort time (which for mid-rebase is the "onto" commit, not
/// the savepoint).
#[test]
fn abort_mid_op_cancels_and_resets_to_savepoint() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000003Z";

    // Savepoint at original HEAD; manufacture a mid-rebase wreckage.
    let server_savepoint = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    // Two diverging commits that touch the same line so the rebase stalls.
    make_commit(
        &ws.server_dir,
        "conflict.txt",
        "main version\n",
        "main: conflict base",
    );
    git(
        &["checkout", "-b", "diverge", &server_savepoint],
        &ws.server_dir,
    );
    make_commit(
        &ws.server_dir,
        "conflict.txt",
        "diverge version\n",
        "diverge: conflict",
    );
    git(&["checkout", "main"], &ws.server_dir);
    let _ = std::process::Command::new("git")
        .args(["rebase", "diverge"])
        .current_dir(&ws.server_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output();
    assert!(
        ws.server_dir.join(".git/rebase-merge").exists()
            || ws.server_dir.join(".git/rebase-apply").exists(),
        "expected mid-rebase state on the server repo"
    );

    // Project repo: untouched.
    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);

    plant_owner_record(&ws.root, op_id, "replay", &[]);

    rwv().arg("abort").current_dir(&ws.root).assert().success();

    // Rebase state gone.
    assert!(!ws.server_dir.join(".git/rebase-merge").exists());
    assert!(!ws.server_dir.join(".git/rebase-apply").exists());
    // Server back at the savepoint.
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ws.server_dir),
        server_savepoint
    );
    // Pre-abort ref written for the server (the mid-op HEAD is "main" =
    // the post-rebase-attempt tip — what matters is that the ref EXISTS,
    // not its exact value, since mid-op semantics vary by git version).
    assert!(
        pre_abort_ref_sha(&ws.server_dir, op_id).is_some(),
        "pre-abort ref must exist after a mid-op restore"
    );
}

// ---------------------------------------------------------------------------
// Case 4: Foreign — repo tip diverged off the attributable set
// ---------------------------------------------------------------------------

/// When the repo's tip is neither the savepoint, nor the recorded
/// converged tip, nor a mid-op state, abort REFUSES to reset that repo.
///
/// Acceptance:
/// - exit nonzero
/// - violation named in output (`foreign-tip`)
/// - the foreign repo's tip is UNCHANGED
/// - the pre-abort ref is present (recording the foreign tip for recovery)
/// - the pre-abort ref name is surfaced in the refusal so the operator
///   can locate the captured tip
/// - the savepoint ref is NOT dropped (so re-running abort after manual
///   reconciliation can still find it)
/// - op-state is RETAINED so the operator can re-run abort
#[test]
fn abort_foreign_tip_refuses_and_preserves_state() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000004Z";

    // Server: savepoint at original HEAD; advance to a "foreign" tip
    // distinct from any recorded converged tip.
    let server_savepoint = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    let foreign_tip = make_commit(
        &ws.server_dir,
        "foreign.txt",
        "foreign commit\n",
        "foreign: someone else built on this",
    );
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    // Project: untouched (so abort succeeds there in isolation).
    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);

    // Record an unrelated converged tip so `recorded_converged_tip` is
    // Some(_) but does NOT match the foreign tip — exercises the "have
    // converged data, still foreign" branch.
    let recorded_converged = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    plant_owner_record(
        &ws.root,
        op_id,
        "relock",
        &[(SERVER_PATH, recorded_converged)],
    );

    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("foreign-tip"))
        .stderr(predicate::str::contains(SERVER_PATH))
        .stderr(predicate::str::contains(pre_abort_ref_path(op_id)));

    // Foreign repo's tip is UNCHANGED.
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ws.server_dir),
        foreign_tip,
        "foreign-tip refusal must not reset the repo"
    );
    // Pre-abort ref present and points at the foreign tip.
    assert_eq!(
        pre_abort_ref_sha(&ws.server_dir, op_id).as_deref(),
        Some(foreign_tip.as_str()),
        "pre-abort ref must preserve the observed foreign tip"
    );
    // Savepoint NOT dropped on a foreign refusal.
    assert_eq!(
        savepoint_sha(&ws.server_dir, op_id).as_deref(),
        Some(server_savepoint.as_str()),
        "savepoint must be retained when restore is refused so re-run is possible"
    );
    // Op-state retained so the operator can re-run abort after reconciling.
    assert!(
        ws.root.join(".rwv-op").exists(),
        "op-state must be retained on foreign-tip refusal"
    );
}

/// First-write-wins: re-running abort for the same op must not overwrite the
/// original pre-abort capture. Scenario: abort refuses (foreign tip), the
/// operator moves the branch back to the savepoint, abort is re-run and
/// succeeds — the pre-abort ref must still point at the ORIGINAL foreign tip
/// (by then the only reference to it), not the reconciled tip.
#[test]
fn pre_abort_ref_first_write_wins_across_abort_reruns() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000006Z";

    let server_savepoint = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    let foreign_tip = make_commit(
        &ws.server_dir,
        "foreign.txt",
        "foreign commit\n",
        "foreign: someone else built on this",
    );
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);

    plant_owner_record(&ws.root, op_id, "relock", &[]);

    // First abort: refuses on the foreign tip; pre-abort ref captures it.
    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("foreign-tip"));
    assert_eq!(
        pre_abort_ref_sha(&ws.server_dir, op_id).as_deref(),
        Some(foreign_tip.as_str()),
        "first abort run must capture the foreign tip"
    );

    // Operator reconciles: move the branch back to the savepoint.
    git(&["reset", "--hard", &server_savepoint], &ws.server_dir);

    // Second abort: succeeds; pre-abort ref must STILL hold the foreign tip.
    rwv().arg("abort").current_dir(&ws.root).assert().success();
    assert_eq!(
        pre_abort_ref_sha(&ws.server_dir, op_id).as_deref(),
        Some(foreign_tip.as_str()),
        "re-run must not overwrite the original pre-abort capture (first write wins)"
    );
}

// ---------------------------------------------------------------------------
// Cross-cutting: pre-abort refs are written on every clean abort.
// ---------------------------------------------------------------------------

/// Asserts the information-preserving rail unconditionally: after ANY
/// successful abort, every restored repo has a `refs/rwv/pre-abort/<op-id>`
/// reference. The exact tip recorded depends on the case (savepoint for
/// untouched; pre-restore HEAD for converged/mid-op).
#[test]
fn pre_abort_refs_persist_after_clean_abort() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000005Z";
    let server_savepoint = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    let server_converged = make_commit(
        &ws.server_dir,
        "converged.txt",
        "converged\n",
        "server: converged",
    );
    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);

    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(
        &ws.root,
        op_id,
        "relock",
        &[(SERVER_PATH, &server_converged)],
    );

    rwv().arg("abort").current_dir(&ws.root).assert().success();

    // Pre-abort refs exist on BOTH repos (server: restored from converged;
    // project: untouched). Information-preserving doctrine: abort never
    // deletes the only reference to a tip.
    assert!(
        pre_abort_ref_sha(&ws.server_dir, op_id).is_some(),
        "pre-abort ref must persist on the server repo after abort"
    );
    assert!(
        pre_abort_ref_sha(&ws.project_dir, op_id).is_some(),
        "pre-abort ref must persist on the project repo after abort"
    );

    // And the captured tips really do exist as commits (a `git cat-file`
    // sanity check, not just a ref pointer to an unreachable SHA).
    assert!(
        try_git(
            &[
                "cat-file",
                "-e",
                &format!("{}^{{commit}}", server_converged)
            ],
            &ws.server_dir
        ),
        "pre-abort tip must be a reachable commit"
    );
}
