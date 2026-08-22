//! Integration tests anchoring documented behavior of `rwv sync-to --json`.
//!
//! Doc claims pinned here:
//!
//!   - `rwv sync-to --json` (serial, `-j 1` or no `-j`) emits the envelope
//!     `{"$schema": "<url>", "outcomes": [<SyncOutcomeOutput>, ...]}`.
//!   - The `$schema` URL points at `docs/reference/schemas/sync-to.json`, and
//!     is not `rwv sync`'s.
//!   - `rwv explain sync-to` prints sync-to's own bundle.
//!   - `--allow-stale-lock` is refused-then-named on both the CWD and the
//!     target side, and bypasses each.
//!   - The target-side dirty preflight refuses on uncommitted TRACKED changes
//!     only; a non-colliding untracked file in the target does not block the
//!     sync. A target-side untracked file that collides with a path the
//!     fast-forward writes still refuses, but at step 3 rather than up front,
//!     naming the path and leaving the op resumable via `sync-to --continue`.
//!   - A successful sync-to leaves no pre-op savepoint on either side.
//!
//! Pinned elsewhere, and deliberately not duplicated here: NDJSON streaming
//! under `-j N` with `N > 1` is driven in
//! `tests/schema_conformance_wire_test.rs`, which validates the emitted
//! records against the published schema rather than merely parsing them.
//!
//! `SyncToJsonOutput` is not `SyncJsonOutput` with a different URL — it adds
//! `source_workweave`, `target`, `retired`, `resumed`,
//! `project_repo_advance`, and a per-outcome `step3_advance` — so a test
//! written from "the shapes are identical" would be pinning something untrue.
//!
//! This test mirrors `tests/doc_claims_sync_test.rs` for the sync-to verb.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
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

const SERVER_URL: &str = "https://github.com/example/server.git";
const SERVER_PATH: &str = "github/example/server";

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

