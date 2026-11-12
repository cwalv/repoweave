use repoweave::git::git_vcs;
use repoweave::vcs::{
    ConflictOp, DerivedContentPolicy, DiscardOutcome, LockDriverPart, LockDriverSetup,
    MidOperation, RawRefName, RegistrationPrune, RemoteRef, ResolvedRevisionId, TrackingRef,
    VcsError,
};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

use crate::common;

/// Create a fresh git repo in a temp directory with one initial commit.
fn init_repo() -> TempDir {
    let dir = common::tempdir().unwrap();
    let p = dir.path();

    common::git_in(p, &["init"]);
    common::git_in(p, &["config", "user.email", "test@test.com"]);
    common::git_in(p, &["config", "user.name", "Test"]);

    fs::write(p.join("README.md"), "init").unwrap();
    common::git_in(p, &["add", "."]);
    common::git_in(p, &["commit", "-m", "initial"]);

    dir
}

// ============================================================================
// has_uncommitted_changes
// ============================================================================

#[test]
fn has_uncommitted_changes_clean_repo() {
    let dir = init_repo();
    let vcs = git_vcs();
    assert!(!vcs.has_uncommitted_changes(dir.path()).unwrap());
}

#[test]
fn has_uncommitted_changes_staged_changes() {
    let dir = init_repo();
    let p = dir.path();

    fs::write(p.join("new.txt"), "staged content").unwrap();
    common::git_in(p, &["add", "new.txt"]);

    let vcs = git_vcs();
    assert!(vcs.has_uncommitted_changes(p).unwrap());
}

#[test]
fn has_uncommitted_changes_unstaged_modification() {
    let dir = init_repo();
    let p = dir.path();

    // Modify a tracked file without staging.
    fs::write(p.join("README.md"), "modified").unwrap();

    let vcs = git_vcs();
    assert!(vcs.has_uncommitted_changes(p).unwrap());
}

#[test]
fn has_uncommitted_changes_untracked_file() {
    let dir = init_repo();
    let p = dir.path();

    fs::write(p.join("untracked.txt"), "hello").unwrap();

    let vcs = git_vcs();
    assert!(vcs.has_uncommitted_changes(p).unwrap());
}

// ============================================================================
// tag_at_head
// ============================================================================

#[test]
fn tag_at_head_no_tag() {
    let dir = init_repo();
    let vcs = git_vcs();
    assert_eq!(vcs.tag_at_head(dir.path()).unwrap(), None);
}

#[test]
fn tag_at_head_lightweight_tag() {
    let dir = init_repo();
    let p = dir.path();

    common::git_in(p, &["tag", "v0.1.0"]);

    let vcs = git_vcs();
    let tag = vcs.tag_at_head(p).unwrap();
    assert_eq!(tag.as_ref().map(|t| t.as_str()), Some("v0.1.0"));
}

#[test]
fn tag_at_head_annotated_tag() {
    let dir = init_repo();
    let p = dir.path();

    common::git_in(p, &["tag", "-a", "v1.0.0", "-m", "release v1.0.0"]);

    let vcs = git_vcs();
    let tag = vcs.tag_at_head(p).unwrap();
    assert_eq!(tag.as_ref().map(|t| t.as_str()), Some("v1.0.0"));
}

#[test]
fn tag_at_head_tag_not_at_head() {
    let dir = init_repo();
    let p = dir.path();

    // Tag the first commit, then create a second commit so HEAD moves past the tag.
    common::git_in(p, &["tag", "v0.0.1"]);

    fs::write(p.join("second.txt"), "second").unwrap();
    common::git_in(p, &["add", "."]);
    common::git_in(p, &["commit", "-m", "second commit"]);

    let vcs = git_vcs();
    assert_eq!(vcs.tag_at_head(p).unwrap(), None);
}

#[test]
fn tag_at_head_multiple_tags() {
    let dir = init_repo();
    let p = dir.path();

    common::git_in(p, &["tag", "v1.0.0"]);
    common::git_in(p, &["tag", "release-1"]);

    let vcs = git_vcs();
    let tag = vcs.tag_at_head(p).unwrap();
    let tag_str = tag.as_ref().map(|t| t.as_str());
    // When multiple tags point at HEAD, we get one of them.
    assert!(
        tag_str == Some("v1.0.0") || tag_str == Some("release-1"),
        "expected one of the two tags, got {:?}",
        tag
    );
}

#[test]
fn tag_at_head_skips_savepoint_only() {
    // When only transient savepoint tags point at HEAD, tag_at_head returns
    // None so the lock writer falls back to the canonical SHA.
    let dir = init_repo();
    let p = dir.path();

    common::git_in(p, &["tag", "savepoint/2024-01-01-abc"]);

    let vcs = git_vcs();
    assert_eq!(vcs.tag_at_head(p).unwrap(), None);
}

#[test]
fn tag_at_head_skips_pre_op_only() {
    let dir = init_repo();
    let p = dir.path();

    common::git_in(p, &["tag", "rwv/pre-op/op-xyz"]);

    let vcs = git_vcs();
    assert_eq!(vcs.tag_at_head(p).unwrap(), None);
}

#[test]
fn tag_at_head_prefers_release_over_transient() {
    // Release-shape tag `v9.9.9` alongside transient savepoint tag — release wins.
    let dir = init_repo();
    let p = dir.path();

    common::git_in(p, &["tag", "savepoint/old"]);
    common::git_in(p, &["tag", "v9.9.9"]);

    let vcs = git_vcs();
    let tag = vcs.tag_at_head(p).unwrap();
    assert_eq!(tag.as_ref().map(|t| t.as_str()), Some("v9.9.9"));
}

#[test]
fn tag_at_head_prefers_release_over_lightweight() {
    // When both a release-shape tag and an arbitrary tag point at HEAD, the
    // release tag is preferred regardless of git's ordering.
    let dir = init_repo();
    let p = dir.path();

    common::git_in(p, &["tag", "aaa-arbitrary"]);
    common::git_in(p, &["tag", "v1.2.3"]);

    let vcs = git_vcs();
    let tag = vcs.tag_at_head(p).unwrap();
    assert_eq!(tag.as_ref().map(|t| t.as_str()), Some("v1.2.3"));
}

#[test]
fn head_revision_skips_savepoint_tag_display() {
    // Acceptance scenario (a): only transient savepoint tags at HEAD →
    // head_revision's display form falls back to the SHA, so generate_lock
    // writes the SHA in `rwv.lock`.
    let dir = init_repo();
    let p = dir.path();

    common::git_in(p, &["tag", "savepoint/op-1"]);

    let vcs = git_vcs();
    let head = vcs.head_revision(p).unwrap();
    // canonical is the SHA, display falls back to canonical (no tag chosen).
    assert_eq!(head.display_str(), head.id().as_str());
    assert_eq!(head.display_str().len(), 40);
}

#[test]
fn head_revision_picks_release_tag_when_mixed() {
    // Acceptance scenario (b): release tag + transient tag → release tag wins.
    let dir = init_repo();
    let p = dir.path();

    common::git_in(p, &["tag", "savepoint/op-1"]);
    common::git_in(p, &["tag", "v9.9.9"]);

    let vcs = git_vcs();
    let head = vcs.head_revision(p).unwrap();
    assert_eq!(head.display_str(), "v9.9.9");
}

#[test]
fn revision_id_tag_form_equals_sha_form_after_resolve() {
    // Acceptance scenario (c) — at the ResolvedRevisionId layer: a tag-form
    // id resolved against the repo compares equal to the head-form SHA id.
    // Post-split: a `RawRevisionId` cannot be compared to a
    // `ResolvedRevisionId` at all (no `PartialEq`), so the only assertion
    // that remains is the canonical-SHA equality after `resolve_revision`.
    // The compile-time invariant is exercised by the `compile_fail`
    // doc-test in `vcs.rs`.
    let dir = init_repo();
    let p = dir.path();
    common::git_in(p, &["tag", "v1.0.0"]);

    let vcs = git_vcs();
    let head_sha_form = vcs.head_revision(p).unwrap();
    let tag_form_resolved = vcs.resolve_revision(p, "v1.0.0").unwrap();
    assert_eq!(tag_form_resolved, head_sha_form);
}

#[test]
fn has_uncommitted_changes_deleted_tracked_file() {
    let dir = init_repo();
    let p = dir.path();

    // Delete a tracked file without staging the deletion.
    fs::remove_file(p.join("README.md")).unwrap();

    let vcs = git_vcs();
    assert!(vcs.has_uncommitted_changes(p).unwrap());
}

#[test]
fn has_uncommitted_changes_gitignored_file() {
    let dir = init_repo();
    let p = dir.path();

    // Add a .gitignore, commit it, then create an ignored file.
    fs::write(p.join(".gitignore"), "*.log\n").unwrap();
    common::git_in(p, &["add", ".gitignore"]);
    common::git_in(p, &["commit", "-m", "add gitignore"]);

    fs::write(p.join("debug.log"), "some logs").unwrap();

    let vcs = git_vcs();
    // Ignored files should NOT count as uncommitted changes.
    assert!(!vcs.has_uncommitted_changes(p).unwrap());
}

#[test]
fn has_uncommitted_changes_staged_deletion() {
    let dir = init_repo();
    let p = dir.path();

    // Stage removal of a tracked file.
    common::git_in(p, &["rm", "README.md"]);

    let vcs = git_vcs();
    assert!(vcs.has_uncommitted_changes(p).unwrap());
}

// ============================================================================
// dirty_file_names / tracked_dirty_file_names / is_tracked
// ============================================================================

