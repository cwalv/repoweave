//! `rwv fetch`: the pruning fetch moves remote-tracking branches and nothing
//! else, keeps the tip a rewrite dropped, and runs one fetch at a time per
//! object store.

use serde_json::Value;
use std::path::PathBuf;

mod common;

const MEMBER: &str = "github/example/server";
const PROJECT: &str = "web-app";

struct Weave {
    _root: tempfile::TempDir,
    parent: PathBuf,
    primary: PathBuf,
    member_origin: PathBuf,
    forge: PathBuf,
}

impl Weave {
    fn member(&self) -> PathBuf {
        self.primary.join(MEMBER)
    }

    fn project(&self) -> PathBuf {
        self.primary.join("projects").join(PROJECT)
    }

    fn git_member(&self, args: &[&str]) -> String {
        common::git_in(self.member(), args)
    }

    fn forge_git(&self, args: &[&str]) -> String {
        common::git_in(&self.forge, args)
    }

    /// A colleague adds a commit on `branch` of the member's origin, creating
    /// the branch from `main` when the origin lacks it.
    fn forge_commit(&self, branch: &str, file: &str) -> String {
        self.forge_git(&["fetch", "--quiet", "origin"]);
        let remote = format!("origin/{branch}");
        if self
            .forge_git(&["branch", "--list", "--remotes", &remote])
            .is_empty()
        {
            self.forge_git(&["switch", "-C", branch, "origin/main"]);
        } else {
            self.forge_git(&["switch", "-C", branch, &remote]);
        }
        std::fs::write(self.forge.join(file), file).unwrap();
        self.forge_git(&["add", file]);
        self.forge_git(&["commit", "-m", file]);
        self.forge_git(&["push", "origin", branch]);
        self.forge_git(&["rev-parse", "HEAD"])
    }

    /// A colleague replaces `branch`'s tip on the origin with another commit
    /// and force-pushes it.
    fn forge_rewrite_tip(&self, branch: &str, file: &str) -> String {
        self.forge_git(&["fetch", "--quiet", "origin"]);
        self.forge_git(&["switch", "-C", branch, &format!("origin/{branch}")]);
        self.forge_git(&["reset", "--hard", "HEAD~1"]);
        std::fs::write(self.forge.join(file), file).unwrap();
        self.forge_git(&["add", file]);
        self.forge_git(&["commit", "-m", file]);
        self.forge_git(&["push", "--force", "origin", branch]);
        self.forge_git(&["rev-parse", "HEAD"])
    }

    fn rwv(&self, args: &[&str]) -> std::process::Output {
        common::rwv()
            .args(args)
            .current_dir(&self.primary)
            .output()
            .expect("rwv runs")
    }

    fn superseded(&self) -> String {
        self.git_member(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/rwv/superseded",
        ])
    }

    /// Everything a fetch must leave alone, as one text.
    fn local_state(&self) -> String {
        let mut text = String::new();
        for dir in [self.member(), self.project()] {
            for args in [
                &["rev-parse", "HEAD"][..],
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                ][..],
                &["status", "--porcelain"][..],
            ] {
                text.push_str(&common::git_in(&dir, args));
                text.push('\n');
            }
        }
        text.push_str(&std::fs::read_to_string(self.project().join("rwv.lock")).unwrap());
        text
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
        parent,
        primary,
        member_origin,
        forge,
    }
}

fn json(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON document ({e}):\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn moves_of<'a>(doc: &'a Value, path: &str) -> &'a Vec<Value> {
    doc["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == path)
        .unwrap_or_else(|| panic!("no record for {path} in {doc:#}"))["moved"]
        .as_array()
        .unwrap()
}

#[test]
fn fetch_reports_what_the_remote_advanced_and_moves_no_local_state() {
    let weave = weave();
    let before = weave.local_state();
    let local_main = weave.git_member(&["rev-parse", "main"]);
    let tip = weave.forge_commit("main", "advance.txt");

    let out = weave.rwv(&["fetch"]);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("{MEMBER}: origin/main")) && stdout.contains(&tip[..12]),
        "the advance is reported with the new tip:\n{stdout}"
    );
    assert_eq!(
        weave.git_member(&["rev-parse", "refs/remotes/origin/main"]),
        tip
    );
    assert_eq!(
        weave.git_member(&["rev-parse", "main"]),
        local_main,
        "the local branch stays"
    );
    assert_eq!(weave.local_state(), before, "nothing local moved");
    assert_eq!(weave.superseded(), "", "a fast-forward supersedes nothing");
}

#[test]
fn fetch_keeps_the_old_tip_of_a_rewritten_branch_and_says_it_was_rewritten() {
    let weave = weave();
    let old = weave.forge_commit("feat", "first.txt");
    assert!(weave.rwv(&["fetch"]).status.success());
    let new = weave.forge_rewrite_tip("feat", "second.txt");

    let out = weave.rwv(&["fetch", "--json"]);

    assert!(out.status.success());
    let doc = json(&out);
    let rewritten: Vec<&Value> = moves_of(&doc, MEMBER)
        .iter()
        .filter(|m| m["move"] == "rewritten")
        .collect();
    assert_eq!(rewritten.len(), 1, "{doc:#}");
    assert_eq!(rewritten[0]["branch"], "origin/feat");
    assert_eq!(rewritten[0]["from"], old.as_str());
    assert_eq!(rewritten[0]["to"], new.as_str());
    assert_eq!(
        weave.superseded(),
        format!("refs/rwv/superseded/origin/feat/{old} {old}"),
        "the old tip is kept under the superseded namespace"
    );
    assert_eq!(
        weave.git_member(&["rev-parse", "refs/remotes/origin/feat"]),
        new
    );

    let text = weave.rwv(&["fetch"]);
    assert!(
        String::from_utf8_lossy(&text.stdout).contains("nothing moved"),
        "a second fetch has nothing to report"
    );
}

