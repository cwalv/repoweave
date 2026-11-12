//! Uncommitted edits to project-repo files rwv does not write: a verb that
//! commits leaves them out of its commit and as they are, and a verb that
//! moves the project repo refuses only where the move would overwrite one.

use std::path::{Path, PathBuf};

mod common;

use common::{git_in, rwv, PushWorkspace};

const APP: &str = "github/acme/app";
const LIB: &str = "github/acme/lib";

fn project(w: &PushWorkspace) -> PathBuf {
    w.workspace.join("projects").join(&w.project_name)
}

fn run(w: &PushWorkspace, args: &[&str]) -> std::process::Output {
    rwv()
        .args(args)
        .current_dir(&w.workspace)
        .output()
        .expect("rwv runs")
}

fn both(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    git_in(dir, &["add", file]);
    git_in(dir, &["commit", "-qm", file]);
    git_in(dir, &["rev-parse", "HEAD"])
}

/// A workspace whose project repo tracks two files rwv does not write, one
/// edited and one edited and staged.
fn with_edits() -> PushWorkspace {
    let w = common::build_workspace("proj", &[(APP, "owned"), (LIB, "owned")]);
    let proj = project(&w);
    commit_in(&proj, "NOTES.md");
    commit_in(&proj, "STAGED.md");
    std::fs::write(proj.join("NOTES.md"), "edited\n").unwrap();
    std::fs::write(proj.join("STAGED.md"), "edited and staged\n").unwrap();
    git_in(&proj, &["add", "STAGED.md"]);
    w
}

fn head(w: &PushWorkspace) -> String {
    git_in(project(w), &["rev-parse", "HEAD"])
}

fn assert_edits_left(w: &PushWorkspace, before: &str, verb: &str, out: &std::process::Output) {
    let text = both(out);
    assert!(out.status.success(), "{verb}:\n{text}");
    let proj = project(w);
    assert_ne!(head(w), before, "{verb} made no commit:\n{text}");
    let committed = git_in(&proj, &["show", "--name-only", "--format=", "HEAD"]);
    assert!(
        !committed.contains("NOTES.md") && !committed.contains("STAGED.md"),
        "{verb} committed an edit it does not own:\n{committed}"
    );
    let unstaged = git_in(&proj, &["diff", "--name-only"]);
    let staged = git_in(&proj, &["diff", "--cached", "--name-only"]);
    assert!(unstaged.contains("NOTES.md"), "{verb}:\n{unstaged}");
    assert_eq!(staged, "STAGED.md", "{verb}");
    assert!(
        text.contains("projects/proj: uncommitted files left as they are: NOTES.md (edited), STAGED.md (staged)"),
        "{verb}:\n{text}"
    );
}

#[test]
fn lock_commit_leaves_unrelated_edits_out_of_its_commit() {
    let w = with_edits();
    commit_in(&w.workspace.join(APP), "app-work");
    let before = head(&w);

    let out = run(&w, &["lock", "--commit"]);

    assert_edits_left(&w, &before, "lock --commit", &out);
}

#[test]
fn a_setter_leaves_unrelated_edits_out_of_its_commit() {
    let w = with_edits();
    let before = head(&w);

    let out = run(&w, &["member", "set-role", LIB, "dependency"]);

    assert_edits_left(&w, &before, "member set-role", &out);
}

#[test]
fn member_pin_commit_leaves_unrelated_edits_out_of_its_commit() {
    let w = with_edits();
    commit_in(&w.workspace.join(APP), "app-work");
    let before = head(&w);

    let out = run(&w, &["member", "pin", APP, "HEAD", "--commit"]);

    assert_edits_left(&w, &before, "member pin --commit", &out);
}

#[test]
fn update_remote_commit_leaves_unrelated_edits_out_of_its_commit() {
    let w = with_edits();
    let app = w.workspace.join(APP);
    commit_in(&app, "app-ahead");
    git_in(&app, &["push", "-q", "origin", "main"]);
    git_in(&app, &["reset", "-q", "--hard", "HEAD~1"]);
    let before = head(&w);

    let out = run(&w, &["update", "--remote", "--commit"]);

    assert_edits_left(&w, &before, "update --remote --commit", &out);
}

#[test]
fn member_remove_leaves_unrelated_edits_out_of_its_commit() {
    let w = with_edits();
    let before = head(&w);

    let out = run(&w, &["member", "remove", LIB]);

    assert_edits_left(&w, &before, "member remove", &out);
}

#[test]
fn member_add_leaves_unrelated_edits_out_of_its_commit() {
    let w = with_edits();
    let bare = w._tmp.path().join("extra.git");
    common::init_bare_repo_with_commit(&bare);
    let before = head(&w);

    let out = run(&w, &["member", "add", &common::file_url(&bare)]);

    assert_edits_left(&w, &before, "member add", &out);
}

/// The project repo's remote gains a commit changing `file`.
fn published_change_to(w: &PushWorkspace, file: &str) -> String {
    let seed = w._tmp.path().join(format!("forge-{file}"));
    git_in(
        w._tmp.path(),
        &[
            "clone",
            "-q",
            w.project_bare.to_str().unwrap(),
            seed.to_str().unwrap(),
        ],
    );
    git_in(&seed, &["config", "user.email", "t@t"]);
    git_in(&seed, &["config", "user.name", "T"]);
    std::fs::write(seed.join(file), "theirs\n").unwrap();
    git_in(&seed, &["add", file]);
    git_in(&seed, &["commit", "-qm", file]);
    git_in(&seed, &["push", "-q", "origin", "main"]);
    git_in(&seed, &["rev-parse", "HEAD"])
}

fn published_edits() -> PushWorkspace {
    let w = common::build_workspace("proj", &[(APP, "owned"), (LIB, "owned")]);
    let proj = project(&w);
    commit_in(&proj, "NOTES.md");
    git_in(&proj, &["push", "-q", "origin", "main"]);
    std::fs::write(proj.join("NOTES.md"), "edited\n").unwrap();
    w
}

#[test]
fn pull_refuses_before_anything_moves_where_the_take_in_would_overwrite_an_edited_file() {
    let w = published_edits();
    published_change_to(&w, "NOTES.md");
    let before = head(&w);

    let out = run(&w, &["pull"]);
    let text = both(&out);

    assert!(!out.status.success(), "{text}");
    assert!(text.contains("projects/proj: edits dirty"), "{text}");
    assert!(text.contains("NOTES.md"), "{text}");
    assert_eq!(head(&w), before, "the refusal came after the project moved");
    assert_eq!(
        std::fs::read_to_string(project(&w).join("NOTES.md")).unwrap(),
        "edited\n"
    );
}

#[test]
fn pull_takes_in_a_change_that_touches_no_edited_file_and_reports_the_edits() {
    let w = published_edits();
    let theirs = published_change_to(&w, "OTHER.md");

    let out = run(&w, &["pull"]);
    let text = both(&out);

    assert!(out.status.success(), "{text}");
    assert_eq!(head(&w), theirs, "{text}");
    assert_eq!(
        std::fs::read_to_string(project(&w).join("NOTES.md")).unwrap(),
        "edited\n"
    );
    assert!(
        text.contains("projects/proj: uncommitted files left as they are: NOTES.md (edited)"),
        "{text}"
    );
}
