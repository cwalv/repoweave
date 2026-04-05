//! E2E tests for `rwv activate PROJECT`.
//!
//! `rwv activate` sets the active project by:
//! 1. Generating ecosystem files in `projects/{project}/` via integrations
//! 2. Creating symlinks at workspace root pointing to generated files
//! 3. Writing `.rwv-active` with the project name
//!
//! Switching projects swaps the symlinks. Only one project is active at a time.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;

/// Build a `Command` for the `rwv` binary.
fn rwv() -> Command {
    Command::cargo_bin("rwv").expect("rwv binary should be buildable")
}

/// Create a minimal workspace with a `github/` marker and `projects/` dir.
/// Returns the workspace root path.
fn make_workspace(tmp: &Path) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

/// Create a project directory with an `rwv.yaml` manifest listing the given repos.
/// Each repo entry is `(path, role)`. Also creates the repo directories with
/// the specified manifest files (e.g., `package.json`, `Cargo.toml`).
fn make_project(
    ws: &Path,
    project: &str,
    repos: &[(&str, &str, &[&str])], // (path, role, manifest_files)
) {
    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let mut yaml = String::from("repositories:\n");
    for (path, role, manifest_files) in repos {
        yaml.push_str(&format!(
            "  {}:\n    type: git\n    url: https://github.com/test/{}.git\n    version: main\n    role: {}\n",
            path,
            path.split('/').last().unwrap(),
            role,
        ));

        // Create the repo directory and its manifest files at workspace root
        let repo_dir = ws.join(path);
        std::fs::create_dir_all(&repo_dir).unwrap();
        for mf in *manifest_files {
            let content = if *mf == "package.json" {
                format!(
                    "{{ \"name\": \"{}\", \"version\": \"1.0.0\" }}\n",
                    path.split('/').last().unwrap()
                )
            } else if *mf == "Cargo.toml" {
                format!(
                    "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                    path.split('/').last().unwrap()
                )
            } else {
                String::new()
            };
            std::fs::write(repo_dir.join(mf), content).unwrap();
        }
    }

    std::fs::write(project_dir.join("rwv.yaml"), yaml).unwrap();
}

// ============================================================================
// Smoke tests -- command recognition
// ============================================================================

#[test]
fn activate_subcommand_is_recognised() {
    let assert = rwv().arg("activate").assert();
    let output = assert.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "activate should be a recognised subcommand, got stderr: {stderr}"
    );
}

#[test]
fn activate_requires_project_argument() {
    rwv()
        .arg("activate")
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
}

// ============================================================================
// Basic activate -- generates files and writes .rwv-active
// ============================================================================

#[test]
fn activate_writes_rwv_active_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    make_project(
        &ws,
        "web-app",
        &[("github/acme/server", "primary", &["package.json"])],
    );

    rwv()
        .args(["activate", "web-app"])
        .current_dir(&ws)
        .assert()
        .success();

    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(active.trim(), "web-app");
}

#[test]
fn activate_generates_ecosystem_files_in_project_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    make_project(
        &ws,
        "web-app",
        &[
            ("github/acme/server", "primary", &["package.json"]),
            ("github/acme/web", "primary", &["package.json"]),
        ],
    );

    rwv()
        .args(["activate", "web-app"])
        .current_dir(&ws)
        .assert()
        .success();

    // The npm-workspaces integration should generate package.json in the
    // project directory (projects/web-app/package.json).
    let generated = ws.join("projects/web-app/package.json");
    assert!(
        generated.exists(),
        "package.json should be generated in the project directory"
    );

    let content = std::fs::read_to_string(&generated).unwrap();
    assert!(
        content.contains("github/acme/server"),
        "generated package.json should list server repo, got: {content}"
    );
    assert!(
        content.contains("github/acme/web"),
        "generated package.json should list web repo, got: {content}"
    );
}

#[test]
fn activate_creates_symlinks_at_workspace_root() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    make_project(
        &ws,
        "web-app",
        &[("github/acme/server", "primary", &["package.json"])],
    );

    rwv()
        .args(["activate", "web-app"])
        .current_dir(&ws)
        .assert()
        .success();

    // The root package.json should be a symlink to projects/web-app/package.json.
    let root_pkg = ws.join("package.json");
    assert!(
        root_pkg.exists(),
        "package.json symlink should exist at workspace root"
    );
    assert!(
        root_pkg
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "package.json at root should be a symlink"
    );

    let target = std::fs::read_link(&root_pkg).unwrap();
    // The symlink target should reference the project directory.
    let target_str = target.to_string_lossy();
    assert!(
        target_str.contains("projects/web-app/package.json"),
        "symlink should point to projects/web-app/package.json, got: {target_str}"
    );
}

