//! `rwv integration set-enabled` and `rwv integration set`: each changes one key
//! of an `[integrations.<name>]` table, commits that change and what its
//! regeneration wrote, and nothing else the project repo holds uncommitted.
//! Enabling authors the managed files; disabling deletes nothing.

use std::path::PathBuf;

use crate::common;
use common::{git_in, PushWorkspace};

const APP: &str = "github/acme/app";

fn weave() -> PushWorkspace {
    common::build_workspace("proj", &[(APP, "owned")])
}

fn project(w: &PushWorkspace) -> PathBuf {
    w.workspace.join("projects").join(&w.project_name)
}

fn run(w: &PushWorkspace, args: &[&str]) -> std::process::Output {
    common::rwv()
        .args(args)
        .current_dir(&w.workspace)
        .output()
        .expect("rwv runs")
}

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn succeeded(out: &std::process::Output) -> String {
    assert!(out.status.success(), "{}", text(out));
    text(out)
}

fn commits(w: &PushWorkspace) -> usize {
    git_in(project(w), &["rev-list", "--count", "HEAD"])
        .trim()
        .parse()
        .unwrap()
}

fn committed_manifest(w: &PushWorkspace) -> String {
    git_in(project(w), &["show", "HEAD:rwv.toml"])
}

fn status(w: &PushWorkspace) -> String {
    git_in(
        project(w),
        &["status", "--porcelain", "--untracked-files=all"],
    )
}

#[test]
fn set_enabled_authors_the_managed_files_and_commits_them_with_the_key() {
    let w = weave();
    let before = commits(&w);

    succeeded(&run(&w, &["integration", "set-enabled", "gita", "true"]));

    assert_eq!(commits(&w), before + 1);
    assert!(committed_manifest(&w).contains("[integrations.gita]\nenabled = true"));
    assert!(project(&w).join("gita/repos.csv").is_file());
    let tracked = git_in(project(&w), &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(tracked.contains("gita/repos.csv"), "{tracked}");
    assert_eq!(status(&w), "");
}

#[test]
fn set_enabled_false_deletes_nothing_and_names_materialize() {
    let w = weave();
    succeeded(&run(&w, &["integration", "set-enabled", "gita", "true"]));

    let said = succeeded(&run(&w, &["integration", "set-enabled", "gita", "false"]));

    assert!(committed_manifest(&w).contains("[integrations.gita]\nenabled = false"));
    assert!(
        project(&w).join("gita/repos.csv").is_file(),
        "nothing was deleted"
    );
    assert!(said.contains("rwv materialize"), "{said}");
    let materialized = succeeded(&run(&w, &["materialize"]));
    assert!(materialized.contains("gita"), "{materialized}");
}

#[test]
fn set_commits_one_key_and_leaves_another_hand_edit_uncommitted() {
    let w = weave();
    let manifest = project(&w).join("rwv.toml");
    let hand = format!(
        "{}\n# a hand edit\n",
        std::fs::read_to_string(&manifest).unwrap()
    );
    std::fs::write(&manifest, hand).unwrap();
    let before = commits(&w);

    succeeded(&run(
        &w,
        &[
            "integration",
            "set",
            "static-files",
            "files",
            r#"["turbo.json"]"#,
        ],
    ));

    assert_eq!(commits(&w), before + 1);
    let committed = committed_manifest(&w);
    assert!(
        committed.contains("files = [\"turbo.json\"]"),
        "{committed}"
    );
    assert!(!committed.contains("a hand edit"), "{committed}");
    assert!(status(&w).contains("M rwv.toml"), "{}", status(&w));
}

#[test]
fn a_name_or_key_no_integration_takes_refuses_naming_the_accepted_ones() {
    let w = weave();
    let before = commits(&w);

    let name = run(&w, &["integration", "set-enabled", "nosuch", "true"]);
    let said = common::cli::refused(
        &name,
        repoweave::refusal::RefusalKind::UnknownIntegrationSetting,
    );
    assert!(
        said.contains("`npm-workspaces`") && said.contains("`gita`"),
        "{said}"
    );

    let key = run(&w, &["integration", "set", "static-files", "nokey", "1"]);
    let said = common::cli::refused(
        &key,
        repoweave::refusal::RefusalKind::UnknownIntegrationSetting,
    );
    assert!(
        said.contains("`enabled`") && said.contains("`files`"),
        "{said}"
    );

    let value = run(
        &w,
        &[
            "integration",
            "set",
            "cargo-workspace",
            "rustc-wrapper",
            "sccache",
        ],
    );
    let said = common::cli::refused(
        &value,
        repoweave::refusal::RefusalKind::UnknownIntegrationSetting,
    );
    assert!(said.contains("quote a string"), "{said}");

    let enabled = run(&w, &["integration", "set", "gita", "enabled", "true"]);
    let said = common::cli::refused(&enabled, repoweave::refusal::RefusalKind::InapplicableFlag);
    assert!(said.contains("rwv integration set-enabled gita"), "{said}");

    assert_eq!(commits(&w), before, "nothing was committed");
    assert_eq!(status(&w), "");
}

#[test]
fn a_value_rwv_toml_already_holds_changes_nothing() {
    let w = weave();
    succeeded(&run(&w, &["integration", "set-enabled", "gita", "true"]));
    let before = commits(&w);

    let said = succeeded(&run(&w, &["integration", "set-enabled", "gita", "true"]));

    assert!(said.contains("nothing to change"), "{said}");
    assert_eq!(commits(&w), before);
}
