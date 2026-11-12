//! E2E integration tests for `rwv land`, `rwv abort`, `rwv doctor --locked`, and `rwv status`.
//!
//! These exercise the acceptance criteria for `rwv land` and `rwv abort`.
//!
//! Scenarios mirror the sync/abort how-tos in
//! docs/how-to/resume-or-abort-mid-op-sync.md and
//! docs/how-to/recover-from-sync-conflict.md.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

/// Init a git repo with one commit. Returns HEAD SHA.
fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "."]);
    commit_at_a_fixed_date(path, "initial");
    common::git_in(path, &["rev-parse", "HEAD"])
}

/// Commit what is staged at a fixed date, so two workspaces this file builds
/// from the same content hold the same commits however long building them
/// takes.
fn commit_at_a_fixed_date(path: &Path, message: &str) {
    let out = common::git()
        .args(["commit", "-m", message])
        .current_dir(path)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Write a file, stage, commit. Returns new HEAD SHA.
fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// Write an rwv.toml manifest into `project_dir`.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)], integrations: Option<&str>) {
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ));
    }
    if let Some(int) = integrations {
        manifest_toml.push('\n');
        manifest_toml.push_str(int);
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
}

fn rwv() -> AssertCommand {
    common::rwv()
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

const SERVER_URL: &str = "https://github.com/chatly/server.git";
const SERVER_PATH: &str = "github/chatly/server";

/// Build a workspace:
///   root/
///     github/chatly/server/   (git repo, initial commit)
///     projects/web-app/       (git repo, rwv.toml + rwv.lock committed)
///
/// Both workspaces share no objects — independent repos. Good for precondition
/// tests where the error fires before any cross-workspace object access.
fn make_locked_workspace(parent: &Path, name: &str) -> (Workspace, String) {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server_dir = root.join(SERVER_PATH);
    let sha = init_repo(&server_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    // Mirror what `rwv init` writes: `.gitattributes` so `rwv land`'s
    // native rebase keeps source's rwv.lock through the replay (the
    // `merge=rwv-ours` driver).
    common::set_up_lock_driver(&project_dir);
    write_manifest(&project_dir, &[(SERVER_PATH, SERVER_URL)], None);
    common::fixture_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &sha)]);
    common::git_in(
        &project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    commit_at_a_fixed_date(&project_dir, "lock: initial");
    common::record_trunk(&project_dir);

    // Action verbs require `.rwv-active` (or --project). Set it here so
    // sync tests don't all need to pass --project.
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    (
        Workspace {
            root,
            project_dir,
            server_dir,
        },
        sha,
    )
}

/// Build two workspaces whose server repos share objects via a git worktree.
///
/// Layout:
///   parent/primary/                          (primary workspace)
///     github/chatly/server/                  (git repo, initial commit C1)
///     projects/web-app/                      (git repo, lock@C1 committed)
///   parent/ww/                               (workweave workspace)
///     github/chatly/server/                  (git worktree of primary's server, on ww/main@C1)
///     projects/web-app/                      (git worktree of primary's project, on ww/project@lock@C1)
///
/// Returns (primary, workweave, shared_c1_sha).
fn make_shared_workspaces(parent: &Path) -> (Workspace, Workspace, String) {
    let (primary, c1) = make_locked_workspace(parent, "primary");

    let ww_root = parent.join("ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let ww_server = ww_root.join(SERVER_PATH);
    common::git_in(
        &primary.server_dir,
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );

    let ww_project = ww_root.join("projects/web-app");
    common::git_in(
        &primary.project_dir,
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );
    common::record_feature(&ww_project, "ww/project", "main");
    // The worktree inherits primary's already-committed rwv.lock (same C1 SHA).
    // No additional commit needed.
    // `make_shared_workspaces` builds two PRIMARY-shaped workspaces (see the
    // marker-based fixtures below) — despite the name, `ww` carries no
    // `.rwv-workweave` marker, so its project comes from the pointer like
    // any primary's.
    std::fs::write(ww_root.join(".rwv-active"), "web-app\n").unwrap();

    let ww = Workspace {
        root: ww_root,
        project_dir: ww_project,
        server_dir: ww_server,
    };
    (primary, ww, c1)
}

const API_URL: &str = "https://github.com/chatly/api.git";
const API_PATH: &str = "github/chatly/api";

/// Two workspaces sharing TWO manifest repos, so one op can reach two
/// different per-repo verdicts. Only the source's project repo declares the
/// second repo, which leaves CWD's project a strict ancestor of the source's
/// and keeps the Phase 1 ancestor gate out of the way.
///
/// Returns `(primary, ww, c1, ww_api)`.
fn make_shared_workspaces_two_repos(parent: &Path) -> (Workspace, Workspace, String, PathBuf) {
    let (primary, ww, c1) = make_shared_workspaces(parent);

    let primary_api = primary.root.join(API_PATH);
    let api_c1 = init_repo(&primary_api);
    let ww_api = ww.root.join(API_PATH);
    common::git_in(
        &primary_api,
        &[
            "worktree",
            "add",
            &ww_api.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );

    write_manifest(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL), (API_PATH, API_URL)],
        None,
    );
    common::fixture_lock(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL, &c1), (API_PATH, API_URL, &api_c1)],
    );
    common::git_in(&primary.project_dir, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: add api"]);

    (primary, ww, c1, ww_api)
}

// ---------------------------------------------------------------------------
// Smoke tests — command recognition
// ---------------------------------------------------------------------------

#[test]
fn sync_subcommand_is_recognized() {
    let out = rwv().args(["sync", "--help"]).assert();
    let output = out.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unrecognized subcommand") && !stderr.contains("unexpected argument"),
        "`rwv sync --help` should be recognized; got stderr: {stderr}"
    );
}

#[test]
fn abort_subcommand_is_recognized() {
    let out = rwv().args(["abort", "--help"]).assert();
    let output = out.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("unrecognized subcommand") && !stderr.contains("unexpected argument"),
        "`rwv abort --help` should be recognized; got stderr: {stderr}"
    );
}

