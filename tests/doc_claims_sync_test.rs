//! Integration tests anchoring documented behavior of `rwv sync --json`.
//!
//! Doc claims pinned here:
//!
//!   - `rwv sync --json` (serial, `-j 1` or no `-j`) emits the envelope
//!     `{"$schema": "<url>", "outcomes": [<RepoSyncOutcome>, ...]}`.
//!     The field is `outcomes` (we pin to the actual implementation
//!     rather than the informal "results" phrasing in prose).
//!   - Under `-j N` with `N > 1`, `--json` switches to NDJSON streaming:
//!     one record per line, no envelope, each line is a complete JSON
//!     object that embeds its own `$schema`.
//!   - The `$schema` URL is embedded in BOTH the envelope and every NDJSON
//!     record, pinning consumers at
//!     `docs/reference/schemas/sync.json` under repoweave's main branch.
//!   - Per-failure records carry a stable kebab-case `kind` tag at
//!     `outcome.failure.kind` (e.g. `ff-impossible`, `rebase-failed`).
//!     The outcome's own `kind` is `failed` in that case.
//!
//! This test is deliberately CLI-only: the lower-level wire-shape
//! invariants are already pinned in `tests/sync_json_test.rs` via direct
//! serde round-trips. Here we anchor the user-facing contract.

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

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

