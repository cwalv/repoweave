//! E2E tests for `rwv clone`.
//!
//! Each test drives the shipped binary against local bare repositories reached
//! by `file://` URLs.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn init_bare_repo(path: &Path) {
    common::git_in(
        path.parent().expect("a bare repo path needs a parent"),
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            &path.to_string_lossy(),
        ],
    );
}

/// A bare remote at `<tmp>/<name>.git` whose `main` holds one commit, and its URL.
fn member_remote(tmp: &Path, name: &str) -> (PathBuf, String) {
    let bare = tmp.join(format!("{name}.git"));
    common::init_bare_repo_with_commit(&bare);
    let url = common::file_url(&bare);
    (bare, url)
}

/// Push one more commit onto `branch` of `bare`, creating the branch from
/// `main` when it does not exist, and return the new tip.
fn push_commit(bare: &Path, branch: &str) -> String {
    let scratch = common::tempdir().expect("tempdir for the pushing clone");
    let work = scratch.path().join("work");
    common::git_in(
        scratch.path(),
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
    );
    common::git_in(&work, &["checkout", "-B", branch]);
    std::fs::write(work.join(format!("{branch}.txt")), branch).unwrap();
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "advance"]);
    common::git_in(&work, &["push", "origin", branch]);
    common::git_in(&work, &["rev-parse", "HEAD"])
}

/// A commit on top of `branch` of `bare` that no remote holds: the scratch
/// clone that made it is gone when this returns.
fn unpublished_commit(bare: &Path, branch: &str) -> String {
    let scratch = common::tempdir().expect("tempdir for the unpublished commit");
    let work = scratch.path().join("work");
    common::git_in(
        scratch.path(),
        &[
            "clone",
            "--branch",
            branch,
            &bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
    );
    std::fs::write(work.join("unpublished.txt"), "unpublished").unwrap();
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "unpublished"]);
    common::git_in(&work, &["rev-parse", "HEAD"])
}

struct Member<'a> {
    path: &'a str,
    url: &'a str,
    branch: &'a str,
    role: &'a str,
}

impl<'a> Member<'a> {
    fn owned(path: &'a str, url: &'a str) -> Self {
        Self {
            path,
            url,
            branch: "main",
            role: "owned",
        }
    }

    fn role(self, role: &'a str) -> Self {
        Self { role, ..self }
    }

    fn on(self, branch: &'a str) -> Self {
        Self { branch, ..self }
    }
}

/// What `rwv.lock` the published project carries.
enum Lock<'a> {
    Absent,
    /// Every member pinned at the tip its remote's `main` has when the project
    /// is published.
    AtRemoteTips,
    /// Exactly these `(path, url, version)` entries.
    Pins(&'a [(&'a str, &'a str, &'a str)]),
}

struct Source {
    bare: PathBuf,
    url: String,
}

/// A bare project repository at `<tmp>/<name>.git` whose only commit holds
/// `rwv.toml` for `members` and the `lock` it is given.
fn publish_project(tmp: &Path, name: &str, members: &[Member<'_>], lock: Lock<'_>) -> Source {
    let bare = tmp.join(format!("{name}.git"));
    init_bare_repo(&bare);
    let work = tmp.join(format!("{name}-work"));
    common::git_in(
        tmp,
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
    );

    let mut manifest = String::from("[repositories]\n");
    for m in members {
        manifest.push_str(&format!(
            "[repositories.\"{}\"]\ntype = \"git\"\nurl = \"{}\"\nbranch = \"{}\"\nrole = \"{}\"\n",
            m.path, m.url, m.branch, m.role
        ));
    }
    std::fs::write(work.join("rwv.toml"), manifest).unwrap();
    match lock {
        Lock::Absent => {}
        Lock::AtRemoteTips => {
            let remotes: Vec<(&str, &str)> = members.iter().map(|m| (m.path, m.url)).collect();
            common::fixture_lock_at_remote_tips(&work, &remotes);
        }
        Lock::Pins(pins) => common::fixture_lock(&work, pins),
    }
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "project"]);
    common::git_in(&work, &["push", "origin", "main"]);

    let url = common::file_url(&bare);
    Source { bare, url }
}

