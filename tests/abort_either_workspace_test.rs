//! `rwv abort` of a parked `sync-to` gives the same verdict on every repo
//! whichever of the op's two workspaces it is invoked from.
//!
//! Each repo is judged by the side it sits on in the owner record: the
//! workweave the op replayed in, whose replay journal (`advanced_tips`)
//! attributes the tips replay moved it to, and the target, which nothing
//! journals before relock. Which workspace abort runs in decides only which
//! side's lines go unlabeled.

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const PROJECT: &str = "app";
const LIB: &str = "local/owner/lib";

struct Side {
    root: PathBuf,
    project: PathBuf,
    lib: PathBuf,
}

struct Parked {
    _tmp: tempfile::TempDir,
    primary: Side,
    seat: Side,
}

#[derive(Clone, Copy, Debug)]
enum Invoked {
    Target,
    Workweave,
}

impl Parked {
    fn dir(&self, from: Invoked) -> &Path {
        match from {
            Invoked::Target => &self.primary.root,
            Invoked::Workweave => &self.seat.root,
        }
    }

    fn abort(&self, from: Invoked) -> Output {
        common::rwv()
            .arg("abort")
            .current_dir(self.dir(from))
            .output()
            .unwrap()
    }
}

fn side(root: PathBuf) -> Side {
    Side {
        project: root.join("projects").join(PROJECT),
        lib: common::under(&root, LIB),
        root,
    }
}

fn head(repo: &Path) -> String {
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn commit_file(repo: &Path, file: &str, text: &str, message: &str) {
    std::fs::write(repo.join(file), text).unwrap();
    common::git_in(repo, &["add", file]);
    common::git_in(repo, &["commit", "-m", message]);
}

fn succeed(dir: &Path, args: &[&str]) {
    let out = common::rwv().args(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "rwv {args:?} in {} failed:\n{}",
        dir.display(),
        transcript(&out)
    );
}

fn transcript(out: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A primary weave with one member, and a seat forked from it by `rwv
/// workweave create`. Both then commit to the same file of the member, and
/// the primary relocks, so the seat's `sync-to` replays the primary's project
/// repo onto the seat and parks on the member's rebase conflict.
///
/// `seat_project_commit` gives the seat a project commit of its own, so the
/// replay rebases it into a commit only the seat's branch holds instead of
/// fast-forwarding the seat onto the primary's tip.
fn park(seat_project_commit: bool) -> Parked {
    let tmp = common::tempdir().unwrap();
    let upstream = tmp.path().join("upstream/owner/lib");
    std::fs::create_dir_all(&upstream).unwrap();
    common::git_in(&upstream, &["init", "-b", "main"]);
    commit_file(&upstream, "README.md", "init\n", "initial");

    let primary = side(tmp.path().join("ws"));
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &common::file_url(&upstream),
            &primary.lib.to_string_lossy(),
        ],
    );
    std::fs::create_dir_all(&primary.project).unwrap();
    common::git_in(&primary.project, &["init", "-b", "main"]);
    std::fs::write(
        primary.project.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    std::fs::write(primary.project.join(".gitignore"), "*.code-workspace\n").unwrap();
    std::fs::write(
        primary.project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\nrole = \"owned\"\n",
            common::file_url(&upstream)
        ),
    )
    .unwrap();
    std::fs::write(primary.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    succeed(&primary.root, &["lock"]);
    common::git_in(&primary.project, &["add", "-A"]);
    common::git_in(&primary.project, &["commit", "-m", "initial"]);

    succeed(&primary.root, &["workweave", PROJECT, "create", "s1"]);
    let seat = side(tmp.path().join(".workweaves").join(format!("{PROJECT}--s1")));

    commit_file(&seat.lib, "c.txt", "seat\n", "seat: lib");
    if seat_project_commit {
        commit_file(&seat.project, "notes.txt", "seat\n", "seat: notes");
    }
    commit_file(&primary.lib, "c.txt", "primary\n", "primary: lib");
    succeed(&primary.root, &["lock"]);
    common::git_in(&primary.project, &["add", "rwv.lock"]);
    common::git_in(&primary.project, &["commit", "-m", "primary: relock"]);

    Parked {
        _tmp: tmp,
        primary,
        seat,
    }
}

struct PreOp {
    primary_lib: String,
    primary_project: String,
    seat_lib: String,
    seat_project: String,
}

fn pre_op(parked: &Parked) -> PreOp {
    PreOp {
        primary_lib: head(&parked.primary.lib),
        primary_project: head(&parked.primary.project),
        seat_lib: head(&parked.seat.lib),
        seat_project: head(&parked.seat.project),
    }
}

fn run_sync_to_into_park(parked: &Parked) {
    let out = common::rwv()
        .arg("sync-to")
        .current_dir(&parked.seat.root)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "the member conflict must park sync-to:\n{}",
        transcript(&out)
    );
    assert!(parked.seat.root.join(".rwv-op").exists());
    assert!(parked.primary.root.join(".rwv-op-lease").exists());
}

