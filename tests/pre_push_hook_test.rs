//! `.githooks/pre-push` used to hand-maintain its own clippy/fmt/test list,
//! which had already drifted from `scripts/ci-local.sh` by the time this was
//! noticed — bare `cargo test` instead of `--release`, no `--all-targets` on
//! clippy, no doc or drift stage at all. A tree assembled from two branches
//! that each gated green on their own could still land drift-red at the
//! merge, and would have pushed clean through this hook regardless. It now
//! delegates the whole gate to the one script, so there is nothing here left
//! to drift.
//!
//! The first test pins that delegation at the source-text level: no bare
//! `cargo` invocation survives in the hook outside its own comments. The
//! second drives the hook as a real subprocess — against an isolated fixture
//! carrying its own copy of `scripts/ci-local.sh`, never the live checkout —
//! with a stub `cargo` on `PATH`, and checks that a plain branch push (no
//! tag on stdin) runs every stage of the delegated gate exactly once.
//!
//! `#![cfg(unix)]`: `.githooks/pre-push` is a `#!/bin/sh` script; every
//! helper here exists for a target this suite already can't run on, and a
//! per-test `#[cfg(unix)]` would strand them as dead code on the platform
//! that denies warnings on the host target.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod common;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn pre_push_hook() -> PathBuf {
    repo_root().join(".githooks/pre-push")
}

fn non_comment_lines(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter(|l| !l.trim_start().starts_with('#'))
}

#[test]
fn contains_no_bare_cargo_invocations() {
    let text = std::fs::read_to_string(pre_push_hook()).expect("pre-push hook should exist");
    for line in non_comment_lines(&text) {
        assert!(
            !line.contains("cargo clippy")
                && !line.contains("cargo fmt")
                && !line.contains("cargo test")
                && !line.contains("cargo check")
                && !line.contains("cargo doc")
                && !line.contains("cargo run"),
            "pre-push hook invokes cargo directly, bypassing scripts/ci-local.sh: {line:?}"
        );
    }
    assert!(
        non_comment_lines(&text).any(|l| l.contains("scripts/ci-local.sh")),
        "pre-push hook should delegate to scripts/ci-local.sh"
    );
}

const STUB_CARGO: &str = "#!/bin/sh\nprintf 'STUB_CARGO: %s\\n' \"$*\"\nexit 0\n";

fn git(args: &[&str], cwd: &Path) {
    let out = common::git()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should run");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A fixture carrying its own `scripts/ci-local.sh` (copied from this
/// checkout) and the three artifact directories the drift stage diffs, so
/// driving the real hook never touches this checkout's working tree.
fn fixture_repo() -> tempfile::TempDir {
    let dir = common::tempdir().expect("tempdir");
    let root = dir.path();

    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::copy(
        repo_root().join("scripts/ci-local.sh"),
        root.join("scripts/ci-local.sh"),
    )
    .unwrap();
    std::fs::set_permissions(
        root.join("scripts/ci-local.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    for sub in [
        "docs/reference/explain",
        "docs/reference/schemas",
        "docs/reference/prime",
    ] {
        std::fs::create_dir_all(root.join(sub)).unwrap();
        std::fs::write(root.join(sub).join("placeholder.txt"), "generated\n").unwrap();
    }

    git(&["init", "-q", "--initial-branch=main"], root);
    git(&["config", "user.email", "test@test.com"], root);
    git(&["config", "user.name", "Test"], root);
    git(&["add", "-A"], root);
    git(&["commit", "-q", "-m", "init"], root);
    dir
}

#[test]
fn a_plain_branch_push_runs_every_stage_of_the_delegated_gate_once() {
    let fixture = fixture_repo();
    let stub_dir = common::tempdir().expect("tempdir");
    let cargo_path = stub_dir.path().join("cargo");
    std::fs::write(&cargo_path, STUB_CARGO).unwrap();
    std::fs::set_permissions(&cargo_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let real_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{real_path}", stub_dir.path().display());

    let mut child = Command::new(pre_push_hook())
        .current_dir(fixture.path())
        .env("PATH", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("pre-push hook should run");
    // No lines on stdin: a plain branch push, not a tag — the tag-verification
    // block's `while read` loop sees EOF immediately and never runs, so this
    // exercises only the delegated-gate half of the hook.
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("hook should exit");

    assert!(
        out.status.success(),
        "hook should pass on a clean fixture with a stub cargo:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.matches("STUB_CARGO:").count(),
        6,
        "expected one cargo invocation per ci-local.sh stage:\n{stdout}"
    );
    assert!(stdout.contains("All checks passed."));
    assert!(stdout.contains("pre-push: all checks passed"));
}
