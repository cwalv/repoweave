//! Integration tests verifying documented behavior of `rwv clone`, `rwv member add`,
//! and `rwv member remove`.
//!
//! Each test states the documented claim it pins in its own section banner.

use assert_cmd::Command;
use repoweave::refusal::RefusalKind;
use std::path::Path;
use std::process;

mod common;

// ---------------------------------------------------------------------------
// Helpers (mirrors patterns from add_remove_test.rs / workweave_test.rs)
// ---------------------------------------------------------------------------

fn rwv() -> Command {
    common::rwv()
}

/// Create a bare git repo at `path`.
fn init_bare_repo(path: &Path) {
    let status = common::git()
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(path)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git init --bare failed");
}

/// Create a bare git repo with one initial commit so it can be cloned.
fn init_bare_repo_with_commit(path: &Path) {
    init_bare_repo(path);

    let tmp = common::tempdir().expect("tempdir for working clone");
    let work = tmp.path().join("work");

    let run = |args: &[&str], cwd: &Path| {
        let status = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("git command failed to start");
        assert!(status.success(), "git {:?} failed", args);
    };

    run(
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);
    std::fs::write(work.join("README"), "init").unwrap();
    run(&["add", "."], &work);
    run(&["commit", "-m", "initial"], &work);
    run(&["push", "origin", "main"], &work);
}

/// Push an `rwv.toml` manifest, and the `rwv.lock` pinning each member at its
/// remote's tip, into a bare repo (via a temporary working clone).
///
/// `repos` is a slice of `(canonical_path, url)` pairs.
fn push_manifest_to_bare(bare: &Path, repos: &[(&str, &str)]) {
    let tmp = common::tempdir().expect("tempdir for manifest work clone");
    let work = tmp.path().join("mwork");

    let run = |args: &[&str], cwd: &Path| {
        let status = common::git()
            .args(args)
            .current_dir(cwd)
            .stdout(process::Stdio::null())
            .stderr(process::Stdio::null())
            .status()
            .expect("git command failed to start");
        assert!(status.success(), "git {:?} failed", args);
    };

    run(
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
    run(&["config", "user.email", "test@test.com"], &work);
    run(&["config", "user.name", "Test"], &work);

    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(work.join("rwv.toml"), &manifest_toml).unwrap();
    common::fixture_lock_at_remote_tips(&work, repos);
    run(&["add", "rwv.toml", "rwv.lock"], &work);
    run(&["commit", "-m", "add manifest and lock"], &work);
    run(&["push", "origin", "main"], &work);
}

/// A bare remote at `<tmp>/<name>.git` holding one commit on `main`; returns
/// its URL.
fn member_remote_url(tmp: &Path, name: &str) -> String {
    let bare = tmp.join(format!("{name}.git"));
    common::init_bare_repo_with_commit(&bare);
    common::file_url(&bare)
}

/// A project remote at `<tmp>/<name>.git` declaring the one member
/// `(path, url)` at `role`, with a lock pinning it at its tip when `locked`;
/// returns its URL.
fn project_with_one_member(
    tmp: &Path,
    name: &str,
    member: (&str, &str),
    role: &str,
    locked: bool,
) -> String {
    let bare = tmp.join(format!("{name}.git"));
    init_bare_repo(&bare);
    let work = tmp.join(format!("{name}-work"));
    common::git_in(
        tmp,
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
    );
    let (path, url) = member;
    std::fs::write(
        work.join("rwv.toml"),
        format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"{role}\"\n"
        ),
    )
    .unwrap();
    if locked {
        common::fixture_lock_at_remote_tips(&work, &[member]);
    }
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "project"]);
    common::git_in(&work, &["push", "origin", "main"]);
    common::file_url(&bare)
}

