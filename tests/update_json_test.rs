//! Tests for `rwv update --json`.
//!
//! Two layers:
//! 1. Library-level: round-trip the `UpdateJsonOutput` envelope type through
//!    serde so the wire shape (`$schema`, `repos` array, per-record fields)
//!    is pinned independent of the live update path.
//! 2. End-to-end: drive `rwv update --json` and `rwv update --json -j 2`
//!    through the binary, parse stdout, assert envelope / NDJSON shape.

use assert_cmd::Command;
use repoweave::update::{
    RepoUpdateRecord, UpdateJsonOutput, UpdateKind, UPDATE_RECORD_SCHEMA_URL, UPDATE_SCHEMA_URL,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn git_run(cwd: &Path, args: &[&str]) -> String {
    let out = common::git()
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git should be available");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn init_bare_repo_with_commit(bare: &Path) {
    let parent = bare.parent().expect("bare repo path needs a parent");
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    git_run(
        parent,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    let seed = parent.join(format!("__seed_{stem}"));
    git_run(
        parent,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    git_run(&seed, &["config", "user.email", "test@test.com"]);
    git_run(&seed, &["config", "user.name", "Test"]);
    std::fs::write(seed.join("README"), "seed").unwrap();
    git_run(&seed, &["add", "."]);
    git_run(&seed, &["commit", "-m", "initial"]);
    git_run(&seed, &["push", "origin", "main"]);
}

/// Push a new commit to a bare repo via a working clone. Returns the new HEAD SHA.
fn advance_bare_main(bare: &Path) -> String {
    let parent = bare.parent().unwrap();
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    let work = parent.join(format!("__adv_{stem}"));
    if !work.exists() {
        git_run(
            parent,
            &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
        );
        git_run(&work, &["config", "user.email", "test@test.com"]);
        git_run(&work, &["config", "user.name", "Test"]);
    } else {
        git_run(&work, &["pull", "origin", "main"]);
    }
    std::fs::write(
        work.join("advance.txt"),
        format!("advance-{stem}-{}", uuid_fragment()),
    )
    .unwrap();
    git_run(&work, &["add", "."]);
    git_run(&work, &["commit", "-m", &format!("advance {stem}")]);
    git_run(&work, &["push", "origin", "main"]);
    git_run(&work, &["rev-parse", "HEAD"])
}

fn uuid_fragment() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos() as u64
}

struct UpdateWorkspace {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    #[allow(dead_code)]
    project_name: String,
    manifest_bares: Vec<(String, PathBuf)>,
}

fn build_workspace(project_name: &str, repos: &[(&str, &str)]) -> UpdateWorkspace {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

    let mut manifest_bares: Vec<(String, PathBuf)> = Vec::new();
    let mut manifest_shas: Vec<(String, String)> = Vec::new();
    let mut manifest_yaml = String::from("[repositories]\n");
    for (repo_path, role) in repos {
        let bare = tmp
            .path()
            .join(format!("{}.git", repo_path.replace('/', "_")));
        init_bare_repo_with_commit(&bare);
        let canonical = workspace.join(repo_path);
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        git_run(
            workspace.parent().unwrap(),
            &[
                "clone",
                "--origin",
                "origin",
                bare.to_str().unwrap(),
                canonical.to_str().unwrap(),
            ],
        );
        git_run(&canonical, &["config", "user.email", "test@test.com"]);
        git_run(&canonical, &["config", "user.name", "Test"]);
        let head = git_run(&canonical, &["rev-parse", "HEAD"]);
        manifest_shas.push(((*repo_path).to_string(), head));
        manifest_bares.push(((*repo_path).to_string(), bare.clone()));
        let bare_url = bare.to_str().unwrap();
        manifest_yaml.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
        ));
    }

    let project_bare = tmp.path().join("project.git");
    init_bare_repo_with_commit(&project_bare);
    let project_dir = workspace.join("projects").join(project_name);
    git_run(
        workspace.parent().unwrap(),
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_dir.to_str().unwrap(),
        ],
    );
    git_run(&project_dir, &["config", "user.email", "test@test.com"]);
    git_run(&project_dir, &["config", "user.name", "Test"]);

    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();
    // Round-trips through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let mut lock_entries = Vec::new();
    for (rp, sha) in &manifest_shas {
        let (_, bare) = manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let bare_url = bare.to_str().unwrap();
        lock_entries.push(format!(
            "{rp:?}: {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {sha:?}}}"
        ));
    }
    let raw_lock = format!("{{\"repositories\": {{{}}}}}", lock_entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "manifest + lock"]);

    std::fs::write(workspace.join(".rwv-active"), format!("{project_name}\n")).unwrap();

    UpdateWorkspace {
        _tmp: tmp,
        workspace,
        project_name: project_name.to_string(),
        manifest_bares,
    }
}

