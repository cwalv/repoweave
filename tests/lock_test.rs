//! E2E tests for `rwv lock` and `rwv lock-all` commands.
//!
//! These tests exercise the CLI binary via `assert_cmd`. Tests that depend on
//! the lock implementation (phase 7b) are marked `#[ignore]`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

mod common;

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
        let out = common::git()
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

/// Write an `rwv.toml` manifest into a **primary** root's project directory.
///
/// Also writes `.rwv-active` at that root pointing at the project, so
/// lock-test scenarios that run `rwv lock` (an action verb) resolve the
/// project correctly. A workweave root takes `write_workweave_manifest`
/// instead: its marker already names the project, and a pointer beside the
/// marker is the state resolution refuses.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    write_workweave_manifest(project_dir, repos);

    // Derive (workspace_root, project_name) and set .rwv-active. The
    // project_dir is `<root>/projects/<name>/`, so the root is two
    // ancestors up.
    if let (Some(name), Some(root)) = (
        project_dir.file_name().and_then(|n| n.to_str()),
        project_dir.parent().and_then(|p| p.parent()),
    ) {
        let _ = std::fs::write(root.join(".rwv-active"), format!("{name}\n"));
    }
}

/// [`write_manifest`] without the pointer, for a project directory inside a
/// workweave root.
fn write_workweave_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    std::fs::create_dir_all(project_dir).unwrap();
    let mut manifest_toml = String::from("[repositories]\n");
    for (repo_path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
}

/// Build a `Command` for the `rwv` binary.
///
/// Sets `current_dir` to an empty temp dir so tests never accidentally pick up
/// the real workspace. Tests that need a specific workspace override with their
/// own `.current_dir()` call.
fn rwv_cmd() -> Command {
    let mut cmd = common::rwv();
    cmd.current_dir(std::env::temp_dir());
    cmd
}

// ---------------------------------------------------------------------------
// 1. `rwv lock` in a primary directory with a project
// ---------------------------------------------------------------------------

#[test]
fn lock_in_primary_creates_lock_file() {
    let tmp = common::tempdir().unwrap();
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
    assert_eq!(lock.len(), 2);

    let entry_a = lock
        .get_entry(&repoweave::manifest::RepoPath::new(repo_a_path).expect("known-safe literal"))
        .expect("lock should contain repo A");
    assert_eq!(entry_a.version.as_str(), &sha_a);
    assert_eq!(entry_a.vcs_type, repoweave::manifest::VcsType::Git);

    let entry_b = lock
        .get_entry(&repoweave::manifest::RepoPath::new(repo_b_path).expect("known-safe literal"))
        .expect("lock should contain repo B");
    assert_eq!(entry_b.version.as_str(), &sha_b);
}

// ---------------------------------------------------------------------------
// 2. `rwv lock` in a workweave — writes to the workweave's own project dir
// ---------------------------------------------------------------------------

