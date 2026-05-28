//! Tests for the dangling `.rwv-active` fail-fast behavior.
//!
//! The invariant: when `.rwv-active` names a project whose `projects/<name>/`
//! directory does not exist on disk, every action verb must exit non-zero with
//! a clear, actionable error message rather than silently proceeding into
//! confusing downstream failures.
//!
//! Also covers `rwv doctor` reporting a `dangling-active-project` violation
//! and `rwv doctor --fix` clearing the stale `.rwv-active` file.

use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Workspace helpers
// ---------------------------------------------------------------------------

/// Build a minimal workspace: `projects/` dir, `github/` registry dir.
/// Returns the workspace root.
fn make_workspace(parent: &Path) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

/// Write a minimal `rwv.yaml` manifest (empty repositories section).
fn write_empty_manifest(project_dir: &Path) {
    std::fs::create_dir_all(project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.yaml"),
        "repositories:\n",
    )
    .unwrap();
}

/// Set `.rwv-active` to a project name that does NOT exist on disk.
fn set_dangling_active(ws: &Path, name: &str) {
    std::fs::write(ws.join(".rwv-active"), format!("{name}\n")).unwrap();
}

/// Helper: run `rwv <args>` from `ws`, assert failure, return stderr.
fn fail(ws: &Path, args: &[&str]) -> String {
    let out = common::rwv()
        .args(args)
        .current_dir(ws)
        .assert()
        .failure()
        .get_output()
        .stderr
        .clone();
    String::from_utf8(out).unwrap()
}

/// Standard error fragment all dangling-active errors must contain.
const DANGLING_MARKER: &str = "does not exist";
const ACTIVATE_HINT: &str = "rwv activate";

fn assert_dangling_error(stderr: &str, project_name: &str) {
    assert!(
        stderr.contains(project_name),
        "error must name the missing project `{project_name}`; got: {stderr}"
    );
    assert!(
        stderr.contains(DANGLING_MARKER),
        "error must say `does not exist`; got: {stderr}"
    );
    assert!(
        stderr.contains(ACTIVATE_HINT),
        "error must mention `rwv activate`; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// 1. `rwv lock` fails fast on a dangling .rwv-active
// ---------------------------------------------------------------------------

#[test]
fn lock_fails_on_dangling_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "nonexistent");

    let stderr = fail(&ws, &["lock"]);
    assert_dangling_error(&stderr, "nonexistent");
}

// ---------------------------------------------------------------------------
// 2. `rwv add` fails fast on a dangling .rwv-active
// ---------------------------------------------------------------------------

#[test]
fn add_fails_on_dangling_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "ghost");

    let stderr = fail(&ws, &["add", "https://example.com/owner/repo.git"]);
    assert_dangling_error(&stderr, "ghost");
}

// ---------------------------------------------------------------------------
// 3. `rwv remove` fails fast on a dangling .rwv-active
// ---------------------------------------------------------------------------

#[test]
fn remove_fails_on_dangling_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "phantom");

    let stderr = fail(&ws, &["remove", "github/owner/repo"]);
    assert_dangling_error(&stderr, "phantom");
}

// ---------------------------------------------------------------------------
// 4. `rwv update` fails fast on a dangling .rwv-active
// ---------------------------------------------------------------------------

#[test]
fn update_fails_on_dangling_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "missing-proj");

    let stderr = fail(&ws, &["update"]);
    assert_dangling_error(&stderr, "missing-proj");
}

// ---------------------------------------------------------------------------
// 5. `rwv push` fails fast on a dangling .rwv-active
// ---------------------------------------------------------------------------

#[test]
fn push_fails_on_dangling_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "gone");

    let stderr = fail(&ws, &["push"]);
    assert_dangling_error(&stderr, "gone");
}

// ---------------------------------------------------------------------------
// 6. `rwv status` fails fast on a dangling .rwv-active
// ---------------------------------------------------------------------------

#[test]
fn status_fails_on_dangling_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "vanished");

    let stderr = fail(&ws, &["status"]);
    assert_dangling_error(&stderr, "vanished");
}

