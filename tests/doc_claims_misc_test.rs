//! Integration tests verifying doc claims about `rwv` context display.
//!
//! The claim covered here: `rwv` with no subcommand prints the weave root, the
//! active project, the workweave when there is one, and the count of repos the
//! active project's manifest names.
//!
//! `tests/context_display_test.rs` drives the same no-subcommand surface from
//! an earlier generation, and the overlap is narrower than it looks. Its
//! fixtures write a manifest with no entries, so the `Repos:` line is one no
//! fixture there ever produces; it reaches `Project:` only from a workweave
//! marker, and asserts it by containment over the whole of stdout. Whole-line
//! equality on `Project:` and `Repos:` lives here and nowhere else in the
//! suite — a display that printed a repo count for the wrong project would be
//! caught by no other test.

use assert_cmd::Command;
use std::path::Path;

mod common;

// ---------------------------------------------------------------------------
// Helpers (mirrored from workweave_test.rs)
// ---------------------------------------------------------------------------

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

/// Create a minimal workspace with one repo and a plain (non-git) project dir.
///
/// Layout:
///   {tmp}/ws/                         -- workspace root
///   {tmp}/ws/github/org/repo/         -- a real git repo
///   {tmp}/ws/projects/{project}/      -- plain dir with rwv.toml
fn make_workspace(tmp: &Path, project: &str) -> std::path::PathBuf {
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

/// Create a workspace with two code repos and a plain project dir listing both.
fn make_workspace_two_repos(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo1 = ws.join("github/org/alpha");
    let repo2 = ws.join("github/org/beta");
    init_repo_with_commit(&repo1);
    init_repo_with_commit(&repo2);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"[repositories."github/org/alpha"]
type = "git"
url = "file://{r1}"
version = "main"
role = "owned"

[repositories."github/org/beta"]
type = "git"
url = "file://{r2}"
version = "main"
role = "owned"
"#,
        r1 = common::url_path(&repo1),
        r2 = common::url_path(&repo2)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    ws
}

// ---------------------------------------------------------------------------
// 1. rwv_display_shows_repos
//
// Doc claim: "`rwv` (no subcommand) shows root, project, workweave, repos"
//
// With an active project, the output carries the weave root path, the active
// project name, and the count of repos the active project's manifest names.
// ---------------------------------------------------------------------------

/// The display names the active project and counts its manifest's repos.
///
/// Both are whole-line equalities because every weaker form is satisfied by
/// the fixture's own scaffolding: this workspace's paths already spell
/// `web-app`, and a bare digit `2` appears in temp-directory names often
/// enough to be no evidence at all. The count in particular has to be read as
/// a value — a display that reported the repos of some other project, or
/// stopped counting `reference` entries, prints a number either way.
///
/// The repo NAMES are deliberately not asserted: the line is a count, and a
/// test looking for `alpha` would be looking for output this surface has
/// never produced.
#[test]
fn rwv_display_shows_repos() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_two_repos(tmp.path(), "web-app");

    // Activate the project by writing .rwv-active.
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();

    let output = rwv()
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");

    common::assert_weave_line(&stdout, ws.canonicalize().unwrap());

    let line = |prefix: &str| {
        stdout
            .lines()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("context display has no `{prefix}` line:\n{stdout}"))
            .to_owned()
    };
    assert_eq!(line("Project:"), "Project: web-app");
    assert_eq!(line("Repos:"), "Repos: 2");
}

// ---------------------------------------------------------------------------
// 3. rwv_display_no_active_project
//
// With no .rwv-active file, the output should still show the root and list
// available projects, but not claim any project is active.
// ---------------------------------------------------------------------------

#[test]
fn rwv_display_no_active_project() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "my-project");

    // Deliberately do NOT write .rwv-active — no project is active.

    let output = rwv()
        .current_dir(&ws)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");

    // Root path must appear.
    common::assert_weave_line(&stdout, ws.canonicalize().unwrap());

    // The available project should be listed (the projects/ dir is scanned).
    assert!(
        stdout.contains("my-project"),
        "output should list available projects (found 'my-project'), got:\n{stdout}"
    );

    // No "Project:" line should appear when nothing is active.
    assert!(
        !stdout.contains("Project:"),
        "output should NOT show 'Active project:' when no project is active, got:\n{stdout}"
    );
}

// ---------------------------------------------------------------------------
// 4. rwv_display_in_workweave
//
// Doc claim: running `rwv` from inside a workweave shows "workweave" location
// and the workweave name.
// ---------------------------------------------------------------------------

#[test]
fn rwv_display_in_workweave() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create the workweave.
    rwv()
        .args(["workweave", "web-app", "create", "display-test"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--display-test");
    assert!(ww_dir.exists(), "workweave directory should exist");

    // Run `rwv` (no subcommand) from inside the workweave directory.
    let output = rwv()
        .current_dir(&ww_dir)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).expect("stdout should be valid UTF-8");

    // Whole-line equality, not containment: the workweave directory's own
    // path spells both `workweave` and `display-test`, so a run that printed
    // the path under any other label — or under none — satisfies a search for
    // either word. The label and the value have to be read together.
    let named = stdout
        .lines()
        .find_map(|l| l.strip_prefix("Workweave: "))
        .unwrap_or_else(|| panic!("context display has no `Workweave:` line:\n{stdout}"));
    assert_eq!(
        named,
        repoweave::path_spelling::operator_path(&ww_dir.canonicalize().unwrap())
    );

    // Root path (primary weave root) must appear.
    common::assert_weave_line(&stdout, ws.canonicalize().unwrap());
}
