//! The deletion rule, driven through the shipped binary against weaves with
//! bare remotes: `rwv branch delete`, `rwv workweave delete` and
//! `rwv sync-to --retire`.
//!
//! What must hold, each pinned below by the test that names it:
//!
//! - a branch that landed is deleted, with its record;
//! - a branch that has not landed survives, the exit status is 3, and the
//!   `--json` outcome names it and why;
//! - `--discard-unlanded` keeps a backup first, then deletes;
//! - `--delete-remote` deletes only push branches that landed, and
//!   `--delete-remote-unlanded <repo>` deletes only the named repo's;
//! - a branch a live weave branch declares never belongs;
//! - a commit a live lock pins, that only the branch reaches, keeps the
//!   branch whatever `--discard-unlanded` says;
//! - no receipts are minted, and a recreated feature branch draws no finding.

use repoweave::diagnosis::State;
use repoweave::refusal::RefusalKind;
use std::path::{Path, PathBuf};

mod common;

const PROJECT: &str = "p";
const LIB: &str = "github/o/lib";
const APP: &str = "github/o/app";

fn weave(members: &[&str]) -> common::PushWorkspace {
    let roles: Vec<(&str, &str)> = members.iter().map(|m| (*m, "owned")).collect();
    common::build_workspace(PROJECT, &roles)
}

fn project_dir(ws: &Path) -> PathBuf {
    ws.join("projects").join(PROJECT)
}

fn run(args: &[&str], dir: &Path) -> std::process::Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn describe(out: &std::process::Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn succeed(args: &[&str], dir: &Path) -> std::process::Output {
    let out = run(args, dir);
    assert!(
        out.status.success(),
        "`rwv {}` failed; {}",
        args.join(" "),
        describe(&out)
    );
    out
}

/// Status 1, ending in the route lines, `route` among them.
fn ends_in_route(out: &std::process::Output, route: &str) {
    assert_eq!(out.status.code(), Some(1), "{}", describe(out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let routes: Vec<&str> = stderr
        .lines()
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take_while(|l| l.starts_with("rwv explain "))
        .collect();
    assert!(
        routes.contains(&format!("rwv explain {route}").as_str()),
        "{}",
        describe(out)
    );
}

/// A refusal judging each repo in its state, each state's axis routed.
fn refused_judging(out: &std::process::Output, judged: &[(&str, State)]) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    for (repo, state) in judged {
        let line = format!("{repo}: {} {}", state.axis().token(), state.name());
        assert!(
            stderr.lines().any(|l| l.ends_with(&line)),
            "no line ends {line:?}: {}",
            describe(out)
        );
        ends_in_route(out, &state.axis().token());
    }
}

fn refused_as(out: &std::process::Output, kind: RefusalKind) {
    ends_in_route(out, &kind.token());
}

fn minted(name: &str) -> String {
    format!("{PROJECT}--{name}")
}

fn create_workweave(ws: &Path, name: &str) -> PathBuf {
    succeed(&["workweave", "create", "--project", PROJECT, name], ws);
    ws.parent().unwrap().join(".workweaves").join(minted(name))
}

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    common::git_in(dir, &["config", "user.email", "test@test.com"]);
    common::git_in(dir, &["config", "user.name", "Test"]);
    common::git_in(dir, &["add", "."]);
    common::git_in(dir, &["commit", "-qm", file]);
    common::git_in(dir, &["rev-parse", "HEAD"])
}

fn has_branch(repo: &Path, name: &str) -> bool {
    common::git()
        .args(["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")])
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success()
}

fn bare_has(bare: &Path, branch: &str) -> bool {
    has_branch(bare, branch)
}

fn bare_of<'a>(w: &'a common::PushWorkspace, member: &str) -> &'a Path {
    &w.manifest_bares
        .iter()
        .find(|(path, _)| path == member)
        .unwrap()
        .1
}

fn recorded(ws: &Path, branch: &str) -> bool {
    common::git()
        .args(["cat-file", "-e", &format!("rwv-records:{}", common::records::path(branch))])
        .current_dir(project_dir(ws))
        .output()
        .unwrap()
        .status
        .success()
}

fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("not JSON ({e}); {}", describe(out)))
}

fn survivor<'a>(doc: &'a serde_json::Value, repo: &str) -> &'a serde_json::Value {
    doc["survivors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["path"] == repo)
        .unwrap_or_else(|| panic!("no survivor for {repo} in {doc}"))
}