#[test]
fn dirty_file_names_lists_tracked_and_untracked() {
    let dir = init_repo();
    let p = dir.path();
    fs::write(p.join("README.md"), "modified").unwrap();
    fs::write(p.join("scratch.txt"), "new").unwrap();

    let mut names = git_vcs().dirty_file_names(p).unwrap();
    names.sort();
    assert_eq!(names, vec!["README.md", "scratch.txt"]);
}

#[test]
fn tracked_dirty_file_names_excludes_untracked() {
    let dir = init_repo();
    let p = dir.path();
    fs::write(p.join("README.md"), "modified").unwrap();
    fs::write(p.join("scratch.txt"), "new").unwrap();

    assert_eq!(
        git_vcs().tracked_dirty_file_names(p).unwrap(),
        ["README.md"]
    );
}

#[test]
fn dirty_file_names_empty_on_clean_repo() {
    let dir = init_repo();
    let p = dir.path();

    assert!(git_vcs().dirty_file_names(p).unwrap().is_empty());
    assert!(git_vcs().tracked_dirty_file_names(p).unwrap().is_empty());
}

#[test]
fn is_tracked_distinguishes_committed_from_untracked() {
    let dir = init_repo();
    let p = dir.path();
    fs::write(p.join("scratch.txt"), "new").unwrap();

    assert!(git_vcs().is_tracked(p, Path::new("README.md")).unwrap());
    assert!(!git_vcs().is_tracked(p, Path::new("scratch.txt")).unwrap());
}

#[test]
fn is_tracked_is_false_rather_than_err_outside_a_repo() {
    let dir = common::tempdir().unwrap();
    let p = dir.path();
    fs::write(p.join("loose.txt"), "loose").unwrap();

    assert!(!git_vcs().is_tracked(p, Path::new("loose.txt")).unwrap());
}

// ============================================================================
// init_repo
// ============================================================================

#[test]
fn init_repo_creates_git_directory() {
    let dir = common::tempdir().unwrap();
    let repo_path = dir.path().join("new-repo");

    let vcs = git_vcs();
    vcs.init_repo(&repo_path).unwrap();

    assert!(
        repo_path.join(".git").exists(),
        "should create .git directory"
    );
}

#[test]
fn init_repo_sets_main_as_initial_branch() {
    let dir = common::tempdir().unwrap();
    let repo_path = dir.path().join("new-repo");

    let vcs = git_vcs();
    vcs.init_repo(&repo_path).unwrap();

    // Verify the initial branch is "main" by reading HEAD.
    let head = fs::read_to_string(repo_path.join(".git/HEAD")).unwrap();
    assert!(
        head.contains("refs/heads/main"),
        "initial branch should be main, got: {head}"
    );
}

#[test]
fn init_repo_creates_nested_directories() {
    let dir = common::tempdir().unwrap();
    let repo_path = dir.path().join("a").join("b").join("c").join("repo");

    let vcs = git_vcs();
    vcs.init_repo(&repo_path).unwrap();

    assert!(
        repo_path.join(".git").exists(),
        "should create nested dirs and init repo"
    );
}

#[test]
fn init_repo_is_recognized_by_is_repo() {
    let dir = common::tempdir().unwrap();
    let repo_path = dir.path().join("new-repo");

    let vcs = git_vcs();
    vcs.init_repo(&repo_path).unwrap();

    assert!(
        vcs.is_repo(&repo_path),
        "init_repo result should be recognized as a repo"
    );
}

/// `git rev-parse --git-dir` walks up past any directory to an enclosing
/// repo's own `.git`, so a directory with no `.git` of its own but sitting
/// inside one must not read as a repo — the trait promises `path` itself is
/// the repo or worktree, not merely somewhere underneath one.
#[test]
fn a_bare_directory_inside_an_enclosing_repo_is_not_a_repo() {
    let outer = common::tempdir().unwrap();
    common::git_in(outer.path(), &["init"]);

    let nested = outer
        .path()
        .join("weave")
        .join("target")
        .join("release")
        .join("deps");
    fs::create_dir_all(&nested).unwrap();

    let vcs = git_vcs();
    assert!(
        common::git()
            .args(["rev-parse", "--git-dir"])
            .current_dir(&nested)
            .output()
            .unwrap()
            .status
            .success(),
        "the walk-up itself must still succeed from here, or this test is not exercising it"
    );
    assert!(
        !vcs.is_repo(&nested),
        "a directory with no .git of its own is not a repo, however deep \
         inside an enclosing one it sits"
    );
}

// ============================================================================
// ResolvedRevisionId — typed identity, equality, and serialization
// ============================================================================

#[test]
fn revision_id_from_canonical_no_display_uses_canonical_for_both() {
    // Without an explicit display form, the id and `display_str` agree.
    let r = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse("0123456789abcdef0123456789abcdef01234567").unwrap(),
        None,
    );
    assert_eq!(r.id().as_str(), "0123456789abcdef0123456789abcdef01234567");
    assert_eq!(r.display_str(), "0123456789abcdef0123456789abcdef01234567");
}

#[test]
fn revision_id_from_canonical_with_display_form() {
    let r = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        Some("v1.0.0".to_string()),
    );
    assert_eq!(r.id().as_str(), "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(r.display_str(), "v1.0.0");
}

#[test]
fn revision_id_from_canonical_suppresses_redundant_display() {
    // When `display` equals canonical, suppress it so serialization is clean.
    let r = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse("0123456789abcdef0123456789abcdef01234567").unwrap(),
        Some("0123456789abcdef0123456789abcdef01234567".to_string()),
    );
    assert_eq!(r.id().as_str(), "0123456789abcdef0123456789abcdef01234567");
    assert_eq!(r.display_str(), "0123456789abcdef0123456789abcdef01234567");
}

#[test]
fn revision_id_equality_compares_canonical() {
    // Tag-form and SHA-form referring to the same canonical commit compare equal.
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let tag_form = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse(&sha).unwrap(),
        Some("v1.0.0".to_string()),
    );
    let sha_form =
        ResolvedRevisionId::from_canonical(repoweave::vcs::CommitId::parse(&sha).unwrap(), None);
    assert_eq!(tag_form, sha_form);

    // Different canonical SHAs are never equal, even with matching display.
    let other = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap(),
        Some("v1.0.0".to_string()),
    );
    assert_ne!(tag_form, other);
}

/// What is serialized is read back as a pin, so a resolved revision writes
/// its commit id even when it carries a name for display.
#[test]
fn revision_id_serializes_the_commit_id_not_the_display_form() {
    let r = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        Some("v1.0.0".to_string()),
    );
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(json, "\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"");
    assert_eq!(r.display_str(), "v1.0.0");
}

#[test]
fn revision_id_serialize_canonical_when_no_display() {
    let r = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse("0123456789abcdef0123456789abcdef01234567").unwrap(),
        None,
    );
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(json, "\"0123456789abcdef0123456789abcdef01234567\"");
}

#[test]
fn raw_revision_id_deserializes_scalar_verbatim() {
    // Post-split: lock-file scalars deserialize into `RawRevisionId`,
    // not `ResolvedRevisionId`. `ResolvedRevisionId` has no `Deserialize`
    // impl on purpose — resolution to a canonical SHA is path-rooted and
    // cannot happen at the deserializer layer.
    let r: repoweave::vcs::RawRevisionId = serde_json::from_str("\"v1.0.0\"").unwrap();
    assert_eq!(r.as_str(), "v1.0.0");
}

#[test]
fn revision_id_round_trip_scalar() {
    let original = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
        Some("v1.0.0".to_string()),
    );
    let json = serde_json::to_string(&original).unwrap();
    // Round-trip through the parse boundary: a resolved value serializes
    // as a single scalar, and re-parsing it lands in `RawRevisionId`
    // (deserialize only ever yields raw — re-resolution requires a repo).
    let restored: repoweave::vcs::RawRevisionId = serde_json::from_str(&json).unwrap();
    assert_eq!(
        restored.as_str(),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert!(restored.is_commit_id());
}

// ============================================================================
// Vcs::resolve_revision — fills in canonical SHA, preserves display form
// ============================================================================

#[test]
fn resolve_revision_tag_to_canonical_sha() {
    let dir = init_repo();
    let p = dir.path();
    common::git_in(p, &["tag", "v1.0.0"]);
    let head_sha = common::git_in(p, &["rev-parse", "HEAD"]);

    let vcs = git_vcs();
    let resolved = vcs.resolve_revision(p, "v1.0.0").unwrap();
    assert_eq!(
        resolved.id().as_str(),
        &head_sha,
        "canonical should be the SHA"
    );
    assert_eq!(
        resolved.display_str(),
        "v1.0.0",
        "display preserves the tag-form"
    );
}

#[test]
fn resolve_revision_sha_passes_through() {
    let dir = init_repo();
    let p = dir.path();
    let head_sha = common::git_in(p, &["rev-parse", "HEAD"]);

    let vcs = git_vcs();
    let resolved = vcs.resolve_revision(p, &head_sha).unwrap();
    assert_eq!(resolved.id().as_str(), &head_sha);
    // SHA input — no separate display form.
    assert_eq!(resolved.display_str(), &head_sha);
}

#[test]
fn resolve_revision_unknown_revision_errors() {
    let dir = init_repo();
    let vcs = git_vcs();
    let result = vcs.resolve_revision(dir.path(), "v9.9.9-nope");
    assert!(result.is_err());
}

