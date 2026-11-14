//! Which checkouts replay adds and removes. Both are decided against the
//! manifest the project-repo replay produces: a member it names is never
//! pruned, a member it does not name is never materialized, and nothing is
//! pruned until every repo has replayed. Role plays no part.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::common;

use repoweave::diagnosis::State;

const PROJECT: &str = "app";
const LIB: &str = "local/owner/lib";
const EXTRA: &str = "local/owner/extra";

struct Weave {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    extra_url: String,
}

fn project_dir(workspace: &Path) -> PathBuf {
    workspace.join("projects").join(PROJECT)
}

fn upstream_repo(upstream: &Path, name: &str) -> PathBuf {
    let repo = upstream.join("owner").join(name);
    std::fs::create_dir_all(&repo).unwrap();
    common::git_in(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "init\n").unwrap();
    common::git_in(&repo, &["add", "."]);
    common::git_in(&repo, &["commit", "-m", "initial"]);
    repo
}

/// A primary weave whose project names one owned member. A second upstream
/// repo, `extra`, is there for a seat to add.
fn weave() -> Weave {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    let upstream = tmp.path().join("upstream");
    let lib = upstream_repo(&upstream, "lib");
    let extra = upstream_repo(&upstream, "extra");
    let lib_clone = common::under(&root, LIB);
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &common::file_url(&lib),
            &lib_clone.to_string_lossy(),
        ],
    );

    let project = project_dir(&root);
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    common::record_trunk(&project);
    common::set_up_lock_driver(&project);
    std::fs::write(project.join(".gitignore"), "*.code-workspace\ngita/\n").unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nbranch = \"main\"\nrole = \"owned\"\n",
            common::file_url(&lib)
        ),
    )
    .unwrap();
    std::fs::write(root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    common::pin_every_member(&root);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);

    Weave {
        _tmp: tmp,
        root,
        extra_url: common::file_url(&extra),
    }
}

