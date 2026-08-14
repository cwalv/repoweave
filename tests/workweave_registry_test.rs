//! Tests for the `projects/<name>/.rwv-workweave-index` registry.
//!
//! Covers the acceptance criteria added by the workweave-addressing
//! design (§5): create-records / delete-removes / marker round-trip
//! guard, doctor's prune / adopt / tracked-index findings, and
//! per-workweave placement.

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
        r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"
"#,
        repo = common::url_path(&repo_path)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

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
        .current_dir(&ws)
        .assert()
        .success();

    let idx = read_index(&ws, "web-app");
    // Container was recorded from the compiled-in default.
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
    repoweave::symlink::create(&real, &link, repoweave::symlink::LinkTarget::Directory).unwrap();

    rwv()
        .args([
            "workweave",
            "web-app",
            "set-container",
            &link.to_string_lossy(),
        ])
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
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
    repoweave::symlink::create(&real, &link, repoweave::symlink::LinkTarget::Directory).unwrap();

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
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["workweave", "web-app", "delete", "feat"])
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
        .current_dir(&ws)
        .assert()
        .success();

    // Sabotage the marker: rewrite `primary` to a foreign path.
    let ww_dir = weaveroot.join("web-app--feat");
    let marker_path = ww_dir.join(".rwv-workweave");
    let marker = std::fs::read_to_string(&marker_path).unwrap();
    let foreign = "/tmp/does-not-belong-here";
    let mut sabotaged: serde_json::Value = serde_json::from_str(&marker).unwrap();
    sabotaged["primary"] = serde_json::Value::String(foreign.to_string());
    std::fs::write(
        &marker_path,
        serde_json::to_string_pretty(&sabotaged).unwrap(),
    )
    .unwrap();

    // Delete must refuse: the marker no longer round-trips.
    rwv()
        .args(["workweave", "web-app", "delete", "feat"])
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
        .current_dir(&ws)
        .assert()
        .success();

    // Delete the workweave directory out-of-band, leaving the registry
    // pointing at a missing path. This is the "stale entry" scenario.
    let ww_dir = weaveroot.join("web-app--feat");
    std::fs::remove_dir_all(&ww_dir).unwrap();

    // Doctor without --fix must surface the stale entry (as the per-class
    // count line; the recorded path is per-item detail in `--json`).
    rwv()
        .args(["doctor"])
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains("stale-registry-entry"));

    // Doctor with --fix must prune it.
    rwv()
        .args(["doctor", "--fix"])
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
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains(
            "not recorded in `.rwv-workweave-index`",
        ));

    // Doctor with --fix must adopt it.
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains("[fixed] core: adopted workweave"));

    let idx = read_index(&ws, "web-app");
    assert!(
        idx["workweaves"].get("feat").is_some(),
        "adopt must add the entry back to the registry"
    );
}

/// Every `kind` `rwv doctor --json` raises in `ws`, sorted, with the sub-kind
/// key appended for the findings that carry one — the token an operator would
/// key a remedy off.
fn doctor_kinds(ws: &Path) -> Vec<String> {
    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(ws)
        .output()
        .expect("rwv should run");
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("`--json` must emit parseable JSON");
    let mut kinds: Vec<String> = doc["violations"]
        .as_array()
        .expect("`violations` must be present")
        .iter()
        .map(|v| {
            let kind = v["kind"].as_str().expect("a violation carries a kind");
            match v["sub_kind"].as_object().and_then(|o| o.keys().next()) {
                Some(sub) => format!("{kind}/{sub}"),
                None => kind.to_owned(),
            }
        })
        .collect();
    kinds.sort();
    kinds
}