#[test]
fn sync_outside_a_workspace_errors() {
    // Outside any workspace, bare `rwv sync` fails loudly rather than
    // silently doing nothing.
    let tmp = common::tempdir().unwrap();
    rwv().arg("sync").current_dir(tmp.path()).assert().failure();
}

// ---------------------------------------------------------------------------
// rwv doctor --locked
// ---------------------------------------------------------------------------

#[test]
fn check_locked_passes_when_lock_matches_head() {
    let tmp = common::tempdir().unwrap();
    let (ws, _) = make_locked_workspace(tmp.path(), "primary");
    rwv()
        .args(["doctor", "--locked"])
        .current_dir(&ws.root)
        .assert()
        .success();
}

#[test]
fn check_locked_fails_when_repo_has_advanced_past_lock() {
    let tmp = common::tempdir().unwrap();
    let (ws, _) = make_locked_workspace(tmp.path(), "primary");

    // Advance server past the locked SHA without updating rwv.lock.
    make_commit(&ws.server_dir, "extra.txt", "extra\n", "advance past lock");

    rwv()
        .args(["doctor", "--locked"])
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stdout(predicate::str::contains(SERVER_PATH));
}

// ---------------------------------------------------------------------------
// rwv status
// ---------------------------------------------------------------------------

#[test]
fn status_shows_per_repo_state() {
    let tmp = common::tempdir().unwrap();
    let (ws, _) = make_locked_workspace(tmp.path(), "primary");
    rwv()
        .arg("status")
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains(SERVER_PATH));
}

#[test]
fn status_json_flag_produces_machine_readable_output() {
    let tmp = common::tempdir().unwrap();
    let (ws, _) = make_locked_workspace(tmp.path(), "primary");
    let assert = rwv()
        .args(["status", "--json"])
        .current_dir(&ws.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Envelope shape: { "$schema": "...", "repos": [...] }
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not parseable as JSON ({e}):\n{stdout}"));
    let obj = parsed.as_object().expect("top level should be object");
    assert_eq!(
        obj.get("$schema").and_then(serde_json::Value::as_str),
        Some(repoweave::status::STATUS_SCHEMA_URL),
        "envelope must include $schema URL"
    );
    let repos = obj
        .get("repos")
        .and_then(serde_json::Value::as_array)
        .expect("repos should be an array");
    assert!(!repos.is_empty(), "repos should not be empty");

    // Per-repo content checks (pre-migration assertions, now on `.repos[]`).
    let server = repos
        .iter()
        .find(|r| r.get("path").and_then(serde_json::Value::as_str) == Some("github/chatly/server"))
        .expect("server repo entry");
    assert_eq!(
        server.get("role").and_then(serde_json::Value::as_str),
        Some("owned")
    );
    assert_eq!(
        server.get("url").and_then(serde_json::Value::as_str),
        Some("https://github.com/chatly/server.git")
    );
    assert_eq!(
        server.get("project").and_then(serde_json::Value::as_str),
        Some("web-app")
    );
    assert!(server.get("absolute_path").is_some());
}

// ---------------------------------------------------------------------------
// rwv land — fast-forward path (shared object store via worktrees)
// ---------------------------------------------------------------------------

/// Tutorial scenario: workweave finishes work → `rwv lock` → `rwv land` from the workweave.
#[test]
fn sync_to_ff_primary_advances_to_workweave_lock() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Workweave: make commit C2, update lock.
    let c2 = make_commit(
        &ww.server_dir,
        "change.txt",
        "workweave change\n",
        "ww: add change",
    );
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww change"]);

    // From primary: sync from the workweave.
    rwv()
        .args(["land"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Primary's server `main` branch should now be at C2.
    let primary_head = common::git_in(&primary.server_dir, &["rev-parse", "main"]);
    assert_eq!(
        primary_head, c2,
        "primary server/main should be at C2 after sync from workweave"
    );
}

/// Tutorial scenario: primary has advanced → the workweave's `rwv land primary` catches up.
#[test]
fn sync_to_ff_is_symmetric_workweave_catches_up_to_primary() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2, update lock.
    let c2 = make_commit(
        &primary.server_dir,
        "upstream.txt",
        "upstream change\n",
        "primary: upstream advance",
    );
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &primary.project_dir,
        &["commit", "-m", "lock: upstream advance"],
    );

    rwv()
        .args(["land"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Workweave's ww/main branch (inside the shared clone) should be at C2.
    let ww_head = common::git_in(&primary.server_dir, &["rev-parse", "ww/project"]);
    assert_eq!(
        ww_head, c2,
        "ww/main should be at C2 after syncing workweave from primary"
    );
}

// ---------------------------------------------------------------------------
// rwv land — project ancestry precondition
// ---------------------------------------------------------------------------
//
// Under `--strategy ff` CWD's project tip must be strictly ahead of (or equal
// to) the target's; `--strategy rebase` replays CWD onto the target instead.

/// Helper: advance primary's project repo by one commit (no-op edit) and re-lock.
/// Leaves primary's lock fresh.
fn primary_advance_project_one_commit(primary: &Workspace, server_sha: &str) {
    common::fixture_lock(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL, server_sha)],
    );
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &primary.project_dir,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "lock: primary project advance",
        ],
    );
}

/// Under `--strategy ff`, land refuses when the target's project tip is not
/// an ancestor of CWD's: the target is ahead of CWD.
#[test]
fn sync_to_ff_refuses_when_target_project_repo_is_ahead_of_cwd() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    primary_advance_project_one_commit(&primary, &c1);

    let assertion = rwv()
        .args(["land", "--strategy", "ff"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("needs this feature to be ahead of its target")
            && stderr.contains("does not contain the target's tip"),
        "expected refusal naming the ancestry relation; got: {stderr}"
    );
    assert!(
        stderr.contains("--strategy rebase"),
        "expected refusal to name the rebase strategy; got: {stderr}"
    );
}

