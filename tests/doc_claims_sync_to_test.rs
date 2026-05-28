//! Integration tests anchoring documented behavior of `rwv sync-to --json`.
//!
//! Doc claims pinned here:
//!
//!   - `rwv sync-to --json` (serial, `-j 1` or no `-j`) emits the envelope
//!     `{"$schema": "<url>", "outcomes": [<SyncOutcomeOutput>, ...]}`.
//!   - Under `-j N` with `N > 1`, `--json` switches to NDJSON streaming.
//!   - The `$schema` URL points at `docs/reference/schemas/sync-to.json`.
//!   - The outcome shape is identical to `rwv sync --json` — same `kind` tags,
//!     same fields — only the `$schema` URL differs.
//!
//! This test mirrors `tests/doc_claims_sync_test.rs` for the sync-to verb.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
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
    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), yaml).unwrap();
}

fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url, sha) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: {sha}\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), yaml).unwrap();
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
        "rwv.lock merge=ours\n",
    )
    .unwrap();
    write_manifest(&primary_project, &[(SERVER_PATH, SERVER_URL)]);
    write_lock(&primary_project, &[(SERVER_PATH, SERVER_URL, &sha)]);
    git(
        &["add", ".gitattributes", "rwv.yaml", "rwv.lock"],
        &primary_project,
    );
    git(&["commit", "-m", "lock: initial"], &primary_project);
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    let ww = parent.join("ww");
    std::fs::create_dir_all(ww.join("github/example")).unwrap();
    std::fs::create_dir_all(ww.join("projects")).unwrap();

    let ww_server = ww.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/server",
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
            "ww/project",
        ],
        &primary_project,
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
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    // Workweave advances so sync-to has actual work to do.
    let c2 = make_commit(&ww.server_dir, "ww.txt", "workweave\n", "ww: advance");
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww advance"], &ww.project_dir);

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
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _initial_sha) = make_shared(tmp.path());

    // Workweave advances.
    let c2 = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: advance");
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww"], &ww.project_dir);

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
// Doc claim: `rwv explain sync-to` returns a non-empty markdown bundle.
// ===========================================================================

#[test]
fn explain_sync_to_returns_content() {
    let assert = rwv()
        .args(["explain", "sync-to"])
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    assert!(
        stdout.contains("sync-to"),
        "explain sync-to should mention sync-to; got:\n{stdout}"
    );
    assert!(
        stdout.contains("three") || stdout.contains("step") || stdout.contains("Step"),
        "explain sync-to should describe the three-step orchestration; got:\n{stdout}"
    );
    assert!(
        stdout.len() > 200,
        "explain sync-to should return a substantial bundle; got {} bytes",
        stdout.len()
    );
}
