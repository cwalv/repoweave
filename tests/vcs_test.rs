use repoweave::git::GitVcs;
use repoweave::manifest::Role;
use repoweave::vcs::{ConflictOp, RefName, ResolvedRevisionId, Vcs, VcsError};
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
    // Acceptance scenario (c) — at the ResolvedRevisionId layer: a tag-form
    // id resolved against the repo compares equal to the head-form SHA id.
    // Post-split: a `RawRevisionId` cannot be compared to a
    // `ResolvedRevisionId` at all (no `PartialEq`), so the only assertion
    // that remains is the canonical-SHA equality after `resolve_revision`.
    // The compile-time invariant is exercised by the `compile_fail`
    // doc-test in `vcs.rs`.
    let dir = init_repo();
    let p = dir.path();
    git(p, &["tag", "v1.0.0"]);

    let vcs = GitVcs;
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
    let yaml = serde_yaml::to_string(&r).unwrap();
    // Single transparent string; serializing yields the display form.
    assert_eq!(yaml.trim(), "v1.0.0");
}

#[test]
fn revision_id_serialize_canonical_when_no_display() {
    let r = ResolvedRevisionId::from_canonical("abc123", None);
    let yaml = serde_yaml::to_string(&r).unwrap();
    assert_eq!(yaml.trim(), "abc123");
}

#[test]
fn raw_revision_id_deserializes_yaml_scalar_verbatim() {
    // Post-split: lock-file YAML scalars deserialize into `RawRevisionId`,
    // not `ResolvedRevisionId`. `ResolvedRevisionId` has no `Deserialize`
    // impl on purpose — resolution to a canonical SHA is path-rooted and
    // cannot happen at the deserializer layer.
    let r: repoweave::vcs::RawRevisionId = serde_yaml::from_str("v1.0.0").unwrap();
    assert_eq!(r.as_str(), "v1.0.0");
}

