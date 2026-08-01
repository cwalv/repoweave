//! Doc-claim anchor for the remedies `docs/reference/doctor-findings.md`
//! promises under `unparseable-project`.
//!
//! These tests pin *arrival*, not existence. A remedy sentence is minted at a
//! parse boundary and then travels through an `anyhow` chain, a violation
//! record and a text template before an operator reads it, and every one of
//! those can drop it while the sentence itself stays present and unit-tested.
//! So nothing here inspects an `anyhow::Error`: each test runs the binary and
//! reads what a person would read, because a test that renders the chain more
//! generously than production is green exactly when the operator is stranded.
//!
//! `unparseable-project` covers two files with two different remedies — the
//! manifest is edited by hand, the lock is regenerated — so both are pinned,
//! and so is the claim that the finding names which of the two failed.
//!
//! Residue: these drive `rwv doctor` only. A remedy that arrives through
//! doctor but is dropped on some other verb's path is not covered here, and
//! neither are the remedies of findings other than `unparseable-project`.

mod common;

use std::path::{Path, PathBuf};

/// A workspace holding one project named `alpha`, with `manifest` and `lock`
/// written verbatim. `lock` is skipped when empty, which is the healthy shape
/// for a project that has never been locked.
fn make_workspace(parent: &Path, manifest: &str, lock: &str) -> PathBuf {
    let workspace = parent.join("ws");
    std::fs::create_dir_all(workspace.join("github")).unwrap();
    let project_dir = workspace.join("projects").join("alpha");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    if !lock.is_empty() {
        std::fs::write(project_dir.join("rwv.lock"), lock).unwrap();
    }
    std::fs::write(workspace.join(".rwv-active"), "alpha\n").unwrap();
    workspace
}

/// Everything `rwv doctor` prints, both streams, whatever the exit status.
fn doctor(workspace: &Path, args: &[&str]) -> String {
    let assertion = {
        let mut cmd = common::rwv();
        cmd.arg("doctor");
        for a in args {
            cmd.arg(a);
        }
        cmd.current_dir(workspace).assert()
    };
    let out = assertion.get_output();
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The `message` of the sole `unparseable-project` entry in `--json`.
fn unparseable_message(workspace: &Path) -> String {
    let assertion = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(workspace)
        .assert();
    let stdout = String::from_utf8(assertion.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
    let violations = parsed
        .get("violations")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("violations missing: {parsed}"));
    let entry = violations
        .iter()
        .find(|v| v.get("kind").and_then(|k| k.as_str()) == Some("unparseable-project"))
        .unwrap_or_else(|| panic!("no unparseable-project entry in {parsed}"));
    entry
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_else(|| panic!("no message in {entry}"))
        .to_string()
}

const GOOD_MANIFEST: &str = "[repositories]\n";

/// A lock left in the pre-JSON format — the shape an abandoned workspace has.
const YAML_ERA_LOCK: &str = "repositories:\n  github/acme/lib:\n    version: main\n";

/// The lock is generated, so its remedy is a command, and an operator who is
/// told only that it failed to parse will edit it by hand instead.
#[test]
fn the_lock_remedy_reaches_the_operator() {
    let tmp = common::tempdir().unwrap();
    let workspace = make_workspace(tmp.path(), GOOD_MANIFEST, YAML_ERA_LOCK);

    let combined = doctor(&workspace, &[]);
    assert!(
        combined.contains(&repoweave::manifest::LockFile::unparseable_hint()),
        "doctor must carry the lock's remedy, got:\n{combined}"
    );
}

/// The remedy is not the whole diagnosis: a lock rwv itself just wrote failing
/// to parse means regenerating it will not help, and only the parser's own
/// error says so.
#[test]
fn the_lock_remedy_does_not_displace_the_parser_error() {
    let tmp = common::tempdir().unwrap();
    let workspace = make_workspace(tmp.path(), GOOD_MANIFEST, YAML_ERA_LOCK);

    let message = unparseable_message(&workspace);
    assert!(
        message.contains("line 1"),
        "the parse error must survive alongside the remedy, got:\n{message}"
    );
}

/// One finding covers two files, so what it prints has to say which one.
///
/// Asserted against the rendered report rather than the `message` field: the
/// finding's `manifest_path` names the manifest whatever failed, because it
/// locates the project, so a template that narrates from the fields instead of
/// relaying the message sends the operator to edit a healthy file — and does
/// it without touching `message` at all.
#[test]
fn a_broken_lock_is_reported_against_the_lock() {
    let tmp = common::tempdir().unwrap();
    let workspace = make_workspace(tmp.path(), GOOD_MANIFEST, YAML_ERA_LOCK);

    let combined = doctor(&workspace, &[]);
    assert!(
        combined.contains(repoweave::manifest::LockFile::FILE_NAME),
        "a lock failure must name the lock, got:\n{combined}"
    );
    assert!(
        !combined.contains(repoweave::manifest::Manifest::FILE_NAME),
        "a lock failure must not be reported against the manifest, got:\n{combined}"
    );
}

/// The manifest is hand-authored, so its remedy is the operator's edit — and
/// the thing they need told is that waiting for `--fix` will not help.
#[test]
fn the_manifest_remedy_reaches_the_operator() {
    let tmp = common::tempdir().unwrap();
    let workspace = make_workspace(tmp.path(), "repositories:\n  github/acme/lib:\n", "");

    let combined = doctor(&workspace, &[]);
    assert!(
        combined.contains(&repoweave::manifest::Manifest::unparseable_hint()),
        "doctor must carry the manifest's remedy, got:\n{combined}"
    );
}

/// The manifest's remedy names the operator's edit, so the finding must locate
/// it. Without the line, "fix it yourself" is the whole of what they get.
#[test]
fn a_broken_manifest_is_located_at_its_line() {
    let tmp = common::tempdir().unwrap();
    let workspace = make_workspace(tmp.path(), "[repositories]\n\n\nnot-a-key\n", "");

    let message = unparseable_message(&workspace);
    assert!(
        message.contains("line 4"),
        "the manifest failure must name the offending line, got:\n{message}"
    );
}
