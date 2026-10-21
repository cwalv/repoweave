//! `rwv doctor` names a workweave by its identity, the project and the name
//! together, on every surface.
//!
//! A workweave name is unique only within its project. The weave gives projects
//! `p1` and `p2` a workweave named `feat` each, and makes a tracked file of
//! each seat's member read-only, so each seat raises one `member-permissions`
//! finding. Keyed by the bare name, the two seats collapse onto one: the wire
//! places `p1`'s finding under `p2--feat`, where its member does not exist,
//! and both surfaces call each seat `feat`.

mod common;

use std::path::{Path, PathBuf};

const PROJECTS: [(&str, &str); 2] = [("p1", "github/acme/a"), ("p2", "github/acme/b")];
const SEAT: &str = "feat";

struct Weave {
    _tmp: tempfile::TempDir,
    primary: PathBuf,
    container: PathBuf,
}

fn rwv_ok(args: &[&str], dir: &Path) -> String {
    let out = common::rwv().args(args).current_dir(dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "`rwv {}` failed:\nstdout:\n{stdout}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn rwv_stdout(args: &[&str], dir: &Path) -> String {
    let out = common::rwv().args(args).current_dir(dir).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn two_projects_with_a_workweave_named_alike() -> Weave {
    let tmp = common::tempdir().unwrap();
    let primary = tmp.path().join("ws");
    let remotes = tmp.path().join("remotes");
    std::fs::create_dir_all(&remotes).unwrap();

    for (project, member) in PROJECTS {
        let bare = remotes.join(format!("{project}.git"));
        common::init_bare_repo_with_commit(&bare);
        let dir = primary.join("projects").join(project);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("rwv.toml"),
            format!(
                "[repositories.\"{member}\"]\ntype = \"git\"\nurl = \"{}\"\nversion = \"main\"\n\
                 role = \"owned\"\n",
                common::file_url(&bare)
            ),
        )
        .unwrap();
        common::git_in(&dir, &["init", "-q", "-b", "main"]);
        common::git_in(&dir, &["config", "user.email", "test@test.com"]);
        common::git_in(&dir, &["config", "user.name", "Test"]);
        common::git_in(&dir, &["add", "-A"]);
        common::git_in(&dir, &["commit", "-qm", "manifest"]);
    }
    std::fs::write(primary.join(".rwv-active"), "p1\n").unwrap();
    assert_eq!(
        rwv_ok(&["-C", primary.to_str().unwrap(), "resolve"], tmp.path()),
        common::operator_path_stdout(&primary),
        "the fixture must resolve to its own weave root before anything writes"
    );

    for (project, _) in PROJECTS {
        rwv_ok(&["fetch", "--project", project], &primary);
        let dir = primary.join("projects").join(project);
        common::git_in(&dir, &["add", "-A"]);
        common::git_in(&dir, &["commit", "-qm", "lock"]);
        rwv_ok(&["workweave", project, "create", SEAT], &primary);
    }

    let container = tmp.path().join(".workweaves");
    for (project, member) in PROJECTS {
        let readme = common::under(&container, &format!("{project}--{SEAT}/{member}/README"));
        let mut permissions = std::fs::metadata(&readme).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&readme, permissions).unwrap();
    }

    Weave {
        _tmp: tmp,
        primary,
        container,
    }
}

/// `(workweave, path, absolute_path)` of every `member-permissions` record.
fn permission_findings(weave: &Weave) -> Vec<(String, String, String)> {
    let stdout = rwv_stdout(&["doctor", "--json", "--all"], &weave.primary);
    let doc: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json must print JSON ({e}):\n{stdout}"));
    let mut found: Vec<(String, String, String)> = doc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "member-permissions")
        .map(|v| {
            (
                v["workweave"].as_str().unwrap().to_owned(),
                v["path"].as_str().unwrap().to_owned(),
                v["absolute_path"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    found.sort();
    found
}

#[test]
fn each_wire_record_names_its_own_workweave_and_a_path_inside_it() {
    let weave = two_projects_with_a_workweave_named_alike();
    let expected: Vec<(String, String, String)> = PROJECTS
        .iter()
        .map(|(project, member)| {
            let seat = format!("{project}--{SEAT}");
            let absolute = repoweave::path_spelling::wire_path(&common::under(
                &weave.container,
                &format!("{seat}/{member}"),
            ));
            (seat, member.to_string(), absolute)
        })
        .collect();

    assert_eq!(permission_findings(&weave), expected);
    for (_, _, absolute) in &expected {
        assert!(
            Path::new(absolute).is_dir(),
            "{absolute} must be the checkout the finding is about"
        );
    }
}

#[test]
fn the_text_report_and_its_repair_name_each_workweave_by_project_and_name() {
    let weave = two_projects_with_a_workweave_named_alike();

    let report = rwv_stdout(&["doctor", "--all"], &weave.primary);
    let fixed = rwv_stdout(&["doctor", "--fix", "--all"], &weave.primary);

    for (project, member) in PROJECTS {
        let location = format!("{project}--{SEAT}/{member}");
        assert!(
            report.contains(&format!("1 tracked file(s) of {location} are read-only")),
            "the report must locate the finding at {location}:\n{report}"
        );
        assert!(
            fixed.contains(&format!(
                "[fixed] core: settled 1 tracked file(s) of {location} for its role (owned)"
            )),
            "the repair must name {location}:\n{fixed}"
        );
    }
    assert_eq!(permission_findings(&weave), vec![]);
}
