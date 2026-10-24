//! Which checkouts replay adds and removes. Both are decided against the
//! manifest the project-repo replay produces: a member it names is never
//! pruned, a member it does not name is never materialized, and nothing is
//! pruned until every repo has replayed. Role plays no part.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

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
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    std::fs::write(project.join(".gitignore"), "*.code-workspace\ngita/\n").unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::file_url(&lib)
        ),
    )
    .unwrap();
    std::fs::write(root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    succeed(&root, &["lock"]);
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
    succeed(from, &["workweave", PROJECT, "create", name]);
    weave
        .root
        .parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{PROJECT}--{name}"))
}

/// `rwv add`, `rwv lock`, then commit both files in the project repo.
fn add_and_commit(workspace: &Path, url: &str, role: &str) {
    succeed(workspace, &["add", url, "--role", role]);
    succeed(workspace, &["lock"]);
    let project = project_dir(workspace);
    common::git_in(&project, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&project, &["commit", "-m", "add extra"]);
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

    let out = run(&s1, &["sync-to", "--retire"]);
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
        "sync-to --retire must land and retire:\nstdout:\n{stdout}\nstderr:\n{}",
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

    let child_landing = succeed(&child, &["sync-to"]);
    assert_not_pruned(&child_landing, &home);

    let home_landing = succeed(&home, &["sync-to"]);
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

    let child_dir = child.to_string_lossy().into_owned();
    let stdout = succeed(&home, &["sync", &child_dir, "--strategy", "rebase"]);
    assert_not_pruned(&stdout, &home);
    assert!(
        names_extra(&home),
        "the pulling seat must still name the member"
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

    succeed(&weave.root, &["remove", EXTRA]);
    succeed(&weave.root, &["lock"]);
    let project = project_dir(&weave.root);
    common::git_in(&project, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&project, &["commit", "-m", "drop extra"]);

    let stdout = succeed(&s1, &["sync", "primary"]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "a member the source dropped must be pruned:\n{stdout}"
    );
    assert!(
        !common::under(&s1, EXTRA).exists(),
        "the seat's checkout of the dropped member must be gone"
    );
}

#[test]
fn discarding_local_commits_prunes_the_member_they_added() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    add_and_commit(&s1, &weave.extra_url, "owned");

    let stdout = succeed(&s1, &["sync", "primary", "--discard-local-commits"]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "a discarded add leaves the member in no lock, so its checkout goes:\n{stdout}"
    );
    assert!(!common::under(&s1, EXTRA).exists());
    assert!(!names_extra(&s1) && !names_extra(&weave.root));
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

    let stdout = succeed(&s1, &["sync-to"]);
    assert_not_pruned(&stdout, &s1);
    assert!(
        stdout.contains(&format!(
            "{EXTRA}: kept (not in the source's lock; the replayed manifest names it)"
        )),
        "the replay must say it kept the member and why:\n{stdout}"
    );
    assert!(names_extra(&weave.root), "the target must name the member");
}

