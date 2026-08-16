//! Which directory a destructive workweave verb is actually pointed at.
//!
//! `delete_workweave` resolves `(project, name)` through the primary-side
//! registry. The marker round-trip it then runs validates the RESOLVED
//! directory against `(primary, project)` — a condition every registered
//! workweave of that project satisfies, including the wrong one. It witnesses
//! that the victim is a legitimate workweave, never that it is the one the
//! caller meant.
//!
//! A caller that arrived through a path and derived a name from it holds both
//! halves and can disagree with itself. These pin that such a caller is
//! refused before anything is destroyed, and that the workweave it named and
//! the workweave it pointed at BOTH survive the refusal — a test asserting
//! only the `Err` would pass if the refusal came after a destructive step.
//!
//! Fixtures are synthetic; the verb is destructive and is never exercised
//! against the live weave.

use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::workweave::{create_workweave, delete_workweave};
use std::path::{Path, PathBuf};

mod common;

const REPO: &str = "github/org/owned";

/// A primary weave holding one project with one owned repo.
fn make_workspace(tmp: &Path, project: &str) -> PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();

    let repo = ws.join(REPO);
    std::fs::create_dir_all(&repo).unwrap();
    common::git_in(&repo, &["init", "--initial-branch=main"]);
    common::git_in(&repo, &["config", "user.email", "test@test.com"]);
    common::git_in(&repo, &["config", "user.name", "Test"]);
    std::fs::write(repo.join("README"), "init").unwrap();
    common::git_in(&repo, &["add", "."]);
    common::git_in(&repo, &["commit", "-m", "initial"]);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            r#"[repositories."{REPO}"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"
"#,
            repo = common::url_path(&repo),
        ),
    )
    .unwrap();

    ws
}

fn create(ws: &Path, project: &str, name: &str) -> PathBuf {
    create_workweave(
        ws,
        ws,
        &ProjectName::new(project).unwrap(),
        &WorkweaveName::new(name).unwrap(),
        false,
        false,
        false,
        None,
    )
    .expect("create should succeed")
}

#[test]
fn delete_refuses_a_name_and_a_path_that_denote_different_workweaves() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    let wa = create(&ws, "proj", "wa");
    let wb = create(&ws, "proj", "wb");

    let err = delete_workweave(
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("wa").unwrap(),
        Some(wb.as_path()),
        false,
        None,
    )
    .expect_err("naming `wa` while holding `wb`'s path must refuse");

    let msg = format!("{err:#}");
    common::assert_names_operator_path(&msg, &wa);
    common::assert_names_operator_path(&msg, &wb);

    assert!(
        wa.exists(),
        "the NAMED workweave must survive a refusal: {}",
        wa.display()
    );
    assert!(
        wb.exists(),
        "the workweave whose path was held must survive a refusal: {}",
        wb.display()
    );
}

/// The refusal above must come from the disagreement and not from the
/// `expected_dir` argument being rejected on sight.
#[test]
fn delete_proceeds_when_the_name_and_the_path_agree() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    let wa = create(&ws, "proj", "wa");
    let wb = create(&ws, "proj", "wb");

    delete_workweave(
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("wa").unwrap(),
        Some(wa.as_path()),
        false,
        None,
    )
    .expect("naming `wa` while holding `wa`'s path must delete it");

    assert!(!wa.exists(), "wa should be gone");
    assert!(wb.exists(), "wb must be untouched");
}

/// A caller holding a spelling of the directory that is not the one the
/// registry recorded still means that directory.
#[test]
fn delete_accepts_an_equivalent_spelling_of_the_registered_path() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    let wa = create(&ws, "proj", "wa");
    let detoured = wa.join(REPO).join("..").join("..").join("..");
    assert_eq!(
        detoured.canonicalize().unwrap(),
        wa.canonicalize().unwrap(),
        "precondition: the detoured spelling resolves to wa"
    );

    delete_workweave(
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("wa").unwrap(),
        Some(detoured.as_path()),
        false,
        None,
    )
    .expect("an equivalent spelling is the same directory and must not refuse");

    assert!(!wa.exists(), "wa should be gone");
}
