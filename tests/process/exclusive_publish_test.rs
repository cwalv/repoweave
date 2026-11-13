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
const WITHHOLD_UNDER: &str = "RWV_TEST_WITHHOLD_UNDER";
const CHILD_CALL: &str = "RWV_PUBLISH_TEST_CALL";
const CHILD_OTHER: &str = "RWV_PUBLISH_TEST_OTHER";
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
    if let Ok(call) = std::env::var(CHILD_CALL) {
        let other = std::env::var(CHILD_OTHER).map(PathBuf::from);
        let outcome = match called(&call, &workspace, other.ok().as_deref(), &op, &record) {
            Ok(()) => "ok".to_owned(),
            Err(e) => match repoweave::refusal::kind_of(&e) {
                Some(kind) => format!("refused {}\n{e:#}", kind.token()),
                None => format!("failed {e:#}"),
            },
        };
        let results = std::env::var(CHILD_RESULTS).expect("results");
        std::fs::write(Path::new(&results).join(op.as_str()), outcome).expect("record");
        return;
    }
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

/// The one exclusive publish a child makes, by the name its test gives it,
/// so a refusal can be withheld for the second file an operation writes and
/// each caller's arm be reached.
fn called(
    call: &str,
    workspace: &Path,
    other: Option<&Path>,
    op: &OpId,
    record: &OwnerRecord,
) -> anyhow::Result<()> {
    let other = || other.expect("this call is given a second directory");
    match call {
        "acquire-op-with-a-lease" => op_state::acquire_op(
            &TouchedWorkspaces::of(OpVerb::Land, workspace, Some(other())),
            record,
        )
        .map(drop),
        "acquire-lease" => op_state::acquire_lease(other(), workspace, op).map(drop),
        "acquire-more-leases" => {
            let mut acquired = op_state::acquire_op(
                &TouchedWorkspaces::of(OpVerb::Land, workspace, None),
                record,
            )?;
            let widened =
                op_state::acquire_more_leases(&mut acquired, &[other().to_path_buf()], record);
            std::mem::forget(acquired);
            widened
        }
        "take" | "take-over" => {
            let vcs = repoweave::git::git_vcs();
            repoweave::landing_claim::LandingClaim::take(
                vcs.as_ref(),
                other(),
                &repoweave::manifest::ProjectName::new("web-app").expect("project"),
                "main",
                op,
                workspace,
            )
            .map(drop)
        }
        "fetch" => {
            let vcs = repoweave::git::git_vcs();
            repoweave::fetch_claim::FetchClaim::hold(vcs.as_ref(), other(), |_| Ok(()))
        }
        "stamp" => repoweave::owned_state::stamp_owned_digest(workspace, "Cargo.lock", b"x"),
        other => panic!("no such call {other}"),
    }
}

/// Runs `call` in a child that lacks both publish modes for every path
/// beginning with `under`, and returns what it recorded.
fn refused_call(
    tmp: &Path,
    workspace: &Path,
    other: Option<&Path>,
    call: &str,
    under: &Path,
) -> String {
    let results = tmp.join("results");
    std::fs::create_dir_all(&results).expect("results");
    let mut cmd = child(workspace, "100", Some(NEITHER));
    cmd.env(CHILD_RESULTS, &results);
    cmd.env(CHILD_CALL, call);
    cmd.env(WITHHOLD_UNDER, under);
    if let Some(other) = other {
        cmd.env(CHILD_OTHER, other);
    }
    let out = cmd.output().expect("run the child");
    assert!(out.status.success(), "{out:?}");
    std::fs::read_to_string(results.join("100")).expect("recorded")
}

