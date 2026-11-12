//! Each member verb commits its own change and nothing else, and `member add`
//! of a registered member refuses a flag that would change nothing.

use std::path::PathBuf;

mod common;

use common::{git_in, rwv, PushWorkspace};

const APP: &str = "github/acme/app";
const LIB: &str = "github/acme/lib";

fn two_members() -> PushWorkspace {
    common::build_workspace("proj", &[(APP, "owned"), (LIB, "owned")])
}

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

fn succeeded(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "expected success:\n{text}");
    text
}

fn refused(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "expected a refusal:\n{text}");
    text
}

fn commits(w: &PushWorkspace) -> usize {
    git_in(project(w), &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap()
}

fn manifest(w: &PushWorkspace) -> String {
    std::fs::read_to_string(project(w).join("rwv.toml")).unwrap()
}

fn committed_manifest(w: &PushWorkspace) -> String {
    git_in(project(w), &["show", "HEAD:rwv.toml"])
}

fn hand_edit_app_role(w: &PushWorkspace) {
    let edited = manifest(w).replace(
        &format!("branch = \"main\"\nrole = \"owned\"\n[repositories.\"{LIB}\"]"),
        &format!("branch = \"main\"\nrole = \"dependency\"\n[repositories.\"{LIB}\"]"),
    );
    assert_ne!(edited, manifest(w), "the hand edit applies");
    std::fs::write(project(w).join("rwv.toml"), edited).unwrap();
}

#[test]
fn set_role_commits_the_role_and_leaves_another_hand_edit_of_the_file_uncommitted() {
    let w = two_members();
    hand_edit_app_role(&w);
    let before = commits(&w);

    succeeded(&run(&w, &["member", "set-role", LIB, "fork"]));

    assert_eq!(commits(&w), before + 1);
    let committed = committed_manifest(&w);
    assert!(committed.contains("role = \"fork\""), "{committed}");
    assert!(
        !committed.contains("role = \"dependency\""),
        "the hand edit of app's role was committed:\n{committed}"
    );
    assert!(manifest(&w).contains("role = \"dependency\""));
    let status = git_in(project(&w), &["status", "--porcelain"]);
    assert!(
        status.contains("M rwv.toml"),
        "the hand edit stays as an uncommitted change:\n{status}"
    );
}

#[test]
fn set_recurse_submodules_commits_the_setting() {
    let w = two_members();
    let before = commits(&w);

    succeeded(&run(
        &w,
        &["member", "set-recurse-submodules", LIB, "false"],
    ));

    assert_eq!(commits(&w), before + 1);
    assert!(committed_manifest(&w).contains("recurse-submodules = false"));
    assert_eq!(git_in(project(&w), &["status", "--porcelain"]), "");
}

#[test]
fn remove_commits_the_removal_and_the_dropped_pin() {
    let w = two_members();
    let before = commits(&w);

    succeeded(&run(&w, &["member", "remove", LIB]));

    assert_eq!(commits(&w), before + 1);
    assert!(!committed_manifest(&w).contains(LIB));
    let lock = git_in(project(&w), &["show", "HEAD:rwv.lock"]);
    assert!(!lock.contains(LIB), "{lock}");
    assert!(lock.contains(APP), "{lock}");
    assert_eq!(git_in(project(&w), &["status", "--porcelain"]), "");
}

#[test]
fn a_member_verb_refuses_unrelated_project_edits_before_it_changes_anything() {
    let w = two_members();
    std::fs::write(project(&w).join("README"), "edited\n").unwrap();
    git_in(project(&w), &["add", "README"]);
    git_in(project(&w), &["commit", "-qm", "readme"]);
    std::fs::write(project(&w).join("README"), "dirty\n").unwrap();
    let before = (commits(&w), manifest(&w));

    for args in [
        &["member", "set-role", LIB, "fork"][..],
        &["member", "set-recurse-submodules", LIB, "false"][..],
        &["member", "remove", LIB][..],
    ] {
        let text = refused(&run(&w, args));
        assert!(text.contains("edits dirty"), "{args:?}: {text}");
        assert_eq!(
            (commits(&w), manifest(&w)),
            before,
            "{args:?} changed something"
        );
    }
}

#[test]
fn add_of_a_registered_member_refuses_a_flag_that_disagrees_and_names_the_setter() {
    let w = two_members();
    let clone = w.workspace.join(LIB);
    let clone = clone.to_str().unwrap();
    let before = (commits(&w), manifest(&w));

    let text = refused(&run(
        &w,
        &["member", "add", "--adopt", clone, "--role", "fork"],
    ));
    assert!(text.contains("rwv member set-role"), "{text}");
    let text = refused(&run(
        &w,
        &["member", "add", "--adopt", clone, "--no-recurse-submodules"],
    ));
    assert!(text.contains("rwv member set-recurse-submodules"), "{text}");
    assert_eq!((commits(&w), manifest(&w)), before);
}