fn empty_workspace(tmp: &Path, name: &str) -> PathBuf {
    let workspace = tmp.join(name);
    std::fs::create_dir_all(&workspace).unwrap();
    workspace
}

fn rev(repo: &Path) -> String {
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// The `rwv.lock` committed on `main` of the project remote `bare`, line
/// endings normalized.
fn committed_lock(bare: &Path) -> String {
    let out = common::git()
        .args(["show", "main:rwv.lock"])
        .current_dir(bare)
        .output()
        .expect("git show runs");
    assert!(out.status.success(), "the project remote carries no lock");
    String::from_utf8(out.stdout)
        .expect("a lock is UTF-8")
        .replace("\r\n", "\n")
}

fn tip(bare: &Path, branch: &str) -> String {
    common::git_in(bare, &["rev-parse", &format!("refs/heads/{branch}")])
}

fn clone_output(source: &str, workspace: &Path, extra: &[&str]) -> std::process::Output {
    rwv()
        .args(["clone", source])
        .args(extra)
        .current_dir(workspace)
        .output()
        .expect("rwv clone runs")
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn envelope(out: &std::process::Output) -> Value {
    let stdout = stdout_of(out);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}):\n{stdout}"))
}

/// The `(path, axis, state)` of each member `left` lists, in order.
fn left_of(envelope: &Value) -> Vec<(String, String, String)> {
    envelope["left"]
        .as_array()
        .unwrap_or_else(|| panic!("`left` must be an array:\n{envelope}"))
        .iter()
        .map(|judged| {
            let field = |key: &str| judged[key].as_str().unwrap_or_default().to_owned();
            (field("path"), field("axis"), field("state"))
        })
        .collect()
}

fn lists_pin(stdout: &str, path: &str, state: &str) -> bool {
    stdout
        .lines()
        .any(|line| line == format!("rwv clone: {path}: pin {state}"))
}

fn routes_to_pin_page(stdout: &str) -> bool {
    stdout.lines().any(|line| line == "rwv explain pin")
}

// ============================================================================
// Basic CLI plumbing
// ============================================================================

#[test]
fn clone_without_a_source_is_a_usage_error() {
    let tmp = common::tempdir().unwrap();
    rwv()
        .arg("clone")
        .current_dir(tmp.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("<SOURCE>"));
}

#[test]
fn clone_refuses_a_plain_path_source_and_names_the_file_url_spelling() {
    for source in ["some-source", "not-a-valid-source-at-all"] {
        let tmp = common::tempdir().unwrap();
        let out = clone_output(source, tmp.path(), &[]);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(1), "{source}: {stderr}");
        assert!(
            stderr.contains(&format!(
                "'{source}' is neither a URL nor a registry shorthand"
            )) && stderr.contains("rwv clone file://<absolute path>")
                && stderr.contains("rwv explain unresolvable-repo-source"),
            "{source}: the refusal names the source, the URL spelling and its page:\n{stderr}"
        );
        assert!(
            std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
            "{source}: a refused source creates nothing"
        );
    }
}

#[test]
fn clone_of_a_remote_that_does_not_exist_fails_naming_the_source() {
    let tmp = common::tempdir().unwrap();
    let out = clone_output("file:///nonexistent/path/to/repo.git", tmp.path(), &[]);
    let stderr = stderr_of(&out);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("failed to clone project source 'file:///nonexistent/path/to/repo.git'"),
        "the failure names the source:\n{stderr}"
    );
    assert!(
        !tmp.path().join("projects/repo").exists(),
        "a failed clone leaves no project directory"
    );
}

// ============================================================================
// A project source with a complete lock: clone the project and every member
// ============================================================================

