//! `scripts/ci-local.sh` is the one script CI, the release gate, the
//! pre-push hook and every contributor run. This pins its `--stages`
//! selector: the no-flag invocation still runs all seven default stages,
//! in the same order, with the same header text and the same terminal line;
//! `--stages=drift` isolates the regenerate-and-diff block, exiting non-zero
//! and naming the remedy when regeneration disagrees with the committed
//! tree; the windows stage skips loudly, printing the install command,
//! whenever the target isn't there — never a hard failure, because
//! ci-checks.yml's windows-check job already owns Windows compile truth
//! authoritatively; a subset run's terminal line names the stages that ran,
//! so it can never be mistaken for a full gate by a reader holding only the
//! log; and the windows-wine stage runs only when named.
//!
//! Every test drives the real script as a subprocess with a stub `cargo` and
//! `rustup` on `PATH` (and, for the wine stage's wiring, a stub
//! `windows-wine.sh` beside a copy of it), so a stage's *shape* — which
//! header prints, in what order, on what exit code — is pinned without paying
//! for a real build or depending on whether this host happens to have the
//! Windows target installed. `#![cfg(unix)]`: the script under test is a
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
    common::git_in(root, &["init", "-q", "--initial-branch=main"]);
    common::git_in(root, &["config", "user.email", "test@test.com"]);
    common::git_in(root, &["config", "user.name", "Test"]);
    common::git_in(root, &["add", "-A"]);
    common::git_in(root, &["commit", "-q", "-m", "init"]);
    dir
}

fn run_ci_local(cwd: &Path, stub_bin: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    run_script(&ci_local_sh(), cwd, stub_bin, args, extra_env)
}

fn run_script(
    script: &Path,
    cwd: &Path,
    stub_bin: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> Output {
    let real_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{real_path}", stub_bin.display());
    let mut cmd = Command::new(script);
    cmd.args(args).current_dir(cwd).env("PATH", path);
    cmd.env_remove("CI_LOCAL_TEST_DRIFT_FILE");
    cmd.env_remove("CI_LOCAL_TEST_WINDOWS_TARGET_INSTALLED");
    cmd.env_remove("CI_LOCAL_TEST_WINE_EXIT");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    // A sibling test's fork can still hold a just-written script open for
    // writing; exec then fails with ETXTBSY until that child execs or exits.
    for _ in 0..100 {
        match cmd.output() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            result => return result.expect("ci-local.sh should run"),
        }
    }
    panic!("ci-local.sh stayed busy (ETXTBSY) for 2s")
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
    "==> cargo test --release --no-fail-fast",
    "==> cargo clippy --all-targets -- -D warnings",
    "==> cargo doc --no-deps --document-private-items (rustdoc warnings deny)",
    "==> cargo fmt --all -- --check",
    "==> explain artifacts up to date (no drift after regeneration)",
];

/// Argv a `==> cargo ...` header claims to run: `cargo ` stripped, and any
/// trailing parenthetical annotation (the doc stage's `(rustdoc warnings
/// deny)`) cut off. `None` for a header that isn't a `cargo` echo at all —
/// the drift stage's header names what the stage checks, not the command it
/// runs, so it carries no claim this can compare against an invocation.
fn cargo_args_claimed_by(header: &str) -> Option<&str> {
    let args = header.strip_prefix("==> cargo ")?;
    Some(match args.find(" (") {
        Some(i) => &args[..i],
        None => args,
    })
}

/// `HEADERS_IN_ORDER`'s substring search only proves a header's text exists
/// somewhere in `stdout`; it cannot tell that from a command that has
/// drifted underneath an unchanged header, since the old header text is
/// still there to find. This instead pairs each `cargo`-echoing header with
/// the STUB_CARGO line logged before the next header and asserts the two
/// name the same argv — the direction a header and its command actually
/// disagree.
///
/// Scope: covers only headers `cargo_args_claimed_by` recognizes (six of
/// the seven — drift's header is prose, not a command echo, and is outside
/// what this can see).
fn assert_cargo_headers_agree_with_invocations(stdout: &str) {
    let positions: Vec<(usize, &str)> = HEADERS_IN_ORDER
        .iter()
        .filter_map(|h| stdout.find(h).map(|pos| (pos, *h)))
        .collect();
    let mut compared = 0;
    for (i, &(pos, header)) in positions.iter().enumerate() {
        let Some(claimed) = cargo_args_claimed_by(header) else {
            continue;
        };
        let end = positions.get(i + 1).map_or(stdout.len(), |&(p, _)| p);
        let logged = stdout[pos..end]
            .lines()
            .find_map(|line| line.strip_prefix("STUB_CARGO: "))
            .unwrap_or_else(|| {
                panic!(
                    "header {header:?} has no STUB_CARGO invocation before the next header:\n{stdout}"
                )
            });
        assert_eq!(
            logged, claimed,
            "header {header:?} claims `cargo {claimed}` but the logged invocation was `cargo {logged}`"
        );
        compared += 1;
    }
    // `positions` only holds headers `stdout.find` actually located; a header
    // that silently stopped printing would just shrink that list, and the
    // loop above would compare fewer pairs without saying so. Cross-check
    // the count against `HEADERS_IN_ORDER` itself, which doesn't depend on
    // what `stdout` contains.
    let claimable = HEADERS_IN_ORDER
        .iter()
        .filter(|h| cargo_args_claimed_by(h).is_some())
        .count();
    assert_eq!(
        compared, claimable,
        "expected {claimable} header/invocation comparisons, only made {compared} — a cargo-echoing header went unchecked:\n{stdout}"
    );
}

