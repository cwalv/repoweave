//! `rwv doctor` reports a member checkout that is not where its workspace
//! expects it, as `alignment`, judged by the engine `rwv update` aligns with.
//!
//! Pinned from both sides: a checkout the engine would leave where it is is a
//! finding, and one it puts in its expected state is none, whatever the role.

use std::path::{Path, PathBuf};

mod common;

const PROJECT: &str = "p";
const OWNED: &str = "github/o/owned";
const DEPENDENCY: &str = "github/o/dependency";

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn weave() -> common::PushWorkspace {
    common::build_workspace(PROJECT, &[(OWNED, "owned"), (DEPENDENCY, "dependency")])
}

fn workweave(w: &common::PushWorkspace) -> PathBuf {
    rwv()
        .args(["workweave", "create", "--project", PROJECT, "feat"])
        .current_dir(&w.workspace)
        .assert()
        .success();
    w.workspace
        .parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{PROJECT}--feat"))
}

fn commit(repo: &Path, name: &str) {
    std::fs::write(repo.join(name), "x\n").unwrap();
    common::git_in(repo, &["add", name]);
    common::git_in(repo, &["commit", "-m", name]);
}

/// The `alignment` records `rwv doctor --json` reports, as `(path, sub_kind)`.
fn alignment(dir: &Path) -> Vec<(String, String)> {
    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(dir)
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "alignment")
        .map(|v| {
            (
                v["repo_path"].as_str().unwrap().to_owned(),
                v["sub_kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn about(findings: &[(String, String)], member: &str) -> Vec<String> {
    findings
        .iter()
        .filter(|(path, _)| path.ends_with(member))
        .map(|(_, sub_kind)| sub_kind.clone())
        .collect()
}

#[test]
fn a_member_in_its_expected_state_is_no_finding_whatever_its_role() {
    let w = weave();
    let ww = workweave(&w);

    let findings = alignment(&ww);
    assert!(
        findings.is_empty(),
        "the owned member is on the workweave's branch and the dependency detached at its pin: \
         {findings:?}"
    );
}

#[test]
fn a_member_on_a_branch_whose_commits_other_refs_hold_is_on_branch() {
    let w = weave();
    let ww = workweave(&w);
    common::git_in(&ww.join(OWNED), &["switch", "-c", "wip"]);

    assert_eq!(
        about(&alignment(&ww), OWNED),
        ["on-branch"],
        "the commits it holds are the workweave branch's too"
    );
}

#[test]
fn a_member_on_a_branch_holding_commits_nothing_else_does_lists_them() {
    let w = weave();
    let ww = workweave(&w);
    common::git_in(&ww.join(OWNED), &["switch", "-c", "wip"]);
    commit(&ww.join(OWNED), "stray.txt");

    let findings = alignment(&ww);
    assert_eq!(about(&findings, OWNED), ["on-branch-with-stray-commits"]);

    let text = rwv().arg("doctor").current_dir(&ww).output().unwrap();
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("stray.txt"),
        "the finding names the commit; got:\n{text}"
    );
}

#[test]
fn a_detached_member_holding_unpushed_commits_is_reported_with_them() {
    let w = weave();
    let ww = workweave(&w);
    common::git_in(&ww.join(OWNED), &["switch", "--detach"]);
    commit(&ww.join(OWNED), "loose.txt");

    assert_eq!(
        about(&alignment(&ww), OWNED),
        ["detached-with-unpushed-commits"]
    );
}

#[test]
fn a_detached_dependency_at_its_pin_is_not_reported() {
    let w = weave();
    let ww = workweave(&w);
    assert_eq!(common::checkout_ref(&ww.join(DEPENDENCY)), None);

    assert!(about(&alignment(&ww), DEPENDENCY).is_empty());
}

#[test]
fn a_checkout_stopped_in_an_operation_of_its_own_is_left_alone() {
    let w = weave();
    let ww = workweave(&w);
    common::git_in(&ww.join(OWNED), &["switch", "-c", "wip"]);
    commit(&ww.join(OWNED), "stray.txt");
    common::git_in(&ww.join(OWNED), &["bisect", "start"]);

    assert!(
        about(&alignment(&ww), OWNED).is_empty(),
        "git refuses to switch branches mid-bisect, and moving HEAD abandons it"
    );
}

#[test]
fn a_project_repo_on_no_branch_is_reported_in_the_primary() {
    let w = weave();
    common::git_in(
        &w.workspace.join("projects").join(PROJECT),
        &["switch", "--detach"],
    );

    let findings = alignment(&w.workspace);
    assert_eq!(
        findings
            .iter()
            .filter(|(path, _)| path.ends_with(&format!("projects/{PROJECT}")))
            .map(|(_, sub_kind)| sub_kind.as_str())
            .collect::<Vec<_>>(),
        ["project-detached"]
    );
}
