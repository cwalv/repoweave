//! `rwv workweave <project> delete` and `create --replace-existing` weigh
//! every checkout they destroy, not only the members the primary weave's
//! manifest names.
//!
//! The workweave directory is removed whole, so a checkout under it that no
//! member covers goes with it: a member replay dropped but could not prune, or
//! one the workweave added that the primary has not taken. Its uncommitted
//! work is waived by `--discard-uncommitted` and its unmerged commits by
//! `--discard-unmerged-commits`, exactly as a member's are.

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
        succeed(&self.root, &["workweave", PROJECT, "create", name]);
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
    common::git_in(&project, &["commit", "-m", message]);
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
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    std::fs::write(project.join(".gitignore"), "*.code-workspace\ngita/\n").unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::file_url(upstream.join("lib"))
        ),
    )
    .unwrap();
    std::fs::write(root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    succeed(&root, &["lock"]);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);
    let weave = Weave {
        _tmp: tmp,
        root,
        upstream,
    };
    succeed(
        &weave.root,
        &["add", &weave.url("extra"), "--role", "owned"],
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
    succeed(&weave.root, &["remove", EXTRA]);
    succeed(&weave.root, &["lock"]);
    commit_all(&weave.root, "drop extra");
    std::fs::write(common::under(&s1, EXTRA).join("scratch.txt"), "untracked\n").unwrap();
    let pulled = run(&s1, &["sync", "primary"]);
    assert_eq!(
        pulled.status.code(),
        Some(1),
        "the pull must leave the checkout stranded:\n{}",
        String::from_utf8_lossy(&pulled.stderr)
    );
    s1
}

fn delete(weave: &Weave, flags: &[&str]) -> Output {
    let mut args = vec!["workweave", PROJECT, "delete", "s1"];
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
        "uncommitted-work",
        &format!("  {EXTRA} {OUTSIDE}"),
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--discard-uncommitted"),
        "the refusal must name the flag that waives the loss"
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

#[test]
fn replace_existing_refuses_over_untracked_work_in_a_checkout_no_member_covers() {
    let weave = weave();
    let s1 = seat_with_a_stranded_dirty_checkout(&weave);

    let out = run(
        &weave.root,
        &["workweave", PROJECT, "create", "s1", "--replace-existing"],
    );
    assert_refused_with_the_work_intact(
        &out,
        &s1,
        "replace-target-holds-work",
        &format!("  {EXTRA} {OUTSIDE}"),
    );
}

/// The seat adds a member the primary has never had and commits in it: those
/// commits are in no baseline, so they count as unmerged.
#[test]
fn delete_refuses_over_commits_in_a_member_only_the_workweave_names() {
    let weave = weave();
    let s1 = weave.seat("s1");
    succeed(&s1, &["add", &weave.url("added"), "--role", "owned"]);
    succeed(&s1, &["lock"]);
    commit_all(&s1, "add added");
    let added = common::under(&s1, ADDED);
    std::fs::write(added.join("work.txt"), "work\n").unwrap();
    common::git_in(&added, &["add", "work.txt"]);
    common::git_in(&added, &["commit", "-m", "work"]);
    let work = common::git_in(&added, &["rev-parse", "HEAD"]);

    let out = delete(&weave, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("\n\nrwv explain unmerged-commits\n")
            && stderr.contains(&format!("  {ADDED} {OUTSIDE}")),
        "{stderr}"
    );
    assert_eq!(common::git_in(&added, &["rev-parse", "HEAD"]), work);

    let out = delete(&weave, &["--discard-unmerged-commits"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!s1.exists());
}

/// A clone placed in the workweave by hand, with a commit of its own: no
/// baseline holds that repo, so nothing vouches that the commit lives
/// anywhere else.
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

    let out = delete(&weave, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stray.join("only-here.txt").is_file(), "{stderr}");
    assert!(!out.status.success(), "{stderr}");
    assert!(
        stderr.contains("\n\nrwv explain unmerged-commits\n")
            && stderr.contains(&format!("  local/owner/stray {OUTSIDE}")),
        "{stderr}"
    );
}

/// Control: what is outside the manifest and holds nothing does not block.
#[test]
fn a_clean_unlisted_checkout_does_not_block_delete() {
    let weave = weave();
    let s1 = weave.seat("s1");
    succeed(&weave.root, &["remove", EXTRA]);
    succeed(&weave.root, &["lock"]);
    commit_all(&weave.root, "drop extra");
    assert!(common::under(&s1, EXTRA).is_dir());

    succeed(&weave.root, &["workweave", PROJECT, "delete", "s1"]);
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
            "has uncommitted changes; refusing to delete without --discard-uncommitted:\n  {LIB}\n"
        )) && !stderr.contains(OUTSIDE),
        "{stderr}"
    );

    std::fs::remove_file(common::under(&s1, LIB).join("scratch.txt")).unwrap();
    succeed(&weave.root, &["workweave", PROJECT, "delete", "s1"]);
    assert!(!s1.exists());
}
