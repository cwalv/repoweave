//! Tests for the global `-C <path>` / `--cwd` flag.
//!
//! The flag resolves the workspace as if rwv were invoked from `<path>` —
//! threaded through the resolver, never chdir'd. Addresses any path inside a
//! checkout; the normal containment walk (marker, root, $HOME ceiling) runs
//! from there. Relative path arguments elsewhere on the command line resolve
//! against this directory.
//!
//! These tests cover:
//!   - Address a workweave from /tmp via -C
//!   - `init -C <dir>` bootstraps there
//!   - Relative-arg resolution (origin is used as base for resolution)
//!   - Duplicate -C rejected
//!   - Corrective error for workweave-shaped argument

use predicates::prelude::*;
use repoweave::manifest::ProjectName;
use repoweave::workspace::WorkweaveMarker;
use std::path::Path;

mod common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

/// Build a minimal workspace and return its root path.
///
/// Layout:
///   {tmp}/ws/            -- workspace root (has `projects/` and `github/`)
///   {tmp}/ws/.rwv-active -- active project name
///   {tmp}/ws/projects/{project}/rwv.yaml
fn make_minimal_workspace(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    std::fs::create_dir_all(ws.join("projects").join(project)).unwrap();
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::write(
        ws.join("projects").join(project).join("rwv.yaml"),
        "repositories: {}\n",
    )
    .unwrap();
    std::fs::write(ws.join(".rwv-active"), format!("{project}\n")).unwrap();
    ws
}

/// Build a workweave directory with a `.rwv-workweave` marker pointing at `ws`.
///
/// Returns the workweave directory path.
fn make_workweave(tmp: &Path, ws: &Path, project: &str, name: &str) -> std::path::PathBuf {
    let ww_dir = tmp.join(format!("{project}--{name}"));
    std::fs::create_dir_all(&ww_dir).unwrap();
    let primary_canon = ws.canonicalize().unwrap();
    let marker = WorkweaveMarker::new(
        primary_canon.clone(),
        ProjectName::new(project).unwrap(),
        &primary_canon,
    );
    marker.write(&ww_dir).unwrap();
    ww_dir
}

// ===========================================================================
// -C addresses a workweave from a different directory
// ===========================================================================

/// `rwv -C <workweave-dir> resolve` should print the workweave path, even
/// when invoked with a cwd of /tmp.
#[test]
fn c_flag_addresses_workweave_from_tmp() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let ww = make_workweave(tmp.path(), &ws, "myproj", "feature-1");
    let ww_canon = ww.canonicalize().unwrap();

    // Run from /tmp but address the workweave via -C.
    rwv()
        .args(["-C", &ww.to_string_lossy(), "resolve"])
        .current_dir(std::env::temp_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ww_canon.to_string_lossy().as_ref(),
        ));
}

/// Same check with the long form `--cwd`.
#[test]
fn cwd_long_flag_addresses_workweave_from_tmp() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let ww = make_workweave(tmp.path(), &ws, "myproj", "feature-1");
    let ww_canon = ww.canonicalize().unwrap();

    rwv()
        .args(["--cwd", &ww.to_string_lossy(), "resolve"])
        .current_dir(std::env::temp_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ww_canon.to_string_lossy().as_ref(),
        ));
}

/// -C pointing at the primary workspace resolves to the primary workspace.
#[test]
fn c_flag_addresses_primary_workspace() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let ws_canon = ws.canonicalize().unwrap();

    rwv()
        .args(["-C", &ws.to_string_lossy(), "resolve"])
        .current_dir(std::env::temp_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ws_canon.to_string_lossy().as_ref(),
        ));
}

/// -C pointing at a subdirectory inside a workspace still resolves the workspace.
#[test]
fn c_flag_resolves_from_subdir_inside_workspace() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let subdir = ws.join("projects").join("myproj");

    // Point -C at the project subdir — the walk should still find the workspace.
    rwv()
        .args(["-C", &subdir.to_string_lossy(), "resolve"])
        .current_dir(std::env::temp_dir())
        .assert()
        .success()
        // Should print the workspace root (active_path for primary = primary root).
        .stdout(predicate::str::contains(
            ws.canonicalize().unwrap().to_string_lossy().as_ref(),
        ));
}

// ===========================================================================
// init -C <dir> bootstraps there
// ===========================================================================

/// `rwv init -C <empty-dir> myproject` bootstraps a workspace in the given
/// directory, not in the process's cwd.
#[test]
fn init_c_flag_bootstraps_in_addressed_directory() {
    let tmp = common::tempdir().unwrap();
    let target = tmp.path().join("fresh-ws");
    std::fs::create_dir_all(&target).unwrap();

    // Run from /tmp but bootstrap in target.
    rwv()
        .args(["-C", &target.to_string_lossy(), "init", "my-project"])
        .current_dir(std::env::temp_dir())
        .assert()
        .success();

    // The workspace structure must be present in target, not in /tmp.
    assert!(
        target.join("projects").join("my-project").is_dir(),
        "projects/my-project must exist in the addressed directory"
    );
    assert!(
        target
            .join("projects")
            .join("my-project")
            .join("rwv.yaml")
            .is_file(),
        "rwv.yaml must exist in the addressed project dir"
    );
    assert!(
        !std::env::temp_dir().join("projects").exists(),
        "bootstrap must not touch the process cwd"
    );
}

