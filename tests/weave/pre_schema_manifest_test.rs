//! The member-entry keys an earlier release wrote — `version` for the declared
//! branch and `submodules` for the recursion setting — are never read.
//!
//! At the checked-out tip a project naming one fails to load as
//! `unparseable-project`, naming `rwv doctor --fix`, which renames the keys in
//! HEAD's file and in the working file and commits only that rename. At any
//! other commit the file declares nothing. Each prohibition is pinned here
//! from the side that would break it: the parser, the verb, the repair and the
//! reader of another commit.

use assert_cmd::Command;
use repoweave::manifest::{
    rewrite_pre_schema_keys, Manifest, PreSchemaManifest, SubmoduleRecursion,
};
use repoweave::refusal::kind_of;
use std::path::{Path, PathBuf};

use crate::common;

use repoweave::diagnosis::State;

const OLD_MANIFEST: &str = "\
# The members of this project.
[repositories.\"github/acme/server\"]
type = \"git\"
url = \"https://github.com/acme/server.git\"
version = \"main\" # tracked
role = \"owned\"
submodules = \"skip\"

[repositories.\"github/acme/lib\"]
type = \"git\"
url = \"https://github.com/acme/lib.git\"
version = \"develop\"
role = \"dependency\"
submodules = \"local\"
";

const NEW_MANIFEST: &str = "\
# The members of this project.
[repositories.\"github/acme/server\"]
type = \"git\"
url = \"https://github.com/acme/server.git\"
branch = \"main\" # tracked
role = \"owned\"
recurse-submodules = false

[repositories.\"github/acme/lib\"]
type = \"git\"
url = \"https://github.com/acme/lib.git\"
branch = \"develop\"
role = \"dependency\"
";

struct Weave {
    root: PathBuf,
    project: PathBuf,
}

fn rwv(weave: &Weave) -> Command {
    let mut cmd = common::rwv();
    cmd.current_dir(&weave.root);
    cmd
}

fn project_git(weave: &Weave, args: &[&str]) -> String {
    common::git_in(&weave.project, args)
}

fn weave_committing(parent: &Path, manifest: &str) -> Weave {
    let root = parent.join("ws");
    let project = root.join("projects/web-app");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    common::set_up_lock_driver(&project);
    std::fs::write(project.join("rwv.toml"), manifest).unwrap();
    std::fs::write(project.join("notes.txt"), "kept\n").unwrap();
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);
    common::record_trunk(&project);
    Weave { root, project }
}

fn assert_routes_to(stderr: &str, token: &str) {
    let routed: Vec<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("rwv explain "))
        .collect();
    assert_eq!(routed, vec![token], "stderr was:\n{stderr}");
}

fn commits(weave: &Weave) -> usize {
    project_git(weave, &["rev-list", "--count", "HEAD"])
        .parse()
        .unwrap()
}

#[test]
fn the_old_member_keys_are_refused_at_the_checked_out_tip_and_never_read() {
    for old in [
        "[repositories.\"github/acme/server\"]\ntype = \"git\"\nurl = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n",
        "[repositories.\"github/acme/server\"]\ntype = \"git\"\nurl = \"https://github.com/acme/server.git\"\nbranch = \"main\"\nrole = \"owned\"\nsubmodules = \"skip\"\n",
    ] {
        let err = Manifest::from_toml_str(old).expect_err("an earlier release's key is not read");
        assert!(
            err.is::<PreSchemaManifest>(),
            "the refusal is the typed pre-schema one, not a generic parse failure: {err:#}"
        );
        assert_eq!(
            kind_of(&err),
            None,
            "the refusal is a state of the table, so no token names it: {err:#}"
        );
        assert!(
            format!("{err:#}").contains("`rwv doctor --fix`"),
            "the refusal names its fix: {err:#}"
        );
    }
    Manifest::from_toml_str(NEW_MANIFEST).expect("control: the current spelling loads");

    let tmp = common::tempdir().unwrap();
    let weave = weave_committing(tmp.path(), OLD_MANIFEST);
    let assert = rwv(&weave).arg("lock").assert().failure();
    let stderr: String = String::from_utf8_lossy(&assert.get_output().stderr)
        .lines()
        .filter(|line| !line.starts_with("target:"))
        .map(|line| format!("{line}\n"))
        .collect();
    assert_eq!(
        stderr,
        "rwv lock: failed to load project 'web-app'\n\
         rwv lock: projects/web-app: unparseable-project here\n\
         \x20   github/acme/lib: `version` is now `branch`\n\
         \x20   github/acme/lib: `submodules` is now `recurse-submodules`\n\
         \x20   github/acme/server: `version` is now `branch`\n\
         \x20   github/acme/server: `submodules` is now `recurse-submodules`\n\
         choices:\n\
         \x20 rewrite the keys an earlier release wrote, committing only that\n\
         \x20   rwv doctor --fix\n\
         \n\
         rwv explain unparseable-project\n",
        "a verb reading the file at the tip refuses with the kind's envelope"
    );
    assert_routes_to(&stderr, "unparseable-project");
}

#[test]
fn the_refusal_carries_axis_state_listing_and_choices_under_json() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_committing(tmp.path(), OLD_MANIFEST);
    let assert = rwv(&weave).args(["update", "--json"]).assert().failure();
    let envelope: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout)
        .expect("a verb refused before printing anything prints the refusal envelope");
    let refused = envelope["refused"].as_array().expect("refused is an array");
    assert_eq!(refused.len(), 1, "{envelope}");
    let line = &refused[0];
    assert_eq!(line["path"], "projects/web-app");
    assert_eq!(line["axis"], "unparseable-project");
    assert_eq!(line["state"], "here");
    assert_eq!(
        line["listed"],
        serde_json::json!([
            "github/acme/lib: `version` is now `branch`",
            "github/acme/lib: `submodules` is now `recurse-submodules`",
            "github/acme/server: `version` is now `branch`",
            "github/acme/server: `submodules` is now `recurse-submodules`",
        ])
    );
    assert_eq!(line["facts"], serde_json::json!([]));
    assert_eq!(
        line["choices"],
        serde_json::json!([{
            "id": null,
            "phrase": "rewrite the keys an earlier release wrote, committing only that",
            "where": null,
            "run": ["rwv doctor --fix"],
        }])
    );
}

