//! What `rwv doctor` says about a project's replay exclusion and what
//! `rwv doctor --fix` does to it are the same decision, driven through the
//! real binary in both directions.
//!
//! The regression: a project whose `.gitattributes` carried the current
//! `rwv.lock merge=rwv-ours` line *and* the legacy `rwv.lock merge=ours` line
//! reported no finding at all — the detector matched the current line and
//! stopped — while the repair, which skipped only on "current and no legacy",
//! rewrote the file and committed the rewrite into the operator's project
//! repo. It is the one auto-fix that authors a commit in a repo the operator
//! owns, and the report they would read first was empty.
//!
//! Asserting only that the both-lines state now reports would pass just as
//! well against a detector stuck on. So every state the file can be in is
//! driven through `doctor --json` and then `doctor --fix` in the same
//! workspace, and the finding and the repair are asserted against each other
//! per state — a repair with no finding and a finding with no repair are both
//! reported here.
//!
//! **Residue** — the report and the repair are two runs of the binary, so
//! this cannot and does not pin that they read the file at one instant: an
//! edit between the two runs changes the answer, exactly as it does for an
//! operator who edits between their own two commands. What it pins is that
//! one classification of the file decides both.
//!
//! Nothing here reads sync's precondition, which asks its own question of the
//! *committed* file and still treats the both-lines state as satisfied.

use std::path::{Path, PathBuf};

mod common;

/// One `.gitattributes` state, the finding it must raise, and whether the
/// repair for it commits.
struct Case {
    attributes: &'static str,
    /// The `sub_kind` `doctor --json` must report, `None` for a state that
    /// raises no finding and must therefore see no repair.
    finding: Option<&'static str>,
    /// Whether `--fix` authors a commit in the project repo. The fresh write
    /// lands in the working tree only; a migration has to reach the committed
    /// tree, which is the form sync reads.
    commits: bool,
}

const CASES: &[(&str, Case)] = &[
    (
        "current line only",
        Case {
            attributes: "rwv.lock merge=rwv-ours\n",
            finding: None,
            commits: false,
        },
    ),
    (
        "no entry for the lock",
        Case {
            attributes: "*.png binary\n",
            finding: Some("absent"),
            commits: false,
        },
    ),
    (
        "legacy line only",
        Case {
            attributes: "rwv.lock merge=ours\n",
            finding: Some("legacy-spelling"),
            commits: true,
        },
    ),
    (
        "both lines",
        Case {
            attributes: "*.png binary\nrwv.lock merge=ours\nrwv.lock merge=rwv-ours\n",
            finding: Some("legacy-alongside-current"),
            commits: true,
        },
    ),
];

/// A workspace with one project whose repo has `attributes` committed, and
/// the merge driver defined so that finding is not raised alongside.
fn workspace(root: &Path, attributes: &str) -> PathBuf {
    let repo_path = "github/acme/server";
    let repo = root.join(repo_path);
    std::fs::create_dir_all(&repo).unwrap();
    common::git_in(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "init\n").unwrap();
    common::git_in(&repo, &["add", "."]);
    common::git_in(&repo, &["commit", "-m", "initial"]);

    let project = root.join("projects").join("my-app");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("rwv.toml"),
        format!(
            "[repositories]\n[repositories.\"{repo_path}\"]\ntype = \"git\"\n\
             url = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    std::fs::write(project.join(".gitattributes"), attributes).unwrap();
    common::git_in(&project, &["init", "-b", "main"]);
    common::git_in(&project, &["add", "."]);
    common::git_in(&project, &["commit", "-m", "seed manifest and attributes"]);
    common::git_in(&project, &["config", "merge.rwv-ours.driver", "true"]);
    project
}

/// The `sub_kind` of the replay-exclusion finding `doctor --json` reports,
/// and `None` when it reports none.
fn reported(root: &Path) -> Option<String> {
    let out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(root)
        .output()
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("doctor --json must emit valid JSON");
    let violations = json["violations"]
        .as_array()
        .expect("doctor --json must carry a violations array")
        .clone();
    let mut found: Vec<String> = violations
        .iter()
        .filter(|v| v["kind"] == "missing-replay-exclusion")
        .map(|v| {
            v["sub_kind"]
                .as_str()
                .unwrap_or_else(|| panic!("finding carries no sub_kind: {v:#}"))
                .to_string()
        })
        .collect();
    assert!(
        found.len() <= 1,
        "one project, so at most one replay-exclusion finding; got {found:?}"
    );
    found.pop()
}

/// The `[fixed]` lines `doctor --fix` prints about `.gitattributes`.
fn repaired(root: &Path) -> Vec<String> {
    let out = common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.starts_with("[fixed] core:") && l.contains(".gitattributes"))
        .map(str::to_string)
        .collect()
}

