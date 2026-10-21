//! `drift_issues` takes who holds the pen, not the two facts it is resolved
//! from, so a swap of them is a compile error instead of a finding.
//!
//! The regression it closes: the call took `marker_present: bool,
//! owned_key_present: bool` and `user_held_suffix: &str, drift_detail: &str`
//! side by side. A swapped bool pair reads unmarked-and-present user content as
//! rwv's, and makes the USER-HELD finding, which `rwv doctor --fix` does not
//! touch, a DRIFT one, which it overwrites.
//!
//! Pinned by error code with `common::compile_probe`: a bare `compile_fail`
//! snippet passes for any error, including a typo. The first test is a control
//! that must compile, without which a broken rustc invocation would make every
//! other assertion pass vacuously.
//!
//! `MarkerEdit` is private to `merge.rs`, which is what makes the second state
//! of its two-flag predecessor unrepresentable outside the module that owns it.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_compile_a_call_that_names_the_pen() {
    let (compiled, stderr) = compile(
        r#"
        use repoweave::integrations::merge::{drift_issues, ManagedDoc, Pen, Remedy, YamlDoc};
        use std::path::Path;

        pub fn legal(doc: &YamlDoc) -> usize {
            let key = vec!["packages".to_string()];
            let pen = doc.pen(&[key.clone()], &key, Some(&[]));
            let user = drift_issues(
                "pnpm-workspaces",
                Path::new("/p/pnpm-workspace.yaml"),
                Pen::User,
                &[],
                Remedy { user_held: "cut over", drift: "detail" },
            );
            let rwv = drift_issues(
                "pnpm-workspaces",
                Path::new("/p/pnpm-workspace.yaml"),
                pen,
                &[],
                Remedy { user_held: "cut over", drift: "detail" },
            );
            user.len() + rwv.len()
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn the_two_bool_call_no_longer_compiles() {
    assert_fails_with(
        "E0061",
        "a marker flag and a key flag are not parameters of drift_issues",
        r#"
        use repoweave::integrations::merge::drift_issues;
        use std::path::Path;

        pub fn old_shape() -> usize {
            drift_issues(
                "pnpm-workspaces",
                Path::new("/p/pnpm-workspace.yaml"),
                false,
                true,
                None,
                &[],
                "cut over",
                "detail",
            )
            .len()
        }
        "#,
    );
}

#[test]
fn a_bool_is_not_a_pen() {
    // Same arity as the real call, so this is the type refusing the bool
    // rather than the argument count refusing the call.
    assert_fails_with(
        "E0308",
        "who holds the pen is stated by naming it, never by a flag",
        r#"
        use repoweave::integrations::merge::{drift_issues, Remedy};
        use std::path::Path;

        pub fn flag() -> usize {
            drift_issues(
                "pnpm-workspaces",
                Path::new("/p/pnpm-workspace.yaml"),
                true,
                &[],
                Remedy { user_held: "cut over", drift: "detail" },
            )
            .len()
        }
        "#,
    );
}

#[test]
fn the_remedy_is_one_value_not_two_adjacent_strings() {
    assert_fails_with(
        "E0308",
        "the two sentences are named fields, so they cannot be passed swapped by position",
        r#"
        use repoweave::integrations::merge::{drift_issues, Pen};
        use std::path::Path;

        pub fn strings() -> usize {
            drift_issues(
                "pnpm-workspaces",
                Path::new("/p/pnpm-workspace.yaml"),
                Pen::User,
                &[],
                "cut over",
            )
            .len()
        }
        "#,
    );
}

#[test]
fn a_marker_edit_cannot_be_named_outside_merge() {
    assert_fails_with(
        "E0603",
        "the marker edit a document applies is private to the module that applies it",
        r#"
        pub fn edit() {
            let _ = repoweave::integrations::merge::MarkerEdit::Set;
        }
        "#,
    );
}
