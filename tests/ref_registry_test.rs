//! The ownership-receipt registry against a real store
//! (`docs/repoweave/branch-model.md` §3.3 R2/R3, §4.2, §7.1).
//!
//! The registry's own file-level behaviour is unit-tested in
//! `src/workweave_index.rs`. What needs a real repo is the half of R2 that
//! is about refs rather than records:
//!
//!   1. **A ref that exists without a receipt is not rwv's.** That is the
//!      state the receipt-first ordering exists to make unreachable, and
//!      asserting it needs a branch that genuinely exists — a fixture where
//!      nothing exists would pass for the wrong reason.
//!   2. **A dangling receipt authorizes nothing.** The benign crash window
//!      is only benign if no warrant can be built against a ref that never
//!      appeared. Each "no warrant" assertion below is paired with the
//!      setup that *does* yield one, so a broken harness cannot make the
//!      negative pass.
//!   3. **The receipt reaches the disk, not just the page cache.** The
//!      ordering rule in §7.1 is a claim about what survives a crash, and a
//!      `write` that returned before `fsync` would satisfy every in-process
//!      assertion in this file while failing the only case it is for. The
//!      last test reads the syscalls.

use repoweave::git::GitVcs;
use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::vcs::{DeletionWarrant, EphemeralRefName, RawRefName, ResolvedRevisionId, Vcs};
use repoweave::workweave_index::RefRegistry;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

mod common;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Run git in `dir`, panicking on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git {:?} failed in {}: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A weave: a primary holding `projects/web-app/`, and one canonical
/// member store with a commit on `main`.
///
/// Returns `(tempdir, primary_root, project, store)`. The store path is
/// canonicalized because that is the spelling the registry records.
fn weave() -> (TempDir, PathBuf, ProjectName, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let primary = tmp.path().join("ws");
    std::fs::create_dir_all(primary.join("projects/web-app")).unwrap();

    let store = tmp.path().join("weave/github/acme/server");
    std::fs::create_dir_all(&store).unwrap();
    git(&store, &["init", "--initial-branch=main"]);
    git(&store, &["config", "user.email", "test@test.com"]);
    git(&store, &["config", "user.name", "Test"]);
    std::fs::write(store.join("one"), "1").unwrap();
    git(&store, &["add", "."]);
    git(&store, &["commit", "-m", "one"]);

    let store = store.canonicalize().unwrap();
    (tmp, primary, ProjectName::new("web-app"), store)
}

fn commit(repo: &Path, file: &str) -> ResolvedRevisionId {
    std::fs::write(repo.join(file), file).unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-m", file]);
    GitVcs.head_revision(repo).unwrap()
}

fn ephemeral(project: &ProjectName, workweave: &str) -> EphemeralRefName {
    EphemeralRefName::mint(project, &WorkweaveName::new(workweave))
}

// ---------------------------------------------------------------------------
// R2 — ownership is by record
// ---------------------------------------------------------------------------

/// The forbidden crash window, from the ref's side: the ref was created
/// and the process died before the receipt landed. Under R2 that ref is
/// not rwv's — which is exactly why the write ordering is the other way
/// round.
#[test]
fn a_ref_that_exists_without_a_receipt_is_not_rwvs() {
    let (_tmp, primary, project, store) = weave();
    let name = RawRefName::new("web-app--hotfix");

    // The ref-first ordering, played out: the branch is created and
    // nothing records it.
    git(&store, &["branch", "web-app--hotfix"]);
    assert!(
        GitVcs
            .resolve_local_branch_tip(&store, &name)
            .unwrap()
            .is_some(),
        "fixture check: the branch this test is about must actually exist"
    );

    let registry = RefRegistry::for_project(&primary, &project);
    assert!(
        registry.lookup(&store, &name).unwrap().is_none(),
        "a ref that looks like rwv's is not rwv's without a receipt"
    );
    assert!(
        registry.list_for_store(&store).unwrap().is_empty(),
        "and it is invisible to the R4 enumeration too"
    );
}

/// The benign crash window, from the ref's side: the receipt is on disk
/// and the ref it names never appeared. No warrant can be built against
/// it, so nothing can be destroyed on its authority.
///
/// The paired positive case is what makes the negatives meaningful: the
/// same registry, the same store, a ref that *does* exist at its recorded
/// tip, yields `Unmoved`.
#[test]
fn a_dangling_receipt_authorizes_nothing_but_a_live_one_does() {
    let (_tmp, primary, project, store) = weave();
    let tip = GitVcs.head_revision(&store).unwrap();
    let mut registry = RefRegistry::for_project(&primary, &project);

    // Crash between the receipt and the birth: receipt, no ref.
    let dangling = registry
        .record_created(&store, ephemeral(&project, "ghost"), tip.clone())
        .unwrap();
    assert!(
        GitVcs
            .resolve_local_branch_tip(&store, &RawRefName::new("web-app--ghost"))
            .unwrap()
            .is_none(),
        "fixture check: the ref must be the one thing that is missing"
    );
    assert!(
        DeletionWarrant::unmoved(&GitVcs, &dangling).is_none(),
        "no ref, no unmoved warrant"
    );
    assert!(
        DeletionWarrant::merged(&GitVcs, &dangling, &tip).is_none(),
        "no ref, no merged warrant"
    );

    // The same flow, completed: receipt, then birth.
    let born = registry
        .record_created(&store, ephemeral(&project, "hotfix"), tip.clone())
        .unwrap();
    git(&store, &["branch", "web-app--hotfix"]);
    let warrant =
        DeletionWarrant::unmoved(&GitVcs, &born).expect("a ref at its recorded tip is unmoved");
    assert!(
        warrant.describe().contains(tip.as_str()),
        "the warrant names the tip it certifies: {}",
        warrant.describe()
    );
}

