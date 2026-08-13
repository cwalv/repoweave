//! Regression and doc-claim tests for `rwv sync-to --json` observability fields.
//!
//! Covers the new fields added:
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
//! - a resumed op's envelope, driven through the library from both sides of
//!   the op, where the three identity fields above are the only place the
//!   machine's answer and the invocation's differ.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use repoweave::sync::{sync_to_json_run, SyncRequest};
use repoweave::workspace::WorkspaceContext;
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
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), manifest_toml).unwrap();
}

fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    // Round-trip through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let entries: Vec<String> = repos
        .iter()
        .map(|(path, url, sha)| {
            format!("{path:?}: {{\"type\": \"git\", \"url\": {url:?}, \"version\": {sha:?}}}")
        })
        .collect();
    let raw = format!("{{\"repositories\": {{{}}}}}", entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
}

const SERVER_URL: &str = "https://github.com/example/server.git";
const SERVER_PATH: &str = "github/example/server";

/// Write a `.rwv-workweave` marker file into `ww_dir`.
///
/// This is necessary for `rwv` to recognise the directory as a workweave via
/// the marker-based resolution path (step 1 in `WorkspaceContext::resolve`).
/// Without it, `rwv` falls back to the `{primary}--{name}` naming convention
/// and infers the project name from the left-hand side of the directory name
/// (e.g. "primary" from `primary--sample-weave`), which doesn't match the actual
/// project "web-app".
fn write_workweave_marker(ww_dir: &Path, primary: &Path, project: &str, workweave_name: &str) {
    // The marker format is JSON: {"primary": <absolute path>, "project":
    // <project name>, "parent": <absolute path of the workspace this was
    // forked from>}
    let marker = format!(
        "{{\"primary\":\"{primary}\",\"project\":\"{project}\",\"parent\":\"{primary}\"}}",
        primary = primary.display(),
    );
    let _ = workweave_name; // stored in the dir name; marker itself doesn't need it
    std::fs::write(ww_dir.join(".rwv-workweave"), marker).unwrap();
}

/// A workspace layout with a primary weave and one workweave sharing repos via
/// `git worktree add`. The workweave is named `sample-weave` so tests can assert
/// that `source_workweave` == `"sample-weave"`.
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
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    write_manifest(&primary_project, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&primary_project, &[(SERVER_PATH, SERVER_URL, &initial_sha)]);
    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
        &primary_project,
    );
    git(&["commit", "-m", "lock: initial"], &primary_project);
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    // Workweave: materialise as git worktrees from the primary repos.
    // Name the directory simply as the workweave name (e.g. "sample-weave") so the
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
            &format!("web-app--{workweave_name}"),
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
            &format!("web-app--{workweave_name}"),
        ],
        &primary_project,
    );

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
// Acceptance criteria:
//   - `source_workweave` matches the workweave name ("sample-weave")
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
    let tmp = common::tempdir().unwrap();
    let workweave_name = "sample-weave";
    let (primary, ww, primary_server, _ww_server, _primary_project, _ww_project, advance_sha) =
        make_workweave_ahead_fixture(tmp.path(), workweave_name);
    let target_server_before = git_out(&["rev-parse", "HEAD"], &primary_server);

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
    assert_eq!(
        Path::new(target).canonicalize().ok(),
        primary.canonicalize().ok(),
        "target must name the workspace step 3 advanced; got:\n{stdout}"
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
    let from_sha = step3
        .get("from_sha")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("step3_advance.from_sha must be a string; got:\n{stdout}"));
    assert_eq!(
        from_sha, target_server_before,
        "step3_advance.from_sha must be the tip the fast-forward moved off, which \
         is the target's HEAD before the op"
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
    let tmp = common::tempdir().unwrap();
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
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    write_manifest(&source_project, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&source_project, &[(SERVER_PATH, SERVER_URL, &initial_sha)]);
    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
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
            "web-app--target",
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
            "web-app--target",
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
    let tmp = common::tempdir().unwrap();
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
// `retired` reports the delete, not the flag
// ===========================================================================

/// `--retire --json` whose retire refuses: the envelope must say the
/// workweave was not retired, because it was not.
///
/// An untracked file passes the op-start dirt preflight (tracked changes
/// only) and fails retire's delete gate (any uncommitted state), so every
/// phase up to retire lands and retire is the one that refuses. What a
/// consumer reads then has to match what is on disk: `retired: false`, and a
/// workweave still standing.
#[test]
fn sync_to_json_retired_false_when_the_retire_check_refuses() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, primary_server, ww_server, _primary_project, _ww_project, advance_sha) =
        make_workweave_ahead_fixture(tmp.path(), "refused-ww");

    std::fs::write(ww_server.join("scratch.txt"), "not committed\n").unwrap();

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
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("--retire: workweave has uncommitted changes"),
        "the fixture must reach retire and be refused there; got stderr:\n{stderr}"
    );
    // The landing itself happened, so the envelope below describes an op that
    // did everything except the retire it is being asked about.
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary_server),
        advance_sha,
        "advance-target must have landed the workweave's tip in the target"
    );
    assert!(
        ww.exists(),
        "the refused retire must leave the workweave standing"
    );

    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("the envelope must still be emitted ({e}):\n{stdout}"));
    let retired = parsed
        .get("retired")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("retired must be a bool; got:\n{stdout}"));
    assert!(
        !retired,
        "retired must be false when the workweave is still on disk; got:\n{stdout}"
    );
}

