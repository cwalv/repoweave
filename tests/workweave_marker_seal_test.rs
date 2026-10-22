//! `WorkweaveMarker`'s three fields are private; `WorkweaveMarker::new` is
//! the only constructor, and it resolves `primary` and `parent` on the way in
//! — a step a field-by-field literal has no way to be forced through. Unlike
//! `WeaveRootIdentity`'s payloads (see `root_identity_seal_test.rs`), `new`
//! is meant to be called from many sites — production and tests alike — so
//! there is no "exactly one caller" invariant to pin here, only "every
//! caller goes through `new`, nobody reaches for the literal directly".
//!
//! The type and `new` sit alone in their own module, so privacy covers every
//! other module of the crate, `workspace.rs` included: the literal in `new`
//! is the only one that compiles. The probes below pin that refusal by
//! diagnostic code from outside the crate, where it is the same refusal any
//! sibling module would hit.
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
        use repoweave::manifest::ProjectName;
        use repoweave::workspace::WorkweaveMarker;
        use std::path::Path;
        pub fn legal(primary: &Path, project: ProjectName, parent: &Path) -> WorkweaveMarker {
            let marker = WorkweaveMarker::new(primary, project, parent);
            let _ = (marker.primary(), marker.project(), marker.parent());
            marker
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_marker_cannot_be_assembled_field_by_field_from_outside_the_crate() {
    assert_fails_with(
        "E0451",
        "WorkweaveMarker's fields are private; only `new` builds one",
        r#"
        use repoweave::manifest::ProjectName;
        use repoweave::path_spelling::CanonicalPath;
        use repoweave::workspace::WorkweaveMarker;
        pub fn forge(
            primary: CanonicalPath,
            project: ProjectName,
            parent: CanonicalPath,
        ) -> WorkweaveMarker {
            WorkweaveMarker { primary, project, parent }
        }
        "#,
    );
}

#[test]
fn a_marker_field_cannot_be_read_directly_from_outside_the_crate() {
    assert_fails_with(
        "E0616",
        "reading `parent` bypasses the `parent()` accessor",
        r#"
        use repoweave::workspace::WorkweaveMarker;
        use std::path::Path;
        pub fn peek(marker: &WorkweaveMarker) -> &Path {
            &marker.parent
        }
        "#,
    );
}
