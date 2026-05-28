//! Negative tests for the `assert_log_ordering` history-shape helper.
//!
//! These tests verify that `assert_log_ordering` catches an *intentionally-wrong*
//! history shape — i.e. an "Option A" end state where commits are in the reverse
//! of the expected order. This is the "at least one negative test" required by
//! acceptance criterion fo-v8hq4.5.
//!
//! ## Why this is load-bearing
//!
//! The silent-fallback epic (fo-vsldv) was triggered by a subagent producing
//! an Option A implementation that satisfied all *tip-movement* assertions but
//! reversed the history shape: primary's commits ended up on top of the
//! workweave's contribution instead of the reverse.
//!
//! These tests construct that exact wrong shape in git and confirm the helper
//! panics (via `std::panic::catch_unwind`) rather than silently passing.

use std::path::Path;

mod common;

// ---------------------------------------------------------------------------
// Minimal git helpers (no rwv involved — purely git plumbing)
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

/// Init a bare git repo with one commit on `main`. Returns HEAD SHA.
fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    let out = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit_file(repo: &Path, filename: &str, content: &str, msg: &str) {
    let path = repo.join(filename);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
}

// ---------------------------------------------------------------------------
// Negative test 1: correct shape passes
//
// Baseline: assert_log_ordering does NOT panic when commits are in the
// expected (newest-first) order.
// ---------------------------------------------------------------------------

#[test]
fn assert_log_ordering_passes_for_correct_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);

    // Build a history: base → A → B
    // After `git log --oneline`, B is first (pos 0), A is second (pos 1).
    commit_file(&repo, "a.txt", "a\n", "feat: commit-A");
    commit_file(&repo, "b.txt", "b\n", "feat: commit-B");

    // B is newer than A → B must appear above A in the log.
    // This should NOT panic.
    common::assert_log_ordering(&repo, &["feat: commit-B", "feat: commit-A"]);
}

// ---------------------------------------------------------------------------
// Negative test 2: wrong shape (Option A) is caught by the helper
//
// We build a history with A above B (A is newer), then ask the helper to
// assert B is above A. The helper must panic — catching the panic proves
// that a real Option-A implementation would not slip past shape assertions.
// ---------------------------------------------------------------------------

#[test]
fn assert_log_ordering_fails_for_wrong_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);

    // Build a history: base → B → A
    // After `git log --oneline`, A is first (pos 0, newest), B is second (pos 1).
    // This is the "Option A" shape: B's contribution is buried under A's.
    commit_file(&repo, "b.txt", "b\n", "feat: ww-contribution");
    commit_file(&repo, "a.txt", "a\n", "feat: target-prior-tip");

    // The CORRECT shape would be: ww-contribution ON TOP of target-prior-tip.
    // But we've built the WRONG shape: target-prior-tip on top of ww-contribution.
    //
    // assert_log_ordering must panic when we request the correct ordering
    // on a repo with the wrong ordering.
    let repo_clone = repo.clone();
    let result = std::panic::catch_unwind(move || {
        common::assert_log_ordering(
            &repo_clone,
            // We're asserting: ww-contribution (pos 1) appears ABOVE target-prior-tip (pos 0).
            // The actual log has target-prior-tip (pos 0) above ww-contribution (pos 1).
            // This ordering request SHOULD FAIL.
            &["feat: ww-contribution", "feat: target-prior-tip"],
        );
    });

    assert!(
        result.is_err(),
        "assert_log_ordering must panic when commits are in the wrong order \
         (Option A end-state). If this assertion fails, the helper would silently \
         pass an inverted history — defeating the purpose of shape assertions."
    );
}

// ---------------------------------------------------------------------------
// Negative test 3: missing commit is caught
//
// assert_log_ordering must panic if a requested commit message is not found
// in the log at all. This prevents false positives from typos in test code.
// ---------------------------------------------------------------------------

#[test]
fn assert_log_ordering_fails_for_missing_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    init_repo(&repo);

    commit_file(&repo, "a.txt", "a\n", "feat: commit-A");

    let repo_clone = repo.clone();
    let result = std::panic::catch_unwind(move || {
        common::assert_log_ordering(&repo_clone, &["feat: commit-A", "feat: nonexistent-commit"]);
    });

    assert!(
        result.is_err(),
        "assert_log_ordering must panic when a commit message is not found in the log"
    );
}
