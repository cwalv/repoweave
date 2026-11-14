//! E2E acceptance tests for `rwv abort`'s hardening:
//!
//! 1. **Restore to the savepoint**: each repo is put back at its savepoint.
//!    A tip that is neither the savepoint nor one the op recorded, and holds
//!    commits the savepoint does not reach, is kept as a backup first.
//! 2. **What a reset discards is kept**: uncommitted edits, and the working
//!    copy of a stopped operation, are kept as backups before the step that
//!    would discard them. No other abort writes a ref: the commits an abort
//!    discards unkept are the ones the op itself made.
//!
//! Each case manufactures the op-state file and savepoint by hand (the
//! same shape `phase_reentry_test.rs` and the conflict-marker abort test
//! use) so the unit under test is the abort path itself, not whatever
//! the surrounding phase machine happened to produce.
//!
//! ## Lock-fixture audit (2026-08-12)
//!
//! Pre-fix, `write_lock` here wrote a TOML shape into `rwv.lock`, which is
//! JSON post-v0.17 and refused by the production parser at line 1 col 2.
//! An earlier fix flagged this class in `atomic_lease_acquisition_test`
//! (where an unparseable lock made every sync-under-test refuse at the
//! lock-read, letting refusal-shaped assertions pass by coincidence).
//!
//! Applying the same suspicion here: I replaced `write_lock` with garbage
//! (`!!!GARBAGE_NOT_PARSEABLE!!!`) and every one of the 9 tests still passed.
//! Reason: `run_abort` (`src/sync.rs`) uses `Project::from_dir_skip_lock` by
//! design — its comment: "abort's contract is 'the state is bad, get me
//! out'. rwv.lock may contain git conflict markers from the half-completed
//! rebase, so we must not try to parse it." So none of these tests routed
//! through a lock parse pre- or post-fix; each was testing what its name
//! claims. Verdict per test: WAS TESTING WHAT IT NAMES — for every case.
//!
//! The fix therefore does not change any assertion's meaning. It aligns
//! the fixture's on-disk contract with production so future regressions
//! (e.g. a new abort-path preflight that DOES parse the lock) can't be
//! masked by the fixture, and so the file stops shipping malformed content
//! for a file the rest of rwv treats as JSON.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

use crate::common;
use repoweave::refusal::RefusalKind;

// ---------------------------------------------------------------------------
// Helpers (kept local — mirroring e2e_sync_abort_test.rs style)
// ---------------------------------------------------------------------------

fn rwv() -> AssertCommand {
    common::rwv()
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
}

const SERVER_URL: &str = "https://github.com/chatly/server.git";
const SERVER_PATH: &str = "github/chatly/server";

/// A workspace usable for in-place abort fixtures: one manifest repo
/// (`github/chatly/server`) plus a project repo with `rwv.toml`/`rwv.lock`
/// committed. Mirrors `make_locked_workspace` in `e2e_sync_abort_test.rs`
/// but kept local so this file's contract is self-contained.
struct Fixture {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

fn make_fixture(parent: &Path, name: &str) -> Fixture {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server_dir = root.join(SERVER_PATH);
    let server_sha = init_repo(&server_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    common::set_up_lock_driver(&project_dir);
    write_manifest(&project_dir, &[(SERVER_PATH, SERVER_URL)]);
    common::fixture_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &server_sha)]);
    common::git_in(
        &project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);

    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    Fixture {
        root,
        project_dir,
        server_dir,
    }
}

/// Plant a v2 owner record at the workspace root with the given op-id,
/// phase, and per-repo converged_tips map (key → SHA). Mirrors the JSON
/// `rwv abort` reads: a sync that takes in the project and the one member.
fn plant_owner_record(workspace: &Path, op_id: &str, phase: &str, converged_tips: &[(&str, &str)]) {
    plant_owner_record_rebasing(workspace, op_id, phase, converged_tips, Vec::new());
}

