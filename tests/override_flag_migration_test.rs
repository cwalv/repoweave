//! `--force` migration hints and the refusal for the verbs with none.
//!
//! Each waiver is named for the precondition it destroys, so the operator
//! consents to a consequence rather than to a category. `fetch` still answers
//! the old `--force` with the flag that replaced it. Every other verb answers
//! it the way it answers a flag it lacks: rwv has no blanket force, and the
//! message says so without naming a replacement. `push --force` is the
//! deliberate exception — it is git's force-push, not a precondition waiver —
//! and must keep parsing.

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

const NO_BLANKET_FORCE: &str = "rwv has no blanket force";

/// `--force` on a verb that never declared it, and on one whose old waiver was
/// renamed in a release that no longer carries a hint: the same refusal, which
/// names no replacement flag.
#[test]
fn force_on_a_verb_without_it_gets_the_generic_refusal() {
    let cases: [&[&str]; 4] = [
        &["member", "remove", "github/org/repo", "--delete", "--force"],
        &[
            "workweave",
            "create",
            "--project",
            "web-app",
            "ww",
            "--force",
        ],
        &[
            "workweave",
            "delete",
            "--project",
            "web-app",
            "ww",
            "--force",
        ],
        &["member", "add", "https://example.com/x.git", "--force"],
    ];
    for args in cases {
        let stderr = stderr_of(args);
        assert!(
            stderr.contains(NO_BLANKET_FORCE) && stderr.contains("rwv explain inapplicable-flag"),
            "{args:?} must get the generic refusal; got: {stderr}"
        );
        for replacement in [
            "--delete-shared-clone",
            "--replace-existing",
            "--discard-uncommitted",
            "--discard-unmerged-commits",
            "has been renamed",
            "has been split",
        ] {
            assert!(
                !stderr.contains(replacement),
                "{args:?} must not map `--force` to `{replacement}`; got: {stderr}"
            );
        }
    }
}

/// A spelling that exists only because an earlier release had it gets the
/// answer any unknown verb gets, with no old-to-new mapping for its flag.
#[test]
fn a_retired_top_level_remove_gets_no_force_mapping() {
    let stderr = stderr_of(&["remove", "github/org/repo", "--force"]);
    assert!(
        stderr.contains("unknown verb `remove`"),
        "expected the unknown-verb refusal; got: {stderr}"
    );
    for mapping in [
        "--delete-shared-clone",
        "has been renamed",
        NO_BLANKET_FORCE,
    ] {
        assert!(
            !stderr.contains(mapping),
            "`rwv remove --force` must not carry `{mapping}`; got: {stderr}"
        );
    }
}

#[test]
fn push_force_is_not_migrated() {
    let stderr = stderr_of(&["push", "--force", "--dry-run"]);
    assert!(
        !stderr.contains("has been renamed")
            && !stderr.contains("has been split")
            && !stderr.contains(NO_BLANKET_FORCE),
        "push --force must survive the rename pass; got: {stderr}"
    );
    assert!(
        stderr.contains("no repoweave workspace") || stderr.contains("workspace"),
        "push --force must reach workspace resolution, past argument parsing; got: {stderr}"
    );
}

#[test]
fn fetch_detach_working_branch_names_detach_checkouts() {
    // Not a `--force` alias: `--detach-working-branch` was itself a shipped
    // flag name that the branch model renames directly.
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
