//! `rwv doctor`'s exit code, text and `--json` alike.
//!
//! Three pins, each an executed outcome:
//!
//!   1. Without `--kind`, a workspace whose only finding is warning-severity
//!      exits 0 — in both modes. `--json` is the regression: unfiltered, it
//!      used to fail on any violation, warnings included.
//!   2. Without `--kind`, a workspace whose only finding is error-severity
//!      exits non-zero — in both modes. The control: severity-gating must
//!      still catch an error after the warning-only fix.
//!   3. With `--kind`, a finding the filter admits exits non-zero regardless
//!      of severity — in both modes. Text's half is newly unified: before
//!      this fix, text mode ignored `--kind` for the exit code and stayed at
//!      the error-only rule even under a filter that matched a warning.
//!
//! The warning-only fixture reproduces the smallest case that showed the two
//! modes disagreeing: `git config --unset merge.rwv-ours.driver` on an
//! otherwise-conforming project repo, leaving `missing-merge-driver-config`
//! as the lone finding.

use std::path::{Path, PathBuf};

mod common;

fn make_workspace(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    root
}

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    common::git_in(path, &["add", "README.md"]);
    common::git_in(path, &["commit", "-m", "init"]);
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    std::fs::create_dir_all(project_dir).unwrap();
    let mut manifest = String::from("[repositories]\n");
    for (repo_path, url) in repos {
        manifest.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
}

/// Make `project_dir` the git repo a conforming project repo is: the
/// `rwv.lock merge=rwv-ours` replay exclusion committed, and the `rwv-ours`
/// merge driver defined in local git config. Call after [`write_manifest`]
/// so `rwv.toml` commits alongside `.gitattributes`.
fn make_project_repo_clean(project_dir: &Path) {
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    common::git_in(project_dir, &["init", "-b", "main"]);
    common::git_in(project_dir, &["add", "."]);
    common::git_in(project_dir, &["commit", "-m", "init"]);
    common::git_in(project_dir, &["config", "merge.rwv-ours.driver", "true"]);
}

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn parse(raw: &[u8]) -> serde_json::Value {
    serde_json::from_slice(raw).expect("doctor --json parses")
}

/// A workspace whose only finding is the warning-severity
/// `missing-merge-driver-config` violation.
fn warning_only_fixture(parent: &Path) -> PathBuf {
    let root = make_workspace(parent, "ws");
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_path, "https://github.com/acme/server.git")],
    );
    make_project_repo_clean(&project_dir);
    // The driver definition itself is the warning this fixture pins:
    // `.gitattributes` is correct, but local git config never got
    // `merge.rwv-ours.driver`.
    common::git_in(
        &project_dir,
        &["config", "--unset", "merge.rwv-ours.driver"],
    );
    root
}

/// A workspace whose only finding is the error-severity `dangling-reference`
/// violation: a manifest entry naming a repo never cloned to disk.
fn error_fixture(parent: &Path) -> PathBuf {
    let root = make_workspace(parent, "ws");
    let repo_path = "github/acme/server";
    init_git_repo(&root.join(repo_path));

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[
            (repo_path, "https://github.com/acme/server.git"),
            (
                "github/acme/vanished",
                "https://github.com/acme/vanished.git",
            ),
        ],
    );
    make_project_repo_clean(&project_dir);
    root
}

