//! A remote is addressed by the role it plays, never by a name rwv chose.
//!
//! Each prohibition is pinned from the side that would break it: a member's
//! url remote is found by its url and added before the fetch, a name another
//! url holds refuses, a branch publishes where git's order sends it, the
//! project repo publishes to the remote it was created with, and a branch
//! whose upstream is gone is marked only while it holds commits no remote has.

use assert_cmd::Command;
use std::path::{Path, PathBuf};

mod common;

const MEMBER: &str = "local/org/a";

fn rwv() -> Command {
    common::rwv()
}

struct Weave {
    ws: common::PushWorkspace,
}

impl Weave {
    fn new() -> Weave {
        Weave {
            ws: common::build_workspace("alpha", &[(MEMBER, "owned")]),
        }
    }

    fn root(&self) -> &Path {
        &self.ws.workspace
    }

    fn member(&self) -> PathBuf {
        self.ws.workspace.join(MEMBER)
    }

    fn project(&self) -> PathBuf {
        self.ws.workspace.join("projects").join("alpha")
    }

    fn bare(&self) -> &Path {
        &self.ws.manifest_bares[0].1
    }

    fn scratch(&self, name: &str) -> PathBuf {
        self.ws.workspace.parent().unwrap().join(name)
    }

    fn rwv(&self, args: &[&str]) -> std::process::Output {
        rwv()
            .args(args)
            .current_dir(self.root())
            .output()
            .expect("rwv runs")
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn init_bare(path: &Path) {
    common::git_in(
        path.parent().unwrap(),
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            path.to_str().unwrap(),
        ],
    );
}

/// A commit on `branch` of `bare`, pushed through a scratch clone, and its id.
fn advance(bare: &Path, branch: &str, file: &str) -> String {
    let scratch = common::tempdir().unwrap();
    let work = scratch.path().join("work");
    common::git_in(
        scratch.path(),
        &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
    );
    common::git_in(&work, &["config", "user.email", "t@t"]);
    common::git_in(&work, &["config", "user.name", "T"]);
    common::git_in(&work, &["checkout", "-B", branch]);
    std::fs::write(work.join(file), file).unwrap();
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", file]);
    common::git_in(&work, &["push", "origin", branch]);
    common::git_in(&work, &["rev-parse", "HEAD"])
}

fn commit(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    common::git_in(dir, &["add", "."]);
    common::git_in(dir, &["commit", "-m", file]);
    common::git_in(dir, &["rev-parse", "HEAD"])
}

/// The remotes of the clone at `dir` with their urls, in name order.
fn remotes(dir: &Path) -> Vec<(String, String)> {
    let listing = common::git_in(dir, &["config", "--get-regexp", r"^remote\..*\.url$"]);
    let mut found: Vec<(String, String)> = listing
        .lines()
        .filter_map(|line| {
            let (key, url) = line.split_once(' ')?;
            let name = key.strip_prefix("remote")?.strip_prefix('.')?;
            Some((name.strip_suffix(".url")?.to_owned(), url.to_owned()))
        })
        .collect();
    found.sort();
    found
}

fn names(dir: &Path) -> Vec<String> {
    remotes(dir).into_iter().map(|(name, _)| name).collect()
}

fn ref_of(dir: &Path, name: &str) -> Option<String> {
    let out = common::git()
        .args(["rev-parse", "--verify", "--quiet", name])
        .current_dir(dir)
        .output()
        .unwrap();
    out.status
        .success()
        .then(|| text(&out.stdout).trim().to_owned())
}

fn bare_main(bare: &Path) -> String {
    common::git_in(bare, &["rev-parse", "main"])
}

/// Replace the member's `origin` by `name` at `url`, so the clone has a
/// remote and no remote with the url its manifest declares.
fn lose_the_url_remote(weave: &Weave, name: &str, url: &str) {
    common::git_in(weave.member(), &["remote", "remove", "origin"]);
    common::git_in(weave.member(), &["remote", "add", name, url]);
}

#[test]
fn fetch_adds_the_url_remote_a_clone_lacks_before_it_fetches() {
    let weave = Weave::new();
    let elsewhere = weave.scratch("elsewhere.git");
    init_bare(&elsewhere);
    lose_the_url_remote(&weave, "fork", &common::file_url(&elsewhere));
    let tip = advance(weave.bare(), "main", "news");

    let out = weave.rwv(&["fetch", "--repo", MEMBER]);

    assert!(out.status.success(), "{}", text(&out.stderr));
    let wanted = common::file_url(weave.bare());
    let added: Vec<String> = remotes(&weave.member())
        .into_iter()
        .filter(|(_, url)| *url == wanted)
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        added.len(),
        1,
        "the member's url gained one remote: {added:?}"
    );
    assert_ne!(added[0], "fork", "{added:?}");
    assert_eq!(
        ref_of(&weave.member(), &format!("refs/remotes/{}/main", added[0])).as_deref(),
        Some(tip.as_str()),
        "the remote was added before the fetch, so the fetch brought its branch"
    );
}

