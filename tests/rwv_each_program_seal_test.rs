//! `Program` in `rwv-each` has one text form, its spelling; only `command_in`
//! reads where the program was found. What this pins is code that does not
//! compile, which no run of the binary can show, so the pins are compile-fail
//! probes.
//!
//! Each probe includes `src/bin/rwv-each/program.rs` by `#[path]`, so the
//! snippet stands where a sibling module of `program` stands inside the
//! plugin, as `run.rs` does. That is what lets a probe see a field or enum
//! widened to `pub(crate)`: from outside the crate, `pub(crate)` and private
//! refuse alike. The module has to stay std-only for this, because the harness
//! passes no `--extern` for the plugin's dependencies.
//!
//! Not pinned: an accessor under a name no probe tries, and the location read
//! back through `Command::get_program`, which compiles and is pinned below as
//! a known limit.

mod common;

use std::path::{Path, PathBuf};

use common::compile_probe::{assert_fails_with, compile};

const READS_THE_LOCATION_FIELD: &str = r#"
    pub fn peek(p: &program::Program) {
        let _ = &p.located;
    }
"#;

const NAMES_A_LOCATION_VARIANT: &str = r#"
    pub fn forge(found: std::path::PathBuf) {
        let _ = program::Located::OnPath(found);
    }
"#;

fn program_rs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/bin/rwv-each/program.rs")
}

fn including(module: &Path, body: &str) -> String {
    format!("#[path = {module:?}]\nmod program;\n{body}")
}

#[test]
fn the_harness_can_compile_a_legal_use_of_the_module() {
    let (compiled, stderr) = compile(&including(
        &program_rs(),
        r#"
        pub fn legal(dir: &std::path::Path) -> std::io::Result<(String, std::process::Command)> {
            let p = program::Program::locate("git".into(), |_| {
                Ok::<_, std::io::Error>(std::path::PathBuf::from("/usr/bin/git"))
            })?;
            Ok((p.spelled().to_owned(), p.command_in(dir)))
        }
        "#,
    ));
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn where_a_program_was_found_cannot_be_read_off_it() {
    assert_fails_with(
        "E0616",
        "`located` is private to program.rs; only `command_in` reads it",
        &including(&program_rs(), READS_THE_LOCATION_FIELD),
    );
}

#[test]
fn a_program_cannot_be_assembled_field_by_field() {
    assert_fails_with(
        "E0451",
        "`locate` is the only way to build a Program",
        &including(
            &program_rs(),
            r#"
            pub fn forge() -> program::Program {
                program::Program { spelled: String::new(), located: todo!() }
            }
            "#,
        ),
    );
}

#[test]
fn a_location_cannot_be_named_outside_the_module() {
    assert_fails_with(
        "E0603",
        "`Located` is private to program.rs",
        &including(&program_rs(), NAMES_A_LOCATION_VARIANT),
    );
}

#[test]
fn a_program_has_no_debug_form_to_print_its_location_through() {
    assert_fails_with(
        "E0277",
        "a derived Debug would print where the program was found",
        &including(
            &program_rs(),
            r#"
            pub fn show(p: &program::Program) -> String {
                format!("{p:?}")
            }
            "#,
        ),
    );
}

#[test]
fn the_probes_see_a_location_widened_to_pub_crate() {
    let source = std::fs::read_to_string(program_rs()).unwrap();
    let mut widened = source.clone();
    for (private, crate_visible) in [
        ("    located: Located,", "    pub(crate) located: Located,"),
        ("\nenum Located {", "\npub(crate) enum Located {"),
    ] {
        assert_eq!(
            widened.matches(private).count(),
            1,
            "program.rs no longer spells `{private}` once"
        );
        widened = widened.replacen(private, crate_visible, 1);
    }
    let tmp = common::tempdir().unwrap();
    let copy = tmp.path().join("program.rs");
    std::fs::write(&copy, widened).unwrap();

    for (what, body) in [
        ("field read", READS_THE_LOCATION_FIELD),
        ("variant", NAMES_A_LOCATION_VARIANT),
    ] {
        let (compiled, stderr) = compile(&including(&copy, body));
        assert!(
            compiled,
            "the {what} probe must compile against a pub(crate) copy, or it cannot \
             tell that widening from private:\n{stderr}"
        );
    }
}

#[test]
fn the_location_still_reads_back_through_std_command() {
    let (compiled, stderr) = compile(&including(
        &program_rs(),
        r#"
        pub fn read_back(p: &program::Program, dir: &std::path::Path) -> std::ffi::OsString {
            p.command_in(dir).get_program().to_owned()
        }
        "#,
    ));
    assert!(
        compiled,
        "known limit no longer holds; update this file's scope:\n{stderr}"
    );
}
