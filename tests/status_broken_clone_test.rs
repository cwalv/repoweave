//! Integration tests for `rwv status` broken-clone states: `[missing]` and
//! `[unreachable]`.
//!
//! Doc claims anchored here:
//!
//!   - When a member clone directory is absent from disk, `rwv status` emits
//!     `relation: "missing"` (not `"no_lock"`), both in human-readable and
//!     `--json` output.
//!
//!   - When the clone directory exists but the locked SHA is not reachable in
//!     the local object store (history rewritten, object pruned), `rwv status`
//!     emits `relation: "unreachable"` (not `"no_lock"`).
//!
//!   - Both states preserve the `lock_sha` field (the raw lock version string)
//!     in `--json` output, so operators can see what SHA / tag was locked.
//!
//!   - `rwv status` exits 0 for both states (status is a read surface; the
//!     `[diverged]`/`[mid-rebase]` precedent does not signal via exit code).
//!
//! Repair-drive tests (standing method rule):
//!
//!   - Detection tests alone let dead advice survive; every detection must be
//!     paired with a test that drives the named repair end-to-end and confirms
//!     status returns to `ok`.
//!
//!   - `[missing]` repair: `rwv fetch` (no SOURCE, in-place re-materialize)
//!     re-clones the absent directory and aligns it to the locked SHA. The
//!     test drives the full loop: status reports `missing` → repair verb runs
//!     → clone is back at the locked SHA → status reports `ok`.
//!
//!   - `[unreachable]` repair: the status.rs doc comment says "Repair: git
//!     fetch / rwv fetch to materialise the missing object."  In-place `rwv
//!     fetch` skips clones whose directory already exists and tries to
//!     `git checkout <lock-sha>` locally — if the SHA is absent from the
//!     local object store (the definition of `unreachable`) that checkout
//!     fails and the repair does NOT restore `ok`.  For `unreachable` the
//!     effective repair is `git fetch` (the git command, pulling the object
//!     from the remote) followed by `git checkout <sha>`.  The test below
//!     drives that path using a `file://` bare remote whose history the
//!     SHA is pruned from the local clone but is still present in the bare.
//!     Because the GC step is environment-sensitive (some CI setups keep
//!     loose objects), the test is skipped when GC did not actually prune
//!     the object — consistent with the approach used in
//!     `status_human_shows_unreachable_not_no_lock_when_sha_gone`.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn init_repo_with_commit(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "--initial-branch=main"]);
    std::fs::write(path.join("README"), "init").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

/// Write a lock file pinning a single repo to the given SHA.
fn write_lock(project_dir: &Path, repo_path: &str, url: &str, sha: &str) {
    // Round-trip through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let raw = format!(
        "{{\"repositories\": {{{repo_path:?}: {{\"type\": \"git\", \"url\": {url:?}, \"version\": {sha:?}}}}}}}"
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
}

