//! `rwv branch set-history`: the `history` setting has no default, the verb
//! commits only its own change, a member's own setting overrides the
//! top-level one, `lock --commit` converges a hand-edited value, and `rwv
//! doctor` reports projects that record different settings for one member.

use std::path::{Path, PathBuf};

use crate::common;

use common::cli::{refused, run, succeed};
use common::{git_in, PushWorkspace};
use repoweave::diagnosis::State;
use repoweave::manifest::{History, Manifest, RepoPath};

const APP: &str = "github/acme/app";
const LIB: &str = "github/acme/lib";

fn two_members() -> PushWorkspace {
    common::build_workspace("proj", &[(APP, "owned"), (LIB, "owned")])
}

fn project(w: &PushWorkspace) -> PathBuf {
    w.workspace.join("projects").join(&w.project_name)
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn commits(dir: &Path) -> usize {
    git_in(dir, &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap()
}

fn working(w: &PushWorkspace) -> String {
    std::fs::read_to_string(project(w).join("rwv.toml")).unwrap()
}

fn committed(dir: &Path) -> String {
    git_in(dir, &["show", "HEAD:rwv.toml"])
}

fn parsed(text: &str) -> Manifest {
    Manifest::from_toml_str(text).unwrap()
}

fn member(path: &str) -> RepoPath {
    RepoPath::new(path).unwrap()
}

fn effective(text: &str) -> (Option<History>, Option<History>, Option<History>) {
    let manifest = parsed(text);
    (
        manifest.history,
        manifest.history_for(&member(APP)),
        manifest.history_for(&member(LIB)),
    )
}

fn changed_files(dir: &Path) -> Vec<String> {
    git_in(dir, &["show", "--name-only", "--format=", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect()
}

fn plain_origin(tmp: &Path, name: &str) -> String {
    let bare = tmp.join("origin").join(format!("{name}.git"));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    git_in(
        tmp,
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    let seed = tmp.join(format!("seed-{name}"));
    git_in(
        tmp,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            seed.to_str().unwrap(),
        ],
    );
    git_in(&seed, &["config", "user.email", "test@test.com"]);
    git_in(&seed, &["config", "user.name", "Test"]);
    std::fs::write(seed.join("README.md"), format!("{name}\n")).unwrap();
    git_in(&seed, &["add", "-A"]);
    git_in(&seed, &["commit", "-qm", "initial"]);
    git_in(&seed, &["push", "-q", "origin", "main"]);
    common::file_url(&bare)
}

#[test]
fn nothing_writes_a_history_the_operator_did_not_name() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    succeed(&["init", "demo"], &ws);
    let project = ws.join("projects/demo");
    git_in(&project, &["config", "user.email", "test@test.com"]);
    git_in(&project, &["config", "user.name", "Test"]);
    succeed(&["member", "add", &plain_origin(tmp.path(), "alpha")], &ws);
    succeed(&["lock", "--commit"], &ws);

    for text in [
        std::fs::read_to_string(project.join("rwv.toml")).unwrap(),
        committed(&project),
    ] {
        let manifest = parsed(&text);
        assert_eq!(manifest.history, None, "{text}");
        for (path, _) in manifest.iter_entries() {
            assert_eq!(
                manifest.history_for(path),
                None,
                "{path} reads as set:\n{text}"
            );
        }
        assert!(
            !common::cli::prose(&text).contains("history"),
            "a history was written:\n{text}"
        );
    }
}

#[test]
fn set_history_commits_only_its_own_change() {
    let w = two_members();
    let hand = format!(
        "{}\n[integrations.vscode-workspace]\nenabled = false\n",
        working(&w)
    );
    std::fs::write(project(&w).join("rwv.toml"), hand).unwrap();
    std::fs::write(project(&w).join("notes.txt"), "untracked\n").unwrap();
    std::fs::write(project(&w).join("README"), "tracked\n").unwrap();
    git_in(project(&w), &["add", "README"]);
    git_in(project(&w), &["commit", "-qm", "readme"]);
    std::fs::write(project(&w).join("README"), "edited\n").unwrap();
    let before = commits(&project(&w));

    let out = succeed(&["branch", "set-history", "merge"], &w.workspace);

    assert!(
        stderr(&out).contains("rwv branch set-history: projects/proj: history unset -> merge"),
        "{}",
        stderr(&out)
    );
    assert_eq!(commits(&project(&w)), before + 1);
    assert_eq!(changed_files(&project(&w)), ["rwv.toml"]);
    let head = committed(&project(&w));
    assert_eq!(parsed(&head).history, Some(History::Merge), "{head}");
    assert!(
        !head.contains("vscode-workspace"),
        "the other edit of rwv.toml was committed:\n{head}"
    );
    let now = working(&w);
    assert_eq!(parsed(&now).history, Some(History::Merge), "{now}");
    assert!(
        now.contains("vscode-workspace"),
        "the other edit was lost:\n{now}"
    );
    assert!(project(&w).join("notes.txt").exists());
    let status = git_in(project(&w), &["status", "--porcelain"]);
    assert!(
        status.contains("rwv.toml") && status.contains("notes.txt") && status.contains("README"),
        "{status}"
    );
    assert_eq!(
        std::fs::read_to_string(project(&w).join("README")).unwrap(),
        "edited\n"
    );
    assert!(
        stderr(&out).contains("uncommitted files left as they are: README"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_members_setting_overrides_the_top_level_one_and_unset_falls_back() {
    let w = two_members();
    let original = committed(&project(&w));
    let start = commits(&project(&w));

    succeed(&["branch", "set-history", "linear"], &w.workspace);
    succeed(
        &["branch", "set-history", "merge", "--repo", LIB],
        &w.workspace,
    );
    let head = committed(&project(&w));
    assert_eq!(
        effective(&head),
        (
            Some(History::Linear),
            Some(History::Linear),
            Some(History::Merge)
        ),
        "{head}"
    );

    succeed(
        &["branch", "set-history", "--unset", "--repo", LIB],
        &w.workspace,
    );
    let head = committed(&project(&w));
    assert_eq!(
        effective(&head),
        (
            Some(History::Linear),
            Some(History::Linear),
            Some(History::Linear)
        ),
        "{head}"
    );
    assert_eq!(parsed(&head).get_entry(&member(LIB)).unwrap().history, None);

    succeed(
        &[
            "branch",
            "set-history",
            "--unset",
            "--repo",
            "projects/proj",
        ],
        &w.workspace,
    );
    let head = committed(&project(&w));
    assert_eq!(effective(&head), (None, None, None), "{head}");
    assert_eq!(head, original, "rwv.toml returns to its prior bytes");
    assert_eq!(commits(&project(&w)), start + 4, "one commit per change");

    let out = succeed(&["branch", "set-history", "--unset"], &w.workspace);
    assert!(
        stderr(&out).contains("nothing to change"),
        "{}",
        stderr(&out)
    );
    assert_eq!(commits(&project(&w)), start + 4);
}

#[test]
fn an_unknown_value_refuses_at_the_verb_and_in_a_hand_edited_file() {
    let w = two_members();
    let before = (commits(&project(&w)), working(&w));

    let out = run(&["branch", "set-history", "rebase"], &w.workspace);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(stderr(&out).contains("ff-only"), "{}", stderr(&out));
    let out = run(&["branch", "set-history", "merge", "--unset"], &w.workspace);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert_eq!((commits(&project(&w)), working(&w)), before);

    let hand = format!("history = \"rebase\"\n{}", working(&w));
    std::fs::write(project(&w).join("rwv.toml"), &hand).unwrap();
    for args in [
        &["lock", "--commit"][..],
        &["branch", "set-history", "merge"],
    ] {
        let out = run(args, &w.workspace);
        assert!(
            !out.status.success(),
            "{args:?} accepted it:\n{}",
            stderr(&out)
        );
        assert!(
            stderr(&out).contains("unknown variant `rebase`"),
            "{args:?}:\n{}",
            stderr(&out)
        );
    }
    assert_eq!(commits(&project(&w)), before.0);
    assert_eq!(working(&w), hand);
}

#[test]
fn a_member_the_manifest_does_not_list_or_another_projects_repo_is_refused() {
    let w = two_members();
    let before = (commits(&project(&w)), working(&w));

    let out = run(
        &[
            "branch",
            "set-history",
            "merge",
            "--repo",
            "github/acme/nope",
        ],
        &w.workspace,
    );
    refused(&out, repoweave::refusal::RefusalKind::RepoNotInManifest);
    let out = run(
        &["branch", "set-history", "merge", "--repo", "projects/other"],
        &w.workspace,
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("projects/proj"), "{}", stderr(&out));

    assert_eq!((commits(&project(&w)), working(&w)), before);
}

#[test]
fn set_history_refuses_a_detached_project_repo_and_an_unrecorded_branch_before_writing() {
    let w = two_members();
    let before = working(&w);
    git_in(project(&w), &["switch", "-q", "--detach"]);
    let head = git_in(project(&w), &["rev-parse", "HEAD"]);

    let out = run(&["branch", "set-history", "merge"], &w.workspace);
    refused(&out, State::AlignmentProjectDetached);
    assert_eq!(working(&w), before);
    assert_eq!(git_in(project(&w), &["rev-parse", "HEAD"]), head);

    git_in(project(&w), &["switch", "-q", "-c", "unrecorded"]);
    let out = run(&["branch", "set-history", "merge"], &w.workspace);
    refused(&out, State::TargetUnrecorded);
    assert_eq!(working(&w), before);
    assert_eq!(git_in(project(&w), &["rev-parse", "HEAD"]), head);
}

#[test]
fn lock_commit_converges_a_hand_edited_history() {
    let w = two_members();
    let lib_entry = format!("[repositories.\"{LIB}\"]\n");
    let text = working(&w);
    assert!(text.contains(&lib_entry), "fixture layout:\n{text}");
    let hand = format!(
        "history = \"ff-only\"\n{}",
        text.replace(&lib_entry, &format!("{lib_entry}history = \"merge\"\n"))
    );
    std::fs::write(project(&w).join("rwv.toml"), hand).unwrap();
    let before = commits(&project(&w));

    let out = succeed(&["lock", "--commit"], &w.workspace);

    let text = stderr(&out);
    assert!(
        text.contains("rwv lock: projects/proj: history unset -> ff-only"),
        "{text}"
    );
    assert!(
        text.contains(&format!("rwv lock: {LIB}: history unset -> merge")),
        "{text}"
    );
    assert_eq!(commits(&project(&w)), before + 1);
    let head = committed(&project(&w));
    assert_eq!(
        effective(&head),
        (
            Some(History::FfOnly),
            Some(History::FfOnly),
            Some(History::Merge)
        ),
        "{head}"
    );
    let status = git_in(project(&w), &["status", "--porcelain"]);
    assert!(!status.contains("rwv.toml"), "{status}");
}

#[test]
fn a_changed_member_history_is_reported_until_published() {
    let w = two_members();
    git_in(project(&w), &["push", "-q", "origin", "HEAD:main"]);
    git_in(project(&w), &["fetch", "-q", "origin"]);
    git_in(
        project(&w),
        &["branch", "-q", "--set-upstream-to", "origin/main"],
    );
    succeed(
        &["branch", "set-history", "merge", "--repo", LIB],
        &w.workspace,
    );

    let out = succeed(&["status"], &w.workspace);

    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), stderr(&out));
    assert!(
        text.contains(&format!("{LIB}: member-changed history: unset -> merge")),
        "{text}"
    );
    assert!(
        text.contains(&format!("rwv branch set-history --unset --repo {LIB}")),
        "the restore is the unsetting spelling:\n{text}"
    );
}

fn second_project(w: &PushWorkspace, name: &str, top_level: Option<&str>) -> PathBuf {
    let lib_entry = format!("[repositories.\"{LIB}\"]\n");
    let ours = working(w);
    let lib = &ours[ours.find(&lib_entry).unwrap()..];
    let lib = &lib[..lib[lib_entry.len()..]
        .find("[repositories.")
        .map_or(lib.len(), |end| end + lib_entry.len())];
    let dir = w.workspace.join("projects").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git_in(&dir, &["init", "-q", "-b", "main"]);
    git_in(&dir, &["config", "user.email", "test@test.com"]);
    git_in(&dir, &["config", "user.name", "Test"]);
    let head = top_level.map_or(String::new(), |h| format!("history = \"{h}\"\n"));
    std::fs::write(dir.join("rwv.toml"), format!("{head}[repositories]\n{lib}")).unwrap();
    git_in(&dir, &["add", "-A"]);
    git_in(&dir, &["commit", "-qm", "project"]);
    common::record_trunk(&dir);
    dir
}

fn commit_history_of_lib(dir: &Path, history: &str) {
    let text = std::fs::read_to_string(dir.join("rwv.toml")).unwrap();
    let lib_entry = format!("[repositories.\"{LIB}\"]\n");
    std::fs::write(
        dir.join("rwv.toml"),
        text.replace(&lib_entry, &format!("{lib_entry}history = \"{history}\"\n")),
    )
    .unwrap();
    git_in(dir, &["commit", "-qam", "lib history"]);
}

fn history_disagreements(w: &PushWorkspace) -> Vec<(String, Vec<String>)> {
    let out = run(&["doctor", "--json", "--all"], &w.workspace);
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("doctor --json prints JSON ({e}):\n{}", stderr(&out)));
    doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "history-disagreement")
        .map(|v| {
            (
                v["path"].as_str().unwrap().to_owned(),
                v["histories"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|h| {
                        format!(
                            "{} {}: {}",
                            h["project"].as_str().unwrap(),
                            h["branch"].as_str().unwrap(),
                            h["history"].as_str().unwrap()
                        )
                    })
                    .collect(),
            )
        })
        .collect()
}

#[test]
fn doctor_reports_history_disagreement_only_where_two_projects_record_different_values() {
    let w = two_members();
    let other = second_project(&w, "other", None);
    assert_eq!(history_disagreements(&w), vec![], "neither records one");

    commit_history_of_lib(&project(&w), "merge");
    assert_eq!(
        history_disagreements(&w),
        vec![],
        "only one project records one"
    );

    git_in(project(&w), &["switch", "-q", "-c", "feat"]);
    let text = std::fs::read_to_string(project(&w).join("rwv.toml")).unwrap();
    std::fs::write(
        project(&w).join("rwv.toml"),
        text.replace("history = \"merge\"", "history = \"linear\""),
    )
    .unwrap();
    git_in(project(&w), &["commit", "-qam", "feat history"]);
    git_in(project(&w), &["switch", "-q", "main"]);
    common::record_feature(&project(&w), "feat", "main");
    assert_eq!(
        history_disagreements(&w),
        vec![],
        "two branches of one project differing is no disagreement"
    );

    commit_history_of_lib(&other, "merge");
    assert_eq!(
        history_disagreements(&w),
        vec![(
            LIB.to_owned(),
            vec![
                "other main: merge".to_owned(),
                "proj feat: linear".to_owned(),
                "proj main: merge".to_owned(),
            ]
        )],
        "proj's feat records linear where other records merge"
    );

    let text = std::fs::read_to_string(other.join("rwv.toml")).unwrap();
    std::fs::write(
        other.join("rwv.toml"),
        format!(
            "history = \"linear\"\n{}",
            text.replace("history = \"merge\"\n", "")
        ),
    )
    .unwrap();
    git_in(&other, &["commit", "-qam", "top-level linear"]);
    assert_eq!(
        history_disagreements(&w),
        vec![(
            LIB.to_owned(),
            vec![
                "other main: linear".to_owned(),
                "proj feat: linear".to_owned(),
                "proj main: merge".to_owned(),
            ]
        )],
        "a top-level setting is what a member without its own records"
    );

    git_in(project(&w), &["branch", "-q", "-D", "feat"]);
    commit_history_of_lib(&other, "merge");
    assert_eq!(
        history_disagreements(&w),
        vec![],
        "both projects' live branches record merge for it"
    );
}