#[test]
fn clone_clones_the_project_and_its_members() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (_, server_url) = member_remote(tmp.path(), "bare_server");
    let (_, client_url) = member_remote(tmp.path(), "bare_client");
    let source = publish_project(
        tmp.path(),
        "project",
        &[
            Member::owned("local/org/server", &server_url),
            Member::owned("local/org/client", &client_url),
        ],
        Lock::AtRemoteTips,
    );

    rwv()
        .args(["clone", &source.url])
        .current_dir(&workspace)
        .assert()
        .success();

    assert!(
        workspace.join("local/org/server").exists(),
        "server repo should be cloned"
    );
    assert!(
        workspace.join("local/org/client").exists(),
        "client repo should be cloned"
    );
}

#[test]
fn clone_creates_project_dir_with_manifest() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let source = publish_project(tmp.path(), "remote", &[], Lock::AtRemoteTips);

    rwv()
        .args(["clone", &source.url])
        .current_dir(&workspace)
        .assert()
        .success();

    assert!(
        workspace.join("projects/remote/rwv.toml").is_file(),
        "the project directory is named for the source and holds rwv.toml"
    );
}

#[test]
fn clone_places_members_at_canonical_paths() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (_, dep_url) = member_remote(tmp.path(), "dep");
    let source = publish_project(
        tmp.path(),
        "project",
        &[Member::owned("local/team/dep", &dep_url)],
        Lock::AtRemoteTips,
    );

    rwv()
        .args(["clone", &source.url])
        .current_dir(&workspace)
        .assert()
        .success();

    let repo_dir = workspace.join("local/team/dep");
    assert!(
        repo_dir.join(".git").exists() || repo_dir.join("HEAD").exists(),
        "canonical path local/team/dep should contain a git repository"
    );
}

#[test]
fn clone_over_an_existing_project_refuses_as_already_existing() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (_, dep_url) = member_remote(tmp.path(), "dep");
    let source = publish_project(
        tmp.path(),
        "project",
        &[Member::owned("local/team/dep", &dep_url)],
        Lock::AtRemoteTips,
    );

    rwv()
        .args(["clone", &source.url])
        .current_dir(&workspace)
        .assert()
        .success();

    let out = clone_output(&source.url, &workspace, &[]);
    let combined = format!("{}{}", stdout_of(&out), stderr_of(&out));
    assert!(
        !out.status.success(),
        "a second clone of the same source should fail, got: {combined}"
    );
    assert!(
        combined.contains("already exists"),
        "a second clone should report 'already exists', got: {combined}"
    );
}

#[test]
fn clone_second_invocation_refuses_and_keeps_the_project_active() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (_, dep_url) = member_remote(tmp.path(), "dep");
    let source = publish_project(
        tmp.path(),
        "project",
        &[Member::owned("local/team/dep", &dep_url)],
        Lock::AtRemoteTips,
    );

    rwv()
        .args(["clone", &source.url])
        .current_dir(&workspace)
        .assert()
        .success();
    assert!(
        workspace.join(".rwv-active").exists(),
        "project should be activated after the first clone"
    );

    let out = clone_output(&source.url, &workspace, &[]);
    let combined = format!("{}{}", stdout_of(&out), stderr_of(&out));
    assert!(
        !out.status.success() && combined.contains("already exists"),
        "the second clone collides on the project directory, got: {combined}"
    );
    assert!(
        workspace.join(".rwv-active").exists(),
        "project should remain activated after the refused second clone"
    );
}

// ============================================================================
// Activation of the project the clone created
// ============================================================================

#[test]
fn clone_activates_the_project_when_none_is_active() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (_, dep_url) = member_remote(tmp.path(), "dep");
    let source = publish_project(
        tmp.path(),
        "project",
        &[Member::owned("local/team/dep", &dep_url)],
        Lock::AtRemoteTips,
    );

    rwv()
        .args(["clone", &source.url])
        .current_dir(&workspace)
        .assert()
        .success();

    let active = std::fs::read_to_string(workspace.join(".rwv-active"))
        .expect("the clone should write .rwv-active");
    assert_eq!(active.trim(), "project");
}