#[test]
fn update_remote_adds_the_url_remote_a_clone_lacks_before_it_advances_from_it() {
    let weave = Weave::new();
    let elsewhere = weave.scratch("elsewhere.git");
    init_bare(&elsewhere);
    lose_the_url_remote(&weave, "fork", &common::file_url(&elsewhere));
    let tip = advance(weave.bare(), "main", "news");

    let out = weave.rwv(&["update", "--remote", "--repo", MEMBER]);

    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        common::git_in(weave.member(), &["rev-parse", "HEAD"]),
        tip,
        "the member advanced from the remote that has its url"
    );
    let wanted = common::file_url(weave.bare());
    assert!(
        remotes(&weave.member())
            .iter()
            .any(|(_, url)| *url == wanted),
        "{:?}",
        remotes(&weave.member())
    );
}

#[test]
fn a_name_another_url_holds_refuses_the_fetch_before_it_adds_or_fetches_anything() {
    let weave = Weave::new();
    let manifest = weave.project().join("rwv.toml");
    let declared = std::fs::read_to_string(&manifest).unwrap();
    let bare_url = common::file_url(weave.bare());
    std::fs::write(
        &manifest,
        declared.replace(&bare_url, "https://example.com/acme/a.git"),
    )
    .unwrap();
    common::git_in(
        weave.member(),
        &[
            "remote",
            "add",
            "acme",
            "https://elsewhere.example/other/a.git",
        ],
    );
    let before = remotes(&weave.member());
    let tracked = ref_of(&weave.member(), "refs/remotes/origin/main");
    advance(weave.bare(), "main", "news");

    let out = weave.rwv(&["fetch", "--repo", MEMBER]);

    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("rwv fetch: local/org/a: name taken (remote acme)"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "acme holds https://elsewhere.example/other/a.git, not https://example.com/acme/a.git"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("choose where it pushes and pulls"),
        "{stderr}"
    );
    assert!(stderr.ends_with("rwv explain name\n"), "{stderr}");
    assert_eq!(remotes(&weave.member()), before, "no remote was added");
    assert_eq!(
        ref_of(&weave.member(), "refs/remotes/origin/main"),
        tracked,
        "nothing was fetched"
    );
}

#[test]
fn a_refused_name_reaches_json_as_the_name_axis_taken_with_its_choice() {
    let weave = Weave::new();
    let manifest = weave.project().join("rwv.toml");
    let declared = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        &manifest,
        declared.replace(
            &common::file_url(weave.bare()),
            "https://example.com/acme/a.git",
        ),
    )
    .unwrap();
    common::git_in(
        weave.member(),
        &[
            "remote",
            "add",
            "acme",
            "https://elsewhere.example/other/a.git",
        ],
    );

    let out = weave.rwv(&["fetch", "--repo", MEMBER, "--json"]);

    assert_eq!(out.status.code(), Some(1));
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("a refusal envelope");
    let line = &doc["refused"][0];
    assert_eq!(line["path"], MEMBER);
    assert_eq!(line["axis"], "name");
    assert_eq!(line["state"], "taken");
    assert_eq!(
        line["choices"][0]["phrase"],
        "choose where it pushes and pulls"
    );
}

