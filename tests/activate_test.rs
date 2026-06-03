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
use repoweave::manifest::ProjectName;
use repoweave::workspace::WorkweaveMarker;
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
///
/// **Trigger-model note (fo-cnpjy.3):** under the new trigger-model split,
/// `rwv activate` is a context verb — it surfaces existing
/// managed/generated content via symlinks but does not author. So this
/// helper now also drives the intent path
/// (`repoweave::activate::activate_intent`) after writing the manifest, to
/// pre-author the integration content into `projects/<project>/` exactly as
/// a real `rwv add` workflow would have. The CLI-level `rwv activate` calls
/// in each test then exercise only the surfacing-and-verify behavior we
/// intend to characterize. This mirrors the real-world workflow (the
/// committed integration files live in the project repo by construction;
/// activate just surfaces them).
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
            path.split('/').next_back().unwrap(),
            role,
        ));

        // Create the repo directory and its manifest files at workspace root
        let repo_dir = ws.join(path);
        std::fs::create_dir_all(&repo_dir).unwrap();
        for mf in *manifest_files {
            let content = if *mf == "package.json" {
                format!(
                    "{{ \"name\": \"{}\", \"version\": \"1.0.0\" }}\n",
                    path.split('/').next_back().unwrap()
                )
            } else if *mf == "Cargo.toml" {
                format!(
                    "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                    path.split('/').next_back().unwrap()
                )
            } else {
                String::new()
            };
            std::fs::write(repo_dir.join(mf), content).unwrap();
        }
    }

    std::fs::write(project_dir.join("rwv.yaml"), yaml).unwrap();

    // Pre-author integration content via the intent path (see trigger-model
    // note above). We ignore the result: if no integration runs (e.g., the
    // test only exercises a manifest with no ecosystem files), activate_intent
    // is a no-op other than setting .rwv-active, which the test will overwrite
    // anyway when it calls `rwv activate`. We do NOT propagate this side-effect
    // back through the CLI — the test scaffolding stands in for the human
    // workflow's "rwv add wrote both the manifest entry and the ecosystem
    // file."
    let _ = repoweave::activate::activate_intent(project, ws);
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
        &[("github/acme/server", "owned", &["package.json"])],
    );

    rwv()
        .args(["activate", "web-app", "--no-install"])
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
            ("github/acme/server", "owned", &["package.json"]),
            ("github/acme/web", "owned", &["package.json"]),
        ],
    );

    rwv()
        .args(["activate", "web-app", "--no-install"])
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
        &[("github/acme/server", "owned", &["package.json"])],
    );

    rwv()
        .args(["activate", "web-app", "--no-install"])
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
    assert!(
        target.ends_with("projects/web-app/package.json"),
        "symlink should point to projects/web-app/package.json, got: {}",
        target.display()
    );
}

