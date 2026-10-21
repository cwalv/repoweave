//! Destroying a workweave's uncommitted work, or a whole workweave to create
//! one in its place, takes a consent token only CLI dispatch can mint, from
//! argv.
//!
//! `delete_workweave` and `create_workweave` are `pub`, and the Claude hook
//! calls both with no operator behind it. With a bool, its "no" was one
//! keystroke from "yes". Pinned by diagnostic code, from an external crate,
//! where every visibility narrower than `pub` looks alike.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_delete_and_create_without_consent() {
    // Control: the same calls the probes below break, spelled legally.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::activate::ActivateOptions;
        use repoweave::manifest::{ProjectName, WorkweaveName};
        use repoweave::workweave::{create_workweave, delete_workweave, DirtySource};
        use std::path::Path;
        pub fn delete(ws: &Path, p: &ProjectName, n: &WorkweaveName) {
            let _ = delete_workweave(ws, p, n, None, None, None);
        }
        pub fn create(ws: &Path, p: &ProjectName, n: &WorkweaveName) {
            let _ = create_workweave(
                ws, ws, p, n, None, DirtySource::Refuse, None, ActivateOptions::default(),
            );
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn delete_does_not_take_a_bool_for_discarding_uncommitted_work() {
    assert_fails_with(
        "E0308",
        "discarding uncommitted work takes a DiscardUncommittedConsent, not true",
        r#"
        use repoweave::manifest::{ProjectName, WorkweaveName};
        use repoweave::workweave::delete_workweave;
        use std::path::Path;
        pub fn delete(ws: &Path, p: &ProjectName, n: &WorkweaveName) {
            let _ = delete_workweave(ws, p, n, None, true, None);
        }
        "#,
    );
}

#[test]
fn create_does_not_take_a_bool_for_replacing_an_existing_workweave() {
    assert_fails_with(
        "E0308",
        "replacing a workweave takes a ReplaceExistingConsent, not true",
        r#"
        use repoweave::activate::ActivateOptions;
        use repoweave::manifest::{ProjectName, WorkweaveName};
        use repoweave::workweave::{create_workweave, DirtySource};
        use std::path::Path;
        pub fn create(ws: &Path, p: &ProjectName, n: &WorkweaveName) {
            let _ = create_workweave(
                ws, ws, p, n, true, DirtySource::Refuse, None, ActivateOptions::default(),
            );
        }
        "#,
    );
}

#[test]
fn create_does_not_take_a_bool_for_the_dirty_source() {
    assert_fails_with(
        "E0308",
        "what create does with a dirty source is a DirtySource, not a bool",
        r#"
        use repoweave::activate::ActivateOptions;
        use repoweave::manifest::{ProjectName, WorkweaveName};
        use repoweave::workweave::create_workweave;
        use std::path::Path;
        pub fn create(ws: &Path, p: &ProjectName, n: &WorkweaveName) {
            let no_replace = Default::default();
            let _ = create_workweave(ws, ws, p, n, no_replace, true, None, ActivateOptions::default());
        }
        "#,
    );
}

#[test]
fn a_discard_uncommitted_consent_is_not_minted_outside_the_cli_module() {
    assert_fails_with(
        "E0624",
        "consent is minted from a named flag, at dispatch, and nowhere else",
        r#"
        use repoweave::cli::consent::DiscardUncommittedConsent;
        pub fn mint() -> Option<DiscardUncommittedConsent> {
            DiscardUncommittedConsent::from_flag(true)
        }
        "#,
    );
}

#[test]
fn a_replace_existing_consent_is_not_minted_outside_the_cli_module() {
    assert_fails_with(
        "E0624",
        "consent is minted from a named flag, at dispatch, and nowhere else",
        r#"
        use repoweave::cli::consent::ReplaceExistingConsent;
        pub fn mint() -> Option<ReplaceExistingConsent> {
            ReplaceExistingConsent::from_flag(true)
        }
        "#,
    );
}

#[test]
fn a_discard_uncommitted_consent_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to cli::consent; only from_flag can produce one",
        r#"
        use repoweave::cli::consent::DiscardUncommittedConsent;
        pub fn forge() -> DiscardUncommittedConsent {
            DiscardUncommittedConsent(())
        }
        "#,
    );
}

#[test]
fn a_replace_existing_consent_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to cli::consent; only from_flag can produce one",
        r#"
        use repoweave::cli::consent::ReplaceExistingConsent;
        pub fn forge() -> ReplaceExistingConsent {
            ReplaceExistingConsent(())
        }
        "#,
    );
}

#[test]
fn dispatch_has_no_delete_entry_point_taking_the_flags() {
    assert_fails_with(
        "E0425",
        "the delete waivers reach a mint only from argv",
        r#"
        use repoweave::manifest::{ProjectName, WorkweaveName};
        use std::path::Path;
        pub fn delete(ws: &Path, p: &ProjectName, n: &WorkweaveName) {
            let _ = repoweave::cli::dispatch::workweave_delete(ws, p, n, true, true);
        }
        "#,
    );
}