#[test]
fn resolve_revision_then_equal_to_head_revision() {
    // Equality between the tag-form lock entry's resolved ResolvedRevisionId and
    // the HEAD's ResolvedRevisionId — the canonical-SHA equality the spec requires.
    let dir = init_repo();
    let p = dir.path();
    common::git_in(p, &["tag", "v0.3.4"]);

    let vcs = git_vcs();
    let lock_entry = vcs.resolve_revision(p, "v0.3.4").unwrap();
    let head = vcs.head_revision(p).unwrap();
    assert_eq!(
        lock_entry, head,
        "tag-form lock and SHA HEAD should compare equal once both resolved"
    );
}

#[test]
fn head_revision_preserves_tag_at_head_as_display() {
    let dir = init_repo();
    let p = dir.path();
    common::git_in(p, &["tag", "v0.3.4"]);

    let vcs = git_vcs();
    let head = vcs.head_revision(p).unwrap();
    let head_sha = common::git_in(p, &["rev-parse", "HEAD"]);
    assert_eq!(head.id().as_str(), &head_sha);
    assert_eq!(head.display_str(), "v0.3.4");
}

// ============================================================================
// Stage D: raw-vs-resolved invariants
// ============================================================================

#[test]
fn raw_revision_id_equality_is_string_identity() {
    // RawRevisionId carries the YAML scalar verbatim; equality is string
    // identity, not any kind of commit-identity. Two distinct strings are
    // distinct values even if they would resolve to the same SHA in some
    // repo — resolution is the boundary that produces SHA-identity.
    use repoweave::vcs::RawRevisionId;
    let a = RawRevisionId::new("v1.0.0");
    let b = RawRevisionId::new("v1.0.0");
    let c = RawRevisionId::new("v2.0.0");
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn raw_revision_id_roundtrips_through_the_lock_codec() {
    // The parse boundary: a raw value serializes to a single scalar and
    // deserializes back into a RawRevisionId with the original string.
    use repoweave::vcs::RawRevisionId;
    let original = RawRevisionId::new("v0.3.4");
    let json = serde_json::to_string(&original).unwrap();
    let restored: RawRevisionId = serde_json::from_str(&json).unwrap();
    assert_eq!(restored, original);
    assert_eq!(restored.as_str(), "v0.3.4");
}

#[test]
fn raw_revision_id_tag_resolves_to_head_sha() {
    // The resolution boundary in miniature: given a repo whose HEAD is at
    // a tag `v1.0.0`, a `RawRevisionId::new("v1.0.0")` fed through
    // `Vcs::resolve_revision` produces a `ResolvedRevisionId` whose
    // canonical SHA matches `Vcs::head_revision`.
    use repoweave::vcs::RawRevisionId;
    let dir = init_repo();
    let p = dir.path();
    common::git_in(p, &["tag", "v1.0.0"]);

    let raw = RawRevisionId::new("v1.0.0");
    let vcs = git_vcs();
    let resolved = vcs.resolve_revision(p, raw.as_str()).unwrap();
    let head = vcs.head_revision(p).unwrap();
    assert_eq!(resolved, head);
    // Display preserves the tag form for human-readable output / lock writes.
    assert_eq!(resolved.display_str(), "v1.0.0");
}

// ============================================================================
// Vcs::resolve_branch_on_remote
// ============================================================================

/// Build a workspace with two repos: a "remote" with one commit on `main`,
/// and a "local" clone whose remote is named `remote_name`. Returns the
/// workspace tempdir and the path to the local clone.
fn repo_with_remote(remote_name: &str) -> (TempDir, std::path::PathBuf) {
    let workspace = common::tempdir().unwrap();
    let remote_path = workspace.path().join("remote");
    let local_path = workspace.path().join("local");

    // Build the remote repo with one commit on `main`.
    fs::create_dir_all(&remote_path).unwrap();
    common::git_in(&remote_path, &["init", "--initial-branch=main"]);
    common::git_in(&remote_path, &["config", "user.email", "test@test.com"]);
    common::git_in(&remote_path, &["config", "user.name", "Test"]);
    fs::write(remote_path.join("README.md"), "remote").unwrap();
    common::git_in(&remote_path, &["add", "."]);
    common::git_in(&remote_path, &["commit", "-m", "initial"]);

    // Clone into local with the requested remote name and fetch.
    let remote_url = common::file_url(&remote_path);
    let local_str = local_path.to_str().unwrap();
    common::git_in(
        workspace.path(),
        &["clone", "--origin", remote_name, &remote_url, local_str],
    );

    (workspace, local_path)
}

fn on_remote(branch: &str) -> RemoteRef {
    TrackingRef::parse(RawRefName::new(branch))
        .unwrap()
        .on_remote()
}

/// The one remote of a clone, as the seam hands it out.
fn only_remote(
    vcs: &dyn repoweave::vcs::Vcs,
    repo: &std::path::Path,
) -> repoweave::vcs::RemoteHandle {
    let mut remotes = vcs.recorded_urls(repo).unwrap();
    assert_eq!(remotes.len(), 1, "the fixture clone has one remote");
    remotes.pop().unwrap().remote
}

#[test]
fn resolve_branch_on_remote_resolves_against_the_remote_it_is_given() {
    let (_ws, local) = repo_with_remote("other");
    let expected_sha = common::git_in(&local, &["rev-parse", "other/main"]);

    let vcs = git_vcs();
    let remote = only_remote(vcs.as_ref(), &local);
    let resolved = vcs
        .resolve_branch_on_remote(&local, &remote, &on_remote("main"))
        .unwrap();

    assert_eq!(resolved.id().as_str(), &expected_sha);
    assert_eq!(resolved.display_str(), "other/main");
}

// ============================================================================
// Publish
// ============================================================================
//
// The `push_with_role` unit tests that lived here moved into `src/git.rs`'s
// `branch_model_tests` module when `push_with_role` was deleted.
// They could not stay: `push_ref`, its replacement,
// takes a `PublishRef`, and the only constructor for one is `pub(crate)` —
// an integration test cannot mint the value, which is the seal doing its
// job. `repo_with_bare_remote` went with them.

#[test]
fn resolve_branch_on_remote_missing_branch_errors() {
    // Remote exists but branch doesn't — also a clear error.
    let (_ws, local) = repo_with_remote("origin");

    let vcs = git_vcs();
    let remote = only_remote(vcs.as_ref(), &local);
    let result = vcs.resolve_branch_on_remote(&local, &remote, &on_remote("nonexistent-branch"));

    let err = result.expect_err("nonexistent branch on remote must error");
    assert!(
        matches!(err, VcsError::RevisionNotFound { ref rev, .. } if rev == "refs/remotes/origin/nonexistent-branch"),
        "expected RevisionNotFound for refs/remotes/origin/nonexistent-branch, got {err:?}"
    );
}

// ============================================================================
// Vcs::conflict_resolution_hint — per-op resume guidance text
// ============================================================================
//
// The hint text is embedded verbatim in sync's conflict-bail messages and is
// the user-visible "what do I type next?" for each VCS op. Lock in the
// per-op `--continue` command name and the canonical "git add <files>" step
// so a wording drift won't slip past CI.

#[test]
fn conflict_resolution_hint_rebase_does_not_spell_git_rebase_continue() {
    // Seam-rule: the VCS impl stops at staging for Rebase; rwv core appends
    // `rwv sync --continue` / `rwv land --continue`. The hint must NOT
    // spell `git rebase --continue` in operator-facing text.
    let vcs = git_vcs();
    let hint = vcs.conflict_resolution_hint(ConflictOp::Rebase);
    assert!(
        !hint.contains("git rebase --continue"),
        "rebase hint must NOT spell `git rebase --continue` (seam-rule violation); got: {hint:?}"
    );
    assert!(
        hint.contains("git add <files>"),
        "rebase hint must mention `git add <files>`; got: {hint:?}"
    );
    // Sanity: must not leak other ops' continue verbs.
    assert!(
        !hint.contains("git merge --continue"),
        "rebase hint leaked merge vocabulary: {hint:?}"
    );
    assert!(
        !hint.contains("git cherry-pick --continue"),
        "rebase hint leaked cherry-pick vocabulary: {hint:?}"
    );
}

#[test]
fn conflict_resolution_hint_merge_uses_git_merge_continue() {
    let vcs = git_vcs();
    let hint = vcs.conflict_resolution_hint(ConflictOp::Merge);
    assert!(
        hint.contains("git merge --continue"),
        "merge hint must mention `git merge --continue`; got: {hint:?}"
    );
    assert!(
        hint.contains("git add <files>"),
        "merge hint must mention `git add <files>`; got: {hint:?}"
    );
    assert!(
        !hint.contains("git rebase --continue"),
        "merge hint leaked rebase vocabulary: {hint:?}"
    );
    assert!(
        !hint.contains("git cherry-pick --continue"),
        "merge hint leaked cherry-pick vocabulary: {hint:?}"
    );
}

#[test]
fn conflict_resolution_hint_cherry_pick_uses_git_cherry_pick_continue() {
    let vcs = git_vcs();
    let hint = vcs.conflict_resolution_hint(ConflictOp::CherryPick);
    assert!(
        hint.contains("git cherry-pick --continue"),
        "cherry-pick hint must mention `git cherry-pick --continue`; got: {hint:?}"
    );
    assert!(
        hint.contains("git add <files>"),
        "cherry-pick hint must mention `git add <files>`; got: {hint:?}"
    );
    assert!(
        !hint.contains("git rebase --continue"),
        "cherry-pick hint leaked rebase vocabulary: {hint:?}"
    );
    assert!(
        !hint.contains("git merge --continue"),
        "cherry-pick hint leaked merge vocabulary: {hint:?}"
    );
}

#[test]
fn conflict_resolution_hint_does_not_mention_rwv_abort() {
    // The trait method returns only the *resolution* steps; the surrounding
    // sync.rs framing supplies the `rwv abort` rollback option. Keeping the
    // VCS-vocabulary text and the rwv-CLI text in their own layers is the
    // whole point of the trait — verify the separation.
    let vcs = git_vcs();
    for op in [
        ConflictOp::Rebase,
        ConflictOp::Merge,
        ConflictOp::CherryPick,
    ] {
        let hint = vcs.conflict_resolution_hint(op);
        assert!(
            !hint.contains("rwv abort"),
            "trait hint for {op:?} leaked rwv-CLI vocabulary: {hint:?}"
        );
        assert!(
            !hint.contains("rwv sync"),
            "trait hint for {op:?} leaked rwv-CLI vocabulary: {hint:?}"
        );
    }
}

// ============================================================================
// The lock merge driver's setup
// ============================================================================

fn setup(dir: &Path) -> LockDriverSetup {
    git_vcs().lock_driver_setup(dir).unwrap()
}

fn commit_attributes(dir: &Path, text: &str) {
    fs::write(dir.join(".gitattributes"), text).unwrap();
    common::git_in(dir, &["add", ".gitattributes"]);
    common::git_in(dir, &["commit", "-m", "attributes"]);
}

#[test]
fn the_declaration_is_read_from_the_checked_out_commit() {
    let dir = init_repo();
    let p = dir.path();
    assert_eq!(setup(p).declaration, LockDriverPart::Absent);

    fs::write(p.join(".gitattributes"), common::LOCK_DRIVER_ATTRIBUTE).unwrap();
    assert_eq!(
        setup(p).declaration,
        LockDriverPart::Absent,
        "an uncommitted line does not travel with the branch"
    );

    commit_attributes(p, common::LOCK_DRIVER_ATTRIBUTE);
    assert_eq!(setup(p).declaration, LockDriverPart::Current);
}

#[test]
fn an_unborn_branch_reads_the_working_declaration() {
    let dir = common::tempdir().unwrap();
    let p = dir.path();
    git_vcs().init_repo(p).unwrap();
    fs::write(p.join(".gitattributes"), common::LOCK_DRIVER_ATTRIBUTE).unwrap();
    assert_eq!(setup(p).declaration, LockDriverPart::Current);
}

#[test]
fn every_driver_an_earlier_release_assigned_the_lock_is_superseded() {
    for text in [
        "rwv.lock merge=rwv-ours\n",
        "rwv.lock merge=ours\n",
        "*.png binary\nrwv.lock merge=ours\nrwv.lock merge=rwv-lock\n",
    ] {
        let dir = init_repo();
        commit_attributes(dir.path(), text);
        assert_eq!(
            setup(dir.path()).declaration,
            LockDriverPart::Superseded,
            "{text:?}"
        );
    }
}

#[test]
fn the_definition_is_current_only_when_it_names_rwvs_command() {
    let dir = init_repo();
    let p = dir.path();
    assert_eq!(setup(p).definition, LockDriverPart::Absent);

    common::git_in(p, &["config", "merge.rwv-ours.driver", "true"]);
    assert_eq!(setup(p).definition, LockDriverPart::Superseded);

    git_vcs().define_lock_driver(p).unwrap();
    assert_eq!(setup(p).definition, LockDriverPart::Current);
    assert_eq!(
        common::git_in(p, &["config", "--get", "merge.rwv-lock.driver"]).trim(),
        common::LOCK_DRIVER_COMMAND
    );
    assert!(
        !common::git()
            .args(["config", "--get", "merge.rwv-ours.driver"])
            .current_dir(p)
            .status()
            .unwrap()
            .success(),
        "defining the driver replaces the earlier definition"
    );

    git_vcs().define_lock_driver(p).unwrap();
    assert_eq!(setup(p).definition, LockDriverPart::Current);

    common::git_in(p, &["config", "merge.rwv-lock.driver", "true"]);
    assert_eq!(
        setup(p).definition,
        LockDriverPart::Superseded,
        "a driver that keeps one side is not rwv's"
    );
}

#[test]
fn a_linked_worktree_shares_its_clones_definition() {
    let dir = init_repo();
    let p = dir.path();
    git_vcs().define_lock_driver(p).unwrap();
    let linked = dir.path().join("linked");
    common::git_in(
        p,
        &["worktree", "add", "-b", "other", linked.to_str().unwrap()],
    );
    assert_eq!(setup(&linked).definition, LockDriverPart::Current);
}

/// A repo whose lock both `main` and `feat` changed from their base, with a
/// lock merge driver that records each run in `driver-ran` and conflicts.
fn diverged_lock_with_a_recording_driver() -> (TempDir, repoweave::vcs::CommitId) {
    let dir = init_repo();
    let p = dir.path();
    fs::write(p.join(".gitattributes"), common::LOCK_DRIVER_ATTRIBUTE).unwrap();
    write_lock_marker(p, "base\n");
    common::git_in(p, &["add", "."]);
    common::git_in(p, &["commit", "-m", "base"]);
    common::git_in(p, &["checkout", "-q", "-b", "feat"]);
    write_lock_marker(p, "feat\n");
    common::git_in(p, &["commit", "-qam", "feat"]);
    let feat = repoweave::vcs::CommitId::parse(&common::git_in(p, &["rev-parse", "HEAD"])).unwrap();
    common::git_in(p, &["checkout", "-q", "main"]);
    write_lock_marker(p, "main\n");
    common::git_in(p, &["commit", "-qam", "main"]);
    common::git_in(
        p,
        &[
            "config",
            "merge.rwv-lock.driver",
            "echo ran >> driver-ran; exit 1",
        ],
    );
    (dir, feat)
}

#[test]
fn rwvs_own_merges_settle_the_lock_without_running_the_driver() {
    let (dir, feat) = diverged_lock_with_a_recording_driver();
    let p = dir.path();
    let main = repoweave::vcs::CommitId::parse(&common::git_in(p, &["rev-parse", "HEAD"])).unwrap();
    if let Some(trees) = git_vcs().tree_merges() {
        assert!(matches!(
            trees.merge_tree(p, &main, &feat).unwrap(),
            repoweave::vcs::TreeMerge::Clean(_)
        ));
    }
    let conflicted = git_vcs()
        .start_merge(
            p,
            &feat,
            repoweave::vcs::DerivedContentPolicy::vcs_default(),
        )
        .unwrap();
    assert!(conflicted.is_empty(), "{conflicted:?}");
    assert!(
        !p.join("driver-ran").exists(),
        "rwv writes the lock of a merge it makes; the driver must not run"
    );
}

fn diverged_in_a_bare_repo_declaring_a_derived_path(
) -> (TempDir, PathBuf, [repoweave::vcs::CommitId; 2]) {
    let dir = common::tempdir().unwrap();
    let work = dir.path().join("work");
    let bare = dir.path().join("bare.git");
    fs::create_dir(&work).unwrap();
    let p = work.as_path();
    common::git_in(p, &["init", "-q", "-b", "main"]);
    common::git_in(p, &["config", "user.email", "test@test.com"]);
    common::git_in(p, &["config", "user.name", "Test"]);
    fs::write(p.join(".gitattributes"), "DERIVED merge=rwv-ours\n").unwrap();
    fs::write(p.join("DERIVED"), "base\n").unwrap();
    fs::write(p.join("OTHER"), "base\n").unwrap();
    common::git_in(p, &["add", "."]);
    common::git_in(p, &["commit", "-qm", "base"]);
    common::git_in(p, &["checkout", "-qb", "feat"]);
    fs::write(p.join("DERIVED"), "feat\n").unwrap();
    fs::write(p.join("FEAT_ONLY"), "feat\n").unwrap();
    common::git_in(p, &["add", "."]);
    common::git_in(p, &["commit", "-qm", "feat"]);
    let feat = common::git_in(p, &["rev-parse", "HEAD"]);
    common::git_in(p, &["checkout", "-q", "main"]);
    fs::write(p.join("DERIVED"), "main\n").unwrap();
    fs::write(p.join("MAIN_ONLY"), "main\n").unwrap();
    common::git_in(p, &["add", "."]);
    common::git_in(p, &["commit", "-qm", "main"]);
    let main = common::git_in(p, &["rev-parse", "HEAD"]);
    common::git_in(
        dir.path(),
        &["clone", "-q", "--bare", "work", bare.to_str().unwrap()],
    );
    let parse = |id: String| repoweave::vcs::CommitId::parse(&id).unwrap();
    (dir, bare, [parse(main), parse(feat)])
}

#[test]
fn a_merge_as_refs_resolves_a_declared_derived_path_to_the_incoming_side() {
    let (_dir, bare, [main, feat]) = diverged_in_a_bare_repo_declaring_a_derived_path();
    for (first, second, incoming, want) in [
        (&main, &feat, &feat, "feat\n"),
        (&main, &feat, &main, "main\n"),
        (&feat, &main, &feat, "feat\n"),
        (&feat, &main, &main, "main\n"),
    ] {
        let merged = git_vcs()
            .merge_as_refs(
                &bare,
                first,
                second,
                "merge",
                repoweave::vcs::DerivedContentPolicy::keep_incoming_side(),
                incoming,
            )
            .unwrap();
        let repoweave::vcs::RefMerge::Merged(commit) = merged else {
            panic!("a declared derived path conflicts: {merged:?}");
        };
        assert_eq!(
            common::git_in(&bare, &["show", &format!("{}:DERIVED", commit.as_str())]),
            want.trim_end()
        );
        for only in ["FEAT_ONLY", "MAIN_ONLY"] {
            common::git_in(
                &bare,
                &["cat-file", "-e", &format!("{}:{only}", commit.as_str())],
            );
        }
    }
}

#[test]
fn a_merge_as_refs_under_the_default_policy_still_conflicts_on_a_derived_path() {
    let (_dir, bare, [main, feat]) = diverged_in_a_bare_repo_declaring_a_derived_path();
    let merged = git_vcs()
        .merge_as_refs(
            &bare,
            &main,
            &feat,
            "merge",
            repoweave::vcs::DerivedContentPolicy::vcs_default(),
            &feat,
        )
        .unwrap();
    assert!(
        matches!(merged, repoweave::vcs::RefMerge::Conflicted { .. }),
        "{merged:?}"
    );
}

#[test]
fn a_merge_as_refs_still_conflicts_on_an_undeclared_path_and_writes_no_ref() {
    let (dir, bare, _) = diverged_in_a_bare_repo_declaring_a_derived_path();
    let p = dir.path().join("work");
    fs::write(p.join("OTHER"), "main-other\n").unwrap();
    common::git_in(&p, &["commit", "-qam", "other on main"]);
    common::git_in(&p, &["checkout", "-q", "feat"]);
    fs::write(p.join("OTHER"), "feat-other\n").unwrap();
    common::git_in(&p, &["commit", "-qam", "other on feat"]);
    common::git_in(
        &bare,
        &[
            "fetch",
            "-q",
            p.to_str().unwrap(),
            "main:main",
            "+feat:feat",
        ],
    );
    let refs = common::git_in(&bare, &["for-each-ref"]);
    let tip = |name: &str| {
        repoweave::vcs::CommitId::parse(&common::git_in(&bare, &["rev-parse", name])).unwrap()
    };
    let (main, feat) = (tip("main"), tip("feat"));
    let merged = git_vcs()
        .merge_as_refs(
            &bare,
            &main,
            &feat,
            "merge",
            repoweave::vcs::DerivedContentPolicy::keep_incoming_side(),
            &feat,
        )
        .unwrap();
    assert_eq!(
        merged,
        repoweave::vcs::RefMerge::Conflicted {
            paths: vec!["OTHER".to_owned()]
        }
    );
    assert_eq!(common::git_in(&bare, &["for-each-ref"]), refs);
}

#[test]
fn reading_the_setup_outside_a_repo_is_an_error_not_an_absence() {
    let dir = common::tempdir().unwrap();
    match git_vcs().lock_driver_setup(dir.path()) {
        Err(VcsError::NotARepo(p)) => assert_eq!(p, dir.path()),
        other => panic!("expected NotARepo, got {other:?}"),
    }
}

// ============================================================================
// Vcs::rebase
// ============================================================================
//
// Native rebase consolidates sync's project-repo path. The three behaviors
// codified: clean rebase succeeds, a real conflict surfaces as
// VcsError::RebaseConflict (repo left mid-rebase for `git rebase --continue`),
// and an rwv.lock collision is silently auto-resolved when replay-exclusion
// is configured.

/// Build a repo with `main` at C1 and a `feat` branch diverged on a
/// non-conflicting path. Returns (tempdir, c1_sha).
fn diverged_repo() -> (TempDir, ResolvedRevisionId) {
    let dir = init_repo();
    let p = dir.path();
    let c1 = common::git_in(p, &["rev-parse", "HEAD"]);

    // main: advance with a new file `main.txt`.
    fs::write(p.join("main.txt"), "main\n").unwrap();
    common::git_in(p, &["add", "main.txt"]);
    common::git_in(p, &["commit", "-m", "main: advance"]);

    // feat branch from C1 with a new file `feat.txt`.
    common::git_in(p, &["checkout", "-b", "feat", &c1]);
    fs::write(p.join("feat.txt"), "feat\n").unwrap();
    common::git_in(p, &["add", "feat.txt"]);
    common::git_in(p, &["commit", "-m", "feat: advance"]);

    let c1 =
        ResolvedRevisionId::from_canonical(repoweave::vcs::CommitId::parse(&c1).unwrap(), None);
    (dir, c1)
}

#[test]
fn rebase_clean_advances_head_onto_target() {
    let (dir, _c1) = diverged_repo();
    let p = dir.path();
    let main_tip = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse(&common::git_in(p, &["rev-parse", "main"])).unwrap(),
        None,
    );

    // feat is checked out — rebase it onto main.
    git_vcs()
        .rebase(
            p,
            &main_tip,
            &main_tip,
            DerivedContentPolicy::keep_target_side(),
        )
        .unwrap();

    // feat's tip is now descended from main; both files exist.
    assert!(p.join("main.txt").exists());
    assert!(p.join("feat.txt").exists());
    let is_descendant = common::git()
        .args([
            "merge-base",
            "--is-ancestor",
            main_tip.id().as_str(),
            "HEAD",
        ])
        .current_dir(p)
        .status()
        .unwrap()
        .success();
    assert!(
        is_descendant,
        "feat's HEAD should be a descendant of main after rebase"
    );
}

