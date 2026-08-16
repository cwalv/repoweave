//! The io cause behind a `VcsError::Io` must reach the operator, not just sit
//! in the enum.
//!
//! `VcsError::Io` holds the failing `io::Error`; whether an operator ever sees
//! it depends on the render, and every surface between the enum and the
//! terminal is free to drop it. Asserting the enum holds the source proves
//! nothing about that, so the load-bearing test here drives a real unreadable
//! file through the real binary and reads both operator renders.
//!
//! The fixture puts a *directory* where `.gitattributes` belongs. Any read
//! failure other than not-found takes the same arm; a directory takes it for
//! every user including root, which `chmod 000` does not.

use std::path::{Path, PathBuf};

mod common;

fn make_workspace(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    root
}

/// Workspace whose one project has an unreadable `.gitattributes`, so
/// `replay_exclusion_state` returns `VcsError::Io`.
fn workspace_with_unreadable_gitattributes(root: &Path) {
    let repo_path = "github/acme/server";
    let repo = root.join(repo_path);
    std::fs::create_dir_all(&repo).unwrap();
    common::git_in(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "init\n").unwrap();
    common::git_in(&repo, &["add", "."]);
    common::git_in(&repo, &["commit", "-m", "initial"]);

    let project_dir = root.join("projects").join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories]\n[repositories.\"{repo_path}\"]\ntype = \"git\"\n\
             url = \"https://github.com/acme/server.git\"\nversion = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    common::git_in(&project_dir, &["init", "-b", "main"]);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "initial"]);
    std::fs::create_dir(project_dir.join(".gitattributes")).unwrap();
}

/// Split a rendered `VcsError::Io` at the boundary between its context
/// sentence and the io cause. `None` means nothing follows the context —
/// the operator was handed rwv's own sentence and no statement of what
/// actually went wrong.
fn io_cause(rendered: &str) -> Option<&str> {
    rendered
        .split_once(".gitattributes: ")
        .map(|(_, cause)| cause)
        .filter(|cause| !cause.trim().is_empty())
}

#[test]
fn doctor_surfaces_the_io_cause_in_both_renders() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    workspace_with_unreadable_gitattributes(&root);

    let json_out = common::rwv()
        .args(["doctor", "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(&json_out.stdout).expect("doctor --json must emit valid JSON");

    let violation = json["violations"]
        .as_array()
        .expect("violations must be an array")
        .iter()
        .find(|v| v["kind"] == "replay-exclusion-unreadable")
        .unwrap_or_else(|| {
            panic!(
                "fixture did not produce an unreadable .gitattributes; without it this \
                 test asserts nothing. doctor --json said:\n{json:#}"
            )
        });

    let wire_error = violation["error"]
        .as_str()
        .expect("violation must carry a rendered error");
    let cause = io_cause(wire_error).unwrap_or_else(|| {
        panic!("--json dropped the io cause; operator got only: {wire_error:?}")
    });

    let human = String::from_utf8(
        common::rwv()
            .arg("doctor")
            .current_dir(&root)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        human.contains("failed to read .gitattributes for replay-exclusion check"),
        "human render lost the finding entirely:\n{human}"
    );
    assert!(
        io_cause(&human).is_some_and(|human_cause| human_cause.starts_with(cause)),
        "human render dropped the io cause {cause:?} that --json carried:\n{human}"
    );
}

/// `--fix` cannot ground a write in a read it cannot perform. It must defer to
/// the same report-only finding plain `doctor` raises rather than attempting
/// the write anyway and layering a second, error-severity finding for the
/// same unreadable file on top of it.
#[test]
fn doctor_fix_defers_to_the_same_report_only_finding_as_plain_doctor() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");
    workspace_with_unreadable_gitattributes(&root);

    let plain = common::rwv()
        .arg("doctor")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        plain.status.success(),
        "plain doctor should exit 0 on a report-only finding:\n{}",
        String::from_utf8_lossy(&plain.stdout)
    );

    let fix = common::rwv()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .unwrap();
    let fix_stdout = String::from_utf8(fix.stdout).unwrap();
    assert!(
        fix.status.success(),
        "doctor --fix must not exit non-zero over a finding it cannot act on:\n{fix_stdout}"
    );
    assert!(
        !fix_stdout.contains("failed to write replay-exclusion"),
        "doctor --fix attempted a write it could not ground in a read:\n{fix_stdout}"
    );
    assert!(
        fix_stdout.contains("failed to read .gitattributes for replay-exclusion check"),
        "doctor --fix dropped the report-only finding plain doctor raises:\n{fix_stdout}"
    );
}

/// The cause is emitted by `Display`, so a caller that renders the error with
/// no formatting of its own still hands it to the operator.
#[test]
fn vcs_error_io_display_emits_its_source() {
    let rendered = repoweave::vcs::VcsError::Io {
        ctx: "failed to read /w/p/.gitattributes".into(),
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "unreadable-by-anyone"),
    }
    .to_string();

    assert!(
        rendered.contains("unreadable-by-anyone"),
        "Display must emit the io cause, not only the context: {rendered:?}"
    );
}
