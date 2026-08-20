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
/// LIMIT, so it fails when the limit STOPS existing. Both halves are asserted
/// against the same planted file, and it takes both: that the OS will not run
/// it, and that `common::skip_without_tool` nonetheless reports it PRESENT.
/// Comparing `which` against the OS alone would say nothing about the guard —
/// the guard is a different function, and a change to it could not redden a
/// test that never calls it. The gap is currently accepted, and closing it
/// changes what every guard in the corpus means, which is a decision that
/// should arrive as a conversation rather than as a silent divergence.
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
    //
    // ETXTBSY IS RETRIED, and it is not hypothetical here. A sibling test in
    // this binary that forks while this thread still holds the freshly written
    // file open makes the kernel refuse to execute it — a race that appeared
    // the moment this file grew to seven concurrent tests, and that would
    // otherwise redden the control below at random. Retrying is correct rather
    // than tolerant: ETXTBSY says the file could not be TRIED, which is not an
    // answer about whether the OS would run it.
    let run = |exe: &std::path::Path| {
        for _ in 0..50 {
            let done = std::process::Command::new(exe)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            if done
                .as_ref()
                .is_err_and(|e| e.raw_os_error() == Some(libc_etxtbsy()))
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
                continue;
            }
            return done;
        }
        panic!("{} stayed ETXTBSY for a second", exe.display())
    };

    let plant = |name: &str, body: &str, mode: u32| {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    };

    // The guard takes a name, and a name carrying a separator is the one way to
    // ask it about a file this test owns rather than about the machine's PATH.
    fn spelled(p: &std::path::Path) -> &str {
        p.to_str().expect("a fixture path this suite can spell")
    }

    // The control. Without it, "the OS would not run it" is equally true of a
    // fixture directory nothing can be run out of.
    let works = plant("probe-runnable", "#!/bin/sh\nexit 0\n", 0o755);
    let found = which::which_in("probe-runnable", Some(dir), dir)
        .expect("the control must resolve, or every case below is vacuous");
    assert_eq!(
        found, works,
        "the walk must yield the planted file rather than something else on the machine"
    );
    let control = run(&found);
    assert!(
        control.as_ref().is_ok_and(|s| s.success()),
        "a well-formed script in this directory must run, or the disagreements \
         below are a property of the fixture and not of the two resolvers. \
         The control said {control:?}"
    );
    assert!(
        !common::skip_without_tool(spelled(&found)),
        "the guard must report a runnable file present, or its answer below \
         says nothing about what it accepts"
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

        assert!(
            !common::skip_without_tool(spelled(&found)),
            "{name}: the guard reported this file ABSENT, so it is no longer \
             resolving the way `which` does. That is the gap closing, not a \
             fixture problem: the guard now asks something closer to what \
             production asks, and this case should be retired"
        );

        let ran = run(&found);
        if cfg!(target_os = "macos") && ran.as_ref().is_ok_and(|s| s.success()) {
            common::report_skip(&format!(
                "skip-pin case {name}: this host's execvp retries ENOEXEC \
                 through sh, which runs the planted file, so the which-vs-OS \
                 divergence this case pins does not exist here"
            ));
            continue;
        }
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

// ---------------------------------------------------------------------------
// The corpus audit: one spelling, and nothing skips in silence.
// ---------------------------------------------------------------------------
//
// The reporter above is a mechanism nobody is obliged to use. These two pins
// are the obligation: they read the source and fail on a site that announces
// its own way, or does not announce at all.
//
// SCOPE, which is the coverage boundary and is stated rather than inferred.
// Every `.rs` file git tracks under `src/` and `tests/`. `tests/` is read
// whole; `src/` is read only from its inline `#[cfg(test)] mod … {` onward,
// because production reporting that it skipped a repo is not a test declining
// to measure, and the two spell the same word.
//
// RESIDUE, next to the rule where a hand sweep will read it:
//
// - An environment read through `std::env::var` is NOT treated as a probe.
//   The same syntactic shape covers an opt-in suite gate, which must announce,
//   and a re-exec child dispatcher, which must not — four of the latter live
//   in this corpus and nothing in the source separates them. Those gates are
//   held by the spelling pin instead, which is why it reads `src/` too.
// - Probe and announcement each resolve exactly ONE level of local helper.
//   `require_go()` calls `go_on_path()`; `go_is_installed()` calls
//   `which::which`. A guard two helpers deep is invisible here.
// - The comment filter is line-leading `//` only, so a needle inside a block
//   comment or after code on the same line is read as source.

/// `ETXTBSY`. Not worth a dependency for one number, and it is the same on
/// every unix this suite builds for.
#[cfg(unix)]
fn libc_etxtbsy() -> i32 {
    26
}

/// One file's source, and where its test region starts.
struct Scanned {
    rel: String,
    lines: Vec<String>,
    test_from: usize,
}

fn scanned_corpus() -> Vec<Scanned> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = Command::new("git")
        .args(["ls-files", "src", "tests"])
        .current_dir(root)
        .output()
        .expect("git ls-files should run");
    assert!(
        listed.status.success(),
        "the corpus is read from git's index; without it this pin measures nothing"
    );

    let mut out = Vec::new();
    for rel in String::from_utf8_lossy(&listed.stdout).lines() {
        if !rel.ends_with(".rs") {
            continue;
        }
        let lines: Vec<String> = std::fs::read_to_string(root.join(rel))
            .unwrap_or_else(|e| panic!("read {rel}: {e}"))
            .lines()
            .map(str::to_owned)
            .collect();

        // `src/` is production until an inline `#[cfg(test)]` OPENS a module
        // body — the same boundary the generator's own scope check uses. A
        // `#[cfg(test)] mod tests;` declaration is not one.
        let test_from = if rel.starts_with("tests/") {
            0
        } else {
            let mut at = lines.len();
            for (i, l) in lines.iter().enumerate() {
                if l.trim() != "#[cfg(test)]" {
                    continue;
                }
                let next = lines[i + 1..].iter().find(|x| !x.trim().is_empty());
                if next.is_some_and(|n| {
                    let t = n.trim_start();
                    (t.starts_with("mod ") || t.starts_with("pub mod ")) && t.ends_with('{')
                }) {
                    at = i;
                    break;
                }
            }
            at
        };
        out.push(Scanned {
            rel: rel.to_owned(),
            lines,
            test_from,
        });
    }
    assert!(
        out.len() > 100,
        "the corpus walk yielded {} files, which is not this repository — a pin \
         that reads nothing reports a clean corpus",
        out.len()
    );
    out
}