#[test]
fn doctor_reports_the_old_keys_as_the_one_unparseable_project_it_repairs() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_committing(tmp.path(), OLD_MANIFEST);
    let assert = rwv(&weave).args(["doctor", "--json"]).assert().failure();
    let report: serde_json::Value =
        serde_json::from_slice(&assert.get_output().stdout).expect("doctor --json prints JSON");
    let finding = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "unparseable-project")
        .unwrap_or_else(|| panic!("no unparseable-project finding in {report}"));
    assert_eq!(finding["sub_kind"], "pre-schema");
    let message = finding["message"].as_str().unwrap();
    assert!(
        message.starts_with("failed to parse rwv.toml at ")
            && message.contains("rwv.toml names member-entry keys an earlier release wrote"),
        "the finding names the file and what is wrong with it: {message}"
    );

    let malformed = weave_committing(
        common::tempdir().unwrap().keep().as_path(),
        "[repositories.\"github/acme/server\"\ntype = \"git\"\n",
    );
    let assert = rwv(&malformed)
        .args(["doctor", "--json"])
        .assert()
        .failure();
    let report: serde_json::Value = serde_json::from_slice(&assert.get_output().stdout).unwrap();
    let finding = report["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "unparseable-project")
        .unwrap_or_else(|| panic!("no unparseable-project finding in {report}"));
    assert_eq!(
        finding["sub_kind"], "malformed",
        "a file that is broken for any other reason stays the operator's to edit"
    );
}

#[test]
fn doctor_fix_commits_only_the_rewrite_and_leaves_the_other_edits_in_place() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_committing(tmp.path(), OLD_MANIFEST);
    let before = commits(&weave);

    let hand_edited = OLD_MANIFEST.replace("# The members", "# Hand-edited. The members");
    std::fs::write(weave.project.join("rwv.toml"), &hand_edited).unwrap();
    std::fs::write(weave.project.join("notes.txt"), "kept, and edited\n").unwrap();

    rwv(&weave).args(["doctor", "--fix"]).assert().success();

    assert_eq!(
        commits(&weave),
        before + 1,
        "the repair is one commit: {}",
        project_git(&weave, &["log", "--oneline"])
    );
    assert_eq!(
        project_git(&weave, &["show", "--name-only", "--format=", "HEAD"]),
        "rwv.toml",
        "the commit touches the manifest and nothing else"
    );
    assert_eq!(
        project_git(&weave, &["show", "HEAD:rwv.toml"]),
        NEW_MANIFEST.trim_end(),
        "HEAD's file is HEAD's old file with only the keys renamed, comments kept and the \
         hand edit left out"
    );
    assert_eq!(
        std::fs::read_to_string(weave.project.join("rwv.toml")).unwrap(),
        NEW_MANIFEST.replace("# The members", "# Hand-edited. The members"),
        "the working file is renamed in place with the hand edit still in it"
    );
    assert_eq!(
        std::fs::read_to_string(weave.project.join("notes.txt")).unwrap(),
        "kept, and edited\n"
    );
    assert_eq!(
        project_git(&weave, &["diff", "--name-only"]),
        "notes.txt\nrwv.toml",
        "both uncommitted edits are still uncommitted"
    );
    assert_eq!(
        project_git(&weave, &["diff", "--cached", "--name-only"]),
        "",
        "and nothing is left staged"
    );
    Manifest::from_path(&weave.project.join("rwv.toml")).expect("the repaired file loads");
}

