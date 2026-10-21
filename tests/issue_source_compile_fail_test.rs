//! A finding names where it came from by a type, not by a string that happens
//! to say `core`, and a skipped hook names its integration and its tool as
//! different kinds of thing.
//!
//! The regression it closes: `Issue.integration` was a `String` in which rwv's
//! own findings wrote the sentinel `"core"` at nine sites and an integration's
//! wrote its name, so an integration named `core` was indistinguishable from
//! rwv. `SkippedEffect::tool_missing(integration: &str, tool: &str)` took the
//! integration's name and the tool's side by side, and its one test passed
//! `"pnpm"` for both, so it could not catch a swap.
//!
//! Pinned by error code with `common::compile_probe`: a bare `compile_fail`
//! snippet passes for any error, including a typo. The first test is a control
//! that must compile, without which a broken rustc invocation would make every
//! other assertion pass vacuously.

mod common;

use common::compile_probe::{assert_fails_with, compile};

#[test]
fn the_harness_can_compile_a_finding_and_a_skip_that_name_their_sources() {
    let (compiled, stderr) = compile(
        r#"
        use repoweave::integration::{Issue, IssueKind, IssueSource, MissingTool, Severity};
        use repoweave::integrations::NpmWorkspaces;
        use repoweave::skipped::SkippedEffect;

        pub fn legal() -> (Issue, Issue, SkippedEffect) {
            let core = Issue {
                source: IssueSource::Core,
                severity: Severity::Warning,
                message: String::new(),
                kind: IssueKind::CoreFinding,
                safe_to_fix: false,
            };
            let integration = Issue {
                source: IssueSource::integration("npm-workspaces"),
                severity: Severity::Warning,
                message: String::new(),
                kind: IssueKind::ToolMissing,
                safe_to_fix: false,
            };
            (core, integration, SkippedEffect::tool_missing(&NpmWorkspaces, MissingTool::of::<NpmWorkspaces>()))
        }
        "#,
    );
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_core_sentinel_string_is_not_a_source() {
    assert_fails_with(
        "E0308",
        "a finding's source is named by the enum, never spelled as a string",
        r#"
        use repoweave::integration::{Issue, IssueKind, Severity};

        pub fn sentinel() -> Issue {
            Issue {
                source: "core".to_string(),
                severity: Severity::Warning,
                message: String::new(),
                kind: IssueKind::CoreFinding,
                safe_to_fix: false,
            }
        }
        "#,
    );
}

#[test]
fn an_issue_has_no_integration_string_field() {
    assert_fails_with(
        "E0560",
        "the integration name rides IssueSource::Integration, not a field of its own",
        r#"
        use repoweave::integration::{Issue, IssueKind, Severity};

        pub fn old_shape() -> Issue {
            Issue {
                integration: "core".to_string(),
                severity: Severity::Warning,
                message: String::new(),
                kind: IssueKind::CoreFinding,
                safe_to_fix: false,
            }
        }
        "#,
    );
}

#[test]
fn the_two_string_tool_missing_call_is_gone() {
    // E0277, not E0308: a `&str` is offered the unsizing coercion to
    // `&dyn Integration` before it is refused, so rustc reports the missing
    // trait bound. The refusal is the same one either way, and pinning the
    // code is how that stays true.
    assert_fails_with(
        "E0277",
        "a skipped hook is built from the integration, not from its name beside its tool",
        r#"
        use repoweave::skipped::SkippedEffect;

        pub fn old_shape() -> SkippedEffect {
            SkippedEffect::tool_missing("npm-workspaces", "npm")
        }
        "#,
    );
}

#[test]
fn a_tool_cannot_stand_where_the_integration_goes() {
    // One error per side, each of its own code: the tool is not an
    // integration, and the integration is not a tool.
    let swapped = r#"
        use repoweave::integrations::NpmWorkspaces;
        use repoweave::skipped::SkippedEffect;

        pub fn swapped() -> SkippedEffect {
            SkippedEffect::tool_missing("npm", &NpmWorkspaces)
        }
        "#;
    assert_fails_with("E0277", "a tool name is not an integration", swapped);
    assert_fails_with("E0308", "an integration is not a tool name", swapped);
}