/// `rwv remove` (checkout kept), `rwv lock`, then commit both files.
fn drop_and_commit(workspace: &Path, message: &str) {
    succeed(workspace, &["remove", EXTRA]);
    succeed(workspace, &["lock"]);
    let project = project_dir(workspace);
    common::git_in(&project, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&project, &["commit", "-m", message]);
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

    let manifest = std::fs::read_to_string(project_dir(&s1).join("rwv.toml")).unwrap();
    drop_and_commit(&s1, "seat drops extra");
    std::fs::write(project_dir(&s1).join("rwv.toml"), manifest).unwrap();
    succeed(&s1, &["lock"]);
    common::git_in(project_dir(&s1), &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(project_dir(&s1), &["commit", "-m", "re-add extra"]);

    drop_and_commit(&target, "target drops extra");

    let canonical = common::under(&weave.root, EXTRA);
    std::fs::write(canonical.join("later.txt"), "later\n").unwrap();
    common::git_in(&canonical, &["add", "later.txt"]);
    common::git_in(&canonical, &["commit", "-m", "primary moves on"]);
    let primary_tip = head(&canonical);

    let stdout = succeed(&s1, &["sync-to", "--strategy", "rebase"]);
    assert_not_pruned(&stdout, &s1);
    assert_eq!(head(&common::under(&s1, EXTRA)), own);
    for workspace in [&s1, &target] {
        assert!(
            names_extra(workspace),
            "{} must name the member",
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
    succeed(&weave.root, &["lock"]);
    drop_and_commit(&weave.root, "drop extra");

    let out = run(&s1, &["sync", "primary", "--strategy", "rebase"]);
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
    let stdout = succeed(&s1, &["sync", "--continue"]);
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

    let s2_dir = s2.to_string_lossy().into_owned();
    let stdout = succeed(&s1, &["sync", &s2_dir]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "{stdout}"
    );

    let stdout = succeed(&s1, &["sync", "primary", "--strategy", "rebase"]);
    assert!(
        !stdout.contains(&format!("{EXTRA}: materialized")),
        "a member the seat dropped must not be re-created:\n{stdout}"
    );
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

    let members = members_of(&s1, &["sync-to", "--json"]);
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

    let members = members_of(&s1, &["sync-to", "--json"]);
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

    let members = members_of(&s1, &["sync", "primary", "--json"]);
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
/// no worktree registration, no branch, no receipt — so `rwv remove --delete`
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
    succeed(&weave.root, &["remove", EXTRA, "--delete"]);
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

    let stdout = succeed(&s1, &["sync", "primary"]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "{stdout}"
    );
    assert_the_clone_is_released(&weave);
}

#[test]
fn pruning_a_member_only_the_seat_added_releases_the_primarys_clone() {
    let weave = weave();
    let s1 = seat(&weave, &weave.root, "s1");
    add_and_commit(&s1, &weave.extra_url, "owned");

    let stdout = succeed(&s1, &["sync", "primary", "--discard-local-commits"]);
    assert!(
        stdout.contains(&format!("{EXTRA}: pruned (dropped from lock)")),
        "{stdout}"
    );
    assert_the_clone_is_released(&weave);
}

/// The seat's branch holds a commit no baseline contains while its checkout
/// sits detached on the clone's tip: the prune removes the checkout, but the
/// branch and its receipt stay, and the warning names the branch.
#[test]
fn pruning_keeps_a_seat_branch_that_holds_commits_nothing_else_has() {
    let weave = weave();
    let s1 = seat_behind_a_drop(&weave);
    let checkout = common::under(&s1, EXTRA);
    commit_note(&checkout, "only on the seat's branch\n");
    let kept = head(&checkout);
    common::git_in(&checkout, &["checkout", "-q", "--detach", "HEAD~1"]);

    let out = run(&s1, &["sync", "primary"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(!checkout.exists(), "{stdout}\n{stderr}");
    let clone = common::under(&weave.root, EXTRA);
    assert_eq!(
        common::git_in(&clone, &["rev-parse", SEAT_BRANCH]),
        kept,
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "  warning: {EXTRA}: branch {SEAT_BRANCH} is rwv's but holds commits no baseline \
             contains; left in place."
        )) && !stderr.contains("is not recorded as rwv's"),
        "{stderr}"
    );
    let out = run(&weave.root, &["remove", EXTRA, "--delete"]);
    assert!(
        !out.status.success()
            && String::from_utf8_lossy(&out.stderr)
                .contains(&format!("ownership receipt for branch {SEAT_BRANCH}")),
        "the receipt still claims the clone:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A seat behind a drop whose checkout of the dropped member holds untracked
/// work, so replay's prune of it is refused.
fn seat_with_an_unprunable_drop(weave: &Weave) -> PathBuf {
    let s1 = seat_behind_a_drop(weave);
    std::fs::write(common::under(&s1, EXTRA).join("scratch.txt"), "untracked\n").unwrap();
    s1
}

const COMPLETED: &str =
    "completed: replay and relock finished and op-state is cleared, so `rwv abort` does not apply";

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
fn a_refused_prune_fails_the_sync_once_it_completes() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["sync", "primary"]);
    assert_completed_with_the_checkout_left(&out, "sync", &s1);
    assert!(!names_extra(&s1), "the relock must have run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!(
            "{EXTRA}: prune skipped: {EXTRA}: dropped from lock but worktree has uncommitted \
             changes; commit/discard, then remove the checkout by hand"
        )) && !stderr.contains("re-run sync"),
        "a re-run cannot prune what the relock no longer names, so the remedy must not \
         offer one:\n{stderr}"
    );
}

#[test]
fn a_refused_prune_fails_sync_to_once_it_completes() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["sync-to"]);
    assert_completed_with_the_checkout_left(&out, "sync-to", &s1);
    assert!(!weave.root.join(".rwv-op-lease").exists());
    assert!(!names_extra(&s1) && !names_extra(&weave.root));
}