/// Under `--strategy ff`, land refuses when CWD's project tip and the
/// target's have diverged (neither is an ancestor of the other).
#[test]
fn sync_to_ff_refuses_when_target_project_repo_has_diverged_from_cwd() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    primary_advance_project_one_commit(&primary, &c1);

    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c1)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &ww.project_dir,
        &["commit", "--allow-empty", "-m", "lock: ww project advance"],
    );

    let primary_tip = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);
    let ww_tip = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    assert_ne!(primary_tip, ww_tip);

    rwv()
        .args(["land", "--strategy", "ff"])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "needs this feature to be ahead of its target",
        ));
}

/// Forward sync (target project tip a strict ancestor of CWD's): allowed.
#[test]
fn sync_to_ff_allows_when_target_project_repo_is_strict_ancestor_of_cwd() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c1)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &ww.project_dir,
        &["commit", "--allow-empty", "-m", "lock: ww project advance"],
    );

    rwv()
        .args(["land", "--strategy", "ff"])
        .current_dir(&ww.root)
        .assert()
        .success();

    let ww_tip = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    let primary_tip = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        primary_tip, ww_tip,
        "the target's project must land on CWD's tip"
    );
}

/// Equal tips: both project repos at the same SHA — no-op, allowed.
#[test]
fn sync_to_ff_allows_when_target_project_repo_equals_cwd() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let primary_tip = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);
    let ww_tip = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    assert_eq!(primary_tip, ww_tip);

    rwv()
        .args(["land", "--strategy", "ff"])
        .current_dir(&ww.root)
        .assert()
        .success();
}

/// The ff refusal names `--strategy=rebase` as the rwv-native path to land
/// diverging project commits.
#[test]
fn sync_to_ff_refusal_message_suggests_rebase_strategy() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    primary_advance_project_one_commit(&primary, &c1);

    let assertion = rwv()
        .args(["land", "--strategy", "ff"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("--strategy rebase"),
        "expected refusal to suggest --strategy rebase; got: {stderr}"
    );
}

/// What ff refuses lands cleanly under `--strategy rebase`: CWD replays onto
/// the target's tip and the target then fast-forwards to the converged tip.
#[test]
fn sync_to_rebase_lands_lock_only_divergence() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    primary_advance_project_one_commit(&primary, &c1);
    let primary_tip_before = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);

    rwv()
        .args(["land", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .success();

    let primary_tip_after = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);
    let ww_tip_after = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        primary_tip_after, primary_tip_before,
        "ww carried no project work of its own, so the target's tip must not move"
    );
    assert_eq!(
        ww_tip_after, primary_tip_after,
        "after the rebase, ww's project tip should equal the target's"
    );
}

/// The refusal's recovery works: replaying onto the target first (rebase)
/// unblocks the ff landing that was refused.
#[test]
fn sync_to_rebase_first_unblocks_a_refused_ff() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    primary_advance_project_one_commit(&primary, &c1);

    rwv()
        .args(["land", "--strategy", "ff"])
        .current_dir(&ww.root)
        .assert()
        .failure();

    rwv()
        .args(["land", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .success();

    rwv()
        .args(["land", "--strategy", "ff"])
        .current_dir(&ww.root)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// rwv land — error message structure
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// rwv land — non-ff strategies
// ---------------------------------------------------------------------------

/// When CWD has local commits on top of an older base, --strategy rebase replays them.
#[test]
fn sync_to_rebase_replays_local_commits_on_source_tip() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2, update lock.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: advance",
    );
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &primary.project_dir,
        &["commit", "-m", "lock: primary advance"],
    );

    // Workweave: add a commit C_ww on top of C1 (before primary's C2).
    let c_ww = make_commit(
        &ww.server_dir,
        "ww_feature.txt",
        "ww feature\n",
        "ww: local commit",
    );
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww feature"]);

    // ww/main (C_ww) and primary main (C2) have both diverged from C1.
    // --strategy rebase should replay C_ww onto C2.
    //
    // Both sides also independently committed locks → ww and primary's project
    // repos diverged too. Phase 1's ancestor precondition refuses divergent
    // project repos; this test deliberately constructs that to exercise Phase
    // 2's rebase strategy on the manifest repo, so it opts in via
    rwv()
        .args(["land", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // After rebase, ww/main should be a descendant of C2.
    let ww_head = common::git_in(&primary.server_dir, &["rev-parse", "ww/project"]);
    let is_descendant = common::git()
        .args(["merge-base", "--is-ancestor", &c2, &ww_head])
        .current_dir(&primary.server_dir)
        .status()
        .unwrap()
        .success();
    assert!(
        is_descendant,
        "after rebase, ww/main ({ww_head}) should be a descendant of primary C2 ({c2})"
    );

    // History-shape assertion: after the rebase, ww's local commit must sit ON
    // TOP of primary's C2 commit in the manifest repo log. The descendant-check
    // above only verifies reachability; this verifies the actual commit ordering
    // (CWD's commit replayed above, not below, the source's base).
    //
    // We check the manifest repo (server) log at the ww/main branch.
    // "ww: local commit" must appear above "primary: advance".
    common::assert_log_ordering(&ww.server_dir, &["ww: local commit", "primary: advance"]);
}

// ---------------------------------------------------------------------------
// rwv abort
// ---------------------------------------------------------------------------

/// abort fails with a clear message when no sync operation is in progress.
#[test]
fn abort_fails_gracefully_when_no_op_in_progress() {
    let tmp = common::tempdir().unwrap();
    let (ws, _) = make_locked_workspace(tmp.path(), "primary");
    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("no operation in progress")
                .or(predicate::str::contains("nothing to abort")),
        );
}

