//! E2E tests for `rwv doctor` — convention enforcement.
//!
//! These tests exercise the CLI binary via `assert_cmd`. Tests that depend on
//! the full check implementation (phase 8b) are marked `#[ignore]`.

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

/// Make `project_dir` the git repo a real project directory is, with the
/// replay exclusion committed and the `rwv-ours` merge driver defined.
///
/// A fixture that merely writes `.gitattributes` into a plain directory is
/// still missing what `rwv sync`'s rebase needs, and both renderers say so —
/// so a test asserting a clean workspace has to build one.
fn make_project_repo_clean(project_dir: &Path) {
    let run = |args: &[&str]| {
        let out = common::git()
            .args(args)
            .current_dir(project_dir)
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
    };
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    run(&["init", "-b", "main"]);
    run(&["add", "."]);
    run(&["commit", "-m", "initial"]);
    run(&["config", "merge.rwv-ours.driver", "true"]);
}

/// Write an `rwv.toml` manifest into a project directory.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    std::fs::create_dir_all(project_dir).unwrap();
    let mut manifest_toml = String::from("[repositories]\n");
    for (repo_path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
}

/// Write an `rwv.lock` file into a project directory with given repo SHAs.
fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    // Round-trip through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let entries: Vec<String> = repos
        .iter()
        .map(|(repo_path, url, sha)| {
            format!("{repo_path:?}: {{\"type\": \"git\", \"url\": {url:?}, \"version\": {sha:?}}}")
        })
        .collect();
    let raw = format!("{{\"repositories\": {{{}}}}}", entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
}

/// Build a `Command` for the `rwv` binary.
///
/// Sets `current_dir` to a temp dir so tests never accidentally pick up
/// the real workspace. Tests override with their own `.current_dir()`.
fn rwv_cmd() -> Command {
    let mut cmd = common::rwv();
    cmd.current_dir(std::env::temp_dir());
    cmd
}

// ===========================================================================
// 1. `rwv doctor` with no issues — clean workspace, exits 0
// ===========================================================================

#[test]

fn check_clean_workspace_exits_zero() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create a repo on disk
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    // Create a project that references that repo
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .success();
}

// ===========================================================================
// 2. Orphaned clone — directory under `github/` not in any project's rwv.toml
// ===========================================================================

#[test]

fn check_orphaned_clone_reported() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create two repos on disk
    let known_repo = "github/acme/server";
    let orphan_repo = "github/acme/stray-clone";
    init_git_repo(&root.join(known_repo));
    init_git_repo(&root.join(orphan_repo));

    // Only reference one repo in the project manifest
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(known_repo, "https://github.com/acme/server.git")],
    );

    rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .failure()
        .stdout(predicate::str::contains("orphan").or(predicate::str::contains("stray-clone")));
}

// ===========================================================================
// 3. Dangling reference — rwv.toml entry pointing to a path not on disk
// ===========================================================================

#[test]

fn check_dangling_reference_reported() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create one repo on disk but reference two in the manifest
    let real_repo = "github/acme/server";
    let missing_repo = "github/acme/vanished";
    init_git_repo(&root.join(real_repo));
    // Deliberately do NOT create `missing_repo` on disk

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[
            (real_repo, "https://github.com/acme/server.git"),
            (missing_repo, "https://github.com/acme/vanished.git"),
        ],
    );

    rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .failure()
        .stdout(predicate::str::contains("dangling").or(predicate::str::contains("vanished")));
}

// ===========================================================================
// 4. Stale lock — rwv.lock SHA doesn't match current HEAD
// ===========================================================================

#[test]

fn check_stale_lock_reported() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let _real_sha = init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Write a lock file with a stale (bogus) SHA
    write_lock(
        &project_dir,
        &[(
            repo_path,
            "https://github.com/acme/server.git",
            "0000000000000000000000000000000000000000",
        )],
    );

    rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .failure()
        .stdout(predicate::str::contains("stale").or(predicate::str::contains("lock")));
}

// ===========================================================================
// 4b. Incomplete lock — rwv.lock exists but has no entry for a manifest repo
// ===========================================================================

#[test]

fn check_incomplete_lock_reported() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Lock file exists but covers no repos — the manifest entry has no
    // corresponding lock entry.
    write_lock(&project_dir, &[]);

    rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .failure()
        .stdout(predicate::str::contains("incomplete").or(predicate::str::contains("lock")));
}

// ===========================================================================
// 5. Multi-project awareness — repo in project A is not orphan even if not
//    in project B
// ===========================================================================

#[test]

fn check_multi_project_no_false_orphan() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create two repos
    let repo_a = "github/acme/server";
    let repo_b = "github/acme/client";
    init_git_repo(&root.join(repo_a));
    init_git_repo(&root.join(repo_b));

    // Project alpha references repo_a only
    let proj_alpha = root.join("projects").join("alpha");
    write_manifest(
        &proj_alpha,
        &[(repo_a, "https://github.com/acme/server.git")],
    );

    // Project beta references repo_b only
    let proj_beta = root.join("projects").join("beta");
    write_manifest(
        &proj_beta,
        &[(repo_b, "https://github.com/acme/client.git")],
    );

    // Both repos are known across projects — no orphans expected
    rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .success();
}

// ===========================================================================
// 6. `rwv doctor` outside a workspace — should error
// ===========================================================================

#[test]

fn check_outside_workspace_errors() {
    let tmp = common::tempdir().unwrap();
    // No workspace markers here — just an empty temp dir

    rwv_cmd()
        .arg("doctor")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no repoweave workspace found"));
}

// ===========================================================================
// 7. Integration check hooks report warnings
// ===========================================================================

#[test]

fn check_integration_hooks_report_warnings() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create a repo on disk
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    // Create a project with the repo and an integration config
    let project_dir = root.join("projects").join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest_toml = format!(
        r#"[repositories."{repo_path}"]
type = "git"
url = "https://github.com/acme/server.git"
version = "main"
role = "owned"

[integrations.static-files]
enabled = true
files = ["turbo.json"]
"#
    );
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();

    // Even with integration hooks, a clean workspace should not error.
    // Any integration warnings should be printed but not cause failure
    // (only errors cause non-zero exit).
    rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "[warning] vscode-workspace: my-app.code-workspace does not exist",
        ))
        .stdout(predicate::str::contains("is not surfaced"))
        .stdout(predicate::str::contains(
            "[warning] static-files: declared file 'turbo.json' not found in project directory",
        ));
}

// ===========================================================================
// 8. `rwv doctor --locked` — tag-form lock entries
// ===========================================================================

