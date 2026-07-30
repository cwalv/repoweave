//! Pins the prohibition this test enforces: `.githooks/commit-msg` must strip
//! `Claude-Session:` trailers from a commit message before it lands, and must
//! not touch any other line.
//!
//! Each test points a fresh repo's `core.hooksPath` at this checkout's
//! `.githooks` and drives a real `git commit`, so it exercises the hook the
//! same way git does rather than calling the script's logic directly.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

fn git_run(cwd: &Path, args: &[&str]) -> Output {
    common::git()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should be available")
}

fn hooks_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks")
}

/// Commit `message` in a fresh repo wired to this checkout's `.githooks`,
/// and return the message git recorded for that commit.
fn commit_through_hook(message: &str) -> String {
    let tmp = common::tempdir().expect("tempdir");
    let repo = tmp.path();
    git_run(repo, &["init", "--initial-branch=main", "-q"]);
    git_run(repo, &["config", "user.email", "test@test.com"]);
    git_run(repo, &["config", "user.name", "Test"]);
    git_run(
        repo,
        &["config", "core.hooksPath", hooks_dir().to_str().unwrap()],
    );
    std::fs::write(repo.join("file.txt"), "content").unwrap();
    git_run(repo, &["add", "file.txt"]);

    let msg_path = repo.join("msg.txt");
    std::fs::write(&msg_path, message).unwrap();
    let output = git_run(repo, &["commit", "-F", "msg.txt"]);
    assert!(
        output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = git_run(repo, &["log", "-1", "--format=%B"]);
    String::from_utf8(log.stdout).unwrap()
}

#[test]
fn strips_the_claude_session_trailer() {
    let message = "subject\n\nbody line\n\nClaude-Session: https://claude.ai/code/session_abc\n";
    let result = commit_through_hook(message);
    assert!(
        !result.contains("Claude-Session:"),
        "trailer survived the hook: {result:?}"
    );
    assert!(result.contains("subject"));
    assert!(result.contains("body line"));
}

#[test]
fn leaves_every_other_line_byte_identical() {
    let message = "subject\n\nSigned-off-by: Test <test@test.com>\nClaude-Session: https://claude.ai/code/session_abc\nCo-authored-by: Someone <someone@example.com>\n";
    let expected = "subject\n\nSigned-off-by: Test <test@test.com>\nCo-authored-by: Someone <someone@example.com>\n\n";
    assert_eq!(commit_through_hook(message), expected);
}

#[test]
fn leaves_a_message_with_no_trailer_untouched() {
    let message = "subject only\n";
    let expected = "subject only\n\n";
    assert_eq!(commit_through_hook(message), expected);
}
