//! A relative path argument names a location from the directory rwv runs in,
//! or from the `-C`/`-w` directory, never from the weave root. Each fixture
//! invokes from somewhere other than the weave root, so a weave-root reading
//! lands somewhere else and the assertion fails.

use std::path::{Path, PathBuf};

mod common;

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "--initial-branch=main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README"), "init").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
}

/// A primary weave at `<tmp>/deep/ws`, so a path climbing out of it one level
/// more than the invocation dir needs still lands inside the temp dir.
fn weave_with_member(tmp: &Path) -> PathBuf {
    let ws = tmp.join("deep").join("ws");
    let repo = ws.join("github/org/repo");
    init_repo_with_commit(&repo);
    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"file://{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::url_path(&repo)
        ),
    )
    .unwrap();
    ws
}

fn index(ws: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(ws.join("projects/web-app/.rwv-workweave-index"))
        .expect("index must exist");
    serde_json::from_str(&raw).expect("index must parse")
}

fn canonical(p: &Path) -> String {
    p.canonicalize().unwrap().to_string_lossy().into_owned()
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, vec![token], "stderr was:\n{stderr}");
}

#[test]
fn set_container_reads_a_relative_path_from_the_invocation_subdirectory() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_member(tmp.path());
    let wanted = tmp.path().join("deep/c");
    std::fs::create_dir_all(&wanted).unwrap();

    common::rwv()
        .args(["workweave", "web-app", "set-container", "../../c"])
        .current_dir(ws.join("projects"))
        .assert()
        .success();

    assert_eq!(
        index(&ws)["container"].as_str().unwrap(),
        canonical(&wanted)
    );
}

#[test]
fn create_dir_reads_a_relative_path_from_the_c_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_member(tmp.path());
    let wanted = tmp.path().join("deep/alt/feat");

    common::rwv()
        .arg("-C")
        .arg(ws.join("projects"))
        .args([
            "workweave",
            "web-app",
            "create",
            "feat",
            "--dir",
            "../../alt/feat",
        ])
        .current_dir(tmp.path())
        .assert()
        .success();

    assert!(wanted.join(".rwv-workweave").is_file());
    assert_eq!(
        index(&ws)["workweaves"]["feat"].as_str().unwrap(),
        canonical(&wanted)
    );
}

#[test]
fn create_from_reads_a_relative_path_from_the_invocation_subdirectory() {
    let tmp = common::tempdir().unwrap();
    let ws = weave_with_member(tmp.path());
    common::rwv()
        .args(["workweave", "web-app", "create", "a"])
        .current_dir(&ws)
        .assert()
        .success();
    let a = tmp.path().join("deep/.workweaves/web-app--a");
    assert!(a.is_dir());

    common::rwv()
        .args([
            "workweave",
            "web-app",
            "create",
            "b",
            "--from",
            "../../.workweaves/web-app--a",
        ])
        .current_dir(ws.join("projects"))
        .assert()
        .success();

    let marker = std::fs::read_to_string(
        tmp.path()
            .join("deep/.workweaves/web-app--b/.rwv-workweave"),
    )
    .unwrap();
    let marker: serde_json::Value = serde_json::from_str(&marker).unwrap();
    assert_eq!(marker["parent"].as_str().unwrap(), canonical(&a));
}

// ---------------------------------------------------------------------------
// add --adopt
// ---------------------------------------------------------------------------

struct AdoptWeave {
    primary: PathBuf,
    workweave: PathBuf,
}

fn adopt_weave(tmp: &Path) -> AdoptWeave {
    let primary = tmp.join("ws");
    let primary_project = primary.join("projects/test-project");
    init_project(&primary_project);
    std::fs::write(primary.join(".rwv-active"), "test-project\n").unwrap();

    let workweave = tmp.join(".workweaves/test-project--feat");
    std::fs::create_dir_all(&workweave).unwrap();
    let primary_canonical = primary.canonicalize().unwrap();
    std::fs::write(
        workweave.join(".rwv-workweave"),
        common::workweave_marker(&primary_canonical, "test-project", &primary_canonical),
    )
    .unwrap();
    common::register_workweave(&primary_canonical, "test-project", "feat", &workweave);
    init_project(&workweave.join("projects/test-project"));

    AdoptWeave { primary, workweave }
}

