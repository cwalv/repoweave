//! `rwv doctor`'s report renderers take only `DoctorFindings`, which only the
//! findings pipeline builds.
//!
//! The regression this holds: the `--json` report once came from a collection
//! of its own that never called a scan the text report did, so `--json` was
//! silent where text listed findings. A renderer that takes a `Vec` can be
//! handed any vector; one that takes `DoctorFindings` can be handed only what
//! the pipeline found, in the scope and through the filter the run was given.
//! Pinned by diagnostic code, from an external crate, where every visibility
//! narrower than `pub` looks alike.
//!
//! `DoctorJsonOutput`'s fields stay public, and that is the test seam rather
//! than a gap: like every other verb's envelope it is a plain wire type, and
//! `tests/common/doctor_corpus.rs` builds one literal from the corpus so the
//! schema tests sample every record through the real type, with a new envelope
//! field a compile error there. What the pipeline's guarantee needs is that
//! production assembles the envelope in one place, from the findings. Field
//! privacy cannot say that: the type and its one literal sit in `check.rs`,
//! where private fields are reachable from every function. So a source pin
//! says it instead, as for the integration context types.
//!
//! Residue:
//!   - privacy stops at `check.rs`, where the fields and the text report
//!     renderer are reachable;
//!   - the envelope pin reads production source for the literal, so a literal
//!     built through a type alias is invisible to it.

mod common;

use common::compile_probe::{assert_fails_with, compile};
use common::src_scan::{body_of, production_lines, struct_literal_needle};
use repoweave::check::DoctorJsonOutput;

#[test]
fn the_harness_can_render_pipeline_findings() {
    // Control. Everything below asserts a failure, so a broken rustc
    // invocation would make them all pass for the wrong reason.
    let (compiled, stderr) = compile(
        r#"
        use repoweave::check::{build_doctor_json, DoctorFindings, DoctorJsonOutput};
        use std::path::Path;
        pub fn render(findings: DoctorFindings, ws: &Path) -> DoctorJsonOutput {
            build_doctor_json(findings, ws, None, Vec::new(), Vec::new())
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn the_json_renderer_refuses_a_hand_built_vec() {
    assert_fails_with(
        "E0308",
        "build_doctor_json takes DoctorFindings, not a Vec of violations",
        r#"
        use repoweave::check::{build_doctor_json, CheckViolation, DoctorJsonOutput};
        use std::path::Path;
        pub fn render(violations: Vec<CheckViolation>, ws: &Path) -> DoctorJsonOutput {
            build_doctor_json(violations, ws, None, Vec::new(), Vec::new())
        }
        "#,
    );
}

#[test]
fn findings_cannot_be_assembled_outside_the_pipeline() {
    assert_fails_with(
        "E0451",
        "DoctorFindings' fields are private to check.rs",
        r#"
        use repoweave::check::{CheckViolation, DoctorFindings};
        pub fn assemble(violations: Vec<CheckViolation>) -> DoctorFindings {
            DoctorFindings {
                violations,
                issues: Vec::new(),
                filtered: false,
                workweave_dirs: Default::default(),
                repo_locations: Default::default(),
            }
        }
        "#,
    );
}

#[test]
fn there_are_no_empty_findings_to_stand_in_for_a_run() {
    assert_fails_with(
        "E0599",
        "an empty DoctorFindings is a silent report, so there is no constructor for one",
        r#"
        use repoweave::check::DoctorFindings;
        pub fn nothing() -> DoctorFindings {
            DoctorFindings::default()
        }
        "#,
    );
}

#[test]
fn the_json_envelope_is_assembled_in_one_place_and_it_is_the_renderer() {
    let needle = struct_literal_needle::<DoctorJsonOutput>();
    let lines = production_lines();

    let sites: Vec<_> = lines
        .iter()
        .filter(|l| l.text.contains(&needle))
        .filter(|l| !l.text.contains("struct ") && !l.text.contains("->"))
        .collect();
    assert!(
        !sites.is_empty(),
        "no `{needle}` literal found — the needle no longer matches the source shape, so \
         emptiness would prove nothing"
    );

    let renderer = body_of(&lines, "check.rs", "build_doctor_json");
    assert!(
        !renderer.is_empty(),
        "build_doctor_json was not found in check.rs — this scan is pointed at the wrong corpus"
    );
    let outside: Vec<String> = sites
        .iter()
        .filter(|site| {
            !renderer
                .iter()
                .any(|l| l.file == site.file && l.line == site.line)
        })
        .map(|site| site.site())
        .collect();
    assert!(
        outside.is_empty(),
        "`{needle}` is the `--json` report's wire form and is built from the findings in \
         `build_doctor_json` only. Found outside it: {outside:?}"
    );
    assert_eq!(
        sites.len(),
        1,
        "exactly one literal is expected in build_doctor_json: {:?}",
        sites.iter().map(|s| s.site()).collect::<Vec<_>>()
    );
}
