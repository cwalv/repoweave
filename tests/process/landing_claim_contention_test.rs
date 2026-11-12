//! Many `rwv` processes contending for one landing claim, some of them dying
//! while they hold it.
//!
//! **Processes, not threads.** A dead claim is taken over under a guard file
//! created exclusively beside it, and only the guard's holder removes the
//! claim. That exclusion is between processes; a threaded drive shares one
//! process and proves nothing about it.
//!
//! The drive: the test holds the claim for a live op, spawns every taker, then
//! releases it. Each taker records its own op, takes the claim, and while it
//! holds it checks, over and over, that the claim file still names it and that
//! no other taker holds it at the same time (an exclusively created marker).
//! Then half of them release the claim and clear their record; the other half
//! clear their record only, as a landing whose record is gone leaves a claim
//! nothing ends, so the next taker must take it over.
//!
//! What a correct claim gives under every interleaving:
//! - no taker ever finds its claim removed or replaced while it holds it;
//! - no two takers hold it at once;
//! - every taker gets it: a claim left by a dead taker never outlasts a live
//!   one waiting, which would wait out the budget and refuse;
//! - every taker exits within a bound, so none deadlocks on a guard;
//! - no takeover guard is left behind.

use crate::common;

use repoweave::landing_claim::LandingClaim;
use repoweave::manifest::ProjectName;
use repoweave::op_state::{self, OpId, OwnerRecord};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TAKERS: usize = 8;
const HOLD_CHECKS: usize = 40;

const CHILD_PROJECT: &str = "RWV_CLAIM_TEST_PROJECT";
const CHILD_OWNER: &str = "RWV_CLAIM_TEST_OWNER";
const CHILD_OP: &str = "RWV_CLAIM_TEST_OP";
const CHILD_DIES: &str = "RWV_CLAIM_TEST_DIES";
const CHILD_RESULTS: &str = "RWV_CLAIM_TEST_RESULTS";

const PROJECT: &str = "web-app";
const TARGET: &str = "main";
const CLAIM_FILE: &str = ".rwv-landing.lock";
const HOLDING_MARKER: &str = "holding";

fn project() -> ProjectName {
    ProjectName::new(PROJECT).expect("project name")
}

fn record(owner: &Path, op: &OpId) {
    let record = OwnerRecord::new_land(op, None, project(), owner, None, false);
    op_state::write_owner(owner, &record).expect("the taker records its op");
}

/// The claim files under the project repo's store, wherever its digest names
/// the directory.
fn claim_files(project_dir: &Path) -> Vec<PathBuf> {
    let store = project_dir.join(".git").join("rwv-landings");
    let Ok(dirs) = std::fs::read_dir(&store) else {
        return Vec::new();
    };
    dirs.flatten()
        .map(|dir| dir.path().join(CLAIM_FILE))
        .filter(|file| file.exists())
        .collect()
}

fn takeover_guards(project_dir: &Path) -> Vec<PathBuf> {
    let store = project_dir.join(".git").join("rwv-landings");
    let Ok(dirs) = std::fs::read_dir(&store) else {
        return Vec::new();
    };
    dirs.flatten()
        .filter_map(|dir| std::fs::read_dir(dir.path()).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains(".takeover."))
        })
        .collect()
}

fn names_op(project_dir: &Path, op: &OpId) -> bool {
    let files = claim_files(project_dir);
    let [file] = files.as_slice() else {
        return false;
    };
    std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|held| held.get("op").and_then(|op| op.as_str()).map(str::to_owned))
        .is_some_and(|held| held == op.as_str())
}

/// Not a test: the child half of the drive. Inert in an ordinary suite run:
/// without [`CHILD_PROJECT`] set it returns at once.
#[test]
fn claim_taker_child() {
    let Ok(project_dir) = std::env::var(CHILD_PROJECT) else {
        return;
    };
    let project_dir = PathBuf::from(project_dir);
    let owner = PathBuf::from(std::env::var(CHILD_OWNER).expect("a taker is given a workspace"));
    let op: OpId = std::env::var(CHILD_OP)
        .expect("a taker is given an op")
        .parse()
        .expect("an op id");
    let dies = std::env::var(CHILD_DIES).is_ok();
    let results = PathBuf::from(std::env::var(CHILD_RESULTS).expect("a taker records"));

    record(&owner, &op);
    let vcs = repoweave::git::git_vcs();
    let outcome =
        match LandingClaim::take(vcs.as_ref(), &project_dir, &project(), TARGET, &op, &owner) {
            Err(e) => match repoweave::refusal::kind_of(&e) {
                Some(kind) => format!("refused {}", kind.token()),
                None => format!("failed {e:#}"),
            },
            Ok(claim) => {
                let marker = results.join(HOLDING_MARKER);
                let mut outcome = match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&marker)
                {
                    Ok(_) => "ok".to_owned(),
                    Err(e) => format!("overlap: another taker holds it too ({e})"),
                };
                for _ in 0..HOLD_CHECKS {
                    if !names_op(&project_dir, &op) {
                        outcome = "stolen: the claim no longer names this taker".to_owned();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                if outcome == "ok" {
                    std::fs::remove_file(&marker).expect("the taker drops its marker");
                }
                if !dies {
                    if let Err(e) = claim.release() {
                        outcome = format!("failed to release: {e:#}");
                    }
                }
                outcome
            }
        };
    op_state::clear_owner(&owner);
    std::fs::write(results.join(op.as_str()), outcome).expect("the taker records its result");
}

fn workspace(root: &Path, name: &str) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("projects")).expect("a taker's workspace");
    dir
}

