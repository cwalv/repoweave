//! Tests for `rwv update --json`.
//!
//! Two layers:
//! 1. Library-level: round-trip the `UpdateJsonOutput` envelope type through
//!    serde so the wire shape (`$schema`, `repos` array, per-record fields)
//!    is pinned independent of the live update path.
//! 2. End-to-end: drive `rwv update --json` through the binary, parse stdout,
//!    assert the envelope shape whatever `-j` is.

use assert_cmd::Command;
use repoweave::update::{RepoUpdateRecord, UpdateJsonOutput, UpdateKind, UPDATE_SCHEMA_URL};
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn init_bare_repo_with_commit(bare: &Path) {
    let parent = bare.parent().expect("bare repo path needs a parent");
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    common::git_in(
        parent,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    let seed = parent.join(format!("__seed_{stem}"));
    common::git_in(
        parent,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    common::git_in(&seed, &["config", "user.email", "test@test.com"]);
    common::git_in(&seed, &["config", "user.name", "Test"]);
    std::fs::write(seed.join("README"), "seed").unwrap();
    common::git_in(&seed, &["add", "."]);
    common::git_in(&seed, &["commit", "-m", "initial"]);
    common::git_in(&seed, &["push", "origin", "main"]);
}

/// Push a new commit to a bare repo via a working clone. Returns the new HEAD SHA.
fn advance_bare_main(bare: &Path) -> String {
    let parent = bare.parent().unwrap();
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    let work = parent.join(format!("__adv_{stem}"));
    if !work.exists() {
        common::git_in(
            parent,
            &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
        );
        common::git_in(&work, &["config", "user.email", "test@test.com"]);
        common::git_in(&work, &["config", "user.name", "Test"]);
    } else {
        common::git_in(&work, &["pull", "origin", "main"]);
    }
    std::fs::write(
        work.join("advance.txt"),
        format!("advance-{stem}-{}", uuid_fragment()),
    )
    .unwrap();
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", &format!("advance {stem}")]);
    common::git_in(&work, &["push", "origin", "main"]);
    common::git_in(&work, &["rev-parse", "HEAD"])
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
        common::git_in(
            workspace.parent().unwrap(),
            &[
                "clone",
                "--origin",
                "origin",
                bare.to_str().unwrap(),
                canonical.to_str().unwrap(),
            ],
        );
        common::git_in(&canonical, &["config", "user.email", "test@test.com"]);
        common::git_in(&canonical, &["config", "user.name", "Test"]);
        let head = common::git_in(&canonical, &["rev-parse", "HEAD"]);
        manifest_shas.push(((*repo_path).to_string(), head));
        manifest_bares.push(((*repo_path).to_string(), bare.clone()));
        let bare_url = common::file_url(&bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
        ));
    }

    let project_bare = tmp.path().join("project.git");
    init_bare_repo_with_commit(&project_bare);
    let project_dir = workspace.join("projects").join(project_name);
    common::git_in(
        workspace.parent().unwrap(),
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_dir.to_str().unwrap(),
        ],
    );
    common::git_in(&project_dir, &["config", "user.email", "test@test.com"]);
    common::git_in(&project_dir, &["config", "user.name", "Test"]);

    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();
    let mut lock_entries: Vec<(String, String, String)> = Vec::new();
    for (rp, sha) in &manifest_shas {
        let (_, bare) = manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let bare_url = common::file_url(bare);
        lock_entries.push((rp.clone(), bare_url, sha.clone()));
    }
    let entries: Vec<(&str, &str, &str)> = lock_entries
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &entries);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "manifest + lock"]);

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
                absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                    "/abs/github/org/repo",
                )),
                branch: "main".into(),
                kind: UpdateKind::Updated,
                old_sha: Some("abc123".into()),
                new_sha: Some("def456".into()),
                error: None,
            },
            RepoUpdateRecord {
                path: "github/org/other".into(),
                absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                    "/abs/github/org/other",
                )),
                branch: "main".into(),
                kind: UpdateKind::UpToDate,
                old_sha: Some("aaa111".into()),
                new_sha: Some("aaa111".into()),
                error: None,
            },
        ],
        placements: Vec::new(),
        resolution: None,
        skipped: vec![repoweave::skipped::SkippedEffect::tool_missing(
            &repoweave::integrations::NpmWorkspaces,
            repoweave::integration::MissingTool::of::<repoweave::integrations::NpmWorkspaces>(),
        )],
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

    assert_eq!(
        v["skipped"],
        serde_json::json!([{
            "effect": "install-hooks",
            "integration": "npm-workspaces",
            "reason": {"tool-missing": {"tool": "npm"}},
            "remedy": "rwv materialize",
        }])
    );

    // Typed round-trip.
    let decoded: UpdateJsonOutput = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(decoded.schema_url, UPDATE_SCHEMA_URL);
    assert_eq!(decoded.repos.len(), 2);
    assert_eq!(decoded.repos[0].kind, UpdateKind::Updated);
    assert_eq!(decoded.repos[1].kind, UpdateKind::UpToDate);
    assert_eq!(decoded.skipped, envelope.skipped);
}

/// The `error` field is omitted when `kind != failed` (skip_serializing_if).
#[test]
fn update_json_error_field_omitted_when_not_failed() {
    let record = RepoUpdateRecord {
        path: "github/org/repo".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
            "/abs/github/org/repo",
        )),
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
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
            "/abs/github/org/repo",
        )),
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
// End-to-end: the worker count does not choose the shape
// ===========================================================================

#[test]
fn update_json_is_one_envelope_whatever_the_worker_count() {
    let repos = [
        ("local/org/a", "owned"),
        ("local/org/b", "owned"),
        ("local/org/c", "owned"),
    ];
    let shape_under = |project: &str, jobs: &str| {
        let ws = build_workspace(project, &repos);
        for (_, bare) in &ws.manifest_bares {
            advance_bare_main(bare);
        }
        let out = rwv()
            .args(["update", "--dirty", "--json", "-j", jobs])
            .current_dir(&ws.workspace)
            .assert()
            .success();
        common::envelope_shape(&out.get_output().stdout, UPDATE_SCHEMA_URL, "repos")
    };
    let serial = shape_under("beta", "1");
    let manifest_order: Vec<&str> = repos.iter().map(|(path, _)| *path).collect();
    assert_eq!(serial.1, manifest_order, "records follow the manifest");
    assert_eq!(shape_under("beta", "4"), serial);
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
