//! An index write and a ledger stamp in one project keep every ignore line.
//!
//! The two records share a directory: the workweave index and the owned-digest
//! ledger are both files of `projects/<p>/`, so their VCS-hygiene appends land
//! on one ignore surface — `.git/info/exclude` when the project is a git
//! checkout. Each writer runs under the claim of its own record (`IndexClaim`,
//! `LedgerClaim`); nothing serialises the two families against each other, and
//! they contribute different names. A surface republished whole-file from a
//! read that predates the other family's append drops that family's names with
//! both processes exiting zero, and the next write of the losing family
//! restores them, so no durable record marks the window. The append must
//! therefore be line-granular: an append cannot drop a peer's line, whatever
//! the interleaving.
//!
//! **Processes, not threads.** An in-process mutex would satisfy a threaded
//! drive while leaving the reachable topology — two `rwv` invocations against
//! one project — exactly as lossy as before. Each writer here is a separate
//! operating-system process, spawned by re-invoking this test binary with one
//! child selected, mirroring `src/parallel.rs`'s fixture-child pattern.
//!
//! **Control, rounds, and what overlap proves.** The serial control's four of
//! four names and disjoint windows are what make the driven rounds evidence.
//! Each driven round records both calls' wall-clock windows; a shared instant
//! proves the calls were in flight together, which is necessary for the append
//! phases to have interleaved but not sufficient, so the drive runs many
//! rounds and requires only that at least one overlapped — retention is
//! asserted in every round regardless.

mod common;

use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::owned_state::stamp_owned_digest;
use repoweave::vcs::{EphemeralRefName, ResolvedRevisionId};
use repoweave::workweave_index::RefRegistry;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Driven rounds: one index write racing one ledger stamp on a fresh fixture.
/// Under [`BALLAST_LINES`]'s window a whole-file publish loses a name in
/// nearly every round, so this many is ample margin over a regression.
const ROUNDS: usize = 8;

/// Lines of ballast in each fixture's pre-state exclude file. Each hygiene
/// append reads the whole surface first, so a writer that republishes the
/// whole surface holds a read-to-write window open in proportion to the
/// pre-state — milliseconds at this size, wide enough that two writers
/// released together land inside it without any scheduling control, on
/// either build profile. A line-granular append keeps no such window.
const BALLAST_LINES: usize = 100_000;

const RECEIPT_ROOT: &str = "RWV_IGNORE_TEST_RECEIPT_ROOT";
const RECEIPT_STORE: &str = "RWV_IGNORE_TEST_RECEIPT_STORE";
const STAMP_DIR: &str = "RWV_IGNORE_TEST_STAMP_DIR";
const CHILD_WINDOWS: &str = "RWV_IGNORE_TEST_WINDOWS";
const CHILD_WHO: &str = "RWV_IGNORE_TEST_WHO";
const CHILD_BARRIER: &str = "RWV_IGNORE_TEST_BARRIER";

const PROJECT: &str = "web-app";

/// Every name the two writers contribute to the shared surface: the index
/// family, then the ledger family.
const EXPECTED_LINES: [&str; 4] = [
    ".rwv-workweave-index",
    ".rwv-workweave-index.lock",
    ".rwv-owned-digests",
    ".rwv-owned-digests.lock",
];

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock at or after the epoch")
        .as_nanos()
}

fn wait_for_barrier() {
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
}

fn record_window(start: u128, end: u128) {
    let windows = std::env::var(CHILD_WINDOWS).expect("a child records its window");
    let who = std::env::var(CHILD_WHO).expect("a child is told its window file");
    std::fs::write(Path::new(&windows).join(&who), format!("{start} {end}"))
        .expect("window record must be writable");
}

/// Not a test: the index-writer half of the drive. Inert in an ordinary suite
/// run — without [`RECEIPT_ROOT`] set it returns immediately.
#[test]
fn receipt_child() {
    let Ok(root) = std::env::var(RECEIPT_ROOT) else {
        return;
    };
    let store = std::env::var(RECEIPT_STORE).expect("a receipt child is given a store");
    wait_for_barrier();

    let project = ProjectName::new(PROJECT).expect("project name");
    let mut registry = RefRegistry::for_project(Path::new(&root), &project);
    let minted = EphemeralRefName::mint(
        &project,
        &WorkweaveName::new("seat").expect("workweave name"),
    );
    let at = ResolvedRevisionId::from_canonical("a".repeat(40), None);

    let start = now_nanos();
    registry
        .record_created(Path::new(&store), minted, at)
        .unwrap_or_else(|e| panic!("recording a receipt must succeed: {e:#}"));
    let end = now_nanos();
    record_window(start, end);
}

/// Not a test: the ledger-stamper half of the drive. Inert in an ordinary
/// suite run — without [`STAMP_DIR`] set it returns immediately.
#[test]
fn stamp_child() {
    let Ok(dir) = std::env::var(STAMP_DIR) else {
        return;
    };
    wait_for_barrier();

    let start = now_nanos();
    stamp_owned_digest(Path::new(&dir), "Cargo.lock", b"version = 4\n")
        .unwrap_or_else(|e| panic!("stamping must succeed: {e:#}"));
    let end = now_nanos();
    record_window(start, end);
}

/// A `Command` re-invoking this test binary with only `child` selected.
fn child_command(child: &str, windows: &Path, who: &str, barrier: Option<&Path>) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
    cmd.args(["--exact", child, "--test-threads=1"]);
    cmd.env(CHILD_WINDOWS, windows);
    cmd.env(CHILD_WHO, who);
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