fn assert_restored(parked: &Parked, pre: &PreOp, out: &Output) {
    for (repo, expected) in [
        (&parked.primary.lib, &pre.primary_lib),
        (&parked.primary.project, &pre.primary_project),
        (&parked.seat.lib, &pre.seat_lib),
        (&parked.seat.project, &pre.seat_project),
    ] {
        assert_eq!(
            &head(repo),
            expected,
            "{} must be back at its pre-op tip\n{}",
            repo.display(),
            transcript(out)
        );
    }
    assert!(!parked.seat.root.join(".rwv-op").exists());
    assert!(!parked.primary.root.join(".rwv-op-lease").exists());
}

fn abort_restores_both_workspaces(from: Invoked) {
    let parked = park(false);
    let pre = pre_op(&parked);
    run_sync_to_into_park(&parked);
    assert_ne!(
        head(&parked.seat.project),
        pre.seat_project,
        "replay must have moved the seat's project repo, or nothing journaled \
         in advanced_tips is under test"
    );

    let out = parked.abort(from);
    assert!(out.status.success(), "{from:?}:\n{}", transcript(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let seat_label = match from {
        Invoked::Target => "[source] ",
        Invoked::Workweave => "",
    };
    for line in [
        format!("  {seat_label}(project): restored (from recorded intent tip)"),
        format!("  {seat_label}{LIB}: restored (from mid-op state)"),
    ] {
        assert!(
            stdout.lines().any(|l| l == line),
            "{from:?}: expected the line {line:?}\n{}",
            transcript(&out)
        );
    }
    if let Invoked::Target = from {
        assert!(
            !stdout.contains("[target]"),
            "invoked from the target, no line may call a repo the target's:\n{}",
            transcript(&out)
        );
    }
    assert_restored(&parked, &pre, &out);
}

#[test]
fn abort_from_the_target_restores_both_workspaces() {
    abort_restores_both_workspaces(Invoked::Target);
}

#[test]
fn abort_from_the_workweave_restores_both_workspaces() {
    abort_restores_both_workspaces(Invoked::Workweave);
}

/// The target's project repo is moved, by hand, onto the commit the seat's
/// replay rebased its own project commit into. That commit is in the seat's
/// replay journal, but the op never put the target there, so the target's
/// tip is foreign from either side.
fn target_at_the_seats_journaled_tip_is_foreign(from: Invoked) {
    let parked = park(true);
    let pre = pre_op(&parked);
    run_sync_to_into_park(&parked);
    let journaled = head(&parked.seat.project);
    assert_ne!(journaled, pre.seat_project);
    assert_ne!(journaled, pre.primary_project);
    common::git_in(&parked.primary.project, &["merge", "--ff-only", &journaled]);

    let out = parked.abort(from);
    assert!(!out.status.success(), "{from:?}:\n{}", transcript(&out));
    let target_label = match from {
        Invoked::Target => "",
        Invoked::Workweave => "[target] ",
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    let refusal = format!("  {target_label}(project): foreign-tip violation");
    assert!(
        stderr.lines().any(|l| l.starts_with(&refusal)),
        "{from:?}: expected {refusal:?}\n{}",
        transcript(&out)
    );
    assert_eq!(
        head(&parked.primary.project),
        journaled,
        "a refused repo keeps its tip"
    );
}

#[test]
fn from_the_target_its_tip_at_the_seats_journaled_tip_is_foreign() {
    target_at_the_seats_journaled_tip_is_foreign(Invoked::Target);
}

#[test]
fn from_the_workweave_the_targets_tip_at_its_journaled_tip_is_foreign() {
    target_at_the_seats_journaled_tip_is_foreign(Invoked::Workweave);
}