// ===========================================================================
// step3_advance is absent for repos already at the target tip (no-op advance)
// ===========================================================================

#[test]
fn sync_to_json_step3_advance_absent_for_noop_repos() {
    // Invoke sync-to when source and target are already in sync.
    // step3_advance should be absent (no-op advance).
    let tmp = common::tempdir().unwrap();
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
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    write_manifest(&primary_project, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&primary_project, &[(SERVER_PATH, SERVER_URL, &initial_sha)]);
    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
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
            "web-app--noop-ww",
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
            "web-app--noop-ww",
        ],
        &primary_project,
    );

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

/// `rwv sync-to <target> --json` from a directory that is not inside any
/// repoweave workspace refuses with the workspace-resolution error, and
/// nothing on the way there panics: no envelope field is worth resolving a
/// workspace for a second time, with an `expect` standing in for the refusal
/// the operator should be reading.
///
/// The binary must:
///   - exit non-zero,
///   - emit the normal no-workspace anyhow error on stderr,
///   - produce NO panic / backtrace output anywhere.
#[test]
fn sync_to_json_outside_workspace_no_panic() {
    // A plain temp dir is not inside any repoweave workspace.
    let tmp = common::tempdir().unwrap();

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

// ===========================================================================
// A resumed op's envelope, on the one path that reaches it
//
// `--json` and `--continue` are mutually exclusive at the CLI, so no
// invocation of the binary emits an envelope for an op it did not itself
// start. A library caller reaches one: `do_continue` sends the machine to the
// op record, whose owner workspace, target and retire flag are not what the
// request carries. Each identity field then has two candidate answers that
// differ — the machine's and the invocation's — which is what makes the
// assertions below behavioural rather than a statement about how the
// derivation is spelled.
//
// One resume is not enough to separate all three. The invocation answers
// `source_workweave` and `target` wrongly only when the caller is somewhere
// other than the owner, and answers `retired` wrongly only when it IS a
// workweave — a workweave-rooted op reporting a retire it did not run. So the
// two tests below drive the same stranded op from the two sides of it.
// ===========================================================================

const RESUMED_OP_ID: &str = "envelope-identity-resume";

/// Plant the owner record a `sync-to` leaves behind when it dies after relock:
/// owned by `workspace`, landing into `target`, retiring nothing.
fn write_sync_to_owner_record_at_advance_target(workspace: &Path, target: &Path) {
    let body = format!(
        "{{\"id\": \"{RESUMED_OP_ID}\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \
         \"phase\": \"advance-target\", \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \
         \"overrides\": [], \"started_at\": \"2026-06-10T00:00:00Z\"}}",
        src = workspace.display(),
        tgt = target.display(),
    );
    std::fs::write(workspace.join(".rwv-op"), body).unwrap();
}

/// Plant the thin lease the target side holds while the op runs.
fn write_lease(workspace: &Path, owner: &Path) {
    let body = format!(
        "{{\"id\": \"{RESUMED_OP_ID}\", \"owner\": \"{owner}\", \
         \"created_at\": \"2026-06-10T00:00:00Z\"}}",
        owner = owner.display(),
    );
    std::fs::write(workspace.join(".rwv-op-lease"), body).unwrap();
}

fn create_savepoint(repo: &Path) {
    let head = git_out(&["rev-parse", "HEAD"], repo);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{RESUMED_OP_ID}"),
            &head,
        ],
        repo,
    );
}