fn init_project(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("rwv.toml"), "[repositories]\n").unwrap();
    common::git_in(dir, &["add", "rwv.toml"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// A clone at `dest` of a fresh bare remote under `tmp/remotes/<name>.git`.
fn clone_at(tmp: &Path, name: &str, dest: &Path) {
    let source = tmp.join("remotes/src").join(name);
    init_repo_with_commit(&source);
    let bare = tmp.join("remotes").join(format!("{name}.git"));
    common::git_in(
        tmp,
        &[
            "clone",
            "--bare",
            &source.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    common::git_in(
        tmp,
        &["clone", &bare.to_string_lossy(), &dest.to_string_lossy()],
    );
}

fn manifest_keys(project_dir: &Path) -> Vec<String> {
    let raw = std::fs::read_to_string(project_dir.join("rwv.toml")).unwrap();
    let doc: toml::Table = raw.parse().unwrap();
    doc.get("repositories")
        .and_then(|r| r.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default()
}

#[test]
fn adopt_from_a_primary_subdirectory_under_c_records_the_weave_relative_member_path() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    clone_at(tmp.path(), "lib", &w.primary.join("bar/lib"));

    common::rwv()
        .arg("-C")
        .arg(w.primary.join("bar"))
        .args(["add", "--adopt", "lib"])
        .current_dir(tmp.path())
        .assert()
        .success();

    assert_eq!(
        manifest_keys(&w.primary.join("projects/test-project")),
        vec!["bar/lib".to_owned()]
    );
}

#[test]
fn adopt_from_a_workweave_subdirectory_names_the_primarys_clone_at_that_member_path() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    clone_at(tmp.path(), "x", &w.primary.join("bar/x"));
    std::fs::create_dir_all(w.workweave.join("bar")).unwrap();

    common::rwv()
        .args(["add", "--adopt", "x"])
        .current_dir(w.workweave.join("bar"))
        .assert()
        .success();

    assert_eq!(
        manifest_keys(&w.workweave.join("projects/test-project")),
        vec!["bar/x".to_owned()]
    );
    assert!(
        w.workweave.join("bar/x/.git").is_file(),
        "the workweave gets a worktree of the primary's clone"
    );
}

#[test]
fn adopt_of_a_location_under_neither_weave_root_refuses_naming_both() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    clone_at(tmp.path(), "stray", &tmp.path().join("outside/stray"));

    let out = common::rwv()
        .args(["add", "--adopt", "../../../outside/stray"])
        .current_dir(w.workweave.join("projects"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert_routes_to(&stderr, "adopt-path-outside-weave");
    assert!(
        stderr.contains(&canonical(&w.primary)),
        "stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains(&canonical(&w.workweave)),
        "stderr was:\n{stderr}"
    );
    assert!(manifest_keys(&w.workweave.join("projects/test-project")).is_empty());
}

#[test]
fn adopt_of_a_workweave_clone_that_is_not_the_primarys_refuses_naming_both() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    clone_at(tmp.path(), "dup", &w.primary.join("bar/dup"));
    clone_at(tmp.path(), "own", &w.workweave.join("bar/dup"));

    let out = common::rwv()
        .args(["add", "--adopt", "bar/dup"])
        .current_dir(&w.workweave)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert_routes_to(&stderr, "foreign-clone-in-workweave");
    assert!(
        stderr.contains(&canonical(&w.primary.join("bar/dup"))),
        "stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains(&canonical(&w.workweave.join("bar/dup"))),
        "stderr was:\n{stderr}"
    );
    assert!(manifest_keys(&w.workweave.join("projects/test-project")).is_empty());
}
