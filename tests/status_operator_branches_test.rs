//! `rwv status` lists, per member, each local branch holding unpushed commits
//! that nothing else accounts for, and no other.
//!
//! A branch is accounted for when it is checked out somewhere, is named after a
//! recorded weave branch, is a branch some weave branch declares for the
//! member, or holds nothing a remote-tracking branch lacks. Each of those four
//! has a test here that fails if the listing stops omitting it.

use std::path::{Path, PathBuf};

mod common;

const PROJECT: &str = "p";
const MEMBER: &str = "github/o/owned";

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn weave() -> common::PushWorkspace {
    common::build_workspace(PROJECT, &[(MEMBER, "owned")])
}

fn member(w: &common::PushWorkspace) -> PathBuf {
    w.workspace.join(MEMBER)
}

fn commit(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), "x\n").unwrap();
    common::git_in(repo, &["add", name]);
    common::git_in(repo, &["commit", "-m", name]);
}

fn status_text(dir: &Path) -> String {
    let out = rwv().arg("status").current_dir(dir).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn operator_branches(dir: &Path) -> Vec<serde_json::Value> {
    let out = rwv()
        .args(["status", "--json"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    doc["operator_branches"].as_array().unwrap().clone()
}

fn listed(dir: &Path) -> Vec<String> {
    operator_branches(dir)
        .iter()
        .map(|b| b["branch"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_branch_holding_unpushed_commits_is_listed_with_its_count_and_its_commits() {
    let w = weave();
    let m = member(&w);
    common::git_in(&m, &["switch", "-c", "wip"]);
    commit(&m, "one.txt");
    commit(&m, "two.txt");
    common::git_in(&m, &["switch", "main"]);

    assert!(
        status_text(&w.workspace).contains(&format!("operator branch {MEMBER}  wip  2 unpushed\n")),
        "{}",
        status_text(&w.workspace)
    );
    let found = operator_branches(&w.workspace);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0]["path"], MEMBER);
    assert_eq!(found[0]["unpushed"], 2);
    assert_eq!(found[0]["gone_upstream"], false);
    let subjects: Vec<&str> = found[0]["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert_eq!(subjects, ["two.txt", "one.txt"]);
}

#[test]
fn a_listed_branch_whose_upstream_is_gone_says_so() {
    let w = weave();
    let m = member(&w);
    common::git_in(&m, &["switch", "-c", "wip"]);
    commit(&m, "one.txt");
    common::git_in(&m, &["push", "-u", "origin", "wip"]);
    commit(&m, "two.txt");
    common::git_in(&m, &["push", "origin", "--delete", "wip"]);
    common::git_in(&m, &["fetch", "--prune", "origin"]);
    common::git_in(&m, &["switch", "main"]);

    assert!(
        status_text(&w.workspace).contains(&format!(
            "operator branch {MEMBER}  wip  2 unpushed (upstream gone)\n"
        )),
        "{}",
        status_text(&w.workspace)
    );
    assert_eq!(operator_branches(&w.workspace)[0]["gone_upstream"], true);
}

#[test]
fn a_branch_checked_out_somewhere_is_not_listed() {
    let w = weave();
    let m = member(&w);
    common::git_in(&m, &["switch", "-c", "wip"]);
    commit(&m, "one.txt");

    assert_eq!(listed(&w.workspace), Vec::<String>::new());
}

#[test]
fn a_branch_a_weave_branch_declares_for_the_member_is_not_listed() {
    let w = weave();
    let m = member(&w);
    commit(&m, "one.txt");
    common::git_in(&m, &["switch", "-c", "elsewhere"]);

    assert_eq!(
        common::git_in(&m, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "elsewhere"
    );
    assert_eq!(
        listed(&w.workspace),
        Vec::<String>::new(),
        "main holds an unpushed commit, is checked out nowhere, and is declared"
    );
}

#[test]
fn a_branch_named_after_a_recorded_weave_branch_is_not_listed() {
    let w = weave();
    rwv()
        .args(["workweave", "create", "--project", PROJECT, "feat"])
        .current_dir(&w.workspace)
        .assert()
        .success();
    let in_workweave = w
        .workspace
        .parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{PROJECT}--feat"))
        .join(MEMBER);
    commit(&in_workweave, "one.txt");
    common::git_in(&in_workweave, &["switch", "--detach"]);

    assert_eq!(
        listed(&w.workspace),
        Vec::<String>::new(),
        "{PROJECT}--feat holds an unpushed commit and is checked out nowhere"
    );
}

#[test]
fn a_branch_holding_nothing_a_remote_lacks_is_not_listed() {
    let w = weave();
    let m = member(&w);
    common::git_in(&m, &["branch", "parked", "origin/main"]);

    assert_eq!(listed(&w.workspace), Vec::<String>::new());
}