/// A workspace directory and a source with one member, for a clone run with
/// `--allow-non-empty-dir` over a pointer the test plants first.
fn setup_bootstrap_source(tmp: &Path) -> (PathBuf, String) {
    let workspace = empty_workspace(tmp, "ws");
    let (_, dep_url) = member_remote(tmp, "dep");
    let source = publish_project(
        tmp,
        "project",
        &[Member::owned("local/team/dep", &dep_url)],
        Lock::AtRemoteTips,
    );
    (workspace, source.url)
}

#[test]
fn clone_refuses_over_a_pointer_that_holds_no_project_name() {
    for content in ["", "  \n  \n", "-not-a-valid-name\n"] {
        let tmp = common::tempdir().unwrap();
        let (workspace, source) = setup_bootstrap_source(tmp.path());
        std::fs::write(workspace.join(".rwv-active"), content).unwrap();

        let out = clone_output(&source, &workspace, &["--allow-non-empty-dir"]);
        let stderr = stderr_of(&out);
        assert!(!out.status.success(), "{content:?}: {stderr}");
        assert!(
            stderr.contains("unreadable-active-project") && stderr.contains("rwv activate"),
            "{content:?}: {stderr}"
        );
        assert!(
            !workspace.join("projects").join("project").exists(),
            "{content:?}: the refusal comes before the project is cloned"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join(".rwv-active")).unwrap(),
            content,
            "the pointer is left for the operator to repair"
        );
    }
}

#[test]
fn clone_skips_auto_activate_over_a_valid_active_pointer() {
    let tmp = common::tempdir().unwrap();
    let (workspace, source) = setup_bootstrap_source(tmp.path());
    std::fs::write(workspace.join(".rwv-active"), "other-project\n").unwrap();

    rwv()
        .args(["clone", &source, "--allow-non-empty-dir"])
        .current_dir(&workspace)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "rwv clone: skipping auto-activate (project 'other-project' already active)",
        ));

    let active_content = std::fs::read_to_string(workspace.join(".rwv-active")).unwrap();
    assert_eq!(active_content.trim(), "other-project");
}

// ============================================================================
// The lock decides where each member is placed
// ============================================================================

#[test]
fn clone_places_a_member_at_its_lock_pin_not_the_branch_tip() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (dep_bare, dep_url) = member_remote(tmp.path(), "dep");
    let source = publish_project(
        tmp.path(),
        "project",
        &[Member::owned("local/team/dep", &dep_url)],
        Lock::AtRemoteTips,
    );
    let pinned = tip(&dep_bare, "main");
    let ahead = push_commit(&dep_bare, "main");
    assert_ne!(pinned, ahead, "fixture: the branch tip is past the pin");
    let lock_before = committed_lock(&source.bare);

    rwv()
        .args(["clone", &source.url])
        .current_dir(&workspace)
        .assert()
        .success();

    let dep_dir = workspace.join("local/team/dep");
    assert_eq!(
        rev(&dep_dir),
        pinned,
        "the member is placed at the revision rwv.lock pins"
    );
    assert_eq!(
        common::read_normalized(workspace.join("projects/project/rwv.lock")),
        lock_before,
        "a lock that covers every member is left as it was cloned"
    );
}

