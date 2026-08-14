//! An op resolves every workspace other than the one it was invoked from under
//! its own project, and refuses when that workspace's own record disagrees.
//!
//! Two claims, one per direction of the same rule:
//!
//!   - a source workspace whose `.rwv-active` names a sibling project is still
//!     read as the project the op is landing — the pointer belongs to whoever
//!     works in that workspace, not to this op;
//!   - a target workspace whose `.rwv-workweave` marker names another project
//!     is refused, naming both records. That one is not a binding to choose but
//!     two structural statements in contradiction, and picking either silently
//!     is how a landing ends up in a workspace that was never its subject.

use assert_cmd::Command;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_out(args: &[&str], dir: &Path) -> String {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), manifest_toml).unwrap();
}

fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    let entries: Vec<String> = repos
        .iter()
        .map(|(path, url, sha)| {
            format!("{path:?}: {{\"type\": \"git\", \"url\": {url:?}, \"version\": {sha:?}}}")
        })
        .collect();
    let raw = format!("{{\"repositories\": {{{}}}}}", entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
}

const SERVER_URL: &str = "https://github.com/example/server.git";
const SERVER_PATH: &str = "github/example/server";
const SYNCED_PROJECT: &str = "web-app";
const OTHER_PROJECT: &str = "other-app";

fn make_project(root: &Path, name: &str, repos: &[(&str, &str)], locked: &[(&str, &str, &str)]) {
    let dir = root.join(format!("projects/{name}"));
    init_repo(&dir);
    std::fs::write(dir.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    write_manifest(&dir, repos);
    write_lock(&dir, locked);
    git(&["add", ".gitattributes", "rwv.toml", "rwv.lock"], &dir);
    git(&["commit", "-m", format!("{name}: initial").as_str()], &dir);
}

struct Weave {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

/// A primary weave holding two projects, plus a second primary-shaped
/// workspace whose repos are worktree pairs of the first's `web-app` repos.
fn make_pair(parent: &Path) -> (Weave, Weave) {
    let source = parent.join("source");
    std::fs::create_dir_all(source.join("github/example")).unwrap();
    std::fs::create_dir_all(source.join("projects")).unwrap();

    let source_server = source.join(SERVER_PATH);
    let sha = init_repo(&source_server);
    make_project(
        &source,
        SYNCED_PROJECT,
        &[(SERVER_PATH, SERVER_URL)],
        &[(SERVER_PATH, SERVER_URL, &sha)],
    );
    make_project(&source, OTHER_PROJECT, &[], &[]);
    std::fs::write(source.join(".rwv-active"), format!("{SYNCED_PROJECT}\n")).unwrap();

    let dest = parent.join("dest");
    std::fs::create_dir_all(dest.join("github/example")).unwrap();
    std::fs::create_dir_all(dest.join("projects")).unwrap();

    let dest_server = dest.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            &dest_server.to_string_lossy(),
            "-b",
            "dest/server",
        ],
        &source_server,
    );
    let dest_project = dest.join(format!("projects/{SYNCED_PROJECT}"));
    git(
        &[
            "worktree",
            "add",
            &dest_project.to_string_lossy(),
            "-b",
            "dest/project",
        ],
        &source.join(format!("projects/{SYNCED_PROJECT}")),
    );
    std::fs::write(dest.join(".rwv-active"), format!("{SYNCED_PROJECT}\n")).unwrap();

    (
        Weave {
            root: source,
            project_dir: parent.join(format!("source/projects/{SYNCED_PROJECT}")),
            server_dir: source_server,
        },
        Weave {
            root: dest,
            project_dir: dest_project,
            server_dir: dest_server,
        },
    )
}

/// `rwv sync <source>` binds the SOURCE to the project this invocation
/// resolved, not to whatever the source workspace happens to have activated.
/// Both workspaces here are primary-shaped, which is the shape that has no
/// marker to fall back on and therefore the one where the pointer used to
/// decide for a workspace that was not the subject of the invocation.
#[test]
fn a_pull_reads_the_source_under_the_invocations_project() {
    let tmp = common::tempdir().unwrap();
    let (source, dest) = make_pair(tmp.path());

    // The source advances the project being synced.
    let advanced = make_commit(&source.server_dir, "src.txt", "source\n", "source: advance");
    write_lock(&source.project_dir, &[(SERVER_PATH, SERVER_URL, &advanced)]);
    git(&["add", "rwv.lock"], &source.project_dir);
    git(
        &["commit", "-m", "lock: source advance"],
        &source.project_dir,
    );

    // ...and its operator then activates the sibling project. Nothing about
    // that is unusual, and nothing about it concerns this pull.
    std::fs::write(
        source.root.join(".rwv-active"),
        format!("{OTHER_PROJECT}\n"),
    )
    .unwrap();

    let before = git_out(&["rev-parse", "HEAD"], &dest.server_dir);
    assert_ne!(
        before, advanced,
        "the fixture must leave the destination behind the source"
    );

    rwv()
        .args(["sync", &source.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&dest.root)
        .assert()
        .success();

    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &dest.server_dir),
        advanced,
        "the pull must read the source's `{SYNCED_PROJECT}` lock; binding the \
         source to its own `.rwv-active` reads a sibling project's lock instead, \
         which either fails naming the wrong path or — where the two projects \
         share history — pulls across projects"
    );
}

/// The refusal §3b of the ambient-binding design adds: a workspace whose
/// marker names another project is a contradiction between two structural
/// records, and the operator sees both.
#[test]
fn landing_into_a_workweave_of_another_project_is_refused_naming_both() {
    let tmp = common::tempdir().unwrap();
    let (source, _dest) = make_pair(tmp.path());

    // A workweave of the OTHER project, marker and all.
    let stray = tmp.path().join("stray");
    std::fs::create_dir_all(&stray).unwrap();
    let primary = source.root.canonicalize().unwrap();
    let marker = common::workweave_marker(&primary, OTHER_PROJECT, &primary);
    std::fs::write(stray.join(".rwv-workweave"), marker).unwrap();

    let output = rwv()
        .args(["sync-to", &stray.to_string_lossy(), "--strategy=ff"])
        .current_dir(&source.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains(OTHER_PROJECT),
        "the refusal must name the project the target's marker claims; got:\n{stderr}"
    );
    assert!(
        stderr.contains(SYNCED_PROJECT),
        "the refusal must name the project the op is bound to; got:\n{stderr}"
    );
    assert!(
        !source.root.join(".rwv-op").exists(),
        "a refusal at binding time must leave no op-state behind"
    );
}
