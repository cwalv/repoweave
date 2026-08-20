//! Two `rwv` processes creating workweaves in one project keep every
//! ownership receipt.
//!
//! The defect: the index is mutated by reading the whole file, appending, and
//! publishing the whole file back. A losing writer's receipt is gone and its
//! `rwv workweave create` still exits zero. What is lost is not bookkeeping —
//! under R2 a ref with no receipt is not rwv's, permanently, so the branch that
//! create just made can never be destroyed by any verb. `workweave delete`
//! then removes the directory and leaves the branch behind; from that point no
//! doctor class names it, because the class that would is scoped to the
//! pre-flat mint shape and production mints flat.
//!
//! **Measured before it was fixed, in the production topology.** Two
//! concurrent `rwv workweave create` invocations dropped at least one receipt
//! in nine runs out of ten. Not a corner: it is what a fleet of agents does.
//!
//! **Processes, not threads.** An in-process mutex satisfies a threaded drive
//! while leaving the reachable topology — two `rwv` invocations against one
//! project — exactly as lossy as before, so a threaded drive cannot tell the
//! two apart. Both drives here spawn real processes.
//!
//! **Controls.** N of N retained proves nothing if the drive never overlapped:
//! a serial run of the same work retains N of N with no exclusion at all. So
//! each drive carries its serial control, and the receipt drive records the
//! wall-clock window of each child's call so the overlap is measured rather
//! than assumed.

mod common;

use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::vcs::{EphemeralRefName, ResolvedRevisionId};
use repoweave::workweave_index::RefRegistry;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Recorders in the primitive drive.
const N: usize = 8;

const CHILD_ROOT: &str = "RWV_INDEX_TEST_ROOT";
const CHILD_STORE: &str = "RWV_INDEX_TEST_STORE";
const CHILD_NAME: &str = "RWV_INDEX_TEST_NAME";
const CHILD_WINDOWS: &str = "RWV_INDEX_TEST_WINDOWS";
const CHILD_BARRIER: &str = "RWV_INDEX_TEST_BARRIER";

const PROJECT: &str = "web-app";
const MEMBER: &str = "github/acme/lib";

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock at or after the epoch")
        .as_nanos()
}

/// Not a test: the child half of the receipt drive. Inert in an ordinary suite
/// run — without [`CHILD_ROOT`] set it returns immediately.
///
/// A barrier, when named, is a file the parent creates once every child is
/// spawned. Spinning on it rather than sleeping keeps the children's start
/// times within microseconds, so the recorded windows measure contention
/// rather than staggered launches.
#[test]
fn receipt_child() {
    let Ok(root) = std::env::var(CHILD_ROOT) else {
        return;
    };
    let store = std::env::var(CHILD_STORE).expect("a child is given a store");
    let name = std::env::var(CHILD_NAME).expect("a child is given a name");
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

    let project = ProjectName::new(PROJECT).expect("project name");
    let mut registry = RefRegistry::for_project(Path::new(&root), &project);
    let minted = EphemeralRefName::mint(&project, &WorkweaveName::new(&name).expect("ww name"));
    let at = ResolvedRevisionId::from_canonical("a".repeat(40), None);

    let start = now_nanos();
    registry
        .record_created(Path::new(&store), minted, at)
        .unwrap_or_else(|e| panic!("recording a receipt for {name} must succeed: {e:#}"));
    let end = now_nanos();

    let windows = std::env::var(CHILD_WINDOWS).expect("a child records its window");
    std::fs::write(Path::new(&windows).join(&name), format!("{start} {end}"))
        .expect("window record must be writable");
}

fn recorder(root: &Path, store: &Path, windows: &Path, name: &str, gate: Option<&Path>) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
    cmd.args(["--exact", "receipt_child", "--test-threads=1"]);
    cmd.env(CHILD_ROOT, root);
    cmd.env(CHILD_STORE, store);
    cmd.env(CHILD_WINDOWS, windows);
    cmd.env(CHILD_NAME, name);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    match gate {
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

fn seat(i: usize) -> String {
    format!("seat{i}")
}

/// A primary root with `projects/<PROJECT>/` holding a current-shape index,
/// plus a directory the receipts key their store to.
fn seeded_primary(tmp: &Path, tag: &str) -> (PathBuf, PathBuf) {
    let root = tmp.join(tag);
    let project_dir = root.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project_dir).expect("project dir");
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
    (root, store)
}

