//! The health floor (P1) and its recording conditions.
//!
//! Executed pins:
//!   1. A CLEAN weave-wide `rwv doctor --all` run records the floor: the
//!      running version and each project repo's tip.
//!   2. A run with findings does not — exit 0 alone (warnings only) is not
//!      clean.
//!   3. A clean but project-SCOPED run does not — the floor licenses arm
//!      removal for the whole weave, and a scoped run proves nothing
//!      weave-wide.
//!   4. The floor only advances: a recorded floor newer than the running
//!      binary is left alone.
//!
//! The P2 step-through refusal is pinned by the module's own tests
//! (`health_floor::tests`), driven through `enforce_with` — no requirement
//! ships while every migratory arm is still present, so the rule is
//! exercised at the seam a future removal will flip.

use std::path::{Path, PathBuf};

mod common;

fn make_workspace(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    root
}

fn git_in(dir: &Path, args: &[&str]) -> String {
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
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_git_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git_in(path, &["init", "--initial-branch=main", "-q"]);
    git_in(path, &["config", "user.email", "test@test.com"]);
    git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git_in(path, &["add", "README.md"]);
    git_in(path, &["commit", "-q", "-m", "init"]);
    git_in(path, &["rev-parse", "HEAD"])
}

/// A weave that `rwv doctor --fix --all` can bring to zero violations: one
/// manifest repo, and a project directory that is itself a git repo so the
/// replay-exclusion and merge-driver arms can plant their config.
fn healable_workspace(parent: &Path) -> PathBuf {
    let root = make_workspace(parent, "ws");
    let repo_abs = root.join("github").join("acme").join("server");
    init_git_repo(&repo_abs);

    let project_dir = root.join("projects").join("my-app");
    init_git_repo(&project_dir);
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/server\"]\ntype = \"git\"\n\
         url = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    root
}

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn floor_path(root: &Path) -> PathBuf {
    root.join(".rwv-health-floor")
}

fn read_floor(root: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(floor_path(root)).expect("floor file exists");
    serde_json::from_str(&raw).expect("floor file is JSON")
}

/// Bring the weave to clean and remove any floor the repairs recorded, so
/// each test asserts exactly one run's behavior.
fn heal_and_clear(root: &Path) {
    let fix = rwv()
        .args(["doctor", "--fix", "--all"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(fix.status.success(), "--fix --all must succeed");
    let _ = std::fs::remove_file(floor_path(root));
}

/// Pin 1: a clean weave-wide run records {version, project tips}.
///
/// **Mutation evidence**: dropping `violations_clean` from the record
/// condition in `run_check` makes pin 2 red; dropping `scope_all` makes
/// pin 3 red — this pin is the positive control both reverts are measured
/// against.
#[test]
fn a_clean_weave_wide_run_records_the_floor() {
    let tmp = common::tempdir().unwrap();
    let root = healable_workspace(tmp.path());
    heal_and_clear(&root);

    let out = rwv()
        .args(["doctor", "--all"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "clean run exits 0; got:\n{stdout}");

    let floor = read_floor(&root);
    let version = floor["version"].as_str().expect("floor records a version");
    assert!(
        !version.is_empty(),
        "the floor records the running version; got: {floor}"
    );
    let tip = floor["project_tips"]["my-app"]
        .as_str()
        .expect("the floor records the project repo tip");
    let head = git_in(
        &root.join("projects").join("my-app"),
        &["rev-parse", "HEAD"],
    );
    assert_eq!(tip, head, "the recorded tip is the project repo's HEAD");
}

/// Pin 2: findings block the floor even when the exit code is 0 — a
/// warning-class violation (a redundant orphaned savepoint) is a finding.
#[test]
fn a_run_with_findings_does_not_record_the_floor() {
    let tmp = common::tempdir().unwrap();
    let root = healable_workspace(tmp.path());
    heal_and_clear(&root);

    // Plant a violation: an orphaned savepoint ref in the manifest repo.
    let repo_abs = root.join("github").join("acme").join("server");
    let head = git_in(&repo_abs, &["rev-parse", "HEAD"]);
    git_in(
        &repo_abs,
        &["update-ref", "refs/rwv/pre-op/611111111111111111", &head],
    );

    let out = rwv()
        .args(["doctor", "--all"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "a warning-only run still exits 0 — which is exactly why exit code \
         alone must not advance the floor"
    );
    assert!(
        !floor_path(&root).exists(),
        "a run with findings must not record the floor; report was:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Pin 3: a clean but project-scoped run does not record the floor.
#[test]
fn a_scoped_run_does_not_record_the_floor() {
    let tmp = common::tempdir().unwrap();
    let root = healable_workspace(tmp.path());
    heal_and_clear(&root);

    let out = rwv().args(["doctor"]).current_dir(&root).output().unwrap();
    assert!(out.status.success());
    assert!(
        !floor_path(&root).exists(),
        "a scoped run proves nothing weave-wide and must not record; \
         report was:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// Pin 4: the floor only advances — a stray downgrade cannot lower what a
/// newer version attested.
///
/// **Mutation evidence**: dropping the advance-only comparison in
/// `record_clean_run` reddens this (the hand-written 99.0.0 floor is
/// overwritten with the running version).
#[test]
fn the_floor_never_moves_backward() {
    let tmp = common::tempdir().unwrap();
    let root = healable_workspace(tmp.path());
    heal_and_clear(&root);

    std::fs::write(
        floor_path(&root),
        "{\"version\":\"99.0.0\",\"project_tips\":{}}",
    )
    .unwrap();

    let out = rwv()
        .args(["doctor", "--all"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(out.status.success());
    let floor = read_floor(&root);
    assert_eq!(
        floor["version"].as_str(),
        Some("99.0.0"),
        "a clean run under an older binary must not lower the floor"
    );
}
