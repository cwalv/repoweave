//! An argument's meaning comes from how it is spelled, and a project from
//! `--project` or `.rwv-active` — never from where CWD happens to be or what a
//! path happens to resolve to. Each refusal here is the binary's stderr, read
//! for its route line.

use std::path::{Path, PathBuf};

mod common;

fn refusal_stderr(args: &[&str], cwd: &Path) -> String {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(
        output.status.code(),
        Some(1),
        "rwv {args:?} was expected to refuse with exit 1\n{stderr}"
    );
    stderr
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routed: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(
        routed,
        vec![token],
        "expected exactly one route line naming `{token}`, stderr was:\n{stderr}"
    );
}

fn init_project(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("rwv.toml"), "[repositories]\n").unwrap();
    common::fixture_lock(dir, &[]);
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// A weave holding `web-app` (active) and `mobile`, both with empty manifests.
fn two_project_weave(root: &Path) -> PathBuf {
    let ws = root.join("ws");
    init_project(&ws.join("projects/web-app"));
    init_project(&ws.join("projects/mobile"));
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();
    ws
}

fn project_files(ws: &Path) -> Vec<(PathBuf, String)> {
    ["web-app", "mobile"]
        .iter()
        .flat_map(|p| ["rwv.toml", "rwv.lock"].map(|f| ws.join("projects").join(p).join(f)))
        .map(|f| {
            let body = std::fs::read_to_string(&f).unwrap();
            (f, body)
        })
        .collect()
}

#[test]
fn every_writing_verb_refuses_from_another_projects_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let mobile = ws.join("projects/mobile");
    let before = project_files(&ws);

    let remote = common::file_url(tmp.path().join("nowhere.git"));
    let cases: &[&[&str]] = &[
        &["add", &remote],
        &["remove", "github/acme/server"],
        &["role", "github/acme/server", "owned"],
        &["lock"],
        &["update"],
        &["push"],
        &["sync", "primary"],
        &["sync-to", "primary"],
        &["fetch"],
        &["doctor", "--fix"],
    ];
    for args in cases {
        let stderr = refusal_stderr(args, &mobile);
        assert_routes_to(&stderr, "cwd-in-inactive-project");
        assert!(
            stderr.contains("projects/mobile/")
                && stderr.contains("web-app")
                && stderr.contains("--project mobile"),
            "rwv {args:?} must name both projects and `--project mobile`:\n{stderr}"
        );
    }
    assert_eq!(
        project_files(&ws),
        before,
        "a refused verb must not have written either project's files"
    );
}

#[test]
fn doctor_without_fix_reports_from_another_projects_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());

    let output = common::rwv()
        .arg("doctor")
        .current_dir(ws.join("projects/mobile"))
        .output()
        .expect("rwv should run");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("cwd-in-inactive-project"),
        "a report changes nothing, so it does not refuse:\n{stderr}"
    );
}

#[test]
fn fetch_refuses_to_guess_among_several_projects_and_names_the_project_flag() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    std::fs::remove_file(ws.join(".rwv-active")).unwrap();

    let stderr = refusal_stderr(&["fetch"], &ws);
    assert_routes_to(&stderr, "no-active-project");
    assert!(stderr.contains("--project <name>"), "{stderr}");
    assert!(
        !ws.join(".rwv-active").exists(),
        "with two projects there is nothing to select"
    );
}

#[test]
fn fetch_project_flag_with_a_source_is_inapplicable() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let source = common::file_url(tmp.path().join("web-app.git"));

    let stderr = refusal_stderr(&["fetch", "--project", "mobile", &source], &ws);
    assert_routes_to(&stderr, "inapplicable-flag");
    assert!(stderr.contains("--project"), "{stderr}");
}

#[test]
fn add_refuses_a_plain_path_to_a_bare_remote_and_names_both_spellings() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let bare = tmp.path().join("remotes/lib.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    let before = project_files(&ws);
    let path = bare.to_str().unwrap();

    let stderr = refusal_stderr(&["add", path], &ws);
    assert_routes_to(&stderr, "unresolvable-repo-source");
    assert!(
        stderr.contains(&format!("`rwv add {}`", common::file_url(&bare)))
            && stderr.contains(&format!("`rwv add --adopt {path}`")),
        "the refusal must name the file:// spelling and --adopt:\n{stderr}"
    );
    assert_eq!(project_files(&ws), before);
    assert!(!ws.join("local").exists(), "nothing may have been cloned");
}

#[test]
fn add_no_longer_adopts_an_existing_clone_named_by_its_path() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let bare = tmp.path().join("other.git");
    common::init_bare_repo_with_commit(&bare);
    let clone = ws.join("vendor/other");
    std::fs::create_dir_all(clone.parent().unwrap()).unwrap();
    common::git_in(
        tmp.path(),
        &["clone", bare.to_str().unwrap(), clone.to_str().unwrap()],
    );
    let before = project_files(&ws);

    let stderr = refusal_stderr(&["add", "vendor/other"], &ws);
    assert_routes_to(&stderr, "unresolvable-repo-source");
    assert!(
        stderr.contains("`rwv add --adopt vendor/other`"),
        "{stderr}"
    );
    assert_eq!(
        project_files(&ws),
        before,
        "a path is not registered without --adopt"
    );
}

#[test]
fn adopt_refuses_a_path_with_no_clone_including_a_bare_remote() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    let bare = ws.join("remotes/lib.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    let before = project_files(&ws);

    for path in ["vendor/missing", "remotes/lib.git"] {
        let stderr = refusal_stderr(&["add", "--adopt", path], &ws);
        assert_routes_to(&stderr, "no-clone-to-adopt");
        assert!(stderr.contains("file://"), "{stderr}");
    }
    assert_eq!(project_files(&ws), before);
}

#[test]
fn adopt_and_new_are_exclusive() {
    let tmp = common::tempdir().unwrap();
    let ws = two_project_weave(tmp.path());
    common::rwv()
        .args(["add", "--adopt", "--new", "github/acme/fresh"])
        .current_dir(&ws)
        .assert()
        .code(2);
}

#[test]
fn fetch_refuses_a_plain_path_source_and_names_both_spellings() {
    let tmp = common::tempdir().unwrap();
    let target = tmp.path().join("fresh");
    std::fs::create_dir_all(&target).unwrap();
    let bare = tmp.path().join("web-app.git");
    common::init_bare_repo_with_commit(&bare);
    let path = bare.to_str().unwrap();

    let stderr = refusal_stderr(&["fetch", path], &target);
    assert_routes_to(&stderr, "unresolvable-repo-source");
    assert!(
        stderr.contains(&format!("`rwv fetch {}`", common::file_url(&bare)))
            && stderr.contains("`rwv fetch --project <name>`"),
        "the refusal must name the file:// spelling and the in-place spelling:\n{stderr}"
    );
    assert!(
        !target.join("projects").exists(),
        "nothing may have been cloned"
    );
}