#[test]
fn takers_contending_for_a_claim_never_remove_a_live_one_and_all_get_it() {
    let tmp = common::tempdir().expect("tempdir");
    let project_dir = tmp.path().join("project");
    std::fs::create_dir_all(&project_dir).expect("project dir");
    common::git_in(&project_dir, &["init", "-q", "-b", "main"]);
    let results = tmp.path().join("results");
    std::fs::create_dir_all(&results).expect("results dir");

    let holder_ws = workspace(tmp.path(), "holder");
    let holder_op: OpId = "1".parse().expect("op id");
    record(&holder_ws, &holder_op);
    let vcs = repoweave::git::git_vcs();
    let held = LandingClaim::take(
        vcs.as_ref(),
        &project_dir,
        &project(),
        TARGET,
        &holder_op,
        &holder_ws,
    )
    .expect("the test takes the claim first");

    let ops: Vec<OpId> = (0..TAKERS)
        .map(|i| format!("{}", 100 + i).parse().expect("op id"))
        .collect();
    let mut children: Vec<_> = ops
        .iter()
        .enumerate()
        .map(|(i, op)| {
            let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
            cmd.args(common::test_name(claim_taker_child).selector());
            cmd.env(CHILD_PROJECT, &project_dir);
            cmd.env(CHILD_OWNER, workspace(tmp.path(), &format!("taker-{i}")));
            cmd.env(CHILD_OP, op.as_str());
            cmd.env(CHILD_RESULTS, &results);
            if i % 2 == 0 {
                cmd.env(CHILD_DIES, "1");
            }
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            cmd.spawn().expect("a taker spawns")
        })
        .collect();

    let recorded = Instant::now() + Duration::from_secs(60);
    while !(0..TAKERS)
        .all(|i| OwnerRecord::path_in(&tmp.path().join(format!("taker-{i}"))).exists())
    {
        assert!(Instant::now() < recorded, "every taker records its op");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        names_op(&project_dir, &holder_op),
        "no taker removed the live claim the test holds"
    );
    held.release().expect("the test releases its claim");
    op_state::clear_owner(&holder_ws);

    let bound = Instant::now() + Duration::from_secs(120);
    for (i, child) in children.iter_mut().enumerate() {
        loop {
            if child
                .try_wait()
                .expect("a taker can be waited on")
                .is_some()
            {
                break;
            }
            if Instant::now() >= bound {
                for child in &mut children[i..] {
                    let _ = child.kill();
                }
                panic!("taker {i} did not exit: a taker is stuck on the claim");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    let outcomes: Vec<(String, String)> = ops
        .iter()
        .map(|op| {
            let outcome = std::fs::read_to_string(results.join(op.as_str()))
                .unwrap_or_else(|e| format!("recorded no result: {e}"));
            (op.to_string(), outcome)
        })
        .collect();
    let expected: Vec<(String, String)> = ops
        .iter()
        .map(|op| (op.to_string(), "ok".to_owned()))
        .collect();
    assert_eq!(outcomes, expected);
    assert_eq!(
        takeover_guards(&project_dir),
        Vec::<PathBuf>::new(),
        "no takeover guard is left behind"
    );

    let last_ws = workspace(tmp.path(), "last");
    let last_op: OpId = "999".parse().expect("op id");
    record(&last_ws, &last_op);
    LandingClaim::take(
        vcs.as_ref(),
        &project_dir,
        &project(),
        TARGET,
        &last_op,
        &last_ws,
    )
    .expect("a claim its dead taker left is taken over")
    .release()
    .expect("released");
    assert_eq!(claim_files(&project_dir), Vec::<PathBuf>::new());
}
