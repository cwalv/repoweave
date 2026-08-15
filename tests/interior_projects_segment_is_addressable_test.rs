//! A project name may carry a `/`, and nothing bars the segment `projects`
//! from appearing inside one. `a/projects/b` is a legal name: the validators
//! refuse `--`, `+`, and non-ref shapes, and none of those is in it.
//!
//! Such a project was invisible to `rwv doctor`. Not misreported — **absent**.
//! The discovery walk read the name back out of the directory path by taking
//! everything after the *last* `projects` component, so `a/projects/b` came
//! back as `b`; nothing classified that as unnameable, and every consumer then
//! addressed a `projects/b` that does not exist. Doctor's job is to notice
//! broken state, and for this class of name it reported clean over it.
//!
//! The pin is a **differential**, not a bare count. A count alone passes when
//! doctor stops reporting anything at all, which is the failure this class of
//! bug already produced once. Two weaves are built identically and broken
//! identically, differing only in the project's name; the control is nested and
//! multi-segment (`a/b/c`) so that a difference in the result is attributable
//! to the `projects` segment rather than to nesting. The control must report,
//! and the subject must report the same.

mod common;

use std::path::{Path, PathBuf};

/// A manifest naming one repo that is not on disk — the breakage both weaves
/// carry, and something doctor has an opinion about.
const MANIFEST_WITH_ABSENT_CLONE: &str = r#"
[repositories."github/acme/server"]
type = "git"
url = "https://github.com/acme/server.git"
version = "main"
role = "owned"
"#;

/// A weave holding exactly one project, named `name`, carrying the breakage.
fn weave_holding(parent: &Path, dir_name: &str, name: &str) -> PathBuf {
    let root = parent.join(dir_name);
    std::fs::create_dir_all(root.join("projects")).unwrap();

    common::rwv()
        .args(["init", name])
        .current_dir(&root)
        .assert()
        .success();

    let project_dir = root.join("projects").join(name);
    std::fs::write(
        project_dir.join("rwv.toml"),
        MANIFEST_WITH_ABSENT_CLONE.trim_start(),
    )
    .unwrap();
    root
}

/// How many findings `rwv doctor --json` emits for `root`.
fn doctor_finding_count(root: &Path) -> usize {
    let out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(root)
        .output()
        .expect("doctor runs");
    String::from_utf8_lossy(&out.stdout)
        .matches("\"kind\"")
        .count()
}

#[test]
fn an_interior_projects_segment_does_not_hide_a_project_from_doctor() {
    let tmp = common::tempdir().unwrap();

    let control = weave_holding(tmp.path(), "control", "a/b/c");
    let subject = weave_holding(tmp.path(), "subject", "a/projects/b");

    let control_findings = doctor_finding_count(&control);
    let subject_findings = doctor_finding_count(&subject);

    assert!(
        control_findings > 0,
        "the control weave reported nothing, so this comparison proves \
         nothing — the seeded breakage stopped being something doctor \
         reports, and the assertion below would hold for both weaves \
         whatever the naming code did"
    );
    assert_eq!(
        subject_findings, control_findings,
        "a project whose name contains an interior `projects` segment must be \
         as visible to doctor as any other nested project. The control \
         (`a/b/c`) and the subject (`a/projects/b`) differ only in that \
         segment and carry the same breakage, so a difference here is the \
         naming code losing the project: the walk derives a name the project \
         does not have, nothing reports the directory as unnameable, and \
         doctor goes on to report clean over broken state"
    );
}
