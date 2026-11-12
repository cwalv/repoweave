//! Alignment: the expected state of every member, the workspace state that
//! remembers what it was last aligned for, and what `rwv update` moves.
//!
//! Each prohibition is pinned from the side that would break it:
//!
//! - workspace state is written only when it changes
//! - a corrupt state file reads as absent, is reported, and `update` restores it
//! - `dependency` and `reference` members never get the workweave's branch
//! - a member whose branch is checked out elsewhere is detached at its pin,
//!   naming where, and the branch is never moved
//! - `update` never writes the lock

use predicates::prelude::*;
use std::path::{Path, PathBuf};

mod common;

const PROJECT: &str = "p";
const OWNED: &str = "github/o/owned";
const DEPENDENCY: &str = "github/o/dependency";
const REFERENCE: &str = "github/o/reference";

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn weave(roles: &[(&str, &str)]) -> common::PushWorkspace {
    common::build_workspace(PROJECT, roles)
}

fn project_dir(ws: &Path) -> PathBuf {
    ws.join("projects").join(PROJECT)
}

fn state_file(project_dir: &Path) -> PathBuf {
    PathBuf::from(common::git_in(
        project_dir,
        &["rev-parse", "--absolute-git-dir"],
    ))
    .join("rwv-workspace-state")
}

fn create_workweave(ws: &Path, name: &str) -> PathBuf {
    rwv()
        .args(["workweave", "create", "--project", PROJECT, name])
        .current_dir(ws)
        .assert()
        .success();
    ws.parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{PROJECT}--{name}"))
}

fn branches(repo: &Path) -> Vec<String> {
    common::git_in(repo, &["branch", "--format=%(refname:short)"])
        .lines()
        .map(str::to_owned)
        .collect()
}

#[cfg(unix)]
fn identity(path: &Path) -> (u64, std::time::SystemTime) {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).unwrap();
    (meta.ino(), meta.modified().unwrap())
}

#[cfg(not(unix))]
fn identity(path: &Path) -> (u64, std::time::SystemTime) {
    (0, std::fs::metadata(path).unwrap().modified().unwrap())
}

#[test]
fn workspace_state_is_written_only_when_it_changes() {
    let w = weave(&[(OWNED, "owned")]);
    let state = state_file(&project_dir(&w.workspace));

    rwv()
        .arg("update")
        .current_dir(&w.workspace)
        .assert()
        .success();
    assert!(
        state.exists(),
        "an aligning verb records what it aligned for"
    );
    let first = identity(&state);

    std::thread::sleep(std::time::Duration::from_millis(30));
    rwv()
        .arg("update")
        .current_dir(&w.workspace)
        .assert()
        .success();
    assert_eq!(
        identity(&state),
        first,
        "aligning for the same branch by the same entries rewrites nothing"
    );

    common::git_in(project_dir(&w.workspace), &["switch", "-c", "other"]);
    common::record_feature(&project_dir(&w.workspace), "other", "main");
    std::thread::sleep(std::time::Duration::from_millis(30));
    rwv()
        .arg("update")
        .current_dir(&w.workspace)
        .assert()
        .success();
    assert_ne!(
        identity(&state),
        first,
        "aligning for another branch is a change"
    );
    assert!(std::fs::read_to_string(&state).unwrap().contains("other"));
}

#[test]
fn a_corrupt_workspace_state_reads_as_absent_is_reported_and_update_restores_it() {
    let w = weave(&[(OWNED, "owned")]);
    let state = state_file(&project_dir(&w.workspace));
    rwv()
        .arg("update")
        .current_dir(&w.workspace)
        .assert()
        .success();
    let sound = std::fs::read(&state).unwrap();

    std::fs::write(&state, &sound[..sound.len() / 2]).unwrap();

    rwv()
        .arg("update")
        .current_dir(&w.workspace)
        .assert()
        .success()
        .stderr(predicate::str::contains("workspace state"))
        .stderr(predicate::str::contains("reads as absent"));

    assert_eq!(
        std::fs::read(&state).unwrap(),
        sound,
        "update records the state again"
    );
}

#[test]
fn a_switch_that_replaces_a_corrupt_workspace_state_says_so() {
    let w = weave(&[(OWNED, "owned")]);
    let project = project_dir(&w.workspace);
    let state = state_file(&project);
    rwv()
        .arg("update")
        .current_dir(&w.workspace)
        .assert()
        .success();
    let sound = std::fs::read(&state).unwrap();
    std::fs::write(&state, &sound[..sound.len() / 2]).unwrap();
    let head = common::git_in(&project, &["rev-parse", "HEAD"]);

    rwv()
        .args(["switch", "--detach", &head])
        .current_dir(&w.workspace)
        .assert()
        .success()
        .stderr(predicate::str::contains("workspace state"))
        .stderr(predicate::str::contains("reads as absent"));
}

