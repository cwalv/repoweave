//! Each member verb commits its own change and nothing else, and `member add`
//! of a registered member refuses a flag that would change nothing. On an
//! `rwv.toml` laid out and commented by hand, a verb's commit changes only the
//! lines of the entry or key it sets, adds or removes.

use std::path::PathBuf;

use crate::common;

use common::{git_in, rwv, PushWorkspace};

const APP: &str = "github/acme/app";
const LIB: &str = "github/acme/lib";

fn two_members() -> PushWorkspace {
    common::build_workspace("proj", &[(APP, "owned"), (LIB, "owned")])
}

fn project(w: &PushWorkspace) -> PathBuf {
    w.workspace.join("projects").join(&w.project_name)
}

fn run(w: &PushWorkspace, args: &[&str]) -> std::process::Output {
    rwv()
        .args(args)
        .current_dir(&w.workspace)
        .output()
        .expect("rwv runs")
}

fn succeeded(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "expected success:\n{text}");
    text
}

fn refused(out: &std::process::Output) -> String {
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "expected a refusal:\n{text}");
    text
}

fn commits(w: &PushWorkspace) -> usize {
    git_in(project(w), &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap()
}

fn manifest(w: &PushWorkspace) -> String {
    std::fs::read_to_string(project(w).join("rwv.toml")).unwrap()
}

fn committed_manifest(w: &PushWorkspace) -> String {
    git_in(project(w), &["show", "HEAD:rwv.toml"])
}

fn hand_edit_app_role(w: &PushWorkspace) {
    let edited = manifest(w).replace(
        &format!("branch = \"main\"\nrole = \"owned\"\n[repositories.\"{LIB}\"]"),
        &format!("branch = \"main\"\nrole = \"dependency\"\n[repositories.\"{LIB}\"]"),
    );
    assert_ne!(edited, manifest(w), "the hand edit applies");
    std::fs::write(project(w).join("rwv.toml"), edited).unwrap();
}

#[test]
fn set_role_commits_the_role_and_leaves_another_hand_edit_of_the_file_uncommitted() {
    let w = two_members();
    hand_edit_app_role(&w);
    let before = commits(&w);

    succeeded(&run(&w, &["member", "set-role", LIB, "fork"]));

    assert_eq!(commits(&w), before + 1);
    let committed = committed_manifest(&w);
    assert!(committed.contains("role = \"fork\""), "{committed}");
    assert!(
        !committed.contains("role = \"dependency\""),
        "the hand edit of app's role was committed:\n{committed}"
    );
    assert!(manifest(&w).contains("role = \"dependency\""));
    let status = git_in(project(&w), &["status", "--porcelain"]);
    assert!(
        status.contains("M rwv.toml"),
        "the hand edit stays as an uncommitted change:\n{status}"
    );
}

#[test]
fn set_recurse_submodules_commits_the_setting() {
    let w = two_members();
    let before = commits(&w);

    succeeded(&run(
        &w,
        &["member", "set-recurse-submodules", LIB, "false"],
    ));

    assert_eq!(commits(&w), before + 1);
    assert!(committed_manifest(&w).contains("recurse-submodules = false"));
    assert_eq!(git_in(project(&w), &["status", "--porcelain"]), "");
}

#[test]
fn remove_commits_the_removal_and_the_dropped_pin() {
    let w = two_members();
    let before = commits(&w);

    succeeded(&run(&w, &["member", "remove", LIB]));

    assert_eq!(commits(&w), before + 1);
    assert!(!committed_manifest(&w).contains(LIB));
    let lock = git_in(project(&w), &["show", "HEAD:rwv.lock"]);
    assert!(!lock.contains(LIB), "{lock}");
    assert!(lock.contains(APP), "{lock}");
    assert_eq!(git_in(project(&w), &["status", "--porcelain"]), "");
}

#[test]
fn a_member_verb_leaves_unrelated_project_edits_out_of_its_commit() {
    let w = two_members();
    std::fs::write(project(&w).join("README"), "edited\n").unwrap();
    git_in(project(&w), &["add", "README"]);
    git_in(project(&w), &["commit", "-qm", "readme"]);
    std::fs::write(project(&w).join("README"), "dirty\n").unwrap();

    for args in [
        &["member", "set-role", LIB, "fork"][..],
        &["member", "set-recurse-submodules", LIB, "false"][..],
        &["member", "remove", LIB][..],
    ] {
        let before = commits(&w);
        let text = succeeded(&run(&w, args));
        assert!(text.contains("README (edited)"), "{args:?}: {text}");
        assert_eq!(commits(&w), before + 1, "{args:?}");
        let committed = git_in(project(&w), &["show", "--name-only", "--format=", "HEAD"]);
        assert!(!committed.contains("README"), "{args:?}: {committed}");
        assert_eq!(
            std::fs::read_to_string(project(&w).join("README")).unwrap(),
            "dirty\n"
        );
    }
}

#[test]
fn add_of_a_registered_member_refuses_a_flag_that_disagrees_and_names_the_setter() {
    let w = two_members();
    let clone = w.workspace.join(LIB);
    let clone = clone.to_str().unwrap();
    let before = (commits(&w), manifest(&w));

    let text = refused(&run(
        &w,
        &["member", "add", "--adopt", clone, "--role", "fork"],
    ));
    assert!(text.contains("rwv member set-role"), "{text}");
    let text = refused(&run(
        &w,
        &["member", "add", "--adopt", clone, "--no-recurse-submodules"],
    ));
    assert!(text.contains("rwv member set-recurse-submodules"), "{text}");
    assert_eq!((commits(&w), manifest(&w)), before);
}

fn url_of(w: &PushWorkspace, repo: &str) -> String {
    w.manifest_bares
        .iter()
        .find(|(p, _)| p == repo)
        .map(|(_, bare)| common::file_url(bare))
        .unwrap()
}

/// `rwv.toml` rewritten as a hand edit leaves it: lib before app, app's keys
/// out of serializer order, comments throughout; committed.
fn hand_laid_out(w: &PushWorkspace) -> String {
    let text = format!(
        "# Members, kept by hand.\n[repositories]\n\n# the library first\n\
         [repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{lib}\" # origin\nbranch = \"main\"\n\
         role = \"owned\"\n\n[repositories.\"{APP}\"]\nrole = \"owned\"\ntype = \"git\"\n\
         url = \"{app}\"\nbranch = \"main\"\n# end\n",
        lib = url_of(w, LIB),
        app = url_of(w, APP),
    );
    std::fs::write(project(w).join("rwv.toml"), &text).unwrap();
    git_in(project(w), &["commit", "-qam", "hand: lay out rwv.toml"]);
    text
}

/// The lines the verb's commit removed from and added to `rwv.toml`.
fn changed_lines(w: &PushWorkspace) -> (Vec<String>, Vec<String>) {
    let diff = git_in(
        project(w),
        &["diff", "-U0", "HEAD~1", "HEAD", "--", "rwv.toml"],
    );
    let mut removed = Vec::new();
    let mut added = Vec::new();
    for line in diff.lines() {
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }
        if let Some(gone) = line.strip_prefix('-') {
            removed.push(gone.to_owned());
        } else if let Some(new) = line.strip_prefix('+') {
            added.push(new.to_owned());
        }
    }
    (removed, added)
}

