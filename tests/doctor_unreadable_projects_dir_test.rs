//! Regression test: `rwv doctor` against a `projects/` directory that exists
//! but cannot be listed used to print "no projects" and exit clean —
//! `discover_project_paths` swallows exactly that read error, and nothing
//! downstream told the two states apart. `rwv doctor` now reports a
//! `projects-dir-unreadable` finding (Error severity) instead.

mod common;

use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn make_workspace(parent: &Path) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

/// Whether permission bits actually block a read for the user running this
/// suite — `false` under root, where every permission check is a no-op and
/// this test's precondition (an unreadable `projects/`) cannot be built.
///
/// Probed behaviorally rather than by checking the effective uid: this crate
/// carries no dependency that reads it, and behavior is what the test's
/// precondition actually needs.
#[cfg(unix)]
fn permissions_are_enforced(parent: &Path) -> bool {
    let probe = parent.join(".rwv-permission-probe");
    std::fs::create_dir(&probe).unwrap();
    let original = std::fs::metadata(&probe).unwrap().permissions();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o000)).unwrap();
    let blocked = std::fs::read_dir(&probe).is_err();
    std::fs::set_permissions(&probe, original).unwrap();
    std::fs::remove_dir(&probe).unwrap();
    blocked
}

/// Make `dir` unreadable, run `rwv <args>` against `ws`, restore `dir`'s
/// permissions, and return the raw output. Restoring before returning
/// (rather than in a guard the caller must remember) keeps a panicking
/// assertion from leaving the fixture directory locked for whatever cleans
/// up the temp dir afterward.
///
/// This is the half that does not port. Mode `0o000` denies a directory
/// listing on Unix; on Windows the read-only attribute is the nearest
/// spelling and it does not deny one at all, so `rwv doctor` would read the
/// directory, find it empty, and report nothing — leaving the callers below
/// red against correct code. An ACL deny entry is the Windows mechanism, and
/// it is different machinery rather than a different call.
#[cfg(unix)]
fn run_with_unreadable_dir(ws: &Path, dir: &Path, args: &[&str]) -> std::process::Output {
    let original = std::fs::metadata(dir).unwrap().permissions();
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o000)).unwrap();

    let out = common::rwv()
        .args(args)
        .current_dir(ws)
        .output()
        .expect("rwv should run");

    std::fs::set_permissions(dir, original).unwrap();
    out
}

/// Gated on the fixture, not the subject: an unreadable `projects/` is built
/// with a mode bit, and Windows has no attribute that denies a listing.
#[test]
#[cfg(unix)]
fn doctor_reports_an_unreadable_projects_dir() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    if !permissions_are_enforced(tmp.path()) {
        eprintln!(
            "skipping doctor_reports_an_unreadable_projects_dir: permission bits do not \
             block reads for this user (likely root) — the test's precondition (an \
             unreadable projects/) cannot be built here"
        );
        return;
    }

    let out = run_with_unreadable_dir(&ws, &ws.join("projects"), &["doctor"]);
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        combined.contains("projects directory unreadable"),
        "plain doctor should report the unreadable projects/ directory, got:\n{combined}"
    );
    assert!(
        !out.status.success(),
        "an unreadable projects/ is an Error-severity finding and must exit non-zero, \
         got:\n{combined}"
    );
}

/// Gated on the fixture, not the subject: an unreadable `projects/` is built
/// with a mode bit, and Windows has no attribute that denies a listing.
#[test]
#[cfg(unix)]
fn doctor_json_includes_projects_dir_unreadable() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    if !permissions_are_enforced(tmp.path()) {
        eprintln!(
            "skipping doctor_json_includes_projects_dir_unreadable: permission bits do not \
             block reads for this user (likely root) — the test's precondition (an \
             unreadable projects/) cannot be built here"
        );
        return;
    }

    let out = run_with_unreadable_dir(&ws, &ws.join("projects"), &["doctor", "--json"]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        !out.status.success(),
        "an unreadable projects/ is an Error-severity finding and must exit non-zero, \
         got:\n{stdout}"
    );

    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));

    let violations = json["violations"].as_array().expect("violations is array");
    let found = violations
        .iter()
        .find(|v| v["kind"] == "projects-dir-unreadable");
    assert!(
        found.is_some(),
        "doctor --json must include a projects-dir-unreadable violation; violations: {violations:?}"
    );
}
