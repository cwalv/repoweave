//! `rwv doctor`'s run is asked for in named terms and answers with a
//! `Verdict`; neither side is a bool.
//!
//! `run_check(ctx, fix, scope_all, ..)` took two adjacent bools, so a call
//! that swapped them compiled and ran the other doctor, and the three runners
//! answered `Result<bool>` where `true` meant "exit 1". Text and `--json`
//! once computed that answer in two places and disagreed. Each runner takes a
//! `Scope` (and a `Repair` where it can fix) and returns a `Verdict`, the one
//! type the process exit is read from. Pinned by diagnostic code, from an
//! external crate, where every visibility narrower than `pub` looks alike.
//!
//! Residue: which findings fail a run is runtime behavior, pinned by
//! `doctor_exit_code_contract_test.rs` through the shipped binary and by the
//! unit tests beside `doctor_verdict`.

mod common;

use common::compile_probe::{assert_fails_n_times, assert_fails_with, compile};

#[test]
fn the_harness_can_ask_doctor_in_named_terms_and_read_a_verdict() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::check::{run_check, run_check_json, run_check_locked, Repair, Scope, Verdict};
        use repoweave::workspace::WorkspaceContext;
        pub fn failed(ctx: &WorkspaceContext) -> bool {
            let text = run_check(ctx, Repair::Report, Scope::Weave, None, None, None);
            let json = run_check_json(ctx, Scope::ActiveProject, None);
            let locked = run_check_locked(ctx);
            [text, json, locked]
                .into_iter()
                .filter_map(|run| run.ok())
                .any(|verdict: Verdict| verdict == Verdict::Fail)
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn no_runner_takes_its_scope_as_a_bool() {
    assert_fails_n_times(
        "E0308",
        2,
        "the scope of a doctor run is a Scope, not `--all` as a bool",
        r#"
        use repoweave::check::{run_check, run_check_json, Repair};
        use repoweave::workspace::WorkspaceContext;
        pub fn run(ctx: &WorkspaceContext) {
            let _ = run_check(ctx, Repair::Report, true, None, None, None);
            let _ = run_check_json(ctx, true, None);
        }
        "#,
    );
}

#[test]
fn run_check_does_not_take_fix_as_a_bool() {
    assert_fails_with(
        "E0308",
        "whether a doctor run repairs is a Repair, not `--fix` as a bool",
        r#"
        use repoweave::check::{run_check, Scope};
        use repoweave::workspace::WorkspaceContext;
        pub fn run(ctx: &WorkspaceContext) {
            let _ = run_check(ctx, true, Scope::Weave, None, None, None);
        }
        "#,
    );
}

#[test]
fn no_runner_answers_with_a_bool() {
    assert_fails_n_times(
        "E0308",
        3,
        "a doctor run answers with a Verdict, not whether to exit 1",
        r#"
        use repoweave::check::{run_check, run_check_json, run_check_locked, Repair, Scope};
        use repoweave::workspace::WorkspaceContext;
        pub fn run(ctx: &WorkspaceContext) -> Result<bool, ()> {
            let a: bool = run_check(ctx, Repair::Report, Scope::Weave, None, None, None).map_err(|_| ())?;
            let b: bool = run_check_json(ctx, Scope::Weave, None).map_err(|_| ())?;
            let c: bool = run_check_locked(ctx).map_err(|_| ())?;
            Ok(a || b || c)
        }
        "#,
    );
}

#[test]
fn a_verdict_is_not_a_bool() {
    assert_fails_with(
        "E0308",
        "Verdict does not stand where a condition is read",
        r#"
        use repoweave::check::Verdict;
        pub fn failed(verdict: Verdict) -> bool {
            if verdict {
                return true;
            }
            false
        }
        "#,
    );
}
