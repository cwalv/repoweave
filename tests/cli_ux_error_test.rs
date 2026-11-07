//! CLI error/help UX regressions.
//!
//! The "For more information, try '--help'" footer is emitted on every clap
//! error even when `--help` was already on the command line. We suppress that
//! one footer line whenever `--help`/`-h` is present, and keep it otherwise.
//! A mistyped workweave subcommand gets clap's own similar-subcommand tip.

mod common;
use common::rwv;
use predicates::prelude::*;

/// A subcommand *typo* (`crete`, edit distance 1 from `create`) reaches clap,
/// whose native "unrecognized subcommand / tip: a similar subcommand exists"
/// message names the real subcommand.
#[test]
fn workweave_subcommand_typo_gets_clap_tip() {
    rwv()
        .args(["workweave", "crete", "myseat"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unrecognized subcommand 'crete'")
                .and(predicate::str::contains("a similar subcommand exists"))
                .and(predicate::str::contains("'create'")),
        );
}

/// A bare `rwv workweave` names no action to run, so it shows the subcommands
/// rather than listing or creating anything.
#[test]
fn bare_workweave_shows_the_subcommands() {
    rwv().args(["workweave"]).assert().failure().stderr(
        predicate::str::contains("Usage: rwv workweave")
            .and(predicate::str::contains("create"))
            .and(predicate::str::contains("delete"))
            .and(predicate::str::contains("list"))
            .and(predicate::str::contains("log"))
            .and(predicate::str::contains("set-default-create-basedir")),
    );
}

/// `rwv setup invalid-sub --help` — InvalidSubcommand error kind. clap shows
/// the error + usage; the footer must be suppressed because `--help` was
/// already passed.
#[test]
fn setup_invalid_subcommand_with_help_has_no_footer() {
    rwv()
        .args(["setup", "invalid-sub", "--help"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unrecognized subcommand")
                .and(predicate::str::contains("try '--help'").not()),
        );
}

/// `rwv unknown-verb --help` — top-level unknown verb. With external-
/// subcommand fallthrough, an unknown verb at the top level
/// resolves through the plugin path: rwv looks for `rwv-unknown-verb` on
/// `$PATH`, and when not found emits the rwv-side unknown-verb error.
/// `--help` is captured as an argument to the (would-be) plugin, so the
/// help footer contract does not apply here — this is now the plugin-
/// dispatch surface, not the clap error surface. We still assert the
/// error path is stable (nonzero exit, verb + PATH mentioned) so a
/// regression in the plugin dispatch surface is caught.
#[test]
fn unknown_verb_with_help_has_no_footer() {
    // Use a PATH that intentionally does NOT contain a real `rwv-unknown-verb`.
    // A tempdir-only PATH prevents an unrelated PATH plugin from silently
    // handling the invocation.
    let plugin_dir = common::tempdir().expect("tempdir");
    rwv()
        .args(["unknown-verb", "--help"])
        .env("PATH", plugin_dir.path().to_string_lossy().to_string())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unknown verb")
                .and(predicate::str::contains("rwv-unknown-verb"))
                .and(predicate::str::contains("PATH")),
        );
}

/// `rwv activate foo bar --help` — UnknownArgument error kind. Confirms footer
/// suppression is uniform across the third clap error kind too.
#[test]
fn unexpected_argument_with_help_has_no_footer() {
    rwv()
        .args(["activate", "foo", "bar", "--help"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unexpected argument")
                .and(predicate::str::contains("try '--help'").not()),
        );
}

/// Footer must still appear on a normal clap error when `--help` was NOT
/// passed — the suppression is conditional, not a blanket removal.
#[test]
fn footer_retained_without_help() {
    rwv()
        .args(["activate", "foo", "bar"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("try '--help'"));
}

/// `rwv workweave --help` shows help and exits 0.
#[test]
fn workweave_help_shows_help() {
    rwv()
        .args(["workweave", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: rwv workweave"));
}

/// The real subcommand API reaches clap: `rwv workweave create` with no name
/// yields clap's standard required-argument error.
#[test]
fn workweave_create_reaches_clap() {
    rwv()
        .args(["workweave", "create", "--project", "foundations"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("<NAME>"));
}