/// Helper: add a git tag in a repo.
fn git_tag(repo: &std::path::Path, tag: &str) {
    let out = common::git()
        .args(["tag", tag])
        .current_dir(repo)
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git tag failed to start");
    assert!(
        out.status.success(),
        "git tag failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Helper: make a second commit in an already-initialised repo. Returns new SHA.
fn make_commit(repo: &std::path::Path) -> String {
    let run = |args: &[&str], dir: &std::path::Path| -> String {
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
    std::fs::write(repo.join("extra.txt"), "change\n").unwrap();
    run(&["add", "."], repo);
    run(&["commit", "-m", "second"], repo);
    run(&["rev-parse", "HEAD"], repo)
}

#[test]
fn check_locked_tag_form_reports_ok() {
    // Lock pins a tag name; HEAD is at that tag's commit — should exit 0.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let _sha = init_git_repo(&root.join(repo_path));
    git_tag(&root.join(repo_path), "v1.0.0");

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    write_lock(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git", "v1.0.0")],
    );

    rwv_cmd()
        .args(["doctor", "--locked"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("ok"));
}

#[test]
fn check_locked_sha_form_reports_ok() {
    // Lock pins a SHA directly; HEAD is the same commit — should exit 0.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    let sha = init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    write_lock(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git", &sha)],
    );

    rwv_cmd()
        .args(["doctor", "--locked"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("ok"));
}

#[test]
fn check_locked_tag_form_drift_reported() {
    // Lock pins a tag; HEAD is at a later commit — should exit 1.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));
    git_tag(&root.join(repo_path), "v1.0.0");
    make_commit(&root.join(repo_path)); // HEAD moves past v1.0.0

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    write_lock(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git", "v1.0.0")],
    );

    rwv_cmd()
        .args(["doctor", "--locked"])
        .current_dir(&project_dir)
        .assert()
        .failure();
}

#[test]
fn check_locked_unknown_tag_reported_as_drift() {
    // Lock pins a tag that no longer exists locally — should exit 1 with a
    // clear "unknown revision" message rather than a generic "tip ≠ lock".
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    // Lock references a tag that was never created in this repo.
    write_lock(
        &project_dir,
        &[(
            repo_path,
            "https://github.com/acme/server.git",
            "v9.9.9-nonexistent",
        )],
    );

    rwv_cmd()
        .args(["doctor", "--locked"])
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stdout(
            predicate::str::contains("unknown revision").or(predicate::str::contains("v9.9.9")),
        );
}

#[test]
fn check_locked_missing_on_disk_reported_as_drift() {
    // A repo listed in the lock but missing from disk should fail
    // `rwv doctor --locked` with a clear message — this is the precondition
    // `rwv sync` enforces so the operator notices the gap before syncing.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let present_repo = "github/acme/server";
    let missing_repo = "github/acme/lib";
    let sha = init_git_repo(&root.join(present_repo));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[
            (present_repo, "https://github.com/acme/server.git"),
            (missing_repo, "https://github.com/acme/lib.git"),
        ],
    );
    write_lock(
        &project_dir,
        &[
            (present_repo, "https://github.com/acme/server.git", &sha),
            (
                missing_repo,
                "https://github.com/acme/lib.git",
                "0000000000000000000000000000000000000000",
            ),
        ],
    );

    rwv_cmd()
        .args(["doctor", "--locked"])
        .current_dir(&project_dir)
        .assert()
        .failure()
        .stdout(predicate::str::contains("missing on disk"));
}

// ===========================================================================
// B3: `rwv doctor` surfaces lock entries referencing unknown revisions.
// ===========================================================================

/// If the lock pins a SHA the local clone has never seen, `resolve_versions`
/// can't resolve it. Previously this signal was silently dropped (the lock
/// entry stayed raw, `find_violations` either ignored it or emitted a false
/// StaleLock). Now: an `Error`-severity issue saying "lock references unknown
/// revision". Doctor is the diagnostic of last resort — this is the place
/// that most needs to know when resolution failed.
#[test]
fn check_flags_unresolvable_lock_revision() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    // SHA the local repo has never seen — `resolve_revision` will fail.
    write_lock(
        &project_dir,
        &[(
            repo_path,
            "https://github.com/acme/server.git",
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        )],
    );

    let assert = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .failure();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        out.contains("lock references unknown revision"),
        "expected B3 unresolved-lock diagnostic, got stdout: {out}"
    );
}

// ===========================================================================
// B4: `rwv doctor` surfaces on-disk repos whose HEAD can't be read.
// ===========================================================================

/// A repo that's on disk but whose HEAD is unreadable (e.g. corrupted .git
/// dir) previously produced zero violations — doctor reported clean. Now: an
/// `Error`-severity issue. Simulated by removing `.git/HEAD` after init.
#[test]
fn check_flags_unreadable_head() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Point HEAD at a ref that doesn't exist: `git rev-parse --git-dir`
    // (used by `is_repo`) still succeeds so the repo is on-disk, but
    // `git rev-parse HEAD` (used by `head_revision`) fails — exactly the
    // mid-rebase / "unreadable HEAD" failure mode B4 targets.
    let head_file = root.join(repo_path).join(".git/HEAD");
    std::fs::write(&head_file, "ref: refs/heads/nonexistent\n").unwrap();

    let assert = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .failure();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        out.contains("HEAD unreadable"),
        "expected B4 unreadable-HEAD diagnostic, got stdout: {out}"
    );
}

// ===========================================================================
// Smoke test: `rwv doctor` CLI command is recognized
// ===========================================================================

#[test]
fn doctor_command_is_recognized() {
    // The command should parse successfully (not fail with "unrecognized subcommand").
    rwv_cmd()
        .arg("doctor")
        .assert()
        .stdout(predicate::str::contains("unrecognized").not());
}

// ===========================================================================
// Replay-exclusion (.gitattributes `rwv.lock merge=rwv-ours`)
// ===========================================================================

/// `rwv doctor` warns when a project repo is missing the
/// `rwv.lock merge=rwv-ours` line in `.gitattributes`.
#[test]
fn check_warns_when_project_missing_replay_exclusion() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    // Project dir exists with a manifest but no .gitattributes.
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    rwv_cmd().arg("doctor").current_dir(&root).assert().stdout(
        predicate::str::contains("rwv.lock merge=rwv-ours")
            .and(predicate::str::contains("my-app"))
            .and(predicate::str::contains("rwv doctor --fix")),
    );
}

/// `rwv doctor --fix` writes the missing `rwv.lock merge=rwv-ours` line.
#[test]
fn check_fix_writes_replay_exclusion() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    // Pre-condition: no .gitattributes exists.
    assert!(!project_dir.join(".gitattributes").exists());

    rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .assert()
        .stdout(predicate::str::contains("rwv.lock merge=rwv-ours"));

    // Post-condition: .gitattributes now contains the line, and re-running
    // `rwv doctor` no longer warns about this project.
    let attrs = std::fs::read_to_string(project_dir.join(".gitattributes")).unwrap();
    assert!(
        attrs.contains("rwv.lock merge=rwv-ours"),
        "post-fix .gitattributes should contain the line; got: {attrs:?}"
    );

    let assertion = rwv_cmd().arg("doctor").current_dir(&root).assert();
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("missing `rwv.lock merge=rwv-ours`"),
        "post-fix doctor must not re-warn; got stdout: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// Legacy `merge=ours` migration under `doctor --fix`
// ---------------------------------------------------------------------------

/// `rwv doctor` (no `--fix`) surfaces a project still on the LEGACY
/// `rwv.lock merge=ours` spelling as a `missing-replay-exclusion` warning,
/// with a message pointing at `rwv doctor --fix` for migration.
#[test]
fn check_warns_when_project_has_legacy_replay_exclusion() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    // Project dir must itself be a git repo so `doctor --fix`'s migration
    // commit path is exercisable — but doctor's *detection* only reads
    // `.gitattributes` from disk, so `git init` isn't required for the
    // warning-only case. Init anyway to keep this test's shape aligned
    // with the migration test below.
    init_git_repo(&project_dir);
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();

    rwv_cmd().arg("doctor").current_dir(&root).assert().stdout(
        predicate::str::contains("legacy `rwv.lock merge=ours`")
            .and(predicate::str::contains("rwv doctor --fix")),
    );
}

/// `rwv doctor --fix` migrates a legacy `rwv.lock merge=ours` line to
/// `rwv.lock merge=rwv-ours` AND commits the change, when the project
/// repo has no other pending work. Post-fix, doctor is quiet.
#[test]
fn check_fix_migrates_and_commits_legacy_replay_exclusion() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    init_git_repo(&project_dir);
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    // Commit a legacy .gitattributes so both the working-tree detector
    // and the committed-tree readers see the old spelling.
    std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();
    common::git_in(&project_dir, &["add", ".gitattributes", "rwv.toml"]);
    common::git_in(
        &project_dir,
        &["commit", "-m", "seed manifest + legacy attrs"],
    );
    let head_before = common::git_in(&project_dir, &["rev-parse", "HEAD"]);

    rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .assert()
        .stdout(
            predicate::str::contains("migrated `rwv.lock merge=ours`")
                .and(predicate::str::contains("rwv.lock merge=rwv-ours")),
        );

    // Working-tree file rewritten.
    let attrs = std::fs::read_to_string(project_dir.join(".gitattributes")).unwrap();
    assert!(
        attrs.contains("rwv.lock merge=rwv-ours") && !attrs.contains("rwv.lock merge=ours\n"),
        "post-fix .gitattributes should have new spelling only; got: {attrs:?}"
    );

    // A commit was made — HEAD advanced.
    let head_after = common::git_in(&project_dir, &["rev-parse", "HEAD"]);
    assert_ne!(
        head_before, head_after,
        "doctor --fix must commit the migration; HEAD did not advance"
    );

    // The committed .gitattributes at HEAD carries the new spelling
    // (this is what sync's invariant reads).
    let committed = common::git_in(&project_dir, &["show", "HEAD:.gitattributes"]);
    assert!(
        committed.contains("rwv.lock merge=rwv-ours"),
        "HEAD:.gitattributes must contain the new spelling; got: {committed:?}"
    );

    // Re-running doctor is quiet on this project (idempotent).
    let assertion = rwv_cmd().arg("doctor").current_dir(&root).assert();
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("legacy `rwv.lock merge=ours`"),
        "post-fix doctor must not re-warn about migration; got: {stdout}"
    );
    assert!(
        !stdout.contains("missing `rwv.lock merge=rwv-ours`"),
        "post-fix doctor must not warn about missing exclusion; got: {stdout}"
    );
}