/// [`plant_owner_record`], with the rebases the op journaled starting.
fn plant_owner_record_rebasing(
    workspace: &Path,
    op_id: &str,
    phase: &str,
    converged_tips: &[(&str, &str)],
    rebases: Vec<repoweave::op_state::Rebasing>,
) {
    let tips_json = converged_tips
        .iter()
        .map(|(key, sha)| format!("\"{key}\": \"{sha}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let json = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"sync\", \"history\": null, \"project\": \"web-app\", \
         \"source\": \"{root}\", \"target\": \"{root}\", \"retire\": false, \"phase\": \"{phase}\", \
         \"converged_tips\": {{{tips_json}}}, \
         \"started_at\": \"2026-05-27T10:00:00Z\", \"schema\": 8, \"release\": \"0.0.0\", \"take_in\": {take_in}}}",
        root = common::json_escaped(workspace),
        take_in = sync_plan(rebases),
    );
    std::fs::write(workspace.join(".rwv-op"), &json).unwrap();
}

fn sync_plan(rebases: Vec<repoweave::op_state::Rebasing>) -> String {
    let planned = repoweave::vcs::CommitId::parse(&"0".repeat(40)).unwrap();
    let mut plan = repoweave::op_state::TakeInRecord {
        sync_from: Some(repoweave::op_state::SyncFrom::Target("main".to_owned())),
        rebases,
        ..Default::default()
    };
    for key in [
        repoweave::manifest::RepoKey::Project,
        repoweave::manifest::RepoKey::Member(
            repoweave::manifest::RepoPath::new(SERVER_PATH).unwrap(),
        ),
    ] {
        plan.upstream_tips.insert(key, planned.clone());
    }
    serde_json::to_string(&plan).unwrap()
}

/// Create a `refs/rwv/pre-op/<op-id>` savepoint pointing at `sha` in `repo`.
fn plant_savepoint(repo: &Path, op_id: &str, sha: &str) {
    common::savepoint::plant_at(repo, op_id, sha);
}

/// Every ref under `prefix` in `repo`, as `(refname, sha)`.
fn refs_under(repo: &Path, prefix: &str) -> Vec<(String, String)> {
    let out = common::git_in(
        repo,
        &["for-each-ref", "--format=%(refname) %(objectname)", prefix],
    );
    out.lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(r, o)| (r.to_owned(), o.to_owned()))
        .collect()
}

/// The ref abort used to write before it restored anything. No abort writes
/// one now.
const PRE_ABORT: &str = "refs/rwv/pre-abort/";
const BACKUP: &str = "refs/rwv/backup/";

fn assert_writes_no_ref(repo: &Path) {
    assert_eq!(refs_under(repo, PRE_ABORT), vec![], "no pre-abort ref");
    assert_eq!(refs_under(repo, BACKUP), vec![], "no backup");
}

fn savepoint_sha(repo: &Path, op_id: &str) -> Option<String> {
    common::savepoint::checkout_tip(repo, op_id)
}

// ---------------------------------------------------------------------------
// Case 1: Untouched — repo tip == savepoint
// ---------------------------------------------------------------------------

/// When the repo's tip never moved, abort succeeds and writes no ref: there
/// is nothing the op made to keep.
#[test]
fn abort_untouched_is_a_noop_and_writes_no_ref() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000001Z";
    let server_pre = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    let project_pre = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);

    plant_savepoint(&ws.server_dir, op_id, &server_pre);
    plant_savepoint(&ws.project_dir, op_id, &project_pre);
    plant_owner_record(&ws.root, op_id, "replay", &[]);

    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("untouched"));

    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_pre
    );
    assert_eq!(
        common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]),
        project_pre
    );
    assert_writes_no_ref(&ws.server_dir);
    assert_writes_no_ref(&ws.project_dir);
    assert!(!ws.root.join(".rwv-op").exists());
}