fn backups_of(ws: &Path, repo: &str) -> Vec<String> {
    let out = succeed(&["backup", "list", "--json"], ws);
    let doc = json(&out);
    doc["repos"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["path"] == repo)
        .flat_map(|r| r["backups"].as_array().unwrap().iter())
        .map(|b| b["tip"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_workweave_whose_branches_landed_is_deleted_whole_with_its_record() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "feat");
    let branch = minted("feat");
    assert!(recorded(&w.workspace, &branch));

    let out = succeed(&["workweave", "delete", "feat"], &w.workspace);

    assert!(!ww.exists());
    assert!(!has_branch(&w.workspace.join(LIB), &branch));
    assert!(!has_branch(&project_dir(&w.workspace), &branch));
    assert!(
        !recorded(&w.workspace, &branch),
        "the record goes by the drop rule; {}",
        describe(&out)
    );
}

/// A branch with no upstream has no settings to remove with it, and whether
/// it had any is read from the repository, so an operator whose git speaks
/// another language sees the same delete.
#[test]
fn a_landed_branch_is_deleted_cleanly_whatever_language_git_speaks() {
    let w = weave(&[LIB]);
    create_workweave(&w.workspace, "feat");
    let branch = minted("feat");

    let out = common::rwv()
        .args(["workweave", "delete", "feat"])
        .env("LANGUAGE", "de")
        .env("LC_MESSAGES", "en_US.UTF-8")
        .current_dir(&w.workspace)
        .output()
        .unwrap();

    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("could not delete"),
        "{}",
        describe(&out)
    );
    assert!(!has_branch(&w.workspace.join(LIB), &branch));
    assert!(!has_branch(&project_dir(&w.workspace), &branch));
}

#[test]
fn an_unlanded_branch_survives_with_status_3_and_a_json_outcome_naming_it() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "feat");
    let branch = minted("feat");
    let work = commit_in(&ww.join(LIB), "unlanded.txt");

    let out = run(&["workweave", "delete", "feat", "--json"], &w.workspace);

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert!(!ww.exists(), "the workweave itself is removed");
    let doc = json(&out);
    let lib = survivor(&doc, LIB);
    assert_eq!(lib["branch"], branch);
    assert_eq!(lib["reason"], "unlanded");
    assert!(
        lib["detail"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("unlanded.txt")),
        "the unlanded commits are listed: {lib}"
    );
    let project = survivor(&doc, "projects/p");
    assert_eq!(
        project["reason"], "members",
        "a project branch whose member branch survives survives with it"
    );
    assert_eq!(
        common::git_in(w.workspace.join(LIB), &["rev-parse", &branch]),
        work,
        "the unlanded commit is still on its branch"
    );
    assert!(
        recorded(&w.workspace, &branch),
        "the record goes only with the branch"
    );
}

#[test]
fn the_text_report_names_what_was_left_and_why() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "feat");
    commit_in(&ww.join(LIB), "unlanded.txt");

    let out = run(&["workweave", "delete", "feat"], &w.workspace);

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("left: github/o/lib p--feat: unlanded"),
        "{}",
        describe(&out)
    );
}

#[test]
fn discard_unlanded_keeps_a_backup_first_and_deletes_the_branch() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "feat");
    let branch = minted("feat");
    let work = commit_in(&ww.join(LIB), "unlanded.txt");

    let out = run(
        &[
            "workweave",
            "delete",
            "feat",
            "--discard-unlanded",
            "--json",
        ],
        &w.workspace,
    );

    assert!(out.status.success(), "{}", describe(&out));
    assert!(!has_branch(&w.workspace.join(LIB), &branch));
    assert_eq!(
        backups_of(&w.workspace, LIB),
        vec![work.clone()],
        "the tip is kept as a backup"
    );
    let doc = json(&out);
    let discarded = &doc["discarded"]["unlanded_branches"];
    assert_eq!(discarded[0]["path"], LIB);
    assert_eq!(discarded[0]["branch"], branch);
    assert!(doc["survivors"].as_array().unwrap().is_empty());
    assert!(!recorded(&w.workspace, &branch));
}

