//! E2E tests for `rwv lock` and `rwv lock-all` commands.
//!
//! These tests exercise the CLI binary via `assert_cmd`. Tests that depend on
//! the lock implementation (bead 7b) are marked `#[ignore]`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
use std::process;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a minimal workspace directory structure with a `github/` registry dir
/// and a `projects/` dir. Returns the workspace root path.
fn make_workspace(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    root
}

/// Initialise a git repo at `path` with a single commit so HEAD exists.
/// Returns the SHA of that commit.
fn init_git_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();

    let run = |args: &[&str], dir: &Path| {
        let out = process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("git command failed to start");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    run(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    run(&["add", "."], path);
    run(&["commit", "-m", "initial"], path);

    // Return HEAD SHA
    run(&["rev-parse", "HEAD"], path)
}

/// Write an `rwv.yaml` manifest into a project directory.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    std::fs::create_dir_all(project_dir).unwrap();
    let mut yaml = String::from("repositories:\n");
    for (repo_path, url) in repos {
        yaml.push_str(&format!(
            "  {repo_path}:\n    type: git\n    url: {url}\n    version: main\n    role: primary\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();
}

/// Build a `Command` for the `rwv` binary.
fn rwv_cmd() -> Command {
    Command::cargo_bin("rwv").expect("rwv binary not found")
}

// ---------------------------------------------------------------------------
// 1. `rwv lock` in a primary directory with a project
// ---------------------------------------------------------------------------

#[test]
fn lock_in_primary_creates_lock_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create two repos under the workspace
    let repo_a_path = "github/acme/server";
    let repo_b_path = "github/acme/client";
    let sha_a = init_git_repo(&root.join(repo_a_path));
    let sha_b = init_git_repo(&root.join(repo_b_path));

    // Create a project that references both repos
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[
            (repo_a_path, "https://github.com/acme/server.git"),
            (repo_b_path, "https://github.com/acme/client.git"),
        ],
    );

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    // Verify rwv.lock was created
    let lock_path = project_dir.join("rwv.lock");
    assert!(lock_path.exists(), "rwv.lock should be created");

    let lock_content = std::fs::read_to_string(&lock_path).unwrap();

    // Verify SHAs appear in lock file
    assert!(
        lock_content.contains(&sha_a),
        "lock should contain repo A SHA {sha_a}, got:\n{lock_content}"
    );
    assert!(
        lock_content.contains(&sha_b),
        "lock should contain repo B SHA {sha_b}, got:\n{lock_content}"
    );

    // Parse as LockFile to verify structure
    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    assert_eq!(lock.repositories.len(), 2);
    assert!(
        lock.workweave.is_none(),
        "primary lock should have no workweave"
    );

    let entry_a = lock
        .repositories
        .get(&repoweave::manifest::RepoPath::new(repo_a_path))
        .expect("lock should contain repo A");
    assert_eq!(entry_a.version.as_str(), &sha_a);
    assert_eq!(entry_a.vcs_type, repoweave::manifest::VcsType::Git);

    let entry_b = lock
        .repositories
        .get(&repoweave::manifest::RepoPath::new(repo_b_path))
        .expect("lock should contain repo B");
    assert_eq!(entry_b.version.as_str(), &sha_b);
}

// ---------------------------------------------------------------------------
// 2. `rwv lock` in a workweave — includes workweave provenance
// ---------------------------------------------------------------------------

#[test]
fn lock_in_workweave_includes_workweave_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let sha = init_git_repo(&root.join(repo_path));

    // Create the project
    let project_dir = root.join("projects").join("ws");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Create a workweave sibling directory: ws--hotfix
    let workweave_dir = tmp.path().join("ws--hotfix");
    std::fs::create_dir_all(workweave_dir.join("github")).unwrap();

    // Also create the repo in the workweave so HEAD can be resolved
    let workweave_repo = workweave_dir.join(repo_path);
    let workweave_sha = init_git_repo(&workweave_repo);

    rwv_cmd()
        .arg("lock")
        .current_dir(&workweave_dir)
        .assert()
        .success();

    // Lock file should be in the project dir
    // Check the lock file includes workweave provenance
    let lock_path = project_dir.join("rwv.lock");
    assert!(lock_path.exists(), "rwv.lock should be created");

    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    assert_eq!(
        lock.workweave,
        Some(repoweave::manifest::WorkweaveName::new("hotfix")),
        "lock should include workweave name"
    );

    let entry = lock
        .repositories
        .get(&repoweave::manifest::RepoPath::new(repo_path))
        .expect("lock should contain repo");
    // The SHA should come from the workweave's repo
    assert_eq!(entry.version.as_str(), &workweave_sha);
    let _ = sha; // primary SHA unused but kept for clarity
}

// ---------------------------------------------------------------------------
// 4. Lock file format validation
// ---------------------------------------------------------------------------

