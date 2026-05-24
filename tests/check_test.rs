//! E2E tests for `rwv check` — convention enforcement.
//!
//! These tests exercise the CLI binary via `assert_cmd`. Tests that depend on
//! the full check implementation (bead 8b) are marked `#[ignore]`.

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

/// Write an `rwv.yaml` manifest into a project directory.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    std::fs::create_dir_all(project_dir).unwrap();
    let mut yaml = String::from("repositories:\n");
    for (repo_path, url) in repos {
        yaml.push_str(&format!(
            "  {repo_path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();
}

/// Write an `rwv.lock` file into a project directory with given repo SHAs.
fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (repo_path, url, sha) in repos {
        yaml.push_str(&format!(
            "  {repo_path}:\n    type: git\n    url: {url}\n    version: {sha}\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), &yaml).unwrap();
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
// 1. `rwv check` with no issues — clean workspace, exits 0
// ===========================================================================

#[test]

fn check_clean_workspace_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
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

    rwv_cmd().arg("check").current_dir(&root).assert().success();
}

// ===========================================================================
// 2. Orphaned clone — directory under `github/` not in any project's rwv.yaml
// ===========================================================================

#[test]

fn check_orphaned_clone_reported() {
    let tmp = tempfile::tempdir().unwrap();
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
        .arg("check")
        .current_dir(&root)
        .assert()
        .failure()
        .stdout(predicate::str::contains("orphan").or(predicate::str::contains("stray-clone")));
}

// ===========================================================================
// 3. Dangling reference — rwv.yaml entry pointing to a path not on disk
// ===========================================================================

#[test]

fn check_dangling_reference_reported() {
    let tmp = tempfile::tempdir().unwrap();
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
        .arg("check")
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
    let tmp = tempfile::tempdir().unwrap();
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
        .arg("check")
        .current_dir(&root)
        .assert()
        .failure()
        .stdout(predicate::str::contains("stale").or(predicate::str::contains("lock")));
}

// ===========================================================================
// 5. Multi-project awareness — repo in project A is not orphan even if not
//    in project B
// ===========================================================================

#[test]

fn check_multi_project_no_false_orphan() {
    let tmp = tempfile::tempdir().unwrap();
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
    rwv_cmd().arg("check").current_dir(&root).assert().success();
}

// ===========================================================================
// 6. `rwv check` outside a workspace — should error
// ===========================================================================

#[test]

fn check_outside_workspace_errors() {
    let tmp = tempfile::tempdir().unwrap();
    // No workspace markers here — just an empty temp dir

    rwv_cmd()
        .arg("check")
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
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create a repo on disk
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    // Create a project with the repo and an integration config
    let project_dir = root.join("projects").join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let yaml = format!(
        r#"repositories:
  {repo_path}:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: owned
integrations:
  cargo:
    enabled: true
"#
    );
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();

    // Even with integration hooks, a clean workspace should not error.
    // Any integration warnings should be printed but not cause failure
    // (only errors cause non-zero exit).
    rwv_cmd().arg("check").current_dir(&root).assert().success();
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
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();
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
// B3: `rwv check` surfaces lock entries referencing unknown revisions.
// ===========================================================================

/// If the lock pins a SHA the local clone has never seen, `resolve_versions`
/// can't resolve it. Previously this signal was silently dropped (the lock
/// entry stayed raw, `find_violations` either ignored it or emitted a false
/// StaleLock). Now: an `Error`-severity issue saying "lock references unknown
/// revision". Doctor is the diagnostic of last resort — this is the place
/// that most needs to know when resolution failed.
#[test]
fn check_flags_unresolvable_lock_revision() {
    let tmp = tempfile::tempdir().unwrap();
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

    let assert = rwv_cmd().arg("check").current_dir(&root).assert().failure();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        out.contains("lock references unknown revision"),
        "expected B3 unresolved-lock diagnostic, got stdout: {out}"
    );
}

// ===========================================================================
// B4: `rwv check` surfaces on-disk repos whose HEAD can't be read.
// ===========================================================================

/// A repo that's on disk but whose HEAD is unreadable (e.g. corrupted .git
/// dir) previously produced zero violations — doctor reported clean. Now: an
/// `Error`-severity issue. Simulated by removing `.git/HEAD` after init.
#[test]
fn check_flags_unreadable_head() {
    let tmp = tempfile::tempdir().unwrap();
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

    let assert = rwv_cmd().arg("check").current_dir(&root).assert().failure();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    assert!(
        out.contains("HEAD unreadable"),
        "expected B4 unreadable-HEAD diagnostic, got stdout: {out}"
    );
}

// ===========================================================================
// Smoke test: `rwv check` CLI command is recognized
// ===========================================================================

#[test]
fn check_command_is_recognized() {
    // The command should parse successfully (not fail with "unrecognized subcommand").
    rwv_cmd()
        .arg("check")
        .assert()
        .stdout(predicate::str::contains("unrecognized").not());
}

// ===========================================================================
// fo-w9ph9: replay-exclusion (.gitattributes `rwv.lock merge=ours`)
// ===========================================================================

/// `rwv doctor` warns when a project repo is missing the
/// `rwv.lock merge=ours` line in `.gitattributes`.
#[test]
fn check_warns_when_project_missing_replay_exclusion() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    // Project dir exists with a manifest but no .gitattributes.
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );

    rwv_cmd().arg("check").current_dir(&root).assert().stdout(
        predicate::str::contains("rwv.lock merge=ours")
            .and(predicate::str::contains("my-app"))
            .and(predicate::str::contains("rwv doctor --fix")),
    );
}

/// `rwv doctor --fix` writes the missing `rwv.lock merge=ours` line.
#[test]
fn check_fix_writes_replay_exclusion() {
    let tmp = tempfile::tempdir().unwrap();
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
        .args(["check", "--fix"])
        .current_dir(&root)
        .assert()
        .stdout(predicate::str::contains("rwv.lock merge=ours"));

    // Post-condition: .gitattributes now contains the line, and re-running
    // `rwv check` no longer warns about this project.
    let attrs = std::fs::read_to_string(project_dir.join(".gitattributes")).unwrap();
    assert!(
        attrs.contains("rwv.lock merge=ours"),
        "post-fix .gitattributes should contain the line; got: {attrs:?}"
    );

    let assertion = rwv_cmd().arg("check").current_dir(&root).assert();
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("missing `rwv.lock merge=ours`"),
        "post-fix check must not re-warn; got stdout: {stdout}"
    );
}

// ===========================================================================
// fo-tn9uk.3: `rwv doctor --json`
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
// Acceptance from the bead body:
//   - top-level envelope `{ "$schema": ..., "violations": [...] }`
//   - each violation has a kebab-case `kind` and (where applicable)
//     `path` + `absolute_path`
//   - round-trip via Deserialize confirms shape stability

mod doctor_json {
    use super::*;
    use repoweave::check::{
        build_doctor_json, CheckViolation, DriftKind, IndexDriftKind, ViolationOutput,
        WorkingTreeDriftKind, DOCTOR_SCHEMA_URL,
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
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let repo_path = "github/acme/server";
        init_git_repo(&root.join(repo_path));

        let project_dir = root.join("projects").join("my-app");
        write_manifest(
            &project_dir,
            &[(repo_path, "https://github.com/acme/server.git")],
        );
        // Pre-create the replay-exclusion line so the workspace is clean.
        std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();

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
        let tmp = tempfile::tempdir().unwrap();
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
        std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();

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
        assert!(
            abs.ends_with(orphan),
            "absolute_path={abs} should end with {orphan}"
        );
        assert!(
            std::path::Path::new(abs).is_absolute(),
            "absolute_path must be absolute"
        );
    }

    #[test]
    fn json_dangling_reference_variant() {
        let tmp = tempfile::tempdir().unwrap();
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
        std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();

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
        let tmp = tempfile::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");

        let repo_path = "github/acme/server";
        let sha = init_git_repo(&root.join(repo_path));

        let project_dir = root.join("projects").join("my-app");
        write_manifest(
            &project_dir,
            &[(repo_path, "https://github.com/acme/server.git")],
        );
        std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();

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
    fn json_missing_replay_exclusion_variant() {
        let tmp = tempfile::tempdir().unwrap();
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
            project: ProjectName::new("alpha"),
            repo: RepoPath::new("github/a/b"),
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
            Some("/ws/github/a/b")
        );
    }

    #[test]
    fn wire_workweave_drift_missing_sub_kind() {
        let v = CheckViolation::WorkweaveDrift {
            workweave: WorkweaveName::new("ww1"),
            kind: DriftKind::Missing,
            repo: RepoPath::new("github/a/b"),
        };
        let mut ww_dirs = empty_workweave_dirs();
        ww_dirs.insert(
            WorkweaveName::new("ww1"),
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
            Some("/ws/.workweaves/proj--ww1/github/a/b")
        );
    }

    #[test]
    fn wire_workweave_drift_extra_sub_kind() {
        let v = CheckViolation::WorkweaveDrift {
            workweave: WorkweaveName::new("ww1"),
            kind: DriftKind::Extra,
            repo: RepoPath::new("github/a/b"),
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
                repo: RepoPath::new("github/a/b"),
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
                workweave: Some(WorkweaveName::new("ww1")),
                repo: RepoPath::new("github/a/b"),
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
    fn wire_all_eight_variant_tags_stable() {
        // Construct one of each variant and confirm the `kind` tags match
        // the table in the bead body verbatim. The table is part of the
        // public contract: if a serde rename strips, capitalises, or
        // reorders these, downstream agents break silently.
        let ws = workspace_dir();
        let no_ww = empty_workweave_dirs();
        let pn = || ProjectName::new("p");
        let rp = || RepoPath::new("github/a/b");

        let cases: Vec<(CheckViolation, &str)> = vec![
            (
                CheckViolation::OrphanedClone { path: rp() },
                "orphaned-clone",
            ),
            (
                CheckViolation::DanglingReference {
                    project: pn(),
                    repo: rp(),
                },
                "dangling-reference",
            ),
            (
                CheckViolation::MissingRole {
                    project: pn(),
                    repo: rp(),
                },
                "missing-role",
            ),
            (
                CheckViolation::StaleLock {
                    project: pn(),
                    repo: rp(),
                    locked: ResolvedRevisionId::from_canonical("aaa", None),
                    actual: ResolvedRevisionId::from_canonical("bbb", None),
                },
                "stale-lock",
            ),
            (
                CheckViolation::WorkweaveDrift {
                    workweave: WorkweaveName::new("ww"),
                    kind: DriftKind::Missing,
                    repo: rp(),
                },
                "workweave-drift",
            ),
            (
                CheckViolation::IndexDrift {
                    workweave: None,
                    repo: rp(),
                    kind: IndexDriftKind::SafeToFix,
                },
                "index-drift",
            ),
            (
                CheckViolation::WorkingTreeDrift {
                    workweave: None,
                    repo: rp(),
                    kind: WorkingTreeDriftKind::SafeToFix,
                },
                "working-tree-drift",
            ),
            (
                CheckViolation::MissingReplayExclusion { project: pn() },
                "missing-replay-exclusion",
            ),
        ];

        for (violation, expected) in cases {
            let json =
                serde_json::to_value(ViolationOutput::from_violation(violation, &ws, &no_ww))
                    .unwrap();
            let tag = json
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("missing `kind`: {json}"));
            assert_eq!(tag, expected, "wire tag mismatch (full record: {json})");
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
                path: RepoPath::new("github/a/b"),
            },
            CheckViolation::StaleLock {
                project: ProjectName::new("p"),
                repo: RepoPath::new("github/c/d"),
                locked: ResolvedRevisionId::from_canonical("dead", None),
                actual: ResolvedRevisionId::from_canonical("beef", None),
            },
            CheckViolation::MissingReplayExclusion {
                project: ProjectName::new("p"),
            },
        ];

        let payload = build_doctor_json(violations, &ws, &ww_dirs);
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

    /// Test-only mirror of the wire shape. If the serde-emitted JSON
    /// shape ever drifts (renames, casing tweaks, missing fields), this
    /// round-trip will fail to deserialize and pinpoint the variant.
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
    }

    #[test]
    fn wire_round_trip_all_variants() {
        let ws = workspace_dir();
        let no_ww = empty_workweave_dirs();
        let violations = vec![
            CheckViolation::OrphanedClone {
                path: RepoPath::new("github/a/b"),
            },
            CheckViolation::DanglingReference {
                project: ProjectName::new("p"),
                repo: RepoPath::new("github/a/c"),
            },
            CheckViolation::MissingRole {
                project: ProjectName::new("p"),
                repo: RepoPath::new("github/a/d"),
            },
            CheckViolation::StaleLock {
                project: ProjectName::new("p"),
                repo: RepoPath::new("github/a/e"),
                locked: ResolvedRevisionId::from_canonical("aaa", None),
                actual: ResolvedRevisionId::from_canonical("bbb", None),
            },
            CheckViolation::WorkweaveDrift {
                workweave: WorkweaveName::new("ww1"),
                kind: DriftKind::Missing,
                repo: RepoPath::new("github/a/f"),
            },
            CheckViolation::IndexDrift {
                workweave: None,
                repo: RepoPath::new("github/a/g"),
                kind: IndexDriftKind::SafeToFix,
            },
            CheckViolation::WorkingTreeDrift {
                workweave: Some(WorkweaveName::new("ww1")),
                repo: RepoPath::new("github/a/h"),
                kind: WorkingTreeDriftKind::LiveEdits,
            },
            CheckViolation::MissingReplayExclusion {
                project: ProjectName::new("p"),
            },
        ];
        let expected_len = violations.len();

        let payload = build_doctor_json(violations, &ws, &no_ww);
        let arr = payload.get("violations").unwrap().clone();
        let parsed: Vec<WireViolation> = serde_json::from_value(arr).expect("round-trip failed");
        assert_eq!(parsed.len(), expected_len);

        // Spot-check each variant deserialised into the expected arm.
        assert!(matches!(parsed[0], WireViolation::OrphanedClone { .. }));
        assert!(matches!(parsed[1], WireViolation::DanglingReference { .. }));
        assert!(matches!(parsed[2], WireViolation::MissingRole { .. }));
        assert!(matches!(parsed[3], WireViolation::StaleLock { .. }));
        assert!(matches!(parsed[4], WireViolation::WorkweaveDrift { .. }));
        assert!(matches!(parsed[5], WireViolation::IndexDrift { .. }));
        assert!(matches!(parsed[6], WireViolation::WorkingTreeDrift { .. }));
        assert!(matches!(
            parsed[7],
            WireViolation::MissingReplayExclusion { .. }
        ));
    }
}

/// `rwv doctor` does NOT warn when the project carries the replay-exclusion entry.
#[test]
fn check_silent_when_project_has_replay_exclusion() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();

    let assertion = rwv_cmd().arg("check").current_dir(&root).assert();
    let output = assertion.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("missing `rwv.lock merge=ours`"),
        "check must not warn when the line is present; got stdout: {stdout}"
    );
}
