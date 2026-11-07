//! `--force` migration hints and the refusal for the verbs with none.
//!
//! Each waiver is named for the precondition it destroys, so the operator
//! consents to a consequence rather than to a category. Every verb answers
//! `--force` the way it answers a flag it lacks: rwv has no blanket force, and
//! the message says so without naming a replacement. `push --force` is the
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

const NO_BLANKET_FORCE: &str = "rwv has no blanket force";

/// The take-in verbs carry no message mapping an old spelling to a new one:
/// `--force` is refused as it is on any verb that lacks it.
#[test]
fn force_on_a_take_in_verb_gets_the_generic_refusal() {
    for args in [
        &["fetch", "--force"][..],
        &["clone", "https://example.com/x.git", "--force"][..],
        &["update", "--force"][..],
        &["pull", "--force"][..],
    ] {
        let stderr = stderr_of(args);
        assert!(
            stderr.contains(NO_BLANKET_FORCE) && !stderr.contains("renamed"),
            "{args:?}: expected the generic refusal and no rename message; got: {stderr}"
        );
    }
}

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

/// `--detach-working-branch` was a flag name an earlier release shipped and
/// renamed. No verb keeps a message for it: clap's own unknown-argument error
/// answers it, naming no replacement.
#[test]
fn the_retired_detach_working_branch_spelling_is_an_unknown_argument() {
    for verb in ["fetch", "update", "clone"] {
        let mut args = vec![verb];
        if verb == "clone" {
            args.push("https://example.com/x.git");
        }
        args.push("--detach-working-branch");
        let stderr = stderr_of(&args);
        assert!(
            stderr.contains("unexpected argument") && !stderr.contains("has been renamed"),
            "{args:?}: expected clap's unknown-argument error; got: {stderr}"
        );
    }
}