#[test]
fn revision_id_round_trip_yaml_string() {
    let original = ResolvedRevisionId::from_canonical(
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        Some("v1.0.0".to_string()),
    );
    let yaml = serde_yaml::to_string(&original).unwrap();
    // Round-trip through the parse boundary: a resolved value serializes
    // as a single scalar, and re-parsing it lands in `RawRevisionId`
    // (deserialize only ever yields raw — re-resolution requires a repo).
    let restored: repoweave::vcs::RawRevisionId = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(restored.as_str(), "v1.0.0");
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
    // Equality between the tag-form lock entry's resolved ResolvedRevisionId and
    // the HEAD's ResolvedRevisionId — the canonical-SHA equality the bead requires.
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
fn raw_revision_id_roundtrips_through_yaml() {
    // The parse boundary: a raw value serializes to a single YAML scalar
    // and deserializes back into a RawRevisionId with the original string.
    use repoweave::vcs::RawRevisionId;
    let original = RawRevisionId::new("v0.3.4");
    let yaml = serde_yaml::to_string(&original).unwrap();
    let restored: RawRevisionId = serde_yaml::from_str(&yaml).unwrap();
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
    git(p, &["tag", "v1.0.0"]);

    let raw = RawRevisionId::new("v1.0.0");
    let vcs = GitVcs;
    let resolved = vcs.resolve_revision(p, raw.as_str()).unwrap();
    let head = vcs.head_revision(p).unwrap();
    assert_eq!(resolved, head);
    // Display preserves the tag form for human-readable output / lock writes.
    assert_eq!(resolved.display_str(), "v1.0.0");
}

// ============================================================================
// Vcs::resolve_branch_on_remote — role-aware remote ref resolution
// ============================================================================

/// Build a workspace with two repos: a "remote" with one commit on `main`,
/// and a "local" clone whose remote is named `remote_name`. Returns the
/// workspace tempdir and the path to the local clone.
fn repo_with_remote(remote_name: &str) -> (TempDir, std::path::PathBuf) {
    let workspace = TempDir::new().unwrap();
    let remote_path = workspace.path().join("remote");
    let local_path = workspace.path().join("local");

    // Build the remote repo with one commit on `main`.
    fs::create_dir_all(&remote_path).unwrap();
    git(&remote_path, &["init", "--initial-branch=main"]);
    git(&remote_path, &["config", "user.email", "test@test.com"]);
    git(&remote_path, &["config", "user.name", "Test"]);
    fs::write(remote_path.join("README.md"), "remote").unwrap();
    git(&remote_path, &["add", "."]);
    git(&remote_path, &["commit", "-m", "initial"]);

    // Clone into local with the requested remote name and fetch.
    let remote_url = remote_path.to_str().unwrap();
    let local_str = local_path.to_str().unwrap();
    git(
        workspace.path(),
        &["clone", "--origin", remote_name, remote_url, local_str],
    );

    (workspace, local_path)
}

#[test]
fn resolve_branch_on_remote_fork_uses_upstream() {
    // Role::Fork resolves `upstream/{branch}`.
    let (_ws, local) = repo_with_remote("upstream");
    let expected_sha = git(&local, &["rev-parse", "upstream/main"]);

    let vcs = GitVcs;
    let resolved = vcs
        .resolve_branch_on_remote(&local, Role::Fork, &RefName::new("main"))
        .unwrap();

    assert_eq!(resolved.as_str(), &expected_sha);
    // Display form preserves the qualified ref we asked for.
    assert_eq!(resolved.display_str(), "upstream/main");
}

#[test]
fn resolve_branch_on_remote_primary_uses_origin() {
    let (_ws, local) = repo_with_remote("origin");
    let expected_sha = git(&local, &["rev-parse", "origin/main"]);

    let vcs = GitVcs;
    let resolved = vcs
        .resolve_branch_on_remote(&local, Role::Owned, &RefName::new("main"))
        .unwrap();

    assert_eq!(resolved.as_str(), &expected_sha);
    assert_eq!(resolved.display_str(), "origin/main");
}

#[test]
fn resolve_branch_on_remote_dependency_uses_origin() {
    let (_ws, local) = repo_with_remote("origin");
    let vcs = GitVcs;
    let resolved = vcs
        .resolve_branch_on_remote(&local, Role::Dependency, &RefName::new("main"))
        .unwrap();
    assert_eq!(resolved.display_str(), "origin/main");
}

#[test]
fn resolve_branch_on_remote_reference_uses_origin() {
    let (_ws, local) = repo_with_remote("origin");
    let vcs = GitVcs;
    let resolved = vcs
        .resolve_branch_on_remote(&local, Role::Reference, &RefName::new("main"))
        .unwrap();
    assert_eq!(resolved.display_str(), "origin/main");
}

#[test]
fn resolve_branch_on_remote_missing_remote_errors() {
    // Repo has no `upstream` remote — Role::Fork resolution must fail
    // clearly rather than silently falling back to a local branch tip.
    let (_ws, local) = repo_with_remote("origin");

    let vcs = GitVcs;
    let result = vcs.resolve_branch_on_remote(&local, Role::Fork, &RefName::new("main"));

    let err = result.expect_err("missing upstream remote must error");
    assert!(
        matches!(err, VcsError::RevisionNotFound { ref rev, .. } if rev == "upstream/main"),
        "expected RevisionNotFound for upstream/main, got {err:?}"
    );
}

// ============================================================================
// Vcs::push_with_role — role-aware push
// ============================================================================

/// Build a workspace with a bare "remote" and a local clone whose remote is
/// named `remote_name`. Returns (workspace, local_clone, bare_remote).
fn repo_with_bare_remote(remote_name: &str) -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let workspace = TempDir::new().unwrap();
    let bare_path = workspace.path().join("remote.git");
    let local_path = workspace.path().join("local");

    // Initialise the bare remote.
    git(
        workspace.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare_path.to_str().unwrap(),
        ],
    );

    // Build a seed clone, commit, push, so the bare repo has `main`.
    let seed_path = workspace.path().join("seed");
    git(
        workspace.path(),
        &[
            "clone",
            bare_path.to_str().unwrap(),
            seed_path.to_str().unwrap(),
        ],
    );
    git(&seed_path, &["config", "user.email", "test@test.com"]);
    git(&seed_path, &["config", "user.name", "Test"]);
    fs::write(seed_path.join("README.md"), "seed").unwrap();
    git(&seed_path, &["add", "."]);
    git(&seed_path, &["commit", "-m", "initial"]);
    git(&seed_path, &["push", "origin", "main"]);

    // Now make the *test*'s local clone with the requested remote name.
    git(
        workspace.path(),
        &[
            "clone",
            "--origin",
            remote_name,
            bare_path.to_str().unwrap(),
            local_path.to_str().unwrap(),
        ],
    );
    git(&local_path, &["config", "user.email", "test@test.com"]);
    git(&local_path, &["config", "user.name", "Test"]);

    (workspace, local_path, bare_path)
}

