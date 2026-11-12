//! `rwv workweave delete` weighs
//! every checkout it destroys, not only the members the primary weave's
//! manifest names.
//!
//! The workweave directory is removed whole, so a checkout under it that no
//! member covers goes with it: a member replay dropped but could not prune, or
//! one the workweave added that the primary has not taken. Its uncommitted
//! work is waived by `--discard-uncommitted` and its unmerged commits by
//! `--discard-unlanded`, exactly as a member's are. Delete refuses
//! when one hosts another tree's worktree, unregisters it from the clone it
//! links into, and retires the seat's branch there, as it does for a member;
//! `sync-to --retire` weighs it the same way before deleting the seat.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const PROJECT: &str = "app";
const LIB: &str = "local/owner/lib";
const EXTRA: &str = "local/owner/extra";
const ADDED: &str = "local/owner/added";

struct Weave {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    upstream: PathBuf,
}

impl Weave {
    fn url(&self, name: &str) -> String {
        common::file_url(self.upstream.join(name))
    }

    fn seat(&self, name: &str) -> PathBuf {
        succeed(
            &self.root,
            &["workweave", "create", "--project", PROJECT, name],
        );
        self.root
            .parent()
            .unwrap()
            .join(".workweaves")
            .join(format!("{PROJECT}--{name}"))
    }
}

fn project_dir(workspace: &Path) -> PathBuf {
    workspace.join("projects").join(PROJECT)
}

fn upstream_repo(upstream: &Path, name: &str) {
    let repo = upstream.join(name);
    std::fs::create_dir_all(&repo).unwrap();
    common::git_in(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "init\n").unwrap();
    common::git_in(&repo, &["add", "."]);
    common::git_in(&repo, &["commit", "-m", "initial"]);
}

fn run(dir: &Path, args: &[&str]) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn succeed(dir: &Path, args: &[&str]) {
    let out = run(dir, args);
    assert!(
        out.status.success(),
        "rwv {args:?} in {} failed:\nstdout:\n{}\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit_all(workspace: &Path, message: &str) {
    let project = project_dir(workspace);
    common::git_in(&project, &["add", "rwv.toml", "rwv.lock"]);
    if !common::git_in(&project, &["status", "--porcelain", "--untracked-files=no"]).is_empty() {
        common::commit_staged(&project, message);
    }
}

/// A primary weave whose project names `lib` and `extra`, both owned.
fn weave() -> Weave {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("projects")).unwrap();
    let upstream = tmp.path().join("upstream").join("owner");
    for name in ["lib", "extra", "added"] {
        upstream_repo(&upstream, name);
    }
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &common::file_url(upstream.join("lib")),
            &common::under(&root, LIB).to_string_lossy(),
        ],
    );
    let project = project_dir(&root);
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    common::record_trunk(&project);
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    std::fs::write(project.join(".gitignore"), "*.code-workspace\ngita/\n").unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nbranch = \"main\"\nrole = \"owned\"\n",
            common::file_url(upstream.join("lib"))
        ),
    )
    .unwrap();
    std::fs::write(root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    common::pin_every_member(&root);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);
    let weave = Weave {
        _tmp: tmp,
        root,
        upstream,
    };
    succeed(
        &weave.root,
        &["member", "add", &weave.url("extra"), "--role", "owned"],
    );
    succeed(&weave.root, &["lock"]);
    commit_all(&weave.root, "add extra");
    weave
}

/// A seat whose checkout of `extra` holds untracked work, after the primary
/// dropped `extra` and the seat pulled the drop: replay refused to prune the
/// checkout, so it sits in the seat with no manifest naming it.
fn seat_with_a_stranded_dirty_checkout(weave: &Weave) -> PathBuf {
    let s1 = weave.seat("s1");
    succeed(&weave.root, &["member", "remove", EXTRA]);
    succeed(&weave.root, &["lock"]);
    commit_all(&weave.root, "drop extra");
    std::fs::write(common::under(&s1, EXTRA).join("scratch.txt"), "untracked\n").unwrap();
    let pulled = run(&s1, &["sync-to", "--strategy", "rebase"]);
    assert_eq!(
        pulled.status.code(),
        Some(1),
        "the pull must leave the checkout stranded:\n{}",
        String::from_utf8_lossy(&pulled.stderr)
    );
    s1
}