#[test]
fn a_report_by_kind_lists_edits_and_detached_commits_the_discard_flags_kept() {
    let w = weave(&[LIB, APP]);
    let ww = create_workweave(&w.workspace, "feat");
    std::fs::write(ww.join(LIB).join("edit.txt"), "uncommitted").unwrap();
    common::git_in(ww.join(APP), &["config", "user.email", "t@t"]);
    common::git_in(ww.join(APP), &["config", "user.name", "T"]);
    common::git_in(ww.join(APP), &["checkout", "-q", "--detach"]);
    commit_in(&ww.join(APP), "detached.txt");

    let refused = run(&["workweave", "delete", "feat"], &w.workspace);
    refused_judging(
        &refused,
        &[
            (LIB, State::EditsDirty),
            (APP, State::AlignmentDetachedUnpushed),
        ],
    );
    assert!(ww.exists(), "a refusal removes nothing");

    let out = run(
        &[
            "workweave",
            "delete",
            "feat",
            "--discard-uncommitted",
            "--discard-unlanded",
            "--json",
        ],
        &w.workspace,
    );
    assert!(out.status.success(), "{}", describe(&out));
    let doc = json(&out);
    assert_eq!(doc["discarded"]["edits"][0]["path"], LIB);
    assert_eq!(doc["discarded"]["detached_commits"][0]["path"], APP);
    assert!(!ww.exists());
}

#[test]
fn delete_remote_deletes_the_landed_push_branches_and_leaves_an_unlanded_one() {
    let w = weave(&[LIB, APP]);
    let ww = create_workweave(&w.workspace, "feat");
    let branch = minted("feat");
    commit_in(&ww.join(LIB), "unlanded.txt");
    common::git_in(ww.join(LIB), &["push", "-q", "origin", &branch]);
    common::git_in(ww.join(APP), &["push", "-q", "origin", &branch]);
    common::git_in(project_dir(&w.workspace), &["push", "-q", "origin", "main"]);
    common::git_in(
        project_dir(&w.workspace),
        &["push", "-q", "origin", &branch],
    );
    let lib_bare = bare_of(&w, LIB);
    let app_bare = bare_of(&w, APP);
    assert!(bare_has(lib_bare, &branch) && bare_has(app_bare, &branch));

    let out = run(
        &[
            "workweave",
            "delete",
            "feat",
            "--discard-unlanded",
            "--delete-remote",
            "--json",
        ],
        &w.workspace,
    );

    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        !bare_has(app_bare, &branch),
        "a push branch that landed is deleted"
    );
    assert!(
        !bare_has(&w.project_bare, &branch),
        "the project's push branch landed too; {}",
        describe(&out)
    );
    assert!(
        bare_has(lib_bare, &branch),
        "an unlanded push branch is not deleted by --delete-remote, whatever \
         --discard-unlanded did to the local branch"
    );
    assert!(!has_branch(&w.workspace.join(LIB), &branch));
}

#[test]
fn delete_remote_unlanded_deletes_only_the_repo_it_names_after_a_backup() {
    let w = weave(&[LIB, APP]);
    let ww = create_workweave(&w.workspace, "feat");
    let branch = minted("feat");
    let lib_work = commit_in(&ww.join(LIB), "lib.txt");
    let app_work = commit_in(&ww.join(APP), "app.txt");
    common::git_in(ww.join(LIB), &["push", "-q", "origin", &branch]);
    common::git_in(ww.join(APP), &["push", "-q", "origin", &branch]);
    let lib_bare = bare_of(&w, LIB);
    let app_bare = bare_of(&w, APP);

    let out = run(
        &[
            "workweave",
            "delete",
            "feat",
            "--discard-unlanded",
            "--delete-remote-unlanded",
            LIB,
        ],
        &w.workspace,
    );

    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        !bare_has(lib_bare, &branch),
        "the named repo's unlanded push branch is deleted"
    );
    assert!(
        bare_has(app_bare, &branch),
        "an unlanded push branch no flag names is kept"
    );
    assert!(backups_of(&w.workspace, LIB).contains(&lib_work));
    assert!(backups_of(&w.workspace, APP).contains(&app_work));
}

