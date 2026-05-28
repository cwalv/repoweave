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
    // Pin `init.defaultBranch=main` for every subprocess git call. CI runners
    // don't ship a user-level `init.defaultBranch` config, so `git init`
    // falls back to `master` and tests that later do `git rev-parse main`
    // explode. Locally this is invisible because most dev machines have
    // `init.defaultBranch = main` set globally. Injecting via
    // `GIT_CONFIG_*` env vars (see git-config(1)) stacks on top of any
    // existing config without touching files.
    cmd.env("GIT_CONFIG_COUNT", "1");
    cmd.env("GIT_CONFIG_KEY_0", "init.defaultBranch");
    cmd.env("GIT_CONFIG_VALUE_0", "main");
    cmd
}

/// Assert that `commit_messages` appear in top-down order (newest-first) in
/// the log of `repo`.
///
/// This is the canonical "history shape" helper for the silent-fallback
/// elimination suite (fo-vsldv). Use it whenever a sync test must verify that
/// CWD's commits land *on top of* a target's prior tip — not below it.
///
/// `commit_messages` is a slice of substrings; each element must match exactly
/// one line in `git log --oneline --no-decorate` output, and the *position* of
/// the first match must be in strictly ascending order (i.e. earlier elements
/// appear higher / newer in the log).
///
/// Panics with a diagnostic showing the full log and the expected ordering if
/// any element is not found or the ordering is violated.
///
/// # Example
/// ```ignore
/// assert_log_ordering(
///     &project_dir,
///     &["feat: ww unique commit", "feat: primary unique commit"],
/// );
/// ```
pub fn assert_log_ordering(repo: &std::path::Path, commit_messages: &[&str]) {
    let out = git()
        .args(["log", "--oneline", "--no-decorate"])
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git log failed to start");
    assert!(
        out.status.success(),
        "git log failed in {}:\n{}",
        repo.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let log = String::from_utf8(out.stdout).unwrap();

    let positions: Vec<(usize, &str)> = commit_messages
        .iter()
        .map(|msg| {
            let pos = log
                .lines()
                .position(|l| l.contains(msg))
                .unwrap_or_else(|| {
                    panic!(
                        "commit message {:?} not found in log of {}.\nLog:\n{log}",
                        msg,
                        repo.display()
                    )
                });
            (pos, *msg)
        })
        .collect();

    for window in positions.windows(2) {
        let (pos_a, msg_a) = window[0];
        let (pos_b, msg_b) = window[1];
        assert!(
            pos_a < pos_b,
            "History shape violation in {}:\n\
             Expected {:?} (pos {pos_a}) to appear ABOVE {:?} (pos {pos_b}) in the log.\n\
             (Lower position number = newer commit = higher in `git log` output.)\n\
             Full log:\n{log}",
            repo.display(),
            msg_a,
            msg_b
        );
    }
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
    // Mirror the `init.defaultBranch=main` pin from [`git`] — rwv shells out
    // to git internally and those subprocesses inherit this env, so any
    // `git init` rwv runs on behalf of a test gets `main` as the default
    // branch regardless of CI runner config.
    cmd.env("GIT_CONFIG_COUNT", "1");
    cmd.env("GIT_CONFIG_KEY_0", "init.defaultBranch");
    cmd.env("GIT_CONFIG_VALUE_0", "main");
    cmd
}
