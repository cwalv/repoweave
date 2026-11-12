//! `rwv member set-branch`: both meanings of the branch an entry declares, the
//! commit that carries the entry and its pin, and what the verb refuses before
//! it changes anything.

use std::path::{Path, PathBuf};

mod common;

use common::{git_in, rwv, PushWorkspace};

const APP: &str = "github/acme/app";
const LIB: &str = "github/acme/lib";

fn two_members() -> PushWorkspace {
    common::build_workspace("proj", &[(APP, "owned"), (LIB, "owned")])
}

fn member(w: &PushWorkspace, repo: &str) -> PathBuf {
    w.workspace.join(repo)
}

fn project(w: &PushWorkspace) -> PathBuf {
    w.workspace.join("projects").join(&w.project_name)
}

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    git_in(dir, &["add", file]);
    git_in(dir, &["commit", "-qm", file]);
    git_in(dir, &["rev-parse", "HEAD"])
}

fn pin_of(w: &PushWorkspace, repo: &str) -> Option<String> {
    let text = std::fs::read_to_string(project(w).join("rwv.lock")).unwrap();
    let lock: serde_json::Value = serde_json::from_str(&text).unwrap();
    lock["repositories"][repo]["version"]
        .as_str()
        .map(str::to_owned)
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

fn files_of_head(dir: &Path) -> Vec<String> {
    let mut files: Vec<String> = git_in(dir, &["show", "--name-only", "--format=", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect();
    files.sort_unstable();
    files
}

fn manifest_text(w: &PushWorkspace) -> String {
    std::fs::read_to_string(project(w).join("rwv.toml")).unwrap()
}

fn branch_of_entry(w: &PushWorkspace, repo: &str) -> String {
    let manifest: toml::Value = toml::from_str(&manifest_text(w)).unwrap();
    manifest["repositories"][repo]["branch"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn committed_manifest(w: &PushWorkspace) -> String {
    git_in(project(w), &["show", "HEAD:rwv.toml"])
}

fn publish_branch(dir: &Path, branch: &str) {
    git_in(dir, &["push", "-q", "origin", branch]);
}

fn lib_with_dev(w: &PushWorkspace) -> String {
    let lib = member(w, LIB);
    git_in(&lib, &["switch", "-q", "-c", "dev"]);
    let tip = commit_in(&lib, "dev-one");
    publish_branch(&lib, "dev");
    git_in(&lib, &["switch", "-q", "main"]);
    tip
}

#[test]
fn set_branch_declares_an_existing_branch_pins_its_tip_and_commits_the_entry_and_the_pin() {
    let w = two_members();
    let tip = lib_with_dev(&w);
    let before = commits(&w);

    let text = succeeded(&run(&w, &["member", "set-branch", LIB, "dev"]));

    assert_eq!(branch_of_entry(&w, LIB), "dev", "{text}");
    assert_eq!(pin_of(&w, LIB).as_deref(), Some(tip.as_str()));
    common::assert_on_branch(&member(&w, LIB), "dev");
    assert_eq!(commits(&w), before + 1, "the setter is one commit");
    assert_eq!(
        files_of_head(&project(&w)),
        ["proj.code-workspace", "rwv.lock", "rwv.toml"],
        "the entry, the pin, and what the integrations regenerate from the entry"
    );
    assert_eq!(git_in(project(&w), &["status", "--porcelain"]), "");
}

#[test]
fn a_branch_that_exists_nowhere_is_refused_naming_the_fetch_and_the_new_spelling_and_nothing_changes() {
    let w = two_members();
    let before = (commits(&w), manifest_text(&w), pin_of(&w, LIB));

    let text = refused(&run(&w, &["member", "set-branch", LIB, "dve"]));

    assert!(text.contains("name no-such-branch (dve)"), "{text}");
    assert!(text.contains("rwv fetch"), "{text}");
    assert!(
        text.contains("rwv member set-branch") && text.contains("--new"),
        "{text}"
    );
    assert_eq!(
        (commits(&w), manifest_text(&w), pin_of(&w, LIB)),
        before,
        "the refusal came after something changed"
    );
    assert!(
        git_in(member(&w, LIB), &["branch", "--list", "dve"]).is_empty(),
        "the typo was created"
    );
}

#[test]
fn new_creates_the_branch_at_the_pin_and_pins_it_there() {
    let w = two_members();
    let pin = pin_of(&w, LIB).unwrap();
    let before = commits(&w);

    succeeded(&run(&w, &["member", "set-branch", LIB, "hold", "--new"]));

    assert_eq!(branch_of_entry(&w, LIB), "hold");
    assert_eq!(pin_of(&w, LIB).as_deref(), Some(pin.as_str()));
    assert_eq!(git_in(member(&w, LIB), &["rev-parse", "hold"]), pin);
    common::assert_on_branch(&member(&w, LIB), "hold");
    assert_eq!(commits(&w), before + 1);
}

#[test]
fn new_at_creates_the_branch_at_the_revision_and_pins_it_there() {
    let w = two_members();
    let lib = member(&w, LIB);
    let older = git_in(&lib, &["rev-parse", "HEAD"]);
    commit_in(&lib, "ahead");
    publish_branch(&lib, "main");

    succeeded(&run(&w, &["member", "set-branch", LIB, "hold", "--new", "--at", &older]));

    assert_eq!(git_in(&lib, &["rev-parse", "hold"]), older);
    assert_eq!(pin_of(&w, LIB).as_deref(), Some(older.as_str()));
}

#[test]
fn new_on_a_branch_that_exists_is_refused_naming_the_spelling_without_it() {
    let w = two_members();
    lib_with_dev(&w);
    let before = (commits(&w), manifest_text(&w));

    let text = refused(&run(&w, &["member", "set-branch", LIB, "dev", "--new"]));

    assert!(text.contains("name exists (dev)"), "{text}");
    assert!(text.contains("rwv member set-branch"), "{text}");
    assert_eq!((commits(&w), manifest_text(&w)), before);
}

#[test]
fn at_a_commit_the_branch_does_not_reach_is_refused_naming_the_pin_choice() {
    let w = two_members();
    let lib = member(&w, LIB);
    lib_with_dev(&w);
    let on_main_only = commit_in(&lib, "main-only");
    let before = (commits(&w), manifest_text(&w));

    let text = refused(&run(&w, &["member", "set-branch", LIB, "dev", "--at", &on_main_only]));

    assert!(text.contains("pin not on declared branch"), "{text}");
    assert!(text.contains("rwv member pin"), "{text}");
    assert_eq!((commits(&w), manifest_text(&w)), before);
}

#[test]
fn the_commit_holds_only_the_entry_and_the_pin_and_other_edits_to_the_file_stay() {
    let w = two_members();
    lib_with_dev(&w);
    let app_role_edit = manifest_text(&w).replace(
        &format!("branch = \"main\"\nrole = \"owned\"\n[repositories.\"{LIB}\"]"),
        &format!("branch = \"main\"\nrole = \"dependency\"\n[repositories.\"{LIB}\"]"),
    );
    assert_ne!(app_role_edit, manifest_text(&w), "the hand edit applies");
    std::fs::write(project(&w).join("rwv.toml"), &app_role_edit).unwrap();

    succeeded(&run(&w, &["member", "set-branch", LIB, "dev"]));

    let committed = committed_manifest(&w);
    assert!(committed.contains("branch = \"dev\""), "{committed}");
    assert!(
        !committed.contains("role = \"dependency\""),
        "the hand edit of app's role was committed:\n{committed}"
    );
    let on_disk = manifest_text(&w);
    assert!(on_disk.contains("role = \"dependency\""), "{on_disk}");
    assert!(on_disk.contains("branch = \"dev\""), "{on_disk}");
}