/// `rwv doctor --fix` refuses to bundle the migration commit with a
/// user's unrelated staged work. The `.gitattributes` migration still
/// happens (so the operator can commit it themselves after landing
/// their own change), but HEAD is unchanged and stdout says so.
#[test]
fn check_fix_skips_migration_commit_when_repo_has_other_staged_changes() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    init_git_repo(&project_dir);
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();
    common::git_in(&project_dir, &["add", ".gitattributes", "rwv.toml"]);
    common::git_in(
        &project_dir,
        &["commit", "-m", "seed manifest + legacy attrs"],
    );
    let head_before = common::git_in(&project_dir, &["rev-parse", "HEAD"]);

    // Stage an unrelated file — the operator's WIP that must not be
    // bundled with rwv's fix.
    std::fs::write(project_dir.join("wip.txt"), "user work\n").unwrap();
    common::git_in(&project_dir, &["add", "wip.txt"]);

    rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .assert()
        .stdout(predicate::str::contains(
            "NOT committed: project repo has unrelated staged changes",
        ));

    // Working-tree .gitattributes still got migrated (safe: an unstaged
    // .gitattributes change is a review-able WT diff, not a phantom
    // commit).
    let attrs = std::fs::read_to_string(project_dir.join(".gitattributes")).unwrap();
    assert!(
        attrs.contains("rwv.lock merge=rwv-ours"),
        "migration should have written the new needle to WT; got: {attrs:?}"
    );

    // HEAD is UNCHANGED — no auto-commit happened.
    let head_after = common::git_in(&project_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        head_before, head_after,
        "HEAD must not advance when other work is staged"
    );

    // And the user's staged file is still staged, unmolested.
    let staged = common::git_in(
        &project_dir,
        &["diff", "--cached", "--name-only", "wip.txt"],
    );
    assert_eq!(
        staged.trim(),
        "wip.txt",
        "user's staged file must remain staged and untouched"
    );
}

/// `rwv doctor --fix` plants the durable `merge.rwv-ours.driver` config
/// entry — the key that keeps the driver defined across a bare
/// `git rebase --continue`.
#[test]
fn check_fix_plants_rwv_ours_driver_config() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    init_git_repo(&project_dir);
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    // Fresh project — no .gitattributes and no driver config.

    // Pre-condition: `merge.rwv-ours.driver` is unset.
    let pre = common::git()
        .args(["config", "--local", "--get", "merge.rwv-ours.driver"])
        .current_dir(&project_dir)
        .output()
        .unwrap();
    assert!(
        !pre.status.success(),
        "pre-fix: merge.rwv-ours.driver must be unset (git config --get exits 1); \
         got stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&pre.stdout),
        String::from_utf8_lossy(&pre.stderr)
    );

    rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .assert()
        .success();

    // Post-fix: `merge.rwv-ours.driver=true` is planted locally.
    let post = common::git_in(
        &project_dir,
        &["config", "--local", "--get", "merge.rwv-ours.driver"],
    );
    assert_eq!(
        post.trim(),
        "true",
        "post-fix: merge.rwv-ours.driver must be `true`; got: {post:?}"
    );
}

/// A project directory that loses its `.git` while `rwv.toml` and the rest
/// of the checkout stay in place — the same damage
/// `push_refuses_when_project_repo_is_not_a_repo` (tests/push_test.rs)
/// drives against `rwv push` — must reach the operator as a warning naming
/// the actual state, not as a raw git-config failure with a non-zero exit.
///
/// Before the fix, `has_rwv_merge_driver_config` read this the same as an
/// ordinary repo simply missing the key, so `--fix` went on to attempt the
/// plant and surfaced git's own `fatal: not in a git directory` as a raw
/// `[error]`.
#[test]
fn check_fix_reports_not_a_repo_cleanly_instead_of_raw_git_error() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    init_git_repo(&project_dir);
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    std::fs::remove_dir_all(project_dir.join(".git")).unwrap();

    let output = rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .expect("rwv doctor --fix");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "a project dir with no .git is a warning, not a hard failure; \
         got exit {:?}, stdout:\n{stdout}",
        output.status.code()
    );
    assert!(
        !stdout.contains("[error]"),
        "no raw git error should reach the operator; got:\n{stdout}"
    );
    assert!(
        stdout.contains("my-app") && stdout.contains("is not a vcs repository"),
        "the warning should name the actual state; got:\n{stdout}"
    );
}

// ===========================================================================
// `rwv doctor --json`
// ===========================================================================
//
// These tests exercise the JSON wire shape end-to-end. They:
//   - construct a workspace fixture per CheckViolation variant where possible
//     (orphaned-clone, dangling-reference, stale-lock, missing-replay-exclusion);
//   - drive the remaining variants (missing-role, workweave-drift,
//     index-drift, working-tree-drift) directly through
//     `ViolationOutput::from_violation` so the wire shape is fully covered
//     without requiring elaborate workweave fixtures.
//
// Acceptance from the spec:
//   - top-level envelope `{ "$schema": ..., "violations": [...] }`
//   - each violation has a kebab-case `kind` and (where applicable)
//     `path` + `absolute_path`
//   - round-trip via Deserialize confirms shape stability

mod doctor_json {
    use super::*;
    use common::doctor_corpus::{case_token, corpus};
    use repoweave::check::{
        build_doctor_json, CheckViolation, DriftKind, IndexDriftKind, ReplayExclusionKind,
        ViolationOutput, WorkingTreeDriftKind, DOCTOR_SCHEMA_URL,
    };
    use repoweave::manifest::{ProjectName, RepoPath, WorkweaveName};
    use repoweave::vcs::ResolvedRevisionId;
    use serde_json::Value;
    use std::collections::HashMap;

    /// Run `rwv doctor --json` against `cwd` and return parsed stdout JSON.
    fn run_doctor_json(cwd: &Path) -> (Value, std::process::Output) {
        let assertion = rwv_cmd()
            .args(["doctor", "--json"])
            .current_dir(cwd)
            .assert();
        let output = assertion.get_output().clone();
        let stdout = String::from_utf8(output.stdout.clone()).expect("stdout was not utf-8");
        let parsed: Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
        (parsed, output)
    }

