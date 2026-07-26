//! Tests for workweave-tree integrity checks (`rwv doctor`).
//!
//! Exercises the four sub-kinds of `WorkweaveTreeIntegrity` violations:
//!
//!   1. `dangling-parent`   — marker `parent:` path no longer exists
//!   2. `parent-chain-anomaly` — cycle, parent==self, parent belongs to a
//!      different project
//!   3. `unregistered-dir` — directory under `.workweaves/` with no marker
//!   4. `foreign-primary`  — marker `primary:` does not match the workspace;
//!      splits into `foreign-primary` (the recorded path resolves to no
//!      workspace) and `foreign-primary-other-workspace` (it resolves to a
//!      different, real one — expected when several weaves share one
//!      container, so filtered from the default text report)
//!
//! Each sub-kind has:
//!   - a synthetic-violation fixture test (reports the violation)
//!   - where relevant, a clean-workspace test that must stay clean
//!
//! Healthy nested parent/child trees must stay clean.

use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Workspace construction helpers
// ---------------------------------------------------------------------------

/// Create a minimal primary workspace with a `github/` registry dir and a
/// `projects/` directory. Returns the workspace root.
fn make_primary(parent: &Path) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

/// Return the `.workweaves/` parent directory for `ws_root`.
fn workweaves_dir(ws_root: &Path) -> PathBuf {
    ws_root
        .parent()
        .expect("ws_root has a parent")
        .join(".workweaves")
}

/// Write a well-formed `.rwv-workweave` marker file into `ww_dir`.
fn write_marker(ww_dir: &Path, primary: &Path, project: &str, parent: &Path) {
    std::fs::create_dir_all(ww_dir).unwrap();
    let primary_str = primary
        .canonicalize()
        .unwrap_or_else(|_| primary.to_path_buf());
    let parent_str = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    let content = format!(
        "primary: {}\nproject: {}\nparent: {}\n",
        primary_str.display(),
        project,
        parent_str.display()
    );
    std::fs::write(ww_dir.join(".rwv-workweave"), content).unwrap();
}

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

// ===========================================================================
// 1. Dangling-parent: marker `parent:` path no longer exists
// ===========================================================================

/// A workweave whose `parent` directory has been deleted → dangling-parent.
#[test]
fn dangling_parent_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let ww_dir = workweaves_dir(&ws).join("my-project--feat");
    std::fs::create_dir_all(&ww_dir).unwrap();

    // Write a marker whose `parent` points at a path that does not exist.
    let missing_parent = tmp.path().join("nonexistent-parent");
    write_marker(&ww_dir, &ws, "my-project", &missing_parent);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("dangling-parent") || stdout.contains("does not exist"),
        "doctor should report dangling-parent; got:\n{stdout}"
    );
    // Should mention the missing path.
    assert!(
        stdout.contains("nonexistent-parent"),
        "report should name the missing parent path; got:\n{stdout}"
    );
}

/// A workweave with a healthy parent (`parent:` == primary) → no dangling-parent.
#[test]
fn healthy_parent_is_clean() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let ww_dir = workweaves_dir(&ws).join("my-project--main");
    write_marker(&ww_dir, &ws, "my-project", &ws);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("dangling-parent"),
        "healthy workweave should not produce dangling-parent; got:\n{stdout}"
    );
}

// ===========================================================================
// 2. Parent-chain anomalies
// ===========================================================================

// ---------------------------------------------------------------------------
// 2a. Parent == self (self-loop)
// ---------------------------------------------------------------------------

#[test]
fn self_loop_parent_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let ww_dir = workweaves_dir(&ws).join("my-project--self-loop");
    std::fs::create_dir_all(&ww_dir).unwrap();

    // Write a marker where `parent` points to the workweave directory itself.
    write_marker(&ww_dir, &ws, "my-project", &ww_dir);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("parent-chain-anomaly")
            || stdout.contains("self-loop")
            || stdout.contains("itself"),
        "doctor should report self-loop anomaly; got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// 2b. Cycle (A → B → A)
// ---------------------------------------------------------------------------