fn run(dir: &Path, args: &[&str]) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn succeed(dir: &Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "rwv {args:?} in {} failed:\nstdout:\n{}\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `rwv workweave create` run from `from`, which forks the seat from it.
fn seat(weave: &Weave, from: &Path, name: &str) -> PathBuf {
    succeed(from, &["workweave", "create", "--project", PROJECT, name]);
    weave
        .root
        .parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{PROJECT}--{name}"))
}

/// `rwv member add`, `rwv lock`, then commit both files in the project repo.
fn add_and_commit(workspace: &Path, url: &str, role: &str) {
    succeed(workspace, &["member", "add", url, "--role", role]);
    succeed(workspace, &["lock"]);
    let project = project_dir(workspace);
    common::git_in(&project, &["add", "rwv.toml", "rwv.lock"]);
    common::commit_staged(&project, "add extra");
}

fn names_extra(workspace: &Path) -> bool {
    let project = project_dir(workspace);
    ["rwv.toml", "rwv.lock"].iter().all(|file| {
        std::fs::read_to_string(project.join(file))
            .unwrap()
            .contains(EXTRA)
    })
}

fn assert_no_prune_line(stdout: &str) {
    assert!(
        !stdout.contains(&format!("{EXTRA}: pruned")),
        "replay pruned a member this side added:\n{stdout}"
    );
}

fn assert_not_pruned(stdout: &str, workspace: &Path) {
    assert_no_prune_line(stdout);
    assert!(
        common::under(workspace, EXTRA).is_dir(),
        "{EXTRA} must still be checked out in {}",
        workspace.display()
    );
}

#[test]
fn a_seat_that_adds_a_reference_member_lands_and_retires_with_it() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    add_and_commit(&s1, &weave.extra_url, "reference");

    let out = run(&s1, &["land", "--retire"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_no_prune_line(&stdout);
    assert!(
        stdout.contains(&format!(
            "{EXTRA}: kept (not in the source's lock; the replayed manifest names it)"
        )),
        "the replay must say it kept the member and why:\n{stdout}"
    );
    assert!(
        out.status.success(),
        "land --retire must land and retire:\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!s1.exists(), "--retire must delete the seat");
    assert!(names_extra(&weave.root), "the target must name the member");
    assert!(
        common::under(&weave.root, EXTRA).is_dir(),
        "the target must keep its checkout of the member"
    );
}

/// The parent seat adds a member; a child forked before the add lands into
/// it, and then the parent lands into primary. Neither landing may take the
/// parent's checkout.
#[test]
fn a_parent_seat_keeps_a_member_it_added_through_its_childs_landing_and_its_own() {
    let weave = weave();
    let home = seat(&weave, &weave.root, "home");
    let child = seat(&weave, &home, "child");
    add_and_commit(&home, &weave.extra_url, "owned");

    let lib = common::under(&child, LIB);
    std::fs::write(lib.join("work.txt"), "work\n").unwrap();
    common::git_in(&lib, &["add", "work.txt"]);
    common::git_in(&lib, &["commit", "-m", "work"]);
    succeed(&child, &["lock", "--commit"]);

    let child_landing = succeed(&child, &["land", "--history", "linear", "--retire"]);
    assert_not_pruned(&child_landing, &home);

    let home_landing = succeed(&home, &["land", "--history", "linear"]);
    assert_not_pruned(&home_landing, &home);
    assert!(
        names_extra(&home),
        "the parent seat must still name the member"
    );
    assert!(names_extra(&weave.root), "primary must name the member");
}

#[test]
fn a_pull_from_a_seat_forked_before_the_add_keeps_the_added_member() {
    let weave = weave();
    let home = seat(&weave, &weave.root, "home");
    let child = seat(&weave, &home, "child");
    add_and_commit(&home, &weave.extra_url, "owned");
    commit_note(&project_dir(&child), "child: note");

    let stdout = succeed(&child, &["land", "--history", "linear"]);
    assert_not_pruned(&stdout, &home);
    assert!(
        names_extra(&home),
        "the seat landed into must still name the member"
    );
}

#[test]
fn a_member_the_source_dropped_is_pruned() {
    let weave = weave();
    add_and_commit(&weave.root, &weave.extra_url, "owned");
    let s1 = seat(&weave, &weave.root, "s1");
    assert!(
        common::under(&s1, EXTRA).is_dir(),
        "the seat starts with the member"
    );

    succeed(&weave.root, &["member", "remove", EXTRA]);
    succeed(&weave.root, &["lock"]);

    let stdout = succeed(&s1, &["land"]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "a member the source dropped must be pruned:\n{stdout}"
    );
    assert!(
        !common::under(&s1, EXTRA).exists(),
        "the seat's checkout of the dropped member must be gone"
    );
}

/// Replaying CWD's whole history onto a source it shares no commit with keeps
/// CWD's member in the replayed manifest, so the checkout stays.
#[test]
fn a_seat_with_no_history_in_common_with_its_target_keeps_its_members() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    let project = project_dir(&s1);
    let root = common::git_in(
        &project,
        &["commit-tree", "HEAD^{tree}", "-m", "unrelated root"],
    );
    common::git_in(&project, &["reset", "--soft", &root]);
    add_and_commit(&s1, &weave.extra_url, "owned");

    let stdout = succeed(&s1, &["land", "--history", "linear"]);
    assert_not_pruned(&stdout, &s1);
    assert!(
        stdout.contains(&format!(
            "{EXTRA}: kept (not in the source's lock; the replayed manifest names it)"
        )),
        "the replay must say it kept the member and why:\n{stdout}"
    );
    assert!(names_extra(&weave.root), "the target must name the member");
}

/// `rwv member remove` (checkout kept), `rwv lock`, then commit both files.
fn drop_and_commit(workspace: &Path, _message: &str) {
    succeed(workspace, &["member", "remove", EXTRA]);
    succeed(workspace, &["lock"]);
}

fn commit_note(repo: &Path, text: &str) {
    std::fs::write(repo.join("notes.txt"), text).unwrap();
    common::git_in(repo, &["add", "notes.txt"]);
    common::git_in(repo, &["commit", "-m", "note"]);
}

fn head(repo: &Path) -> String {
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// The target drops the member; the seat drops it and adds it back. The
/// replayed manifest names it, so the seat keeps its own checkout and the
/// landed lock pins the seat's commit — never the primary clone's, which has
/// moved on since.
#[test]
fn a_member_dropped_and_re_added_here_keeps_its_checkout_through_a_landing() {
    let weave = weave();
    add_and_commit(&weave.root, &weave.extra_url, "owned");
    let target = seat(&weave, &weave.root, "target");
    let s1 = seat(&weave, &target, "s1");
    let own = head(&common::under(&s1, EXTRA));

    commit_note(&project_dir(&s1), "the seat's own history\n");
    drop_and_commit(&s1, "seat drops extra");
    succeed(&s1, &["member", "add", &weave.extra_url, "--role", "owned"]);

    drop_and_commit(&target, "target drops extra");

    let canonical = common::under(&weave.root, EXTRA);
    std::fs::write(canonical.join("later.txt"), "later\n").unwrap();
    common::git_in(&canonical, &["add", "later.txt"]);
    common::git_in(&canonical, &["commit", "-m", "primary moves on"]);
    let primary_tip = head(&canonical);

    let stdout = succeed(&s1, &["land", "--history", "linear"]);
    assert_not_pruned(&stdout, &s1);
    assert_eq!(head(&common::under(&s1, EXTRA)), own);
    for workspace in [&s1, &target] {
        assert!(
            names_extra(workspace),
            "{} must name the member:\n{stdout}",
            workspace.display()
        );
        let lock = std::fs::read_to_string(project_dir(workspace).join("rwv.lock")).unwrap();
        assert!(
            lock.contains(&own) && !lock.contains(&primary_tip),
            "the lock in {} must pin the seat's own commit {own}, not the primary \
             clone's {primary_tip}:\n{lock}",
            workspace.display()
        );
    }
}

/// The source drops the member, and the repo `conflicted` picks out holds a
/// commit on each side that conflicts, so the replay parks on that repo.
fn parked_pull_of_a_drop(weave: &Weave, conflicted: fn(&Path) -> PathBuf) -> PathBuf {
    add_and_commit(&weave.root, &weave.extra_url, "owned");
    let s1 = seat(weave, &weave.root, "s1");
    commit_note(&conflicted(&s1), "seat\n");
    commit_note(&conflicted(&weave.root), "primary\n");
    succeed(&weave.root, &["lock", "--commit"]);
    drop_and_commit(&weave.root, "drop extra");

    let out = run(&s1, &["land", "--history", "linear"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "the conflicting note must park the replay:\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_not_pruned(&stdout, &s1);
    s1
}

fn lib_dir(workspace: &Path) -> PathBuf {
    common::under(workspace, LIB)
}

#[test]
fn a_replay_parked_on_the_project_repo_prunes_nothing_and_abort_keeps_the_checkout() {
    let weave = weave();
    let s1 = parked_pull_of_a_drop(&weave, project_dir);

    succeed(&s1, &["abort"]);
    assert!(
        common::under(&s1, EXTRA).is_dir(),
        "abort must leave the checkout the parked op found"
    );
    assert!(
        names_extra(&s1),
        "abort must leave the seat naming the member"
    );
}

#[test]
fn a_replay_parked_on_a_member_repo_prunes_nothing_and_abort_keeps_the_checkout() {
    let weave = weave();
    let s1 = parked_pull_of_a_drop(&weave, lib_dir);

    succeed(&s1, &["abort"]);
    assert!(
        common::under(&s1, EXTRA).is_dir(),
        "abort must leave the checkout the parked op found"
    );
    assert!(
        names_extra(&s1),
        "abort must leave the seat naming the member"
    );
}

#[test]
fn a_prune_deferred_by_a_parked_replay_runs_on_continue() {
    let weave = weave();
    let s1 = parked_pull_of_a_drop(&weave, project_dir);

    let project = project_dir(&s1);
    std::fs::write(project.join("notes.txt"), "resolved\n").unwrap();
    common::git_in(&project, &["add", "notes.txt"]);
    let stdout = succeed(&s1, &["land", "--continue"]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "the resumed replay must prune the member the source dropped:\n{stdout}"
    );
    assert!(!common::under(&s1, EXTRA).exists());
}

/// The seat's own history drops the member (a pull that pruned it); a later
/// pull from a source still naming it must not bring the checkout back.
#[test]
fn a_member_this_side_dropped_is_not_materialized_from_a_source_that_names_it() {
    let weave = weave();
    add_and_commit(&weave.root, &weave.extra_url, "owned");
    let s1 = seat(&weave, &weave.root, "s1");
    let s2 = seat(&weave, &weave.root, "s2");
    drop_and_commit(&s2, "drop extra");

    let stdout = succeed(&s1, &["land", "--target", "app--s2", "--history", "linear"]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "{stdout}"
    );

    let out = run(&s1, &["land"]);
    let stderr = common::cli::refused(&out, State::TargetMemberChanges);
    assert!(stderr.contains("removed"), "{stderr}");
    assert!(!common::under(&s1, EXTRA).exists());
    assert!(!names_extra(&s1));
}

/// The `members` array of a `--json` envelope, after asserting the op
/// succeeded.
fn members_of(dir: &Path, args: &[&str]) -> Vec<serde_json::Value> {
    let stdout = succeed(dir, args);
    let envelope: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("rwv {args:?} printed no envelope ({e}):\n{stdout}"));
    envelope["members"]
        .as_array()
        .unwrap_or_else(|| panic!("the envelope must carry `members`:\n{stdout}"))
        .clone()
}

fn assert_one_member(members: &[serde_json::Value], kind: &str, workspace: &Path) {
    assert_eq!(members.len(), 1, "{members:#?}");
    assert_eq!(members[0]["kind"], kind, "{members:#?}");
    assert_eq!(members[0]["path"], EXTRA, "{members:#?}");
    assert!(
        members[0]["absolute_path"]
            .as_str()
            .is_some_and(|p| Path::new(p) == common::under(workspace, EXTRA)),
        "{members:#?}"
    );
}

#[test]
fn json_records_a_member_kept_because_this_side_added_it() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    add_and_commit(&s1, &weave.extra_url, "owned");

    let members = members_of(&s1, &["land", "--json"]);
    assert_one_member(&members, "kept", &s1);
}

#[test]
fn json_records_a_member_kept_across_histories_with_no_commit_in_common() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    let project = project_dir(&s1);
    let root = common::git_in(
        &project,
        &["commit-tree", "HEAD^{tree}", "-m", "unrelated root"],
    );
    common::git_in(&project, &["reset", "--soft", &root]);
    add_and_commit(&s1, &weave.extra_url, "owned");

    let members = members_of(&s1, &["land", "--history", "linear", "--json"]);
    assert_one_member(&members, "kept", &s1);
}

/// A seat holding the member, after the primary dropped it.
fn seat_behind_a_drop(weave: &Weave) -> PathBuf {
    add_and_commit(&weave.root, &weave.extra_url, "owned");
    let s1 = seat(weave, &weave.root, "s1");
    drop_and_commit(&weave.root, "drop extra");
    s1
}

#[test]
fn json_records_a_pruned_member() {
    let weave = weave();
    let s1 = seat_behind_a_drop(&weave);

    let members = members_of(&s1, &["land", "--json"]);
    assert_one_member(&members, "pruned", &s1);
    assert!(!common::under(&s1, EXTRA).exists());
}

const SEAT_BRANCH: &str = "app--s1";

fn branch_exists(store: &Path, branch: &str) -> bool {
    !common::git_in(store, &["branch", "--list", branch]).is_empty()
}

fn registered_worktrees(store: &Path) -> usize {
    common::git_in(store, &["worktree", "list", "--porcelain"])
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count()
}

/// After the seat's prune the primary's clone holds nothing of the seat's —
/// no worktree registration, no branch — so `rwv member remove --delete`
/// in the primary deletes it.
fn assert_the_clone_is_released(weave: &Weave) {
    let clone = common::under(&weave.root, EXTRA);
    assert_eq!(
        registered_worktrees(&clone),
        1,
        "only the clone's own checkout may stay registered:\n{}",
        common::git_in(&clone, &["worktree", "list", "--porcelain"])
    );
    assert!(
        !branch_exists(&clone, SEAT_BRANCH),
        "the prune must retire the branch the seat was given in the clone"
    );
    succeed(&weave.root, &["member", "remove", EXTRA, "--delete"]);
    assert!(!clone.exists());
}

#[test]
fn pruning_a_member_the_source_dropped_releases_the_primarys_clone() {
    let weave = weave();
    let s1 = seat_behind_a_drop(&weave);
    assert!(
        branch_exists(&common::under(&weave.root, EXTRA), SEAT_BRANCH),
        "fixture: the seat holds a branch in the clone"
    );

    let stdout = succeed(&s1, &["land"]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "{stdout}"
    );
    assert_the_clone_is_released(&weave);
}

/// The seat's branch holds a commit no baseline contains while its checkout of
/// a member the target dropped sits detached on the clone's tip. `rwv sync`
/// judges every member's alignment before anything moves, so it refuses the
/// detached checkout and prunes nothing: the checkout and the branch with its
/// commit stay where they were. Keeping such a branch when its checkout is
/// released is `member remove --delete`'s to show.
#[test]
fn a_detached_checkout_of_a_dropped_member_refuses_the_sync_and_keeps_its_branch() {
    let weave = weave();
    let s1 = seat_behind_a_drop(&weave);
    let checkout = common::under(&s1, EXTRA);
    commit_note(&checkout, "only on the seat's branch\n");
    let kept = head(&checkout);
    common::git_in(&checkout, &["checkout", "-q", "--detach", "HEAD~1"]);
    let project_before = head(&s1.join("projects/app"));

    let out = run(&s1, &["sync"]);
    let stderr = common::cli::refused(
        &out,
        repoweave::diagnosis::State::AlignmentDetachedElsewhere,
    );

    assert!(stderr.contains(EXTRA), "{stderr}");
    assert!(checkout.exists(), "{stderr}");
    assert_eq!(head(&s1.join("projects/app")), project_before);
    let clone = common::under(&weave.root, EXTRA);
    assert_eq!(common::git_in(&clone, &["rev-parse", SEAT_BRANCH]), kept);
}

/// A seat behind a drop whose checkout of the dropped member holds untracked
/// work, so replay's prune of it is refused.
fn seat_with_an_unprunable_drop(weave: &Weave) -> PathBuf {
    let s1 = seat_behind_a_drop(weave);
    std::fs::write(common::under(&s1, EXTRA).join("scratch.txt"), "untracked\n").unwrap();
    s1
}

const COMPLETED: &str =
    "completed: replay and relock finished and the op record is cleared, so `rwv abort` does not apply";

/// The op ran to completion and only the prune was refused: exit 1, a message
/// that says abort does not apply and names the checkout, the checkout and
/// its work still on disk, and no op-state left for abort to find.
fn assert_completed_with_the_checkout_left(out: &Output, verb: &str, s1: &Path) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    for expected in [
        format!("{verb} {COMPLETED}"),
        format!(":\n  {EXTRA}\n"),
        format!("Re-running {verb} will not prune it."),
        format!("\n  rwv member remove {EXTRA} --delete"),
    ] {
        assert!(
            stderr.contains(&expected),
            "expected {expected:?}:\n{stderr}"
        );
    }
    assert!(common::under(s1, EXTRA).join("scratch.txt").is_file());
    assert!(!s1.join(".rwv-op").exists(), "{stderr}");
    let abort = run(s1, &["abort"]);
    assert!(
        String::from_utf8_lossy(&abort.stderr).contains("no operation in progress"),
        "abort must agree that it does not apply"
    );
}

#[test]
fn a_refused_prune_fails_sync_to_once_it_completes() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["land"]);
    assert_completed_with_the_checkout_left(&out, "land", &s1);
    assert!(!weave.root.join(".rwv-op-lease").exists());
    assert!(!names_extra(&s1) && !names_extra(&weave.root));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!(
            "{EXTRA}: prune skipped: {EXTRA}: edits dirty\n    scratch.txt\n{EXTRA}: dropped \
             from lock; once nothing above refuses it, remove the checkout with `rwv member \
             remove {EXTRA} --delete`\n"
        )) && !stderr.contains("re-run sync"),
        "a re-run cannot prune what the relock no longer names, so the remedy must not \
         offer one:\n{stderr}"
    );
}

