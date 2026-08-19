//! `rwv abort` restores the repos the op touched, in both of the op's
//! workspaces, when a workspace's `.rwv-active` names a different project than
//! the one the op is landing.
//!
//! The pointer is ambient state the op never consults: savepoint creation,
//! advance-target and cleanup all resolve the target under the op's own
//! project. Abort resolving either side by pointer enumerates a different
//! project's manifest, so `abort_one_repo` never runs on the repos the op
//! advanced — and because a repo with no savepoint is demoted to the aggregate
//! noise line, the abort that restored nothing still reports clean.
//!
//! Two quadrants, one per side of the op, distinguished by where abort is
//! invoked from:
//!
//!   - from the owner (the workweave that ran `sync-to`), the target is the
//!     extra workspace, resolved from the recorded target path;
//!   - from the target (which holds the lease), the target is the invoking
//!     workspace and the owner is the extra.
//!
//! Both fixtures park the op with the target's manifest repo ALREADY advanced,
//! so a restore that does not happen is visible: the collision that parks the
//! op is planted in the target's project repo, which advance-target reaches
//! only after every manifest repo has landed.

use assert_cmd::Command;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), manifest_toml).unwrap();
}

const SERVER_URL: &str = "https://github.com/example/server.git";
const SERVER_PATH: &str = "github/example/server";
const SYNCED_PROJECT: &str = "web-app";
const POINTER_PROJECT: &str = "other-app";

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

/// A primary weave and a second workspace whose repos are `git worktree`
/// pairs of the primary's — the shape a workweave has, and the shape that
/// makes the two sides share one refdb per repo.
fn make_shared(parent: &Path) -> (Workspace, Workspace) {
    let primary = parent.join("primary");
    std::fs::create_dir_all(primary.join("github/example")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let primary_server = primary.join(SERVER_PATH);
    let sha = init_repo(&primary_server);

    let primary_project = primary.join(format!("projects/{SYNCED_PROJECT}"));
    init_repo(&primary_project);
    std::fs::write(
        primary_project.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    write_manifest(&primary_project, &[(SERVER_PATH, SERVER_URL)]);
    common::fixture_lock(&primary_project, &[(SERVER_PATH, SERVER_URL, &sha)]);
    common::git_in(
        &primary_project,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&primary_project, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary.join(".rwv-active"), format!("{SYNCED_PROJECT}\n")).unwrap();

    let ww = parent.join("ww");
    std::fs::create_dir_all(ww.join("github/example")).unwrap();
    std::fs::create_dir_all(ww.join("projects")).unwrap();

    let ww_server = ww.join(SERVER_PATH);
    common::git_in(
        &primary_server,
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/server",
        ],
    );

    let ww_project = ww.join(format!("projects/{SYNCED_PROJECT}"));
    common::git_in(
        &primary_project,
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );
    std::fs::write(ww.join(".rwv-active"), format!("{SYNCED_PROJECT}\n")).unwrap();

    (
        Workspace {
            root: primary,
            project_dir: primary_project,
            server_dir: primary_server,
        },
        Workspace {
            root: ww,
            project_dir: ww_project,
            server_dir: ww_server,
        },
    )
}

/// A parked sync-to: the target's manifest repo has landed CWD's tip and the
/// target's project repo has not, so abort has real target-side work to do.
struct Parked {
    primary: Workspace,
    ww: Workspace,
    target_server_pre_op: String,
    target_project_pre_op: String,
}

/// Drive a real `sync-to` into the parked state, in the divergent-pointer
/// topology: the target's `.rwv-active` names a real project that is not the
/// one being synced.
fn park_with_target_manifest_advanced(parent: &Path) -> Parked {
    let (primary, ww) = make_shared(parent);

    let pointer_project = primary.root.join(format!("projects/{POINTER_PROJECT}"));
    init_repo(&pointer_project);
    write_manifest(&pointer_project, &[]);
    common::git_in(&pointer_project, &["add", "rwv.toml"]);
    common::git_in(
        &pointer_project,
        &["commit", "-m", "pointer project: manifest"],
    );
    std::fs::write(
        primary.root.join(".rwv-active"),
        format!("{POINTER_PROJECT}\n"),
    )
    .unwrap();

    // CWD advances the manifest repo, and its project commit writes a path
    // the target does not have.
    let landed = make_commit(&ww.server_dir, "ww.txt", "workweave\n", "ww: advance");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &landed)]);
    std::fs::write(ww.project_dir.join("notes.txt"), "ww notes\n").unwrap();
    common::git_in(&ww.project_dir, &["add", "rwv.lock", "notes.txt"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    // The target holds an untracked file at that path. Manifest repos advance
    // first, so the op parks on the project repo with the manifest repo landed.
    std::fs::write(primary.project_dir.join("notes.txt"), "primary scratch\n").unwrap();

    let target_server_pre_op = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    let target_project_pre_op = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);

    rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&ww.root)
        .assert()
        .failure();

    assert_ne!(
        landed, target_server_pre_op,
        "fixture must move the manifest repo at all"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        landed,
        "the parked op must leave the target's manifest repo ADVANCED past its \
         pre-op tip — otherwise abort has nothing to restore there and a pin \
         asserting restoration passes without exercising anything"
    );
    assert_eq!(
        common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]),
        target_project_pre_op,
        "the collision must park the target's project repo at its pre-op tip"
    );
    assert!(
        ww.root.join(".rwv-op").exists(),
        "the parked op must leave the owner record in CWD"
    );
    assert!(
        primary.root.join(".rwv-op-lease").exists(),
        "the parked op must leave the lease at the target"
    );

    Parked {
        primary,
        ww,
        target_server_pre_op,
        target_project_pre_op,
    }
}

fn assert_target_restored(parked: &Parked) {
    assert_eq!(
        common::git_in(&parked.primary.server_dir, &["rev-parse", "HEAD"]),
        parked.target_server_pre_op,
        "abort must restore the SYNCED project's target-side manifest repo to \
         its pre-op tip; resolving the workspace by its `.rwv-active` pointer \
         enumerates a different project's manifest and leaves this repo advanced"
    );
    assert_eq!(
        common::git_in(&parked.primary.project_dir, &["rev-parse", "HEAD"]),
        parked.target_project_pre_op,
        "abort must leave the target's project repo at its pre-op tip"
    );
    assert!(
        !parked.ww.root.join(".rwv-op").exists(),
        "a clean abort clears the owner record"
    );
    assert!(
        !parked.primary.root.join(".rwv-op-lease").exists(),
        "a clean abort clears the target's lease"
    );
}

/// Abort from the owner: the target is the extra workspace, reached through
/// the recorded target path.
#[test]
fn abort_from_owner_restores_target_whose_active_project_differs() {
    let tmp = common::tempdir().unwrap();
    let parked = park_with_target_manifest_advanced(tmp.path());

    rwv()
        .args(["abort"])
        .current_dir(&parked.ww.root)
        .assert()
        .success();

    assert_target_restored(&parked);
}

/// Abort from the target: the workspace holding the lease is the one whose
/// pointer diverges, so the same defect sits one door over — on the invoking
/// side's own project resolution.
#[test]
fn abort_from_target_restores_target_whose_active_project_differs() {
    let tmp = common::tempdir().unwrap();
    let parked = park_with_target_manifest_advanced(tmp.path());

    rwv()
        .args(["abort"])
        .current_dir(&parked.primary.root)
        .assert()
        .success();

    assert_target_restored(&parked);
}