/// Build a workspace with one real manifest repo and a committed lock.
///
/// Returns `(workspace_root, repo_abs_path, url, initial_sha)`.
fn make_workspace_with_lock(parent: &Path, project: &str) -> (PathBuf, PathBuf, String, String) {
    let ws = parent.join("ws");
    let repo_path = ws.join("github/org/repo");
    let sha = init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let url = common::file_url(&repo_path);
    let manifest = format!(
        "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    write_lock(&project_dir, "github/org/repo", &url, &sha);

    std::fs::write(ws.join(".rwv-active"), format!("{project}\n")).unwrap();

    (ws, repo_path, url, sha)
}

// ===========================================================================
// 1. Missing clone dir — `[missing]` label, not `[no-lock]`
// ===========================================================================

#[test]
fn status_human_shows_missing_not_no_lock_when_clone_dir_absent() {
    let tmp = common::tempdir().unwrap();
    let (ws, repo_abs, _url, _sha) = make_workspace_with_lock(tmp.path(), "alpha");

    // Remove the clone directory out-of-band.
    std::fs::remove_dir_all(&repo_abs).unwrap();

    let output = rwv()
        .args(["status"])
        .current_dir(&ws)
        .output()
        .expect("rwv status");

    // Status is a read surface — must exit 0 even with a broken clone.
    assert!(
        output.status.success(),
        "status must exit 0 for a missing clone; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("[missing]"),
        "status must show [missing] for an absent clone dir; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("[no-lock]"),
        "status must NOT show [no-lock] for an absent clone dir (lock is fine); got:\n{stdout}"
    );
}

#[test]
fn status_json_relation_is_missing_when_clone_dir_absent() {
    let tmp = common::tempdir().unwrap();
    let (ws, repo_abs, _url, sha) = make_workspace_with_lock(tmp.path(), "alpha");

    // Remove the clone directory out-of-band.
    std::fs::remove_dir_all(&repo_abs).unwrap();

    let output = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json");

    assert!(
        output.status.success(),
        "status --json must exit 0 for a missing clone; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout should parse as JSON ({e}):\n{stdout}"));

    let repos = parsed["repos"].as_array().expect("repos array");
    assert_eq!(repos.len(), 1, "expected one repo entry; got:\n{stdout}");
    let record = &repos[0];

    assert_eq!(
        record["relation"], "missing",
        "relation must be 'missing' when clone dir is absent; got:\n{stdout}"
    );
    assert_ne!(
        record["relation"], "no_lock",
        "relation must NOT be 'no_lock' for a missing clone (lock is fine); got:\n{stdout}"
    );
    // The lock_sha field should carry the pinned SHA so operators can see
    // what the lock was pointing at before the clone was removed.
    let lock_sha = record["lock_sha"].as_str().unwrap_or("");
    assert!(
        !lock_sha.is_empty(),
        "lock_sha must be present even for a missing clone; got:\n{stdout}"
    );
    assert!(
        lock_sha.starts_with(&sha[..8]),
        "lock_sha should match the pinned SHA; got lock_sha={lock_sha}, expected prefix of {sha}"
    );
    // tip must be null (no HEAD to read from an absent clone).
    assert!(
        record["tip"].is_null(),
        "tip must be null when clone is absent; got:\n{stdout}"
    );
}

// ===========================================================================
// 2. Locked SHA unreachable — `[unreachable]` label, not `[no-lock]`
// ===========================================================================

#[test]
fn status_human_shows_unreachable_not_no_lock_when_sha_gone() {
    let tmp = common::tempdir().unwrap();
    let (ws, repo_abs, url, sha) = make_workspace_with_lock(tmp.path(), "alpha");

    // Pin the lock to the initial SHA, then rewrite history so that SHA is
    // no longer reachable (force-push simulation via orphan branch).
    //
    // Strategy: create a new orphan commit on main so the original SHA is no
    // longer reachable from HEAD or any ref, then make it unreachable via
    // `git gc --prune=now`. The locked SHA still refers to the old (now
    // pruned) object — this is the "history rewritten" / SHA-unreachable case.
    //
    // Implementation: checkout an orphan branch, commit a new root, then
    // replace main with that orphan so the old SHA is dangling.
    common::git_in(&repo_abs, &["checkout", "--orphan", "newroot"]);
    std::fs::write(repo_abs.join("README"), "rewritten").unwrap();
    common::git_in(&repo_abs, &["add", "README"]);
    common::git_in(&repo_abs, &["commit", "-m", "rewritten root"]);
    // Move main to the new root (force — so old SHA becomes unreachable).
    common::git_in(&repo_abs, &["branch", "-f", "main", "HEAD"]);
    common::git_in(&repo_abs, &["checkout", "main"]);
    // Prune the old unreferenced object.
    common::git_in(&repo_abs, &["gc", "--prune=now"]);

    // Verify the old SHA is truly gone (sanity check for the test itself).
    let cat_file = common::git()
        .args(["cat-file", "-e", &sha])
        .current_dir(&repo_abs)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git cat-file");
    if cat_file.success() {
        // The unreachable variant is still pinned structurally by the JSON test
        // below, which writes a fabricated bad SHA rather than relying on GC.
        common::report_skip(
            "the object survived GC here, so the unreachable-SHA relation cannot \
             be built — some hosts keep loose objects longer",
        );
        return;
    }

    // Write the lock pinning the old (now unreachable) SHA.
    write_lock(&ws.join("projects/alpha"), "github/org/repo", &url, &sha);

    let output = rwv()
        .args(["status"])
        .current_dir(&ws)
        .output()
        .expect("rwv status");

    assert!(
        output.status.success(),
        "status must exit 0 for an unreachable SHA; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("[unreachable]"),
        "status must show [unreachable] for a clone with an unreachable locked SHA; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("[no-lock]"),
        "status must NOT show [no-lock] when the lock exists but SHA is unreachable; got:\n{stdout}"
    );
}