/// A sync that takes in a drop it cannot prune says which checkout it kept
/// and why, under either history, before it fails on it.
#[test]
fn a_sync_that_cannot_prune_a_dropped_checkout_says_so_and_why() {
    for history in ["linear", "merge"] {
        let weave = weave();
        let s1 = seat_with_an_unprunable_drop(&weave);
        commit_note(&project_dir(&s1), "the seat's own project commit\n");

        let out = run(&s1, &["sync", "--history", history]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{history}:\n{stderr}");
        assert!(
            stderr.contains(&format!(
                "rwv sync: {EXTRA}: prune skipped: {EXTRA}: edits dirty\n    scratch.txt\n"
            )),
            "{history}:\n{stderr}"
        );
        assert!(common::under(&s1, EXTRA).join("scratch.txt").is_file());
    }
}

/// Replay refuses the prune, then the op `args` starts reaches a relock commit
/// the project repo's hook refuses: the seat declares the member on a branch
/// it holds no work of its own on, so the landing moves the member there and
/// relocks. Returns the seat, the hook, and the run.
fn park_in_relock_after_a_refused_prune(
    weave: &Weave,
    args: &[&str],
) -> (PathBuf, PathBuf, Output) {
    let s1 = seat_behind_a_drop(weave);
    let lib = lib_dir(&weave.root);
    common::git_in(&lib, &["branch", "dev"]);
    common::git_in(&lib, &["switch", "-q", "dev"]);
    std::fs::write(lib.join("dev.txt"), "work\n").unwrap();
    common::git_in(&lib, &["add", "dev.txt"]);
    common::git_in(&lib, &["commit", "-m", "dev.txt"]);
    common::git_in(&lib, &["switch", "-q", "main"]);
    succeed(&s1, &["member", "set-branch", LIB, "dev"]);
    std::fs::write(common::under(&s1, EXTRA).join("scratch.txt"), "untracked\n").unwrap();
    let hooks = weave.root.parent().unwrap().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("commit-msg");
    common::write_executable(&hook, "#!/bin/sh\nexit 1\n");

    common::git_in(
        project_dir(&weave.root),
        &["config", "core.hooksPath", &hooks.to_string_lossy()],
    );

    let parked = run(&s1, args);
    (s1, hook, parked)
}

fn assert_parked_after_the_refused_prune(parked: &Output, s1: &Path) {
    let stderr = String::from_utf8_lossy(&parked.stderr);
    assert!(
        s1.join(".rwv-op").exists(),
        "the refused relock commit must park the op:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("{EXTRA}: prune skipped")),
        "replay must have refused the prune before the park:\n{stderr}"
    );
}

/// The resume re-enters at relock, past replay, and still finds the checkout
/// replay left.
#[test]
fn a_resume_past_replay_still_fails_on_the_checkout_replay_left() {
    let weave = weave();
    let (s1, hook, parked) = park_in_relock_after_a_refused_prune(
        &weave,
        &["land", "--history", "linear", "--allow-member-change", LIB],
    );
    assert!(!parked.status.success(), "the hook must have refused");
    assert_parked_after_the_refused_prune(&parked, &s1);

    std::fs::remove_file(&hook).unwrap();
    let out = run(&s1, &["land", "--continue"]);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("prune skipped"),
        "the resume must not have re-run replay"
    );
    assert_completed_with_the_checkout_left(&out, "land", &s1);
}