#[test]
fn push_with_role_primary_pushes_to_origin() {
    let (_ws, local, bare) = repo_with_bare_remote("origin");

    // Make a local commit so there's something to push.
    fs::write(local.join("new.txt"), "added").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "local advance"]);
    let local_head = git(&local, &["rev-parse", "HEAD"]);

    let vcs = GitVcs;
    vcs.push_with_role(&local, Role::Owned, false).unwrap();

    // The bare's `main` should now match the local HEAD.
    let bare_main = git(&bare, &["rev-parse", "main"]);
    assert_eq!(
        bare_main, local_head,
        "push should land local HEAD on bare main"
    );
}

#[test]
fn push_with_role_fork_pushes_to_upstream() {
    // Trait-level neutrality: Role::Fork uses the `upstream` remote. The
    // "skip forks at the rwv-push loop" policy lives in src/push.rs, not
    // in the trait method.
    let (_ws, local, bare) = repo_with_bare_remote("upstream");

    fs::write(local.join("new.txt"), "added").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "local advance"]);
    let local_head = git(&local, &["rev-parse", "HEAD"]);

    let vcs = GitVcs;
    vcs.push_with_role(&local, Role::Fork, false).unwrap();

    let bare_main = git(&bare, &["rev-parse", "main"]);
    assert_eq!(bare_main, local_head);
}

#[test]
fn push_with_role_dependency_pushes_to_origin() {
    let (_ws, local, bare) = repo_with_bare_remote("origin");
    fs::write(local.join("new.txt"), "added").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "advance"]);
    let local_head = git(&local, &["rev-parse", "HEAD"]);

    GitVcs
        .push_with_role(&local, Role::Dependency, false)
        .unwrap();
    assert_eq!(git(&bare, &["rev-parse", "main"]), local_head);
}

#[test]
fn push_with_role_detached_head_errors() {
    let (_ws, local, _bare) = repo_with_bare_remote("origin");
    // Detach HEAD by checking out the SHA directly.
    let head_sha = git(&local, &["rev-parse", "HEAD"]);
    git(&local, &["checkout", &head_sha]);

    let result = GitVcs.push_with_role(&local, Role::Owned, false);
    let err = result.expect_err("detached HEAD must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("detached"),
        "expected detached-HEAD message, got: {msg}"
    );
}

#[test]
fn push_with_role_non_fast_forward_errors_without_force() {
    // Set up a divergence: bare advances via a seed-clone-and-push, then the
    // local repo gets its own non-ancestor commit. A non-force push must
    // refuse the non-fast-forward update.
    let (ws, local, bare) = repo_with_bare_remote("origin");

    // Push a divergent commit through a second clone so bare/main moves
    // past the local clone's current HEAD.
    let other = ws.path().join("other");
    git(
        ws.path(),
        &["clone", bare.to_str().unwrap(), other.to_str().unwrap()],
    );
    git(&other, &["config", "user.email", "test@test.com"]);
    git(&other, &["config", "user.name", "Test"]);
    fs::write(other.join("other.txt"), "other").unwrap();
    git(&other, &["add", "."]);
    git(&other, &["commit", "-m", "other advance"]);
    git(&other, &["push", "origin", "main"]);

    // Now make a local commit that doesn't have the new tip as an ancestor.
    fs::write(local.join("local.txt"), "local").unwrap();
    git(&local, &["add", "."]);
    git(&local, &["commit", "-m", "local advance"]);

    // Without --force the push must fail.
    let result = GitVcs.push_with_role(&local, Role::Owned, false);
    assert!(
        result.is_err(),
        "non-fast-forward push without --force must fail"
    );

    // With force=true it should succeed.
    GitVcs
        .push_with_role(&local, Role::Owned, true)
        .expect("force-push should overwrite the divergent remote tip");
    let local_head = git(&local, &["rev-parse", "HEAD"]);
    let bare_main = git(&bare, &["rev-parse", "main"]);
    assert_eq!(bare_main, local_head);
}

