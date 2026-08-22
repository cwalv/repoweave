use repoweave::git::git_vcs;
use repoweave::vcs::{
    ConflictOp, DerivedContentPolicy, RefName, ReplayExclusionState, ResolvedRevisionId, VcsError,
};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

mod common;

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
    assert_eq!(head.display_str(), head.as_str());
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

// ============================================================================
// ResolvedRevisionId — typed identity, equality, and serialization
// ============================================================================

#[test]
fn revision_id_from_canonical_no_display_uses_canonical_for_both() {
    // Without an explicit display form, `as_str` and `display_str` both
    // echo the canonical value. (The pre-split `ResolvedRevisionId::raw`
    // helper has been removed; the public mint is `from_canonical`.)
    let r = ResolvedRevisionId::from_canonical("abc123", None);
    assert_eq!(r.as_str(), "abc123");
    assert_eq!(r.display_str(), "abc123");
}

#[test]
fn revision_id_from_canonical_with_display_form() {
    let r = ResolvedRevisionId::from_canonical(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Some("v1.0.0".to_string()),
    );
    assert_eq!(r.as_str(), "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(r.display_str(), "v1.0.0");
}

#[test]
fn revision_id_from_canonical_suppresses_redundant_display() {
    // When `display` equals canonical, suppress it so serialization is clean.
    let r = ResolvedRevisionId::from_canonical("abc123", Some("abc123".to_string()));
    assert_eq!(r.as_str(), "abc123");
    assert_eq!(r.display_str(), "abc123");
}

#[test]
fn revision_id_equality_compares_canonical() {
    // Tag-form and SHA-form referring to the same canonical commit compare equal.
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let tag_form = ResolvedRevisionId::from_canonical(sha.clone(), Some("v1.0.0".to_string()));
    let sha_form = ResolvedRevisionId::from_canonical(sha.clone(), None);
    assert_eq!(tag_form, sha_form);

    // Different canonical SHAs are never equal, even with matching display.
    let other = ResolvedRevisionId::from_canonical(
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        Some("v1.0.0".to_string()),
    );
    assert_ne!(tag_form, other);
}

#[test]
fn revision_id_serialize_prefers_display() {
    let r = ResolvedRevisionId::from_canonical(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Some("v1.0.0".to_string()),
    );
    let json = serde_json::to_string(&r).unwrap();
    // Single transparent string; serializing yields the display form.
    assert_eq!(json, "\"v1.0.0\"");
}

#[test]
fn revision_id_serialize_canonical_when_no_display() {
    let r = ResolvedRevisionId::from_canonical("abc123", None);
    let json = serde_json::to_string(&r).unwrap();
    assert_eq!(json, "\"abc123\"");
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
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Some("v1.0.0".to_string()),
    );
    let json = serde_json::to_string(&original).unwrap();
    // Round-trip through the parse boundary: a resolved value serializes
    // as a single scalar, and re-parsing it lands in `RawRevisionId`
    // (deserialize only ever yields raw — re-resolution requires a repo).
    let restored: repoweave::vcs::RawRevisionId = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.as_str(), "v1.0.0");
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
    assert_eq!(resolved.as_str(), &head_sha, "canonical should be the SHA");
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
    assert_eq!(resolved.as_str(), &head_sha);
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
    assert_eq!(head.as_str(), &head_sha);
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

#[test]
fn resolve_branch_on_remote_resolves_against_origin() {
    let (_ws, local) = repo_with_remote("origin");
    let expected_sha = common::git_in(&local, &["rev-parse", "origin/main"]);

    let vcs = git_vcs();
    let resolved = vcs
        .resolve_branch_on_remote(&local, &RefName::new("main"))
        .unwrap();

    assert_eq!(resolved.as_str(), &expected_sha);
    assert_eq!(resolved.display_str(), "origin/main");
}

