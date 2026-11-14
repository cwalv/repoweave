//! While an operation is recorded in a workspace, or leases it, every verb
//! but the ones `rwv` lets run there refuses before it changes anything: in
//! the landing's source, in the target it leases, and in a stopped sync.
//!
//! The verbs swept are the ones the dispatch table decides as refused, read
//! from the library, so a verb added to that list is swept here too and needs
//! a sample invocation below before this file passes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use repoweave::cli::during_op::{decided, DuringAnOperation};
use repoweave::refusal::RefusalKind;

use crate::common;
use crate::common::cli::{describe, refused, run, succeed};

const PROJECT: &str = "app";
const LIB: &str = "local/owner/lib";
const DOCS: &str = "local/owner/docs";

/// One invocation of each refused verb, spelled as the table spells it.
fn sample(spelling: &str) -> Option<Vec<&'static str>> {
    Some(match spelling {
        "activate" => vec!["activate", PROJECT],
        "backup drop" => vec!["backup", "drop", LIB, "--all"],
        "backup prune" => vec!["backup", "prune"],
        "branch create" => vec!["branch", "create", "zz"],
        "branch delete" => vec!["branch", "delete", "zz"],
        "branch rename" => vec!["branch", "rename", "zz"],
        "branch set-history" => vec!["branch", "set-history", "merge"],
        "branch set-mode" => vec!["branch", "set-mode", "declared"],
        "cherry-pick" => vec!["cherry-pick", "HEAD"],
        "clone" => vec!["clone", "file:///nowhere/web.git"],
        "fetch" => vec!["fetch"],
        "init" => vec!["init", "other"],
        "land" => vec!["land"],
        "lock" => vec!["lock"],
        "materialize" => vec!["materialize"],
        "member add" => vec!["member", "add", "file:///nowhere/zed.git"],
        "member pin" => vec!["member", "pin", LIB, "HEAD"],
        "member remove" => vec!["member", "remove", DOCS],
        "member set-branch" => vec!["member", "set-branch", DOCS, "other"],
        "member set-recurse-submodules" => {
            vec!["member", "set-recurse-submodules", DOCS, "true"]
        }
        "member set-role" => vec!["member", "set-role", DOCS, "reference"],
        "member set-url" => vec!["member", "set-url", DOCS, "file:///nowhere/docs.git"],
        "pull" => vec!["pull"],
        "push" => vec!["push"],
        "switch" => vec!["switch", "main"],
        "sync" => vec!["sync"],
        "update" => vec!["update"],
        "workweave" => vec!["workweave", "--hook-mode"],
        "workweave create" => vec!["workweave", "create", "x2"],
        "workweave delete" => vec!["workweave", "delete", "s1"],
        "workweave set-default-create-basedir" => {
            vec!["workweave", "set-default-create-basedir", "elsewhere"]
        }
        _ => return None,
    })
}

fn refused_spellings() -> Vec<&'static str> {
    decided()
        .filter(|(_, decision)| *decision == DuringAnOperation::Refused)
        .map(|(spelling, _)| spelling)
        .collect()
}

struct Weave {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    seat: PathBuf,
}

fn commit_file(repo: &Path, file: &str, text: &str, message: &str) {
    std::fs::write(repo.join(file), text).unwrap();
    common::git_in(repo, &["add", file]);
    common::git_in(repo, &["commit", "-m", message]);
}

