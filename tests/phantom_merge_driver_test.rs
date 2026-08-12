//! `rwv doctor` reports a `.gitattributes` line that assigns an
//! `rwv-`-prefixed merge driver rwv does not define.
//!
//! Both directions are pinned here, because only one of them is a finding:
//!
//! - **Loud**: a phantom `merge=rwv-…` name is a violation. The line reads as
//!   a working derived-content declaration and resolves to nothing — git falls
//!   back to a textual merge without a word — and under the `rwv-` prefix
//!   nothing but rwv could ever define the name, so the silence is permanent.
//! - **Silent, deliberately**: a derived path carrying no attribute is NOT a
//!   finding. Which paths a repo declares derived is the repo's own business
//!   — declaration is per-repo, opt-in. The check is asymmetric on
//!   purpose; `no_finding_for_a_derived_path_that_declares_nothing` is the
//!   test that keeps it that way.

use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

const PROJECT: &str = "my-app";
const MEMBER: &str = "github/acme/server";

/// A workspace holding one project, one member repo, and nothing wrong with
/// it: `rwv doctor --json` reports **zero** violations until a test writes an
/// attribute worth reporting. That baseline is what lets the headline test
/// assert "exactly one violation" against the whole payload rather than
/// against a filtered view of it.
struct Weave {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Weave {
    fn new() -> Self {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(root.join("projects")).unwrap();
        std::fs::create_dir_all(root.join("github")).unwrap();

        let member = root.join(MEMBER);
        std::fs::create_dir_all(&member).unwrap();
        let git = |args: &[&str]| {
            let out = common::git()
                .args(args)
                .current_dir(&member)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@test.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@test.com")
                .output()
                .expect("git failed to start");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        std::fs::write(member.join("README.md"), "init\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "initial"]);

        let project = root.join("projects").join(PROJECT);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("rwv.toml"),
            format!(
                "[repositories.\"{MEMBER}\"]\ntype = \"git\"\nurl = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n"
            ),
        )
        .unwrap();
        // Both of the project repo's replay-exclusion preconditions. Without
        // the declaration it reports `missing-replay-exclusion`; without the
        // driver definition, `missing-merge-driver-config`. Either leaves the
        // baseline unclean.
        std::fs::write(project.join(".gitattributes"), "rwv.lock merge=rwv-ours\n").unwrap();
        let project_git = |args: &[&str]| {
            let out = common::git()
                .args(args)
                .current_dir(&project)
                .output()
                .expect("git failed to start");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        project_git(&["init", "-b", "main"]);
        project_git(&["config", "merge.rwv-ours.driver", "true"]);

        Self { _tmp: tmp, root }
    }

    fn member_repo(&self) -> PathBuf {
        self.root.join(MEMBER)
    }

    fn project_repo(&self) -> PathBuf {
        self.root.join("projects").join(PROJECT)
    }

    fn write_attributes(&self, repo: &Path, contents: &str) {
        std::fs::write(repo.join(".gitattributes"), contents).unwrap();
    }

    /// Every violation `rwv doctor --json` reports, in order.
    fn violations(&self) -> Vec<Value> {
        let out = common::rwv()
            .args(["doctor", "--json"])
            .current_dir(&self.root)
            .output()
            .expect("rwv failed to start");
        let stdout = String::from_utf8(out.stdout).expect("stdout was not utf-8");
        let parsed: Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
        parsed
            .get("violations")
            .and_then(|v| v.as_array())
            .unwrap_or_else(|| panic!("violations missing or not an array: {parsed}"))
            .clone()
    }

    fn phantoms(&self) -> Vec<Value> {
        self.violations()
            .into_iter()
            .filter(|v| v.get("kind").and_then(|k| k.as_str()) == Some("phantom-merge-driver"))
            .collect()
    }

    /// `rwv doctor`'s human-readable stdout.
    fn doctor_text(&self) -> String {
        let out = common::rwv()
            .arg("doctor")
            .current_dir(&self.root)
            .output()
            .expect("rwv failed to start");
        String::from_utf8(out.stdout).expect("stdout was not utf-8")
    }
}

fn field<'a>(violation: &'a Value, name: &str) -> &'a str {
    violation
        .get(name)
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("`{name}` missing from {violation}"))
}

// ---------------------------------------------------------------------------
// The acceptance: one phantom, one violation
// ---------------------------------------------------------------------------

#[test]
fn a_phantom_rwv_driver_is_exactly_one_violation() {
    let weave = Weave::new();
    weave.write_attributes(&weave.member_repo(), "docs/generated/** merge=rwv-bogus\n");

    let violations = weave.violations();
    assert_eq!(
        violations.len(),
        1,
        "the fixture is otherwise clean, so the phantom must be the only \
         thing doctor reports: {violations:?}"
    );
    let v = &violations[0];
    assert_eq!(field(v, "kind"), "phantom-merge-driver");
    assert_eq!(field(v, "path"), MEMBER);
    assert_eq!(field(v, "pattern"), "docs/generated/**");
    assert_eq!(field(v, "driver"), "rwv-bogus");
    assert_eq!(
        field(v, "absolute_path"),
        weave.member_repo().to_string_lossy()
    );
}

