//! Two `rwv` processes creating workweaves in one project keep every index
//! entry.
//!
//! The index is mutated by reading the whole file, adding an entry, and
//! publishing the whole file back. Without the claim a losing writer's entry
//! is gone, its `rwv workweave create` still exits zero, and the workweave it
//! made is on disk but unlisted: `workweave list` and `workweave delete`
//! cannot name it.
//!
//! **Processes, not threads.** An in-process mutex satisfies a threaded drive
//! while leaving the reachable topology — two `rwv` invocations against one
//! project — exactly as lossy as before, so a threaded drive cannot tell the
//! two apart. Both drives here spawn real processes.
//!
//! **Exclusion is pinned by holding the claim, not by racing for it.** The
//! index's exclusion is a claim file created exclusively; a recorder that
//! cannot create it waits out the claim budget and refuses with
//! `state-claim-held`. The recorder drive creates that file itself, so while it
//! holds the claim a recorder that honors the exclusion has exactly one
//! possible outcome — refusal — and one that does not succeeds. Neither
//! outcome depends on how the scheduler interleaves the children, because the
//! holder is the test and releases only after every child has exited.
//!
//! **Convergence is the other half.** With no claim held, N concurrent
//! recorders all succeed and all N entries are kept. That holds under every
//! interleaving for a correct index; the exclusion pin carries the proof that
//! it is the claim, not luck, that makes it hold.
//!
//! The `rwv workweave create` drive below runs the reachable topology end to
//! end and asserts the same property of its outcome: every workweave it made
//! is listed.

use crate::common;

use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::path_spelling::CanonicalPath;
use repoweave::workweave_index::record_workweave;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Recorders in the primitive drive.
const N: usize = 8;

const CHILD_ROOT: &str = "RWV_INDEX_TEST_ROOT";
const CHILD_STORE: &str = "RWV_INDEX_TEST_STORE";
const CHILD_NAME: &str = "RWV_INDEX_TEST_NAME";
const CHILD_RESULTS: &str = "RWV_INDEX_TEST_RESULTS";

const PROJECT: &str = "web-app";
const MEMBER: &str = "github/acme/lib";

/// The index's claim file. Not exported by the crate, so the literal is spelled
/// here; a wrong spelling holds nothing, the recorders succeed, and the
/// exclusion pin goes red.
const INDEX_CLAIM_FILE: &str = "rwv-workweave-index.lock";
const INDEX_FILE: &str = "rwv-workweave-index";

/// Not a test: the child half of the recorder drive. Inert in an ordinary suite
/// run — without [`CHILD_ROOT`] set it returns immediately. It records one
/// workweave and writes `ok`, or `refused <kind token>` when the record
/// refuses.
#[test]
fn recorder_child() {
    let Ok(root) = std::env::var(CHILD_ROOT) else {
        return;
    };
    let store = std::env::var(CHILD_STORE).expect("a child is given a store");
    let name = std::env::var(CHILD_NAME).expect("a child is given a name");
    let results = std::env::var(CHILD_RESULTS).expect("a child records its result");

    let project = ProjectName::new(PROJECT).expect("project name");
    let outcome = match record_workweave(
        Path::new(&root),
        &project,
        &WorkweaveName::new(&name).expect("ww name"),
        CanonicalPath::of(Path::new(&store)),
        CanonicalPath::of(Path::new(&root)),
    ) {
        Ok(()) => "ok".to_string(),
        Err(e) => match repoweave::refusal::kind_of(&e) {
            Some(kind) => format!("refused {}", kind.token()),
            None => format!("failed {e:#}"),
        },
    };
    std::fs::write(Path::new(&results).join(&name), outcome)
        .expect("result record must be writable");
}

fn recorder(root: &Path, store: &Path, results: &Path, name: &str) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
    cmd.args(common::test_name(recorder_child).selector());
    cmd.env(CHILD_ROOT, root);
    cmd.env(CHILD_STORE, store);
    cmd.env(CHILD_RESULTS, results);
    cmd.env(CHILD_NAME, name);
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

fn seat(i: usize) -> String {
    format!("seat{i}")
}

/// A primary root with `projects/<PROJECT>/` holding a current-shape index,
/// plus a directory the recorded workweaves point at.
fn seeded_primary(tmp: &Path, tag: &str) -> (PathBuf, PathBuf) {
    let root = tmp.join(tag);
    let project_dir = root.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project_dir).expect("project dir");
    common::git_in(&project_dir, &["init", "-q"]);
    std::fs::write(
        common::index_file_of(&project_dir),
        format!(
            r#"{{"basedir":{:?},"workweaves":{{}}}}"#,
            root.join(".workweaves").to_string_lossy()
        ),
    )
    .expect("seed index");
    let store = root.join("store");
    std::fs::create_dir_all(&store).expect("store dir");
    (root, store)
}

