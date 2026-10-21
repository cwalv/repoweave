//! A tool is declared by the integration that shells out to it, a hook can
//! report only the tool its integration declared, and an incompatibility is
//! recorded with its four strings named.
//!
//! The regressions it closes: `HookOutcome::ToolMissing(&'static str)` let a
//! hook name any string, including a tool its integration never ran or no tool
//! at all, and six `check()` probes spelled the tool again beside it. And
//! `MemberIncompatibility::new` took the integration, the key, the value on
//! disk, the value required and the requiring file as five adjacent `&str`, so
//! a transposed pair compiled and published a finding that said the opposite
//! of what was observed.
//!
//! Pinned by error code with `common::compile_probe`: a bare `compile_fail`
//! snippet passes for any error, including a typo. The first test is a control
//! that must compile, without which a broken rustc invocation would make every
//! other assertion pass vacuously.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_compile_a_declared_tool_and_a_named_observation() {
    let (compiled, stderr) = compile(
        r#"
        use repoweave::integration::{
            DrivesTool, HookOutcome, MemberIncompatibility, MissingTool, Observed,
        };
        use repoweave::integrations::{GoWork, NpmWorkspaces};
        use std::path::Path;

        pub fn legal() -> (HookOutcome, &'static str, MemberIncompatibility) {
            (
                HookOutcome::ToolMissing(MissingTool::of::<NpmWorkspaces>()),
                NpmWorkspaces::TOOL.name(),
                MemberIncompatibility::new(
                    &GoWork,
                    Path::new("/w/go.work"),
                    Observed {
                        key: "go",
                        on_disk: "1.21",
                        required: "1.26",
                        required_by: "github/org/a/go.mod",
                    },
                ),
            )
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn an_integration_that_drives_no_tool_has_none_to_report_missing() {
    assert_fails_with(
        "E0277",
        "a tool-less integration does not implement DrivesTool, so it cannot name one",
        r#"
        use repoweave::integration::MissingTool;
        use repoweave::integrations::VscodeWorkspace;

        pub fn none() -> MissingTool {
            MissingTool::of::<VscodeWorkspace>()
        }
        "#,
    );
}

#[test]
fn a_hook_cannot_report_a_tool_by_its_spelling() {
    assert_fails_with(
        "E0308",
        "the outcome carries the declared tool, not a string a hook typed",
        r#"
        use repoweave::integration::HookOutcome;

        pub fn spelled() -> HookOutcome {
            HookOutcome::ToolMissing("npm")
        }
        "#,
    );
}

#[test]
fn a_tool_is_declared_inside_the_crate_only() {
    assert_fails_with(
        "E0624",
        "a missing tool is read off a declaration, so no caller can mint one",
        r#"
        use repoweave::integration::Tool;

        pub fn minted() -> Tool {
            Tool::new("npm")
        }
        "#,
    );
}

#[test]
fn the_five_string_incompatibility_call_is_gone() {
    assert_fails_with(
        "E0061",
        "the integration, the key and the three values are no longer adjacent strings",
        r#"
        use repoweave::integration::MemberIncompatibility;
        use std::path::Path;

        pub fn old_shape() -> MemberIncompatibility {
            MemberIncompatibility::new(
                "go-work",
                Path::new("/w/go.work"),
                "go",
                "1.21",
                "1.26",
                "github/org/a/go.mod",
            )
        }
        "#,
    );
}

#[test]
fn an_observation_is_named_not_positional() {
    assert_fails_with(
        "E0308",
        "the four strings ride Observed, so one cannot be passed in another's place",
        r#"
        use repoweave::integration::MemberIncompatibility;
        use repoweave::integrations::GoWork;
        use std::path::Path;

        pub fn positional() -> MemberIncompatibility {
            MemberIncompatibility::new(&GoWork, Path::new("/w/go.work"), "go")
        }
        "#,
    );
}

#[test]
fn an_observation_with_a_misnamed_field_does_not_compile() {
    assert_fails_with(
        "E0560",
        "a transposed pair has to be spelled out by name, where a reader sees it",
        r#"
        use repoweave::integration::Observed;

        pub fn misnamed() -> Observed<'static> {
            Observed {
                key: "go",
                on_disk: "1.21",
                required: "1.26",
                the_file_requiring_it: "github/org/a/go.mod",
            }
        }
        "#,
    );
}
