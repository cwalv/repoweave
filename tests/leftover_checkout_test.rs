//! A member removed from `rwv.toml` by hand leaves a checkout that `rwv.lock`
//! still names. Default-scope doctor reports it as `workweave-drift/extra`,
//! with the primary's canonical clone carrying no workweave; a path neither
//! file names is not this finding's.

use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

const KEPT: &str = "github/acme/kept";
const GONE: &str = "github/acme/gone";
const NEVER: &str = "github/acme/never";
const PROJECT: &str = "my-app";

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn write_manifest(project_dir: &Path, repos: &[&str]) {
    let mut toml = String::from("[repositories]\n");
    for repo in repos {
        let name = repo.rsplit('/').next().unwrap();
        toml.push_str(&format!(
            "[repositories.{repo:?}]\ntype = \"git\"\nurl = \"https://github.com/acme/{name}.git\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), toml).unwrap();
}

fn write_lock(project_dir: &Path, repos: &[(&str, &str)]) {
    let entries: Vec<String> = repos
        .iter()
        .map(|(repo, sha)| {
            let name = repo.rsplit('/').next().unwrap();
            format!(
                "{repo:?}: {{\"type\": \"git\", \"url\": \"https://github.com/acme/{name}.git\", \"version\": {sha:?}}}"
            )
        })
        .collect();
    let raw = format!("{{\"repositories\": {{{}}}}}", entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
}

fn commit_project(project_dir: &Path, message: &str) {
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    if !project_dir.join(".git").exists() {
        common::git_in(project_dir, &["init", "-b", "main"]);
        common::git_in(project_dir, &["config", "merge.rwv-ours.driver", "true"]);
    }
    common::git_in(project_dir, &["add", "."]);
    common::git_in(project_dir, &["commit", "-m", message]);
}

struct Weave {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    project_dir: PathBuf,
}

/// A primary weave whose project `my-app` lists only KEPT and whose lock
/// names KEPT and GONE, both cloned. NEVER is cloned and named
/// by no manifest or lock; a second project `other` lists nothing. `my-app` is active.
fn weave() -> Weave {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    let project_dir = root.join("projects").join(PROJECT);
    let other_dir = root.join("projects/other");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&other_dir).unwrap();
    let kept = init_repo(&root.join(KEPT));
    let gone = init_repo(&root.join(GONE));
    init_repo(&root.join(NEVER));

    write_manifest(&project_dir, &[KEPT]);
    write_lock(&project_dir, &[(KEPT, &kept), (GONE, &gone)]);
    commit_project(&project_dir, "initial");
    write_manifest(&other_dir, &[]);
    commit_project(&other_dir, "initial");
    std::fs::write(root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    Weave {
        _tmp: tmp,
        root,
        project_dir,
    }
}

fn doctor(dir: &Path, args: &[&str]) -> (Value, String) {
    let mut full = vec!["doctor", "--json"];
    full.extend_from_slice(args);
    let out = common::rwv().args(&full).current_dir(dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let doc = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{e}\n{stdout}"));
    (doc, String::from_utf8_lossy(&out.stderr).into_owned())
}

fn drift_for(doc: &Value, repo: &str) -> Vec<Value> {
    doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["path"] == repo && v["kind"] != "provenance")
        .cloned()
        .collect()
}

#[test]
fn default_doctor_reports_a_hand_removed_members_checkout_and_names_both_remedies() {
    let w = weave();

    let (doc, _) = doctor(&w.root, &[]);
    let found = drift_for(&doc, GONE);
    assert_eq!(found.len(), 1, "{doc}");
    assert_eq!(found[0]["kind"], "workweave-drift");
    assert_eq!(found[0]["sub_kind"], "extra");
    assert!(found[0]["workweave"].is_null(), "{}", found[0]);

    let out = common::rwv()
        .args(["doctor"])
        .current_dir(&w.root)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("rwv remove <path> --delete") && text.contains("rwv add <url>"),
        "{text}"
    );
}

#[test]
fn a_member_still_in_the_manifest_and_a_path_no_file_names_raise_nothing() {
    let w = weave();
    let (doc, _) = doctor(&w.root, &[]);
    assert_eq!(drift_for(&doc, KEPT), Vec::<Value>::new());
    assert_eq!(drift_for(&doc, NEVER), Vec::<Value>::new());

    let (all, _) = doctor(&w.root, &["--all"]);
    let never = drift_for(&all, NEVER);
    assert_eq!(never.len(), 1, "{all}");
    assert_eq!(never[0]["kind"], "orphaned-clone");
    let gone = drift_for(&all, GONE);
    assert_eq!(
        gone.iter().map(|v| v["kind"].clone()).collect::<Vec<_>>(),
        vec![Value::from("workweave-drift")],
        "--all reports the leftover once, not also as an orphan: {all}"
    );
}

#[test]
fn a_clone_another_project_lists_is_not_a_leftover() {
    let w = weave();
    let other_dir = w.root.join("projects/other");
    write_manifest(&other_dir, &[GONE]);
    common::git_in(&other_dir, &["commit", "-qam", "list gone"]);
    let (doc, _) = doctor(&w.root, &[]);
    assert_eq!(drift_for(&doc, GONE), Vec::<Value>::new(), "{doc}");
}

#[test]
fn doctor_fix_leaves_the_leftover_checkout_in_place_and_reported() {
    let w = weave();
    let out = common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(&w.root)
        .output()
        .unwrap();
    let _ = out;
    assert!(w.root.join(GONE).join("README.md").exists());
    let (doc, _) = doctor(&w.root, &[]);
    assert_eq!(drift_for(&doc, GONE).len(), 1, "{doc}");
}

#[test]
fn a_workweave_reports_its_own_leftover_worktree() {
    let w = weave();
    write_manifest(&w.project_dir, &[KEPT, GONE]);
    common::git_in(&w.project_dir, &["commit", "-qam", "list gone"]);
    let out = common::rwv()
        .args(["workweave", PROJECT, "create", "s1"])
        .current_dir(&w.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seat = w
        .root
        .parent()
        .unwrap()
        .join(".workweaves")
        .join(format!("{PROJECT}--s1"));
    assert!(seat.join(GONE).exists());

    let seat_manifest = seat.join("projects").join(PROJECT);
    write_manifest(&seat_manifest, &[KEPT]);

    let (doc, _) = doctor(&seat, &[]);
    let found = drift_for(&doc, GONE);
    assert_eq!(found.len(), 1, "{doc}");
    assert_eq!(found[0]["sub_kind"], "extra");
    assert_eq!(found[0]["workweave"], "s1");
    assert_eq!(drift_for(&doc, KEPT), Vec::<Value>::new());
}