#[test]
fn clone_with_a_complete_lock_exits_zero_and_leaves_no_member() {
    let tmp = common::tempdir().unwrap();
    let (alpha_bare, alpha_url) = member_remote(tmp.path(), "alpha");
    let (_, beta_url) = member_remote(tmp.path(), "beta");
    let source = publish_project(
        tmp.path(),
        "project",
        &[
            Member::owned("local/team/alpha", &alpha_url),
            Member::owned("local/team/beta", &beta_url),
        ],
        Lock::AtRemoteTips,
    );
    let alpha_pin = tip(&alpha_bare, "main");
    push_commit(&alpha_bare, "main");

    let text_workspace = empty_workspace(tmp.path(), "ws-text");
    let out = clone_output(&source.url, &text_workspace, &[]);
    let stdout = stdout_of(&out);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{}", stderr_of(&out));
    assert!(
        !stdout.contains("pin none")
            && !stdout.contains("pin unreachable")
            && !routes_to_pin_page(&stdout),
        "a complete lock lists nothing:\n{stdout}"
    );

    let json_workspace = empty_workspace(tmp.path(), "ws-json");
    let out = clone_output(&source.url, &json_workspace, &["--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr_of(&out));
    let envelope = envelope(&out);
    assert_eq!(envelope["left"], serde_json::json!([]), "{envelope}");
    assert_eq!(
        envelope["outcomes"].as_array().map(Vec::len),
        Some(2),
        "{envelope}"
    );
    assert_eq!(
        rev(&json_workspace.join("local/team/alpha")),
        alpha_pin,
        "a member whose pin is behind its branch tip still counts as placed"
    );
}

// ============================================================================
// A member the lock cannot place: the clone completes, lists it, exits 3
// ============================================================================

#[test]
fn clone_lists_a_member_the_lock_lacks_and_one_pinned_where_no_remote_has_it() {
    let tmp = common::tempdir().unwrap();
    let (lacking_bare, lacking_url) = member_remote(tmp.path(), "lacking");
    let (unpublished_bare, unpublished_url) = member_remote(tmp.path(), "unpublished");
    let (placed_bare, placed_url) = member_remote(tmp.path(), "placed");
    let lacking_tip = push_commit(&lacking_bare, "main");
    let unpublished_main_tip = tip(&unpublished_bare, "main");
    let unpublished_tip = push_commit(&unpublished_bare, "release");
    assert_ne!(
        unpublished_main_tip, unpublished_tip,
        "fixture: the declared branch is not the remote's default"
    );
    let unpublished_pin = unpublished_commit(&unpublished_bare, "release");
    let placed_pin = tip(&placed_bare, "main");
    push_commit(&placed_bare, "main");
    let source = publish_project(
        tmp.path(),
        "project",
        &[
            Member::owned("local/team/lacking", &lacking_url),
            Member::owned("local/team/unpublished", &unpublished_url).on("release"),
            Member::owned("local/team/placed", &placed_url),
        ],
        Lock::Pins(&[
            ("local/team/unpublished", &unpublished_url, &unpublished_pin),
            ("local/team/placed", &placed_url, &placed_pin),
        ]),
    );
    let committed_lock = committed_lock(&source.bare);

    let text_workspace = empty_workspace(tmp.path(), "ws-text");
    let out = clone_output(&source.url, &text_workspace, &[]);
    let stdout = stdout_of(&out);
    assert_eq!(out.status.code(), Some(3), "{stdout}\n{}", stderr_of(&out));
    assert!(
        lists_pin(&stdout, "local/team/lacking", "none"),
        "the member the lock lacks is listed `pin none`:\n{stdout}"
    );
    assert!(
        lists_pin(&stdout, "local/team/unpublished", "unreachable"),
        "the member pinned at a commit no remote has is listed `pin unreachable`:\n{stdout}"
    );
    assert!(
        routes_to_pin_page(&stdout),
        "the listing ends on the route line `rwv explain pin`:\n{stdout}"
    );
    assert!(
        !stdout.contains("local/team/placed: pin"),
        "a member at its pin is not listed:\n{stdout}"
    );

    let lacking_dir = text_workspace.join("local/team/lacking");
    common::assert_on_branch(&lacking_dir, "main");
    assert_eq!(
        rev(&lacking_dir),
        lacking_tip,
        "the member the lock lacks sits at the remote tip of its declared branch"
    );
    let unpublished_dir = text_workspace.join("local/team/unpublished");
    common::assert_on_branch(&unpublished_dir, "release");
    assert_eq!(
        rev(&unpublished_dir),
        unpublished_tip,
        "the member pinned at an absent commit sits at the remote tip of its declared branch"
    );
    assert_eq!(
        rev(&text_workspace.join("local/team/placed")),
        placed_pin,
        "the member the lock places sits at its pin"
    );
    assert_eq!(
        common::read_normalized(text_workspace.join("projects/project/rwv.lock")),
        committed_lock,
        "the clone writes nothing into rwv.lock"
    );

    let json_workspace = empty_workspace(tmp.path(), "ws-json");
    let out = clone_output(&source.url, &json_workspace, &["--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr_of(&out));
    let envelope = envelope(&out);
    assert_eq!(
        left_of(&envelope),
        [
            ("local/team/lacking", "pin", "none"),
            ("local/team/unpublished", "pin", "unreachable"),
        ]
        .map(|(path, axis, state)| (path.to_owned(), axis.to_owned(), state.to_owned())),
        "{envelope}"
    );
    assert_eq!(
        envelope["outcomes"].as_array().map(Vec::len),
        Some(3),
        "the clone completed every member: {envelope}"
    );
}

#[test]
fn clone_of_a_project_with_no_lock_lists_every_member_and_writes_none() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (alpha_bare, alpha_url) = member_remote(tmp.path(), "alpha");
    let (beta_bare, beta_url) = member_remote(tmp.path(), "beta");
    let source = publish_project(
        tmp.path(),
        "project",
        &[
            Member::owned("local/team/alpha", &alpha_url),
            Member::owned("local/team/beta", &beta_url),
        ],
        Lock::Absent,
    );

    let out = clone_output(&source.url, &workspace, &[]);
    let stdout = stdout_of(&out);
    assert_eq!(out.status.code(), Some(3), "{stdout}\n{}", stderr_of(&out));
    for path in ["local/team/alpha", "local/team/beta"] {
        assert!(
            lists_pin(&stdout, path, "none"),
            "{path} is listed `pin none`:\n{stdout}"
        );
    }
    assert!(
        !stdout.contains("pin unreachable") && routes_to_pin_page(&stdout),
        "an absent lock pins nothing, so nothing is unreachable:\n{stdout}"
    );
    assert!(
        !workspace.join("projects/project/rwv.lock").exists(),
        "the clone does not write the lock it found missing"
    );
    for (path, bare) in [
        ("local/team/alpha", &alpha_bare),
        ("local/team/beta", &beta_bare),
    ] {
        let dir = workspace.join(path);
        common::assert_on_branch(&dir, "main");
        assert_eq!(
            rev(&dir),
            tip(bare, "main"),
            "{path} sits at the branch tip"
        );
    }
}

#[test]
fn clone_places_a_member_on_its_declared_branch_when_the_lock_lacks_it() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (release_bare, release_url) = member_remote(tmp.path(), "release");
    let (other_bare, other_url) = member_remote(tmp.path(), "other");
    let main_tip = tip(&release_bare, "main");
    let release_tip = push_commit(&release_bare, "release");
    assert_ne!(main_tip, release_tip, "fixture: the branches differ");
    let source = publish_project(
        tmp.path(),
        "project",
        &[
            Member::owned("local/team/release", &release_url).on("release"),
            Member::owned("local/team/other", &other_url),
        ],
        Lock::Pins(&[("local/team/other", &other_url, &tip(&other_bare, "main"))]),
    );

    let out = clone_output(&source.url, &workspace, &[]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}\n{}",
        stdout_of(&out),
        stderr_of(&out)
    );

    let dir = workspace.join("local/team/release");
    common::assert_on_branch(&dir, "release");
    assert_eq!(
        rev(&dir),
        release_tip,
        "the member is born at the remote tip of its declared branch, not the remote's default"
    );
}

#[test]
fn clone_no_reference_does_not_list_a_reference_member_the_lock_lacks() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let source = reference_member_the_lock_lacks(tmp.path());

    let out = clone_output(&source, &workspace, &["--no-reference"]);
    let stdout = stdout_of(&out);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{}", stderr_of(&out));
    assert!(
        !stdout.contains("pin none"),
        "a member the clone skipped is not listed:\n{stdout}"
    );
    assert!(workspace.join("local/team/primary").exists());
    assert!(
        !workspace.join("local/team/reference").exists(),
        "the reference member is skipped"
    );
}