// ===========================================================================
// Library-level: serde round-trip tests
// ===========================================================================

/// Round-trip the UpdateJsonOutput envelope so the wire shape is pinned
/// independent of the live update path.
#[test]
fn update_json_envelope_round_trips() {
    let envelope = UpdateJsonOutput {
        schema_url: UPDATE_SCHEMA_URL.to_string(),
        repos: vec![
            RepoUpdateRecord {
                path: "github/org/repo".into(),
                absolute_path: "/abs/github/org/repo".into(),
                branch: "main".into(),
                kind: UpdateKind::Updated,
                old_sha: Some("abc123".into()),
                new_sha: Some("def456".into()),
                error: None,
            },
            RepoUpdateRecord {
                path: "github/org/other".into(),
                absolute_path: "/abs/github/org/other".into(),
                branch: "main".into(),
                kind: UpdateKind::UpToDate,
                old_sha: Some("aaa111".into()),
                new_sha: Some("aaa111".into()),
                error: None,
            },
        ],
        resolution: None,
    };

    let json = serde_json::to_string(&envelope).expect("serializes");
    let v: serde_json::Value = serde_json::from_str(&json).expect("parses");

    // Top-level: $schema + repos.
    assert_eq!(
        v["$schema"],
        serde_json::Value::String(UPDATE_SCHEMA_URL.to_string())
    );
    let repos = v["repos"].as_array().expect("repos is array");
    assert_eq!(repos.len(), 2);

    // First record: updated.
    assert_eq!(repos[0]["path"], "github/org/repo");
    assert_eq!(repos[0]["kind"], "updated");
    assert_eq!(repos[0]["old_sha"], "abc123");
    assert_eq!(repos[0]["new_sha"], "def456");
    assert!(repos[0].get("error").is_none_or(Value::is_null));

    // Second record: up-to-date.
    assert_eq!(repos[1]["kind"], "up-to-date");

    // Typed round-trip.
    let decoded: UpdateJsonOutput = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(decoded.schema_url, UPDATE_SCHEMA_URL);
    assert_eq!(decoded.repos.len(), 2);
    assert_eq!(decoded.repos[0].kind, UpdateKind::Updated);
    assert_eq!(decoded.repos[1].kind, UpdateKind::UpToDate);
}

/// The `error` field is omitted when `kind != failed` (skip_serializing_if).
#[test]
fn update_json_error_field_omitted_when_not_failed() {
    let record = RepoUpdateRecord {
        path: "github/org/repo".into(),
        absolute_path: "/abs/github/org/repo".into(),
        branch: "main".into(),
        kind: UpdateKind::Updated,
        old_sha: Some("abc".into()),
        new_sha: Some("def".into()),
        error: None,
    };
    let v = serde_json::to_value(&record).unwrap();
    assert!(
        !v.as_object().unwrap().contains_key("error"),
        "error field should be absent when None: {v}"
    );
}

/// A failed record serializes with `kind = failed` and `error` present.
#[test]
fn update_json_failed_record_serializes() {
    let record = RepoUpdateRecord {
        path: "github/org/repo".into(),
        absolute_path: "/abs/github/org/repo".into(),
        branch: "main".into(),
        kind: UpdateKind::Failed,
        old_sha: Some("abc".into()),
        new_sha: None,
        error: Some("git fetch failed".into()),
    };
    let v = serde_json::to_value(&record).unwrap();
    assert_eq!(v["kind"], "failed");
    assert_eq!(v["error"], "git fetch failed");
    assert!(
        !v.as_object().unwrap().contains_key("new_sha"),
        "new_sha should be absent when None: {v}"
    );
}

// ===========================================================================
// End-to-end: `rwv update --json -j 1` emits envelope
// ===========================================================================