/// A `.rwv-workweave-index` that does not parse is reported as itself, not as
/// the `unregistered-workweave` whose named repair reads the same file.
///
/// Two arms over one fixture, differing only in whether the index parses. The
/// first is the control: the same workweave, the same empty registry, an index
/// that parses — `unregistered-workweave`, and `--fix` performs the adoption
/// it names. Without it a scan that reported nothing at all would satisfy the
/// second arm's absence assertion.
#[test]
fn an_unparseable_index_reports_itself_rather_than_an_adoption_that_cannot_run() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .current_dir(&ws)
        .assert()
        .success();

    let idx_p = index_path(&ws, "web-app");
    let recorded = std::fs::read_to_string(&idx_p).unwrap();

    let mut emptied: serde_json::Value = serde_json::from_str(&recorded).unwrap();
    emptied["workweaves"] = serde_json::json!({});
    std::fs::write(&idx_p, serde_json::to_string_pretty(&emptied).unwrap()).unwrap();

    let control = doctor_kinds(&ws);
    assert!(
        control.contains(&"workweave-tree-integrity/unregistered-workweave".to_string()),
        "an index that parses and records nothing must report the orphan: {control:?}"
    );
    assert!(
        !control.contains(&"unreadable-workweave-index".to_string()),
        "and must not report an unreadable index: {control:?}"
    );
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains("[fixed] core: adopted workweave"));

    std::fs::write(&idx_p, "{ this is not json").unwrap();

    let kinds = doctor_kinds(&ws);
    assert!(
        kinds.contains(&"unreadable-workweave-index".to_string()),
        "an index that does not parse must name the parse failure: {kinds:?}"
    );
    assert!(
        !kinds.contains(&"workweave-tree-integrity/unregistered-workweave".to_string()),
        "and must not also report the orphan, whose repair reads the same file: {kinds:?}"
    );

    rwv().args(["doctor"]).current_dir(&ws).assert().stdout(
        predicate::str::contains("workweave index does not parse")
            .and(predicate::str::contains("not recorded in `.rwv-workweave-index`").not()),
    );

    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains("[fixed] core: adopted workweave").not());
    assert_eq!(
        std::fs::read_to_string(&idx_p).unwrap(),
        "{ this is not json",
        "--fix must not rewrite an index it cannot read"
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
        .current_dir(&ws)
        .assert()
        .success();

    // Force-add the index into the project repo — even though `create`
    // ensured the `.gitignore` line.
    git(&["add", "--force", ".rwv-workweave-index"], &project_dir);
    git(&["commit", "-m", "track index"], &project_dir);

    rwv()
        .args(["doctor"])
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains("tracked by the project repo"));
}

// ---------------------------------------------------------------------------
// RWV_WORKWEAVE_DIR is inert
// ---------------------------------------------------------------------------

#[test]
fn env_var_no_longer_steers_placement_or_output() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    // A container distinct from the compiled-in default. If the env var
    // still had any effect, the workweave would land here instead.
    let decoy = tmp.path().join("decoy-container");
    std::fs::create_dir_all(&decoy).unwrap();

    let assert = rwv()
        .args(["workweave", "web-app", "create", "feat"])
        .env("RWV_WORKWEAVE_DIR", &decoy)
        .current_dir(&ws)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        !stderr.to_lowercase().contains("rwv_workweave_dir"),
        "the retired env var must not appear in output: {stderr}"
    );

    assert!(
        decoy.read_dir().unwrap().next().is_none(),
        "RWV_WORKWEAVE_DIR must not steer placement — the decoy container it named must stay empty"
    );

    let idx = read_index(&ws, "web-app");
    let recorded_path = PathBuf::from(idx["workweaves"]["feat"].as_str().unwrap());
    let default_container = repoweave::workweave_index::default_container(&ws)
        .canonicalize()
        .unwrap();
    assert_eq!(
        recorded_path.parent().unwrap(),
        default_container,
        "workweave must land under the compiled-in default container"
    );
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
        .current_dir(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains("feat"));

    rwv()
        .args(["workweave", "web-app", "delete", "feat"])
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
        .current_dir(&ws)
        .assert()
        .success();

    let expected_ww_dir = alt.join("web-app--feat");
    assert!(expected_ww_dir.exists());
}
