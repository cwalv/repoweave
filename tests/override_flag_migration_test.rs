//! `--force` migration hints for the renamed precondition waivers.
//!
//! Each waiver is now named for the precondition it destroys, so the operator
//! consents to a consequence rather than to a category. The old spelling is
//! gone (no hidden aliases); it must fail with an error that names the
//! replacement. `push --force` is the deliberate exception — it is git's
//! force-push, not a precondition waiver — and must keep parsing.

use assert_cmd::Command;

mod common;

fn rwv() -> Command {
    common::rwv()
}

/// Run `rwv <args>` in a scratch directory and return stderr. The migration
/// hints fire in early dispatch, before any workspace resolution, so no
/// fixture is needed.
fn stderr_of(args: &[&str]) -> String {
    let tmp = common::tempdir().unwrap();
    let output = rwv()
        .args(args)
        .current_dir(tmp.path())
        .output()
        .expect("rwv should run");
    assert!(
        !output.status.success(),
        "expected failure for {args:?}; got success"
    );
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn fetch_force_names_allow_non_empty_dir() {
    let stderr = stderr_of(&["fetch", "https://example.com/x.git", "--force"]);
    assert!(
        stderr.contains("--allow-non-empty-dir"),
        "expected the replacement flag; got: {stderr}"
    );
}

#[test]
fn remove_force_names_delete_shared_clone() {
    let stderr = stderr_of(&["remove", "github/org/repo", "--delete", "--force"]);
    assert!(
        stderr.contains("--delete-shared-clone"),
        "expected the replacement flag; got: {stderr}"
    );
}

#[test]
fn workweave_create_force_names_replace_existing() {
    let stderr = stderr_of(&["workweave", "web-app", "create", "ww", "--force"]);
    assert!(
        stderr.contains("--replace-existing"),
        "expected the replacement flag; got: {stderr}"
    );
}

#[test]
fn workweave_delete_force_names_both_discard_flags() {
    let stderr = stderr_of(&["workweave", "web-app", "delete", "ww", "--force"]);
    assert!(
        stderr.contains("--discard-uncommitted") && stderr.contains("--discard-unmerged-commits"),
        "expected both replacement flags; got: {stderr}"
    );
}

#[test]
fn push_force_is_not_migrated() {
    let stderr = stderr_of(&["push", "--force", "--dry-run"]);
    assert!(
        !stderr.contains("has been renamed") && !stderr.contains("has been split"),
        "push --force must survive the rename pass; got: {stderr}"
    );
}

#[test]
fn fetch_detach_working_branch_names_detach_checkouts() {
    // Not a `--force` alias: `--detach-working-branch` was itself a shipped
    // flag name (fo-r8ahsp.5) that the branch model renames directly.
    let stderr = stderr_of(&[
        "fetch",
        "https://example.com/x.git",
        "--detach-working-branch",
    ]);
    assert!(
        stderr.contains("--detach-checkouts"),
        "expected the replacement flag; got: {stderr}"
    );
}
