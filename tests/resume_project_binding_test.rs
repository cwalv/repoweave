//! A resumed op takes its project from the op record, and `--project` at
//! `--continue` asserts that binding rather than rebinding it.
//!
//! The owner of an op is where `--continue` reads its parameters from. When
//! that owner is primary-shaped, its `.rwv-active` can move between the strand
//! and the resume — an operator activating another project in the same weave is
//! ordinary. Every phase that already ran is bound to the project the op was
//! started for, so re-deriving the binding from the pointer at resume time
//! lands the remaining phases somewhere the completed ones never were.
//!
//! The fixtures park a real `sync-to` with the target's manifest repo landed
//! and its project repo blocked, then move the pointer. What the resume must
//! still do is advance the target's project repo — asserted directly, because
//! the manifest repo landed before the strand and would look landed either way.

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
const OTHER_PROJECT: &str = "other-app";

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    other_project_dir: PathBuf,
    server_dir: PathBuf,
}

/// Set up a project repo the way `rwv init` does, and return its directory.
fn make_project(root: &Path, name: &str, repos: &[(&str, &str)], locked: &[(&str, &str, &str)]) {
    let dir = root.join(format!("projects/{name}"));
    init_repo(&dir);
    std::fs::write(dir.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    write_manifest(&dir, repos);
    common::fixture_lock(&dir, locked);
    common::git_in(&dir, &["add", ".gitattributes", "rwv.toml", "rwv.lock"]);
    common::git_in(&dir, &["commit", "-m", format!("{name}: initial").as_str()]);
}

/// A primary weave carrying TWO projects, and a second workspace whose repos
/// are `git worktree` pairs of the primary's. Both projects exist on both
/// sides, so activating either one in either workspace is a legitimate state
/// rather than a dangling pointer.
fn make_two_project_weave(parent: &Path) -> (Workspace, Workspace) {
    let primary = parent.join("primary");
    std::fs::create_dir_all(primary.join("github/example")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let primary_server = primary.join(SERVER_PATH);
    let sha = init_repo(&primary_server);

    make_project(
        &primary,
        SYNCED_PROJECT,
        &[(SERVER_PATH, SERVER_URL)],
        &[(SERVER_PATH, SERVER_URL, &sha)],
    );
    make_project(&primary, OTHER_PROJECT, &[], &[]);
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

    for (name, branch) in [
        (SYNCED_PROJECT, "ww/project"),
        (OTHER_PROJECT, "ww/other-project"),
    ] {
        let dest = ww.join(format!("projects/{name}"));
        common::git_in(
            primary.join(format!("projects/{name}")),
            &["worktree", "add", &dest.to_string_lossy(), "-b", branch],
        );
    }
    std::fs::write(ww.join(".rwv-active"), format!("{SYNCED_PROJECT}\n")).unwrap();

    (
        Workspace {
            root: primary.clone(),
            project_dir: primary.join(format!("projects/{SYNCED_PROJECT}")),
            other_project_dir: primary.join(format!("projects/{OTHER_PROJECT}")),
            server_dir: primary_server,
        },
        Workspace {
            root: ww.clone(),
            project_dir: ww.join(format!("projects/{SYNCED_PROJECT}")),
            other_project_dir: ww.join(format!("projects/{OTHER_PROJECT}")),
            server_dir: ww_server,
        },
    )
}

/// A `sync-to` parked inside advance-target: the target's manifest repo has
/// landed CWD's tip, its project repo has not.
struct Parked {
    primary: Workspace,
    ww: Workspace,
    /// The tip the target's project repo must reach when the resume completes.
    owner_project_tip: String,
    /// Where the target's project repo sits while the op is parked.
    target_project_parked: String,
    landed_server_tip: String,
}

fn park_with_target_project_blocked(parent: &Path) -> Parked {
    let (primary, ww) = make_two_project_weave(parent);

    let landed_server_tip = make_commit(&ww.server_dir, "ww.txt", "workweave\n", "ww: advance");
    common::fixture_lock(
        &ww.project_dir,
        &[(SERVER_PATH, SERVER_URL, &landed_server_tip)],
    );
    std::fs::write(ww.project_dir.join("notes.txt"), "ww notes\n").unwrap();
    common::git_in(&ww.project_dir, &["add", "rwv.lock", "notes.txt"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww advance"]);

    // The target holds an untracked file where the incoming project commit
    // writes one. Manifest repos advance first, so the op parks on the project
    // repo with the manifest repo already landed.
    std::fs::write(primary.project_dir.join("notes.txt"), "primary scratch\n").unwrap();

    let target_project_parked = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);

    rwv()
        .args(["sync-to", &primary.root.to_string_lossy(), "--strategy=ff"])
        .current_dir(&ww.root)
        .assert()
        .failure();

    let owner_project_tip = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    assert_ne!(
        owner_project_tip, target_project_parked,
        "the fixture must leave the target's project repo BEHIND the owner's — \
         otherwise the resume has nothing to land there and the pins below pass \
         without exercising anything"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        landed_server_tip,
        "the parked op must have landed the target's manifest repo already"
    );
    assert!(
        ww.root.join(".rwv-op").exists(),
        "the parked op must leave the owner record in CWD"
    );

    // Clear the collision so the resume has a path forward.
    std::fs::rename(
        primary.project_dir.join("notes.txt"),
        primary.project_dir.join("notes.txt.bak"),
    )
    .unwrap();

    Parked {
        primary,
        ww,
        owner_project_tip,
        target_project_parked,
        landed_server_tip,
    }
}

/// Point the OWNER workspace at the other project, as an operator switching
/// tasks while the op sits parked would.
fn activate_other_project_in_owner(parked: &Parked) {
    std::fs::write(
        parked.ww.root.join(".rwv-active"),
        format!("{OTHER_PROJECT}\n"),
    )
    .unwrap();
}

fn assert_resume_landed(parked: &Parked) {
    assert_eq!(
        common::git_in(&parked.primary.project_dir, &["rev-parse", "HEAD"]),
        parked.owner_project_tip,
        "the resumed op must advance the target's project repo in the project the \
         op was STARTED for; re-deriving the binding from the owner's `.rwv-active` \
         runs the remaining phases in whatever is active now and leaves this repo \
         at its parked tip"
    );
    assert_eq!(
        common::git_in(&parked.primary.server_dir, &["rev-parse", "HEAD"]),
        parked.landed_server_tip,
        "the resume must not disturb the manifest repo the op already landed"
    );
    assert_eq!(
        common::git_in(&parked.primary.other_project_dir, &["rev-parse", "HEAD"]),
        common::git_in(&parked.ww.other_project_dir, &["rev-parse", "HEAD"]),
        "the project the pointer names must be left exactly as it was"
    );
    assert!(
        !parked.ww.root.join(".rwv-op").exists(),
        "a completed resume clears the owner record"
    );
}

/// No flag: the record decides, and the owner's moved pointer does not.
#[test]
fn resume_follows_the_record_when_the_owners_pointer_moved() {
    let tmp = common::tempdir().unwrap();
    let parked = park_with_target_project_blocked(tmp.path());
    activate_other_project_in_owner(&parked);

    rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&parked.ww.root)
        .assert()
        .success();

    assert_resume_landed(&parked);
}

/// A `--project` naming the recorded project is an assertion that holds, so
/// the resume proceeds — including when it contradicts the ambient pointer,
/// which is the whole reason the flag stays available at `--continue`.
#[test]
fn resume_proceeds_when_project_flag_matches_the_record() {
    let tmp = common::tempdir().unwrap();
    let parked = park_with_target_project_blocked(tmp.path());
    activate_other_project_in_owner(&parked);

    rwv()
        .args(["sync-to", "--continue", "--project", SYNCED_PROJECT])
        .current_dir(&parked.ww.root)
        .assert()
        .success();

    assert_resume_landed(&parked);
}

/// A `--project` naming anything else is a contradiction between two explicit
/// sources, refused with both values named — and refused before the resume
/// writes the re-entered phase, so the op is left exactly as it was found.
#[test]
fn resume_refuses_a_project_flag_that_contradicts_the_record() {
    let tmp = common::tempdir().unwrap();
    let parked = park_with_target_project_blocked(tmp.path());

    let record_before = std::fs::read_to_string(parked.ww.root.join(".rwv-op")).unwrap();

    let output = rwv()
        .args(["sync-to", "--continue", "--project", OTHER_PROJECT])
        .current_dir(&parked.ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains(SYNCED_PROJECT),
        "the refusal must name the project the op was started for; got:\n{stderr}"
    );
    assert!(
        stderr.contains(OTHER_PROJECT),
        "the refusal must name the contradicting flag value; got:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(parked.ww.root.join(".rwv-op")).unwrap(),
        record_before,
        "the refusal must land before the resume writes its re-entered phase, so a \
         contradicted invocation leaves op-state byte-identical"
    );
    assert_eq!(
        common::git_in(&parked.primary.project_dir, &["rev-parse", "HEAD"]),
        parked.target_project_parked,
        "a refused resume must not advance anything"
    );
}
