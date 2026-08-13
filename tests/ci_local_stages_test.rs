//! `scripts/ci-local.sh` is the one script CI, the release gate, the
//! pre-push hook and every contributor run. This pins four things about its
//! `--stages` selector: the no-flag invocation still runs all seven stages,
//! in the same order, with the same header text and the same terminal line;
//! `--stages=drift` isolates the regenerate-and-diff block, exiting non-zero
//! and naming the remedy when regeneration disagrees with the committed
//! tree; the windows stage skips loudly, printing the install command,
//! whenever the target isn't there — never a hard failure, because
//! ci-checks.yml's windows-check job already owns Windows compile truth
//! authoritatively; and a subset run's terminal line names the stages that
//! ran, so it can never be mistaken for a full gate by a reader holding only
//! the log.
//!
//! Every test drives the real script as a subprocess with a stub `cargo` and
//! `rustup` on `PATH`, so a stage's *shape* — which header prints, in what
//! order, on what exit code — is pinned without paying for a real build or
//! depending on whether this host happens to have the Windows target
//! installed. `#![cfg(unix)]`: the script under test is a
//! `#!/usr/bin/env bash` file, so every helper here exists for a target this
//! suite already can't run on; a per-test `#[cfg(unix)]` would strand them
//! all as dead code on the one platform that denies warnings on the host
//! target.

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

// `rustup target list --installed` is queried live at runtime rather than
// baked into the stub file, so one stub serves both the installed and
// missing-target scenarios via CI_LOCAL_TEST_WINDOWS_TARGET_INSTALLED.
const STUB_RUSTUP: &str = "#!/bin/sh\n\
if [ \"$*\" = \"target list --installed\" ]; then\n\
    if [ \"${CI_LOCAL_TEST_WINDOWS_TARGET_INSTALLED:-1}\" = \"1\" ]; then\n\
        echo x86_64-pc-windows-msvc\n\
    fi\n\
    exit 0\n\
fi\n\
exit 0\n";

