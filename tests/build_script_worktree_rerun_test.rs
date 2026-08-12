//! `build.rs` pins `cargo:rerun-if-changed` on the repo's HEAD and refs so a
//! dev build's embedded version string tracks the checkout. In a `git
//! worktree` checkout `.git` is a file, not a directory, so a path assumed
//! relative to the crate root can name nothing on disk — and cargo treats a
//! missing `rerun-if-changed` path as permanently dirty, not as "unwatched".
//!
//! The regression is about cargo's fingerprint decision, not about which
//! path `build.rs` prints: a build script that prints a *resolved-looking*
//! path which still doesn't exist reproduces the same bug while passing any
//! check of the emitted directive. So this builds a real worktree checkout
//! twice and reads cargo's own Fresh/Dirty verdict.

use std::path::Path;
use std::process::Output;

mod common;

/// The crate's own `build.rs`, so a regression in the shipped script fails
/// this test without a second copy to keep in sync.
const BUILD_RS: &str = include_str!("../build.rs");

fn git(args: &[&str], dir: &Path) {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A from-scratch crate carrying the real `build.rs` and nothing else, so a
/// build exercises that script without pulling in `repoweave`'s own
/// dependency graph.
fn write_probe_crate(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"buildrs-worktree-probe\"\nversion = \"0.0.0\"\n\
         edition = \"2021\"\nbuild = \"build.rs\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), "").unwrap();
    std::fs::write(dir.join("build.rs"), BUILD_RS).unwrap();
}

/// `cargo build -v` in `dir`, isolated to `target_dir` so it cannot collide
/// with the target directory the test binary itself was built into.
fn cargo_build_verbose(dir: &Path, target_dir: &Path) -> Output {
    std::process::Command::new("cargo")
        .args(["build", "-v"])
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target_dir)
        .output()
        .expect("cargo build failed to start")
}

/// Build twice with no changes in between in a worktree checkout, then move
/// HEAD and build a third time.
///
/// The middle build is the regression pin: it must come back Fresh. The
/// third is the control — without it, a `build.rs` that dropped the git
/// pins entirely (rather than resolving them) would also make the middle
/// build pass, for the wrong reason.
#[test]
fn build_script_rerun_survives_a_worktree_checkout() {
    let root = common::tempdir().expect("tempdir");
    let main = root.path().join("main");
    write_probe_crate(&main);
    git(&["init", "-q"], &main);
    git(&["add", "-A"], &main);
    git(&["commit", "-q", "-m", "init"], &main);

    let worktree = root.path().join("worktree");
    git(
        &[
            "worktree",
            "add",
            "-q",
            worktree.to_str().unwrap(),
            "-b",
            "probe",
        ],
        &main,
    );
    assert!(
        !worktree.join(".git").is_dir(),
        "fixture bug: {} is not a worktree checkout (.git is a directory, not a file)",
        worktree.display()
    );

    let target_dir = root.path().join("target");
    let populate = cargo_build_verbose(&worktree, &target_dir);
    assert!(
        populate.status.success(),
        "populating build failed:\n{}",
        String::from_utf8_lossy(&populate.stderr)
    );

    let unchanged = cargo_build_verbose(&worktree, &target_dir);
    assert!(
        unchanged.status.success(),
        "unchanged build failed:\n{}",
        String::from_utf8_lossy(&unchanged.stderr)
    );
    let unchanged_log = String::from_utf8_lossy(&unchanged.stderr);
    assert!(
        unchanged_log.contains("Fresh buildrs-worktree-probe"),
        "second build should find the crate Fresh (fingerprint clean) in a \
         worktree checkout; got:\n{unchanged_log}"
    );
    assert!(
        !unchanged_log.contains("build-script-build`"),
        "second build re-ran the build script with no source change — the \
         git rerun-if-changed pin is watching a path that does not exist in \
         this worktree checkout:\n{unchanged_log}"
    );

    std::fs::write(worktree.join("marker"), "").unwrap();
    git(&["add", "-A"], &worktree);
    git(&["commit", "-q", "-m", "move head"], &worktree);

    let after_commit = cargo_build_verbose(&worktree, &target_dir);
    assert!(
        after_commit.status.success(),
        "post-commit build failed:\n{}",
        String::from_utf8_lossy(&after_commit.stderr)
    );
    let after_commit_log = String::from_utf8_lossy(&after_commit.stderr);
    assert!(
        after_commit_log.contains("build-script-build`"),
        "control failed: moving HEAD in the worktree should still re-run the \
         build script — if it doesn't, the fix stopped watching real git \
         state instead of resolving the path:\n{after_commit_log}"
    );
}
