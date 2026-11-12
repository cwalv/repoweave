//! A `git` older than the backend's floor, one that names no release, and no
//! `git` at all: each refuses a repository verb before it reads or writes
//! anything, and leaves the verbs that touch no repository working.
//!
//! The `git` here is a script that records every invocation and answers only
//! `version`, so "nothing was read" is the record holding that one call.
//! `#![cfg(unix)]`: the stand-in is a shell script.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use repoweave::refusal::RefusalKind;

mod common;

const FLOOR: &str = "2.36";

struct StandIn {
    _dir: tempfile::TempDir,
    bin: PathBuf,
    calls: PathBuf,
}

fn stand_in(answer: &str) -> StandIn {
    let dir = common::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = dir.path().join("calls");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{answer}\n",
        calls.display()
    );
    let git = bin.join("git");
    std::fs::write(&git, script).unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    StandIn {
        _dir: dir,
        bin,
        calls,
    }
}

impl StandIn {
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(&self.calls)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn rwv_with_path(path: &Path, args: &[&str], dir: &Path) -> Output {
    common::rwv()
        .args(args)
        .current_dir(dir)
        .env("PATH", path)
        .output()
        .unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = walk(dir)
        .into_iter()
        .map(|p| p.strip_prefix(dir).unwrap().display().to_string())
        .collect();
    names.sort();
    names
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            let mut below = if path.is_dir() {
                walk(&path)
            } else {
                Vec::new()
            };
            below.push(path);
            below
        })
        .collect()
}

fn weave() -> tempfile::TempDir {
    let ws = common::tempdir().unwrap();
    std::fs::create_dir_all(ws.path().join("projects")).unwrap();
    ws
}

#[test]
fn a_git_below_the_floor_refuses_before_anything_is_read_or_written() {
    let old = stand_in("echo 'git version 2.35.9'");
    let ws = weave();
    let before = entries(ws.path());

    for args in [&["init", "demo"][..], &["status"][..], &["lock"][..]] {
        let out = rwv_with_path(&old.bin, args, ws.path());
        let stderr = common::cli::refused(&out, RefusalKind::UnsupportedVcsTool);
        assert!(
            stderr.contains(&format!("needs git {FLOOR} or newer")) && stderr.contains("2.35.9"),
            "`rwv {}` names neither the floor nor the release found:\n{stderr}",
            args.join(" ")
        );
    }

    assert_eq!(
        entries(ws.path()),
        before,
        "a refused verb wrote into the weave"
    );
    assert_eq!(
        old.calls(),
        vec!["version"; 3],
        "a refused verb ran more than `git version`"
    );
}

#[test]
fn a_git_at_the_floor_is_accepted() {
    let at = stand_in("[ \"$1\" = version ] && echo 'git version 2.36.0' || exit 1");
    let ws = weave();

    let out = rwv_with_path(&at.bin, &["init", "demo"], ws.path());

    let route = format!("rwv explain {}", RefusalKind::UnsupportedVcsTool.token());
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains(&route),
        "{}",
        common::cli::describe(&out)
    );
    assert!(
        at.calls().len() > 1,
        "rwv stopped at `git version`: {:?}",
        at.calls()
    );
}

#[test]
fn a_git_naming_no_release_refuses_as_unsupported() {
    let odd = stand_in("echo 'hub version 2.14.2'");
    let ws = weave();

    let out = rwv_with_path(&odd.bin, &["status"], ws.path());

    let stderr = common::cli::refused(&out, RefusalKind::UnsupportedVcsTool);
    assert!(stderr.contains("hub version 2.14.2"), "{stderr}");
}

#[test]
fn no_git_refuses_as_missing_not_as_too_old() {
    let empty = common::tempdir().unwrap();
    let ws = weave();
    let before = entries(ws.path());

    let out = rwv_with_path(empty.path(), &["init", "demo"], ws.path());

    let stderr = common::cli::refused(&out, RefusalKind::MissingVcsTool);
    assert!(
        stderr.contains(&format!("git {FLOOR} or newer")),
        "{stderr}"
    );
    assert_eq!(entries(ws.path()), before);
}

#[test]
fn verbs_that_touch_no_repository_run_with_an_old_git_or_none() {
    let old = stand_in("echo 'git version 2.25.1'");
    let empty = common::tempdir().unwrap();
    let ws = weave();

    for path in [old.bin.as_path(), empty.path()] {
        for args in [
            &["explain"][..],
            &["explain", "unsupported-vcs-tool"][..],
            &["--help"][..],
            &["--version"][..],
            &["completions", "bash"][..],
        ] {
            let out = rwv_with_path(path, args, ws.path());
            assert!(
                out.status.success(),
                "`rwv {}` with PATH={}:\n{}",
                args.join(" "),
                path.display(),
                common::cli::describe(&out)
            );
        }
    }
    assert!(old.calls().is_empty(), "{:?}", old.calls());
}
