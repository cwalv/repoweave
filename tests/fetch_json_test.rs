//! Tests for `rwv fetch --json`.
//!
//! Two layers:
//! 1. Library-level: round-trip the envelope struct through serde so the wire
//!    shape (`$schema`, `outcomes` array, `status` field) is pinned
//!    independent of the live fetch path.
//! 2. End-to-end: drive `rwv fetch --json` through the binary using local bare
//!    repos, parse stdout as JSON, and assert the documented shape.

use repoweave::fetch::{FetchJsonOutput, FetchOutcomeOutput, FetchOutcomeStatus, FETCH_SCHEMA_URL};
use serde_json::Value;
use std::path::Path;
use std::process;

mod common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

// ---------------------------------------------------------------------------
// Library-level: serde round-trip / wire-shape snapshot tests
// ---------------------------------------------------------------------------

#[test]
fn fetch_json_envelope_round_trips() {
    let envelope = FetchJsonOutput {
        schema: FETCH_SCHEMA_URL.to_owned(),
        outcomes: vec![
            FetchOutcomeOutput {
                path: "github/org/alpha".into(),
                absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                    "/abs/github/org/alpha",
                )),
                status: FetchOutcomeStatus::Ok,
                message: None,
            },
            FetchOutcomeOutput {
                path: "github/org/beta".into(),
                absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                    "/abs/github/org/beta",
                )),
                status: FetchOutcomeStatus::Skipped,
                message: Some("skipped github/org/beta".into()),
            },
        ],
        placements: Vec::new(),
        resolution: None,
        skipped: Vec::new(),
    };

    let json = serde_json::to_string(&envelope).expect("serializes");
    let v: serde_json::Value = serde_json::from_str(&json).expect("parses");

    // Wire-shape: top-level $schema + outcomes array.
    assert_eq!(
        v["$schema"],
        serde_json::Value::String(FETCH_SCHEMA_URL.to_string())
    );
    let outcomes = v["outcomes"].as_array().expect("outcomes is array");
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0]["path"], "github/org/alpha");
    assert_eq!(outcomes[0]["status"], "ok");
    // ok outcomes must not carry a `message` key (skip_serializing_if = None).
    assert!(
        outcomes[0].get("message").is_none() || outcomes[0]["message"].is_null(),
        "ok outcome must omit message or set null: {}",
        outcomes[0]
    );
    assert_eq!(outcomes[1]["status"], "skipped");
    assert!(
        outcomes[1]["message"].as_str().is_some(),
        "skipped outcome must carry message"
    );

    // Typed round-trip.
    let decoded: FetchJsonOutput = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(decoded.schema, FETCH_SCHEMA_URL);
    assert_eq!(decoded.outcomes.len(), 2);
    assert_eq!(decoded.outcomes[0].path, "github/org/alpha");
    assert_eq!(decoded.outcomes[0].status, FetchOutcomeStatus::Ok);
    assert!(decoded.outcomes[0].message.is_none());
    assert_eq!(decoded.outcomes[1].status, FetchOutcomeStatus::Skipped);
}

#[test]
fn fetch_outcome_failed_serializes() {
    let record = FetchOutcomeOutput {
        path: "github/org/repo".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
            "/abs/github/org/repo",
        )),
        status: FetchOutcomeStatus::Failed,
        message: Some("clone failed: connection refused".into()),
    };
    let v = serde_json::to_value(&record).expect("serializes");
    assert_eq!(v["status"], "failed");
    assert_eq!(v["path"], "github/org/repo");
    assert_eq!(v["absolute_path"], "/abs/github/org/repo");
    assert!(v["message"].as_str().unwrap().contains("clone failed"));
}

#[test]
fn fetch_schema_url_points_at_committed_artifact() {
    assert!(
        FETCH_SCHEMA_URL.ends_with("/docs/reference/schemas/fetch.json"),
        "FETCH_SCHEMA_URL must end with /docs/reference/schemas/fetch.json; got: {FETCH_SCHEMA_URL}"
    );
    assert!(
        FETCH_SCHEMA_URL.contains("cwalv/repoweave"),
        "FETCH_SCHEMA_URL must reference the cwalv/repoweave repo; got: {FETCH_SCHEMA_URL}"
    );
}

// ---------------------------------------------------------------------------
// End-to-end helpers
// ---------------------------------------------------------------------------

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