fn commit_file(p: &Path, file: &str, content: &str, subject: &str, date: Option<&str>) -> String {
    fs::write(p.join(file), content).unwrap();
    common::git_in(p, &["add", file]);
    match date {
        Some(date) => common::git_in(p, &["commit", "-q", "-m", subject, "--date", date]),
        None => common::git_in(p, &["commit", "-q", "-m", subject]),
    };
    common::git_in(p, &["rev-parse", "HEAD"])
}

fn commit(id: &str) -> repoweave::vcs::CommitId {
    repoweave::vcs::CommitId::parse(id.trim()).unwrap()
}

fn resolved(id: &str) -> ResolvedRevisionId {
    ResolvedRevisionId::from_canonical(commit(id), None)
}

#[test]
fn a_rebase_moves_no_branch_but_head_s_under_the_operator_s_update_refs() {
    let (dir, c1) = diverged_repo();
    let p = dir.path();
    common::git_in(p, &["config", "rebase.updateRefs", "true"]);
    let feat = common::git_in(p, &["rev-parse", "feat"]);
    common::git_in(p, &["branch", "dep", "feat"]);
    let main_tip = resolved(&common::git_in(p, &["rev-parse", "main"]));
    git_vcs()
        .rebase(p, &main_tip, &c1, DerivedContentPolicy::keep_target_side())
        .unwrap();
    assert_ne!(common::git_in(p, &["rev-parse", "feat"]), feat);
    assert_eq!(common::git_in(p, &["rev-parse", "dep"]), feat);
}

