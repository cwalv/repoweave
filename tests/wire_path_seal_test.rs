//! A path reaches `--json` only through `WirePath::of`.
//!
//! The spelling a `--json` path field carries is the one the published
//! schemas state: absolute, `/`-separated on every platform. `WirePath`
//! withholds every other way to build one, so a wire field cannot be filled
//! with the host's own spelling of a path. Pinned by diagnostic code, from an
//! external crate, where every visibility narrower than `pub` looks alike.
//!
//! Residue: privacy stops at `path_spelling.rs`, where the tuple constructor
//! is reachable; and deserializing reads a `WirePath` back off the wire,
//! refusing only what the mint never writes.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_mint_a_wire_path() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::path_spelling::WirePath;
        use std::path::Path;
        pub fn mint(path: &Path) -> String {
            WirePath::of(path).as_str().to_owned()
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_wire_path_cannot_be_built_from_its_field() {
    assert_fails_with(
        "E0603",
        "WirePath's field is private to path_spelling",
        r#"
        pub fn mint(text: String) -> repoweave::path_spelling::WirePath {
            repoweave::path_spelling::WirePath(text)
        }
        "#,
    );
}

#[test]
fn an_imported_wire_path_cannot_be_built_from_its_field() {
    assert_fails_with(
        "E0423",
        "the same literal through an imported name",
        r#"
        use repoweave::path_spelling::WirePath;
        pub fn mint(text: String) -> WirePath {
            WirePath(text)
        }
        "#,
    );
}

#[test]
fn a_wire_path_cannot_be_converted_from_a_string() {
    assert_fails_with(
        "E0277",
        "WirePath has no From<String>",
        r#"
        use repoweave::path_spelling::WirePath;
        pub fn mint(text: String) -> WirePath {
            text.into()
        }
        "#,
    );
}

#[test]
fn a_wire_field_cannot_hold_the_host_spelling() {
    assert_fails_with(
        "E0308",
        "a --json path field takes a WirePath, not a displayed path",
        r#"
        use repoweave::placement::PlacementOutput;
        use std::path::Path;
        pub fn fill(record: &mut PlacementOutput, checkout: &Path) {
            record.absolute_path = checkout.display().to_string();
        }
        "#,
    );
}