/// Build a primary workspace + a workweave that shares its repos via
/// `git worktree add`. Returns `(primary, workweave, initial_lock_sha)`.
fn make_shared(parent: &Path) -> (Workspace, Workspace, String) {
    // --- primary ----------------------------------------------------------
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
    write_lock(&primary_project, &[(SERVER_PATH, SERVER_URL, &sha)]);
    common::git_in(
        &primary_project,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&primary_project, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    // --- workweave --------------------------------------------------------
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
            "ww/main",
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

/// Schema fragment for the serial envelope (`-j 1`).
const SCHEMA_FRAGMENT: &str = "docs/reference/schemas/sync.json";
/// Schema fragment for NDJSON per-repo records (`-j N`, `N > 1`).
const RECORD_SCHEMA_FRAGMENT: &str = "docs/reference/schemas/sync-record.json";

// ===========================================================================
// 1. Envelope shape under serial mode
//
// Doc claim: `rwv sync --json` (no `-j` or `-j 1`) emits an object with
// `$schema` + `outcomes` (an array). The $schema URL is embedded.
// ===========================================================================

#[test]
fn sync_json_serial_emits_envelope_with_schema_and_outcomes() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    // Advance the workweave's server repo and re-lock so sync from primary
    // has something to converge on (otherwise we'd get no-op, which would
    // still exercise the envelope, but using a real advance also pins the
    // happy-path outcome shape).
    let c2 = make_commit(&ww.server_dir, "ww.txt", "workweave\n", "ww: advance");
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    let assert = rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--json"])
        .current_dir(&primary.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Whole stdout parses as one JSON document — the envelope.
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope must parse as one JSON doc ({e}):\n{stdout}"));
    let obj = parsed.as_object().expect("envelope is an object");

    // `$schema` URL points at the committed status/sync schema artifact.
    let schema = obj["$schema"]
        .as_str()
        .expect("envelope must carry `$schema` string");
    assert!(
        schema.contains(SCHEMA_FRAGMENT),
        "$schema must point at {SCHEMA_FRAGMENT}; got: {schema}"
    );

    // `outcomes` is a non-empty array of per-repo records.
    let outcomes = obj["outcomes"]
        .as_array()
        .expect("envelope must carry `outcomes` array");
    assert!(
        !outcomes.is_empty(),
        "outcomes array must not be empty; got:\n{stdout}"
    );

    // Each outcome has `kind` + `path` + `absolute_path` (the
    // identifying triple).
    for o in outcomes {
        let entry = o.as_object().expect("each outcome is an object");
        for field in ["kind", "path", "absolute_path"] {
            assert!(entry.contains_key(field), "outcome missing `{field}`: {o}");
        }
    }
}

// ===========================================================================
// 2. NDJSON shape under `-j N` (N > 1)
//
// Doc claim: under `-j > 1` with `--json`, output switches to NDJSON: each
// non-empty line parses as a self-describing JSON record with `$schema`,
// `kind`, `path`, `absolute_path`. The full stdout does NOT parse as one
// document (proves the envelope is bypassed).
// ===========================================================================

#[test]
fn sync_json_parallel_emits_ndjson_records_with_embedded_schema() {
    // For NDJSON we need more than one repo to actually exercise the
    // parallel pool and produce multiple records. Reuse the
    // sync_json_test.rs multi-repo fixture shape but inline-trimmed for
    // the four claims we need to anchor.
    let tmp = common::tempdir().unwrap();
    let parent = tmp.path();

    // Build a primary with two manifest repos.
    let primary = parent.join("primary");
    std::fs::create_dir_all(primary.join("github/example")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let repo_paths = ["github/example/alpha", "github/example/beta"];
    let mut initial_shas = Vec::new();
    for rp in &repo_paths {
        let dir = primary.join(rp);
        initial_shas.push(init_repo(&dir));
    }

    let primary_project = primary.join("projects/web-app");
    init_repo(&primary_project);
    std::fs::write(
        primary_project.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    let manifest_pairs: Vec<(&str, String)> = repo_paths
        .iter()
        .map(|p| (*p, format!("https://github.com/{p}.git")))
        .collect();
    let manifest_refs: Vec<(&str, &str)> = manifest_pairs
        .iter()
        .map(|(p, u)| (*p, u.as_str()))
        .collect();
    write_manifest(&primary_project, &manifest_refs);
    let lock_owned: Vec<(String, String, String)> = repo_paths
        .iter()
        .zip(&initial_shas)
        .map(|(p, sha)| {
            (
                (*p).to_string(),
                format!("https://github.com/{p}.git"),
                sha.clone(),
            )
        })
        .collect();
    let lock_refs: Vec<(&str, &str, &str)> = lock_owned
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    write_lock(&primary_project, &lock_refs);
    common::git_in(
        &primary_project,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&primary_project, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    // Workweave: worktree-add each repo + the project repo, then advance.
    let ww = parent.join("ww");
    std::fs::create_dir_all(ww.join("github/example")).unwrap();
    std::fs::create_dir_all(ww.join("projects")).unwrap();
    for (i, rp) in repo_paths.iter().enumerate() {
        let dest = ww.join(rp);
        let branch = format!("ww/{i}");
        common::git_in(
            primary.join(rp),
            &["worktree", "add", &dest.to_string_lossy(), "-b", &branch],
        );
    }
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

    let mut advanced = Vec::new();
    for (i, rp) in repo_paths.iter().enumerate() {
        let dir = ww.join(rp);
        let sha = make_commit(&dir, &format!("c{i}.txt"), "x\n", &format!("ww {i}"));
        advanced.push(sha);
    }
    let ww_lock_owned: Vec<(String, String, String)> = repo_paths
        .iter()
        .zip(&advanced)
        .map(|(p, sha)| {
            (
                (*p).to_string(),
                format!("https://github.com/{p}.git"),
                sha.clone(),
            )
        })
        .collect();
    let ww_lock_refs: Vec<(&str, &str, &str)> = ww_lock_owned
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    write_lock(&ww_project, &ww_lock_refs);
    common::git_in(&ww_project, &["add", "rwv.lock"]);
    common::git_in(&ww_project, &["commit", "-m", "lock: ww"]);

    let assert = rwv()
        .args(["sync", &ww.to_string_lossy(), "--json", "-j", "2"])
        .current_dir(&primary)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Whole stdout must NOT parse as one document — proves NDJSON, not
    // envelope.
    assert!(
        serde_json::from_str::<Value>(&stdout).is_err(),
        "NDJSON stdout must not parse as one document; got:\n{stdout}"
    );

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.len() >= repo_paths.len(),
        "expected >= {} NDJSON lines, got {}; stdout:\n{stdout}",
        repo_paths.len(),
        lines.len()
    );

    let mut seen_paths = std::collections::BTreeSet::new();
    for line in &lines {
        let v: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line not parseable ({e}): {line}"));
        let obj = v
            .as_object()
            .unwrap_or_else(|| panic!("line not an object: {line}"));
        // $schema embedded per-record must point at the per-record artifact.
        let schema = obj["$schema"]
            .as_str()
            .unwrap_or_else(|| panic!("line missing `$schema`: {line}"));
        assert!(
            schema.contains(RECORD_SCHEMA_FRAGMENT),
            "per-line $schema must point at {RECORD_SCHEMA_FRAGMENT}; got: {schema}"
        );
        // Identifying fields.
        for field in ["kind", "path", "absolute_path"] {
            assert!(obj.contains_key(field), "line missing `{field}`: {line}");
        }
        if let Some(p) = obj["path"].as_str() {
            seen_paths.insert(p.to_string());
        }
    }

    for rp in &repo_paths {
        assert!(
            seen_paths.contains(*rp),
            "expected {rp} in NDJSON stream; saw {:?}\nstdout:\n{stdout}",
            seen_paths
        );
    }
}

// ===========================================================================
// 3. Per-failure records carry stable kebab-case `kind` tags
//
// Doc claim: when a per-repo sync fails, the outcome's `kind` is `failed`
// and the inner `failure.kind` is a stable kebab-case tag (e.g.
// `ff-impossible`). Consumers branch on these tags without parsing prose.
// ===========================================================================

/// The one-repo manifest diverged under the default strategy mints exactly
/// one outcome, `failed`, whose `failure.kind` is `ff-impossible`.
///
/// The value is what the fixture was measured to produce, not what the
/// documented tag set permits: an OR over the three tags is satisfied by a
/// default strategy that silently became `rebase`, and by a divergence the
/// engine mistook for an unreadable HEAD. Both mint a documented tag.
///
/// The three wire tags themselves — that `FastForwardImpossible` serializes
/// `ff-impossible` and not `fast-forward-impossible` — are pinned by direct
/// serde round-trip in `sync_json_test.rs`, which is why this test asserts a
/// value rather than a character class.
#[test]
fn sync_json_failed_outcome_has_stable_kebab_kind() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    // Primary advances to C2; workweave diverges. Strategy=ff cannot
    // resolve the divergence; the per-repo outcome is `failed` with
    // `failure.kind = "ff-impossible"`.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: C2"]);

    let c_ww = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: diverge");
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww"]);

    // --discard-local-commits bypasses Phase 1 ancestor check so the ff
    // failure surfaces in Phase 2 (where the per-repo outcome is produced)
    // rather than failing fast pre-outcome-generation.
    // (Adapted from --force.)
    let assert = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--json",
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope must parse ({e}):\n{stdout}"));
    let outcomes = parsed["outcomes"]
        .as_array()
        .expect("envelope must carry outcomes");

    assert_eq!(
        outcomes.len(),
        1,
        "the manifest names one repo, so one outcome is owed:\n{stdout}"
    );
    let failed = &outcomes[0];
    assert_eq!(failed["path"], Value::from(SERVER_PATH), "\n{stdout}");
    assert_eq!(failed["kind"], Value::from("failed"), "\n{stdout}");

    let failure = failed.get("failure").unwrap_or_else(|| {
        panic!("failed outcome must carry an inner `failure` object:\n{stdout}")
    });
    let kind = failure["kind"]
        .as_str()
        .unwrap_or_else(|| panic!("failure.kind must be a string:\n{stdout}"));
    assert_eq!(kind, "ff-impossible", "\n{stdout}");
}

/// `sync --discard-local-commits` consents to discarding committed divergence
/// (recoverable via the pre-op savepoint), not uncommitted work — a dirty
/// CWD project repo must refuse before any side effects, and the content must
/// survive. (Adapted from sync_force_refuses_when_cwd_project_dirty —
/// same end-state assertions, new flag spelling.)
#[test]
fn sync_discard_local_commits_refuses_when_cwd_project_dirty() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared(tmp.path());

    // Uncommitted edit in ww's project repo — the file --discard-local-commits'
    // hard-reset would have destroyed.
    std::fs::write(ww.project_dir.join("README.md"), "uncommitted edit\n").unwrap();
    let tip_before = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);

    let assert = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("uncommitted"),
        "sync --discard-local-commits must name the dirty-project precondition; got:\n{stderr}"
    );
    assert!(
        !stderr.contains("--force"),
        "refusal must not mention removed --force flag; got:\n{stderr}"
    );

    assert_eq!(
        std::fs::read_to_string(ww.project_dir.join("README.md")).unwrap(),
        "uncommitted edit\n",
        "uncommitted content must survive the refusal byte-for-byte"
    );
    assert_eq!(
        common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
        tip_before,
        "project tip must be untouched"
    );

    // No op-state left behind, and the precondition is satisfiable as
    // documented: once the edit is committed, --discard-local-commits
    // proceeds (the commit is discarded but preserved in the pre-op savepoint).
    common::git_in(&ww.project_dir, &["add", "README.md"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "ww: readme"]);
    rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();
}

