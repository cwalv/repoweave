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
