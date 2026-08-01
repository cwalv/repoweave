//! Integration tests verifying the copy-pasteable recipes in
//! `docs/adjacent-tools.md` ("CI multi-repo checkout" and "Devcontainers /
//! Codespaces") actually run.
//!
//! Both recipes land the *project* repo (the one carrying `rwv.toml`) under
//! `projects/<name>/` — via `actions/checkout@v4`'s `path:` in CI, via
//! `workspaceMount`/`workspaceFolder` in a devcontainer — then, from the
//! parent directory:
//!   1. `rwv activate <name> --no-install` — sets `.rwv-active`; no manifest
//!      repos need to be on disk yet.
//!   2. `rwv fetch --frozen` (in-place, no SOURCE) — materializes the
//!      manifest's repos at the revisions `rwv.lock` pins.
//!
//! The shape that originally shipped skipped the nesting and ran SOURCE-mode
//! `rwv fetch <source>` afterward; `shipped_recipe_without_nesting_fails`
//! pins the failure that motivated this fix.

use assert_cmd::Command;
use std::path::Path;
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
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

fn git_stdout(args: &[&str], dir: &Path) -> String {
    let output = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should be available");
    assert!(
        output.status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Create a bare repo at `path`, seeded with `files`, and return the SHA of
/// the seeding commit.
fn init_bare_repo_with_commit(path: &Path, files: &[(&str, &str)]) -> String {
    let status = common::git()
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(path)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git init --bare failed");

    let tmp = common::tempdir().expect("tempdir for working clone");
    let work = tmp.path().join("work");
    git(
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
    git(&["config", "user.email", "test@test.com"], &work);
    git(&["config", "user.name", "Test"], &work);
    for (name, contents) in files {
        std::fs::write(work.join(name), contents).unwrap();
    }
    git(&["add", "."], &work);
    git(&["commit", "-m", "initial"], &work);
    git(&["push", "origin", "main"], &work);

    git_stdout(&["rev-parse", "HEAD"], &work)
}

/// Build the fixtures shared by both recipe tests: a bare "dependency" repo
/// and a bare "project" repo (`web-app`) carrying `rwv.toml` (referencing
/// the dependency) and `rwv.lock` (pinning it to the dependency's HEAD SHA).
///
/// `<tmp>/ws/projects/web-app` is a real git checkout of the project repo —
/// standing in for `actions/checkout@v4`'s `path: projects/web-app` / a
/// devcontainer's `workspaceMount` into the same location. Nothing else
/// exists under `<tmp>/ws`: no `.rwv-active`, no sibling repos, matching a
/// fresh CI runner or a fresh devcontainer build.
///
/// Returns `(workspace_root, dependency_sha)`.
fn setup_ci_shaped_workspace(tmp: &Path) -> (std::path::PathBuf, String) {
    let dep_bare = tmp.join("dep.git");
    let dep_sha = init_bare_repo_with_commit(&dep_bare, &[("README", "dep\n")]);
    let dep_url = format!("file://{}", dep_bare.display());

    let project_bare = tmp.join("web-app.git");
    let manifest_toml = format!(
        "[repositories.\"local/org/dep\"]\ntype = \"git\"\nurl = \"{dep_url}\"\nversion = \"main\"\nrole = \"owned\"\n"
    );
    // Round-trips through the real parser + serializer so this fixture is
    // byte-identical to what `rwv lock` itself would write for the same
    // content, not merely equivalent YAML-vs-JSON.
    let raw_lock = format!(
        "{{\"repositories\": {{\"local/org/dep\": {{\"type\": \"git\", \"url\": {dep_url:?}, \"version\": {dep_sha:?}}}}}}}"
    );
    let mut lock = serde_json::to_string_pretty(
        &repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap(),
    )
    .unwrap();
    lock.push('\n');
    init_bare_repo_with_commit(&project_bare, &[("rwv.toml", &manifest_toml), ("rwv.lock", &lock)]);

    let workspace = tmp.join("ws");
    std::fs::create_dir_all(workspace.join("projects")).unwrap();
    git(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &workspace.join("projects/web-app").to_string_lossy(),
        ],
        tmp,
    );

    (workspace, dep_sha)
}

// ============================================================================
// Doc claim — docs/adjacent-tools.md "CI multi-repo checkout": checkout with
// `path: projects/web-app`, then `rwv activate web-app --no-install` and
// `rwv fetch --frozen` run from the parent both succeed, and the manifest's
// repo is materialized at the revision the lock pins.
// ============================================================================

#[test]
fn ci_recipe_activate_then_frozen_fetch_succeeds() {
    let tmp = common::tempdir().unwrap();
    let (workspace, dep_sha) = setup_ci_shaped_workspace(tmp.path());

    rwv()
        .args(["activate", "web-app", "--no-install"])
        .current_dir(&workspace)
        .assert()
        .success();

    assert_eq!(
        std::fs::read_to_string(workspace.join(".rwv-active"))
            .expect(".rwv-active should exist after activate")
            .trim(),
        "web-app",
    );

    rwv()
        .args(["fetch", "--frozen"])
        .current_dir(&workspace)
        .assert()
        .success();

    let dep_dir = workspace.join("local/org/dep");
    assert!(
        dep_dir.join("README").exists(),
        "in-place fetch should materialize the manifest's repo"
    );
    assert_eq!(
        git_stdout(&["rev-parse", "HEAD"], &dep_dir),
        dep_sha,
        "in-place fetch should check out the locked revision"
    );
}

// ============================================================================
// Doc claim — docs/adjacent-tools.md "Devcontainers / Codespaces": the
// `postCreateCommand` chains `rwv activate` and `rwv fetch --frozen` in one
// shell command from the parent directory. Exercise the literal chained
// form (not just the two `rwv` invocations run separately).
// ============================================================================

#[test]
fn devcontainer_recipe_chained_shell_command_succeeds() {
    let tmp = common::tempdir().unwrap();
    let (workspace, dep_sha) = setup_ci_shaped_workspace(tmp.path());

    let rwv_bin = env!("CARGO_BIN_EXE_rwv");
    let status = process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "'{rwv_bin}' activate web-app --no-install && '{rwv_bin}' fetch --frozen"
        ))
        .current_dir(&workspace)
        .status()
        .expect("shell should run");

    assert!(
        status.success(),
        "chained postCreateCommand form should succeed"
    );

    let dep_dir = workspace.join("local/org/dep");
    assert_eq!(git_stdout(&["rev-parse", "HEAD"], &dep_dir), dep_sha);
}

