//! Tests for `rwv fetch --json`.
//!
//! Two layers:
//! 1. Library-level: round-trip the envelope struct and NDJSON record through
//!    serde so the wire shape (`$schema`, `outcomes` array, `status` field) is
//!    pinned independent of the live fetch path.
//! 2. End-to-end: drive `rwv fetch --json` / `rwv fetch -j 2 --json` through
//!    the binary using local bare repos, parse stdout as JSON / NDJSON, and
//!    assert the documented shape.

use repoweave::fetch::{
    FetchJsonOutput, FetchOutcomeNdjsonRecord, FetchOutcomeOutput, FetchOutcomeStatus,
    FETCH_SCHEMA_URL,
};
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
                absolute_path: "/abs/github/org/alpha".into(),
                status: FetchOutcomeStatus::Ok,
                message: None,
            },
            FetchOutcomeOutput {
                path: "github/org/beta".into(),
                absolute_path: "/abs/github/org/beta".into(),
                status: FetchOutcomeStatus::Skipped,
                message: Some("skipped github/org/beta".into()),
            },
        ],
        resolution: None,
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
        absolute_path: "/abs/github/org/repo".into(),
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
fn fetch_ndjson_record_embeds_schema_and_flattens_outcome() {
    let outcome = FetchOutcomeOutput {
        path: "github/org/repo".into(),
        absolute_path: "/abs/github/org/repo".into(),
        status: FetchOutcomeStatus::Ok,
        message: None,
    };
    let record = FetchOutcomeNdjsonRecord {
        schema: FETCH_SCHEMA_URL,
        outcome: &outcome,
    };
    let v = serde_json::to_value(&record).expect("serializes");

    // The record must be flat: $schema, path, absolute_path, status all at top level.
    assert_eq!(
        v["$schema"],
        serde_json::Value::String(FETCH_SCHEMA_URL.to_string())
    );
    assert_eq!(v["path"], "github/org/repo");
    assert_eq!(v["status"], "ok");
    // Must NOT be nested under an "outcome" key.
    assert!(v.get("outcome").is_none(), "must be flat, got: {v}");
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
    let repo_a_url = format!("file://{}", repo_a_bare.display());

    let repo_b_bare = tmp.path().join("repo-b.git");
    init_bare_repo_with_commit(&repo_b_bare);
    let repo_b_url = format!("file://{}", repo_b_bare.display());

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
    let project_url = format!("file://{}", project_bare.display());

    // Run `rwv fetch --json -j 1` (explicit serial = envelope mode).
    let assert = rwv()
        .args(["fetch", &project_url, "--json", "-j", "1"])
        .current_dir(&workspace)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Must parse as a single JSON document (envelope, not NDJSON).
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
// End-to-end: `rwv fetch -j 2 --json` (NDJSON streaming mode)
// ---------------------------------------------------------------------------

#[test]
fn fetch_json_ndjson_emits_one_record_per_line_under_jobs_gt_one() {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // Create three manifest repos.
    let mut repo_urls = Vec::new();
    let mut repo_paths = Vec::new();
    for i in 0..3 {
        let bare = tmp.path().join(format!("repo-{i}.git"));
        init_bare_repo_with_commit(&bare);
        repo_urls.push(format!("file://{}", bare.display()));
        repo_paths.push(format!("local/org/repo-{i}"));
    }

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);
    let repo_pairs: Vec<(&str, &str)> = repo_paths
        .iter()
        .zip(repo_urls.iter())
        .map(|(p, u)| (p.as_str(), u.as_str()))
        .collect();
    push_manifest_to_bare(&project_bare, &repo_pairs);
    let project_url = format!("file://{}", project_bare.display());

    // Run with -j 2 --json → NDJSON mode.
    let assert = rwv()
        .args(["fetch", &project_url, "--json", "-j", "2"])
        .current_dir(&workspace)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Acceptance: NOT parseable as one big JSON document (NDJSON, not envelope).
    assert!(
        serde_json::from_str::<Value>(&stdout).is_err(),
        "NDJSON stdout must not parse as one document; got:\n{stdout}"
    );

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.len() >= 3,
        "expected >= 3 NDJSON lines (one per manifest repo), got {} lines:\n{stdout}",
        lines.len()
    );

    let mut seen_paths = std::collections::BTreeSet::new();
    for line in &lines {
        // Each non-empty line must start with '{'.
        let trimmed = line.trim_start();
        assert!(
            trimmed.starts_with('{'),
            "NDJSON line must start with `{{`; got: {line}"
        );
        assert!(
            !trimmed.starts_with('['),
            "NDJSON line must not start with `[` (Reporter prefix); got: {line}"
        );

        let v: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line not parseable as JSON ({e}): {line}"));
        let obj = v.as_object().expect("line must be object");

        // Every NDJSON record must carry $schema.
        assert_eq!(
            obj.get("$schema").and_then(Value::as_str),
            Some(FETCH_SCHEMA_URL),
            "every NDJSON record must embed $schema: {line}"
        );
        assert!(obj.contains_key("path"), "missing `path`: {line}");
        assert!(
            obj.contains_key("absolute_path"),
            "missing `absolute_path`: {line}"
        );
        assert!(obj.contains_key("status"), "missing `status`: {line}");

        if let Some(path) = obj.get("path").and_then(Value::as_str) {
            seen_paths.insert(path.to_string());
        }
    }

    // All three manifest repos appear in the stream.
    for rp in &repo_paths {
        assert!(
            seen_paths.contains(rp.as_str()),
            "expected path {rp} in NDJSON stream; seen: {seen_paths:?}\nstdout:\n{stdout}"
        );
    }
}

#[test]
fn fetch_json_ndjson_lines_are_not_interleaved() {
    // Under -j 4 --json, the Mutex stdout_lock must prevent byte interleaving.
    // We verify by parsing every non-empty line as JSON — any torn line fails parse.
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    // Create four repos (enough to exercise the multi-worker path).
    let mut repo_urls = Vec::new();
    let mut repo_paths = Vec::new();
    for i in 0..4 {
        let bare = tmp.path().join(format!("repo-{i}.git"));
        init_bare_repo_with_commit(&bare);
        repo_urls.push(format!("file://{}", bare.display()));
        repo_paths.push(format!("local/org/repo-{i}"));
    }

    let project_bare = tmp.path().join("project.git");
    init_bare_repo(&project_bare);
    let repo_pairs: Vec<(&str, &str)> = repo_paths
        .iter()
        .zip(repo_urls.iter())
        .map(|(p, u)| (p.as_str(), u.as_str()))
        .collect();
    push_manifest_to_bare(&project_bare, &repo_pairs);
    let project_url = format!("file://{}", project_bare.display());

    let assert = rwv()
        .args(["fetch", &project_url, "--json", "-j", "4"])
        .current_dir(&workspace)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("torn/interleaved line ({e}): {line}\nstdout:\n{stdout}"));
        assert!(parsed.is_object(), "line not object: {line}");
    }
}