fn receipt_names(root: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(
        root.join("projects")
            .join(PROJECT)
            .join(".rwv-workweave-index"),
    )
    .expect("index readable");
    let value: serde_json::Value = serde_json::from_str(&text).expect("index parses");
    value["receipts"]
        .as_array()
        .expect("a current-shape index has a receipts array")
        .iter()
        .map(|r| r["name"].as_str().expect("receipt name").to_string())
        .collect()
}

fn windows_in(dir: &Path) -> Vec<(u128, u128)> {
    (0..N)
        .map(|i| {
            let path = dir.join(seat(i));
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("child {i} recorded no window at {path:?}: {e}"));
            let (s, e) = text.split_once(' ').expect("window is `start end`");
            (
                s.parse().expect("start nanos"),
                e.parse().expect("end nanos"),
            )
        })
        .collect()
}

/// Every call in flight at one instant, which is what makes an N-of-N result
/// evidence rather than a report that the drive serialised itself.
fn share_an_instant(w: &[(u128, u128)]) -> bool {
    w.iter().map(|x| x.0).max().expect("non-empty")
        < w.iter().map(|x| x.1).min().expect("non-empty")
}

#[test]
fn concurrent_recorders_keep_every_receipt_and_serial_ones_are_the_control() {
    let tmp = common::tempdir().expect("tempdir");

    // Control: the same N recorders, one at a time.
    let (serial_root, serial_store) = seeded_primary(tmp.path(), "serial");
    let serial_windows = tmp.path().join("serial-windows");
    std::fs::create_dir_all(&serial_windows).expect("windows dir");
    for i in 0..N {
        let out = recorder(&serial_root, &serial_store, &serial_windows, &seat(i), None)
            .output()
            .expect("serial recorder spawns");
        assert!(
            out.status.success(),
            "serial recorder {i} failed: {}\n{}",
            out.status,
            transcript(&out)
        );
    }
    assert_eq!(
        receipt_names(&serial_root).len(),
        N,
        "the serial drive must keep every receipt; if it does not, the \
         concurrent result below is measuring something other than contention"
    );
    let serial = windows_in(&serial_windows);
    assert!(
        !share_an_instant(&serial),
        "the control must not overlap — if these windows share an instant the \
         drive did not run serially and is no control at all: {serial:?}"
    );

    // Drive: all N released together against one index.
    let (root, store) = seeded_primary(tmp.path(), "concurrent");
    let windows = tmp.path().join("windows");
    std::fs::create_dir_all(&windows).expect("windows dir");
    let barrier = windows.join("go");
    let children: Vec<_> = (0..N)
        .map(|i| {
            recorder(&root, &store, &windows, &seat(i), Some(&barrier))
                .spawn()
                .expect("concurrent recorder spawns")
        })
        .collect();
    std::fs::write(&barrier, b"go").expect("barrier must be writable");
    for (i, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().expect("concurrent recorder exits");
        assert!(
            out.status.success(),
            "concurrent recorder {i} failed: {}\n{}",
            out.status,
            transcript(&out)
        );
    }

    let kept = receipt_names(&root);
    let concurrent = windows_in(&windows);
    assert!(
        share_an_instant(&concurrent),
        "no instant had every recorder in flight, so this drive did not \
         exercise the read-modify-write against itself and its result is not \
         evidence either way: {concurrent:?}"
    );
    assert_eq!(
        kept.len(),
        N,
        "every concurrent receipt must survive: a whole-file publish that lost \
         one returns Ok, and the ref it named is then disowned for good. \
         Kept {kept:?}"
    );
}

fn rwv_at(bin: &Path, root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    cmd.current_dir(root);
    // rwv shells out to git; a polluted GIT_* env would point those
    // subprocesses at the wrong repo, and the default-branch pin mirrors
    // `common::rwv`.
    cmd.env("GIT_CONFIG_COUNT", "1");
    cmd.env("GIT_CONFIG_KEY_0", "init.defaultBranch");
    cmd.env("GIT_CONFIG_VALUE_0", "main");
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_COMMON_DIR",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        cmd.env_remove(var);
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

/// Ephemeral branches this project minted in `repo`.
fn minted_branches(repo: &Path) -> Vec<String> {
    common::git_in(repo, &["branch", "--format=%(refname:short)"])
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|b| b.starts_with(&format!("{PROJECT}--")))
        .collect()
}

