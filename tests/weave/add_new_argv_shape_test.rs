//! `rwv member add <path> --new` keeps its ratified argv shape: `--new` is a bare
//! boolean and the positional is one `String`.
//!
//! Fork 2's ruling kept the three-segment path as a creation shorthand read
//! through the existing `--new` flag rather than growing a value of its own.
//! 26 `["member", "add", "<path>", "--new"]` invocations and one
//! `["member", "add", "--new", "<path>"]` in `tests/docs/refusal_arrival_test.rs`
//! only both parse because
//! `--new` takes no value — a future refactor reaching for `Option<String>`
//! or a value-taking `--new` would leave a green build and a wall of red
//! tests with no single place explaining why. This pins the two structural
//! facts a green build alone does not state.

use clap::Parser;
use repoweave::cli::{Cli, Commands, MemberAction};

#[test]
fn new_flag_is_a_bare_boolean_in_either_argument_order() {
    for args in [
        ["rwv", "member", "add", "local", "--new"],
        ["rwv", "member", "add", "--new", "local"],
    ] {
        let cli = Cli::try_parse_from(args)
            .unwrap_or_else(|e| panic!("{args:?} must parse (fork 2 keeps --new value-free): {e}"));
        match cli.command {
            Some(Commands::Member {
                action: MemberAction::Add { url, new, .. },
            }) => {
                assert_eq!(url, "local", "the positional must still be one String");
                assert!(new, "--new must still be a bare boolean");
            }
            _ => panic!("{args:?} must parse as MemberAction::Add"),
        }
    }
}

#[test]
fn new_flag_rejects_being_given_a_value() {
    // If --new ever grows a value, this starts parsing instead of refusing —
    // the signal that the ratified shape moved.
    let result = Cli::try_parse_from(["rwv", "member", "add", "local", "--new=github"]);
    assert!(
        result.is_err(),
        "--new must not accept a value; a bare boolean flag rejects `--new=github`"
    );
}