fn make_shared(parent: &Path) -> (Workspace, Workspace, String) {
    let primary = parent.join("primary");
    std::fs::create_dir_all(primary.join("github/example")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let primary_server = primary.join(SERVER_PATH);
    let sha = init_repo(&primary_server);

    let primary_project = primary.join("projects/web-app");
    init_repo(&primary_project);
    std::fs::write(
        primary_project.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    write_manifest(&primary_project, &[(SERVER_PATH, SERVER_URL)]);
    common::fixture_lock(&primary_project, &[(SERVER_PATH, SERVER_URL, &sha)]);
    common::git_in(
        &primary_project,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&primary_project, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    let ww = parent.join("ww");
    std::fs::create_dir_all(ww.join("github/example")).unwrap();
    std::fs::create_dir_all(ww.join("projects")).unwrap();

    let ww_server = ww.join(SERVER_PATH);
    common::git_in(
        &primary_server,
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/server",
        ],
    );

    let ww_project = ww.join("projects/web-app");
    common::git_in(
        &primary_project,
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );
    std::fs::write(ww.join(".rwv-active"), "web-app\n").unwrap();

    (
        Workspace {
            root: primary,
            project_dir: primary_project,
            server_dir: primary_server,
        },
        Workspace {
            root: ww,
            project_dir: ww_project,
            server_dir: ww_server,
        },
        sha,
    )
}

const SCHEMA_FRAGMENT: &str = "docs/reference/schemas/sync-to.json";

// ===========================================================================
// 1. Envelope shape under serial mode
//
// Doc claim: `rwv sync-to --json` (no `-j` or `-j 1`) emits an object with
// `$schema` + `outcomes` (an array). The $schema URL points at sync-to.json.
// ===========================================================================

#[test]
fn sync_to_json_serial_emits_envelope_with_schema_and_outcomes() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    // Workweave advances so sync-to has actual work to do.
    let c2 = make_commit(&ww.server_dir, "ww.txt", "workweave\n", "ww: advance");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    let assert = rwv()
        .args([
            "sync-to",
            &primary.root.to_string_lossy(),
            "--strategy=ff",
            "--json",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Whole stdout parses as one JSON document — the envelope.
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope must parse as one JSON doc ({e}):\n{stdout}"));
    let obj = parsed.as_object().expect("envelope is an object");

    // `$schema` URL points at the sync-to schema artifact (not sync.json).
    let schema = obj["$schema"]
        .as_str()
        .expect("envelope must carry `$schema` string");
    assert!(
        schema.contains(SCHEMA_FRAGMENT),
        "$schema must point at {SCHEMA_FRAGMENT}; got: {schema}"
    );

    // `outcomes` is present (may be empty for ff-clean with no manifest repos to sync).
    assert!(
        obj.contains_key("outcomes"),
        "envelope must carry `outcomes` key; got:\n{stdout}"
    );
}

// ===========================================================================
// 2. $schema URL is sync-to.json, not sync.json
//
// Doc claim: sync-to's JSON output embeds a distinct $schema URL that points
// at sync-to.json rather than sync.json. Consumers can distinguish the two.
// ===========================================================================

#[test]
fn sync_to_json_schema_url_differs_from_sync_schema_url() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    // Workweave advances.
    let c2 = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: advance");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww"]);

    let assert = rwv()
        .args([
            "sync-to",
            &primary.root.to_string_lossy(),
            "--strategy=ff",
            "--json",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let schema = parsed["$schema"].as_str().unwrap();

    assert!(
        schema.contains("sync-to.json"),
        "$schema must contain 'sync-to.json'; got: {schema}"
    );
    assert!(
        !schema.ends_with("/sync.json"),
        "$schema must NOT end with '/sync.json' (that's rwv sync's URL); got: {schema}"
    );
}

// ===========================================================================
// 3. Explain verb works for sync-to
//
// Doc claim: `rwv explain sync-to` returns the sync-to bundle — the four
// sections every `--json`-capable verb's bundle carries, and the schema
// generated from sync-to's own output type.
// ===========================================================================

/// `rwv explain sync-to` prints sync-to's bundle, not `rwv sync`'s.
///
/// The two are neighbouring entries in one dispatch table, so a swapped
/// mapping is the defect here — and containment on `"sync-to"` is blind to
/// it: the sync bundle names `rwv sync-to` in its own prose eight times, and
/// describes steps of its own. Two things are the sync-to bundle's alone: the
/// heading it opens with, and the title of the schema block, which the
/// generator writes from the Rust type backing `sync-to --json`.
#[test]
fn explain_sync_to_returns_the_sync_to_bundle() {
    let assert = rwv().args(["explain", "sync-to"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    assert_eq!(
        stdout.lines().next(),
        Some("# rwv sync-to"),
        "the bundle opens with its own heading; got:\n{stdout}"
    );
    for section in ["## Purpose", "## Invocation", "## Output", "## Exit codes"] {
        assert!(
            stdout.lines().any(|line| line == section),
            "the bundle is missing its `{section}` section; got:\n{stdout}"
        );
    }
    assert!(
        stdout.contains(r#""title": "SyncToJsonOutput""#),
        "the schema block must be generated from sync-to's own output type; got:\n{stdout}"
    );
}

// ===========================================================================
// 4. --allow-stale-lock for sync-to: refusal names condition + flag; flag
//    bypasses both source (target workspace) and destination (CWD) preconditions.
//
// Doc claim (cli.md §sync-to, --allow-stale-lock row):
//   "Consent: skip the lock-freshness precondition on both source and
//   destination."
//
// For sync-to, the "source" lock check runs against the TARGET workspace's
// committed lock, and the "destination" check runs against CWD's lock on disk.
//
// (i)  Without --allow-stale-lock, a stale lock produces a refusal that names
//      "lock-freshness precondition" AND "--allow-stale-lock".
// (ii) With --allow-stale-lock, sync-to succeeds despite the stale lock.
// ===========================================================================

/// Helper: build a fixture where the CWD (ww) has a stale lock for sync-to.
///
/// ww's lock file is patched to a fabricated SHA that does not match the
/// actual server HEAD. Primary's lock is fresh (matches its server HEAD).
///
/// When running sync-to from ww → primary: the "destination" lock check fires
/// on CWD (ww's lock != ww's server HEAD).
/// With --allow-stale-lock, sync-to proceeds. The target (primary) already
/// matches ww's server state → no-op convergence → success.
fn make_shared_with_stale_cwd_for_sync_to(parent: &Path) -> (Workspace, Workspace) {
    let (primary, ww, initial_sha) = make_shared(parent);

    // Patch ww's lock to a fabricated SHA — stale destination for sync-to.
    let fake_sha = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    assert_ne!(fake_sha, initial_sha.as_str());
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, fake_sha)]);

    (primary, ww)
}

/// Helper: build a fixture where the target workspace (primary) has a stale lock.
///
/// The stale condition here is "lock ahead of HEAD": primary's committed lock
/// records C2, but primary's server is reset back to C1. Ww's server is at C2
/// (ahead of primary's current HEAD, which is C1). This is the "intentionally
/// ahead" stale-lock scenario.
///
/// Stale check on target: committed lock=C2, server HEAD=C1 → fires.
/// With --allow-stale-lock + strategy=ff: snapshot reads lock=C2; ww is at C2
/// (strictly ahead of C1 which is primary's server HEAD); step 3 advances
/// primary's server from C1 to C2 → success.
fn make_shared_with_stale_target_for_sync_to(parent: &Path) -> (Workspace, Workspace) {
    let (primary, ww, initial_sha) = make_shared(parent);

    // Step 1: advance primary's server to C2 and commit C2 to ww's worktree
    // (ww's branch ww/server starts at C1 which is primary's initial commit).
    let c2 = make_commit(
        &primary.server_dir,
        "advance.txt",
        "advance\n",
        "primary: advance to C2",
    );

    // ww's server (worktree from primary) can reach C2 via the shared object db.
    // Fast-forward ww's worktree branch to C2 so ww is also at C2.
    common::git_in(&ww.server_dir, &["merge", "--ff-only", &c2]);

    // Update ww's lock to C2 and commit.
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww at C2"]);

    // Update primary's lock to C2 and commit.
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &primary.project_dir,
        &["commit", "-m", "lock: primary at C2"],
    );

    // Now reset primary's server back to C1 (simulate rollback or out-of-sync).
    // primary's committed lock still says C2, but server HEAD is now C1 → stale.
    common::git_in(&primary.server_dir, &["reset", "--hard", &initial_sha]);

    // Verify invariant: primary lock=C2, server=C1 (stale); ww lock=C2, server=C2 (fresh).
    assert_ne!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        c2,
        "primary server must be at C1, not C2, after reset"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        initial_sha,
        "primary server must be at initial_sha after reset"
    );

    (primary, ww)
}

