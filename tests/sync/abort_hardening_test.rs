//! E2E acceptance tests for `rwv abort`'s hardening:
//!
//! 1. **HEAD-verified restore**: `reset --hard` is gated on the tip being
//!    attributable to the op — equal to the savepoint, equal to the
//!    recorded converged tip, or the repo is in a VCS-native mid-op
//!    state. Anything else (foreign commits landed after a crash, or
//!    another agent built on an advanced target) is REPORTED with a named
//!    `foreign-tip` violation and recovery hints, never reset, and the
//!    refusal writes nothing.
//! 2. **Abandon keeps a backup**: `--abandon-foreign-tip <repo>` moves a
//!    repo off a foreign tip only after the tip is kept as an `abandoned`
//!    backup, which `rwv backup list` shows and `rwv backup drop` drops.
//!    No other abort writes a ref: the commits an abort discards are the
//!    ones the op itself made.
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

// ---------------------------------------------------------------------------
// Helpers (kept local — mirroring e2e_sync_abort_test.rs style)
// ---------------------------------------------------------------------------

fn rwv() -> AssertCommand {
    common::rwv()
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routes: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routes, vec![token], "stderr was:\n{stderr}");
}

fn try_git(args: &[&str], dir: &Path) -> bool {
    common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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
         \"advanced_tips\": {{}}, \"converged_tips\": {{{tips_json}}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\", \"schema\": 6, \"release\": \"0.0.0\", \"take_in\": {take_in}}}",
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

/// A workspace with `repo_paths.len()` manifest repos instead of
/// [`make_fixture`]'s one, for the cases that must show consent given for one
/// repo doing nothing to another.
struct MultiRepoFixture {
    root: PathBuf,
    project_dir: PathBuf,
    /// Manifest repo directories, in the order `repo_paths` named them.
    repo_dirs: Vec<PathBuf>,
}

fn make_multi_repo_fixture(parent: &Path, name: &str, repo_paths: &[&str]) -> MultiRepoFixture {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let mut repo_dirs = Vec::new();
    let mut shas = Vec::new();
    for path in repo_paths {
        let dir = root.join(path);
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        shas.push(init_repo(&dir));
        repo_dirs.push(dir);
    }

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    common::set_up_lock_driver(&project_dir);

    let urls: Vec<String> = repo_paths
        .iter()
        .map(|p| format!("https://example.invalid/{p}.git"))
        .collect();
    let manifest_rows: Vec<(&str, &str)> = repo_paths
        .iter()
        .zip(urls.iter())
        .map(|(p, u)| (*p, u.as_str()))
        .collect();
    write_manifest(&project_dir, &manifest_rows);
    let lock_rows: Vec<(&str, &str, &str)> = repo_paths
        .iter()
        .zip(urls.iter())
        .zip(shas.iter())
        .map(|((p, u), s)| (*p, u.as_str(), s.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &lock_rows);
    common::git_in(
        &project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    MultiRepoFixture {
        root,
        project_dir,
        repo_dirs,
    }
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
/// someone else's: abort refuses and leaves it there; named on
/// `--abandon-foreign-tip`, it switches back to `main`, `topic` keeping the
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

    let refusal = common::cli::run(&["abort"], &ws.root);
    common::cli::refused(&refusal, repoweave::refusal::RefusalKind::ForeignTip);
    assert_eq!(
        common::git_in(&ws.server_dir, &["branch", "--show-current"]),
        "topic"
    );

    rwv()
        .args(["abort", "--abandon-foreign-tip", SERVER_PATH])
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
fn abort_converged_resets_to_savepoint_and_keeps_nothing_the_op_made() {
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
    assert!(
        !stdout.contains("backup") && !stdout.contains(op_id),
        "a clean restore's output names neither a backup nor the op-id:\n{stdout}"
    );

    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_savepoint
    );
    assert_eq!(
        common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]),
        project_savepoint,
        "the project never moved; abort is a no-op there"
    );
    assert_writes_no_ref(&ws.server_dir);
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
        String::from_utf8_lossy(&out.stdout).contains("its uncommitted edits are kept as backup"),
        "{}",
        common::cli::describe(&out)
    );
    assert_eq!(common::git_in(dir, &["rev-parse", "HEAD"]), savepoint);
    let backups = refs_under(dir, BACKUP);
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(
        common::git_in(dir, &["show", &format!("{}:{made}", backups[0].1)]),
        text
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
    // Server back at the savepoint.
    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_savepoint
    );
    assert_writes_no_ref(&ws.server_dir);
}