/// After a conflicted rebase, abort restores every repo to its pre-sync state.
#[test]
fn abort_restores_repos_to_pre_op_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Both sides make conflicting changes to the same file.
    let c_primary = make_commit(
        &primary.server_dir,
        "shared.txt",
        "primary version\n",
        "primary: conflict candidate",
    );
    common::fixture_lock(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL, &c_primary)],
    );
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: primary"]);

    let c_ww = make_commit(
        &ww.server_dir,
        "shared.txt",
        "ww version\n",
        "ww: conflict candidate",
    );
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww"]);

    // Record ww's server tip before the attempted sync.
    let pre_op_sha = common::git_in(&primary.server_dir, &["rev-parse", "ww/project"]);
    assert_eq!(pre_op_sha, c_ww);

    // Attempt rebase sync — should hit a conflict and leave repos mid-op.
    // Project repos diverged from independent lock commits.
    // is required to reach Phase 2 where the rebase conflict is the focus
    // (adapted from --force).
    let _ = rwv()
        .args(["land", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert();

    // Abort should restore ww/main back to pre-op state.
    rwv().arg("abort").current_dir(&ww.root).assert().success();

    let post_abort_sha = common::git_in(&primary.server_dir, &["rev-parse", "ww/project"]);
    assert_eq!(
        post_abort_sha, pre_op_sha,
        "abort should restore ww/main to pre-op SHA {pre_op_sha}; got {post_abort_sha}"
    );
}

/// Regression: `rwv abort` must succeed when `rwv.lock` contains git
/// conflict markers (the loader was once too strict to recover).
///
/// Setup mirrors the real-world scenario: a sync left the project repo
/// mid-rebase and `rwv.lock` contains conflict markers. The operator's only
/// viable escape should be `rwv abort`.
///
/// Strategy: use `make_locked_workspace` to get a valid project repo, then
/// manually plant the `.rwv-op` op-state file, a savepoint ref, a mid-rebase
/// state, and conflict markers in `rwv.lock` — then drive `rwv abort` and
/// assert exit 0 + clean git state (`.git/rebase-merge/` gone, working tree
/// clean).
#[test]
fn abort_succeeds_when_rwv_lock_contains_conflict_markers() {
    let tmp = common::tempdir().unwrap();
    let (ws, _server_sha) = make_locked_workspace(tmp.path(), "primary");
    let project_dir = &ws.project_dir;

    // Capture the project repo's HEAD (the "lock: initial" commit). The SHA
    // returned by make_locked_workspace is the *server* repo's commit, which
    // doesn't exist in the project repo's history.
    let sha = common::git_in(project_dir, &["rev-parse", "HEAD"]);

    // Make a second commit so we have something to rebase onto.
    let sha2 = make_commit(project_dir, "extra.txt", "extra\n", "extra commit");

    // Write a v2 owner record so `rwv abort` thinks an op is in progress —
    // through the real serializer, so a field the type gains later can't
    // silently vanish from this fixture.
    let op_id = repoweave::op_state::OpId::new_now();
    let owner = repoweave::op_state::OwnerRecord::new_sync(
        &op_id,
        repoweave::op_state::SyncStrategy::Rebase,
        repoweave::manifest::ProjectName::new("web-app").unwrap(),
        &ws.root,
        &ws.root,
    );
    let op_id = owner.id.clone();
    repoweave::op_state::write_owner(&ws.root, &owner).unwrap();

    // Create the savepoint ref pointing at sha2 (the pre-op HEAD).
    common::git_in(
        project_dir,
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), &sha2],
    );

    // Manufacture a mid-rebase state: create a diverging commit on a temp
    // branch, then start a rebase that will conflict.
    //
    // We need two commits that touch the same line so git rebase stalls.
    // `sha` is the initial commit. We'll:
    //   1. Create branch `diverge` at `sha` with conflict content in conflict.txt
    //   2. Create a commit on main (after sha2) with different conflict.txt content
    //   3. git rebase diverge onto current HEAD → conflict on conflict.txt
    //   4. Rebase will stop mid-way, leaving .git/rebase-merge/.

    // Step 1: make a conflicting commit on the current branch (after sha2).
    make_commit(
        project_dir,
        "conflict.txt",
        "main version\n",
        "main: conflict base",
    );

    // Step 2: create branch `diverge` starting from sha (before sha2), add conflicting file.
    common::git_in(project_dir, &["checkout", "-b", "diverge", &sha]);
    make_commit(
        project_dir,
        "conflict.txt",
        "diverge version\n",
        "diverge: conflict",
    );

    // Step 3: return to main, start rebase of diverge onto main — this will conflict.
    common::git_in(project_dir, &["checkout", "main"]);
    // git rebase may fail; we just need it to leave the rebase-merge dir.
    let _ = std::process::Command::new("git")
        .args(["rebase", "diverge"])
        .current_dir(project_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output();

    // Verify we're in mid-rebase state (the test harness only proceeds if so).
    let rebase_merge = project_dir.join(".git/rebase-merge");
    assert!(
        rebase_merge.exists(),
        "expected mid-rebase state (.git/rebase-merge should exist)"
    );

    // raw lock bytes: a lock carrying 3-way merge markers, which is what a
    // rebase that stopped on it leaves behind and what `rwv abort` must
    // survive. No writer that parses before it writes can produce one.
    std::fs::write(
        project_dir.join("rwv.lock"),
        "<<<<<<< HEAD\n[repositories]\n=======\n[repositories]\n>>>>>>> abc1234\n",
    )
    .unwrap();

    // Run `rwv abort` — must exit 0 despite the malformed rwv.lock.
    rwv().arg("abort").current_dir(&ws.root).assert().success();

    // Assert the rebase state is gone.
    assert!(
        !rebase_merge.exists(),
        "after abort, .git/rebase-merge/ should be gone"
    );

    // Assert the working tree is clean (no uncommitted changes).
    let status = common::git_in(project_dir, &["status", "--porcelain"]);
    assert!(
        status.is_empty(),
        "after abort, working tree should be clean; got: {status}"
    );
}

