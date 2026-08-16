//! rwv's clones name their own remote, rather than inheriting the name the
//! operator's git config picks.
//!
//! The defect this pins: git resolves `clone.defaultRemoteName` from the
//! operator's own config, so a clone rwv made could land its remote under any
//! name. Every later interaction rwv performs on that clone looks the remote up
//! by rwv's convention instead, so the clone is one rwv cannot read back — a
//! `git remote` rwv itself created and immediately misreads.
//!
//! Driven through `rwv init --adopt`, whose project-repo clone `rwv push` then
//! reads `origin/HEAD` from before it will publish anything. `rwv push` is
//! where the misread surfaces as a refusal.
//!
//! This is the pin that distinguishes. `GitVcs::clone_repo`'s own unit test
//! cannot: it runs in the test process, whose git config is the developer's,
//! and absent a `clone.defaultRemoteName` there a clone that names the remote
//! and one that leaves the name to git are byte-identical. Setting the config
//! for a subprocess is what makes the two outcomes different, and only a test
//! that spawns `rwv` can do it.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

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
/// `rwv push` can read `origin/HEAD` from.
///
/// Without the fix `rwv init --adopt` leaves the clone's only remote named
/// `weavecheck`, `refs/remotes/origin/HEAD` never exists, and `rwv push`
/// refuses at its canonical-branch gate.
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
        !stderr.contains("origin/HEAD is unset"),
        "the adopted clone's remote is one rwv cannot read back; got:\n{stderr}"
    );
    assert!(
        out.status.success(),
        "`rwv push` should publish the adopted project; got:\n{stderr}"
    );
}