// ---------------------------------------------------------------------------
// Case 4: Foreign — repo tip diverged off the attributable set
// ---------------------------------------------------------------------------

/// When the repo's tip is neither the savepoint, nor the recorded
/// converged tip, nor a mid-op state, abort REFUSES to reset that repo.
///
/// Acceptance:
/// - exit nonzero
/// - violation named in output (`foreign-tip`)
/// - the foreign repo's tip is UNCHANGED, and the refusal says it was left
///   in place
/// - the refusal writes no ref: nothing moved, so nothing needs keeping
/// - the savepoint ref is NOT dropped (so re-running abort after manual
///   reconciliation can still find it)
/// - op-state is RETAINED so the operator can re-run abort
#[test]
fn abort_foreign_tip_refuses_and_preserves_state() {
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

    // Record an unrelated converged tip so `recorded_converged_tip` is
    // Some(_) but does NOT match the foreign tip — exercises the "have
    // converged data, still foreign" branch.
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
        .failure()
        .stderr(predicate::str::contains("foreign-tip"))
        .stderr(predicate::str::contains(SERVER_PATH))
        .stderr(predicate::str::contains("(left in place)"));

    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        foreign_tip,
        "foreign-tip refusal must not reset the repo"
    );
    assert_writes_no_ref(&ws.server_dir);
    assert_eq!(
        savepoint_sha(&ws.server_dir, op_id).as_deref(),
        Some(server_savepoint.as_str()),
        "savepoint must be retained when restore is refused so re-run is possible"
    );
    assert!(
        ws.root.join(".rwv-op").exists(),
        "op-state must be retained on foreign-tip refusal"
    );
}

/// A refused abort writes nothing, so the operator reconciling by hand and
/// re-running abort finds a plain restore: it succeeds and keeps nothing.
#[test]
fn abort_rerun_after_reconciling_a_foreign_tip_succeeds_and_keeps_nothing() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000006Z";

    let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    make_commit(
        &ws.server_dir,
        "foreign.txt",
        "foreign commit\n",
        "foreign: someone else built on this",
    );
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);

    plant_owner_record(&ws.root, op_id, "relock", &[]);

    rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("foreign-tip"));
    assert_writes_no_ref(&ws.server_dir);

    common::git_in(&ws.server_dir, &["reset", "--hard", &server_savepoint]);

    rwv().arg("abort").current_dir(&ws.root).assert().success();
    assert_writes_no_ref(&ws.server_dir);
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

