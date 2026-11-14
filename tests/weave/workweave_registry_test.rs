//! Tests for the workweave index a project clone keeps.
//!
//! Covers the acceptance criteria added by the workweave-addressing
//! design (§5): create-records / delete-removes / marker round-trip
//! guard, doctor's prune / adopt / tracked-index findings, and
//! per-workweave placement.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

use crate::common;
use repoweave::refusal::RefusalKind;

fn rwv() -> Command {
    common::rwv()
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "--initial-branch=main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README"), "init").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
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
branch = "main"
role = "owned"
"#,
        repo = common::url_path(&repo_path)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    common::make_project_repo(&project_dir);

    ws
}

fn index_path(ws: &Path, project: &str) -> PathBuf {
    common::index_file_of(ws.join("projects").join(project))
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
        .args(["workweave", "create", "--project", "web-app", "feat"])
        .current_dir(&ws)
        .assert()
        .success();

    let idx = read_index(&ws, "web-app");
    assert!(
        idx.get("basedir").is_none(),
        "a basedir nobody set is the compiled-in default and is not written: {idx}"
    );
    assert_eq!(
        idx["workweaves"]["feat"]["parent"].as_str().unwrap(),
        repoweave::path_spelling::resolve(&ws)
            .unwrap()
            .to_string_lossy(),
        "a workweave created from the primary records the primary as its parent"
    );
    let recorded_path = idx["workweaves"]["feat"]["path"]
        .as_str()
        .unwrap()
        .to_string();
    let ww_dir = weaveroot.join("web-app--feat");
    assert_eq!(
        recorded_path,
        repoweave::path_spelling::resolve(&ww_dir)
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
            "set-default-create-basedir",
            "--project",
            "web-app",
            &link.to_string_lossy(),
        ])
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args(["workweave", "create", "--project", "web-app", "feat"])
        .current_dir(&ws)
        .assert()
        .success();

    let idx = read_index(&ws, "web-app");
    let container = idx["basedir"].as_str().unwrap();
    assert_eq!(
        container,
        repoweave::path_spelling::resolve(&real)
            .unwrap()
            .to_string_lossy()
    );

    let placement = idx["workweaves"]["feat"]["path"].as_str().unwrap();
    assert!(
        repoweave::path_spelling::CanonicalPath::of(std::path::Path::new(container)).contains(
            &repoweave::path_spelling::CanonicalPath::of(std::path::Path::new(placement))
        ),
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
        .args([
            "workweave",
            "set-default-create-basedir",
            "--project",
            "web-app",
        ])
        .arg(&link)
        .current_dir(&ws)
        .assert()
        .success();

    let idx = read_index(&ws, "web-app");
    assert_eq!(
        idx["basedir"].as_str().unwrap(),
        repoweave::path_spelling::resolve(&real)
            .unwrap()
            .to_string_lossy()
    );
}

#[test]
fn delete_removes_registry_entry() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "create", "--project", "web-app", "feat"])
        .current_dir(&ws)
        .assert()
        .success();
    rwv()
        .args(["workweave", "delete", "--project", "web-app", "feat"])
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
        .args(["workweave", "create", "--project", "web-app", "feat"])
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
        .args(["workweave", "delete", "--project", "web-app", "feat"])
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
        .args(["workweave", "list", "--project", "web-app"])
        .current_dir(&ws)
        .assert()
        .success()
        .stdout("");
}

// ---------------------------------------------------------------------------
// doctor: prune stale, adopt orphan
// ---------------------------------------------------------------------------

#[test]
fn doctor_fix_prunes_stale_registry_entry() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "create", "--project", "web-app", "feat"])
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
        .args(["workweave", "create", "--project", "web-app", "feat"])
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
            "not recorded in the workweave index",
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
/// second arm's absence assertion. In the second arm `--fix` rebuilds the
/// index from the workweave's marker, as the adoption recorded it.
#[test]
fn an_unparseable_index_reports_itself_and_doctor_fix_rebuilds_it() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "create", "--project", "web-app", "feat"])
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
    let emptied_then_adopted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&idx_p).unwrap()).unwrap();

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
            .and(predicate::str::contains("not recorded in the workweave index").not()),
    );

    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .assert()
        .stdout(
            predicate::str::contains(
                "[fixed] core: unreadable-workweave-index of project `web-app`: rebuilt it \
                 listing 1 workweave(s) the project clone's checkouts hold: feat",
            )
            .and(predicate::str::contains("[fixed] core: adopted workweave").not()),
        );
    let rebuilt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&idx_p).unwrap()).unwrap();
    assert_eq!(
        rebuilt["workweaves"], emptied_then_adopted["workweaves"],
        "--fix rebuilds the index the workweave on disk records"
    );
}

