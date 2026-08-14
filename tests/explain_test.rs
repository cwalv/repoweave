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
//! - {{MSG:auto_relock}} resolves in the assembled sync-to doc and matches the
//!   string `repoweave::sync::auto_relock_commit_message` emits.

mod common;

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use repoweave::explain::known_verbs;
use std::path::PathBuf;
use std::process::Command;

fn rwv() -> AssertCommand {
    AssertCommand::cargo_bin("rwv").expect("rwv binary built")
}

#[test]
fn explain_index_lists_every_acceptance_verb() {
    let assert = rwv().arg("explain").assert().success();
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    for verb in known_verbs() {
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
    // Note: fetch and update were later made --json-capable;
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
    // fetch is --json-capable; its bundle now embeds a schema block.
    rwv().args(["explain", "fetch"]).assert().success().stdout(
        predicate::str::contains("# rwv fetch")
            .and(predicate::str::contains("## Purpose"))
            .and(predicate::str::contains("FetchJsonOutput"))
            .and(predicate::str::contains("```json")),
    );
}

#[test]
fn explain_update_includes_schema_block() {
    // update is --json-capable; its bundle now embeds a JSON Schema block.
    rwv().args(["explain", "update"]).assert().success().stdout(
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
    // With external-subcommand fallthrough, `rwv explain <foo>`
    // for a non-core `foo` that is NOT a close typo of a core verb redirects
    // the operator to `rwv foo --help`. explain deliberately never execs PATH
    // content — plugins own their own `--help`.
    rwv()
        .args(["explain", "no-such-verb"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("external command")
                .and(predicate::str::contains("rwv no-such-verb --help")),
        );
}

#[test]
fn explain_close_typo_suggests_status() {
    // "statu" is one deletion away from "status" — should trigger did-you-mean.
    rwv()
        .args(["explain", "statu"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("did you mean: status"));
}

#[test]
fn explain_close_typo_suggests_sync_to() {
    // "sync-tto" has an extra 't' — should suggest "sync-to".
    rwv()
        .args(["explain", "sync-tto"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("did you mean: sync-to"));
}

#[test]
fn explain_far_typo_no_spurious_suggestion() {
    // "frobnicate" is unrelated to any known verb — the "did you mean" hint
    // must not fire spuriously. With external-subcommand fallthrough,
    // the message redirects to the plugin's own `--help` for
    // any non-close input; the "did you mean" absence guarantee stays.
    rwv()
        .args(["explain", "frobnicate"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("external command")
                .and(predicate::str::contains("rwv frobnicate --help"))
                .and(predicate::str::contains("did you mean").not()),
        );
}

#[test]
fn every_acceptance_verb_has_a_discoverable_explain_entry() {
    for verb in known_verbs() {
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
        .map(|p| (p.clone(), common::read_normalized(p)))
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
        // Read modulo the eol filter, the same equivalence the gate's
        // `git diff` drift stage applies: a checkout smudged to CRLF is not
        // drift against the LF bytes the generator writes.
        let after = common::read_normalized(path);
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

/// Verify that the `{{MSG:auto_relock}}` splice mechanism works end-to-end:
///
/// 1. The assembled `sync-to.md` must contain the exact string that
///    `repoweave::sync::auto_relock_commit_message` emits (with the `<source>`
///    sentinel the generator uses).  This proves the doc and the code share one
///    origin and will never silently diverge again.
///
/// 2. No raw `{{MSG:…}}` placeholder must survive in any assembled explain doc
///    (the generator resolves all of them or aborts with an error, but this
///    guards against a regression in the resolver's coverage).
#[test]
fn msg_auto_relock_splice_resolves_and_matches_code() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sync_to_md = manifest_dir.join("docs/reference/explain/sync-to.md");

    let content = std::fs::read_to_string(&sync_to_md).expect(
        "docs/reference/explain/sync-to.md must exist (run cargo run --bin generate-explain)",
    );

    // The sentinel form the generator uses: auto_relock_commit_message("<source>").
    let expected = repoweave::sync::auto_relock_commit_message("<source>");
    assert!(
        content.contains(&expected),
        "sync-to.md does not contain the auto-relock commit message '{expected}'; \
         the {{{{MSG:auto_relock}}}} splice may not have resolved correctly.\n\
         Hint: run `cargo run --bin generate-explain` and commit the output.\n\
         sync-to.md content (first 500 chars):\n{}",
        &content[..content.len().min(500)]
    );

    // Guard: no raw {{MSG:...}} placeholder must survive in any assembled doc.
    let explain_dir = manifest_dir.join("docs/reference/explain");
    let md_files: Vec<_> = std::fs::read_dir(&explain_dir)
        .expect("read explain dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();

    let mut unresolved: Vec<String> = Vec::new();
    for path in &md_files {
        let text = std::fs::read_to_string(path).expect("read assembled md");
        if text.contains("{{MSG:") {
            // Find the first occurrence for the error message.
            let snippet = text
                .find("{{MSG:")
                .map(|i| &text[i..text.len().min(i + 40)])
                .unwrap_or("(unknown)");
            unresolved.push(format!(
                "{}: unresolved placeholder near '{snippet}'",
                path.display()
            ));
        }
    }
    assert!(
        unresolved.is_empty(),
        "raw {{{{MSG:…}}}} placeholders found in assembled explain docs — \
         the generator resolver did not substitute them:\n{}",
        unresolved.join("\n")
    );
}