fn delete(weave: &Weave, flags: &[&str]) -> Output {
    let mut args = vec!["workweave", "delete", "--project", PROJECT, "s1"];
    args.extend_from_slice(flags);
    run(&weave.root, &args)
}

fn assert_refused_with_the_work_intact(out: &Output, s1: &Path, token: &str, listed: &str) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        std::fs::read_to_string(common::under(s1, EXTRA).join("scratch.txt")).ok(),
        Some("untracked\n".to_owned()),
        "the untracked work must survive the refused delete:\n{stderr}"
    );
    assert!(project_dir(s1).join("rwv.toml").is_file(), "{stderr}");
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!("\n\nrwv explain {token}\n")),
        "expected the {token} route:\n{stderr}"
    );
    assert!(stderr.contains(listed), "expected {listed:?}:\n{stderr}");
}

const OUTSIDE: &str = "(not a manifest member)";

#[test]
fn delete_refuses_over_untracked_work_in_a_checkout_no_member_covers() {
    let weave = weave();
    let s1 = seat_with_a_stranded_dirty_checkout(&weave);

    let out = delete(&weave, &[]);
    assert_refused_with_the_work_intact(
        &out,
        &s1,
        "edits",
        &format!("rwv workweave delete: {EXTRA}: edits dirty {OUTSIDE}\n    scratch.txt\n"),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("commit your edits, or set them aside"),
        "the refusal must print the state's choice"
    );
    assert!(
        stderr.contains("    rwv workweave delete s1 --discard-uncommitted\n"),
        "the refusal must offer the delete that waives the loss as a form of the state's choice:\n{stderr}"
    );
    assert!(
        !stderr.contains("deletes the workweave with these uncommitted changes in it"),
        "the waiver is a choice in the table, not a note line beside it:\n{stderr}"
    );
}

