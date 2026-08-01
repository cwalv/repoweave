//! E2E integration tests for `rwv sync`, `rwv abort`, `rwv doctor --locked`, and `rwv status`.
//!
//! These exercise the acceptance criteria for `rwv sync` and `rwv abort`.
//!
//! Scenarios mirror the sync/abort how-tos in docs/how-to/ (e.g.
//! resume-or-abort-mid-op-sync.md, recover-from-sync-conflict.md).

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

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

/// Init a git repo with one commit. Returns HEAD SHA.
fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

/// Write a file, stage, commit. Returns new HEAD SHA.
fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
}

/// Write an rwv.yaml manifest into `project_dir`.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)], integrations: Option<&str>) {
    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    if let Some(int) = integrations {
        yaml.push_str("\nintegrations:\n");
        yaml.push_str(int);
    }
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();
}

/// Write an rwv.lock file into `project_dir`.
fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url, sha) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: {sha}\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), &yaml).unwrap();
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
///     projects/web-app/       (git repo, rwv.yaml + rwv.lock committed)
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
    // Mirror what `rwv init` writes: `.gitattributes` so `rwv sync`'s
    // native rebase keeps source's rwv.lock through the replay (the
    // `merge=rwv-ours` driver).
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    write_manifest(&project_dir, &[(SERVER_PATH, SERVER_URL)], None);
    write_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &sha)]);
    git(
        &["add", ".gitattributes", "rwv.yaml", "rwv.lock"],
        &project_dir,
    );
    git(&["commit", "-m", "lock: initial"], &project_dir);

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
    git(
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/main",
        ],
        &primary.server_dir,
    );

    let ww_project = ww_root.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
        &primary.project_dir,
    );
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
fn sync_without_source_outside_workweave_errors() {
    // Bare `rwv sync` is only valid inside a workweave (where it follows the
    // recorded parent). Outside any workspace it should fail loudly — and
    // outside a workweave (in a primary weave) it should also fail with a
    // helpful message rather than silently doing nothing.
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
// rwv sync — fast-forward path (shared object store via worktrees)
// ---------------------------------------------------------------------------

/// Tutorial scenario: workweave finishes work → `rwv lock` → from primary `rwv sync <ww>`.
#[test]
fn sync_ff_primary_advances_to_workweave_lock() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Workweave: make commit C2, update lock.
    let c2 = make_commit(
        &ww.server_dir,
        "change.txt",
        "workweave change\n",
        "ww: add change",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww change"], &ww.project_dir);

    // From primary: sync from the workweave.
    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Primary's server `main` branch should now be at C2.
    let primary_head = git_out(&["rev-parse", "main"], &primary.server_dir);
    assert_eq!(
        primary_head, c2,
        "primary server/main should be at C2 after sync from workweave"
    );
}

/// Tutorial scenario: primary has advanced → from workweave `rwv sync primary` catches up.
#[test]
fn sync_ff_is_symmetric_workweave_catches_up_to_primary() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2, update lock.
    let c2 = make_commit(
        &primary.server_dir,
        "upstream.txt",
        "upstream change\n",
        "primary: upstream advance",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: upstream advance"],
        &primary.project_dir,
    );

    // From workweave: sync to primary.
    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Workweave's ww/main branch (inside the shared clone) should be at C2.
    let ww_head = git_out(&["rev-parse", "ww/main"], &primary.server_dir);
    assert_eq!(
        ww_head, c2,
        "ww/main should be at C2 after syncing workweave from primary"
    );
}

// ---------------------------------------------------------------------------
// rwv sync — precondition enforcement
// ---------------------------------------------------------------------------