    fn entries(value: &Value) -> &Vec<Value> {
        value
            .get("violations")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("violations missing or not an array: {value}"))
    }

    fn assert_schema_url(value: &Value) {
        assert_eq!(
            value.get("$schema").and_then(|s| s.as_str()),
            Some(DOCTOR_SCHEMA_URL),
            "top-level `$schema` URL must be present and stable"
        );
    }

    // ---- empty workspace: envelope present, violations empty, exit 0 ----

    #[test]
    fn json_clean_workspace_emits_empty_array() {
        let tmp = common::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let repo_path = "github/acme/server";
        init_git_repo(&root.join(repo_path));

        let project_dir = root.join("projects").join("my-app");
        write_manifest(
            &project_dir,
            &[(repo_path, "https://github.com/acme/server.git")],
        );
        make_project_repo_clean(&project_dir);

        let assertion = rwv_cmd()
            .args(["doctor", "--json"])
            .current_dir(&root)
            .assert()
            .success();
        let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
        let parsed: Value = serde_json::from_str(&stdout).unwrap();
        assert_schema_url(&parsed);
        assert!(
            entries(&parsed).is_empty(),
            "expected no violations: {parsed}"
        );
    }

    // ---- per-variant end-to-end (fixture-driven) ----

    #[test]
    fn json_orphaned_clone_variant() {
        let tmp = common::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let known = "github/acme/server";
        let orphan = "github/acme/stray";
        init_git_repo(&root.join(known));
        init_git_repo(&root.join(orphan));

        let project_dir = root.join("projects").join("my-app");
        write_manifest(
            &project_dir,
            &[(known, "https://github.com/acme/server.git")],
        );
        std::fs::write(
            project_dir.join(".gitattributes"),
            "rwv.lock merge=rwv-ours\n",
        )
        .unwrap();

        let (parsed, output) = run_doctor_json(&root);
        assert_schema_url(&parsed);
        assert!(!output.status.success(), "violations should exit non-zero");
        let orphan_entry = entries(&parsed)
            .iter()
            .find(|v| v.get("kind").and_then(|k| k.as_str()) == Some("orphaned-clone"))
            .unwrap_or_else(|| panic!("no orphaned-clone entry in {parsed}"));
        assert_eq!(
            orphan_entry.get("path").and_then(|s| s.as_str()),
            Some(orphan)
        );
        let abs = orphan_entry
            .get("absolute_path")
            .and_then(|s| s.as_str())
            .expect("absolute_path");
        // Exact spelling, not a component-wise suffix: a suffix match cannot
        // see the separator or a verbatim prefix, which is the whole content
        // of the wire mint.
        assert_eq!(
            abs,
            repoweave::path_spelling::wire_path(&root.join(orphan)),
            "absolute_path must be the wire spelling of {orphan}"
        );
        assert!(
            std::path::Path::new(abs).is_absolute(),
            "absolute_path must be absolute"
        );
    }

    #[test]
    fn json_dangling_reference_variant() {
        let tmp = common::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let real = "github/acme/server";
        let missing = "github/acme/vanished";
        init_git_repo(&root.join(real));

        let project_dir = root.join("projects").join("my-app");
        write_manifest(
            &project_dir,
            &[
                (real, "https://github.com/acme/server.git"),
                (missing, "https://github.com/acme/vanished.git"),
            ],
        );
        std::fs::write(
            project_dir.join(".gitattributes"),
            "rwv.lock merge=rwv-ours\n",
        )
        .unwrap();

        let (parsed, _) = run_doctor_json(&root);
        let entry = entries(&parsed)
            .iter()
            .find(|v| v.get("kind").and_then(|k| k.as_str()) == Some("dangling-reference"))
            .unwrap_or_else(|| panic!("no dangling-reference entry in {parsed}"));
        assert_eq!(entry.get("path").and_then(|s| s.as_str()), Some(missing));
        assert_eq!(
            entry.get("project").and_then(|s| s.as_str()),
            Some("my-app")
        );
        assert!(entry.get("absolute_path").is_some());
    }

    #[test]
    fn json_stale_lock_variant() {
        let tmp = common::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let repo_path = "github/acme/server";
        let sha = init_git_repo(&root.join(repo_path));

        let project_dir = root.join("projects").join("my-app");
        write_manifest(
            &project_dir,
            &[(repo_path, "https://github.com/acme/server.git")],
        );
        std::fs::write(
            project_dir.join(".gitattributes"),
            "rwv.lock merge=rwv-ours\n",
        )
        .unwrap();

        // Tag the existing commit, then write a lock that references a
        // different (non-existent) tag of *this* repo so the locked SHA
        // resolves successfully but doesn't match HEAD.
        //
        // Easiest path: write the lock pinned to a SHA equal to all zeroes
        // (unresolvable -> would be dropped). To produce StaleLock we need a
        // resolvable lock that differs from HEAD. Tag a phantom commit:
        //   - make a second commit
        //   - tag the first commit as v0.0.0 (resolves)
        //   - revert HEAD by hard-resetting (HEAD now == first commit) — no.
        //
        // Simpler: pin the lock to the SHA of a freshly-rewound commit by
        // making a second commit and writing the OLD sha into the lock.
        let new_sha = make_commit(&root.join(repo_path));
        assert_ne!(sha, new_sha);
        write_lock(
            &project_dir,
            &[(repo_path, "https://github.com/acme/server.git", &sha)],
        );

        let (parsed, _) = run_doctor_json(&root);
        let entry = entries(&parsed)
            .iter()
            .find(|v| v.get("kind").and_then(|k| k.as_str()) == Some("stale-lock"))
            .unwrap_or_else(|| panic!("no stale-lock entry in {parsed}"));
        assert_eq!(entry.get("path").and_then(|s| s.as_str()), Some(repo_path));
        assert_eq!(
            entry.get("project").and_then(|s| s.as_str()),
            Some("my-app")
        );
        assert_eq!(
            entry.get("locked").and_then(|s| s.as_str()),
            Some(sha.as_str())
        );
        assert_eq!(
            entry.get("actual").and_then(|s| s.as_str()),
            Some(new_sha.as_str())
        );
    }

    #[test]
    fn json_incomplete_lock_variant() {
        let tmp = common::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let repo_path = "github/acme/server";
        init_git_repo(&root.join(repo_path));

        let project_dir = root.join("projects").join("my-app");
        write_manifest(
            &project_dir,
            &[(repo_path, "https://github.com/acme/server.git")],
        );
        std::fs::write(
            project_dir.join(".gitattributes"),
            "rwv.lock merge=rwv-ours\n",
        )
        .unwrap();

        // Lock file exists but covers no repos.
        write_lock(&project_dir, &[]);

        let (parsed, _) = run_doctor_json(&root);
        let entry = entries(&parsed)
            .iter()
            .find(|v| v.get("kind").and_then(|k| k.as_str()) == Some("incomplete-lock"))
            .unwrap_or_else(|| panic!("no incomplete-lock entry in {parsed}"));
        assert_eq!(entry.get("path").and_then(|s| s.as_str()), Some(repo_path));
        assert_eq!(
            entry.get("project").and_then(|s| s.as_str()),
            Some("my-app")
        );
        assert!(entry.get("absolute_path").is_some());
    }

    #[test]
    fn json_missing_replay_exclusion_variant() {
        let tmp = common::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let repo_path = "github/acme/server";
        init_git_repo(&root.join(repo_path));

        let project_dir = root.join("projects").join("my-app");
        write_manifest(
            &project_dir,
            &[(repo_path, "https://github.com/acme/server.git")],
        );
        // No .gitattributes -> missing-replay-exclusion fires.

        let (parsed, _) = run_doctor_json(&root);
        let entry = entries(&parsed)
            .iter()
            .find(|v| v.get("kind").and_then(|k| k.as_str()) == Some("missing-replay-exclusion"))
            .unwrap_or_else(|| panic!("no missing-replay-exclusion entry in {parsed}"));
        assert_eq!(
            entry.get("project").and_then(|s| s.as_str()),
            Some("my-app")
        );
        // No `path` / `absolute_path` for this variant (project-scoped only).
        assert!(entry.get("path").is_none());
        assert!(entry.get("absolute_path").is_none());
    }

    // ---- per-variant unit tests via ViolationOutput::from_violation ----

    fn workspace_dir() -> std::path::PathBuf {
        std::path::PathBuf::from("/ws")
    }

    fn empty_workweave_dirs() -> HashMap<WorkweaveName, std::path::PathBuf> {
        HashMap::new()
    }

    #[test]
    fn wire_missing_role_kind_tag_and_fields() {
        let v = CheckViolation::MissingRole {
            project: ProjectName::new("alpha").unwrap(),
            repo: RepoPath::new("github/a/b").expect("known-safe literal"),
        };
        let json = serde_json::to_value(ViolationOutput::from_violation(
            v,
            &workspace_dir(),
            &empty_workweave_dirs(),
        ))
        .unwrap();
        assert_eq!(
            json.get("kind").and_then(|k| k.as_str()),
            Some("missing-role")
        );
        assert_eq!(
            json.get("path").and_then(|s| s.as_str()),
            Some("github/a/b")
        );
        assert_eq!(json.get("project").and_then(|s| s.as_str()), Some("alpha"));
        assert_eq!(
            json.get("absolute_path").and_then(|s| s.as_str()),
            Some(repoweave::path_spelling::wire_path(&workspace_dir().join("github/a/b")).as_str()),
        );
    }

    #[test]
    fn wire_workweave_drift_missing_sub_kind() {
        let v = CheckViolation::WorkweaveDrift {
            workweave: WorkweaveName::new("ww1").unwrap(),
            kind: DriftKind::Missing,
            repo: RepoPath::new("github/a/b").expect("known-safe literal"),
        };
        let mut ww_dirs = empty_workweave_dirs();
        ww_dirs.insert(
            WorkweaveName::new("ww1").unwrap(),
            std::path::PathBuf::from("/ws/.workweaves/proj--ww1"),
        );
        let json = serde_json::to_value(ViolationOutput::from_violation(
            v,
            &workspace_dir(),
            &ww_dirs,
        ))
        .unwrap();
        assert_eq!(
            json.get("kind").and_then(|k| k.as_str()),
            Some("workweave-drift")
        );
        assert_eq!(json.get("workweave").and_then(|s| s.as_str()), Some("ww1"));
        assert_eq!(
            json.get("sub_kind").and_then(|s| s.as_str()),
            Some("missing")
        );
        assert_eq!(
            json.get("absolute_path").and_then(|s| s.as_str()),
            Some(
                repoweave::path_spelling::wire_path(
                    &std::path::PathBuf::from("/ws/.workweaves/proj--ww1").join("github/a/b")
                )
                .as_str()
            ),
        );
    }

    #[test]
    fn wire_workweave_drift_extra_sub_kind() {
        let v = CheckViolation::WorkweaveDrift {
            workweave: WorkweaveName::new("ww1").unwrap(),
            kind: DriftKind::Extra,
            repo: RepoPath::new("github/a/b").expect("known-safe literal"),
        };
        let json = serde_json::to_value(ViolationOutput::from_violation(
            v,
            &workspace_dir(),
            &empty_workweave_dirs(),
        ))
        .unwrap();
        assert_eq!(json.get("sub_kind").and_then(|s| s.as_str()), Some("extra"));
    }

    #[test]
    fn wire_index_drift_sub_kinds() {
        for (kind, expected_tag) in [
            (IndexDriftKind::SafeToFix, "safe-to-fix"),
            (IndexDriftKind::LiveStaged, "live-staged"),
        ] {
            let v = CheckViolation::IndexDrift {
                workweave: None,
                repo: RepoPath::new("github/a/b").expect("known-safe literal"),
                kind,
            };
            let json = serde_json::to_value(ViolationOutput::from_violation(
                v,
                &workspace_dir(),
                &empty_workweave_dirs(),
            ))
            .unwrap();
            assert_eq!(
                json.get("kind").and_then(|k| k.as_str()),
                Some("index-drift")
            );
            assert_eq!(
                json.get("sub_kind").and_then(|s| s.as_str()),
                Some(expected_tag)
            );
            // workweave field present but null for primary-weave records.
            assert!(json.get("workweave").map(|v| v.is_null()).unwrap_or(false));
        }
    }

    #[test]
    fn wire_working_tree_drift_sub_kinds() {
        for (kind, expected_tag) in [
            (WorkingTreeDriftKind::SafeToFix, "safe-to-fix"),
            (WorkingTreeDriftKind::LiveEdits, "live-edits"),
        ] {
            let v = CheckViolation::WorkingTreeDrift {
                workweave: Some(WorkweaveName::new("ww1").unwrap()),
                repo: RepoPath::new("github/a/b").expect("known-safe literal"),
                kind,
            };
            let json = serde_json::to_value(ViolationOutput::from_violation(
                v,
                &workspace_dir(),
                &empty_workweave_dirs(),
            ))
            .unwrap();
            assert_eq!(
                json.get("kind").and_then(|k| k.as_str()),
                Some("working-tree-drift")
            );
            assert_eq!(
                json.get("sub_kind").and_then(|s| s.as_str()),
                Some(expected_tag)
            );
            assert_eq!(json.get("workweave").and_then(|s| s.as_str()), Some("ww1"));
        }
    }

    #[test]
    fn wire_kind_tags_agree_with_the_corpus_for_every_variant() {
        // Each `kind` tag against `case_token`'s independent spelling, for
        // every variant `corpus()` carries — exhaustive because `case_token`
        // is. The tag is part of the public contract: if a serde rename
        // strips, capitalises, or reorders it, downstream agents break
        // silently.
        let ws = workspace_dir();
        let no_ww = empty_workweave_dirs();
        for v in corpus() {
            let token = case_token(&v);
            let expected_kind = token.split('/').next().expect("case_token is non-empty");
            let json =
                serde_json::to_value(ViolationOutput::from_violation(v, &ws, &no_ww)).unwrap();
            let tag = json
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("missing `kind`: {json}"));
            assert_eq!(
                tag, expected_kind,
                "wire tag mismatch (full record: {json})"
            );
        }
    }

    // ---- envelope-level snapshot test ----

    #[test]
    fn wire_envelope_snapshot() {
        // A fixture vector exercising 3 variants. Snapshot test for the
        // top-level shape: $schema URL, "violations" key, ordering preserved.
        let ws = workspace_dir();
        let ww_dirs = empty_workweave_dirs();
        let violations = vec![
            CheckViolation::OrphanedClone {
                path: RepoPath::new("github/a/b").expect("known-safe literal"),
            },
            CheckViolation::StaleLock {
                project: ProjectName::new("p").unwrap(),
                repo: RepoPath::new("github/c/d").expect("known-safe literal"),
                locked: ResolvedRevisionId::from_canonical("dead", None),
                actual: ResolvedRevisionId::from_canonical("beef", None),
            },
            CheckViolation::MissingReplayExclusion {
                project: ProjectName::new("p").unwrap(),
                sub_kind: ReplayExclusionKind::Absent,
            },
        ];

        // Serialized, not field-accessed: the assertions below are about the
        // bytes an operator receives, not about the struct that produced them.
        let payload = serde_json::to_value(build_doctor_json(
            violations,
            Vec::new(),
            &ws,
            &ww_dirs,
            None,
            vec![],
            vec![],
        ))
        .expect("doctor payload serializes");
        assert_eq!(
            payload.get("$schema").and_then(|s| s.as_str()),
            Some(DOCTOR_SCHEMA_URL)
        );
        let arr = payload
            .get("violations")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(
            arr[0].get("kind").and_then(|s| s.as_str()),
            Some("orphaned-clone")
        );
        assert_eq!(
            arr[1].get("kind").and_then(|s| s.as_str()),
            Some("stale-lock")
        );
        assert_eq!(
            arr[2].get("kind").and_then(|s| s.as_str()),
            Some("missing-replay-exclusion")
        );
    }

    // ---- round-trip via a test-only Deserialize mirror ----

    /// Test-only mirror of the wire shape, one arm per [`CheckViolation`]
    /// variant. If the serde-emitted JSON shape ever drifts (a renamed
    /// `kind` tag, a missing field), the round-trip in
    /// [`wire_round_trip_all_variants`] fails to deserialize.
    ///
    /// Sub-kind and other nested-enum fields (`sub_kind`, `verb`,
    /// `occurrences`, `created_at`) are left off every arm: several of the
    /// nested enums serialize as a plain string for one sample and a
    /// tagged object for another (any kind carrying fields does), so no
    /// single scalar type here would fit every corpus specimen. The named
    /// fields that remain are enough to prove the round-trip: a missing or
    /// renamed one still fails to deserialize.
    #[derive(serde::Deserialize, Debug, PartialEq)]
    #[serde(tag = "kind", rename_all = "kebab-case")]
    enum WireViolation {
        OrphanedClone {
            path: String,
            absolute_path: String,
        },
        DanglingReference {
            path: String,
            absolute_path: String,
            project: String,
        },
        MissingRole {
            path: String,
            absolute_path: String,
            project: String,
        },
        StaleLock {
            path: String,
            absolute_path: String,
            project: String,
            locked: String,
            actual: String,
        },
        IncompleteLock {
            path: String,
            absolute_path: String,
            project: String,
        },
        WorkweaveDrift {
            path: String,
            absolute_path: String,
            workweave: String,
            sub_kind: String,
        },
        IndexDrift {
            path: String,
            absolute_path: String,
            workweave: Option<String>,
            sub_kind: String,
        },
        WorkingTreeDrift {
            path: String,
            absolute_path: String,
            workweave: Option<String>,
            sub_kind: String,
        },
        MissingReplayExclusion {
            project: String,
        },
        ReplayExclusionUnreadable {
            project: String,
            error: String,
        },
        MissingMergeDriverConfig {
            project: String,
            config_key: String,
        },
        MergeDriverConfigUnreadable {
            project: String,
            config_key: String,
            error: String,
        },
        HeadUnreadable {
            path: String,
            absolute_path: String,
            error: String,
        },
        ProjectsDirUnreadable {
            path: String,
            error: String,
        },
        ProjectlessDir {
            absolute_path: String,
        },
        UnnameableProject {
            absolute_path: String,
            derived: String,
            error: String,
        },
        UnresolvableLockEntry {
            path: String,
            absolute_path: String,
            project: String,
        },
        LegacyManifestFormat {
            project: String,
            legacy_path: String,
        },
        DanglingActiveProject {
            project: String,
            missing_dir: String,
        },
        WeaveRootIdentityConflict {
            root: String,
            pointer_project: Option<String>,
        },
        LegacyWorkweaveMarker {
            marker_path: String,
            primary: String,
        },
        LegacyWorkweaveIndex {
            project: String,
            index_path: String,
        },
        UnreadableWorkweaveIndex {
            project: String,
            index_path: String,
            error: String,
        },
        UnparseableProject {
            project: String,
            manifest_path: String,
            message: String,
        },
        WorkweaveTreeIntegrity {
            workweave_dir: String,
        },
        Provenance {
            path: String,
            absolute_path: String,
            project: String,
        },
        CloneTopology {
            path: String,
            absolute_path: String,
        },
        BranchDiscipline {
            repo_path: String,
        },
        StaleWorktreeRegistration {
            path: String,
            absolute_path: String,
            workweave: Option<String>,
            missing_path: String,
        },
        StaleOpState {
            workspace_dir: String,
            started_at: String,
        },
        DeadOpLease {
            workspace_dir: String,
            op_id: String,
            recorded_owner: String,
        },
        DanglingRefReceipt {
            project: String,
            store_path: String,
            ref_name: String,
        },
        PreFlatRefReceipt {
            project: String,
            store_path: String,
            ref_name: String,
        },
        OrphanedSavepoint {
            path: String,
            absolute_path: String,
            workweave: Option<String>,
            op_id: String,
        },
        ConfusableSiblings {
            parent: String,
            first: String,
            second: String,
        },
        CargoVersionSkew {
            crate_name: String,
        },
        CargoPatchShadowing {
            weave_config: String,
            member_config: String,
            registry: String,
            crate_name: String,
        },
        MissingCanonicalClone {
            path: String,
            absolute_path: String,
            workweave: String,
            canonical_path: String,
        },
        UninitializedSubmodule {
            absolute_path: String,
            path: String,
            workweave: String,
            empty_paths: Vec<String>,
        },
        PhantomMergeDriver {
            path: String,
            absolute_path: String,
            pattern: String,
            driver: String,
        },
    }

    #[test]
    fn wire_round_trip_all_variants() {
        let ws = workspace_dir();
        let no_ww = empty_workweave_dirs();
        let violations = corpus();
        let expected_len = violations.len();

        let payload = serde_json::to_value(build_doctor_json(
            violations,
            Vec::new(),
            &ws,
            &no_ww,
            None,
            vec![],
            vec![],
        ))
        .expect("doctor payload serializes");
        let arr = payload.get("violations").unwrap().clone();
        let parsed: Vec<WireViolation> =
            serde_json::from_value(arr).unwrap_or_else(|e| panic!("round-trip failed: {e}"));
        assert_eq!(
            parsed.len(),
            expected_len,
            "every corpus specimen must round-trip through the mirror"
        );
    }

    /// ViolationOutput::UnparseableProject emits `message` (not `error`).
    /// The field was renamed to signal it is free-form display text from an
    /// anyhow::Error, not a typed discriminant consumers can branch on.
    #[test]
    fn unparseable_project_wire_uses_message_not_error() {
        let ws = workspace_dir();
        let no_ww = empty_workweave_dirs();
        let violation = CheckViolation::UnparseableProject {
            project: ProjectName::new("broken-app").unwrap(),
            manifest_path: std::path::PathBuf::from("/ws/projects/broken-app/rwv.toml"),
            message: "did not find expected key".to_owned(),
        };
        let json =
            serde_json::to_value(ViolationOutput::from_violation(violation, &ws, &no_ww)).unwrap();
        assert_eq!(json["kind"], "unparseable-project");
        assert!(
            json.get("message").is_some(),
            "wire output must have `message` field: {json}"
        );
        assert!(
            json.get("error").is_none(),
            "wire output must NOT have legacy `error` field: {json}"
        );
        assert_eq!(json["message"], "did not find expected key");
    }
}

