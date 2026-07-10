//! Regression and doc-claim tests for `rwv sync-to --json` observability fields.
//!
//! Covers the new fields added in fo-w977z:
//! - `source_workweave` — name of the workweave CWD was invoked from (or null).
//! - `target` — absolute path of the target workspace.
//! - `retired` — true iff `--retire` was passed AND the workweave was deleted.
//! - `project_repo_advance` — step-3 advance for the project repo.
//! - per-outcome `step3_advance` — step-3 advance for each manifest repo.
//!
//! Round-trip test (spec acceptance criterion): invoke `rwv sync-to --retire
//! --json` from a workweave; parse the envelope; assert
//! - `source_workweave` matches the workweave basename,
//! - `retired == true`,
//! - `step3_advance.to_sha` of the manifest repo matches `git rev-parse HEAD`
//!   in the target's repo post-sync.
//!
//! Additional tests cover:
//! - `source_workweave` is null when invoked from the primary weave.
//! - `retired` is false when `--retire` is not passed.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> AssertCommand {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_out(args: &[&str], dir: &Path) -> String {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), yaml).unwrap();
}

fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url, sha) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: {sha}\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), yaml).unwrap();
}

const SERVER_URL: &str = "https://github.com/example/server.git";
const SERVER_PATH: &str = "github/example/server";

/// Write a `.rwv-workweave` marker file into `ww_dir`.
///
/// This is necessary for `rwv` to recognise the directory as a workweave via
/// the marker-based resolution path (step 1 in `WorkspaceContext::resolve`).
/// Without it, `rwv` falls back to the `{primary}--{name}` naming convention
/// and infers the project name from the left-hand side of the directory name
/// (e.g. "primary" from `primary--fo-test`), which doesn't match the actual
/// project "web-app".
fn write_workweave_marker(ww_dir: &Path, primary: &Path, project: &str, workweave_name: &str) {
    // The marker format is:
    //   primary: <absolute path>
    //   project: <project name>
    //   parent: <absolute path of the workspace this was forked from>
    let marker = format!(
        "primary: {primary}\nproject: {project}\nparent: {primary}\n",
        primary = primary.display(),
    );
    let _ = workweave_name; // stored in the dir name; marker itself doesn't need it
    std::fs::write(ww_dir.join(".rwv-workweave"), marker).unwrap();
}

/// A workspace layout with a primary weave and one workweave sharing repos via
/// `git worktree add`. The workweave is named `fo-test` so tests can assert
/// that `source_workweave` == `"fo-test"`.
///
/// The workweave has advanced by one commit so `sync-to` has real work to do.
/// Returns `(primary_root, ww_root, primary_server_dir, ww_server_dir,
///           primary_project_dir, ww_project_dir, advance_sha)`.
#[allow(clippy::type_complexity)]
fn make_workweave_ahead_fixture(
    parent: &Path,
    workweave_name: &str,
) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf, PathBuf, String) {
    let primary = parent.join("primary");
    std::fs::create_dir_all(primary.join("github/example")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let primary_server = primary.join(SERVER_PATH);
    let initial_sha = init_repo(&primary_server);

    let primary_project = primary.join("projects/web-app");
    init_repo(&primary_project);
    std::fs::write(
        primary_project.join(".gitattributes"),
        "rwv.lock merge=ours\n",
    )
    .unwrap();
    write_manifest(&primary_project, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&primary_project, &[(SERVER_PATH, SERVER_URL, &initial_sha)]);
    git(
        &["add", ".gitattributes", "rwv.yaml", "rwv.lock"],
        &primary_project,
    );
    git(&["commit", "-m", "lock: initial"], &primary_project);
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    // Workweave: materialise as git worktrees from the primary repos.
    // Name the directory simply as the workweave name (e.g. "fo-test") so the
    // marker-based resolution path is unambiguous and doesn't conflict with the
    // legacy `{primary}--{name}` convention (which uses the sibling's dir name
    // as the project, not `.rwv-active`).
    let ww = parent.join(workweave_name);
    std::fs::create_dir_all(ww.join("github/example")).unwrap();
    std::fs::create_dir_all(ww.join("projects")).unwrap();

    // Write the `.rwv-workweave` marker BEFORE `rwv` resolution runs so
    // the marker-based path (step 1 in `WorkspaceContext::resolve`) fires.
    write_workweave_marker(&ww, &primary, "web-app", workweave_name);

    let ww_server = ww.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            &format!("web-app--{workweave_name}/server"),
        ],
        &primary_server,
    );

    let ww_project = ww.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            &format!("web-app--{workweave_name}/project"),
        ],
        &primary_project,
    );
    std::fs::write(ww.join(".rwv-active"), "web-app\n").unwrap();

    // Advance the workweave's server repo and relock so sync-to has work.
    let advance_sha = make_commit(&ww_server, "ww.txt", "workweave\n", "ww: advance server");
    write_lock(&ww_project, &[(SERVER_PATH, SERVER_URL, &advance_sha)]);
    git(&["add", "rwv.lock"], &ww_project);
    git(&["commit", "-m", "lock: ww advance"], &ww_project);

    (
        primary,
        ww,
        primary_server,
        ww_server,
        primary_project,
        ww_project,
        advance_sha,
    )
}

