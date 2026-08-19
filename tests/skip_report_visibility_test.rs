//! A test that returns without measuring anything reports `ok`, and the
//! default summary has no other outcome to give it. `common::report_skip` is
//! the corpus's answer: a notice printed beside the `ok` line that contradicts
//! it. This pins the only property that makes the notice worth writing — that
//! it survives the capture which discards `eprintln!` from a passing test.
//!
//! Nothing observable from inside a test can measure that. libtest installs
//! the capture around the test that is running, so a test reading back its own
//! output sees what it wrote either way. The two subjects below are therefore
//! driven as a subprocess: this same binary, re-invoked with a single-test
//! filter and *without* `--nocapture`, is exactly the run whose silence is the
//! defect.
//!
//! The subjects are ordinary members of the suite, not fixtures the driver
//! switches on. So every full run prints one notice for a tool that cannot
//! exist, which is what keeps the reporter from decaying into a mechanism
//! nobody has seen fire.

use std::process::{Command, Output};

mod common;

/// A name no PATH lookup can satisfy.
const ABSENT_TOOL: &str = "rwv-absent-tool-probe";

/// Re-invoke this binary for one test, under the default capture.
fn run_subject(test: &str) -> Output {
    let exe = std::env::current_exe().expect("this test binary's own path");
    Command::new(&exe)
        .args([test, "--exact", "--test-threads=1"])
        .output()
        .unwrap_or_else(|e| panic!("re-invoking {} failed: {e}", exe.display()))
}

// ---------------------------------------------------------------------------
// Subjects — driven both as ordinary tests and as the driver's subprocess.
// ---------------------------------------------------------------------------

/// The reporting direction: an absent tool is announced, and the test passes.
#[test]
fn a_tool_that_cannot_exist_is_announced() {
    if common::skip_without_tool(ABSENT_TOOL) {
        return;
    }
    panic!("`{ABSENT_TOOL}` resolved on PATH; the probe name is no longer absent");
}

/// The silent direction: a tool that is there is not announced, and the caller
/// is told to carry on.
#[test]
fn a_tool_that_is_there_is_not_announced() {
    let exe = std::env::current_exe().expect("this test binary's own path");
    let exe = exe
        .to_str()
        .expect("a test binary path this suite can spell");
    assert!(
        !common::skip_without_tool(exe),
        "{exe} is running, so the probe must report it present"
    );
}

// ---------------------------------------------------------------------------
// The predicate's own limit.
// ---------------------------------------------------------------------------