/// A resumed `land` that re-entered past replay has no outcomes to report,
/// and still prints its envelope once complete.
#[test]
fn json_a_complete_sync_to_resumed_past_replay_prints_its_envelope() {
    let weave = weave();
    let (s1, hook, parked) = park_in_relock_after_a_refused_prune(
        &weave,
        &["land", "--history", "linear", "--allow-member-change", LIB],
    );
    assert!(!parked.status.success(), "the hook must have refused");
    assert_parked_after_the_refused_prune(&parked, &s1);

    std::fs::remove_file(&hook).unwrap();
    let out = run(&s1, &["land", "--continue", "--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&format!("land {COMPLETED}")), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let envelope: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("a complete op prints its envelope ({e}):\n{stdout}"));
    assert_eq!(envelope["complete"], true, "{envelope:#}");
    assert_eq!(envelope["resumed"], "relock", "{envelope:#}");
    assert_eq!(envelope["outcomes"], serde_json::json!([]), "{envelope:#}");
}

/// The route line a refusal ends with, and the only one in `stderr`.
/// Retire refused over the edits in the checkout replay could not prune: they
/// are judged before anything moves, so the workweave, the checkout, the work
/// in it are intact, and no op was started.
fn assert_retire_refused_with_the_work_intact(out: &Output, weave: &Weave, s1: &Path) {
    let stderr = common::cli::refused(out, State::EditsDirty);
    assert_eq!(
        std::fs::read_to_string(common::under(s1, EXTRA).join("scratch.txt")).ok(),
        Some("untracked\n".to_owned()),
        "the uncommitted work must survive the refused retire:\n{stderr}"
    );
    assert!(s1.join("projects").join(PROJECT).join("rwv.toml").is_file());
    assert!(!s1.join(".rwv-op").exists());
    assert!(!weave.root.join(".rwv-op-lease").exists());
}

#[test]
fn a_retire_refuses_over_the_edits_in_a_checkout_replay_could_not_prune() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["land", "--retire"]);
    assert_retire_refused_with_the_work_intact(&out, &weave, &s1);
}