#[test]
fn discard_uncommitted_waives_the_unlisted_checkout_too() {
    let weave = weave();
    let s1 = seat_with_a_stranded_dirty_checkout(&weave);

    let out = delete(&weave, &["--discard-uncommitted"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!s1.exists());
}

/// The seat adds a member the primary has never had and commits in it: the
/// branch holding those commits has not landed, so the delete removes the
/// workweave and keeps the branch, and `branch delete --discard-unlanded` is the
/// consent that backs it up and deletes it.
#[test]
fn delete_keeps_the_branch_of_a_member_only_the_workweave_names() {
    let weave = weave();
    let s1 = weave.seat("s1");
    succeed(
        &s1,
        &["member", "add", &weave.url("added"), "--role", "owned"],
    );
    succeed(&s1, &["lock"]);
    commit_all(&s1, "add added");
    let added = common::under(&s1, ADDED);
    std::fs::write(added.join("work.txt"), "work\n").unwrap();
    common::git_in(&added, &["add", "work.txt"]);
    common::git_in(&added, &["commit", "-m", "work"]);
    let work = common::git_in(&added, &["rev-parse", "HEAD"]);

    let out = delete(&weave, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(
        stderr.contains(&format!("left: {ADDED} app--s1: unlanded")),
        "{stderr}"
    );
    assert!(!s1.exists());
    let clone = common::under(&weave.root, ADDED);
    assert_eq!(common::git_in(&clone, &["rev-parse", "app--s1"]), work);

    succeed(
        &weave.root,
        &[
            "branch",
            "delete",
            "app--s1",
            "--project",
            PROJECT,
            "--discard-unlanded",
        ],
    );
    assert!(
        !branch_exists(&clone, "app--s1"),
        "the discard consents to losing the commits the branch holds"
    );
}

/// A clone placed in the workweave by hand, with a commit of its own: it is its
/// own store and no remote has the commit, so no backup written there could
/// keep it, and no waiver consents to losing it.
#[test]
fn delete_refuses_over_a_commit_in_a_clone_no_baseline_holds() {
    let weave = weave();
    let s1 = weave.seat("s1");
    let stray = common::under(&s1, "local/owner/stray");
    common::git_in(
        &s1,
        &["clone", &weave.url("added"), &stray.to_string_lossy()],
    );
    std::fs::write(stray.join("only-here.txt"), "only here\n").unwrap();
    common::git_in(&stray, &["add", "only-here.txt"]);
    common::git_in(&stray, &["commit", "-m", "only here"]);

    for flags in [
        &[][..],
        &["--discard-uncommitted", "--discard-unlanded"][..],
    ] {
        let out = delete(&weave, flags);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stray.join("only-here.txt").is_file(), "{stderr}");
        assert!(!out.status.success(), "{stderr}");
        assert!(
            stderr.contains("\n\nrwv explain standalone-in-workweave\n")
                && stderr.contains(&format!("local/owner/stray {OUTSIDE}: main (1 commit(s))")),
            "{stderr}"
        );
    }
}

/// Control: what is outside the manifest and holds nothing does not block.
#[test]
fn a_clean_unlisted_checkout_does_not_block_delete() {
    let weave = weave();
    let s1 = weave.seat("s1");
    succeed(&weave.root, &["member", "remove", EXTRA]);
    succeed(&weave.root, &["lock"]);
    commit_all(&weave.root, "drop extra");
    assert!(common::under(&s1, EXTRA).is_dir());

    succeed(
        &weave.root,
        &["workweave", "delete", "--project", PROJECT, "s1"],
    );
    assert!(!s1.exists());
}

/// Control: a delete with every checkout a member behaves as before, clean
/// or dirty.
#[test]
fn a_delete_with_only_members_is_unchanged() {
    let weave = weave();
    let s1 = weave.seat("s1");
    std::fs::write(common::under(&s1, LIB).join("scratch.txt"), "untracked\n").unwrap();

    let out = delete(&weave, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "rwv workweave delete: {LIB}: edits dirty\n    scratch.txt\n"
        )) && !stderr.contains(OUTSIDE),
        "{stderr}"
    );

    std::fs::remove_file(common::under(&s1, LIB).join("scratch.txt")).unwrap();
    succeed(
        &weave.root,
        &["workweave", "delete", "--project", PROJECT, "s1"],
    );
    assert!(!s1.exists());
}

fn worktree_list(store: &Path) -> String {
    common::git_in(store, &["worktree", "list", "--porcelain"])
}

fn branch_exists(store: &Path, branch: &str) -> bool {
    !common::git_in(store, &["branch", "--list", branch]).is_empty()
}

fn registers(store: &Path, checkout: &Path) -> bool {
    let checkout = repoweave::path_spelling::resolve(checkout).unwrap_or(checkout.to_path_buf());
    worktree_list(store)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|listed| {
            let listed = Path::new(listed);
            repoweave::path_spelling::resolve(listed).unwrap_or(listed.to_path_buf()) == checkout
        })
}

