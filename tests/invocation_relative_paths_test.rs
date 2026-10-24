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
    repoweave::path_spelling::resolve(p)
        .unwrap()
        .to_string_lossy()
        .into_owned()
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
    let primary_canonical = repoweave::path_spelling::resolve(&primary).unwrap();
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

fn doctor_finding_kinds_at(weave: &Path, member: &str) -> Vec<String> {
    let out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(weave)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must parse ({e}):\n{stdout}"));
    report["violations"]
        .as_array()
        .expect("the report carries a violations array")
        .iter()
        .filter(|v| v["path"] == member)
        .map(|v| v["kind"].as_str().unwrap_or_default().to_owned())
        .collect()
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
    assert!(
        !doctor_finding_kinds_at(&w.primary, "bar/lib").contains(&"dangling-reference".to_owned()),
        "the adopted clone is the member on disk"
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
    assert!(
        !doctor_finding_kinds_at(&w.workweave, "bar/x").contains(&"dangling-reference".to_owned()),
        "the workweave's worktree is the member on disk"
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
    common::assert_names_operator_path(
        &stderr,
        repoweave::path_spelling::resolve(&w.primary).unwrap(),
    );
    common::assert_names_operator_path(
        &stderr,
        repoweave::path_spelling::resolve(&w.workweave).unwrap(),
    );
    assert!(manifest_keys(&w.workweave.join("projects/test-project")).is_empty());

    std::fs::create_dir_all(w.primary.join("github/o")).unwrap();
    std::fs::rename(
        tmp.path().join("outside/stray"),
        w.primary.join("github/o/stray"),
    )
    .unwrap();
    let findings_before = common::doctor_findings(&w.workweave);
    let followed = common::run_advised("rwv add --adopt github/o/stray", &w.workweave);
    assert!(
        followed.status.success(),
        "naming the clone by a path inside the weave:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert_eq!(
        manifest_keys(&w.workweave.join("projects/test-project")),
        vec!["github/o/stray".to_owned()]
    );
    common::assert_no_new_findings(&w.workweave, &findings_before);
}

#[test]
fn adopt_of_a_workweave_clone_that_is_not_the_primarys_refuses_naming_both() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    clone_at(tmp.path(), "dup", &w.primary.join("github/o/dup"));
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &tmp.path().join("remotes/dup.git").to_string_lossy(),
            &w.workweave.join("github/o/dup").to_string_lossy(),
        ],
    );

    let out = common::rwv()
        .args(["add", "--adopt", "github/o/dup"])
        .current_dir(&w.workweave)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert_routes_to(&stderr, "foreign-clone-in-workweave");
    common::assert_names_operator_path(
        &stderr,
        repoweave::path_spelling::resolve(&w.primary.join("github/o/dup")).unwrap(),
    );
    common::assert_names_operator_path(
        &stderr,
        repoweave::path_spelling::resolve(&w.workweave.join("github/o/dup")).unwrap(),
    );
    assert!(manifest_keys(&w.workweave.join("projects/test-project")).is_empty());

    let named = advised_primary_clone(&stderr);
    let findings_before = common::doctor_findings(&w.workweave);
    let followed = common::run_advised(&format!("rwv add --adopt {named}"), &w.workweave);
    assert!(
        followed.status.success(),
        "naming the primary's clone directly:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert_eq!(
        manifest_keys(&w.workweave.join("projects/test-project")),
        vec!["github/o/dup".to_owned()]
    );
    let fixed = common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(&w.workweave)
        .output()
        .unwrap();
    assert!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stdout)
    );
    assert!(
        w.workweave.join("github/o/dup/.git").is_file(),
        "the workweave's own clone became a worktree of the primary's"
    );
    common::assert_no_new_findings(&w.workweave, &findings_before);
}

/// The primary's clone path a foreign-clone refusal tells the operator to
/// name.
fn advised_primary_clone(stderr: &str) -> String {
    let after = stderr
        .split("or name '")
        .nth(1)
        .expect("the refusal names the primary's clone");
    after.split("' directly").next().unwrap().to_owned()
}