/// When a foreign-tip refusal occurs, the recovery-options block must appear
/// EXACTLY ONCE in stderr, not once per refused repo. Two repos both foreign
/// → single options block.
#[test]
fn abort_foreign_tip_options_block_printed_once() {
    let tmp = common::tempdir().unwrap();

    // Build a fixture with TWO manifest repos to exercise the "multiple
    // refusals, options block once" path.
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(root.join("github/chatly2")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server1_dir = root.join("github/chatly/server");
    let server1_sha = init_repo(&server1_dir);
    let server2_dir = root.join("github/chatly2/server");
    let server2_sha = init_repo(&server2_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    common::set_up_lock_driver(&project_dir);
    // Manifest is TOML — hand-rolled shape is fine. Lock is JSON — route it
    // through `write_lock` (round-trips via the production parser) so the
    // fixture's on-disk shape matches production.
    write_manifest(
        &project_dir,
        &[
            (
                "github/chatly/server",
                "https://github.com/chatly/server.git",
            ),
            (
                "github/chatly2/server",
                "https://github.com/chatly2/server.git",
            ),
        ],
    );
    common::fixture_lock(
        &project_dir,
        &[
            (
                "github/chatly/server",
                "https://github.com/chatly/server.git",
                &server1_sha,
            ),
            (
                "github/chatly2/server",
                "https://github.com/chatly2/server.git",
                &server2_sha,
            ),
        ],
    );
    common::git_in(
        &project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    let op_id = "20991231T000011Z";

    // Advance both repos to foreign tips.
    let s1_savepoint = server1_sha.clone();
    make_commit(&server1_dir, "foreign.txt", "foreign\n", "foreign: s1");
    plant_savepoint(&server1_dir, op_id, &s1_savepoint);

    let s2_savepoint = server2_sha.clone();
    make_commit(&server2_dir, "foreign.txt", "foreign\n", "foreign: s2");
    plant_savepoint(&server2_dir, op_id, &s2_savepoint);

    let project_savepoint = common::git_in(&project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&project_dir, op_id, &project_savepoint);

    plant_owner_record(&root, op_id, "replay", &[]);

    let output = rwv()
        .arg("abort")
        .current_dir(&root)
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr);

    // "recovery options" must appear exactly once.
    let occurrence_count = stderr.matches("recovery options").count();
    assert_eq!(
        occurrence_count, 1,
        "recovery options block must appear exactly once in stderr, found {occurrence_count} time(s).\nstderr:\n{stderr}"
    );

    // Both repos must be mentioned (one line per refused repo).
    assert!(
        stderr.contains("github/chatly/server"),
        "stderr must name first refused repo"
    );
    assert!(
        stderr.contains("github/chatly2/server"),
        "stderr must name second refused repo"
    );
}

/// When a foreign-tip refusal occurs, the output must include the blocking
/// commits (savepoint..tip) inline for each refused repo.
#[test]
fn abort_foreign_tip_shows_blocking_commits() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000012Z";

    let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    // Make two identifiable commits on the server.
    make_commit(
        &ws.server_dir,
        "a.txt",
        "a\n",
        "chore: upstream-main-advance-1",
    );
    let newest = make_commit(
        &ws.server_dir,
        "b.txt",
        "b\n",
        "chore: upstream-main-advance-2",
    );
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "replay", &[]);

    let output = rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Commit subjects from the blocking range should appear in stderr.
    assert!(
        stderr.contains("upstream-main-advance-1") || stderr.contains("upstream-main-advance-2"),
        "blocking commits must appear in stderr.\nstderr:\n{stderr}"
    );

    // Shape text should indicate strictly-ahead (2 commits ahead, 0 behind).
    assert!(
        stderr.contains("ahead"),
        "shape description must appear in stderr.\nstderr:\n{stderr}"
    );

    let listed = format!("\t  {} chore: upstream-main-advance-2", &newest[..12]);
    assert!(
        stderr.lines().any(|l| l == listed),
        "a blocking commit is listed by its 12-character id.\nstderr:\n{stderr}"
    );
}

/// A tip slot holding a name rather than a commit id is a corrupt record.
/// Abort reads only its header, so the name is never resolved: the repo the
/// tag points at is not attributed to the op and is refused as foreign, its
/// tip left in place.
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

    let output = rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("'v2.0' is not a commit id"),
        "the refusal names the value in the slot.\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("foreign-tip violation"),
        "the tagged tip is not attributed to the op.\nstderr:\n{stderr}"
    );
    assert_writes_no_ref(&ws.server_dir);
    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_converged
    );
}

// ---------------------------------------------------------------------------
// `--abandon-foreign-tip`: the named, per-repo waiver of rail 2
// ---------------------------------------------------------------------------

