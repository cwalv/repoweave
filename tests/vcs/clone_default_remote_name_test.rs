//! rwv's clones keep the name git gave their remote, and every later
//! interaction resolves the remote by the role it plays.
//!
//! git resolves `clone.defaultRemoteName` from the operator's own config, so a
//! clone rwv made can land its remote under any name. rwv does not choose one:
//! the project repo's url remote is the remote its clone made, recorded when it
//! was made, and `rwv push` publishes to it, reading its default branch there.
//!
//! Driven through `rwv init --adopt`, whose project-repo clone `rwv push` then
//! publishes to. The test spawns `rwv` because only a subprocess can be given a
//! git config that differs from the developer's: the in-process unit test of
//! the seam cannot tell a remote rwv named from one git named.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common;

/// The name this fixture's operator configured. Neither `origin` nor a name
/// any git or rwv path spells on its own, so a remote carrying it can only
/// have taken it from the config planted below.
const OPERATOR_REMOTE_NAME: &str = "weavecheck";

/// Stack `clone.defaultRemoteName` onto the `GIT_CONFIG_*` pair
/// [`common::rwv`] already sets, so `rwv`'s own `git` subprocesses see it.
fn rwv_under_renamed_default_remote() -> assert_cmd::Command {
    let mut cmd = common::rwv();
    cmd.env("GIT_CONFIG_COUNT", "2");
    cmd.env("GIT_CONFIG_KEY_1", "clone.defaultRemoteName");
    cmd.env("GIT_CONFIG_VALUE_1", OPERATOR_REMOTE_NAME);
    cmd
}

/// [`rwv_under_renamed_default_remote`]'s config, on a direct `git` call.
fn git_under_renamed_default_remote() -> Command {
    let mut cmd = common::git();
    cmd.env("GIT_CONFIG_COUNT", "2");
    cmd.env("GIT_CONFIG_KEY_1", "clone.defaultRemoteName");
    cmd.env("GIT_CONFIG_VALUE_1", OPERATOR_REMOTE_NAME);
    cmd
}

/// A bare repo carrying a committed `rwv.toml` on `main`, ready to be adopted
/// as a project repo and pushed back to.
fn seed_bare_project_repo(tmp: &Path) -> PathBuf {
    let bare = tmp.join("project.git");
    common::git_in(
        tmp,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );

    let seed = tmp.join("project-seed");
    common::git_in(
        tmp,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    common::git_in(&seed, &["config", "user.email", "test@test.com"]);
    common::git_in(&seed, &["config", "user.name", "Test"]);
    std::fs::write(seed.join("rwv.toml"), "[repositories]\n").unwrap();
    common::git_in(&seed, &["add", "."]);
    common::git_in(&seed, &["commit", "-m", "seed project manifest"]);
    common::git_in(&seed, &["push", "origin", "main"]);

    bare
}

fn make_empty_workspace(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

/// An operator whose git renames the default remote still gets a project repo
/// `rwv push` publishes: the remote git named is the one recorded and used.
#[test]
fn adopted_project_is_publishable_under_a_renamed_default_remote() {
    let tmp = common::tempdir().unwrap();
    let ws = make_empty_workspace(tmp.path());
    let bare = seed_bare_project_repo(tmp.path());

    // Reachability control: the planted config has to actually reach a `git
    // clone`, or every assertion below is green against a fixture that never
    // posed the question. A malformed `GIT_CONFIG_COUNT` is silently ignored
    // by git, which is exactly what that failure would look like.
    let control = tmp.path().join("control-clone");
    let out = git_under_renamed_default_remote()
        .args(["clone", bare.to_str().unwrap(), control.to_str().unwrap()])
        .output()
        .expect("git should be available");
    assert!(
        out.status.success(),
        "control clone failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        common::git_in(&control, &["remote"]),
        OPERATOR_REMOTE_NAME,
        "the fixture's `clone.defaultRemoteName` must reach git, or this test \
         asks nothing"
    );

    rwv_under_renamed_default_remote()
        .args(["init", "--adopt", &common::file_url(&bare)])
        .current_dir(&ws)
        .assert()
        .success();

    let out = rwv_under_renamed_default_remote()
        .args(["push"])
        .current_dir(&ws)
        .output()
        .expect("rwv should be runnable");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !stderr.contains("origin"),
        "no message names a remote nobody resolved; got:\n{stderr}"
    );
    let project = ws.join("projects").join("project");
    assert_eq!(
        common::git_in(&project, &["config", "--get", "rwv.url-remote"]),
        OPERATOR_REMOTE_NAME,
        "the recorded url remote is the remote the clone made"
    );
    assert_eq!(
        common::git_in(&project, &["remote"]),
        OPERATOR_REMOTE_NAME,
        "rwv added no remote of its own"
    );
    assert!(
        out.status.success(),
        "`rwv push` should publish the adopted project; got:\n{stderr}"
    );
}
