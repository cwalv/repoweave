//! Regression test: `rwv doctor` under-reported a stale workweave surfacing
//! symlink that `--fix` already knew how to clean up.
//!
//! `rwv workweave create` surfaces a project's declared files as symlinks
//! into the checkout. When the checkout later stops carrying a declared
//! file's source — a manual delete, or history the workweave has not pulled
//! yet — the symlink `create` left behind now resolves to nothing. `--fix`
//! removes it (it is in the owned set and points into `projects/`), but
//! plain `rwv doctor` said nothing: `verify_surfacing`'s workweave arm
//! (`skip_missing_sources`) skipped the file entirely whenever its source was
//! absent, without checking whether a symlink was already sitting there.

mod common;

use std::path::{Path, PathBuf};

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

fn git_init_with_commit(dir: &Path) {
    git(&["init", "--initial-branch=main"], dir);
    git(&["config", "user.email", "test@test.com"], dir);
    git(&["config", "user.name", "Test"], dir);
    git(&["add", "-A"], dir);
    git(&["commit", "-m", "init"], dir);
}

fn rwv_output(cwd: &Path, args: &[&str]) -> String {
    let output = common::rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// A primary weave with one project (`alpha`) whose `static-files`
/// integration declares `.claude`, a workweave created off it, and the
/// source `.claude` then removed from the workweave's own checkout — leaving
/// the symlink `workweave create` made pointing at nothing.
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let ws = root.join("ws");
    std::fs::create_dir_all(ws.join("projects")).unwrap();

    let project_dir = ws.join("projects").join("alpha");
    std::fs::create_dir_all(&project_dir).unwrap();
    let manifest = "[repositories]\n\n[integrations.static-files]\nenabled = true\nfiles = [\".claude\"]\n\n[integrations.vscode-workspace]\nenabled = false\n\n[integrations.go-work]\nenabled = false\n";
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    std::fs::write(project_dir.join(".claude"), "claude config\n").unwrap();
    git_init_with_commit(&project_dir);

    std::fs::write(ws.join(".rwv-active"), "alpha\n").unwrap();
    let activate_out = rwv_output(&ws, &["activate", "alpha", "--no-materialize"]);
    assert!(
        ws.join(".claude").symlink_metadata().is_ok(),
        "fixture: activate should surface `.claude` at primary; output:\n{activate_out}"
    );

    let weaveroot = root.join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    let create_out = rwv_output(&ws, &["workweave", "alpha", "create", "agent-1"]);
    let ww_dir = weaveroot.join("alpha--agent-1");
    assert!(
        ww_dir.join(".claude").symlink_metadata().is_ok(),
        "fixture: workweave create should surface `.claude`; output:\n{create_out}"
    );

    std::fs::remove_file(ww_dir.join("projects").join("alpha").join(".claude")).unwrap();
    assert!(
        ww_dir.join(".claude").symlink_metadata().is_ok(),
        "fixture: the create-time symlink must survive removing its target"
    );
    assert!(
        !ww_dir.join(".claude").exists(),
        "fixture: the symlink must now resolve to nothing"
    );

    (tmp, ww_dir)
}

#[test]
fn doctor_flags_a_stale_symlink_whose_source_is_gone() {
    let (_tmp, ww_dir) = fixture();

    let out = rwv_output(&ww_dir, &["doctor"]);
    assert!(
        out.contains("surfacing") && out.contains(".claude") && out.contains("no longer exists"),
        "plain doctor should name the stale symlink, got:\n{out}"
    );
}

#[test]
fn doctor_fix_removes_the_stale_symlink_and_does_not_recreate_it() {
    let (_tmp, ww_dir) = fixture();

    let fix_out = rwv_output(&ww_dir, &["doctor", "--fix"]);
    assert!(
        ww_dir.join(".claude").symlink_metadata().is_err(),
        "--fix should remove the stale symlink (its source is still gone); output:\n{fix_out}"
    );

    // A different, still-true finding survives (the source really is gone,
    // which is `static-files`' own concern) — only the surfacing-stale
    // finding this test is about must be cleared.
    let after = rwv_output(&ww_dir, &["doctor"]);
    assert!(
        !after.contains("no longer exists"),
        "after --fix, doctor should report no stale surfacing symlink, got:\n{after}"
    );
}