/// The flag's happy path, and the ordering proof that makes "abandon" the
/// honest word for it.
///
/// After abort restores over the foreign tip, an `abandoned` backup must hold
/// the FOREIGN tip, and `rwv backup list` must show it. That is what
/// separates abandonment from destruction: a backup written after the reset,
/// or not at all, leaves the foreign commits named by nothing.
#[test]
fn abandon_foreign_tip_restores_and_keeps_the_abandoned_tip_as_a_backup() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000020Z";

    let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    let foreign_tip = make_commit(
        &ws.server_dir,
        "foreign.txt",
        "foreign\n",
        "foreign: another agent advanced this branch",
    );
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "relock", &[]);

    rwv()
        .arg("abort")
        .arg(format!("--abandon-foreign-tip={SERVER_PATH}"))
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains("abandoned foreign tip"))
        .stdout(predicate::str::contains("kept as backup"));

    assert_eq!(
        common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
        server_savepoint,
        "consent must let the restore proceed to the savepoint"
    );

    let kept = refs_under(&ws.server_dir, BACKUP);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(
        kept[0].1, foreign_tip,
        "the abandoned tip must stay reachable through the backup"
    );
    assert!(
        kept[0].0.contains("/abandoned-"),
        "the backup is marked abandoned: {kept:?}"
    );
    assert_eq!(
        refs_under(&ws.server_dir, PRE_ABORT),
        vec![],
        "abandoning keeps a backup, not a pre-abort ref"
    );

    let listing = rwv()
        .args(["backup", "list", "--json"])
        .current_dir(&ws.root)
        .assert()
        .success()
        .get_output()
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&listing.stdout).unwrap();
    let repos = json["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1, "{json}");
    assert_eq!(repos[0]["path"], SERVER_PATH);
    let backup = &repos[0]["backups"][0];
    assert_eq!(backup["tip"], foreign_tip);
    assert_eq!(backup["mark"], "abandoned");
    assert_eq!(backup["held"][0]["sha"], foreign_tip);

    assert!(
        !ws.root.join(".rwv-op").exists(),
        "op-state must clear once every repo is resolved"
    );
    assert_eq!(
        savepoint_sha(&ws.server_dir, op_id),
        None,
        "the savepoint is consumed by a completed restore"
    );

    let name = backup["name"].as_str().unwrap().to_owned();
    rwv()
        .args(["backup", "drop", SERVER_PATH, &name])
        .current_dir(&ws.root)
        .assert()
        .success()
        .stdout(predicate::str::contains(&name));
    assert_eq!(refs_under(&ws.server_dir, BACKUP), vec![], "dropped");
}

/// A shell completes a path typed from the workspace root as `./<path>/`, and
/// a Windows shell separates it with `\\`. All of those name the repo abort
/// prints, so consent given in any of them restores it.
#[test]
fn abandon_foreign_tip_reads_a_completed_path_in_either_separator() {
    for (n, spelling) in [
        "./github/chatly/server/",
        ".\\github\\chatly\\server\\",
        "github\\chatly\\server",
    ]
    .into_iter()
    .enumerate()
    {
        let tmp = common::tempdir().unwrap();
        let ws = make_fixture(tmp.path(), "primary");
        let op_id = format!("20991231T00003{n}Z");

        let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
        make_commit(&ws.server_dir, "foreign.txt", "foreign\n", "foreign: agent");
        plant_savepoint(&ws.server_dir, &op_id, &server_savepoint);

        let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
        plant_savepoint(&ws.project_dir, &op_id, &project_savepoint);
        plant_owner_record(&ws.root, &op_id, "relock", &[]);

        rwv()
            .arg("abort")
            .arg(format!("--abandon-foreign-tip={spelling}"))
            .current_dir(&ws.root)
            .assert()
            .success();

        assert_eq!(
            common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
            server_savepoint,
            "`{spelling}` must name the server repo"
        );
    }
}

/// A value that names no repo is refused where the operator typed it, not
/// dropped on the way to a consent that covers nothing.
#[test]
fn abandon_foreign_tip_refuses_a_value_naming_no_repo() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    rwv()
        .arg("abort")
        .arg("--abandon-foreign-tip=./")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("--abandon-foreign-tip"));
}

