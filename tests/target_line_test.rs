//! Tests for the "target:" line surfacing.
//!
//! Project-scoped verbs whose active project was chosen by the
//! `.rwv-active` pointer fall-through print a target line to **stderr**
//! before acting, so operators catch a wrong-pointer mis-target at
//! invocation time instead of by post-hoc git-status forensics.
//!
//! Explicitly (`--project`) or structurally (workweave marker) resolved
//! invocations stay silent — the operator or the workweave named the
//! target already.
//!
//! Coverage:
//! - Presence: prints when the pointer decides (project-scoped verbs
//!   through the primary; both 1-project and N-project workspaces).
//! - Silence: never prints under `--project` (explicit).
//! - Silence: never prints when the marker decides (structural).
//! - Silence: never prints for workspace-scoped verbs even when a
//!   `.rwv-active` happens to be set.
//! - Stream discipline: writes to stderr; does not contaminate stdout
//!   (important for `--json` verbs).

use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Build a workspace: `projects/` dir, `github/` registry dir, plus one or
/// more project directories under `projects/<name>/` with an empty
/// `rwv.yaml` each. Returns the workspace root.
fn make_workspace_with_projects(parent: &Path, project_names: &[&str]) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    for name in project_names {
        let dir = ws.join("projects").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rwv.yaml"), "repositories:\n").unwrap();
    }
    ws
}

fn set_active(ws: &Path, name: &str) {
    std::fs::write(ws.join(".rwv-active"), format!("{name}\n")).unwrap();
}

/// The expected target line, matching the resolver's format:
/// `target: workspace <path> · project <name> (.rwv-active)`.
fn expected_target_line(ws: &Path, project: &str) -> String {
    let canon = ws.canonicalize().unwrap();
    format!(
        "target: workspace {} · project {} (.rwv-active)",
        canon.display(),
        project,
    )
}

// ---------------------------------------------------------------------------
// PRESENCE: target line prints when the pointer decides
// ---------------------------------------------------------------------------

/// N-project workspace: `.rwv-active` picks one of many. The pointer
/// decides — target line MUST print to stderr. `rwv status` is a
/// project-scoped verb.
#[test]
fn target_line_prints_when_pointer_decides_multi_project_workspace() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_projects(tmp.path(), &["alpha", "beta", "gamma"]);
    set_active(&ws, "beta");

    let out = common::rwv()
        .args(["status"])
        .current_dir(&ws)
        .assert()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let expected = expected_target_line(&ws, "beta");
    assert!(
        stderr.contains(&expected),
        "expected target line on stderr: {expected}\ngot stderr: {stderr}"
    );
}

/// Single-project workspace: `.rwv-active` still names the project (all
/// creation paths write it). The pointer still decides — target line
/// MUST print. Rule is uniform; no special-case suppression.
#[test]
fn target_line_prints_when_pointer_decides_single_project_workspace() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_projects(tmp.path(), &["only"]);
    set_active(&ws, "only");

    let out = common::rwv()
        .args(["status"])
        .current_dir(&ws)
        .assert()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("target:"),
        "target line must print for pointer-decided invocation; stderr: {stderr}"
    );
    assert!(
        stderr.contains("only"),
        "target line must name the project; stderr: {stderr}"
    );
    assert!(
        stderr.contains("(.rwv-active)"),
        "target line must annotate the provenance; stderr: {stderr}"
    );
}

/// `rwv status --json`: the target line goes to stderr and MUST NOT
/// contaminate the JSON envelope on stdout. This is the whole point of
/// the stderr-vs-stdout discipline.
#[test]
fn target_line_does_not_contaminate_json_stdout() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_projects(tmp.path(), &["a", "b"]);
    set_active(&ws, "a");

    let out = common::rwv()
        .args(["status", "--json"])
        .current_dir(&ws)
        .assert()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    // stderr carries the target line
    assert!(
        stderr.contains("target:"),
        "target line must print to stderr under --json; stderr: {stderr}"
    );
    // stdout stays parseable — no `target:` prose leaks in
    assert!(
        !stdout.contains("target:"),
        "stdout must be JSON only; target line must not leak: {stdout}"
    );
    // And stdout is valid JSON.
    let _parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}\nstdout: {stdout}"));
}

// ---------------------------------------------------------------------------
// SILENCE: target line does NOT print when resolution is explicit or structural
// ---------------------------------------------------------------------------

