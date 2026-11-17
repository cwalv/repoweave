//! The io cause behind an io failure must reach the operator, not just sit in
//! the error.
//!
//! Whether an operator ever sees the cause depends on the render, and every
//! surface between the error and the terminal is free to drop it. The
//! fixture puts a *directory* where `.gitattributes` belongs: any read
//! failure other than not-found takes the same arm, and a directory takes it
//! for every user including root, which `chmod 000` does not. The VCS reads
//! the attribute in force and treats a file it cannot read as absent, so the
//! failure arises where `doctor --fix` reads the file to rewrite it.

use std::path::{Path, PathBuf};

use crate::common;

fn make_workspace(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    root
}

/// Workspace whose one project, on an unborn branch, has an unreadable
/// `.gitattributes`.
fn workspace_with_unreadable_gitattributes(root: &Path) {
    let repo_path = "github/acme/server";
    let repo = root.join(repo_path);
    std::fs::create_dir_all(&repo).unwrap();
    common::git_in(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "init\n").unwrap();
    common::git_in(&repo, &["add", "."]);
    common::git_in(&repo, &["commit", "-m", "initial"]);

    let project_dir = root.join("projects").join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories]\n[repositories.\"{repo_path}\"]\ntype = \"git\"\n\
             url = \"https://github.com/acme/server.git\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    common::git_in(&project_dir, &["init", "-b", "main"]);
    std::fs::create_dir(project_dir.join(".gitattributes")).unwrap();
}

#[test]
fn doctor_fix_names_the_io_cause_of_a_gitattributes_it_cannot_rewrite() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    workspace_with_unreadable_gitattributes(&root);

    let fix = common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .unwrap();
    let printed = common::cli::describe(&fix);
    let cause = std::fs::read_to_string(root.join("projects/my-app/.gitattributes"))
        .expect_err("a directory does not read as a file");
    assert!(
        printed.contains("failed to set up the lock merge driver")
            && printed.contains(&format!(".gitattributes: {cause}")),
        "the operator got no cause for the failure:\n{printed}"
    );
}

/// The cause is emitted by `Display`, so a caller that renders the error with
/// no formatting of its own still hands it to the operator.
#[test]
fn vcs_error_io_display_emits_its_source() {
    let rendered = repoweave::vcs::VcsError::Io {
        ctx: "failed to read /w/p/.gitattributes".into(),
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "unreadable-by-anyone"),
    }
    .to_string();

    assert!(
        rendered.contains("unreadable-by-anyone"),
        "Display must emit the io cause, not only the context: {rendered:?}"
    );
}
