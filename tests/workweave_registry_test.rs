//! Tests for the `projects/<name>/.rwv-workweave-index` registry.
//!
//! Covers the acceptance criteria added by the workweave-addressing
//! design (§5): create-records / delete-removes / marker round-trip
//! guard, doctor's prune / adopt / tracked-index findings, deprecation
//! warning for `RWV_WORKWEAVE_DIR`, and per-workweave placement.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Build a minimal workspace with a single project + one repo.
fn make_workspace(tmp: &Path, project: &str) -> PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: owned
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    ws
}

fn index_path(ws: &Path, project: &str) -> PathBuf {
    ws.join("projects")
        .join(project)
        .join(".rwv-workweave-index")
}

fn read_index(ws: &Path, project: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(index_path(ws, project)).expect("index must exist");
    serde_json::from_str(&text).expect("index must parse")
}

// ---------------------------------------------------------------------------
// create-records / delete-removes
// ---------------------------------------------------------------------------

#[test]
fn create_records_workweave_in_the_index() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let idx = read_index(&ws, "web-app");
    // Container was recorded from the env var fallback.
    assert_eq!(
        idx["container"].as_str().unwrap(),
        weaveroot
            .canonicalize()
            .unwrap_or_else(|_| weaveroot.clone())
            .to_string_lossy()
    );
    let recorded_path = idx["workweaves"]["feat"].as_str().unwrap().to_string();
    let ww_dir = weaveroot.join("web-app--feat");
    assert_eq!(
        recorded_path,
        ww_dir
            .canonicalize()
            .unwrap_or_else(|_| ww_dir.clone())
            .to_string_lossy()
    );
}

// The container is recorded in the same canonical form as the placements under
// it. macOS reproduces this for free — `tempfile` hands out a path under `/var`,
// which is a symlink to `/private/var` — so an explicit symlink is what makes
// the same assertion bite on Linux instead of only in CI.

#[test]
fn a_symlinked_container_is_recorded_canonicalized() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let real = tmp.path().join("real-container");
    std::fs::create_dir_all(&real).unwrap();
    let link = tmp.path().join("link-container");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &link)
        .current_dir(&ws)
        .assert()
        .success();

    let idx = read_index(&ws, "web-app");
    let container = idx["container"].as_str().unwrap();
    assert_eq!(container, real.canonicalize().unwrap().to_string_lossy());

    let placement = idx["workweaves"]["feat"].as_str().unwrap();
    assert!(
        placement.starts_with(&format!("{container}/")),
        "placement {placement} must sit under the recorded container {container}"
    );
}

#[test]
fn set_container_records_a_symlinked_path_canonicalized() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let real = tmp.path().join("real-container");
    std::fs::create_dir_all(&real).unwrap();
    let link = tmp.path().join("link-container");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    rwv()
        .args(["workweave", "web-app", "set-container"])
        .arg(&link)
        .current_dir(&ws)
        .assert()
        .success();

    let idx = read_index(&ws, "web-app");
    assert_eq!(
        idx["container"].as_str().unwrap(),
        real.canonicalize().unwrap().to_string_lossy()
    );
}

#[test]
fn delete_removes_registry_entry() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["workweave", "web-app", "delete", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let idx = read_index(&ws, "web-app");
    assert!(
        idx["workweaves"].get("feat").is_none(),
        "delete must remove the registry entry"
    );
}