/// sync refuses when the source workspace's lock is stale (source HEAD ≠ source lock).
#[test]
fn sync_refuses_when_source_lock_is_stale() {
    let tmp = common::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    // Advance source repo past its lock without updating the lock.
    make_commit(
        &source.server_dir,
        "extra.txt",
        "extra\n",
        "source: advance past lock",
    );

    rwv()
        .args(["sync", &source.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("lock").or(predicate::str::contains("stale")));
}

/// sync refuses when the CWD workspace's lock is stale (CWD HEAD ≠ CWD lock).
#[test]
fn sync_refuses_when_cwd_lock_is_stale() {
    let tmp = common::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    // Advance primary past its lock without updating the lock.
    make_commit(
        &primary.server_dir,
        "extra.txt",
        "extra\n",
        "primary: advance past lock",
    );

    rwv()
        .args(["sync", &source.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("lock").or(predicate::str::contains("stale")));
}

/// --allow-stale-lock bypasses the lock-freshness precondition; the specific
/// "stale lock" error must not appear even when CWD lock is stale.
/// (Adapted from sync_force_bypasses_lock_freshness_precondition — same
/// end-state assertion, new flag spelling.)
#[test]
fn sync_allow_stale_lock_bypasses_lock_freshness_precondition() {
    let tmp = common::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    // Advance primary past its lock — this would normally trigger a precondition refusal.
    make_commit(
        &primary.server_dir,
        "extra.txt",
        "extra\n",
        "advance past lock",
    );

    let out = rwv()
        .args(["sync", &source.root.to_string_lossy(), "--allow-stale-lock"])
        .current_dir(&primary.root)
        .assert();

    // With --allow-stale-lock the lock-staleness precondition is bypassed.
    // The op may fail for other reasons (diverged repos, missing objects) but NOT
    // with the lock-freshness message.
    let output = out.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let is_lock_freshness_error = (stderr.contains("stale lock")
        || stderr.contains("lock-freshness precondition failed"))
        && !stderr.contains("--allow-stale-lock");
    assert!(
        !is_lock_freshness_error,
        "--allow-stale-lock should bypass the lock-freshness precondition; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// rwv sync — Phase 1 ancestor precondition
// ---------------------------------------------------------------------------
//
// Phase 1 hard-resets the destination's project repo to the source's tip.
// The precondition refuses when this would discard reachable commits — i.e.
// when the destination's project tip is NOT an ancestor of the source's tip.

/// Helper: advance primary's project repo by one commit (no-op edit) and re-lock.
/// Leaves primary's lock fresh.
fn primary_advance_project_one_commit(primary: &Workspace, server_sha: &str) {
    write_lock(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL, server_sha)],
    );
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &[
            "commit",
            "--allow-empty",
            "-m",
            "lock: primary project advance",
        ],
        &primary.project_dir,
    );
}

/// Backward sync: when CWD's project tip is ahead of source's, sync refuses.
/// The error names both workspaces and the proper recovery path.
#[test]
fn sync_refuses_when_destination_project_repo_is_ahead_of_source() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances its project repo by one commit (still pointing at C1
    // server). ww's project repo stays at C1. Both locks are fresh.
    primary_advance_project_one_commit(&primary, &c1);

    // Sync ww → primary: source=ww (project @ C1), CWD=primary (project ahead of C1).
    // primary has a project commit ww doesn't — refuse.
    let assertion = rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let output = assertion.get_output();
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Names the destination workspace.
    assert!(
        stderr.contains("destination workspace") && stderr.contains("project repo"),
        "expected refusal naming destination project repo; got: {stderr}"
    );
    // Names the source workspace too.
    assert!(
        stderr.contains("source workspace"),
        "expected refusal naming source workspace; got: {stderr}"
    );
    // Names the rwv-native recovery path.
    assert!(
        stderr.contains("sync the other direction"),
        "expected refusal to name the rwv-native recovery; got: {stderr}"
    );
    // Names the --discard-local-commits override explicitly.
    assert!(
        stderr.contains("--discard-local-commits") || stderr.contains("discard"),
        "expected refusal to name the --discard-local-commits override; got: {stderr}"
    );
}

/// Diverged sync: when CWD's project tip and source's project tip have diverged
/// (neither is an ancestor of the other), sync refuses.
#[test]
fn sync_refuses_when_destination_project_repo_has_diverged_from_source() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances its project by one commit (still pointing at C1 server).
    primary_advance_project_one_commit(&primary, &c1);

    // ww independently advances its project by a different commit (still C1 server).
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c1)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(
        &["commit", "--allow-empty", "-m", "lock: ww project advance"],
        &ww.project_dir,
    );

    // Sanity: tips diverged from common ancestor C1.
    let primary_tip = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    let ww_tip = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    assert_ne!(primary_tip, ww_tip);

    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("destination workspace")
                .and(predicate::str::contains("project repo"))
                .and(predicate::str::contains("commits not in source workspace")),
        );
}

/// Forward sync (CWD strict ancestor of source): allowed — the normal case.
/// Already covered by `sync_ff_primary_advances_to_workweave_lock` etc., but
/// guard explicitly that the ancestor precondition does NOT fire.
#[test]
fn sync_allows_when_destination_project_repo_is_strict_ancestor_of_source() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // ww advances its project (server still at C1). primary's project stays at C1.
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c1)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(
        &["commit", "--allow-empty", "-m", "lock: ww project advance"],
        &ww.project_dir,
    );

    // Sync ww → primary: primary (CWD) at C1, ww (source) ahead. primary IS
    // ancestor of ww — forward, allowed.
    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();
}

/// Equal tips: both project repos at the same SHA — no-op, allowed.
#[test]
fn sync_allows_when_destination_project_repo_equals_source() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // No advances anywhere — both project tips share C1.
    let primary_tip = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    let ww_tip = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    assert_eq!(primary_tip, ww_tip);

    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();
}

