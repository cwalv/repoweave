//! `rwv log`: the VCS's log per repo, and the commits beyond a weave branch's
//! pins.
//!
//! Every test drives the binary against a primary weave built from plain git
//! repos. The world is `main` with both members pinned at their first commit;
//! `feature_world` then forks `feat-x` at that point, moves `main` and its
//! lock past the fork, and leaves the weave on `feat-x`, so a listing that read
//! `main`'s branch tips, or diffed against a pin directly, would differ from
//! one that reads the pins and anchors at the merge-base.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

const PROJECT: &str = "proj";
const LIB: &str = "github/org/lib";
const APP: &str = "github/org/app";

struct World {
    _tmp: tempfile::TempDir,
    ws: PathBuf,
}

impl World {
    fn repo(&self, rel: &str) -> PathBuf {
        self.ws.join(rel)
    }

    fn lib(&self) -> PathBuf {
        self.repo(LIB)
    }

    fn app(&self) -> PathBuf {
        self.repo(APP)
    }

    fn project(&self) -> PathBuf {
        self.ws.join("projects").join(PROJECT)
    }

    fn rwv(&self, args: &[&str]) -> Output {
        rwv_in(&self.ws, args)
    }
}

fn rwv_in(dir: &Path, args: &[&str]) -> Output {
    common::rwv()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("rwv runs")
}

fn commit(repo: &Path, file: &str, message: &str) -> String {
    std::fs::write(repo.join(file), format!("{message}\n")).unwrap();
    common::git_in(repo, &["add", file]);
    common::git_in(repo, &["commit", "-m", message]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn init_member(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    commit(path, "README.md", "initial")
}

fn rwv_toml(entries: &[(&str, &Path)]) -> String {
    entries
        .iter()
        .map(|(rel, repo)| {
            format!(
                "[repositories.\"{rel}\"]\ntype = \"git\"\nurl = \"{}\"\nbranch = \"main\"\nrole = \"owned\"\n\n",
                common::file_url(repo)
            )
        })
        .collect()
}

fn write_lock(project: &Path, pins: &[(&str, &Path, &str)]) {
    let urls: Vec<(String, String)> = pins
        .iter()
        .map(|(rel, repo, _)| ((*rel).to_owned(), common::file_url(repo)))
        .collect();
    let rows: Vec<(&str, &str, &str)> = pins
        .iter()
        .zip(&urls)
        .map(|((_, _, sha), (rel, url))| (rel.as_str(), url.as_str(), *sha))
        .collect();
    common::fixture_lock(project, &rows);
}

/// `main` with `lib` and `app` pinned at their first commits. With
/// `pin_app` false the lock pins `lib` alone and `rwv.toml` lists only it, as a
/// weave branch does before a feature adds `app`.
fn world(pin_app: bool) -> World {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    let lib = ws.join(LIB);
    let app = ws.join(APP);
    let lib_first = init_member(&lib);
    let app_first = init_member(&app);

    let project = ws.join("projects").join(PROJECT);
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
    if pin_app {
        std::fs::write(
            project.join("rwv.toml"),
            rwv_toml(&[(LIB, lib.as_path()), (APP, app.as_path())]),
        )
        .unwrap();
        write_lock(
            &project,
            &[
                (LIB, lib.as_path(), &lib_first),
                (APP, app.as_path(), &app_first),
            ],
        );
    } else {
        std::fs::write(project.join("rwv.toml"), rwv_toml(&[(LIB, lib.as_path())])).unwrap();
        write_lock(&project, &[(LIB, lib.as_path(), &lib_first)]);
    }
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "lock: initial"]);
    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    World { _tmp: tmp, ws }
}

/// What `feature_world` leaves behind, for tests to name commits by.
struct Feature {
    fork: String,
    main_moved: String,
    feat: String,
    app_unlocked: String,
}

