//! A member branch some live weave branch declares is never deleted or
//! rewritten by a verb that was asked to: the verbs refuse, name the declaring
//! branches, and change nothing.

use std::path::{Path, PathBuf};

mod common;

const PROJECT: &str = "p";
const LIB: &str = "github/o/lib";

fn project_dir(ws: &Path) -> PathBuf {
    ws.join("projects").join(PROJECT)
}

fn run(args: &[&str], dir: &Path) -> std::process::Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn succeed(args: &[&str], dir: &Path) {
    let out = run(args, dir);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn refuse(args: &[&str], dir: &Path) -> String {
    let out = run(args, dir);
    assert!(!out.status.success(), "{args:?} should refuse");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn declare_lib_on(ws: &Path, line: &str, branch: &str) {
    let project = project_dir(ws);
    common::git_in(&project, &["switch", "-qc", line]);
    let manifest = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    let at = manifest.find(LIB).unwrap();
    let (head, tail) = manifest.split_at(at);
    let tail = tail.replacen("branch = \"main\"", &format!("branch = \"{branch}\""), 1);
    std::fs::write(project.join("rwv.toml"), format!("{head}{tail}")).unwrap();
    common::git_in(&project, &["commit", "-qam", "declare"]);
    common::git_in(&project, &["switch", "-q", "main"]);
    succeed(&["branch", "set-mode", line, "declared"], ws);
}

fn seat_with_declared_branch() -> (common::PushWorkspace, PathBuf, String) {
    let w = common::build_workspace(PROJECT, &[(LIB, "owned")]);
    succeed(
        &["workweave", "create", "--project", PROJECT, "anon"],
        &w.workspace,
    );
    let ww = w
        .workspace
        .parent()
        .unwrap()
        .join(".workweaves")
        .join("p--anon");
    let branch = "p--anon".to_owned();
    declare_lib_on(&w.workspace, "line", &branch);
    (w, ww, branch)
}

#[test]
fn delete_remote_refuses_a_member_branch_a_live_weave_branch_declares_and_changes_nothing() {
    let (w, ww, branch) = seat_with_declared_branch();

    let text = refuse(
        &[
            "workweave",
            "delete",
            "anon",
            "--project",
            PROJECT,
            "--delete-remote",
        ],
        &w.workspace,
    );

    assert!(text.contains("protected declared"), "{text}");
    assert!(text.contains("line (projects/p)"), "{text}");
    assert!(ww.exists(), "the workweave was removed");
    assert!(
        !common::git_in(w.workspace.join(LIB), &["branch", "--list", &branch]).is_empty(),
        "the declared branch was deleted"
    );
}

fn commit_in(dir: &Path, file: &str) {
    std::fs::write(dir.join(file), file).unwrap();
    common::git_in(dir, &["add", "."]);
    common::git_in(dir, &["commit", "-qm", file]);
}

#[test]
fn a_rebase_refuses_a_member_on_a_declared_branch_with_commits_to_replay_and_moves_nothing() {
    let w = common::build_workspace(PROJECT, &[(LIB, "owned")]);
    let project = project_dir(&w.workspace);
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    common::git_in(&project, &["add", ".gitattributes"]);
    common::git_in(&project, &["commit", "-qm", "replay exclusion"]);
    succeed(
        &["workweave", "create", "--project", PROJECT, "anon"],
        &w.workspace,
    );
    let ww = w
        .workspace
        .parent()
        .unwrap()
        .join(".workweaves")
        .join("p--anon");
    commit_in(&ww.join(LIB), "theirs");
    succeed(&["lock", "--commit"], &ww);
    commit_in(&w.workspace.join(LIB), "ours");
    succeed(&["lock", "--commit"], &w.workspace);
    let before = common::git_in(w.workspace.join(LIB), &["rev-parse", "HEAD"]);

    let text = refuse(&["sync", "--strategy", "rebase"], &ww);

    assert!(text.contains("protected declared"), "{text}");
    assert_eq!(
        common::git_in(w.workspace.join(LIB), &["rev-parse", "HEAD"]),
        before
    );
}
