//! Integration tests for documentation claims about `rwv activate`, `rwv doctor`,
//! and the `static-files` integration.
//!
//! Tests are keyed to their spec/claim IDs:
//!   - project-reporoot-201  workspace context from project dir
//!   - project-reporoot-85h9 check: missing role field, workweave drift
//!   - project-reporoot-c3ad activate symlinks ecosystem + lock files
//!   - project-reporoot-1ejx static-files integration
//!   - project-reporoot-l56a activate runs install commands

use assert_cmd::Command;
use std::path::{Path, PathBuf};
use std::process;

mod common;

// ===========================================================================
// Shared helpers
// ===========================================================================

/// Build a `Command` for the `rwv` binary.
fn rwv() -> Command {
    common::rwv()
}

/// Run a git command in `dir`, panicking on failure.
fn git(args: &[&str], dir: &Path) {
    let status = common::git()
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
///   {parent}/ws/projects/{project}/   — project dir with rwv.toml
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
        "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nversion = \"main\"\nrole = \"owned\"\n",
        repo = common::url_path(&repo_path)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    (ws, repo_path)
}

/// Create a minimal workspace with no real git repo — just the directory
/// structure and an rwv.toml.  Useful for tests that exercise parsing/check
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
    let tmp = common::tempdir().unwrap();
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

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    // Canonicalize the expected path so /var vs /private/var symlinks (macOS)
    // don't cause spurious mismatches.
    let ws_canonical = std::fs::canonicalize(&ws).unwrap();
    assert_eq!(
        stdout,
        common::operator_path_stdout(&ws_canonical),
        "rwv resolve should print the workspace root, not the project subdir"
    );
}

#[test]
fn workspace_context_from_project_dir_no_subcommand() {
    // Doc claim: `rwv` (no subcommand) from projects/<project>/ shows the
    // correct project name in its output.
    let tmp = common::tempdir().unwrap();
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
// Doc claim: `rwv doctor` reports entries without a `role` field.
//
// Corrected: `role` has no serde default, so a role-less entry is not a
// finding ABOUT an entry — the manifest does not parse at all, and doctor
// reports the project. The claim named the trigger and got the shape wrong.
// ===========================================================================

/// A repo entry with no `role` makes `rwv doctor` report the whole project
/// unparseable, naming the field serde rejected.
///
/// Both halves are the pin. The kind is what a consumer branches on, and
/// every malformed manifest produces it — so the kind alone would be
/// satisfied by a stray brace. The message is the only place the operator
/// learns which field is missing, which is the claim being anchored.
#[test]
fn check_missing_role_field() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_no_repo(tmp.path(), "my-project");

    let bad_manifest = r#"[repositories."github/org/repo"]
type = "git"
url = "https://github.com/org/repo.git"
version = "main"
"#;
    std::fs::write(ws.join("projects/my-project/rwv.toml"), bad_manifest).unwrap();

    let output = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "a manifest rwv cannot parse is a doctor failure, not a pass"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{stdout}"));
    let violations = report["violations"]
        .as_array()
        .expect("report carries violations");

    assert_eq!(
        violations.len(),
        1,
        "an unparseable manifest stops the walk at the project, so nothing \
         downstream of it is reported:\n{stdout}"
    );
    assert_eq!(violations[0]["kind"], "unparseable-project");
    assert_eq!(violations[0]["project"], "my-project");

    let message = violations[0]["message"]
        .as_str()
        .expect("the violation carries a message");
    assert!(
        message.contains("missing field `role`"),
        "the operator's only route to the offending field is this message; \
         got: {message}"
    );
}

// ===========================================================================
// 3. check_workweave_drift — extra worktree (project-reporoot-85h9)
//
// A git repo directory lives inside the workspace that is not referenced by
// any project's rwv.toml.  `rwv doctor` reports it as an orphaned clone.
// ===========================================================================