/// `feat-x` forked from `main` in `lib` and the project repo; `main` then
/// moves `lib` and relocks; the weave ends on `feat-x` with one commit on it in
/// each of those two repos. `app` stays on `main` and gains a commit `main`'s
/// lock does not pin.
fn feature_world() -> (World, Feature) {
    let w = world(true);
    let fork = common::git_in(w.lib(), &["rev-parse", "HEAD"]);
    for repo in [w.lib(), w.project()] {
        common::git_in(&repo, &["branch", "feat-x"]);
    }
    let main_moved = commit(&w.lib(), "main-moved.txt", "lib: main moves");
    let relock = w.rwv(&["lock", "--commit"]);
    assert!(relock.status.success(), "{}", stderr(&relock));
    for repo in [w.lib(), w.project()] {
        common::git_in(&repo, &["switch", "feat-x"]);
    }
    let feat = commit(&w.lib(), "feat.txt", "lib: feat work");
    commit(&w.project(), "notes.txt", "proj: feat note");
    let app_unlocked = commit(&w.app(), "unlocked.txt", "app: unlocked work");
    (
        w,
        Feature {
            fork,
            main_moved,
            feat,
            app_unlocked,
        },
    )
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("one JSON document ({e}):\n{}", stdout(out)))
}

fn repo_record<'a>(doc: &'a Value, path: &str) -> &'a Value {
    doc["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == path)
        .unwrap_or_else(|| panic!("no record for {path}:\n{doc:#}"))
}

fn subjects(record: &Value) -> Vec<&str> {
    record["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["subject"].as_str().unwrap())
        .collect()
}

/// The text of `path`'s `=== <path> ===` section.
fn section<'a>(text: &'a str, path: &str) -> &'a str {
    let header = format!("=== {path} ===\n");
    let start = text
        .find(&header)
        .unwrap_or_else(|| panic!("no section for {path}:\n{text}"))
        + header.len();
    let rest = &text[start..];
    &rest[..rest.find("\n=== ").unwrap_or(rest.len())]
}

