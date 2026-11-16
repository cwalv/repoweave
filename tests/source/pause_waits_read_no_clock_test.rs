//! No wait that holds rwv at a pause point, or that drives a scenario or the
//! kill matrix through one, ends on elapsed time.
//!
//! A structural pin, licensed because no behavioral assertion can tell a
//! wait with no deadline from one with a long deadline: a run of any finite
//! length shows only that a deadline, if there is one, is longer. What the
//! wait does end on (a release, its driver gone, the process it waits on
//! exiting) is pinned behaviorally in `src/test_pause.rs` and
//! `tests/process/exclusive_publish_test.rs`.
//!
//! Scope: the code lines, `//` comments stripped, of exactly the files
//! [`HOLDING`] names. A clock read is any of [`CLOCK_READS`]. A fixed sleep
//! inside a poll loop reads no clock and is not seen, and neither is a clock
//! read anywhere else in the suite.

const HOLDING: &[&str] = &[
    "src/test_pause.rs",
    "tests/common/pause.rs",
    "tests/common/scenario_runner.rs",
    "tests/common/scenario_sandbox.rs",
    "tests/common/scenario_invariants.rs",
    "tests/common/scenario_steps.rs",
    "tests/kill_matrix/kill_matrix_test.rs",
];

const CLOCK_READS: &[&str] = &["Instant", "SystemTime", "elapsed", "_timeout("];

fn clock_reads(text: &str) -> Vec<String> {
    text.lines()
        .enumerate()
        .filter_map(|(at, line)| {
            let code = line.split("//").next().unwrap_or_default();
            CLOCK_READS
                .iter()
                .find(|read| code.contains(*read))
                .map(|read| format!("{}: {read}: {}", at + 1, line.trim()))
        })
        .collect()
}

#[test]
fn the_waits_that_hold_rwv_at_a_pause_point_read_no_clock() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found = Vec::new();
    for file in HOLDING {
        let text = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
        assert!(text.contains("fn "), "{file} holds no code");
        found.extend(
            clock_reads(&text)
                .into_iter()
                .map(|at| format!("{file}:{at}")),
        );
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}

#[test]
fn a_deadline_in_a_wait_is_a_clock_read_and_one_in_a_comment_is_not() {
    let text = "let deadline = std::time::Instant::now() + LIMIT;\n\
                // no Instant decides a verdict\n\
                let Ok(out) = rx.recv_timeout(bound) else { panic!() };\n";
    let found = clock_reads(text);
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found[0].starts_with("1: Instant"), "{found:?}");
    assert!(found[1].starts_with("3: _timeout("), "{found:?}");
}
