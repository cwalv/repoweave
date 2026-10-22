//! A doctor finding on the wire names its workweave with the `WorkweaveId`
//! itself, so `WorkweaveId`'s `Display` is the one place its
//! `<project>--<name>` spelling is produced. A wire field typed `String`
//! invited each conversion to spell it again.
//!
//! Pinned by error code with `common::compile_probe`; the first test is the
//! control that must compile.

mod common;

use common::compile_probe::{assert_fails_with, compile};

fn missing_canonical_clone(workweave_expr: &str) -> String {
    format!(
        r#"
        use repoweave::check::ViolationOutput;
        use repoweave::manifest::{{ProjectName, WorkweaveId, WorkweaveName}};
        pub fn finding(project: ProjectName, name: WorkweaveName) -> ViolationOutput {{
            let _ = (&project, &name);
            ViolationOutput::MissingCanonicalClone {{
                path: String::new(),
                absolute_path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/x")),
                workweave: {workweave_expr},
                canonical_path: repoweave::path_spelling::WirePath::of(std::path::Path::new("/x")),
            }}
        }}
        "#
    )
}

#[test]
fn a_finding_carries_its_workweave_s_identity() {
    let (compiled, stderr) = compile(&missing_canonical_clone("WorkweaveId { project, name }"));
    assert!(compiled, "control snippet must compile; got:\n{stderr}");
}

#[test]
fn a_finding_does_not_take_a_spelled_workweave() {
    assert_fails_with(
        "E0308",
        "the wire field holds the identity, not its spelling",
        &missing_canonical_clone("format!(\"{project}--{name}\")"),
    );
}
