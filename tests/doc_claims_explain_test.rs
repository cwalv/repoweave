//! Doc-claim test: `rwv explain <verb>` matches `docs/reference/explain/<verb>.md`
//! byte-for-byte, once the runtime version banner the committed doc does not
//! carry is accounted for.
//!
//! Belt-and-suspenders with `scripts/ci-local.sh` and the existing
//! `explain_test.rs::generator_produces_no_drift_against_committed_artifacts`
//! test. That test confirms the generator's *templates* match the
//! committed Markdown; this one confirms the *runtime* `rwv explain`
//! output matches the same Markdown — closing the loop where someone
//! refactors the runtime dispatch path or adds whitespace/normalization
//! between template and emit. Future refactors of the dispatch path
//! cannot quietly diverge from the docs without this test failing.
//!
//! Why per-verb tests instead of a single loop: a single loop reports
//! "one of {N} verbs drifted" with no useful diff in the failure
//! header; per-verb tests let `cargo test --test doc_claims_explain_test
//! explain_status` work, and the failure name immediately identifies
//! the drifting verb. Each test still does the byte-comparison with a
//! diff-friendly assertion message so the offending lines are
//! visible.
//!
//! The acceptance verb set is `repoweave::explain::known_verbs()` — the same
//! registry `tests/explain_test.rs` and the runtime dispatch read.

use assert_cmd::Command as AssertCommand;
use repoweave::explain::known_verbs;
use std::path::PathBuf;

fn rwv() -> AssertCommand {
    AssertCommand::cargo_bin("rwv").expect("rwv binary should be buildable")
}

fn explain_doc_path(verb: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs/reference/explain")
        .join(format!("{verb}.md"))
}

/// Run `rwv explain <verb>` and return stdout as UTF-8. Asserts success.
fn rwv_explain(verb: &str) -> String {
    let output = rwv().args(["explain", verb]).output().expect("rwv runs");
    assert!(
        output.status.success(),
        "rwv explain {verb} failed: status={:?} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("explain output is valid UTF-8")
}

/// The banner every `explain` invocation opens with: the same string `rwv
/// --version` reports, which the committed doc does not carry.
fn version_banner() -> String {
    format!("rwv {}\n\n", repoweave::rwv_version_with_build())
}

/// Verb-by-verb byte comparison. A custom assertion message surfaces a
/// concise diff hint so a regression maintainer can locate the drift
/// without re-running the generator by hand.
fn assert_byte_identical(verb: &str) {
    let runtime = rwv_explain(verb);
    let doc_path = explain_doc_path(verb);
    let doc = std::fs::read_to_string(&doc_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", doc_path.display()));
    let expected = format!("{}{doc}", version_banner());
    if runtime != expected {
        // Localize the first diverging byte to make the failure actionable.
        let mut first_diff: Option<(usize, u8, u8)> = None;
        for (i, (a, b)) in runtime.bytes().zip(expected.bytes()).enumerate() {
            if a != b {
                first_diff = Some((i, a, b));
                break;
            }
        }
        let trail = if first_diff.is_none() && runtime.len() != expected.len() {
            format!(
                " (lengths differ: runtime={} expected={})",
                runtime.len(),
                expected.len()
            )
        } else if let Some((i, a, b)) = first_diff {
            // Show ~40 bytes of context centered on the divergence.
            let start = i.saturating_sub(20);
            let end = (i + 20).min(runtime.len().min(expected.len()));
            let runtime_ctx = &runtime[start..end];
            let expected_ctx = &expected[start..end];
            format!(
                " first diff at byte {i}: runtime={a:#x} expected={b:#x}\n  runtime ctx: {runtime_ctx:?}\n  expected ctx: {expected_ctx:?}"
            )
        } else {
            String::new()
        };
        panic!(
            "drift: `rwv explain {verb}` does not match the version banner + {}{trail}.\n\
             Hint: re-run `cargo run --example generate-explain` and recommit.",
            doc_path.display()
        );
    }
}

#[test]
fn explain_status_matches_committed_doc() {
    assert_byte_identical("status");
}

#[test]
fn explain_doctor_matches_committed_doc() {
    assert_byte_identical("doctor");
}

#[test]
fn explain_sync_matches_committed_doc() {
    assert_byte_identical("sync");
}

#[test]
fn explain_fetch_matches_committed_doc() {
    assert_byte_identical("fetch");
}

#[test]
fn explain_update_matches_committed_doc() {
    assert_byte_identical("update");
}

#[test]
fn explain_push_matches_committed_doc() {
    assert_byte_identical("push");
}

#[test]
fn explain_prime_matches_committed_doc() {
    assert_byte_identical("prime");
}

#[test]
fn explain_explain_matches_committed_doc() {
    assert_byte_identical("explain");
}

#[test]
fn explain_workweave_matches_committed_doc() {
    assert_byte_identical("workweave");
}

#[test]
fn explain_abort_matches_committed_doc() {
    assert_byte_identical("abort");
}

#[test]
fn explain_add_matches_committed_doc() {
    assert_byte_identical("add");
}

#[test]
fn explain_remove_matches_committed_doc() {
    assert_byte_identical("remove");
}

#[test]
fn explain_lock_matches_committed_doc() {
    assert_byte_identical("lock");
}

#[test]
fn explain_activate_matches_committed_doc() {
    assert_byte_identical("activate");
}

#[test]
fn explain_init_matches_committed_doc() {
    assert_byte_identical("init");
}

/// Single-shot acceptance test: every verb in the acceptance set
/// matches its committed doc. The per-verb tests above give precise
/// failure messages; this one gives a single signal that the whole
/// acceptance surface is pinned in case `known_verbs()` ever grows
/// without each entry getting its own test function.
#[test]
fn every_acceptance_verb_matches_committed_doc() {
    for verb in known_verbs() {
        assert_byte_identical(verb);
    }
}

/// Bound the acceptance set: every `<verb>.md` in
/// `docs/reference/explain/` (except `index.md`) corresponds to a
/// verb in `known_verbs()`. Without this, someone could add a new
/// verb file under `docs/reference/explain/` and the byte-identity
/// check would silently skip it.
#[test]
fn acceptance_verb_set_covers_every_explain_doc() {
    let explain_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/reference/explain");
    let mut on_disk: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&explain_dir).expect("read explain dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem == "index" {
            continue;
        }
        on_disk.push(stem.to_string());
    }
    on_disk.sort();
    let mut declared: Vec<String> = known_verbs().map(|s| s.to_string()).collect();
    declared.sort();
    assert_eq!(
        on_disk, declared,
        "every <verb>.md under docs/reference/explain/ must appear in known_verbs()\n\
         on disk: {on_disk:?}\n\
         declared: {declared:?}"
    );
}