// ---------------------------------------------------------------------------
// Stale CWD (destination) lock for sync-to
// ---------------------------------------------------------------------------

/// (i) Stale CWD lock on sync-to names "lock-freshness precondition" AND
/// "--allow-stale-lock". The fabricated SHA in this fixture resolves nowhere,
/// so the condition that fires is `unresolvable-lock-entry`, not a
/// lock↔HEAD relation mismatch (`stale-lock`) — pinning the route line tells
/// the two apart, since both share the "lock-freshness precondition" prefix
/// and both print `--allow-stale-lock`.
#[test]
fn sync_to_stale_cwd_lock_names_condition_and_flag() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_with_stale_cwd_for_sync_to(tmp.path());

    let assert = rwv()
        .args(["sync-to", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("lock-freshness precondition"),
        "sync-to stale-CWD refusal must name 'lock-freshness precondition'; got:\n{stderr}"
    );
    assert!(
        stderr.contains("--allow-stale-lock"),
        "sync-to stale-CWD refusal must name '--allow-stale-lock'; got:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv explain unresolvable-lock-entry"),
        "sync-to stale-CWD refusal must route to 'unresolvable-lock-entry', the condition its \
         unresolvable fabricated SHA actually drives, not 'stale-lock' or 'target-lock-behind'; \
         got:\n{stderr}"
    );
}

