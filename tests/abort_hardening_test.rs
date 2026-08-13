//! E2E acceptance tests for `rwv abort`'s two hardening rails
//! (design § 5):
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
//!
//! ## Lock-fixture audit (rwv-fo3x, 2026-08-12)
//!
//! Pre-fix, `write_lock` here wrote a TOML shape into `rwv.lock`, which is
//! JSON post-v0.17 and refused by the production parser at line 1 col 2.
//! rwv-g8qb's earlier fix flagged this class in `atomic_lease_acquisition_test`
//! (where an unparseable lock made every sync-under-test refuse at the
//! lock-read, letting refusal-shaped assertions pass by coincidence).
//!
//! Applying the same suspicion here: I replaced `write_lock` with garbage
//! (`!!!GARBAGE_NOT_PARSEABLE!!!`) and every one of the 9 tests still passed.
//! Reason: `run_abort` (`src/sync.rs`) uses `Project::from_dir_skip_lock` by
//! design — its comment: "abort's contract is 'the state is bad, get me
//! out'. rwv.lock may contain git conflict markers from the half-completed
//! rebase, so we must not try to parse it." So none of these tests routed
//! through a lock parse pre- or post-fix; each was testing what its name
//! claims. Verdict per test: WAS TESTING WHAT IT NAMES — for every case.
//!
//! The fix therefore does not change any assertion's meaning. It aligns
//! the fixture's on-disk contract with production so future regressions
//! (e.g. a new abort-path preflight that DOES parse the lock) can't be
//! masked by the fixture, and so the file stops shipping malformed content
//! for a file the rest of rwv treats as JSON.

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
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
}

fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    // Round-trip through the real parser + `lock::write_lock` so the on-disk
    // shape matches what `rwv lock` itself would emit. Direct string writes
    // drift silently: pre-v0.17 the lock was TOML and hand-rolled TOML was
    // fine here; post-v0.17 the lock is JSON and the same string is refused
    // by `serde_json` at line 1 column 2. Audit (see file header) confirms
    // `rwv abort` uses `Project::from_dir_skip_lock` and does not parse
    // this file — so the drift never masked test scope here — but the
    // fixture's on-disk contract must still match production's.
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

const SERVER_URL: &str = "https://github.com/chatly/server.git";
const SERVER_PATH: &str = "github/chatly/server";

/// A workspace usable for in-place abort fixtures: one manifest repo
/// (`github/chatly/server`) plus a project repo with `rwv.toml`/`rwv.lock`
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
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    write_manifest(&project_dir, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &server_sha)]);
    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
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
/// phase, and per-repo converged_tips map (key → SHA). Mirrors the JSON
/// `rwv abort` reads.
fn plant_owner_record(workspace: &Path, op_id: &str, phase: &str, converged_tips: &[(&str, &str)]) {
    let tips_json = converged_tips
        .iter()
        .map(|(key, sha)| format!("\"{key}\": \"{sha}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let json = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"sync\", \"strategy\": \"rebase\", \
         \"source\": \"{root}\", \"target\": \"{root}\", \"retire\": false, \"phase\": \"{phase}\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{{tips_json}}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        root = workspace.display(),
    );
    std::fs::write(workspace.join(".rwv-op"), &json).unwrap();
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

/// A workspace with `repo_paths.len()` manifest repos instead of
/// [`make_fixture`]'s one, for the cases that must show consent given for one
/// repo doing nothing to another.
struct MultiRepoFixture {
    root: PathBuf,
    project_dir: PathBuf,
    /// Manifest repo directories, in the order `repo_paths` named them.
    repo_dirs: Vec<PathBuf>,
}

fn make_multi_repo_fixture(parent: &Path, name: &str, repo_paths: &[&str]) -> MultiRepoFixture {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let mut repo_dirs = Vec::new();
    let mut shas = Vec::new();
    for path in repo_paths {
        let dir = root.join(path);
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        shas.push(init_repo(&dir));
        repo_dirs.push(dir);
    }

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();

    let urls: Vec<String> = repo_paths
        .iter()
        .map(|p| format!("https://example.invalid/{p}.git"))
        .collect();
    let manifest_rows: Vec<(&str, &str)> = repo_paths
        .iter()
        .zip(urls.iter())
        .map(|(p, u)| (*p, u.as_str()))
        .collect();
    write_manifest(&project_dir, &manifest_rows);
    let lock_rows: Vec<(&str, &str, &str)> = repo_paths
        .iter()
        .zip(urls.iter())
        .zip(shas.iter())
        .map(|((p, u), s)| (*p, u.as_str(), s.as_str()))
        .collect();
    write_lock(&project_dir, &lock_rows);
    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
        &project_dir,
    );
    git(&["commit", "-m", "lock: initial"], &project_dir);
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    MultiRepoFixture {
        root,
        project_dir,
        repo_dirs,
    }
}

// ---------------------------------------------------------------------------
// Case 1: Untouched — repo tip == savepoint
// ---------------------------------------------------------------------------

/// When the repo's tip never moved, abort succeeds and the pre-abort ref
/// records the (unchanged) tip. Verifies the information-preserving rail
/// runs even on the no-op restore path.
#[test]
fn abort_untouched_is_noop_and_writes_pre_abort_ref() {
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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

// ---------------------------------------------------------------------------
// Legible refusal output
// ---------------------------------------------------------------------------

/// When repos are skipped (no savepoint) or untouched, they must NOT produce
/// one line each — instead a single aggregate "summary:" line is emitted on
/// stdout. This avoids the 90-line noise seen in the motivating incident.
#[test]
fn abort_noise_collapses_to_summary_line() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000010Z";
    // Plant ONLY a project savepoint; server has no savepoint → NoSavepoint.
    // Project tip == savepoint → Untouched.
    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "replay", &[]);

    let assert = rwv().arg("abort").current_dir(&ws.root).assert().success();

    // A "summary:" line must appear somewhere in stdout.
    assert.stdout(predicate::str::contains("summary:"));
}