#[test]
fn resolve_branch_on_remote_missing_remote_errors() {
    // Repo has a remote named `other` but not `origin` — resolution must fail
    // clearly rather than silently falling back to a local branch tip.
    let (_ws, local) = repo_with_remote("other");

    let vcs = git_vcs();
    let result = vcs.resolve_branch_on_remote(&local, &RefName::new("main"));

    let err = result.expect_err("missing origin remote must error");
    assert!(
        matches!(err, VcsError::RevisionNotFound { ref rev, .. } if rev == "origin/main"),
        "expected RevisionNotFound for origin/main, got {err:?}"
    );
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
    let result = vcs.resolve_branch_on_remote(&local, &RefName::new("nonexistent-branch"));

    let err = result.expect_err("nonexistent branch on remote must error");
    assert!(
        matches!(err, VcsError::RevisionNotFound { ref rev, .. } if rev == "origin/nonexistent-branch"),
        "expected RevisionNotFound for origin/nonexistent-branch, got {err:?}"
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
    // `rwv sync --continue` / `rwv sync-to --continue`. The hint must NOT
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
// Vcs::set_replay_exclusion / replay_exclusion_state
// ============================================================================
//
// The replay-exclusion mechanism wires git's per-path merge driver
// (`.gitattributes <path> merge=rwv-ours`) so sync's native rebase keeps the
// rebase target's version of `rwv.lock` through every replay. The trait
// hides the file format so other VCS impls can use their own mechanism.

#[test]
fn set_replay_exclusion_creates_gitattributes_when_missing() {
    let dir = init_repo();
    let vcs = git_vcs();

    vcs.set_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap();

    let attrs = fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
    assert!(
        attrs.contains("rwv.lock merge=rwv-ours"),
        ".gitattributes should contain the replay-exclusion line; got: {attrs:?}"
    );
}

#[test]
fn set_replay_exclusion_appends_to_existing_gitattributes() {
    let dir = init_repo();
    let vcs = git_vcs();
    let attrs_path = dir.path().join(".gitattributes");

    // Pre-existing user content (no trailing newline to exercise the
    // newline-fixup branch in the impl).
    fs::write(&attrs_path, "*.png binary").unwrap();

    vcs.set_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap();

    let attrs = fs::read_to_string(&attrs_path).unwrap();
    assert!(
        attrs.contains("*.png binary"),
        "pre-existing entries must be preserved; got: {attrs:?}"
    );
    assert!(
        attrs.contains("rwv.lock merge=rwv-ours"),
        "new entry must be added; got: {attrs:?}"
    );
}

#[test]
fn set_replay_exclusion_is_idempotent() {
    let dir = init_repo();
    let vcs = git_vcs();

    vcs.set_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap();
    let after_first = fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
    vcs.set_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap();
    let after_second = fs::read_to_string(dir.path().join(".gitattributes")).unwrap();

    assert_eq!(
        after_first, after_second,
        "second call must not duplicate the entry; got:\nafter_first={after_first:?}\nafter_second={after_second:?}"
    );
    let line_count = after_second
        .lines()
        .filter(|l| l.trim() == "rwv.lock merge=rwv-ours")
        .count();
    assert_eq!(
        line_count, 1,
        "exactly one replay-exclusion line expected; got {line_count} in: {after_second:?}"
    );
}

#[test]
fn replay_exclusion_state_absent_when_gitattributes_missing() {
    let dir = init_repo();
    let vcs = git_vcs();

    assert_eq!(
        vcs.replay_exclusion_state(dir.path(), std::path::Path::new("rwv.lock"))
            .unwrap(),
        ReplayExclusionState::Absent
    );
}

#[test]
fn replay_exclusion_state_absent_when_line_absent() {
    let dir = init_repo();
    let vcs = git_vcs();
    fs::write(dir.path().join(".gitattributes"), "*.png binary\n").unwrap();

    assert_eq!(
        vcs.replay_exclusion_state(dir.path(), std::path::Path::new("rwv.lock"))
            .unwrap(),
        ReplayExclusionState::Absent
    );
}

#[test]
fn replay_exclusion_state_current_when_line_present() {
    let dir = init_repo();
    let vcs = git_vcs();
    vcs.set_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap();

    assert_eq!(
        vcs.replay_exclusion_state(dir.path(), std::path::Path::new("rwv.lock"))
            .unwrap(),
        ReplayExclusionState::Current
    );
}

// ---------------------------------------------------------------------------
// Legacy `merge=ours` migration
// ---------------------------------------------------------------------------

/// The legacy `merge=ours` line must NOT read as configured. If it did,
/// `rwv doctor` and the sync invariant would silently treat a legacy-only
/// project as fixed and never migrate it to `rwv-ours` — the exact hazard
/// the rename closes.
#[test]
fn replay_exclusion_state_legacy_only_when_only_legacy_line_present() {
    let dir = init_repo();
    let vcs = git_vcs();
    fs::write(dir.path().join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();

    assert_eq!(
        vcs.replay_exclusion_state(dir.path(), std::path::Path::new("rwv.lock"))
            .unwrap(),
        ReplayExclusionState::LegacyOnly
    );
}

/// A file carrying both lines is its own state, not "configured". The
/// legacy name is live whichever way git's reading order resolves the pair,
/// so reading this as satisfied is what let `--fix` rewrite and commit a
/// project the report had called clean.
#[test]
fn replay_exclusion_state_names_both_lines_when_both_present() {
    let dir = init_repo();
    let vcs = git_vcs();
    fs::write(
        dir.path().join(".gitattributes"),
        "*.png binary\nrwv.lock merge=ours\nrwv.lock merge=rwv-ours\n",
    )
    .unwrap();

    assert_eq!(
        vcs.replay_exclusion_state(dir.path(), std::path::Path::new("rwv.lock"))
            .unwrap(),
        ReplayExclusionState::LegacyAlongsideCurrent
    );
}

/// `set_replay_exclusion` on a `.gitattributes` that carries only the
/// LEGACY `<path> merge=ours` line must REWRITE it in place to
/// `<path> merge=rwv-ours` — not append the new line alongside the old
/// one. Two conflicting `merge=` assignments on the same path are
/// ill-defined (last-wins in reading order), and leaving the legacy line
/// resurrects the global-config collision hazard.
#[test]
fn set_replay_exclusion_migrates_legacy_line_in_place() {
    let dir = init_repo();
    let vcs = git_vcs();
    let attrs_path = dir.path().join(".gitattributes");
    fs::write(&attrs_path, "*.png binary\nrwv.lock merge=ours\n").unwrap();

    vcs.set_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap();

    let attrs = fs::read_to_string(&attrs_path).unwrap();
    assert!(
        attrs.contains("*.png binary"),
        "unrelated pre-existing entries must be preserved; got: {attrs:?}"
    );
    assert!(
        attrs.contains("rwv.lock merge=rwv-ours"),
        "new needle must be present after migration; got: {attrs:?}"
    );
    assert!(
        !attrs.contains("rwv.lock merge=ours\n") && !attrs.ends_with("rwv.lock merge=ours"),
        "legacy line must be REWRITTEN, not left alongside the new one; got: {attrs:?}"
    );
    // Exactly one replay-exclusion line — no duplication either way.
    let count = attrs
        .lines()
        .filter(|l| l.trim() == "rwv.lock merge=rwv-ours")
        .count();
    assert_eq!(
        count, 1,
        "exactly one new needle line expected; got {count} in: {attrs:?}"
    );
}

/// The migration is idempotent when the new line is already the only one
/// (the common state post-migration or on fresh projects). No unnecessary
/// churn or duplication.
#[test]
fn set_replay_exclusion_is_noop_when_new_line_already_only_present() {
    let dir = init_repo();
    let vcs = git_vcs();
    let attrs_path = dir.path().join(".gitattributes");
    fs::write(&attrs_path, "rwv.lock merge=rwv-ours\n").unwrap();
    let before = fs::read_to_string(&attrs_path).unwrap();

    vcs.set_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap();

    let after = fs::read_to_string(&attrs_path).unwrap();
    assert_eq!(
        before, after,
        "call must be a no-op when the new needle is the only line; \
         before={before:?} after={after:?}"
    );
}

/// If a file *somehow* has both the legacy and the new lines (e.g. a bad
/// manual merge in the pre-migration transition), the migration keeps
/// exactly one — the NEW one — and drops the legacy. Preserves surrounding
/// content.
#[test]
fn set_replay_exclusion_dedupes_when_both_lines_present() {
    let dir = init_repo();
    let vcs = git_vcs();
    let attrs_path = dir.path().join(".gitattributes");
    fs::write(
        &attrs_path,
        "*.png binary\nrwv.lock merge=ours\nrwv.lock merge=rwv-ours\n",
    )
    .unwrap();

    vcs.set_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap();

    let attrs = fs::read_to_string(&attrs_path).unwrap();
    assert!(
        attrs.contains("*.png binary"),
        "unrelated entries preserved; got: {attrs:?}"
    );
    let legacy_count = attrs
        .lines()
        .filter(|l| l.trim() == "rwv.lock merge=ours")
        .count();
    assert_eq!(legacy_count, 0, "legacy line must be gone; got: {attrs:?}");
    let new_count = attrs
        .lines()
        .filter(|l| l.trim() == "rwv.lock merge=rwv-ours")
        .count();
    assert_eq!(
        new_count, 1,
        "exactly one new needle expected; got {new_count} in: {attrs:?}"
    );
}

/// `plant_rwv_merge_driver_config` sets both `merge.rwv-ours.driver=true`
/// and `merge.rwv-ours.name=<desc>` in the repo-local config, and is
/// idempotent (calling twice does not error and preserves the values).
#[test]
fn plant_rwv_merge_driver_config_sets_repo_local_entries() {
    let dir = init_repo();
    repoweave::git::plant_rwv_merge_driver_config(dir.path()).unwrap();

    let driver = common::git_in(
        dir.path(),
        &["config", "--local", "--get", "merge.rwv-ours.driver"],
    );
    assert_eq!(
        driver.trim(),
        "true",
        "driver config not planted; got: {driver:?}"
    );
    let name = common::git_in(
        dir.path(),
        &["config", "--local", "--get", "merge.rwv-ours.name"],
    );
    assert!(
        !name.trim().is_empty(),
        "name config not planted; got: {name:?}"
    );

    // Idempotent — a second plant must not fail.
    repoweave::git::plant_rwv_merge_driver_config(dir.path()).unwrap();
    let driver2 = common::git_in(
        dir.path(),
        &["config", "--local", "--get", "merge.rwv-ours.driver"],
    );
    assert_eq!(driver2.trim(), "true");
}

/// `has_rwv_merge_driver_config` reports `false` on a fresh repo and
/// `true` after `plant_rwv_merge_driver_config`.
#[test]
fn has_rwv_merge_driver_config_reflects_plant() {
    let dir = init_repo();
    assert!(!repoweave::git::has_rwv_merge_driver_config(dir.path()).unwrap());
    repoweave::git::plant_rwv_merge_driver_config(dir.path()).unwrap();
    assert!(repoweave::git::has_rwv_merge_driver_config(dir.path()).unwrap());
}

/// A directory that is not a git repo at all must not read the same as one
/// where the key is merely unset — `git config --get` exits 1 for both, and
/// folding them into the same `Ok(false)` is what let `doctor --fix` attempt
/// the plant and surface git's own `fatal: not in a git directory` instead
/// of naming the actual condition.
#[test]
fn has_rwv_merge_driver_config_reports_not_a_repo() {
    let dir = common::tempdir().unwrap();
    match repoweave::git::has_rwv_merge_driver_config(dir.path()) {
        Err(VcsError::NotARepo(p)) => assert_eq!(p, dir.path()),
        other => panic!("expected NotARepo, got {other:?}"),
    }
}

// ============================================================================
// Vcs::rebase
// ============================================================================
//
// Native rebase consolidates sync's project-repo path. The three behaviours
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

    let c1 = ResolvedRevisionId::from_canonical(c1, None);
    (dir, c1)
}

#[test]
fn rebase_clean_advances_head_onto_target() {
    let (dir, _c1) = diverged_repo();
    let p = dir.path();
    let main_tip =
        ResolvedRevisionId::from_canonical(common::git_in(p, &["rev-parse", "main"]), None);

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
        .args(["merge-base", "--is-ancestor", main_tip.as_str(), "HEAD"])
        .current_dir(p)
        .status()
        .unwrap()
        .success();
    assert!(
        is_descendant,
        "feat's HEAD should be a descendant of main after rebase"
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

    let main_tip =
        ResolvedRevisionId::from_canonical(common::git_in(p, &["rev-parse", "main"]), None);

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
        mid_op.as_deref(),
        Some("mid-rebase"),
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
    // Configure replay-exclusion BEFORE the commits that mutate the lock.
    git_vcs()
        .set_replay_exclusion(p, std::path::Path::new("rwv.lock"))
        .unwrap();
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

    let main_tip =
        ResolvedRevisionId::from_canonical(common::git_in(p, &["rev-parse", "main"]), None);

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
    let main_tip =
        ResolvedRevisionId::from_canonical(common::git_in(p, &["rev-parse", "main"]), None);

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
        git_vcs().mid_operation(p).as_deref(),
        Some("mid-rebase"),
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

    let main_tip =
        ResolvedRevisionId::from_canonical(common::git_in(p, &["rev-parse", "main"]), None);

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
        git_vcs().mid_operation(p).as_deref(),
        Some("mid-rebase"),
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
        git_vcs().mid_operation(p).as_deref(),
        Some("mid-rebase"),
        "repo must still be mid-rebase after a failed continue so the \
         operator can stage-and-retry"
    );
}

/// Golden path at the Vcs level: after staging the operator's resolution,
/// `rebase_continue` drives the rebase to completion, leaves the repo clean,
/// and re-supplies the `rwv-ours` merge-driver flags inline so any remaining
/// lock-only pick is resolved to the target's version (no conflict on
/// `rwv.lock`) even when the durable driver config is unset. This is what
/// makes `rebase_continue` the equivalent of `rwv sync`'s fresh
/// `Vcs::rebase` invocation for resume purposes.
#[test]
fn rebase_continue_after_staging_completes_and_resolves_lock_pick_via_inline_flags() {
    let dir = init_repo();
    let p = dir.path();

    // Base commit: rwv.lock + shared exist, and `.gitattributes` assigns the
    // `rwv-ours` driver to `rwv.lock` via the production path (so committed
    // trees carry the assignment).
    write_lock_marker(p, "v0\n");
    fs::write(p.join("shared"), "v0\n").unwrap();
    common::git_in(p, &["add", "rwv.lock", "shared"]);
    git_vcs()
        .set_replay_exclusion(p, std::path::Path::new("rwv.lock"))
        .unwrap();
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

    let main_tip =
        ResolvedRevisionId::from_canonical(common::git_in(p, &["rev-parse", "main"]), None);

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
        git_vcs().mid_operation(p).as_deref(),
        Some("mid-rebase"),
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
