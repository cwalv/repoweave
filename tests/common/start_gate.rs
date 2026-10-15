//! A start gate for fixture children that must begin together.
//!
//! Each child signals that it has reached the gate before it waits on it, and
//! the parent opens the gate only once every child has signalled. A child
//! therefore never finds the gate open on arrival, so the children's start
//! times are within microseconds of each other whatever the host's process
//! startup skew.
//!
//! Releasing children together does not make their critical sections overlap:
//! on a saturated host the scheduler can let the first released child finish
//! before the rest run. A drive whose result is only evidence when its windows
//! overlap is therefore repeated, at most [`MAX_ATTEMPTS`] times, by
//! [`drive_until_overlap`]. Every attempt asserts its own invariants whether or
//! not it overlapped, so a lost entry in any attempt fails the test; overlap
//! decides only whether an attempt counts as evidence, and the first attempt
//! that does ends the drive.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(60);

fn ready_path(gate: &Path, who: &str) -> PathBuf {
    gate.with_file_name(format!("ready-{who}"))
}

/// Child side: record whether the gate was already open, signal readiness, and
/// spin until the parent opens it.
pub fn arrive(gate: &Path, who: &str) {
    let already_open = gate.exists();
    std::fs::write(
        ready_path(gate, who),
        if already_open { "open" } else { "closed" },
    )
    .expect("readiness signal must be writable");
    let deadline = Instant::now() + DEADLINE;
    while !gate.exists() {
        assert!(
            Instant::now() < deadline,
            "the parent never opened the gate at {}",
            gate.display()
        );
        std::hint::spin_loop();
    }
}

/// Parent side: the gate file for children sharing `dir`.
pub fn gate_in(dir: &Path) -> PathBuf {
    dir.join("go")
}

/// Parent side: wait until every child in `who` has signalled, then open the
/// gate.
pub fn open_when_all_ready(gate: &Path, who: &[String]) {
    let deadline = Instant::now() + DEADLINE;
    while !who.iter().all(|w| ready_path(gate, w).exists()) {
        assert!(
            Instant::now() < deadline,
            "not every child reached the gate at {}",
            gate.display()
        );
        std::hint::spin_loop();
    }
    std::fs::write(gate, b"go").expect("gate must be writable");
}

/// Parent side, after the children exit: the children that found the gate open
/// on arrival.
pub fn early_arrivals(gate: &Path, who: &[String]) -> Vec<String> {
    who.iter()
        .filter(|w| {
            std::fs::read_to_string(ready_path(gate, w))
                .unwrap_or_else(|e| panic!("child {w} never signalled readiness: {e}"))
                != "closed"
        })
        .cloned()
        .collect()
}

pub const MAX_ATTEMPTS: usize = 10;

/// How far the latest start trails the earliest end, in nanoseconds; negative
/// when some instant had every window in flight.
pub fn overlap_miss(windows: &[(u128, u128)]) -> i128 {
    let latest_start = windows.iter().map(|w| w.0).max().expect("non-empty");
    let earliest_end = windows.iter().map(|w| w.1).min().expect("non-empty");
    latest_start as i128 - earliest_end as i128
}

/// Run `attempt` until one overlaps, returning how many attempts that took.
///
/// `attempt` is a whole drive in fresh directories. It asserts the drive's
/// invariants itself and returns its [`overlap_miss`].
pub fn drive_until_overlap(what: &str, mut attempt: impl FnMut(usize) -> i128) -> usize {
    let mut narrowest = i128::MAX;
    for n in 1..=MAX_ATTEMPTS {
        let miss = attempt(n);
        if miss < 0 {
            return n;
        }
        narrowest = narrowest.min(miss);
    }
    panic!(
        "no instant had every {what} in flight in any of {MAX_ATTEMPTS} attempts \
         (the narrowest miss: the latest start trailed the earliest end by \
         {narrowest} ns), so this drive did not exercise the read-modify-write \
         against itself and its result is not evidence either way"
    );
}