/// A receipt is not blanket authorization: once the ref has moved, the
/// `Unmoved` warrant is gone, and the only warrants left are the ones that
/// account for what moved onto it.
#[test]
fn a_receipt_stops_yielding_unmoved_once_the_ref_moves() {
    let (_tmp, primary, project, store) = weave();
    let tip = GitVcs.head_revision(&store).unwrap();
    let mut registry = RefRegistry::for_project(&primary, &project);

    let owned = registry
        .record_created(&store, ephemeral(&project, "hotfix"), tip.clone())
        .unwrap();
    git(&store, &["checkout", "-q", "-b", "web-app--hotfix"]);
    let moved = commit(&store, "operator-work");
    assert_ne!(moved, tip, "fixture check: the ref really moved");

    assert!(
        DeletionWarrant::unmoved(&GitVcs, &owned).is_none(),
        "operator commits are not 'unmoved since rwv created it'"
    );
    // Merged against a baseline that contains the work does hold — the
    // warrant that accounts for it rather than ignoring it.
    git(&store, &["checkout", "-q", "main"]);
    git(
        &store,
        &["merge", "-q", "--no-ff", "-m", "merge", "web-app--hotfix"],
    );
    let baseline = GitVcs.head_revision(&store).unwrap();
    assert!(
        DeletionWarrant::merged(&GitVcs, &owned, &baseline).is_some(),
        "a merged ref is destroyable; that is the point of the second warrant"
    );
}

// ---------------------------------------------------------------------------
// §7.1 — "durably, before the ref write it describes"
// ---------------------------------------------------------------------------

/// Env var carrying the probe's workspace root to the child process.
const PROBE_ROOT: &str = "RWV_RECEIPT_DURABILITY_PROBE_ROOT";

/// Not a test: the child half of
/// [`the_receipt_reaches_the_disk_before_record_created_returns`]. It runs
/// as a normal (instant, inert) test in an ordinary suite run, and does the
/// one receipt write when the parent invokes it under `strace`.
#[test]
fn strace_probe_child_records_one_receipt() {
    let Ok(root) = std::env::var(PROBE_ROOT) else {
        return;
    };
    let root = PathBuf::from(root);
    let primary = root.join("ws");
    let store = root.join("weave/github/acme/server");
    std::fs::create_dir_all(primary.join("projects/web-app")).unwrap();
    std::fs::create_dir_all(&store).unwrap();

    let project = ProjectName::new("web-app");
    RefRegistry::for_project(&primary, &project)
        .record_created(
            &store,
            ephemeral(&project, "hotfix"),
            ResolvedRevisionId::from_canonical("a".repeat(40), None),
        )
        .unwrap();
}

/// The receipt is fsynced — file *and* containing directory — and the
/// directory fsync happens after the rename that publishes it.
///
/// Every other assertion in this file would pass just as happily if
/// `record_created` left the receipt in the page cache: a read-back in the
/// same process cannot tell durable from not. The crash class §7.1 names
/// (the machine stops, not the process) is the one where it matters, and
/// the observable that distinguishes them is the syscall trace. So this
/// test reads the syscalls.
///
/// Skipped, loudly, where `strace` cannot run. It is not skipped when
/// strace runs and the calls are absent — that is the regression.
#[test]
fn the_receipt_reaches_the_disk_before_record_created_returns() {
    if Command::new("strace").arg("-V").output().is_err() {
        eprintln!("SKIP: strace not installed; the durability claim is unchecked here");
        return;
    }
    let tmp = TempDir::new().unwrap();
    let out = Command::new("strace")
        .args([
            "-f",
            "-y",
            "-e",
            "trace=fsync,fdatasync,rename,renameat,renameat2",
        ])
        .arg(std::env::current_exe().expect("test binary path"))
        .args([
            "--exact",
            "strace_probe_child_records_one_receipt",
            "--test-threads=1",
        ])
        .env(PROBE_ROOT, tmp.path())
        .output()
        .expect("run strace");

    let trace = String::from_utf8_lossy(&out.stderr);
    let child_stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() && trace.contains("ptrace") {
        eprintln!("SKIP: ptrace is not permitted here: {trace}");
        return;
    }
    assert!(
        child_stdout.contains("1 passed"),
        "the probe must have run: stdout={child_stdout}\nstderr={trace}"
    );

    // The three calls the durability story rests on, in order.
    let calls: Vec<&str> = trace
        .lines()
        .filter(|l| l.contains(".rwv-workweave-index") || l.contains("projects/web-app>"))
        .collect();
    let position = |pred: &dyn Fn(&str) -> bool| calls.iter().position(|l| pred(l));

    let fsync_temp =
        position(&|l: &str| l.contains("fsync(") && l.contains(".rwv-workweave-index.tmp"))
            .unwrap_or_else(|| panic!("no fsync of the temp file in:\n{}", calls.join("\n")));
    let rename = position(&|l: &str| l.contains("rename") && l.contains(".tmp"))
        .unwrap_or_else(|| panic!("no rename publishing the index in:\n{}", calls.join("\n")));
    let fsync_dir = position(&|l: &str| {
        l.contains("fsync(") && l.trim_end().ends_with("projects/web-app>) = 0")
    })
    .unwrap_or_else(|| {
        panic!(
            "no fsync of the containing directory in:\n{}",
            calls.join("\n")
        )
    });

    assert!(
        fsync_temp < rename,
        "the contents must be on disk before the rename publishes them:\n{}",
        calls.join("\n")
    );
    assert!(
        rename < fsync_dir,
        "the directory fsync is what makes the rename itself survive:\n{}",
        calls.join("\n")
    );
}