/// (ii) --allow-stale-lock bypasses the CWD stale-lock precondition for sync-to.
#[test]
fn sync_to_allow_stale_lock_bypasses_cwd_precondition() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_with_stale_cwd_for_sync_to(tmp.path());

    // Use --strategy=ff: ww's server is at the same SHA as primary's lock,
    // so the convergence step is a no-op and ff succeeds.
    rwv()
        .args([
            "sync-to",
            &primary.root.to_string_lossy(),
            "--allow-stale-lock",
            "--strategy=ff",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// Stale target (source) lock for sync-to
// ---------------------------------------------------------------------------

/// (i) Stale target lock on sync-to names "lock-freshness precondition" AND
/// "--allow-stale-lock". The target's committed lock is a real, resolvable
/// commit that its reset-back server HEAD lacks, so this drives the
/// lock↔HEAD relation mismatch `stale-lock` — not `unresolvable-lock-entry`
/// (no unresolvable revision here) and not `target-lock-behind` (that arm
/// wants the opposite relation, lock behind HEAD, and never offers
/// `--allow-stale-lock`).
#[test]
fn sync_to_stale_target_lock_names_condition_and_flag() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_with_stale_target_for_sync_to(tmp.path());

    let assert = rwv()
        .args(["sync-to", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("lock-freshness precondition"),
        "sync-to stale-target refusal must name 'lock-freshness precondition'; got:\n{stderr}"
    );
    assert!(
        stderr.contains("--allow-stale-lock"),
        "sync-to stale-target refusal must name '--allow-stale-lock'; got:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv explain stale-lock"),
        "sync-to stale-target refusal must route to 'stale-lock', the lock↔HEAD relation \
         mismatch this fixture actually drives, not 'unresolvable-lock-entry' or \
         'target-lock-behind'; got:\n{stderr}"
    );
}

/// (ii) --allow-stale-lock bypasses the target stale-lock precondition for sync-to.
///
/// Fixture: primary's committed lock says C2 but primary's server was reset
/// to C1. Ww's server is at C2. With --allow-stale-lock, the stale check is
/// skipped; snapshot reads primary's committed lock (C2) for convergence; ww
/// is already at C2 (step 1 is a no-op); step 3 FF-advances primary's server
/// from C1 to C2 (C1 is ancestor of C2) → success.
#[test]
fn sync_to_allow_stale_lock_bypasses_target_precondition() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_with_stale_target_for_sync_to(tmp.path());

    // Default strategy (rebase): step 1 rebases ww against primary's lock
    // state (C2); ww is already at C2, so step 1 is a no-op. Step 3 then
    // FF-advances primary's server from C1 to C2.
    rwv()
        .args([
            "sync-to",
            &primary.root.to_string_lossy(),
            "--allow-stale-lock",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();
}

// ===========================================================================
// 5. Target-side dirty preflight is tracked-only
//
// Doc claim (`rwv explain sync-to`, "Target-side preflights"): sync-to
// refuses on uncommitted TRACKED changes in a target repo. An untracked file
// that doesn't collide with a path the fast-forward writes is not scanned by
// the preflight and must not block the sync. A colliding untracked file is
// still refused, but at fast-forward time rather than up front, naming the
// path and leaving the op resumable via `rwv sync-to --continue`.
// ===========================================================================

/// List savepoints under `refs/rwv/pre-op/` in `repo`, without needing the
/// op-id: enough to tell "a savepoint exists" from "none does", and a red
/// assertion prints which refs survived.
fn savepoint_refs(repo: &Path) -> Vec<String> {
    let out = common::git()
        .args(["for-each-ref", "--format=%(refname)", "refs/rwv/pre-op"])
        .current_dir(repo)
        .output()
        .expect("git for-each-ref failed to start");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

fn savepoint_count(repo: &Path) -> usize {
    savepoint_refs(repo).len()
}

/// A target-side untracked file that the incoming fast-forward never writes
/// is not dirt: sync-to must succeed and the file must survive untouched.
#[test]
fn sync_to_target_non_colliding_untracked_file_syncs_clean() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    let c2 = make_commit(&ww.server_dir, "ww.txt", "workweave\n", "ww: advance");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    // Untracked scratch file in the target, at a path the fast-forward
    // never touches.
    std::fs::write(primary.server_dir.join("scratch.txt"), "scratch\n").unwrap();

    rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        c2,
        "a non-colliding untracked file must not block the fast-forward"
    );
    assert_eq!(
        std::fs::read_to_string(primary.server_dir.join("scratch.txt")).unwrap(),
        "scratch\n",
        "the target's untracked scratch file must survive the sync untouched"
    );
}

/// A target-side untracked file that DOES collide with a path the incoming
/// fast-forward writes still refuses — git itself cannot resolve that
/// collision — but the refusal happens at step 3, names the path, and
/// leaves op-state and savepoints intact so `rwv sync-to --continue`
/// completes once the file is moved or removed.
#[test]
fn sync_to_target_colliding_untracked_file_parks_recoverably_then_continues() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    // ww adds a NEW path the target does not have — exactly what the
    // fast-forward will try to write into the target's worktree.
    let c2 = make_commit(
        &ww.server_dir,
        "newfile.txt",
        "ww content\n",
        "ww: add newfile",
    );
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    // Target holds an untracked file at that exact path — the collision.
    std::fs::write(primary.server_dir.join("newfile.txt"), "primary scratch\n").unwrap();

    let primary_project_tip_before = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);
    let primary_server_tip_before = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);

    let err_output = rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&err_output.stderr);

    assert!(
        stderr.contains("newfile.txt"),
        "refusal must name the colliding path; got:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv sync-to --continue"),
        "refusal must name the resume command; got:\n{stderr}"
    );
    // Pins the named per-repo refusal specifically (not just the raw git
    // stderr, which also happens to mention the path, or the whole-op
    // fallback bail, which also happens to mention --continue).
    assert!(
        stderr.contains("collide with paths this fast-forward would write"),
        "refusal must be the named per-repo collision message, not a raw \
         passthrough of git's own error; got:\n{stderr}"
    );

    // Nothing was clobbered.
    assert_eq!(
        common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]),
        primary_project_tip_before,
        "target project repo must not advance when a manifest repo collided"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        primary_server_tip_before,
        "target server repo must not advance past the collision"
    );
    assert_eq!(
        common::read_normalized(primary.server_dir.join("newfile.txt")),
        "primary scratch\n",
        "the target's untracked file must survive the refused advance"
    );

    // Op-state parked recoverably in both workspaces (record + savepoints).
    assert!(
        ww.root.join(".rwv-op").exists(),
        "owner record must remain in CWD after the collision refusal"
    );
    assert!(
        primary.root.join(".rwv-op-lease").exists(),
        "target lease must remain after the collision refusal"
    );
    assert!(
        savepoint_count(&ww.server_dir) > 0,
        "CWD savepoint must remain after the collision refusal"
    );
    assert!(
        savepoint_count(&primary.server_dir) > 0,
        "target savepoint must remain after the collision refusal"
    );
    assert!(
        savepoint_refs(&primary.server_dir)
            .iter()
            .any(|r| r.ends_with("-target")),
        "the parked op must hold a target-side (-target) savepoint; refs: {:?}",
        savepoint_refs(&primary.server_dir)
    );

    // Move the colliding file aside and resume.
    std::fs::rename(
        primary.server_dir.join("newfile.txt"),
        primary.server_dir.join("newfile.txt.bak"),
    )
    .unwrap();

    rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        c2,
        "--continue should complete the fast-forward once the collision is cleared"
    );
    assert_eq!(
        common::read_normalized(primary.server_dir.join("newfile.txt")),
        "ww content\n",
        "the landed newfile.txt must carry ww's content, not the target's old scratch file"
    );
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "op-state should be cleared once --continue completes"
    );
}

