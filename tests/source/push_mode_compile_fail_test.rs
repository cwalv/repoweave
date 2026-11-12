//! `rwv push` is a dry run or a publish, and a publish replaces a remote
//! branch only under a lease that `OverwriteConsent` mints, for a repo named
//! with `--overwrite`. Pinned by diagnostic code, from an external crate,
//! where every visibility narrower than `pub` looks alike.

use crate::common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_push_in_each_mode() {
    // Control: the calls the probes below break, spelled legally.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::cli::consent::OverwriteConsent;
        use repoweave::parallel::OutputMode;
        use repoweave::push::{run_push, PushMode, PushRequest};
        use repoweave::selector::RepoFilter;
        use repoweave::vcs::{PublishRef, RemoteHandle, Replace, Vcs};
        use repoweave::workspace::WorkspaceContext;
        use std::path::Path;
        pub fn push(ctx: &WorkspaceContext, filter: &RepoFilter) {
            let overwrite = OverwriteConsent::default();
            for mode in [PushMode::DryRun, PushMode::Publish] {
                let _ = run_push(
                    ctx,
                    PushRequest {
                        mode,
                        filter,
                        overwrite: &overwrite,
                        branch: None,
                        jobs: 1,
                        output: OutputMode::Text,
                    },
                );
            }
        }
        pub fn push_one(vcs: &dyn Vcs, repo: &Path, to: &RemoteHandle, r: &PublishRef) {
            let _ = vcs.push_ref(repo, to, r, &Replace::FastForwardOnly);
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn run_push_does_not_take_dry_run_and_overwrite_bools() {
    assert_fails_with(
        "E0061",
        "the request is one PushRequest, not loose flags",
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
fn push_ref_does_not_take_a_bool_for_replacing_the_remote() {
    assert_fails_with(
        "E0308",
        "replacing the remote takes a Replace, not true",
        r#"
        use repoweave::vcs::{PublishRef, RemoteHandle, Vcs};
        use std::path::Path;
        pub fn push_one(vcs: &dyn Vcs, repo: &Path, to: &RemoteHandle, r: &PublishRef) {
            let _ = vcs.push_ref(repo, to, r, &true);
        }
        "#,
    );
}

#[test]
fn an_overwrite_consent_is_not_minted_outside_the_cli_module() {
    assert_fails_with(
        "E0624",
        "consent is minted from a named flag, at dispatch, and nowhere else",
        r#"
        use repoweave::cli::consent::OverwriteConsent;
        use repoweave::manifest::RepoOperand;
        pub fn mint(repos: &[RepoOperand]) -> OverwriteConsent {
            OverwriteConsent::from_flag(repos)
        }
        "#,
    );
}

#[test]
fn an_overwrite_consent_has_no_unconditional_mint() {
    assert_fails_with(
        "E0599",
        "granted() exists only in a test build of the crate",
        r#"
        use repoweave::cli::consent::OverwriteConsent;
        use repoweave::manifest::RepoOperand;
        pub fn mint(repo: RepoOperand) -> OverwriteConsent {
            OverwriteConsent::granted(repo)
        }
        "#,
    );
}

#[test]
fn a_lease_cannot_be_forged_by_struct_literal() {
    assert_fails_with(
        "E0451",
        "the lease's field is private to cli::consent; only OverwriteConsent::lease can mint one",
        r#"
        use repoweave::cli::consent::Lease;
        use repoweave::vcs::ResolvedRevisionId;
        pub fn forge(tip: ResolvedRevisionId) -> Lease {
            Lease { expected: tip }
        }
        "#,
    );
}