/// Set up a workspace with a single project that has an `rwv.toml` and is
/// itself a git repo (required by workspace resolution).
///
/// Also writes `.rwv-active` to the new project so action verbs resolve
/// cleanly (there is no CWD override).
///
/// Returns `(workspace_root, project_dir)`.
fn setup_workspace_with_project(
    tmp: &tempfile::TempDir,
    project_name: &str,
    repos: &[(&str, &str)],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let project_dir = workspace.join("projects").join(project_name);
    std::fs::create_dir_all(&project_dir).unwrap();

    common::git_in(&project_dir, &["init", "--initial-branch=main"]);
    common::git_in(&project_dir, &["config", "user.email", "test@test.com"]);
    common::git_in(&project_dir, &["config", "user.name", "Test"]);

    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
    common::git_in(&project_dir, &["add", "rwv.toml"]);
    common::git_in(&project_dir, &["commit", "-m", "init"]);

    std::fs::write(workspace.join(".rwv-active"), format!("{project_name}\n")).unwrap();

    (workspace, project_dir)
}

// ============================================================================
// Test 1 — name collision on clone
//
// `rwv clone <source>` over an occupied `projects/<name>/` refuses, naming
// `rwv update --project <name>` for the project already there and an empty
// directory for cloning this one beside it. Both run.
// ============================================================================

#[test]
fn clone_name_collision_names_the_in_place_update_and_an_empty_directory() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let project_bare = tmp.path().join("web-app.git");
    init_bare_repo(&project_bare);
    push_manifest_to_bare(&project_bare, &[]);
    let project_url = common::file_url(&project_bare);
    rwv()
        .args(["clone", &project_url])
        .current_dir(&workspace)
        .assert()
        .success();

    let output = rwv()
        .args(["clone", &project_url])
        .current_dir(&workspace)
        .output()
        .expect("rwv clone should run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success() && stderr.contains("rwv explain project-dir-occupied"),
        "fixture: the second clone refuses for the occupied directory:\n{stderr}"
    );
    assert!(
        stderr.contains("`rwv update --project web-app` brings up its members in place")
            && stderr.contains(&format!(
                "run `rwv clone {project_url}` in an empty directory"
            )),
        "the refusal names the in-place update and the empty directory:\n{stderr}"
    );

    rwv()
        .args(["update", "--project", "web-app"])
        .current_dir(&workspace)
        .assert()
        .success();

    let beside = tmp.path().join("beside");
    std::fs::create_dir_all(&beside).unwrap();
    rwv()
        .args(["clone", &project_url])
        .current_dir(&beside)
        .assert()
        .success();
    assert!(beside.join("projects/web-app/rwv.toml").is_file());
}

// ============================================================================
// Test 2 — `rwv member remove --delete` against a clone another project shares
//
// `rwv member remove --delete` checks whether other projects reference the same repo
// before deleting.  If a reference is found:
//   - A warning is printed: "warning: repo also referenced by project 'X'"
//   - The deletion is refused unless `--delete-shared-clone` is also passed.
// ============================================================================

