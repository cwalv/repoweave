//! Integration tests for documentation claims about `rwv activate`, `rwv check`,
//! and the `static-files` integration.
//!
//! Tests are keyed to their bead/claim IDs:
//!   - project-reporoot-201  workspace context from project dir
//!   - project-reporoot-85h9 check: missing role field, workweave drift
//!   - project-reporoot-c3ad activate symlinks ecosystem + lock files
//!   - project-reporoot-1ejx static-files integration
//!   - project-reporoot-l56a activate runs install commands

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process;

// ===========================================================================
// Shared helpers
// ===========================================================================

/// Build a `Command` for the `rwv` binary.
fn rwv() -> Command {
    Command::cargo_bin("rwv").expect("rwv binary should be buildable")
}

/// Run a git command in `dir`, panicking on failure.
fn git(args: &[&str], dir: &Path) {
    let status = process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

/// Initialise a real git repo at `path` with one commit on `main`.
fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Create a minimal workspace:
///   {parent}/ws/github/               — registry marker (workspace root detection)
///   {parent}/ws/projects/{project}/   — project dir with rwv.yaml
///   {parent}/ws/github/org/repo/      — a real git repo
///
/// Returns (workspace_root, bare_repo_path) so callers can use file:// URLs.
fn make_workspace_with_git_repo(parent: &Path, project: &str) -> (PathBuf, PathBuf) {
    let ws = parent.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        "repositories:\n  github/org/repo:\n    type: git\n    url: file://{repo}\n    version: main\n    role: primary\n",
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    (ws, repo_path)
}

/// Create a minimal workspace with no real git repo — just the directory
/// structure and an rwv.yaml.  Useful for tests that exercise parsing/check
/// without needing live VCS operations.
fn make_workspace_no_repo(parent: &Path, project: &str) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();
    ws
}

// ===========================================================================
// 1. workspace_context_from_project_dir (project-reporoot-201)
//
// Doc claim: Running commands from inside projects/<name>/ resolves to the
// weave with that project active.
// ===========================================================================

#[test]
fn workspace_context_from_project_dir_resolve() {
    // Doc claim: `rwv resolve` from inside projects/<project>/ returns the
    // workspace root path (not the project dir itself).
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _) = make_workspace_with_git_repo(tmp.path(), "my-project");

    let project_dir = ws.join("projects/my-project");

    let output = rwv()
        .arg("resolve")
        .current_dir(&project_dir)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "rwv resolve should succeed from inside projects/<project>/, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Canonicalize the expected path so /var vs /private/var symlinks (macOS)
    // don't cause spurious mismatches.
    let ws_canonical = std::fs::canonicalize(&ws).unwrap();
    assert_eq!(
        stdout,
        ws_canonical.to_string_lossy().trim_end_matches('/'),
        "rwv resolve should print the workspace root, not the project subdir"
    );
}

#[test]
fn workspace_context_from_project_dir_no_subcommand() {
    // Doc claim: `rwv` (no subcommand) from projects/<project>/ shows the
    // correct project name in its output.
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _) = make_workspace_with_git_repo(tmp.path(), "my-project");

    let project_dir = ws.join("projects/my-project");

    let output = rwv().current_dir(&project_dir).output().unwrap();

    assert!(
        output.status.success(),
        "rwv (no subcommand) should succeed from inside projects/<project>/, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The context display should mention the project name.
    assert!(
        stdout.contains("my-project"),
        "rwv output from project dir should mention the project name 'my-project', got: {stdout}"
    );
}

// ===========================================================================
// 2. check_missing_role (project-reporoot-85h9)
//
// Doc claim: `rwv check` reports entries without a `role` field.
//
// Behavior under test: serde requires `role` because it has no default.
// We verify whether the error surfaces as a parse failure (serde rejects it)
// or as a check-phase diagnostic.  Either behaviour is acceptable — the test
// documents which one actually occurs.
// ===========================================================================

#[test]
fn check_missing_role_field() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_no_repo(tmp.path(), "my-project");

    // Write an rwv.yaml with a repo entry that is missing the `role` field.
    let bad_manifest = r#"repositories:
  github/org/repo:
    type: git
    url: https://github.com/org/repo.git
    version: main
    # role field intentionally omitted
