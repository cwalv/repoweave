#![allow(dead_code)]

use std::process::Command;

/// `GIT_*` environment variables that git itself sets for hooks and that
/// would silently misdirect any subprocess `git` invocation if inherited.
const GIT_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_OBJECT_DIRECTORY",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
];

/// Build a `git` command with all inherited `GIT_*` environment variables
/// stripped. The returned `Command` has no cwd set; callers add
/// `current_dir(...)` (and any args) themselves.
///
/// Tests create temp git repos and run subprocess `git` against them. If
/// the outer process has any of `GIT_DIR`, `GIT_WORK_TREE`,
/// `GIT_INDEX_FILE`, etc. set (as is the case under a `pre-push` hook,
/// where `git` exports these for the hook), every subprocess `git` call
/// inherits them and silently operates on the *outer* repo regardless of
/// `current_dir`. That has historically corrupted the source repo's
/// `.git/config` (writing `core.bare = true`, the test `[user]` block,
/// etc.) when the test suite ran from a hook context.
pub fn git() -> Command {
    let mut cmd = Command::new("git");
    for var in GIT_ENV_VARS {
        cmd.env_remove(var);
    }
    // Make `git` non-interactive. `git rebase --continue` and any other
    // commit-completing path invoke `$EDITOR` for the commit message. In CI
    // there is no editor and no TTY, so git aborts with "Terminal is dumb,
    // but EDITOR unset". `GIT_EDITOR=true` substitutes the `true` command,
    // which exits 0 without modifying the prepared message — git uses
    // whatever it already has.
    cmd.env("GIT_EDITOR", "true");
    cmd.env("GIT_SEQUENCE_EDITOR", "true");
    cmd
}

/// Build an `assert_cmd::Command` for the `rwv` binary with inherited
/// `GIT_*` environment variables stripped.
///
/// `rwv` shells out to `git` internally; if it inherits a polluted
/// `GIT_*` env from the test process, those subprocesses operate on the
/// wrong repo. See [`git`] for context.
pub fn rwv() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("rwv").expect("rwv binary should be buildable");
    for var in GIT_ENV_VARS {
        cmd.env_remove(var);
    }
    cmd
}