/// `--project <name>`: explicit invocation. Provenance = Flag. Target
/// line MUST NOT print — the operator already named the target.
#[test]
fn target_line_silent_under_project_flag() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_projects(tmp.path(), &["a", "b"]);
    set_active(&ws, "a");

    let out = common::rwv()
        .args(["status", "--project", "b"])
        .current_dir(&ws)
        .assert()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("target:"),
        "target line must be silent under --project; stderr: {stderr}"
    );
}

/// Workspace with no `.rwv-active` set at all, project passed via
/// `--project`: still silent (no chain step consulted the pointer).
#[test]
fn target_line_silent_when_no_pointer_and_project_flag() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_projects(tmp.path(), &["p"]);
    // No .rwv-active written.

    let out = common::rwv()
        .args(["status", "--project", "p"])
        .current_dir(&ws)
        .assert()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("target:"),
        "target line must be silent without pointer + with --project; stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// SILENCE: workspace-scoped verbs never print the target line
// ---------------------------------------------------------------------------

/// `rwv resolve` is workspace-scoped — no project selection involved.
/// Even with `.rwv-active` set, no target line prints.
#[test]
fn target_line_silent_for_workspace_scoped_verb() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_projects(tmp.path(), &["p"]);
    set_active(&ws, "p");

    let out = common::rwv()
        .args(["resolve"])
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("target:"),
        "workspace-scoped verb must not print target line; stderr: {stderr}"
    );
}

/// `rwv explain` is workspace-scoped — silent.
#[test]
fn target_line_silent_for_explain() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_projects(tmp.path(), &["p"]);
    set_active(&ws, "p");

    let out = common::rwv()
        .args(["explain"])
        .current_dir(&ws)
        .assert()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("target:"),
        "explain must not print target line; stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// PRESENCE: target line prints for other project-scoped verbs
// ---------------------------------------------------------------------------

/// `rwv lock` is project-scoped — prints target line when the pointer
/// decides. Covers a mutating verb (the incident-class case: the
/// v0.14.0 incident was `rwv lock` writing to the wrong project via
/// silent `.rwv-active`).
#[test]
fn target_line_prints_for_lock_when_pointer_decides() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_projects(tmp.path(), &["foundations", "tmuxcc"]);
    set_active(&ws, "tmuxcc");

    let out = common::rwv()
        .args(["lock"])
        .current_dir(&ws)
        .assert()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("target:") && stderr.contains("tmuxcc"),
        "lock must print target line when pointer decides; stderr: {stderr}"
    );
}

/// `rwv doctor` is project-scoped when a project is active — prints
/// target line under pointer-decided resolution.
#[test]
fn target_line_prints_for_doctor_when_pointer_decides() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_projects(tmp.path(), &["a", "b"]);
    set_active(&ws, "b");

    let out = common::rwv()
        .args(["doctor"])
        .current_dir(&ws)
        .assert()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("target:") && stderr.contains("project b"),
        "doctor must print target line when pointer decides; stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// SILENCE: workweave marker resolves the project structurally
// ---------------------------------------------------------------------------

/// Inside a workweave, the marker decides the project — target line MUST
/// NOT print, even if `.rwv-active` at the primary happens to name a
/// different project. The workweave is structurally scoped to one
/// project; the pointer is not consulted from a workweave.
#[test]
fn target_line_silent_inside_workweave() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_projects(tmp.path(), &["p1", "p2"]);
    // Primary pointer names p2 — must not leak into the workweave.
    set_active(&ws, "p2");

    // Create a workweave dir with a .rwv-workweave marker naming p1.
    let weave_dir = tmp.path().join("ws--feat");
    std::fs::create_dir_all(weave_dir.join("github")).unwrap();
    let canon_ws = ws.canonicalize().unwrap();
    let marker_yaml = format!(
        "primary: {}\nproject: p1\nparent: {}\n",
        canon_ws.display(),
        canon_ws.display(),
    );
    std::fs::write(weave_dir.join(".rwv-workweave"), marker_yaml).unwrap();

    let out = common::rwv()
        .args(["status"])
        .current_dir(&weave_dir)
        .assert()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        !stderr.contains("target:"),
        "target line must be silent inside a workweave (marker resolves \
         structurally); stderr: {stderr}"
    );
}