// ---------------------------------------------------------------------------
// Round-trip convergence
// ---------------------------------------------------------------------------
// rwv land — tag-form lock freshness (mirrors check_test.rs §8)
// ---------------------------------------------------------------------------

/// Add a lightweight tag at HEAD in `repo`.
fn git_tag(repo: &Path, tag: &str) {
    common::git_in(
        repo,
        &[
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=Test",
            "tag",
            tag,
        ],
    );
}

/// sync proceeds when the source lock is pinned by a tag that resolves to the source HEAD.
/// Regression test: before the fix, this spuriously produced "source lock is stale".
#[test]
fn sync_to_proceeds_when_source_lock_tag_matches_head() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Tag primary's server at its current HEAD and update the source lock to the tag name.
    git_tag(&primary.server_dir, "v1.0.0");
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, "v1.0.0")]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &primary.project_dir,
        &["commit", "-m", "lock: pin v1.0.0 (tag form)"],
    );

    rwv()
        .args(["land"])
        .current_dir(&ww.root)
        .assert()
        .success();
}

/// sync proceeds when the source lock is pinned by a SHA that equals the source HEAD.
/// Regression guard: SHA-form should always have worked; this ensures it still does.
#[test]
fn sync_to_proceeds_when_source_lock_sha_matches_head() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, c1) = make_shared_workspaces(tmp.path());

    // primary's project already has the lock with C1 SHA committed.
    // Syncing ww from primary is a no-op; the freshness check must pass.
    let source_lock_sha = c1; // the SHA already in the lock
    assert!(!source_lock_sha.is_empty());

    rwv()
        .args(["land"])
        .current_dir(&ww.root)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------