/// `--discard-local-commits` bypasses the ancestor precondition: backward sync
/// succeeds and the savepoint preserves the discarded commits for `rwv abort`.
/// (Adapted from sync_force_bypasses_phase1_ancestor_refusal_and_preserves_savepoint
/// — same end-state assertions, new flag spelling.)
#[test]
fn sync_discard_local_commits_bypasses_phase1_ancestor_refusal_and_preserves_savepoint() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances its project past C1; ww's project stays at C1.
    primary_advance_project_one_commit(&primary, &c1);
    let primary_pre_sync = git_out(&["rev-parse", "HEAD"], &primary.project_dir);

    // Backward sync with --discard-local-commits: must succeed.
    rwv()
        .args([
            "sync",
            &ww.root.to_string_lossy(),
            "--discard-local-commits",
        ])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Primary's project repo should now be at ww's project tip (hard-reset).
    let ww_tip = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    let primary_post_sync = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    assert_eq!(
        primary_post_sync, ww_tip,
        "with --discard-local-commits, primary's project tip should now equal ww's"
    );

    // The pre-sync commit is no longer reachable from HEAD but MUST be
    // reachable from some `refs/rwv/pre-op/*` savepoint — that's the
    // contract --discard-local-commits makes with the operator (recoverable
    // via rwv abort).
    let savepoint_refs = git_out(
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/rwv/pre-op",
        ],
        &primary.project_dir,
    );
    assert!(
        savepoint_refs.contains(&primary_pre_sync),
        "pre-sync commit {primary_pre_sync} should be preserved in a refs/rwv/pre-op/* savepoint; \
         got: {savepoint_refs}"
    );
}

/// Refusal message names `--strategy rebase` as the rwv-native path to land
/// diverging project commits without `--force`.
#[test]
fn sync_refusal_message_suggests_rebase_strategy() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    primary_advance_project_one_commit(&primary, &c1);

    let assertion = rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("--strategy rebase"),
        "expected refusal to suggest --strategy rebase; got: {stderr}"
    );
}

/// Backward sync that ff refuses lands cleanly under `--strategy rebase`:
/// CWD's lock-only divergence is replayed onto source's tip — the
/// `.gitattributes rwv.lock merge=rwv-ours` contract plus native `git rebase`
/// with `--force-rebase --no-keep-empty --empty=drop` makes the lock-only
/// commit become empty (or it was already empty via `--allow-empty`) and
/// git drops it. Phase 3 then leaves the lock consistent with manifest
/// tips.
#[test]
fn sync_rebase_lands_lock_only_divergence_without_force() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // primary advances its project by a lock-only commit; ww stays at C1.
    primary_advance_project_one_commit(&primary, &c1);
    let ww_tip_before = git_out(&["rev-parse", "HEAD"], &ww.project_dir);

    rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--strategy", "rebase"])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Primary's project tip should match ww's (the lock-only commit was
    // dropped during replay; Phase 3 found no lock change to commit).
    let primary_tip_after = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    assert_eq!(
        primary_tip_after, ww_tip_before,
        "after rebase landed lock-only divergence, primary tip should equal ww tip; \
         got primary={primary_tip_after} ww={ww_tip_before}"
    );
}

