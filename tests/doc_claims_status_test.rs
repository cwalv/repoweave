//! Integration tests anchoring documented behavior of `rwv status --json`.
//!
//! Doc claims pinned here:
//!
//!   - `rwv status --json` emits the envelope object
//!     `{"$schema": "<url>", "repos": [<RepoStatus>, ...]}` — NOT a bare
//!     array of records (which was the earlier shape).
//!   - The `$schema` URL points at
//!     `docs/reference/schemas/status.json` under the repoweave repo's main
//!     branch (the canonical published artifact location).
//!   - Each per-repo record includes `path`, `absolute_path`, `role`,
//!     `url`, `project`. Earlier the shape had only `path`; downstream
//!     tooling now reads `absolute_path` for workweave/primary
//!     disambiguation.
//!
//! Style note: mirrors `doc_claims_activate_test.rs`'s
//! `make_workspace_with_git_repo` pattern. `rwv status` doesn't need a
//! committed lock or push targets, so the helper is intentionally smaller
//! than the push/update fixtures.

use assert_cmd::Command;
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "--initial-branch=main"]);
    std::fs::write(path.join("README"), "init").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
}

/// Build a workspace with one real manifest repo. Returns (workspace_root,
/// canonical_repo_path, file_url_for_manifest).
fn make_workspace(parent: &Path, project: &str) -> (PathBuf, PathBuf, String) {
    let ws = parent.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let url = common::file_url(&repo_path);
    let manifest = format!(
        "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    std::fs::write(ws.join(".rwv-active"), format!("{project}\n")).unwrap();

    (ws, repo_path, url)
}

// ===========================================================================
// 1. Envelope shape: `{ "$schema", "repos" }`, not a bare array
// ===========================================================================

#[test]
fn status_json_emits_envelope_object_not_bare_array() {
    let tmp = common::tempdir().unwrap();
    let (ws, _repo, _url) = make_workspace(tmp.path(), "alpha");

    let output = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json");

    assert!(
        output.status.success(),
        "status --json should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout should parse as JSON ({e}):\n{stdout}"));

    // The top level is an object envelope, not an array.
    assert!(
        parsed.is_object(),
        "status --json must emit an object envelope (not a bare array); got:\n{stdout}"
    );
    let obj = parsed.as_object().unwrap();
    assert!(
        obj.contains_key("$schema"),
        "envelope must carry `$schema`; got:\n{stdout}"
    );
    assert!(
        obj.contains_key("repos"),
        "envelope must carry a `repos` array; got:\n{stdout}"
    );
    assert!(
        obj["repos"].is_array(),
        "`repos` must be an array; got:\n{stdout}"
    );
}

// ===========================================================================
// 2. `$schema` URL points at docs/reference/schemas/status.json
//
// Doc claim: the embedded schema URL pins to the committed schema artifact
// at `docs/reference/schemas/status.json` under repoweave's main branch.
// Consumers use it to look up the JSON Schema without out-of-band context.
// ===========================================================================

#[test]
fn status_json_schema_url_points_to_committed_artifact() {
    let tmp = common::tempdir().unwrap();
    let (ws, _repo, _url) = make_workspace(tmp.path(), "alpha");

    let output = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout).expect("parseable JSON");

    let schema = parsed["$schema"]
        .as_str()
        .expect("$schema must be a string");

    assert!(
        schema.contains("docs/reference/schemas/status.json"),
        "$schema URL must point at docs/reference/schemas/status.json; got: {schema}"
    );
    // Be defensive: the URL should also identify it as the repoweave artifact
    // (so downstream tooling that fetches it doesn't pull from a fork).
    assert!(
        schema.contains("repoweave"),
        "$schema URL should be the repoweave-published artifact; got: {schema}"
    );
}

// ===========================================================================
// 3. Each repo record carries the identifying fields
//
// Doc claim: per-repo records include `path`, `absolute_path`, `role`,
// `url`, and `project`. `absolute_path` is the disk path the user can `cd`
// into; `project` discriminates between projects when multiple are
// activated simultaneously (a workweave or multi-project workspace).
// ===========================================================================

#[test]
fn status_json_per_repo_record_has_identifying_fields() {
    let tmp = common::tempdir().unwrap();
    let (ws, repo_dir, url) = make_workspace(tmp.path(), "alpha");

    let output = rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .output()
        .expect("rwv status --json");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout).expect("parseable JSON");

    let repos = parsed["repos"].as_array().expect("repos array");
    assert_eq!(repos.len(), 1, "expected one repo entry; got:\n{stdout}");
    let record = &repos[0];

    // Identifying fields.
    assert_eq!(
        record["path"], "github/org/repo",
        "record.path should be the canonical path; got:\n{stdout}"
    );
    let abs = record["absolute_path"]
        .as_str()
        .unwrap_or_else(|| panic!("record.absolute_path should be a string; got:\n{stdout}"));
    // absolute_path should be a real on-disk path. Compare via canonicalize
    // to neutralise macOS /var vs /private/var symlinks.
    let expected_abs = std::fs::canonicalize(&repo_dir).unwrap();
    let got_abs = std::fs::canonicalize(abs).unwrap();
    assert_eq!(
        got_abs, expected_abs,
        "record.absolute_path must point at the on-disk clone; got: {abs}"
    );

    // Role + url come straight from the manifest. The canonical spelling
    // is `owned` — the legacy `role: primary` alias has been removed, so
    // the wire form and the manifest form must agree.
    assert_eq!(
        record["role"], "owned",
        "record.role should mirror the manifest entry; got:\n{stdout}"
    );
    assert_eq!(
        record["url"], url,
        "record.url should mirror the manifest entry; got:\n{stdout}"
    );

    // Project name discriminates multi-project workspaces.
    assert_eq!(
        record["project"], "alpha",
        "record.project should name the owning project; got:\n{stdout}"
    );
}