#[test]
fn the_index_lives_in_the_clone_and_leaves_the_project_checkout_untouched() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let project_dir = ws.join("projects").join("web-app");

    rwv()
        .args(["workweave", "create", "--project", "web-app", "feat"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        project_dir
            .join(".git")
            .join("rwv-workweave-index")
            .is_file(),
        "the index is kept in the clone's shared directory"
    );
    assert!(
        !common::old_path_index_of(&project_dir).exists(),
        "nothing writes the path earlier releases kept it at"
    );
    assert_eq!(
        common::git_in(&project_dir, &["status", "--porcelain", "--ignored"]),
        "",
        "the project checkout holds no index file, ignored or not"
    );
    assert!(
        !project_dir.join(".gitignore").exists(),
        "no `.gitignore` entry is needed"
    );
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
        .args(["workweave", "create", "--project", "web-app", "feat"])
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
    let recorded_path = PathBuf::from(idx["workweaves"]["feat"]["path"].as_str().unwrap());
    let default_container =
        repoweave::path_spelling::resolve(&repoweave::workweave_index::default_basedir(&ws))
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
            "create",
            "--project",
            "web-app",
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
    let recorded_path = idx["workweaves"]["feat"]["path"].as_str().unwrap();
    let expected = repoweave::path_spelling::resolve(&custom)
        .unwrap_or_else(|_| custom.clone())
        .to_string_lossy()
        .into_owned();
    assert_eq!(recorded_path, expected);

    // Even though the workweave lives outside the container, list / delete
    // work via the registry.
    rwv()
        .args(["workweave", "list", "--project", "web-app"])
        .current_dir(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains("feat"));

    rwv()
        .args(["workweave", "delete", "--project", "web-app", "feat"])
        .current_dir(&ws)
        .assert()
        .success();
    assert!(!custom.exists());
}

/// A workweave made with `--dir` in a directory whose name does not spell
/// its name stays a workweave of its project however its index entry is lost:
/// the rebuild of an unreadable index lists it, doctor reports it when the
/// index parses and lacks it and `--fix` adopts it, and a verb run inside it
/// lists it on its own. Each exit is run as its text prints it.
#[test]
fn a_dir_workweave_named_otherwise_than_its_directory_survives_a_lost_entry() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let far = tmp.path().join("elsewhere").join("xyz");
    rwv()
        .args([
            "workweave",
            "create",
            "--project",
            "web-app",
            "far",
            "--dir",
        ])
        .arg(&far)
        .current_dir(&ws)
        .assert()
        .success();
    let idx_p = index_path(&ws, "web-app");
    let lists_far = || {
        let path = read_index(&ws, "web-app")["workweaves"]["far"]["path"]
            .as_str()
            .map(PathBuf::from);
        path.is_some_and(|path| {
            repoweave::path_spelling::resolve(&path).unwrap()
                == repoweave::path_spelling::resolve(&far).unwrap()
        })
    };
    let run_printed = |text: &str, needle: &str| {
        let line = text
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("rwv ") && line.contains(needle))
            .unwrap_or_else(|| panic!("no `{needle}` line printed:\n{text}"));
        let words = shell_words(line);
        let out = rwv().args(&words[1..]).current_dir(&far).output().unwrap();
        assert!(
            out.status.success(),
            "{line}:\n{}",
            common::cli::describe(&out)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };

    std::fs::write(&idx_p, "garbage{").unwrap();
    let out = rwv().args(["status"]).current_dir(&far).output().unwrap();
    let printed = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let fixed = run_printed(&printed, "doctor --fix");
    assert!(fixed.contains("checkouts hold: far"), "{fixed}");
    assert!(lists_far(), "the rebuild must list `far`");

    let mut emptied = read_index(&ws, "web-app");
    emptied["workweaves"] = serde_json::json!({});
    std::fs::write(&idx_p, serde_json::to_string_pretty(&emptied).unwrap()).unwrap();
    assert!(
        doctor_kinds(&ws).contains(&"workweave-tree-integrity/unregistered-workweave".to_string())
    );
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains("adopted workweave `far`"));
    assert!(lists_far(), "doctor --fix must adopt `far`");

    std::fs::write(&idx_p, serde_json::to_string_pretty(&emptied).unwrap()).unwrap();
    let out = rwv().args(["status"]).current_dir(&far).output().unwrap();
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("listed workweave `far`"),
        "{}",
        common::cli::describe(&out)
    );
    assert!(lists_far(), "a verb run inside must list `far`");

    rwv()
        .args(["workweave", "delete", "--project", "web-app", "far"])
        .current_dir(&ws)
        .assert()
        .success();
    assert!(!far.exists());
}