#[test]
fn a_bare_log_is_the_vcs_log_of_head_with_no_default_endpoint() {
    let (w, f) = feature_world();

    let out = w.rwv(&["log", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    let lib = repo_record(&doc, LIB);
    assert_eq!(
        subjects(lib),
        ["lib: feat work", "initial"],
        "the whole of lib's history from HEAD, the fork point's own commit included: {doc:#}"
    );
    assert_eq!(lib["head"], f.feat.as_str());
    assert!(
        lib.get("pin").is_none() && doc.get("against").is_none(),
        "a bare log reads no weave branch: {doc:#}"
    );
    assert_eq!(
        subjects(repo_record(&doc, "(project)")),
        ["proj: feat note", "lock: initial"]
    );
}

#[test]
fn against_lists_what_head_holds_beyond_the_pins_and_not_the_branch_tips() {
    let (w, f) = feature_world();

    let out = w.rwv(&["log", "--against", "main", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["against"], "main");
    assert_eq!(doc["diff"], false);

    let lib = repo_record(&doc, LIB);
    assert_eq!(
        lib["pin"],
        f.main_moved.as_str(),
        "main's lock pins lib at the commit main moved to"
    );
    assert_eq!(lib["head"], f.feat.as_str());
    assert_eq!(subjects(lib), ["lib: feat work"]);
    assert_eq!(lib["commits"][0]["sha"], f.feat.as_str());
    assert_eq!(lib["commits"][0]["short"], f.feat[..12]);

    let app = repo_record(&doc, APP);
    assert_eq!(
        subjects(app),
        ["app: unlocked work"],
        "app's main holds this commit, but main's lock does not pin it: {doc:#}"
    );
    assert_eq!(app["head"], f.app_unlocked.as_str());

    let project = repo_record(&doc, "(project)");
    assert_eq!(subjects(project), ["proj: feat note"]);
    assert_eq!(
        project["pin"],
        common::git_in(w.project(), &["rev-parse", "main"]).as_str(),
        "the project repo is measured against the branch's own tip"
    );

    let text = stdout(&w.rwv(&["log", "--against", "main"]));
    assert!(text.starts_with("log against main\n"), "{text}");
    assert!(
        section(&text, LIB).contains(&format!("{} lib: feat work", &f.feat[..12])),
        "{text}"
    );
    assert!(!section(&text, LIB).contains("lib: main moves"), "{text}");
}

#[test]
fn diff_anchors_at_the_merge_base_so_a_moved_pin_shows_no_reversals() {
    let (w, f) = feature_world();

    let out = w.rwv(&["log", "--against", "main", "--diff", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["diff"], true);
    let lib = repo_record(&doc, LIB);
    assert_eq!(
        lib["diff_base"],
        f.fork.as_str(),
        "the diff starts where the pin meets HEAD, not at the pin"
    );
    let diff = lib["diff"].as_str().unwrap();
    assert!(diff.contains("feat.txt"), "{diff}");
    assert!(
        !diff.contains("main-moved.txt"),
        "main's later work is not a reversal in the feature's diff:\n{diff}"
    );
    assert!(lib["commits"].as_array().unwrap().is_empty());
    assert!(repo_record(&doc, APP)["diff"]
        .as_str()
        .unwrap()
        .contains("unlocked.txt"));

    let text = stdout(&w.rwv(&["log", "--against", "main", "--diff"]));
    assert!(text.starts_with("diff against main\n"), "{text}");
    assert!(
        section(&text, LIB).contains(&format!("(diff range: {}..HEAD)", &f.fork[..12])),
        "{text}"
    );
}

#[test]
fn a_member_the_branch_does_not_pin_is_listed_as_not_in_it() {
    let w = world(false);
    common::git_in(w.project(), &["branch", "feat-x"]);
    common::git_in(w.project(), &["switch", "feat-x"]);
    let lib = w.lib();
    let app = w.app();
    std::fs::write(
        w.project().join("rwv.toml"),
        rwv_toml(&[(LIB, lib.as_path()), (APP, app.as_path())]),
    )
    .unwrap();
    let app_first = common::git_in(&app, &["rev-parse", "HEAD"]);
    let lib_first = common::git_in(&lib, &["rev-parse", "HEAD"]);
    write_lock(
        &w.project(),
        &[
            (LIB, lib.as_path(), &lib_first),
            (APP, app.as_path(), &app_first),
        ],
    );
    common::git_in(w.project(), &["add", "-A"]);
    common::git_in(w.project(), &["commit", "-m", "proj: add app"]);

    let out = w.rwv(&["log", "--against", "main", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    let app = repo_record(&doc, APP);
    assert!(app.get("pin").is_none(), "{doc:#}");
    assert_eq!(app["note"], "not in main");
    assert!(app["commits"].as_array().unwrap().is_empty());
    assert!(repo_record(&doc, LIB)["pin"].is_string());

    let text = stdout(&w.rwv(&["log", "--against", "main"]));
    assert!(section(&text, APP).contains("note: not in main"), "{text}");
}

#[test]
fn a_range_endpoint_that_names_nothing_is_shown_in_its_repo_and_the_others_still_print() {
    let (w, f) = feature_world();

    let out = w.rwv(&["log", "main..feat-x"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        section(&text, LIB).contains("lib: feat work"),
        "lib has both endpoints and is listed:\n{text}"
    );
    assert!(
        section(&text, APP).contains("name no-such-branch (feat-x)"),
        "app has no feat-x:\n{text}"
    );
    assert!(
        section(&text, "(project)").contains("proj: feat note"),
        "a repo after the failing one is still listed:\n{text}"
    );
    let err = stderr(&out);
    assert!(
        err.contains(&format!("rwv log: {APP}: name no-such-branch (feat-x)")),
        "{err}"
    );
    assert!(err.ends_with("rwv explain name\n"), "{err}");

    let out = w.rwv(&["log", "main..feat-x", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["range"], "main..feat-x");
    let lib = repo_record(&doc, LIB);
    assert_eq!(subjects(lib), ["lib: feat work"]);
    assert_eq!(lib["endpoints"][1]["spelled"], "feat-x");
    assert_eq!(lib["endpoints"][1]["id"], f.feat.as_str());
    let app = repo_record(&doc, APP);
    assert!(app["commits"].as_array().unwrap().is_empty());
    assert_eq!(app["refused"][0]["axis"], "name");
    assert_eq!(app["refused"][0]["state"], "no-such-branch");
    assert_eq!(app["refused"][0]["subject"], "feat-x");
    assert!(repo_record(&doc, LIB).get("refused").is_none());
}

#[test]
fn a_range_resolves_in_each_repo_as_git_would() {
    let (w, f) = feature_world();

    let doc = json(&w.rwv(&["log", "main...feat-x", "--json"]));
    let lib = repo_record(&doc, LIB);
    let mut listed = subjects(lib);
    listed.sort_unstable();
    assert_eq!(
        listed,
        ["lib: feat work", "lib: main moves"],
        "a symmetric difference lists both sides: {doc:#}"
    );
    assert_eq!(lib["endpoints"][0]["id"], f.main_moved.as_str());

    let doc = json(&w.rwv(&["log", "HEAD~1..", "--json"]));
    assert_eq!(subjects(repo_record(&doc, LIB)), ["lib: feat work"]);
    assert_eq!(
        repo_record(&doc, LIB)["endpoints"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "an empty side is HEAD and names no endpoint"
    );
}

#[test]
fn an_endpoint_git_would_read_as_an_option_names_nothing() {
    let (w, _f) = feature_world();

    let out = w.rwv(&["log", "--", "--all"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        section(&stdout(&out), LIB).contains("name no-such-branch (--all)"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn against_a_name_that_is_no_weave_branch_refuses_before_listing() {
    let (w, _f) = feature_world();

    let out = w.rwv(&["log", "--against", "nosuch"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "", "nothing is listed");
    let err = stderr(&out);
    assert!(
        err.contains("rwv log: projects/proj: name no-such-branch (nosuch)"),
        "{err}"
    );
    assert!(err.ends_with("rwv explain name\n"), "{err}");

    let out = w.rwv(&["log", "--against", "nosuch", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["refused"][0]["state"], "no-such-branch");
    assert_eq!(doc["refused"][0]["path"], "projects/proj");
}

#[test]
fn a_branch_whose_tip_carries_no_manifest_is_no_weave_branch() {
    let (w, _f) = feature_world();
    let project = w.project();
    common::git_in(&project, &["switch", "--orphan", "records"]);
    std::fs::write(project.join("entry.toml"), "mode = \"feature\"\n").unwrap();
    common::git_in(&project, &["add", "entry.toml"]);
    common::git_in(&project, &["commit", "-m", "records: entry"]);
    common::git_in(&project, &["switch", "feat-x"]);

    let out = w.rwv(&["log", "--against", "records"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("name no-such-branch (records)"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn against_reads_a_remote_tracking_branch_as_last_fetched() {
    let (w, f) = feature_world();
    let main_tip = common::git_in(w.project(), &["rev-parse", "main"]);
    common::git_in(
        w.project(),
        &["update-ref", "refs/remotes/origin/stable", &main_tip],
    );

    let out = w.rwv(&["log", "--against", "stable", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(repo_record(&doc, LIB)["pin"], f.main_moved.as_str());
}

#[test]
fn a_diff_without_against_and_a_range_with_it_are_usage_errors() {
    let (w, _f) = feature_world();

    let out = w.rwv(&["log", "--diff"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--against"), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");

    let out = w.rwv(&["log", "main..", "--against", "main"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--against"), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
}

#[test]
fn the_workweave_log_is_gone_and_names_no_replacement() {
    let (w, _f) = feature_world();

    let out = w.rwv(&["workweave", "log"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    assert!(err.contains("unrecognized subcommand 'log'"), "{err}");
    for mapping in ["rwv log", "--against", "moved", "replaced", "renamed"] {
        assert!(
            !err.contains(mapping),
            "the retired spelling gets clap's refusal and no mapping ({mapping:?}):\n{err}"
        );
    }
}

#[test]
fn a_workweave_is_read_from_the_primary_by_name() {
    let w = world(true);
    let create = w.rwv(&["workweave", "create", "--project", PROJECT, "seat"]);
    assert!(create.status.success(), "{}", stderr(&create));
    let seat =
        w.ws.parent()
            .unwrap()
            .join(".workweaves")
            .join(format!("{PROJECT}--seat"));
    commit(&seat.join(LIB), "seat.txt", "lib: seat work");

    let out = w.rwv(&[
        "-w",
        &format!("{PROJECT}--seat"),
        "log",
        "--against",
        "main",
        "--json",
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    let doc = json(&out);
    assert_eq!(subjects(repo_record(&doc, LIB)), ["lib: seat work"]);
    assert!(subjects(repo_record(&doc, APP)).is_empty());

    let from_primary = json(&w.rwv(&["log", "--against", "main", "--json"]));
    assert!(
        subjects(repo_record(&from_primary, LIB)).is_empty(),
        "the primary's own HEAD holds nothing beyond main's pin"
    );
}
