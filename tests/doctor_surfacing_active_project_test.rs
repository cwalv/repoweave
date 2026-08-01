//! Regression test: weave-root surfacing follows the project the root
//! presents, never the project a verb was scoped to.
//!
//! The incident. `.rwv-active` named one project while
//! `rwv doctor --project <other> --fix` ran for another. Doctor reported
//!
//!     [fixed] core: re-surfaced symlinks for project `<other>`
//!             (missing/mis-resolved surfacing)
//!
//! and re-pointed seven shared-name weave-root symlinks — `.beads`,
//! `CLAUDE.md`, `Cargo.toml`, `go.work`, `go.sum`, `package.json`,
//! `package-lock.json` — at the project that was NOT active, while leaving
//! `.rwv-active` alone. A second agent working the active project then had
//! `bd` resolving the wrong database and root manifests describing the wrong
//! member set. Nothing in the repair consulted the pointer: the detector
//! expected every declared file of the scoped project to be surfaced at the
//! root, and the fix arm made it so.
//!
//! The rule the fix installs has two classes:
//!
//!   * A **per-project name** — `<project>.code-workspace` — cannot collide
//!     with another project's, so it may be surfaced for any project and two
//!     of them may sit at the root at once.
//!   * A **shared name** — everything else — can be produced by more than one
//!     project, so the root can show only one project's, and which one is what
//!     `.rwv-active` (a workweave's marker) answers.
//!
//! `doctor --fix --project X` with X not the presented project therefore
//! repairs X's per-project surfacing and leaves every shared name where the
//! presented project put it. The detector run for the presented project
//! additionally flags a shared name resolving into some other project even
//! when the presented project declares no such file — otherwise the residue of
//! this incident (`Cargo.toml` and `go.work` still pointing at the wrong
//! project, under an active project with no cargo or go integration) stays
//! invisible, and re-fixing as the active project is not a complete undo.
//!
//! Scope. `rwv sync-to`, `rwv sync-to --retire` and `rwv workweave create`
//! scoped to a non-active project were measured against the live weave before
//! and after: the shared-name links were byte-identical every time. The
//! surfacing repair is the reproducer, so that is what this pins.

use std::path::{Path, PathBuf};

mod common;

/// Two projects at one weave root, each declaring the same shared name plus
/// one named for itself. `static-files` carries both classes so the surfacing
/// union under test is exactly what the fixture declares.
fn make_workspace(parent: &Path) -> PathBuf {
    let root = parent.join("ws");
    std::fs::create_dir_all(root.join("github")).unwrap();
    for project in ["alpha", "beta"] {
        let project_dir = root.join("projects").join(project);
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("rwv.toml"),
            format!(
                "[repositories]\n\n[integrations.static-files]\nenabled = true\nfiles = [\"CLAUDE.md\", \"{project}.code-workspace\"]\n\n[integrations.vscode-workspace]\nenabled = false\n\n[integrations.go-work]\nenabled = false\n"
            ),
        )
        .unwrap();
        std::fs::write(
            project_dir.join("CLAUDE.md"),
            format!("{project}'s instructions\n"),
        )
        .unwrap();
        std::fs::write(
            project_dir.join(format!("{project}.code-workspace")),
            format!("{{ \"folders\": [] }} // {project}\n"),
        )
        .unwrap();
    }
    root
}

/// Activate `project` so the root presents it and its files are surfaced.
fn activate(root: &Path, project: &str) {
    common::rwv()
        .args(["activate", project, "--no-install"])
        .current_dir(root)
        .assert()
        .success();
}

fn link_target(root: &Path, name: &str) -> PathBuf {
    std::fs::read_link(root.join(name))
        .unwrap_or_else(|e| panic!("{name} should be a symlink at the weave root: {e}"))
}

fn doctor(root: &Path, args: &[&str]) -> String {
    let out = common::rwv()
        .arg("doctor")
        .args(args)
        .current_dir(root)
        .output()
        .expect("doctor should run");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The pinned stomp: active = alpha, `--fix --project beta`.
#[test]
fn fix_for_a_non_active_project_leaves_shared_names_with_the_active_one() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path());
    activate(&root, "alpha");

    assert_eq!(
        link_target(&root, "CLAUDE.md"),
        Path::new("projects/alpha/CLAUDE.md"),
        "fixture precondition: the active project's shared name is surfaced"
    );

    doctor(&root, &["--fix", "--project", "beta"]);

    assert_eq!(
        link_target(&root, "CLAUDE.md"),
        Path::new("projects/alpha/CLAUDE.md"),
        "a repair scoped to beta must not re-point a shared name away from alpha"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".rwv-active"))
            .unwrap()
            .trim(),
        "alpha",
        "the repair must not change which project the root presents"
    );
    assert_eq!(
        link_target(&root, "beta.code-workspace"),
        Path::new("projects/beta/beta.code-workspace"),
        "beta's per-project name is safe to surface and must still be repaired"
    );
    assert_eq!(
        link_target(&root, "alpha.code-workspace"),
        Path::new("projects/alpha/alpha.code-workspace"),
        "alpha's per-project name must survive a repair scoped to beta"
    );
}

/// The detector half, on the shape the residue took: a shared name resolving
/// into a project the root does not present, which the presented project does
/// not declare and so cannot reach through its own surfacing set.
#[test]
fn doctor_flags_and_reclaims_a_shared_name_surfaced_out_of_another_project() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path());
    activate(&root, "alpha");
    std::os::unix::fs::symlink("projects/beta/Cargo.toml", root.join("Cargo.toml")).unwrap();

    let report = doctor(&root, &[]);
    assert!(
        report.contains("Cargo.toml") && report.contains("beta"),
        "doctor must flag the foreign shared name and say where it resolves: {report}"
    );

    doctor(&root, &["--fix"]);
    assert!(
        root.join("Cargo.toml").symlink_metadata().is_err(),
        "--fix for the presented project must reclaim the weave root's shared names"
    );
    assert_eq!(
        link_target(&root, "CLAUDE.md"),
        Path::new("projects/alpha/CLAUDE.md"),
        "the reclaim must not disturb the presented project's own surfacing"
    );
}
