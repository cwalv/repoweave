//! `rwv pull`: fetch, fast-forward the project branch only, then update.

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
    project_origin: PathBuf,
    project_forge: PathBuf,
}

impl Weave {
    fn member(&self) -> PathBuf {
        self.primary.join(MEMBER)
    }

    fn project(&self) -> PathBuf {
        self.primary.join("projects").join(PROJECT)
    }

    fn member_url(&self) -> String {
        common::file_url(&self.member_origin)
    }

    fn git_member(&self, args: &[&str]) -> String {
        common::git_in(self.member(), args)
    }

    fn git_project(&self, args: &[&str]) -> String {
        common::git_in(self.project(), args)
    }

    /// A colleague adds a commit on the member origin's `main`.
    fn forge_member_commit(&self, file: &str) -> String {
        common::git_in(&self.forge, &["pull", "--quiet", "origin", "main"]);
        std::fs::write(self.forge.join(file), file).unwrap();
        common::git_in(&self.forge, &["add", file]);
        common::git_in(&self.forge, &["commit", "-m", file]);
        common::git_in(&self.forge, &["push", "origin", "main"]);
        common::git_in(&self.forge, &["rev-parse", "HEAD"])
    }

    /// A colleague commits to the project origin's `main`: `rwv.toml`
    /// rewritten by `manifest` when given, `rwv.lock` pinning the member at
    /// `pin` when given.
    fn forge_project_commit(&self, manifest: Option<String>, pin: Option<&str>) -> String {
        let forge = &self.project_forge;
        common::git_in(forge, &["pull", "--quiet", "origin", "main"]);
        if let Some(manifest) = manifest {
            std::fs::write(forge.join("rwv.toml"), manifest).unwrap();
        }
        if let Some(pin) = pin {
            common::fixture_lock(forge, &[(MEMBER, &self.member_url(), pin)]);
        }
        std::fs::write(forge.join("notes.txt"), "colleague\n").unwrap();
        common::git_in(forge, &["add", "-A"]);
        common::git_in(forge, &["commit", "-m", "colleague"]);
        common::git_in(forge, &["push", "origin", "main"]);
        common::git_in(forge, &["rev-parse", "HEAD"])
    }

