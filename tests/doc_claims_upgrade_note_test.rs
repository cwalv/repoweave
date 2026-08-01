//! Doc-claim anchor for `docs/how-to/upgrade-rwv.md`'s Step 1: a legacy
//! `.rwv-workweave` marker cannot be migrated by running `rwv doctor --fix`
//! from inside the workweave it belongs to — only from primary, or with `-C`
//! pointed at primary.
//!
//! Every other command run from inside a legacy-marker workweave refuses at
//! the same resolution step before reaching any verb-specific logic (pinned
//! elsewhere by `legacy_workweave_marker_causes_error_on_rwv_invocation` in
//! `doctor_test.rs`); what is unique to `--fix` is that the refusal names it,
//! so the wording has to send the reader somewhere other than the cwd they
//! are standing in. A test that only checked the primary-invoked path would
//! miss that the self-invoked one stays broken, and one that only checked the
//! sentence would miss whether the command in it can be run from there.

mod common;

use predicates::prelude::*;
use std::path::{Path, PathBuf};

fn make_workspace(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    root
}

/// A workweave directory carrying a legacy (pre-JSON, no `parent:`) marker
/// naming `primary`. The default container sits *beside* primary, at
/// `<tmp>/.workweaves/<flat-name>` — not nested under the primary root — so
/// doctor's scan (which walks the container next to primary, not primary's
/// own tree) finds it.
fn make_legacy_workweave(tmp_root: &Path, primary: &Path, flat_name: &str) -> PathBuf {
    let ww_dir = tmp_root.join(".workweaves").join(flat_name);
    std::fs::create_dir_all(&ww_dir).unwrap();
    let legacy_marker = format!(
        "primary: {}\nproject: my-app\n",
        primary.canonicalize().unwrap().display()
    );
    std::fs::write(ww_dir.join(".rwv-workweave"), legacy_marker).unwrap();
    ww_dir
}

/// Step 1 quotes the refusal verbatim in a `text` fence. Nothing regenerates
/// that page, so the quote drifts silently the moment the sentence changes —
/// and a reader who trusts a stale quote is back to following an instruction
/// the binary no longer gives.
///
/// Compared from `.rwv-workweave` onward: everything left of it is the
/// `<workweave>` placeholder the page uses in place of an absolute path.
/// Catches wording drift, not a quote that has drifted only in its path.
#[test]
fn step_1_quotes_the_refusal_the_binary_actually_prints() {
    let page = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/how-to/upgrade-rwv.md"),
    )
    .unwrap();
    let quoted = page
        .lines()
        .find(|line| line.starts_with("Error: ") && line.contains("legacy workweave marker"))
        .expect("Step 1 must quote the legacy-marker refusal");
    let (_, quoted_tail) = quoted.split_once(".rwv-workweave").unwrap();

    let tmp = common::tempdir().unwrap();
    let primary = make_workspace(tmp.path(), "ws");
    let ww_dir = make_legacy_workweave(tmp.path(), &primary, "ws--feat");

    common::rwv()
        .arg("doctor")
        .current_dir(&ww_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains(quoted_tail.to_string()));
}

/// Running `rwv doctor --fix` from inside the workweave whose own marker is
/// legacy does not migrate it — the refusal it names is not a working
/// instruction from that cwd.
#[test]
fn doctor_fix_run_from_inside_the_workweave_does_not_migrate_its_own_marker() {
    let tmp = common::tempdir().unwrap();
    let primary = make_workspace(tmp.path(), "ws");
    let ww_dir = make_legacy_workweave(tmp.path(), &primary, "ws--feat");
    let marker_path = ww_dir.join(".rwv-workweave");
    let before = std::fs::read_to_string(&marker_path).unwrap();

    common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ww_dir)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("legacy workweave marker")
                // The refusal has to warn the reader off the invocation this
                // test just made. Without these it names `rwv doctor --fix`
                // and no place to run it, which from this cwd is the command
                // that produced the refusal being read.
                .and(predicate::str::contains("from the primary weave"))
                .and(predicate::str::contains("not from inside this workweave"))
                .and(predicate::str::contains("changes nothing")),
        );

    assert_eq!(
        std::fs::read_to_string(&marker_path).unwrap(),
        before,
        "a self-invoked --fix must leave the marker it could not resolve past untouched"
    );
}

