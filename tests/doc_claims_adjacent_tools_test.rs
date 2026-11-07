//! Integration tests verifying the copy-pasteable recipes in
//! `docs/adjacent-tools.md` ("CI multi-repo checkout" and "Devcontainers /
//! Codespaces") actually run.
//!
//! Both recipes land the *project* repo (the one carrying `rwv.toml`) under
//! `projects/<name>/` — via `actions/checkout@v4`'s `path:` in CI, via
//! `workspaceMount`/`workspaceFolder` in a devcontainer — then run
//! `rwv update` from the parent directory. With that project the weave's only
//! one and none active, the update materializes the manifest's repos at the
//! revisions `rwv.lock` pins and then activates the project.
//!
//! The shape that originally shipped skipped the nesting and ran
//! `rwv clone <source>` afterward; `shipped_recipe_without_nesting_fails`
//! pins the failure that motivated this fix. A job that must fail on a member
//! the lock does not pin runs `rwv clone <source>` in an empty directory
//! instead, and exits `3`.

use assert_cmd::Command;
use std::path::Path;
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
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
    common::git_in(
        tmp.path(),
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
    );
    common::git_in(&work, &["config", "user.email", "test@test.com"]);
    common::git_in(&work, &["config", "user.name", "Test"]);
    for (name, contents) in files {
        std::fs::write(work.join(name), contents).unwrap();
    }
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "initial"]);
    common::git_in(&work, &["push", "origin", "main"]);

    common::git_in(&work, &["rev-parse", "HEAD"])
}

/// What the project repo's `rwv.lock` holds for the dependency.
enum DepPin {
    AtHead,
    Absent,
    Unreachable,
}

const UNREACHABLE_PIN: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

/// Build a bare "dependency" repo and a bare "project" repo (`web-app`)
/// carrying `rwv.toml` (referencing the dependency) and `rwv.lock`, pinning
/// the dependency as `pin` says.
///
/// Returns `(project_bare, dependency_sha)`.
fn publish_ci_shaped_project(tmp: &Path, pin: DepPin) -> (std::path::PathBuf, String) {
    let dep_bare = tmp.join("dep.git");
    let dep_sha = init_bare_repo_with_commit(&dep_bare, &[("README", "dep\n")]);
    let dep_url = common::file_url(&dep_bare);

    let project_bare = tmp.join("web-app.git");
    let manifest_toml = format!(
        "[repositories.\"local/org/dep\"]\ntype = \"git\"\nurl = \"{dep_url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
    );
    let lock = match pin {
        DepPin::AtHead => {
            common::fixture_lock_bytes(&[("local/org/dep", dep_url.as_str(), dep_sha.as_str())])
        }
        DepPin::Absent => common::fixture_lock_bytes(&[]),
        DepPin::Unreachable => {
            common::fixture_lock_bytes(&[("local/org/dep", dep_url.as_str(), UNREACHABLE_PIN)])
        }
    };
    init_bare_repo_with_commit(
        &project_bare,
        &[("rwv.toml", &manifest_toml), ("rwv.lock", &lock)],
    );
    (project_bare, dep_sha)
}

/// Build the fixtures shared by the `rwv update` recipe tests: the published
/// project of [`publish_ci_shaped_project`] with the dependency pinned at its
/// HEAD.
///
/// `<tmp>/ws/projects/web-app` is a real git checkout of the project repo —
/// standing in for `actions/checkout@v4`'s `path: projects/web-app` / a
/// devcontainer's `workspaceMount` into the same location. Nothing else
/// exists under `<tmp>/ws`: no `.rwv-active`, no sibling repos, matching a
/// fresh CI runner or a fresh devcontainer build.
///
/// Returns `(workspace_root, dependency_sha)`.
fn setup_ci_shaped_workspace(tmp: &Path, pin: DepPin) -> (std::path::PathBuf, String) {
    let (project_bare, dep_sha) = publish_ci_shaped_project(tmp, pin);

    let workspace = tmp.join("ws");
    std::fs::create_dir_all(workspace.join("projects")).unwrap();
    common::git_in(
        tmp,
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &workspace.join("projects/web-app").to_string_lossy(),
        ],
    );

    (workspace, dep_sha)
}

// ============================================================================
// Doc claim — docs/adjacent-tools.md "CI multi-repo checkout": checkout with
// `path: projects/web-app`, then `rwv update` from the parent materializes
// the manifest's repo at the locked revision and activates the project.
// ============================================================================

#[test]
fn ci_recipe_update_materializes_and_activates() {
    let tmp = common::tempdir().unwrap();
    let (workspace, dep_sha) = setup_ci_shaped_workspace(tmp.path(), DepPin::AtHead);

    rwv()
        .args(["update"])
        .current_dir(&workspace)
        .assert()
        .success();

    let dep_dir = workspace.join("local/org/dep");
    assert!(
        dep_dir.join("README").exists(),
        "update should materialize the manifest's repo"
    );
    assert_eq!(
        common::git_in(&dep_dir, &["rev-parse", "HEAD"]),
        dep_sha,
        "update should check out the locked revision"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join(".rwv-active"))
            .expect("the update should activate the weave's only project")
            .trim(),
        "web-app",
    );
}