#[test]
fn a_rebase_maps_what_it_recorded_where_two_commits_share_authorship_and_the_first_is_dropped() {
    let dir = init_repo();
    let p = dir.path();
    let base = common::git_in(p, &["rev-parse", "HEAD"]);
    common::git_in(p, &["checkout", "-q", "-b", "feat"]);
    let date = "1700000000 +0000";
    let first = commit_file(p, "one", "1\n", "wip", Some(date));
    let second = commit_file(p, "two", "2\n", "wip", Some(date));
    let old = common::git_in(p, &["rev-parse", "HEAD"]);
    common::git_in(p, &["checkout", "-q", "main"]);
    common::git_in(p, &["cherry-pick", &first]);
    let onto = common::git_in(p, &["rev-parse", "HEAD"]);
    common::git_in(p, &["checkout", "-q", "feat"]);
    git_vcs()
        .rebase(
            p,
            &resolved(&onto),
            &resolved(&base),
            DerivedContentPolicy::keep_target_side(),
        )
        .unwrap();
    let new = common::git_in(p, &["rev-parse", "HEAD"]);
    let replayed = git_vcs()
        .replayed(
            p,
            &commit(&base),
            &commit(&old),
            &commit(&onto),
            &commit(&new),
        )
        .unwrap()
        .expect("the rebase recorded what it replayed");
    assert_eq!(replayed.image(&commit(&first)), None);
    assert_eq!(replayed.image(&commit(&second)), Some(&commit(&new)));
}

#[test]
fn a_rebase_finished_by_hand_maps_as_recorded_and_an_amended_tip_has_no_map() {
    let dir = init_repo();
    let p = dir.path();
    let base = commit_file(p, "shared", "v0\n", "add shared", None);
    common::git_in(p, &["checkout", "-q", "-b", "feat"]);
    let mine = commit_file(p, "mine", "m\n", "mine", None);
    let clash = commit_file(p, "shared", "feat\n", "clash", None);
    let old = common::git_in(p, &["rev-parse", "HEAD"]);
    common::git_in(p, &["checkout", "-q", "main"]);
    let onto = commit_file(p, "shared", "main\n", "main", None);
    common::git_in(p, &["checkout", "-q", "feat"]);
    let stopped = git_vcs().rebase(
        p,
        &resolved(&onto),
        &resolved(&base),
        DerivedContentPolicy::keep_target_side(),
    );
    assert!(matches!(stopped, Err(VcsError::RebaseConflict { .. })));
    fs::write(p.join("shared"), "resolved\n").unwrap();
    common::git_in(p, &["add", "shared"]);
    common::git_in_configured(p, &[("core.editor", "true")], &["rebase", "--continue"]);
    let new = common::git_in(p, &["rev-parse", "HEAD"]);
    let replayed = git_vcs()
        .replayed(
            p,
            &commit(&base),
            &commit(&old),
            &commit(&onto),
            &commit(&new),
        )
        .unwrap()
        .expect("a continue by hand records what it replayed");
    assert_eq!(replayed.image(&commit(&clash)), Some(&commit(&new)));
    assert!(replayed.image(&commit(&mine)).is_some());
    fs::write(p.join("mine"), "typo fixed\n").unwrap();
    common::git_in(p, &["commit", "-q", "-a", "--amend", "--no-edit"]);
    let amended = common::git_in(p, &["rev-parse", "HEAD"]);
    assert_eq!(
        git_vcs()
            .replayed(
                p,
                &commit(&base),
                &commit(&old),
                &commit(&onto),
                &commit(&amended)
            )
            .unwrap(),
        None
    );
}