/// When a foreign-tip refusal occurs, the recovery-options block must appear
/// EXACTLY ONCE in stderr, not once per refused repo. Two repos both foreign
/// → single options block.
#[test]
fn abort_foreign_tip_options_block_printed_once() {
    let tmp = common::tempdir().unwrap();

    // Build a fixture with TWO manifest repos to exercise the "multiple
    // refusals, options block once" path.
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(root.join("github/chatly2")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server1_dir = root.join("github/chatly/server");
    let server1_sha = init_repo(&server1_dir);
    let server2_dir = root.join("github/chatly2/server");
    let server2_sha = init_repo(&server2_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    // Manifest is TOML — hand-rolled shape is fine. Lock is JSON — route it
    // through `write_lock` (round-trips via the production parser) so the
    // fixture's on-disk shape matches production.
    write_manifest(
        &project_dir,
        &[
            (
                "github/chatly/server",
                "https://github.com/chatly/server.git",
            ),
            (
                "github/chatly2/server",
                "https://github.com/chatly2/server.git",
            ),
        ],
    );
    write_lock(
        &project_dir,
        &[
            (
                "github/chatly/server",
                "https://github.com/chatly/server.git",
                &server1_sha,
            ),
            (
                "github/chatly2/server",
                "https://github.com/chatly2/server.git",
                &server2_sha,
            ),
        ],
    );
    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
        &project_dir,
    );
    git(&["commit", "-m", "lock: initial"], &project_dir);
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    let op_id = "20991231T000011Z";

    // Advance both repos to foreign tips.
    let s1_savepoint = server1_sha.clone();
    make_commit(&server1_dir, "foreign.txt", "foreign\n", "foreign: s1");
    plant_savepoint(&server1_dir, op_id, &s1_savepoint);

    let s2_savepoint = server2_sha.clone();
    make_commit(&server2_dir, "foreign.txt", "foreign\n", "foreign: s2");
    plant_savepoint(&server2_dir, op_id, &s2_savepoint);

    let project_savepoint = git_out(&["rev-parse", "HEAD"], &project_dir);
    plant_savepoint(&project_dir, op_id, &project_savepoint);

    plant_owner_record(&root, op_id, "replay", &[]);

    let output = rwv()
        .arg("abort")
        .current_dir(&root)
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr);

    // "recovery options" must appear exactly once.
    let occurrence_count = stderr.matches("recovery options").count();
    assert_eq!(
        occurrence_count, 1,
        "recovery options block must appear exactly once in stderr, found {occurrence_count} time(s).\nstderr:\n{stderr}"
    );

    // Both repos must be mentioned (one line per refused repo).
    assert!(
        stderr.contains("github/chatly/server"),
        "stderr must name first refused repo"
    );
    assert!(
        stderr.contains("github/chatly2/server"),
        "stderr must name second refused repo"
    );
}