#[test]
fn adopt_of_a_workweave_clone_clears_once_moved_to_where_the_primary_has_none() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    clone_at(tmp.path(), "own", &w.workweave.join("github/o/dup"));

    let out = common::rwv()
        .args(["add", "--adopt", "github/o/dup"])
        .current_dir(&w.workweave)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert_routes_to(&stderr, "foreign-clone-in-workweave");

    let there = advised_primary_clone(&stderr);
    assert!(!Path::new(&there).exists(), "the primary holds no clone");
    std::fs::create_dir_all(w.primary.join("github/o")).unwrap();
    std::fs::rename(w.workweave.join("github/o/dup"), &there).unwrap();
    let findings_before = common::doctor_findings(&w.workweave);
    let followed = common::run_advised("rwv add --adopt github/o/dup", &w.workweave);
    assert!(
        followed.status.success(),
        "re-running the adopt after moving the clone:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert_eq!(
        manifest_keys(&w.workweave.join("projects/test-project")),
        vec!["github/o/dup".to_owned()]
    );
    assert!(w.workweave.join("github/o/dup/.git").is_file());
    common::assert_no_new_findings(&w.workweave, &findings_before);
}

#[test]
fn adopt_into_a_workweave_whose_placement_holds_a_plain_directory_refuses_naming_it() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    clone_at(tmp.path(), "dup", &w.primary.join("github/o/dup"));
    let occupied = w.workweave.join("github/o/dup");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("notes.txt"), "not a clone\n").unwrap();

    let out = common::rwv()
        .args(["add", "--adopt", "github/o/dup"])
        .current_dir(&w.workweave)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr was:\n{stderr}");
    assert_routes_to(&stderr, "non-clone-at-placement");
    common::assert_names_operator_path(
        &stderr,
        repoweave::path_spelling::resolve(&occupied).unwrap(),
    );
    assert!(manifest_keys(&w.workweave.join("projects/test-project")).is_empty());
    assert!(occupied.join("notes.txt").is_file());

    let retry = common::advised_command(&stderr, "rwv add");
    std::fs::rename(&occupied, w.workweave.join("github/o/dup.aside")).unwrap();
    let findings_before = common::doctor_findings(&w.workweave);
    let followed = common::run_advised(&retry, &w.workweave);
    assert!(
        followed.status.success(),
        "`{retry}` after moving the directory aside:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert_eq!(
        manifest_keys(&w.workweave.join("projects/test-project")),
        vec!["github/o/dup".to_owned()]
    );
    assert!(
        occupied.join(".git").is_file(),
        "the workweave gets a worktree of the primary's clone"
    );
    common::assert_no_new_findings(&w.workweave, &findings_before);
}

#[test]
fn add_from_a_workweave_whose_placement_holds_a_plain_directory_refuses_naming_it() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    let bare = tmp.path().join("ours/remotes/lib.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    let occupied = w.workweave.join("local/remotes/lib");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("notes.txt"), "not a clone\n").unwrap();

    let out = common::rwv()
        .args(["add", &common::file_url(&bare)])
        .current_dir(&w.workweave)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr was:\n{stderr}");
    assert_routes_to(&stderr, "non-clone-at-placement");
    common::assert_names_operator_path(
        &stderr,
        repoweave::path_spelling::resolve(&occupied).unwrap(),
    );
    assert!(manifest_keys(&w.workweave.join("projects/test-project")).is_empty());
    assert!(occupied.join("notes.txt").is_file());

    let retry = common::advised_command(&stderr, "rwv add");
    assert_eq!(retry, format!("rwv add {}", common::file_url(&bare)));
    std::fs::rename(&occupied, w.workweave.join("local/remotes/lib.aside")).unwrap();
    let findings_before = common::doctor_findings(&w.workweave);
    let followed = common::run_advised(&retry, &w.workweave);
    assert!(
        followed.status.success(),
        "`{retry}` after moving the directory aside:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert_eq!(
        manifest_keys(&w.workweave.join("projects/test-project")),
        vec!["local/remotes/lib".to_owned()]
    );
    assert!(occupied.join(".git").exists());
    common::assert_no_new_findings(&w.workweave, &findings_before);
}