#[test]
fn lock_file_format_has_correct_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let sha = init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    let lock_path = project_dir.join("rwv.lock");
    let lock_content = std::fs::read_to_string(&lock_path).unwrap();

    // Verify raw YAML contains expected keys
    assert!(
        lock_content.contains("repositories:"),
        "lock file should have repositories key"
    );
    assert!(
        lock_content.contains("type: git"),
        "lock entries should have VcsType"
    );
    assert!(
        lock_content.contains(&format!("version: {sha}")),
        "lock entries should have pinned SHA as version"
    );
    assert!(
        lock_content.contains("url: https://github.com/acme/server.git"),
        "lock entries should have repo url"
    );
    assert!(
        lock_content.contains("github/acme/server"),
        "lock entries should have repo path as key"
    );

    // Parse and validate types
    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let entry = &lock.repositories[&repoweave::manifest::RepoPath::new(repo_path)];
    assert_eq!(entry.vcs_type, repoweave::manifest::VcsType::Git);
    assert_eq!(entry.version.as_str(), &sha);
    assert_eq!(entry.url, "https://github.com/acme/server.git");

    // SHA should look like a full git SHA (40 hex chars)
    assert_eq!(
        entry.version.as_str().len(),
        40,
        "RevisionId should be a full 40-char SHA"
    );
    assert!(
        entry
            .version
            .as_str()
            .chars()
            .all(|c| c.is_ascii_hexdigit()),
        "RevisionId should be hex"
    );
}

// ---------------------------------------------------------------------------
// 5. `rwv lock` with no active project — should error
// ---------------------------------------------------------------------------

#[test]
fn lock_with_no_project_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Run `rwv lock` from workspace root with no project context
    rwv_cmd()
        .arg("lock")
        .current_dir(&root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("project").or(predicate::str::contains("Project")));
}

// ---------------------------------------------------------------------------
// 6. Stale lock detection — lock doesn't match current HEADs
// ---------------------------------------------------------------------------

#[test]
fn stale_lock_detected_after_new_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let sha_old = init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Create initial lock
    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    let lock_path = project_dir.join("rwv.lock");
    let lock_before = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let pinned_sha = lock_before.repositories[&repoweave::manifest::RepoPath::new(repo_path)]
        .version
        .as_str()
        .to_string();
    assert_eq!(pinned_sha, sha_old);

    // Make a new commit in the repo so HEAD advances
    let repo_dir = root.join(repo_path);
    std::fs::write(repo_dir.join("new_file.txt"), "change\n").unwrap();
    let run_git = |args: &[&str], dir: &Path| -> String {
        let out = process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    run_git(&["add", "."], &repo_dir);
    run_git(&["commit", "-m", "second"], &repo_dir);
    let sha_new = run_git(&["rev-parse", "HEAD"], &repo_dir);
    assert_ne!(sha_old, sha_new, "new commit should have different SHA");

    // The existing lock file still has the old SHA — it's stale
    let stale_lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let stale_sha = stale_lock.repositories[&repoweave::manifest::RepoPath::new(repo_path)]
        .version
        .as_str()
        .to_string();
    assert_eq!(
        stale_sha, sha_old,
        "lock should still have old SHA before re-lock"
    );
    assert_ne!(
        stale_sha, sha_new,
        "lock SHA should differ from current HEAD"
    );

    // Re-lock to update
    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    let updated_lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let updated_sha = updated_lock.repositories[&repoweave::manifest::RepoPath::new(repo_path)]
        .version
        .as_str()
        .to_string();
    assert_eq!(
        updated_sha, sha_new,
        "re-lock should update to current HEAD SHA"
    );
}

// ---------------------------------------------------------------------------
// Smoke test: `rwv lock` CLI parses without error (no #[ignore])
// ---------------------------------------------------------------------------

#[test]
fn lock_command_is_recognized() {
    // The command should parse successfully (not fail with "unrecognized subcommand").
    // It will fail because there's no workspace, but the error should NOT be about
    // an unrecognized subcommand.
    // Run from an empty temp dir so we don't accidentally pick up a real workspace.
    let tmp = tempfile::tempdir().unwrap();
    rwv_cmd()
        .arg("lock")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized").not());
}

// ---------------------------------------------------------------------------
// 7. Dirty check: lock errors on uncommitted changes
// ---------------------------------------------------------------------------

#[test]
fn lock_errors_on_uncommitted_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Create an uncommitted change in the repo
    let repo_dir = root.join(repo_path);
    std::fs::write(repo_dir.join("dirty.txt"), "uncommitted\n").unwrap();

    // `rwv lock` should fail because the repo has uncommitted changes
    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("uncommitted")
                .or(predicate::str::contains("dirty"))
                .or(predicate::str::contains("changes")),
        );

    // Lock file should NOT have been written
    assert!(
        !project_dir.join("rwv.lock").exists(),
        "rwv.lock should not be created when repos have uncommitted changes"
    );
}

#[test]
fn lock_errors_on_staged_uncommitted_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Stage a change but don't commit it
    let repo_dir = root.join(repo_path);
    std::fs::write(repo_dir.join("staged.txt"), "staged\n").unwrap();
    let _ = process::Command::new("git")
        .args(["add", "staged.txt"])
        .current_dir(&repo_dir)
        .output()
        .unwrap();

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .failure();
}

