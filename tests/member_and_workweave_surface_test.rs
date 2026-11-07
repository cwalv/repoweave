//! What the `member` and `workweave` surface answers to spellings it does not
//! have.
//!
//! A bare `add`, `remove` or `list` is no verb, and the answer names the noun
//! whose subcommand it is, built from the verbs the CLI has now. A spelling
//! that exists only because an earlier release had it is answered as any
//! unknown spelling is: nothing here keeps a table of old-to-new names.

mod common;
use common::rwv;

/// `rwv <args>` in a scratch directory with no plugin on `PATH`, so an unknown
/// verb reaches rwv's own refusal.
fn run(args: &[&str]) -> (Option<i32>, String) {
    let tmp = common::tempdir().unwrap();
    let plugins = common::tempdir().unwrap();
    let out = rwv()
        .args(args)
        .current_dir(tmp.path())
        .env("PATH", plugins.path())
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn a_bare_add_names_member_add() {
    let (code, stderr) = run(&["add", "https://example.com/o/r.git"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("unknown verb `add`") && stderr.contains("Did you mean `rwv member add`?"),
        "{stderr}"
    );
}

#[test]
fn every_noun_that_has_the_verb_is_named() {
    let (_, stderr) = run(&["remove", "github/o/r"]);
    assert!(stderr.contains("`rwv member remove`"), "{stderr}");
    let (_, stderr) = run(&["list"]);
    assert!(stderr.contains("`rwv workweave list`"), "{stderr}");
}

/// The suggestion is read from the verbs the CLI has, not from a list of what
/// it used to call them: `role` is no subcommand of any noun, so it gets none.
#[test]
fn a_retired_name_that_is_no_subcommand_gets_no_suggestion() {
    let (code, stderr) = run(&["role", "github/o/r", "owned"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(stderr.contains("unknown verb `role`"), "{stderr}");
    assert!(!stderr.contains("Did you mean"), "{stderr}");
    assert!(!stderr.contains("set-role"), "{stderr}");
}

/// The project operand `workweave` once took is gone with no message of its
/// own: the word reads as the subcommand it fails to be.
#[test]
fn the_retired_workweave_project_operand_is_an_unrecognized_subcommand() {
    let (code, stderr) = run(&["workweave", "web-app", "create", "hotfix"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("unrecognized subcommand 'web-app'"),
        "{stderr}"
    );
    for mapping in ["--project", "<PROJECT>", "Did you mean", "has been"] {
        assert!(
            !stderr.contains(mapping),
            "no old-to-new mapping (`{mapping}`):\n{stderr}"
        );
    }
}

#[test]
fn the_retired_set_container_is_an_unrecognized_subcommand() {
    let (code, stderr) = run(&["workweave", "set-container", "/some/path"]);
    assert_eq!(code, Some(2), "{stderr}");
    assert!(
        stderr.contains("unrecognized subcommand 'set-container'"),
        "{stderr}"
    );
    assert!(!stderr.contains("has been"), "{stderr}");
}

/// `push --force` gets the one generic refusal: rwv has no blanket force, and
/// `--overwrite <repo>` is the flag that names the loss it consents to.
#[test]
fn push_force_gets_the_generic_refusal() {
    let (code, stderr) = run(&["push", "--force", "--dry-run"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("no blanket force") && stderr.contains("`--force` is not a flag"),
        "{stderr}"
    );
}
