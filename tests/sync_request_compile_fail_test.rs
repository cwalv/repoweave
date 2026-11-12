//! A landing request is a fresh op or a resume, and only the fresh one takes
//! the op's parameters. Pinned by diagnostic code, from an external crate,
//! where every visibility narrower than `pub` looks alike.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_build_both_requests_without_consent() {
    // Control: the request the probe below breaks, spelled legally.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::op_state::{ResolvedOwner, SyncStrategy};
        use repoweave::land::Landing;
        use repoweave::sync::SyncRequest;
        pub fn fresh(landing: Box<Landing>) -> SyncRequest {
            SyncRequest::Fresh {
                landing,
                strategy: SyncStrategy::Ff,
                retire: false,
                jobs: 1,
            }
        }
        pub fn resume(op: Box<ResolvedOwner>) -> SyncRequest {
            SyncRequest::Continue { op, project_override: None }
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
                project_override: None,
                retire: true,
            }
        }
        "#,
    );
}
