//! Project selection is expressible only where it is legal.
//!
//! Writing `.rwv-active` into a workweave root puts a second, unread copy of
//! the workweave's own identity beside the marker that already names it —
//! the state `rwv doctor` reports as `weave-root-identity-conflict`. Until
//! now the prohibition was a runtime `if` in front of a free
//! `set_active_project(root, project)`: correct, and invisible to anything
//! but review. A caller reaching that function with a marker root compiled
//! fine.
//!
//! The write now hangs off `PrimaryIdentity`, which `require_exclusive`
//! produces only for a root carrying no marker, and which carries the root it
//! was observed at. Both halves are load-bearing. Without the projection any
//! caller could declare itself primary; without the carried root the write
//! would still take a directory as an argument, and the witness would attest
//! that *some* primary root exists while the caller wrote into a different
//! one.
//!
//! Two halves, mirroring `root_identity_seal_test.rs`:
//!
//! 1. **Compile probes.** The workweave arm has no way to select, and there
//!    is no path-taking selection function to fall back to. Pinned by
//!    diagnostic code — "somehow fails" is satisfied by a typo — and from an
//!    external crate, where every visibility narrower than `pub` looks alike.
//! 2. **A source scan**, in `weave_root_probes_stay_deleted_test.rs`, because
//!    privacy stops at the module that defines the field: a second selection
//!    path added inside `workspace.rs` would compile.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_compile_a_legal_selection() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::manifest::ProjectName;
        use repoweave::workspace::{observe_root, WeaveRootIdentity};
        use std::path::Path;
        pub fn legal(dir: &Path, project: &ProjectName) -> bool {
            match observe_root(dir) {
                Some(observation) => match observation.require_exclusive() {
                    Ok(WeaveRootIdentity::Primary(primary)) => {
                        primary.select_project(project).is_ok()
                    }
                    _ => false,
                },
                None => false,
            }
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn the_workweave_arm_cannot_select_a_project() {
    // The shape the guard is about: a caller that has classified the root,
    // knows it is a workweave, and writes the pointer anyway.
    assert_fails_with(
        "E0599",
        "selection is not reachable from the workweave arm",
        r#"
        use repoweave::manifest::ProjectName;
        use repoweave::workspace::WorkweaveIdentity;
        pub fn select(identity: WorkweaveIdentity, project: &ProjectName) {
            let _ = identity.select_project(project);
        }
        "#,
    );
}

#[test]
fn a_bare_path_cannot_select_a_project() {
    // The free function the witness replaced. Its absence is what stops a
    // caller from routing around the classification entirely.
    assert_fails_with(
        "E0432",
        "no path-taking selection function survives",
        r#"
        use repoweave::manifest::ProjectName;
        use repoweave::workspace::set_active_project;
        use std::path::Path;
        pub fn select(root: &Path, project: &ProjectName) {
            let _ = set_active_project(root, project);
        }
        "#,
    );
}

#[test]
fn the_witness_cannot_be_pointed_at_a_root_it_did_not_observe() {
    // The carried root is why the witness attests about *this* directory
    // rather than about primary roots in general: there is no second
    // argument for a caller to disagree with the first.
    assert_fails_with(
        "E0061",
        "selection takes no directory argument",
        r#"
        use repoweave::manifest::ProjectName;
        use repoweave::workspace::PrimaryIdentity;
        use std::path::Path;
        pub fn select(identity: PrimaryIdentity, elsewhere: &Path, project: &ProjectName) {
            let _ = identity.select_project(elsewhere, project);
        }
        "#,
    );
}