#[test]
fn resolve_branch_on_remote_missing_branch_errors() {
    // Remote exists but branch doesn't — also a clear error.
    let (_ws, local) = repo_with_remote("origin");

    let vcs = GitVcs;
    let result =
        vcs.resolve_branch_on_remote(&local, Role::Owned, &RefName::new("nonexistent-branch"));

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
fn conflict_resolution_hint_rebase_uses_git_rebase_continue() {
    let vcs = GitVcs;
    let hint = vcs.conflict_resolution_hint(ConflictOp::Rebase);
    assert!(
        hint.contains("git rebase --continue"),
        "rebase hint must mention `git rebase --continue`; got: {hint:?}"
    );
    assert!(
        hint.contains("git add <files>"),
        "rebase hint must mention `git add <files>`; got: {hint:?}"
    );
    // Sanity: the hint must NOT carry the wrong op's continue verb.
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
    let vcs = GitVcs;
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
    let vcs = GitVcs;
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
    let vcs = GitVcs;
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
// Vcs::set_replay_exclusion / has_replay_exclusion
// ============================================================================
//
// The replay-exclusion mechanism wires git's per-path merge driver
// (`.gitattributes <path> merge=ours`) so sync's native rebase keeps the
// rebase target's version of `rwv.lock` through every replay. The trait
// hides the file format so other VCS impls can use their own mechanism.

#[test]
fn set_replay_exclusion_creates_gitattributes_when_missing() {
    let dir = init_repo();
    let vcs = GitVcs;

    vcs.set_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap();

    let attrs = fs::read_to_string(dir.path().join(".gitattributes")).unwrap();
    assert!(
        attrs.contains("rwv.lock merge=ours"),
        ".gitattributes should contain the replay-exclusion line; got: {attrs:?}"
    );
}

#[test]
fn set_replay_exclusion_appends_to_existing_gitattributes() {
    let dir = init_repo();
    let vcs = GitVcs;
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
        attrs.contains("rwv.lock merge=ours"),
        "new entry must be added; got: {attrs:?}"
    );
}

#[test]
fn set_replay_exclusion_is_idempotent() {
    let dir = init_repo();
    let vcs = GitVcs;

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
        .filter(|l| l.trim() == "rwv.lock merge=ours")
        .count();
    assert_eq!(
        line_count, 1,
        "exactly one replay-exclusion line expected; got {line_count} in: {after_second:?}"
    );
}

#[test]
fn has_replay_exclusion_false_when_gitattributes_missing() {
    let dir = init_repo();
    let vcs = GitVcs;

    assert!(!vcs
        .has_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap());
}

#[test]
fn has_replay_exclusion_false_when_line_absent() {
    let dir = init_repo();
    let vcs = GitVcs;
    fs::write(dir.path().join(".gitattributes"), "*.png binary\n").unwrap();

    assert!(!vcs
        .has_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap());
}

#[test]
fn has_replay_exclusion_true_when_line_present() {
    let dir = init_repo();
    let vcs = GitVcs;
    vcs.set_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap();

    assert!(vcs
        .has_replay_exclusion(dir.path(), std::path::Path::new("rwv.lock"))
        .unwrap());
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
    let c1 = git(p, &["rev-parse", "HEAD"]);

    // main: advance with a new file `main.txt`.
    fs::write(p.join("main.txt"), "main\n").unwrap();
    git(p, &["add", "main.txt"]);
    git(p, &["commit", "-m", "main: advance"]);

    // feat branch from C1 with a new file `feat.txt`.
    git(p, &["checkout", "-b", "feat", &c1]);
    fs::write(p.join("feat.txt"), "feat\n").unwrap();
    git(p, &["add", "feat.txt"]);
    git(p, &["commit", "-m", "feat: advance"]);

    let c1 = ResolvedRevisionId::from_canonical(c1, None);
    (dir, c1)
}

#[test]
fn rebase_clean_advances_head_onto_target() {
    let (dir, _c1) = diverged_repo();
    let p = dir.path();
    let main_tip = ResolvedRevisionId::from_canonical(git(p, &["rev-parse", "main"]), None);

    // feat is checked out — rebase it onto main.
    GitVcs.rebase(p, &main_tip, &main_tip).unwrap();

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
    git(p, &["add", "shared"]);
    git(p, &["commit", "-m", "add shared"]);
    let c1 = git(p, &["rev-parse", "HEAD"]);

    fs::write(p.join("shared"), "main version\n").unwrap();
    git(p, &["add", "shared"]);
    git(p, &["commit", "-m", "main: change shared"]);

    git(p, &["checkout", "-b", "feat", &c1]);
    fs::write(p.join("shared"), "feat version\n").unwrap();
    git(p, &["add", "shared"]);
    git(p, &["commit", "-m", "feat: change shared"]);

    let main_tip = ResolvedRevisionId::from_canonical(git(p, &["rev-parse", "main"]), None);

    let result = GitVcs.rebase(p, &main_tip, &main_tip);

    let err = result.expect_err("rebase with conflicting paths must surface an error");
    assert!(
        matches!(err, VcsError::RebaseConflict { ref op, .. } if *op == ConflictOp::Rebase),
        "expected RebaseConflict, got {err:?}"
    );
    // Repo is left mid-rebase so `git rebase --continue` works.
    let mid_op = repoweave::git::GitVcs::mid_op_state(p);
    assert_eq!(
        mid_op.as_deref(),
        Some("mid-rebase"),
        "repo should be left mid-rebase for the operator to resolve and continue"
    );
}

#[test]
fn rebase_auto_resolves_lock_collision_when_replay_exclusion_set() {
    // Both sides modify the same `rwv.lock` content; with replay-exclusion
    // configured, the rebase should keep main's version with no conflict.
    let dir = init_repo();
    let p = dir.path();
    fs::write(p.join("rwv.lock"), "v0\n").unwrap();
    git(p, &["add", "rwv.lock"]);
    // Configure replay-exclusion BEFORE the commits that mutate the lock.
    GitVcs
        .set_replay_exclusion(p, std::path::Path::new("rwv.lock"))
        .unwrap();
    git(p, &["add", ".gitattributes"]);
    git(p, &["commit", "-m", "lock + .gitattributes"]);
    let c1 = git(p, &["rev-parse", "HEAD"]);

    fs::write(p.join("rwv.lock"), "main version\n").unwrap();
    git(p, &["add", "rwv.lock"]);
    git(p, &["commit", "-m", "main: change lock"]);
    let main_lock_content = fs::read_to_string(p.join("rwv.lock")).unwrap();

    git(p, &["checkout", "-b", "feat", &c1]);
    fs::write(p.join("rwv.lock"), "feat version\n").unwrap();
    git(p, &["add", "rwv.lock"]);
    git(p, &["commit", "-m", "feat: change lock"]);

    let main_tip = ResolvedRevisionId::from_canonical(git(p, &["rev-parse", "main"]), None);

    GitVcs
        .rebase(p, &main_tip, &main_tip)
        .expect("rebase should succeed thanks to replay-exclusion auto-resolve");

    // Working tree should hold MAIN's lock content — replay-exclusion keeps
    // the rebase target's version through every replayed commit.
    let final_lock = fs::read_to_string(p.join("rwv.lock")).unwrap();
    assert_eq!(
        final_lock, main_lock_content,
        "replay-exclusion should keep main's lock content after rebase; \
         got {final_lock:?}, expected {main_lock_content:?}"
    );
    // And the repo should not be left in a mid-op state.
    assert!(
        repoweave::git::GitVcs::mid_op_state(p).is_none(),
        "successful rebase must leave repo in a clean state"
    );
}