/// Refusal message names the source-side recovery: "sync the other direction
/// first" — and that recovery actually works (no infinite refusal loop).
#[test]
fn sync_other_direction_first_unblocks_a_refused_backward_sync() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances its project by one commit (C1 server still).
    primary_advance_project_one_commit(&primary, &c1);

    // Backward sync from primary refuses (verified in another test).
    // Operator runs the named recovery: sync the other direction first
    // (primary → ww), bringing primary's commit to ww.
    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Now primary and ww are aligned. The originally-refused sync (primary
    // CWD, ww source) is now allowed (equal tips → no-op).
    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// rwv sync — error message structure
// ---------------------------------------------------------------------------

/// Lock-freshness error names the source workspace, the recovery path, and
/// the --allow-stale-lock override. Does NOT mention the old `--force` flag
/// (removed). The recovery hint includes `--project <p>` so the operator
/// locks the right project even when the active project differs from the
/// one being synced.
#[test]
fn lock_freshness_source_error_names_workspace_and_recovery_path() {
    let tmp = common::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    make_commit(
        &source.server_dir,
        "extra.txt",
        "extra\n",
        "source: advance past lock",
    );

    let assertion = rwv()
        .args(["sync", &source.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("source workspace 'source'"),
        "expected named source workspace; got: {stderr}"
    );
    assert!(
        stderr.contains("stale lock"),
        "expected 'stale lock' phrasing; got: {stderr}"
    );
    assert!(
        stderr.contains("rwv lock"),
        "expected named recovery (`rwv lock`); got: {stderr}"
    );
    // The refusal must spell `--project <p>` so bare `rwv lock`
    // doesn't accidentally lock the wrong (active) project.
    assert!(
        stderr.contains("--project web-app"),
        "lock-freshness error must name --project <p> in recovery hint; got: {stderr}"
    );
    // Refusal must name the --allow-stale-lock override that opens this door.
    assert!(
        stderr.contains("--allow-stale-lock"),
        "lock-freshness error must name --allow-stale-lock override; got: {stderr}"
    );
    assert!(
        !stderr.contains("--force"),
        "lock-freshness error must not mention removed --force; got: {stderr}"
    );
}

/// Lock-freshness destination error names the destination workspace, the
/// recovery path, and the --allow-stale-lock override. Does NOT mention
/// the old `--force` flag (removed). The recovery hint includes
/// `--project <p>` so the operator locks the right
/// project even when the active project differs from the one being synced.
#[test]
fn lock_freshness_destination_error_names_workspace_and_recovery_path() {
    let tmp = common::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    make_commit(
        &primary.server_dir,
        "extra.txt",
        "extra\n",
        "primary: advance past lock",
    );

    let assertion = rwv()
        .args(["sync", &source.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("destination workspace 'primary'"),
        "expected named destination workspace; got: {stderr}"
    );
    assert!(
        stderr.contains("stale lock"),
        "expected 'stale lock' phrasing; got: {stderr}"
    );
    assert!(
        stderr.contains("rwv lock"),
        "expected named recovery (`rwv lock`); got: {stderr}"
    );
    // The refusal must spell `--project <p>` so bare `rwv lock`
    // doesn't accidentally lock the wrong (active) project.
    assert!(
        stderr.contains("--project web-app"),
        "lock-freshness error must name --project <p> in recovery hint; got: {stderr}"
    );
    // Refusal must name the --allow-stale-lock override that opens this door.
    assert!(
        stderr.contains("--allow-stale-lock"),
        "lock-freshness error must name --allow-stale-lock override; got: {stderr}"
    );
    assert!(
        !stderr.contains("--force"),
        "lock-freshness error must not mention removed --force; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// rwv sync — non-ff strategies
// ---------------------------------------------------------------------------

/// When CWD has local commits on top of an older base, --strategy rebase replays them.
#[test]
fn sync_rebase_replays_local_commits_on_source_tip() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2, update lock.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: advance",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: primary advance"],
        &primary.project_dir,
    );

    // Workweave: add a commit C_ww on top of C1 (before primary's C2).
    let c_ww = make_commit(
        &ww.server_dir,
        "ww_feature.txt",
        "ww feature\n",
        "ww: local commit",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww feature"], &ww.project_dir);

    // ww/main (C_ww) and primary main (C2) have both diverged from C1.
    // --strategy rebase should replay C_ww onto C2.
    //
    // Both sides also independently committed locks → ww and primary's project
    // repos diverged too. Phase 1's ancestor precondition refuses divergent
    // project repos; this test deliberately constructs that to exercise Phase
    // 2's rebase strategy on the manifest repo, so it opts in via
    // --discard-local-commits (adapted from --force).
    rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "rebase",
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();

    // After rebase, ww/main should be a descendant of C2.
    let ww_head = git_out(&["rev-parse", "ww/main"], &primary.server_dir);
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
    write_lock(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL, &c_primary)],
    );
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: primary"], &primary.project_dir);

    let c_ww = make_commit(
        &ww.server_dir,
        "shared.txt",
        "ww version\n",
        "ww: conflict candidate",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww"], &ww.project_dir);

    // Record ww's server tip before the attempted sync.
    let pre_op_sha = git_out(&["rev-parse", "ww/main"], &primary.server_dir);
    assert_eq!(pre_op_sha, c_ww);

    // Attempt rebase sync — should hit a conflict and leave repos mid-op.
    // Project repos diverged from independent lock commits → --discard-local-commits
    // is required to reach Phase 2 where the rebase conflict is the focus
    // (adapted from --force).
    let _ = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--strategy",
            "rebase",
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert();

    // Abort should restore ww/main back to pre-op state.
    rwv().arg("abort").current_dir(&ww.root).assert().success();

    let post_abort_sha = git_out(&["rev-parse", "ww/main"], &primary.server_dir);
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
    let sha = git_out(&["rev-parse", "HEAD"], project_dir);

    // Make a second commit so we have something to rebase onto.
    let sha2 = make_commit(project_dir, "extra.txt", "extra\n", "extra commit");

    // Invent an op-id that abort will read from the marker file.
    let op_id = "20991231T000000Z";

    // Create the savepoint ref pointing at sha2 (the pre-op HEAD).
    git(
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), &sha2],
        project_dir,
    );

    // Write a v2 owner record so `rwv abort` thinks an op is in progress.
    let op_state_json = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"sync\", \"strategy\": \"rebase\", \
         \"source\": \"{root}\", \"target\": \"{root}\", \"retire\": false, \"phase\": \"replay\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        root = ws.root.display(),
    );
    std::fs::write(ws.root.join(".rwv-op"), &op_state_json).unwrap();

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
    git(&["checkout", "-b", "diverge", &sha], project_dir);
    make_commit(
        project_dir,
        "conflict.txt",
        "diverge version\n",
        "diverge: conflict",
    );

    // Step 3: return to main, start rebase of diverge onto main — this will conflict.
    git(&["checkout", "main"], project_dir);
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

    // Now write conflict markers into rwv.lock — simulating a sync that
    // left rwv.lock with conflict markers.
    std::fs::write(
        project_dir.join("rwv.lock"),
        "<<<<<<< HEAD workweave\nrepositories: {}\n=======\nrepositories: {}\n>>>>>>> abc1234\n",
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
    let status = git_out(&["status", "--porcelain"], project_dir);
    assert!(
        status.is_empty(),
        "after abort, working tree should be clean; got: {status}"
    );
}