#[test]
fn activate_symlinks_point_to_correct_project_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    make_project(
        &ws,
        "web-app",
        &[("github/acme/server", "owned", &["package.json"])],
    );

    rwv()
        .args(["activate", "web-app", "--no-install"])
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
            ("github/acme/web", "owned", &["package.json"]),
            ("github/acme/svc", "owned", &["Cargo.toml"]),
        ],
    );

    rwv()
        .args(["activate", "polyglot", "--no-install"])
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
        &[("github/acme/alpha", "owned", &["package.json"])],
    );
    // Project B: has a different npm repo
    make_project(
        &ws,
        "project-b",
        &[("github/acme/beta", "owned", &["package.json"])],
    );

    // Activate project A
    rwv()
        .args(["activate", "project-a", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();

    let root_pkg = ws.join("package.json");
    let link_a = std::fs::read_link(&root_pkg).unwrap();
    assert!(
        link_a.components().any(|c| c.as_os_str() == "project-a"),
        "after activating A, symlink should point to project-a, got: {}",
        link_a.display()
    );

    let content_a = std::fs::read_to_string(&root_pkg).unwrap();
    assert!(
        content_a.contains("github/acme/alpha"),
        "project-a's package.json should reference alpha"
    );

    // Activate project B -- symlinks should swap
    rwv()
        .args(["activate", "project-b", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();

    let link_b = std::fs::read_link(&root_pkg).unwrap();
    assert!(
        link_b.components().any(|c| c.as_os_str() == "project-b"),
        "after activating B, symlink should point to project-b, got: {}",
        link_b.display()
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
        &[("github/acme/alpha", "owned", &["package.json"])],
    );
    make_project(
        &ws,
        "proj-b",
        &[("github/acme/beta", "owned", &["package.json"])],
    );

    rwv()
        .args(["activate", "proj-a", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();

    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(active.trim(), "proj-a");

    rwv()
        .args(["activate", "proj-b", "--no-install"])
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
            ("github/acme/web", "owned", &["package.json"]),
            ("github/acme/svc", "owned", &["Cargo.toml"]),
        ],
    );
    // Project B has only npm (no cargo)
    make_project(
        &ws,
        "proj-b",
        &[("github/acme/frontend", "owned", &["package.json"])],
    );

    // Activate A -- both symlinks appear
    rwv()
        .args(["activate", "proj-a", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(ws.join("package.json").exists(), "package.json from A");
    assert!(ws.join("Cargo.toml").exists(), "Cargo.toml from A");

    // Activate B -- Cargo.toml symlink should be removed since B has no cargo repos
    rwv()
        .args(["activate", "proj-b", "--no-install"])
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
            assert!(
                !target.components().any(|c| c.as_os_str() == "proj-a"),
                "stale Cargo.toml symlink to proj-a should be removed, got: {}",
                target.display()
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
        &[("github/acme/alpha", "owned", &["package.json"])],
    );
    make_project(
        &ws,
        "proj-b",
        &[("github/acme/beta", "owned", &["package.json"])],
    );

    // A -> B -> A
    rwv()
        .args(["activate", "proj-a", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["activate", "proj-b", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["activate", "proj-a", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();

    let link = std::fs::read_link(ws.join("package.json")).unwrap();
    assert!(
        link.components().any(|c| c.as_os_str() == "proj-a"),
        "after switching back to A, symlink should point to proj-a, got: {}",
        link.display()
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
        &[("github/acme/repo", "owned", &["package.json"])],
    );

    rwv()
        .args(["activate", "my-proj", "--no-install"])
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
        &[("github/acme/repo", "owned", &["package.json"])],
    );

    rwv()
        .args(["activate", "my-proj", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();

    let content1 = std::fs::read_to_string(ws.join("package.json")).unwrap();

    rwv()
        .args(["activate", "my-proj", "--no-install"])
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
// A3: activation-symlink sweep skips ALL builtin registry dirs (not just the
// hardcoded github/gitlab/bitbucket set) and otherwise descends.
// ============================================================================

/// The recursive sweep must (a) skip every builtin registry directory and
/// (b) still descend into ordinary subdirectories to clean up nested
/// activation symlinks. Verified via deactivate, which runs the sweep.
#[test]
fn deactivate_descends_into_nondir_registry_subtrees() {
    // Under fo-cnpjy.3 (owner-scoped symlink removal), this test exercises
    // the recursion + the owner-scoping rule together:
    //
    //   1. The sweep descends into non-registry, non-`projects/`, non-`.git/`
    //      subdirectories.
    //   2. WITHIN those subdirectories, it unlinks ONLY symlinks whose
    //      root-relative path is in the owned set AND whose target resolves
    //      to `projects/<some-project>/<that-file>`. User-planted symlinks
    //      at paths the integrations don't own are preserved.
    //   3. It refuses to descend into registry directories.
    //
    // The pre-existing version of this test relied on the over-broad
    // "any target with a `projects` component" predicate (which would sweep
    // a user's `docs/package.json` symlink). That predicate is replaced; the
    // new behavior preserves user-owned paths. We assert the new contract.
    //
    // We use the `gita` integration (enabled in this project's rwv.yaml
    // via `integrations:`) to get a *real* nested owned path
    // (`gita/repos.csv`), so the descent-into-subdirectory leg is exercised
    // with an actually-owned name.
    use std::os::unix::fs::symlink;

    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    // Manifest enables gita so its `gita/repos.csv` + `gita/groups.csv` are
    // in the owned set; we don't care about their actual content here.
    std::fs::write(
        project_dir.join("rwv.yaml"),
        "repositories:\n  github/acme/server:\n    type: git\n    url: https://github.com/test/server.git\n    version: main\n    role: owned\n\
integrations:\n  gita:\n    enabled: true\n",
    )
    .unwrap();
    std::fs::create_dir_all(ws.join("github/acme/server")).unwrap();

    // Pre-author via intent path so context-mode activate has something
    // to surface (matches the new trigger-model contract).
    let _ = repoweave::activate::activate_intent("web-app", &ws);

    // Activate to refresh top-level + nested gita symlinks (context mode).
    rwv()
        .args(["activate", "web-app", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();

    // The framework should have produced gita/repos.csv as a symlink under
    // a subdirectory — proving the create path descends.
    let gita_csv = ws.join("gita/repos.csv");
    assert!(
        gita_csv.symlink_metadata().is_ok(),
        "gita/repos.csv symlink should have been created (descent on create path)"
    );

    // Plant a user-owned symlink at `gita/extras.txt` pointing into the
    // project — `extras.txt` is NOT in any integration's owned set.
    // Under the new owner-scoped predicate this MUST be preserved
    // (rwv-c5h shape: don't reap what we don't own).
    let user_relative_target = std::path::PathBuf::from("../projects/web-app/gita/extras.txt");
    let user_nested_link = ws.join("gita/extras.txt");
    symlink(&user_relative_target, &user_nested_link).unwrap();

    // And plant a symlink inside `github/` — it must NOT be removed
    // because the sweep refuses to descend into registry dirs.
    let registry_target = std::path::PathBuf::from("../projects/web-app/gita/repos.csv");
    let registry_link = ws.join("github/squat.csv");
    symlink(&registry_target, &registry_link).unwrap();

    // Re-activate: this runs `remove_activation_symlinks` before re-creating
    // links, exercising the sweep on the gita subdirectory.
    rwv()
        .args(["activate", "web-app", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();

    // After re-activate, the OWNED gita/repos.csv symlink should have been
    // removed and recreated (we don't observe the in-between state — just
    // that it currently exists). The descent-and-sweep is proven by the
    // user-symlink-preservation + registry-skip assertions below.
    assert!(
        gita_csv.symlink_metadata().is_ok(),
        "gita/repos.csv symlink should exist after re-activate (removed-and-recreated)"
    );
    assert!(
        user_nested_link.symlink_metadata().is_ok(),
        "nested USER symlink (name NOT in any owned set) must be preserved — rwv only sweeps what it owns"
    );
    assert!(
        registry_link.symlink_metadata().is_ok(),
        "symlink inside a registry-named dir must NOT be swept (skip set)"
    );
}

// ============================================================================
// Workweave guard -- activate is rejected inside a workweave
// ============================================================================

/// `rwv activate` from a workweave must exit non-zero and include both the
/// "no effect in a workweave" explanation and the primary path in the error
/// message, so the user can copy-paste a `cd` command to primary and rerun.
/// The primary's `.rwv-active` must not change.
///
/// Acceptance criteria from fo-9fnae:
///   - exits non-zero
///   - error message contains "workweave" and the actual primary path
///   - works for any project name (including the one already active in primary)
#[test]
fn activate_from_workweave_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    make_project(
        &ws,
        "my-proj",
        &[("github/acme/repo", "owned", &["package.json"])],
    );

    // Set a known .rwv-active in primary so we can verify it is not changed.
    std::fs::write(ws.join(".rwv-active"), "my-proj\n").unwrap();

    // Create a workweave directory with a .rwv-workweave marker pointing to
    // primary. The workweave lives at ws/../ws--ww1 (sibling of primary).
    let workweave_dir = tmp.path().join("ws--ww1");
    std::fs::create_dir_all(&workweave_dir).unwrap();
    let primary_canon = ws.canonicalize().unwrap();
    let marker = WorkweaveMarker {
        primary: primary_canon.clone(),
        project: ProjectName::new("my-proj"),
        parent: primary_canon.clone(),
    };
    marker.write(&workweave_dir).unwrap();

    // Running activate from the workweave root must fail.
    rwv()
        .args(["activate", "my-proj", "--no-install"])
        .current_dir(&workweave_dir)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("workweave").and(predicate::str::contains(
                primary_canon.to_string_lossy().as_ref(),
            )),
        );

    // Primary's .rwv-active must be unchanged.
    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(active.trim(), "my-proj", ".rwv-active must not be modified");
}

/// The guard fires even when activating a *different* project from the workweave.
#[test]
fn activate_different_project_from_workweave_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    make_project(
        &ws,
        "proj-a",
        &[("github/acme/alpha", "owned", &["package.json"])],
    );
    make_project(
        &ws,
        "proj-b",
        &[("github/acme/beta", "owned", &["package.json"])],
    );

    // Activate proj-a in primary so .rwv-active is set.
    rwv()
        .args(["activate", "proj-a", "--no-install"])
        .current_dir(&ws)
        .assert()
        .success();

    let workweave_dir = tmp.path().join("ws--ww2");
    std::fs::create_dir_all(&workweave_dir).unwrap();
    let primary_canon = ws.canonicalize().unwrap();
    let marker = WorkweaveMarker {
        primary: primary_canon.clone(),
        project: ProjectName::new("proj-a"),
        parent: primary_canon.clone(),
    };
    marker.write(&workweave_dir).unwrap();

    // Attempt to activate proj-b from the workweave — must fail.
    rwv()
        .args(["activate", "proj-b", "--no-install"])
        .current_dir(&workweave_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains("workweave"));

    // Primary should still have proj-a active.
    let active = std::fs::read_to_string(ws.join(".rwv-active")).unwrap();
    assert_eq!(
        active.trim(),
        "proj-a",
        "primary .rwv-active must not be changed to proj-b"
    );
}

// ============================================================================
// No ecosystem files -- still activates and writes .rwv-active
// ============================================================================

#[test]
fn activate_project_with_no_ecosystem_files() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    // Project with repos that have no ecosystem manifest files
    make_project(&ws, "plain-proj", &[("github/acme/docs", "owned", &[])]);

    rwv()
        .args(["activate", "plain-proj", "--no-install"])
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