/// Every `(store, name)` pair the index records — the receipt's full key.
/// One create mints N receipts sharing one name, distinguished only by
/// store, so a name-keyed read lets a surviving peer answer for a lost one.
fn recorded_receipts(root: &Path) -> Vec<(std::path::PathBuf, String)> {
    let text = std::fs::read_to_string(
        root.join("projects")
            .join(PROJECT)
            .join(".rwv-workweave-index"),
    )
    .expect("index readable");
    let value: serde_json::Value = serde_json::from_str(&text).expect("index parses");
    value["receipts"]
        .as_array()
        .expect("a current-shape index has a receipts array")
        .iter()
        .map(|r| {
            (
                std::path::PathBuf::from(r["store"].as_str().expect("receipt store")),
                r["name"].as_str().expect("receipt name").to_string(),
            )
        })
        .collect()
}

/// Every minted branch that no receipt covers — the state R2 disowns for good.
fn unreceipted(root: &Path) -> Vec<String> {
    let held = recorded_receipts(root);
    let mut orphans = Vec::new();
    for repo in [MEMBER, &format!("projects/{PROJECT}")] {
        let store = root
            .join(repo)
            .canonicalize()
            .expect("minted repo store exists");
        for branch in minted_branches(&root.join(repo)) {
            let covered = held
                .iter()
                .any(|(s, n)| *n == branch && s.canonicalize().is_ok_and(|c| c == store));
            if !covered {
                orphans.push(format!("{repo}:{branch}"));
            }
        }
    }
    orphans
}

/// The production drive: `rwv workweave create` twice at once, in one project.
///
/// This is the reachable topology. `create` reaches no op-state check, so
/// nothing anywhere refuses to let the two overlap, and each one records a
/// receipt per repo into the one shared index.
#[test]
fn two_concurrent_creates_leave_no_branch_without_a_receipt() {
    let bin = assert_cmd::cargo::cargo_bin("rwv");

    // Control: the same two creates, one after the other.
    let serial_ws = common::build_workspace(PROJECT, &[(MEMBER, "owned")]);
    for name in ["alpha", "beta"] {
        let out = rwv_at(
            &bin,
            &serial_ws.workspace,
            &["workweave", PROJECT, "create", name],
        )
        .output()
        .expect("serial create spawns");
        assert!(
            out.status.success(),
            "serial create {name} failed: {}\n{}",
            out.status,
            transcript(&out)
        );
    }
    assert_eq!(
        unreceipted(&serial_ws.workspace),
        Vec::<String>::new(),
        "the serial drive must leave no branch unreceipted; if it does, the \
         concurrent result below is not about concurrency"
    );

    // Drive: both creates in flight together.
    let ws = common::build_workspace(PROJECT, &[(MEMBER, "owned")]);
    let children: Vec<_> = ["alpha", "beta"]
        .iter()
        .map(|name| {
            (
                *name,
                rwv_at(&bin, &ws.workspace, &["workweave", PROJECT, "create", name])
                    .spawn()
                    .expect("concurrent create spawns"),
            )
        })
        .collect();
    for (name, child) in children {
        let out = child.wait_with_output().expect("concurrent create exits");
        assert!(
            out.status.success(),
            "concurrent create {name} failed: {}\n{}",
            out.status,
            transcript(&out)
        );
    }

    let branches = minted_branches(&ws.workspace.join(MEMBER));
    assert_eq!(
        branches.len(),
        2,
        "both creates must have minted their branch, or this drive is not \
         measuring what it claims: {branches:?}"
    );
    assert_eq!(
        unreceipted(&ws.workspace),
        Vec::<String>::new(),
        "a branch with no receipt is not rwv's, permanently: no verb will \
         reclaim it, `workweave delete` leaves it behind, and once the \
         directory is gone no doctor class names it. Both creates exited zero"
    );
}