#[test]
fn remove_delete_does_not_check_other_projects() {
    let tmp = common::tempdir().unwrap();

    // Create a bare repo that both projects will reference.
    let shared_bare = tmp.path().join("shared.git");
    init_bare_repo_with_commit(&shared_bare);
    let shared_url = common::file_url(&shared_bare);
    let repo_path = "local/org/shared";

    // Project A references the shared repo.
    let (workspace, _project_a_dir) =
        setup_workspace_with_project(&tmp, "project-a", &[(repo_path, &shared_url)]);

    // Project B also references the same repo.
    let project_b_dir = workspace.join("projects").join("project-b");
    std::fs::create_dir_all(&project_b_dir).unwrap();
    let yaml_b = format!(
        "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{shared_url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_b_dir.join("rwv.toml"), &yaml_b).unwrap();

    // Clone the shared repo to disk so --delete has something to remove.
    let repo_dir = workspace.join(repo_path);
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    let clone_status = common::git()
        .args([
            "clone",
            &shared_bare.to_string_lossy(),
            &repo_dir.to_string_lossy(),
        ])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git clone failed");
    assert!(clone_status.success());

    // --- Without --delete-shared-clone: should fail and print a warning ---
    let output = rwv()
        .args(["member", "remove", repo_path, "--delete"])
        .current_dir(workspace.join("projects").join("project-a"))
        .output()
        .expect("rwv member remove should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "rwv member remove --delete should fail when another project references the repo; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("warning: repo also referenced by project"),
        "expected cross-project warning in stderr, got:\n{stderr}"
    );
    // The directory must still exist — deletion was refused.
    assert!(
        repo_dir.exists(),
        "directory should NOT be deleted when another project references the repo"
    );

    // --- With --delete-shared-clone: should succeed and delete ---
    rwv()
        .args([
            "member",
            "remove",
            repo_path,
            "--delete",
            "--delete-shared-clone",
        ])
        .current_dir(workspace.join("projects").join("project-a"))
        .assert()
        .success();

    assert!(
        !repo_dir.exists(),
        "rwv member remove --delete --delete-shared-clone should delete the directory even \
         when shared"
    );
}

// ============================================================================
// Test 3 — `rwv member add --adopt <path>` infers the URL from the origin remote
//
// `rwv member add --adopt <path> --role reference` infers the URL from the clone's
// origin remote for a clone that exists under the weave root.
// ============================================================================

#[test]
fn add_from_local_path_infers_url() {
    let tmp = common::tempdir().unwrap();

    // Create a bare repo to act as the remote origin.
    let bare = tmp.path().join("origin.git");
    init_bare_repo_with_commit(&bare);
    let origin_url = common::file_url(&bare);

    // Set up workspace with an empty project manifest.
    let (workspace, _project_dir) = setup_workspace_with_project(&tmp, "test-project", &[]);

    // Clone the bare repo to the canonical path github/org/repo inside the workspace.
    let repo_dir = workspace.join("github").join("org").join("repo");
    std::fs::create_dir_all(repo_dir.parent().unwrap()).unwrap();
    let clone_status = common::git()
        .args([
            "clone",
            &bare.to_string_lossy(),
            &repo_dir.to_string_lossy(),
        ])
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git clone failed");
    assert!(clone_status.success());

    // Run `rwv member add --adopt github/org/repo --role reference` from the
    // workspace root; rwv infers the URL from the clone's origin remote.
    rwv()
        .args([
            "member",
            "add",
            "--adopt",
            "github/org/repo",
            "--role",
            "reference",
        ])
        .current_dir(&workspace)
        .assert()
        .success();

    // Verify the manifest contains the correct origin URL and repo path.
    let manifest_path = workspace.join("projects/test-project/rwv.toml");
    let manifest_content =
        std::fs::read_to_string(&manifest_path).expect("rwv.toml should exist after add");

    assert!(
        manifest_content.contains("github/org/repo"),
        "manifest should contain the repo path 'github/org/repo', got:\n{manifest_content}"
    );
    assert!(
        manifest_content.contains(&origin_url),
        "manifest should contain the inferred origin URL '{origin_url}', got:\n{manifest_content}"
    );
}

// ============================================================================
// Test 4 — the registry shorthand names its registry
//
// Doc claim: `rwv clone <registry>/<owner>/<repo>` resolves through the named
// registry, and a two-segment argument is refused as neither a URL nor a
// shorthand, naming both spellings it could mean.
//
// Resolution is seen without network access: the failed clone's error names
// github.com, so the shorthand was expanded before the clone was attempted.
// ============================================================================

#[test]
fn clone_registry_shorthand_resolves_through_the_named_registry() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let output = rwv()
        .args([
            "clone",
            "github/nonexistent-owner-xyzzy/nonexistent-repo-xyzzy",
        ])
        .current_dir(&workspace)
        .output()
        .expect("rwv clone should run");

    assert!(!output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(combined.contains("github.com"), "{combined}");
}

#[test]
fn every_verb_taking_a_source_refuses_two_segments_naming_the_shorthand_and_a_file_url() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(workspace.join("projects")).unwrap();
    rwv()
        .args(["init", "web"])
        .current_dir(&workspace)
        .assert()
        .success();

    for verb in [
        &["clone"][..],
        &["init", "--adopt"][..],
        &["member", "add"][..],
    ] {
        let out = rwv()
            .args(verb)
            .arg("chatly/web-app")
            .current_dir(&workspace)
            .output()
            .unwrap();
        let stderr = common::cli::refused(&out, RefusalKind::UnresolvableRepoSource);
        assert!(
            stderr.contains("`<registry>/<owner>/<repo>`"),
            "{verb:?}: {stderr}"
        );
        assert!(
            stderr.contains("github, gitlab, bitbucket"),
            "{verb:?}: {stderr}"
        );
        assert!(stderr.contains("file://"), "{verb:?}: {stderr}");
        assert!(!stderr.contains("github.com"), "{verb:?}: {stderr}");
    }
}

