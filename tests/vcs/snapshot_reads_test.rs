//! Tests for snapshot reads: `read_file_at_revision` returns committed content
//! and is immune to working-tree mutation.

use crate::common;

/// `read_file_at_revision` returns the file content at a specific commit.
///
/// This directly tests the new `Vcs::read_file_at_revision` method on
/// the git backend, verifying that:
///   - It returns the content at the specified revision
///   - Subsequent working-tree modifications do not affect the read
#[test]
fn read_file_at_revision_returns_committed_content() {
    let tmp = common::tempdir().unwrap();
    let tmp = tmp.path();

    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    common::git_in(&repo, &["init", "-b", "main"]);

    // Commit v1.
    std::fs::write(repo.join("file.txt"), "version one\n").unwrap();
    common::git_in(&repo, &["add", "file.txt"]);
    common::git_in(&repo, &["commit", "-m", "v1"]);
    let sha_v1 = common::git_in(&repo, &["rev-parse", "HEAD"]);
    let rev_v1 = repoweave::vcs::ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse(&sha_v1.clone()).unwrap(),
        None,
    );

    // Commit v2.
    std::fs::write(repo.join("file.txt"), "version two\n").unwrap();
    common::git_in(&repo, &["add", "file.txt"]);
    common::git_in(&repo, &["commit", "-m", "v2"]);
    let sha_v2 = common::git_in(&repo, &["rev-parse", "HEAD"]);
    let rev_v2 = repoweave::vcs::ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse(&sha_v2).unwrap(),
        None,
    );

    // Overwrite the working tree with "version three" (uncommitted).
    std::fs::write(repo.join("file.txt"), "version three\n").unwrap();

    let vcs = repoweave::git::git_vcs();

    // Reading at rev_v1 should give "version one" regardless of WT state.
    let content_v1 = vcs
        .read_file_at_revision(&repo, &rev_v1, std::path::Path::new("file.txt"))
        .expect("read at v1 should succeed");
    assert!(
        content_v1.contains("version one"),
        "Expected 'version one', got: {content_v1}"
    );

    // Reading at rev_v2 should give "version two" regardless of WT state.
    let content_v2 = vcs
        .read_file_at_revision(&repo, &rev_v2, std::path::Path::new("file.txt"))
        .expect("read at v2 should succeed");
    assert!(
        content_v2.contains("version two"),
        "Expected 'version two', got: {content_v2}"
    );

    // The working-tree file is "version three" — the revision reads must
    // have been immune to that mutation.
    let wt_content =
        std::fs::read_to_string(repo.join("file.txt")).expect("working tree file should exist");
    assert!(
        wt_content.contains("version three"),
        "Working tree should still have 'version three'"
    );
}

/// The read returns the blob's bytes, surrounding whitespace included.
#[test]
fn read_file_at_revision_returns_the_blob_byte_exact() {
    let tmp = common::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    common::git_in(&repo, &["init", "-b", "main"]);
    let content = "\n  \tleading and trailing\n\n \t";
    std::fs::write(repo.join("file.txt"), content).unwrap();
    common::git_in(&repo, &["add", "file.txt"]);
    common::git_in(&repo, &["commit", "-m", "padded"]);
    let sha = common::git_in(&repo, &["rev-parse", "HEAD"]);

    let read = repoweave::git::git_vcs()
        .read_file_at_revision(
            &repo,
            &repoweave::vcs::ResolvedRevisionId::from_canonical(
                repoweave::vcs::CommitId::parse(&sha).unwrap(),
                None,
            ),
            std::path::Path::new("file.txt"),
        )
        .expect("read at HEAD");
    assert_eq!(read, content);
}
