#![allow(dead_code)]

pub mod compile_probe;
pub mod contract;
pub mod doctor_corpus;
pub mod src_scan;

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

/// A temporary directory whose path is already canonical. Drop-in for
/// `tempfile::tempdir()`; use it for every fixture root in the suite.
///
/// `tempfile` hands back whatever `$TMPDIR` names, and on macOS that is under
/// `/var`, a symlink to `/private/var`. rwv canonicalizes the paths it prints,
/// so an expected path a test builds from a raw temp root is a *different
/// spelling of the same file* than the one rwv reports. Every such comparison
/// passes on Linux, where `/tmp` is a real directory, and fails on macOS —
/// which is how macOS CI stayed red through a release while Linux was green.
/// `git worktree list --porcelain` resolves the same way, so the mismatch is
/// not limited to paths rwv itself prints.
///
/// Canonicalizing the root here rather than at each comparison is the point.
/// A rule applied where paths are compared has to be remembered by every test
/// anyone adds later; rooted here there is no non-canonical path in the suite
/// to get wrong. `canonical_temp_root_test.rs` keeps it that way.
///
/// Reproduce the macOS geometry on any platform:
///
/// ```sh
/// mkdir -p $T/real && ln -s $T/real $T/link
/// TMPDIR=$T/link cargo test --release --no-fail-fast
/// ```
///
/// Pick a `$T` outside any repoweave weave: a temp root nested under one puts
/// every fixture inside it, and the suite's "outside a workspace" tests then
/// fail for that reason instead.
pub fn tempdir() -> std::io::Result<tempfile::TempDir> {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    let root = ROOT.get_or_init(|| {
        let raw = std::env::temp_dir();
        raw.canonicalize()
            .unwrap_or_else(|e| panic!("temp dir {} does not resolve: {e}", raw.display()))
    });
    tempfile::TempDir::new_in(root)
}

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
/// elimination suite. Use it whenever a sync test must verify that
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

// ---------------------------------------------------------------------------
// "Which ref is this checkout on"
// ---------------------------------------------------------------------------
//
// This is the enforcement primitive the suite was missing: a fetch detach
// survived because the test that should have caught it asserted only
// `rev-parse HEAD` equality, against a fixture that had pre-detached the repo.
// A tip comparison cannot see a detach — HEAD points at the same commit either
// way. The question has to be asked about the *ref*.
//
// Two things make this a primitive rather than another local helper:
//
//  1. **It asks the production classifier.** `Vcs::head_attachment` is the
//     code under test's own answer, so a test cannot pass because the test
//     and the product disagree about what "on a branch" means.
//  2. **It does not ask git for a short name.** Every hand-rolled version of
//     this in the suite ran `git symbolic-ref --short HEAD`, and `--short`
//     answers the shortest *unambiguous* name: with a tag named `main` in the
//     repo it returns `heads/main`, which does not round-trip through
//     `refs/heads/<name>`. `observe_head` avoids `--short` deliberately, and
//     a test that reintroduces it is asserting against a different function
//     than the one that ships.
//
// The four states `current_ref`'s `Ok(None)` used to collapse stay apart
// here: `Attached` and `Unborn` answer with a name, `Detached` answers
// `None`, and a directory that is not a repo — or a ref database that cannot
// be read — panics rather than quietly reading as "no branch".

/// The name of the ref `repo`'s checkout is on, or `None` when HEAD is
/// detached.
///
/// Panics when `repo` is not a repository or its ref database is unreadable:
/// in a test those are fixture bugs, and letting them read as "detached"
/// would conflate a fixture bug with a real detached HEAD.
pub fn checkout_ref(repo: &std::path::Path) -> Option<String> {
    use repoweave::vcs::HeadAttachment;
    match repoweave::git::git_vcs().head_attachment(repo) {
        Ok(HeadAttachment::Attached(a)) => Some(a.to_string()),
        Ok(HeadAttachment::Unborn(u)) => Some(u.name().as_str().to_owned()),
        Ok(HeadAttachment::Detached(_)) => None,
        Err(e) => panic!(
            "head_attachment failed for {}: {e} — this is a fixture bug, not a \
             detached HEAD",
            repo.display()
        ),
    }
}

/// Assert that `repo`'s checkout is on the branch named `branch`.
pub fn assert_on_branch(repo: &std::path::Path, branch: &str) {
    match checkout_ref(repo) {
        Some(actual) => assert_eq!(actual, branch, "{} should be on '{branch}'", repo.display()),
        None => panic!(
            "{} should be on '{branch}' but HEAD is detached",
            repo.display()
        ),
    }
}

/// Assert that `repo`'s HEAD names no branch.
///
/// Use where a detach is the *specified* outcome (`--detach-checkouts`, a
/// lock-pinned materialization). Everywhere else, prefer [`assert_on_branch`]:
/// asserting the positive is what makes an unintended detach a failure.
pub fn assert_detached(repo: &std::path::Path) {
    if let Some(branch) = checkout_ref(repo) {
        panic!("{} should be detached but is on '{branch}'", repo.display());
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