/// A stage whose header line goes missing from `stdout` altogether — the
/// clippy header is absent here, its STUB_CARGO line along with it — leaves
/// every present header/invocation pair still agreeing, so the loop's own
/// `assert_eq!` has nothing to object to. Only the comparison count catches
/// the drop.
#[test]
#[should_panic(expected = "expected 6 header/invocation comparisons")]
fn header_invocation_floor_catches_a_header_the_position_scan_never_finds() {
    let stdout = "\n==> cargo check\nSTUB_CARGO: check\n\
\n==> cargo check --locked --all-targets --target x86_64-pc-windows-msvc\nSTUB_CARGO: check --locked --all-targets --target x86_64-pc-windows-msvc\n\
\n==> cargo test --release\nSTUB_CARGO: test --release\n\
\n==> cargo doc --no-deps (rustdoc warnings deny)\nSTUB_CARGO: doc --no-deps\n\
\n==> cargo fmt --all -- --check\nSTUB_CARGO: fmt --all -- --check\n\
\n==> explain artifacts up to date (no drift after regeneration)\n";
    assert_cargo_headers_agree_with_invocations(stdout);
}

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
    assert_cargo_headers_agree_with_invocations(&stdout);
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
        stdout
            .trim_end()
            .ends_with("All checks passed (stages: check)."),
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
        stdout
            .trim_end()
            .ends_with("All checks passed (stages: drift)."),
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
        "explain artifacts changed by regeneration — commit them (this check compares the regenerated docs/reference/explain, docs/reference/schemas, and docs/reference/prime against the index; it does not inspect files the generator does not write)"
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

const WINE_HEADER: &str = "==> test suite under Wine as x86_64-pc-windows-gnu binaries";

/// `scripts/ci-local.sh` copied beside a stub `windows-wine.sh` that logs
/// its call and exits with `CI_LOCAL_TEST_WINE_EXIT`, so the stage's wiring
/// is observed without the real script's downloads.
fn ci_local_beside_a_stub_wine_script() -> (tempfile::TempDir, PathBuf) {
    let dir = common::tempdir().expect("tempdir");
    let scripts = dir.path().join("scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    let ci_local = scripts.join("ci-local.sh");
    std::fs::copy(ci_local_sh(), &ci_local).unwrap();
    let wine = scripts.join("windows-wine.sh");
    std::fs::write(
        &wine,
        "#!/bin/sh\nprintf 'STUB_WINDOWS_WINE: %s\\n' \"$*\"\nexit \"${CI_LOCAL_TEST_WINE_EXIT:-0}\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wine, std::fs::Permissions::from_mode(0o755)).unwrap();
    (dir, ci_local)
}

#[test]
fn default_run_never_runs_the_wine_stage() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let (_scripts, ci_local) = ci_local_beside_a_stub_wine_script();
    let out = run_script(&ci_local, fixture.path(), stub.path(), &[], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        !stdout.contains("STUB_WINDOWS_WINE") && !stdout.contains(WINE_HEADER),
        "the default run reached the opt-in wine stage:\n{stdout}"
    );
    assert!(
        stdout.trim_end().ends_with("All checks passed."),
        "{stdout}"
    );
}

#[test]
fn wine_stage_runs_its_script_when_named() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let (_scripts, ci_local) = ci_local_beside_a_stub_wine_script();
    let out = run_script(
        &ci_local,
        fixture.path(),
        stub.path(),
        &["--stages=windows-wine"],
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains(WINE_HEADER), "{stdout}");
    assert_eq!(stdout.matches("STUB_WINDOWS_WINE:").count(), 1, "{stdout}");
    assert!(!stdout.contains("STUB_CARGO:"), "{stdout}");
    assert!(
        stdout
            .trim_end()
            .ends_with("All checks passed (stages: windows-wine)."),
        "{stdout}"
    );
}

#[test]
fn a_red_wine_stage_fails_the_run() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let (_scripts, ci_local) = ci_local_beside_a_stub_wine_script();
    let out = run_script(
        &ci_local,
        fixture.path(),
        stub.path(),
        &["--stages=windows-wine"],
        &[("CI_LOCAL_TEST_WINE_EXIT", "1")],
    );
    assert!(!out.status.success(), "{}", stdout_of(&out));
    assert!(!stdout_of(&out).contains("All checks passed"));
}