#[test]
fn cycle_in_parent_chain_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let ww_a = workweaves_dir(&ws).join("my-project--cycle-a");
    let ww_b = workweaves_dir(&ws).join("my-project--cycle-b");
    std::fs::create_dir_all(&ww_a).unwrap();
    std::fs::create_dir_all(&ww_b).unwrap();

    // A's parent is B, B's parent is A → cycle.
    write_marker(&ww_a, &ws, "my-project", &ww_b);
    write_marker(&ww_b, &ws, "my-project", &ww_a);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("parent-chain-anomaly") || stdout.contains("cycle"),
        "doctor should report cycle anomaly; got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// 2c. Cross-project parent (child is project-A, parent is project-B)
// ---------------------------------------------------------------------------

#[test]
fn cross_project_parent_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let parent_ww = workweaves_dir(&ws).join("project-b--base");
    let child_ww = workweaves_dir(&ws).join("project-a--child");
    std::fs::create_dir_all(&parent_ww).unwrap();
    std::fs::create_dir_all(&child_ww).unwrap();

    // parent_ww belongs to project-b; child_ww belongs to project-a but
    // its marker's `parent` points at parent_ww.
    write_marker(&parent_ww, &ws, "project-b", &ws);
    write_marker(&child_ww, &ws, "project-a", &parent_ww);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("parent-chain-anomaly") || stdout.contains("project"),
        "doctor should report cross-project parent anomaly; got:\n{stdout}"
    );
    // Should mention both project names.
    assert!(
        stdout.contains("project-a") || stdout.contains("project-b"),
        "report should name the involved project(s); got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// 2d. Healthy nested parent/child tree must stay clean
// ---------------------------------------------------------------------------

/// Primary → workweave A → workweave A-child: all same project, no anomalies.
#[test]
fn healthy_nested_tree_is_clean() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    let ww_a = workweaves_dir(&ws).join("my-project--feat-a");
    let ww_a_child = workweaves_dir(&ws).join("my-project--feat-a-child");
    std::fs::create_dir_all(&ww_a).unwrap();
    std::fs::create_dir_all(&ww_a_child).unwrap();

    // A's parent is the primary.
    write_marker(&ww_a, &ws, "my-project", &ws);
    // A-child's parent is A.
    write_marker(&ww_a_child, &ws, "my-project", &ww_a);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("parent-chain-anomaly")
            && !stdout.contains("dangling-parent")
            && !stdout.contains("cycle"),
        "healthy nested tree should be clean; got:\n{stdout}"
    );
}

// ===========================================================================
// 3. Unregistered directory (no marker)
// ===========================================================================

#[test]
fn unregistered_dir_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    // Create a directory under .workweaves/ with no .rwv-workweave marker.
    let bare_dir = workweaves_dir(&ws).join("my-project--no-marker");
    std::fs::create_dir_all(&bare_dir).unwrap();

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("unregistered-dir") || stdout.contains("no `.rwv-workweave` marker"),
        "doctor should report unregistered directory; got:\n{stdout}"
    );
}

/// A registered workweave (with a valid marker) must not be flagged as unregistered.
#[test]
fn registered_workweave_is_not_flagged_as_unregistered() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let ww_dir = workweaves_dir(&ws).join("my-project--registered");
    write_marker(&ww_dir, &ws, "my-project", &ws);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("unregistered-dir"),
        "registered workweave should not be flagged as unregistered; got:\n{stdout}"
    );
}

// ===========================================================================
// 4. Foreign-primary marker
// ===========================================================================

#[test]
fn foreign_primary_is_reported() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let ww_dir = workweaves_dir(&ws).join("my-project--foreign");
    std::fs::create_dir_all(&ww_dir).unwrap();

    // Write a marker whose `primary` points at a completely different path
    // (simulating an rsync'd workweave from another machine).
    let foreign_primary = PathBuf::from("/some/other/machine/workspace");
    let content = format!(
        "primary: {}\nproject: my-project\nparent: {}\n",
        foreign_primary.display(),
        foreign_primary.display()
    );
    std::fs::write(ww_dir.join(".rwv-workweave"), content).unwrap();

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("foreign-primary") || stdout.contains("does not match"),
        "doctor should report foreign-primary; got:\n{stdout}"
    );
    // Should mention the foreign path.
    assert!(
        stdout.contains("other/machine") || stdout.contains("foreign"),
        "report should reference the foreign primary path; got:\n{stdout}"
    );
    // A primary that resolves to no workspace at all is the one case the
    // copied-from-another-machine hypothesis actually fits; pin the exact
    // phrase so it can't quietly erode once a second, unrelated cause
    // (foreign-primary-other-workspace, below) also lives in this match.
    assert!(
        stdout.contains("copied from another machine"),
        "an unresolvable primary must still name the likely cause; got:\n{stdout}"
    );
}