// ---------------------------------------------------------------------------
// Round-trip convergence
// ---------------------------------------------------------------------------
// rwv sync — tag-form lock freshness (mirrors check_test.rs §8)
// ---------------------------------------------------------------------------

/// Add a lightweight tag at HEAD in `repo`.
fn git_tag(repo: &Path, tag: &str) {
    git(
        &[
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=Test",
            "tag",
            tag,
        ],
        repo,
    );
}

/// sync proceeds when the source lock is pinned by a tag that resolves to the source HEAD.
/// Regression test: before the fix, this spuriously produced "source lock is stale".
#[test]
fn sync_proceeds_when_source_lock_tag_matches_head() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Tag primary's server at its current HEAD and update the source lock to the tag name.
    git_tag(&primary.server_dir, "v1.0.0");
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, "v1.0.0")]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: pin v1.0.0 (tag form)"],
        &primary.project_dir,
    );

    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();
}

/// sync proceeds when the source lock is pinned by a SHA that equals the source HEAD.
/// Regression guard: SHA-form should always have worked; this ensures it still does.
#[test]
fn sync_proceeds_when_source_lock_sha_matches_head() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // primary's project already has the lock with C1 SHA committed.
    // Syncing ww from primary is a no-op; the freshness check must pass.
    let source_lock_sha = c1; // the SHA already in the lock
    assert!(!source_lock_sha.is_empty());

    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();
}

/// sync refuses when the source lock is pinned by a tag whose commit differs from HEAD.
/// The "stale" error must fire even when the lock version is a tag name, not a raw SHA.
#[test]
fn sync_refuses_when_source_lock_tag_is_genuinely_stale() {
    let tmp = common::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (cwd_ws, _) = make_locked_workspace(tmp.path(), "cwd");

    // Tag primary's server at C1, then advance past it — now HEAD ≠ tag commit.
    git_tag(&primary.server_dir, "v1.0.0");
    make_commit(
        &primary.server_dir,
        "advance.txt",
        "advance\n",
        "primary: advance past v1.0.0",
    );

    // Update source lock to the tag name — it's now genuinely stale (HEAD > tag commit).
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, "v1.0.0")]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: v1.0.0 (stale)"],
        &primary.project_dir,
    );

    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&cwd_ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("lock").or(predicate::str::contains("stale")));
}

/// sync refuses and reports "unknown revision" when the source lock references a tag
/// that no longer exists locally.
#[test]
fn sync_refuses_with_unknown_revision_when_source_lock_tag_missing() {
    let tmp = common::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (cwd_ws, _) = make_locked_workspace(tmp.path(), "cwd");

    // Write a source lock pinned by a tag that was never created.
    write_lock(
        &primary.project_dir,
        &[(SERVER_PATH, SERVER_URL, "v9.9.9-nonexistent")],
    );
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: nonexistent tag"],
        &primary.project_dir,
    );

    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&cwd_ws.root)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unknown revision")
                .or(predicate::str::contains("v9.9.9-nonexistent")),
        );
}

// ---------------------------------------------------------------------------

/// sync A→B then B→A should be a no-op on B (project repo must not grow unbounded).
#[test]
fn sync_roundtrip_converges_without_project_repo_growth() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Advance primary to C2, update lock.
    let c2 = make_commit(
        &primary.server_dir,
        "advance.txt",
        "advance\n",
        "primary: advance",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: advance"], &primary.project_dir);

    let count_before: usize = git_out(&["rev-list", "--count", "HEAD"], &primary.project_dir)
        .parse()
        .unwrap();

    // Sync primary → workweave (workweave catches up to C2).
    rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Sync workweave → primary (now a no-op: ww is at C2, primary is already at C2).
    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Primary's project repo commit count must not have grown.
    let count_after: usize = git_out(&["rev-list", "--count", "HEAD"], &primary.project_dir)
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
    git(
        &[
            "worktree",
            "add",
            "-b",
            &format!("primary--{ww_name}/main"),
            &ww_server.to_string_lossy(),
        ],
        &primary.server_dir,
    );

    let ww_project = ww_root.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            "-b",
            &format!("primary--{ww_name}/project"),
            &ww_project.to_string_lossy(),
        ],
        &primary.project_dir,
    );

    let primary_canon = primary.root.canonicalize().unwrap().display().to_string();
    let marker = format!(
        "primary: {p}\nproject: web-app\nparent: {p}\n",
        p = primary_canon
    );
    std::fs::write(ww_root.join(".rwv-workweave"), marker).unwrap();

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
/// workweave, `rwv sync`'s "CWD lock" freshness check must read the workweave's
/// own committed lock — not primary's — to determine staleness from the
/// operator's perspective.
#[test]
fn sync_in_marker_workweave_uses_workweave_own_lock_for_cwd_freshness() {
    let tmp = common::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Primary advances to C2 and commits the C2 lock on its main branch.
    let c2 = make_commit(
        &ws.primary.server_dir,
        "primary-advance.txt",
        "primary advance\n",
        "primary: C2",
    );
    write_lock(&ws.primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &ws.primary.project_dir);
    git(
        &["commit", "-m", "lock: primary advance"],
        &ws.primary.project_dir,
    );

    // Workweave's own server tip is still at C1, and its committed lock is
    // still at C1 (inherited from the worktree-creation snapshot of project
    // repo @ C1). From the operator's perspective in the workweave, "CWD lock
    // is fresh" — workweave-tip C1 == workweave-lock C1 — even though primary
    // has diverged.
    rwv()
        .args(["sync", &ws.primary.root.to_string_lossy()])
        .current_dir(&ws.ww_root)
        .assert()
        .success();

    // After sync the workweave's server tip should have caught up to C2.
    let ww_server_head = git_out(&["rev-parse", "HEAD"], &ws.ww_server_dir);
    assert_eq!(
        ww_server_head, c2,
        "workweave server should be at C2 after sync from primary"
    );
}