/// Seven stages ran, so a count would call this the full gate; one of the
/// seven is the opt-in stage standing in for a default one that never ran.
#[test]
fn the_wine_stage_cannot_stand_in_for_a_default_stage_on_the_terminal_line() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let (_scripts, ci_local) = ci_local_beside_a_stub_wine_script();
    let out = run_script(
        &ci_local,
        fixture.path(),
        stub.path(),
        &["--stages=check,windows,test,clippy,doc,fmt,windows-wine"],
        &[("CI_LOCAL_TEST_WINDOWS_TARGET_INSTALLED", "1")],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert_eq!(
        stdout.trim_end().lines().last().unwrap_or(""),
        "All checks passed (stages: check,windows,test,clippy,doc,fmt,windows-wine).",
        "{stdout}"
    );
}

#[test]
fn wine_stage_refuses_with_the_install_command_when_the_target_is_missing() {
    let fixture = fixture_repo();
    let stub = stub_bin_dir();
    let out = run_ci_local(
        fixture.path(),
        stub.path(),
        &["--stages=windows-wine"],
        &[("CI_LOCAL_TEST_WINDOWS_TARGET_INSTALLED", "1")],
    );
    assert!(!out.status.success(), "{}", stdout_of(&out));
    assert!(stdout_of(&out).contains(WINE_HEADER), "{}", stdout_of(&out));
    assert!(
        stderr_of(&out).contains(
            "windows-wine: x86_64-pc-windows-gnu is not installed — rustup target add x86_64-pc-windows-gnu"
        ),
        "{}",
        stderr_of(&out)
    );
    assert!(
        !stdout_of(&out).contains("STUB_CARGO:"),
        "{}",
        stdout_of(&out)
    );
}

const STUB_CARGO_PROBING_DOC: &str = "#!/bin/sh\n\
cp docs/note.md \"$CI_LOCAL_TEST_PROBE\"\n\
sleep 1\n\
git status --porcelain > /dev/null\n\
exit \"$CI_LOCAL_TEST_CARGO_EXIT\"\n";

/// Runs the test-autocrlf stage over a fixture whose tracked `docs/note.md`
/// is committed LF, with a stub `cargo` that copies that file aside when the
/// stage runs it. Returns the bytes the stage's cargo saw, the bytes left on
/// disk afterward, and `git status --porcelain` afterward. Blind spot: the
/// stage's real `cargo test` is never run, and the fixture holds one text file.
/// The stub sleeps and refreshes the index so the entries are no longer
/// racily clean when the restore runs, as they are after any real test run;
/// without that a no-op restore still rewrites the file.
fn run_test_autocrlf_stage(cargo_exit: &str) -> (Vec<u8>, Vec<u8>, String, Output) {
    let fixture = common::tempdir().expect("tempdir");
    let root = fixture.path();
    std::fs::create_dir_all(root.join("docs")).unwrap();
    std::fs::write(root.join("docs/note.md"), "one\ntwo\n").unwrap();
    common::git_in(root, &["init", "-q", "--initial-branch=main"]);
    common::git_in(root, &["add", "-A"]);
    common::git_in(root, &["commit", "-q", "-m", "init"]);

    let stub = stub_bin_dir();
    std::fs::write(stub.path().join("cargo"), STUB_CARGO_PROBING_DOC).unwrap();
    let probe = common::tempdir().expect("tempdir");
    let probe_file = probe.path().join("seen");
    let out = run_ci_local(
        root,
        stub.path(),
        &["--stages=test-autocrlf"],
        &[
            ("CI_LOCAL_TEST_PROBE", probe_file.to_str().unwrap()),
            ("CI_LOCAL_TEST_CARGO_EXIT", cargo_exit),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ],
    );
    let seen = std::fs::read(&probe_file).unwrap_or_default();
    let after = std::fs::read(root.join("docs/note.md")).unwrap();
    let status = common::git_in(root, &["status", "--porcelain"]);
    (seen, after, status, out)
}

#[test]
fn test_autocrlf_stage_runs_cargo_over_crlf_files_and_restores_lf_on_success() {
    let (seen, after, status, out) = run_test_autocrlf_stage("0");
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        seen, b"one\r\ntwo\r\n",
        "tracked docs file was not CRLF under the stage"
    );
    assert_eq!(
        after, b"one\ntwo\n",
        "tracked docs file was not restored to LF"
    );
    assert_eq!(status, "", "tree not clean after the stage");
}

#[test]
fn test_autocrlf_stage_restores_lf_when_cargo_fails() {
    let (seen, after, status, out) = run_test_autocrlf_stage("1");
    assert!(!out.status.success(), "{}", stdout_of(&out));
    assert_eq!(
        seen, b"one\r\ntwo\r\n",
        "tracked docs file was not CRLF under the stage"
    );
    assert_eq!(after, b"one\ntwo\n", "trap path left the docs file CRLF");
    assert_eq!(status, "", "tree not clean after the failed stage");
}