/// A git repo on disk that no manifest names is reported as
/// `orphaned-clone`, and the repo that IS named is not.
///
/// Naming the orphan is half the pin; the count is the other half. A check
/// that reported every clone in the workspace would satisfy an assertion
/// that only looked for `extra-repo`, and would be worthless — the finding
/// means "this one is unreferenced", which is a statement about the ones it
/// leaves out.
#[test]
fn check_workweave_drift_extra_repo() {
    let tmp = common::tempdir().unwrap();
    let (ws, _) = make_workspace_with_git_repo(tmp.path(), "my-project");

    let extra_repo = ws.join("github/org/extra-repo");
    init_repo_with_commit(&extra_repo);

    let output = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "an unreferenced clone is a violation, so doctor exits non-zero"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{stdout}"));

    let orphans: Vec<&serde_json::Value> = report["violations"]
        .as_array()
        .expect("report carries violations")
        .iter()
        .filter(|v| v["kind"] == "orphaned-clone")
        .collect();

    assert_eq!(
        orphans.len(),
        1,
        "one clone is unreferenced and one is named by the manifest:\n{stdout}"
    );
    assert_eq!(orphans[0]["path"], "github/org/extra-repo");
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
    let tmp = common::tempdir().unwrap();
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

    let manifest = "[repositories.\"github/org/mylib\"]\ntype = \"git\"\nurl = \"https://github.com/org/mylib.git\"\nversion = \"main\"\nrole = \"owned\"\n";
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    // Under the trigger-model split, `rwv activate` is a context
    // verb — it surfaces existing content but does not author. Drive the
    // intent path first so the project_dir/Cargo.toml exists for the
    // context-mode activate to surface. (Mirrors what `rwv add` does in
    // a real workflow.) Use the no-materialize variant to skip
    // `cargo generate-lockfile` (the CLI step is gated by --no-materialize).
    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(&ws_root, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "cargo-proj",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent-mode activation should author Cargo.toml in project dir");

    rwv()
        .args(["activate", "cargo-proj", "--no-materialize"])
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
        target
            .ancestors()
            .any(|a| a.ends_with("projects/cargo-proj")),
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
            .ancestors()
            .any(|a| a.ends_with("projects/cargo-proj")),
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_no_repo(tmp.path(), "my-project");

    let project_dir = ws.join("projects/my-project");

    // Create only one of the two declared files.
    std::fs::write(project_dir.join("exists.txt"), "present").unwrap();
    // missing.txt is intentionally NOT created.

    let manifest = r#"[repositories]

[integrations.static-files]
enabled = true
files = ["exists.txt", "missing.txt"]
"#;
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    let output = rwv()
        .args(["activate", "my-project", "--no-materialize"])
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
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_no_repo(tmp.path(), "my-project");

    let project_dir = ws.join("projects/my-project");

    // Create the file that will be symlinked.
    std::fs::write(project_dir.join("turbo.json"), r#"{"$schema": "..."}"#).unwrap();

    let manifest = r#"[repositories]

[integrations.static-files]
enabled = true
files = ["turbo.json"]
"#;
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    rwv()
        .args(["activate", "my-project", "--no-materialize"])
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
    assert!(
        target.ends_with("projects/my-project/turbo.json"),
        "turbo.json symlink should point to projects/my-project/turbo.json, got: {}",
        target.display()
    );

    // Reading through the symlink should give the original content.
    let content = std::fs::read_to_string(&link).unwrap();
    assert!(
        content.contains("$schema"),
        "symlinked turbo.json should have the original content"
    );
}

// ===========================================================================
// static-files / workweave.link collision regression
//
// When the same name appears in BOTH `integrations.static-files.files` AND
// `workweave.link`, the operator gets two layers of protection:
//   1. `static-files.check()` emits Severity::Error so `rwv doctor` and
//      Context-mode activate surface the conflict pre-symlink.
//   2. `static-files.activate()` itself bails with the same message in
//      Intent-mode entry paths (where the framework runs check-then-bail).
//
// Pre-fix: the workweave.link entry was silently nuked by
// remove_activation_symlinks during workweave creation and replaced with a
// relative symlink to the project's own (partial) checkout.
//
// Post-fix: activation fails loud with a message naming both integrations.
// ===========================================================================

#[test]
fn static_files_collides_with_workweave_link_errors_loud() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_no_repo(tmp.path(), "my-project");

    let project_dir = ws.join("projects/my-project");

    // .beads is the shipped repro: a static file the user
    // wanted shared via workweave.link, mistakenly also listed in
    // static-files.files. Materialize the file so the missing-file warning
    // doesn't muddle the assertion.
    std::fs::write(project_dir.join(".beads"), "primary").unwrap();

    let manifest = r#"[repositories]

[integrations.static-files]
enabled = true
files = [".beads"]

[workweave]
link = [".beads"]
"#;
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    // Intent-mode entry — `rwv activate --intent` would gate through
    // run_activations which calls integration.activate() and bails on
    // Severity::Error. We exercise the surface via `rwv doctor`, which runs
    // checks against every project's manifest and reports per-integration
    // issues. The collision must surface as a Severity::Error there.
    let output = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let combined = format!("{stdout}\n{stderr}");

    assert!(
        combined.contains(".beads")
            && combined.contains("static-files")
            && combined.contains("workweave"),
        "rwv doctor should surface the static-files/workweave.link collision; \
         got stdout: {stdout}\nstderr: {stderr}"
    );
}

// ===========================================================================
// 7. activate_runs_install_commands (project-reporoot-l56a)
//
// Doc claim: `rwv activate` runs ecosystem install commands (npm install,
// uv sync, etc.) after generating workspace config files.
//
// The claim holds, and `--no-materialize` is the documented suppression.
// Both are driven here against a recording `npm` on the child's PATH, so
// what is measured is what rwv spawned rather than what this host installs.
// ===========================================================================

/// Build an npm-flavoured workspace at `ws_root` and pre-author its managed
/// files, leaving a context-mode `rwv activate` something to surface.
fn make_npm_workspace(ws_root: &Path, repo: &str) {
    std::fs::create_dir_all(ws_root.join("github")).unwrap();

    let project_dir = ws_root.join("projects/npm-proj");
    std::fs::create_dir_all(&project_dir).unwrap();

    let repo_dir = ws_root.join("github/org").join(repo);
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::write(
        repo_dir.join("package.json"),
        format!(r#"{{"name": "{repo}", "version": "1.0.0"}}"#),
    )
    .unwrap();

    let manifest = format!(
        "[repositories.\"github/org/{repo}\"]\ntype = \"git\"\nurl = \"https://github.com/org/{repo}.git\"\nversion = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    let ctx = repoweave::workspace::WorkspaceContext::resolve_invocation(ws_root, None).unwrap();
    repoweave::activate::activate_intent_with_options(
        "npm-proj",
        &ctx,
        repoweave::activate::ActivateOptions {
            no_materialize: true,
        },
    )
    .expect("intent-mode activation should author package.json in project dir");
}

/// Write an `npm` into `bin_dir` that appends its argv to `log` and exits 0.
///
/// Unix only: what the shim has to be is a file the spawned child resolves on
/// PATH and executes itself. On Windows an extensionless `#!` script is not a
/// candidate — lookup selects on `PATHEXT` — so porting needs a Windows
/// spelling for the shim, which is the same condition holding
/// `write_exit_code_shim` in `tests/integrations_test.rs` to unix.
#[cfg(unix)]
fn write_recording_npm(bin_dir: &Path, log: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin_dir).unwrap();
    let path = bin_dir.join("npm");
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho \"$@\" >> {}\nexit 0\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// `rwv activate` spawns the ecosystem install command; `--no-materialize`
/// withholds it.
///
/// The suppressed run and the plain run differ only in the flag, and that is
/// what makes the absence readable. On its own, "no install ran" is equally
/// green on a host without npm, on a fixture the integration never detected,
/// and on an activate that spawns nothing at all — the second run is the
/// control that separates those from the flag doing its job.
#[cfg(unix)]
#[test]
fn activate_runs_the_install_hook_and_no_materialize_withholds_it() {
    let tmp = common::tempdir().unwrap();
    let ws_root = tmp.path().join("ws");
    make_npm_workspace(&ws_root, "webapp");

    let bin_dir = tmp.path().join("bin");
    let log = tmp.path().join("npm-argv.log");
    write_recording_npm(&bin_dir, &log);
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    rwv()
        .args(["activate", "npm-proj", "--no-materialize"])
        .env("PATH", &path)
        .current_dir(&ws_root)
        .assert()
        .success();

    assert!(
        !log.exists(),
        "--no-materialize must withhold the install command; npm ran with: {}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    assert!(
        ws_root.join("package.json").exists(),
        "the flag withholds the install hook, not the surfacing symlink"
    );

    rwv()
        .args(["activate", "npm-proj"])
        .env("PATH", &path)
        .current_dir(&ws_root)
        .assert()
        .success();

    let argv = std::fs::read_to_string(&log)
        .expect("without the flag, activate must run the install command");
    assert_eq!(argv.trim(), "install", "activate runs `npm install`");
}

/// When the install command is not on PATH, activate refuses: it names the
/// missing tool, exits non-zero, and leaves `.rwv-active` unwritten.
///
/// The precondition is forced by handing the child a PATH with nothing on
/// it, because a test that merely hopes npm is absent measures the host. The
/// withheld `.rwv-active` is the half worth having: an exit code says the run
/// failed, and this says the workspace was not left pointing at a project
/// whose tooling never installed.
#[cfg(unix)]
#[test]
fn activate_refuses_when_the_install_command_is_missing() {
    let tmp = common::tempdir().unwrap();
    let ws_root = tmp.path().join("ws");
    make_npm_workspace(&ws_root, "frontend");

    let empty_bin = tmp.path().join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();

    let output = rwv()
        .args(["activate", "npm-proj"])
        .env("PATH", &empty_bin)
        .current_dir(&ws_root)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "a failed install hook is an activate failure, not a warning"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("npm is not on PATH"),
        "the operator is owed the name of the tool that is missing; got:\n{stderr}"
    );
    assert!(
        stderr.contains("activate hook failed"),
        "the refusal names the stage that failed; got:\n{stderr}"
    );
    assert!(
        !ws_root.join(".rwv-active").exists(),
        ".rwv-active must not be written when an install hook fails"
    );
}