fn recorded_workweaves(root: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(common::index_file_of(root.join("projects").join(PROJECT)))
        .expect("index readable");
    let value: serde_json::Value = serde_json::from_str(&text).expect("index parses");
    value["workweaves"]
        .as_object()
        .expect("an index has a workweaves map")
        .keys()
        .cloned()
        .collect()
}

/// Spawn every recorder at once and wait for all of them to exit; the result
/// each recorded, in seat order.
fn run_recorders(root: &Path, store: &Path, results: &Path) -> Vec<String> {
    let children: Vec<_> = (0..N)
        .map(|i| {
            recorder(root, store, results, &seat(i))
                .spawn()
                .expect("concurrent recorder spawns")
        })
        .collect();
    for (i, child) in children.into_iter().enumerate() {
        let out = child.wait_with_output().expect("concurrent recorder exits");
        assert!(
            out.status.success(),
            "recorder {i} crashed: {}\n{}",
            out.status,
            transcript(&out)
        );
    }
    (0..N)
        .map(|i| {
            std::fs::read_to_string(results.join(seat(i)))
                .unwrap_or_else(|e| panic!("recorder {i} recorded no result: {e}"))
        })
        .collect()
}

#[test]
fn a_held_claim_refuses_every_recorder_and_leaves_the_index_untouched() {
    let tmp = common::tempdir().expect("tempdir");
    let (root, store) = seeded_primary(tmp.path(), "held");
    let results = tmp.path().join("results");
    std::fs::create_dir_all(&results).expect("results dir");
    let project_dir = root.join("projects").join(PROJECT);
    let store_dir = project_dir.join(".git");
    let before = std::fs::read(store_dir.join(INDEX_FILE)).expect("seeded index");

    let claim = store_dir.join(INDEX_CLAIM_FILE);
    let holder = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&claim)
        .expect("the test takes the claim itself");
    holder.try_lock().expect("the test holds the claim's lock");
    let outcomes = run_recorders(&root, &store, &results);
    let refused = format!(
        "refused {}",
        repoweave::refusal::RefusalKind::StateClaimHeld.token()
    );
    assert_eq!(
        outcomes,
        vec![refused; N],
        "while a peer holds the claim every recorder must refuse; one that \
         succeeded wrote without the exclusion"
    );
    assert_eq!(
        std::fs::read(store_dir.join(INDEX_FILE)).expect("index still present"),
        before,
        "a refused record must write nothing"
    );
    std::fs::remove_file(&claim).expect("the test releases the claim");
    drop(holder);
}

#[test]
fn concurrent_recorders_keep_every_entry() {
    let tmp = common::tempdir().expect("tempdir");
    let (root, store) = seeded_primary(tmp.path(), "concurrent");
    let results = tmp.path().join("results");
    std::fs::create_dir_all(&results).expect("results dir");
    let outcomes = run_recorders(&root, &store, &results);
    assert_eq!(outcomes, vec!["ok".to_string(); N]);
    let kept = recorded_workweaves(&root);
    assert_eq!(
        kept.len(),
        N,
        "every concurrent entry must survive: a whole-file publish that lost \
         one returns Ok, and the workweave it named is then unlisted. \
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

/// The production drive: `rwv workweave create` twice at once, in one project.
///
/// This is the reachable topology. `create` reaches no op-state check, so
/// nothing anywhere refuses to let the two overlap, and each one records its
/// workweave into the one shared index.
#[test]
fn two_concurrent_creates_list_both_workweaves() {
    let bin = assert_cmd::cargo::cargo_bin("rwv");

    // Control: the same two creates, one after the other.
    let serial_ws = common::build_workspace(PROJECT, &[(MEMBER, "owned")]);
    for name in ["alpha", "beta"] {
        let out = rwv_at(
            &bin,
            &serial_ws.workspace,
            &["workweave", "create", "--project", PROJECT, name],
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
        recorded_workweaves(&serial_ws.workspace),
        vec!["alpha".to_string(), "beta".to_string()],
        "the serial drive must list both; if it does not, the concurrent \
         result below is not about concurrency"
    );

    // Drive: both creates in flight together.
    let ws = common::build_workspace(PROJECT, &[(MEMBER, "owned")]);
    let children: Vec<_> = ["alpha", "beta"]
        .iter()
        .map(|name| {
            (
                *name,
                rwv_at(
                    &bin,
                    &ws.workspace,
                    &["workweave", "create", "--project", PROJECT, name],
                )
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

    assert_eq!(
        recorded_workweaves(&ws.workspace),
        vec!["alpha".to_string(), "beta".to_string()],
        "a workweave the index lost is on disk but unlisted: `workweave list` \
         and `workweave delete` cannot name it. Both creates exited zero"
    );
}