fn is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

/// Every skip announcement goes through one reporter, so there is one spelling
/// by construction rather than by everyone remembering.
///
/// The defect this catches is not untidiness. `eprintln!` is intercepted by
/// libtest and discarded for a test that PASSES, which every skipping test
/// does — so a site that announces its own way announces to nobody, and reads
/// in the source as though it had been handled.
#[test]
fn every_skip_announcement_goes_through_the_one_reporter() {
    let mut local = Vec::new();
    let mut through_the_reporter = 0usize;

    for file in scanned_corpus() {
        for (n, line) in file.lines.iter().enumerate() {
            if n < file.test_from || is_comment(line) {
                continue;
            }
            if line.contains("report_skip(") {
                through_the_reporter += 1;
            }
            let Some((_, after)) = line
                .split_once("eprintln!(")
                .or_else(|| line.split_once("println!("))
            else {
                continue;
            };
            let literal = after.trim_start().strip_prefix('"').unwrap_or("");
            if literal.len() >= 4 && literal[..4].eq_ignore_ascii_case("skip") {
                local.push(format!("{}:{}  {}", file.rel, n + 1, line.trim()));
            }
        }
    }

    assert!(
        through_the_reporter > 10,
        "only {through_the_reporter} calls to the reporter were found, so the walk \
         is not reading the corpus and the emptiness below is vacuous"
    );
    assert!(
        local.is_empty(),
        "a skip announced with a print macro is discarded by libtest's capture for \
         the passing test that wrote it. Route it through `common::report_skip` \
         (under `tests/`) or `crate::report_skip` (an inline `#[cfg(test)]` module, \
         which cannot name `common`). Sites:\n{}",
        local.join("\n")
    );
}