#[test]
fn a_refused_retire_lands_and_retires_once_the_edits_are_gone() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);
    let refused = run(&s1, &["land", "--retire"]);
    assert_retire_refused_with_the_work_intact(&refused, &weave, &s1);

    let checkout = common::under(&s1, EXTRA);
    std::fs::rename(
        checkout.join("scratch.txt"),
        weave.root.parent().unwrap().join("saved.txt"),
    )
    .unwrap();
    common::git_in(
        common::under(&weave.root, EXTRA),
        &["worktree", "remove", &checkout.to_string_lossy()],
    );

    let out = run(&s1, &["land", "--retire"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("retired workweave s1"), "{stderr}");
    assert!(!s1.exists(), "the resolved retire must delete the seat");
    assert!(!weave.root.join(".rwv-op-lease").exists());
}

#[test]
fn json_records_a_refused_prune_with_its_judged_state() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["land", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let envelope: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("a completed op prints its envelope ({e}):\n{stdout}"));
    assert_eq!(envelope["complete"], true, "{envelope:#}");
    let members = envelope["members"].as_array().unwrap().clone();
    assert_one_member(&members, "prune-refused", &s1);
    assert_eq!(
        members[0]["refusal"],
        serde_json::Value::Null,
        "{members:#?}"
    );
    assert_eq!(members[0]["refused"][0]["axis"], "edits", "{members:#?}");
    assert_eq!(members[0]["refused"][0]["state"], "dirty", "{members:#?}");
    assert_eq!(
        members[0]["refused"][0]["listed"][0], "scratch.txt",
        "{members:#?}"
    );
    assert!(
        members[0]["message"]
            .as_str()
            .is_some_and(|m| m.contains("edits dirty")
                && m.contains(&format!("rwv member remove {EXTRA} --delete"))),
        "{members:#?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(&format!("land {COMPLETED}")),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(common::under(&s1, EXTRA).is_dir());
    assert!(!s1.join(".rwv-op").exists());
}

