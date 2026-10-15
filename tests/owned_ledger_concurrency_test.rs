//! Two rwv processes stamping different files into one directory's
//! owned-digest ledger keep both entries.
//!
//! The defect: the ledger is mutated by reading the whole map, inserting one
//! entry and publishing the whole map back. Atomic publication settles what a
//! reader sees and nothing about what a writer loses, so two processes whose
//! reads both precede either publish leave exactly one entry behind — and the
//! losing stamp returns `Ok`, so no surface anywhere reports the loss.
//!
//! **Processes, not threads.** An in-process mutex would satisfy a threaded
//! drive while leaving the reachable topology — two `rwv` invocations against
//! one workspace — exactly as lossy as before. Each stamper here is a separate
//! operating-system process, spawned by re-invoking this test binary with only
//! [`stamper_child`] selected.
//!
//! **Exclusion is pinned by holding the claim, not by racing for it.** The
//! ledger's exclusion is a claim file created exclusively; a stamper that
//! cannot create it waits out the claim budget and refuses with
//! `state-claim-held`. The test creates that file itself, so while it holds
//! the claim a stamper that honours the exclusion has exactly one possible
//! outcome — refusal — and one that does not succeeds. Neither outcome depends
//! on how the scheduler interleaves the children, because the holder is the
//! test and releases only after every child has exited.
//!
//! **Convergence is the other half.** With no claim held, N concurrent
//! stampers all succeed and all N entries are retained. That holds under every
//! interleaving for a correct ledger; the exclusion pin carries the proof that
//! it is the claim, not luck, that makes it hold.

mod common;

use repoweave::owned_state::{attested_owned_files, stamp_owned_digest};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How many stampers each drive runs.
const STAMPERS: usize = 8;

const CHILD_DIR: &str = "RWV_LEDGER_TEST_DIR";
const CHILD_ENTRY: &str = "RWV_LEDGER_TEST_ENTRY";
const CHILD_RESULTS: &str = "RWV_LEDGER_TEST_RESULTS";

/// The ledger's claim file. Not exported by the crate, so the literal is
/// spelled here; a wrong spelling holds nothing, the stampers succeed, and the
/// exclusion pin goes red.
const LEDGER_CLAIM_FILE: &str = ".rwv-owned-digests.lock";
const LEDGER_FILE: &str = ".rwv-owned-digests";

/// Not a test: the child half of the drives below. Inert in an ordinary suite
/// run — without [`CHILD_DIR`] set it returns immediately. It stamps one entry
/// and records `ok`, or `refused <kind token>` when the stamp refuses.
#[test]
fn stamper_child() {
    let Ok(dir) = std::env::var(CHILD_DIR) else {
        return;
    };
    let entry = std::env::var(CHILD_ENTRY).expect("a stamper child is given an entry name");
    let results = std::env::var(CHILD_RESULTS).expect("a stamper child records its result");

    let outcome = match stamp_owned_digest(Path::new(&dir), &entry, entry.as_bytes()) {
        Ok(()) => "ok".to_string(),
        Err(e) => match repoweave::refusal::kind_of(&e) {
            Some(kind) => format!("refused {}", kind.token()),
            None => format!("failed {e:#}"),
        },
    };
    std::fs::write(Path::new(&results).join(&entry), outcome)
        .expect("result record must be writable");
}

fn stamper(dir: &Path, results: &Path, entry: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
    cmd.args(["--exact", "stamper_child", "--test-threads=1"]);
    cmd.env(CHILD_DIR, dir);
    cmd.env(CHILD_RESULTS, results);
    cmd.env(CHILD_ENTRY, entry);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

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

fn ledger_dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = common::tempdir().expect("tempdir");
    let ledger = tmp.path().join("ledger");
    let results = tmp.path().join("results");
    std::fs::create_dir_all(&ledger).expect("ledger dir");
    std::fs::create_dir_all(&results).expect("results dir");
    (tmp, ledger, results)
}

/// Spawn every stamper at once and wait for all of them to exit; the result
/// each recorded, in entry order.
fn run_stampers(ledger: &Path, results: &Path) -> Vec<String> {
    let children: Vec<_> = (0..STAMPERS)
        .map(|i| {
            stamper(ledger, results, &entry_name(i))
                .spawn()
                .expect("concurrent stamper spawns")
        })
        .collect();
    for (i, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().expect("concurrent stamper exits");
        assert!(
            out.status.success(),
            "stamper {i} crashed: {}\n{}",
            out.status,
            transcript(&out)
        );
    }
    (0..STAMPERS)
        .map(|i| {
            std::fs::read_to_string(results.join(entry_name(i)))
                .unwrap_or_else(|e| panic!("stamper {i} recorded no result: {e}"))
        })
        .collect()
}

#[test]
fn a_held_claim_refuses_every_stamper_and_leaves_the_ledger_untouched() {
    let (_tmp, ledger, results) = ledger_dirs();
    stamp_owned_digest(&ledger, "seed.lock", b"seed").expect("an unclaimed stamp succeeds");
    let before = std::fs::read(ledger.join(LEDGER_FILE)).expect("seeded ledger");

    let claim = ledger.join(LEDGER_CLAIM_FILE);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&claim)
        .expect("the test takes the claim itself");
    let outcomes = run_stampers(&ledger, &results);
    let refused = format!(
        "refused {}",
        repoweave::refusal::RefusalKind::StateClaimHeld.token()
    );
    assert_eq!(
        outcomes,
        vec![refused; STAMPERS],
        "while a peer holds the claim every stamper must refuse; one that \
         succeeded stamped without the exclusion"
    );
    assert_eq!(
        std::fs::read(ledger.join(LEDGER_FILE)).expect("ledger still present"),
        before,
        "a refused stamp must write nothing"
    );

    std::fs::remove_file(&claim).expect("the test releases the claim");
    stamp_owned_digest(&ledger, "after.lock", b"after")
        .expect("with the claim released a stamp succeeds again");
}

#[test]
fn concurrent_processes_keep_every_stamp() {
    let (_tmp, ledger, results) = ledger_dirs();
    let outcomes = run_stampers(&ledger, &results);
    assert_eq!(outcomes, vec!["ok".to_string(); STAMPERS]);
    let retained = attested_owned_files(&ledger);
    assert_eq!(
        retained.len(),
        STAMPERS,
        "every concurrent stamp must survive: a whole-map publish that lost \
         one returns Ok, so the entry is gone with nothing reporting it. \
         Retained {retained:?}"
    );
}
