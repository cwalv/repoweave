//! `WeaveRootIdentity` has exactly one producer: `require_exclusive`.
//!
//! The type is a projection of `RootObservation`, and the two observations
//! that projection refuses — a root carrying both identity files, and a
//! marker that cannot witness what it claims — have no representation in it.
//! That is only worth anything while the projection is the sole way to get
//! one. A consumer that could assemble the workweave arm from a marker it
//! read itself would be deciding, at its own site, the marker-versus-pointer
//! tiebreak the projection exists to refuse — and it would do it silently,
//! because the assembled value is indistinguishable from a projected one.
//!
//! The payload structs carry private fields and sit, with `require_exclusive`
//! and nothing else that builds them, in their own module. Privacy therefore
//! refuses the literal in every other module of the crate, `workspace.rs`
//! included. Pinned by diagnostic code rather than by "somehow fails", and
//! pinned from an external crate — where every visibility narrower than `pub`
//! looks alike, so the refusal an out-of-crate probe demonstrates is the same
//! one `check.rs` or `dispatch.rs` would hit.
//!
//! Residue: a field widened to `pub(super)` or `pub(crate)` lets the rest of
//! the crate build the literal and leaves these probes green. A reviewer
//! changing a field's visibility is the person this note is for.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_compile_a_legal_snippet() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::workspace::{observe_root, WeaveRootIdentity};
        use std::path::Path;
        pub fn legal(dir: &Path) -> Option<WeaveRootIdentity> {
            observe_root(dir)?.require_exclusive().ok()
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn the_workweave_arm_cannot_be_assembled_from_a_marker() {
    // This is the shape the guard is about: a caller that reads the marker
    // itself, sees a workweave, and declares one — never having looked for
    // the `.rwv-active` beside it.
    assert_fails_with(
        "E0451",
        "the workweave arm is projected from an observation, not declared",
        r#"
        use repoweave::workspace::{WeaveRootIdentity, WorkweaveIdentity, WorkweaveMarker};
        pub fn forge(marker: WorkweaveMarker) -> WeaveRootIdentity {
            WeaveRootIdentity::Workweave(WorkweaveIdentity { marker })
        }
        "#,
    );
}

#[test]
fn the_primary_arm_cannot_be_assembled_from_a_pointer() {
    // Every field is supplied. A literal that omits one fails for the
    // uninteresting reason instead, and would keep failing after the fields
    // it does name became public.
    assert_fails_with(
        "E0451",
        "the primary arm is projected from an observation, not declared",
        r#"
        use repoweave::manifest::ProjectName;
        use repoweave::workspace::{PrimaryIdentity, WeaveRootIdentity};
        use std::path::PathBuf;
        pub fn forge(root: PathBuf, selection: Option<ProjectName>) -> WeaveRootIdentity {
            WeaveRootIdentity::Primary(PrimaryIdentity { root, selection })
        }
        "#,
    );
}