fn lines(of: &[&str]) -> Vec<String> {
    of.iter().map(|l| (*l).to_owned()).collect()
}

fn verb_on_hand_laid_out(args: &[&str]) -> (PushWorkspace, Vec<String>, Vec<String>) {
    let w = two_members();
    hand_laid_out(&w);
    let before = commits(&w);
    succeeded(&run(&w, args));
    assert_eq!(commits(&w), before + 1, "{args:?} commits its change");
    assert_eq!(
        committed_manifest(&w),
        manifest(&w).trim_end(),
        "{args:?}: the working file is what was committed"
    );
    let (removed, added) = changed_lines(&w);
    (w, removed, added)
}

#[test]
fn set_role_on_a_hand_laid_out_manifest_changes_only_the_role_line() {
    let (_w, removed, added) = verb_on_hand_laid_out(&["member", "set-role", APP, "fork"]);
    assert_eq!(
        (removed, added),
        (lines(&["role = \"owned\""]), lines(&["role = \"fork\""]))
    );
}

#[test]
fn set_recurse_submodules_on_a_hand_laid_out_manifest_adds_only_its_key() {
    let (_w, removed, added) =
        verb_on_hand_laid_out(&["member", "set-recurse-submodules", LIB, "false"]);
    assert_eq!(
        (removed, added),
        (lines(&[]), lines(&["recurse-submodules = false"]))
    );
}