fn transcript(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

struct Fixture {
    root: PathBuf,
    project_dir: PathBuf,
    store: PathBuf,
    windows: PathBuf,
}

/// A primary root whose `projects/<PROJECT>/` is a git checkout holding a
/// current-shape index — the observed production shape, where both writers'
/// hygiene appends resolve to the checkout's `.git/info/exclude`.
fn seeded_fixture(tmp: &Path, tag: &str) -> Fixture {
    let root = tmp.join(tag);
    let project_dir = root.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project_dir).expect("project dir");
    common::git_in(&project_dir, &["init"]);
    let mut ballast = String::with_capacity(BALLAST_LINES * 32);
    for line in 0..BALLAST_LINES {
        ballast.push_str(&format!("# operator exclude entry {line}\n"));
    }
    let exclude = project_dir.join(".git").join("info").join("exclude");
    std::fs::create_dir_all(exclude.parent().expect("info dir")).expect("info dir");
    std::fs::write(&exclude, ballast).expect("seed exclude ballast");
    std::fs::write(
        project_dir.join(".rwv-workweave-index"),
        format!(
            r#"{{"container":{:?},"workweaves":{{}},"receipts":[]}}"#,
            root.join(".workweaves").to_string_lossy()
        ),
    )
    .expect("seed index");
    let store = root.join("store");
    std::fs::create_dir_all(&store).expect("store dir");
    let windows = root.join("windows");
    std::fs::create_dir_all(&windows).expect("windows dir");
    Fixture {
        root,
        project_dir,
        store,
        windows,
    }
}

/// The names from [`EXPECTED_LINES`] that the fixture's exclude file lacks.
fn missing_lines(fixture: &Fixture) -> Vec<&'static str> {
    let exclude = fixture
        .project_dir
        .join(".git")
        .join("info")
        .join("exclude");
    let text = std::fs::read_to_string(&exclude)
        .unwrap_or_else(|e| panic!("exclude at {} must be readable: {e}", exclude.display()));
    let present: Vec<&str> = text.lines().map(str::trim).collect();
    EXPECTED_LINES
        .iter()
        .copied()
        .filter(|name| !present.contains(name))
        .collect()
}

fn window_in(dir: &Path, who: &str) -> (u128, u128) {
    let path = dir.join(who);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{who} recorded no window at {path:?}: {e}"));
    let (s, e) = text.split_once(' ').expect("window is `start end`");
    (
        s.parse().expect("start nanos"),
        e.parse().expect("end nanos"),
    )
}

/// Both calls in flight at one instant.
fn share_an_instant(a: (u128, u128), b: (u128, u128)) -> bool {
    a.0.max(b.0) < a.1.min(b.1)
}

fn run_child(fixture: &Fixture, child: &str, barrier: Option<&Path>) -> Command {
    let mut cmd = child_command(child, &fixture.windows, child, barrier);
    match child {
        "receipt_child" => {
            cmd.env(RECEIPT_ROOT, &fixture.root);
            cmd.env(RECEIPT_STORE, &fixture.store);
        }
        "stamp_child" => {
            cmd.env(STAMP_DIR, &fixture.project_dir);
        }
        other => panic!("no child named {other}"),
    }
    cmd
}

#[test]
fn concurrent_index_write_and_ledger_stamp_keep_every_ignore_line() {
    let tmp = common::tempdir().expect("tempdir");

    // Control: the same two writers, one after the other.
    let serial = seeded_fixture(tmp.path(), "serial");
    for child in ["receipt_child", "stamp_child"] {
        let out = run_child(&serial, child, None)
            .output()
            .expect("serial child spawns");
        assert!(
            out.status.success(),
            "serial {child} failed: {}\n{}",
            out.status,
            transcript(&out)
        );
    }
    assert_eq!(
        missing_lines(&serial),
        Vec::<&str>::new(),
        "the serial control must leave every name on the surface; if it does \
         not, the driven rounds below measure something other than concurrency"
    );
    let serial_receipt = window_in(&serial.windows, "receipt_child");
    let serial_stamp = window_in(&serial.windows, "stamp_child");
    assert!(
        !share_an_instant(serial_receipt, serial_stamp),
        "the control must not overlap — if these windows share an instant the \
         drive did not run serially and is no control at all: \
         {serial_receipt:?} {serial_stamp:?}"
    );

    // The drive: one writer of each family released together, per round.
    let mut overlapped = 0usize;
    for round in 0..ROUNDS {
        let fixture = seeded_fixture(tmp.path(), &format!("round{round}"));
        let barrier = fixture.windows.join("go");
        let children: Vec<_> = ["receipt_child", "stamp_child"]
            .iter()
            .map(|child| {
                (
                    *child,
                    run_child(&fixture, child, Some(&barrier))
                        .spawn()
                        .expect("concurrent child spawns"),
                )
            })
            .collect();
        std::fs::write(&barrier, b"go").expect("barrier must be writable");
        for (child, handle) in children {
            let out = handle.wait_with_output().expect("concurrent child exits");
            assert!(
                out.status.success(),
                "round {round} {child} failed: {}\n{}",
                out.status,
                transcript(&out)
            );
        }

        let receipt = window_in(&fixture.windows, "receipt_child");
        let stamp = window_in(&fixture.windows, "stamp_child");
        if share_an_instant(receipt, stamp) {
            overlapped += 1;
        }
        assert_eq!(
            missing_lines(&fixture),
            Vec::<&str>::new(),
            "round {round} lost an ignore line: a publish that dropped it \
             returned Ok on both sides, and the next writer of the losing \
             family silently restores it, so only this drive sees the window. \
             Windows: receipt {receipt:?} stamp {stamp:?}"
        );
    }
    assert!(
        overlapped > 0,
        "no round had both writers in flight at one instant, so this drive \
         never exercised the two claim families against each other and its \
         green is not evidence"
    );
}
