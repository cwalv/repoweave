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
//! - The runtime version banner: `rwv explain <verb>` and `rwv explain
//!   <token>` each open with the same version line `rwv --version` reports.

mod common;

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use repoweave::explain::{documented_tokens, known_verbs};
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

/// A reader that stops reading ends the page quietly: `rwv explain doctor |
/// head -1` is a use of the command, not a crash of it. The page is larger
/// than any pipe buffer, so the write is still in flight when the reader goes.
#[test]
fn a_reader_that_closes_stdout_after_one_line_ends_explain_quietly() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let mut child = Command::new(env!("CARGO_BIN_EXE_rwv"))
        .args(["explain", "doctor"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("rwv runs");
    let mut reader = BufReader::new(child.stdout.take().expect("stdout is piped"));
    let mut first = String::new();
    reader.read_line(&mut first).expect("a first line arrives");
    assert!(first.starts_with("rwv "), "{first:?}");
    drop(reader);

    let out = child.wait_with_output().expect("rwv ends");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "a closed stdout must not be reported:\n{stderr}"
    );
    assert!(out.status.success(), "{:?}\n{stderr}", out.status);
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

/// Checking out the locked SHA detaches HEAD, and push then refuses
/// (`unpushable-repo-branch`; see `remedy_hints_test.rs`). The lock-state
/// mismatch remedy in `rwv explain push`'s Common errors must not offer it.
/// Revert this line to watch it go red: push.md.tmpl:132-134 said "check out
/// the locked SHA in each repo".
#[test]
fn explain_push_lock_state_mismatch_remedy_has_no_dead_checkout_advice() {
    rwv().args(["explain", "push"]).assert().success().stdout(
        predicate::str::contains("check out the locked SHA")
            .not()
            .and(predicate::str::contains("git reset --keep <lock commit>")),
    );
}

/// A `--new` member's worktree, skipped in a workweave because the canonical
/// clone has no commits yet, is created by `rwv lock` then `rwv doctor
/// --fix` (src/add_remove.rs), not `rwv sync` (there is no such verb).
/// Revert this line to watch it go red: add.md.tmpl:33-35 said "run `rwv
/// sync` after the first commit".
#[test]
fn explain_add_worktree_skip_names_lock_then_doctor_fix() {
    let out = rwv().args(["explain", "add"]).assert().success();
    let text = String::from_utf8_lossy(&out.get_output().stdout).replace("\r\n", "\n");
    assert!(!text.contains("run `rwv sync`"), "{text}");
    assert!(
        text.contains("`rwv lock` then\n`rwv doctor --fix`"),
        "{text}"
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

/// A short token must not answer for an unrelated short input.
///
/// The suggestion threshold is a flat edit distance, which reads as "a typo"
/// only while every candidate is longer than the threshold. `io` is two
/// characters, so a distance-2 rule puts it within reach of every string of
/// length four or less. The failure is not a merely unhelpful hint: a
/// suggestion suppresses the external-command pointer, so an operator running
/// a real plugin verb is told about `io` instead of being pointed at their
/// own tool.
#[test]
fn a_short_token_does_not_answer_for_an_unrelated_input() {
    rwv().args(["explain", "foo"]).assert().failure().stderr(
        predicate::str::contains("external command")
            .and(predicate::str::contains("rwv foo --help"))
            .and(predicate::str::contains("did you mean").not()),
    );
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

/// Both `rwv explain <verb>` and `rwv explain <token>` must open with the
/// exact line `rwv --version` prints — the committed `.md` carries no stamp
/// of its own, so this is the only place the running binary's version
/// reaches an `explain` reader. Pinned against the `--version` subprocess
/// output rather than `repoweave::rwv_version()` directly, so the test fails
/// if the emission is dropped from `explain()` even though the library
/// function it would have read from still exists.
#[test]
fn explain_verb_and_token_open_with_the_version_line() {
    let version_assert = rwv().arg("--version").assert().success();
    let version_stdout = String::from_utf8(version_assert.get_output().stdout.clone()).unwrap();
    let version_line = version_stdout.trim_end();

    let verb_assert = rwv().args(["explain", "status"]).assert().success();
    let verb_stdout = String::from_utf8(verb_assert.get_output().stdout.clone()).unwrap();
    assert_eq!(
        verb_stdout.lines().next(),
        Some(version_line),
        "`rwv explain status` must open with the same line as `rwv --version` \
         ({version_line:?}); got:\n{verb_stdout}"
    );

    let token = documented_tokens()
        .into_iter()
        .next()
        .expect("at least one token is documented");
    let token_assert = rwv().args(["explain", token]).assert().success();
    let token_stdout = String::from_utf8(token_assert.get_output().stdout.clone()).unwrap();
    assert_eq!(
        token_stdout.lines().next(),
        Some(version_line),
        "`rwv explain {token}` must open with the same line as `rwv --version` \
         ({version_line:?}); got:\n{token_stdout}"
    );
}

/// Drift-safety check: re-running the generator must produce byte-for-byte
/// the same artifacts already on disk. This is the local equivalent of the
/// CI drift gate (`cargo run --example generate-explain && git diff
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
        .args(["run", "--quiet", "--example", "generate-explain"])
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
        "docs/reference/explain/sync-to.md must exist (run cargo run --example generate-explain)",
    );

    // The sentinel form the generator uses: auto_relock_commit_message("<source>").
    let expected = repoweave::sync::auto_relock_commit_message("<source>");
    assert!(
        content.contains(&expected),
        "sync-to.md does not contain the auto-relock commit message '{expected}'; \
         the {{{{MSG:auto_relock}}}} splice may not have resolved correctly.\n\
         Hint: run `cargo run --example generate-explain` and commit the output.\n\
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
