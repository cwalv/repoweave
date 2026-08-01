//! Resolution refuses a weave root carrying both identity files, and exactly
//! two verbs are exempt.
//!
//! `.rwv-active` and `.rwv-workweave` are mutually exclusive. A root holding
//! both is an illegal state no writer produces, and every verb that acts
//! *through* a root refuses it — the alternative, tolerating it with a
//! warning, is the soft edge that lets it persist.
//!
//! `doctor` (repair) and `status` (inspection) are exempt: their subject is
//! the root's identity, so refusing them would withhold the command that
//! names the state and the one that clears it. Both proceed by marker, which
//! is what resolution reads at an undisputed workweave root anyway.
//!
//! The exemption is per-verb wiring, so the unit tests over the two entry
//! points cannot pin it — a verb gets the exemption by naming the exempt
//! entry point at its dispatch site, and nothing but running the verb shows
//! which one it named. Both exempt verbs are pinned here, and a non-exempt
//! one alongside them: a green exemption test proves nothing if refusal
//! stopped happening at all.

use std::path::{Path, PathBuf};

mod common;

const REFUSAL: &str = "both exist: a weave root carries the workweave marker";

/// A primary weave with one project, and a workweave of it whose root has
/// acquired a stray `.rwv-active` naming a *different* project — so a verb
/// that read the pointer instead of the marker would be visibly wrong.
fn make_disputed_workweave(tmp: &Path) -> (PathBuf, PathBuf) {
    let primary = tmp.join("ws");
    std::fs::create_dir_all(primary.join("github")).unwrap();
    let project = primary.join("projects").join("web-app");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("rwv.yaml"), "repositories:\n").unwrap();
    std::fs::write(primary.join(".rwv-active"), "web-app\n").unwrap();

    let ww = tmp.join(".workweaves").join("web-app--feat");
    std::fs::create_dir_all(ww.join("github")).unwrap();
    let ww_project = ww.join("projects").join("web-app");
    std::fs::create_dir_all(&ww_project).unwrap();
    std::fs::write(ww_project.join("rwv.yaml"), "repositories:\n").unwrap();
    std::fs::write(
        ww.join(".rwv-workweave"),
        format!(
            "{{\"primary\":\"{p}\",\"project\":\"web-app\",\"parent\":\"{p}\"}}",
            p = primary.display()
        ),
    )
    .unwrap();
    std::fs::write(ww.join(".rwv-active"), "other-project\n").unwrap();

    (primary, ww)
}

#[test]
fn a_non_exempt_verb_refuses_a_root_carrying_both_identity_files() {
    let tmp = common::tempdir().unwrap();
    let (_, ww) = make_disputed_workweave(tmp.path());

    let out = common::rwv()
        .arg("resolve")
        .current_dir(&ww)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "resolve must refuse; stderr: {stderr}"
    );
    assert!(stderr.contains(REFUSAL), "unexpected error: {stderr}");
    assert!(
        stderr.contains("rwv doctor --fix"),
        "the refusal must name the repair: {stderr}"
    );
}

#[test]
fn status_proceeds_by_marker_from_a_root_carrying_both_identity_files() {
    let tmp = common::tempdir().unwrap();
    let (_, ww) = make_disputed_workweave(tmp.path());

    let out = common::rwv()
        .args(["status", "--json"])
        .current_dir(&ww)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "status must proceed; stderr: {stderr}"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("status --json must emit JSON ({e}): {stdout}"));
    assert_eq!(
        parsed["resolution"]["project"], "web-app",
        "the marker decides, not the pointer's `other-project`: {stdout}"
    );
}

#[test]
fn doctor_proceeds_by_marker_from_a_root_carrying_both_identity_files() {
    let tmp = common::tempdir().unwrap();
    let (_, ww) = make_disputed_workweave(tmp.path());

    let out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(&ww)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains(REFUSAL),
        "doctor must not refuse the state it repairs: {stderr}"
    );

    // Non-vacuity: absence of the refusal is only evidence if doctor got far
    // enough to produce its report.
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<serde_json::Value>(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must emit JSON ({e}): {stdout}{stderr}"));
}