#[test]
fn push_publishes_a_branch_to_its_push_remote_not_to_the_url_remote() {
    let weave = Weave::new();
    let fork = weave.scratch("fork.git");
    init_bare(&fork);
    common::git_in(
        weave.member(),
        &["remote", "add", "fork", &common::file_url(&fork)],
    );
    common::git_in(
        weave.member(),
        &["config", "branch.main.pushRemote", "fork"],
    );
    let origin_before = bare_main(weave.bare());
    let tip = commit(&weave.member(), "work");
    let locked = weave.rwv(&["lock", "--commit"]);
    assert!(locked.status.success(), "{}", text(&locked.stderr));

    let out = weave.rwv(&["push"]);

    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(bare_main(&fork), tip, "the push remote received the branch");
    assert_eq!(
        bare_main(weave.bare()),
        origin_before,
        "the remote the clone was made from did not"
    );
}

#[test]
fn the_project_repo_publishes_to_the_remote_it_was_created_with() {
    let weave = Weave::new();
    let other = weave.scratch("other.git");
    init_bare(&other);
    common::git_in(
        weave.project(),
        &["remote", "add", "other", &common::file_url(&other)],
    );
    common::git_in(weave.project(), &["config", "rwv.url-remote", "origin"]);
    let tip = commit(&weave.member(), "work");
    assert!(weave.rwv(&["lock", "--commit"]).status.success());
    let project_tip = common::git_in(weave.project(), &["rev-parse", "HEAD"]);

    let out = weave.rwv(&["push"]);

    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(bare_main(&weave.ws.project_bare), project_tip);
    assert_eq!(bare_main(weave.bare()), tip);
    assert!(
        ref_of(&other, "refs/heads/main").is_none(),
        "the project repo's other remote received nothing"
    );
}

#[test]
fn a_project_branch_with_no_upstream_and_several_remotes_none_recorded_has_no_push_target() {
    let weave = Weave::new();
    let other = weave.scratch("other.git");
    init_bare(&other);
    common::git_in(
        weave.project(),
        &["remote", "add", "other", &common::file_url(&other)],
    );
    common::git_in(weave.project(), &["branch", "--unset-upstream"]);
    commit(&weave.member(), "work");
    assert!(weave.rwv(&["lock", "--commit"]).status.success());
    let member_before = bare_main(weave.bare());
    let project_before = bare_main(&weave.ws.project_bare);

    let out = weave.rwv(&["push"]);

    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("rwv push: projects/alpha: push-branch no push target (main)"),
        "{stderr}"
    );
    assert!(
        stderr.contains("choose where it pushes and pulls"),
        "{stderr}"
    );
    assert!(stderr.ends_with("rwv explain push-branch\n"), "{stderr}");
    assert_eq!(bare_main(weave.bare()), member_before, "nothing was pushed");
    assert_eq!(bare_main(&weave.ws.project_bare), project_before);
}

#[test]
fn a_branch_taking_in_from_a_branch_of_another_name_is_a_foreign_upstream_push_refuses() {
    let weave = Weave::new();
    let main = weave.member();
    common::git_in(&main, &["push", "origin", "main:release"]);
    common::git_in(&main, &["fetch", "origin"]);
    common::git_in(&main, &["branch", "--set-upstream-to", "origin/release"]);
    commit(&main, "work");
    assert!(weave.rwv(&["lock", "--commit"]).status.success());
    let before = bare_main(weave.bare());

    let out = weave.rwv(&["push"]);

    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("push-branch foreign upstream (main takes in from origin/release)"),
        "{stderr}"
    );
    assert_eq!(bare_main(weave.bare()), before);
}

/// A member whose branch fell behind its push branch, the remote having
/// moved and been fetched.
fn behind(weave: &Weave) -> String {
    let tip = advance(weave.bare(), "main", "theirs");
    common::git_in(weave.member(), &["fetch", "origin"]);
    tip
}

#[test]
fn push_skips_a_branch_behind_its_push_branch_and_names_the_way_to_take_it_in() {
    let weave = Weave::new();
    let theirs = behind(&weave);

    let out = weave.rwv(&["push"]);

    let stderr = text(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(
        stderr.contains("rwv push: local/org/a: push-branch behind"),
        "{stderr}"
    );
    assert!(
        stderr.contains("rwv update --remote --repo local/org/a"),
        "{stderr}"
    );
    assert_eq!(
        bare_main(weave.bare()),
        theirs,
        "the remote kept its commit"
    );
}

#[test]
fn push_refuses_a_diverged_branch_holding_commits_of_its_own_before_any_push() {
    let weave = Weave::new();
    behind(&weave);
    commit(&weave.member(), "mine");
    assert!(weave.rwv(&["lock", "--commit"]).status.success());
    let project_before = bare_main(&weave.ws.project_bare);

    let out = weave.rwv(&["push"]);

    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("rwv push: local/org/a: push-branch diverged"),
        "{stderr}"
    );
    assert!(
        stderr.contains("mine"),
        "the commits it holds are listed: {stderr}"
    );
    assert_eq!(
        bare_main(&weave.ws.project_bare),
        project_before,
        "the project repo was not reached"
    );
}