#[test]
fn activate_symlinks_point_to_correct_project_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    make_project(
        &ws,
        "web-app",
        &[("github/acme/server", "primary", &["package.json"])],
    );

    rwv()
        .args(["activate", "web-app"])
        .current_dir(&ws)
        .assert()
        .success();

    // Read the symlink target and verify the actual file content matches
    // what was generated in the project directory.
    let root_content = std::fs::read_to_string(ws.join("package.json")).unwrap();
    let project_content =
        std::fs::read_to_string(ws.join("projects/web-app/package.json")).unwrap();
    assert_eq!(
        root_content, project_content,
        "symlink at root should serve the same content as the project dir file"
    );
}

// ============================================================================
// Multiple ecosystem files
// ============================================================================

#[test]
fn activate_handles_multiple_ecosystem_types() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    make_project(
        &ws,
        "polyglot",
        &[
            ("github/acme/web", "primary", &["package.json"]),
            ("github/acme/svc", "primary", &["Cargo.toml"]),
        ],
    );

    rwv()
        .args(["activate", "polyglot"])
        .current_dir(&ws)
        .assert()
        .success();

    // Both ecosystem files should be generated and symlinked.
    let root_pkg = ws.join("package.json");
    let root_cargo = ws.join("Cargo.toml");
    assert!(root_pkg.exists(), "package.json symlink should exist");
    assert!(root_cargo.exists(), "Cargo.toml symlink should exist");

    assert!(
        root_pkg
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "package.json should be a symlink"
    );
    assert!(
        root_cargo
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "Cargo.toml should be a symlink"
    );
}

// ============================================================================
// Switching projects -- activate A then B swaps symlinks
// ============================================================================

#[test]
fn switching_projects_swaps_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    // Project A: has one npm repo
    make_project(
        &ws,
        "project-a",
        &[("github/acme/alpha", "primary", &["package.json"])],
    );
    // Project B: has a different npm repo
    make_project(
        &ws,
        "project-b",
        &[("github/acme/beta", "primary", &["package.json"])],
    );

    // Activate project A
    rwv()
        .args(["activate", "project-a"])
        .current_dir(&ws)
        .assert()
        .success();

    let root_pkg = ws.join("package.json");
    let link_a = std::fs::read_link(&root_pkg).unwrap();
    let link_a_str = link_a.to_string_lossy().to_string();
    assert!(
        link_a_str.contains("project-a"),
        "after activating A, symlink should point to project-a, got: {link_a_str}"
    );

    let content_a = std::fs::read_to_string(&root_pkg).unwrap();
    assert!(
        content_a.contains("github/acme/alpha"),
        "project-a's package.json should reference alpha"
    );

    // Activate project B -- symlinks should swap
    rwv()
        .args(["activate", "project-b"])
        .current_dir(&ws)
        .assert()
        .success();

    let link_b = std::fs::read_link(&root_pkg).unwrap();
    let link_b_str = link_b.to_string_lossy().to_string();
    assert!(
        link_b_str.contains("project-b"),
        "after activating B, symlink should point to project-b, got: {link_b_str}"
    );

    let content_b = std::fs::read_to_string(&root_pkg).unwrap();
    assert!(
        content_b.contains("github/acme/beta"),
        "project-b's package.json should reference beta"
    );
    assert!(
        !content_b.contains("github/acme/alpha"),
        "project-b's package.json should NOT reference alpha"
    );
}

#[test]
fn switching_projects_updates_rwv_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    make_project(
        &ws,
        "proj-a",
        &[("github/acme/alpha", "primary", &["package.json"])],
    );
    make_project(
        &ws,
        "proj-b",
        &[("github/acme/beta", "primary", &["package.json"])],
    );

    rwv()
        .args(["activate", "proj-a"])
        .current_dir(&ws)
        .assert()
        .success();

    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(active.trim(), "proj-a");

    rwv()
        .args(["activate", "proj-b"])
        .current_dir(&ws)
        .assert()
        .success();

    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(active.trim(), "proj-b");
}