/// A savepoint taken with the checkout detached, which is now on `main` at
/// that same tip: abort switches it back to a detached HEAD at the savepoint
/// and leaves `main` where it is.
#[test]
fn abort_switches_back_to_a_detached_savepoint_leaving_the_branch() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");
    let op_id = "20991231T000011Z";
    let server_pre = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    let project_pre = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    common::savepoint::plant_on(
        &ws.server_dir,
        op_id,
        common::savepoint::SavedOn::Detached,
        &server_pre,
    );
    plant_savepoint(&ws.project_dir, op_id, &project_pre);
    plant_owner_record(&ws.root, op_id, "replay", &[]);

    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("switched back off 'main'"));

    assert_eq!(
        common::git_in(&ws.server_dir, &["branch", "--show-current"]),
        ""
    );
    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_pre
    );
    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "main"]),
        server_pre
    );
    assert!(savepoint_sha(&ws.server_dir, op_id).is_none());
    assert!(!ws.root.join(".rwv-op").exists());
}

/// A savepoint taken on `main`, the checkout now on `topic` at a commit of
/// someone else's: abort switches back to `main`, `topic` keeping the
/// commit.
#[test]
fn abort_switches_back_to_the_branch_its_savepoint_names() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");
    let op_id = "20991231T000012Z";
    let server_pre = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    let project_pre = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    common::savepoint::plant_on(
        &ws.server_dir,
        op_id,
        common::savepoint::SavedOn::Branch("main"),
        &server_pre,
    );
    plant_savepoint(&ws.project_dir, op_id, &project_pre);
    plant_owner_record(&ws.root, op_id, "replay", &[]);
    common::git_in(&ws.server_dir, &["switch", "-q", "-c", "topic"]);
    let theirs = make_commit(&ws.server_dir, "theirs.txt", "theirs\n", "theirs");

    rwv()
        .args(["abort"])
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("switched back off 'topic'"));
    assert_eq!(
        common::git_in(&ws.server_dir, &["branch", "--show-current"]),
        "main"
    );
    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_pre
    );
    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "topic"]),
        theirs
    );
}

// ---------------------------------------------------------------------------
// Case 2: Converged — repo tip == recorded converged tip
// ---------------------------------------------------------------------------

/// When the repo's tip is the recorded converged tip (relock completed),
/// abort resets back to the savepoint. The converged tip is a commit the op
/// made, so nothing is kept for it and the output names no backup. This is
/// the "post-replay crash" path.
#[test]
fn abort_of_the_ops_own_converged_tip_keeps_it_in_the_branch_slot_in_one_line() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000002Z";

    let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    let server_converged = make_commit(
        &ws.server_dir,
        "converged.txt",
        "converged\n",
        "server: converged",
    );

    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);

    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(
        &ws.root,
        op_id,
        "relock",
        &[(SERVER_PATH, &server_converged)],
    );

    let output = rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("restored"))
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let server: Vec<&str> = stdout
        .lines()
        .filter(|line| line.starts_with(&format!("  {SERVER_PATH}: ")))
        .filter(|line| !line.contains(" reflinked, "))
        .collect();
    let [line] = server.as_slice() else {
        panic!("one line for the server:\n{stdout}");
    };
    assert!(
        line.contains("what abort discarded is kept as backup main/aborted (restored)")
            && !stdout.contains(op_id),
        "{stdout}"
    );
    let show = line.split_once("this shows it: ").unwrap().1;
    let words: Vec<&str> = show.split_whitespace().collect();
    assert_eq!(words[0], "git");
    assert!(common::git_in(Path::new("."), &words[1..]).contains("+converged"));

    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_savepoint
    );
    assert_eq!(
        common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]),
        project_savepoint,
        "the project never moved; abort is a no-op there"
    );
    assert_eq!(
        refs_under(&ws.server_dir, BACKUP),
        vec![("refs/rwv/backup/main/aborted".to_owned(), server_converged)]
    );
    assert_writes_no_ref(&ws.project_dir);
    assert!(
        savepoint_sha(&ws.server_dir, op_id).is_none(),
        "savepoint should be dropped after a successful restore"
    );
}

