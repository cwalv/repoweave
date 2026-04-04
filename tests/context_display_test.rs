//! E2E tests for `rwv` (no subcommand) context display and `rwv resolve`.
//!
//! These tests run the `rwv` binary via `std::process::Command` and verify
//! exit codes and output patterns. The actual implementation lands in bead 4b;
//! tests that require that implementation are marked `#[ignore]`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;

/// Create a minimal workspace root at `parent/name` with `github/` and
/// `projects/` marker directories. Returns the root path.
fn make_workspace(parent: &Path, name: &str) -> std::path::PathBuf {
    let root = parent.join(name);
    fs::create_dir_all(root.join("github")).unwrap();
    fs::create_dir_all(root.join("projects")).unwrap();
    root
}

// ============================================================================
// 1. `rwv` (no subcommand) in a primary directory
// ============================================================================

#[test]

fn context_display_in_primary_shows_root_and_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "myws");

    // Create a project so the display has something to list
    fs::create_dir_all(root.join("projects").join("web-app")).unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .current_dir(&root)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            root.canonicalize().unwrap().to_string_lossy().as_ref(),
        ))
        .stdout(predicate::str::contains("web-app"));
}

#[test]

fn context_display_in_primary_subdir() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    let deep = root.join("github").join("acme").join("server");
    fs::create_dir_all(&deep).unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .current_dir(&deep)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            root.canonicalize().unwrap().to_string_lossy().as_ref(),
        ));
}

// ============================================================================
// 2. `rwv` (no subcommand) in a weave directory
// ============================================================================

#[test]

fn context_display_in_weave_shows_weave_info() {
    let tmp = tempfile::tempdir().unwrap();
    let _root = make_workspace(tmp.path(), "ws");

    // Create the weave sibling directory
    let weave_dir = tmp.path().join("ws--hotfix");
    fs::create_dir_all(&weave_dir).unwrap();

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
    let tmp = tempfile::tempdir().unwrap();
    let _root = make_workspace(tmp.path(), "ws");

    let weave_dir = tmp.path().join("ws--feat-login");
    let repo_dir = weave_dir.join("github").join("acme").join("server");
    fs::create_dir_all(&repo_dir).unwrap();

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
    let tmp = tempfile::tempdir().unwrap();
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
        .stdout(predicate::str::contains(
            canonical_root.to_string_lossy().as_ref(),
        ));
}

#[test]

fn resolve_at_workspace_root_prints_root_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let canonical_root = root.canonicalize().unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("resolve")
        .current_dir(&root)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            canonical_root.to_string_lossy().as_ref(),
        ));
}

// ============================================================================
// 4. `rwv resolve` in a weave directory
// ============================================================================

#[test]

fn resolve_in_weave_prints_weave_dir_path() {
    let tmp = tempfile::tempdir().unwrap();
    let _root = make_workspace(tmp.path(), "ws");

    let weave_dir = tmp.path().join("ws--hotfix");
    fs::create_dir_all(&weave_dir).unwrap();

    let canonical_weave = weave_dir.canonicalize().unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("resolve")
        .current_dir(&weave_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            canonical_weave.to_string_lossy().as_ref(),
        ));
}

#[test]

fn resolve_in_weave_subdir_prints_weave_dir_path() {
    let tmp = tempfile::tempdir().unwrap();
    let _root = make_workspace(tmp.path(), "ws");

    let weave_dir = tmp.path().join("ws--agent-42");
    let repo_dir = weave_dir.join("github").join("acme").join("client");
    fs::create_dir_all(&repo_dir).unwrap();

    let canonical_weave = weave_dir.canonicalize().unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("resolve")
        .current_dir(&repo_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            canonical_weave.to_string_lossy().as_ref(),
        ));
}

// ============================================================================
// 5. `rwv` outside any workspace — error
// ============================================================================

#[test]

fn context_display_outside_workspace_errors() {
    let tmp = tempfile::tempdir().unwrap();
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
    let tmp = tempfile::tempdir().unwrap();

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
