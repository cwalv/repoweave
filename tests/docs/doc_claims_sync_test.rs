//! Integration tests anchoring documented behavior of `rwv land --json`.
//!
//! Doc claims pinned here:
//!
//!   - `rwv land --json` emits the envelope
//!     `{"$schema": "<url>", "outcomes": [<RepoSyncOutcome>, ...]}`
//!     whatever `-j` is. The field is `outcomes` (we pin to the actual
//!     implementation rather than the informal "results" phrasing in prose).
//!   - The `$schema` URL is embedded in the envelope, pinning consumers at
//!     `docs/reference/schemas/land.json` under repoweave's main branch.
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

use crate::common;

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
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
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
    common::set_up_lock_driver(&primary_project);
    write_manifest(&primary_project, &[(SERVER_PATH, SERVER_URL)]);
    common::fixture_lock(&primary_project, &[(SERVER_PATH, SERVER_URL, &sha)]);
    common::git_in(
        &primary_project,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&primary_project, &["commit", "-m", "lock: initial"]);
    common::record_trunk(&primary_project);
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
            "ww/project",
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
    common::record_feature(&primary_project, "ww/project", "main");
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
const SCHEMA_FRAGMENT: &str = "docs/reference/schemas/land.json";

// ===========================================================================
// 1. Envelope shape under serial mode
//
// Doc claim: `rwv land --json` (no `-j` or `-j 1`) emits an object with
// `$schema` + `outcomes` (an array). The $schema URL is embedded.
// ===========================================================================

#[test]
fn sync_to_json_serial_emits_envelope_with_schema_and_outcomes() {
    let tmp = common::tempdir().unwrap();
    let (_primary, ww, _initial_sha) = make_shared(tmp.path());

    // Advance the workweave's server repo and re-lock so sync from primary
    // has something to converge on (otherwise we'd get no-op, which would
    // still exercise the envelope, but using a real advance also pins the
    // happy-path outcome shape).
    let c2 = make_commit(&ww.server_dir, "ww.txt", "workweave\n", "ww: advance");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    let assert = rwv()
        .args(["land", "--json"])
        .current_dir(&ww.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Whole stdout parses as one JSON document — the envelope.
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("envelope must parse as one JSON doc ({e}):\n{stdout}"));
    let obj = parsed.as_object().expect("envelope is an object");

    // `$schema` URL points at the committed land schema artifact.
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
// 2. Envelope shape under `-j N` (N > 1)
//
// Doc claim: `-j` changes how the work runs, never the shape: under `-j 2`
// `--json` still prints one envelope, its `outcomes` sorted by repo path.
// ===========================================================================

#[test]
fn sync_to_json_parallel_emits_the_envelope() {
    // Two repos, so the parallel pool runs more than one worker. Reuse the
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
    common::set_up_lock_driver(&primary_project);
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
    common::fixture_lock(&primary_project, &lock_refs);
    common::git_in(
        &primary_project,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&primary_project, &["commit", "-m", "lock: initial"]);
    common::record_trunk(&primary_project);
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    // Workweave: worktree-add each repo + the project repo, then advance.
    let ww = parent.join("ww");
    std::fs::create_dir_all(ww.join("github/example")).unwrap();
    std::fs::create_dir_all(ww.join("projects")).unwrap();
    for rp in repo_paths.iter() {
        let dest = ww.join(rp);
        let branch = "ww/project".to_owned();
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
    common::record_feature(&primary_project, "ww/project", "main");
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
    common::fixture_lock(&ww_project, &ww_lock_refs);
    common::git_in(&ww_project, &["add", "rwv.lock"]);
    common::git_in(&ww_project, &["commit", "-m", "lock: ww"]);

    let assert = rwv()
        .args(["land", "--json", "-j", "2"])
        .current_dir(&ww)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not one JSON document ({e}):\n{stdout}"));
    let schema = parsed["$schema"].as_str().expect("$schema missing");
    assert!(schema.contains(SCHEMA_FRAGMENT), "got: {schema}");
    let outcomes = parsed["outcomes"].as_array().expect("outcomes array");
    for o in outcomes {
        for field in ["kind", "path", "absolute_path"] {
            assert!(o.get(field).is_some(), "outcome missing `{field}`: {o}");
        }
    }
    let paths: Vec<&str> = outcomes.iter().filter_map(|o| o["path"].as_str()).collect();
    assert_eq!(paths, repo_paths, "{stdout}");
}

// ===========================================================================
// 3. Per-failure records carry stable kebab-case `kind` tags
//
// Doc claim: when a per-repo sync fails, the outcome's `kind` is `failed`
// and the inner `failure.kind` is a stable kebab-case tag (e.g.
// `rebase-failed`). Consumers branch on these tags without parsing prose.
// ===========================================================================

/// The one-repo manifest whose two sides edit the same file under the rebase
/// strategy mints exactly one outcome, `failed`, whose `failure.kind` is
/// `rebase-failed`.
///
/// The value is what the fixture was measured to produce, not what the
/// documented tag set permits. The wire tags themselves are pinned by direct
/// serde round-trip in `sync_json_test.rs`, which is why this test asserts a
/// value rather than a character class.
#[test]
fn sync_to_json_failed_outcome_has_stable_kebab_kind() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    let c2 = make_commit(&primary.server_dir, "clash.txt", "primary\n", "primary: C2");
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: C2"]);

    let c_ww = make_commit(&ww.server_dir, "clash.txt", "ww\n", "ww: diverge");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww"]);

    let assert = rwv()
        .args(["land", "--json", "--strategy=rebase"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!(
            "envelope must parse ({e}):\n{stdout}\n{}",
            String::from_utf8_lossy(&assert.get_output().stderr)
        )
    });
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
    assert_eq!(kind, "rebase-failed", "\n{stdout}");
}