/// When a foreign-tip refusal occurs, the output must include the blocking
/// commits (savepoint..tip) inline for each refused repo.
#[test]
fn abort_foreign_tip_shows_blocking_commits() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000012Z";

    let server_savepoint = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    // Make two identifiable commits on the server.
    make_commit(
        &ws.server_dir,
        "a.txt",
        "a\n",
        "chore: upstream-main-advance-1",
    );
    make_commit(
        &ws.server_dir,
        "b.txt",
        "b\n",
        "chore: upstream-main-advance-2",
    );
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "replay", &[]);

    let output = rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Commit subjects from the blocking range should appear in stderr.
    assert!(
        stderr.contains("upstream-main-advance-1") || stderr.contains("upstream-main-advance-2"),
        "blocking commits must appear in stderr.\nstderr:\n{stderr}"
    );

    // Shape text should indicate strictly-ahead (2 commits ahead, 0 behind).
    assert!(
        stderr.contains("ahead"),
        "shape description must appear in stderr.\nstderr:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// `--abandon-foreign-tip`: the named, per-repo waiver of rail 2
// ---------------------------------------------------------------------------

/// The flag's happy path, and the ordering proof that makes "abandon" the
/// honest word for it.
///
/// After abort restores over the foreign tip, the pre-abort ref must hold the
/// FOREIGN tip — not the savepoint. That single assertion is what separates
/// abandonment from destruction: rail 1 writing after the reset, or not at
/// all, leaves the ref at the savepoint (or absent) and the foreign commits
/// named by nothing.
#[test]
fn abandon_foreign_tip_restores_and_leaves_the_abandoned_tip_reachable() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000020Z";

    let server_savepoint = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    let foreign_tip = make_commit(
        &ws.server_dir,
        "foreign.txt",
        "foreign\n",
        "foreign: another agent advanced this branch",
    );
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "relock", &[]);

    rwv()
        .arg("abort")
        .arg(format!("--abandon-foreign-tip={SERVER_PATH}"))
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("abandoned foreign tip"));

    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ws.server_dir),
        server_savepoint,
        "consent must let the restore proceed to the savepoint"
    );

    // The ordering pin. Rail 1 runs before the move, so the ref names the tip
    // the branch was moved OFF; a ref holding `server_savepoint` here would
    // mean the capture happened after the reset and the foreign commit is
    // unreachable.
    assert_eq!(
        pre_abort_ref_sha(&ws.server_dir, op_id).as_deref(),
        Some(foreign_tip.as_str()),
        "the abandoned tip must stay reachable through the pre-abort ref"
    );
    assert!(
        try_git(
            &["cat-file", "-e", &format!("{foreign_tip}^{{commit}}")],
            &ws.server_dir
        ),
        "the abandoned commit object must survive the abort"
    );

    // A resolved repo is not a refusal: op-state clears and the savepoint goes.
    assert!(
        !ws.root.join(".rwv-op").exists(),
        "op-state must clear once every repo is resolved"
    );
    assert_eq!(
        savepoint_sha(&ws.server_dir, op_id),
        None,
        "the savepoint is consumed by a completed restore"
    );
}

/// Per-repo means per-repo: consent naming one repo must not reach another
/// whose tip is foreign in exactly the same way. The unnamed repo keeps its
/// tip, abort still fails, and op-state is still retained.
///
/// This is the pin for "no bare all-repos form" at the level that matters —
/// not the absence of a spelling, but the absence of the behaviour.
#[test]
fn abandon_consent_does_not_reach_a_repo_it_did_not_name() {
    let tmp = common::tempdir().unwrap();
    let named_path = "github/chatly/server";
    let unnamed_path = "github/chatly2/server";
    let fx = make_multi_repo_fixture(tmp.path(), "ws", &[named_path, unnamed_path]);
    let (named_dir, unnamed_dir) = (&fx.repo_dirs[0], &fx.repo_dirs[1]);

    let op_id = "20991231T000021Z";

    let named_savepoint = git_out(&["rev-parse", "HEAD"], named_dir);
    make_commit(named_dir, "foreign.txt", "foreign\n", "foreign: named repo");
    plant_savepoint(named_dir, op_id, &named_savepoint);

    let unnamed_savepoint = git_out(&["rev-parse", "HEAD"], unnamed_dir);
    let unnamed_foreign = make_commit(
        unnamed_dir,
        "foreign.txt",
        "foreign\n",
        "foreign: unnamed repo",
    );
    plant_savepoint(unnamed_dir, op_id, &unnamed_savepoint);

    let project_savepoint = git_out(&["rev-parse", "HEAD"], &fx.project_dir);
    plant_savepoint(&fx.project_dir, op_id, &project_savepoint);
    plant_owner_record(&fx.root, op_id, "relock", &[]);

    let output = rwv()
        .arg("abort")
        .arg(format!("--abandon-foreign-tip={named_path}"))
        .current_dir(&fx.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        git_out(&["rev-parse", "HEAD"], named_dir),
        named_savepoint,
        "the named repo must be restored"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], unnamed_dir),
        unnamed_foreign,
        "consent for one repo must not move another repo's branch"
    );
    assert!(
        stderr.contains(unnamed_path) && stderr.contains("foreign-tip"),
        "the unnamed repo must still be refused.\nstderr:\n{stderr}"
    );
    assert!(
        fx.root.join(".rwv-op").exists(),
        "op-state must be retained while any repo is still refused"
    );
}