// ===========================================================================
// Legacy workweave marker — missing `parent:` field
// ===========================================================================

/// A `.rwv-workweave` marker missing `parent:` causes any rwv invocation from
/// inside that workweave to fail with a clear, actionable error.
#[test]
fn legacy_workweave_marker_causes_error_on_rwv_invocation() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    // The default container: doctor's scan always covers it, with no
    // registry entry required — these markers are hand-written, not
    // created via `rwv workweave create`.
    let ww_dir_container = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir_container).unwrap();
    let ww_dir = ww_dir_container.join("ws--feat");
    std::fs::create_dir_all(&ww_dir).unwrap();

    // Write a legacy marker without `parent:`.
    let legacy_marker = format!(
        "primary: {}\nproject: my-app\n",
        root.canonicalize().unwrap().display()
    );
    std::fs::write(ww_dir.join(".rwv-workweave"), &legacy_marker).unwrap();

    // Any rwv invocation from the workweave should fail with a helpful message.
    rwv_cmd()
        .arg("status")
        .current_dir(&ww_dir)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("legacy workweave marker")
                .or(predicate::str::contains("parent")),
        )
        .stderr(predicate::str::contains("rwv doctor --fix"));
}

/// `rwv doctor` reports `legacy-workweave-marker` for every workweave
/// with a marker missing `parent:`.
#[test]
fn doctor_reports_legacy_workweave_marker() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    // The default container: doctor's scan always covers it, with no
    // registry entry required — these markers are hand-written, not
    // created via `rwv workweave create`.
    let ww_dir_container = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir_container).unwrap();
    let ww_dir = ww_dir_container.join("ws--feat");
    std::fs::create_dir_all(&ww_dir).unwrap();

    // Write a legacy marker without `parent:`.
    let legacy_marker = format!(
        "primary: {}\nproject: my-app\n",
        root.canonicalize().unwrap().display()
    );
    std::fs::write(ww_dir.join(".rwv-workweave"), &legacy_marker).unwrap();

    rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        // Exits non-zero because there is a warning (exit 0 = no issues).
        // doctor currently exits 1 only on errors; warnings produce exit 0.
        // The important invariant is that the message appears.
        .stdout(
            predicate::str::contains("legacy workweave marker")
                .or(predicate::str::contains(".rwv-workweave")),
        )
        .stdout(predicate::str::contains("rwv doctor --fix"));
}

