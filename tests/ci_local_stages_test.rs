//! `scripts/ci-local.sh` is the one script CI, the release gate, the
//! pre-push hook and every contributor run. This pins two things about its
//! `--stages` selector: the no-flag invocation still runs all six stages, in
//! the same order, with the same header text, as before the selector
//! existed; and `--stages=drift` isolates the regenerate-and-diff block,
//! exiting non-zero and naming the remedy when regeneration disagrees with
//! the committed tree.
//!
//! Every test drives the real script as a subprocess with a stub `cargo` on
//! `PATH`, so a stage's *shape* — which header prints, in what order, on what
//! exit code — is pinned without paying for a real build. `#![cfg(unix)]`:
//! the script under test is a `#!/usr/bin/env bash` file, so every helper
//! here exists for a target this suite already can't run on; a per-test
//! `#[cfg(unix)]` would strand them all as dead code on the one platform that
//! denies warnings on the host target.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod common;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ci_local_sh() -> PathBuf {
    repo_root().join("scripts/ci-local.sh")
}

const STUB_CARGO: &str = "#!/bin/sh\n\
printf 'STUB_CARGO: %s\\n' \"$*\"\n\
if [ \"$1 $2 $3\" = \"run --quiet --bin\" ] && [ -n \"${CI_LOCAL_TEST_DRIFT_FILE:-}\" ]; then\n\
    printf 'drift\\n' >> \"$CI_LOCAL_TEST_DRIFT_FILE\"\n\
fi\n\
exit 0\n";

/// A directory holding only a stub `cargo` that logs its argv and exits 0.
/// Prepend it to `PATH` so `scripts/ci-local.sh` never reaches a real build.
fn stub_bin_dir() -> tempfile::TempDir {
    let dir = common::tempdir().expect("tempdir");
    let cargo_path = dir.path().join("cargo");
    std::fs::write(&cargo_path, STUB_CARGO).unwrap();
    std::fs::set_permissions(&cargo_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

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

/// A fresh git repo with the three artifact directories the drift stage
/// diffs, each holding one committed placeholder — isolated from the real
/// repoweave checkout, so a test that induces drift never touches a tracked
/// file this suite did not create.
fn fixture_repo() -> tempfile::TempDir {
    let dir = common::tempdir().expect("tempdir");
    let root = dir.path();
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

fn run_ci_local(cwd: &Path, stub_bin: &Path, args: &[&str], drift_file: Option<&Path>) -> Output {
    let real_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{real_path}", stub_bin.display());
    let mut cmd = Command::new(ci_local_sh());
    cmd.args(args).current_dir(cwd).env("PATH", path);
    if let Some(f) = drift_file {
        cmd.env("CI_LOCAL_TEST_DRIFT_FILE", f);
    } else {
        cmd.env_remove("CI_LOCAL_TEST_DRIFT_FILE");
    }
    cmd.output().expect("ci-local.sh should run")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

const HEADERS_IN_ORDER: &[&str] = &[
    "==> cargo check",
    "==> cargo test --release",
    "==> cargo clippy --all-targets -- -D warnings",
    "==> cargo doc --no-deps (rustdoc warnings deny)",
    "==> cargo fmt --all -- --check",
    "==> explain artifacts up to date (no drift after regeneration)",
];

#[test]
fn default_run_executes_all_six_stages_in_order() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(fixture.path(), stub.path(), &[], None);
    assert!(
        out.status.success(),
        "default run should pass on a clean fixture: {}",
        stderr_of(&out)
    );
    let stdout = stdout_of(&out);

    let mut positions = Vec::new();
    for h in HEADERS_IN_ORDER {
        let pos = stdout
            .find(h)
            .unwrap_or_else(|| panic!("missing header {h:?} in:\n{stdout}"));
        positions.push(pos);
    }
    assert!(
        positions.windows(2).all(|w| w[0] < w[1]),
        "headers out of order:\n{stdout}"
    );
    assert!(
        stdout.trim_end().ends_with("All checks passed."),
        "missing final success line:\n{stdout}"
    );
    assert_eq!(
        stdout.matches("STUB_CARGO:").count(),
        6,
        "expected one cargo invocation per stage:\n{stdout}"
    );
}

#[test]
fn stages_flag_check_only_runs_check_and_nothing_else() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(fixture.path(), stub.path(), &["--stages=check"], None);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("==> cargo check"));
    for h in &HEADERS_IN_ORDER[1..] {
        assert!(!stdout.contains(h), "unexpected header {h:?} in:\n{stdout}");
    }
    assert_eq!(stdout.matches("STUB_CARGO:").count(), 1);
    assert!(stdout.contains("STUB_CARGO: check"));
}

#[test]
fn stages_flag_drift_only_skips_every_other_stage() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(fixture.path(), stub.path(), &["--stages=drift"], None);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains(HEADERS_IN_ORDER[5]));
    for h in &HEADERS_IN_ORDER[..5] {
        assert!(!stdout.contains(h), "unexpected header {h:?} in:\n{stdout}");
    }
    assert_eq!(stdout.matches("STUB_CARGO:").count(), 1);
    assert!(stdout.contains("STUB_CARGO: run --quiet --bin generate-explain"));
}

#[test]
fn unknown_stage_is_rejected_before_anything_runs() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(fixture.path(), stub.path(), &["--stages=bogus"], None);
    assert!(!out.status.success(), "bogus stage should be refused");
    assert!(stderr_of(&out).contains("unknown stage: bogus"));
    assert!(
        !stdout_of(&out).contains("STUB_CARGO:"),
        "no stage should have run: {}",
        stdout_of(&out)
    );
}

#[test]
fn drift_stage_exits_non_zero_and_names_the_remedy_on_real_drift() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let drift_target = fixture
        .path()
        .join("docs/reference/explain/placeholder.txt");
    let out = run_ci_local(
        fixture.path(),
        stub.path(),
        &["--stages=drift"],
        Some(&drift_target),
    );
    assert!(
        !out.status.success(),
        "drift should fail the stage, got: {}",
        stdout_of(&out)
    );
    assert!(stderr_of(&out).contains(
        "explain artifacts changed by regeneration — commit them (this check diffs the working tree against the index; it cannot pass with uncommitted regen)"
    ));
}

#[test]
fn drift_stage_passes_when_regeneration_matches_the_committed_tree() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(fixture.path(), stub.path(), &["--stages=drift"], None);
    assert!(out.status.success(), "{}", stderr_of(&out));
}
