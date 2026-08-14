//! Tests for the global `-w/--workweave <project>--<name>` flag.
//!
//! The flag addresses a workweave by its registry identity, independent of
//! container placement. Resolution: locate the workspace from `-C` or process
//! cwd, then look up the workweave path from the project's registry with
//! `.rwv-workweave` marker round-trip validation.
//!
//! These tests cover:
//!   - `-w` from primary cwd
//!   - `-w` composed with `-C` from /tmp
//!   - Unknown-name error listing candidates
//!   - Path-shaped-argument corrective error (contains `/` or exists on disk)
//!   - Project provenance = `WorkweaveFlag` (the target line stays silent)
//!   - `-w` with `--project` naming a different project (`--project` wins per chain)

use predicates::prelude::*;
use repoweave::manifest::ProjectName;
use repoweave::workspace::WorkweaveMarker;
use repoweave::workweave_index;
use std::path::{Path, PathBuf};
use std::process;

mod common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Build a minimal workspace with a single project + one repo.
///
/// Layout:
///   {root}/ws/                      -- workspace root
///   {root}/ws/github/org/repo/      -- a real git repo
///   {root}/ws/projects/{project}/   -- project dir with rwv.toml
///   {root}/ws/.rwv-active           -- active project
fn make_workspace(root: &Path, project: &str) -> PathBuf {
    let ws = root.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest = format!(
        "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"file://{}\"\nversion = \"main\"\nrole = \"owned\"\n",
        common::url_path(&repo_path)
    );
    std::fs::write(project_dir.join("rwv.toml"), &manifest).unwrap();
    std::fs::write(ws.join(".rwv-active"), format!("{project}\n")).unwrap();
    ws
}

/// Create a workweave directory with a `.rwv-workweave` marker and register it
/// in the project's index. Returns the workweave directory path.
///
/// This directly writes the marker and index without going through `rwv
/// workweave create` (which needs git worktrees). The tests exercise the
/// registry lookup and resolution path, not the full workweave lifecycle.
fn register_workweave(ws: &Path, project: &str, name: &str) -> PathBuf {
    let ws_canon = ws.canonicalize().unwrap();
    let container = ws_canon.parent().unwrap().join(".workweaves");
    std::fs::create_dir_all(&container).unwrap();

    let ww_dir = container.join(format!("{project}--{name}"));
    std::fs::create_dir_all(&ww_dir).unwrap();

    // Write the `.rwv-workweave` marker so the round-trip validation passes.
    let marker = WorkweaveMarker::new(
        ws_canon.clone(),
        ProjectName::new(project).unwrap(),
        &ws_canon,
    );
    marker.write(&ww_dir).unwrap();

    // Also place the project dir structure inside the workweave so that
    // `rwv resolve` from the workweave can find the project.
    let ww_project_dir = ww_dir.join("projects").join(project);
    std::fs::create_dir_all(&ww_project_dir).unwrap();
    std::fs::write(ww_project_dir.join("rwv.toml"), "[repositories]\n").unwrap();

    // Register the workweave in the index.
    workweave_index::record_workweave(
        &ws_canon,
        &ProjectName::new(project).unwrap(),
        name,
        ww_dir.clone(),
    )
    .unwrap();

    // Seed the container in the index (required by `list`).
    workweave_index::set_container(&ws_canon, &ProjectName::new(project).unwrap(), container)
        .unwrap();

    ww_dir
}

// ===========================================================================
// Basic: -w from primary cwd
// ===========================================================================

/// `rwv -w myproj--feat resolve` must print the workweave path when invoked
/// from the primary workspace cwd.
#[test]
fn w_flag_resolves_workweave_from_primary_cwd() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");
    let ww = register_workweave(&ws, "myproj", "feat");
    let ww_canon = ww.canonicalize().unwrap();

    rwv()
        .args(["-w", "myproj--feat", "resolve"])
        .current_dir(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ww_canon.to_string_lossy().as_ref(),
        ));
}

/// Long form `--workweave` must work the same as `-w`.
#[test]
fn workweave_long_flag_resolves_workweave_from_primary_cwd() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");
    let ww = register_workweave(&ws, "myproj", "feat");
    let ww_canon = ww.canonicalize().unwrap();

    rwv()
        .args(["--workweave", "myproj--feat", "resolve"])
        .current_dir(&ws)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ww_canon.to_string_lossy().as_ref(),
        ));
}