#[test]
fn a_replay_aborted_and_redone_by_hand_has_no_map() {
    let dir = init_repo();
    let p = dir.path();
    let base = commit_file(p, "shared", "v0\n", "add shared", None);
    common::git_in(p, &["checkout", "-q", "-b", "feat"]);
    commit_file(p, "shared", "feat\n", "clash", None);
    commit_file(p, "mine", "m\n", "mine", None);
    let old = common::git_in(p, &["rev-parse", "HEAD"]);
    common::git_in(p, &["checkout", "-q", "main"]);
    let onto = commit_file(p, "shared", "main\n", "main", None);
    common::git_in(p, &["checkout", "-q", "feat"]);
    let stopped = git_vcs().rebase(
        p,
        &resolved(&onto),
        &resolved(&base),
        DerivedContentPolicy::keep_target_side(),
    );
    assert!(matches!(stopped, Err(VcsError::RebaseConflict { .. })));
    common::git_in(p, &["rebase", "--abort"]);
    common::git_in(p, &["rebase", "-q", "-X", "theirs", "main"]);
    let new = common::git_in(p, &["rev-parse", "HEAD"]);
    assert_eq!(
        git_vcs()
            .replayed(
                p,
                &commit(&base),
                &commit(&old),
                &commit(&onto),
                &commit(&new)
            )
            .unwrap(),
        None
    );
}

#[test]
fn a_commit_git_leaves_out_of_the_replay_as_already_upstream_is_recorded_as_dropped() {
    let dir = init_repo();
    let p = dir.path();
    common::git_in(p, &["checkout", "-q", "-b", "feat"]);
    let first = commit_file(p, "one", "1\n", "one", None);
    let second = commit_file(p, "two", "2\n", "two", None);
    let old = common::git_in(p, &["rev-parse", "HEAD"]);
    common::git_in(p, &["checkout", "-q", "main"]);
    common::git_in(p, &["cherry-pick", "-x", &first]);
    let onto = common::git_in(p, &["rev-parse", "HEAD"]);
    common::git_in(p, &["checkout", "-q", "feat"]);
    git_vcs()
        .rebase(
            p,
            &resolved(&onto),
            &resolved(&onto),
            DerivedContentPolicy::keep_target_side(),
        )
        .unwrap();
    let new = common::git_in(p, &["rev-parse", "HEAD"]);
    let replayed = git_vcs()
        .replayed(
            p,
            &commit(&onto),
            &commit(&old),
            &commit(&onto),
            &commit(&new),
        )
        .unwrap()
        .expect("git's own todo records the commit it left out");
    assert_eq!(replayed.image(&commit(&first)), None);
    assert_eq!(replayed.image(&commit(&second)), Some(&commit(&new)));
}

#[test]
fn a_replay_rwv_did_not_start_has_no_map() {
    let (dir, c1) = diverged_repo();
    let p = dir.path();
    let old = common::git_in(p, &["rev-parse", "feat"]);
    let onto = common::git_in(p, &["rev-parse", "main"]);
    common::git_in(p, &["rebase", "-q", "main"]);
    let new = common::git_in(p, &["rev-parse", "HEAD"]);
    assert_eq!(
        git_vcs()
            .replayed(p, c1.id(), &commit(&old), &commit(&onto), &commit(&new))
            .unwrap(),
        None
    );
}

#[test]
fn rebase_conflict_on_non_lock_file_returns_rebase_conflict_and_leaves_mid_op() {
    // Build a repo where main and feat both modify the same line of `shared`.
    let dir = init_repo();
    let p = dir.path();
    fs::write(p.join("shared"), "v0\n").unwrap();
    common::git_in(p, &["add", "shared"]);
    common::git_in(p, &["commit", "-m", "add shared"]);
    let c1 = common::git_in(p, &["rev-parse", "HEAD"]);

    fs::write(p.join("shared"), "main version\n").unwrap();
    common::git_in(p, &["add", "shared"]);
    common::git_in(p, &["commit", "-m", "main: change shared"]);

    common::git_in(p, &["checkout", "-b", "feat", &c1]);
    fs::write(p.join("shared"), "feat version\n").unwrap();
    common::git_in(p, &["add", "shared"]);
    common::git_in(p, &["commit", "-m", "feat: change shared"]);

    let main_tip = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse(&common::git_in(p, &["rev-parse", "main"])).unwrap(),
        None,
    );

    let result = git_vcs().rebase(
        p,
        &main_tip,
        &main_tip,
        DerivedContentPolicy::keep_target_side(),
    );

    let err = result.expect_err("rebase with conflicting paths must surface an error");
    assert!(
        matches!(err, VcsError::RebaseConflict { ref op, .. } if *op == ConflictOp::Rebase),
        "expected RebaseConflict, got {err:?}"
    );
    // Repo is left mid-rebase so `git rebase --continue` works.
    let mid_op = repoweave::git::git_vcs().mid_operation(p);
    assert_eq!(
        mid_op,
        Some(MidOperation::Conflict(ConflictOp::Rebase)),
        "repo should be left mid-rebase for the operator to resolve and continue"
    );
}

/// Put a per-branch distinguishable body at the path a replay exclusion is
/// configured for.
///
/// What the rebase tests below drive is git's resolution of the `rwv.lock`
/// path, not the lock's content: the body only has to differ per side so the
/// side that survives is legible.
fn write_lock_marker(repo: &Path, body: &str) {
    // raw lock bytes: an arbitrary marker, because the path is the subject and
    // nothing on this route parses one. A real lock would make the surviving
    // side unreadable in the failure output.
    fs::write(repo.join("rwv.lock"), body).unwrap();
}

#[test]
fn rebase_auto_resolves_lock_collision_when_replay_exclusion_set() {
    // Both sides modify the same `rwv.lock` content; with replay-exclusion
    // configured, the rebase should keep main's version with no conflict.
    let dir = init_repo();
    let p = dir.path();
    write_lock_marker(p, "v0\n");
    common::git_in(p, &["add", "rwv.lock"]);
    // The driver is declared BEFORE the commits that mutate the lock.
    fs::write(p.join(".gitattributes"), common::LOCK_DRIVER_ATTRIBUTE).unwrap();
    common::git_in(p, &["add", ".gitattributes"]);
    common::git_in(p, &["commit", "-m", "lock + .gitattributes"]);
    let c1 = common::git_in(p, &["rev-parse", "HEAD"]);

    write_lock_marker(p, "main version\n");
    common::git_in(p, &["add", "rwv.lock"]);
    common::git_in(p, &["commit", "-m", "main: change lock"]);
    let main_lock_content = fs::read_to_string(p.join("rwv.lock")).unwrap();

    common::git_in(p, &["checkout", "-b", "feat", &c1]);
    write_lock_marker(p, "feat version\n");
    common::git_in(p, &["add", "rwv.lock"]);
    common::git_in(p, &["commit", "-m", "feat: change lock"]);

    let main_tip = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse(&common::git_in(p, &["rev-parse", "main"])).unwrap(),
        None,
    );

    git_vcs()
        .rebase(
            p,
            &main_tip,
            &main_tip,
            DerivedContentPolicy::keep_target_side(),
        )
        .expect("rebase should succeed thanks to replay-exclusion auto-resolve");

    // Working tree should hold MAIN's lock content — replay-exclusion keeps
    // the rebase target's version through every replayed commit.
    let final_lock = common::read_normalized(p.join("rwv.lock"));
    assert_eq!(
        final_lock, main_lock_content,
        "replay-exclusion should keep main's lock content after rebase; \
         got {final_lock:?}, expected {main_lock_content:?}"
    );
    // And the repo should not be left in a mid-op state.
    assert!(
        repoweave::git::git_vcs().mid_operation(p).is_none(),
        "successful rebase must leave repo in a clean state"
    );
}

// ============================================================================
// rebase_stopped_commit_detail
// ============================================================================

/// When a rebase is stopped mid-way by a conflict, `rebase_stopped_commit_detail`
/// returns a string containing the short SHA and subject of the stopped commit.
#[test]
fn rebase_stopped_commit_detail_returns_sha_and_subject() {
    // Set up a repo where feature and main conflict on shared.txt.
    let dir = init_repo();
    let p = dir.path();
    fs::write(p.join("shared.txt"), "base\n").unwrap();
    common::git_in(p, &["add", "shared.txt"]);
    common::git_in(p, &["commit", "-m", "add shared"]);
    let c1 = common::git_in(p, &["rev-parse", "HEAD"]);

    // main advances with a conflicting change.
    fs::write(p.join("shared.txt"), "main side\n").unwrap();
    common::git_in(p, &["add", "shared.txt"]);
    common::git_in(p, &["commit", "-m", "main: conflicting change"]);
    let main_tip = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse(&common::git_in(p, &["rev-parse", "main"])).unwrap(),
        None,
    );

    // feature branches off c1 and commits with a known subject.
    common::git_in(p, &["checkout", "-b", "feature", &c1]);
    fs::write(p.join("shared.txt"), "feature side\n").unwrap();
    common::git_in(p, &["add", "shared.txt"]);
    common::git_in(
        p,
        &[
            "commit",
            "-m",
            "lock: refresh — post-OOB drift in gc-formulas",
        ],
    );
    let feature_sha = common::git_in(p, &["rev-parse", "HEAD"]);

    // Rebase feature onto main — will conflict on shared.txt.
    let result = git_vcs().rebase(
        p,
        &main_tip,
        &main_tip,
        DerivedContentPolicy::keep_target_side(),
    );
    assert!(result.is_err(), "expected conflict on rebase");
    assert_eq!(
        git_vcs().mid_operation(p),
        Some(MidOperation::Conflict(ConflictOp::Rebase)),
        "repo must be mid-rebase after conflicting rebase"
    );

    // rebase_stopped_commit_detail must include the short SHA and subject.
    let detail = git_vcs().rebase_stopped_commit_detail(p);
    let short = &feature_sha[..7];
    assert!(
        detail.contains(short),
        "expected short SHA '{short}' in detail: {detail}"
    );
    assert!(
        detail.contains("lock: refresh"),
        "expected commit subject in detail: {detail}"
    );
    assert!(
        detail.contains("post-OOB drift"),
        "expected subject continuation in detail: {detail}"
    );
}

