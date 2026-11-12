//! `rwv lock --commit` over a hand-edited `rwv.toml`: every refusal is judged
//! before anything is written, each changed entry runs its setter's step, and
//! the rest of the file is committed as it stands.

use std::path::{Path, PathBuf};

use crate::common;

use common::{git_in, rwv, PushWorkspace};

const APP: &str = "github/acme/app";
const LIB: &str = "github/acme/lib";

fn two_members() -> PushWorkspace {
    common::build_workspace("proj", &[(APP, "owned"), (LIB, "owned")])
}

fn member(w: &PushWorkspace, repo: &str) -> PathBuf {
    w.workspace.join(repo)
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

fn lock_text(w: &PushWorkspace) -> String {
    std::fs::read_to_string(project(w).join("rwv.lock")).unwrap()
}

fn pin_of(w: &PushWorkspace, repo: &str) -> Option<String> {
    let lock: serde_json::Value = serde_json::from_str(&lock_text(w)).unwrap();
    lock["repositories"][repo]["version"]
        .as_str()
        .map(str::to_owned)
}

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    git_in(dir, &["add", file]);
    git_in(dir, &["commit", "-qm", file]);
    git_in(dir, &["rev-parse", "HEAD"])
}

fn hand_declare_lib_on(w: &PushWorkspace, branch: &str) {
    let edited = manifest(w);
    let lib_section = edited.find(&format!("[repositories.\"{LIB}\"]")).unwrap();
    let (head, tail) = edited.split_at(lib_section);
    let tail = tail.replacen("branch = \"main\"", &format!("branch = \"{branch}\""), 1);
    std::fs::write(project(w).join("rwv.toml"), format!("{head}{tail}")).unwrap();
}

fn lib_with_dev(w: &PushWorkspace) -> String {
    let lib = member(w, LIB);
    git_in(&lib, &["switch", "-q", "-c", "dev"]);
    let tip = commit_in(&lib, "dev-one");
    git_in(&lib, &["push", "-q", "origin", "dev"]);
    git_in(&lib, &["switch", "-q", "main"]);
    tip
}

#[test]
fn every_refusal_is_judged_before_anything_is_written() {
    let w = two_members();
    hand_declare_lib_on(&w, "nowhere");
    std::fs::write(project(&w).join("README"), "tracked\n").unwrap();
    git_in(project(&w), &["add", "README"]);
    git_in(project(&w), &["commit", "-qm", "readme"]);
    std::fs::write(project(&w).join("README"), "dirty\n").unwrap();
    commit_in(&member(&w, APP), "app-ahead");
    let before = (commits(&w), manifest(&w), lock_text(&w));

    let text = refused(&run(&w, &["lock", "--commit"]));

    assert!(text.contains("name no-such-branch (nowhere)"), "{text}");
    assert!(!text.contains("projects/proj: edits"), "{text}");
    assert!(
        text.contains("rwv fetch") && text.contains("--new"),
        "the refusal names the fetch and the creating spelling:\n{text}"
    );
    assert_eq!(
        (commits(&w), manifest(&w), lock_text(&w)),
        before,
        "a refusal came after a write"
    );
    common::assert_on_branch(&member(&w, LIB), "main");
}

#[test]
fn a_hand_declared_branch_runs_the_setters_step_and_commits_the_whole_file_once() {
    let w = two_members();
    let tip = lib_with_dev(&w);
    hand_declare_lib_on(&w, "dev");
    let hand = format!(
        "{}\n[integrations.vscode-workspace]\nenabled = false\n",
        manifest(&w)
    );
    std::fs::write(project(&w).join("rwv.toml"), hand).unwrap();
    let before = commits(&w);

    let text = succeeded(&run(&w, &["lock", "--commit"]));

    assert_eq!(
        commits(&w),
        before + 1,
        "one commit carries the whole file:\n{text}"
    );
    assert_eq!(pin_of(&w, LIB).as_deref(), Some(tip.as_str()));
    common::assert_on_branch(&member(&w, LIB), "dev");
    let committed = git_in(project(&w), &["show", "HEAD:rwv.toml"]);
    assert!(committed.contains("branch = \"dev\""), "{committed}");
    assert!(committed.contains("vscode-workspace"), "{committed}");
    assert_eq!(
        git_in(
            project(&w),
            &["status", "--porcelain", "--untracked-files=no"]
        ),
        ""
    );
    assert!(
        text.contains(&format!("{LIB}: branch main -> dev")),
        "{text}"
    );
}

#[test]
fn a_hand_removed_member_is_committed_removed_and_its_checkout_is_kept() {
    let w = two_members();
    let edited = {
        let text = manifest(&w);
        let at = text.find(&format!("[repositories.\"{LIB}\"]")).unwrap();
        text[..at].to_owned()
    };
    std::fs::write(project(&w).join("rwv.toml"), edited).unwrap();

    let text = succeeded(&run(&w, &["lock", "--commit"]));

    assert!(!git_in(project(&w), &["show", "HEAD:rwv.toml"]).contains(LIB));
    assert!(!git_in(project(&w), &["show", "HEAD:rwv.lock"]).contains(LIB));
    assert!(
        member(&w, LIB).join(".git").exists(),
        "the checkout was deleted"
    );
    assert!(
        text.contains(&format!("{LIB}: a member removed by hand")),
        "{text}"
    );
}

#[test]
fn a_whole_file_commit_leaves_other_project_edits_out_of_its_commit() {
    let w = two_members();
    let hand = format!(
        "{}\n[integrations.vscode-workspace]\nenabled = false\n",
        manifest(&w)
    );
    std::fs::write(project(&w).join("rwv.toml"), hand).unwrap();
    std::fs::write(project(&w).join("README"), "tracked\n").unwrap();
    git_in(project(&w), &["add", "README"]);
    git_in(project(&w), &["commit", "-qm", "readme"]);
    std::fs::write(project(&w).join("README"), "dirty\n").unwrap();
    commit_in(&member(&w, APP), "app-ahead");
    let before = commits(&w);

    let text = succeeded(&run(&w, &["lock", "--commit"]));

    assert_eq!(commits(&w), before + 1, "{text}");
    assert!(text.contains("README (edited)"), "{text}");
    let committed = git_in(project(&w), &["show", "--name-only", "--format=", "HEAD"]);
    assert!(!committed.contains("README"), "{committed}");
    assert!(git_in(project(&w), &["diff", "--name-only"]).contains("README"));
}
