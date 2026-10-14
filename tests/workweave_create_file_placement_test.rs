//! `rwv workweave create` reports, for each member, how its tracked files
//! reached the seat: cloned from a workspace that already holds them, or
//! written by the VCS. Which one depends on whether the filesystem can clone,
//! and a filesystem declared able to must show clones rather than pass on a
//! fallback.

use std::path::{Path, PathBuf};

mod common;

const MEMBERS: &[(&str, &str)] = &[
    ("github/acme/lib", "owned"),
    ("github/acme/docs", "reference"),
];

fn git_init_with_commit(dir: &Path) {
    common::git_in(dir, &["init", "--initial-branch=main"]);
    common::git_in(dir, &["add", "-A"]);
    common::git_in(dir, &["commit", "-m", "init"]);
}

/// A primary with one project holding [`MEMBERS`], each with three tracked
/// files.
fn weave(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    let mut manifest = String::new();
    for (repo, role) in MEMBERS {
        let clone = ws.join(repo);
        std::fs::create_dir_all(clone.join("src")).unwrap();
        for (file, body) in [
            ("README", "readme\n"),
            ("src/a.txt", "a\n"),
            ("src/b.txt", "b\n"),
        ] {
            std::fs::write(clone.join(file), body).unwrap();
        }
        git_init_with_commit(&clone);
        manifest.push_str(&format!(
            "[repositories.\"{repo}\"]\ntype = \"git\"\nurl = \"https://example.com/{repo}.git\"\n\
             version = \"main\"\nrole = \"{role}\"\n\n"
        ));
    }
    let project = ws.join("projects/web-app");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("rwv.toml"), manifest).unwrap();
    git_init_with_commit(&project);
    std::fs::write(ws.join(".rwv-active"), "web-app\n").unwrap();
    ws
}

/// Whether the filesystem under `dir` is declared able to clone; `None`
/// where nothing is declared, and the counts are only checked to add up.
fn declared_to_clone(dir: &Path) -> Option<bool> {
    if cfg!(target_os = "macos") {
        return Some(true);
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    let out = std::process::Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(dir)
        .output()
        .ok()?;
    match String::from_utf8_lossy(&out.stdout).trim() {
        "ext2/ext3" | "tmpfs" => Some(false),
        "btrfs" => Some(true),
        _ => None,
    }
}

/// `(reflinked, hardlinked, written)` from create's line for `repo`.
fn counts_for(stderr: &str, repo: &str) -> (usize, usize, usize) {
    let prefix = format!("rwv workweave create: {repo}: ");
    let line = stderr
        .lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("no file placement line for {repo} in:\n{stderr}"));
    let numbers: Vec<usize> = line
        .split(", ")
        .map(|part| part.split(' ').next().unwrap().parse().unwrap())
        .collect();
    (numbers[0], numbers[1], numbers[2])
}

#[test]
fn create_reports_how_each_members_files_were_placed() {
    let tmp = common::tempdir().unwrap();
    let ws = weave(tmp.path());

    let output = common::rwv()
        .args(["workweave", "web-app", "create", "agent-1"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "create failed:\n{stderr}");

    let seat = tmp.path().join(".workweaves/web-app--agent-1");
    for (repo, _) in MEMBERS {
        let member = seat.join(repo);
        let tracked = common::git_in(&member, &["ls-files"]).lines().count();
        assert_eq!(common::git_in(&member, &["status", "--porcelain"]), "");
        let (reflinked, hardlinked, written) = counts_for(&stderr, repo);
        assert_eq!(hardlinked, 0);
        match declared_to_clone(tmp.path()) {
            Some(true) => assert_eq!(
                (reflinked, written),
                (tracked, 0),
                "{repo}: declared able to clone, on {}",
                std::env::consts::OS
            ),
            Some(false) => assert_eq!((reflinked, written), (0, tracked), "{repo}"),
            None => assert_eq!(reflinked + written, tracked, "{repo}"),
        }
    }
}
