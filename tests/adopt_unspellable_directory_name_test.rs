//! `rwv add --adopt` refuses a clone whose position under the weave root
//! passes through a directory name no member path can spell, names that
//! directory and why, and leaves the renaming to the operator.

#![cfg(unix)]

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

mod common;

fn weave(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    let project = ws.join("projects/test-project");
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "--initial-branch=main"]);
    std::fs::write(project.join("rwv.toml"), "[repositories]\n").unwrap();
    std::fs::write(ws.join(".rwv-active"), "test-project\n").unwrap();
    ws
}

fn clone_into(tmp: &Path, dest: &Path) {
    let bare = tmp.join("remotes/lib.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    common::init_bare_repo_with_commit(&bare);
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    common::git_in(
        tmp,
        &["clone", &bare.to_string_lossy(), &dest.to_string_lossy()],
    );
}

fn adopt(ws: &Path, path: &Path) -> (bool, String) {
    let out = common::rwv()
        .arg("add")
        .arg("--adopt")
        .arg(path)
        .current_dir(ws)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, [token], "stderr was:\n{stderr}");
}

fn manifest_unchanged(ws: &Path) {
    assert_eq!(
        std::fs::read_to_string(ws.join("projects/test-project/rwv.toml")).unwrap(),
        "[repositories]\n"
    );
}

#[test]
fn a_backslash_in_a_directory_name_refuses_naming_the_rename() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    clone_into(tmp.path(), &ws.join("github/o/a\\b"));

    let (ok, stderr) = adopt(&ws, Path::new("github/o/a\\b"));

    assert!(!ok, "stderr was:\n{stderr}");
    assert_routes_to(&stderr, "unspellable-directory-name");
    assert!(
        stderr.contains("'a\\b'") && stderr.contains("Rename the directory"),
        "the refusal names the directory and the rename; stderr was:\n{stderr}"
    );
    manifest_unchanged(&ws);
}

#[test]
fn a_directory_name_that_is_not_unicode_refuses_naming_the_rename() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());
    let spelled = ws.join("github/o/lib");
    clone_into(tmp.path(), &spelled);
    let unspellable = ws
        .join("github/o")
        .join(std::ffi::OsStr::from_bytes(b"lib\xff"));
    std::fs::rename(&spelled, &unspellable).unwrap();
    let link = tmp.path().join("to-the-clone");
    std::os::unix::fs::symlink(&unspellable, &link).unwrap();

    let (ok, stderr) = adopt(&ws, &link);

    assert!(!ok, "stderr was:\n{stderr}");
    assert_routes_to(&stderr, "unspellable-directory-name");
    assert!(
        stderr.contains(r#""lib\xFF""#) && stderr.contains("not Unicode"),
        "the refusal names the directory and why; stderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("Rename the directory"),
        "stderr was:\n{stderr}"
    );
    manifest_unchanged(&ws);
}
