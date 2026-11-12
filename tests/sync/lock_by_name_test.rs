//! `rwv lock` moves a pin forward only, `--repo` included: a backward pin
//! refuses and prints the by-name verb, which succeeds as printed. A member's
//! dirt refuses and prints `--dirty`, which records the committed HEAD.

use std::path::{Path, PathBuf};

use crate::common;

use common::cli::refused;
use common::{git_in, PushWorkspace};
use repoweave::diagnosis::State;

const APP: &str = "github/acme/app";
const LIB: &str = "github/acme/lib";

fn workspace() -> PushWorkspace {
    common::build_workspace("proj", &[(APP, "owned"), (LIB, "owned")])
}

fn member(w: &PushWorkspace, repo: &str) -> PathBuf {
    w.workspace.join(repo)
}

fn pin_of(w: &PushWorkspace, repo: &str) -> String {
    let text = std::fs::read_to_string(
        w.workspace
            .join("projects")
            .join(&w.project_name)
            .join("rwv.lock"),
    )
    .unwrap();
    let lock: serde_json::Value = serde_json::from_str(&text).unwrap();
    lock["repositories"][repo]["version"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    git_in(dir, &["add", file]);
    git_in(dir, &["commit", "-qm", file]);
    git_in(dir, &["rev-parse", "HEAD"])
}

/// The printed choice line beginning `prefix`, as the terminal shows it.
fn printed(text: &str, prefix: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("no `{prefix}...` line is printed:\n{text}"))
        .to_owned()
}

/// `app` sits one commit behind the pin the lock holds for it.
fn app_behind_its_pin(w: &PushWorkspace) -> String {
    let app = member(w, APP);
    let behind = git_in(&app, &["rev-parse", "HEAD"]);
    commit_in(&app, "ahead");
    common::cli::succeed(&["lock", "--commit"], &w.workspace);
    git_in(&app, &["reset", "-q", "--hard", "HEAD~1"]);
    behind
}

#[test]
fn lock_repo_narrows_only_and_refuses_a_backward_pin() {
    let w = workspace();
    app_behind_its_pin(&w);
    let pin = pin_of(&w, APP);

    for args in [
        &["lock"][..],
        &["lock", "--repo", APP],
        &["lock", "--repo", "glob:github/*/*"],
    ] {
        let out = common::cli::run(args, &w.workspace);
        let text = refused(&out, State::PinBackwards);
        assert!(!text.contains("rwv lock --repo"), "{args:?}:\n{text}");
        assert_eq!(pin_of(&w, APP), pin, "{args:?} moved the pin");
    }
}

#[test]
fn the_by_name_choice_a_backward_pin_prints_succeeds_as_printed() {
    let w = workspace();
    let behind = app_behind_its_pin(&w);

    let out = common::cli::run(&["lock"], &w.workspace);
    let text = refused(&out, State::PinBackwards);
    let choice = printed(&text, "rwv member pin");
    assert_eq!(choice, format!("rwv member pin {APP} HEAD --commit"));

    let ran = common::run_advised(&choice, &w.workspace);
    assert!(ran.status.success(), "{}", common::cli::describe(&ran));
    assert_eq!(pin_of(&w, APP), behind);
}

#[test]
fn a_dirty_member_at_lock_prints_dirty_which_records_the_committed_head() {
    let w = workspace();
    let app = member(&w, APP);
    let committed = commit_in(&app, "work");
    std::fs::write(app.join("work"), "edited\n").unwrap();

    let out = common::cli::run(&["lock"], &w.workspace);
    let text = refused(&out, State::EditsDirty);
    let choice = printed(&text, "rwv lock --dirty");
    assert_eq!(choice, "rwv lock --dirty");
    assert!(
        text.contains("record the committed HEAD, leaving these uncommitted edits out of the lock"),
        "{text}"
    );

    let ran = common::run_advised(&choice, &w.workspace);
    assert!(ran.status.success(), "{}", common::cli::describe(&ran));
    assert_eq!(pin_of(&w, APP), committed);
    assert_eq!(
        std::fs::read_to_string(app.join("work")).unwrap(),
        "edited\n"
    );
}