// ===========================================================================
// Round-trip test: sync-to --retire --json from a workweave
//
// Acceptance criteria (spec fo-w977z):
//   - `source_workweave` matches the workweave name ("fo-test")
//   - `retired == true`
//   - per-outcome `step3_advance.to_sha` matches `git rev-parse HEAD` of the
//     target's manifest repo (primary's server) post-sync
//   - `project_repo_advance` is present and non-null (project repo advanced)
//
// We use `--strategy=rebase` so the replay phase runs and produces per-repo
// outcomes (with `step3_advance` attached). `--strategy=ff` skips replay in
// sync-to (CWD is strictly ahead), leaving `outcomes` empty.
// ===========================================================================

#[test]
fn sync_to_retire_json_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let workweave_name = "fo-test";
    let (primary, ww, primary_server, _ww_server, _primary_project, _ww_project, advance_sha) =
        make_workweave_ahead_fixture(tmp.path(), workweave_name);

    let assert = rwv()
        .args([
            "sync-to",
            &primary.to_string_lossy(),
            "--strategy=rebase",
            "--retire",
            "--json",
        ])
        .current_dir(&ww)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope must parse as JSON ({e}):\n{stdout}"));
    let obj = parsed.as_object().expect("envelope is an object");

    // --- source_workweave ---
    let source_ww = obj
        .get("source_workweave")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("source_workweave must be present and a string; got:\n{stdout}"));
    assert_eq!(
        source_ww, workweave_name,
        "source_workweave must match the workweave basename; got: {source_ww}"
    );

    // --- target ---
    let target = obj
        .get("target")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("target must be present; got:\n{stdout}"));
    assert!(
        !target.is_empty(),
        "target must be a non-empty path; got:\n{stdout}"
    );

    // --- retired ---
    let retired = obj
        .get("retired")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("retired must be a bool; got:\n{stdout}"));
    assert!(
        retired,
        "retired must be true when --retire was passed and sync-to succeeded; got:\n{stdout}"
    );

    // --- step3_advance.to_sha matches target's HEAD after sync ---
    // The primary's server repo should now be at `advance_sha` (CWD's tip).
    let primary_server_head = git_out(&["rev-parse", "HEAD"], &primary_server);
    assert_eq!(
        primary_server_head, advance_sha,
        "primary server must have been fast-forwarded to the workweave's tip"
    );

    // The per-outcome record for SERVER_PATH must carry step3_advance.
    let outcomes = obj
        .get("outcomes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("outcomes must be an array; got:\n{stdout}"));

    let server_outcome = outcomes
        .iter()
        .find(|o| o.get("path").and_then(Value::as_str) == Some(SERVER_PATH))
        .unwrap_or_else(|| panic!("expected outcome for {SERVER_PATH}; outcomes:\n{stdout}"));

    let step3 = server_outcome.get("step3_advance").unwrap_or_else(|| {
        panic!("server outcome must carry step3_advance; got outcome:\n{server_outcome}")
    });
    assert!(
        !step3.is_null(),
        "step3_advance must be non-null when the repo was advanced; got:\n{stdout}"
    );
    let to_sha = step3
        .get("to_sha")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("step3_advance.to_sha must be a string; got:\n{stdout}"));
    assert_eq!(
        to_sha, advance_sha,
        "step3_advance.to_sha must equal the workweave's tip (== primary server HEAD after sync)"
    );

    // --- project_repo_advance ---
    let proj_adv = obj
        .get("project_repo_advance")
        .unwrap_or_else(|| panic!("project_repo_advance must be present; got:\n{stdout}"));
    assert!(
        !proj_adv.is_null(),
        "project_repo_advance must be non-null (project repo was advanced); got:\n{stdout}"
    );
    assert!(
        proj_adv.get("from_sha").and_then(Value::as_str).is_some(),
        "project_repo_advance.from_sha must be a string; got:\n{stdout}"
    );
    assert!(
        proj_adv.get("to_sha").and_then(Value::as_str).is_some(),
        "project_repo_advance.to_sha must be a string; got:\n{stdout}"
    );
}

