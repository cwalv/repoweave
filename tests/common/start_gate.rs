//! A start gate for fixture children that must begin together.
//!
//! Each child signals that it has reached the gate before it waits on it, and
//! the parent opens the gate only once every child has signalled. A child
//! therefore never finds the gate open on arrival, so the children's start
//! times are within microseconds of each other whatever the host's process
//! startup skew.

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
