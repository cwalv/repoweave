use repoweave::git::GitVcs;
use repoweave::vcs::{RevisionId, Vcs};
use std::fs;
use tempfile::TempDir;

mod common;

/// Create a fresh git repo in a temp directory with one initial commit.
fn init_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let p = dir.path();

    git(p, &["init"]);
    git(p, &["config", "user.email", "test@test.com"]);
    git(p, &["config", "user.name", "Test"]);

    fs::write(p.join("README.md"), "init").unwrap();
    git(p, &["add", "."]);
    git(p, &["commit", "-m", "initial"]);

    dir
}

/// Helper: run git in `dir` and panic on failure.
fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    if !output.status.success() {
        panic!(
            "git {:?} failed in {}: {}",
            args,
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

// ============================================================================
// has_uncommitted_changes
// ============================================================================

#[test]
fn has_uncommitted_changes_clean_repo() {
    let dir = init_repo();
    let vcs = GitVcs;
    assert!(!vcs.has_uncommitted_changes(dir.path()).unwrap());
}

#[test]
fn has_uncommitted_changes_staged_changes() {
    let dir = init_repo();
    let p = dir.path();

    fs::write(p.join("new.txt"), "staged content").unwrap();
    git(p, &["add", "new.txt"]);

    let vcs = GitVcs;
    assert!(vcs.has_uncommitted_changes(p).unwrap());
}

#[test]
fn has_uncommitted_changes_unstaged_modification() {
    let dir = init_repo();
    let p = dir.path();

    // Modify a tracked file without staging.
    fs::write(p.join("README.md"), "modified").unwrap();

    let vcs = GitVcs;
    assert!(vcs.has_uncommitted_changes(p).unwrap());
}

#[test]
fn has_uncommitted_changes_untracked_file() {
    let dir = init_repo();
    let p = dir.path();

    fs::write(p.join("untracked.txt"), "hello").unwrap();

    let vcs = GitVcs;
    assert!(vcs.has_uncommitted_changes(p).unwrap());
}

// ============================================================================
// tag_at_head
// ============================================================================

#[test]
fn tag_at_head_no_tag() {
    let dir = init_repo();
    let vcs = GitVcs;
    assert_eq!(vcs.tag_at_head(dir.path()).unwrap(), None);
}

#[test]
fn tag_at_head_lightweight_tag() {
    let dir = init_repo();
    let p = dir.path();

    git(p, &["tag", "v0.1.0"]);

    let vcs = GitVcs;
    let tag = vcs.tag_at_head(p).unwrap();
    assert_eq!(tag.as_ref().map(|t| t.as_str()), Some("v0.1.0"));
}

#[test]
fn tag_at_head_annotated_tag() {
    let dir = init_repo();
    let p = dir.path();

    git(p, &["tag", "-a", "v1.0.0", "-m", "release v1.0.0"]);

    let vcs = GitVcs;
    let tag = vcs.tag_at_head(p).unwrap();
    assert_eq!(tag.as_ref().map(|t| t.as_str()), Some("v1.0.0"));
}

#[test]
fn tag_at_head_tag_not_at_head() {
    let dir = init_repo();
    let p = dir.path();

    // Tag the first commit, then create a second commit so HEAD moves past the tag.
    git(p, &["tag", "v0.0.1"]);

    fs::write(p.join("second.txt"), "second").unwrap();
    git(p, &["add", "."]);
    git(p, &["commit", "-m", "second commit"]);

    let vcs = GitVcs;
    assert_eq!(vcs.tag_at_head(p).unwrap(), None);
}

#[test]
fn tag_at_head_multiple_tags() {
    let dir = init_repo();
    let p = dir.path();

    git(p, &["tag", "v1.0.0"]);
    git(p, &["tag", "release-1"]);

    let vcs = GitVcs;
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

    git(p, &["tag", "savepoint/2024-01-01-abc"]);

    let vcs = GitVcs;
    assert_eq!(vcs.tag_at_head(p).unwrap(), None);
}

#[test]
fn tag_at_head_skips_pre_op_only() {
    let dir = init_repo();
    let p = dir.path();

    git(p, &["tag", "rwv/pre-op/op-xyz"]);

    let vcs = GitVcs;
    assert_eq!(vcs.tag_at_head(p).unwrap(), None);
}

#[test]
fn tag_at_head_prefers_release_over_transient() {
    // Release-shape tag `v9.9.9` alongside transient savepoint tag — release wins.
    let dir = init_repo();
    let p = dir.path();

    git(p, &["tag", "savepoint/old"]);
    git(p, &["tag", "v9.9.9"]);

    let vcs = GitVcs;
    let tag = vcs.tag_at_head(p).unwrap();
    assert_eq!(tag.as_ref().map(|t| t.as_str()), Some("v9.9.9"));
}

#[test]
fn tag_at_head_prefers_release_over_lightweight() {
    // When both a release-shape tag and an arbitrary tag point at HEAD, the
    // release tag is preferred regardless of git's ordering.
    let dir = init_repo();
    let p = dir.path();

    git(p, &["tag", "aaa-arbitrary"]);
    git(p, &["tag", "v1.2.3"]);

    let vcs = GitVcs;
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

    git(p, &["tag", "savepoint/op-1"]);

    let vcs = GitVcs;
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

    git(p, &["tag", "savepoint/op-1"]);
    git(p, &["tag", "v9.9.9"]);

    let vcs = GitVcs;
    let head = vcs.head_revision(p).unwrap();
    assert_eq!(head.display_str(), "v9.9.9");
}

#[test]
fn revision_id_tag_form_equals_sha_form_after_resolve() {
    // Acceptance scenario (c) — at the RevisionId layer: a tag-form id
    // resolved against the repo compares equal to the head-form SHA id.
    let dir = init_repo();
    let p = dir.path();
    git(p, &["tag", "v1.0.0"]);

    let vcs = GitVcs;
    let head_sha_form = vcs.head_revision(p).unwrap();
    let tag_form_raw = RevisionId::raw("v1.0.0");
    // Raw tag-form does NOT compare equal to a SHA — that's the bug class.
    assert_ne!(tag_form_raw, head_sha_form);
    // After resolution it does — this is the comparison rwv check/status use.
    let tag_form_resolved = vcs.resolve_revision(p, "v1.0.0").unwrap();
    assert_eq!(tag_form_resolved, head_sha_form);
}

#[test]
fn has_uncommitted_changes_deleted_tracked_file() {
    let dir = init_repo();
    let p = dir.path();

    // Delete a tracked file without staging the deletion.
    fs::remove_file(p.join("README.md")).unwrap();

    let vcs = GitVcs;
    assert!(vcs.has_uncommitted_changes(p).unwrap());
}

#[test]
fn has_uncommitted_changes_gitignored_file() {
    let dir = init_repo();
    let p = dir.path();

    // Add a .gitignore, commit it, then create an ignored file.
    fs::write(p.join(".gitignore"), "*.log\n").unwrap();
    git(p, &["add", ".gitignore"]);
    git(p, &["commit", "-m", "add gitignore"]);

    fs::write(p.join("debug.log"), "some logs").unwrap();

    let vcs = GitVcs;
    // Ignored files should NOT count as uncommitted changes.
    assert!(!vcs.has_uncommitted_changes(p).unwrap());
}

#[test]
fn has_uncommitted_changes_staged_deletion() {
    let dir = init_repo();
    let p = dir.path();

    // Stage removal of a tracked file.
    git(p, &["rm", "README.md"]);

    let vcs = GitVcs;
    assert!(vcs.has_uncommitted_changes(p).unwrap());
}

// ============================================================================
// init_repo
// ============================================================================

#[test]
fn init_repo_creates_git_directory() {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path().join("new-repo");

    let vcs = GitVcs;
    vcs.init_repo(&repo_path).unwrap();

    assert!(
        repo_path.join(".git").exists(),
        "should create .git directory"
    );
}

#[test]
fn init_repo_sets_main_as_initial_branch() {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path().join("new-repo");

    let vcs = GitVcs;
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
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path().join("a").join("b").join("c").join("repo");

    let vcs = GitVcs;
    vcs.init_repo(&repo_path).unwrap();

    assert!(
        repo_path.join(".git").exists(),
        "should create nested dirs and init repo"
    );
}

#[test]
fn init_repo_is_recognized_by_is_repo() {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path().join("new-repo");

    let vcs = GitVcs;
    vcs.init_repo(&repo_path).unwrap();

    assert!(
        vcs.is_repo(&repo_path),
        "init_repo result should be recognized as a repo"
    );
}

// ============================================================================
// RevisionId — typed identity, equality, and serialization
// ============================================================================

#[test]
fn revision_id_raw_canonical_equals_input() {
    let r = RevisionId::raw("abc123");
    assert_eq!(r.as_str(), "abc123");
    assert_eq!(r.display_str(), "abc123");
}

#[test]
fn revision_id_from_canonical_with_display_form() {
    let r = RevisionId::from_canonical(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Some("v1.0.0".to_string()),
    );
    assert_eq!(r.as_str(), "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(r.display_str(), "v1.0.0");
}

#[test]
fn revision_id_from_canonical_suppresses_redundant_display() {
    // When `display` equals canonical, suppress it so serialization is clean.
    let r = RevisionId::from_canonical("abc123", Some("abc123".to_string()));
    assert_eq!(r.as_str(), "abc123");
    assert_eq!(r.display_str(), "abc123");
}

#[test]
fn revision_id_equality_compares_canonical() {
    // Tag-form and SHA-form referring to the same canonical commit compare equal.
    let sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let tag_form = RevisionId::from_canonical(sha.clone(), Some("v1.0.0".to_string()));
    let sha_form = RevisionId::from_canonical(sha.clone(), None);
    assert_eq!(tag_form, sha_form);

    // Different canonical SHAs are never equal, even with matching display.
    let other = RevisionId::from_canonical(
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        Some("v1.0.0".to_string()),
    );
    assert_ne!(tag_form, other);
}

#[test]
fn revision_id_serialize_prefers_display() {
    let r = RevisionId::from_canonical(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Some("v1.0.0".to_string()),
    );
    let yaml = serde_yaml::to_string(&r).unwrap();
    // Single transparent string; serializing yields the display form.
    assert_eq!(yaml.trim(), "v1.0.0");
}

#[test]
fn revision_id_serialize_canonical_when_no_display() {
    let r = RevisionId::raw("abc123");
    let yaml = serde_yaml::to_string(&r).unwrap();
    assert_eq!(yaml.trim(), "abc123");
}

#[test]
fn revision_id_deserialize_produces_raw_form() {
    // Without a repo to resolve against, deserialization lands the string in
    // `canonical` with `display: None` — resolution to a real SHA happens
    // later via Vcs::resolve_revision.
    let r: RevisionId = serde_yaml::from_str("v1.0.0").unwrap();
    assert_eq!(r.as_str(), "v1.0.0");
    assert_eq!(r.display_str(), "v1.0.0");
}

#[test]
fn revision_id_round_trip_yaml_string() {
    let original = RevisionId::from_canonical(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Some("v1.0.0".to_string()),
    );
    let yaml = serde_yaml::to_string(&original).unwrap();
    let restored: RevisionId = serde_yaml::from_str(&yaml).unwrap();
    // After round-trip we have only the display form; canonical equals it.
    assert_eq!(restored.as_str(), "v1.0.0");
    assert_eq!(restored.display_str(), "v1.0.0");
}

// ============================================================================
// Vcs::resolve_revision — fills in canonical SHA, preserves display form
// ============================================================================

#[test]
fn resolve_revision_tag_to_canonical_sha() {
    let dir = init_repo();
    let p = dir.path();
    git(p, &["tag", "v1.0.0"]);
    let head_sha = git(p, &["rev-parse", "HEAD"]);

    let vcs = GitVcs;
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
    let head_sha = git(p, &["rev-parse", "HEAD"]);

    let vcs = GitVcs;
    let resolved = vcs.resolve_revision(p, &head_sha).unwrap();
    assert_eq!(resolved.as_str(), &head_sha);
    // SHA input — no separate display form.
    assert_eq!(resolved.display_str(), &head_sha);
}

#[test]
fn resolve_revision_unknown_revision_errors() {
    let dir = init_repo();
    let vcs = GitVcs;
    let result = vcs.resolve_revision(dir.path(), "v9.9.9-nope");
    assert!(result.is_err());
}

#[test]
fn resolve_revision_then_equal_to_head_revision() {
    // Equality between the tag-form lock entry's resolved RevisionId and
    // the HEAD's RevisionId — the canonical-SHA equality the bead requires.
    let dir = init_repo();
    let p = dir.path();
    git(p, &["tag", "v0.3.4"]);

    let vcs = GitVcs;
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
    git(p, &["tag", "v0.3.4"]);

    let vcs = GitVcs;
    let head = vcs.head_revision(p).unwrap();
    let head_sha = git(p, &["rev-parse", "HEAD"]);
    assert_eq!(head.as_str(), &head_sha);
    assert_eq!(head.display_str(), "v0.3.4");
}