/// The savepoint holds `saved` at `path`; the op's converged tip removed it
/// (and ignores `ign.txt`). Someone then made `made`, holding `text`, which
/// the reset to the savepoint destroys: abort keeps it as a backup first.
fn abort_keeps_what_the_reset_destroys(path: &str, made: &str, text: &str) {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");
    let op_id = "20991231T000013Z";
    let dir = &ws.server_dir;
    let saved = dir.join(path);
    std::fs::create_dir_all(saved.parent().unwrap()).unwrap();
    std::fs::write(&saved, "saved\n").unwrap();
    common::git_in(dir, &["add", path]);
    common::git_in(dir, &["commit", "-qm", "saved path"]);
    let savepoint = common::git_in(dir, &["rev-parse", "HEAD"]);
    common::git_in(dir, &["rm", "-rq", path]);
    std::fs::write(dir.join(".gitignore"), "ign.txt\n").unwrap();
    common::git_in(dir, &["add", ".gitignore"]);
    common::git_in(dir, &["commit", "-qm", "converged"]);
    let converged = common::git_in(dir, &["rev-parse", "HEAD"]);
    let made_at = dir.join(made);
    std::fs::create_dir_all(made_at.parent().unwrap()).unwrap();
    std::fs::write(&made_at, format!("{text}\n")).unwrap();
    plant_savepoint(dir, op_id, &savepoint);
    let project_pre = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.project_dir, op_id, &project_pre);
    plant_owner_record(&ws.root, op_id, "relock", &[(SERVER_PATH, &converged)]);

    let out = common::cli::run(&["abort"], &ws.root);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("what abort discarded is kept as backup"),
        "{}",
        common::cli::describe(&out)
    );
    assert_eq!(common::git_in(dir, &["rev-parse", "HEAD"]), savepoint);
    let backups = refs_under(dir, BACKUP);
    assert_eq!(
        backups
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [
            "refs/rwv/backup/main/aborted",
            "refs/rwv/backup/main/aborted-working-copy"
        ],
    );
    let backups = &backups[1..];
    assert_eq!(
        common::git_in(dir, &["show", &format!("{}:{made}", backups[0].1)]),
        text
    );
}

/// A change staged, then edited again in the working tree, before abort
/// resets the checkout the op converged: the backup holds both, the staged
/// content as its second parent.
#[test]
fn abort_keeps_a_staged_change_the_working_tree_no_longer_holds() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");
    let op_id = "20991231T000014Z";
    let dir = &ws.server_dir;
    let savepoint = common::git_in(dir, &["rev-parse", "HEAD"]);
    let converged = make_commit(dir, "converged.txt", "converged\n", "converged");
    std::fs::write(dir.join("README.md"), "staged\n").unwrap();
    common::git_in(dir, &["add", "README.md"]);
    std::fs::write(dir.join("README.md"), "edited after staging\n").unwrap();
    plant_savepoint(dir, op_id, &savepoint);
    let project_pre = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.project_dir, op_id, &project_pre);
    plant_owner_record(&ws.root, op_id, "relock", &[(SERVER_PATH, &converged)]);

    let out = common::cli::run(&["abort"], &ws.root);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    let backups = refs_under(dir, BACKUP);
    assert_eq!(
        backups
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        [
            "refs/rwv/backup/main/aborted",
            "refs/rwv/backup/main/aborted-working-copy"
        ],
    );
    let backups = &backups[1..];
    let kept = &backups[0].1;
    assert_eq!(
        common::git_in(dir, &["show", &format!("{kept}:README.md")]),
        "edited after staging"
    );
    assert_eq!(
        common::git_in(dir, &["show", &format!("{kept}^2:README.md")]),
        "staged"
    );
}

#[test]
fn abort_keeps_an_ignored_file_at_a_path_the_savepoint_tracks() {
    abort_keeps_what_the_reset_destroys("ign.txt", "ign.txt", "ignored edit");
}

#[test]
fn abort_keeps_an_untracked_file_where_the_savepoint_has_a_directory() {
    abort_keeps_what_the_reset_destroys("d/file", "d", "file edit");
}

#[test]
fn abort_keeps_an_untracked_directory_where_the_savepoint_has_a_file() {
    abort_keeps_what_the_reset_destroys("p", "p/inner", "dir edit");
}

