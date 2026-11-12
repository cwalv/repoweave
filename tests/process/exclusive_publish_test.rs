//! rwv's exclusive publish of its op record, driven from separate processes
//! through each publish mode: a hard link, and the no-replace rename a
//! filesystem without hard links gets.
//!
//! No filesystem without hard links can be mounted here without privilege,
//! so a child is told to treat its filesystem as lacking one
//! (`RWV_TEST_WITHHOLD`), and the link or rename then fails with the error
//! such a filesystem returns. Which platform's calls ran is whatever this
//! suite runs on.

use crate::common;

use repoweave::op_state::{self, OpId, OpVerb, OwnerRecord, TouchedWorkspaces};
use repoweave::state_file::StateFile;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const CHILD_WORKSPACE: &str = "RWV_PUBLISH_TEST_WORKSPACE";
const CHILD_OP: &str = "RWV_PUBLISH_TEST_OP";
const CHILD_RESULTS: &str = "RWV_PUBLISH_TEST_RESULTS";
const CHILD_REPLACES: &str = "RWV_PUBLISH_TEST_REPLACES";
const WITHHOLD: &str = "RWV_TEST_WITHHOLD";
const NO_HARD_LINKS: &str = "hard-link";
const NEITHER: &str = "hard-link,no-replace-rename";
const CONTENDERS: usize = 8;
const OP_FILE: &str = ".rwv-op";

fn owner_record(workspace: &Path, op: &OpId) -> OwnerRecord {
    OwnerRecord::new_land(
        op,
        None,
        repoweave::manifest::ProjectName::new("web-app").expect("project name"),
        workspace,
        None,
        false,
    )
}

/// Not a test: the child half of each drive. Inert in an ordinary suite run.
#[test]
fn publishing_child() {
    let Ok(workspace) = std::env::var(CHILD_WORKSPACE) else {
        return;
    };
    let workspace = PathBuf::from(workspace);
    let op: OpId = std::env::var(CHILD_OP)
        .expect("a child is given an op")
        .parse()
        .expect("an op id");
    let record = owner_record(&workspace, &op);
    let outcome = if std::env::var(CHILD_REPLACES).is_ok() {
        let bytes = serde_json::to_vec(&record).expect("serialize");
        match StateFile::OpRecord.in_dir(&workspace).publish(&bytes) {
            Ok(()) => "replaced".to_owned(),
            Err(e) => format!("failed {e:#}"),
        }
    } else {
        match op_state::acquire_op(
            &TouchedWorkspaces::of(OpVerb::Land, &workspace, None),
            &record,
        ) {
            Ok(_) => "won".to_owned(),
            Err(e) => match repoweave::refusal::kind_of(&e) {
                Some(kind) => format!("refused {}\n{e:#}", kind.token()),
                None => format!("failed {e:#}"),
            },
        }
    };
    if let Ok(results) = std::env::var(CHILD_RESULTS) {
        std::fs::write(Path::new(&results).join(op.as_str()), outcome).expect("record");
    }
}

fn child(workspace: &Path, op: &str, withhold: Option<&str>) -> Command {
    let mut cmd = Command::new(std::env::current_exe().expect("test binary path"));
    cmd.args(common::test_name(publishing_child).selector());
    cmd.env(CHILD_WORKSPACE, workspace);
    cmd.env(CHILD_OP, op);
    cmd.env_remove(common::pause::ARMING_VARIABLE);
    match withhold {
        Some(modes) => cmd.env(WITHHOLD, modes),
        None => cmd.env_remove(WITHHOLD),
    };
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd
}

fn temps(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read workspace")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp."))
        .collect();
    names.sort();
    names
}

/// What a reader of the op record may see while it is being published:
/// nothing, or a whole record.
fn read_whole_or_absent(workspace: &Path) {
    match std::fs::read(workspace.join(OP_FILE)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("a reader failed on the op record: {e}"),
        Ok(bytes) => {
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap_or_else(|e| {
                panic!(
                    "a reader saw a partial op record ({e}): {:?}",
                    String::from_utf8_lossy(&bytes)
                )
            });
        }
    }
}

fn contend(withhold: Option<&str>) {
    let tmp = common::tempdir().expect("tempdir");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let results = tmp.path().join("results");
    std::fs::create_dir_all(&results).expect("results");

    let mut children: Vec<_> = (0..CONTENDERS)
        .map(|i| {
            let mut cmd = child(&workspace, &format!("{}", 100 + i), withhold);
            cmd.env(CHILD_RESULTS, &results);
            cmd.spawn().expect("spawn a contender")
        })
        .collect();
    while children
        .iter_mut()
        .any(|c| c.try_wait().expect("poll").is_none())
    {
        read_whole_or_absent(&workspace);
    }
    let outcomes: Vec<String> = (0..CONTENDERS)
        .map(|i| {
            std::fs::read_to_string(results.join(format!("{}", 100 + i)))
                .unwrap_or_else(|e| panic!("contender {i} recorded nothing: {e}"))
        })
        .collect();
    let won = outcomes.iter().filter(|o| *o == "won").count();
    assert_eq!(won, 1, "exactly one contender must win: {outcomes:#?}");
    for lost in outcomes.iter().filter(|o| *o != "won") {
        assert!(
            lost.starts_with("refused op-in-progress")
                || lost.starts_with("refused op-acquisition-raced"),
            "a loser must find the record held: {lost}"
        );
    }
    read_whole_or_absent(&workspace);
    assert_eq!(temps(&workspace), Vec::<String>::new());
}

