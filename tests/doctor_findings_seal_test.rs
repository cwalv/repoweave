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
//! Residue:
//!   - privacy stops at `check.rs`, where the fields and the text report
//!     renderer are reachable;
//!   - `DoctorJsonOutput`'s fields are public, so a hand-assembled envelope
//!     still compiles — `tests/common/doctor_corpus.rs` builds one from the
//!     corpus, which is how the schema tests sample every record.

mod common;

use common::compile_probe::{assert_fails_with, compile};

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
