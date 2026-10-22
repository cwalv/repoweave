//! A path argument becomes a path only by being read from the directory rwv
//! runs in. An `Operand` is argv text with no route to a `Path` except
//! `locate`, which takes an `InvocationDir`, and both `locate` and the
//! `InvocationDir` mint are `pub(in crate::cli)`. Pinned by diagnostic code,
//! from an external crate, where every visibility narrower than `pub` looks
//! alike; the same codes answer a library module outside `crate::cli`.
//!
//! Residue: declaring a new path argument in `src/cli.rs` as `String` or
//! `PathBuf` instead of `Operand`.

mod common;

use common::compile_probe::{assert_fails_with, compile};

const PRELUDE: &str = r#"
    use repoweave::cli::operand::{InvocationDir, Operand};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
"#;

fn probe(body: &str) -> String {
    format!("{PRELUDE}\n{body}")
}

#[test]
fn the_harness_can_hold_an_operand_and_read_a_base() {
    // Control: argv text becomes an Operand, and a base is readable as a Path.
    let (compiled, stderr) = compile(&probe(
        r#"
        pub fn operand() -> Operand {
            Operand::from(OsString::from("sub/dir"))
        }
        pub fn base(dir: &InvocationDir) -> &Path {
            dir.as_path()
        }
        pub fn unused(_: PathBuf) {}
        "#,
    ));
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn an_operand_cannot_be_joined_onto_a_root() {
    assert_fails_with(
        "E0277",
        "an operand has no AsRef<Path>, so no join can take it",
        &probe(
            r#"
            pub fn join(root: &Path, operand: &Operand) -> PathBuf {
                root.join(operand)
            }
            "#,
        ),
    );
}

#[test]
fn an_operand_has_no_as_ref() {
    assert_fails_with(
        "E0599",
        "an operand is not viewable as a path",
        &probe(
            r#"
            pub fn view(operand: &Operand) -> &Path {
                operand.as_ref()
            }
            "#,
        ),
    );
}

#[test]
fn an_operand_does_not_convert_into_a_path() {
    assert_fails_with(
        "E0277",
        "no From<Operand> for PathBuf",
        &probe(
            r#"
            pub fn convert(operand: Operand) -> PathBuf {
                PathBuf::from(operand)
            }
            "#,
        ),
    );
}

#[test]
fn an_operand_has_no_display() {
    assert_fails_with(
        "E0277",
        "argv text is not rendered as if it named a location",
        &probe(
            r#"
            pub fn render(operand: &Operand) -> String {
                format!("{}", operand)
            }
            "#,
        ),
    );
}

#[test]
fn an_operand_is_not_located_outside_cli() {
    assert_fails_with(
        "E0624",
        "only crate::cli reads an operand from its base",
        &probe(
            r#"
            pub fn locate(operand: Operand, dir: &InvocationDir) -> PathBuf {
                operand.locate(dir)
            }
            "#,
        ),
    );
}

#[test]
fn an_invocation_dir_cannot_be_forged_by_tuple_literal() {
    assert_fails_with(
        "E0423",
        "the field is private to cli::operand",
        &probe(
            r#"
            pub fn forge(p: PathBuf) -> InvocationDir {
                InvocationDir(p)
            }
            "#,
        ),
    );
}

#[test]
fn an_invocation_dir_is_not_minted_outside_cli() {
    assert_fails_with(
        "E0624",
        "an invocation dir is minted only in crate::cli",
        &probe(
            r#"
            pub fn mint(p: &Path) -> std::io::Result<InvocationDir> {
                InvocationDir::resolved(p)
            }
            "#,
        ),
    );
}