/// A directory holding stub `cargo` and `rustup` binaries that log their
/// argv and exit 0. Prepend it to `PATH` so `scripts/ci-local.sh` never
/// reaches a real build or depends on this host's installed targets.
fn stub_bin_dir() -> tempfile::TempDir {
    let dir = common::tempdir().expect("tempdir");
    for (name, body) in [("cargo", STUB_CARGO), ("rustup", STUB_RUSTUP)] {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
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

fn run_ci_local(cwd: &Path, stub_bin: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let real_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{real_path}", stub_bin.display());
    let mut cmd = Command::new(ci_local_sh());
    cmd.args(args).current_dir(cwd).env("PATH", path);
    cmd.env_remove("CI_LOCAL_TEST_DRIFT_FILE");
    cmd.env_remove("CI_LOCAL_TEST_WINDOWS_TARGET_INSTALLED");
    for (k, v) in extra_env {
        cmd.env(k, v);
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
    "==> cargo check --locked --all-targets --target x86_64-pc-windows-msvc",
    "==> cargo test --release",
    "==> cargo clippy --all-targets -- -D warnings",
    "==> cargo doc --no-deps (rustdoc warnings deny)",
    "==> cargo fmt --all -- --check",
    "==> explain artifacts up to date (no drift after regeneration)",
];

#[test]
fn default_run_executes_all_seven_stages_in_order() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(fixture.path(), stub.path(), &[], &[]);
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
        7,
        "expected one cargo invocation per stage:\n{stdout}"
    );
}

#[test]
fn stages_flag_check_only_runs_check_and_nothing_else() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(fixture.path(), stub.path(), &["--stages=check"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("==> cargo check"));
    for h in &HEADERS_IN_ORDER[1..] {
        assert!(!stdout.contains(h), "unexpected header {h:?} in:\n{stdout}");
    }
    assert_eq!(stdout.matches("STUB_CARGO:").count(), 1);
    assert!(stdout.contains("STUB_CARGO: check"));
    assert!(
        stdout.trim_end().ends_with("All checks passed (stages: check)."),
        "subset run should name the stage it ran, not print the full-gate line:\n{stdout}"
    );
}

#[test]
fn stages_flag_drift_only_skips_every_other_stage() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(fixture.path(), stub.path(), &["--stages=drift"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    let last = HEADERS_IN_ORDER.len() - 1;
    assert!(stdout.contains(HEADERS_IN_ORDER[last]));
    for h in &HEADERS_IN_ORDER[..last] {
        assert!(!stdout.contains(h), "unexpected header {h:?} in:\n{stdout}");
    }
    assert_eq!(stdout.matches("STUB_CARGO:").count(), 1);
    assert!(stdout.contains("STUB_CARGO: run --quiet --bin generate-explain"));
    assert!(
        stdout.trim_end().ends_with("All checks passed (stages: drift)."),
        "subset run should name the stage it ran, not print the full-gate line:\n{stdout}"
    );
}

/// The regression this suite exists to catch: a subset run's terminal line
/// must not be the same string a full run prints. `default_run_executes_all_seven_stages_in_order`
/// pins the full-run line as exactly `All checks passed.`; this pins that no
/// subset invocation ever produces that same line, so a log holding only the
/// terminal output can always tell the two apart.
#[test]
fn subset_run_terminal_line_never_equals_the_full_run_line() {
    let stub = stub_bin_dir();
    for stages in ["check", "drift", "windows", "check,drift"] {
        let fixture = fixture_repo();
        let out = run_ci_local(
            fixture.path(),
            stub.path(),
            &[&format!("--stages={stages}")],
            &[("CI_LOCAL_TEST_WINDOWS_TARGET_INSTALLED", "1")],
        );
        assert!(out.status.success(), "{}", stderr_of(&out));
        let stdout = stdout_of(&out);
        let terminal_line = stdout.trim_end().lines().last().unwrap_or("");
        assert_ne!(
            terminal_line, "All checks passed.",
            "--stages={stages} produced the same terminal line as a full run:\n{stdout}"
        );
    }
}

/// Naming every stage explicitly, out of order, is a full run in substance —
/// it should get the plain line a no-flag invocation gets, not a stages-list
/// line that would (falsely) suggest something was left out.
#[test]
fn stages_flag_naming_all_seven_explicitly_gets_the_full_run_line() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(
        fixture.path(),
        stub.path(),
        &["--stages=drift,fmt,doc,clippy,test,windows,check"],
        &[("CI_LOCAL_TEST_WINDOWS_TARGET_INSTALLED", "1")],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.trim_end().ends_with("All checks passed."),
        "naming all seven stages should print the full-run line:\n{stdout}"
    );
    assert_eq!(stdout.matches("STUB_CARGO:").count(), 7);
}

#[test]
fn unknown_stage_is_rejected_before_anything_runs() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(fixture.path(), stub.path(), &["--stages=bogus"], &[]);
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
        &[(
            "CI_LOCAL_TEST_DRIFT_FILE",
            drift_target.to_str().expect("utf8 fixture path"),
        )],
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
    let out = run_ci_local(fixture.path(), stub.path(), &["--stages=drift"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
}

#[test]
fn windows_stage_runs_the_cross_check_when_the_target_is_installed() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(
        fixture.path(),
        stub.path(),
        &["--stages=windows"],
        &[("CI_LOCAL_TEST_WINDOWS_TARGET_INSTALLED", "1")],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("STUB_CARGO: check --locked --all-targets --target x86_64-pc-windows-msvc")
    );
    assert!(
        !stdout.contains("windows cross-check skipped"),
        "should not skip when the target is installed:\n{stdout}"
    );
}

#[test]
fn windows_stage_skips_loudly_with_the_install_command_when_the_target_is_missing() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(
        fixture.path(),
        stub.path(),
        &["--stages=windows"],
        &[("CI_LOCAL_TEST_WINDOWS_TARGET_INSTALLED", "0")],
    );
    assert!(
        out.status.success(),
        "a missing target should skip, not fail: {}",
        stderr_of(&out)
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains(
            "windows cross-check skipped: x86_64-pc-windows-msvc not installed — rustup target add x86_64-pc-windows-msvc"
        ),
        "missing the loud skip line with its remedy:\n{stdout}"
    );
    assert!(
        !stdout
            .contains("STUB_CARGO: check --locked --all-targets --target x86_64-pc-windows-msvc"),
        "the cross-check itself should not have run:\n{stdout}"
    );
}