// ============================================================================
// Doc claim — the recipe that originally shipped: checkout with no `path:`
// (the runner directory IS the project repo), then SOURCE-mode
// `rwv fetch <source>`. Pins the failure that motivated nesting under
// `projects/<name>/` above.
// ============================================================================

#[test]
fn shipped_recipe_without_nesting_fails() {
    let tmp = common::tempdir().unwrap();
    let dep_bare = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&dep_bare, &[("README", "dep\n")]);
    let dep_url = format!("file://{}", dep_bare.display());

    let project_bare = tmp.path().join("web-app.git");
    let manifest_toml = format!(
        "[repositories.\"local/org/dep\"]\ntype = \"git\"\nurl = \"{dep_url}\"\nversion = \"main\"\nrole = \"owned\"\n"
    );
    init_bare_repo_with_commit(&project_bare, &[("rwv.toml", &manifest_toml)]);

    // actions/checkout@v4 with no `path:` — the checkout IS the runner
    // directory, not a workspace, and not empty (it has a `.git`).
    let workspace = tmp.path().join("ws");
    git(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &workspace.to_string_lossy(),
        ],
        tmp.path(),
    );

    let output = rwv()
        .args(["fetch", "chatly/web-app"])
        .current_dir(&workspace)
        .output()
        .expect("rwv fetch should run");

    assert!(
        !output.status.success(),
        "unnested checkout + SOURCE-mode fetch should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is not empty"),
        "expected the require_workspace_or_empty bail, got:\n{stderr}"
    );
}