#[test]
fn set_history_on_a_hand_laid_out_manifest_adds_only_its_key() {
    let (w, removed, added) =
        verb_on_hand_laid_out(&["branch", "set-history", "merge", "--repo", APP]);
    assert_eq!(
        (removed, added),
        (lines(&[]), lines(&["history = \"merge\""]))
    );
    assert!(
        manifest(&w).contains("branch = \"main\"\nhistory = \"merge\"\n# end\n"),
        "app's keys stand out of serializer order, so the key goes at the end of its table:\n{}",
        manifest(&w)
    );

    let (w, removed, added) = verb_on_hand_laid_out(&["branch", "set-history", "linear"]);
    assert_eq!(
        (removed, added),
        (lines(&[]), lines(&["history = \"linear\"", ""]))
    );
    assert!(
        manifest(&w)
            .starts_with("history = \"linear\"\n\n# Members, kept by hand.\n[repositories]\n"),
        "{}",
        manifest(&w)
    );
}

#[test]
fn set_branch_on_a_hand_laid_out_manifest_changes_only_the_branch_line() {
    let w = two_members();
    hand_laid_out(&w);
    let lib = w.workspace.join(LIB);
    git_in(&lib, &["switch", "-q", "-c", "dev"]);
    git_in(&lib, &["push", "-q", "origin", "dev"]);
    git_in(&lib, &["switch", "-q", "main"]);
    succeeded(&run(&w, &["member", "set-branch", LIB, "dev"]));
    assert_eq!(
        changed_lines(&w),
        (lines(&["branch = \"main\""]), lines(&["branch = \"dev\""]))
    );
}

#[test]
fn set_url_on_a_hand_laid_out_manifest_changes_only_the_url_and_keeps_its_comment() {
    let w = two_members();
    hand_laid_out(&w);
    let mirror = w._tmp.path().join("lib-mirror.git");
    let lib_bare = w
        .manifest_bares
        .iter()
        .find(|(p, _)| p == LIB)
        .unwrap()
        .1
        .clone();
    git_in(
        w._tmp.path(),
        &[
            "clone",
            "-q",
            "--bare",
            lib_bare.to_str().unwrap(),
            mirror.to_str().unwrap(),
        ],
    );
    let lib = w.workspace.join(LIB);
    git_in(
        &lib,
        &["remote", "add", "mirror", &common::file_url(&mirror)],
    );
    git_in(&lib, &["fetch", "-q", "mirror"]);
    succeeded(&run(
        &w,
        &["member", "set-url", LIB, &common::file_url(&mirror)],
    ));
    assert_eq!(
        changed_lines(&w),
        (
            vec![format!("url = \"{}\" # origin", url_of(&w, LIB))],
            vec![format!("url = \"{}\" # origin", common::file_url(&mirror))]
        )
    );
}

#[test]
fn member_add_on_a_hand_laid_out_manifest_adds_only_the_new_table_after_the_last_one() {
    let w = two_members();
    let text = hand_laid_out(&w);
    let docs = w._tmp.path().join("docs.git");
    common::init_bare_repo_with_commit(&docs);
    succeeded(&run(&w, &["member", "add", &common::file_url(&docs)]));
    let (removed, added) = changed_lines(&w);
    assert!(removed.is_empty(), "{removed:?}");
    let now = manifest(&w);
    let header = added
        .iter()
        .find(|l| l.starts_with("[repositories."))
        .unwrap_or_else(|| panic!("a table was added: {added:?}"));
    let at = now.find(header.as_str()).unwrap();
    assert!(
        now[..at] == format!("{}\n", &text[..text.find("# end\n").unwrap()]),
        "the new table follows app, the last table, and nothing before it changed:\n{now}"
    );
    assert!(now.ends_with("# end\n"), "{now}");
}