#[test]
fn contenders_publishing_by_hard_link_have_exactly_one_winner() {
    contend(None);
}

#[test]
fn contenders_publishing_by_no_replace_rename_have_exactly_one_winner() {
    contend(Some(NO_HARD_LINKS));
}

#[test]
fn where_neither_mode_is_made_the_op_refuses_by_name_and_leaves_nothing() {
    let tmp = common::tempdir().expect("tempdir");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let results = tmp.path().join("results");
    std::fs::create_dir_all(&results).expect("results");
    let mut cmd = child(&workspace, "100", Some(NEITHER));
    cmd.env(CHILD_RESULTS, &results);
    let out = cmd.output().expect("run the child");
    assert!(out.status.success(), "{out:?}");
    let outcome = std::fs::read_to_string(results.join("100")).expect("recorded");
    assert!(
        outcome.starts_with("refused filesystem-without-exclusive-publish"),
        "{outcome}"
    );
    assert!(
        outcome.contains("Move this workspace to a filesystem that makes hard links"),
        "the refusal must say what to do: {outcome}"
    );
    let left: Vec<String> = std::fs::read_dir(&workspace)
        .expect("read workspace")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        Vec::<String>::new(),
        "a refused op must leave nothing"
    );
}

#[cfg(feature = "test-pause")]
mod paused {
    use super::*;
    use common::pause::Paused;

    const STAGED: &str = "durable-file-staged";

    fn paused_acquire(tmp: &Path, workspace: &Path, withhold: Option<&str>) -> Paused {
        Paused::spawn(
            child(workspace, "100", withhold),
            STAGED,
            &tmp.join("pause"),
        )
        .unwrap_or_else(|why| panic!("{why}"))
    }

    fn reader_sees_nothing_mid_publish_then_the_whole_record(withhold: Option<&str>) {
        let tmp = common::tempdir().expect("tempdir");
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let held = paused_acquire(tmp.path(), &workspace, withhold);

        assert_eq!(temps(&workspace).len(), 1, "the writer has staged its temp");
        let mid = std::fs::read(workspace.join(OP_FILE));
        assert!(
            mid.as_ref()
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "mid-publish the op record must be absent, not partial: {mid:?}"
        );

        let out = held.release();
        assert!(out.status.success(), "{out:?}");
        let record = std::fs::read(workspace.join(OP_FILE)).expect("published");
        serde_json::from_slice::<serde_json::Value>(&record).expect("a whole record");
        assert_eq!(temps(&workspace), Vec::<String>::new());
    }

    #[test]
    fn a_reader_never_sees_a_partial_record_published_by_hard_link() {
        reader_sees_nothing_mid_publish_then_the_whole_record(None);
    }

    #[test]
    fn a_reader_never_sees_a_partial_record_published_by_rename() {
        reader_sees_nothing_mid_publish_then_the_whole_record(Some(NO_HARD_LINKS));
    }

    #[test]
    fn a_temp_left_by_a_killed_writer_is_removed_by_the_next_publish_of_its_name() {
        let tmp = common::tempdir().expect("tempdir");
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let held = paused_acquire(tmp.path(), &workspace, Some(NO_HARD_LINKS));
        drop(held);
        let left = temps(&workspace);
        assert_eq!(left.len(), 1, "the killed writer leaves its temp");
        assert!(!workspace.join(OP_FILE).exists(), "and publishes nothing");

        let op: OpId = "200".parse().expect("op id");
        let _taken = op_state::acquire_op(
            &TouchedWorkspaces::of(OpVerb::Land, &workspace, None),
            &owner_record(&workspace, &op),
        )
        .expect("the next op takes the workspace");
        assert_eq!(temps(&workspace), Vec::<String>::new(), "swept: {left:?}");
    }

    #[test]
    fn a_temp_whose_writer_is_paused_mid_publish_is_left_and_its_publish_completes() {
        let tmp = common::tempdir().expect("tempdir");
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut cmd = child(&workspace, "100", None);
        cmd.env(CHILD_REPLACES, "1");
        let held = Paused::spawn(cmd, STAGED, &tmp.path().join("pause"))
            .unwrap_or_else(|why| panic!("{why}"));
        let theirs = temps(&workspace);
        assert_eq!(theirs.len(), 1);

        let mine = owner_record(&workspace, &"200".parse().expect("op id"));
        StateFile::OpRecord
            .in_dir(&workspace)
            .publish(&serde_json::to_vec(&mine).expect("serialize"))
            .expect("a publish beside a live writer");
        assert_eq!(
            temps(&workspace),
            theirs,
            "a live writer's temp is never swept"
        );

        let out = held.release();
        assert!(out.status.success(), "{out:?}");
        let published: serde_json::Value =
            serde_json::from_slice(&std::fs::read(workspace.join(OP_FILE)).expect("published"))
                .expect("whole");
        assert_eq!(
            published["id"], "100",
            "the paused writer's publish completed"
        );
        assert_eq!(temps(&workspace), Vec::<String>::new());
    }
}