/// When the repo is NOT mid-rebase, `rebase_stopped_commit_detail` returns a
/// non-empty fallback string (no panic, no empty string).
#[test]
fn rebase_stopped_commit_detail_falls_back_when_no_rebase_in_progress() {
    let dir = init_repo();
    let p = dir.path();

    // Repo is clean (init_repo leaves it in a normal state).
    assert!(
        git_vcs().mid_operation(p).is_none(),
        "repo should not be mid-rebase after init"
    );

    let detail = git_vcs().rebase_stopped_commit_detail(p);
    assert!(
        !detail.is_empty(),
        "fallback must be non-empty; got: {detail}"
    );
    // Fallback must not start with "commit " since there is no stopped commit.
    assert!(
        !detail.starts_with("commit "),
        "unexpected SHA prefix in fallback: {detail}"
    );
}

// ============================================================================
// rebase_continue
// ============================================================================

/// Calling `rebase_continue` on a repo that is NOT mid-rebase must fail with
/// an error rather than silently no-op. Silent no-op would hide a caller bug:
/// the replay re-entry code routes between `Vcs::rebase` and
/// `Vcs::rebase_continue` by inspecting `mid_op`, so reaching this method on
/// a clean repo means the invariant was violated somewhere upstream.
#[test]
fn rebase_continue_on_clean_repo_returns_error_not_silent_noop() {
    let dir = init_repo();
    let p = dir.path();

    assert!(
        git_vcs().mid_operation(p).is_none(),
        "fixture precondition: repo must not be mid-rebase"
    );

    let err = git_vcs()
        .rebase_continue(p, DerivedContentPolicy::keep_target_side())
        .expect_err("rebase_continue on a clean repo must return an error");

    // The exact variant matters: it must be a structured error the sync layer
    // can distinguish from `RebaseConflict` (which is the operator-loop case
    // where more resolution is needed). We surface `CommandFailed` as the
    // generic "wrong state" class, consistent with `rebase`'s own fall-through
    // when the underlying command failed for a reason not further classified.
    assert!(
        matches!(err, VcsError::CommandFailed { .. }),
        "expected CommandFailed, got {err:?}"
    );

    // The HEAD must not have moved — the wrong-state error must be diagnostic,
    // not destructive.
    let head_after = common::git_in(p, &["rev-parse", "HEAD"]);
    assert_eq!(
        head_after,
        common::git_in(p, &["rev-parse", "HEAD"]),
        "HEAD must be stable across a wrong-state rebase_continue call"
    );
}

/// When the operator's resolution left conflict markers unstaged (or a
/// further pick brought its own conflict), `rebase_continue` must surface
/// `RebaseConflict` and leave the repo mid-rebase for another resolve-and-
/// continue cycle. Losing the mid-rebase state here would strand the
/// operator: `git rebase --continue` outside rwv would then bail with
/// "no rebase in progress" and abort would be the only escape.
#[test]
fn rebase_continue_with_unstaged_conflicts_bails_and_leaves_mid_rebase() {
    // Build a repo where main and feat both modify `shared`.
    let dir = init_repo();
    let p = dir.path();
    fs::write(p.join("shared"), "v0\n").unwrap();
    common::git_in(p, &["add", "shared"]);
    common::git_in(p, &["commit", "-m", "add shared"]);
    let c1 = common::git_in(p, &["rev-parse", "HEAD"]);

    fs::write(p.join("shared"), "main version\n").unwrap();
    common::git_in(p, &["add", "shared"]);
    common::git_in(p, &["commit", "-m", "main: change shared"]);

    common::git_in(p, &["checkout", "-b", "feat", &c1]);
    fs::write(p.join("shared"), "feat version\n").unwrap();
    common::git_in(p, &["add", "shared"]);
    common::git_in(p, &["commit", "-m", "feat: change shared"]);

    let main_tip = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse(&common::git_in(p, &["rev-parse", "main"])).unwrap(),
        None,
    );

    // First rebase stops on the conflict.
    let first = git_vcs().rebase(
        p,
        &main_tip,
        &main_tip,
        DerivedContentPolicy::keep_target_side(),
    );
    assert!(
        matches!(first, Err(VcsError::RebaseConflict { .. })),
        "first rebase must conflict; got {first:?}"
    );
    assert_eq!(
        git_vcs().mid_operation(p),
        Some(MidOperation::Conflict(ConflictOp::Rebase)),
        "fixture precondition: repo must be mid-rebase"
    );

    // Operator "resolves" by writing merged content but forgets to stage —
    // the working tree still shows the conflicted path as needing merge.
    // git refuses to continue in this state.
    fs::write(p.join("shared"), "merged\n").unwrap();
    // Deliberately no `git add`.

    let cont = git_vcs().rebase_continue(p, DerivedContentPolicy::keep_target_side());

    assert!(
        matches!(cont, Err(VcsError::RebaseConflict { ref op, .. }) if *op == ConflictOp::Rebase),
        "expected RebaseConflict, got {cont:?}"
    );
    assert_eq!(
        git_vcs().mid_operation(p),
        Some(MidOperation::Conflict(ConflictOp::Rebase)),
        "repo must still be mid-rebase after a failed continue so the \
         operator can stage-and-retry"
    );
}

/// Golden path at the Vcs level: after staging the operator's resolution,
/// `rebase_continue` drives the rebase to completion, leaves the repo clean,
/// and re-supplies the held lock merge driver inline so any remaining
/// lock-only pick is resolved to the target's version (no conflict on
/// `rwv.lock`) even when the durable driver config is unset. This is what
/// makes `rebase_continue` the equivalent of `rwv sync`'s fresh
/// `Vcs::rebase` invocation for resume purposes.
#[test]
fn rebase_continue_after_staging_completes_and_resolves_lock_pick_via_inline_flags() {
    let dir = init_repo();
    let p = dir.path();

    // Base commit: rwv.lock + shared exist, and `.gitattributes` assigns the
    // lock merge driver to `rwv.lock` (so committed trees carry it).
    write_lock_marker(p, "v0\n");
    fs::write(p.join("shared"), "v0\n").unwrap();
    common::git_in(p, &["add", "rwv.lock", "shared"]);
    fs::write(p.join(".gitattributes"), common::LOCK_DRIVER_ATTRIBUTE).unwrap();
    common::git_in(p, &["add", ".gitattributes"]);
    common::git_in(p, &["commit", "-m", "base + attrs"]);
    let c1 = common::git_in(p, &["rev-parse", "HEAD"]);

    // main: bump shared AND lock in one commit (both will conflict with feat).
    fs::write(p.join("shared"), "main version\n").unwrap();
    write_lock_marker(p, "main lock\n");
    common::git_in(p, &["add", "shared", "rwv.lock"]);
    common::git_in(p, &["commit", "-m", "main: bump shared + lock"]);
    let main_lock = fs::read_to_string(p.join("rwv.lock")).unwrap();

    // feat: F1 = shared conflict, then F2 = lock-only bump.
    common::git_in(p, &["checkout", "-b", "feat", &c1]);
    fs::write(p.join("shared"), "feat version\n").unwrap();
    common::git_in(p, &["add", "shared"]);
    common::git_in(p, &["commit", "-m", "F1: change shared"]);
    write_lock_marker(p, "feat lock\n");
    common::git_in(p, &["add", "rwv.lock"]);
    common::git_in(p, &["commit", "-m", "F2: bump lock only"]);

    let main_tip = ResolvedRevisionId::from_canonical(
        repoweave::vcs::CommitId::parse(&common::git_in(p, &["rev-parse", "main"])).unwrap(),
        None,
    );

    // Sanity: the durable merge-driver config MUST be unset, so the test
    // proves `rebase_continue` re-supplies the flag inline: the resume rung
    // must not silently depend on the plant any more than the initial
    // `Vcs::rebase` does.
    let pre = common::git()
        .args(["config", "--local", "--get", "merge.rwv-ours.driver"])
        .current_dir(p)
        .output()
        .expect("git config failed to spawn");
    assert!(
        !pre.status.success(),
        "fixture precondition: merge.rwv-ours.driver must be unset"
    );

    // Step 1: `Vcs::rebase` stops on F1's shared conflict.
    let first = git_vcs().rebase(
        p,
        &main_tip,
        &main_tip,
        DerivedContentPolicy::keep_target_side(),
    );
    assert!(
        matches!(first, Err(VcsError::RebaseConflict { .. })),
        "expected first rebase to conflict on shared; got {first:?}"
    );
    assert_eq!(
        git_vcs().mid_operation(p),
        Some(MidOperation::Conflict(ConflictOp::Rebase)),
        "repo must be mid-rebase after F1 conflict"
    );

    // Operator resolves and stages.
    fs::write(p.join("shared"), "merged version\n").unwrap();
    common::git_in(p, &["add", "shared"]);

    // Step 2: `rebase_continue` must apply the resolved F1 pick, then also
    // apply F2 (the lock-only pick) via the inline merge-driver flag — no
    // second conflict on rwv.lock.
    git_vcs()
        .rebase_continue(p, DerivedContentPolicy::keep_target_side())
        .expect("rebase_continue must complete after staging");

    // Repo is no longer mid-rebase.
    assert!(
        git_vcs().mid_operation(p).is_none(),
        "rebase must be complete after successful continue"
    );

    // Lock ended up as MAIN's version — the F2 lock-only pick merged to
    // "ours" via the inline driver flag (never touched the durable config).
    let final_lock = common::read_normalized(p.join("rwv.lock"));
    assert_eq!(
        final_lock, main_lock,
        "rwv.lock must be main's version after continue (inline rwv-ours flag)"
    );

    // And the resolved non-lock content survived.
    let final_shared = fs::read_to_string(p.join("shared")).unwrap();
    assert_eq!(final_shared, "merged version\n");
}