#[test]
fn doctor_fix_withholds_while_another_file_is_staged() {
    let tmp = common::tempdir().unwrap();
    let weave = weave_committing(tmp.path(), OLD_MANIFEST);
    let before = commits(&weave);
    std::fs::write(weave.project.join("notes.txt"), "staged\n").unwrap();
    project_git(&weave, &["add", "notes.txt"]);

    let assert = rwv(&weave).args(["doctor", "--fix"]).assert().failure();

    let output = String::from_utf8_lossy(&assert.get_output().stdout).into_owned()
        + &String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        output.contains("rwv.toml not rewritten") && output.contains("staged changes"),
        "{output}"
    );
    assert_eq!(commits(&weave), before, "nothing was committed");
    assert_eq!(
        std::fs::read_to_string(weave.project.join("rwv.toml")).unwrap(),
        OLD_MANIFEST,
        "nothing was rewritten, so a re-run converges once the staged file is dealt with"
    );
    assert_eq!(
        project_git(&weave, &["diff", "--cached", "--name-only"]),
        "notes.txt"
    );
}

#[test]
fn a_default_recursion_is_not_written_out() {
    let manifest = Manifest::from_toml_str(NEW_MANIFEST).unwrap();
    let lib = manifest
        .iter_entries()
        .find(|(path, _)| path.as_str() == "github/acme/lib")
        .unwrap()
        .1;
    assert_eq!(lib.recurse_submodules, SubmoduleRecursion::Recurse);
    let server = manifest
        .iter_entries()
        .find(|(path, _)| path.as_str() == "github/acme/server")
        .unwrap()
        .1;
    assert_eq!(server.recurse_submodules, SubmoduleRecursion::Skip);

    let tmp = common::tempdir().unwrap();
    let path = tmp.path().join("rwv.toml");
    manifest.write(&path).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        written.matches("recurse-submodules").count(),
        1,
        "only the member that opted out carries the key:\n{written}"
    );
    assert!(written.contains("recurse-submodules = false"), "{written}");

    let spelled_default = NEW_MANIFEST.replace(
        "role = \"dependency\"\n",
        "role = \"dependency\"\nrecurse-submodules = true\n",
    );
    let explicit = Manifest::from_toml_str(&spelled_default).unwrap();
    explicit.write(&path).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path)
            .unwrap()
            .matches("recurse-submodules")
            .count(),
        1,
        "`recurse-submodules = true` reads back and is written out as the default: absent"
    );

    let renamed = rewrite_pre_schema_keys(OLD_MANIFEST).unwrap().unwrap();
    assert_eq!(
        renamed.matches("recurse-submodules").count(),
        1,
        "`submodules = \"local\"` is what an absent key means, so the rename drops it:\n{renamed}"
    );
}

#[test]
fn a_pre_schema_file_at_another_commit_declares_nothing() {
    let declared = Manifest::from_other_commit(OLD_MANIFEST).expect("an old file is not refused");
    assert!(
        declared.is_empty() && declared.integrations.is_empty() && declared.workweave.is_none(),
        "no member, integration or setting of it is read"
    );
    assert!(
        Manifest::from_other_commit("[repositories.\"github/acme/server\"\ntype = \"git\"\n")
            .is_err(),
        "a file broken for any other reason is still refused"
    );
    assert_eq!(
        Manifest::from_other_commit(NEW_MANIFEST).unwrap().len(),
        2,
        "control: the current spelling is read in full"
    );
}

const SERVER_PATH: &str = "github/chatly/server";
const SERVER_URL: &str = "https://github.com/chatly/server.git";

struct Workspace {
    root: PathBuf,
    project: PathBuf,
    server: PathBuf,
}