// ---------------------------------------------------------------------------
// 8. --dirty flag bypasses uncommitted-changes check
// ---------------------------------------------------------------------------

#[test]
fn lock_dirty_flag_bypasses_uncommitted_check() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let sha = init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Create an uncommitted change
    let repo_dir = root.join(repo_path);
    std::fs::write(repo_dir.join("dirty.txt"), "uncommitted\n").unwrap();

    // `rwv lock --dirty` should succeed despite uncommitted changes
    rwv_cmd()
        .args(["lock", "--dirty"])
        .current_dir(&project_dir)
        .assert()
        .success();

    // Lock file should exist and contain the HEAD SHA
    let lock_path = project_dir.join("rwv.lock");
    assert!(
        lock_path.exists(),
        "rwv.lock should be created with --dirty"
    );

    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let entry = lock
        .repositories
        .get(&repoweave::manifest::RepoPath::new(repo_path))
        .expect("lock should contain repo");
    assert_eq!(entry.version.as_str(), &sha);
}

// ---------------------------------------------------------------------------
// 9. Lock records tag name when HEAD is tagged
// ---------------------------------------------------------------------------

#[test]
fn lock_records_tag_name_when_head_is_tagged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let _sha = init_git_repo(&root.join(repo_path));

    // Create a tag at HEAD
    let repo_dir = root.join(repo_path);
    let _ = process::Command::new("git")
        .args(["tag", "v1.0.0"])
        .current_dir(&repo_dir)
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    let lock_path = project_dir.join("rwv.lock");
    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let entry = lock
        .repositories
        .get(&repoweave::manifest::RepoPath::new(repo_path))
        .expect("lock should contain repo");

    // Version should be the tag name, not the raw SHA
    assert_eq!(
        entry.version.as_str(),
        "v1.0.0",
        "lock should record tag name when HEAD is tagged"
    );
}

#[test]
fn lock_records_sha_when_head_is_not_tagged() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let sha = init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    let lock_path = project_dir.join("rwv.lock");
    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let entry = lock
        .repositories
        .get(&repoweave::manifest::RepoPath::new(repo_path))
        .expect("lock should contain repo");

    // Version should be the raw SHA when no tag points at HEAD
    assert_eq!(
        entry.version.as_str(),
        &sha,
        "lock should record raw SHA when HEAD is not tagged"
    );
}

#[test]
fn lock_records_tag_per_repo_independently() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_a = "github/acme/server";
    let repo_b = "github/acme/client";
    let _sha_a = init_git_repo(&root.join(repo_a));
    let sha_b = init_git_repo(&root.join(repo_b));

    // Tag only repo A
    let _ = process::Command::new("git")
        .args(["tag", "v2.0.0"])
        .current_dir(root.join(repo_a))
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[
            (repo_a, "https://github.com/acme/server.git"),
            (repo_b, "https://github.com/acme/client.git"),
        ],
    );

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    let lock_path = project_dir.join("rwv.lock");
    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();

    let entry_a = &lock.repositories[&repoweave::manifest::RepoPath::new(repo_a)];
    assert_eq!(
        entry_a.version.as_str(),
        "v2.0.0",
        "tagged repo should use tag name"
    );

    let entry_b = &lock.repositories[&repoweave::manifest::RepoPath::new(repo_b)];
    assert_eq!(
        entry_b.version.as_str(),
        &sha_b,
        "untagged repo should use raw SHA"
    );
}

// ---------------------------------------------------------------------------
// 10. Integration lock hooks run after rwv.lock write
// ---------------------------------------------------------------------------

#[test]
fn lock_runs_integration_lock_hooks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    // Create a project with a Cargo.toml in the repo so cargo integration detects it
    let repo_dir = root.join(repo_path);
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    // Commit the Cargo.toml so the repo is clean
    let _ = process::Command::new("git")
        .args(["add", "."])
        .current_dir(&repo_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();
    let _ = process::Command::new("git")
        .args(["commit", "-m", "add cargo.toml"])
        .current_dir(&repo_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Run lock — it should succeed and write the lock file.
    // The integration lock hooks should run after the lock file is written.
    // We verify this by checking that `rwv lock` completes successfully
    // (integration errors would cause a non-zero exit or stderr warnings).
    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    // Lock file should exist (written before hooks ran)
    let lock_path = project_dir.join("rwv.lock");
    assert!(
        lock_path.exists(),
        "rwv.lock should be written before lock hooks"
    );
}

// ---------------------------------------------------------------------------
// 11. `lock-all` removed — CLI error
// ---------------------------------------------------------------------------

#[test]
fn lock_all_is_removed_cli_error() {
    // `rwv lock-all` should be rejected as an unrecognized subcommand
    // (or produce a helpful error telling users to use `rwv lock` instead).
    rwv_cmd().arg("lock-all").assert().failure().stderr(
        predicate::str::contains("unrecognized")
            .or(predicate::str::contains("removed"))
            .or(predicate::str::contains("no longer"))
            .or(predicate::str::contains("not a valid")),
    );
}
