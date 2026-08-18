//! A surfacing declaration cannot be written without answering where its
//! write lands, pinned **by error code**.
//!
//! The rule these probes hold is not a style preference. An integration that
//! declares a lockfile as if it were an operator's committed file gets no link
//! created before its hook runs, so the ecosystem tool drops a real file at the
//! weave root that no repo tracks and no later pass can see. The wrong answer
//! is silent and costs data placement, which is why the type refuses the
//! question rather than defaulting it.
//!
//! `compile_fail` doctests would be the cheaper form and are not enough here:
//! on stable, rustdoc accepts an `Exxxx` annotation and ignores it, so such a
//! doctest passes when the snippet fails for an unrelated reason — or for a
//! typo. `common::compile_probe` compiles each snippet against the built
//! library and asserts the exact diagnostic.
//!
//! The first test is a control that must **succeed**: everything below asserts
//! a failure, so a broken invocation would make them all pass vacuously.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_compile_a_legal_declaration() {
    let (compiled, stderr) = compile(
        r#"
        use repoweave::integration::{SurfacedFile, SurfacedSource};
        pub fn legal() -> (String, SurfacedSource) {
            let f = SurfacedFile::written_through_link("Cargo.lock");
            (f.name().to_owned(), f.source())
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_bare_string_is_not_a_declaration() {
    // The shape every pre-provenance integration was written in. It has to
    // stop compiling rather than acquire a meaning, because "the name, with
    // the usual default" is exactly the guess this type exists to refuse.
    assert_fails_with(
        "E0308",
        "a path with no answer about where its write lands is not a declaration",
        r#"
        use repoweave::integration::SurfacedFile;
        pub fn declare() -> Vec<SurfacedFile> {
            vec!["Cargo.lock".to_string()]
        }
        "#,
    );
}

#[test]
fn there_is_no_conversion_from_a_bare_name() {
    // The escape hatch someone reaches for once the probe above stops them:
    // a `From<String>` would answer the question on the author's behalf at
    // every call site at once, and silently.
    //
    // E0308 rather than the "no such method" E0599: the reflexive blanket
    // `impl<T> From<T> for T` means `SurfacedFile::from` always resolves, so
    // what refuses here is the argument type. That also makes this probe
    // non-redundant with the one above rather than a second spelling of it —
    // adding `From<String>` greens this snippet while the `vec![String]` one
    // stays red, because a conversion that exists is still not applied inside
    // a `vec!` literal.
    assert_fails_with(
        "E0308",
        "no conversion may supply the answer a declaration has to state",
        r#"
        use repoweave::integration::SurfacedFile;
        pub fn declare() -> SurfacedFile {
            SurfacedFile::from("Cargo.lock".to_string())
        }
        "#,
    );
}

#[test]
fn the_fields_cannot_be_set_directly() {
    // The other way past the constructors. Keeping the fields private is what
    // makes the two named constructors the whole vocabulary, so a reader
    // grepping for either one finds every declaration in the tree.
    assert_fails_with(
        "E0451",
        "a declaration cannot be assembled field by field from outside",
        r#"
        use repoweave::integration::{SurfacedFile, SurfacedSource};
        pub fn declare() -> SurfacedFile {
            SurfacedFile {
                name: "Cargo.lock".to_string(),
                source: SurfacedSource::WrittenThroughLink,
            }
        }
        "#,
    );
}