/// A marker written before markers carried their workweave's name is
/// migrated with the name its project's index records: by the first verb run
/// inside the workweave, and by `rwv doctor --fix` in the primary.
#[test]
fn a_marker_without_a_name_takes_the_one_its_index_records() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let far = tmp.path().join("elsewhere").join("xyz");
    rwv()
        .args([
            "workweave",
            "create",
            "--project",
            "web-app",
            "far",
            "--dir",
        ])
        .arg(&far)
        .current_dir(&ws)
        .assert()
        .success();
    let marker = far.join(".rwv-workweave");
    let unnamed = || {
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("name").unwrap();
        std::fs::write(&marker, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    };
    let name = || {
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
        json["name"].as_str().map(str::to_owned)
    };

    unnamed();
    let out = rwv().args(["status"]).current_dir(&far).output().unwrap();
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(name().as_deref(), Some("far"));

    unnamed();
    rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .assert()
        .success();
    assert_eq!(name().as_deref(), Some("far"));
}

/// The weave an earlier release left: markers with no `name`, and the index
/// at the project checkout's `.rwv-workweave-index` as `name: path`, holding
/// the workweaves `listed` names. Returns the directories of `ww` (default
/// container), `far` (`--dir` outside it) and `nest` (made from `ww`).
fn weave_an_earlier_release_left(ws: &Path, tmp: &Path, listed: &[&str]) -> [PathBuf; 3] {
    let far = tmp.join("elsewhere").join("far");
    let create = |name: &str, from: &Path, dir: Option<&Path>| {
        let mut cmd = rwv();
        cmd.args(["workweave", "create", "--project", "web-app", name]);
        if let Some(dir) = dir {
            cmd.arg("--dir").arg(dir);
        }
        cmd.current_dir(from).assert().success();
    };
    create("ww", ws, None);
    let ww = tmp.join(".workweaves").join("web-app--ww");
    create("far", ws, Some(&far));
    create("nest", &ww, None);
    let nest = tmp.join(".workweaves").join("web-app--nest");
    let dirs = [ww, far, nest];
    let mut old = serde_json::Map::new();
    for (name, dir) in ["ww", "far", "nest"].iter().zip(&dirs) {
        let marker = dir.join(".rwv-workweave");
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("name").unwrap();
        std::fs::write(&marker, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        if listed.contains(name) {
            old.insert((*name).to_owned(), serde_json::json!(dir.to_string_lossy()));
        }
    }
    std::fs::remove_file(index_path(ws, "web-app")).unwrap();
    std::fs::write(
        ws.join("projects/web-app/.rwv-workweave-index"),
        serde_json::to_string_pretty(&serde_json::json!({
            "container": tmp.join(".workweaves").to_string_lossy(),
            "workweaves": old,
            "receipts": {},
        }))
        .unwrap(),
    )
    .unwrap();
    dirs
}

fn marker_name(dir: &Path) -> Option<String> {
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join(".rwv-workweave")).unwrap())
            .unwrap();
    json["name"].as_str().map(str::to_owned)
}

fn listed_workweaves(ws: &Path) -> Vec<String> {
    let out = rwv()
        .args(["workweave", "list", "--project", "web-app"])
        .current_dir(ws)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.starts_with("target:"))
        .map(|line| line.trim().to_owned())
        .collect()
}

