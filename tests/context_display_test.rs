//! E2E tests for `rwv` (no subcommand) context display and `rwv resolve`.
//!
//! These tests run the `rwv` binary via `std::process::Command` and verify
//! exit codes and output patterns. The actual implementation lands in phase 4b;
//! tests that require that implementation are marked `#[ignore]`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

mod common;

/// Create a minimal workspace root at `parent/name` with `github/` and
/// `projects/` marker directories. Returns the root path.
fn make_workspace(parent: &Path, name: &str) -> std::path::PathBuf {
    let root = parent.join(name);
    fs::create_dir_all(root.join("github")).unwrap();
    fs::create_dir_all(root.join("projects")).unwrap();
    root
}

/// Write a `.rwv-workweave` marker into `weave_dir` pointing at `primary`
/// with the given project name. Required because marker-less resolution has
/// been removed.
fn write_marker(weave_dir: &Path, primary: &Path, project: &str) {
    let primary_canon = primary.canonicalize().unwrap();
    let marker = common::workweave_marker(&primary_canon, project, &primary_canon);
    fs::write(weave_dir.join(".rwv-workweave"), marker).unwrap();
}

// ============================================================================
// 1. `rwv` (no subcommand) in a primary directory
// ============================================================================

#[test]

fn context_display_in_primary_shows_root_and_projects() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "myws");

    // Create a project so the display has something to list
    let project = root.join("projects").join("web-app");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("rwv.toml"), "[repositories]\n").unwrap();

    let out = Command::cargo_bin("rwv")
        .unwrap()
        .current_dir(&root)
        .assert()
        .success()
        .stdout(predicate::str::contains("web-app"))
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(out).expect("stdout should be valid UTF-8");
    common::assert_weave_line(&stdout, root.canonicalize().unwrap());
}

#[test]

fn context_display_in_primary_subdir() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let deep = root.join("github").join("acme").join("server");
    fs::create_dir_all(&deep).unwrap();

    let out = Command::cargo_bin("rwv")
        .unwrap()
        .current_dir(&deep)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(out).expect("stdout should be valid UTF-8");
    common::assert_weave_line(&stdout, root.canonicalize().unwrap());
}

// ============================================================================
// 2. `rwv` (no subcommand) in a workweave directory
// ============================================================================

#[test]

fn context_display_in_weave_shows_weave_info() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    // Create the workweave sibling directory with a marker.
    let weave_dir = tmp.path().join("ws--hotfix");
    fs::create_dir_all(&weave_dir).unwrap();
    write_marker(&weave_dir, &root, "ws");

    Command::cargo_bin("rwv")
        .unwrap()
        .current_dir(&weave_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("weave").or(predicate::str::contains("Weave")))
        .stdout(predicate::str::contains("hotfix"))
        .stdout(predicate::str::contains("ws"));
}

#[test]

fn context_display_in_weave_subdir() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let weave_dir = tmp.path().join("ws--feat-login");
    let repo_dir = weave_dir.join("github").join("acme").join("server");
    fs::create_dir_all(&repo_dir).unwrap();
    write_marker(&weave_dir, &root, "ws");

    Command::cargo_bin("rwv")
        .unwrap()
        .current_dir(&repo_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("feat-login"))
        .stdout(predicate::str::contains("ws"));
}

// ============================================================================
// 3. `rwv resolve` in a primary directory
// ============================================================================

#[test]

fn resolve_in_primary_prints_root_path() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let deep = root.join("github").join("acme").join("server");
    fs::create_dir_all(&deep).unwrap();

    let canonical_root = root.canonicalize().unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("resolve")
        .current_dir(&deep)
        .assert()
        .success()
        .stdout(common::operator_path_stdout(&canonical_root));
}

#[test]

fn resolve_at_workspace_root_prints_root_path() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let canonical_root = root.canonicalize().unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("resolve")
        .current_dir(&root)
        .assert()
        .success()
        .stdout(common::operator_path_stdout(&canonical_root));
}

// ============================================================================
// 4. `rwv resolve` in a workweave directory
// ============================================================================

#[test]

fn resolve_in_weave_prints_weave_dir_path() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let weave_dir = tmp.path().join("ws--hotfix");
    fs::create_dir_all(&weave_dir).unwrap();
    write_marker(&weave_dir, &root, "ws");

    let canonical_weave = weave_dir.canonicalize().unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("resolve")
        .current_dir(&weave_dir)
        .assert()
        .success()
        .stdout(common::operator_path_stdout(&canonical_weave));
}

#[test]

fn resolve_in_weave_subdir_prints_weave_dir_path() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let weave_dir = tmp.path().join("ws--agent-42");
    let repo_dir = weave_dir.join("github").join("acme").join("client");
    fs::create_dir_all(&repo_dir).unwrap();
    write_marker(&weave_dir, &root, "ws");

    let canonical_weave = weave_dir.canonicalize().unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("resolve")
        .current_dir(&repo_dir)
        .assert()
        .success()
        .stdout(common::operator_path_stdout(&canonical_weave));
}

// ============================================================================
// 5. `rwv` outside any workspace — error
// ============================================================================

#[test]

fn context_display_outside_workspace_errors() {
    let tmp = common::tempdir().unwrap();
    // No workspace markers — just an empty temp dir

    Command::cargo_bin("rwv")
        .unwrap()
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("no repoweave workspace found")
                .or(predicate::str::contains("not in a workspace")),
        );
}

#[test]

fn resolve_outside_workspace_errors() {
    let tmp = common::tempdir().unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("resolve")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("no repoweave workspace found")
                .or(predicate::str::contains("not in a workspace")),
        );
}