// ===========================================================================
// source_workweave is null when invoked from the primary weave
// ===========================================================================

#[test]
fn sync_to_json_source_workweave_is_null_from_primary() {
    // For this test, we set up two independent primary-like workspaces.
    // "source" plays the role of a primary workweave; "target" is the destination.
    // Invoking sync-to from "source" (a Weave, not a Workweave) should yield
    // source_workweave == null.
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path();

    // Source workspace (plays the role of primary / non-workweave).
    let source = parent.join("source");
    std::fs::create_dir_all(source.join("github/example")).unwrap();
    std::fs::create_dir_all(source.join("projects")).unwrap();

    let source_server = source.join(SERVER_PATH);
    let initial_sha = init_repo(&source_server);

    let source_project = source.join("projects/web-app");
    init_repo(&source_project);
    std::fs::write(
        source_project.join(".gitattributes"),
        "rwv.lock merge=ours\n",
    )
    .unwrap();
    write_manifest(&source_project, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&source_project, &[(SERVER_PATH, SERVER_URL, &initial_sha)]);
    git(
        &["add", ".gitattributes", "rwv.yaml", "rwv.lock"],
        &source_project,
    );
    git(&["commit", "-m", "lock: initial"], &source_project);
    std::fs::write(source.join(".rwv-active"), "web-app\n").unwrap();

    // Target workspace: worktrees from source repos.
    let target = parent.join("target");
    std::fs::create_dir_all(target.join("github/example")).unwrap();
    std::fs::create_dir_all(target.join("projects")).unwrap();

    let target_server = target.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            &target_server.to_string_lossy(),
            "-b",
            "web-app--target/server",
        ],
        &source_server,
    );

    let target_project = target.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            &target_project.to_string_lossy(),
            "-b",
            "web-app--target/project",
        ],
        &source_project,
    );
    std::fs::write(target.join(".rwv-active"), "web-app\n").unwrap();

    // Advance source so sync-to has work.
    let c2 = make_commit(
        &source_server,
        "source.txt",
        "source advance\n",
        "source: advance",
    );
    write_lock(&source_project, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &source_project);
    git(&["commit", "-m", "lock: source at c2"], &source_project);

    // Invoke sync-to FROM source (a Weave, not a Workweave) TO target.
    let assert = rwv()
        .args([
            "sync-to",
            &target.to_string_lossy(),
            "--strategy=ff",
            "--json",
        ])
        .current_dir(&source)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope must parse ({e}):\n{stdout}"));

    // source_workweave must be null (or missing) since we're in a primary weave.
    let source_ww = parsed.get("source_workweave");
    let is_null_or_absent = source_ww.is_none() || source_ww.map(Value::is_null).unwrap_or(false);
    assert!(
        is_null_or_absent,
        "source_workweave must be null when invoked from a primary weave; got:\n{stdout}"
    );
}

// ===========================================================================
// retired is false when --retire is not passed
// ===========================================================================

#[test]
fn sync_to_json_retired_false_without_retire_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _primary_server, _ww_server, _primary_project, _ww_project, _advance_sha) =
        make_workweave_ahead_fixture(tmp.path(), "test-ww");

    // Invoke WITHOUT --retire.
    let assert = rwv()
        .args([
            "sync-to",
            &primary.to_string_lossy(),
            "--strategy=ff",
            "--json",
        ])
        .current_dir(&ww)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope must parse ({e}):\n{stdout}"));

    let retired = parsed
        .get("retired")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("retired must be a bool; got:\n{stdout}"));
    assert!(
        !retired,
        "retired must be false when --retire was not passed; got:\n{stdout}"
    );
}