#[test]
fn lock_in_workweave_writes_to_workweave_project_dir_not_primary() {
    // Regression: `rwv lock` from a workweave must write to the workweave's
    // own project worktree, not primary's. Each worktree has its own working
    // -tree copy of `rwv.lock` on a separate branch; writing to primary from
    // a workweave clobbers primary's committed lock state with workweave-tip
    // values.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let primary_sha = init_git_repo(&root.join(repo_path));

    // Primary's project dir with rwv.toml.
    let primary_project_dir = root.join("projects").join("ws");
    write_manifest(
        &primary_project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Workweave dir using the `{project}--{name}` naming convention with a marker.
    // Mirror the layout produced by `rwv workweave create`: the workweave has
    // its own project dir with the same manifest committed.
    let workweave_dir = tmp.path().join("ws--hotfix");
    std::fs::create_dir_all(workweave_dir.join("github")).unwrap();
    let workweave_project_dir = workweave_dir.join("projects").join("ws");
    write_workweave_manifest(
        &workweave_project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Write the .rwv-workweave marker so resolve() recognises this as a
    // workweave. The marker alone does it, and is the workweave root's only
    // identity file — a `.rwv-active` beside it is the state doctor reports
    // as `weave-root-identity-conflict`.
    let primary_canon = root.canonicalize().unwrap();
    let marker = common::workweave_marker(&primary_canon, "ws", &primary_canon);
    std::fs::write(workweave_dir.join(".rwv-workweave"), marker).unwrap();

    // Repo also exists in the workweave on a different commit so we can
    // observe whose tip ends up in which lock.
    let workweave_repo = workweave_dir.join(repo_path);
    let workweave_sha = init_git_repo(&workweave_repo);

    rwv_cmd()
        .arg("lock")
        .current_dir(&workweave_dir)
        .assert()
        .success();

    // The workweave's own lock must be created, pinned to the workweave's tip.
    let workweave_lock_path = workweave_project_dir.join("rwv.lock");
    assert!(
        workweave_lock_path.exists(),
        "workweave's rwv.lock should be created"
    );
    let workweave_lock = repoweave::manifest::LockFile::from_path(&workweave_lock_path).unwrap();
    let entry = workweave_lock
        .get_entry(&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal"))
        .expect("workweave lock should contain repo");
    assert_eq!(
        entry.version.as_str(),
        &workweave_sha,
        "workweave lock SHA must come from the workweave's repo, not primary's"
    );

    // Primary's lock must NOT have been touched by the workweave's `rwv lock`.
    let primary_lock_path = primary_project_dir.join("rwv.lock");
    assert!(
        !primary_lock_path.exists(),
        "primary's rwv.lock must not be created by `rwv lock` running in a workweave"
    );
    let _ = primary_sha;
}

// ---------------------------------------------------------------------------
// 4. Lock file format validation
// ---------------------------------------------------------------------------

#[test]
fn lock_file_format_has_correct_fields() {
    let tmp = common::tempdir().unwrap();
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

    // Verify raw JSON contains expected keys
    assert!(
        lock_content.contains("\"repositories\""),
        "lock file should have repositories key"
    );
    assert!(
        lock_content.contains("\"type\": \"git\""),
        "lock entries should have VcsType"
    );
    assert!(
        lock_content.contains(&format!("\"version\": \"{sha}\"")),
        "lock entries should have pinned SHA as version"
    );
    assert!(
        lock_content.contains("\"url\": \"https://github.com/acme/server.git\""),
        "lock entries should have repo url"
    );
    assert!(
        lock_content.contains("github/acme/server"),
        "lock entries should have repo path as key"
    );

    // Parse and validate types
    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let entry = &lock.repo_map()
        [&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal")];
    assert_eq!(entry.vcs_type, repoweave::manifest::VcsType::Git);
    assert_eq!(entry.version.as_str(), &sha);
    assert_eq!(entry.url.to_string(), "https://github.com/acme/server.git");

    // SHA should look like a full git SHA (40 hex chars)
    assert_eq!(
        entry.version.as_str().len(),
        40,
        "ResolvedRevisionId should be a full 40-char SHA"
    );
    assert!(
        entry
            .version
            .as_str()
            .chars()
            .all(|c| c.is_ascii_hexdigit()),
        "ResolvedRevisionId should be hex"
    );
}

// ---------------------------------------------------------------------------
// 5. `rwv lock` with no active project — should error
// ---------------------------------------------------------------------------

#[test]
fn lock_with_no_project_errors() {
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let pinned_sha = lock_before.repo_map()
        [&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal")]
        .version
        .as_str()
        .to_string();
    assert_eq!(pinned_sha, sha_old);

    // Make a new commit in the repo so HEAD advances
    let repo_dir = root.join(repo_path);
    std::fs::write(repo_dir.join("new_file.txt"), "change\n").unwrap();
    let run_git = |args: &[&str], dir: &Path| -> String {
        let out = common::git()
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
    let stale_sha = stale_lock.repo_map()
        [&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal")]
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
    let updated_sha = updated_lock.repo_map()
        [&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal")]
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
    rwv_cmd()
        .arg("lock")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized").not());
}

// ---------------------------------------------------------------------------
// 7. Dirty check: lock errors on uncommitted changes
// ---------------------------------------------------------------------------

#[test]
fn lock_errors_on_uncommitted_changes() {
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let _ = common::git()
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
    let tmp = common::tempdir().unwrap();
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
        .get_entry(&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal"))
        .expect("lock should contain repo");
    assert_eq!(entry.version.as_str(), &sha);
}

// ---------------------------------------------------------------------------
// 9. Lock records tag name when HEAD is tagged
// ---------------------------------------------------------------------------

#[test]
fn lock_records_tag_name_when_head_is_tagged() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let _sha = init_git_repo(&root.join(repo_path));

    // Create a tag at HEAD
    let repo_dir = root.join(repo_path);
    let _ = common::git()
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
        .get_entry(&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal"))
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
    let tmp = common::tempdir().unwrap();
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
        .get_entry(&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal"))
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
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_a = "github/acme/server";
    let repo_b = "github/acme/client";
    let _sha_a = init_git_repo(&root.join(repo_a));
    let sha_b = init_git_repo(&root.join(repo_b));

    // Tag only repo A
    let _ = common::git()
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

    let entry_a =
        &lock.repo_map()[&repoweave::manifest::RepoPath::new(repo_a).expect("known-safe literal")];
    assert_eq!(
        entry_a.version.as_str(),
        "v2.0.0",
        "tagged repo should use tag name"
    );

    let entry_b =
        &lock.repo_map()[&repoweave::manifest::RepoPath::new(repo_b).expect("known-safe literal")];
    assert_eq!(
        entry_b.version.as_str(),
        &sha_b,
        "untagged repo should use raw SHA"
    );
}

// ---------------------------------------------------------------------------
// 9b. Forgoing tag names — the project-level escape hatch
// ---------------------------------------------------------------------------

/// Append a `[lock]` policy table to an already-written manifest.
fn append_lock_policy(project_dir: &Path, body: &str) {
    let path = project_dir.join("rwv.toml");
    let mut manifest_toml = std::fs::read_to_string(&path).unwrap();
    manifest_toml.push_str("\n[lock]\n");
    manifest_toml.push_str(body);
    std::fs::write(&path, manifest_toml).unwrap();
}

/// The escape hatch exists so the lock reproduces the tree by itself, and the
/// only place that promise is kept or broken is the lock file — nothing rwv
/// prints reports it. `lock_records_tag_name_when_head_is_tagged` pins the
/// default from the other side, on the same fixture.
#[test]
fn lock_forgoing_tag_names_records_the_commit_id() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let sha = init_git_repo(&root.join(repo_path));

    let _ = common::git()
        .args(["tag", "v1.0.0"])
        .current_dir(root.join(repo_path))
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    append_lock_policy(&project_dir, "forgo-tag-names = true\n");

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    let lock = repoweave::manifest::LockFile::from_path(&project_dir.join("rwv.lock")).unwrap();
    let entry = lock
        .get_entry(&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal"))
        .expect("lock should contain repo");

    assert_eq!(
        entry.version.as_str(),
        &sha,
        "a project forgoing tag names must record the commit id even at a tag"
    );
}

/// A misspelled policy key is refused, and the refusal names the key.
///
/// The failure this prevents is invisible: an operator who typed the key
/// believes their locks record commit ids, and a lock still full of tag names
/// looks exactly like a lock they asked for. Driven end to end because the
/// refusal has to survive to stdout — a parse error that only exists inside
/// the process is the same as no check at all.
#[test]
fn a_misspelled_lock_policy_key_is_refused() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    append_lock_policy(&project_dir, "forgo-tag-nmaes = true\n");

    let assertion = rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .failure();
    let out = assertion.get_output();
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        combined.contains("forgo-tag-nmaes"),
        "the refusal must quote the key the operator typed, got:\n{combined}"
    );
    assert!(
        combined.contains("forgo-tag-names"),
        "the refusal must name the spelling that works, got:\n{combined}"
    );
    assert!(
        !project_dir.join("rwv.lock").exists(),
        "a manifest that did not parse must not produce a lock"
    );
}

// ---------------------------------------------------------------------------
// 10. `rwv lock` does NOT run integration hooks
// ---------------------------------------------------------------------------

#[test]
fn lock_does_not_run_integration_hooks() {
    // `rwv lock` is a pure git SHA snapshot. The cargo integration's
    // hook (which would run `cargo generate-lockfile` and fail with no
    // workspace-root Cargo.toml) fires on `rwv activate`, not on
    // `rwv lock`. So `rwv lock` should succeed cleanly without touching
    // ecosystem state.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let repo_dir = root.join(repo_path);
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let _ = common::git()
        .args(["add", "."])
        .current_dir(&repo_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();
    let _ = common::git()
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

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    let lock_path = project_dir.join("rwv.lock");
    assert!(
        lock_path.exists(),
        "rwv.lock should be written by `rwv lock`"
    );

    // The workspace-root Cargo.toml would only be created by activation
    // (run_activations). Since we never activated, it should be absent.
    // This is a sanity check that `rwv lock` is not running activations
    // as a side effect either.
    assert!(
        !root.join("Cargo.toml").exists(),
        "rwv lock must not generate the workspace-root Cargo.toml"
    );
}

// ---------------------------------------------------------------------------
// 11. `lock-all` removed — CLI error
// ---------------------------------------------------------------------------

#[test]
fn lock_all_is_removed_cli_error() {
    // `rwv lock-all` should be rejected: `unknown verb` (external-subcommand
    // dispatch, when no `rwv-lock-all` binary is on PATH) or
    // one of the historical clap wordings. The PATH is pinned to an empty
    // dir so a stray plugin installed on the test host cannot hide the
    // regression.
    let plugin_dir = common::tempdir().expect("tempdir");
    rwv_cmd()
        .arg("lock-all")
        .env("PATH", plugin_dir.path().to_string_lossy().to_string())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unknown verb")
                .or(predicate::str::contains("unrecognized"))
                .or(predicate::str::contains("removed"))
                .or(predicate::str::contains("no longer"))
                .or(predicate::str::contains("not a valid")),
        );
}

// ---------------------------------------------------------------------------
// 12. ResolvedRevisionId round-trip via lock load + resolve
// ---------------------------------------------------------------------------

#[test]
fn lock_round_trip_preserves_tag_form_in_json() {
    // Generate a lock with a tag at HEAD, parse it back, write again — the
    // tag-form should survive the round-trip (i.e., `"version": "v1.0.0"`,
    // not the canonical SHA).
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));
    let _ = common::git()
        .args(["tag", "v1.0.0"])
        .current_dir(root.join(repo_path))
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
    let json_first = std::fs::read_to_string(&lock_path).unwrap();
    assert!(
        json_first.contains("\"version\": \"v1.0.0\""),
        "first lock should serialize tag-form: {json_first}"
    );

    // Reparse and reserialize via the public LockFile API.
    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let json_round = serde_json::to_string(&lock).unwrap();
    assert!(
        json_round.contains("\"version\":\"v1.0.0\""),
        "round-tripped JSON should preserve tag-form: {json_round}"
    );
}

#[test]
fn lock_resolve_versions_makes_tag_form_equal_head() {
    // After loading a lock with `version: v1.0.0` and calling
    // `resolve_versions(workspace_dir)`, the entry's ResolvedRevisionId compares equal
    // to the head's ResolvedRevisionId — equality goes through canonical SHAs.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));
    let _ = common::git()
        .args(["tag", "v1.0.0"])
        .current_dir(root.join(repo_path))
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();

    // Hand-write a lock pinning the tag-form; resolution must populate
    // canonical from rev-parse for the comparison to succeed.
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    let lock_path = project_dir.join("rwv.lock");
    common::fixture_lock(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git", "v1.0.0")],
    );

    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let (resolved_lock, failures) = lock.resolve_versions(&root);
    assert!(
        failures.is_empty(),
        "resolution should succeed: {failures:?}"
    );

    let head = repoweave::git::git_vcs()
        .head_revision(&root.join(repo_path))
        .unwrap();
    let entry = &resolved_lock.repo_map()
        [&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal")];
    assert_eq!(
        entry.version, head,
        "tag-form lock entry should be == HEAD after resolve_versions"
    );
    // Display form is preserved post-resolve so writing back keeps the tag.
    assert_eq!(entry.version.display_str(), "v1.0.0");
}

#[test]
fn lock_resolve_versions_unknown_revision_returns_failure() {
    // A lock pinning a nonexistent revision is reported in the failures list
    // and the entry is left as-is so callers can craft a meaningful error.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    let lock_path = project_dir.join("rwv.lock");
    common::fixture_lock(
        &project_dir,
        &[(
            repo_path,
            "https://github.com/acme/server.git",
            "v9.9.9-nonexistent",
        )],
    );

    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let (resolved_lock, failures) = lock.resolve_versions(&root);
    let repo = repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal");
    assert_eq!(failures.len(), 1, "exactly one failure expected");
    assert_eq!(
        failures[0].0, repo,
        "unknown revision should appear in failures"
    );
    assert_eq!(
        failures[0].1.as_str(),
        "v9.9.9-nonexistent",
        "raw version preserved in failure tuple"
    );
    assert!(
        !resolved_lock.contains_repo(&repo),
        "unresolvable entry must not appear in ResolvedLockFile"
    );
}

// ---------------------------------------------------------------------------
// 14. --commit flag: commits rwv.lock after writing
// ---------------------------------------------------------------------------

#[test]
fn lock_commit_flag_commits_lock_file() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let run_git = |args: &[&str]| {
        let out = common::git()
            .args(args)
            .current_dir(&root)
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

    // Init a git repo at the workspace root and configure local identity.
    run_git(&["init", "-b", "main"]);
    run_git(&["config", "user.email", "test@test.com"]);
    run_git(&["config", "user.name", "Test"]);

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Commit the initial workspace state so HEAD exists.
    run_git(&["add", "."]);
    run_git(&["commit", "-m", "initial"]);

    rwv_cmd()
        .args(["lock", "--commit"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Committed rwv.lock"));

    // Lock file must exist.
    assert!(project_dir.join("rwv.lock").exists());

    // A commit with the multi-repo summary message must appear in the log.
    let log = run_git(&["log", "--oneline"]);
    assert!(
        log.contains("lock: refresh"),
        "expected lock summary message in log: {log}"
    );
}

#[test]
fn lock_commit_flag_skips_when_lock_unchanged() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let run_git = |args: &[&str]| {
        let out = common::git()
            .args(args)
            .current_dir(&root)
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

    run_git(&["init", "-b", "main"]);
    run_git(&["config", "user.email", "test@test.com"]);
    run_git(&["config", "user.name", "Test"]);

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    run_git(&["add", "."]);
    run_git(&["commit", "-m", "initial"]);

    // First lock --commit creates the commit.
    rwv_cmd()
        .args(["lock", "--commit"])
        .current_dir(&project_dir)
        .assert()
        .success();

    let log_after_first = run_git(&["log", "--oneline"]);

    // Second lock --commit: lock unchanged — must skip the commit.
    rwv_cmd()
        .args(["lock", "--commit"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("nothing to commit"));

    let log_after_second = run_git(&["log", "--oneline"]);
    assert_eq!(
        log_after_first, log_after_second,
        "second --commit must not create a new commit when lock is unchanged"
    );
}

// ---------------------------------------------------------------------------
// 13. SME reproducer: tag-form lock + HEAD at tag commit reports `ok`
// ---------------------------------------------------------------------------

#[test]
fn status_ok_when_lock_pins_tag_at_current_head() {
    // From SME's gc-wisp-mdcj: a workspace where rwv.lock has
    // `"version": "v0.3.3"` for repoweave and HEAD is at the v0.3.3 commit
    // should report `ok` (not `ahead`).
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));
    let _ = common::git()
        .args(["tag", "v0.3.3"])
        .current_dir(root.join(repo_path))
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    // The reproducer pinned a tag rather than a SHA.
    common::fixture_lock(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git", "v0.3.3")],
    );

    rwv_cmd()
        .arg("status")
        .current_dir(&root)
        .assert()
        .success()
        .stdout(predicate::str::contains("[ok]"))
        .stdout(predicate::str::contains("[ahead]").not());
}

#[test]
fn check_locked_ok_when_lock_pins_tag_at_current_head() {
    // Same scenario as the status test, but exercising `rwv doctor --locked`.
    // Should exit 0 (no drift) and not flag the entry as `tip ≠ lock`.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));
    let _ = common::git()
        .args(["tag", "v0.3.3"])
        .current_dir(root.join(repo_path))
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .unwrap();

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    common::fixture_lock(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git", "v0.3.3")],
    );

    rwv_cmd()
        .args(["doctor", "--locked"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains(": ok"));
}

// ---------------------------------------------------------------------------
// 15. --commit flag: multi-repo summary message
// ---------------------------------------------------------------------------

#[test]
fn lock_commit_message_summarises_repos() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let run_git = |args: &[&str]| {
        let out = common::git()
            .args(args)
            .current_dir(&root)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("git command failed");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    run_git(&["init", "-b", "main"]);
    run_git(&["config", "user.email", "test@test.com"]);
    run_git(&["config", "user.name", "Test"]);

    let repo_a = "github/acme/server";
    let repo_b = "github/acme/client";
    init_git_repo(&root.join(repo_a));
    init_git_repo(&root.join(repo_b));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[
            (repo_a, "https://github.com/acme/server.git"),
            (repo_b, "https://github.com/acme/client.git"),
        ],
    );

    run_git(&["add", "."]);
    run_git(&["commit", "-m", "initial"]);

    rwv_cmd()
        .args(["lock", "--commit"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("Committed rwv.lock"));

    // Commit subject must name both repos.
    let log = run_git(&["log", "--format=%B", "-1"]);
    assert!(
        log.contains("lock: refresh 2 repos"),
        "expected '2 repos' in message: {log}"
    );
    assert!(log.contains(repo_a), "message should list repo A: {log}");
    assert!(log.contains(repo_b), "message should list repo B: {log}");
}

// ---------------------------------------------------------------------------
// 16. --commit flag: dirty check refuses non-lock uncommitted changes
// ---------------------------------------------------------------------------

#[test]
fn lock_commit_dirty_check_refuses_non_lock_changes() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let run_git = |args: &[&str]| {
        let out = common::git()
            .args(args)
            .current_dir(&root)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("git command failed");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    run_git(&["init", "-b", "main"]);
    run_git(&["config", "user.email", "test@test.com"]);
    run_git(&["config", "user.name", "Test"]);

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Commit initial state, then create a tracked file and modify it.
    run_git(&["add", "."]);
    run_git(&["commit", "-m", "initial"]);
    std::fs::write(root.join("work.txt"), "committed\n").unwrap();
    run_git(&["add", "work.txt"]);
    run_git(&["commit", "-m", "add work file"]);
    std::fs::write(root.join("work.txt"), "uncommitted change\n").unwrap();

    // --commit must refuse because work.txt has uncommitted changes.
    rwv_cmd()
        .args(["lock", "--commit"])
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("uncommitted")
                .or(predicate::str::contains("outside"))
                .or(predicate::str::contains("stash")),
        );
}

#[test]
fn lock_commit_dirty_check_refuses_staged_non_lock_changes() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let run_git = |args: &[&str]| {
        let out = common::git()
            .args(args)
            .current_dir(&root)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .expect("git command failed");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };

    run_git(&["init", "-b", "main"]);
    run_git(&["config", "user.email", "test@test.com"]);
    run_git(&["config", "user.name", "Test"]);

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    run_git(&["add", "."]);
    run_git(&["commit", "-m", "initial"]);

    // Stage a new file (would be bundled into the lock commit without the check).
    std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
    run_git(&["add", "staged.txt"]);

    rwv_cmd()
        .args(["lock", "--commit"])
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("uncommitted")
                .or(predicate::str::contains("outside"))
                .or(predicate::str::contains("stash")),
        );
}

// ============================================================================
// Stage D: LockFile<->ResolvedLockFile boundary invariants
// ============================================================================

#[test]
fn lock_file_from_path_yields_raw_entries() {
    // LockFile::from_path is the parse boundary: it produces a LockFile
    // whose entries' versions are RawRevisionId — no canonical-SHA
    // resolution has happened yet.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let repo_path = "github/acme/server";
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    let lock_path = project_dir.join("rwv.lock");
    common::fixture_lock(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git", "v1.0.0")],
    );

    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let entry = &lock.repo_map()
        [&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal")];
    // The static type of `entry.version` is `RawRevisionId`. We can only
    // ask for its string identity — there is no `display_str()` or
    // canonical-SHA accessor (those live on ResolvedRevisionId).
    assert_eq!(entry.version, repoweave::vcs::RawRevisionId::new("v1.0.0"));
}

#[test]
fn resolve_versions_surfaces_unknown_ref_in_failures() {
    // resolve_versions returns ResolvedLockFile + failure list. An unknown
    // ref does not appear in the resolved view and surfaces in failures
    // with its raw string intact, so callers can craft a precise
    // diagnostic ("lock pins unknown revision X").
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    let lock_path = project_dir.join("rwv.lock");
    common::fixture_lock(
        &project_dir,
        &[(
            repo_path,
            "https://github.com/acme/server.git",
            "deadbeef-not-a-real-ref",
        )],
    );

    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let (resolved, failures) = lock.resolve_versions(&root);
    let repo = repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal");
    assert!(
        !resolved.contains_repo(&repo),
        "unresolved entry must not appear in ResolvedLockFile"
    );
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0, repo);
    assert_eq!(failures[0].1.as_str(), "deadbeef-not-a-real-ref");
}

#[test]
fn resolve_versions_roundtrip_raw_then_resolved_json_shape() {
    // Both LockFile (raw) and ResolvedLockFile (post-resolve) serialize
    // to a single JSON string per version — the parse-boundary type does
    // not leak into the on-disk shape. A round-trip through
    // `from_path` -> `resolve_versions` -> `write_lock` -> `from_path`
    // preserves the version's display string for a tag-form entry.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));
    let _ = common::git()
        .args(["tag", "v1.0.0"])
        .current_dir(root.join(repo_path))
        .output()
        .unwrap();

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    let lock_path = project_dir.join("rwv.lock");
    common::fixture_lock(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git", "v1.0.0")],
    );

    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let (resolved, _failures) = lock.resolve_versions(&root);
    // A scratch directory rather than a scratch file name: what is under test
    // is the serialized round-trip, and `write_lock` publishes a lock file
    // rather than arbitrary bytes at an arbitrary path.
    let out_dir = project_dir.join("roundtrip");
    std::fs::create_dir_all(&out_dir).unwrap();
    let out_path = out_dir.join("rwv.lock");
    repoweave::lock::write_lock(&resolved, &out_path).unwrap();
    let round = std::fs::read_to_string(&out_path).unwrap();
    assert!(
        round.contains("\"version\": \"v1.0.0\""),
        "post-resolve JSON should preserve tag-form display string: {round}"
    );
    // And re-parsing through the parse boundary yields the same raw value.
    let reparsed = repoweave::manifest::LockFile::from_path(&out_path).unwrap();
    let entry = &reparsed.repo_map()
        [&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal")];
    assert_eq!(entry.version, repoweave::vcs::RawRevisionId::new("v1.0.0"));
}

// ---------------------------------------------------------------------------
// Regression: `rwv lock` succeeds over a conflict-markered rwv.lock
// ---------------------------------------------------------------------------
//
// Before the fix, `rwv lock` loaded the project via `Project::from_dir`,
// which hard-parses `rwv.lock` and errors on git conflict markers
// (`<<<<<<<`, `=======`, `>>>>>>>`). That turned the naive recovery
// sequence for a lock-only rebase conflict — `rwv lock; git add
// rwv.lock; git rebase --continue` — into a footgun: `rwv lock` failed,
// the operator kept the markered file, `git add + git rebase --continue`
// then silently committed the markers into the lock.
//
// Fix: `rwv lock` uses `Project::from_dir_skip_lock`, the parse-free
// loader — `rwv.lock` is derived state and must be regenerable OVER an
// arbitrarily corrupt existing file.
#[test]
fn lock_succeeds_over_conflict_markered_rwv_lock() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let sha = init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // raw lock bytes: a conflict-markered rwv.lock — the state a `git rebase`
    // that stopped on it leaves behind. Strict JSON parsing dies on the marker
    // lines and `Project::from_dir_skip_lock` bypasses that, so no writer that
    // parses before it writes can produce this file.
    let markered = format!(
        "{{\n  \"repositories\": {{\n\
         <<<<<<< HEAD\n\
             \"{path}\": {{ \"type\": \"git\", \"url\": \"https://github.com/acme/server.git\", \
         \"version\": \"cafefacecafefacecafefacecafefacecafeface\" }}\n\
         =======\n\
             \"{path}\": {{ \"type\": \"git\", \"url\": \"https://github.com/acme/server.git\", \
         \"version\": \"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\" }}\n\
         >>>>>>> upstream\n  }}\n}}\n",
        path = repo_path,
    );
    let lock_path = project_dir.join("rwv.lock");
    std::fs::write(&lock_path, &markered).unwrap();

    // Sanity: the current file really is unparseable via the strict loader.
    // (If this ever starts to succeed the regression is meaningless.)
    assert!(
        repoweave::manifest::LockFile::from_path(&lock_path).is_err(),
        "test precondition: markered rwv.lock must fail strict parse"
    );

    // The actual regression: `rwv lock` must succeed and rewrite the file.
    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    // Post-condition: no conflict markers remain, and the fresh lock
    // pins the manifest repo at its actual HEAD.
    let regen = std::fs::read_to_string(&lock_path).unwrap();
    assert!(
        !regen.contains("<<<<<<<") && !regen.contains(">>>>>>>") && !regen.contains("======="),
        "regenerated rwv.lock must not contain conflict markers, got:\n{regen}"
    );
    assert!(
        regen.contains(&sha),
        "regenerated rwv.lock must pin the manifest repo at HEAD ({sha}), got:\n{regen}"
    );

    // And the strict loader now parses it — the write really produced
    // clean JSON, not something merely "not conflict-markered".
    let reparsed = repoweave::manifest::LockFile::from_path(&lock_path)
        .expect("strict parse must succeed post-regeneration");
    let entry = &reparsed.repo_map()
        [&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal")];
    assert_eq!(entry.version.as_str(), &sha);
}

// ---------------------------------------------------------------------------
// 17. Detached HEAD member: lock succeeds with a warning
// ---------------------------------------------------------------------------

/// Helper: initialise a git repo, detach HEAD at the tip, and return the SHA.
fn init_git_repo_detached(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();

    let run = |args: &[&str], dir: &Path| -> String {
        let out = common::git()
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

    let sha = run(&["rev-parse", "HEAD"], path);

    // Detach HEAD at the current tip.
    run(&["checkout", "--detach", "HEAD"], path);

    sha
}

#[test]
fn lock_detached_head_member_succeeds_with_warning() {
    // A repo member in detached-HEAD state: `rwv lock` must succeed
    // (pinning the detached SHA) but emit a warning to stderr that names:
    //   - the state ("pinning detached HEAD")
    //   - the consequence ("no branch names this commit; a later fetch will
    //     materialize detached")
    //   - the next verb ("Create/checkout a branch if this is unintended")
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let sha = init_git_repo_detached(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Must succeed (not fail) — detached HEAD is a warning, not a hard error.
    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("detached HEAD"))
        .stderr(predicate::str::contains("no branch names this commit"))
        .stderr(predicate::str::contains("Create/checkout a branch"));

    // The lock file must be written with the detached SHA pinned.
    let lock_path = project_dir.join("rwv.lock");
    assert!(lock_path.exists(), "rwv.lock should be created");
    let lock = repoweave::manifest::LockFile::from_path(&lock_path).unwrap();
    let entry = lock
        .get_entry(&repoweave::manifest::RepoPath::new(repo_path).expect("known-safe literal"))
        .expect("lock should contain the repo");
    assert_eq!(
        entry.version.as_str(),
        &sha,
        "lock should pin the detached SHA"
    );
}

#[test]
fn lock_detached_head_warning_contains_short_sha() {
    // The warning must include the abbreviated SHA so the operator can
    // identify which commit is pinned without opening rwv.lock.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let sha = init_git_repo_detached(&root.join(repo_path));
    let short_sha = &sha[..7];

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains(short_sha));
}

#[test]
fn lock_normal_head_no_detached_warning() {
    // A repo on a named branch must NOT produce a detached-HEAD warning.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success()
        .stderr(predicate::str::contains("detached HEAD").not());
}

// ---------------------------------------------------------------------------
// 18. Unborn HEAD member: lock refuses with a clear message
// ---------------------------------------------------------------------------

/// Helper: create an empty git repo with no commits (unborn HEAD).
fn init_git_repo_unborn(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    let out = common::git()
        .args(["init", "-b", "main"])
        .current_dir(path)
        .output()
        .expect("git init failed to start");
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Deliberately do NOT make any commit — leaves HEAD unborn.
}

#[test]
fn lock_unborn_head_member_refuses_with_clear_message() {
    // A repo member that has no commits yet (unborn HEAD): `rwv lock` must
    // refuse and emit a clear message that:
    //   - names the repo path
    //   - names the state ("unborn HEAD")
    //   - tells the operator what to do ("make an initial commit")
    //
    // Before this fix, the raw git error ("ambiguous argument 'HEAD'")
    // leaked to the terminal.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo_unborn(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Must fail — cannot pin an unborn HEAD.
    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .failure()
        // Clear state name, not the raw git error.
        .stderr(predicate::str::contains("unborn HEAD"))
        // Operator action.
        .stderr(predicate::str::contains("initial commit"))
        // Raw git error must NOT leak.
        .stderr(predicate::str::contains("ambiguous argument").not());
}

#[test]
fn lock_unborn_head_does_not_write_lock_file() {
    // When the member has an unborn HEAD, no rwv.lock should be written —
    // an early failure before the write step is the safe outcome.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo_unborn(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    rwv_cmd()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .failure();

    assert!(
        !project_dir.join("rwv.lock").exists(),
        "rwv.lock must not be written when a member has an unborn HEAD"
    );
}