#[test]
fn status_reports_a_branch_behind_its_push_branch_with_the_choice_to_take_it_in() {
    let weave = Weave::new();
    behind(&weave);

    let out = weave.rwv(&["status"]);

    let stdout = text(&out.stdout);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(
        stdout.contains("rwv status: local/org/a: push-branch behind"),
        "{stdout}"
    );
    assert!(
        stdout.contains("rwv update --remote --repo local/org/a"),
        "{stdout}"
    );
}

/// A branch `feature` of the member, pushed with its upstream, whose commit
/// the remote's `main` also reaches, and whose upstream was then deleted and
/// pruned.
fn feature_whose_upstream_is_gone_with_nothing_unpushed(weave: &Weave) {
    let member = weave.member();
    common::git_in(&member, &["switch", "-c", "feature"]);
    commit(&member, "landed");
    common::git_in(&member, &["push", "-u", "origin", "feature"]);
    common::git_in(&member, &["push", "origin", "feature:main"]);
    common::git_in(weave.bare(), &["branch", "-D", "feature"]);
    assert!(weave.rwv(&["fetch", "--repo", MEMBER]).status.success());
    assert!(
        ref_of(&member, "refs/remotes/origin/feature").is_none(),
        "the prune removed the tracking branch"
    );
}

#[test]
fn a_gone_upstream_is_marked_only_while_the_branch_holds_unpushed_commits() {
    let weave = Weave::new();
    feature_whose_upstream_is_gone_with_nothing_unpushed(&weave);

    let quiet = weave.rwv(&["status"]);
    assert!(quiet.status.success(), "{}", text(&quiet.stderr));
    assert!(
        !text(&quiet.stdout).contains("push-branch"),
        "everything the branch holds is on the remote's main:\n{}",
        text(&quiet.stdout)
    );

    commit(&weave.member(), "mine alone");
    let marked = weave.rwv(&["status"]);
    let stdout = text(&marked.stdout);
    assert!(marked.status.success(), "{}", text(&marked.stderr));
    assert!(
        stdout.contains("rwv status: local/org/a: push-branch gone"),
        "{stdout}"
    );
    assert!(
        stdout.contains("mine alone"),
        "its commits are listed: {stdout}"
    );
    assert!(
        stdout.contains("copy these commits onto"),
        "and so is the choice to keep them: {stdout}"
    );
}

#[test]
fn push_leaves_a_gone_branch_unpublished_unless_the_repo_is_named() {
    let weave = Weave::new();
    feature_whose_upstream_is_gone_with_nothing_unpushed(&weave);
    let project = weave.project();
    common::git_in(&project, &["switch", "-c", "feature"]);
    common::record_feature(&project, "feature", "main");
    let head = commit(&weave.member(), "mine alone");
    common::fixture_lock(
        &project,
        &[(MEMBER, &common::file_url(weave.bare()), &head)],
    );
    common::git_in(&project, &["add", "rwv.lock"]);
    common::commit_staged(&project, "pin the member's commit");

    let skipped = weave.rwv(&["push"]);
    assert!(
        !skipped.status.success(),
        "the project branch waits for the pin the skipped branch holds"
    );
    assert!(
        text(&skipped.stderr).contains("local/org/a: push-branch gone")
            && text(&skipped.stderr).contains("local/org/a: publish withheld"),
        "{}",
        text(&skipped.stderr)
    );
    assert!(
        ref_of(weave.bare(), "refs/heads/feature").is_none(),
        "the branch the remote deleted stayed deleted"
    );

    let named = weave.rwv(&["push", "--repo", MEMBER]);
    assert!(named.status.success(), "{}", text(&named.stderr));
    assert!(
        ref_of(weave.bare(), "refs/heads/feature").is_some(),
        "naming the repo publishes the branch again"
    );
}