// ---------------------------------------------------------------------------
// Case 3: Mid-op — repo is in a VCS-native rebase
// ---------------------------------------------------------------------------

/// When the repo is mid-rebase (VCS-native wreckage), abort cancels the
/// rebase + resets to the savepoint. A rebase in flight is the op's own
/// wreckage, so nothing is kept for it.
#[test]
fn abort_mid_op_cancels_and_resets_to_savepoint() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000003Z";

    // Two diverging commits that touch the same line so the rebase stalls;
    // the savepoint is the tip the rebase starts from, as the op's own
    // rebase starts from the tip it savepointed.
    let base = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    make_commit(
        &ws.server_dir,
        "conflict.txt",
        "main version\n",
        "main: conflict base",
    );
    let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);
    common::git_in(&ws.server_dir, &["checkout", "-b", "diverge", &base]);
    make_commit(
        &ws.server_dir,
        "conflict.txt",
        "diverge version\n",
        "diverge: conflict",
    );
    common::git_in(&ws.server_dir, &["checkout", "main"]);
    let onto = common::git_in(&ws.server_dir, &["rev-parse", "diverge"]);
    let _ = std::process::Command::new("git")
        .args(["rebase", "diverge"])
        .current_dir(&ws.server_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output();
    assert!(
        ws.server_dir.join(".git/rebase-merge").exists()
            || ws.server_dir.join(".git/rebase-apply").exists(),
        "expected mid-rebase state on the server repo"
    );

    // Project repo: untouched.
    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);

    let id = |sha: &str| repoweave::vcs::CommitId::parse(sha).unwrap();
    plant_owner_record_rebasing(
        &ws.root,
        op_id,
        "replay",
        &[],
        vec![repoweave::op_state::Rebasing {
            repo: repoweave::manifest::RepoKey::Member(
                repoweave::manifest::RepoPath::new(SERVER_PATH).unwrap(),
            ),
            branch: "main".to_owned(),
            base: id(&base),
            from: id(&server_savepoint),
            onto: id(&onto),
        }],
    );

    rwv().arg("abort").current_dir(&ws.root).assert().success();

    // Rebase state gone.
    assert!(!ws.server_dir.join(".git/rebase-merge").exists());
    assert!(!ws.server_dir.join(".git/rebase-apply").exists());
    // Server back at the savepoint, the stopped rebase's working copy kept.
    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_savepoint
    );
    assert_eq!(refs_under(&ws.server_dir, "refs/rwv/backup/").len(), 1);
}

// ---------------------------------------------------------------------------
// Case 4: a tip the op did not write
// ---------------------------------------------------------------------------

/// A repo's tip that is neither the savepoint nor one the op recorded, and
/// holds a commit the savepoint does not reach, is kept as a backup before
/// abort puts the repo back at its savepoint, and the abort ends the op.
#[test]
fn abort_keeps_a_tip_the_op_did_not_write_and_restores() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000004Z";

    let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    let foreign_tip = make_commit(
        &ws.server_dir,
        "foreign.txt",
        "foreign commit\n",
        "foreign: someone else built on this",
    );
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    let recorded_converged = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    plant_owner_record(
        &ws.root,
        op_id,
        "relock",
        &[(SERVER_PATH, recorded_converged)],
    );

    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("kept as backup"));

    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_savepoint
    );
    let kept = refs_under(&ws.server_dir, "refs/rwv/backup/");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(kept[0].1, foreign_tip);
    assert!(!ws.root.join(".rwv-op").exists());
}

// ---------------------------------------------------------------------------
// Cross-cutting: pre-abort refs are written on every clean abort.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Legible refusal output
// ---------------------------------------------------------------------------

/// When repos are skipped (no savepoint) or untouched, they must NOT produce
/// one line each — instead a single aggregate "summary:" line is emitted on
/// stdout. This avoids the 90-line noise seen in the motivating incident.
#[test]
fn abort_noise_collapses_to_summary_line() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000010Z";
    // Plant ONLY a project savepoint; server has no savepoint → NoSavepoint.
    // Project tip == savepoint → Untouched.
    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "replay", &[]);

    let assert = rwv().arg("abort").current_dir(&ws.root).assert().success();

    // A "summary:" line must appear somewhere in stdout.
    assert.stdout(predicate::str::contains("summary:"));
}

