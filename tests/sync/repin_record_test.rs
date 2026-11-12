//! The `Rwv-Repin` record a lock commit carries: every verb that re-pins a
//! member by name writes one per member whose pin it moves, naming the
//! member, the old pin, the new pin and the verb; a relock writes none.

use std::path::{Path, PathBuf};

use crate::common;

use common::{git_in, rwv, PushWorkspace};

const PROJECT: &str = "p";
const APP: &str = "github/acme/app";
const LIB: &str = "github/acme/lib";

fn weave() -> PushWorkspace {
    common::build_workspace(PROJECT, &[(APP, "owned"), (LIB, "owned")])
}

fn member(w: &PushWorkspace, repo: &str) -> PathBuf {
    w.workspace.join(repo)
}

fn project(w: &PushWorkspace) -> PathBuf {
    w.workspace.join("projects").join(PROJECT)
}

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    git_in(dir, &["add", file]);
    git_in(dir, &["commit", "-qm", file]);
    git_in(dir, &["rev-parse", "HEAD"])
}

fn succeed(w: &PushWorkspace, args: &[&str]) -> String {
    let out = rwv()
        .args(args)
        .current_dir(&w.workspace)
        .output()
        .expect("rwv runs");
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "`rwv {}` failed:\n{text}",
        args.join(" ")
    );
    text
}

/// The re-pin records the commit at `rev` of the repo at `dir` carries.
fn records_at(dir: &Path, rev: &str) -> Vec<String> {
    git_in(
        dir,
        &[
            "log",
            "-1",
            "--format=%(trailers:key=Rwv-Repin,valueonly,unfold)",
            rev,
        ],
    )
    .lines()
    .filter(|line| !line.is_empty())
    .map(str::to_owned)
    .collect()
}

fn records(w: &PushWorkspace) -> Vec<String> {
    records_at(&project(w), "HEAD")
}

fn record(repo: &str, from: &str, to: &str, verb: &str) -> String {
    format!("\"{repo}\" {from} {to} {verb}")
}

fn head(w: &PushWorkspace) -> String {
    git_in(project(w), &["rev-parse", "HEAD"])
}

/// `LIB` pinned at a commit one ahead of `dev`, which holds its first commit
/// and is published. Returns (dev's tip, the pin).
fn lib_ahead_of_dev(w: &PushWorkspace) -> (String, String) {
    let lib = member(w, LIB);
    let first = git_in(&lib, &["rev-parse", "HEAD"]);
    git_in(&lib, &["branch", "dev"]);
    git_in(&lib, &["push", "-q", "origin", "dev"]);
    let ahead = commit_in(&lib, "ahead");
    succeed(w, &["lock", "--commit"]);
    (first, ahead)
}

#[test]
fn member_pin_records_a_backward_and_a_forward_re_pin() {
    let w = weave();
    let (first, ahead) = lib_ahead_of_dev(&w);

    succeed(&w, &["member", "pin", LIB, &first, "--commit"]);
    assert_eq!(records(&w), [record(LIB, &ahead, &first, "member pin")]);

    succeed(&w, &["member", "pin", LIB, &ahead, "--commit"]);
    assert_eq!(records(&w), [record(LIB, &first, &ahead, "member pin")]);
}

#[test]
fn a_relock_records_no_re_pin_with_or_without_repo() {
    let w = weave();
    let lib = member(&w, LIB);
    let before = head(&w);
    commit_in(&lib, "one");
    succeed(&w, &["lock", "--commit"]);
    assert_ne!(head(&w), before, "the relock committed");
    assert_eq!(records(&w), Vec::<String>::new());

    let before = head(&w);
    commit_in(&lib, "two");
    succeed(&w, &["lock", "--repo", LIB, "--commit"]);
    assert_ne!(head(&w), before, "the relock committed");
    assert_eq!(records(&w), Vec::<String>::new());
}

/// A colleague's clone of `LIB` publishes a commit on `main`; returns it.
fn upstream_moves(w: &PushWorkspace) -> String {
    let (_, bare) = w.manifest_bares.iter().find(|(p, _)| p == LIB).unwrap();
    let other = w._tmp.path().join("colleague");
    if other.exists() {
        git_in(&other, &["pull", "-q", "--ff-only"]);
    } else {
        git_in(
            w._tmp.path(),
            &[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
    }
    let count = git_in(&other, &["rev-list", "--count", "HEAD"]);
    let theirs = commit_in(&other, &format!("theirs-{count}"));
    git_in(&other, &["push", "-q", "origin", "main"]);
    theirs
}

#[test]
fn update_remote_records_only_the_member_reset_to_upstream_names() {
    let w = weave();
    let before = head(&w);
    upstream_moves(&w);
    succeed(&w, &["update", "--remote", "--commit"]);
    assert_ne!(head(&w), before, "the update committed its lock");
    assert_eq!(records(&w), Vec::<String>::new());

    let pin = git_in(member(&w, LIB), &["rev-parse", "HEAD"]);
    commit_in(&member(&w, LIB), "unpinned-local");
    let theirs = upstream_moves(&w);
    succeed(
        &w,
        &["update", "--remote", "--reset-to-upstream", LIB, "--commit"],
    );
    assert_eq!(records(&w), [record(LIB, &pin, &theirs, "update --remote")]);
}

#[test]
fn set_branch_records_the_re_pin_it_makes() {
    let w = weave();
    let (first, ahead) = lib_ahead_of_dev(&w);

    succeed(&w, &["member", "set-branch", LIB, "dev"]);

    assert_eq!(
        records(&w),
        [record(LIB, &ahead, &first, "member set-branch")]
    );
}

#[test]
fn a_hand_edited_entry_lock_commit_converges_records_each_member_it_re_pins() {
    let w = weave();
    let app = member(&w, APP);
    let app_first = git_in(&app, &["rev-parse", "HEAD"]);
    git_in(&app, &["branch", "dev"]);
    git_in(&app, &["push", "-q", "origin", "dev"]);
    commit_in(&app, "app-ahead");
    let app_ahead = git_in(&app, &["rev-parse", "HEAD"]);
    let (lib_first, lib_ahead) = lib_ahead_of_dev(&w);
    let manifest = project(&w).join("rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        text.replace("branch = \"main\"", "branch = \"dev\""),
    )
    .unwrap();

    succeed(&w, &["lock", "--commit"]);

    assert_eq!(
        records(&w),
        [
            record(APP, &app_ahead, &app_first, "lock --commit"),
            record(LIB, &lib_ahead, &lib_first, "lock --commit"),
        ]
    );
}

#[test]
fn branch_create_and_workweave_create_record_the_set_branch_re_pin() {
    let w = weave();
    let (first, ahead) = lib_ahead_of_dev(&w);
    let set = format!("{LIB}=dev");

    succeed(
        &w,
        &[
            "branch",
            "create",
            "dec",
            "--declared",
            "--set-branch",
            &set,
        ],
    );
    assert_eq!(records(&w), [record(LIB, &ahead, &first, "branch create")]);

    git_in(project(&w), &["switch", "-q", "main"]);
    succeed(&w, &["update"]);
    succeed(
        &w,
        &[
            "workweave",
            "create",
            "--project",
            PROJECT,
            "ww",
            "--declared",
            "--set-branch",
            &set,
        ],
    );
    let seat = w
        .workspace
        .parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{PROJECT}--ww"))
        .join("projects")
        .join(PROJECT);
    assert_eq!(
        records_at(&seat, "HEAD"),
        [record(LIB, &ahead, &first, "workweave create")]
    );
}
