//! `untracked-collision` must name the condition it says it names.
//!
//! The kind is for untracked files standing where an advance writes. Two
//! neighbouring refusals are not it, and both are reachable from a fixture a
//! few lines apart: a tracked file the operator modified, remedied by
//! commit-or-stash rather than move-or-delete; and a diverged tip, remedied
//! by neither.
//!
//! Driven through the real git implementation rather than by constructing the
//! error directly. What the classification rests on is git's own account of
//! the repo, so a hand-built error proves only that the arms agree with the
//! hand-built error.

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

/// The same obstruction, on a repo where git's refusal says less.
///
/// git prints `Please move or remove them before you merge.` only when
/// `advice.commitBeforeMerge` is on. GitHub's macOS runner image turns it off
/// globally, so on that host git names the obstructing files and stops. The
/// condition is identical and the classification must be too.
#[test]
fn an_untracked_obstruction_is_named_when_git_advice_is_off() {
    let tmp = common::tempdir().unwrap();
    let repo = repo_at(tmp.path(), "advice-off");
    let tip = seed(&repo);
    common::git_in(&repo, &["config", "advice.commitBeforeMerge", "false"]);
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
        "paths are the obstructing files, and stay so when git's closing \
         sentence is absent to bound them"
    );
}

/// `main` and `side` each hold a commit the other lacks, and `side` adds
/// `arriving.txt`.
fn seed_diverged(repo: &Path) -> ResolvedRevisionId {
    let tip = seed(repo);
    std::fs::write(repo.join("main-only.txt"), "main\n").unwrap();
    common::git_in(repo, &["add", "."]);
    common::git_in(repo, &["commit", "-m", "main diverges"]);
    tip
}

/// A diverged tip refuses for a reason moving files cannot fix. Its incoming
/// tree still adds paths, so one of them happening to exist untracked here is
/// a coincidence — naming it a collision would send the operator to clear
/// files and retry into the identical refusal.
#[test]
fn a_diverged_tip_is_not_an_untracked_collision() {
    let tmp = common::tempdir().unwrap();
    let repo = repo_at(tmp.path(), "diverged");
    let tip = seed_diverged(&repo);
    std::fs::write(repo.join("arriving.txt"), "mine, untracked\n").unwrap();

    let vcs = repoweave::git::git_vcs();
    let head = vcs.head_revision(&repo).unwrap();
    assert!(
        !vcs.is_ancestor(&repo, &head, &tip).unwrap(),
        "fixture precondition: the tip must not be reachable, else this \
         exercises the collision arm rather than the guard before it"
    );
    assert_eq!(
        common::git_in(&repo, &["ls-files", "--others", "--exclude-standard"]),
        "arriving.txt",
        "fixture precondition: an untracked file must sit at a path the \
         incoming tree adds, else the guard is untested either way"
    );

    let err = vcs
        .advance_if_fast_forward(&repo, &tip)
        .expect_err("git refuses to fast-forward a diverged tip");

    if let VcsError::UntrackedCollision { paths, .. } = &err {
        panic!("a diverged-tip refusal was reported as untracked-collision: {paths:?}");
    }
    assert_eq!(
        err.kind(),
        "command-failed",
        "divergence has no kind of its own here: {err}"
    );
}
