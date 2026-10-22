//! A `CanonicalPath` is a location's identity, and an identity is compared
//! only with another identity. It has no `PartialEq<Path>`, no `Deref` and no
//! `AsRef<Path>`, so comparing it with an unresolved path, or testing
//! containment against one in either direction, does not compile; its field
//! is private, so the only ways in are the constructors that resolve. It has
//! no `JsonSchema`, so a `--json` type cannot publish one in place of a
//! location's wire spelling.
//!
//! Pinned by diagnostic code from an external crate. Residue: privacy stops
//! at `path_spelling.rs`, where the tuple constructor is reachable, and
//! `id.as_path() == raw` compiles anywhere — that comparison is written out
//! in full, which is what makes it reviewable.

mod common;

use common::compile_probe::{
    assert_fails_with, assert_fails_with_dependencies, compile, compile_with,
};

#[test]
fn the_harness_can_compare_and_contain_identities() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::path_spelling::CanonicalPath;
        use std::path::Path;
        pub fn legal(a: &Path, b: &Path) -> bool {
            let (a, b) = (CanonicalPath::of(a), CanonicalPath::of(b));
            a == b || a.contains(&b) || a.same_directory(b.as_path())
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn an_identity_is_not_compared_with_a_borrowed_path() {
    assert_fails_with(
        "E0277",
        "&CanonicalPath == &PathBuf compares an identity with an unresolved spelling",
        r#"
        use repoweave::path_spelling::CanonicalPath;
        use std::path::PathBuf;
        pub fn same(id: &CanonicalPath, raw: &PathBuf) -> bool {
            id == raw
        }
        "#,
    );
}

#[test]
fn an_identity_is_not_compared_with_an_owned_path() {
    assert_fails_with(
        "E0308",
        "CanonicalPath == PathBuf compares an identity with an unresolved spelling",
        r#"
        use repoweave::path_spelling::CanonicalPath;
        use std::path::PathBuf;
        pub fn same(id: CanonicalPath, raw: PathBuf) -> bool {
            id == raw
        }
        "#,
    );
}

#[test]
fn a_path_cannot_test_containment_against_an_identity() {
    assert_fails_with(
        "E0277",
        "raw.starts_with(&id) would need CanonicalPath: AsRef<Path>",
        r#"
        use repoweave::path_spelling::CanonicalPath;
        use std::path::Path;
        pub fn under(raw: &Path, id: &CanonicalPath) -> bool {
            raw.starts_with(id)
        }
        "#,
    );
}

#[test]
fn an_identity_does_not_offer_path_prefix_tests() {
    assert_fails_with(
        "E0599",
        "id.starts_with(raw) would need Deref to Path; containment is `contains`",
        r#"
        use repoweave::path_spelling::CanonicalPath;
        use std::path::Path;
        pub fn under(id: &CanonicalPath, raw: &Path) -> bool {
            id.starts_with(raw)
        }
        "#,
    );
}

#[test]
fn an_identity_cannot_be_written_as_a_literal() {
    assert_fails_with(
        "E0423",
        "CanonicalPath(p) skips resolution",
        r#"
        use repoweave::path_spelling::CanonicalPath;
        use std::path::PathBuf;
        pub fn forge(p: PathBuf) -> CanonicalPath {
            CanonicalPath(p)
        }
        "#,
    );
}

#[test]
fn the_harness_can_derive_a_schema_for_a_json_type() {
    // Control for the probe below: the same derive over a type the library
    // publishes compiles, so the failure there is about `CanonicalPath`, not
    // about the derive or the schemars build the probe was handed.
    let (compiled, stderr) = compile_with(
        r#"
        #[derive(schemars::JsonSchema)]
        pub struct Out {
            pub resolution: repoweave::workspace::Resolution,
        }
        "#,
        &["schemars"],
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_json_type_cannot_hold_an_identity() {
    assert_fails_with_dependencies(
        "E0277",
        "a --json field publishes a location's wire spelling, not an identity",
        &["schemars"],
        r#"
        #[derive(schemars::JsonSchema)]
        pub struct Out {
            pub workspace: repoweave::path_spelling::CanonicalPath,
        }
        "#,
    );
}