#[test]
fn member_remove_on_a_hand_laid_out_manifest_removes_only_that_table() {
    let (w, removed, added) = verb_on_hand_laid_out(&["member", "remove", APP]);
    assert!(added.is_empty(), "{added:?}");
    assert_eq!(
        removed,
        vec![
            String::new(),
            format!("[repositories.\"{APP}\"]"),
            "role = \"owned\"".to_owned(),
            "type = \"git\"".to_owned(),
            format!("url = \"{}\"", url_of(&w, APP)),
            "branch = \"main\"".to_owned(),
        ]
    );
}

#[test]
fn member_remove_of_the_last_member_works_when_members_are_root_dotted_keys() {
    let w = two_members();
    let mut text = String::new();
    for member in [APP, LIB] {
        for (key, value) in [
            ("type", "git".to_owned()),
            ("url", url_of(&w, member)),
            ("branch", "main".to_owned()),
            ("role", "owned".to_owned()),
        ] {
            text.push_str(&format!("repositories.\"{member}\".{key} = \"{value}\"\n"));
        }
    }
    std::fs::write(project(&w).join("rwv.toml"), &text).unwrap();
    git_in(
        project(&w),
        &["commit", "-qam", "hand: root-dotted members"],
    );

    succeeded(&run(&w, &["member", "remove", APP]));
    succeeded(&run(&w, &["member", "remove", LIB]));
    let now = committed_manifest(&w);
    assert!(!now.contains(APP) && !now.contains(LIB), "{now}");
    assert_eq!(manifest(&w).trim_end(), now);
}

#[test]
fn add_commits_the_member_and_its_pin_and_leaves_another_hand_edit_of_the_file_uncommitted() {
    let w = two_members();
    hand_edit_app_role(&w);
    let before = commits(&w);
    let docs = w._tmp.path().join("docs.git");
    common::init_bare_repo_with_commit(&docs);

    succeeded(&run(&w, &["member", "add", &common::file_url(&docs)]));

    assert_eq!(commits(&w), before + 1);
    let committed = committed_manifest(&w);
    assert!(committed.contains("docs"), "{committed}");
    assert!(
        !committed.contains("role = \"dependency\""),
        "the hand edit of app's role was committed:\n{committed}"
    );
    assert!(manifest(&w).contains("role = \"dependency\""));
    let lock = git_in(project(&w), &["show", "HEAD:rwv.lock"]);
    assert!(
        lock.contains("docs") && lock.contains(APP) && lock.contains(LIB),
        "{lock}"
    );
}

#[test]
fn add_on_an_emptied_manifest_refuses_and_commits_nothing() {
    let w = two_members();
    std::fs::write(project(&w).join("rwv.toml"), "").unwrap();
    let before = commits(&w);
    let docs = w._tmp.path().join("docs.git");
    common::init_bare_repo_with_commit(&docs);

    let text = refused(&run(&w, &["member", "add", &common::file_url(&docs)]));

    assert!(text.contains("holds no `repositories` table"), "{text}");
    assert_eq!(commits(&w), before);
    let committed = committed_manifest(&w);
    assert!(
        committed.contains(APP) && committed.contains(LIB),
        "{committed}"
    );
}

#[test]
fn a_fresh_project_with_no_members_takes_its_first_member() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let init = rwv()
        .args(["-C", &ws.to_string_lossy(), "init", "fresh"])
        .output()
        .unwrap();
    succeeded(&init);
    let docs = tmp.path().join("docs.git");
    common::init_bare_repo_with_commit(&docs);

    let add = rwv()
        .args(["-C", &ws.to_string_lossy(), "member", "add"])
        .arg(common::file_url(&docs))
        .output()
        .unwrap();

    succeeded(&add);
    let project = ws.join("projects").join("fresh");
    let committed = git_in(&project, &["show", "HEAD:rwv.toml"]);
    assert!(committed.contains("[repositories"), "{committed}");
    assert!(committed.contains("docs"), "{committed}");
}
