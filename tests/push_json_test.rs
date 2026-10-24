//! Tests for `rwv push --json`.
//!
//! Two layers:
//! 1. Library-level snapshot tests of `PushOutcomeOutput` serialization,
//!    so kind tags and field names are pinned independent of the live push path.
//! 2. End-to-end: drive `rwv push --json` through the binary, parse stdout
//!    as JSON, assert envelope + per-repo shape and the project-repo
//!    distinguishability requirement.

use assert_cmd::Command as AssertCommand;
use repoweave::push::{PushJsonOutput, PushOutcomeOutput, PUSH_SCHEMA_URL};
use serde_json::Value;

mod common;

fn rwv() -> AssertCommand {
    common::rwv()
}

// ---------------------------------------------------------------------------
// Library-level snapshot tests: wire-output serialization
// ---------------------------------------------------------------------------

fn to_value(outcome: &PushOutcomeOutput) -> Value {
    serde_json::to_value(outcome).unwrap()
}

#[test]
fn pushed_outcome_serializes_with_kind_pushed() {
    let v = to_value(&PushOutcomeOutput::Pushed {
        path: "github/cwalv/foo".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/abs/foo")),
    });
    assert_eq!(v["kind"], "pushed");
    assert_eq!(v["path"], "github/cwalv/foo");
    assert_eq!(v["absolute_path"], "/abs/foo");
}

#[test]
fn skipped_outcome_serializes_with_kind_skipped() {
    let v = to_value(&PushOutcomeOutput::Skipped {
        path: "github/cwalv/fork".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/abs/fork")),
    });
    assert_eq!(v["kind"], "skipped");
    assert_eq!(v["path"], "github/cwalv/fork");
}

#[test]
fn failed_outcome_serializes_with_kind_failed_and_message() {
    let v = to_value(&PushOutcomeOutput::Failed {
        path: "github/cwalv/broken".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/abs/broken")),
        message: "git push error".into(),
    });
    assert_eq!(v["kind"], "failed");
    assert_eq!(v["message"], "git push error");
}

#[test]
fn project_repo_pushed_serializes_with_project_repo_pushed_kind() {
    let v = to_value(&PushOutcomeOutput::ProjectRepoPushed {
        path: "projects/my-app".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
            "/abs/projects/my-app",
        )),
        project: repoweave::manifest::ProjectName::new("my-app").unwrap(),
    });
    assert_eq!(v["kind"], "project-repo-pushed");
    assert_eq!(v["path"], "projects/my-app");
    assert_eq!(v["project"], "my-app");
}

#[test]
fn project_repo_failed_serializes_with_project_repo_failed_kind() {
    let v = to_value(&PushOutcomeOutput::ProjectRepoFailed {
        path: "projects/my-app".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
            "/abs/projects/my-app",
        )),
        project: repoweave::manifest::ProjectName::new("my-app").unwrap(),
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
                absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                    "/abs/github/org/repo",
                )),
            },
            PushOutcomeOutput::Skipped {
                path: "github/org/fork".into(),
                absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                    "/abs/github/org/fork",
                )),
            },
            PushOutcomeOutput::ProjectRepoPushed {
                path: "projects/my-app".into(),
                absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new(
                    "/abs/projects/my-app",
                )),
                project: repoweave::manifest::ProjectName::new("my-app").unwrap(),
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
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/a"))
    }
    .is_failure());
    assert!(!PushOutcomeOutput::Skipped {
        path: "p".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/a"))
    }
    .is_failure());
    assert!(PushOutcomeOutput::Failed {
        path: "p".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/a")),
        message: "err".into()
    }
    .is_failure());
    assert!(!PushOutcomeOutput::ProjectRepoPushed {
        path: "p".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/a")),
        project: repoweave::manifest::ProjectName::new("proj").unwrap()
    }
    .is_failure());
    assert!(PushOutcomeOutput::ProjectRepoFailed {
        path: "p".into(),
        absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/a")),
        project: repoweave::manifest::ProjectName::new("proj").unwrap(),
        message: "err".into()
    }
    .is_failure());
}

// ---------------------------------------------------------------------------
// Workspace builder (mirrors doc_claims_push_test.rs helpers)
// ---------------------------------------------------------------------------

fn advance_all_and_relock(ws: &common::PushWorkspace, repos: &[(&str, &str)]) {
    let mut manifest_yaml = String::from("[repositories]\n");
    let mut lock_entries: Vec<(String, String, String)> = Vec::new();
    for (rp, role) in repos {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let local = ws.workspace.join(rp);
        std::fs::write(local.join(format!("ch_{}.txt", rp.replace('/', "_"))), "x").unwrap();
        common::git_in(&local, &["add", "."]);
        common::git_in(&local, &["commit", "-m", "advance"]);
        let sha = common::git_in(&local, &["rev-parse", "HEAD"]);
        let bare_url = common::file_url(bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{rp}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
        ));
        lock_entries.push(((*rp).to_string(), bare_url, sha));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();
    let entries: Vec<(&str, &str, &str)> = lock_entries
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &entries);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "advance lock"]);
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
    let ws = common::build_workspace("alpha", &repos);
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
    let ws = common::build_workspace("alpha", &repos);
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
// End-to-end: the worker count does not choose the shape
// ---------------------------------------------------------------------------

const PUSH_REPO_PATHS: &[&str] = &["local/org/alpha", "local/org/beta", "local/org/gamma"];

#[test]
fn push_json_is_one_envelope_whatever_the_worker_count() {
    let repos: Vec<(&str, &str)> = PUSH_REPO_PATHS.iter().map(|p| (*p, "owned")).collect();
    let shape_under = |jobs: &str| {
        let ws = common::build_workspace("alpha", &repos);
        advance_all_and_relock(&ws, &repos);
        let out = rwv()
            .args(["push", "--json", "-j", jobs])
            .current_dir(&ws.workspace)
            .assert()
            .success();
        common::envelope_shape(&out.get_output().stdout, PUSH_SCHEMA_URL, "outcomes")
    };
    let serial = shape_under("1");
    assert_eq!(
        serial.1[..PUSH_REPO_PATHS.len()],
        *PUSH_REPO_PATHS,
        "member records follow the manifest"
    );
    assert_eq!(
        serial.1.len(),
        PUSH_REPO_PATHS.len() + 1,
        "the project-repo record comes last"
    );
    assert_eq!(shape_under("4"), serial);
}
