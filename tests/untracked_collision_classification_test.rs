//! `untracked-collision` must name the condition it says it names.
//!
//! git refuses a fast-forward with two different messages that share a tail.
//! One is about untracked files standing where the incoming tree writes; the
//! other is about tracked files the operator has modified. They are different
//! conditions with different remedies — move-or-delete versus commit-or-stash
//! — and the second is not this kind.
//!
//! Driven through the real git implementation rather than by handing the
//! classifier a string. The refusals this depends on are git's own wording,
//! so a fixture that quotes them proves the parser matches the fixture; only
//! a real repo proves it matches git.

use repoweave::vcs::{ResolvedRevisionId, VcsError};
use std::path::Path;

mod common;

/// `main` holds `tracked.txt`; the side branch modifies it and adds
/// `arriving.txt`. Fast-forwarding `main` to the returned tip therefore has to
/// write both paths, which is what makes an obstruction at either one a
/// collision rather than a no-op.
fn seed(repo: &Path) -> ResolvedRevisionId {
    common::git_in(repo, &["init", "-b", "main"]);
    common::git_in(repo, &["config", "user.email", "test@test.com"]);
    common::git_in(repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
    common::git_in(repo, &["add", "."]);
    common::git_in(repo, &["commit", "-m", "base"]);
    common::git_in(repo, &["checkout", "-b", "side"]);
    std::fs::write(repo.join("tracked.txt"), "moved-by-side\n").unwrap();
    std::fs::write(repo.join("arriving.txt"), "arriving\n").unwrap();
    common::git_in(repo, &["add", "."]);
    common::git_in(repo, &["commit", "-m", "side"]);
    let tip = repoweave::git::git_vcs().head_revision(repo).unwrap();
    common::git_in(repo, &["checkout", "main"]);
    tip
}

fn repo_at(tmp: &Path, name: &str) -> std::path::PathBuf {
    let p = tmp.join(name);
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// The condition the kind is for: an untracked file where the merge must write.
#[test]
fn an_untracked_obstruction_is_named_with_its_paths() {
    let tmp = common::tempdir().unwrap();
    let repo = repo_at(tmp.path(), "untracked");
    let tip = seed(&repo);
    std::fs::write(repo.join("arriving.txt"), "mine, untracked\n").unwrap();

    let err = repoweave::git::git_vcs()
        .advance_if_fast_forward(&repo, &tip)
        .expect_err("git refuses to clobber an untracked file");

    let VcsError::UntrackedCollision { paths, .. } = &err else {
        panic!("expected untracked-collision, got {}: {err}", err.kind());
    };
    assert_eq!(
        paths,
        &["arriving.txt".to_owned()],
        "paths must be the obstructing files and nothing else"
    );
}

/// The near-miss. Tracked-and-modified is a different condition, remedied
/// differently, and must not borrow this kind.
///
/// The failing shape this pins is specific: git's two refusals share
/// `would be overwritten by merge:`, so a header matched on that fragment
/// accepts both, and the untracked trailer is absent here, so a trailer used
/// as a split rather than a requirement yields the rest of the message. The
/// result was `untracked-collision` carrying `["tracked.txt", "Please commit
/// your changes or stash them before you merge.", "Aborting"]` — two of git's
/// sentences presented to a consumer as filenames.
#[test]
fn a_tracked_modification_is_not_an_untracked_collision() {
    let tmp = common::tempdir().unwrap();
    let repo = repo_at(tmp.path(), "tracked");
    let tip = seed(&repo);
    std::fs::write(repo.join("tracked.txt"), "live edit\n").unwrap();

    let err = repoweave::git::git_vcs()
        .advance_if_fast_forward(&repo, &tip)
        .expect_err("git refuses to overwrite a modified tracked file");

    if let VcsError::UntrackedCollision { paths, .. } = &err {
        panic!("a tracked-modification refusal was reported as untracked-collision, with git's own prose as paths: {paths:?}");
    }
    assert_eq!(
        err.kind(),
        "command-failed",
        "with no kind of its own, this falls back to the raw failure: {err}"
    );
    let VcsError::CommandFailed { stderr, .. } = &err else {
        unreachable!("kind() just reported command-failed");
    };
    assert!(
        stderr.contains("Your local changes to the following files"),
        "the fallback must carry git's own account of the refusal: {stderr:?}"
    );
}
