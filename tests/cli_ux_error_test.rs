//! CLI error/help UX regressions (rwv-b2z, epic fo-gyt1tq).
//!
//! Two problem classes, fixed uniformly in `src/main.rs` via pre-parse
//! raw-arg interception (see the rwv CLI UX audit,
//! `projects/foundations/docs/repoweave/rwv-cli-ux-audit.md`):
//!
//!   1. `rwv workweave <PROJECT> <WORD>` — clap consumes <PROJECT> as the
//!      `[PROJECT]` positional, then sees <WORD> as an "unexpected argument"
//!      and never reaches the subcommand "did you mean" path. We intercept and
//!      reframe it as a missing-subcommand error with a `create`-shaped hint.
//!
//!   2. The "For more information, try '--help'" footer is emitted on every
//!      clap error even when `--help` was already on the command line. We
//!      suppress that one footer line whenever `--help`/`-h` is present.

mod common;
use common::rwv;
use predicates::prelude::*;

/// `rwv workweave foundations fo-city` must name the problem as a missing
/// subcommand, list the available subcommands, and offer the `create`-shaped
/// did-you-mean — NOT clap's opaque "unexpected argument 'fo-city'".
#[test]
fn workweave_bare_name_is_reframed_as_missing_subcommand() {
    rwv()
        .args(["workweave", "foundations", "fo-city"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("is not a valid subcommand")
                .and(predicate::str::contains("Available subcommands"))
                .and(predicate::str::contains("create"))
                .and(predicate::str::contains("delete"))
                .and(predicate::str::contains("list"))
                .and(predicate::str::contains("Did you mean"))
                .and(predicate::str::contains("create fo-city"))
                // The old clap message must be gone.
                .and(predicate::str::contains("unexpected argument").not()),
        );
}

/// Same invocation with a trailing `--help`: still the reframed error (the
/// 3rd token is a bare name, not a flag, so interception still fires), and the
/// "try '--help'" footer must NOT appear.
#[test]
fn workweave_bare_name_with_help_has_no_footer() {
    rwv()
        .args(["workweave", "foundations", "fo-city", "--help"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("is not a valid subcommand")
                .and(predicate::str::contains("create fo-city"))
                .and(predicate::str::contains("try '--help'").not()),
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

/// `rwv unknown-verb --help` — top-level InvalidSubcommand. Same footer
/// suppression contract.
#[test]
fn unknown_verb_with_help_has_no_footer() {
    rwv()
        .args(["unknown-verb", "--help"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("unrecognized subcommand")
                .and(predicate::str::contains("try '--help'").not()),
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

/// Regression guard: `rwv workweave foundations` (no 3rd token) must keep
/// defaulting to `list` — interception must NOT fire here. Running outside a
/// workspace just needs to not be the reframed subcommand error; whatever the
/// list path does (succeed with names, or fail resolving the workspace) it
/// must not produce the "is not a valid subcommand" message.
#[test]
fn workweave_project_only_is_not_intercepted() {
    rwv()
        .args(["workweave", "foundations"])
        .assert()
        .stderr(predicate::str::contains("is not a valid subcommand").not());
}

/// `rwv workweave foundations --help` (3rd token is a flag) must show help and
/// exit 0 — the interceptor must skip flag tokens.
#[test]
fn workweave_project_help_shows_help() {
    rwv()
        .args(["workweave", "foundations", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage: rwv workweave"));
}

/// The real subcommand API must keep working: `rwv workweave <PROJECT> create`
/// (a known subcommand) must reach clap, not the interceptor — so a missing
/// `<NAME>` yields clap's standard required-argument error, not the reframed
/// "is not a valid subcommand" message.
#[test]
fn workweave_create_reaches_clap() {
    rwv()
        .args(["workweave", "foundations", "create"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("is not a valid subcommand")
                .not()
                .and(predicate::str::contains("<NAME>")),
        );
}