/// Symmetric regression: `rwv sync` from primary with a marker-based workweave
/// as source must read the workweave's lock for `source` freshness — not
/// primary's lock interpreted as the source's.
#[test]
fn sync_from_primary_with_marker_workweave_source_uses_source_own_lock() {
    let tmp = common::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Workweave advances to C2 and commits the C2 lock on its own project branch.
    let c2 = make_commit(
        &ws.ww_server_dir,
        "ww-advance.txt",
        "ww advance\n",
        "ww: C2",
    );
    write_lock(&ws.ww_project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &ws.ww_project_dir);
    git(&["commit", "-m", "lock: ww advance"], &ws.ww_project_dir);

    // Primary syncs from the workweave. Source freshness must be checked
    // against the workweave's own lock (C2 vs ww-tip C2), not primary's lock.
    rwv()
        .args(["sync", &ws.ww_root.to_string_lossy()])
        .current_dir(&ws.primary.root)
        .assert()
        .success();

    // Primary's server should have advanced to C2 (the workweave's lock target).
    let primary_server_head = git_out(&["rev-parse", "HEAD"], &ws.primary.server_dir);
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
fn sync_in_marker_workweave_refuses_when_workweave_own_lock_is_stale() {
    let tmp = common::tempdir().unwrap();
    let ws = make_marker_workweave(tmp.path(), "feat");

    // Advance the workweave's server tip without updating the workweave's
    // own committed lock. (Primary's lock is left untouched at C1 to guarantee
    // we are not silently reading primary's value.)
    make_commit(
        &ws.ww_server_dir,
        "ww-advance.txt",
        "ww advance\n",
        "ww: advance past lock",
    );

    rwv()
        .args(["sync", &ws.primary.root.to_string_lossy()])
        .current_dir(&ws.ww_root)
        .assert()
        .failure()
        .stderr(
            // Workweave name is the part after `<project>--` in the dir
            // name; previously the resolver returned the whole dir
            // basename, so downstream lookups like delete_workweave would
            // re-prefix and miss the actual on-disk path.
            predicate::str::contains("destination workspace 'feat'")
                .and(predicate::str::contains("stale lock")),
        );
}

// ---------------------------------------------------------------------------
// RepoSyncOutcome — per-repo status reporting
// ---------------------------------------------------------------------------

/// Regression: CWD is ahead of the lock target → sync reports `already-ahead`, not bare `ok`.
///
/// Repro from SME triage: after `rwv sync`, the per-repo line said "ok" even
/// though HEAD didn't move to the lock SHA, leaving `rwv status` showing [ahead].
#[test]
fn sync_reports_already_ahead_when_cwd_is_past_lock_target() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2, lock at C2.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: C2"], &primary.project_dir);

    // Workweave: fast-forward to C2, then add C3 (ww is now ahead of primary's lock).
    git(&["merge", "--ff-only", "main"], &ww.server_dir);
    let c3 = make_commit(
        &ww.server_dir,
        "ww-extra.txt",
        "ww extra\n",
        "ww: C3 (ahead of C2)",
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c3)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: C3"], &ww.project_dir);

    // Sync ww from primary: target is C2, but ww is at C3 (C2 is ancestor of C3).
    // --discard-local-commits bypasses the Phase 1 ancestor precondition because
    // ww's project repo has the C3 lock commit primary doesn't; the test's intent
    // is Phase 2 behavior on the manifest repo (already-ahead reporting), not the
    // Phase 1 guard. (Adapted from --force.)
    let out = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--discard-local-commits",
        ])
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
}

