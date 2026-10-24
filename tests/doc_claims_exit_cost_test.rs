//! README and the introduction promise that leaving rwv by the steps they
//! give leaves plain standalone clones, with exactly two traces: a
//! `reference` member's tracked files without write bits, and the recovery
//! refs under `refs/rwv/`. The list is closed, so this drives a weave
//! through a workweave, an aborted `sync-to` and a landed one, leaves by
//! those steps, and compares each clone with a fresh `git clone` of the same
//! upstream.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

const PROJECT: &str = "app";
const LIB: &str = "local/owner/lib";
const DOCS: &str = "local/owner/docs";

fn rwv(dir: &Path, args: &[&str]) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn succeed(dir: &Path, args: &[&str]) {
    let out = rwv(dir, args);
    assert!(
        out.status.success(),
        "rwv {args:?} in {} failed:\nstdout:\n{}\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn commit_file(repo: &Path, file: &str, text: &str, message: &str) {
    std::fs::write(repo.join(file), text).unwrap();
    common::git_in(repo, &["add", file]);
    common::git_in(repo, &["commit", "-m", message]);
}

fn upstream(tmp: &Path, name: &str) -> String {
    let dir = tmp.join("upstream").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    common::git_in(&dir, &["init", "-b", "main"]);
    commit_file(&dir, "README.md", "init\n", "initial");
    common::file_url(&dir)
}

fn lines(repo: &Path, args: &[&str]) -> Vec<String> {
    common::git_in(repo, args)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn local_config_keys(repo: &Path) -> BTreeSet<String> {
    lines(repo, &["config", "--local", "--name-only", "--list"])
        .into_iter()
        .collect()
}

fn hooks(repo: &Path) -> BTreeSet<String> {
    std::fs::read_dir(repo.join(".git/hooks"))
        .map(|dir| {
            dir.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn exclude(repo: &Path) -> String {
    std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap_or_default()
}

fn tracked_files(repo: &Path) -> Vec<PathBuf> {
    lines(repo, &["ls-files"])
        .into_iter()
        .map(|f| repo.join(f))
        .collect()
}

#[cfg(unix)]
fn writable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o222 != 0
}

#[cfg(not(unix))]
fn writable(path: &Path) -> bool {
    !std::fs::metadata(path).unwrap().permissions().readonly()
}

#[test]
fn leaving_rwv_by_the_documented_steps_leaves_only_the_two_named_traces() {
    let tmp = common::tempdir().unwrap();
    let lib_url = upstream(tmp.path(), "lib");
    let docs_url = upstream(tmp.path(), "docs");

    let ws = tmp.path().join("ws");
    let project = ws.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    std::fs::write(project.join(".gitignore"), "*.code-workspace\n").unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{lib_url}\"\nversion = \"main\"\nrole = \"owned\"\n\
             [repositories.\"{DOCS}\"]\ntype = \"git\"\nurl = \"{docs_url}\"\nversion = \"main\"\nrole = \"reference\"\n"
        ),
    )
    .unwrap();
    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    succeed(&ws, &["fetch"]);
    succeed(&ws, &["lock"]);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);

    succeed(&ws, &["workweave", PROJECT, "create", "s1"]);
    let seat = tmp
        .path()
        .join(".workweaves")
        .join(format!("{PROJECT}--s1"));
    let primary_lib = common::under(&ws, LIB);
    let seat_lib = common::under(&seat, LIB);

    commit_file(&seat_lib, "c.txt", "seat\n", "seat: lib");
    commit_file(&primary_lib, "c.txt", "primary\n", "primary: lib");
    succeed(&ws, &["lock"]);
    common::git_in(&project, &["add", "rwv.lock"]);
    common::git_in(&project, &["commit", "-m", "primary: relock"]);
    let parked = rwv(&seat, &["sync-to"]);
    assert!(!parked.status.success(), "the conflict must park sync-to");
    succeed(&seat, &["abort"]);

    let primary_tip = common::git_in(&primary_lib, &["rev-parse", "HEAD"]);
    common::git_in(&seat_lib, &["reset", "--hard", &primary_tip]);
    commit_file(&seat_lib, "d.txt", "seat\n", "seat: lib again");
    succeed(&seat, &["sync-to"]);
    succeed(&ws, &["workweave", PROJECT, "delete", "s1"]);

    std::fs::remove_dir_all(ws.join("projects")).unwrap();
    std::fs::remove_file(ws.join(".rwv-active")).unwrap();
    for entry in std::fs::read_dir(&ws).unwrap() {
        let path = entry.unwrap().path();
        if path.symlink_metadata().unwrap().file_type().is_symlink() {
            std::fs::remove_file(&path).unwrap();
        }
    }

    let mut recovery_refs = 0;
    for (member, url) in [(LIB, &lib_url), (DOCS, &docs_url)] {
        let clone = common::under(&ws, member);
        let control = tmp.path().join("control").join(member);
        common::git_in(
            tmp.path(),
            &["clone", url.as_str(), &control.to_string_lossy()],
        );

        assert_eq!(
            lines(&clone, &["worktree", "list", "--porcelain"])
                .iter()
                .filter(|l| l.starts_with("worktree "))
                .count(),
            1,
            "{member}: its workweave worktrees must be gone"
        );
        assert!(
            std::fs::read_dir(clone.join(".git/worktrees"))
                .map(|mut d| d.next().is_none())
                .unwrap_or(true),
            "{member}: no worktree administration may remain"
        );
        let refs = lines(&clone, &["for-each-ref", "--format=%(refname)"]);
        for name in &refs {
            assert!(
                name == "refs/heads/main"
                    || name.starts_with("refs/remotes/origin/")
                    || name.starts_with("refs/rwv/"),
                "{member}: ref {name} is a trace the docs do not name; refs: {refs:?}"
            );
        }
        recovery_refs += refs.iter().filter(|r| r.starts_with("refs/rwv/")).count();

        assert_eq!(
            local_config_keys(&clone),
            local_config_keys(&control),
            "{member}: its local config must be a plain clone's"
        );
        assert_eq!(hooks(&clone), hooks(&control), "{member}: hooks");
        assert_eq!(exclude(&clone), exclude(&control), "{member}: info/exclude");

        let read_only = member == DOCS;
        for file in tracked_files(&clone) {
            assert_eq!(
                writable(&file),
                !read_only,
                "{}: only a reference member's tracked files lack write bits",
                file.display()
            );
        }
    }
    assert!(
        recovery_refs > 0,
        "the abort must leave its recovery ref, or the second trace is untested"
    );
}
