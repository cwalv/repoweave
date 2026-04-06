//! E2E tests for `rwv prime`.

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

fn write_manifest(root: &Path, project: &str, yaml: &str) {
    let dir = root.join("projects").join(project);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("rwv.yaml"), yaml).unwrap();
}

// ============================================================================
// 1. Silent outside workspace
// ============================================================================

#[test]
fn prime_silent_outside_workspace() {
    let tmp = tempfile::tempdir().unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("prime")
        .current_dir(tmp.path())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

// ============================================================================
// 2. Basic output in primary with project
// ============================================================================

#[test]
fn prime_primary_with_project() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    write_manifest(
        &root,
        "web-app",
        r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: primary
  github/acme/client:
    type: git
    url: https://github.com/acme/client.git
    version: develop
    role: fork
integrations:
  cargo:
    enabled: true
"#,
    );

    fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("prime")
        .current_dir(&root)
        .assert()
        .success()
        .stdout(predicate::str::contains("# repoweave workspace"))
        .stdout(predicate::str::contains("**Project**: `web-app`"))
        .stdout(predicate::str::contains("## Repositories"))
        .stdout(predicate::str::contains("github/acme/server"))
        .stdout(predicate::str::contains("primary"))
        .stdout(predicate::str::contains("github/acme/client"))
        .stdout(predicate::str::contains("fork"))
        .stdout(predicate::str::contains("## Integrations"))
        .stdout(predicate::str::contains("- cargo"))
        .stdout(predicate::str::contains("## Key commands"))
        .stdout(predicate::str::contains("## Directory layout"));
}

// ============================================================================
// 3. No project active — minimal output
// ============================================================================

#[test]
fn prime_no_active_project() {
    let tmp = tempfile::tempdir().unwrap();
    let _root = make_workspace(tmp.path(), "ws");

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("prime")
        .current_dir(tmp.path().join("ws"))
        .assert()
        .success()
        .stdout(predicate::str::contains("# repoweave workspace"))
        .stdout(predicate::str::contains("**Weave**"))
        .stdout(predicate::function(|s: &str| !s.contains("**Project**")));
}

// ============================================================================
// 4. Inside a workweave
// ============================================================================

#[test]
fn prime_in_workweave() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    write_manifest(
        &root,
        "ws",
        r#"
repositories:
  github/acme/server:
    type: git
    url: https://github.com/acme/server.git
    version: main
    role: primary
"#,
    );

    let weave_dir = tmp.path().join("ws--hotfix");
    fs::create_dir_all(&weave_dir).unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("prime")
        .current_dir(&weave_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("**Workweave**"))
        .stdout(predicate::str::contains("**Project**: `ws`"))
        .stdout(predicate::str::contains("## Repositories"));
}

// ============================================================================
// 5. Directory layout shows active marker
// ============================================================================

#[test]
fn prime_directory_layout_active_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    fs::create_dir_all(root.join("projects").join("web-app")).unwrap();
    fs::create_dir_all(root.join("projects").join("mobile")).unwrap();

    write_manifest(&root, "web-app", "repositories: {}\n");
    fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    Command::cargo_bin("rwv")
        .unwrap()
        .arg("prime")
        .current_dir(&root)
        .assert()
        .success()
        .stdout(predicate::str::contains("web-app/ (active)"));
}