#[test]
fn add_from_a_workweave_whose_primary_placement_holds_a_plain_directory_clears_once_moved_aside() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    let bare = tmp.path().join("ours/remotes/lib.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    let occupied = w.primary.join("local/remotes/lib");
    std::fs::create_dir_all(occupied.join("junk")).unwrap();

    let out = common::rwv()
        .args(["add", &common::file_url(&bare)])
        .current_dir(&w.workweave)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "stderr was:\n{stderr}");
    assert_routes_to(&stderr, "non-clone-at-placement");
    common::assert_names_operator_path(
        &stderr,
        repoweave::path_spelling::resolve(&occupied).unwrap(),
    );

    let retry = common::advised_command(&stderr, "rwv add");
    std::fs::rename(&occupied, w.primary.join("local/remotes/lib.aside")).unwrap();
    let findings_before = common::doctor_findings(&w.workweave);
    let followed = common::run_advised(&retry, &w.workweave);
    assert!(
        followed.status.success(),
        "`{retry}` after moving the directory aside:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert_eq!(
        manifest_keys(&w.workweave.join("projects/test-project")),
        vec!["local/remotes/lib".to_owned()]
    );
    assert!(
        occupied.join(".git").is_dir(),
        "the primary holds the clone"
    );
    assert!(w.workweave.join("local/remotes/lib/.git").is_file());
    common::assert_no_new_findings(&w.workweave, &findings_before);
}

// ---------------------------------------------------------------------------
// add --adopt asks which weave root holds the location the path resolves to
//
// `..` after a symlink climbs from where the link points, so the weave root
// containing the named clone is a question about the resolved location, not
// about the spelling. Unix only for the `sym/..` shapes: Win32 folds `..`
// against the spelling before it follows a link, so there the two readings
// name one location and no fixture can tell them apart.
// ---------------------------------------------------------------------------

fn link_dir(target: &Path, link: &Path) {
    repoweave::symlink::create(target, link, repoweave::symlink::LinkTarget::Directory).unwrap();
}

#[cfg(unix)]
#[test]
fn adopt_through_a_symlink_and_dotdot_registers_the_clone_the_os_names() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    clone_at(tmp.path(), "top", &w.primary.join("github/o/r"));
    clone_at(tmp.path(), "deep", &w.primary.join("deep/github/o/r"));
    std::fs::create_dir_all(w.primary.join("deep/x")).unwrap();
    link_dir(&w.primary.join("deep/x"), &w.primary.join("sym"));

    common::rwv()
        .args(["add", "--adopt", "sym/../github/o/r"])
        .current_dir(&w.primary)
        .assert()
        .success();

    assert_eq!(
        manifest_keys(&w.primary.join("projects/test-project")),
        vec!["deep/github/o/r".to_owned()],
        "`sym/..` climbs from `deep/x`, so the clone named is the one under `deep`"
    );
}

#[cfg(unix)]
#[test]
fn adopt_through_a_symlink_and_dotdot_out_of_the_weave_refuses() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    clone_at(tmp.path(), "inside", &w.primary.join("github/o/r"));
    clone_at(
        tmp.path(),
        "outside",
        &tmp.path().join("elsewhere/a/github/o/r"),
    );
    std::fs::create_dir_all(tmp.path().join("elsewhere/a/b")).unwrap();
    link_dir(&tmp.path().join("elsewhere/a/b"), &w.primary.join("sym"));

    let out = common::rwv()
        .args(["add", "--adopt", "sym/../github/o/r"])
        .current_dir(&w.primary)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "stderr was:\n{stderr}");
    assert_routes_to(&stderr, "adopt-path-outside-weave");
    common::assert_names_operator_path(
        &stderr,
        repoweave::path_spelling::resolve(&tmp.path().join("elsewhere/a/github/o/r")).unwrap(),
    );
    assert!(manifest_keys(&w.primary.join("projects/test-project")).is_empty());

    let findings_before = common::doctor_findings(&w.primary);
    let followed = common::run_advised("rwv add --adopt github/o/r", &w.primary);
    assert!(
        followed.status.success(),
        "naming the clone by a path inside the weave:\n{}",
        String::from_utf8_lossy(&followed.stderr)
    );
    assert_eq!(
        manifest_keys(&w.primary.join("projects/test-project")),
        vec!["github/o/r".to_owned()]
    );
    common::assert_no_new_findings(&w.primary, &findings_before);
}

#[test]
fn adopt_through_a_symlinked_spelling_of_the_weave_records_the_member_path() {
    let tmp = common::tempdir().unwrap();
    let w = adopt_weave(tmp.path());
    clone_at(tmp.path(), "lib", &w.primary.join("github/o/r"));
    let link = tmp.path().join("link");
    link_dir(&w.primary, &link);

    common::rwv()
        .args(["add", "--adopt"])
        .arg(link.join("github").join("o").join("r"))
        .current_dir(&w.primary)
        .assert()
        .success();

    assert_eq!(
        manifest_keys(&w.primary.join("projects/test-project")),
        vec!["github/o/r".to_owned()]
    );
}
