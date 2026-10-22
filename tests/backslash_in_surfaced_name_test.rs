//! A `\` in a surfaced name is a character of that name on Unix, not a
//! separator. The link rwv surfaces for `docs/a\b.md` sits in `docs/`, and
//! every later pass that reads a link's weave-relative name back — the
//! undeclared-link sweep, the activation switch — must recognize it as that
//! declaration's link rather than as a link at `docs/a/b.md`.
//!
//! Each case carries a plain-named control beside the subject, so a pass that
//! stopped running altogether cannot satisfy it.

#![cfg(unix)]

use std::path::{Path, PathBuf};

mod common;

const BACKSLASHED: &str = r"docs/a\b.md";
const PLAIN: &str = "SHARED.md";
const DROPPED: &str = "DROPPED.md";

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

fn rwv_output(cwd: &Path, args: &[&str]) -> String {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn write_manifest(project_dir: &Path, files: &[&str]) {
    let list = files
        .iter()
        .map(|f| format!("'{f}'"))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories]\n\n\
             [integrations.static-files]\nenabled = true\nfiles = [{list}]\n\n\
             [integrations.vscode-workspace]\nenabled = false\n"
        ),
    )
    .unwrap();
}

fn project(ws: &Path, name: &str, files: &[&str]) -> PathBuf {
    let project_dir = ws.join("projects").join(name);
    std::fs::create_dir_all(&project_dir).unwrap();
    write_manifest(&project_dir, files);
    for file in files {
        let path = project_dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("{file}\n")).unwrap();
    }
    git_init_with_commit(&project_dir);
    project_dir
}

fn is_link(ws: &Path, name: &str) -> bool {
    ws.join(name)
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

fn activate(ws: &Path, project: &str) -> String {
    rwv_output(ws, &["activate", project, "--no-materialize"])
}

#[test]
fn the_backslashed_link_is_surfaced_under_its_declared_name() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    project(&ws, "alpha", &[BACKSLASHED, PLAIN]);

    let out = activate(&ws, "alpha");

    assert!(is_link(&ws, BACKSLASHED), "{out}");
    assert!(
        !ws.join("docs/a").exists(),
        "the `\\` was read as a separator:\n{out}"
    );
}

#[test]
fn a_declared_backslashed_link_is_not_an_undeclared_one() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let alpha = project(&ws, "alpha", &[BACKSLASHED, PLAIN, DROPPED]);
    let out = activate(&ws, "alpha");
    assert!(is_link(&ws, DROPPED), "fixture: {out}");
    write_manifest(&alpha, &[BACKSLASHED, PLAIN]);

    let doctor = rwv_output(&ws, &["doctor"]);
    assert!(
        doctor.contains(DROPPED),
        "control: the undeclared-link check did not report the dropped name:\n{doctor}"
    );
    assert!(
        !doctor.contains(BACKSLASHED),
        "the declared `{BACKSLASHED}` was reported as undeclared:\n{doctor}"
    );

    let swept = rwv_output(&ws, &["materialize", "--remove-undeclared-links"]);
    assert!(
        !is_link(&ws, DROPPED),
        "control: the sweep did not remove the dropped name:\n{swept}"
    );
    assert!(
        is_link(&ws, BACKSLASHED),
        "the sweep removed the declared `{BACKSLASHED}`:\n{swept}"
    );
}

#[test]
fn switching_projects_unlinks_the_backslashed_link() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    project(&ws, "alpha", &[BACKSLASHED, PLAIN]);
    project(&ws, "beta", &["OTHER.md"]);
    let out = activate(&ws, "alpha");
    assert!(is_link(&ws, BACKSLASHED), "fixture: {out}");

    let out = activate(&ws, "beta");

    assert!(
        is_link(&ws, "OTHER.md") && !is_link(&ws, PLAIN),
        "control: the switch did not replace alpha's links with beta's:\n{out}"
    );
    assert!(
        !is_link(&ws, BACKSLASHED),
        "alpha's `{BACKSLASHED}` survived the switch to beta:\n{out}"
    );
}