// ===========================================================================
// step3_advance is absent for repos already at the target tip (no-op advance)
// ===========================================================================

#[test]
fn sync_to_json_step3_advance_absent_for_noop_repos() {
    // Invoke sync-to when source and target are already in sync.
    // step3_advance should be absent (no-op advance).
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path();

    let primary = parent.join("primary");
    std::fs::create_dir_all(primary.join("github/example")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let primary_server = primary.join(SERVER_PATH);
    let initial_sha = init_repo(&primary_server);

    let primary_project = primary.join("projects/web-app");
    init_repo(&primary_project);
    std::fs::write(
        primary_project.join(".gitattributes"),
        "rwv.lock merge=ours\n",
    )
    .unwrap();
    write_manifest(&primary_project, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&primary_project, &[(SERVER_PATH, SERVER_URL, &initial_sha)]);
    git(
        &["add", ".gitattributes", "rwv.yaml", "rwv.lock"],
        &primary_project,
    );
    git(&["commit", "-m", "lock: initial"], &primary_project);
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    let ww = parent.join("noop-ww");
    std::fs::create_dir_all(ww.join("github/example")).unwrap();
    std::fs::create_dir_all(ww.join("projects")).unwrap();

    // Write the `.rwv-workweave` marker so resolution uses marker-based path.
    write_workweave_marker(&ww, &primary, "web-app", "noop-ww");

    let ww_server = ww.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "web-app--noop-ww/server",
        ],
        &primary_server,
    );

    let ww_project = ww.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "web-app--noop-ww/project",
        ],
        &primary_project,
    );
    std::fs::write(ww.join(".rwv-active"), "web-app\n").unwrap();

    // No additional commits in ww; primary and ww are at the same tip.
    // sync-to --strategy=ff from ww to primary: step 1 is a no-op, step 3
    // is also a no-op (already at the same tip).
    let assert = rwv()
        .args([
            "sync-to",
            &primary.to_string_lossy(),
            "--strategy=ff",
            "--json",
        ])
        .current_dir(&ww)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope must parse ({e}):\n{stdout}"));

    let outcomes = parsed
        .get("outcomes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("outcomes must be an array; got:\n{stdout}"));

    // Every outcome must either lack step3_advance entirely or have it null.
    for o in outcomes {
        let adv = o.get("step3_advance");
        let is_absent_or_null = adv.is_none() || adv.map(Value::is_null).unwrap_or(false);
        assert!(
            is_absent_or_null,
            "step3_advance must be absent or null for a no-op advance; got outcome:\n{o}"
        );
    }

    // project_repo_advance must also be absent or null.
    let proj_adv = parsed.get("project_repo_advance");
    let proj_is_absent_or_null =
        proj_adv.is_none() || proj_adv.map(Value::is_null).unwrap_or(false);
    assert!(
        proj_is_absent_or_null,
        "project_repo_advance must be absent or null for a no-op advance; got:\n{stdout}"
    );
}

/// Regression: `rwv sync-to <target> --json` from a directory that is not
/// inside any repoweave workspace must NOT panic. Before the fix, the
/// `target_path` derivation block called
/// `WorkspaceContext::resolve(cwd, None).expect("cwd must be resolvable")` as
/// its error fallback — which panicked with a backtrace instead of surfacing
/// the real "no repoweave workspace found" error (fo-wbbqof.2).
///
/// After the fix the binary must:
///   - exit non-zero,
///   - emit the normal no-workspace anyhow error on stderr,
///   - produce NO panic / backtrace output anywhere.
#[test]
fn sync_to_json_outside_workspace_no_panic() {
    // A plain temp dir is not inside any repoweave workspace.
    let tmp = tempfile::tempdir().unwrap();

    rwv()
        .args(["sync-to", "/some/nonexistent/target", "--json"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        // The real no-workspace error must appear on stderr.
        .stderr(predicate::str::contains("no repoweave workspace found"))
        // No panic or backtrace.
        .stderr(predicate::str::contains("panicked").not())
        .stderr(predicate::str::contains("RUST_BACKTRACE").not());
}