// ===========================================================================
// 4. --allow-stale-lock: refusal names condition + flag; flag bypasses both
//    source and destination preconditions.
//
// Doc claim (cli.md §sync, --allow-stale-lock row):
//   "Consent: skip the lock-freshness precondition on both source and
//   destination."
//
// (i)  Without --allow-stale-lock, a stale lock produces an error message
//      that names the condition ("lock-freshness precondition") AND the flag
//      ("--allow-stale-lock").
// (ii) With --allow-stale-lock, the sync succeeds despite the stale lock
//      on both source and destination.
// ===========================================================================

/// Helper: build the two-workspace fixture with a stale destination (CWD) lock.
///
/// Both workspaces start at SHA_INIT, but ww's lock is patched to a fabricated
/// SHA that does not match the actual server HEAD. Primary's lock is fresh.
///
/// When syncing from primary → ww: the lock-freshness check fires on the
/// destination because ww's lock doesn't match ww's actual server HEAD.
/// With --allow-stale-lock, the sync proceeds using primary's lock (SHA_INIT)
/// to converge ww's server, which is already at SHA_INIT → no-op → success.
fn make_shared_with_stale_destination(parent: &Path) -> (Workspace, Workspace) {
    let (primary, ww, initial_sha) = make_shared(parent);

    // Patch ww's lock to a fabricated SHA that doesn't match server HEAD.
    // This is the stale-lock condition: lock says X, repo HEAD says Y.
    let fake_sha = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert_ne!(
        fake_sha,
        initial_sha.as_str(),
        "fake sha must differ from real"
    );
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, fake_sha)]);
    // Commit the stale lock so it is NOT a tracked-dirty file. The stale-lock
    // check (`classify_lock_relations`) reads the lock from disk, so a committed
    // stale lock (pointing at a fake SHA) still triggers the lock-freshness
    // precondition.
    //
    // Why the commit is required (verified): an UNCOMMITTED
    // hand-written fake-SHA lock is GENUINE user dirt by the dirt scan's
    // structural attribution — its blob was never committed anywhere, so it
    // classifies as live working-tree edits, not shared-ref-advance drift —
    // and the dirt refusal would correctly dominate, masking the stale-lock
    // error this test validates. Committing gives the fixture a clean tree
    // while preserving the stale-lock condition.
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "test: plant stale lock"]);

    (primary, ww)
}

