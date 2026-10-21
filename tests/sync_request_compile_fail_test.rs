//! A sync request is a fresh op or a resume, and only the fresh one takes the
//! op's parameters. Rewinding the project repo past local commits takes a
//! consent token only CLI dispatch can mint, from argv or from the record a
//! `--continue` resumes. Pinned by diagnostic code, from an external crate,
//! where every visibility narrower than `pub` looks alike.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_build_both_requests_without_consent() {
    // Control: the requests the probes below break, spelled legally.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::op_state::{ResolvedOwner, SyncStrategy};
        use repoweave::sync::{SyncRequest, SyncSource};
        pub fn fresh() -> SyncRequest {
            SyncRequest::Fresh {
                source: SyncSource::Primary,
                strategy: SyncStrategy::Ff,
                allow_stale_lock: false,
                discard_local_commits: None,
                retire: false,
                jobs: 1,
            }
        }
        pub fn resume(op: Box<ResolvedOwner>) -> SyncRequest {
            SyncRequest::Continue { op, discard_local_commits: None, project_override: None }
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_resume_cannot_ask_for_a_retire() {
    assert_fails_with(
        "E0559",
        "a resume takes its parameters from the op's record, retire included",
        r#"
        use repoweave::op_state::ResolvedOwner;
        use repoweave::sync::SyncRequest;
        pub fn resume(op: Box<ResolvedOwner>) -> SyncRequest {
            SyncRequest::Continue {
                op,
                discard_local_commits: None,
                project_override: None,
                retire: true,
            }
        }
        "#,
    );
}

#[test]
fn a_fresh_request_does_not_take_a_bool_for_discarding_local_commits() {
    assert_fails_with(
        "E0308",
        "discarding local commits takes a DiscardLocalCommitsConsent, not true",
        r#"
        use repoweave::op_state::SyncStrategy;
        use repoweave::sync::{SyncRequest, SyncSource};
        pub fn fresh() -> SyncRequest {
            SyncRequest::Fresh {
                source: SyncSource::Primary,
                strategy: SyncStrategy::Ff,
                allow_stale_lock: false,
                discard_local_commits: true,
                retire: false,
                jobs: 1,
            }
        }
        "#,
    );
}

#[test]
fn a_discard_local_commits_consent_is_not_minted_from_a_flag_outside_cli() {
    assert_fails_with(
        "E0624",
        "consent is minted from a named flag, at dispatch, and nowhere else",
        r#"
        use repoweave::cli::consent::DiscardLocalCommitsConsent;
        pub fn mint() -> Option<DiscardLocalCommitsConsent> {
            DiscardLocalCommitsConsent::from_flag(true)
        }
        "#,
    );
}

#[test]
fn a_discard_local_commits_consent_is_not_minted_from_a_record_outside_cli() {
    assert_fails_with(
        "E0624",
        "a resumed op's consent is minted at dispatch from the record it resolved",
        r#"
        use repoweave::cli::consent::DiscardLocalCommitsConsent;
        use repoweave::op_state::ResolvedOwner;
        pub fn mint(op: &ResolvedOwner) -> Option<DiscardLocalCommitsConsent> {
            DiscardLocalCommitsConsent::from_record(op)
        }
        "#,
    );
}

#[test]
fn a_discard_local_commits_consent_has_no_unconditional_mint() {
    assert_fails_with(
        "E0599",
        "granted() exists only in a test build of the crate",
        r#"
        use repoweave::cli::consent::DiscardLocalCommitsConsent;
        pub fn mint() -> DiscardLocalCommitsConsent {
            DiscardLocalCommitsConsent::granted()
        }
        "#,
    );
}

#[test]
fn a_discard_local_commits_consent_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to cli::consent; only its mints can produce one",
        r#"
        use repoweave::cli::consent::DiscardLocalCommitsConsent;
        pub fn forge() -> DiscardLocalCommitsConsent {
            DiscardLocalCommitsConsent(())
        }
        "#,
    );
}