#[test]
fn doctor_reports_every_state_its_fix_acts_on() {
    for (name, case) in CASES {
        let tmp = common::tempdir().unwrap();
        let root = tmp.path().join("ws");
        std::fs::create_dir_all(root.join("github")).unwrap();
        std::fs::create_dir_all(root.join("projects")).unwrap();
        let project = workspace(&root, case.attributes);
        let head_before = common::git_in(&project, &["rev-parse", "HEAD"]);

        let reported = reported(&root);
        let repaired = repaired(&root);

        assert_eq!(
            !repaired.is_empty(),
            reported.is_some(),
            "{name}: `--fix` and the report disagree about whether this state needs \
             repairing. doctor reported {reported:?}; `--fix` said {repaired:?}.\n\
             A repair with no finding is one the operator cannot be warned about, and \
             this one writes and commits in their own project repo.",
        );
        assert_eq!(
            reported.as_deref(),
            case.finding,
            "{name}: doctor reported the wrong finding for\n{:?}",
            case.attributes
        );

        let head_after = common::git_in(&project, &["rev-parse", "HEAD"]);
        assert_eq!(
            head_before != head_after,
            case.commits,
            "{name}: `--fix` was expected to {} the project repo; HEAD went {head_before} -> \
             {head_after}",
            if case.commits {
                "commit in"
            } else {
                "leave alone"
            }
        );
    }
}

#[test]
fn doctor_leaves_a_clean_project_byte_identical() {
    let (name, case) = &CASES[0];
    assert!(
        case.finding.is_none(),
        "{name} is the clean case this test is written against"
    );

    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    let project = workspace(&root, case.attributes);
    let attrs_path = project.join(".gitattributes");
    let before = std::fs::read_to_string(&attrs_path).unwrap();

    let repaired = repaired(&root);
    assert!(
        repaired.is_empty(),
        "`--fix` acted on a project doctor raises no finding for; it said {repaired:?}"
    );

    assert_eq!(
        before,
        std::fs::read_to_string(&attrs_path).unwrap(),
        "`--fix` rewrote a `.gitattributes` doctor called clean"
    );
}

#[test]
fn doctor_fix_drops_the_legacy_line_and_commits_it() {
    let (name, case) = CASES
        .iter()
        .find(|(_, c)| c.finding == Some("legacy-alongside-current"))
        .expect("the both-lines case is what this file exists for");
    assert!(
        case.attributes.contains("rwv.lock merge=ours\n")
            && case.attributes.contains("rwv.lock merge=rwv-ours"),
        "{name}: the fixture must carry both lines or the assertions below are vacuous"
    );

    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    let project = workspace(&root, case.attributes);

    let repaired = repaired(&root);
    assert!(
        repaired
            .iter()
            .any(|l| l.contains("dropped the legacy `rwv.lock merge=ours` line")
                && l.contains("(committed)")),
        "the operator's account of the commit `--fix` authored must say what it did; got {repaired:?}"
    );

    let committed = common::git_in(&project, &["show", "HEAD:.gitattributes"]);
    assert_eq!(
        committed
            .lines()
            .filter(|l| l.trim() == "rwv.lock merge=ours")
            .count(),
        0,
        "the legacy line must be gone from the committed tree; got {committed:?}"
    );
    assert_eq!(
        committed
            .lines()
            .filter(|l| l.trim() == "rwv.lock merge=rwv-ours")
            .count(),
        1,
        "exactly one current line must survive; got {committed:?}"
    );
    assert!(
        committed.contains("*.png binary"),
        "unrelated attributes must survive the repair; got {committed:?}"
    );

    assert_eq!(
        reported(&root),
        None,
        "the repair must settle the finding it was raised for"
    );
}