/// (i) Stale lock on the *destination* (CWD) produces a refusal that names
/// "lock-freshness precondition" AND "--allow-stale-lock".
#[test]
fn sync_stale_destination_lock_names_condition_and_flag() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_with_stale_destination(tmp.path());

    let assert = rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("lock-freshness precondition"),
        "stale-lock refusal must name 'lock-freshness precondition'; got:\n{stderr}"
    );
    assert!(
        stderr.contains("--allow-stale-lock"),
        "stale-lock refusal must name the '--allow-stale-lock' flag; got:\n{stderr}"
    );
}

/// (ii) --allow-stale-lock bypasses the destination stale-lock precondition.
///
/// With the same stale-destination fixture, passing --allow-stale-lock makes
/// `rwv sync` succeed. The fixture commits the stale lock so the project repo
/// is clean for the pre-flight dirt scan; a --strategy=rebase is used because
/// the stale-lock commit makes ww's project branch 1 commit ahead of primary
/// (the ff ancestry precondition would fire without it).
#[test]
fn sync_allow_stale_lock_bypasses_destination_precondition() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_with_stale_destination(tmp.path());

    rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--allow-stale-lock",
            "--strategy=rebase",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();
}

/// Helper: build a fixture where the *source* (primary) has a stale lock.
///
/// Both workspaces start at SHA_INIT. Primary's lock is patched to a fabricated
/// SHA that does not match the actual server HEAD. WW's lock is fresh.
///
/// When syncing from primary → ww: the lock-freshness check fires on the source
/// because primary's lock doesn't match primary's actual server HEAD.
/// With --allow-stale-lock, sync proceeds using primary's lock as-is. The lock
/// says a fake SHA for the server — the server repo in ww is already clean and
/// at the initial state, so the sync resolves without content conflicts.
///
/// Note: because primary's lock has a fake SHA, the sync may encounter an
/// unknown-ref error during convergence. To avoid this, we make the source
/// lock point at the SAME real SHA as the ww server HEAD (but via a different
/// written string that still doesn't match primary's server HEAD), so that
/// the convergence step can find the target SHA in the ww clone.
fn make_shared_with_stale_source(parent: &Path) -> (Workspace, Workspace) {
    let (primary, ww, initial_sha) = make_shared(parent);

    // Advance primary's server to C2 (without relocking primary's lock).
    // Primary's lock still says initial_sha (stale); server is at C2.
    // The WW's lock says initial_sha and its server IS at initial_sha (clean).
    make_commit(
        &primary.server_dir,
        "primary_advance.txt",
        "primary\n",
        "primary: advance without relock",
    );
    // primary's project lock still records initial_sha — stale (server is at C2).
    // primary's project dir has the old lock committed; the stale check reads
    // the lock from the source's project dir on disk (from the last commit).
    // For the stale-lock check to fire, we don't need to commit the change;
    // we just need lock-on-disk != server-HEAD. Since we didn't relock,
    // primary's lock (committed) = initial_sha, and primary's server HEAD = C2.
    drop(initial_sha);

    (primary, ww)
}

/// (i) Stale lock on the *source* produces a refusal that names both the
/// condition and the flag — establishing that the check runs on both sides.
#[test]
fn sync_stale_source_lock_names_condition_and_flag() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_with_stale_source(tmp.path());

    let assert = rwv()
        .args(["sync", &primary.root.to_string_lossy()])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("lock-freshness precondition"),
        "source stale-lock refusal must name 'lock-freshness precondition'; got:\n{stderr}"
    );
    assert!(
        stderr.contains("--allow-stale-lock"),
        "source stale-lock refusal must name the '--allow-stale-lock' flag; got:\n{stderr}"
    );
}

/// (ii) --allow-stale-lock bypasses the source stale-lock precondition.
#[test]
fn sync_allow_stale_lock_bypasses_source_precondition() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_with_stale_source(tmp.path());

    rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--allow-stale-lock",
        ])
        .current_dir(&ww.root)
        .assert()
        .success();
}