// ============================================================================
// discard_captured_change
// ============================================================================

fn status_of(repo: &Path) -> String {
    String::from_utf8(
        common::git()
            .args(["status", "--porcelain"])
            .current_dir(repo)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
}

/// Each shape of uncommitted change the working file is the whole of goes back
/// to the parent commit's version, or away where that commit has none.
#[test]
fn discard_captured_change_discards_a_change_the_working_file_is_all_of() {
    let vcs = git_vcs();

    let unstaged = init_repo();
    fs::write(unstaged.path().join("README.md"), "edited").unwrap();
    let staged = init_repo();
    fs::write(staged.path().join("README.md"), "edited").unwrap();
    common::git_in(staged.path(), &["add", "README.md"]);
    for repo in [&unstaged, &staged] {
        assert_eq!(
            vcs.discard_captured_change(repo.path(), Path::new("README.md"), b"edited")
                .unwrap(),
            DiscardOutcome::Discarded
        );
        assert_eq!(
            fs::read_to_string(repo.path().join("README.md")).unwrap(),
            "init"
        );
        assert_eq!(status_of(repo.path()), "");
    }

    let untracked = init_repo();
    fs::write(untracked.path().join("new.lock"), "fresh").unwrap();
    let added = init_repo();
    fs::write(added.path().join("new.lock"), "fresh").unwrap();
    common::git_in(added.path(), &["add", "new.lock"]);
    for repo in [&untracked, &added] {
        assert_eq!(
            vcs.discard_captured_change(repo.path(), Path::new("new.lock"), b"fresh")
                .unwrap(),
            DiscardOutcome::Discarded
        );
        assert!(!repo.path().join("new.lock").exists());
        assert_eq!(status_of(repo.path()), "");
    }
}

/// A byte the caller did not capture is never discarded: not one the working
/// file gained since, and not a staged version the working file does not show.
#[test]
fn discard_captured_change_never_discards_what_was_not_captured() {
    let vcs = git_vcs();

    let moved_on = init_repo();
    fs::write(moved_on.path().join("README.md"), "edited again").unwrap();
    assert_eq!(
        vcs.discard_captured_change(moved_on.path(), Path::new("README.md"), b"edited")
            .unwrap(),
        DiscardOutcome::ChangedSinceCapture
    );
    assert_eq!(
        fs::read_to_string(moved_on.path().join("README.md")).unwrap(),
        "edited again"
    );

    let staged_apart = init_repo();
    fs::write(staged_apart.path().join("README.md"), "staged").unwrap();
    common::git_in(staged_apart.path(), &["add", "README.md"]);
    fs::write(staged_apart.path().join("README.md"), "edited").unwrap();
    assert_eq!(
        vcs.discard_captured_change(staged_apart.path(), Path::new("README.md"), b"edited")
            .unwrap(),
        DiscardOutcome::HoldsUncapturedChange
    );
    assert_eq!(
        fs::read_to_string(staged_apart.path().join("README.md")).unwrap(),
        "edited"
    );
    assert_eq!(
        common::git_in(staged_apart.path(), &["show", ":README.md"]),
        "staged",
        "the staged version must survive"
    );
}

// ============================================================================
// prune_stale_worktree_registration
// ============================================================================

/// A store at `<tmp>/store` with a worktree registered at `<tmp>/wt`.
fn store_with_a_worktree() -> (TempDir, PathBuf, PathBuf) {
    let tmp = common::tempdir().unwrap();
    let store = tmp.path().join("store");
    fs::create_dir(&store).unwrap();
    common::git_in(&store, &["init"]);
    common::git_in(&store, &["config", "user.email", "test@test.com"]);
    common::git_in(&store, &["config", "user.name", "Test"]);
    common::git_in(&store, &["commit", "--allow-empty", "-m", "initial"]);
    let worktree = tmp.path().join("wt");
    common::git_in(
        &store,
        &["worktree", "add", "-b", "wt", worktree.to_str().unwrap()],
    );
    (tmp, store, worktree)
}

fn registers(store: &Path, worktree: &Path) -> bool {
    common::git_in(store, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|registered| Path::new(registered) == worktree)
}

#[test]
fn prune_drops_the_registration_of_a_removed_worktree() {
    let (_tmp, store, worktree) = store_with_a_worktree();
    fs::remove_dir_all(&worktree).unwrap();

    let outcome = git_vcs()
        .prune_stale_worktree_registration(&store, &worktree)
        .unwrap();

    assert_eq!(outcome, RegistrationPrune::Pruned);
    assert!(!registers(&store, &worktree));
    common::git_in(&store, &["branch", "-d", "wt"]);
}

#[test]
fn prune_keeps_a_locked_registration() {
    let (_tmp, store, worktree) = store_with_a_worktree();
    common::git_in(&store, &["worktree", "lock", worktree.to_str().unwrap()]);
    fs::remove_dir_all(&worktree).unwrap();

    let outcome = git_vcs()
        .prune_stale_worktree_registration(&store, &worktree)
        .unwrap();

    assert_eq!(outcome, RegistrationPrune::Locked);
    assert!(registers(&store, &worktree));
}

#[test]
fn prune_keeps_a_registration_whose_directory_stands() {
    let (_tmp, store, worktree) = store_with_a_worktree();
    fs::remove_file(worktree.join(".git")).unwrap();
    fs::write(worktree.join("work.txt"), "keep").unwrap();
    assert!(
        common::git_in(&store, &["worktree", "list", "--porcelain"]).contains("prunable"),
        "fixture: git calls the registration prunable"
    );

    let outcome = git_vcs()
        .prune_stale_worktree_registration(&store, &worktree)
        .unwrap();

    assert_eq!(outcome, RegistrationPrune::DirectoryPresent);
    assert!(registers(&store, &worktree));
    assert!(worktree.join("work.txt").exists());
}

#[test]
fn prune_of_an_unregistered_path_changes_nothing() {
    let (tmp, store, worktree) = store_with_a_worktree();
    fs::remove_dir_all(&worktree).unwrap();

    let outcome = git_vcs()
        .prune_stale_worktree_registration(&store, &tmp.path().join("elsewhere"))
        .unwrap();

    assert_eq!(outcome, RegistrationPrune::NotRegistered);
    assert!(registers(&store, &worktree));
}

// ============================================================================
// Vcs::stage_paths then Vcs::commit_only: a commit of named paths' working
// content, as `rwv init` makes one
// ============================================================================

fn status(repo: &Path) -> String {
    let out = common::git()
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

fn names_at_head(repo: &Path) -> String {
    common::git_in(repo, &["ls-tree", "-r", "--name-only", "HEAD"])
}

/// Another staged file, an untracked file, and `ours.txt` written for the
/// commit.
fn seed_unrelated_work(p: &Path) {
    fs::write(p.join("staged.txt"), "staged").unwrap();
    common::git_in(p, &["add", "staged.txt"]);
    fs::write(p.join("loose.txt"), "loose").unwrap();
    fs::write(p.join("ours.txt"), "ours").unwrap();
}

fn commit_ours(p: &Path) -> bool {
    let vcs = git_vcs();
    vcs.stage_paths(p, &["ours.txt"]).unwrap();
    vcs.commit_only(p, &["ours.txt"], "ours", &[]).unwrap()
}

#[test]
fn a_named_path_commit_leaves_other_staged_edited_and_untracked_work_as_it_was() {
    let dir = init_repo();
    let p = dir.path();
    seed_unrelated_work(p);
    fs::write(p.join("README.md"), "edited").unwrap();
    let parent = common::git_in(p, &["rev-parse", "HEAD"]);

    assert!(commit_ours(p));

    assert_eq!(common::git_in(p, &["rev-parse", "HEAD~1"]), parent);
    assert_eq!(names_at_head(p), "README.md\nours.txt");
    assert_eq!(common::git_in(p, &["show", "HEAD:README.md"]), "init");
    assert_eq!(status(p), " M README.md\nA  staged.txt\n?? loose.txt\n");
}

#[test]
fn a_named_path_commit_before_the_first_commit_leaves_other_staged_work_staged() {
    let dir = common::tempdir().unwrap();
    let p = dir.path();
    common::git_in(p, &["init", "-q", "-b", "main"]);
    common::git_in(p, &["config", "user.email", "test@test.com"]);
    common::git_in(p, &["config", "user.name", "Test"]);
    seed_unrelated_work(p);

    assert!(commit_ours(p));

    assert_eq!(common::git_in(p, &["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(names_at_head(p), "ours.txt");
    assert_eq!(status(p), "A  staged.txt\n?? loose.txt\n");
}