/// A test whose environment decided it would not run says so.
///
/// The population is every bare `return;` in test scope whose enclosing guard
/// was opened by an environment probe. Keyed on the RETURN rather than on the
/// probe: a guard hides behind a helper or a macro, and a scan keyed on
/// `which::which` finds the helper's definition rather than its callers — which
/// is how two earlier counts of this same population disagreed.
///
/// A block that asserts is measuring, not skipping, and is not a finding.
///
/// **`PROBES` IS THE POPULATION, AND IT IS NARROWER THAN "ENVIRONMENTAL".** A
/// return guarded by anything not in that list is invisible here, however
/// plainly its test is skipping. The known case is in this repository already:
/// the two sites in `tests/status_broken_clone_test.rs` decide on
/// `if cat_file.success()`, the exit status of a `git cat-file` probing whether
/// GC has collected an object. That is an environment question — those two skip
/// on this host today — but the guard names a local variable, so neither the
/// probe list nor the one level of helper resolution reaches the subprocess
/// behind it, and stripping their announcement leaves this test green.
///
/// The list is not widened to close that, deliberately: `PROBES` is matched
/// against the guard line, and widening it to reach that shape would match
/// every `Command` status check in the corpus, most of which are measuring
/// rather than skipping. The boundary is stated instead, because a structural
/// pin's scope IS its coverage claim.
#[test]
fn no_environment_guard_returns_without_announcing() {
    const PROBES: &[&str] = &[
        "which::which",
        "skip_without_tool(",
        "go_on_path(",
        "cfg!(windows)",
        "cfg!(target_os",
        "Command::new(",
    ];
    const ANNOUNCEMENTS: &[&str] = &["report_skip", "skip_without_tool"];

    let mut silent = Vec::new();
    let mut guarded = 0usize;

    for file in scanned_corpus() {
        // One level of local helper, resolved per file: the names of `fn`s in
        // this file whose body carries a primitive, and the names of those that
        // announce. `require_go` is both.
        let mut probing = Vec::new();
        let mut announcing = Vec::new();
        let mut current: Option<(String, usize)> = None;
        for (n, line) in file.lines.iter().enumerate() {
            if let Some(rest) = line.trim_start().split_once("fn ") {
                let name: String = rest
                    .1
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    current = Some((name, n));
                }
            }
            let Some((name, _)) = &current else { continue };
            if PROBES.iter().any(|p| line.contains(p)) && !is_comment(line) {
                probing.push(format!("{name}("));
            }
            if ANNOUNCEMENTS.iter().any(|a| line.contains(a)) && !is_comment(line) {
                announcing.push(format!("{name}("));
            }
        }

        for (n, line) in file.lines.iter().enumerate() {
            if n < file.test_from || line.trim() != "return;" {
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            let Some(opened) = (n.saturating_sub(40)..n).rev().find(|&k| {
                let prev = &file.lines[k];
                !prev.trim().is_empty()
                    && prev.len() - prev.trim_start().len() < indent
                    && prev.trim_end().ends_with('{')
            }) else {
                continue;
            };
            let block = file.lines[opened..=n].join("\n");
            let guard = &file.lines[opened];

            let by_probe = PROBES.iter().any(|p| guard.contains(p))
                || probing.iter().any(|h| guard.contains(h.as_str()));
            if !by_probe || block.contains("assert") {
                continue;
            }
            guarded += 1;

            let announced = ANNOUNCEMENTS.iter().any(|a| block.contains(a))
                || announcing.iter().any(|h| guard.contains(h.as_str()));
            if !announced {
                silent.push(format!(
                    "{}:{}  guard at :{}  {}",
                    file.rel,
                    n + 1,
                    opened + 1,
                    guard.trim()
                ));
            }
        }
    }

    assert!(
        guarded > 20,
        "only {guarded} environment-guarded returns were found, so the walk is not \
         reaching this corpus and the emptiness below is vacuous"
    );
    assert!(
        silent.is_empty(),
        "a test that returns because of its environment must say so, or a green run \
         reports that it ran rather than what it measured. Announce through \
         `common::report_skip` / `common::skip_without_tool` (`tests/`) or \
         `crate::report_skip` (an inline `#[cfg(test)]` module). Sites:\n{}",
        silent.join("\n")
    );
}