/// Replay refuses the prune, then the op `args` starts reaches a relock commit
/// the project repo's hook refuses. Returns the seat, the hook, and the run.
fn park_in_relock_after_a_refused_prune(
    weave: &Weave,
    args: &[&str],
) -> (PathBuf, PathBuf, Output) {
    let s1 = seat_behind_a_drop(weave);
    for (workspace, file) in [(&weave.root, "primary.txt"), (&s1, "seat.txt")] {
        let lib = lib_dir(workspace);
        std::fs::write(lib.join(file), "work\n").unwrap();
        common::git_in(&lib, &["add", file]);
        common::git_in(&lib, &["commit", "-m", file]);
        succeed(workspace, &["lock", "--commit"]);
    }
    std::fs::write(common::under(&s1, EXTRA).join("scratch.txt"), "untracked\n").unwrap();
    let hooks = weave.root.parent().unwrap().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("commit-msg");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
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
    let (s1, hook, parked) =
        park_in_relock_after_a_refused_prune(&weave, &["sync", "primary", "--strategy", "rebase"]);
    assert!(!parked.status.success(), "the hook must have refused");
    assert_parked_after_the_refused_prune(&parked, &s1);

    std::fs::remove_file(&hook).unwrap();
    let out = run(&s1, &["sync", "--continue"]);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("prune skipped"),
        "the resume must not have re-run replay"
    );
    assert_completed_with_the_checkout_left(&out, "sync", &s1);
}

/// A resumed `sync-to` that re-entered past replay has no outcomes to report,
/// and still prints its envelope once complete.
#[test]
fn json_a_complete_sync_to_resumed_past_replay_prints_its_envelope() {
    let weave = weave();
    let (s1, hook, parked) = park_in_relock_after_a_refused_prune(&weave, &["sync-to"]);
    assert!(!parked.status.success(), "the hook must have refused");
    assert_parked_after_the_refused_prune(&parked, &s1);

    std::fs::remove_file(&hook).unwrap();
    let out = run(&s1, &["sync-to", "--continue", "--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&format!("sync-to {COMPLETED}")), "{stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let envelope: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("a complete op prints its envelope ({e}):\n{stdout}"));
    assert_eq!(envelope["complete"], true, "{envelope:#}");
    assert_eq!(envelope["resumed"], "relock", "{envelope:#}");
    assert_eq!(envelope["outcomes"], serde_json::json!([]), "{envelope:#}");
}

/// The route line a refusal ends with, and the only one in `stderr`.
fn assert_routes_to(stderr: &str, token: &str) {
    let routed: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routed, vec![token], "stderr was:\n{stderr}");
    assert!(
        stderr.ends_with(&format!("\n\nrwv explain {token}\n")),
        "the route line must be the last line, stderr was:\n{stderr}"
    );
}

/// Retire refused over the checkout replay could not prune: the workweave,
/// the checkout and the work in it are intact, and the op is parked at retire
/// in both workspaces.
fn assert_retire_refused_with_the_work_intact(out: &Output, weave: &Weave, s1: &Path) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        std::fs::read_to_string(common::under(s1, EXTRA).join("scratch.txt")).ok(),
        Some("untracked\n".to_owned()),
        "the uncommitted work must survive the refused retire:\n{stderr}"
    );
    assert!(s1.join("projects").join(PROJECT).join("rwv.toml").is_file());
    assert!(!out.status.success(), "retire must refuse:\n{stderr}");
    assert_routes_to(&stderr, "retire-unpruned-checkout");
    for expected in [
        "--retire: replay could not prune 1 member checkout the manifest no longer names"
            .to_owned(),
        format!(":\n  {EXTRA}\n"),
        "rwv sync-to --continue".to_owned(),
        "`rwv abort`".to_owned(),
    ] {
        assert!(
            stderr.contains(&expected),
            "expected {expected:?}:\n{stderr}"
        );
    }
    let record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(s1.join(".rwv-op")).unwrap()).unwrap();
    assert_eq!(record["phase"], "retire", "{record:#}");
    assert!(weave.root.join(".rwv-op-lease").exists());
}