#[test]
fn switching_removes_stale_symlinks_from_previous_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    // Project A has both npm and cargo
    make_project(
        &ws,
        "proj-a",
        &[
            ("github/acme/web", "primary", &["package.json"]),
            ("github/acme/svc", "primary", &["Cargo.toml"]),
        ],
    );
    // Project B has only npm (no cargo)
    make_project(
        &ws,
        "proj-b",
        &[("github/acme/frontend", "primary", &["package.json"])],
    );

    // Activate A -- both symlinks appear
    rwv()
        .args(["activate", "proj-a"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(ws.join("package.json").exists(), "package.json from A");
    assert!(ws.join("Cargo.toml").exists(), "Cargo.toml from A");

    // Activate B -- Cargo.toml symlink should be removed since B has no cargo repos
    rwv()
        .args(["activate", "proj-b"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        ws.join("package.json").exists(),
        "package.json should still exist for B"
    );

    // Cargo.toml symlink should be gone (B has no Cargo repos).
    // If a regular Cargo.toml file remains, it should not be a symlink pointing
    // to project-a.
    let cargo_path = ws.join("Cargo.toml");
    if cargo_path.exists() {
        // If it exists, it should not be a stale symlink to project-a
        if cargo_path
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
        {
            let target = std::fs::read_link(&cargo_path).unwrap();
            let target_str = target.to_string_lossy();
            assert!(
                !target_str.contains("proj-a"),
                "stale Cargo.toml symlink to proj-a should be removed, got: {target_str}"
            );
        }
    }
    // If Cargo.toml doesn't exist at all, that's the expected outcome.
}

// ============================================================================
// Switching back restores correct symlinks
// ============================================================================

#[test]
fn switching_back_restores_original_symlinks() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    make_project(
        &ws,
        "proj-a",
        &[("github/acme/alpha", "primary", &["package.json"])],
    );
    make_project(
        &ws,
        "proj-b",
        &[("github/acme/beta", "primary", &["package.json"])],
    );

    // A -> B -> A
    rwv()
        .args(["activate", "proj-a"])
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["activate", "proj-b"])
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["activate", "proj-a"])
        .current_dir(&ws)
        .assert()
        .success();

    let link = std::fs::read_link(ws.join("package.json")).unwrap();
    let link_str = link.to_string_lossy();
    assert!(
        link_str.contains("proj-a"),
        "after switching back to A, symlink should point to proj-a, got: {link_str}"
    );

    let content = std::fs::read_to_string(ws.join("package.json")).unwrap();
    assert!(
        content.contains("github/acme/alpha"),
        "content should reference alpha after switching back to A"
    );
}

// ============================================================================
// Activate from subdirectory
// ============================================================================

#[test]
fn activate_works_from_workspace_subdirectory() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    make_project(
        &ws,
        "my-proj",
        &[("github/acme/repo", "primary", &["package.json"])],
    );

    rwv()
        .args(["activate", "my-proj"])
        .current_dir(ws.join("github"))
        .assert()
        .success();

    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(active.trim(), "my-proj");
    assert!(
        ws.join("package.json").exists(),
        "symlink should be created at workspace root even when run from subdirectory"
    );
}

// ============================================================================
// Error cases
// ============================================================================

#[test]
fn activate_nonexistent_project_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    rwv()
        .args(["activate", "does-not-exist"])
        .current_dir(&ws)
        .assert()
        .failure();
}

// ============================================================================
// Re-activate same project is idempotent
// ============================================================================

#[test]
fn activate_same_project_twice_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    make_project(
        &ws,
        "my-proj",
        &[("github/acme/repo", "primary", &["package.json"])],
    );

    rwv()
        .args(["activate", "my-proj"])
        .current_dir(&ws)
        .assert()
        .success();

    let content1 = std::fs::read_to_string(ws.join("package.json")).unwrap();

    rwv()
        .args(["activate", "my-proj"])
        .current_dir(&ws)
        .assert()
        .success();

    let content2 = std::fs::read_to_string(ws.join("package.json")).unwrap();
    assert_eq!(
        content1, content2,
        "re-activating should produce identical output"
    );

    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(active.trim(), "my-proj");
}

// ============================================================================
// No ecosystem files -- still activates and writes .rwv-active
// ============================================================================

#[test]
fn activate_project_with_no_ecosystem_files() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    // Project with repos that have no ecosystem manifest files
    make_project(&ws, "plain-proj", &[("github/acme/docs", "primary", &[])]);

    rwv()
        .args(["activate", "plain-proj"])
        .current_dir(&ws)
        .assert()
        .success();

    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(active.trim(), "plain-proj");

    // No ecosystem symlinks should be created
    assert!(
        !ws.join("package.json").exists(),
        "no package.json symlink when no npm repos"
    );
    assert!(
        !ws.join("Cargo.toml").exists(),
        "no Cargo.toml symlink when no cargo repos"
    );
}