/// One `rwv doctor --fix` in the primary names every workweave an earlier
/// release left from the index that release kept, `--dir` ones included,
/// before it removes that index.
#[test]
fn one_doctor_fix_names_every_workweave_from_the_index_an_earlier_release_kept() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let dirs = weave_an_earlier_release_left(&ws, tmp.path(), &["ww", "far", "nest"]);

    let out = rwv()
        .args(["status"])
        .current_dir(&dirs[1])
        .output()
        .unwrap();
    common::cli::refused(&out, RefusalKind::LegacyWorkweaveMarker);

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    for (dir, name) in dirs.iter().zip(["ww", "far", "nest"]) {
        assert_eq!(marker_name(dir).as_deref(), Some(name), "{}", dir.display());
        let out = rwv().args(["status"]).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "{}", common::cli::describe(&out));
    }
    assert_eq!(listed_workweaves(&ws), ["far", "nest", "ww"]);
    assert!(!ws.join("projects/web-app/.rwv-workweave-index").exists());
}

/// A workweave an earlier release left that no index records and whose
/// directory's name renders none is reported with the verb that names it,
/// which runs as printed.
#[test]
fn a_workweave_no_record_names_is_reported_with_the_verb_that_names_it() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let dirs = weave_an_earlier_release_left(&ws, tmp.path(), &["ww", "nest"]);
    let far = &dirs[1];

    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", common::cli::describe(&out));
    assert_eq!(marker_name(far), None);
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("rwv ") && line.contains("workweave create"))
        .unwrap_or_else(|| panic!("no naming verb printed:\n{text}"));
    let words = shell_words(line);
    let out = rwv().args(&words[1..]).current_dir(far).output().unwrap();
    assert!(
        out.status.success(),
        "{line}:\n{}",
        common::cli::describe(&out)
    );

    assert_eq!(marker_name(far).as_deref(), Some("far"));
    assert_eq!(listed_workweaves(&ws), ["far", "nest", "ww"]);
    let out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    let out = rwv().args(["status"]).current_dir(far).output().unwrap();
    assert!(out.status.success(), "{}", common::cli::describe(&out));
}

/// A workweave directory renamed by hand: `rwv doctor --all --fix` records
/// its checkouts where they stand instead of pruning them, and lists it under
/// its marker's name. What a checkout had marked for its next commit stays.
#[test]
fn a_workweave_moved_by_hand_is_recorded_where_it_stands_under_its_marker_name() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    rwv()
        .args(["workweave", "create", "--project", "web-app", "ww"])
        .current_dir(&ws)
        .assert()
        .success();
    let was = tmp.path().join(".workweaves").join("web-app--ww");
    let now = tmp.path().join(".workweaves").join("web-app--zz");
    let checkout = now.join("projects/web-app");
    std::fs::write(was.join("projects/web-app/staged.txt"), "staged\n").unwrap();
    common::git_in(was.join("projects/web-app"), &["add", "staged.txt"]);
    std::fs::rename(&was, &now).unwrap();

    let out = rwv()
        .args(["doctor", "--all", "--fix", "--project", "web-app"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert!(
        !String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.contains("stale-worktree-registration") && line.ends_with("pruned")),
        "{}",
        common::cli::describe(&out)
    );

    let listed = read_index(&ws, "web-app")["workweaves"]["ww"]["path"]
        .as_str()
        .map(PathBuf::from)
        .expect("the workweave stays listed as `ww`");
    assert_eq!(
        repoweave::path_spelling::resolve(&listed).unwrap(),
        repoweave::path_spelling::resolve(&now).unwrap()
    );
    assert!(read_index(&ws, "web-app")["workweaves"].get("zz").is_none());
    assert_eq!(
        common::git_in(&checkout, &["status", "--porcelain"]).trim(),
        "A  staged.txt"
    );
    for repo in [checkout.clone(), now.join("github/org/repo")] {
        let listed = common::git_in(&repo, &["worktree", "list", "--porcelain"]);
        assert!(!listed.contains("prunable"), "{listed}");
    }
    let out = rwv().args(["status"]).current_dir(&now).output().unwrap();
    assert!(out.status.success(), "{}", common::cli::describe(&out));
}

fn shell_words(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(|word| word.trim_matches('`').to_owned())
        .collect()
}

// ---------------------------------------------------------------------------
// per-workweave basedir (--basedir)
// ---------------------------------------------------------------------------