fn assert_refused(outcome: &str) {
    assert!(
        outcome.starts_with("refused filesystem-without-exclusive-publish"),
        "{outcome}"
    );
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn two_workspaces(tmp: &Path) -> (PathBuf, PathBuf) {
    let ws = tmp.join("ws");
    let other = tmp.join("other");
    for dir in [&ws, &other] {
        std::fs::create_dir_all(dir.join("projects")).expect("workspace");
    }
    (ws, other)
}

#[test]
fn an_op_whose_leased_workspace_refuses_writes_nothing_in_either() {
    let tmp = common::tempdir().expect("tempdir");
    let (ws, other) = two_workspaces(tmp.path());
    let outcome = refused_call(
        tmp.path(),
        &ws,
        Some(&other),
        "acquire-op-with-a-lease",
        &other,
    );
    assert_refused(&outcome);
    assert!(outcome.contains(&other.display().to_string()), "{outcome}");
    assert_eq!(listing(&ws), ["projects"]);
    assert_eq!(listing(&other), ["projects"]);
}

#[test]
fn a_lease_refused_after_its_owner_was_recorded_takes_the_record_back() {
    let tmp = common::tempdir().expect("tempdir");
    let (ws, other) = two_workspaces(tmp.path());
    let outcome = refused_call(
        tmp.path(),
        &ws,
        Some(&other),
        "acquire-op-with-a-lease",
        &other.join(".rwv-op-lease"),
    );
    assert_refused(&outcome);
    assert_eq!(listing(&ws), ["projects"]);
    assert_eq!(listing(&other), ["projects"]);
}

#[test]
fn a_lease_taken_alone_refuses_and_writes_nothing() {
    let tmp = common::tempdir().expect("tempdir");
    let (ws, other) = two_workspaces(tmp.path());
    let outcome = refused_call(tmp.path(), &ws, Some(&other), "acquire-lease", &other);
    assert_refused(&outcome);
    assert_eq!(listing(&other), ["projects"]);
}

#[test]
fn a_lease_widened_onto_a_refusing_workspace_refuses_and_is_not_taken() {
    let tmp = common::tempdir().expect("tempdir");
    let (ws, other) = two_workspaces(tmp.path());
    let outcome = refused_call(tmp.path(), &ws, Some(&other), "acquire-more-leases", &other);
    assert_refused(&outcome);
    assert_eq!(listing(&other), ["projects"]);
}

fn project_clone(tmp: &Path) -> PathBuf {
    let project = tmp.join("project");
    std::fs::create_dir_all(&project).expect("project");
    common::git_in(&project, &["init", "-q", "-b", "main"]);
    project
}

#[test]
fn a_landing_claim_on_a_refusing_store_refuses_and_writes_nothing() {
    let tmp = common::tempdir().expect("tempdir");
    let (ws, _) = two_workspaces(tmp.path());
    let project = project_clone(tmp.path());
    let store = project.join(".git");
    let before = listing(&store);
    let outcome = refused_call(tmp.path(), &ws, Some(&project), "take", &store);
    assert_refused(&outcome);
    assert_eq!(listing(&store), before);
}

#[test]
fn a_takeover_whose_guard_refuses_leaves_the_dead_claim_and_no_guard() {
    let tmp = common::tempdir().expect("tempdir");
    let (ws, _) = two_workspaces(tmp.path());
    let project = project_clone(tmp.path());
    let gone = tmp.path().join("gone");
    std::fs::create_dir_all(gone.join("projects")).expect("gone");
    let dead: OpId = "1".parse().expect("op id");
    op_state::write_owner(&gone, &owner_record(&gone, &dead)).expect("record");
    let vcs = repoweave::git::git_vcs();
    let _dead = repoweave::landing_claim::LandingClaim::take(
        vcs.as_ref(),
        &project,
        &repoweave::manifest::ProjectName::new("web-app").expect("project"),
        "main",
        &dead,
        &gone,
    )
    .expect("the dead op's claim");
    op_state::clear_owner(&gone);
    let claims = project.join(".git").join("rwv-landings");
    let [claim_dir] = listing(&claims).try_into().expect("one claim");
    let claim_dir = claims.join(claim_dir);

    let outcome = refused_call(
        tmp.path(),
        &ws,
        Some(&project),
        "take-over",
        &claim_dir.join(".rwv-landing.lock.takeover"),
    );
    assert_refused(&outcome);
    assert_eq!(listing(&claim_dir), [".rwv-landing.lock"]);
}

#[test]
fn a_fetch_claim_on_a_refusing_store_refuses() {
    let tmp = common::tempdir().expect("tempdir");
    let (ws, _) = two_workspaces(tmp.path());
    let project = project_clone(tmp.path());
    let store = project.join(".git");
    let before = listing(&store);
    let outcome = refused_call(tmp.path(), &ws, Some(&project), "fetch", &store);
    assert_refused(&outcome);
    assert_eq!(listing(&store), before);
}

#[test]
fn a_ledger_claim_in_a_refusing_workspace_refuses() {
    let tmp = common::tempdir().expect("tempdir");
    let (ws, _) = two_workspaces(tmp.path());
    let outcome = refused_call(tmp.path(), &ws, None, "stamp", &ws);
    assert_refused(&outcome);
    assert_eq!(listing(&ws), ["projects"]);
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
        outcome.contains(&format!(
            "Move what holds {} (a workspace, or the project clone whose store it is) to a \
             filesystem that makes hard links",
            workspace.display()
        )),
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

    /// The refusal comes before the op stages its record: armed at the first
    /// staging, the child never reaches it.
    #[test]
    fn an_op_whose_leased_workspace_refuses_stages_nothing_first() {
        let tmp = common::tempdir().expect("tempdir");
        let (ws, other) = two_workspaces(tmp.path());
        let results = tmp.path().join("results");
        std::fs::create_dir_all(&results).expect("results");
        let mut cmd = child(&ws, "100", Some(NEITHER));
        cmd.env(CHILD_RESULTS, &results);
        cmd.env(CHILD_CALL, "acquire-op-with-a-lease");
        cmd.env(CHILD_OTHER, &other);
        cmd.env(WITHHOLD_UNDER, &other);
        if let Ok(held) = Paused::spawn(cmd, STAGED, &tmp.path().join("pause")) {
            drop(held);
            panic!("the op staged its record before the refusal");
        }
        assert_refused(&std::fs::read_to_string(results.join("100")).expect("recorded"));
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

    /// A sweep that meets a writer's temp before the writer holds it removes
    /// it; the writer finds it gone at its publish, stages it again, and its
    /// publish lands.
    #[test]
    #[cfg(unix)]
    fn a_temp_swept_before_its_writer_held_it_is_staged_again_and_published() {
        let tmp = common::tempdir().expect("tempdir");
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mut cmd = child(&workspace, "100", None);
        cmd.env(CHILD_REPLACES, "1");
        let held = Paused::spawn(cmd, "durable-file-created", &tmp.path().join("pause"))
            .unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(temps(&workspace).len(), 1);

        let mine = owner_record(&workspace, &"200".parse().expect("op id"));
        StateFile::OpRecord
            .in_dir(&workspace)
            .publish(&serde_json::to_vec(&mine).expect("serialize"))
            .expect("a publish whose sweep meets an unheld temp");
        assert_eq!(temps(&workspace), Vec::<String>::new(), "the sweep took it");

        let out = held.release();
        assert!(out.status.success(), "{out:?}");
        let published: serde_json::Value =
            serde_json::from_slice(&std::fs::read(workspace.join(OP_FILE)).expect("published"))
                .expect("whole");
        assert_eq!(
            published["id"], "100",
            "the swept writer staged again and published"
        );
        assert_eq!(temps(&workspace), Vec::<String>::new());
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