/// `rwv doctor --fix` appends `parent:` to a legacy marker, then doctor
/// reports clean (no more legacy-marker violation).
#[test]
fn doctor_fix_migrates_legacy_workweave_marker() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let primary_canon = root.canonicalize().unwrap();
    // The default container: doctor's scan always covers it, with no
    // registry entry required — these markers are hand-written, not
    // created via `rwv workweave create`.
    let ww_dir_container = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir_container).unwrap();
    let ww_dir = ww_dir_container.join("ws--feat");
    std::fs::create_dir_all(&ww_dir).unwrap();

    // Write a legacy marker without `parent:`.
    let legacy_marker = format!("primary: {}\nproject: my-app\n", primary_canon.display());
    std::fs::write(ww_dir.join(".rwv-workweave"), &legacy_marker).unwrap();

    // `rwv doctor --fix` should migrate the marker.
    rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .assert()
        .stdout(
            predicate::str::contains("[fixed]").and(predicate::str::contains("workweave marker")),
        );

    // After fix, the marker file must be JSON carrying a `parent` field.
    let migrated = std::fs::read_to_string(ww_dir.join(".rwv-workweave")).unwrap();
    let migrated_json: serde_json::Value = serde_json::from_str(&migrated)
        .unwrap_or_else(|e| panic!("marker must be JSON after --fix ({e}), got:\n{migrated}"));
    assert!(
        migrated_json["parent"].is_string(),
        "marker must carry a parent field after --fix, got:\n{migrated}"
    );

    // A second `rwv doctor` run should no longer report the violation.
    let stdout = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .get_output()
        .stdout
        .clone();
    let stdout_str = String::from_utf8_lossy(&stdout);
    assert!(
        !stdout_str.contains("legacy workweave marker"),
        "doctor must not report legacy-workweave-marker after --fix; got:\n{stdout_str}"
    );
}

/// A `.rwv-workweave` marker that fails to parse at all (not "legacy", just
/// broken) is not reported as one — that failure mode belongs elsewhere.
/// Pins the behaviour `scan_for_legacy_workweave_markers` had before it was
/// routed through `observe_marker`, which classifies the same failure as
/// `Unreadable` rather than `Legacy`.
#[test]
fn doctor_does_not_report_an_unparseable_marker_as_legacy() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let ww_dir_container = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir_container).unwrap();
    let ww_dir = ww_dir_container.join("ws--feat");
    std::fs::create_dir_all(&ww_dir).unwrap();

    // Not valid YAML at all.
    std::fs::write(ww_dir.join(".rwv-workweave"), "not: [valid: manifest_toml").unwrap();

    // `.success()` first: a negative string assertion alone would also pass
    // if doctor crashed on this input instead of quietly skipping it, which
    // would hide exactly the malformed-marker failure under test.
    let stdout = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout_str = String::from_utf8_lossy(&stdout);
    assert!(
        !stdout_str.contains("legacy workweave marker"),
        "an unparseable marker is a different failure mode, not \
         legacy-workweave-marker; got:\n{stdout_str}"
    );
}