/// Create a bare repo with an initial commit.
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

/// Push an `rwv.toml` manifest into a bare repo.
fn push_manifest_to_bare(bare: &Path, repos: &[(&str, &str)]) {
    let tmp = common::tempdir().expect("tempdir");
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
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(work.join("rwv.toml"), &manifest_toml).unwrap();
    run(&["add", "rwv.toml"], &work);
    run(&["commit", "-m", "add manifest"], &work);
    run(&["push", "origin", "main"], &work);
}

// ---------------------------------------------------------------------------
// End-to-end: `rwv fetch --json` (serial / envelope mode)
// ---------------------------------------------------------------------------

#[test]
fn fetch_json_envelope_emits_schema_and_outcomes_array() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // Create two manifest repos.
    let repo_a_bare = tmp.path().join("repo-a.git");
    init_bare_repo_with_commit(&repo_a_bare);
    let repo_a_url = common::file_url(&repo_a_bare);

    let repo_b_bare = tmp.path().join("repo-b.git");
    init_bare_repo_with_commit(&repo_b_bare);
    let repo_b_url = common::file_url(&repo_b_bare);

    // Project bare with a manifest pointing at both repos.
    let project_bare = tmp.path().join("myproject.git");
    init_bare_repo(&project_bare);
    push_manifest_to_bare(
        &project_bare,
        &[
            ("local/org/repo-a", &repo_a_url),
            ("local/org/repo-b", &repo_b_url),
        ],
    );
    let project_url = common::file_url(&project_bare);

    // Run `rwv fetch --json -j 1` (explicit serial = envelope mode).
    let assert = rwv()
        .args(["fetch", &project_url, "--json", "-j", "1"])
        .current_dir(&workspace)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Must parse as a single JSON document.
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not parseable as JSON ({e}):\n{stdout}"));
    let obj = parsed.as_object().expect("top level must be object");

    assert_eq!(
        obj.get("$schema").and_then(Value::as_str),
        Some(FETCH_SCHEMA_URL),
        "envelope must include $schema URL"
    );

    let outcomes = obj
        .get("outcomes")
        .and_then(Value::as_array)
        .expect("outcomes must be an array");
    assert_eq!(outcomes.len(), 2, "two repos in manifest, two outcomes");

    for o in outcomes {
        let entry = o.as_object().expect("each outcome must be an object");
        assert!(entry.contains_key("path"), "outcome missing `path`: {o}");
        assert!(
            entry.contains_key("absolute_path"),
            "outcome missing `absolute_path`: {o}"
        );
        assert!(
            entry.contains_key("status"),
            "outcome missing `status`: {o}"
        );
        assert_eq!(
            entry.get("status").and_then(Value::as_str),
            Some("ok"),
            "all repos should succeed: {o}"
        );
    }
}

// ---------------------------------------------------------------------------
// End-to-end: the worker count does not choose the shape
// ---------------------------------------------------------------------------

#[test]
fn fetch_json_is_one_envelope_whatever_the_worker_count() {
    let tmp = common::tempdir().unwrap();
    let repo_paths: Vec<String> = (0..3).map(|i| format!("local/org/repo-{i}")).collect();
    let repo_urls: Vec<String> = (0..3)
        .map(|i| {
            let bare = tmp.path().join(format!("repo-{i}.git"));
            init_bare_repo_with_commit(&bare);
            common::file_url(&bare)
        })
        .collect();
    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);
    let repo_pairs: Vec<(&str, &str)> = repo_paths
        .iter()
        .zip(&repo_urls)
        .map(|(p, u)| (p.as_str(), u.as_str()))
        .collect();
    push_manifest_to_bare(&project_bare, &repo_pairs);
    let project_url = common::file_url(&project_bare);

    let shape_under = |jobs: &str| {
        let workspace = tmp.path().join(format!("ws-j{jobs}"));
        std::fs::create_dir_all(&workspace).unwrap();
        let out = rwv()
            .args(["fetch", &project_url, "--json", "-j", jobs])
            .current_dir(&workspace)
            .assert()
            .success();
        common::envelope_shape(&out.get_output().stdout, FETCH_SCHEMA_URL, "outcomes")
    };
    let serial = shape_under("1");
    assert_eq!(serial.1, repo_paths, "outcomes follow the manifest");
    assert_eq!(shape_under("4"), serial);
}
