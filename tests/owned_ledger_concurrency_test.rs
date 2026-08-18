//! Two rwv processes stamping different files into one directory's
//! owned-digest ledger keep both entries.
//!
//! The defect: the ledger is mutated by reading the whole map, inserting one
//! entry and publishing the whole map back. Atomic publication settles what a
//! reader sees and nothing about what a writer loses, so two processes whose
//! reads both precede either publish leave exactly one entry behind — and the
//! losing stamp returns `Ok`, so no surface anywhere reports the loss. The
//! entry that goes missing is what the drift and staleness axes read, so the
//! file it described is silently unattested from then on.
//!
//! **Processes, not threads.** An in-process mutex would satisfy a threaded
//! drive while leaving the reachable topology — two `rwv` invocations against
//! one workspace — exactly as lossy as before. Each stamper here is a separate
//! operating-system process, spawned by re-invoking this test binary with only
//! [`stamper_child`] selected, mirroring `src/parallel.rs`'s fixture-child
//! pattern.
//!
//! **The control and the overlap proof.** N of N retained means nothing on its
//! own: a drive whose stampers never overlapped retains N of N with no
//! exclusion at all. So the serial drive runs alongside as the control, and
//! each child records the wall-clock window of its own stamp call. The
//! concurrent windows must share a common instant — every stamp in flight at
//! once — and the serial windows must not.

mod common;

use repoweave::owned_state::{attested_owned_files, stamp_owned_digest};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How many stampers each drive runs. The measurement that filed this defect
/// used eight.
const STAMPERS: usize = 8;

/// Where the child stamps, what it stamps, where it records its window, and
/// the file whose appearance releases it.
const CHILD_DIR: &str = "RWV_LEDGER_TEST_DIR";
const CHILD_ENTRY: &str = "RWV_LEDGER_TEST_ENTRY";
const CHILD_WINDOWS: &str = "RWV_LEDGER_TEST_WINDOWS";
const CHILD_BARRIER: &str = "RWV_LEDGER_TEST_BARRIER";

/// One stamp's wall-clock window, in nanoseconds since the epoch.
#[derive(Debug, Clone, Copy)]
struct Window {
    start: u128,
    end: u128,
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock at or after the epoch")
        .as_nanos()
}

/// Not a test: the child half of the drives below. Inert in an ordinary suite
/// run — without [`CHILD_DIR`] set it returns immediately — and a single
/// stamper when a parent selects it by name.
///
/// A barrier, when named, is a file the parent creates once every child is
/// spawned. Waiting on it with a spin rather than a sleep is what keeps the
/// children's start times within microseconds of each other, so the recorded
/// windows measure contention rather than staggered launches.
#[test]
fn stamper_child() {
    let Ok(dir) = std::env::var(CHILD_DIR) else {
        return;
    };
    let entry = std::env::var(CHILD_ENTRY).expect("a stamper child is given an entry name");

    if let Ok(barrier) = std::env::var(CHILD_BARRIER) {
        let barrier = PathBuf::from(barrier);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !barrier.exists() {
            assert!(
                Instant::now() < deadline,
                "the parent never opened the barrier at {}",
                barrier.display()
            );
            std::hint::spin_loop();
        }
    }

    let start = now_nanos();
    stamp_owned_digest(Path::new(&dir), &entry, entry.as_bytes())
        .unwrap_or_else(|e| panic!("stamping {entry} must succeed: {e:#}"));
    let end = now_nanos();

    let windows = std::env::var(CHILD_WINDOWS).expect("a stamper child records its window");
    std::fs::write(Path::new(&windows).join(&entry), format!("{start} {end}"))
        .expect("window record must be writable");
}