#[test]
fn clone_lists_a_reference_member_the_lock_lacks() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let source = reference_member_the_lock_lacks(tmp.path());

    let out = clone_output(&source, &workspace, &[]);
    let stdout = stdout_of(&out);
    assert_eq!(out.status.code(), Some(3), "{stdout}\n{}", stderr_of(&out));
    assert!(
        lists_pin(&stdout, "local/team/reference", "none"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("local/team/primary: pin"),
        "the pinned member is not listed:\n{stdout}"
    );
    assert!(workspace.join("local/team/reference").exists());
}

/// A source with an owned member the lock pins and a reference member it does
/// not; returns its URL.
fn reference_member_the_lock_lacks(tmp: &Path) -> String {
    let (primary_bare, primary_url) = member_remote(tmp, "primary");
    let (_, reference_url) = member_remote(tmp, "reference");
    publish_project(
        tmp,
        "project",
        &[
            Member::owned("local/team/primary", &primary_url),
            Member::owned("local/team/reference", &reference_url).role("reference"),
        ],
        Lock::Pins(&[(
            "local/team/primary",
            &primary_url,
            &tip(&primary_bare, "main"),
        )]),
    )
    .url
}

// ============================================================================
// --no-reference
// ============================================================================

#[test]
fn clone_no_reference_skips_reference_role_members() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (_, primary_url) = member_remote(tmp.path(), "primary");
    let (_, reference_url) = member_remote(tmp.path(), "reference");
    let source = publish_project(
        tmp.path(),
        "project",
        &[
            Member::owned("local/team/primary", &primary_url),
            Member::owned("local/team/reference", &reference_url).role("reference"),
        ],
        Lock::AtRemoteTips,
    );

    rwv()
        .args(["clone", &source.url, "--no-reference"])
        .current_dir(&workspace)
        .assert()
        .success();

    assert!(
        workspace.join("local/team/primary").exists(),
        "primary repo should be cloned"
    );
    assert!(
        !workspace.join("local/team/reference").exists(),
        "reference repo should be skipped with --no-reference"
    );
}