/// Use a fabricated (nonsense) SHA to test the `unreachable` path without
/// relying on GC removing objects — this is the structurally guaranteed form
/// of the test.
#[test]
fn status_json_relation_is_unreachable_for_fabricated_bad_sha() {
    let tmp = common::tempdir().unwrap();
    let (ws, _repo_abs, url, _sha) = make_workspace_with_lock(tmp.path(), "alpha");

    // Overwrite the lock with a SHA that can never exist on disk.
    let fake_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    write_lock(
        &ws.join("projects/alpha"),
        "github/org/repo",
        &url,
        fake_sha,
    );

    let output = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json");

    assert!(
        output.status.success(),
        "status --json must exit 0 for an unreachable SHA; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout should parse as JSON ({e}):\n{stdout}"));

    let repos = parsed["repos"].as_array().expect("repos array");
    assert_eq!(repos.len(), 1, "expected one repo entry; got:\n{stdout}");
    let record = &repos[0];

    assert_eq!(
        record["relation"], "unreachable",
        "relation must be 'unreachable' when locked SHA is not in object store; got:\n{stdout}"
    );
    assert_ne!(
        record["relation"], "no_lock",
        "relation must NOT be 'no_lock' when the lock exists but SHA is unreachable; got:\n{stdout}"
    );
    // lock_sha must carry the pinned SHA (the one that can't be resolved).
    let lock_sha = record["lock_sha"].as_str().unwrap_or("");
    assert_eq!(
        lock_sha, fake_sha,
        "lock_sha must carry the (unreachable) pinned SHA; got:\n{stdout}"
    );
    // tip must still be present (clone dir exists, HEAD is readable).
    assert!(
        !record["tip"].is_null(),
        "tip must be present when clone dir exists (even with unreachable lock SHA); got:\n{stdout}"
    );
}

// ===========================================================================
// 3. Normal (healthy) state is unaffected
// ===========================================================================