/// A clone placed in the workweave by hand that other worktrees link into:
/// removing the workweave would orphan them, and no waiver consents to that.
#[test]
fn delete_refuses_when_an_unlisted_checkout_hosts_a_foreign_worktree_even_with_waivers() {
    let weave = weave();
    let s1 = weave.seat("s1");
    let stray = common::under(&s1, "local/owner/stray");
    common::git_in(
        &s1,
        &["clone", &weave.url("added"), &stray.to_string_lossy()],
    );
    let foreign = weave.root.parent().unwrap().join("foreign");
    common::git_in(
        &stray,
        &[
            "worktree",
            "add",
            "-b",
            "foreign",
            &foreign.to_string_lossy(),
        ],
    );

    let out = delete(&weave, &["--discard-uncommitted", "--discard-unlanded"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("\n\nrwv explain standalone-in-workweave\n")
            && stderr.contains(&format!(
                "local/owner/stray {OUTSIDE}: checkout is itself a canonical store with 1 \
                 dependent worktree(s):"
            )),
        "{stderr}"
    );
    assert!(stray.join(".git").is_dir(), "{stderr}");
    common::git_in(&foreign, &["status"]);
}

/// The primary dropped a member the seat still has checked out: delete takes
/// the checkout's registration and the seat's branch out of the clone the
/// primary keeps, as it does for a member.
#[test]
fn delete_unregisters_a_dropped_members_checkout_and_retires_its_branch() {
    let weave = weave();
    let s1 = weave.seat("s1");
    let checkout = common::under(&s1, EXTRA);
    let clone = common::under(&weave.root, EXTRA);
    succeed(&weave.root, &["member", "remove", EXTRA]);
    succeed(&weave.root, &["lock"]);
    commit_all(&weave.root, "drop extra");
    assert!(registers(&clone, &checkout) && branch_exists(&clone, "app--s1"));

    succeed(
        &weave.root,
        &["workweave", "delete", "--project", PROJECT, "s1"],
    );

    assert!(!s1.exists());
    assert!(
        !worktree_list(&clone).contains("app--s1"),
        "{}",
        worktree_list(&clone)
    );
    assert!(!branch_exists(&clone, "app--s1"));
}

/// A member only the seat added has its clone in the primary. Deleting the
/// seat with `--discard-unlanded` deletes the branch, which holds nothing the
/// clone lacks, so the primary can then remove the clone.
#[test]
fn delete_releases_the_primary_clone_of_a_member_only_the_workweave_added() {
    let weave = weave();
    let s1 = weave.seat("s1");
    succeed(
        &s1,
        &["member", "add", &weave.url("added"), "--role", "owned"],
    );
    succeed(&s1, &["lock"]);
    commit_all(&s1, "add added");
    let clone = common::under(&weave.root, ADDED);
    assert!(registers(&clone, &common::under(&s1, ADDED)));

    succeed(
        &weave.root,
        &[
            "workweave",
            "delete",
            "--project",
            PROJECT,
            "s1",
            "--discard-uncommitted",
            "--discard-unlanded",
        ],
    );

    assert!(!s1.exists());
    assert!(
        !worktree_list(&clone).contains("app--s1"),
        "{}",
        worktree_list(&clone)
    );
    assert!(!branch_exists(&clone, "app--s1"));
    succeed(&weave.root, &["member", "remove", ADDED, "--delete"]);
    assert!(!clone.exists());
}

/// `sync-to --retire` weighs a checkout its manifest does not name before it
/// deletes the seat, as it weighs a member.
#[test]
fn retire_refuses_over_untracked_work_in_a_checkout_no_member_covers() {
    let weave = weave();
    let s1 = weave.seat("s1");
    let stray = common::under(&s1, "local/owner/stray");
    common::git_in(
        &s1,
        &["clone", &weave.url("added"), &stray.to_string_lossy()],
    );
    std::fs::write(stray.join("scratch.txt"), "untracked\n").unwrap();

    let out = run(&s1, &["sync-to", "--retire"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        std::fs::read_to_string(stray.join("scratch.txt")).ok(),
        Some("untracked\n".to_owned()),
        "the untracked work must survive the refused retire:\n{stderr}"
    );
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.ends_with("\n\nrwv explain edits\n")
            && stderr.contains(&format!(
                "rwv sync-to: local/owner/stray: edits dirty {OUTSIDE}\n    scratch.txt\n"
            )),
        "{stderr}"
    );
}
