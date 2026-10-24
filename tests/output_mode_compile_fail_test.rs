//! A verb is handed the output mode the dispatcher resolved, not the `--json`
//! flag it resolves it from.
//!
//! The regression it closes: `json: bool` was threaded through the verb entry
//! points, and each one re-derived the shape of the output below it from the
//! flag and the worker count, so the formula lived in six places and drifted in
//! one (a streamed run printed an envelope). The entry points now take an
//! `OutputMode`, and the runner that has no text mode takes no mode at all.
//!
//! Pinned by error code with `common::compile_probe`: a bare `compile_fail`
//! snippet passes for any error, including a typo. The first test is a control
//! that must compile, without which a broken rustc invocation would make every
//! other assertion pass vacuously.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_compile_a_verb_handed_its_mode() {
    let (compiled, stderr) = compile(
        r#"
        use repoweave::parallel::OutputMode;
        use repoweave::selector::RepoFilter;
        use repoweave::status::run_status;
        use repoweave::sync::{run_sync, sync_to_json_run, SyncRequest};
        use repoweave::workspace::WorkspaceContext;

        pub fn modes(ctx: &WorkspaceContext, filter: &RepoFilter, request: SyncRequest) {
            let _ = run_status(ctx, OutputMode::from_flag(true), filter);
            let _ = sync_to_json_run(ctx, request);
        }
        pub fn sync(ctx: &WorkspaceContext, request: SyncRequest) {
            let _ = run_sync(ctx, request, OutputMode::Text);
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_verb_is_not_handed_the_json_flag() {
    assert_fails_with(
        "E0308",
        "status takes the resolved mode, never the flag",
        r#"
        use repoweave::selector::RepoFilter;
        use repoweave::status::run_status;
        use repoweave::workspace::WorkspaceContext;

        pub fn flag(ctx: &WorkspaceContext, filter: &RepoFilter) {
            let _ = run_status(ctx, true, filter);
        }
        "#,
    );
}

#[test]
fn push_is_not_handed_the_json_flag() {
    assert_fails_with(
        "E0308",
        "push takes the resolved mode, never the flag",
        r#"
        use repoweave::push::{run_push, PushMode};
        use repoweave::selector::RepoFilter;
        use repoweave::workspace::WorkspaceContext;

        pub fn flag(ctx: &WorkspaceContext, filter: &RepoFilter) {
            let _ = run_push(ctx, PushMode::DryRun, filter, 1, true);
        }
        "#,
    );
}

#[test]
fn the_workweave_log_is_not_handed_the_json_flag() {
    assert_fails_with(
        "E0308",
        "the workweave log takes the resolved mode, never the flag",
        r#"
        use repoweave::workspace::WorkspaceContext;

        pub fn flag(ctx: &WorkspaceContext) {
            let _ = repoweave::workweave::workweave_log(ctx, false, true);
        }
        "#,
    );
}

#[test]
fn sync_decides_text_or_json_from_the_mode_it_is_handed() {
    assert_fails_with(
        "E0061",
        "run_sync has no separate json entry to choose between; it takes the mode",
        r#"
        use repoweave::sync::{run_sync, SyncRequest};
        use repoweave::workspace::WorkspaceContext;

        pub fn two_args(ctx: &WorkspaceContext, request: SyncRequest) {
            let _ = run_sync(ctx, request);
        }
        "#,
    );
}

#[test]
fn the_json_only_runner_is_handed_no_mode() {
    assert_fails_with(
        "E0061",
        "the sync-to json runner has one shape, so nothing chooses it",
        r#"
        use repoweave::parallel::OutputMode;
        use repoweave::sync::{sync_to_json_run, SyncRequest};
        use repoweave::workspace::WorkspaceContext;

        pub fn text(ctx: &WorkspaceContext, request: SyncRequest) {
            let _ = sync_to_json_run(ctx, request, OutputMode::Text);
        }
        "#,
    );
}
