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

/// How many findings `rwv doctor --json` emits for `root`, under `scope`.
///
/// Both scopes are exercised because the project went missing for two
/// independent reasons, and each scope reaches only one of them. Under the
/// default the enumerated name is compared against the active project and a
/// mismatch skips it; under `--all` there is no such comparison and the run
/// instead builds a directory from the enumerated name, which for a misderived
/// name does not exist. A pin on one scope leaves the other half unmeasured.
fn doctor_finding_count(root: &Path, scope: &[&str]) -> usize {
    let mut args = vec!["doctor", "--json"];
    args.extend_from_slice(scope);
    let out = common::rwv()
        .args(&args)
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

    let control_findings = doctor_finding_count(&control, &[]);
    let subject_findings = doctor_finding_count(&subject, &[]);

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

#[test]
fn an_interior_projects_segment_does_not_hide_a_project_under_all_scope() {
    let tmp = common::tempdir().unwrap();

    let control = weave_holding(tmp.path(), "control", "a/b/c");
    let subject = weave_holding(tmp.path(), "subject", "a/projects/b");

    let control_findings = doctor_finding_count(&control, &["--all"]);
    let subject_findings = doctor_finding_count(&subject, &["--all"]);

    assert!(
        control_findings > 0,
        "the control weave reported nothing under --all, so this comparison \
         proves nothing"
    );
    assert_eq!(
        subject_findings, control_findings,
        "under --all the active-project comparison does not run, so this arm \
         reaches the other reason the project used to vanish: the run builds a \
         directory from the enumerated name and loads it. A name derived wrong \
         names a directory that is not there, the load fails, and the project \
         is skipped as silently as the default scope skipped it"
    );
}

/// A directory the walk *can* name but the validators refuse is still
/// reported — the class did not become invisible when the derivation stopped
/// being able to fail.
///
/// Naming a project by the path walked to it removed the one route by which
/// deriving a name could produce nothing at all. What remains is narrower and
/// entirely a validator question: the walked name is a legal path and not a
/// legal `ProjectName`. `a--b` is refused because `--` is what joins a project
/// to a workweave, so a directory spelled that way is exactly the shape the
/// scan must still surface rather than drop.
#[test]
fn a_directory_the_validators_refuse_is_reported_not_dropped() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("weave");
    std::fs::create_dir_all(root.join("projects").join("a--b")).unwrap();
    std::fs::write(
        root.join("projects").join("a--b").join("rwv.toml"),
        MANIFEST_WITH_ABSENT_CLONE.trim_start(),
    )
    .unwrap();

    let out = common::rwv()
        .args(["doctor", "--json", "--all"])
        .current_dir(&root)
        .output()
        .expect("doctor runs");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("unnameable-project"),
        "a manifest-carrying directory under projects/ that cannot be named \
         must be reported, not skipped: it is on disk and no verb can address \
         it, which is precisely what the operator needs told. Got:\n{stdout}"
    );
    assert!(
        stdout.contains("a--b"),
        "the finding must carry the derived name so the operator can find the \
         directory it is about. Got:\n{stdout}"
    );
}