/// A `sync-to` op owned by the workweave and stranded at advance-target, with
/// the lease on the target. Returns `(primary, workweave)`.
fn stranded_sync_to_op(parent: &Path, workweave_name: &str) -> (PathBuf, PathBuf) {
    let (primary, ww, _primary_server, ww_server, _primary_project, ww_project, _advance_sha) =
        make_workweave_ahead_fixture(parent, workweave_name);

    write_sync_to_owner_record_at_advance_target(&ww, &primary);
    write_lease(&primary, &ww);
    create_savepoint(&ww_project);
    create_savepoint(&ww_server);

    (primary, ww)
}

/// The request a resume carries. `retire` disagrees with the record on purpose:
/// the resume path takes `project_override` and `jobs` off the request and
/// everything else off the op, so the retire phase does not run and this flag
/// is only reachable by a `retired` that reads the invocation.
fn resume_request() -> SyncRequest {
    SyncRequest {
        do_continue: true,
        retire: true,
        ..SyncRequest::default()
    }
}

/// The envelope of a run that completed, or a panic naming why it did not — a
/// fixture that drifts into a refusal must not read as a wrong field.
fn completed_envelope(run: repoweave::sync::SyncToJsonRun) -> repoweave::sync::SyncToJsonOutput {
    if let Err(e) = &run.project_level_result {
        panic!("the resumed op did not complete: {e:#}");
    }
    run.envelope
        .expect("a resumed op that ran its phases has an envelope to emit")
}

#[test]
fn resumed_sync_to_json_run_reports_the_op_not_the_invocation() {
    let tmp = common::tempdir().unwrap();
    let workweave_name = "sample-weave";
    let (primary, _ww) = stranded_sync_to_op(tmp.path(), workweave_name);

    // From the target, which holds the lease and not the op: this checkout is
    // the primary weave and owns nothing, and the request names no target.
    let invocation = WorkspaceContext::resolve(&primary, None).unwrap();
    let envelope = completed_envelope(sync_to_json_run(&invocation, resume_request()));

    assert_eq!(
        envelope.source_workweave.as_deref(),
        Some(workweave_name),
        "source_workweave must name the workweave that OWNS the op, not the \
         primary weave this resume was invoked from"
    );
    assert_eq!(
        Path::new(&envelope.target).canonicalize().ok(),
        primary.canonicalize().ok(),
        "target must be the workspace the op recorded; the request carries none"
    );
}

#[test]
fn resumed_sync_to_json_run_reports_the_retire_that_did_not_run() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = stranded_sync_to_op(tmp.path(), "sample-weave");

    // From the owner workweave, so a `retired` read off the request has
    // everything it wants: a workweave to name and a run that succeeds. The op
    // still records no retire, and no delete happened.
    let invocation = WorkspaceContext::resolve(&ww, None).unwrap();
    let envelope = completed_envelope(sync_to_json_run(&invocation, resume_request()));

    assert!(
        !envelope.retired,
        "retired must come from the delete's witness — this op records no \
         retire, whatever the request asked for"
    );
    assert!(
        ww.join(".rwv-workweave").exists(),
        "the workweave must still be here, so `retired: false` is the fact and \
         not an accident of the fixture"
    );
    assert_eq!(
        Path::new(&envelope.target).canonicalize().ok(),
        primary.canonicalize().ok(),
        "target must be the workspace the op recorded; the request carries none"
    );
}