#[test]
fn json_a_sync_to_whose_prune_was_refused_is_complete_and_exits_1() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["land", "--json"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["complete"], true, "{envelope:#}");
    assert_one_member(
        envelope["members"].as_array().unwrap(),
        "prune-refused",
        &s1,
    );
}

/// A parked op prints its envelope too, and says it is not complete.
#[test]
fn json_a_parked_sync_is_not_complete() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    commit_note(&lib_dir(&s1), "seat\n");
    commit_note(&lib_dir(&weave.root), "primary\n");
    succeed(&weave.root, &["lock", "--commit"]);

    let out = run(&s1, &["land", "--history", "linear", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(s1.join(".rwv-op").exists(), "the conflict must park the op");
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "a parked op prints its envelope ({e}):\n{}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(envelope["complete"], false, "{envelope:#}");
}

#[test]
fn json_a_clean_sync_is_complete() {
    let weave = weave();
    let s1 = seat_behind_a_drop(&weave);

    let stdout = succeed(&s1, &["land", "--json"]);
    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(envelope["complete"], true, "{envelope:#}");
}

#[test]
fn json_under_two_workers_a_refused_prune_is_a_member_and_exits_1() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["land", "--json", "-j", "2"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["complete"], true, "{envelope:#}");
    assert_one_member(
        envelope["members"].as_array().unwrap(),
        "prune-refused",
        &s1,
    );
}

#[test]
fn json_under_two_workers_carries_the_pruned_member() {
    let weave = weave();
    let s1 = seat_behind_a_drop(&weave);

    let stdout = succeed(&s1, &["land", "--json", "-j", "2"]);
    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_one_member(envelope["members"].as_array().unwrap(), "pruned", &s1);
}

/// A landing that drops a member prunes the target's checkout of it; `rwv
/// abort` after that leaves the checkout pruned and says so, with the verb that
/// makes it again once the restored target lists it.
#[test]
fn abort_after_a_landing_pruned_a_dropped_member_in_the_target_names_the_checkout() {
    let weave = weave();
    add_and_commit(&weave.root, &weave.extra_url, "dependency");
    let s1 = seat(&weave, &weave.root, "s1");
    succeed(&s1, &["member", "remove", EXTRA, "--delete"]);
    succeed(&s1, &["lock"]);
    let project = project_dir(&s1);
    common::git_in(&project, &["add", "rwv.toml", "rwv.lock"]);
    common::commit_staged(&project, "drop extra");

    let mut land = common::rwv_std();
    land.args(["land", "--retire", "--allow-member-change", EXTRA])
        .current_dir(&s1);
    let held = common::pause::Paused::spawn(land, "landed", &weave.root.join("..").join("pause"))
        .expect("the landing is held before it releases its target");
    if common::under(&weave.root, EXTRA).exists() {
        let out = held.release();
        panic!(
            "the landing must have pruned the target's checkout of {EXTRA}:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    drop(held);

    let out = run(&s1, &["abort"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains(&format!(
            "{EXTRA}: its checkout here was pruned as the landing dropped it"
        )) && stdout.contains("`rwv update`"),
        "{stdout}"
    );
    assert!(names_extra(&weave.root));
    succeed(&weave.root, &["update"]);
    assert!(common::under(&weave.root, EXTRA).is_dir());
}

/// A ref at the savepoint namespace's own name in `lib`'s store, below which
/// git files no ref: the landing cannot file its savepoints, so it stops
/// before anything moves, drops the ones it filed, and leaves no op in either
/// workspace. With the ref gone it lands.
#[test]
fn a_landing_that_cannot_file_a_savepoint_does_not_start() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    let lib = common::under(&s1, LIB);
    std::fs::write(lib.join("feature.txt"), "feature\n").unwrap();
    common::git_in(&lib, &["add", "feature.txt"]);
    common::commit_staged(&lib, "feature");
    let primary_lib = common::under(&weave.root, LIB);
    let before = common::git_in(&primary_lib, &["rev-parse", "main"]);
    let primary_project = common::git_in(project_dir(&weave.root), &["rev-parse", "HEAD"]);
    common::git_in(&primary_lib, &["update-ref", "refs/rwv/pre-op", "HEAD"]);

    let out = run(&s1, &["land", "--history", "linear"]);
    let stderr = common::cli::refused(&out, repoweave::refusal::RefusalKind::SavepointUnfiled);
    assert!(
        stderr.contains("could not file its savepoints, so it did not start"),
        "{stderr}"
    );
    assert_eq!(common::git_in(&primary_lib, &["rev-parse", "main"]), before);
    assert_eq!(
        common::git_in(project_dir(&weave.root), &["rev-parse", "HEAD"]),
        primary_project
    );
    for dir in [project_dir(&s1), project_dir(&weave.root)] {
        assert_eq!(
            common::git_in(&dir, &["for-each-ref", "refs/rwv/pre-op/"]),
            "",
            "{}",
            dir.display()
        );
    }
    for workspace in [&s1, &weave.root] {
        assert!(!workspace.join(".rwv-op").exists());
        assert!(!workspace.join(".rwv-op-lease").exists());
    }

    common::git_in(&primary_lib, &["update-ref", "-d", "refs/rwv/pre-op"]);
    succeed(&s1, &["land", "--history", "linear"]);
    assert!(common::git_in(&primary_lib, &["log", "--format=%s", "main"]).contains("feature"));
}