#[test]
fn delete_remote_unlanded_without_discard_unlanded_is_a_usage_error() {
    let w = weave(&[LIB]);
    create_workweave(&w.workspace, "feat");

    let out = run(
        &[
            "workweave",
            "delete",
            "feat",
            "--delete-remote-unlanded",
            LIB,
        ],
        &w.workspace,
    );

    assert_eq!(out.status.code(), Some(2), "{}", describe(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("required arguments were not provided")
            && stderr.contains("--discard-unlanded"),
        "{}",
        describe(&out)
    );
}

#[test]
fn delete_remote_unlanded_naming_a_repo_the_branch_is_not_in_is_refused() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "feat");

    let out = run(
        &[
            "workweave",
            "delete",
            "feat",
            "--discard-unlanded",
            "--delete-remote-unlanded",
            "github/o/elsewhere",
        ],
        &w.workspace,
    );

    refused_as(&out, RefusalKind::InapplicableFlag);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("names no repo that `p--feat` has a branch in"),
        "{}",
        describe(&out)
    );
    assert!(ww.exists(), "a refusal removes nothing");
}

fn feature_branch(w: &common::PushWorkspace, name: &str, members: &[&str]) {
    for member in members {
        common::git_in(w.workspace.join(member), &["branch", name, "main"]);
    }
    common::git_in(project_dir(&w.workspace), &["branch", name, "main"]);
    common::record_feature(&project_dir(&w.workspace), name, "main");
}

#[test]
fn branch_delete_deletes_what_landed_in_every_repo_and_drops_the_record() {
    let w = weave(&[LIB]);
    feature_branch(&w, "feat", &[LIB]);

    let out = run(&["branch", "delete", "feat", "--json"], &w.workspace);

    assert!(out.status.success(), "{}", describe(&out));
    let doc = json(&out);
    assert_eq!(doc["deleted"].as_array().unwrap().len(), 2);
    assert!(!has_branch(&w.workspace.join(LIB), "feat"));
    assert!(!has_branch(&project_dir(&w.workspace), "feat"));
    assert!(!recorded(&w.workspace, "feat"));
}

#[test]
fn branch_delete_keeps_an_unlanded_branch_unless_discard_unlanded_is_given() {
    let w = weave(&[LIB]);
    feature_branch(&w, "feat", &[LIB]);
    let wt = w._tmp.path().join("scratch");
    common::git_in(
        w.workspace.join(LIB),
        &["worktree", "add", "-q", wt.to_str().unwrap(), "feat"],
    );
    let work = commit_in(&wt, "unlanded.txt");
    common::git_in(
        w.workspace.join(LIB),
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );

    let out = run(&["branch", "delete", "feat", "--json"], &w.workspace);
    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    let doc = json(&out);
    assert_eq!(survivor(&doc, LIB)["reason"], "unlanded");
    assert!(has_branch(&w.workspace.join(LIB), "feat"));
    assert!(
        recorded(&w.workspace, "feat"),
        "the record stays with the project branch the members hold"
    );

    let out = run(
        &["branch", "delete", "feat", "--discard-unlanded"],
        &w.workspace,
    );
    assert!(out.status.success(), "{}", describe(&out));
    assert!(!has_branch(&w.workspace.join(LIB), "feat"));
    assert_eq!(backups_of(&w.workspace, LIB), vec![work]);
}

#[test]
fn branch_delete_of_a_declared_branch_needs_the_branch_its_commits_landed_on() {
    let w = weave(&[LIB]);

    let out = run(&["branch", "delete", "main"], &w.workspace);

    assert!(!out.status.success(), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("rwv explain target"),
        "{}",
        describe(&out)
    );
}

#[test]
fn branch_delete_refuses_a_branch_checked_out_in_a_workweave() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "feat");

    let out = run(&["branch", "delete", &minted("feat")], &w.workspace);

    assert!(!out.status.success(), "{}", describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("rwv explain checkout"),
        "{}",
        describe(&out)
    );
    assert!(ww.exists());
    assert!(has_branch(&w.workspace.join(LIB), &minted("feat")));
}