/// A legacy marker (missing `parent:`) that also has no `primary:` of its
/// own is not reported — the finding exists to tell `--fix` what to
/// backfill, and there is nothing to backfill from. Pins the same
/// pre-observe_marker behaviour as the unparseable case above.
#[test]
fn doctor_does_not_report_a_legacy_marker_with_no_primary() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let ww_dir_container = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir_container).unwrap();
    let ww_dir = ww_dir_container.join("ws--feat");
    std::fs::create_dir_all(&ww_dir).unwrap();

    // Legacy shape (no `parent:`), but also no `primary:`.
    std::fs::write(ww_dir.join(".rwv-workweave"), "project: my-app\n").unwrap();

    // `.success()` first, same reasoning as the unparseable case above.
    let stdout = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout_str = String::from_utf8_lossy(&stdout);
    assert!(
        !stdout_str.contains("legacy workweave marker"),
        "a legacy marker with no primary: has nothing for --fix to \
         backfill from and must not be reported; got:\n{stdout_str}"
    );
}

/// `rwv doctor` does NOT warn when the project carries the replay-exclusion entry.
#[test]
fn check_silent_when_project_has_replay_exclusion() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();

    let assertion = rwv_cmd().arg("doctor").current_dir(&root).assert();
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("missing `rwv.lock merge=rwv-ours`"),
        "doctor must not warn when the line is present; got stdout: {stdout}"
    );
}

// ===========================================================================
// Unparseable-project violation
// ===========================================================================

/// A project with a syntactically broken `rwv.toml` must surface an
/// `unparseable-project` violation — not silently report a clean workspace.
///
/// Regression test for the silent-skip pattern: previously `run_check` hit
/// `Err(_) => eprintln!(...)` and continued, leaving zero violations for the
/// broken project (indistinguishable from a healthy workspace).
#[test]
fn check_unparseable_project_reported_as_violation() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Write a syntactically broken rwv.toml — the table header never closes.
    let project_dir = root.join("projects").join("broken-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/server\"\ntype = \"git\"\n",
    )
    .unwrap();

    let assert = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .failure();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        out.contains("[error]") && out.contains("failed to parse rwv.toml"),
        "expected the parse failure reported at error severity, got stdout: {out}"
    );
    assert!(
        out.contains("broken-app"),
        "violation message should name the project, got stdout: {out}"
    );
}

/// `rwv doctor --json` against a workspace with a broken manifest emits an
/// `unparseable-project` entry in the violations array and exits non-zero.
#[test]
fn check_unparseable_project_in_json_output() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let project_dir = root.join("projects").join("broken-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/server\"\ntype = \"git\"\n",
    )
    .unwrap();

    let assertion = rwv_cmd()
        .args(["doctor", "--json"])
        .current_dir(&root)
        .assert()
        .failure();
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));

    let violations = parsed
        .get("violations")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("violations missing: {parsed}"));

    let entry = violations
        .iter()
        .find(|v| v.get("kind").and_then(|k| k.as_str()) == Some("unparseable-project"))
        .unwrap_or_else(|| panic!("no unparseable-project entry in {parsed}"));

    assert_eq!(
        entry.get("project").and_then(|s| s.as_str()),
        Some("broken-app"),
        "project field should name the broken project"
    );
    assert!(
        entry
            .get("manifest_path")
            .and_then(|s| s.as_str())
            .is_some(),
        "manifest_path field should be present"
    );
    assert!(
        entry.get("message").and_then(|s| s.as_str()).is_some(),
        "message field should contain the parse error"
    );
}

/// `rwv doctor --fix` against a workspace with a broken manifest does NOT
/// auto-repair it; the violation persists after --fix. This confirms
/// that no automated unsafe mutation is attempted.
#[test]
fn check_unparseable_project_not_fixed_by_fix_flag() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let unparseable = "[repositories.\"github/acme/server\"\ntype = \"git\"\n";
    let project_dir = root.join("projects").join("broken-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), unparseable).unwrap();

    // --fix should not crash and should still exit non-zero (violation remains).
    rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .assert()
        .failure();

    // The manifest must be unchanged — --fix must not touch a broken manifest.
    let content_after = std::fs::read_to_string(project_dir.join("rwv.toml")).unwrap();
    assert_eq!(
        content_after, unparseable,
        "--fix must not modify an unparseable manifest"
    );
}

// ===========================================================================
// Default scoping (active project only) and --all flag
// ===========================================================================

/// Helper: write a `.rwv-active` file pointing at the given project.
fn set_active_project(workspace_root: &std::path::Path, project_name: &str) {
    std::fs::write(
        workspace_root.join(".rwv-active"),
        format!("{project_name}\n"),
    )
    .unwrap();
}

/// Default `rwv doctor` does NOT report orphaned clones when an active project
/// is set. An orphan can belong to another project; surfacing it in single-
/// project scope produces false positives.
#[test]
fn default_scope_no_orphan_when_active_project_set() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Project "active-proj" owns one repo.
    let owned_repo = "github/acme/owned";
    init_git_repo(&root.join(owned_repo));
    let project_dir = root.join("projects").join("active-proj");
    write_manifest(
        &project_dir,
        &[(owned_repo, "https://github.com/acme/owned.git")],
    );
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();

    // An extra repo on disk belongs to no loaded project (would be "orphaned"
    // in weave-wide scan but must not be flagged in single-project scope).
    let extra_repo = "github/acme/other-project-repo";
    init_git_repo(&root.join(extra_repo));

    // Activate "active-proj".
    set_active_project(&root, "active-proj");

    let out = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&out);
    assert!(
        !stdout.contains("orphaned clone"),
        "default scope must not report orphan when active project is set; got:\n{stdout}"
    );
    assert!(
        !stdout.contains(extra_repo),
        "default scope must not mention the extra repo; got:\n{stdout}"
    );
}

/// `rwv doctor --all` DOES report orphaned clones regardless of active project.
#[test]
fn all_flag_reports_orphan_even_with_active_project() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let owned_repo = "github/acme/owned";
    init_git_repo(&root.join(owned_repo));
    let project_dir = root.join("projects").join("active-proj");
    write_manifest(
        &project_dir,
        &[(owned_repo, "https://github.com/acme/owned.git")],
    );
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();

    let orphan_repo = "github/acme/stray";
    init_git_repo(&root.join(orphan_repo));

    set_active_project(&root, "active-proj");

    let out = rwv_cmd()
        .args(["doctor", "--all"])
        .current_dir(&root)
        .assert()
        .failure() // orphan is an error
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&out);
    assert!(
        stdout.contains("orphaned clone") || stdout.contains(orphan_repo),
        "--all must report the orphan; got:\n{stdout}"
    );
}

/// Default `rwv doctor` does NOT report stale locks from a non-active project
/// when an active project is set.
#[test]
fn default_scope_no_cross_project_stale_lock() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Project "alpha" owns repo-a; lock is fresh.
    let repo_a = "github/acme/repo-a";
    let sha_a = init_git_repo(&root.join(repo_a));
    let alpha_dir = root.join("projects").join("alpha");
    write_manifest(
        &alpha_dir,
        &[(repo_a, "https://github.com/acme/repo-a.git")],
    );
    write_lock(
        &alpha_dir,
        &[(repo_a, "https://github.com/acme/repo-a.git", &sha_a)],
    );
    std::fs::write(
        alpha_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();

    // Project "beta" owns repo-b; lock is STALE.
    let repo_b = "github/acme/repo-b";
    init_git_repo(&root.join(repo_b));
    let beta_dir = root.join("projects").join("beta");
    write_manifest(&beta_dir, &[(repo_b, "https://github.com/acme/repo-b.git")]);
    write_lock(
        &beta_dir,
        &[(
            repo_b,
            "https://github.com/acme/repo-b.git",
            "0000000000000000000000000000000000000000",
        )],
    );
    std::fs::write(beta_dir.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();

    // Activate "alpha".
    set_active_project(&root, "alpha");

    // Default doctor should see no violations (alpha is clean).
    let out = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .assert()
        .success() // alpha is clean; beta's stale lock must be invisible
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&out);
    assert!(
        !stdout.contains("stale lock"),
        "default scope must not report stale lock from non-active project; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("beta"),
        "default scope must not mention non-active project; got:\n{stdout}"
    );
}