// ============================================================================
// Doc claim — docs/adjacent-tools.md "Devcontainers / Codespaces": the
// `postCreateCommand` runs from `workspaceFolder` (the project repo) and
// chains `cd` to the weave root with `rwv update`. Exercise the literal
// chained form.
// ============================================================================

#[test]
fn devcontainer_recipe_chained_shell_command_succeeds() {
    let tmp = common::tempdir().unwrap();
    let (workspace, dep_sha) = setup_ci_shaped_workspace(tmp.path(), DepPin::AtHead);

    let rwv_bin = env!("CARGO_BIN_EXE_rwv");
    let status = process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "cd '{}' && '{rwv_bin}' update",
            workspace.display()
        ))
        .current_dir(workspace.join("projects/web-app"))
        .status()
        .expect("shell should run");

    assert!(
        status.success(),
        "chained postCreateCommand form should succeed"
    );

    let dep_dir = workspace.join("local/org/dep");
    assert_eq!(common::git_in(&dep_dir, &["rev-parse", "HEAD"]), dep_sha);
    assert_eq!(
        std::fs::read_to_string(workspace.join(".rwv-active"))
            .expect("the update should activate the weave's only project")
            .trim(),
        "web-app",
    );
}

// ============================================================================
// Doc claim — the recipe that originally shipped: checkout with no `path:`
// (the runner directory IS the project repo), then `rwv clone <source>`. Pins
// the failure that motivated nesting under `projects/<name>/` above.
// ============================================================================

#[test]
fn shipped_recipe_without_nesting_fails() {
    let tmp = common::tempdir().unwrap();
    let dep_bare = tmp.path().join("dep.git");
    init_bare_repo_with_commit(&dep_bare, &[("README", "dep\n")]);
    let dep_url = common::file_url(&dep_bare);

    let project_bare = tmp.path().join("web-app.git");
    let manifest_toml = format!(
        "[repositories.\"local/org/dep\"]\ntype = \"git\"\nurl = \"{dep_url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
    );
    init_bare_repo_with_commit(&project_bare, &[("rwv.toml", &manifest_toml)]);

    // actions/checkout@v4 with no `path:` — the checkout IS the runner
    // directory, not a workspace, and not empty (it has a `.git`).
    let workspace = tmp.path().join("ws");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &workspace.to_string_lossy(),
        ],
    );

    let output = rwv()
        .args(["clone", "chatly/web-app"])
        .current_dir(&workspace)
        .output()
        .expect("rwv clone should run");

    assert!(
        !output.status.success(),
        "unnested checkout + rwv clone should fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("is not empty"),
        "expected the require_workspace_or_empty bail, got:\n{stderr}"
    );
}

// ============================================================================
// Doc claim — docs/adjacent-tools.md "CI multi-repo checkout": `rwv update`
// writes no lock and leaves a member the lock has no entry for at its branch
// tip.
// ============================================================================

#[test]
fn ci_recipe_update_leaves_an_unpinned_member_at_its_branch_tip_and_writes_no_lock() {
    let tmp = common::tempdir().unwrap();
    let (workspace, dep_sha) = setup_ci_shaped_workspace(tmp.path(), DepPin::Absent);
    let project_dir = workspace.join("projects/web-app");
    let lock_before = std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap();

    rwv()
        .args(["update"])
        .current_dir(&workspace)
        .assert()
        .success();

    let dep_dir = workspace.join("local/org/dep");
    assert_eq!(common::git_in(&dep_dir, &["rev-parse", "HEAD"]), dep_sha);
    assert_eq!(
        common::git_in(&dep_dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "main",
        "an unpinned member is cloned on its branch, not detached"
    );
    assert_eq!(
        std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap(),
        lock_before,
        "update must not write the lock"
    );
}

// ============================================================================
// Doc claim — docs/adjacent-tools.md "CI multi-repo checkout": a job that must
// fail on a member the lock does not pin runs `rwv clone <source>` in an
// empty directory, which exits `3` when a member is unpinned or its pin is
// unreachable.
// ============================================================================

fn clone_in_an_empty_directory(pin: DepPin) -> process::Output {
    let tmp = common::tempdir().unwrap();
    let (project_bare, _) = publish_ci_shaped_project(tmp.path(), pin);
    let runner = tmp.path().join("runner");
    std::fs::create_dir_all(&runner).unwrap();

    rwv()
        .args(["clone", &common::file_url(&project_bare)])
        .current_dir(&runner)
        .output()
        .expect("rwv clone should run")
}

#[test]
fn ci_clone_in_an_empty_directory_exits_3_when_a_member_is_unpinned() {
    let output = clone_in_an_empty_directory(DepPin::Absent);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(3), "{stdout}");
    assert!(stdout.contains("local/org/dep: pin none"), "{stdout}");
}

#[test]
fn ci_clone_in_an_empty_directory_exits_3_when_a_pin_is_unreachable() {
    let output = clone_in_an_empty_directory(DepPin::Unreachable);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(output.status.code(), Some(3), "{stdout}");
    assert!(
        stdout.contains("local/org/dep: pin unreachable"),
        "{stdout}"
    );
}

#[test]
fn ci_clone_in_an_empty_directory_exits_0_when_every_member_sits_at_its_pin() {
    let output = clone_in_an_empty_directory(DepPin::AtHead);

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
