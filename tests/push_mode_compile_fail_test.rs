//! `rwv push` is a dry run or a publish, and a publish overwrites remote
//! history only with a consent token CLI dispatch mints from `--force`. Pinned
//! by diagnostic code, from an external crate, where every visibility narrower
//! than `pub` looks alike.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_push_in_each_mode() {
    // Control: the calls the probes below break, spelled legally.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::parallel::OutputMode;
        use repoweave::push::{run_push, PushMode};
        use repoweave::selector::RepoFilter;
        use repoweave::vcs::{Force, PublishRef, Vcs};
        use repoweave::workspace::WorkspaceContext;
        use std::path::Path;
        pub fn push(ctx: &WorkspaceContext, filter: &RepoFilter) {
            let _ = run_push(ctx, PushMode::DryRun, filter, 1, OutputMode::Text);
            let _ = run_push(
                ctx,
                PushMode::Publish(Force::FfOnly),
                filter,
                1,
                OutputMode::Text,
            );
        }
        pub fn push_one(vcs: &dyn Vcs, repo: &Path, r: &PublishRef) {
            let _ = vcs.push_ref(repo, r, Force::FfOnly);
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn run_push_does_not_take_dry_run_and_force_bools() {
    assert_fails_with(
        "E0061",
        "the dry-run and force bools are one PushMode",
        r#"
        use repoweave::push::run_push;
        use repoweave::selector::RepoFilter;
        use repoweave::workspace::WorkspaceContext;
        pub fn push(ctx: &WorkspaceContext, filter: &RepoFilter) {
            let _ = run_push(ctx, true, false, filter, 1, false);
        }
        "#,
    );
}

#[test]
fn push_ref_does_not_take_a_bool_for_force() {
    assert_fails_with(
        "E0308",
        "overwriting the remote takes a Force, not true",
        r#"
        use repoweave::vcs::{PublishRef, Vcs};
        use std::path::Path;
        pub fn push_one(vcs: &dyn Vcs, repo: &Path, r: &PublishRef) {
            let _ = vcs.push_ref(repo, r, true);
        }
        "#,
    );
}

#[test]
fn a_force_push_consent_is_not_minted_outside_the_cli_module() {
    assert_fails_with(
        "E0624",
        "consent is minted from a named flag, at dispatch, and nowhere else",
        r#"
        use repoweave::cli::consent::ForcePushConsent;
        pub fn mint() -> Option<ForcePushConsent> {
            ForcePushConsent::from_flag(true)
        }
        "#,
    );
}

#[test]
fn a_force_push_consent_has_no_unconditional_mint() {
    assert_fails_with(
        "E0599",
        "granted() exists only in a test build of the crate",
        r#"
        use repoweave::cli::consent::ForcePushConsent;
        pub fn mint() -> ForcePushConsent {
            ForcePushConsent::granted()
        }
        "#,
    );
}

#[test]
fn a_force_push_consent_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to cli::consent; only from_flag can produce one",
        r#"
        use repoweave::cli::consent::ForcePushConsent;
        pub fn forge() -> ForcePushConsent {
            ForcePushConsent(())
        }
        "#,
    );
}