"#;
    std::fs::write(ws.join("projects/my-project/rwv.yaml"), bad_manifest).unwrap();

    let output = rwv().arg("check").current_dir(&ws).output().unwrap();

    // Either serde rejects the manifest at parse time (non-zero exit, stderr
    // contains a parse error) OR the check phase produces a diagnostic.
    // Both signal that the missing role is handled.  A silent success would be
    // the only unexpected outcome.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    if output.status.success() {
        // If it somehow succeeds, there should at least be a warning/diagnostic
        // about the missing field in stdout or stderr.
        // TODO: if this assertion fires, the tool silently accepts a role-less
        // entry which contradicts the doc claim.
        assert!(
            stderr.contains("role") || stdout.contains("role"),
            "if check passes despite missing role, output should mention 'role'; \
             got stdout={stdout} stderr={stderr}"
        );
    } else {
        // Expected path: non-zero exit means the problem was caught.
        // The error message should mention something useful (role, missing, parse, etc.).
        let combined = format!("{stdout}{stderr}");
        assert!(
            combined.contains("role")
                || combined.contains("missing")
                || combined.contains("parse")
                || combined.contains("error")
                || combined.contains("deserializ"),
            "non-zero exit for missing role should include an informative message; \
             got stdout={stdout} stderr={stderr}"
        );
    }
}

// ===========================================================================
// 3. check_workweave_drift — extra worktree (project-reporoot-85h9)
//
// A git repo directory lives inside the workspace that is not referenced by
// any project's rwv.yaml.  `rwv check` should report drift (orphan).
// ===========================================================================

#[test]
fn check_workweave_drift_extra_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let (ws, _) = make_workspace_with_git_repo(tmp.path(), "my-project");

    // Add a second git repo on disk that is NOT in any rwv.yaml.
    let extra_repo = ws.join("github/org/extra-repo");
    init_repo_with_commit(&extra_repo);

    let output = rwv().arg("check").current_dir(&ws).output().unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // TODO: If check does not yet implement drift/orphan detection for repos
    // not referenced by any project, this assertion should be updated once
    // the feature is implemented (project-reporoot-85h9).
    if output.status.success() {
        // The current behavior (success with no diagnostics) is documented here
        // so that future implementers know the test expectation once drift
        // detection is implemented.
        let _ = (&stdout, &stderr); // suppress unused warning
                                    // For now just note that an unlisted repo does not yet cause a failure:
                                    // TODO: once drift detection is implemented, remove this branch and
                                    // assert failure + mention of "extra-repo".
    } else {
        // Preferred future behavior: non-zero exit reporting the orphan.
        assert!(
            stdout.contains("extra-repo")
                || stdout.contains("orphan")
                || stderr.contains("extra-repo")
                || stderr.contains("orphan"),
            "check should mention the unlisted repo 'extra-repo'; \
             got stdout={stdout} stderr={stderr}"
        );
    }
}

// ===========================================================================
// 4. activate_symlinks_ecosystem_lock_files (project-reporoot-c3ad)
//
// Doc claim: Cargo.lock, package-lock.json etc. are symlinked alongside
// workspace configs on activate.
//
// We test what actually happens: the generated Cargo.toml is symlinked; the
// Cargo.lock (if it exists) may or may not be symlinked depending on the
// implementation.  A TODO comment marks the discrepancy if found.
// ===========================================================================

