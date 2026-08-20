//! One `Error:` per refusal, and a refusal that reaches stderr once.
//!
//! Two defects, both of which read as symmetry to a later editor. A message
//! literal that spells its own `Error: ` renders `Error: Error: …`, because
//! the decoration is already the terminal reporter's to add. A refusal printed
//! straight to stderr and *then* bailed puts the same sentence in front of the
//! operator twice, once above the decorated copy.

mod common;

use common::src_scan;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

/// A workspace holding one active, committed, repo-less project.
fn workspace_with_project(tmp: &tempfile::TempDir) -> std::path::PathBuf {
    let workspace = tmp.path().join("ws");
    let project_dir = workspace.join("projects").join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    common::git_in(&project_dir, &["init", "--initial-branch=main"]);
    common::git_in(&project_dir, &["config", "user.email", "test@test.com"]);
    common::git_in(&project_dir, &["config", "user.name", "Test"]);
    std::fs::write(project_dir.join("rwv.toml"), "[repositories]\n").unwrap();
    common::git_in(&project_dir, &["add", "rwv.toml"]);
    common::git_in(&project_dir, &["commit", "-m", "init"]);

    std::fs::write(workspace.join(".rwv-active"), "test-project\n").unwrap();
    workspace
}

/// Stderr of a run that must fail, with the count of `Error:` decorations it
/// carries — the number a reader sees, not a property of the error value.
fn refusal_stderr(args: &[&str], cwd: &std::path::Path) -> (String, usize) {
    let output = rwv()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("rwv should run");
    assert!(
        !output.status.success(),
        "rwv {args:?} was expected to refuse, exit was {}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let decorations = stderr.matches("Error:").count();
    (stderr, decorations)
}

fn assert_one_decoration(args: &[&str], cwd: &std::path::Path, sentence: &str) {
    let (stderr, decorations) = refusal_stderr(args, cwd);
    assert!(
        stderr.contains(sentence),
        "rwv {args:?} did not refuse with the expected sentence, stderr was:\n{stderr}"
    );
    assert_eq!(
        decorations, 1,
        "rwv {args:?} decorated its refusal {decorations} time(s), stderr was:\n{stderr}"
    );
}

#[test]
fn add_unrecognized_url_is_decorated_once() {
    let tmp = common::tempdir().unwrap();
    let workspace = workspace_with_project(&tmp);
    assert_one_decoration(
        &["add", "not-a-valid-url-at-all"],
        &workspace,
        "unrecognized URL",
    );
}

#[test]
fn remove_absent_path_is_decorated_once() {
    let tmp = common::tempdir().unwrap();
    let workspace = workspace_with_project(&tmp);
    assert_one_decoration(
        &["remove", "nonexistent/path/repo"],
        &workspace,
        "not found in manifest",
    );
}

#[test]
fn add_new_malformed_path_is_decorated_once() {
    let tmp = common::tempdir().unwrap();
    let workspace = workspace_with_project(&tmp);
    assert_one_decoration(
        &["add", "not-a-path", "--new"],
        &workspace,
        "does not look like a valid repo path",
    );
}

#[test]
fn add_new_unknown_registry_is_decorated_once() {
    let tmp = common::tempdir().unwrap();
    let workspace = workspace_with_project(&tmp);
    assert_one_decoration(
        &["add", "unknownhost/owner/repo", "--new"],
        &workspace,
        "could not infer a URL",
    );
}

/// The occupied-project refusal and its scoped-path hint each reach the
/// operator exactly once, through the error the verb returns.
///
/// Counting the sentence rather than the decoration is what separates this
/// from its siblings: a print-then-bail pair whose printed half carries no
/// `Error: ` of its own doubles the sentence while leaving the decoration
/// count at one.
#[test]
fn fetch_occupied_project_refuses_once() {
    let tmp = common::tempdir().unwrap();
    let source = tmp.path().join("web-app.git");
    common::git_in(
        tmp.path(),
        &["init", "--bare", "--initial-branch=main", "web-app.git"],
    );

    let workspace = tmp.path().join("ws");
    let occupied = workspace.join("projects").join("web-app");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("rwv.toml"), "[repositories]\n").unwrap();

    let (stderr, decorations) = refusal_stderr(&["fetch", &common::file_url(&source)], &workspace);

    assert_eq!(
        stderr.matches("cannot fetch project").count(),
        1,
        "the refusal reached the operator more than once, stderr was:\n{stderr}"
    );
    assert_eq!(
        stderr.matches("try a scoped path").count(),
        1,
        "the hint reached the operator more than once, stderr was:\n{stderr}"
    );
    assert_eq!(
        decorations, 1,
        "the refusal was decorated {decorations} time(s), stderr was:\n{stderr}"
    );
}

/// No message literal in `src/` opens with the `Error: ` decoration.
///
/// Structural because the population is a prohibition over every operator
/// message the crate can mint, and the behavioural pins above reach only the
/// five refusals a fixture can drive: a sixth site added tomorrow in a verb
/// nothing here invokes is invisible to them and visible to this.
///
/// Scope: production lines under `src/`, comment lines and `#[cfg(test)]`
/// items already dropped by the scanner, matched on a literal opening with
/// `"Error: `. A message that acquires the decoration by concatenation, by
/// interpolation, or through a raw-string literal is outside what this reads.
#[test]
fn no_message_literal_spells_the_error_decoration() {
    let lines = src_scan::production_lines();
    assert!(
        lines.len() > 1000,
        "the source scan yielded {} lines, which is too few to have read src/",
        lines.len()
    );

    let sites: Vec<String> = lines
        .iter()
        .filter(|l| l.text.contains("\"Error: "))
        .map(|l| format!("{}: {}", l.site(), l.text.trim()))
        .collect();

    assert!(
        sites.is_empty(),
        "the terminal reporter prints `Error: `; these literals print a second one:\n{}",
        sites.join("\n")
    );
}