/// The refusal is not merely located but executable from where it is printed:
/// carrying out its own instructions, with cwd never leaving the workweave,
/// migrates the marker.
///
/// Distinct from the `-C` test below, which supplies primary from the fixture.
/// Here the argument comes from reading `primary:` out of the marker, which is
/// the step the refusal tells the reader to take — so this reddens if the
/// refusal stops naming `-C`, and it reddens if a legacy marker's `primary:`
/// ever stops being readable before migration, neither of which the fixture-fed
/// version can see.
#[test]
fn the_refusal_names_a_repair_that_works_from_the_shell_showing_it() {
    let tmp = common::tempdir().unwrap();
    let primary = make_workspace(tmp.path(), "ws");
    let ww_dir = make_legacy_workweave(tmp.path(), &primary, "ws--feat");
    let marker_path = ww_dir.join(".rwv-workweave");

    let refusal = String::from_utf8(
        common::rwv()
            .arg("doctor")
            .current_dir(&ww_dir)
            .assert()
            .failure()
            .get_output()
            .stderr
            .clone(),
    )
    .unwrap();
    assert!(
        refusal.contains("--fix -C"),
        "the refusal must name the form that needs no cd, got: {refusal}"
    );

    let named_primary = std::fs::read_to_string(&marker_path)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("primary:").map(|v| v.trim().to_string()))
        .expect("the refusal claims `primary:` is readable before migration");

    common::rwv()
        .args(["doctor", "--fix", "-C"])
        .arg(&named_primary)
        .current_dir(&ww_dir)
        .assert()
        .stdout(
            predicate::str::contains("[fixed]").and(predicate::str::contains("workweave marker")),
        );

    let migrated = std::fs::read_to_string(&marker_path).unwrap();
    serde_json::from_str::<serde_json::Value>(&migrated).unwrap_or_else(|e| {
        panic!("following the refusal must leave JSON ({e}), got:\n{migrated}")
    });
}

/// Pointing `-C` at primary from that same shell — no `cd` required — is a
/// working order: it migrates the marker to JSON in one pass.
#[test]
fn doctor_fix_dash_c_primary_migrates_the_marker_from_inside_the_workweave() {
    let tmp = common::tempdir().unwrap();
    let primary = make_workspace(tmp.path(), "ws");
    let ww_dir = make_legacy_workweave(tmp.path(), &primary, "ws--feat");
    let marker_path = ww_dir.join(".rwv-workweave");

    // Not asserted `.success()`: this minimal fixture has no `projects/my-app`
    // for the marker's own `unregistered-workweave` finding to adopt into,
    // which fails downstream of the migration this test pins. Matches
    // `doctor_fix_migrates_legacy_workweave_marker` in `doctor_test.rs`,
    // which carries the same gap for the same reason.
    common::rwv()
        .args(["doctor", "--fix", "-C"])
        .arg(&primary)
        .current_dir(&ww_dir)
        .assert()
        .stdout(
            predicate::str::contains("[fixed]").and(predicate::str::contains("workweave marker")),
        );

    let migrated = std::fs::read_to_string(&marker_path).unwrap();
    let migrated_json: serde_json::Value = serde_json::from_str(&migrated)
        .unwrap_or_else(|e| panic!("marker must be JSON after --fix ({e}), got:\n{migrated}"));
    assert!(
        migrated_json["parent"].is_string(),
        "migrated marker must carry a backfilled parent, got:\n{migrated}"
    );
}
