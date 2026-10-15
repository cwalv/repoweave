//! git's stdout is data, and the seam must not edit it on the way through.

mod common;

use std::path::Path;

use repoweave::manifest::Role;

const LEADING_SPACE: &str = " leading-space.txt";

fn repo_with_one_commit(root: &Path) {
    common::git_in(root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("keep.txt"), "one\n").unwrap();
    std::fs::write(root.join(LEADING_SPACE), "one\n").unwrap();
    common::git_in(root, &["add", "."]);
    common::git_in(root, &["commit", "-qm", "init"]);
}

fn commit_edit_to_leading_space_file(root: &Path) {
    std::fs::write(root.join(LEADING_SPACE), "two\n").unwrap();
    common::git_in(root, &["commit", "-qam", "edit"]);
}

#[test]
fn a_changed_path_keeps_its_leading_whitespace() {
    let tmp = common::tempdir().unwrap();
    let repo = tmp.path();
    repo_with_one_commit(repo);
    let vcs = repoweave::git::git_vcs();
    let from = vcs.head_revision(repo).unwrap();
    commit_edit_to_leading_space_file(repo);
    let to = vcs.head_revision(repo).unwrap();

    assert_eq!(
        vcs.changed_paths_between(repo, &from, &to).unwrap(),
        vec![LEADING_SPACE.to_owned()]
    );
}

#[test]
fn a_moved_member_reshares_the_file_whose_name_begins_with_whitespace() {
    let tmp = common::tempdir().unwrap();
    let repo = tmp.path();
    repo_with_one_commit(repo);
    let vcs = repoweave::git::git_vcs();
    let from = vcs.head_revision(repo).unwrap();
    commit_edit_to_leading_space_file(repo);

    let settled =
        repoweave::settle::settle_moved(vcs.as_ref(), repo, Role::Reference, Some(&from)).unwrap();

    let placement = settled.files.expect("git answers population questions");
    assert_eq!(
        placement.reflinked + placement.hardlinked + placement.written,
        1,
        "the rewritten file with a leading-space name is counted"
    );
}