/// The flag cannot be spelled as a blanket. Its bare form is a parse error
/// (clap requires the value), and the two obvious all-repos spellings do not
/// exist.
#[test]
fn abandon_foreign_tip_has_no_all_repos_spelling() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    // Bare: no repo named, so nothing is consented to.
    rwv()
        .arg("abort")
        .arg("--abandon-foreign-tip")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("--abandon-foreign-tip"));

    for blanket in ["--abandon-foreign-tips", "--abandon-all-foreign-tips"] {
        rwv()
            .arg("abort")
            .arg(blanket)
            .current_dir(&ws.root)
            .assert()
            .failure()
            .stderr(predicate::str::contains("unexpected argument"));
    }
}

/// The guard that keeps the consent from becoming destruction.
///
/// `create_pre_abort_ref` is first-write-wins, so a second abort run gets back
/// the ref an earlier run wrote. If the branch has advanced since, that ref no
/// longer names the tip about to be left behind — restoring would strand the
/// commits in between with nothing pointing at them. Consent does not buy
/// that, so abort refuses and says why.
#[test]
fn abandon_refuses_when_the_pre_abort_ref_no_longer_holds_the_observed_tip() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000022Z";

    let server_savepoint = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    let first_foreign = make_commit(&ws.server_dir, "f1.txt", "f1\n", "foreign: first");
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "relock", &[]);

    // Run 1, no consent: refuses and captures the tip as it stands.
    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("foreign-tip"));
    assert_eq!(
        pre_abort_ref_sha(&ws.server_dir, op_id).as_deref(),
        Some(first_foreign.as_str()),
    );

    // The foreign agent keeps going.
    let second_foreign = make_commit(&ws.server_dir, "f2.txt", "f2\n", "foreign: second");

    // Run 2, with consent: the ref still holds the FIRST tip, so restoring
    // would lose the second. Refuse.
    let output = rwv()
        .arg("abort")
        .arg(format!("--abandon-foreign-tip={SERVER_PATH}"))
        .current_dir(&ws.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ws.server_dir),
        second_foreign,
        "a stale capture must not be treated as consent to destroy the tip past it"
    );
    assert_eq!(
        pre_abort_ref_sha(&ws.server_dir, op_id).as_deref(),
        Some(first_foreign.as_str()),
        "first-write-wins still holds — the refusal must not rewrite the capture"
    );
    // "It refused" is not the claim. The claim is that it refused FOR THIS
    // REASON and said which tip the reference actually holds — without that
    // SHA the operator cannot tell which commits are already safe, and a
    // message naming the observed tip instead would read as if nothing were
    // wrong. So the assertion is on the line, not on the transcript: the
    // whole-stderr `contains` it replaces was green for any SHA at all.
    let consent_line = stderr
        .lines()
        .find(|line| line.contains("--abandon-foreign-tip named this repo"))
        .unwrap_or_else(|| {
            panic!("a refusal that arrives despite consent must say why.\nstderr:\n{stderr}")
        });
    assert!(
        consent_line.contains(&first_foreign),
        "the refusal must name the stale capture ({first_foreign}) as the cause, \
         not merely report a refusal.\nconsent line: {consent_line}"
    );
    assert!(
        ws.root.join(".rwv-op").exists(),
        "op-state must be retained on this refusal like any other"
    );
}

/// The project repo is reachable through the flag under the same key abort
/// prints for it — the one repo whose spelling an operator would otherwise
/// have to guess, since it is not a path.
#[test]
fn abandon_foreign_tip_accepts_the_project_repo_key() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000023Z";

    let server_savepoint = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);
    let project_foreign = make_commit(&ws.project_dir, "foreign.txt", "foreign\n", "foreign: proj");
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "relock", &[]);

    rwv()
        .arg("abort")
        .arg("--abandon-foreign-tip=(project)")
        .current_dir(&ws.root)
        .assert()
        .success();

    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ws.project_dir),
        project_savepoint,
        "`(project)` must name the project repo"
    );
    assert_eq!(
        pre_abort_ref_sha(&ws.project_dir, op_id).as_deref(),
        Some(project_foreign.as_str()),
        "the abandoned project-repo tip must stay reachable"
    );
}

/// The gap this flag closes: the refusal used to hand the operator a raw
/// `git update-ref` because no verb did the job. It must now name the verb,
/// and must not teach the raw command as the way through.
#[test]
fn foreign_tip_recovery_options_name_the_flag_not_raw_git() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000024Z";

    let server_savepoint = git_out(&["rev-parse", "HEAD"], &ws.server_dir);
    make_commit(&ws.server_dir, "foreign.txt", "foreign\n", "foreign: agent");
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = git_out(&["rev-parse", "HEAD"], &ws.project_dir);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "relock", &[]);

    let output = rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("rwv abort --abandon-foreign-tip=<repo>"),
        "the recovery options must name the verb that does this.\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("git update-ref"),
        "the raw command must no longer be the documented path.\nstderr:\n{stderr}"
    );
}