/// sync A→B then B→A should be a no-op on B (project repo must not grow unbounded).
#[test]
fn sync_to_roundtrip_converges_without_project_repo_growth() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Advance primary to C2, update lock.
    let c2 = make_commit(
        &primary.server_dir,
        "advance.txt",
        "advance\n",
        "primary: advance",
    );
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: advance"]);

    let count_before: usize =
        common::git_in(&primary.project_dir, &["rev-list", "--count", "HEAD"])
            .parse()
            .unwrap();

    // Sync primary → workweave (workweave catches up to C2).
    rwv()
        .args(["land"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Sync workweave → primary (now a no-op: ww is at C2, primary is already at C2).
    rwv()
        .args(["land"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Primary's project repo commit count must not have grown.
    let count_after: usize = common::git_in(&primary.project_dir, &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap();
    assert_eq!(
        count_after, count_before,
        "no-op round-trip sync must not add commits to the project repo (auto-relock idempotence)"
    );
}

// ---------------------------------------------------------------------------
// Marker-based workweave fixtures and regression tests
// ---------------------------------------------------------------------------
//
// `make_shared_workspaces` produces two workspaces that both look like primary
// weaves to `WorkspaceContext::resolve` (no `.rwv-workweave` marker, no
// `{primary}--{name}` naming). The marker-based fixtures below mirror what
// `rwv workweave create` produces in production and exercise the
// lock-cross-workspace-confusion code paths: lock writes and
// freshness checks must use the workweave's own `projects/<name>/rwv.lock`,
// not primary's, when CWD is inside the workweave.

/// Workspaces produced by `make_marker_workweave`.
struct MarkerSharedWorkspaces {
    primary: Workspace,
    /// The workweave's root directory (with `.rwv-workweave` marker).
    ww_root: PathBuf,
    /// The workweave's project worktree (`<ww>/projects/web-app`), on its own branch.
    ww_project_dir: PathBuf,
    /// The workweave's server worktree (`<ww>/github/chatly/server`), on its own branch.
    ww_server_dir: PathBuf,
}

/// Mirror what `rwv workweave create` produces: a workweave directory under
/// `parent/.workweaves/<primary-name>--<ww-name>/` with a `.rwv-workweave`
/// marker, with each repo (and the project repo) as a worktree of primary's
/// on a per-workweave ephemeral branch.
fn make_marker_workweave(parent: &Path, ww_name: &str) -> MarkerSharedWorkspaces {
    let (primary, _c1) = make_locked_workspace(parent, "primary");
    let ww_root = parent
        .join(".workweaves")
        .join(format!("primary--{ww_name}"));
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let ww_server = ww_root.join(SERVER_PATH);
    common::git_in(
        &primary.server_dir,
        &[
            "worktree",
            "add",
            "-b",
            &format!("primary--{ww_name}/project"),
            &ww_server.to_string_lossy(),
        ],
    );

    let ww_project = ww_root.join("projects/web-app");
    common::git_in(
        &primary.project_dir,
        &[
            "worktree",
            "add",
            "-b",
            &format!("primary--{ww_name}/project"),
            &ww_project.to_string_lossy(),
        ],
    );
    common::record_feature(&ww_project, &format!("primary--{ww_name}/project"), "main");

    let primary_canon = repoweave::path_spelling::resolve(&primary.root)
        .unwrap()
        .display()
        .to_string();
    let marker = common::workweave_marker(&primary_canon, "web-app", &primary_canon);
    std::fs::write(ww_root.join(".rwv-workweave"), marker).unwrap();
    common::register_workweave(&primary_canon, "web-app", ww_name, &ww_root);

    MarkerSharedWorkspaces {
        primary,
        ww_root,
        ww_project_dir: ww_project,
        ww_server_dir: ww_server,
    }
}

/// Anomaly B regression: `rwv lock` running inside a marker-based workweave
/// must write to the workweave's own `projects/<name>/rwv.lock`, leaving
/// primary's lock file content untouched.
#[test]
fn lock_in_marker_workweave_does_not_mutate_primary_lock_file() {
    let tmp = common::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Advance the workweave's server tip past the inherited C1 lock.
    let c2 = make_commit(&ws.ww_server_dir, "ww-only.txt", "ww only\n", "ww: advance");

    // Snapshot primary's lock file content before running lock in the workweave.
    let primary_lock_path = ws.primary.project_dir.join("rwv.lock");
    let primary_lock_before = std::fs::read_to_string(&primary_lock_path).unwrap();

    rwv()
        .arg("lock")
        .current_dir(&ws.ww_root)
        .assert()
        .success();

    // Primary's lock file content must be unchanged on disk.
    let primary_lock_after = std::fs::read_to_string(&primary_lock_path).unwrap();
    assert_eq!(
        primary_lock_before, primary_lock_after,
        "`rwv lock` from a workweave must not mutate primary's rwv.lock content"
    );

    // Workweave's lock must contain the workweave's tip (C2), not primary's (C1).
    let ww_lock_path = ws.ww_project_dir.join("rwv.lock");
    let ww_lock = repoweave::manifest::LockFile::from_path(&ww_lock_path).unwrap();
    let entry = ww_lock
        .get_entry(
            &repoweave::manifest::RepoPath::new(SERVER_PATH)
                .expect("SERVER_PATH is a forward-slash constant"),
        )
        .expect("workweave lock should contain server entry");
    assert_eq!(
        entry.version.as_str(),
        &c2,
        "workweave lock entry must reflect workweave's tip (C2), not primary's (C1)"
    );
}

/// Anomaly A regression: when locks diverge between primary and a marker-based
/// workweave, `rwv land`'s "CWD lock" freshness check must read the workweave's
/// own committed lock — not primary's — to determine staleness from the
/// operator's perspective.
#[test]
fn sync_to_in_marker_workweave_uses_workweave_own_lock_for_cwd_freshness() {
    let tmp = common::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Primary advances to C2 and commits the C2 lock on its main branch.
    let c2 = make_commit(
        &ws.primary.server_dir,
        "primary-advance.txt",
        "primary advance\n",
        "primary: C2",
    );
    common::fixture_lock(&ws.primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ws.primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &ws.primary.project_dir,
        &["commit", "-m", "lock: primary advance"],
    );

    // Workweave's own server tip is still at C1, and its committed lock is
    // still at C1 (inherited from the worktree-creation snapshot of project
    // repo @ C1). From the operator's perspective in the workweave, "CWD lock
    // is fresh" — workweave-tip C1 == workweave-lock C1 — even though primary
    // has diverged.
    rwv()
        .args(["land"])
        .current_dir(&ws.ww_root)
        .assert()
        .success();

    // After sync the workweave's server tip should have caught up to C2.
    let ww_server_head = common::git_in(&ws.ww_server_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        ww_server_head, c2,
        "workweave server should be at C2 after sync from primary"
    );
}

/// Symmetric regression: `rwv land` into primary from a marker-based workweave
/// as source must read the workweave's lock for `source` freshness — not
/// primary's lock interpreted as the source's.
#[test]
fn sync_to_from_primary_with_marker_workweave_source_uses_source_own_lock() {
    let tmp = common::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Workweave advances to C2 and commits the C2 lock on its own project branch.
    let c2 = make_commit(
        &ws.ww_server_dir,
        "ww-advance.txt",
        "ww advance\n",
        "ww: C2",
    );
    common::fixture_lock(&ws.ww_project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ws.ww_project_dir, &["add", "rwv.lock"]);
    common::git_in(&ws.ww_project_dir, &["commit", "-m", "lock: ww advance"]);

    // Primary syncs from the workweave. Source freshness must be checked
    // against the workweave's own lock (C2 vs ww-tip C2), not primary's lock.
    rwv()
        .args(["land"])
        .current_dir(&ws.ww_root)
        .assert()
        .success();

    // Primary's server should have advanced to C2 (the workweave's lock target).
    let primary_server_head = common::git_in(&ws.primary.server_dir, &["rev-parse", "HEAD"]);
    assert_eq!(
        primary_server_head, c2,
        "primary server should be at C2 after sync from workweave"
    );
}

/// Anomaly A precondition guard: when the workweave's own committed lock is
/// genuinely stale (workweave-tip ≠ workweave-lock), sync from the workweave
/// must refuse with a stale-lock error naming the destination workspace —
/// not silently pass by reading primary's lock.
#[test]
fn sync_to_in_marker_workweave_refuses_when_workweave_own_lock_is_stale() {
    let tmp = common::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Drive the workweave's own lock ANOMALOUS: advance to C2, relock, then
    // reset the worktree back, so the workweave's lock records a commit its
    // HEAD lacks. Primary is left at C1 with a matching (ok) lock, so this
    // still refuses only if the freshness check reads the WORKWEAVE's lock —
    // reading primary's would see `ok` and pass. A lock merely behind HEAD is
    // now benign on a pull, so it can no longer serve as that discriminator.
    let ww_c1 = common::git_in(&ws.ww_server_dir, &["rev-parse", "HEAD"]);
    make_commit(
        &ws.ww_server_dir,
        "ww-advance.txt",
        "ww advance\n",
        "ww: advance past lock",
    );
    rwv()
        .args(["lock", "--commit", "--project", "web-app"])
        .current_dir(&ws.ww_root)
        .assert()
        .success();
    common::git_in(&ws.ww_server_dir, &["reset", "--hard", &ww_c1]);

    rwv()
        .args(["land"])
        .current_dir(&ws.ww_root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("pin backwards"));
}

// ---------------------------------------------------------------------------
// RepoSyncOutcome — per-repo status reporting
// ---------------------------------------------------------------------------

/// Regression: CWD is ahead of the lock target → sync reports `already-ahead`, not bare `ok`.
///
/// Repro from SME triage: after `rwv land`, the per-repo line said "ok" even
/// though HEAD didn't move to the lock SHA, leaving `rwv status` showing [ahead].
#[test]
fn sync_to_reports_already_ahead_when_cwd_is_past_lock_target() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2, lock at C2.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: C2"]);

    // Workweave: fast-forward to C2, then add C3 (ww is now ahead of primary's lock).
    common::git_in(&ww.server_dir, &["merge", "--ff-only", "main"]);
    let c3 = make_commit(
        &ww.server_dir,
        "ww-extra.txt",
        "ww extra\n",
        "ww: C3 (ahead of C2)",
    );
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c3)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: C3"]);

    // Land ww: target is C2, but ww is at C3 (C2 is ancestor of C3).
    let out = rwv()
        .args(["land"])
        .current_dir(&ww.root)
        .assert()
        .success()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("already-ahead"),
        "sync should report already-ahead when CWD is past the lock target; got stdout: {stdout}"
    );
    assert!(
        !stdout.contains(": ok"),
        "sync must not report bare 'ok' for already-ahead case; got stdout: {stdout}"
    );
    // The state the condition proved is containment. A rebase onto a baseline
    // HEAD already contains changes nothing, and an operator who is told to
    // run one spends the attempt discovering that.
    assert!(
        !stdout.contains("rebase") && !stdout.contains("diverge"),
        "a strictly-ahead pull must not advise a rebase or call itself a \
         divergence; got stdout: {stdout}"
    );
    assert!(
        stdout.contains("strictly ahead of the source lock entry"),
        "the line must name the baseline HEAD is ahead of; got stdout: {stdout}"
    );
}

/// Two repos in one op reach two different verdicts. Each per-repo line
/// carries its own, and the summary that follows names no cause at all — a
/// single-cause sentence is wrong for at least one of the repos it covers, and
/// the operator reads the summary first.
#[test]
fn mixed_per_repo_verdicts_are_reported_per_repo_with_no_single_cause_summary() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1, ww_api) = make_shared_workspaces_two_repos(tmp.path());

    // server: primary advances, ww advances differently → diverged.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    let api_c1 = common::git_in(primary.root.join(API_PATH), &["rev-parse", "HEAD"]);
    common::fixture_lock(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL, &c2), (API_PATH, API_URL, &api_c1)],
    );
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: C2"]);
    assert_ne!(
        c1, c2,
        "fixture precondition: the target's server tip moved"
    );

    // ww's project contains the target's project tip, so the ff gate on the
    // project repo passes and the verdicts are the member repos' own.
    common::git_in(&ww.project_dir, &["merge", "--ff-only", "main"]);
    let ww_server = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: diverged from C1");

    // api: only ww advances → ww strictly contains the target's lock entry.
    let ww_api_tip = make_commit(&ww_api, "ww-api.txt", "ww api\n", "ww: api ahead");
    common::fixture_lock(
        &ww.project_dir,
        &[
            (SERVER_PATH, SERVER_URL, &ww_server),
            (API_PATH, API_URL, &ww_api_tip),
        ],
    );
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww tips"]);

    let out = rwv()
        .args(["land", "--strategy", "ff"])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();

    let both = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        both.contains("github/chatly/server: HEAD is")
            && both.contains("rwv explain not-fast-forwardable"),
        "the diverged repo's line must carry its own verdict; got:\n{both}"
    );
    assert!(
        !both.contains("github/chatly/api: HEAD is"),
        "the contained repo needs no composing and is not named; got:\n{both}"
    );
    assert!(
        !both.contains("concurrent modification"),
        "the refusal must not assert a cause the engine did not observe; got:\n{both}"
    );
}

