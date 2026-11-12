//! A member branch some live weave branch declares is never deleted or
//! rewritten by a verb that was asked to: the verbs refuse, name the declaring
//! branches, and change nothing.

use std::path::{Path, PathBuf};

use crate::common;

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

    let text = common::cli::refused(
        &run(
            &[
                "workweave",
                "delete",
                "anon",
                "--project",
                PROJECT,
                "--delete-remote",
            ],
            &w.workspace,
        ),
        repoweave::diagnosis::State::ProtectedDeclared,
    );

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
fn a_sync_of_a_feature_whose_member_branch_another_branch_declares_is_refused_and_moves_nothing() {
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
    commit_in(&ww.join(LIB), "theirs");
    succeed(&["lock", "--commit"], &ww);
    commit_in(&w.workspace.join(LIB), "ours");
    succeed(&["lock", "--commit"], &w.workspace);
    declare_lib_on(&w.workspace, "line", "p--anon");
    let before = common::git_in(ww.join(LIB), &["rev-parse", "HEAD"]);

    let out = run(&["sync", "--history", "linear"], &ww);
    let text = common::cli::refused(&out, repoweave::diagnosis::State::NameDeclared);

    assert!(text.contains("line (projects/p)"), "{text}");
    assert_eq!(common::git_in(ww.join(LIB), &["rev-parse", "HEAD"]), before);
}

/// A landing rebases only the feature's own member branches, so the Names
/// rule, refusing a feature whose member branch a live weave branch declares,
/// is what keeps it from rewriting a declared branch. Declaring the branch,
/// as printed, makes it one no landing takes.
#[test]
fn a_landing_that_would_rebase_a_member_branch_another_branch_declares_is_refused_and_moves_nothing(
) {
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
    commit_in(&ww.join(LIB), "theirs");
    succeed(&["lock", "--commit"], &ww);
    commit_in(&w.workspace.join(LIB), "ours");
    succeed(&["lock", "--commit"], &w.workspace);
    declare_lib_on(&w.workspace, "line", "p--anon");
    let before = common::git_in(ww.join(LIB), &["rev-parse", "HEAD"]);
    let target_before = common::git_in(w.workspace.join(LIB), &["rev-parse", "HEAD"]);

    let text = common::cli::refused(
        &run(&["land"], &ww),
        repoweave::diagnosis::State::NameDeclared,
    );

    assert!(text.contains("line (projects/p)"), "{text}");
    assert!(text.contains("rwv branch rename <new>"), "{text}");
    assert_eq!(common::git_in(ww.join(LIB), &["rev-parse", "HEAD"]), before);
    assert_eq!(
        common::git_in(w.workspace.join(LIB), &["rev-parse", "HEAD"]),
        target_before
    );

    succeed(&["branch", "set-mode", "p--anon", "declared"], &ww);
    common::cli::refused(
        &run(&["land"], &ww),
        repoweave::diagnosis::State::TargetNoTarget,
    );
    assert_eq!(common::git_in(ww.join(LIB), &["rev-parse", "HEAD"]), before);
}

/// The rename `name` declared prints gives the feature its own name: the
/// declared branch stays where it stands, the member takes the new name at
/// its tip, and the landing the state refused goes through.
#[test]
fn the_printed_rename_of_a_feature_whose_member_branch_is_declared_clears_the_state() {
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
    commit_in(&ww.join(LIB), "theirs");
    succeed(&["lock", "--commit"], &ww);
    declare_lib_on(&w.workspace, "line", "p--anon");
    let declared_before = common::git_in(ww.join(LIB), &["rev-parse", "p--anon"]);

    let text = common::cli::refused(
        &run(&["land"], &ww),
        repoweave::diagnosis::State::NameDeclared,
    );
    let printed = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("rwv branch rename "))
        .unwrap_or_else(|| panic!("no rename printed:\n{text}"));
    let args: Vec<&str> = printed
        .split_whitespace()
        .skip(1)
        .map(|word| if word == "<new>" { "renamed" } else { word })
        .collect();
    succeed(&args, &ww);

    assert_eq!(
        common::git_in(ww.join(LIB), &["rev-parse", "p--anon"]),
        declared_before,
        "the declared branch moved"
    );
    assert_eq!(
        common::git_in(ww.join(LIB), &["symbolic-ref", "--short", "HEAD"]),
        "renamed"
    );
    assert_eq!(
        common::git_in(ww.join(LIB), &["rev-parse", "HEAD"]),
        declared_before
    );
    assert_eq!(
        common::git_in(project_dir(&ww), &["symbolic-ref", "--short", "HEAD"]),
        "renamed"
    );
    succeed(&["land"], &ww);
    assert_eq!(
        common::git_in(w.workspace.join(LIB), &["rev-parse", "main"]),
        declared_before,
        "the feature's work did not land"
    );
}