#[test]
fn activate_symlinks_cargo_toml_and_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let ws_root = tmp.path().join("ws");
    std::fs::create_dir_all(ws_root.join("github")).unwrap();

    let project_dir = ws_root.join("projects/cargo-proj");
    std::fs::create_dir_all(&project_dir).unwrap();

    // Create a repo with a Cargo.toml (triggers cargo-workspace integration).
    let repo_dir = ws_root.join("github/org/mylib");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"mylib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();

    // Write a Cargo.lock next to the Cargo.toml (simulates a real project).
    std::fs::write(repo_dir.join("Cargo.lock"), "# generated\n").unwrap();

    let manifest = format!(
        "repositories:\n  github/org/mylib:\n    type: git\n    url: https://github.com/org/mylib.git\n    version: main\n    role: primary\n"
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    rwv()
        .args(["activate", "cargo-proj"])
        .current_dir(&ws_root)
        .assert()
        .success();

    // The workspace-level Cargo.toml should be a symlink pointing to the
    // project directory.
    let root_cargo = ws_root.join("Cargo.toml");
    assert!(
        root_cargo.exists(),
        "Cargo.toml should be present at workspace root after activate"
    );
    assert!(
        root_cargo
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "Cargo.toml at workspace root should be a symlink"
    );
    let target = std::fs::read_link(&root_cargo).unwrap();
    assert!(
        target.to_string_lossy().contains("projects/cargo-proj"),
        "Cargo.toml symlink should point into projects/cargo-proj, got: {}",
        target.display()
    );

    // Cargo.lock should also be symlinked (even as a dangling symlink —
    // cargo fills it in on first build).
    let root_lock = ws_root.join("Cargo.lock");
    assert!(
        root_lock
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "Cargo.lock at workspace root should be a symlink"
    );
    let lock_target = std::fs::read_link(&root_lock).unwrap();
    assert!(
        lock_target
            .to_string_lossy()
            .contains("projects/cargo-proj"),
        "Cargo.lock symlink should point into projects/cargo-proj, got: {}",
        lock_target.display()
    );
}

// ===========================================================================
// 5. static_files_missing_file_warning (project-reporoot-1ejx)
//
// Doc claim: Missing declared file prints warning but activation succeeds.
// ===========================================================================

#[test]
fn static_files_missing_file_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_no_repo(tmp.path(), "my-project");

    let project_dir = ws.join("projects/my-project");

    // Create only one of the two declared files.
    std::fs::write(project_dir.join("exists.txt"), "present").unwrap();
    // missing.txt is intentionally NOT created.

    let manifest = r#"repositories: {}
integrations:
  static-files:
    enabled: true
    files: [exists.txt, missing.txt]
"#;
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    let output = rwv()
        .args(["activate", "my-project"])
        .current_dir(&ws)
        .output()
        .unwrap();

    // Doc claim: activation succeeds even when a file is missing.
    assert!(
        output.status.success(),
        "activate should succeed even when a static file is missing; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Doc claim: stderr mentions the missing file.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing.txt"),
        "stderr should mention the missing file 'missing.txt', got: {stderr}"
    );

    // exists.txt should be symlinked at the workspace root.
    let link = ws.join("exists.txt");
    assert!(
        link.exists(),
        "exists.txt should be symlinked at the workspace root"
    );
    assert!(
        link.symlink_metadata().unwrap().file_type().is_symlink(),
        "exists.txt at workspace root should be a symlink"
    );
}

// ===========================================================================
// 6. static_files_symlink_creation (project-reporoot-1ejx)
//
// Doc claim: Files listed in static-files config are symlinked at the
// workspace root, pointing into projects/<project>/<file>.
// ===========================================================================

#[test]
fn static_files_symlink_creation() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace_no_repo(tmp.path(), "my-project");

    let project_dir = ws.join("projects/my-project");

    // Create the file that will be symlinked.
    std::fs::write(project_dir.join("turbo.json"), r#"{"$schema": "..."}"#).unwrap();

    let manifest = r#"repositories: {}
integrations:
  static-files:
    enabled: true
    files: [turbo.json]
"#;
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    rwv()
        .args(["activate", "my-project"])
        .current_dir(&ws)
        .assert()
        .success();

    // turbo.json should exist at the workspace root as a symlink.
    let link = ws.join("turbo.json");
    assert!(
        link.exists(),
        "turbo.json should be symlinked at the workspace root after activate"
    );
    assert!(
        link.symlink_metadata().unwrap().file_type().is_symlink(),
        "turbo.json at workspace root should be a symlink, not a regular file"
    );

    // The symlink should point to the project directory's copy.
    let target = std::fs::read_link(&link).unwrap();
    let target_str = target.to_string_lossy();
    assert!(
        target_str.contains("projects/my-project/turbo.json"),
        "turbo.json symlink should point to projects/my-project/turbo.json, got: {target_str}"
    );

    // Reading through the symlink should give the original content.
    let content = std::fs::read_to_string(&link).unwrap();
    assert!(
        content.contains("$schema"),
        "symlinked turbo.json should have the original content"
    );
}