/// Pin 1: a lone warning exits 0, unfiltered, in both modes — the json
/// half is the regression this fix closes (it used to fail on any
/// violation, warnings included).
///
/// **Mutation evidence**: reverting `doctor_exit_signal`'s unfiltered arm
/// (`else { has_error }`, check.rs) to `else { true }` reddens this test;
/// `run_check` and `run_check_json` both call that one function with the
/// same arguments, so the mutation holds for either caller.
#[test]
fn warning_only_exits_zero_without_kind_in_both_modes() {
    let tmp = common::tempdir().unwrap();
    let root = warning_only_fixture(tmp.path());

    let text = rwv().arg("doctor").current_dir(&root).output().unwrap();
    assert!(
        text.status.success(),
        "text: a lone warning exits 0; got:\n{}",
        String::from_utf8_lossy(&text.stdout)
    );
    assert!(
        String::from_utf8_lossy(&text.stdout).contains("merge.rwv-ours.driver"),
        "control: the fixture's warning must actually be reported; got:\n{}",
        String::from_utf8_lossy(&text.stdout)
    );

    let json = rwv()
        .args(["doctor", "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    let doc = parse(&json.stdout);
    assert!(
        json.status.success(),
        "json: a lone warning exits 0; got: {doc}"
    );
    let violations = doc["violations"]
        .as_array()
        .expect("violations is an array");
    assert_eq!(
        violations.len(),
        1,
        "fixture carries exactly one finding; got: {doc}"
    );
    assert_eq!(violations[0]["kind"], "missing-merge-driver-config");
}

/// Pin 2: an error-severity finding exits non-zero, unfiltered, in both
/// modes — severity-gating still catches an error once the warning-only
/// case no longer does.
///
/// **Mutation evidence**: reverting `doctor_exit_signal`'s unfiltered arm
/// (`else { has_error }`, check.rs) to `else { false }` reddens both
/// halves — the same shared function
/// `warning_only_exits_zero_without_kind_in_both_modes` pins the other
/// direction of. Moving `severity()`'s `DanglingReference` arm to
/// `Warning` instead reddens only the json half here, because text's
/// severity for this violation still comes from
/// `itemized_violations_to_issues`'s own match, untouched by `severity()`;
/// `severity_agrees_with_the_rendered_issue_severity_for_the_whole_corpus`
/// below is the test that would catch the two sources drifting apart.
#[test]
fn error_finding_exits_nonzero_without_kind_in_both_modes() {
    let tmp = common::tempdir().unwrap();
    let root = error_fixture(tmp.path());

    let text = rwv().arg("doctor").current_dir(&root).output().unwrap();
    assert!(
        !text.status.success(),
        "text: an error-severity finding exits non-zero; got:\n{}",
        String::from_utf8_lossy(&text.stdout)
    );

    let json = rwv()
        .args(["doctor", "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    let doc = parse(&json.stdout);
    assert!(
        !json.status.success(),
        "json: an error-severity finding exits non-zero; got: {doc}"
    );
    let violations = doc["violations"]
        .as_array()
        .expect("violations is an array");
    assert_eq!(
        violations.len(),
        1,
        "fixture carries exactly one finding; got: {doc}"
    );
    assert_eq!(violations[0]["kind"], "dangling-reference");
}

/// Pin 3: under `--kind`, a finding the filter admits exits non-zero
/// regardless of severity — in both modes, reusing the warning-only
/// fixture. Text's half is the newly-unified behavior: before this fix,
/// `run_check` ignored `kind_filter` for its exit code entirely and
/// returned `Ok(has_errors)`, so the matched warning exited 0 under
/// `--kind` while json's own (pre-existing) `--kind` rule already failed
/// on it.
///
/// **Mutation evidence**: reverting `doctor_exit_signal`'s `kind_filtered`
/// arm (check.rs) from `finding_count > 0` to `has_error` reddens this
/// test — the matched warning no longer counts as a match on its own, in
/// either caller.
#[test]
fn kind_filtered_warning_exits_nonzero_in_both_modes() {
    let tmp = common::tempdir().unwrap();
    let root = warning_only_fixture(tmp.path());

    let text = rwv()
        .args(["doctor", "--kind", "missing-merge-driver-config"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        !text.status.success(),
        "text: `--kind` admits the warning — exit non-zero; got:\n{}",
        String::from_utf8_lossy(&text.stdout)
    );

    let json = rwv()
        .args(["doctor", "--json", "--kind", "missing-merge-driver-config"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        !json.status.success(),
        "json: `--kind` admits the warning — exit non-zero; got: {}",
        parse(&json.stdout)
    );
}

/// `CheckViolation::severity` — the one place `run_check_json`'s exit-code
/// computation reads a violation's severity — agrees with the severity text
/// mode renders through `violations_to_issues`, for every violation in the
/// corpus.
///
/// The two are computed independently: `violations_to_issues` match-builds
/// a message and a severity together per (kind, sub-kind);
/// `CheckViolation::severity` is a second, smaller match over the same
/// enum. Nothing but this test stops them drifting apart — unlike
/// `wire_kind`, there is no serialized field a schema-conformance test could
/// cross-check it against, since `--json` violations carry no severity of
/// their own.
///
/// **Mutation evidence**: moving `DanglingReference` from `severity`'s
/// `Error` group to its `Warning` group (check.rs) reddens this test. The
/// same mutation's user-visible effect — `--json`'s exit code going quiet on
/// an error while text still reports it — is what
/// `error_finding_exits_nonzero_without_kind_in_both_modes` above would miss
/// on its own, since that fixture carries only one variant.
#[test]
fn severity_agrees_with_the_rendered_issue_severity_for_the_whole_corpus() {
    use common::doctor_corpus::corpus;
    use repoweave::check::violations_to_issues;

    for v in corpus() {
        let declared = v.severity();
        let wire = v.wire_kind().to_string();
        let rendered = violations_to_issues(vec![v]);
        // The one corpus violation `itemized_violations_to_issues` drops
        // rather than rendering: a `ForeignPrimaryOtherWorkspace` marker
        // belongs to another workspace's scan, so text mode reports none of
        // it (see that function's own comment).
        if wire == "workweave-tree-integrity" && rendered.is_empty() {
            continue;
        }
        assert_eq!(
            rendered.len(),
            1,
            "`{wire}` must render to exactly one issue; got {rendered:?}"
        );
        assert_eq!(
            declared, rendered[0].severity,
            "`{wire}`: severity() says {declared:?}, rendering says {:?}",
            rendered[0].severity
        );
    }
}