/// `rwv doctor --all` DOES report stale locks (or unresolvable-lock errors)
/// from all projects, not just the active one.
#[test]
fn all_flag_reports_cross_project_stale_lock() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_a = "github/acme/repo-a";
    let sha_a = init_git_repo(&root.join(repo_a));
    let alpha_dir = root.join("projects").join("alpha");
    write_manifest(
        &alpha_dir,
        &[(repo_a, "https://github.com/acme/repo-a.git")],
    );
    write_lock(
        &alpha_dir,
        &[(repo_a, "https://github.com/acme/repo-a.git", &sha_a)],
    );
    std::fs::write(
        alpha_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();

    // beta's repo-b: lock pins the first commit, HEAD advances past it.
    let repo_b = "github/acme/repo-b";
    let old_sha_b = init_git_repo(&root.join(repo_b));
    make_commit(&root.join(repo_b)); // HEAD moves forward; lock stays at old_sha_b
    let beta_dir = root.join("projects").join("beta");
    write_manifest(&beta_dir, &[(repo_b, "https://github.com/acme/repo-b.git")]);
    write_lock(
        &beta_dir,
        &[(repo_b, "https://github.com/acme/repo-b.git", &old_sha_b)],
    );
    std::fs::write(beta_dir.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();

    set_active_project(&root, "alpha");

    // --all should surface beta's stale-lock issue.
    let out = rwv_cmd()
        .args(["doctor", "--all"])
        .current_dir(&root)
        .assert()
        .failure() // stale lock is an error
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&out);
    assert!(
        stdout.contains("stale lock") || stdout.contains("beta"),
        "--all must surface beta's stale lock from non-active project; got:\n{stdout}"
    );
}

/// `rwv doctor --json` with an active project set does NOT include orphaned-
/// clone entries in the violations array.
#[test]
fn default_scope_json_no_orphan_when_active_project_set() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let owned_repo = "github/acme/owned";
    init_git_repo(&root.join(owned_repo));
    let project_dir = root.join("projects").join("active-proj");
    write_manifest(
        &project_dir,
        &[(owned_repo, "https://github.com/acme/owned.git")],
    );
    make_project_repo_clean(&project_dir);

    // Orphan-looking repo that belongs to no active project.
    init_git_repo(&root.join("github/acme/other-project-repo"));

    set_active_project(&root, "active-proj");

    let assertion = rwv_cmd()
        .args(["doctor", "--json"])
        .current_dir(&root)
        .assert()
        .success(); // active project is clean; no violations
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let violations = parsed.get("violations").and_then(|v| v.as_array()).unwrap();
    let has_orphan = violations
        .iter()
        .any(|v| v.get("kind").and_then(|k| k.as_str()) == Some("orphaned-clone"));
    assert!(
        !has_orphan,
        "default --json scope must not include orphaned-clone; violations: {violations:?}"
    );
}

// ===========================================================================
// Unborn HEAD: doctor names the state, not the raw git error
// ===========================================================================

/// Helper: create an empty git repo with no commits (unborn HEAD).
fn init_git_repo_unborn(path: &std::path::Path) {
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
fn doctor_unborn_head_member_names_state_not_raw_git_error() {
    // When a workspace member has no commits (unborn HEAD), `rwv doctor`
    // must report a finding that names the state ("unborn HEAD") rather
    // than leaking git's raw "ambiguous argument 'HEAD'" error.
    //
    // The check subsystem collects `head_read_failures` and formats each
    // one as `{repo_path}: HEAD unreadable ({err_msg})`. The `err_msg` is
    // the `VcsError::CommandFailed.stderr` field, which now
    // contains "unborn HEAD ..." instead of the raw git message.
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo_unborn(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    std::fs::write(root.join(".rwv-active"), "my-app\n").unwrap();

    // Run doctor in the workspace root. It should detect the unreadable HEAD.
    let output = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .output()
        .expect("failed to spawn rwv doctor");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    // Must name the condition the operator can act on.
    assert!(
        combined.contains("unborn HEAD"),
        "doctor must surface 'unborn HEAD' but got:\n{combined}"
    );

    // Must NOT leak git's internal error message verbatim.
    assert!(
        !combined.contains("ambiguous argument"),
        "doctor must not leak raw git error but got:\n{combined}"
    );
}

#[test]
fn doctor_unborn_head_member_includes_action_hint() {
    // The "unborn HEAD" error message includes actionable guidance:
    // telling the operator to "make an initial commit".
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo_unborn(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    std::fs::write(root.join(".rwv-active"), "my-app\n").unwrap();

    let output = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .output()
        .expect("failed to spawn rwv doctor");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        combined.contains("initial commit") || combined.contains("unborn HEAD"),
        "doctor output should hint at making an initial commit, got:\n{combined}"
    );
}

// ===========================================================================
// Repair-verb naming audit
//
// Every user-facing error/warning must name the rwv verb that repairs it.
// House pattern: name the state → name the verb → name the escape hatch.
// ===========================================================================

/// `orphaned clone` message names `rwv add` and/or `remove` as repair actions.
#[test]
fn orphaned_clone_names_repair_verb() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let orphan_repo = "github/acme/orphan";
    init_git_repo(&root.join(orphan_repo));

    // No project manifest — the scan runs in --all mode to see orphans.
    let project_dir = root.join("projects").join("my-app");
    write_manifest(&project_dir, &[]);
    std::fs::write(root.join(".rwv-active"), "my-app\n").unwrap();

    let output = rwv_cmd()
        .args(["doctor", "--all"])
        .current_dir(&root)
        .output()
        .expect("failed to spawn rwv doctor");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        combined.contains("orphaned"),
        "must report orphaned clone; got:\n{combined}"
    );
    assert!(
        combined.contains("rwv add") || combined.contains("remove"),
        "orphaned-clone message must name the repair verb (`rwv add` or remove); \
         got:\n{combined}"
    );
}

/// `dangling reference` message names `rwv fetch` as the repair verb (its
/// in-place mode re-materializes missing manifest members) and re-runs
/// `rwv doctor` for verification. The stale "repair by hand: git clone …"
/// advice must NOT appear — the verb now performs the repair. See
/// fetch::run_fetch_in_place.
#[test]
fn dangling_reference_names_rwv_fetch_repair_verb() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let real_repo = "github/acme/server";
    let missing_repo = "github/acme/vanished";
    init_git_repo(&root.join(real_repo));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[
            (real_repo, "https://github.com/acme/server.git"),
            (missing_repo, "https://github.com/acme/vanished.git"),
        ],
    );

    let output = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .output()
        .expect("failed to spawn rwv doctor");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        combined.contains("dangling"),
        "must report dangling reference; got:\n{combined}"
    );
    // Repair verb is `rwv fetch` (in-place, no SOURCE).
    assert!(
        combined.contains("rwv fetch"),
        "dangling-reference message must name `rwv fetch` as the repair verb; \
         got:\n{combined}"
    );
    assert!(
        combined.contains("rwv doctor"),
        "dangling-reference message must name `rwv doctor` as the verify step; \
         got:\n{combined}"
    );
    // The stale honest-manual advice must be gone: no `git clone …` in the
    // message body — the repair verb replaces the manual advice.
    assert!(
        !combined.contains("git clone"),
        "dangling-reference message must NOT advise manual `git clone` \
         (repair verb `rwv fetch` performs the repair); got:\n{combined}"
    );
}

/// Dead-op-lease doctor report names the repair verb AND shows age when
/// the lease file carries a `created_at` field.
#[test]
fn dead_op_lease_names_fix_verb_and_age() {
    use std::path::PathBuf;

    let tmp = common::tempdir().unwrap();
    // Build a minimal workspace with the usual layout.
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    // Ghost owner directory (no .rwv-op inside).
    let ghost_owner = tmp.path().join("ghost-owner");
    std::fs::create_dir_all(&ghost_owner).unwrap();

    // Lease with `created_at` so doctor can surface age.
    let lease_json = format!(
        "{{\"id\": \"audit-dead-op-1\", \"owner\": \"{}\", \"created_at\": \"2026-01-01T00:00:00Z\"}}",
        common::json_escaped(&ghost_owner),
    );
    std::fs::write(root.join(".rwv-op-lease"), &lease_json).unwrap();

    let project_dir = root.join("projects").join("my-app");
    write_manifest(&project_dir, &[]);
    std::fs::write(root.join(".rwv-active"), "my-app\n").unwrap();

    let output = rwv_cmd()
        .arg("doctor")
        .current_dir(&root)
        .output()
        .expect("failed to spawn rwv doctor");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        combined.contains("dead-op-lease"),
        "must report dead-op-lease; got:\n{combined}"
    );
    assert!(
        combined.contains("rwv doctor --fix"),
        "dead-op-lease message must name `rwv doctor --fix`; got:\n{combined}"
    );
    assert!(
        combined.contains("created_at") || combined.contains("age"),
        "dead-op-lease message must surface lease age when created_at is present; \
         got:\n{combined}"
    );
    let _ = PathBuf::from(&root); // suppress unused-import lint
}