#[test]
fn fetch_text_names_a_rewritten_branch() {
    let weave = weave();
    weave.forge_commit("feat", "first.txt");
    assert!(weave.rwv(&["fetch"]).status.success());
    weave.forge_rewrite_tip("feat", "second.txt");

    let out = weave.rwv(&["fetch"]);

    assert!(
        String::from_utf8_lossy(&out.stdout).contains("origin/feat rewritten"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn fetch_records_no_superseded_tip_for_a_branch_the_remote_deleted() {
    let weave = weave();
    weave.forge_commit("feat", "first.txt");
    assert!(weave.rwv(&["fetch"]).status.success());
    weave.forge_git(&["push", "origin", "--delete", "feat"]);

    let out = weave.rwv(&["fetch", "--json"]);

    assert!(out.status.success());
    let doc = json(&out);
    assert!(
        moves_of(&doc, MEMBER)
            .iter()
            .any(|m| m["move"] == "pruned" && m["branch"] == "origin/feat"),
        "{doc:#}"
    );
    assert_eq!(weave.superseded(), "", "a prune supersedes nothing");
    assert_eq!(
        weave.git_member(&["branch", "--list", "--remotes", "origin/feat"]),
        "",
        "the remote-tracking branch is dropped"
    );
}

#[test]
fn fetch_covers_the_project_repo_and_a_selector_narrows_it_to_members() {
    let weave = weave();

    let all = json(&weave.rwv(&["fetch", "--json"]));
    let paths: Vec<&str> = all["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, [format!("projects/{PROJECT}"), MEMBER.to_owned()]);

    let narrowed = json(&weave.rwv(&["fetch", "--json", "--repo", MEMBER]));
    let paths: Vec<&str> = narrowed["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, [MEMBER]);
}

#[test]
fn fetch_skips_a_member_that_is_not_on_disk_and_succeeds() {
    let weave = weave();
    std::fs::remove_dir_all(weave.member()).unwrap();

    let out = weave.rwv(&["fetch", "--json"]);

    assert!(out.status.success());
    let doc = json(&out);
    let record = doc["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == MEMBER)
        .unwrap();
    assert_eq!(record["status"], "skipped");
    assert!(record["message"].as_str().unwrap().contains("rwv update"));
}

#[test]
fn fetch_leaves_a_repo_unconfirmed_and_exits_one_once_every_repo_was_tried() {
    let weave = weave();
    let away = weave.parent.join("server.git.away");
    std::fs::rename(&weave.member_origin, &away).unwrap();

    let out = weave.rwv(&["fetch", "--json"]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    let status_of = |path: &str| {
        doc["repos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["path"] == path)
            .unwrap()["status"]
            .clone()
    };
    assert_eq!(status_of(MEMBER), "unconfirmed");
    assert_eq!(
        status_of(&format!("projects/{PROJECT}")),
        "fetched",
        "the other repos are still tried"
    );
}

#[test]
fn fetch_refuses_arguments_that_belong_to_other_verbs() {
    let weave = weave();
    for flag in ["--frozen", "--detach-checkouts", "--allow-non-empty-dir"] {
        let out = weave.rwv(&["fetch", flag]);
        assert_eq!(out.status.code(), Some(2), "{flag}");
    }
    let out = weave.rwv(&["fetch", "file:///elsewhere"]);
    assert_eq!(out.status.code(), Some(2), "fetch takes no source");
}

#[cfg(feature = "test-pause")]
mod claim {
    use super::*;
    use std::time::{Duration, Instant};

    /// A fetch held under the object store's claim keeps a second fetch into
    /// the same store waiting, and the second goes on once the first lets go.
    #[test]
    fn a_second_fetch_into_one_store_waits_for_the_first_to_finish() {
        let weave = weave();
        let pause_dir = weave.parent.join("pause");
        let mut first = common::rwv_std();
        first
            .args(["fetch", "--repo", MEMBER])
            .current_dir(&weave.primary);
        let held = common::pause::Paused::spawn(first, "fetch-under-claim", &pause_dir)
            .expect("the first fetch is held under the claim");

        let mut second = common::rwv_std();
        second
            .args(["fetch", "--repo", MEMBER])
            .current_dir(&weave.primary)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut waiting = second.spawn().expect("the second fetch starts");
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            assert!(
                waiting.try_wait().unwrap().is_none(),
                "the second fetch ran to the end while the first held the store"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let first_out = held.release();
        assert!(first_out.status.success());
        let status = waiting.wait().expect("the second fetch ends");
        assert!(status.success(), "the second fetch proceeds once released");
    }

    /// The pruning fetch of `rwv backup prune` is the same fetch: it waits
    /// for one holding the store.
    #[test]
    fn a_backup_prune_waits_for_a_fetch_holding_the_store() {
        let weave = weave();
        let pause_dir = weave.parent.join("pause");
        let mut fetch = common::rwv_std();
        fetch
            .args(["fetch", "--repo", MEMBER])
            .current_dir(&weave.primary);
        let held = common::pause::Paused::spawn(fetch, "fetch-under-claim", &pause_dir)
            .expect("the fetch is held under the claim");

        let mut prune = common::rwv_std();
        prune
            .args(["backup", "prune"])
            .current_dir(&weave.primary)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut waiting = prune.spawn().expect("the prune starts");
        let deadline = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < deadline {
            assert!(
                waiting.try_wait().unwrap().is_none(),
                "the prune ran to the end while the fetch held the store"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let fetch_out = held.release();
        assert!(fetch_out.status.success());
        let status = waiting.wait().expect("the prune ends");
        assert!(status.success(), "the prune proceeds once released");
    }
}