    fn manifest_with_role(&self, role: &str) -> String {
        format!(
            "[repositories]\n[repositories.\"{MEMBER}\"]\ntype = \"git\"\nurl = \"{}\"\n\
             branch = \"main\"\nrole = \"{role}\"\n",
            self.member_url()
        )
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
    let project_forge = parent.join("project-forge");
    common::git_in(
        &parent,
        &[
            "clone",
            &project_origin.to_string_lossy(),
            &project_forge.to_string_lossy(),
        ],
    );
    common::git_in(&project_forge, &["config", "user.email", "t@t.com"]);

    Weave {
        _root: root,
        parent,
        primary,
        member_origin,
        forge,
        project_origin,
        project_forge,
    }
}

fn stdout(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn pull_fast_forwards_the_project_branch_then_aligns_the_members_to_its_lock() {
    let weave = weave();
    let old_member = weave.git_member(&["rev-parse", "HEAD"]);
    let new_member = weave.forge_member_commit("member-work.txt");
    let new_project = weave.forge_project_commit(None, Some(&new_member));

    let out = weave.rwv(&["pull"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(weave.git_project(&["rev-parse", "HEAD"]), new_project);
    assert_eq!(
        weave.git_member(&["rev-parse", "HEAD"]),
        new_member,
        "the member follows the lock the pulled branch holds (was {old_member})"
    );
    assert!(
        stdout(&out).contains(&format!("projects/{PROJECT}: took 'main'")),
        "{}",
        stdout(&out)
    );
}

#[test]
fn pull_json_reports_the_fetch_the_project_branch_and_the_update() {
    let weave = weave();
    let new_member = weave.forge_member_commit("member-work.txt");
    let new_project = weave.forge_project_commit(None, Some(&new_member));

    let out = weave.rwv(&["pull", "--json"]);

    assert!(out.status.success(), "{}", stderr(&out));
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["project"]["result"], "advanced");
    assert_eq!(doc["project"]["to"], new_project.as_str());
    assert!(doc["fetched"].as_array().unwrap().len() == 2);
    assert!(doc["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["path"] == MEMBER && o["status"] == "ok"));
}

#[test]
fn pull_reports_a_member_the_pulled_lock_moved_backward_as_re_pinned() {
    let weave = weave();
    let old_member = weave.git_member(&["rev-parse", "HEAD"]);
    let new_member = weave.forge_member_commit("member-work.txt");
    weave.git_member(&["pull", "--quiet", "origin", "main"]);
    weave.git_project(&["config", "user.email", "t@t.com"]);
    common::fixture_lock(
        &weave.project(),
        &[(MEMBER, &weave.member_url(), &new_member)],
    );
    weave.git_project(&["commit", "-qam", "lock: member work"]);
    weave.git_project(&["push", "origin", "main"]);
    weave.forge_project_commit(None, Some(&old_member));

    let out = weave.rwv(&["pull"]);

    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains(&format!(
            "re-pinned {MEMBER}: {} -> {}",
            &new_member[..12],
            &old_member[..12]
        )),
        "{text}"
    );
}

#[test]
fn pull_reports_each_member_entry_the_pulled_manifest_changed() {
    let weave = weave();
    weave.forge_project_commit(Some(weave.manifest_with_role("dependency")), None);

    let out = weave.rwv(&["pull"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains(&format!(
            "member-changed {MEMBER}: role: owned -> dependency"
        )),
        "{}",
        stdout(&out)
    );
}

#[test]
fn pull_refuses_a_diverged_project_branch_and_moves_nothing() {
    let weave = weave();
    std::fs::write(weave.project().join("local.txt"), "mine\n").unwrap();
    weave.git_project(&["add", "local.txt"]);
    weave.git_project(&["commit", "-m", "mine"]);
    let mine = weave.git_project(&["rev-parse", "HEAD"]);
    let new_member = weave.forge_member_commit("member-work.txt");
    weave.forge_project_commit(None, Some(&new_member));
    let member_before = weave.git_member(&["rev-parse", "HEAD"]);

    let out = weave.rwv(&["pull"]);

    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains(&format!(
            "projects/{PROJECT}: push-branch diverged (1 ahead, 1 behind)"
        )),
        "{err}"
    );
    assert!(
        !err.contains("choices:"),
        "no choice is printed for a state it would refuse: {err}"
    );
    assert!(err.contains("rwv explain push-branch"), "{err}");
    assert_eq!(weave.git_project(&["rev-parse", "HEAD"]), mine);
    assert_eq!(
        weave.git_member(&["rev-parse", "HEAD"]),
        member_before,
        "the update step does not run behind a refused take-in"
    );
}

#[test]
fn pull_refuses_while_the_project_repo_has_uncommitted_changes_the_branch_would_cross() {
    let weave = weave();
    let before = weave.git_project(&["rev-parse", "HEAD"]);
    weave.forge_project_commit(Some(weave.manifest_with_role("dependency")), None);
    let mut manifest = std::fs::read_to_string(weave.project().join("rwv.toml")).unwrap();
    manifest.push_str("# local note\n");
    std::fs::write(weave.project().join("rwv.toml"), manifest).unwrap();

    let out = weave.rwv(&["pull"]);

    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains(&format!("projects/{PROJECT}: edits dirty")),
        "{}",
        stderr(&out)
    );
    assert_eq!(weave.git_project(&["rev-parse", "HEAD"]), before);
}

#[test]
fn pull_with_nothing_to_take_in_still_updates_the_members() {
    let weave = weave();
    std::fs::write(weave.project().join("local.txt"), "mine\n").unwrap();
    weave.git_project(&["add", "local.txt"]);
    weave.git_project(&["commit", "-m", "mine"]);
    let tip = weave.git_project(&["rev-parse", "HEAD"]);
    weave.git_member(&["switch", "--detach", "HEAD"]);

    let out = weave.rwv(&["pull"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("is ahead of its upstream, so nothing to take in"),
        "{}",
        stdout(&out)
    );
    assert_eq!(weave.git_project(&["rev-parse", "HEAD"]), tip);
    assert!(stdout(&out).contains("rwv update:"), "{}", stdout(&out));
}

#[test]
fn pull_stops_before_taking_anything_in_when_a_fetch_could_not_complete() {
    let weave = weave();
    let before = weave.git_project(&["rev-parse", "HEAD"]);
    weave.forge_project_commit(None, None);
    std::fs::rename(&weave.member_origin, weave.parent.join("server.git.away")).unwrap();

    let out = weave.rwv(&["pull"]);

    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("nothing was taken in"),
        "{}",
        stderr(&out)
    );
    assert_eq!(weave.git_project(&["rev-parse", "HEAD"]), before);
    let _ = &weave.project_origin;
}