#[test]
fn delete_refuses_when_marker_round_trip_fails() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Sabotage the marker: rewrite `primary:` to a foreign path.
    let ww_dir = weaveroot.join("web-app--feat");
    let marker_path = ww_dir.join(".rwv-workweave");
    let marker = std::fs::read_to_string(&marker_path).unwrap();
    let foreign = "/tmp/does-not-belong-here";
    let sabotaged = marker
        .lines()
        .map(|line| {
            if line.starts_with("primary:") {
                format!("primary: {foreign}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&marker_path, sabotaged).unwrap();

    // Delete must refuse: the marker no longer round-trips.
    rwv()
        .args(["workweave", "web-app", "delete", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("primary").or(predicate::str::contains("foreign")));

    // The directory must survive an aborted delete.
    assert!(ww_dir.exists());
}

// ---------------------------------------------------------------------------
// list uses the registry (missing index → empty)
// ---------------------------------------------------------------------------

#[test]
fn list_returns_empty_when_no_registry() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // No workweave create ran; no `.rwv-workweave-index` should exist.
    assert!(!index_path(&ws, "web-app").exists());

    rwv()
        .args(["workweave", "web-app", "list"])
        .current_dir(&ws)
        .assert()
        .success()
        .stdout("");
}

// ---------------------------------------------------------------------------
// doctor: prune stale, adopt orphan, flag tracked index
// ---------------------------------------------------------------------------

#[test]
fn doctor_fix_prunes_stale_registry_entry() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Delete the workweave directory out-of-band, leaving the registry
    // pointing at a missing path. This is the "stale entry" scenario.
    let ww_dir = weaveroot.join("web-app--feat");
    std::fs::remove_dir_all(&ww_dir).unwrap();

    // Doctor without --fix must surface the stale entry.
    rwv()
        .args(["doctor"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains("stale entry"));

    // Doctor with --fix must prune it.
    rwv()
        .args(["doctor", "--fix"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains(
            "[fixed] core: pruned stale registry entry",
        ));

    let idx = read_index(&ws, "web-app");
    assert!(idx["workweaves"].get("feat").is_none());
}

#[test]
fn doctor_fix_adopts_unregistered_workweave() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Delete the registry entry (simulating a workspace whose index was
    // wiped or predates the registry). The workweave dir stays.
    let idx_p = index_path(&ws, "web-app");
    let mut idx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&idx_p).unwrap()).unwrap();
    idx["workweaves"] = serde_json::json!({});
    std::fs::write(&idx_p, serde_json::to_string_pretty(&idx).unwrap()).unwrap();

    // Doctor without --fix must surface the unregistered workweave.
    rwv()
        .args(["doctor"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains(
            "not recorded in `.rwv-workweave-index`",
        ));

    // Doctor with --fix must adopt it.
    rwv()
        .args(["doctor", "--fix"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains("[fixed] core: adopted workweave"));

    let idx = read_index(&ws, "web-app");
    assert!(
        idx["workweaves"].get("feat").is_some(),
        "adopt must add the entry back to the registry"
    );
}

#[test]
fn doctor_flags_tracked_index() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // Make the project a git repo so we can commit the index into it.
    let project_dir = ws.join("projects").join("web-app");
    init_repo_with_commit(&project_dir);

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    // Force-add the index into the project repo — even though `create`
    // ensured the `.gitignore` line.
    git(&["add", "--force", ".rwv-workweave-index"], &project_dir);
    git(&["commit", "-m", "track index"], &project_dir);

    rwv()
        .args(["doctor"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains("tracked by the project repo"));
}

// ---------------------------------------------------------------------------
// deprecation warning when RWV_WORKWEAVE_DIR is set
// ---------------------------------------------------------------------------

#[test]
fn deprecation_warning_fires_when_env_var_set() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success()
        .stderr(predicate::str::contains("RWV_WORKWEAVE_DIR is deprecated"));
}

#[test]
fn no_deprecation_warning_when_env_var_unset() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env_remove("RWV_WORKWEAVE_DIR")
        .current_dir(&ws)
        .assert()
        .success()
        .stderr(predicate::str::contains("RWV_WORKWEAVE_DIR is deprecated").not());
}

// ---------------------------------------------------------------------------
// per-workweave placement (--dir override)
// ---------------------------------------------------------------------------

#[test]
fn per_workweave_dir_override_lands_and_records_absolute_path() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let alt = tmp.path().join("alt-container");
    std::fs::create_dir_all(&alt).unwrap();
    let custom = alt.join("custom-name");

    rwv()
        .args([
            "workweave",
            "web-app",
            "create",
            "feat",
            "--dir",
            &custom.to_string_lossy(),
        ])
        .env_remove("RWV_WORKWEAVE_DIR")
        .current_dir(&ws)
        .assert()
        .success();

    assert!(custom.exists(), "workweave must land at --dir path");
    assert!(custom.join(".rwv-workweave").exists());

    let idx = read_index(&ws, "web-app");
    let recorded_path = idx["workweaves"]["feat"].as_str().unwrap();
    let expected = custom
        .canonicalize()
        .unwrap_or_else(|_| custom.clone())
        .to_string_lossy()
        .into_owned();
    assert_eq!(recorded_path, expected);

    // Even though the workweave lives outside the container, list / delete
    // work via the registry.
    rwv()
        .args(["workweave", "web-app", "list"])
        .env_remove("RWV_WORKWEAVE_DIR")
        .current_dir(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains("feat"));

    rwv()
        .args(["workweave", "web-app", "delete", "feat"])
        .env_remove("RWV_WORKWEAVE_DIR")
        .current_dir(&ws)
        .assert()
        .success();
    assert!(!custom.exists());
}

// ---------------------------------------------------------------------------
// set-container verb
// ---------------------------------------------------------------------------

#[test]
fn set_container_verb_records_the_container() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let alt = tmp.path().join("alt-container");
    std::fs::create_dir_all(&alt).unwrap();

    rwv()
        .args([
            "workweave",
            "web-app",
            "set-container",
            &alt.to_string_lossy(),
        ])
        .env_remove("RWV_WORKWEAVE_DIR")
        .current_dir(&ws)
        .assert()
        .success();

    let idx = read_index(&ws, "web-app");
    let recorded = idx["container"].as_str().unwrap();
    let expected = alt
        .canonicalize()
        .unwrap_or_else(|_| alt.clone())
        .to_string_lossy()
        .into_owned();
    assert_eq!(recorded, expected);

    // Subsequent workweave create lands in the recorded container.
    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env_remove("RWV_WORKWEAVE_DIR")
        .current_dir(&ws)
        .assert()
        .success();

    let expected_ww_dir = alt.join("web-app--feat");
    assert!(expected_ww_dir.exists());
}