#[test]
fn a_retire_refuses_while_a_checkout_replay_could_not_prune_is_on_disk() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["sync-to", "--retire"]);
    assert_retire_refused_with_the_work_intact(&out, &weave, &s1);
    assert!(
        !names_extra(&weave.root),
        "the landing itself completed before retire"
    );
}

#[test]
fn a_continued_retire_refuses_until_the_checkout_is_gone_then_retires() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);
    let parked = run(&s1, &["sync-to", "--retire"]);
    assert!(!parked.status.success());

    let still = run(&s1, &["sync-to", "--continue"]);
    assert_retire_refused_with_the_work_intact(&still, &weave, &s1);

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

    let out = run(&s1, &["sync-to", "--continue"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("retired workweave s1"), "{stderr}");
    assert!(!s1.exists(), "the resolved retire must delete the seat");
    assert!(!weave.root.join(".rwv-op-lease").exists());
}

#[test]
fn json_records_a_refused_prune_with_its_token() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["sync", "primary", "--json"]);
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
    assert_eq!(members[0]["refusal"], "dirty-checkout", "{members:#?}");
    assert!(
        members[0]["message"].as_str().is_some_and(
            |m| m.contains("uncommitted changes") && m.contains("remove the checkout by hand")
        ),
        "{members:#?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains(&format!("sync {COMPLETED}")),
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

    let out = run(&s1, &["sync-to", "--json"]);
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

    let out = run(&s1, &["sync", "primary", "--strategy", "rebase", "--json"]);
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

/// The case `complete` exists for: a `prune-refused` member AND exit 1, from
/// an op parked after replay (here at retire) rather than a complete one.
#[test]
fn json_a_sync_to_parked_at_retire_after_a_refused_prune_is_not_complete() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["sync-to", "--retire", "--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        s1.join(".rwv-op").exists(),
        "retire must park the op:\n{stderr}"
    );
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["complete"], false, "{envelope:#}");
    assert_one_member(
        envelope["members"].as_array().unwrap(),
        "prune-refused",
        &s1,
    );
}

#[test]
fn json_a_clean_sync_is_complete() {
    let weave = weave();
    let s1 = seat_behind_a_drop(&weave);

    let stdout = succeed(&s1, &["sync", "primary", "--json"]);
    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(envelope["complete"], true, "{envelope:#}");
}

#[test]
fn json_under_two_workers_a_refused_prune_is_a_member_and_exits_1() {
    let weave = weave();
    let s1 = seat_with_an_unprunable_drop(&weave);

    let out = run(&s1, &["sync", "primary", "--json", "-j", "2"]);
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

    let stdout = succeed(&s1, &["sync", "primary", "--json", "-j", "2"]);
    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_one_member(envelope["members"].as_array().unwrap(), "pruned", &s1);
}
