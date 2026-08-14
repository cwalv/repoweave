//! Tests for `rwv push --json`.
//!
//! Two layers:
//! 1. Library-level snapshot tests of `PushOutcomeOutput` serialisation,
//!    so kind tags and field names are pinned independent of the live push path.
//! 2. End-to-end: drive `rwv push --json` through the binary, parse stdout
//!    as JSON, assert envelope + per-repo shape and the project-repo
//!    distinguishability requirement.

use assert_cmd::Command as AssertCommand;
use repoweave::push::{PushJsonOutput, PushOutcomeOutput, PUSH_RECORD_SCHEMA_URL, PUSH_SCHEMA_URL};
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> AssertCommand {
    common::rwv()
}

fn git_run(cwd: &Path, args: &[&str]) -> String {
    let output = common::git()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should be available");
    if !output.status.success() {
        panic!(
            "git {:?} in {} failed: {}",
            args,
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

// ---------------------------------------------------------------------------
// Library-level snapshot tests: wire-output serialisation
// ---------------------------------------------------------------------------

fn to_value(outcome: &PushOutcomeOutput) -> Value {
    serde_json::to_value(outcome).unwrap()
}

#[test]
fn pushed_outcome_serializes_with_kind_pushed() {
    let v = to_value(&PushOutcomeOutput::Pushed {
        path: "github/cwalv/foo".into(),
        absolute_path: "/abs/foo".into(),
    });
    assert_eq!(v["kind"], "pushed");
    assert_eq!(v["path"], "github/cwalv/foo");
    assert_eq!(v["absolute_path"], "/abs/foo");
}

#[test]
fn skipped_outcome_serializes_with_kind_skipped() {
    let v = to_value(&PushOutcomeOutput::Skipped {
        path: "github/cwalv/fork".into(),
        absolute_path: "/abs/fork".into(),
    });
    assert_eq!(v["kind"], "skipped");
    assert_eq!(v["path"], "github/cwalv/fork");
}

#[test]
fn failed_outcome_serializes_with_kind_failed_and_message() {
    let v = to_value(&PushOutcomeOutput::Failed {
        path: "github/cwalv/broken".into(),
        absolute_path: "/abs/broken".into(),
        message: "git push error".into(),
    });
    assert_eq!(v["kind"], "failed");
    assert_eq!(v["message"], "git push error");
}

#[test]
fn project_repo_pushed_serializes_with_project_repo_pushed_kind() {
    let v = to_value(&PushOutcomeOutput::ProjectRepoPushed {
        path: "projects/my-app".into(),
        absolute_path: "/abs/projects/my-app".into(),
        project: "my-app".into(),
    });
    assert_eq!(v["kind"], "project-repo-pushed");
    assert_eq!(v["path"], "projects/my-app");
    assert_eq!(v["project"], "my-app");
}

#[test]
fn project_repo_failed_serializes_with_project_repo_failed_kind() {
    let v = to_value(&PushOutcomeOutput::ProjectRepoFailed {
        path: "projects/my-app".into(),
        absolute_path: "/abs/projects/my-app".into(),
        project: "my-app".into(),
        message: "push failed: no remote".into(),
    });
    assert_eq!(v["kind"], "project-repo-failed");
    assert_eq!(v["project"], "my-app");
    assert_eq!(v["message"], "push failed: no remote");
}

/// The project-repo kind tags are distinguishable from manifest-repo kind tags.
/// This is the core distinguishability requirement from the spec.
#[test]
fn project_repo_kind_is_distinguishable_from_manifest_kind() {
    let manifest_kinds = ["pushed", "skipped", "failed"];
    let project_kinds = ["project-repo-pushed", "project-repo-failed"];

    for mk in &manifest_kinds {
        for pk in &project_kinds {
            assert_ne!(
                mk, pk,
                "manifest kind {mk} must differ from project kind {pk}"
            );
        }
    }

    // Project-repo kinds both start with "project-repo-" prefix.
    for pk in &project_kinds {
        assert!(
            pk.starts_with("project-repo-"),
            "project-repo kind must start with 'project-repo-': {pk}"
        );
    }

    // Manifest-repo kinds do not start with "project-repo-" prefix.
    for mk in &manifest_kinds {
        assert!(
            !mk.starts_with("project-repo-"),
            "manifest kind must NOT start with 'project-repo-': {mk}"
        );
    }
}

/// Envelope round-trip: build a synthetic PushJsonOutput and verify the wire shape.
#[test]
fn push_json_envelope_round_trips() {
    let envelope = PushJsonOutput {
        schema_url: PUSH_SCHEMA_URL.to_string(),
        outcomes: vec![
            PushOutcomeOutput::Pushed {
                path: "github/org/repo".into(),
                absolute_path: "/abs/github/org/repo".into(),
            },
            PushOutcomeOutput::Skipped {
                path: "github/org/fork".into(),
                absolute_path: "/abs/github/org/fork".into(),
            },
            PushOutcomeOutput::ProjectRepoPushed {
                path: "projects/my-app".into(),
                absolute_path: "/abs/projects/my-app".into(),
                project: "my-app".into(),
            },
        ],
        resolution: None,
    };

    let json = serde_json::to_string(&envelope).expect("serializes");
    let v: Value = serde_json::from_str(&json).expect("parses");

    assert_eq!(v["$schema"], PUSH_SCHEMA_URL);
    let outcomes = v["outcomes"].as_array().expect("outcomes is array");
    assert_eq!(outcomes.len(), 3);
    assert_eq!(outcomes[0]["kind"], "pushed");
    assert_eq!(outcomes[1]["kind"], "skipped");
    assert_eq!(outcomes[2]["kind"], "project-repo-pushed");
    assert_eq!(outcomes[2]["project"], "my-app");

    // Typed round-trip back to the struct.
    let decoded: PushJsonOutput = serde_json::from_str(&json).expect("deserializes");
    assert_eq!(decoded.schema_url, PUSH_SCHEMA_URL);
    assert_eq!(decoded.outcomes.len(), 3);
    assert!(!decoded.outcomes[2].is_failure());
}

#[test]
fn push_outcome_is_failure_only_for_failed_variants() {
    assert!(!PushOutcomeOutput::Pushed {
        path: "p".into(),
        absolute_path: "/a".into()
    }
    .is_failure());
    assert!(!PushOutcomeOutput::Skipped {
        path: "p".into(),
        absolute_path: "/a".into()
    }
    .is_failure());
    assert!(PushOutcomeOutput::Failed {
        path: "p".into(),
        absolute_path: "/a".into(),
        message: "err".into()
    }
    .is_failure());
    assert!(!PushOutcomeOutput::ProjectRepoPushed {
        path: "p".into(),
        absolute_path: "/a".into(),
        project: "proj".into()
    }
    .is_failure());
    assert!(PushOutcomeOutput::ProjectRepoFailed {
        path: "p".into(),
        absolute_path: "/a".into(),
        project: "proj".into(),
        message: "err".into()
    }
    .is_failure());
}

// ---------------------------------------------------------------------------
// Workspace builder (mirrors doc_claims_push_test.rs helpers)
// ---------------------------------------------------------------------------

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

struct PushWorkspace {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    project_name: String,
    _project_bare: PathBuf,
    manifest_bares: Vec<(String, PathBuf)>,
}

fn build_workspace(project_name: &str, repos: &[(&str, &str)]) -> PushWorkspace {
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
        let bare_url = common::file_url(&bare);
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
        let bare_url = common::file_url(bare);
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

    PushWorkspace {
        _tmp: tmp,
        workspace,
        project_name: project_name.to_string(),
        _project_bare: project_bare,
        manifest_bares,
    }
}

fn advance_all_and_relock(ws: &PushWorkspace, repos: &[(&str, &str)]) {
    let mut manifest_yaml = String::from("[repositories]\n");
    let mut lock_entries = Vec::new();
    for (rp, role) in repos {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let local = ws.workspace.join(rp);
        std::fs::write(local.join(format!("ch_{}.txt", rp.replace('/', "_"))), "x").unwrap();
        git_run(&local, &["add", "."]);
        git_run(&local, &["commit", "-m", "advance"]);
        let sha = git_run(&local, &["rev-parse", "HEAD"]);
        let bare_url = common::file_url(bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{rp}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
        ));
        lock_entries.push(format!(
            "{rp:?}: {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {sha:?}}}"
        ));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();
    // Round-trips through the real parser + `lock::write_lock` (see
    // `build_workspace` above for why).
    let raw_lock = format!("{{\"repositories\": {{{}}}}}", lock_entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "advance lock"]);
}

// ---------------------------------------------------------------------------
// End-to-end: `rwv push --json` envelope
// ---------------------------------------------------------------------------

/// `rwv push --json` emits a valid envelope with `$schema` and `outcomes`.
/// The project-repo record is the last entry and uses a `project-repo-pushed`
/// kind tag — distinct from manifest-repo `pushed` records.
#[test]
fn push_json_emits_envelope_with_schema_and_outcomes() {
    let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
    let ws = build_workspace("alpha", &repos);
    advance_all_and_relock(&ws, &repos);

    let assert = rwv()
        .args(["push", "--json"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not parseable as JSON ({e}):\n{stdout}"));
    let obj = parsed.as_object().expect("top level is object");

    assert_eq!(
        obj.get("$schema").and_then(Value::as_str),
        Some(PUSH_SCHEMA_URL),
        "envelope must include $schema URL"
    );

    let outcomes = obj
        .get("outcomes")
        .and_then(Value::as_array)
        .expect("outcomes must be an array");

    // Should have 2 manifest repo outcomes + 1 project-repo outcome.
    assert_eq!(
        outcomes.len(),
        3,
        "expected 2 manifest + 1 project-repo: {stdout}"
    );

    for o in &outcomes[..2] {
        let kind = o.get("kind").and_then(Value::as_str).expect("kind field");
        assert_eq!(
            kind, "pushed",
            "manifest repo outcome kind must be 'pushed': {o}"
        );
        assert!(o.get("path").is_some(), "outcome missing path: {o}");
        assert!(
            o.get("absolute_path").is_some(),
            "outcome missing absolute_path: {o}"
        );
    }

    // The last outcome is the project-repo record.
    let last = outcomes.last().unwrap();
    let last_kind = last
        .get("kind")
        .and_then(Value::as_str)
        .expect("kind field");
    assert_eq!(
        last_kind, "project-repo-pushed",
        "last outcome must be project-repo-pushed: {stdout}"
    );
    assert!(
        last.get("project").is_some(),
        "project-repo record must include 'project' field: {last}"
    );
    assert_eq!(
        last.get("project").and_then(Value::as_str),
        Some("alpha"),
        "project field must be the project name: {last}"
    );
}

/// Project-repo record is distinguishable from manifest-repo records:
/// manifest records have kind `pushed`/`skipped`/`failed`; project-repo
/// record has kind `project-repo-pushed` or `project-repo-failed`.
#[test]
fn push_json_project_repo_distinguishable_from_manifest_repos() {
    let repos = [("local/org/a", "owned"), ("local/org/b", "fork")];
    let ws = build_workspace("alpha", &repos);
    advance_all_and_relock(&ws, &repos);

    let assert = rwv()
        .args(["push", "--json"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout).expect("parseable");
    let outcomes = parsed["outcomes"].as_array().expect("outcomes array");

    // Separate manifest records from project-repo record.
    let manifest_records: Vec<&Value> = outcomes
        .iter()
        .filter(|o| {
            let kind = o.get("kind").and_then(Value::as_str).unwrap_or("");
            !kind.starts_with("project-repo-")
        })
        .collect();
    let project_records: Vec<&Value> = outcomes
        .iter()
        .filter(|o| {
            let kind = o.get("kind").and_then(Value::as_str).unwrap_or("");
            kind.starts_with("project-repo-")
        })
        .collect();

    assert_eq!(
        manifest_records.len(),
        2,
        "expected 2 manifest-repo records: {stdout}"
    );
    assert_eq!(
        project_records.len(),
        1,
        "expected exactly 1 project-repo record: {stdout}"
    );

    // Fork is now treated like Owned — should be pushed, not skipped.
    let any_pushed = manifest_records
        .iter()
        .any(|o| o.get("kind").and_then(Value::as_str) == Some("pushed"));
    assert!(
        any_pushed,
        "fork repo must produce 'pushed' outcome (same as Owned): {stdout}"
    );

    // Project-repo record must have `project` field.
    let proj_record = project_records[0];
    assert!(
        proj_record.get("project").is_some(),
        "project-repo record must have 'project' field: {proj_record}"
    );

    // Manifest-repo records must NOT have `project` field (or at least not
    // the same identifying `project` field that marks the project-repo).
    // The distinguishing invariant: only project-repo records have the
    // `project` field.
    for mr in &manifest_records {
        assert!(
            mr.get("project").is_none(),
            "manifest-repo record must NOT have 'project' field: {mr}"
        );
    }
}

// ---------------------------------------------------------------------------
// End-to-end: NDJSON streaming under -j > 1
// ---------------------------------------------------------------------------

const PUSH_REPO_PATHS: &[&str] = &["local/org/alpha", "local/org/beta", "local/org/gamma"];

/// `rwv push --json -j 2` streams NDJSON: one record per line, each
/// self-describing with `$schema`, and the project-repo record last.
#[test]
fn push_json_ndjson_emits_one_record_per_line_under_jobs_gt_one() {
    let repos: Vec<(&str, &str)> = PUSH_REPO_PATHS.iter().map(|p| (*p, "owned")).collect();
    let ws = build_workspace("alpha", &repos);
    advance_all_and_relock(&ws, &repos);

    let assert = rwv()
        .args(["push", "--json", "-j", "2"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // NDJSON: must NOT parse as one big JSON document.
    assert!(
        serde_json::from_str::<Value>(&stdout).is_err(),
        "NDJSON stdout must not parse as a single envelope: {stdout}"
    );

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();

    // 3 manifest repos + 1 project-repo = 4 records.
    assert_eq!(
        lines.len(),
        PUSH_REPO_PATHS.len() + 1,
        "expected {} NDJSON lines (repos + project-repo), got {}:\n{stdout}",
        PUSH_REPO_PATHS.len() + 1,
        lines.len()
    );

    let mut seen_project_repo = false;
    let mut seen_manifest_paths = std::collections::BTreeSet::new();

    for line in &lines {
        let v: Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("line not JSON ({e}): {line}"));
        let obj = v
            .as_object()
            .unwrap_or_else(|| panic!("line not object: {line}"));

        // Every NDJSON record must embed $schema pointing at the per-record
        // artifact, not the serial envelope.
        assert_eq!(
            obj.get("$schema").and_then(Value::as_str),
            Some(PUSH_RECORD_SCHEMA_URL),
            "every NDJSON line must embed $schema: {line}"
        );
        assert!(obj.contains_key("kind"), "missing kind: {line}");
        assert!(obj.contains_key("path"), "missing path: {line}");
        assert!(
            obj.contains_key("absolute_path"),
            "missing absolute_path: {line}"
        );

        let kind = obj["kind"].as_str().unwrap();
        if kind.starts_with("project-repo-") {
            seen_project_repo = true;
            assert!(
                obj.contains_key("project"),
                "project-repo NDJSON record must have 'project' field: {line}"
            );
        } else {
            if let Some(path) = obj.get("path").and_then(Value::as_str) {
                seen_manifest_paths.insert(path.to_string());
            }
        }
    }

    assert!(
        seen_project_repo,
        "NDJSON stream must include a project-repo record: {stdout}"
    );

    // All manifest repos appear.
    for repo_path in PUSH_REPO_PATHS {
        assert!(
            seen_manifest_paths.contains(*repo_path),
            "expected manifest repo {repo_path} in NDJSON stream; got {:?}\n{stdout}",
            seen_manifest_paths
        );
    }
}

/// Under `--json -j > 1`, NDJSON lines must NOT start with `[<prefix>]`
/// (the Reporter parallel prefix must be bypassed in JSON mode).
#[test]
fn push_json_ndjson_no_text_prefix_wrapping() {
    let repos: Vec<(&str, &str)> = PUSH_REPO_PATHS.iter().map(|p| (*p, "owned")).collect();
    let ws = build_workspace("alpha", &repos);
    advance_all_and_relock(&ws, &repos);

    let assert = rwv()
        .args(["push", "--json", "-j", "4"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        assert!(
            !trimmed.starts_with('['),
            "NDJSON line must not start with `[` (Reporter prefix bypass): {line}\n{stdout}"
        );
        if !trimmed.is_empty() {
            assert!(
                trimmed.starts_with('{'),
                "NDJSON line must start with `{{`: {line}\n{stdout}"
            );
        }
    }
}

/// Under `--json -j 1`, output is the envelope (not NDJSON) — pins the
/// serial-mode contract.
#[test]
fn push_json_serial_emits_envelope_with_explicit_jobs_one() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("alpha", &repos);
    advance_all_and_relock(&ws, &repos);

    let assert = rwv()
        .args(["push", "--json", "-j", "1"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("-j 1 must emit envelope, not NDJSON ({e}):\n{stdout}"));
    assert_eq!(
        parsed.get("$schema").and_then(Value::as_str),
        Some(PUSH_SCHEMA_URL)
    );
    let outcomes = parsed["outcomes"].as_array().expect("outcomes");
    // 1 manifest repo + 1 project-repo.
    assert_eq!(outcomes.len(), 2, "expected 2 outcomes: {stdout}");
}

/// NDJSON lines must be complete JSON objects (not interleaved).
#[test]
fn push_json_ndjson_lines_are_not_interleaved() {
    let repos: Vec<(&str, &str)> = PUSH_REPO_PATHS.iter().map(|p| (*p, "owned")).collect();
    let ws = build_workspace("alpha", &repos);
    advance_all_and_relock(&ws, &repos);

    let assert = rwv()
        .args(["push", "--json", "-j", "4"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("torn/interleaved line ({e}): {line}\n{stdout}"));
        assert!(parsed.is_object(), "line not object: {line}");
    }
}