/// Per-repo means per-repo: consent naming one repo must not reach another
/// whose tip is foreign in exactly the same way. The unnamed repo keeps its
/// tip, abort still fails, and op-state is still retained.
///
/// This is the pin for "no bare all-repos form" at the level that matters —
/// not the absence of a spelling, but the absence of the behavior.
#[test]
fn abandon_consent_does_not_reach_a_repo_it_did_not_name() {
    let tmp = common::tempdir().unwrap();
    let named_path = "github/chatly/server";
    let unnamed_path = "github/chatly2/server";
    let fx = make_multi_repo_fixture(tmp.path(), "ws", &[named_path, unnamed_path]);
    let (named_dir, unnamed_dir) = (&fx.repo_dirs[0], &fx.repo_dirs[1]);

    let op_id = "20991231T000021Z";

    let named_savepoint = common::git_in(named_dir, &["rev-parse", "HEAD"]);
    make_commit(named_dir, "foreign.txt", "foreign\n", "foreign: named repo");
    plant_savepoint(named_dir, op_id, &named_savepoint);

    let unnamed_savepoint = common::git_in(unnamed_dir, &["rev-parse", "HEAD"]);
    let unnamed_foreign = make_commit(
        unnamed_dir,
        "foreign.txt",
        "foreign\n",
        "foreign: unnamed repo",
    );
    plant_savepoint(unnamed_dir, op_id, &unnamed_savepoint);

    let project_savepoint = common::git_in(&fx.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&fx.project_dir, op_id, &project_savepoint);
    plant_owner_record(&fx.root, op_id, "relock", &[]);

    let output = rwv()
        .arg("abort")
        .arg(format!("--abandon-foreign-tip={named_path}"))
        .current_dir(&fx.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        common::git_in(named_dir, &["rev-parse", "HEAD"]),
        named_savepoint,
        "the named repo must be restored"
    );
    assert_eq!(
        common::git_in(unnamed_dir, &["rev-parse", "HEAD"]),
        unnamed_foreign,
        "consent for one repo must not move another repo's branch"
    );
    assert!(
        stderr.contains(unnamed_path) && stderr.contains("foreign-tip"),
        "the unnamed repo must still be refused.\nstderr:\n{stderr}"
    );
    assert!(
        fx.root.join(".rwv-op").exists(),
        "op-state must be retained while any repo is still refused"
    );
}

/// The flag cannot be spelled as a blanket. Its bare form is a parse error
/// (clap requires the value), and the two obvious all-repos spellings do not
/// exist.
#[test]
fn abandon_foreign_tip_has_no_all_repos_spelling() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    // Bare: no repo named, so nothing is consented to.
    rwv()
        .arg("abort")
        .arg("--abandon-foreign-tip")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("--abandon-foreign-tip"));

    for blanket in ["--abandon-foreign-tips", "--abandon-all-foreign-tips"] {
        rwv()
            .arg("abort")
            .arg(blanket)
            .current_dir(&ws.root)
            .assert()
            .failure()
            .stderr(predicate::str::contains("unexpected argument"));
    }
}