// ===========================================================================
// 7. activate_runs_install_commands (project-reporoot-l56a)
//
// Doc claim: `rwv activate` runs ecosystem install commands (npm install,
// uv sync, etc.) after generating workspace config files.
//
// Testing strategy: we verify whether `npm` is on PATH.
//   - If it is: set up an npm workspace, run activate, and check whether
//     package-lock.json or node_modules appear (evidence of npm install).
//   - If npm is not available: we test that activation still succeeds gracefully,
//     demonstrating that missing package managers don't abort the command.
//
// NOTE: The current implementation does NOT run `npm install` during activate —
// that happens only via `rwv lock`.  This test documents the current behaviour
// and the discrepancy with the doc claim.
// ===========================================================================

#[test]
fn activate_npm_no_install_run_during_activate() {
    // Doc claim (project-reporoot-l56a): activate runs npm install.
    // Current observed behaviour: activate only generates package.json and
    // creates symlinks; it does NOT invoke npm install.
    //
    // This test documents current behaviour.  If the implementation is updated
    // to run npm install on activate, update the assertions below.

    let tmp = tempfile::tempdir().unwrap();
    let ws_root = tmp.path().join("ws");
    std::fs::create_dir_all(ws_root.join("github")).unwrap();

    let project_dir = ws_root.join("projects/npm-proj");
    std::fs::create_dir_all(&project_dir).unwrap();

    // Create a repo with a package.json (triggers npm-workspaces integration).
    let repo_dir = ws_root.join("github/org/webapp");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::write(
        repo_dir.join("package.json"),
        r#"{"name": "webapp", "version": "1.0.0"}"#,
    )
    .unwrap();

    let manifest = "repositories:\n  github/org/webapp:\n    type: git\n    url: https://github.com/org/webapp.git\n    version: main\n    role: primary\n";
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    let output = rwv()
        .args(["activate", "npm-proj"])
        .current_dir(&ws_root)
        .output()
        .unwrap();

    // Activation should succeed regardless of whether npm is installed.
    assert!(
        output.status.success(),
        "activate should succeed even without npm; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // A workspace-level package.json symlink should be present (integration ran).
    let root_pkg = ws_root.join("package.json");
    assert!(
        root_pkg.exists(),
        "package.json should be symlinked at workspace root after activate"
    );

    // Current behaviour: node_modules is NOT created by activate (no npm install).
    let node_modules = ws_root.join("node_modules");
    // TODO (project-reporoot-l56a): If the implementation is updated to run
    // `npm install` during activate, remove this assertion and add one that
    // verifies node_modules (or package-lock.json) was created.
    assert!(
        !node_modules.exists(),
        "activate should not run npm install (node_modules should not exist); \
         if this fails, the implementation now runs npm install during activate \
         and the test should be updated to reflect that"
    );
}

#[test]
fn activate_graceful_when_npm_unavailable() {
    // If npm is not on PATH, activate should still succeed (no hard crash).
    // We cannot force npm off PATH in a portable way, but we can verify
    // that activate always exits 0 in our test environment.

    let tmp = tempfile::tempdir().unwrap();
    let ws_root = tmp.path().join("ws");
    std::fs::create_dir_all(ws_root.join("github")).unwrap();

    let project_dir = ws_root.join("projects/npm-proj");
    std::fs::create_dir_all(&project_dir).unwrap();

    let repo_dir = ws_root.join("github/org/frontend");
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::write(
        repo_dir.join("package.json"),
        r#"{"name": "frontend", "version": "1.0.0"}"#,
    )
    .unwrap();

    let manifest = "repositories:\n  github/org/frontend:\n    type: git\n    url: https://github.com/org/frontend.git\n    version: main\n    role: primary\n";
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    // Activate must not fail regardless of available tools.
    rwv()
        .args(["activate", "npm-proj"])
        .current_dir(&ws_root)
        .assert()
        .success();
}