/// A `Command` re-invoking this test binary with only [`stamper_child`]
/// selected, pointed at `dir` and stamping `entry`.
fn stamper(dir: &Path, windows: &Path, entry: &str, barrier: Option<&Path>) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
    cmd.args(["--exact", "stamper_child", "--test-threads=1"]);
    cmd.env(CHILD_DIR, dir);
    cmd.env(CHILD_WINDOWS, windows);
    cmd.env(CHILD_ENTRY, entry);
    // Piped rather than inherited: a child's own libtest summary in this
    // test's output reads as a suite result, and the capture is what carries
    // a failing child's panic message into the assertion below.
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    match barrier {
        Some(path) => cmd.env(CHILD_BARRIER, path),
        None => cmd.env_remove(CHILD_BARRIER),
    };
    cmd
}

/// The child's own output, for an assertion that has to say why it failed.
fn transcript(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn entry_name(i: usize) -> String {
    format!("stamped-{i}.lock")
}

/// The window each child recorded, in the order the entries were requested.
fn windows_in(dir: &Path) -> Vec<Window> {
    (0..STAMPERS)
        .map(|i| {
            let path = dir.join(entry_name(i));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("child {i} recorded no window at {path:?}: {e}"));
            let (start, end) = text.split_once(' ').expect("window is `start end`");
            Window {
                start: start.parse().expect("start nanos"),
                end: end.parse().expect("end nanos"),
            }
        })
        .collect()
}

/// Every stamper alive at one instant, which is what makes an N-of-N result
/// evidence rather than a report that the drive serialised itself.
fn share_an_instant(windows: &[Window]) -> bool {
    let latest_start = windows.iter().map(|w| w.start).max().expect("non-empty");
    let earliest_end = windows.iter().map(|w| w.end).min().expect("non-empty");
    latest_start < earliest_end
}

fn ledger_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = common::tempdir().expect("tempdir");
    let ledger = tmp.path().join("ledger");
    let windows = tmp.path().join("windows");
    std::fs::create_dir_all(&ledger).expect("ledger dir");
    std::fs::create_dir_all(&windows).expect("windows dir");
    (tmp, ledger, windows)
}

#[test]
fn concurrent_processes_keep_every_stamp_and_serial_ones_are_the_control() {
    // Control first: the same stampers, one at a time. Its N of N is what
    // makes the concurrent N of N mean something.
    let (_serial_tmp, serial_ledger, serial_windows) = ledger_dirs();
    for i in 0..STAMPERS {
        let out = stamper(&serial_ledger, &serial_windows, &entry_name(i), None)
            .output()
            .expect("serial stamper spawns");
        assert!(
            out.status.success(),
            "serial stamper {i} failed: {}\n{}",
            out.status,
            transcript(&out)
        );
    }
    assert_eq!(
        attested_owned_files(&serial_ledger).len(),
        STAMPERS,
        "the serial drive must retain every stamp; if it does not, the \
         concurrent result below is measuring something other than contention"
    );
    let serial = windows_in(&serial_windows);
    assert!(
        !share_an_instant(&serial),
        "the control must not overlap — if these windows share an instant the \
         drive did not run serially and is no control at all: {serial:?}"
    );

    // The drive: all N released together, contending on one ledger.
    let (_tmp, ledger, windows) = ledger_dirs();
    let barrier = windows.join("go");
    let children: Vec<_> = (0..STAMPERS)
        .map(|i| {
            stamper(&ledger, &windows, &entry_name(i), Some(&barrier))
                .spawn()
                .expect("concurrent stamper spawns")
        })
        .collect();
    std::fs::write(&barrier, b"go").expect("barrier must be writable");
    for (i, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().expect("concurrent stamper exits");
        assert!(
            out.status.success(),
            "concurrent stamper {i} failed: {}\n{}",
            out.status,
            transcript(&out)
        );
    }

    let retained = attested_owned_files(&ledger);
    let concurrent = windows_in(&windows);
    assert!(
        share_an_instant(&concurrent),
        "no instant had every stamp in flight, so this drive did not exercise \
         the read-modify-write against itself and its result is not evidence \
         either way: {concurrent:?}"
    );
    assert_eq!(
        retained.len(),
        STAMPERS,
        "every concurrent stamp must survive: a whole-map publish that lost \
         one returns Ok, so the entry is gone with nothing reporting it. \
         Retained {retained:?}"
    );
}