/// A primary with two members and a seat forked from it, both of which have
/// committed to one file of `lib`, the primary relocked: the seat's landing
/// or sync stops on that member's conflict.
fn weave() -> Weave {
    let tmp = common::tempdir().unwrap();
    let primary = tmp.path().join("ws");
    let project = primary.join("projects").join(PROJECT);
    let mut entries = String::new();
    for member in [LIB, DOCS] {
        let upstream = tmp.path().join("upstream").join(member);
        std::fs::create_dir_all(&upstream).unwrap();
        common::git_in(&upstream, &["init", "-b", "main"]);
        commit_file(&upstream, "README.md", "init\n", "initial");
        common::git_in(
            tmp.path(),
            &[
                "clone",
                &common::file_url(&upstream),
                &common::under(&primary, member).to_string_lossy(),
            ],
        );
        entries.push_str(&format!(
            "[repositories.\"{member}\"]\ntype = \"git\"\nurl = \"{}\"\nbranch = \"main\"\nrole = \"owned\"\n",
            common::file_url(&upstream)
        ));
    }
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    common::record_trunk(&project);
    common::set_up_lock_driver(&project);
    std::fs::write(project.join(".gitignore"), "*.code-workspace\n").unwrap();
    std::fs::write(project.join("rwv.toml"), entries).unwrap();
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    common::pin_every_member(&primary);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);
    succeed(
        &["workweave", "create", "--project", PROJECT, "s1"],
        &primary,
    );
    let seat = tmp
        .path()
        .join(".workweaves")
        .join(format!("{PROJECT}--s1"));
    commit_file(&common::under(&seat, LIB), "c.txt", "seat\n", "seat: lib");
    commit_file(
        &common::under(&primary, LIB),
        "c.txt",
        "primary\n",
        "primary: lib",
    );
    succeed(&["lock", "--commit"], &primary);
    Weave {
        _tmp: tmp,
        primary,
        seat,
    }
}

/// Every repo of `workspace` as it stands: each ref of its store, HEAD and
/// what it holds uncommitted, and the op-state files at its root.
fn snapshot(workspace: &Path) -> BTreeMap<String, String> {
    let mut seen = BTreeMap::new();
    for repo in [
        workspace.join("projects").join(PROJECT),
        common::under(workspace, LIB),
        common::under(workspace, DOCS),
    ] {
        let name = repo.display().to_string();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        seen.insert(format!("{name} refs"), git(&["for-each-ref"]));
        seen.insert(format!("{name} HEAD"), git(&["rev-parse", "HEAD"]));
        seen.insert(format!("{name} on"), git(&["symbolic-ref", "-q", "HEAD"]));
        seen.insert(
            format!("{name} status"),
            git(&["status", "--porcelain", "--untracked-files=all"]),
        );
    }
    let mut root: Vec<String> = std::fs::read_dir(workspace)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    root.sort();
    seen.insert("root".to_owned(), root.join(" "));
    seen
}

fn sweep(workspaces: &[&Path]) {
    let spellings = refused_spellings();
    assert!(spellings.contains(&"member set-role"));
    for spelling in spellings {
        let args =
            sample(spelling).unwrap_or_else(|| panic!("no sample invocation of `rwv {spelling}`"));
        for workspace in workspaces {
            let before = snapshot(workspace);
            let out = run(&args, workspace);
            assert_eq!(
                out.status.code(),
                Some(1),
                "`rwv {}` in {} must refuse:\n{}",
                args.join(" "),
                workspace.display(),
                describe(&out)
            );
            refused(&out, RefusalKind::OpInProgress);
            assert_eq!(
                snapshot(workspace),
                before,
                "`rwv {}` in {} changed it",
                args.join(" "),
                workspace.display()
            );
        }
    }
}

#[test]
fn every_writing_verb_refuses_in_a_stopped_landings_source_and_leased_target() {
    let weave = weave();
    let out = run(&["land", "--history", "linear"], &weave.seat);
    assert!(!out.status.success(), "{}", describe(&out));
    assert!(weave.seat.join(".rwv-op").exists());
    assert!(weave.primary.join(".rwv-op-lease").exists());
    sweep(&[&weave.seat, &weave.primary]);
}

#[test]
fn every_writing_verb_refuses_in_a_stopped_sync() {
    let weave = weave();
    let out = run(&["sync", "--history", "linear"], &weave.seat);
    assert!(!out.status.success(), "{}", describe(&out));
    assert!(weave.seat.join(".rwv-op").exists(), "{}", describe(&out));
    sweep(&[&weave.seat]);
}

#[test]
fn the_verbs_that_run_during_an_operation_run() {
    let weave = weave();
    let out = run(&["land", "--history", "linear"], &weave.seat);
    assert!(!out.status.success(), "{}", describe(&out));
    for workspace in [&weave.seat, &weave.primary] {
        for args in [
            &["status"][..],
            &["log"],
            &["doctor"],
            &["backup", "list"],
            &["branch", "list"],
            &["workweave", "list"],
        ] {
            let out = run(args, workspace);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                !stderr.contains("op-in-progress"),
                "`rwv {}` in {} must run:\n{}",
                args.join(" "),
                workspace.display(),
                describe(&out)
            );
        }
    }
}