/// Regression: diverged repo + `--strategy ff` must still report `failed (cannot fast-forward...)`.
/// Verifies the failure message structure carries through the new outcome enum unchanged.
#[test]
fn sync_to_ff_reports_failed_for_diverged_repo() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: C2"]);
    common::git_in(&ww.project_dir, &["merge", "--ff-only", "main"]);

    // Workweave: diverge from C1 (different file, cannot fast-forward to C2).
    let c_ww = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: diverged from C1");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: C_ww"]);

    // ww's project already contains the target's project tip, so the ff
    // failure is the manifest repo's: C2 and C_ww diverge from C1.
    let out = rwv()
        .args(["land", "--strategy", "ff"])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("github/chatly/server: HEAD is diverged"),
        "diverged ff landing should name the repo and its verdict; got stderr: {stderr}"
    );
}

/// The diverged ff refusal states rwv's own verdict and remedy, and keeps
/// git's `fatal:` cause underneath — but must not carry git's `hint:` block,
/// which recommends `git merge --no-ff` / `git rebase`, either of which
/// bypasses the sync op this refusal is inside of.
#[test]
fn sync_to_ff_diverged_refusal_keeps_cause_drops_advice() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: C2"]);
    common::git_in(&ww.project_dir, &["merge", "--ff-only", "main"]);

    let c_ww = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: diverged from C1");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: C_ww"]);

    let out = rwv()
        .args(["land", "--strategy", "ff"])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--strategy rebase") && stderr.contains("not-fast-forwardable"),
        "rwv's own verdict and remedy must survive; got: {stderr}"
    );
    assert!(
        !stderr.contains("hint:"),
        "git's advice block recommends bypassing rwv; must not appear: {stderr}"
    );
}