#[test]
fn a_phantom_in_the_project_repo_is_reported() {
    let weave = Weave::new();
    weave.write_attributes(
        &weave.project_repo(),
        "rwv.lock merge=rwv-ours\ndocs/** merge=rwv-typo\n",
    );

    let phantoms = weave.phantoms();
    assert_eq!(phantoms.len(), 1, "{phantoms:?}");
    assert_eq!(field(&phantoms[0], "path"), format!("projects/{PROJECT}"));
    assert_eq!(field(&phantoms[0], "driver"), "rwv-typo");
}

// ---------------------------------------------------------------------------
// The over-eager direction: correctly-configured repos stay silent
// ---------------------------------------------------------------------------

#[test]
fn a_defined_rwv_driver_is_not_a_violation() {
    // The declaration D4 exists to make safe. A check that flagged this
    // would fail `rwv doctor` on every repo that adopted the primitive.
    let weave = Weave::new();
    weave.write_attributes(
        &weave.member_repo(),
        "docs/reference/explain/** merge=rwv-ours\n",
    );

    assert_eq!(weave.violations(), Vec::<Value>::new());
}

#[test]
fn driver_names_outside_the_rwv_namespace_are_not_doctors_business() {
    // `union` is git's own; `ours` is the plain name a third-party repo may
    // define for itself (and the legacy spelling rwv migrated away from
    // precisely because it collides with such repos). Neither is rwv's to
    // have an opinion about, and `-merge` unsets rather than assigns.
    let weave = Weave::new();
    weave.write_attributes(
        &weave.member_repo(),
        "docs/x/** merge=ours\nother/** merge=union\nplain/** -merge\n",
    );

    assert_eq!(weave.phantoms(), Vec::<Value>::new());
}

#[test]
fn no_finding_for_a_derived_path_that_declares_nothing() {
    // The silent inverse, deliberately unenforced: generated content with no
    // `.gitattributes` at all. Declaration is per-repo opt-in (D1), so
    // "you have derived content and did not declare it" is not a finding —
    // making this check symmetric would be a design violation, not an
    // improvement.
    let weave = Weave::new();
    let generated = weave.member_repo().join("docs/reference/explain");
    std::fs::create_dir_all(&generated).unwrap();
    std::fs::write(generated.join("doctor.md"), "generated\n").unwrap();

    assert_eq!(weave.violations(), Vec::<Value>::new());
}

// ---------------------------------------------------------------------------
// Reading `.gitattributes` the way git reads it
// ---------------------------------------------------------------------------

#[test]
fn comments_and_macro_definitions_are_not_assignments() {
    let weave = Weave::new();
    weave.write_attributes(
        &weave.member_repo(),
        "# docs/x/** merge=rwv-commented\n[attr]binary -diff -merge\n\n",
    );

    assert_eq!(weave.phantoms(), Vec::<Value>::new());
}

#[test]
fn only_the_effective_driver_on_a_line_is_judged() {
    // Git resolves repeated assignments of one attribute on a single line
    // last-wins, so the last `merge=` token is what the line means.
    let weave = Weave::new();

    weave.write_attributes(
        &weave.member_repo(),
        "docs/x/** merge=rwv-first merge=rwv-ours\n",
    );
    assert_eq!(
        weave.phantoms(),
        Vec::<Value>::new(),
        "an overridden phantom is not what the line means"
    );

    weave.write_attributes(
        &weave.member_repo(),
        "docs/x/** merge=rwv-ours merge=rwv-second\n",
    );
    let phantoms = weave.phantoms();
    assert_eq!(phantoms.len(), 1, "{phantoms:?}");
    assert_eq!(field(&phantoms[0], "driver"), "rwv-second");
}

#[test]
fn a_quoted_pattern_is_reported_whole() {
    // gitattributes(5) allows a double-quoted pattern so it can contain
    // spaces. Splitting on whitespace would report `"a` as the pattern and
    // send the operator looking for a line that isn't there.
    let weave = Weave::new();
    weave.write_attributes(&weave.member_repo(), "\"a path/**\" merge=rwv-bogus\n");

    let phantoms = weave.phantoms();
    assert_eq!(phantoms.len(), 1, "{phantoms:?}");
    assert_eq!(field(&phantoms[0], "pattern"), "a path/**");
}

// ---------------------------------------------------------------------------
// The text channel
// ---------------------------------------------------------------------------

#[test]
fn the_text_channel_names_the_driver_the_pattern_and_the_remedy() {
    let weave = Weave::new();
    weave.write_attributes(&weave.member_repo(), "docs/generated/** merge=rwv-bogus\n");

    let stdout = weave.doctor_text();
    let line = stdout
        .lines()
        .find(|l| l.contains("rwv-bogus"))
        .unwrap_or_else(|| panic!("no line names the phantom driver:\n{stdout}"));
    assert!(
        line.contains("docs/generated/**"),
        "the operator has to be told which line to look at: {line}"
    );
    assert!(
        line.contains("merge=rwv-ours"),
        "the remedy names the driver rwv does define: {line}"
    );
    assert!(
        line.starts_with("[warning]"),
        "report-only: a phantom degrades to a textual merge, which the \
         repo's own gates still catch — it does not fail doctor: {line}"
    );
}
