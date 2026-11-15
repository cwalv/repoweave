//! A workweave whose own checkout of the project still holds what an earlier
//! release wrote is named by the primary's `rwv doctor --all`, with the line
//! that migrates it, which only that workweave's own `doctor --fix` does.

use std::path::Path;
use std::process::Output;

use crate::common;

const PROJECT: &str = "app";
const LIB: &str = "local/owner/lib";

fn rwv(dir: &Path, args: &[&str]) -> Output {
    common::rwv().args(args).current_dir(dir).output().unwrap()
}

fn unmigrated(primary: &Path) -> Vec<serde_json::Value> {
    let out = rwv(primary, &["doctor", "--all", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    json["violations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| v["kind"] == "unmigrated-workweave")
        .cloned()
        .collect()
}

/// The rehearsal's primary `rwv doctor --fix`, then `rwv doctor --all`, over
/// a seat whose branch still carries the earlier release's `version` keys and
/// `merge=rwv-ours` line: the primary names the seat and the line, and the
/// line run as printed migrates it.
#[test]
fn the_primary_names_a_workweave_an_earlier_release_left_unmigrated() {
    let tmp = common::tempdir().unwrap();
    let upstream = tmp.path().join("upstream/owner/lib");
    std::fs::create_dir_all(&upstream).unwrap();
    common::git_in(&upstream, &["init", "-b", "main"]);
    std::fs::write(upstream.join("README.md"), "init\n").unwrap();
    common::git_in(&upstream, &["add", "-A"]);
    common::git_in(&upstream, &["commit", "-m", "initial"]);

    let primary = tmp.path().join("ws");
    let project = primary.join("projects").join(PROJECT);
    common::git_in(
        tmp.path(),
        &[
            "clone",
            &common::file_url(&upstream),
            &common::under(&primary, LIB).to_string_lossy(),
        ],
    );
    std::fs::create_dir_all(&project).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    common::set_up_lock_driver(&project);
    std::fs::write(project.join(".gitignore"), "*.code-workspace\n").unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories.\"{LIB}\"]\ntype = \"git\"\nurl = \"{}\"\nbranch = \"main\"\nrole = \"owned\"\n",
            common::file_url(&upstream)
        ),
    )
    .unwrap();
    std::fs::write(primary.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    common::record_trunk(&project);
    common::pin_every_member(&primary);
    common::git_in(&project, &["add", "-A"]);
    common::git_in(&project, &["commit", "-m", "initial"]);
    let created = rwv(
        &primary,
        &["workweave", "create", "--project", PROJECT, "s1"],
    );
    assert!(
        created.status.success(),
        "{}",
        common::cli::describe(&created)
    );
    assert!(unmigrated(&primary).is_empty());

    let seat_project = tmp
        .path()
        .join(".workweaves")
        .join(format!("{PROJECT}--s1"))
        .join("projects")
        .join(PROJECT);
    let manifest = std::fs::read_to_string(seat_project.join("rwv.toml")).unwrap();
    std::fs::write(
        seat_project.join("rwv.toml"),
        manifest.replace("branch = \"main\"", "version = \"main\""),
    )
    .unwrap();
    let attributes = std::fs::read_to_string(seat_project.join(".gitattributes")).unwrap();
    std::fs::write(
        seat_project.join(".gitattributes"),
        attributes.replace("merge=rwv-lock", "merge=rwv-ours"),
    )
    .unwrap();
    common::git_in(
        &seat_project,
        &["commit", "-qam", "an earlier release's keys"],
    );

    let found = unmigrated(&primary);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(
        found[0]["parts"],
        serde_json::json!(["pre-schema-keys", "lock-attribute"])
    );
    let text = rwv(&primary, &["doctor", "--all"]);
    let stdout = String::from_utf8_lossy(&text.stdout);
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("rwv -C") && line.ends_with("doctor --fix"))
        .unwrap_or_else(|| panic!("no migration line in:\n{stdout}"));
    let ran = common::run_advised(line, &primary);
    assert!(ran.status.success(), "{}", common::cli::describe(&ran));
    assert!(unmigrated(&primary).is_empty());
}