/// A workweave with its `primary:` correctly set to the current workspace
/// must not be flagged as foreign.
#[test]
fn local_primary_is_not_flagged_as_foreign() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let ww_dir = workweaves_dir(&ws).join("my-project--local");
    write_marker(&ww_dir, &ws, "my-project", &ws);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("foreign-primary"),
        "workweave with correct primary should not be flagged as foreign; got:\n{stdout}"
    );
}

// ===========================================================================
// 4b. Foreign-primary pointing at a different, but real, workspace
//
// The shape at issue: several weaves share one `.workweaves` container, so
// each one's marker names its own primary — which is "foreign" to every
// *other* weave scanning that same container. That is not a copied-marker
// defect, and the fix must not say it is.
// ===========================================================================

/// A marker whose `primary` resolves to a different, *real* workspace root
/// must not claim to have been copied from another machine — and, per the
/// filtered-by-default design here, must not appear in the default text
/// report at all (it is not this workspace's problem).
#[test]
fn foreign_primary_pointing_at_a_real_workspace_is_not_reported_as_copied() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let other_ws = make_primary(&tmp.path().join("elsewhere"));
    let ww_dir = workweaves_dir(&ws).join("my-project--sibling");
    write_marker(&ww_dir, &other_ws, "my-project", &other_ws);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("copied from another machine"),
        "a marker pointing at a different but real workspace must not claim \
         to have been copied; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("my-project--sibling"),
        "foreign-primary-other-workspace is filtered from the default text \
         report — it belongs to a sibling weave sharing this container, not \
         a finding this workspace's operator needs to act on; got:\n{stdout}"
    );
}

/// The suppressed-from-text finding above is not simply dropped: `--json`
/// still carries it, correctly discriminated from the unresolvable case.
#[test]
fn foreign_primary_other_workspace_is_visible_via_json() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let other_ws = make_primary(&tmp.path().join("elsewhere"));
    let ww_dir = workweaves_dir(&ws).join("my-project--sibling");
    write_marker(&ww_dir, &other_ws, "my-project", &other_ws);

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));

    let violations = json["violations"].as_array().expect("violations is array");
    let finding = violations
        .iter()
        .find(|v| {
            v["kind"] == "workweave-tree-integrity"
                && v["sub_kind"]
                    .get("foreign-primary-other-workspace")
                    .is_some()
        })
        .unwrap_or_else(|| {
            panic!("expected a foreign-primary-other-workspace finding; got: {violations:?}")
        });

    let other_ws_canonical = other_ws.canonicalize().unwrap();
    assert_eq!(
        finding["sub_kind"]["foreign-primary-other-workspace"]["marker_primary"],
        serde_json::Value::String(other_ws_canonical.to_string_lossy().into_owned()),
        "must name the resolved path of the other workspace"
    );
}

// ===========================================================================
// 5. JSON output (`--json`) includes workweave-tree-integrity kind
// ===========================================================================

#[test]
fn json_output_includes_workweave_tree_integrity_kind() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());

    // Create an unregistered directory (simplest violation to synthesize).
    let bare_dir = workweaves_dir(&ws).join("my-project--no-marker");
    std::fs::create_dir_all(&bare_dir).unwrap();

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));

    let violations = json["violations"].as_array().expect("violations is array");
    let found = violations
        .iter()
        .any(|v| v["kind"] == "workweave-tree-integrity");
    assert!(
        found,
        "doctor --json must include a workweave-tree-integrity violation; violations: {violations:?}"
    );
}

// ===========================================================================
// 6. Empty workspace (no .workweaves/ directory at all) stays clean
// ===========================================================================

#[test]
fn workspace_with_no_workweaves_dir_is_clean_of_tree_checks() {
    let tmp = common::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    // Do NOT create .workweaves/ — it simply doesn't exist.

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    // None of the tree-integrity sub-kinds should fire.
    assert!(
        !stdout.contains("dangling-parent")
            && !stdout.contains("parent-chain-anomaly")
            && !stdout.contains("unregistered-dir")
            && !stdout.contains("foreign-primary"),
        "workspace with no .workweaves/ should be clean of tree checks; got:\n{stdout}"
    );
}