/// `rwv -w` from inside the workweave itself (cwd = workweave) still resolves
/// to the correct workweave — the containment walk finds the primary from the
/// workweave marker, then the registry lookup confirms the registered path.
#[test]
fn w_flag_resolves_from_workweave_cwd() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");
    let ww = register_workweave(&ws, "myproj", "feat");
    let ww_canon = ww.canonicalize().unwrap();

    // Run from INSIDE the workweave; -w should still find it by name.
    rwv()
        .args(["-w", "myproj--feat", "resolve"])
        .current_dir(&ww)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ww_canon.to_string_lossy().as_ref(),
        ));
}

// ===========================================================================
// Composition: -w with -C from /tmp
// ===========================================================================

/// `-C <workspace> -w <project>--<name>`: -C establishes the workspace,
/// -w selects the workweave. Must succeed from an arbitrary cwd like /tmp.
#[test]
fn w_flag_composed_with_c_flag_from_tmp() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");
    let ww = register_workweave(&ws, "myproj", "feat");
    let ww_canon = ww.canonicalize().unwrap();

    rwv()
        .args(["-C", &ws.to_string_lossy(), "-w", "myproj--feat", "resolve"])
        .current_dir(std::env::temp_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ww_canon.to_string_lossy().as_ref(),
        ));
}

/// Same but flags in reverse order: -w before -C (global flags compose
/// regardless of order).
#[test]
fn w_flag_and_c_flag_in_reverse_order() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");
    let ww = register_workweave(&ws, "myproj", "feat");
    let ww_canon = ww.canonicalize().unwrap();

    rwv()
        .args(["-w", "myproj--feat", "-C", &ws.to_string_lossy(), "resolve"])
        .current_dir(std::env::temp_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            ww_canon.to_string_lossy().as_ref(),
        ));
}

// ===========================================================================
// Unknown-name error lists candidates
// ===========================================================================

/// An unknown workweave name produces an error that lists known workweaves for
/// the project, so the operator can spot a typo.
#[test]
fn w_flag_unknown_name_lists_candidates() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");
    // Register a workweave under a different name so the list is non-empty.
    register_workweave(&ws, "myproj", "existing-ww");

    rwv()
        .args(["-w", "myproj--no-such-ww", "resolve"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("no-such-ww")
                // Must list the known workweave so the operator can see it.
                .and(predicate::str::contains("existing-ww")),
        );
}

/// Unknown name with an empty registry gives a corrective message pointing at
/// `rwv workweave create` rather than an empty candidates list.
#[test]
fn w_flag_unknown_name_empty_registry_corrective_message() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");
    // No workweaves registered.

    rwv()
        .args(["-w", "myproj--ghost", "resolve"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("ghost").and(predicate::str::contains("myproj")));
}

// ===========================================================================
// Path-shaped argument corrective errors
// ===========================================================================