#[test]
fn gita_is_opt_in() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(root.join("projects/no-gita")).unwrap();
    std::fs::create_dir_all(root.join("projects/with-gita")).unwrap();

    let server_dir = root.join(SERVER_PATH);
    init_repo(&server_dir);

    // Generation is an INTENT-verb effect, so each arm activates (selection)
    // and then runs the repair verb that authors. `rwv activate` alone never
    // writes a CSV for either arm, so asserting on it could only ever measure
    // the surfacing scaffold — which is what this test used to do, back when a
    // declared-but-unwritten file still got a dangling link and the `gita/`
    // directory that held it.
    let generated = |project: &str| root.join("projects").join(project).join("gita/repos.csv");

    // 1. Default: no gita generated
    write_manifest(
        &root.join("projects/no-gita"),
        &[(SERVER_PATH, SERVER_URL)],
        None,
    );
    std::fs::write(root.join(".rwv-active"), "no-gita\n").unwrap();
    rwv()
        .args(["activate", "no-gita"])
        .current_dir(&root)
        .assert()
        .success();
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        !generated("no-gita").exists(),
        "gita must not be generated by default"
    );
    assert!(
        !root.join("gita").exists(),
        "and nothing gita-shaped may appear at the weave root either"
    );

    // 2. Opt-in: gita is generated
    write_manifest(
        &root.join("projects/with-gita"),
        &[(SERVER_PATH, SERVER_URL)],
        Some("[integrations.gita]\nenabled = true\n"),
    );
    std::fs::write(root.join(".rwv-active"), "with-gita\n").unwrap();
    rwv()
        .args(["activate", "with-gita"])
        .current_dir(&root)
        .assert()
        .success();
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        generated("with-gita").is_file(),
        "gita must be generated when explicitly enabled — as a real file at \
         the canonical path, which is the claim the directory's mere presence \
         never made"
    );
    assert_eq!(
        std::fs::read_link(root.join("gita/repos.csv")).unwrap(),
        Path::new("../projects/with-gita/gita/repos.csv"),
        "and surfaced at the weave root, now that there is something to surface"
    );
}

// ---------------------------------------------------------------------------
// Post-Phase-1' manifest reload failure is a hard bail, not
// a warn-and-proceed.
//
// Before the fix: if Project::from_dir fails after Phase 1', sync emitted a
// warning (suppressed in --json mode) and fell through to Phase 3, which then
// regenerated a lock from the pre-Phase-1' snapshot — silently omitting newly-
// added repos.
//
// After the fix: sync bails immediately with an error that names `rwv abort`
// as the recovery path.
// ---------------------------------------------------------------------------

/// Runs `rwv land <primary> <args>` from a workweave whose project repo
/// rebases onto the primary's, with a `post-rewrite` hook that leaves
/// `rwv.toml` unparseable once the project rebase lands, so the manifest cannot be
/// reloaded after Phase 1'.
///
/// The hook lives in the shared git hooks directory, which the workweave's
/// project worktree shares with the primary's project repo, and writes
/// relative to the git work tree: the workweave's project directory.
fn sync_with_a_manifest_corrupted_after_phase_1(args: &[&str]) -> std::process::Output {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let c2 = make_commit(
        &primary.server_dir,
        "primary_advance.txt",
        "primary advance\n",
        "primary: advance",
    );
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &primary.project_dir,
        &["commit", "-m", "lock: primary advance"],
    );

    make_commit(
        &ww.project_dir,
        "ww-note.txt",
        "ww note\n",
        "ww: project note",
    );

    let hooks_dir = primary.project_dir.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join("post-rewrite");
    std::fs::write(
        &hook_path,
        "#!/bin/sh\nprintf '[[[not a manifest\\n' > rwv.toml\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    rwv()
        .arg("land")
        .args(args)
        .current_dir(&ww.root)
        .output()
        .expect("rwv process should start")
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routed: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routed, vec![token], "stderr was:\n{stderr}");
}

/// Regression test: sync bails hard when the project manifest is corrupted
/// immediately after Phase 1' (project repo fast-forward merge), and the
/// refusal is the `malformed` state of `unparseable-project`.
///
/// Assertions:
///   - sync exits non-zero
///   - stderr names the state, the file that failed and the recovery
///     (`rwv abort`), and routes to the kind
///   - stderr does NOT contain "warning:" (old warn-and-proceed fingerprint)
#[test]
fn sync_to_bails_hard_when_post_phase1_manifest_reload_fails() {
    let out = sync_with_a_manifest_corrupted_after_phase_1(&[]);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "sync should fail when post-Phase-1' manifest reload fails; stderr: {stderr}"
    );
    assert!(
        stderr.contains("rwv abort"),
        "error must mention `rwv abort` as recovery path; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("warning:"),
        "must not emit the old warn-and-proceed fingerprint; stderr: {stderr}"
    );
    assert!(
        stderr.contains("rwv land: projects/web-app: unparseable-project malformed\n"),
        "the refusal is the malformed state of the kind; stderr: {stderr}"
    );
    assert!(
        stderr.contains("failed to parse rwv.toml at ") && stderr.contains("TOML parse error"),
        "the file that failed and the parser's error are listed; stderr: {stderr}"
    );
    assert!(
        !stderr.contains("choices:"),
        "the table gives the state no choice, so none is printed; stderr: {stderr}"
    );
    assert_routes_to(&stderr, "unparseable-project");
}

#[test]
fn the_malformed_reload_carries_its_axis_and_state_under_json() {
    let out = sync_with_a_manifest_corrupted_after_phase_1(&["--json"]);
    assert!(!out.status.success());
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("sync --json prints an envelope: {e}"));
    let refused = envelope["refused"].as_array().expect("refused is an array");
    assert_eq!(refused.len(), 1, "{envelope}");
    assert_eq!(refused[0]["path"], "projects/web-app");
    assert_eq!(refused[0]["axis"], "unparseable-project");
    assert_eq!(refused[0]["state"], "malformed");
    assert_eq!(refused[0]["choices"], serde_json::json!([]));
    assert!(
        refused[0]["listed"][0]
            .as_str()
            .unwrap()
            .starts_with("failed to parse rwv.toml at "),
        "{envelope}"
    );
}

// NOTE: A previous `fetch_no_reference_skips_reference_repos` integration
// test was removed — it was broken at commit time (asserted `rwv lock`
// success when a referenced repo had no on-disk clone, which fails at the
// `git status` step). Core --no-reference logic is now covered by the
// `find_incomplete_repos_*_no_reference_*` unit tests in src/fetch.rs. A proper
// end-to-end test would set up bare repos for both the project source and
// each manifest repo (per the fetch_test.rs pattern); follow-up work item
// captures that work.