// ============================================================================
// Arguments that belong to other verbs
// ============================================================================

#[test]
fn clone_refuses_the_arguments_that_belong_to_other_verbs() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (_, dep_url) = member_remote(tmp.path(), "dep");
    let source = publish_project(
        tmp.path(),
        "project",
        &[Member::owned("local/team/dep", &dep_url)],
        Lock::AtRemoteTips,
    );

    let refused: [&[&str]; 5] = [
        &["--frozen"],
        &["--role", "owned"],
        &["--repo", "local/team/dep"],
        &["--dirty"],
        &["--project", "x"],
    ];
    for args in refused {
        let flag = args[0];
        let out = clone_output(&source.url, &workspace, args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(2), "{flag}: {stderr}");
        assert!(
            stderr.contains(&format!("unexpected argument '{flag}' found")),
            "{flag}: {stderr}"
        );
    }
    assert!(
        !workspace.join("projects").exists(),
        "a refused invocation clones nothing"
    );

    rwv()
        .args(["clone", &source.url])
        .current_dir(&workspace)
        .assert()
        .success();
}

// ============================================================================
// The source's name must be a project name
// ============================================================================

/// A source whose repository name is not a legal project name is refused
/// before anything is cloned: `projects/<name>` would be a project no later
/// verb could name.
#[test]
fn clone_refuses_a_source_named_like_no_project_before_cloning() {
    let tmp = common::tempdir().unwrap();
    let workspace = empty_workspace(tmp.path(), "ws");
    let (_, url) = member_remote(tmp.path(), "bad--name");

    let out = clone_output(&url, &workspace, &[]);
    let stderr = stderr_of(&out);
    assert!(
        !out.status.success()
            && stderr.contains("'bad--name' is not a valid project name")
            && stderr.contains("rwv explain unrenderable-name"),
        "the derived name is refused with its token:\n{stderr}"
    );
    assert!(
        !workspace.join("projects").join("bad--name").exists(),
        "nothing is cloned for a name that is refused"
    );
}