/// Regression guard: a healthy repo still shows `[ok]`, not `[missing]` or
/// `[unreachable]`.
#[test]
fn status_healthy_repo_still_shows_ok() {
    let tmp = common::tempdir().unwrap();
    let (ws, _repo_abs, _url, _sha) = make_workspace_with_lock(tmp.path(), "alpha");

    let output = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json");

    assert!(
        output.status.success(),
        "status must exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout).expect("parseable JSON");
    let repos = parsed["repos"].as_array().expect("repos array");
    let record = &repos[0];

    assert_eq!(
        record["relation"], "ok",
        "a healthy repo must still show relation=ok; got:\n{stdout}"
    );
}

// ===========================================================================
// 4. Repair-drive: [missing] → rwv fetch → [ok]
//
// This test drives the named repair end-to-end:
//   1. Construct a workspace whose manifest repo's clone directory is absent.
//   2. Confirm `rwv status` reports `[missing]`.
//   3. Run the documented repair: `rwv fetch` (no SOURCE — in-place mode).
//   4. Confirm the clone is back at the locked SHA.
//   5. Confirm `rwv status` now reports `[ok]`.
// ===========================================================================

/// Build a workspace backed by a separate bare repo (so the remote stays
/// available after the clone is deleted).  Returns
/// `(workspace_root, project_dir, bare_repo_path, clone_abs_path, locked_sha)`.
fn make_workspace_with_bare_remote(parent: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf, String) {
    // Bare repo acts as the remote — stays alive when the clone is deleted.
    let bare = parent.join("origin.git");
    let status = common::git()
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&bare)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git init --bare");
    assert!(status.success(), "git init --bare failed");

    // Seed the bare repo with an initial commit via a throw-away working clone.
    let tmp_work = common::tempdir().unwrap();
    let work = tmp_work.path().join("work");
    common::git_in(
        tmp_work.path(),
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
    );
    std::fs::write(work.join("README"), "initial\n").unwrap();
    common::git_in(&work, &["config", "user.email", "test@test.com"]);
    common::git_in(&work, &["config", "user.name", "Test"]);
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "initial"]);
    common::git_in(&work, &["push", "origin", "main"]);
    let sha = common::git_in(&work, &["rev-parse", "HEAD"]);

    let ws = parent.join("ws");
    let clone_abs = ws.join("github/org/repo");
    std::fs::create_dir_all(clone_abs.parent().unwrap()).unwrap();

    // Clone the bare into the canonical workspace slot.
    common::git_in(
        ws.parent().unwrap_or(&ws),
        &[
            "clone",
            &bare.to_string_lossy(),
            &clone_abs.to_string_lossy(),
        ],
    );

    // Build project structure.
    let project_dir = ws.join("projects").join("alpha");
    std::fs::create_dir_all(&project_dir).unwrap();

    let url = common::file_url(&bare);
    let manifest = format!(
        "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_dir.join("rwv.toml"), &manifest).unwrap();
    write_lock(&project_dir, "github/org/repo", &url, &sha);

    std::fs::write(ws.join(".rwv-active"), "alpha\n").unwrap();

    (ws, project_dir, bare, clone_abs, sha)
}

/// Repair-drive for the `[missing]` state.
///
/// Follows the shape of `dangling_reference_end_to_end_fetch_repairs_and_doctor_clean`
/// in `fetch_in_place_test.rs` but anchors the test to `rwv status` output
/// rather than `rwv doctor`:
///
///   status [missing] → rwv fetch (in-place) → status [ok], clone at locked SHA
#[test]
fn missing_repair_drive_fetch_in_place_restores_ok() {
    let tmp = common::tempdir().unwrap();
    let (ws, _project_dir, _bare, clone_abs, locked_sha) =
        make_workspace_with_bare_remote(tmp.path());

    // 1. Remove the clone directory to enter the [missing] state.
    std::fs::remove_dir_all(&clone_abs).unwrap();
    assert!(
        !clone_abs.exists(),
        "precondition: clone must be absent for the [missing] state"
    );

    // 2. Confirm rwv status reports [missing].
    let out_before = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json (before repair)");
    assert!(
        out_before.status.success(),
        "status must exit 0 even for a missing clone; stderr: {}",
        String::from_utf8_lossy(&out_before.stderr)
    );
    let stdout_before = String::from_utf8_lossy(&out_before.stdout).into_owned();
    let parsed_before: Value = serde_json::from_str(&stdout_before)
        .unwrap_or_else(|e| panic!("stdout should parse as JSON ({e}):\n{stdout_before}"));
    let repos_before = parsed_before["repos"].as_array().expect("repos array");
    assert_eq!(
        repos_before.len(),
        1,
        "expected one repo; got:\n{stdout_before}"
    );
    assert_eq!(
        repos_before[0]["relation"], "missing",
        "status must report [missing] when clone dir is absent; got:\n{stdout_before}"
    );

    // 3. Run the documented repair: `rwv fetch` (no SOURCE — in-place re-materialize).
    let repair_out = rwv()
        .arg("fetch")
        .current_dir(&ws)
        .output()
        .expect("rwv fetch (in-place)");
    let repair_combined = format!(
        "{}{}",
        String::from_utf8_lossy(&repair_out.stdout),
        String::from_utf8_lossy(&repair_out.stderr)
    );
    assert!(
        repair_out.status.success(),
        "rwv fetch (in-place) must succeed as the repair for [missing]; got:\n{repair_combined}"
    );

    // 4. The clone is back on disk at the locked SHA.
    assert!(
        clone_abs.exists(),
        "repair must re-materialize the missing clone at its canonical path"
    );
    assert!(
        clone_abs.join(".git").exists(),
        "re-materialized path must be a real git clone"
    );
    let head_after_repair = common::git_in(&clone_abs, &["rev-parse", "HEAD"]);
    assert_eq!(
        head_after_repair, locked_sha,
        "repaired clone must be at the LOCKED SHA (not branch HEAD)"
    );

    // 5. rwv status now reports [ok].
    let out_after = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json (after repair)");
    assert!(
        out_after.status.success(),
        "status must exit 0 after repair; stderr: {}",
        String::from_utf8_lossy(&out_after.stderr)
    );
    let stdout_after = String::from_utf8_lossy(&out_after.stdout).into_owned();
    let parsed_after: Value = serde_json::from_str(&stdout_after)
        .unwrap_or_else(|e| panic!("stdout should parse as JSON ({e}):\n{stdout_after}"));
    let repos_after = parsed_after["repos"].as_array().expect("repos array");
    assert_eq!(
        repos_after.len(),
        1,
        "expected one repo; got:\n{stdout_after}"
    );
    assert_eq!(
        repos_after[0]["relation"], "ok",
        "status must report [ok] after the in-place fetch repair; got:\n{stdout_after}"
    );
}