#[test]
fn branch_delete_refuses_a_branch_other_records_land_into() {
    let w = weave(&[LIB]);
    feature_branch(&w, "feat", &[LIB]);
    common::git_in(project_dir(&w.workspace), &["branch", "child", "main"]);
    common::record_feature(&project_dir(&w.workspace), "child", "feat");

    let out = run(&["branch", "delete", "feat"], &w.workspace);

    assert!(!out.status.success(), "{}", describe(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("child"), "{stderr}");
    assert!(stderr.contains("rwv explain target"), "{stderr}");
    assert!(has_branch(&project_dir(&w.workspace), "feat"));
}

#[test]
fn a_branch_a_live_weave_branch_declares_never_belongs() {
    let w = weave(&[LIB]);
    feature_branch(&w, "feat", &[LIB]);
    let project = project_dir(&w.workspace);
    common::git_in(&project, &["switch", "-q", "-c", "line"]);
    let manifest = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        manifest.replace("branch = \"main\"", "branch = \"feat\""),
    )
    .unwrap();
    common::git_in(&project, &["commit", "-qam", "line declares feat"]);
    common::git_in(&project, &["switch", "-q", "main"]);
    common::git_in(&project, &["config", "user.email", "t@t"]);

    let out = run(&["branch", "delete", "feat", "--json"], &w.workspace);

    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        has_branch(&w.workspace.join(LIB), "feat"),
        "lib's `feat` is the branch `line` declares for it: no cleanup touches it"
    );
    assert!(!has_branch(&project, "feat"));
}

#[test]
fn a_commit_a_live_lock_pins_that_only_the_branch_reaches_keeps_the_branch() {
    let w = weave(&[LIB]);
    feature_branch(&w, "feat", &[LIB]);
    let wt = w._tmp.path().join("scratch");
    common::git_in(
        w.workspace.join(LIB),
        &["worktree", "add", "-q", wt.to_str().unwrap(), "feat"],
    );
    let pinned = commit_in(&wt, "pinned.txt");
    common::git_in(
        w.workspace.join(LIB),
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    let project = project_dir(&w.workspace);
    common::git_in(&project, &["switch", "-q", "-c", "other"]);
    let url = common::file_url(bare_of(&w, LIB));
    common::fixture_lock(&project, &[(LIB, &url, &pinned)]);
    common::git_in(&project, &["config", "user.email", "t@t"]);
    common::git_in(&project, &["commit", "-qam", "other pins the work"]);
    common::git_in(&project, &["switch", "-q", "main"]);

    let out = run(
        &["branch", "delete", "feat", "--discard-unlanded", "--json"],
        &w.workspace,
    );

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    let doc = json(&out);
    assert_eq!(
        survivor(&doc, LIB)["reason"],
        "pinned",
        "--discard-unlanded lifts the landed clause and no other"
    );
    assert!(has_branch(&w.workspace.join(LIB), "feat"));
}

#[test]
fn a_published_project_branch_whose_push_branch_has_not_landed_is_an_open_head() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "feat");
    let branch = minted("feat");
    let project_wt = ww.join("projects").join(PROJECT);
    commit_in(&project_wt, "pr.txt");
    common::git_in(&project_wt, &["push", "-q", "origin", &branch]);
    common::git_in(&project_wt, &["reset", "-q", "--hard", "main"]);

    let out = run(&["workweave", "delete", "feat", "--json"], &w.workspace);

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    let doc = json(&out);
    assert_eq!(survivor(&doc, "projects/p")["reason"], "open-head");

    let out = run(
        &[
            "branch",
            "delete",
            &branch,
            "--discard-unlanded",
            "--delete-remote-unlanded",
            "projects/p",
        ],
        &w.workspace,
    );
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        !bare_has(&w.project_bare, &branch),
        "naming the repo deletes an open head; clause 3 is judged after it"
    );
    assert!(!has_branch(&project_dir(&w.workspace), &branch));
}

#[test]
fn no_receipts_are_minted() {
    let w = weave(&[LIB]);
    create_workweave(&w.workspace, "feat");
    let index = common::index_file_of(project_dir(&w.workspace));

    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index).unwrap()).unwrap();
    assert!(written.get("receipts").is_none(), "{written}");
    succeed(&["workweave", "delete", "feat"], &w.workspace);
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index).unwrap()).unwrap();
    assert!(written.get("receipts").is_none(), "{written}");
}

#[test]
fn a_recreated_feature_branch_draws_no_doctor_finding() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "feat");
    let branch = minted("feat");
    let before = common::doctor_findings(&ww);

    common::git_in(ww.join(LIB), &["checkout", "-q", "--detach"]);
    common::git_in(w.workspace.join(LIB), &["branch", "-D", &branch]);
    succeed(&["update"], &ww);

    assert!(
        has_branch(&w.workspace.join(LIB), &branch),
        "update recreates the feature branch at its pin"
    );
    common::assert_no_new_findings(&ww, &before);
    let out = run(&["doctor", "--json", "--all"], &ww);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("unrecorded-ephemeral-branch") && !text.contains("branch-discipline"),
        "{text}"
    );
}

