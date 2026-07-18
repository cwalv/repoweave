//! Integration test: CLI type definitions are accessible from the library crate.
//!
//! Pins `repoweave::cli::Cli` as a stable lib export so that other binaries and
//! tests can call `Cli::command()` to introspect the command tree without
//! depending on the `rwv` bin crate.

use clap::CommandFactory;
use repoweave::cli::Cli;

/// The top-level command tree must include the known top-level subcommands.
/// Any accidental removal would be caught here before the binary is shipped.
#[test]
fn cli_command_tree_contains_known_subcommands() {
    let cmd = Cli::command();

    let subcommand_names: Vec<&str> = cmd.get_subcommands().map(|sc| sc.get_name()).collect();

    for expected in &[
        "status",
        "sync",
        "sync-to",
        "workweave",
        "doctor",
        "fetch",
        "explain",
        "lock",
        "abort",
        "push",
        "update",
    ] {
        assert!(
            subcommand_names.contains(expected),
            "expected subcommand {:?} not found in Cli::command() tree; got: {:?}",
            expected,
            subcommand_names
        );
    }
}

/// `workweave` has nested subcommands; verify the tree descends correctly.
#[test]
fn cli_workweave_has_expected_actions() {
    let cmd = Cli::command();

    let ww = cmd
        .get_subcommands()
        .find(|sc| sc.get_name() == "workweave")
        .expect("workweave subcommand must be present");

    let actions: Vec<&str> = ww.get_subcommands().map(|sc| sc.get_name()).collect();

    for expected in &["create", "delete", "list", "log"] {
        assert!(
            actions.contains(expected),
            "expected workweave action {:?} not found; got: {:?}",
            expected,
            actions
        );
    }
}