// ===========================================================================
// 5. Repair-drive: [unreachable] → git fetch + checkout → [ok]
//
// The documented repair in status.rs for [unreachable] is:
//   "git fetch / rwv fetch to materialise the missing object"
//
// In-place `rwv fetch` does NOT network-fetch individual objects for clones
// that already exist on disk — it tries to resolve the locked SHA locally
// (git checkout <sha>) and fails when the SHA is absent from the local store.
// The effective repair for the [unreachable] state is therefore `git fetch`
// (the git command, pulling objects from the remote) followed by
// `git checkout <sha>`.  This is what the status comment says first:
// "git fetch / rwv fetch".
//
// The test drives this path using GC to make the SHA unreachable locally
// while it remains available in the bare-remote.  Because some CI environments
// retain loose objects past GC, the test skips when GC did not actually prune
// the SHA (consistent with
// `status_human_shows_unreachable_not_no_lock_when_sha_gone`).
// ===========================================================================

/// Repair-drive for the `[unreachable]` state.
///
/// Demonstrates the git-native repair path: `git fetch` re-fetches the pruned
/// object from the remote; the clone then resolves the locked SHA and returns
/// to `[ok]`.
///
/// NOTE on `rwv fetch` (in-place) for the `[unreachable]` state: when the
/// clone directory exists and `rwv fetch` is run in-place, it attempts
/// `git checkout <lock-sha>` against the local object store — it does NOT
/// run a network `git fetch`.  If the SHA is absent from the local store
/// (exactly the `unreachable` condition), the checkout fails and `rwv fetch`
/// exits non-zero.  The effective repair is therefore the git-native
/// `git fetch` to pull the missing object, not `rwv fetch` in-place.
/// This is recorded here rather than in a `failure:` closure because the
/// repair does work — just via `git fetch` (the git command), not the
/// `rwv fetch` in-place verb.
#[test]
fn unreachable_repair_drive_git_fetch_restores_ok() {
    let tmp = common::tempdir().unwrap();
    let (ws, project_dir, bare, repo_abs, sha) = make_workspace_with_bare_remote(tmp.path());

    // Rewrite history in the local clone so the original SHA becomes
    // unreachable, then GC to prune it from the local object store.
    // The SHA remains present in the bare remote.
    common::git_in(&repo_abs, &["checkout", "--orphan", "newroot"]);
    std::fs::write(repo_abs.join("README"), "rewritten\n").unwrap();
    common::git_in(&repo_abs, &["add", "README"]);
    common::git_in(&repo_abs, &["commit", "-m", "rewritten root"]);
    common::git_in(&repo_abs, &["branch", "-f", "main", "HEAD"]);
    common::git_in(&repo_abs, &["checkout", "main"]);
    // Remove the orphan branch ref so the old SHA has no references.
    let _ = common::git()
        .args(["branch", "-D", "newroot"])
        .current_dir(&repo_abs)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status();
    common::git_in(&repo_abs, &["gc", "--prune=now"]);

    // Sanity-check: if GC did not prune the object, the [unreachable] state
    // cannot be constructed — skip rather than asserting a wrong relation.
    let cat_file = common::git()
        .args(["cat-file", "-e", &sha])
        .current_dir(&repo_abs)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git cat-file");
    if cat_file.success() {
        common::report_skip(
            "the object survived GC here, so the unreachable-SHA relation cannot \
             be built — some hosts keep loose objects longer",
        );
        return;
    }

    // Re-point the lock to the (now-unreachable) SHA.
    let url = common::file_url(&bare);
    write_lock(&project_dir, "github/org/repo", &url, &sha);

    // 1. Confirm rwv status reports [unreachable].
    let out_before = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json (before repair)");
    assert!(
        out_before.status.success(),
        "status must exit 0 for an unreachable SHA; stderr: {}",
        String::from_utf8_lossy(&out_before.stderr)
    );
    let stdout_before = String::from_utf8_lossy(&out_before.stdout).into_owned();
    let parsed_before: Value = serde_json::from_str(&stdout_before)
        .unwrap_or_else(|e| panic!("stdout should parse as JSON ({e}):\n{stdout_before}"));
    let repos_before = parsed_before["repos"].as_array().expect("repos array");
    assert_eq!(
        repos_before.len(),
        1,
        "expected one repo; got:\n{stdout_before}"
    );
    assert_eq!(
        repos_before[0]["relation"], "unreachable",
        "status must report [unreachable] when locked SHA is absent from local store; \
         got:\n{stdout_before}"
    );

    // 2. Run the repair: `git fetch` to re-pull the pruned object from the
    //    bare remote (which still has it), then checkout the locked SHA.
    common::git_in(&repo_abs, &["fetch", "origin"]);
    // After fetch the SHA should now be reachable via FETCH_HEAD / remote refs.
    // Directly checkout the locked SHA to align the local clone.
    common::git_in(&repo_abs, &["checkout", &sha]);

    // 3. Verify the clone is at the locked SHA.
    let head_after = common::git_in(&repo_abs, &["rev-parse", "HEAD"]);
    assert_eq!(
        head_after, sha,
        "after repair the clone must be at the locked SHA"
    );

    // 4. rwv status now reports [ok].
    let out_after = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json (after repair)");
    assert!(
        out_after.status.success(),
        "status must exit 0 after repair; stderr: {}",
        String::from_utf8_lossy(&out_after.stderr)
    );
    let stdout_after = String::from_utf8_lossy(&out_after.stdout).into_owned();
    let parsed_after: Value = serde_json::from_str(&stdout_after)
        .unwrap_or_else(|e| panic!("stdout should parse as JSON ({e}):\n{stdout_after}"));
    let repos_after = parsed_after["repos"].as_array().expect("repos array");
    assert_eq!(
        repos_after.len(),
        1,
        "expected one repo; got:\n{stdout_after}"
    );
    assert_eq!(
        repos_after[0]["relation"], "ok",
        "status must report [ok] after git-fetch repair; got:\n{stdout_after}"
    );
}