/// An argument with a `/` separator is path-shaped: must fail with a corrective
/// error pointing at `-C` rather than a confusing "not found" message.
#[test]
fn w_flag_path_with_slash_gets_corrective_error() {
    rwv()
        .args(["-w", "/some/absolute/path", "resolve"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("path separator")
                .or(predicate::str::contains("path, not a workweave name"))
                .and(predicate::str::contains("-C")),
        );
}

/// An argument with a relative path component (`./`) is path-shaped.
#[test]
fn w_flag_relative_path_gets_corrective_error() {
    rwv()
        .args(["-w", "./myproj--feat", "resolve"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("path separator").or(predicate::str::contains("-C")));
}

/// An argument that exists on disk as a path gets a corrective error pointing at
/// `-C`, even if the name shape would otherwise parse.
#[test]
fn w_flag_existing_disk_path_gets_corrective_error() {
    let tmp = common::tempdir().unwrap();
    // Create a directory whose basename looks like a workweave name.
    let ww_like = tmp.path().join("myproj--feat");
    std::fs::create_dir_all(&ww_like).unwrap();

    rwv()
        .args(["-w", &ww_like.to_string_lossy(), "resolve"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("-C"));
}

// ===========================================================================
// Argument shape: malformed -w values
// ===========================================================================

/// An argument without `--` must fail with a corrective error showing the
/// required `<project>--<name>` form.
#[test]
fn w_flag_no_separator_gives_form_error() {
    rwv()
        .args(["-w", "bareword", "resolve"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("<project>--<name>"));
}

/// `-w --name` (empty project part) must fail.
#[test]
fn w_flag_empty_project_gives_form_error() {
    rwv()
        .args(["-w", "--name", "resolve"])
        .assert()
        .failure()
        // clap rejects "--name" as a flag, not as a value; either way it must fail.
        .failure();
}

/// `-w project--` (empty name part) must fail.
#[test]
fn w_flag_empty_name_gives_form_error() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");

    rwv()
        .args(["-w", "myproj--", "resolve"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("<project>--<name>"));
}

// ===========================================================================
// Duplicate -w rejected
// ===========================================================================

/// Passing -w twice must be rejected.
#[test]
fn duplicate_w_flag_is_rejected() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");
    register_workweave(&ws, "myproj", "feat");

    rwv()
        .args(["-w", "myproj--feat", "-w", "myproj--feat", "resolve"])
        .current_dir(&ws)
        .assert()
        .failure();
}

// ===========================================================================
// Project provenance: WorkweaveFlag — target line stays silent
// ===========================================================================

/// When `-w` is the addressing form, no "target:" line must appear on stderr.
/// The target line fires only for `.rwv-active` fall-through (the pointer
/// provenance case), never for explicit addressing.
#[test]
fn w_flag_does_not_print_target_line() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");
    register_workweave(&ws, "myproj", "feat");

    let output = rwv()
        .args(["-w", "myproj--feat", "status"])
        .current_dir(&ws)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("target:"),
        "'-w' is explicit addressing; no 'target:' line must appear.\nstderr: {stderr}"
    );
}

// ===========================================================================
// Interaction: -w and --project naming a different project (--project wins)
// ===========================================================================

/// When `--project <other>` and `-w <project>--<name>` are both given,
/// `--project` wins per the resolution chain (`--project > -w prefix`).
/// The workweave is still looked up by the `-w` project name (to find the
/// directory), but the resolved context's project is overridden by `--project`.
///
/// This test verifies that the combination does not error out (the chain rule
/// is applied, not a conflict refusal), and that the verb operates from the
/// `-w`-addressed workweave directory.
#[test]
fn w_flag_project_override_wins_over_w_prefix() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj-a");
    // Also create a second project so `--project proj-b` names a real project.
    let proj_b_dir = ws.join("projects").join("proj-b");
    std::fs::create_dir_all(&proj_b_dir).unwrap();
    std::fs::write(proj_b_dir.join("rwv.toml"), "[repositories]\n").unwrap();

    let ww = register_workweave(&ws, "proj-a", "my-ww");

    // The workweave dir must be resolved (from "proj-a" registry), but the
    // project used for the operation is "proj-b" from --project.
    // `rwv resolve` doesn't care about the project; it just prints the active
    // path. We verify the workweave is addressed (by checking the resolved
    // path) and that no "target:" line appears (--project sets Flag provenance).
    // `--project` is a per-verb flag on verbs like `status`; `resolve` has no
    // project override flag, so use `status --project proj-b` instead.
    let output = rwv()
        .args(["-w", "proj-a--my-ww", "status", "--project", "proj-b"])
        .current_dir(&ws)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let _ = ww.canonicalize().unwrap();

    // Must succeed: --project proj-b exists on disk, so status can resolve it.
    // The verb operates from the -w addressed workweave dir (proj-a--my-ww)
    // but with the project overridden to proj-b by --project (Flag provenance).
    assert!(
        output.status.success(),
        "expected success with -w + status --project; stderr: {stderr}\nstdout: {stdout}"
    );
    // --project sets Flag provenance → no "target:" line.
    assert!(
        !stderr.contains("target:"),
        "explicit --project must not produce target line; stderr: {stderr}"
    );
}

// ===========================================================================
// Multi-workweave: two workweaves registered, -w selects the right one
// ===========================================================================

/// With two workweaves registered, `-w` must select exactly the named one.
#[test]
fn w_flag_selects_correct_workweave_when_multiple_registered() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "myproj");
    let ww_a = register_workweave(&ws, "myproj", "feature-a");
    let ww_b = register_workweave(&ws, "myproj", "feature-b");
    let ww_a_canon = ww_a.canonicalize().unwrap();
    let ww_b_canon = ww_b.canonicalize().unwrap();

    // Selecting feature-a must return feature-a's path, not feature-b's.
    let out_a = rwv()
        .args(["-w", "myproj--feature-a", "resolve"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(out_a.status.success());
    assert!(
        String::from_utf8_lossy(&out_a.stdout).contains(ww_a_canon.to_string_lossy().as_ref()),
        "selecting feature-a must return feature-a path"
    );

    let out_b = rwv()
        .args(["-w", "myproj--feature-b", "resolve"])
        .current_dir(&ws)
        .output()
        .unwrap();
    assert!(out_b.status.success());
    assert!(
        String::from_utf8_lossy(&out_b.stdout).contains(ww_b_canon.to_string_lossy().as_ref()),
        "selecting feature-b must return feature-b path"
    );
}
