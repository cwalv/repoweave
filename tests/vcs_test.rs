use repoweave::git::GitVcs;
use repoweave::vcs::Vcs;
use std::fs;
use std::process::Command;
use tempfile::TempDir;

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
    let output = Command::new("git")
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
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .to_string()
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
    assert_eq!(tag.as_deref(), Some("v0.1.0"));
}

#[test]
fn tag_at_head_annotated_tag() {
    let dir = init_repo();
    let p = dir.path();

    git(p, &["tag", "-a", "v1.0.0", "-m", "release v1.0.0"]);

    let vcs = GitVcs;
    let tag = vcs.tag_at_head(p).unwrap();
    assert_eq!(tag.as_deref(), Some("v1.0.0"));
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
    // When multiple tags point at HEAD, we get one of them.
    assert!(
        tag.as_deref() == Some("v1.0.0") || tag.as_deref() == Some("release-1"),
        "expected one of the two tags, got {:?}",
        tag
    );
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

    assert!(repo_path.join(".git").exists(), "should create .git directory");
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

    assert!(repo_path.join(".git").exists(), "should create nested dirs and init repo");
}

#[test]
fn init_repo_is_recognized_by_is_repo() {
    let dir = TempDir::new().unwrap();
    let repo_path = dir.path().join("new-repo");

    let vcs = GitVcs;
    vcs.init_repo(&repo_path).unwrap();

    assert!(vcs.is_repo(&repo_path), "init_repo result should be recognized as a repo");
}