/// `common::skip_without_tool` resolves with `which`, which walks PATH and
/// applies its own notion of executability. Production spawns and lets the OS
/// decide. Those are two predicates, and this plants the entries they disagree
/// on.
///
/// WHY IT IS WORTH A TEST. On such an entry the guard reports the tool PRESENT,
/// so a guarded test does not skip; the spawn then fails inside the integration
/// and surfaces as that integration's own error on a host where the tool was
/// declared available. Driving `doctor_workweave_cargo_lock_fix_test` with a
/// broken-interpreter `cargo` on PATH produces exactly that: `cargo-workspace:
/// activate hook failed: failed to run cargo`, from a test whose guard had just
/// said cargo was there.
///
/// READ THE ASSERTION DIRECTION BEFORE CHANGING ANYTHING. This pins a KNOWN
/// LIMIT, so it fails when the limit STOPS existing — someone who makes the
/// guard ask what production asks reddens this test. That is the intent: the
/// gap is currently accepted, and closing it changes what every guard in the
/// corpus means, which is a decision that should arrive as a conversation
/// rather than as a silent divergence.
///
/// The helper resolves against the process's own PATH, which no test may narrow
/// — `set_var` is unsound under a parallel runner. So the predicate is exercised
/// through `which_in` against a directory this test owns: the same executability
/// question, asked of a path source the test can choose. The spawn half then
/// runs the exact file `which_in` handed back, so what is compared is one file
/// and not two lookups.
///
/// Unix only: the shapes are permission bits and interpreter lines, and what
/// Windows accepts as executable is a different question under `PATHEXT`.
#[cfg(unix)]
#[test]
fn the_guards_resolver_accepts_files_the_os_will_not_run() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = common::tempdir().unwrap();
    let dir = tmp.path();

    // The child's own diagnostics go nowhere: one of these shapes reaches a
    // shell that reports it cannot open the file, and a gate log carrying that
    // line beside a passing test is a reader's false alarm.
    let run = |exe: &std::path::Path| {
        std::process::Command::new(exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    };

    let plant = |name: &str, body: &str, mode: u32| {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    };

    // The control. Without it, "the OS would not run it" is equally true of a
    // fixture directory nothing can be run out of.
    let works = plant("probe-runnable", "#!/bin/sh\nexit 0\n", 0o755);
    let found = which::which_in("probe-runnable", Some(dir), dir)
        .expect("the control must resolve, or every case below is vacuous");
    assert_eq!(
        found, works,
        "the walk must yield the planted file rather than something else on the machine"
    );
    assert!(
        run(&found).is_ok_and(|s| s.success()),
        "a well-formed script in this directory must run, or the disagreements \
         below are a property of the fixture and not of the two resolvers"
    );

    let unrunnable = [
        (
            "probe-broken-interpreter",
            "#!/nonexistent/interpreter\nexit 0\n",
            0o755,
        ),
        (
            "probe-write-only-to-its-interpreter",
            "#!/bin/sh\nexit 0\n",
            0o111,
        ),
        (
            "probe-neither-script-nor-binary",
            "this is not a program\n",
            0o755,
        ),
        ("probe-empty", "", 0o755),
    ];
    for (name, body, mode) in unrunnable {
        let planted = plant(name, body, mode);
        let found = which::which_in(name, Some(dir), dir).unwrap_or_else(|e| {
            panic!(
                "`which` must still accept {name}: this test exists because it does, \
                 and an `{e}` here means the predicate moved and the guard's blind \
                 spot with it — reread common::skip_without_tool's doc comment"
            )
        });
        assert_eq!(found, planted, "the walk must yield the planted file");

        let ran = run(&found);
        assert!(
            !ran.as_ref().is_ok_and(|s| s.success()),
            "{name}: `which` accepted this file and the OS ran it successfully, so \
             the two resolvers now agree here. That is the gap closing, not a \
             fixture problem: check whether common::skip_without_tool now spawns \
             what production spawns, and retire this case if it does. Got {ran:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Driver — reads the subjects' output from outside their capture.
// ---------------------------------------------------------------------------

/// A skip reaches a reader who did not ask for `--nocapture`.
///
/// This is the whole mechanism: swap the write in `common::report_skip` for an
/// `eprintln!` and the notice below vanishes while the subject still reports
/// `ok` — the state the notice exists to end.
#[test]
fn a_skip_notice_survives_the_default_capture() {
    let out = run_subject("a_tool_that_cannot_exist_is_announced");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("1 passed"),
        "the subject must have run and passed, or the notice below is unreached:\
         \nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("SKIP: `{ABSENT_TOOL}` not found on PATH")),
        "a skipped test's notice must reach a run that did not pass --nocapture:\
         \nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}

/// A test that measured something says nothing.
///
/// The reachability half: `1 passed` proves the child ran, so the silence
/// below is a measured absence rather than a subprocess that never started.
#[test]
fn a_test_that_did_not_skip_prints_no_notice() {
    let out = run_subject("a_tool_that_is_there_is_not_announced");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stdout.contains("1 passed"),
        "the subject must have run and passed, or the silence below is vacuous:\
         \nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        !stdout.contains("SKIP:") && !stderr.contains("SKIP:"),
        "a test that measured something must not announce a skip:\
         \nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