#[test]
fn update_json_envelope_emitted_under_j1() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("alpha", &repos);

    // Advance the remote so there is something to update.
    let (_, bare) = &ws.manifest_bares[0];
    let new_sha = advance_bare_main(bare);

    let assert = rwv()
        .args(["update", "--dirty", "--json", "-j", "1"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("-j 1 must emit envelope ({e}):\n{stdout}"));

    let obj = parsed.as_object().expect("top level must be an object");
    assert_eq!(
        obj.get("$schema").and_then(Value::as_str),
        Some(UPDATE_SCHEMA_URL),
        "envelope must have correct $schema"
    );

    let repos_arr = obj
        .get("repos")
        .and_then(Value::as_array)
        .expect("repos must be an array");
    assert_eq!(repos_arr.len(), 1, "one repo in manifest");

    let rec = &repos_arr[0];
    assert_eq!(rec["path"], "local/org/a");
    assert!(
        rec.get("absolute_path").and_then(Value::as_str).is_some(),
        "absolute_path must be present"
    );
    assert_eq!(rec["branch"], "main");
    assert_eq!(rec["kind"], "updated", "repo was advanced so kind=updated");
    assert_eq!(rec["new_sha"], new_sha.as_str());
}

// ===========================================================================
// End-to-end: `rwv update --json -j 2` emits NDJSON
// ===========================================================================

#[test]
fn update_json_ndjson_emitted_under_j_gt_1() {
    let repos = [
        ("local/org/a", "owned"),
        ("local/org/b", "owned"),
        ("local/org/c", "owned"),
    ];
    let ws = build_workspace("beta", &repos);

    // Advance each bare so all three have something to update.
    let mut new_shas: Vec<(String, String)> = Vec::new();
    for (rp, bare) in &ws.manifest_bares {
        let sha = advance_bare_main(bare);
        new_shas.push((rp.clone(), sha));
    }

    let assert = rwv()
        .args(["update", "--dirty", "--json", "-j", "2"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Acceptance: the whole stdout must NOT parse as one JSON document.
    assert!(
        serde_json::from_str::<Value>(&stdout).is_err(),
        "NDJSON stdout must not parse as one document; got:\n{stdout}"
    );

    // Every non-empty line is a self-describing JSON object.
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.len() >= repos.len(),
        "expected >= {} NDJSON lines, got {}:\n{stdout}",
        repos.len(),
        lines.len()
    );

    let mut seen_paths = std::collections::BTreeSet::new();
    for line in &lines {
        let v: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line not valid JSON ({e}): {line}"));
        let obj = v.as_object().unwrap();
        assert_eq!(
            obj.get("$schema").and_then(Value::as_str),
            Some(UPDATE_RECORD_SCHEMA_URL),
            "every NDJSON record must embed $schema: {line}"
        );
        assert!(obj.contains_key("kind"), "missing kind: {line}");
        assert!(obj.contains_key("path"), "missing path: {line}");
        assert!(
            obj.contains_key("absolute_path"),
            "missing absolute_path: {line}"
        );
        assert!(obj.contains_key("branch"), "missing branch: {line}");
        if let Some(path) = obj.get("path").and_then(Value::as_str) {
            seen_paths.insert(path.to_string());
        }
    }

    // All three manifest repos must appear in the stream.
    for (rp, _) in &repos {
        assert!(
            seen_paths.contains(*rp),
            "expected path {rp} in NDJSON stream; got {seen_paths:?}\nstdout:\n{stdout}"
        );
    }

    // Each record must not start with a `[<prefix>]` wrapper.
    for line in &lines {
        let trimmed = line.trim_start();
        assert!(
            trimmed.starts_with('{'),
            "NDJSON line must start with '{{' (no Reporter prefix): {line}"
        );
    }
}

// ===========================================================================
// End-to-end: up-to-date kind when repo is already at branch HEAD
// ===========================================================================

#[test]
fn update_json_up_to_date_when_already_at_head() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("gamma", &repos);
    // Do NOT advance the bare — repo is already at branch HEAD.

    let assert = rwv()
        .args(["update", "--dirty", "--json", "-j", "1"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("must emit envelope ({e}):\n{stdout}"));

    let repos_arr = parsed["repos"].as_array().expect("repos array");
    assert_eq!(repos_arr.len(), 1);
    assert_eq!(
        repos_arr[0]["kind"], "up-to-date",
        "repo at HEAD must be up-to-date: {stdout}"
    );
    // old_sha and new_sha should be equal.
    let old = repos_arr[0]["old_sha"].as_str();
    let new = repos_arr[0]["new_sha"].as_str();
    assert!(
        old.is_some() && old == new,
        "old_sha must equal new_sha for up-to-date: old={old:?} new={new:?}"
    );
}