#[test]
fn basedir_places_one_workweave_under_it_and_records_the_path() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let alt = tmp.path().join("alt-basedir");
    std::fs::create_dir_all(&alt).unwrap();

    rwv()
        .args([
            "workweave",
            "create",
            "--project",
            "web-app",
            "feat",
            "--basedir",
            &alt.to_string_lossy(),
        ])
        .current_dir(&ws)
        .assert()
        .success();

    let placed = alt.join("web-app--feat");
    assert!(
        placed.join(".rwv-workweave").exists(),
        "workweave must land at <basedir>/<project>--<name>"
    );
    let idx = read_index(&ws, "web-app");
    let expected = repoweave::path_spelling::resolve(&placed)
        .unwrap_or_else(|_| placed.clone())
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        idx["workweaves"]["feat"]["path"].as_str().unwrap(),
        expected
    );

    // The override is for one run: the recorded default is untouched.
    rwv()
        .args(["workweave", "create", "--project", "web-app", "next"])
        .current_dir(&ws)
        .assert()
        .success();
    assert!(
        !alt.join("web-app--next").exists(),
        "a later create without --basedir must not reuse the override"
    );
}

/// `--dir` names the workweave's own path and `--basedir` the directory above
/// it; asking for both is refused and nothing is placed.
#[test]
fn basedir_together_with_dir_is_refused() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let dir = tmp.path().join("exact");
    let basedir = tmp.path().join("above");
    std::fs::create_dir_all(&basedir).unwrap();

    let out = rwv()
        .args([
            "workweave",
            "create",
            "--project",
            "web-app",
            "feat",
            "--dir",
            &dir.to_string_lossy(),
            "--basedir",
            &basedir.to_string_lossy(),
        ])
        .current_dir(&ws)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "stderr:\n{stderr}");
    assert!(
        stderr.contains("--dir")
            && stderr.contains("--basedir")
            && stderr.contains("cannot be used with"),
        "the refusal must name both flags:\n{stderr}"
    );
    assert!(!dir.exists(), "nothing may be placed at --dir");
    assert!(
        basedir.read_dir().unwrap().next().is_none(),
        "nothing may be placed under --basedir"
    );
}

// ---------------------------------------------------------------------------
// set-default-create-basedir verb
// ---------------------------------------------------------------------------

#[test]
fn set_default_create_basedir_verb_records_the_basedir() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let alt = tmp.path().join("alt-container");
    std::fs::create_dir_all(&alt).unwrap();

    rwv()
        .args([
            "workweave",
            "set-default-create-basedir",
            "--project",
            "web-app",
            &alt.to_string_lossy(),
        ])
        .current_dir(&ws)
        .assert()
        .success();

    let idx = read_index(&ws, "web-app");
    let recorded = idx["basedir"].as_str().unwrap();
    let expected = repoweave::path_spelling::resolve(&alt)
        .unwrap_or_else(|_| alt.clone())
        .to_string_lossy()
        .into_owned();
    assert_eq!(recorded, expected);

    // Subsequent workweave create lands in the recorded container.
    rwv()
        .args(["workweave", "create", "--project", "web-app", "feat"])
        .current_dir(&ws)
        .assert()
        .success();

    let expected_ww_dir = alt.join("web-app--feat");
    assert!(expected_ww_dir.exists());
}

/// A recorded name that is not a workweave name is a corrupt index, refused
/// where the index is read: doctor names the file and the name rather than
/// treating the entry as recorded while every reader that needs the name
/// passes over it.
#[test]
fn an_index_recording_an_illegal_workweave_name_is_refused_at_load() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    std::fs::create_dir_all(tmp.path().join(".workweaves")).unwrap();
    rwv()
        .args(["workweave", "create", "--project", "web-app", "feat"])
        .current_dir(&ws)
        .assert()
        .success();

    let control = doctor_kinds(&ws);
    assert!(
        !control.contains(&"unreadable-workweave-index".to_string()),
        "the index create wrote reads: {control:?}"
    );

    let idx_p = index_path(&ws, "web-app");
    let mut index: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&idx_p).unwrap()).unwrap();
    let entry = index["workweaves"]["feat"].clone();
    index["workweaves"] = serde_json::json!({ "fe--at": entry });
    std::fs::write(&idx_p, serde_json::to_string_pretty(&index).unwrap()).unwrap();

    let kinds = doctor_kinds(&ws);
    assert!(
        kinds.contains(&"unreadable-workweave-index".to_string()),
        "an index recording an illegal name must not read as an index: {kinds:?}"
    );
    rwv()
        .args(["doctor"])
        .current_dir(&ws)
        .assert()
        .stdout(predicate::str::contains(
            "'fe--at' is not a valid workweave name",
        ));
}