/// A full `file://` URL is passed through as it stands (shorthand only expands
/// for known registries like github and gitlab), and the project clones under
/// the name of the repository.
#[test]
fn clone_of_a_file_url_names_the_project_for_the_repository() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let project_bare = tmp.path().join("myproject.git");
    init_bare_repo(&project_bare);
    push_manifest_to_bare(&project_bare, &[]);
    let project_url = common::file_url(&project_bare);

    rwv()
        .args(["clone", &project_url])
        .current_dir(&workspace)
        .assert()
        .success();

    assert!(
        workspace.join("projects/myproject").exists(),
        "project should be cloned to projects/myproject"
    );
}

// ============================================================================
// Test 4b — activation is not lock coverage
//
// A clone activates the project it created when none is active, whether the
// clone lists members the lock cannot place or skips members with a filter.
// ============================================================================

#[test]
fn a_clone_that_lists_members_or_skips_references_still_activates_the_project() {
    let tmp = common::tempdir().unwrap();

    let owned_url = member_remote_url(tmp.path(), "owned-member");
    let unlocked = project_with_one_member(
        tmp.path(),
        "unlocked-app",
        ("local/org/member", &owned_url),
        "owned",
        false,
    );
    let reference_url = member_remote_url(tmp.path(), "reference-member");
    let filtered = project_with_one_member(
        tmp.path(),
        "filtered-app",
        ("local/org/reference", &reference_url),
        "reference",
        true,
    );

    for (name, url, extra, code) in [
        ("unlocked-app", unlocked, None, 3),
        ("filtered-app", filtered, Some("--no-reference"), 0),
    ] {
        let workspace = tmp.path().join(format!("ws-{name}"));
        std::fs::create_dir_all(&workspace).unwrap();

        let mut cmd = rwv();
        cmd.arg("clone").arg(&url);
        if let Some(extra) = extra {
            cmd.arg(extra);
        }
        cmd.current_dir(&workspace).assert().code(code);

        assert_eq!(
            std::fs::read_to_string(workspace.join(".rwv-active"))
                .unwrap_or_default()
                .trim(),
            name,
            "`rwv clone <source> {extra:?}` must activate the project it cloned"
        );
    }
}

// ============================================================================
// Test 5 — cloning a second project leaves the active project alone
//
// Doc claim: Cloning a second project does not change the active project.
// ============================================================================

#[test]
fn clone_second_project_does_not_auto_activate() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // ------------------------------------------------------------------
    // Set up project A.
    // ------------------------------------------------------------------
    let project_a_bare = tmp.path().join("project-a.git");
    init_bare_repo(&project_a_bare);
    push_manifest_to_bare(&project_a_bare, &[]);
    let url_a = common::file_url(&project_a_bare);

    // Clone project A — this should activate it (first clone, no .rwv-active yet).
    rwv()
        .args(["clone", &url_a])
        .current_dir(&workspace)
        .assert()
        .success();

    // Verify project A is active.
    let active_path = workspace.join(".rwv-active");
    assert!(
        active_path.exists(),
        ".rwv-active should exist after first clone"
    );
    let active_after_a = std::fs::read_to_string(&active_path).expect("failed to read .rwv-active");
    assert_eq!(
        active_after_a.trim(),
        "project-a",
        "project-a should be active after cloning it"
    );

    // ------------------------------------------------------------------
    // Set up project B.
    // ------------------------------------------------------------------
    let project_b_bare = tmp.path().join("project-b.git");
    init_bare_repo(&project_b_bare);
    push_manifest_to_bare(&project_b_bare, &[]);
    let url_b = common::file_url(&project_b_bare);

    // Clone project B — .rwv-active already exists, so activation is skipped.
    rwv()
        .args(["clone", &url_b])
        .current_dir(&workspace)
        .assert()
        .success();

    // .rwv-active must still point to project-a.
    let active_after_b = std::fs::read_to_string(&active_path)
        .expect("failed to read .rwv-active after second clone");
    assert_eq!(
        active_after_b.trim(),
        "project-a",
        "second clone must not overwrite .rwv-active; project-a should remain active"
    );
}