/// Regression: diverged repo + `--strategy ff` must still report `failed (cannot fast-forward...)`.
/// Verifies the failure message structure carries through the new outcome enum unchanged.
#[test]
fn sync_ff_reports_failed_for_diverged_repo() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: C2"], &primary.project_dir);

    // Workweave: diverge from C1 (different file, cannot fast-forward to C2).
    let c_ww = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: diverged from C1");
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: C_ww"], &ww.project_dir);

    // ff sync: C2 and C_ww both diverge from C1 → cannot fast-forward.
    // --discard-local-commits bypasses the Phase 1 ancestor precondition because
    // ww's project repo has the C_ww lock commit primary doesn't; the test's
    // intent is Phase 2's ff failure on the manifest repo, not the Phase 1 guard.
    // (Adapted from --force.)
    let out = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot fast-forward") || stderr.contains("failed"),
        "diverged ff sync should report failure; got stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Named override flags — new tests
// ---------------------------------------------------------------------------

/// `--force` is rejected on `rwv sync` with a migration hint naming both
/// replacement flags (early-dispatch in `cli::dispatch`).
#[test]
fn sync_force_flag_rejected_with_migration_hint() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let assertion = rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--force"])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("--allow-stale-lock") && stderr.contains("--discard-local-commits"),
        "expected migration hint naming both replacement flags; got: {stderr}"
    );
    assert!(
        !assertion.get_output().status.success(),
        "rwv sync --force must exit non-zero"
    );
}

/// `--force` is rejected on `rwv sync-to` with a migration hint.
#[test]
fn sync_to_force_flag_rejected_with_migration_hint() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let assertion = rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--force"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("--allow-stale-lock") || stderr.contains("--discard-local-commits"),
        "expected migration hint; got: {stderr}"
    );
}

/// --allow-stale-lock and --discard-local-commits parse correctly on sync.
#[test]
fn sync_new_override_flags_parse_correctly() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Both flags parse without a clap error. We don't need the operation to
    // succeed — just that clap accepts the flags.
    let out_stale = rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--allow-stale-lock"])
        .current_dir(&primary.root)
        .output()
        .unwrap();
    let stale_stderr = String::from_utf8_lossy(&out_stale.stderr);
    assert!(
        !stale_stderr.contains("unexpected argument")
            && !stale_stderr.contains("unrecognized")
            && !stale_stderr.contains("--force"),
        "--allow-stale-lock should be a recognized flag; got: {stale_stderr}"
    );

    let out_discard = rwv()
        .args([
            "sync",
            &ww.root.to_string_lossy(),
            "--discard-local-commits",
        ])
        .current_dir(&primary.root)
        .output()
        .unwrap();
    let discard_stderr = String::from_utf8_lossy(&out_discard.stderr);
    assert!(
        !discard_stderr.contains("unexpected argument")
            && !discard_stderr.contains("unrecognized")
            && !discard_stderr.contains("--force"),
        "--discard-local-commits should be a recognized flag; got: {discard_stderr}"
    );
}

/// --discard-local-commits refuses when the project repo has uncommitted changes
/// (unrecoverable loss if hard-reset proceeded).
#[test]
fn sync_discard_local_commits_refuses_on_uncommitted_changes() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances its project past C1; ww's project stays at C1.
    primary_advance_project_one_commit(&primary, &c1);

    // Plant an uncommitted change in primary's project repo (the CWD).
    std::fs::write(primary.project_dir.join("dirty.txt"), "uncommitted\n").unwrap();

    let assertion = rwv()
        .args([
            "sync",
            &ww.root.to_string_lossy(),
            "--discard-local-commits",
        ])
        .current_dir(&primary.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assertion.get_output().stderr).to_string();

    assert!(
        stderr.contains("uncommitted"),
        "expected refusal mentioning uncommitted changes; got: {stderr}"
    );
    assert!(
        !stderr.contains("--force"),
        "refusal must not mention removed --force; got: {stderr}"
    );
}

/// `discard-local-commits` override is recorded in the op record overrides field
/// and the tombstone savepoint is preserved after a successful sync.
#[test]
fn sync_discard_local_commits_records_override_and_preserves_tombstone() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());

    // Primary advances its project past C1; ww's project stays at C1.
    primary_advance_project_one_commit(&primary, &c1);
    let primary_pre_sync = git_out(&["rev-parse", "HEAD"], &primary.project_dir);

    // Run with --discard-local-commits; should succeed.
    rwv()
        .args([
            "sync",
            &ww.root.to_string_lossy(),
            "--discard-local-commits",
        ])
        .current_dir(&primary.root)
        .assert()
        .success();

    // The pre-sync project commit must be in a refs/rwv/pre-op/* savepoint
    // (tombstone — cleanup preserved it because override was recorded).
    let savepoint_refs = git_out(
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/rwv/pre-op",
        ],
        &primary.project_dir,
    );
    assert!(
        savepoint_refs.contains(&primary_pre_sync),
        "tombstone savepoint must contain the pre-sync SHA {primary_pre_sync}; \
         got refs: {savepoint_refs}"
    );
}