#[test]
fn sync_to_retire_deletes_what_landed_and_keeps_what_the_record_does_not_see_landed() {
    let w = weave(&[LIB]);
    let project = project_dir(&w.workspace);
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    common::git_in(&project, &["add", ".gitattributes"]);
    common::git_in(&project, &["commit", "-qm", "replay exclusion"]);
    let ww = create_workweave(&w.workspace, "feat");
    let branch = minted("feat");
    commit_in(&ww.join(LIB), "work.txt");

    let out = run(&["sync-to", "--retire"], &ww);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(!ww.exists());
    assert!(
        !has_branch(&w.workspace.join(LIB), &branch),
        "the landing put the work on main, so the branch landed"
    );

    let ww = create_workweave(&w.workspace, "later");
    let later = minted("later");
    common::git_in(w.workspace.join(LIB), &["branch", "elsewhere", "main"]);
    common::git_in(project_dir(&w.workspace), &["branch", "elsewhere", "main"]);
    succeed(
        &["branch", "set-mode", "elsewhere", "declared"],
        &w.workspace,
    );
    succeed(
        &[
            "branch",
            "set-mode",
            &later,
            "feature",
            "--target",
            "elsewhere",
        ],
        &ww,
    );
    commit_in(&ww.join(LIB), "more.txt");

    let out = run(&["sync-to", "--retire", "--json"], &ww);

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    let doc = json(&out);
    assert_eq!(doc["retired"], true);
    assert_eq!(
        survivor(&doc, "projects/p")["reason"],
        "unlanded",
        "judged against the record's target, not against where the work went"
    );
    assert!(
        doc["survivors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["path"] != LIB),
        "the member's work reached the branch the target declares for it: {doc}"
    );
    assert!(!ww.exists());
}

fn work_on(w: &common::PushWorkspace, member: &str, branch: &str, file: &str) -> String {
    let wt = w._tmp.path().join(format!("scratch-{file}"));
    common::git_in(
        w.workspace.join(member),
        &["worktree", "add", "-q", wt.to_str().unwrap(), branch],
    );
    let work = commit_in(&wt, file);
    common::git_in(
        w.workspace.join(member),
        &["worktree", "remove", "--force", wt.to_str().unwrap()],
    );
    work
}

/// `main` once declared `lib -> next`; a later feature named `next` gives lib
/// a branch of that name. Its unlanded work is not "landed" on itself.
#[test]
fn a_branch_is_never_proved_landed_on_itself() {
    let w = weave(&[LIB]);
    let project = project_dir(&w.workspace);
    let manifest = std::fs::read_to_string(project.join("rwv.toml")).unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        manifest.replace("branch = \"main\"", "branch = \"next\""),
    )
    .unwrap();
    common::git_in(&project, &["commit", "-qam", "declare next"]);
    std::fs::write(project.join("rwv.toml"), &manifest).unwrap();
    common::git_in(&project, &["commit", "-qam", "declare main again"]);
    feature_branch(&w, "next", &[LIB]);
    let work = work_on(&w, LIB, "next", "unlanded.txt");

    let out = run(&["branch", "delete", "next", "--json"], &w.workspace);

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert_eq!(survivor(&json(&out), LIB)["reason"], "unlanded");
    assert_eq!(
        common::git_in(w.workspace.join(LIB), &["rev-parse", "next"]),
        work
    );
}

