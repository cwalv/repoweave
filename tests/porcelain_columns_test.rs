//! `git status --porcelain` is a fixed-column grammar, and the seam's three
//! readers now share one parse of it.
//!
//! The reason they must is not tidiness. Porcelain v1 writes `XY<space>path`,
//! where `X` is the index column and `Y` the working-tree column, and a
//! *leading space* is meaningful: `" M foo"` is modified-but-not-staged.
//! Trimming the output — which is what the seam's generic command runner does
//! to every result — deletes that space on the first line and leaves `"M foo"`,
//! which is what a *staged* modification looks like. Any reader that wants the
//! staged/unstaged distinction therefore cannot be built on trimmed output at
//! all.
//!
//! Only one dirty file is planted in each fixture below, because the hazard
//! lives on the first line and a second entry would mask it.

mod common;

use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo with one committed file, ready to be dirtied one way.
fn repo_with_one_commit(root: &Path) -> &Path {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "t@example.invalid"]);
    git(root, &["config", "user.name", "t"]);
    std::fs::write(root.join("tracked.txt"), "original\n").unwrap();
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-qm", "init"]);
    root
}

/// An unstaged modification is not a staged path.
///
/// The whole point. Porcelain reports this as `" M tracked.txt"`, and a reader
/// built on trimmed output sees `"M tracked.txt"` and reports it as staged —
/// which would make `doctor --fix`'s bundling refusal fire on a repo with
/// nothing staged at all, and would let the migration commit sweep up an edit
/// the operator never staged.
#[test]
fn an_unstaged_modification_is_not_reported_as_staged() {
    let tmp = common::tempdir().expect("tempdir");
    let repo = repo_with_one_commit(tmp.path());
    std::fs::write(repo.join("tracked.txt"), "edited\n").unwrap();

    let vcs = repoweave::git::git_vcs();
    assert_eq!(
        vcs.staged_paths(repo).expect("read staged paths"),
        Vec::<String>::new(),
        "an edit that was never `git add`ed is not staged"
    );
    assert!(
        vcs.has_uncommitted_changes(repo).expect("read status"),
        "the repo is still dirty — the point is which column says so"
    );
}

/// A staged modification is reported, so the test above is not passing
/// because `staged_paths` returns nothing at all.
#[test]
fn a_staged_modification_is_reported_as_staged() {
    let tmp = common::tempdir().expect("tempdir");
    let repo = repo_with_one_commit(tmp.path());
    std::fs::write(repo.join("tracked.txt"), "edited\n").unwrap();
    git(repo, &["add", "tracked.txt"]);

    assert_eq!(
        repoweave::git::git_vcs()
            .staged_paths(repo)
            .expect("read staged paths"),
        vec!["tracked.txt".to_owned()],
        "a staged edit is staged"
    );
}

/// An untracked file is dirt but is not staged.
///
/// Its index column is `?`, which is neither "staged" nor a space, so it is
/// the one entry a column test could plausibly get backwards.
#[test]
fn an_untracked_file_is_dirty_but_not_staged() {
    let tmp = common::tempdir().expect("tempdir");
    let repo = repo_with_one_commit(tmp.path());
    std::fs::write(repo.join("scratch.txt"), "new\n").unwrap();

    let vcs = repoweave::git::git_vcs();
    assert_eq!(
        vcs.staged_paths(repo).expect("read staged paths"),
        Vec::<String>::new(),
        "an untracked file has never been staged"
    );
    assert_eq!(
        vcs.dirty_file_names(repo).expect("read dirty names"),
        vec!["scratch.txt".to_owned()],
        "but it is dirt, and the untracked-inclusive listing reports it"
    );
    assert_eq!(
        vcs.tracked_dirty_file_names(repo)
            .expect("read tracked dirty names"),
        Vec::<String>::new(),
        "and the tracked-only listing does not"
    );
}

/// The two listings disagree about renames on purpose.
///
/// `staged_paths` answers "what lands in the commit" and reports the name
/// after the arrow. `dirty_file_names` answers "what is in the way" and hands
/// back the raw field, which `lock` compares against the paths it owns —
/// a rename matching neither is what makes it refuse to bundle. Unifying the
/// two would turn that refusal into a silent commit, so this pins that they
/// still differ.
#[test]
fn a_staged_rename_reads_differently_to_each_listing() {
    let tmp = common::tempdir().expect("tempdir");
    let repo = repo_with_one_commit(tmp.path());
    git(repo, &["mv", "tracked.txt", "renamed.txt"]);

    let vcs = repoweave::git::git_vcs();
    assert_eq!(
        vcs.staged_paths(repo).expect("read staged paths"),
        vec!["renamed.txt".to_owned()],
        "the commit will carry the post-rename name"
    );

    let dirty = vcs.dirty_file_names(repo).expect("read dirty names");
    assert_eq!(dirty.len(), 1, "one rename, one record: {dirty:?}");
    assert!(
        dirty[0].contains("tracked.txt") && dirty[0].contains("renamed.txt"),
        "the dirt listing keeps both halves so an owned-path comparison \
         cannot match one by accident, got {:?}",
        dirty[0]
    );
}