// ---------------------------------------------------------------------------
// 7. `rwv sync` fails fast on a dangling .rwv-active
// ---------------------------------------------------------------------------

#[test]
fn sync_fails_on_dangling_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "deleted-proj");

    // `rwv sync` requires a <source> argument; provide "primary" so we get
    // past argument-parsing and hit the active-project check.
    let stderr = fail(&ws, &["sync", "primary"]);
    assert_dangling_error(&stderr, "deleted-proj");
}

// ---------------------------------------------------------------------------
// 8. `rwv sync-to` fails fast on a dangling .rwv-active
// ---------------------------------------------------------------------------

#[test]
fn sync_to_fails_on_dangling_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    // Create a sibling workweave so sync-to has a valid source to parse
    // (the active-project check fires before any git operations).
    set_dangling_active(&ws, "evaporated");

    let stderr = fail(&ws, &["sync-to", "primary"]);
    assert_dangling_error(&stderr, "evaporated");
}

// ---------------------------------------------------------------------------
// 9. Error message lists existing projects when any are present
// ---------------------------------------------------------------------------

#[test]
fn error_lists_existing_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());

    // Create a real project on disk.
    let real_proj = ws.join("projects/real-project");
    write_empty_manifest(&real_proj);

    // But .rwv-active points at a non-existent one.
    set_dangling_active(&ws, "fake");

    let stderr = fail(&ws, &["lock"]);
    assert!(
        stderr.contains("real-project"),
        "error should list existing projects; got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// 10. `rwv doctor` reports dangling-active-project violation (exits non-zero)
// ---------------------------------------------------------------------------

#[test]
fn doctor_reports_dangling_active_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "stale-proj");

    let out = common::rwv()
        .args(["doctor"])
        .current_dir(&ws)
        .assert()
        .failure()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("stale-proj") && stdout.contains("does not exist"),
        "doctor output must name the dangling project; got: {stdout}"
    );
    assert!(
        stdout.contains("dangling-active-project")
            || stdout.contains("stale-proj"),
        "doctor must surface the violation; got: {stdout}"
    );
}

// ---------------------------------------------------------------------------
// 11. `rwv doctor --fix` clears the stale .rwv-active
// ---------------------------------------------------------------------------

#[test]
fn doctor_fix_clears_dangling_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "deleted");

    // --fix should succeed (exit 0 after fixing).
    common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .assert()
        .success();

    // .rwv-active should now be absent (cleared).
    assert!(
        !ws.join(".rwv-active").exists(),
        ".rwv-active must be removed by --fix"
    );
}

// ---------------------------------------------------------------------------
// 12. `rwv doctor --json` includes dangling-active-project kind
// ---------------------------------------------------------------------------

#[test]
fn doctor_json_includes_dangling_active_project() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "absent");

    let out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .assert()
        .failure()
        .get_output()
        .clone();

    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));

    let violations = json["violations"].as_array().expect("violations is array");
    let dangling = violations
        .iter()
        .find(|v| v["kind"] == "dangling-active-project");
    assert!(
        dangling.is_some(),
        "doctor --json must include a dangling-active-project violation; violations: {violations:?}"
    );

    let v = dangling.unwrap();
    assert_eq!(
        v["project"].as_str().unwrap(),
        "absent",
        "violation must name the missing project"
    );
}

// ---------------------------------------------------------------------------
// 13. Commands that genuinely tolerate no active project are unaffected
// ---------------------------------------------------------------------------

/// `rwv prime` and `rwv resolve` must NOT fail when .rwv-active is dangling;
/// they operate on workspace context without needing the project on disk.
#[test]
fn prime_and_resolve_tolerate_dangling_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    set_dangling_active(&ws, "gone");

    // `rwv resolve` prints the workspace path — should not fail.
    common::rwv()
        .args(["resolve"])
        .current_dir(&ws)
        .assert()
        .success();

    // `rwv prime` with --no-suppress also must not fail on a dangling active.
    // (It may or may not emit output — what matters is no crash / non-zero exit
    // specifically from the dangling-active check.)
    common::rwv()
        .args(["prime", "--no-suppress"])
        .current_dir(&ws)
        .assert()
        .success();
}
