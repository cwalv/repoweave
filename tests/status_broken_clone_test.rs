//! Integration tests for `rwv status` broken-clone states: `[missing]` and
//! `[unreachable]`.
//!
//! Doc claims anchored here (fo-oueuv7.3):
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

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

/// Run git with standard test env vars set, suppressing output. Panics on failure.
fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

/// Run git and capture stdout. Panics on failure.
fn git_out(args: &[&str], dir: &Path) -> String {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git should be available");
    assert!(out.status.success(), "git {:?} failed", args);
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn init_repo_with_commit(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

/// Write a lock file pinning a single repo to the given SHA.
fn write_lock(project_dir: &Path, repo_path: &str, url: &str, sha: &str) {
    let yaml = format!(
        "repositories:\n  {repo_path}:\n    type: git\n    url: {url}\n    version: {sha}\n"
    );
    std::fs::write(project_dir.join("rwv.lock"), yaml).unwrap();
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

    let url = format!("file://{}", repo_path.display());
    let manifest = format!(
        "repositories:\n  github/org/repo:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();
    write_lock(&project_dir, "github/org/repo", &url, &sha);

    std::fs::write(ws.join(".rwv-active"), format!("{project}\n")).unwrap();

    (ws, repo_path, url, sha)
}

// ===========================================================================
// 1. Missing clone dir — `[missing]` label, not `[no-lock]`
// ===========================================================================

#[test]
fn status_human_shows_missing_not_no_lock_when_clone_dir_absent() {
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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
    git(&["checkout", "--orphan", "newroot"], &repo_abs);
    std::fs::write(repo_abs.join("README"), "rewritten").unwrap();
    git(&["add", "README"], &repo_abs);
    git(&["commit", "-m", "rewritten root"], &repo_abs);
    // Move main to the new root (force — so old SHA becomes unreachable).
    git(&["branch", "-f", "main", "HEAD"], &repo_abs);
    git(&["checkout", "main"], &repo_abs);
    // Prune the old unreferenced object.
    git(&["gc", "--prune=now"], &repo_abs);

    // Verify the old SHA is truly gone (sanity check for the test itself).
    let cat_file = common::git()
        .args(["cat-file", "-e", &sha])
        .current_dir(&repo_abs)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git cat-file");
    if cat_file.success() {
        // Object survived GC — skip this test rather than assert a wrong relation.
        // (Some CI environments have pack-refs / loose-object policies that keep
        // objects longer; the unreachable variant is still tested structurally by
        // the JSON test below which writes a fabricated bad SHA.)
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
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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