#[test]
fn dependency_and_reference_members_never_get_the_workweave_branch() {
    let w = weave(&[
        (OWNED, "owned"),
        (DEPENDENCY, "dependency"),
        (REFERENCE, "reference"),
    ]);
    let ww = create_workweave(&w.workspace, "feat");
    let minted = format!("{PROJECT}--feat");

    assert_eq!(
        common::checkout_ref(&ww.join(OWNED)).as_deref(),
        Some(minted.as_str())
    );
    for member in [DEPENDENCY, REFERENCE] {
        assert_eq!(
            common::checkout_ref(&ww.join(member)),
            None,
            "{member} sits detached at its pin"
        );
        assert!(
            !branches(&w.workspace.join(member)).contains(&minted),
            "{member} has no branch named for the workweave"
        );
    }

    rwv().arg("update").current_dir(&ww).assert().success();
    for member in [DEPENDENCY, REFERENCE] {
        assert!(
            !branches(&w.workspace.join(member)).contains(&minted),
            "{member} still has no branch named for the workweave after update"
        );
    }
}

#[test]
fn a_member_checked_out_elsewhere_is_detached_at_its_pin_naming_where_and_its_branch_is_never_moved(
) {
    let w = weave(&[(OWNED, "owned"), (DEPENDENCY, "dependency")]);
    let ww = create_workweave(&w.workspace, "feat");
    let primary_dep = w.workspace.join(DEPENDENCY);
    let pin = common::git_in(&primary_dep, &["rev-parse", "HEAD"]);

    std::fs::write(primary_dep.join("later"), "x\n").unwrap();
    common::git_in(&primary_dep, &["add", "later"]);
    common::git_in(&primary_dep, &["commit", "-m", "later"]);
    let main_tip = common::git_in(&primary_dep, &["rev-parse", "main"]);
    let status = common::git_in(&primary_dep, &["status", "--porcelain"]);

    rwv()
        .arg("update")
        .current_dir(&ww)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "github/o/dependency: checkout: main is checked out in",
        ));

    let worktree = ww.join(DEPENDENCY);
    assert_eq!(common::checkout_ref(&worktree), None);
    assert_eq!(common::git_in(&worktree, &["rev-parse", "HEAD"]), pin);
    assert_eq!(
        common::git_in(&primary_dep, &["rev-parse", "main"]),
        main_tip
    );
    assert_eq!(
        common::git_in(&primary_dep, &["status", "--porcelain"]),
        status
    );
}

#[test]
fn update_never_writes_the_lock() {
    let w = weave(&[(OWNED, "owned")]);
    let lock = project_dir(&w.workspace).join("rwv.lock");
    let owned = w.workspace.join(OWNED);
    common::git_in(&owned, &["switch", "--detach", "HEAD"]);
    std::fs::write(owned.join("ahead"), "x\n").unwrap();
    common::git_in(&owned, &["add", "ahead"]);
    common::git_in(&owned, &["commit", "-m", "ahead"]);
    let before = (std::fs::read(&lock).unwrap(), identity(&lock));

    rwv()
        .arg("update")
        .current_dir(&w.workspace)
        .assert()
        .success();

    assert_eq!(
        (std::fs::read(&lock).unwrap(), identity(&lock)),
        before,
        "the lock is read, never written"
    );
}

#[test]
fn a_declared_branch_that_exists_nowhere_is_not_created_and_is_reported() {
    let w = weave(&[(OWNED, "owned")]);
    let manifest = project_dir(&w.workspace).join("rwv.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(text.contains("branch = \"main\""), "{text}");
    std::fs::write(
        &manifest,
        text.replace("branch = \"main\"", "branch = \"ghost\""),
    )
    .unwrap();
    let owned = w.workspace.join(OWNED);
    let head = common::git_in(&owned, &["rev-parse", "HEAD"]);

    rwv()
        .arg("update")
        .current_dir(&w.workspace)
        .assert()
        .success()
        .stdout(predicate::str::contains("name no-such-branch"))
        .stdout(predicate::str::contains("ghost"));

    assert!(
        !branches(&owned).contains(&"ghost".to_owned()),
        "no alignment creates a declared branch"
    );
    assert_eq!(common::git_in(&owned, &["rev-parse", "HEAD"]), head);
    assert_eq!(common::checkout_ref(&owned).as_deref(), Some("main"));
}

/// A declared branch that exists nowhere offers what D13 lists for it: a
/// fetch, the setter creating it, and starting it by hand.
#[test]
fn a_declared_branch_existing_nowhere_offers_its_creation_at_update() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    let manifest = std::fs::read_to_string(project_dir.join("rwv.toml")).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        manifest.replace("branch = \"main\"", "branch = \"nosuch\""),
    )
    .unwrap();
    common::git_in(&project_dir, &["commit", "-qam", "declare nosuch"]);

    let out = common::rwv()
        .args(["update"])
        .current_dir(&ws.workspace)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("name no-such-branch"), "{text}");
    assert!(text.contains("rwv fetch"), "{text}");
    assert!(
        text.contains("rwv member set-branch local/org/a nosuch --new"),
        "{text}"
    );
}