/// `allow-stale-lock` override is recorded in the op record when used.
/// Verified indirectly: the sync succeeds (no lock-freshness refusal) and
/// the op record file is gone after cleanup (op completed, overrides persisted
/// through to cleanup).
#[test]
fn sync_allow_stale_lock_override_recorded_and_op_completes() {
    let tmp = common::tempdir().unwrap();
    let (primary, _) = make_locked_workspace(tmp.path(), "primary");
    let (source, _) = make_locked_workspace(tmp.path(), "source");

    // Advance primary's server past its lock (stale).
    make_commit(
        &primary.server_dir,
        "extra.txt",
        "extra\n",
        "advance past lock",
    );

    // With --allow-stale-lock, sync should not fail on lock-freshness.
    let out = rwv()
        .args(["sync", &source.root.to_string_lossy(), "--allow-stale-lock"])
        .current_dir(&primary.root)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stderr.contains("lock-freshness precondition failed"),
        "--allow-stale-lock must suppress lock-freshness failure; got: {stderr}"
    );
    // Op state file must be gone (cleanup ran).
    assert!(
        !primary.root.join(".rwv-op").exists(),
        "op state file should be cleaned up after successful sync"
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
    assert!(
        !root.join("gita").exists(),
        "gita directory should not be generated by default"
    );

    // 2. Opt-in: gita is generated
    write_manifest(
        &root.join("projects/with-gita"),
        &[(SERVER_PATH, SERVER_URL)],
        Some("  gita:\n    enabled: true\n"),
    );
    std::fs::write(root.join(".rwv-active"), "with-gita\n").unwrap();
    rwv()
        .args(["activate", "with-gita"])
        .current_dir(&root)
        .assert()
        .success();
    assert!(
        root.join("gita").exists(),
        "gita directory should be generated when explicitly enabled"
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

/// Regression test: sync bails hard when the project manifest is corrupted
/// immediately after Phase 1' (project repo fast-forward merge).
///
/// We simulate post-Phase-1' corruption via a `post-merge` git hook installed
/// in the shared git hooks directory.  `git merge --ff-only` (the default
/// Phase 1' path when CWD is behind source) triggers `post-merge`, which
/// replaces `rwv.yaml` with invalid YAML.  The reload at the fixed code site
/// sees garbage and must bail — not warn and proceed.
///
/// The workweave's project repo is a git worktree of primary's project, so
/// hooks live in primary.project_dir/.git/hooks/ and fire for both repos.
/// The `post-merge` hook writes its corruption to `rwv.yaml` relative to the
/// git work tree, which at hook time is the workweave's project directory.
///
/// Assertions:
///   - sync exits non-zero
///   - stderr contains "rwv abort" (recovery hint)
///   - stderr does NOT contain "warning:" (old warn-and-proceed fingerprint)
#[test]
fn sync_bails_hard_when_post_phase1_manifest_reload_fails() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance server to C2 and commit an updated lock.  The WW
    // project is behind (no extra WW commits), so Phase 1' can fast-forward.
    let c2 = make_commit(
        &primary.server_dir,
        "primary_advance.txt",
        "primary advance\n",
        "primary: advance",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: primary advance"],
        &primary.project_dir,
    );

    // Install a `post-merge` hook in the shared git hooks directory.
    // Because ww.project_dir is a worktree of primary.project_dir, both
    // share the same hooks directory (primary.project_dir/.git/hooks/).
    // `git merge --ff-only` (the default Phase 1' path) triggers post-merge.
    // The hook writes invalid YAML to rwv.yaml in the current working tree
    // (the workweave's project dir at hook-call time).
    let hooks_dir = primary.project_dir.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join("post-merge");
    // The hook receives a single squash-merge flag argument.  We ignore it
    // and unconditionally corrupt the manifest.
    std::fs::write(
        &hook_path,
        "#!/bin/sh\nprintf '!!!invalid yaml!!!\\n' > rwv.yaml\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Run sync with the default FF strategy.  Phase 1' calls
    // `git merge --ff-only <source_project_tip>` in the WW project dir,
    // which fires the post-merge hook and corrupts rwv.yaml on disk.
    // The fixed reload code should bail immediately rather than proceeding
    // with a stale snapshot.
    let out = rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .output()
        .expect("rwv process should start");

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
}

// NOTE: A previous `fetch_no_reference_skips_reference_repos` integration
// test was removed — it was broken at commit time (asserted `rwv lock`
// success when a referenced repo had no on-disk clone, which fails at the
// `git status` step). Core --no-reference logic is now covered by the
// `find_incomplete_repos_*_no_reference_*` unit tests in src/fetch.rs. A proper
// end-to-end test would set up bare repos for both the project source and
// each manifest repo (per the fetch_test.rs pattern); follow-up work item
// captures that work.