// ============================================================================
// Test 6 — --json envelope assertion
//
// Doc claim: `rwv clone --json -j 1` emits
// `{ "$schema": "<url>", "outcomes": [...], "left": [...] }` envelope. Each
// element of `outcomes` has `path`, `absolute_path`, and `status`; `left` is
// empty when every member sits at its pin.
// ============================================================================

#[test]
fn clone_json_envelope_shape() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let repo_bare_url = member_remote_url(tmp.path(), "the-repo");

    // Create project bare with manifest pointing at the repo.
    let project_bare = tmp.path().join("proj.git");
    init_bare_repo(&project_bare);
    push_manifest_to_bare(&project_bare, &[("local/org/the-repo", &repo_bare_url)]);
    let project_url = common::file_url(&project_bare);

    // Run `rwv clone --json -j 1` (explicit serial = envelope).
    let assert = rwv()
        .args(["clone", &project_url, "--json", "-j", "1"])
        .current_dir(&workspace)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}):\n{stdout}"));
    let obj = parsed.as_object().expect("top level must be object");

    // $schema must be present and point to the committed artifact.
    let schema_val = obj
        .get("$schema")
        .and_then(serde_json::Value::as_str)
        .expect("$schema missing");
    assert!(
        schema_val.ends_with("/docs/reference/schemas/clone.json"),
        "$schema must end with /docs/reference/schemas/clone.json; got: {schema_val}"
    );

    // outcomes must be an array.
    let outcomes = obj
        .get("outcomes")
        .and_then(serde_json::Value::as_array)
        .expect("outcomes must be array");
    assert_eq!(outcomes.len(), 1, "one repo in manifest");

    let first = outcomes[0].as_object().expect("outcome must be object");
    assert!(first.contains_key("path"), "missing path");
    assert!(first.contains_key("absolute_path"), "missing absolute_path");
    assert_eq!(
        first.get("status").and_then(serde_json::Value::as_str),
        Some("ok"),
        "repo should be ok"
    );

    assert_eq!(
        obj.get("left"),
        Some(&serde_json::json!([])),
        "the member sits at its pin, so none is left"
    );
}

// ============================================================================
// Test 7 — --json -j N envelope assertion
//
// Doc claim: `rwv clone -j 2 --json` emits the same single envelope as
// `-j 1`, whatever `-j` is, with `outcomes` sorted by repo path.
// ============================================================================

#[test]
fn clone_json_envelope_under_parallel_jobs() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // Create two repos so we actually hit the parallel path.
    let url_a = member_remote_url(tmp.path(), "aa");
    let url_b = member_remote_url(tmp.path(), "bb");

    let project_bare = tmp.path().join("proj.git");
    init_bare_repo(&project_bare);
    push_manifest_to_bare(
        &project_bare,
        &[("local/org/aa", &url_a), ("local/org/bb", &url_b)],
    );
    let project_url = common::file_url(&project_bare);

    let assert = rwv()
        .args(["clone", &project_url, "--json", "-j", "2"])
        .current_dir(&workspace)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not one JSON document ({e}):\n{stdout}"));
    let schema_val = parsed["$schema"].as_str().expect("$schema missing");
    assert!(
        schema_val.ends_with("/docs/reference/schemas/clone.json"),
        "$schema must end with /docs/reference/schemas/clone.json; got: {schema_val}"
    );
    let outcomes = parsed["outcomes"]
        .as_array()
        .expect("outcomes must be array");
    let paths: Vec<&str> = outcomes.iter().filter_map(|o| o["path"].as_str()).collect();
    assert_eq!(paths, ["local/org/aa", "local/org/bb"], "{stdout}");
    for outcome in outcomes {
        assert_eq!(outcome["status"], "ok", "{stdout}");
    }
}
