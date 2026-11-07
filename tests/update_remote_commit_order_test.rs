//! `rwv update --remote --commit` judges the project repo's uncommitted
//! changes before it fetches, advances a member or writes the lock.

use std::path::PathBuf;

mod common;

const MEMBER: &str = "github/example/server";
const PROJECT: &str = "web-app";

struct Weave {
    _root: tempfile::TempDir,
    primary: PathBuf,
    forge: PathBuf,
}

impl Weave {
    fn member(&self) -> PathBuf {
        self.primary.join(MEMBER)
    }

    fn project(&self) -> PathBuf {
        self.primary.join("projects").join(PROJECT)
    }

    fn rwv(&self, args: &[&str]) -> std::process::Output {
        common::rwv()
            .args(args)
            .current_dir(&self.primary)
            .output()
            .expect("rwv runs")
    }
}

fn weave() -> Weave {
    let root = common::tempdir().expect("temp root");
    let parent = root.path().to_path_buf();
    let primary = parent.join("primary");
    let origin = parent.join("origin");
    std::fs::create_dir_all(&origin).unwrap();
    std::fs::create_dir_all(primary.join("github/example")).unwrap();
    std::fs::create_dir_all(primary.join("projects")).unwrap();

    let member_origin = origin.join("server.git");
    let project_origin = origin.join("web-app.git");
    for bare in [&member_origin, &project_origin] {
        common::git_in(
            &origin,
            &["init", "--bare", "-b", "main", &bare.to_string_lossy()],
        );
    }

    let member = primary.join(MEMBER);
    common::git_in(
        &parent,
        &[
            "clone",
            &member_origin.to_string_lossy(),
            &member.to_string_lossy(),
        ],
    );
    std::fs::write(member.join("README.md"), "init\n").unwrap();
    common::git_in(&member, &["add", "."]);
    common::git_in(&member, &["commit", "-m", "initial"]);
    common::git_in(&member, &["push", "origin", "main"]);

    let project = primary.join("projects").join(PROJECT);
    common::git_in(
        &parent,
        &[
            "clone",
            &project_origin.to_string_lossy(),
            &project.to_string_lossy(),
        ],
    );
    let url = common::file_url(&member_origin);
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories]\n[repositories.\"{MEMBER}\"]\ntype = \"git\"\nurl = \"{url}\"\n\
             branch = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    common::fixture_lock_at_remote_tips(&project, &[(MEMBER, &url)]);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "manifest and lock"]);
    common::git_in(&project, &["push", "origin", "main"]);
    common::record_trunk(&project);
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    let forge = parent.join("forge");
    common::git_in(
        &parent,
        &[
            "clone",
            &member_origin.to_string_lossy(),
            &forge.to_string_lossy(),
        ],
    );
    Weave {
        _root: root,
        primary,
        forge,
    }
}

#[test]
fn update_remote_commit_refuses_project_repo_dirt_before_anything_moves() {
    let weave = weave();
    std::fs::write(weave.project().join("notes.txt"), "committed\n").unwrap();
    common::git_in(weave.project(), &["add", "notes.txt"]);
    common::git_in(weave.project(), &["commit", "-m", "notes"]);
    std::fs::write(
        weave.project().join("notes.txt"),
        "edited and uncommitted\n",
    )
    .unwrap();

    std::fs::write(weave.forge.join("upstream.txt"), "upstream\n").unwrap();
    common::git_in(&weave.forge, &["add", "upstream.txt"]);
    common::git_in(&weave.forge, &["commit", "-m", "upstream"]);
    common::git_in(&weave.forge, &["push", "origin", "main"]);

    let lock_before = std::fs::read(weave.project().join("rwv.lock")).unwrap();
    let member_before = common::git_in(weave.member(), &["rev-parse", "HEAD"]);

    let out = weave.rwv(&["update", "--remote", "--commit"]);

    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("projects/{PROJECT}: edits dirty"))
            && stderr.contains("notes.txt"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read(weave.project().join("rwv.lock")).unwrap(),
        lock_before,
        "the lock is not written before the refusal"
    );
    assert_eq!(
        common::git_in(weave.member(), &["rev-parse", "HEAD"]),
        member_before,
        "no member is advanced before the refusal"
    );
    assert_eq!(
        common::git_in(weave.member(), &["rev-parse", "refs/remotes/origin/main"]),
        member_before,
        "nothing is fetched before the refusal"
    );
}

#[test]
fn update_remote_without_commit_still_writes_the_lock_over_project_repo_dirt() {
    let weave = weave();
    std::fs::write(weave.project().join("scratch.txt"), "untracked\n").unwrap();
    std::fs::write(weave.forge.join("upstream.txt"), "upstream\n").unwrap();
    common::git_in(&weave.forge, &["add", "upstream.txt"]);
    common::git_in(&weave.forge, &["commit", "-m", "upstream"]);
    common::git_in(&weave.forge, &["push", "origin", "main"]);
    let tip = common::git_in(&weave.forge, &["rev-parse", "HEAD"]);

    let out = weave.rwv(&["update", "--remote"]);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(std::fs::read_to_string(weave.project().join("rwv.lock"))
        .unwrap()
        .contains(&tip));
}