/// A tip slot holding a name rather than a commit id is a corrupt record.
/// Abort refuses it whole, naming the value, and never resolves the name:
/// nothing is reset and nothing is kept.
#[test]
fn abort_never_resolves_a_tag_name_in_an_owner_records_tip_slot() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000020Z";

    let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    let server_converged = make_commit(
        &ws.server_dir,
        "converged.txt",
        "converged\n",
        "server: converged",
    );
    common::git_in(&ws.server_dir, &["tag", "v2.0", &server_converged]);
    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "relock", &[(SERVER_PATH, "v2.0")]);

    let out = rwv().arg("abort").current_dir(&ws.root).output().unwrap();
    let stderr = common::cli::refused(&out, RefusalKind::UnreadableOpRecord);
    assert!(
        stderr.contains("'v2.0' is not a commit id"),
        "abort names the value in the slot.\nstderr:\n{stderr}"
    );
    assert!(refs_under(&ws.server_dir, "refs/rwv/backup/").is_empty());
    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_converged
    );
    assert!(ws.root.join(".rwv-op").exists());
}

// ---------------------------------------------------------------------------
// Case: partial restore failure — exit 1, "abort completed with failures",
// op-state retained, retry succeeds once the obstruction is gone
// ---------------------------------------------------------------------------

/// cli.md:347 / the Exit codes section: a partial restore failure prints
/// "abort completed with failures", exits 1, and op-state is retained so
/// `rwv abort` can be retried. Forces the server repo's `git reset --hard`
/// to fail by holding its `.git/index.lock`; the project repo is already at
/// its savepoint (untouched, no reset attempted) so it restores regardless.
///
/// Revert to watch it go red: in `run_abort` (src/sync.rs), change
/// `if tally.clean() { op_state::clear_all_at(&workspace_dir); } else if
/// tally.any_foreign { ... }` to an unconditional
/// `op_state::clear_all_at(&workspace_dir);` — op-state then clears even on
/// a failed abort, and the "op-state must be retained" assertion below goes
/// red.
#[test]
fn abort_partial_failure_exits_with_message_and_retains_op_state_for_retry() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000099Z";

    let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    let server_converged = make_commit(
        &ws.server_dir,
        "converged.txt",
        "converged\n",
        "server: converged",
    );
    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);

    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(
        &ws.root,
        op_id,
        "relock",
        &[(SERVER_PATH, &server_converged)],
    );

    // Hold the server repo's index lock so its `git reset --hard` fails.
    let index_lock = ws.server_dir.join(".git/index.lock");
    std::fs::write(&index_lock, b"").unwrap();

    let output = rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .output()
        .expect("rwv abort");
    assert_eq!(
        output.status.code(),
        Some(1),
        "a partial restore failure must exit 1; stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("abort completed with failures"),
        "expected the partial-failure message; got:\n{combined}"
    );

    // The server repo's reset never completed — still at the converged tip.
    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_converged,
        "the failed repo must be left exactly where it was"
    );
    // Op-state is retained so the operator can retry.
    assert!(
        ws.root.join(".rwv-op").exists(),
        "op-state must be retained after a partial restore failure"
    );
    // The savepoint survives the failed attempt too.
    assert_eq!(
        savepoint_sha(&ws.server_dir, op_id).as_deref(),
        Some(server_savepoint.as_str()),
        "a failed reset must not drop the savepoint it never reached"
    );

    // Clear the obstruction and retry: this time it goes through cleanly.
    std::fs::remove_file(&index_lock).unwrap();
    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("restored"));

    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_savepoint,
        "the retry must finish the restore the lock blocked"
    );
    assert!(
        !ws.root.join(".rwv-op").exists(),
        "op-state must clear once the retry succeeds"
    );
}
