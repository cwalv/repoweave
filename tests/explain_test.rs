//! Tests for `rwv explain` JIT-reflection dispatch and generator drift safety.
//!
//! Covers:
//! - Index form (no arg) lists every explainable verb.
//! - Per-verb form prints the bundle.
//! - Unknown verb returns non-zero with a friendly pointer to the index.
//! - JSON-capable verbs' bundles embed a JSON Schema.
//! - Each verb advertised in the acceptance set has a discoverable entry.
//! - Drift safety: re-running the generator over the committed tree
//!   produces no changes (templates + Rust types are in sync).

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::path::PathBuf;
use std::process::Command;

const ACCEPTANCE_VERBS: &[&str] = &[
    "status", "doctor", "sync", "push", "fetch", "update", "prime", "explain",
];

fn rwv() -> AssertCommand {
    AssertCommand::cargo_bin("rwv").expect("rwv binary built")
}

#[test]
fn explain_index_lists_every_acceptance_verb() {
    let assert = rwv().arg("explain").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    for verb in ACCEPTANCE_VERBS {
        assert!(
            stdout.contains(verb),
            "index missing verb '{verb}'; full stdout:\n{stdout}"
        );
    }
    // Index should self-identify so callers can grep for it.
    assert!(
        stdout.contains("rwv explain"),
        "index does not name itself; got:\n{stdout}"
    );
}

#[test]
fn explain_status_includes_purpose_and_schema() {
    rwv().args(["explain", "status"]).assert().success().stdout(
        predicate::str::contains("# rwv status")
            .and(predicate::str::contains("## Purpose"))
            .and(predicate::str::contains("RepoStatus"))
            .and(predicate::str::contains("```json")),
    );
}

#[test]
fn explain_doctor_includes_purpose_and_schema() {
    rwv().args(["explain", "doctor"]).assert().success().stdout(
        predicate::str::contains("# rwv doctor")
            .and(predicate::str::contains("## Purpose"))
            .and(predicate::str::contains("ViolationOutput"))
            .and(predicate::str::contains("```json")),
    );
}

#[test]
fn explain_sync_includes_purpose_and_schema() {
    rwv().args(["explain", "sync"]).assert().success().stdout(
        predicate::str::contains("# rwv sync")
            .and(predicate::str::contains("## Purpose"))
            .and(predicate::str::contains("SyncJsonOutput"))
            .and(predicate::str::contains("```json")),
    );
}

#[test]
fn explain_push_includes_purpose_and_schema() {
    rwv().args(["explain", "push"]).assert().success().stdout(
        predicate::str::contains("# rwv push")
            .and(predicate::str::contains("## Purpose"))
            .and(predicate::str::contains("PushJsonOutput"))
            .and(predicate::str::contains("```json")),
    );
}

#[test]
fn explain_markdown_only_verbs_have_no_schema_block() {
    // prime has no `--json` and therefore no schema section.
    // Note: fetch was made --json-capable in fo-p89x0.1 and update in fo-p89x0.2;
    // both now embed schema blocks and are excluded.
    let verb = "prime";
    let assert = rwv().args(["explain", verb]).assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.contains(&format!("# rwv {verb}")),
        "explain {verb} missing heading; got:\n{stdout}"
    );
    // Markdown-only verbs should not embed a schema block.
    assert!(
        !stdout.contains("\"$schema\""),
        "explain {verb} unexpectedly includes a JSON Schema block; got:\n{stdout}"
    );
}

#[test]
fn explain_fetch_includes_purpose_and_schema() {
    // fetch was made --json-capable in fo-p89x0.1; its bundle now embeds a schema block.
    rwv().args(["explain", "fetch"]).assert().success().stdout(
        predicate::str::contains("# rwv fetch")
            .and(predicate::str::contains("## Purpose"))
            .and(predicate::str::contains("FetchJsonOutput"))
            .and(predicate::str::contains("```json")),
    );
}

#[test]
fn explain_update_includes_schema_block() {
    // update was made --json-capable in fo-p89x0.2; its bundle now embeds a JSON Schema block.
    rwv()
        .args(["explain", "update"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("# rwv update")
                .and(predicate::str::contains("## Output"))
                .and(predicate::str::contains("UpdateJsonOutput"))
                .and(predicate::str::contains("```json")),
        );
}

#[test]
fn explain_self_referential_entry() {
    rwv()
        .args(["explain", "explain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# rwv explain").and(predicate::str::contains("JIT")));
}

#[test]
fn explain_unknown_verb_exits_nonzero_with_friendly_pointer() {
    rwv()
        .args(["explain", "no-such-verb"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("no explain entry for 'no-such-verb'")
                .and(predicate::str::contains("rwv explain")),
        );
}

#[test]
fn every_acceptance_verb_has_a_discoverable_explain_entry() {
    for verb in ACCEPTANCE_VERBS {
        rwv()
            .args(["explain", verb])
            .assert()
            .success()
            .stdout(predicate::str::contains(format!("# rwv {verb}")));
    }
}

/// Drift-safety check: re-running the generator must produce byte-for-byte
/// the same artifacts already on disk. This is the local equivalent of the
/// CI drift gate (`cargo run --bin generate-explain && git diff
/// --exit-code`), but framed against the working tree rather than git so
/// it works pre-commit and in environments without a git checkout.
///
/// We snapshot each generated file, re-run the generator, and compare.
/// Any difference means a template or Rust type changed without
/// regeneration — CI would fail on the same condition.
#[test]
fn generator_produces_no_drift_against_committed_artifacts() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let explain_dir = manifest_dir.join("docs/reference/explain");
    let schemas_dir = manifest_dir.join("docs/reference/schemas");

    // Gather the set of files we expect the generator to (re)write.
    let mut tracked: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&explain_dir).expect("read explain dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|s| s.to_str()) == Some("md") {
            tracked.push(path);
        }
    }
    for entry in std::fs::read_dir(&schemas_dir).expect("read schemas dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            tracked.push(path);
        }
    }
    assert!(
        !tracked.is_empty(),
        "no generated artifacts found under {} or {}",
        explain_dir.display(),
        schemas_dir.display()
    );

    // Snapshot current contents.
    let snapshots: Vec<(PathBuf, String)> = tracked
        .iter()
        .map(|p| {
            let content = std::fs::read_to_string(p).expect("read snapshot");
            (p.clone(), content)
        })
        .collect();

    // Re-run the generator.
    let status = Command::new("cargo")
        .args(["run", "--quiet", "--bin", "generate-explain"])
        .current_dir(&manifest_dir)
        .status()
        .expect("spawn generator");
    assert!(status.success(), "generator exited non-zero");

    // Compare.
    let mut drift: Vec<String> = Vec::new();
    for (path, before) in &snapshots {
        let after = std::fs::read_to_string(path).expect("read post-gen");
        if before != &after {
            drift.push(format!("drift: {}", path.display()));
        }
    }
    assert!(
        drift.is_empty(),
        "regenerating produced drift; commit the generator output or update templates:\n{}",
        drift.join("\n")
    );
}