/// `--delete-remote-unlanded` is under the pin rule: a published lock pinning
/// a commit only the push branch holds keeps it.
#[test]
fn delete_remote_unlanded_keeps_a_push_branch_a_published_lock_needs() {
    let w = weave(&[LIB]);
    feature_branch(&w, "feat", &[LIB]);
    let work = work_on(&w, LIB, "feat", "pinned.txt");
    common::git_in(w.workspace.join(LIB), &["push", "-q", "origin", "feat"]);
    let project = project_dir(&w.workspace);
    common::git_in(&project, &["switch", "-q", "-c", "other"]);
    let url = common::file_url(bare_of(&w, LIB));
    common::fixture_lock(&project, &[(LIB, &url, &work)]);
    common::git_in(&project, &["commit", "-qam", "other pins the work"]);
    common::git_in(&project, &["push", "-q", "origin", "other"]);
    common::git_in(&project, &["switch", "-q", "main"]);

    let out = run(
        &[
            "branch",
            "delete",
            "feat",
            "--discard-unlanded",
            "--delete-remote-unlanded",
            LIB,
            "--json",
        ],
        &w.workspace,
    );

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    assert!(
        bare_has(bare_of(&w, LIB), "feat"),
        "origin/other's lock pins a commit only the push branch holds"
    );
    assert_eq!(survivor(&json(&out), LIB)["reason"], "pinned");
}

/// A remote that refuses a delete keeps its branch, reported, and the rule
/// goes on: what it already deleted is still listed.
#[test]
fn a_remote_that_refuses_the_delete_keeps_the_branch_and_the_rest_is_reported() {
    let w = weave(&[LIB, APP]);
    feature_branch(&w, "feat", &[LIB, APP]);
    common::git_in(w.workspace.join(LIB), &["push", "-q", "origin", "feat"]);
    common::git_in(w.workspace.join(APP), &["push", "-q", "origin", "feat"]);
    common::refuse_pushes_to(bare_of(&w, LIB), "refs/heads/feat");

    let out = run(
        &["branch", "delete", "feat", "--delete-remote", "--json"],
        &w.workspace,
    );

    assert_eq!(out.status.code(), Some(3), "{}", describe(&out));
    let doc = json(&out);
    assert_eq!(survivor(&doc, LIB)["reason"], "unconfirmed");
    assert!(
        doc["deleted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["path"] == APP && d["site"] == "remote origin"),
        "{doc}"
    );
    assert!(!bare_has(bare_of(&w, APP), "feat"));
    assert!(bare_has(bare_of(&w, LIB), "feat"));
}

/// "Landed on `T`'s remote-tracking branch" reads `T`'s upstream: a
/// colleague's remote whose `main` holds the work does not make it landed.
#[test]
fn delete_remote_reads_only_the_targets_upstream() {
    let w = weave(&[LIB]);
    feature_branch(&w, "feat", &[LIB]);
    let work = work_on(&w, LIB, "feat", "pr.txt");
    let lib = w.workspace.join(LIB);
    common::git_in(&lib, &["push", "-q", "origin", "feat"]);
    let colleague = w._tmp.path().join("colleague.git");
    common::git_in(
        w._tmp.path(),
        &["init", "-q", "--bare", colleague.to_str().unwrap()],
    );
    common::git_in(
        &lib,
        &["remote", "add", "colleague", colleague.to_str().unwrap()],
    );
    common::git_in(
        &lib,
        &[
            "push",
            "-q",
            "colleague",
            &format!("{work}:refs/heads/main"),
        ],
    );
    common::git_in(&lib, &["fetch", "-q", "colleague"]);
    common::git_in(&lib, &["config", "remote.pushDefault", "origin"]);

    let out = run(
        &[
            "branch",
            "delete",
            "feat",
            "--discard-unlanded",
            "--delete-remote",
            "--json",
        ],
        &w.workspace,
    );

    assert!(
        bare_has(bare_of(&w, LIB), "feat"),
        "origin/feat has not landed on origin/main; {}",
        describe(&out)
    );
}

/// A branch deleted locally and on its remote after one backup is reported
/// as discarded once.
#[test]
fn a_branch_deleted_here_and_on_its_remote_is_reported_discarded_once() {
    let w = weave(&[LIB]);
    let ww = create_workweave(&w.workspace, "feat");
    let branch = minted("feat");
    commit_in(&ww.join(LIB), "lib.txt");
    common::git_in(ww.join(LIB), &["push", "-q", "origin", &branch]);

    let out = run(
        &[
            "workweave",
            "delete",
            "feat",
            "--discard-unlanded",
            "--delete-remote-unlanded",
            LIB,
        ],
        &w.workspace,
    );

    assert!(out.status.success(), "{}", describe(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let discarded = format!("discarded unlanded branch: {LIB} {branch}");
    assert_eq!(stderr.matches(&discarded).count(), 1, "{stderr}");
    assert!(!bare_has(bare_of(&w, LIB), &branch));
}