/// Consent applies to the tip as it stands when abort runs. A refusal leaves
/// the foreign agent free to keep going — a plain commit on top, or a reset
/// and a different line of work — and either way a rerun with consent keeps
/// the tip it finds, the whole of it, as a backup.
#[test]
fn abandon_keeps_the_tip_as_it_stands_however_the_foreign_agent_moved_it() {
    for (n, diverge) in [false, true].into_iter().enumerate() {
        let tmp = common::tempdir().unwrap();
        let ws = make_fixture(tmp.path(), "primary");
        let op_id = format!("20991231T00004{n}Z");

        let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
        let first = make_commit(&ws.server_dir, "f1.txt", "f1\n", "foreign: first");
        plant_savepoint(&ws.server_dir, &op_id, &server_savepoint);
        let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
        plant_savepoint(&ws.project_dir, &op_id, &project_savepoint);
        plant_owner_record(&ws.root, &op_id, "relock", &[]);

        rwv()
            .arg("abort")
            .current_dir(&ws.root)
            .assert()
            .failure()
            .stderr(predicate::str::contains("foreign-tip"));
        assert_writes_no_ref(&ws.server_dir);

        if diverge {
            common::git_in(&ws.server_dir, &["reset", "--hard", &server_savepoint]);
        }
        let latest = make_commit(&ws.server_dir, "f2.txt", "f2\n", "foreign: second");
        assert_eq!(
            try_git(
                &["merge-base", "--is-ancestor", &first, &latest],
                &ws.server_dir
            ),
            !diverge,
            "fixture: the first foreign tip is an ancestor of the latest only when not diverged"
        );

        rwv()
            .arg("abort")
            .arg(format!("--abandon-foreign-tip={SERVER_PATH}"))
            .current_dir(&ws.root)
            .assert()
            .success()
            .stdout(predicate::str::contains("abandoned foreign tip"));

        assert_eq!(
            common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]),
            server_savepoint
        );
        let kept = refs_under(&ws.server_dir, BACKUP);
        assert_eq!(kept.len(), 1, "diverge={diverge}: {kept:?}");
        assert_eq!(kept[0].1, latest, "diverge={diverge}");
        if !diverge {
            assert!(
                try_git(
                    &["merge-base", "--is-ancestor", &first, &kept[0].0],
                    &ws.server_dir
                ),
                "the earlier foreign commit is reachable through the backup"
            );
        }
    }
}

/// The project repo is named by its directory, `projects/<name>`, in the
/// flag and in abort's own per-repo output. The record spelling `(project)` is
/// no operand, and another project's repo is refused by name.
#[test]
fn abandon_foreign_tip_names_the_project_repo_by_its_directory() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000023Z";

    let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    let project_foreign = make_commit(&ws.project_dir, "foreign.txt", "foreign\n", "foreign: proj");
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "relock", &[]);

    let refused = rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("projects/web-app: foreign-tip violation"),
        "abort names the project repo as the flag takes it:\n{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    rwv()
        .arg("abort")
        .arg("--abandon-foreign-tip=(project)")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("--abandon-foreign-tip"))
        .stderr(predicate::str::contains("projects/<name>"));

    let other = rwv()
        .arg("abort")
        .arg("--abandon-foreign-tip=projects/other")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("project `other`"))
        .stderr(predicate::str::contains("projects/web-app"))
        .get_output()
        .clone();
    let refusal = String::from_utf8_lossy(&other.stderr);
    assert_routes_to(&refusal, "inapplicable-flag");
    assert_eq!(
        common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]),
        project_foreign,
        "the refusals moved nothing"
    );

    rwv()
        .arg("abort")
        .arg("--abandon-foreign-tip=projects/web-app")
        .current_dir(&ws.root)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]),
        project_savepoint,
        "`projects/web-app` must name the project repo"
    );
    let kept = refs_under(&ws.project_dir, BACKUP);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(
        kept[0].1, project_foreign,
        "the abandoned project-repo tip must stay reachable"
    );
}

/// The gap this flag closes: the refusal used to hand the operator a raw
/// `git update-ref` because no verb did the job. It must now name the verb,
/// and must not teach the raw command as the way through.
#[test]
fn foreign_tip_recovery_options_name_the_flag_not_raw_git() {
    let tmp = common::tempdir().unwrap();
    let ws = make_fixture(tmp.path(), "primary");

    let op_id = "20991231T000024Z";

    let server_savepoint = common::git_in(&ws.server_dir, &["rev-parse", "HEAD"]);
    make_commit(&ws.server_dir, "foreign.txt", "foreign\n", "foreign: agent");
    plant_savepoint(&ws.server_dir, op_id, &server_savepoint);

    let project_savepoint = common::git_in(&ws.project_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ws.project_dir, op_id, &project_savepoint);
    plant_owner_record(&ws.root, op_id, "relock", &[]);

    let output = rwv()
        .arg("abort")
        .current_dir(&ws.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains(&format!("\trwv abort --abandon-foreign-tip {SERVER_PATH}\n")),
        "the recovery options must name the verb that does this, for the repo refused.\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("git update-ref"),
        "the raw command must no longer be the documented path.\nstderr:\n{stderr}"
    );
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