/// A completed sync-to leaves zero `refs/rwv/pre-op/*` on either side: the
/// cleanup phase drops the owner-side savepoints AND the `<op-id>-target`
/// refs it minted for the target's repos. The worktree pairs share one refdb
/// per repo, so each count below covers both sides of that repo at once.
#[test]
fn sync_to_success_drops_every_pre_op_ref_on_both_sides() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    let c2 = make_commit(&ww.server_dir, "ww.txt", "workweave\n", "ww: advance");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert_eq!(
        savepoint_refs(&primary.server_dir),
        Vec::<String>::new(),
        "a completed sync-to must drop every pre-op ref in the manifest repo"
    );
    assert_eq!(
        savepoint_refs(&primary.project_dir),
        Vec::<String>::new(),
        "a completed sync-to must drop every pre-op ref in the project repo"
    );
}

/// Same claim as above, with the target's `.rwv-active` naming a DIFFERENT
/// project than the one being synced. The pointer is ambient state the op
/// never consults for its landing — savepoint creation and advance-target
/// both resolve the target under CWD's project — so cleanup must find the
/// `<op-id>-target` refs where creation minted them, not where the target's
/// pointer happens to aim. Resolving by pointer deletes refs in the wrong
/// project's repos, and since dropping a savepoint swallows misses, every
/// successful sync-to then leaks one `-target` ref per touched repo.
#[test]
fn sync_to_success_drops_target_refs_when_target_active_project_differs() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    let other_project = primary.root.join("projects/other-app");
    init_repo(&other_project);
    write_manifest(&other_project, &[]);
    common::git_in(&other_project, &["add", "rwv.toml"]);
    common::git_in(&other_project, &["commit", "-m", "other-app: manifest"]);
    std::fs::write(primary.root.join(".rwv-active"), "other-app\n").unwrap();

    let c2 = make_commit(&ww.server_dir, "ww.txt", "workweave\n", "ww: advance");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        c2,
        "the landing must reach the synced project regardless of the target's pointer"
    );
    assert_eq!(
        savepoint_refs(&primary.server_dir),
        Vec::<String>::new(),
        "cleanup must drop the target-side refs in the synced project's manifest repo"
    );
    assert_eq!(
        savepoint_refs(&primary.project_dir),
        Vec::<String>::new(),
        "cleanup must drop the target-side refs in the synced project's project repo"
    );
}