fn commit_all(repo: &Path, file: &str, content: &str, message: &str) -> String {
    std::fs::write(repo.join(file), content).unwrap();
    common::git_in(repo, &["add", file]);
    common::git_in(repo, &["commit", "-m", message]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn primary_and_workweave(parent: &Path) -> (Workspace, Workspace, String) {
    let root = parent.join("primary");
    let server = root.join(SERVER_PATH);
    std::fs::create_dir_all(&server).unwrap();
    common::git_in(&server, &["init", "-b", "main"]);
    let pinned = commit_all(&server, "README.md", "init\n", "initial");

    let project = root.join("projects/web-app");
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    common::set_up_lock_driver(&project);
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories.\"{SERVER_PATH}\"]\ntype = \"git\"\nurl = \"{SERVER_URL}\"\n\
             version = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    common::fixture_lock(&project, &[(SERVER_PATH, SERVER_URL, &pinned)]);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "lock: initial"]);
    common::record_trunk(&project);
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    let ww_root = parent.join("ww");
    let ww_server = ww_root.join(SERVER_PATH);
    let ww_project = ww_root.join("projects/web-app");
    std::fs::create_dir_all(ww_server.parent().unwrap()).unwrap();
    std::fs::create_dir_all(ww_project.parent().unwrap()).unwrap();
    common::git_in(
        &server,
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );
    common::git_in(
        &project,
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );
    common::record_feature(&project, "ww/project", "main");
    std::fs::write(ww_root.join(".rwv-active"), "web-app\n").unwrap();

    (
        Workspace {
            root,
            project,
            server,
        },
        Workspace {
            root: ww_root,
            project: ww_project,
            server: ww_server,
        },
        pinned,
    )
}

#[test]
fn a_landing_into_a_target_whose_tip_names_the_old_keys_refuses_naming_doctor_fix() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _pinned) = primary_and_workweave(tmp.path());

    let mut doctor = common::rwv();
    doctor.current_dir(&ww.root).args(["doctor", "--fix"]);
    doctor.assert().success();
    assert!(
        Manifest::from_path(&ww.project.join("rwv.toml")).is_ok(),
        "the workweave was repaired"
    );
    assert!(
        Manifest::from_path(&primary.project.join("rwv.toml")).is_err(),
        "the target was not"
    );

    let advanced = commit_all(&ww.server, "change.txt", "workweave change\n", "ww: change");
    common::fixture_lock(&ww.project, &[(SERVER_PATH, SERVER_URL, &advanced)]);
    common::git_in(&ww.project, &["add", "rwv.lock"]);
    common::git_in(&ww.project, &["commit", "-m", "lock: ww change"]);

    let before = common::git_in(&primary.server, &["rev-parse", "main"]);
    let out = common::cli::run(&["land", "--strategy", "rebase"], &ww.root);
    common::cli::refused(&out, State::UnparseableProjectAt);
    assert_eq!(
        common::git_in(&primary.server, &["rev-parse", "main"]),
        before,
        "a landing into a pre-schema tip moves nothing"
    );
    let _ = advanced;
}

#[test]
fn the_rewrite_keeps_every_spelling_a_hand_authored_entry_can_take() {
    let inline = "[repositories]\n\"github/acme/server\" = { type = \"git\", url = \"https://github.com/acme/server.git\", version = \"main\", role = \"owned\", submodules = \"skip\" }\n";
    let renamed = rewrite_pre_schema_keys(inline).unwrap().unwrap();
    assert_eq!(
        renamed,
        "[repositories]\n\"github/acme/server\" = { type = \"git\", url = \"https://github.com/acme/server.git\", branch = \"main\", role = \"owned\", recurse-submodules = false }\n"
    );
    Manifest::from_toml_str(&renamed).expect("the rewrite of an inline entry loads");

    let dotted = "[repositories]\n\"github/acme/server\".type = \"git\"\n\"github/acme/server\".url = \"https://github.com/acme/server.git\"\n\"github/acme/server\".role = \"owned\"\n# the tracked branch\n\"github/acme/server\".version = \"main\"\n";
    let renamed = rewrite_pre_schema_keys(dotted).unwrap().unwrap();
    assert_eq!(
        renamed,
        dotted.replace(".version = ", ".branch = "),
        "the key is renamed where it stands and nothing around it moves"
    );
    Manifest::from_toml_str(&renamed).expect("the rewrite of a dotted-key entry loads");

    assert_eq!(
        rewrite_pre_schema_keys(NEW_MANIFEST).unwrap(),
        None,
        "a file that names none is left alone"
    );
}

#[test]
fn the_rewrite_declines_what_it_would_have_to_guess() {
    let both = "[repositories.\"github/acme/server\"]\ntype = \"git\"\nurl = \"https://github.com/acme/server.git\"\nversion = \"main\"\nbranch = \"dev\"\nrole = \"owned\"\n";
    let err = rewrite_pre_schema_keys(both).unwrap_err().to_string();
    assert!(
        err.contains("github/acme/server") && err.contains("`version`"),
        "{err}"
    );

    let odd = "[repositories.\"github/acme/server\"]\ntype = \"git\"\nurl = \"https://github.com/acme/server.git\"\nbranch = \"main\"\nrole = \"owned\"\nsubmodules = \"sometimes\"\n";
    let err = rewrite_pre_schema_keys(odd).unwrap_err().to_string();
    assert!(err.contains("neither \"local\" nor \"skip\""), "{err}");
}