/// `rwv init -C <existing-workspace-dir> another-project` creates a second
/// project inside the existing workspace rather than refusing.
#[test]
fn init_c_flag_creates_project_in_existing_workspace() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "first-project");

    rwv()
        .args(["-C", &ws.to_string_lossy(), "init", "second-project"])
        .current_dir(std::env::temp_dir())
        .assert()
        .success();

    assert!(
        ws.join("projects").join("second-project").is_dir(),
        "second-project directory must be created inside the addressed workspace"
    );
}

// ===========================================================================
// Relative-arg resolution — origin is the base for FS path resolution
// ===========================================================================

/// When -C addresses a workspace, a status run resolves context from that
/// workspace and succeeds, regardless of the process cwd.
///
/// This verifies that the origin dir (from -C) is what drives resolution;
/// commands that operate against the workspace do so correctly.
#[test]
fn c_flag_status_resolves_from_addressed_workspace() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");

    rwv()
        .args(["-C", &ws.to_string_lossy(), "status"])
        .current_dir(std::env::temp_dir())
        .assert()
        .success();
}

/// When -C addresses a workweave subdirectory (a path INSIDE the workweave),
/// the containment walk resolves the workweave correctly.
#[test]
fn c_flag_resolves_from_subdir_inside_workweave() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");
    let ww = make_workweave(tmp.path(), &ws, "myproj", "my-feature");

    // Create a subdirectory inside the workweave.
    let subdir = ww.join("some").join("nested");
    std::fs::create_dir_all(&subdir).unwrap();

    // Point -C at the nested dir; the walk should find the workweave marker.
    rwv()
        .args(["-C", &subdir.to_string_lossy(), "resolve"])
        .current_dir(std::env::temp_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ww.canonicalize().unwrap().to_string_lossy().as_ref(),
        ));
}

// ===========================================================================
// Duplicate -C rejected
// ===========================================================================

/// Passing -C twice must be rejected.
#[test]
fn duplicate_c_flag_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");

    rwv()
        .args([
            "-C",
            &ws.to_string_lossy(),
            "-C",
            &ws.to_string_lossy(),
            "resolve",
        ])
        .assert()
        .failure();
}

/// Passing --cwd twice must also be rejected.
#[test]
fn duplicate_cwd_long_flag_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");

    rwv()
        .args([
            "--cwd",
            &ws.to_string_lossy(),
            "--cwd",
            &ws.to_string_lossy(),
            "resolve",
        ])
        .assert()
        .failure();
}

/// Mixing -C and --cwd is also a duplicate.
#[test]
fn mixing_c_and_cwd_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");

    rwv()
        .args([
            "-C",
            &ws.to_string_lossy(),
            "--cwd",
            &ws.to_string_lossy(),
            "resolve",
        ])
        .assert()
        .failure();
}

// ===========================================================================
// Corrective error for workweave-shaped argument
// ===========================================================================

/// `-C myproject--my-feature` (workweave name shape, not a real path) must
/// emit a corrective error pointing at -w/--workweave.
#[test]
fn c_flag_workweave_name_shape_gets_corrective_error() {
    rwv()
        .args(["-C", "myproject--my-feature", "resolve"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("workweave name")
                .and(predicate::str::contains("-w/--workweave")),
        );
}

/// Same check for a more complex name with numeric suffix.
#[test]
fn c_flag_workweave_name_with_numeric_suffix_gets_corrective_error() {
    rwv()
        .args(["-C", "foundations--patch2", "resolve"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("-w/--workweave"));
}

/// A relative path that happens to contain `--` but also has a `/` separator
/// should NOT trigger the corrective error (it's clearly a path).
#[test]
fn c_flag_path_with_double_dash_in_component_is_not_confused_for_workweave_name() {
    // A path like `./some--dir/sub` has a path separator, so the corrective
    // error must not fire. Instead, it should fail with a "path does not
    // exist" error (not the workweave-name corrective message).
    rwv()
        .args(["-C", "./some--dir/sub", "resolve"])
        .assert()
        .failure()
        // Must NOT say "workweave name" or point at -w/--workweave.
        .stderr(
            predicate::str::contains("-w/--workweave")
                .not()
                .and(predicate::str::contains("workweave name").not()),
        );
}

/// An ordinary non-existent path that does NOT match workweave-name shape
/// must fail with a plain "path does not exist" error, NOT the corrective
/// workweave message.
#[test]
fn c_flag_nonexistent_ordinary_path_gives_plain_error() {
    rwv()
        .args(["-C", "/this/path/does/not/exist/at/all", "resolve"])
        .assert()
        .failure()
        .stderr(
            // Must NOT be the workweave corrective message.
            predicate::str::contains("-w/--workweave")
                .not()
                // Must contain some indication the path couldn't be accessed.
                .and(
                    predicate::str::contains("does not exist")
                        .or(predicate::str::contains("cannot be accessed")),
                ),
        );
}

// ===========================================================================
// -C with verb placement — flag can appear before or after the subcommand
// ===========================================================================

/// -C after the subcommand name (global flag, so clap should accept this).
#[test]
fn c_flag_after_subcommand_name_is_accepted() {
    let tmp = common::tempdir().unwrap();
    let ws = make_minimal_workspace(tmp.path(), "myproj");

    // Clap supports global flags anywhere; verify it works after the verb too.
    rwv()
        .args(["resolve", "-C", &ws.to_string_lossy()])
        .current_dir(std::env::temp_dir())
        .assert()
        .success();
}