#[test]
fn init_with_a_provider_records_the_remote_it_adds_as_the_project_url_remote() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    let out = rwv()
        .args(["init", "web", "--provider", "github/acme"])
        .current_dir(&ws)
        .output()
        .unwrap();

    assert!(out.status.success(), "{}", text(&out.stderr));
    let project = ws.join("projects").join("web");
    assert_eq!(
        remotes(&project),
        [(
            "origin".to_owned(),
            "https://github.com/acme/web.git".to_owned()
        )]
    );
    assert_eq!(
        common::git_in(&project, &["config", "--get", "rwv.url-remote"]),
        "origin"
    );
}

/// A project source holding `rwv.toml` and a lock that pins the member at its
/// remote's tip, and its url.
fn project_source(tmp: &Path, member_bare: &Path) -> String {
    let bare = tmp.join("project.git");
    common::init_bare_repo_with_commit(&bare);
    let seed = tmp.join("project-seed");
    common::git_in(
        tmp,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    common::git_in(&seed, &["config", "user.email", "t@t"]);
    common::git_in(&seed, &["config", "user.name", "T"]);
    let member_url = common::file_url(member_bare);
    std::fs::write(
        seed.join("rwv.toml"),
        format!(
            "[repositories.\"{MEMBER}\"]\ntype = \"git\"\nurl = \"{member_url}\"\n\
             branch = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    common::fixture_lock_at_remote_tips(&seed, &[(MEMBER, member_url.as_str())]);
    common::set_up_lock_driver(&seed);
    common::git_in(&seed, &["add", "."]);
    common::git_in(&seed, &["commit", "-m", "project"]);
    common::git_in(&seed, &["push", "origin", "main"]);
    common::file_url(&bare)
}

#[test]
fn clone_records_the_remote_it_made_as_the_project_url_remote() {
    let tmp = common::tempdir().unwrap();
    let member = tmp.path().join("member.git");
    common::init_bare_repo_with_commit(&member);
    let source = project_source(tmp.path(), &member);
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let out = rwv()
        .args(["clone", &source])
        .current_dir(&ws)
        .output()
        .unwrap();

    assert!(out.status.success(), "{}", text(&out.stderr));
    let project = ws.join("projects").join("project");
    let made = names(&project);
    assert_eq!(made.len(), 1, "{made:?}");
    assert_eq!(
        common::git_in(&project, &["config", "--get", "rwv.url-remote"]),
        made[0]
    );
}

#[test]
fn every_verb_runs_where_the_operator_names_a_first_remote_something_else() {
    const CALLED: &str = "weavecheck";
    let called = |mut cmd: Command| {
        cmd.envs(common::git_config_env(&[(
            "clone.defaultRemoteName",
            CALLED,
        )]));
        cmd
    };
    let tmp = common::tempdir().unwrap();
    let member_bare = tmp.path().join("member.git");
    common::init_bare_repo_with_commit(&member_bare);
    let source = project_source(tmp.path(), &member_bare);
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let run = |args: &[&str]| {
        let out = called(rwv()).args(args).current_dir(&ws).output().unwrap();
        assert!(out.status.success(), "rwv {args:?}: {}", text(&out.stderr));
        text(&out.stdout) + &text(&out.stderr)
    };

    run(&["clone", &source]);
    let project = ws.join("projects").join("project");
    let member = ws.join(MEMBER);
    assert_eq!(names(&project), [CALLED], "git named the project's remote");
    assert_eq!(names(&member), [CALLED], "and the member's");

    let tip = advance(&member_bare, "main", "news");
    run(&["fetch"]);
    run(&["status"]);
    run(&["update"]);
    run(&["update", "--remote"]);
    assert_eq!(common::git_in(&member, &["rev-parse", "HEAD"]), tip);

    let work = commit(&member, "work");
    run(&["lock", "--commit"]);
    let all = run(&["push"]);
    assert_eq!(bare_main(&member_bare), work);
    assert!(
        !all.contains("origin"),
        "no message names a remote nobody resolved:\n{all}"
    );
    assert_eq!(names(&project), [CALLED]);
    assert_eq!(names(&member), [CALLED]);
}
