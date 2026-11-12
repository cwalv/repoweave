//! E2E tests for `rwv update`.
//!
//! `rwv update` re-materializes missing manifest members of the active project
//! and aligns each present clone to `rwv.lock`; it never writes the lock.
//!
//! MOVE, consent, the birth-target rule and the version-relatedness guard are
//! the branch model's vocabulary; `docs/internals/branch-model.md` specifies
//! each one.
//!
//! Adversarial coverage:
//! - missing member re-clone is BORN ATTACHED at the LOCKED SHA — not at
//!   branch HEAD, and not detached
//! - present member ALREADY at the locked SHA is not moved (mtime/HEAD
//!   unchanged) — which is not the same as "present members are untouched":
//!   a present member whose HEAD differs from the pin IS realigned
//! - alignment puts a present member in its expected state: on its declared
//!   branch (fast-forwarded when behind the pin, left where it is when ahead),
//!   moved off a branch whose commits other refs hold (reported), and left
//!   and reported when the branch holds commits nothing else does, when it is
//!   dirty, or when it is mid-operation
//! - `--detach-checkouts` is no argument
//! - the lock is never written: a member it lacks is cloned at the declared
//!   branch's tip when missing and left where it is when present
//! - `--repo` filter limits materialization
//! - end-to-end DanglingReference: doctor reports → update → doctor clean
//! - usage error when there is no workspace
//! - `--frozen`, `--no-reference` and `--allow-non-empty-dir` are not arguments

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn init_bare_repo(path: &Path) {
    let status = common::git()
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(path)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git init --bare failed");
}

/// Initialize a bare git repo with an initial commit AND a second commit,
/// so tests can distinguish between "aligned at locked SHA" and "at branch
/// HEAD".
fn init_bare_repo_with_two_commits(path: &Path) -> (String, String) {
    init_bare_repo(path);

    let tmp = common::tempdir().expect("tempdir for working clone");
    let work = tmp.path().join("work");

    common::git_in(
        tmp.path(),
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
    );
    common::git_in(&work, &["config", "user.email", "test@test.com"]);
    common::git_in(&work, &["config", "user.name", "Test"]);
    std::fs::write(work.join("README"), "initial\n").unwrap();
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "initial"]);
    let first_sha = common::git_in(&work, &["rev-parse", "HEAD"]);
    std::fs::write(work.join("README"), "second\n").unwrap();
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "second"]);
    let second_sha = common::git_in(&work, &["rev-parse", "HEAD"]);
    common::git_in(&work, &["push", "origin", "main"]);
    (first_sha, second_sha)
}

/// Set up a workspace with an active project. Returns the workspace root.
/// The project has two manifest entries; both bare repos are pre-materialized
/// on the caller (via clone from bare). The rwv.lock pins each repo to its
/// FIRST commit; the bare repo's HEAD is at the SECOND commit — so aligning
/// to the lock vs branch HEAD is observably different.
struct Setup {
    workspace: std::path::PathBuf,
    /// (repo_path, bare_repo_path, first_sha, second_sha)
    repos: Vec<(String, std::path::PathBuf, String, String)>,
    _tmp: tempfile::TempDir,
}

fn setup_workspace_with_locked_project(repo_paths: &[&str]) -> Setup {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let mut repos = Vec::new();
    for rp in repo_paths {
        let bare = tmp.path().join(format!("{}.git", rp.replace('/', "_")));
        let (first, second) = init_bare_repo_with_two_commits(&bare);
        repos.push((rp.to_string(), bare, first, second));
    }

    // Build the project directory in-place under projects/<name>.
    let project_dir = workspace.join("projects").join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let mut manifest = String::from("[repositories]\n");
    for (rp, bare, _, _) in &repos {
        let url = common::file_url(bare);
        manifest.push_str(&format!(
            "[repositories.\"{rp}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest).unwrap();

    let mut lock_entries: Vec<(String, String, String)> = Vec::new();
    for (rp, bare, first, _) in &repos {
        let url = common::file_url(bare);
        lock_entries.push((rp.clone(), url, first.clone()));
    }
    let entries: Vec<(&str, &str, &str)> = lock_entries
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &entries);

    // Activate the project.
    std::fs::write(workspace.join(".rwv-active"), "my-app\n").unwrap();

    Setup {
        workspace,
        repos,
        _tmp: tmp,
    }
}

/// Materialize a manifest repo on disk at the locked SHA (mimics what a
/// previous update would have done).
fn materialize_repo_at(workspace: &Path, repo_path: &str, bare: &Path, sha: &str) {
    let dest = workspace.join(repo_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    common::git_in(
        workspace.parent().unwrap_or(workspace),
        &["clone", &bare.to_string_lossy(), &dest.to_string_lossy()],
    );
    common::git_in(&dest, &["checkout", sha]);
}

/// Materialize a manifest repo and leave it ON its default branch, at branch
/// HEAD — the state an operator's clone is normally in, and the one where the
/// lock pin (an older SHA) and the checked-out commit disagree.
fn materialize_repo_on_branch(
    workspace: &Path,
    repo_path: &str,
    bare: &Path,
) -> std::path::PathBuf {
    let dest = workspace.join(repo_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    common::git_in(
        workspace.parent().unwrap_or(workspace),
        &["clone", &bare.to_string_lossy(), &dest.to_string_lossy()],
    );
    common::git_in(&dest, &["config", "user.email", "test@test.com"]);
    common::git_in(&dest, &["config", "user.name", "Test"]);
    dest
}

/// Materialize a manifest repo on its default branch and rewind the BRANCH
/// (not just HEAD) to `sha`, so the lock pin is a strict descendant of the
/// branch tip — the shape where realignment is a legal fast-forward.
fn materialize_repo_on_branch_at(
    workspace: &Path,
    repo_path: &str,
    bare: &Path,
    sha: &str,
) -> std::path::PathBuf {
    let dest = materialize_repo_on_branch(workspace, repo_path, bare);
    common::git_in(&dest, &["reset", "--hard", sha]);
    dest
}

/// The ref this checkout is on, or `None` when HEAD is detached.
///
/// Delegates to the shared primitive, which asks `Vcs::head_attachment` — the
/// production classifier — rather than `git symbolic-ref --short HEAD`.
/// `--short` returns the shortest *unambiguous* name, so a same-named tag
/// makes it answer `heads/main`; and its failure exit collapses detached,
/// unborn, and not-a-repo into one `None`.
fn current_branch(repo: &Path) -> Option<String> {
    common::checkout_ref(repo)
}

// ============================================================================
// In-place mode: missing member re-cloned at the LOCKED SHA
// ============================================================================

#[test]
fn update_materializes_missing_member_at_locked_sha() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo, _bare, first_sha, _second_sha) = &s.repos[0];

    // No repo directory yet — this is the dangling-reference state.
    let dest = s.workspace.join(repo);
    assert!(!dest.exists(), "precondition: repo dir must not exist");

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success();

    // Clone materialized...
    assert!(dest.exists(), "update must materialize the missing clone");
    assert!(dest.join(".git").exists(), "must be a real git clone");

    // ... AT THE LOCKED SHA (not branch HEAD). This is the load-bearing
    // assertion: `rwv update` aligns to the lock, unlike `rwv update --remote`,
    // which would advance to the second commit.
    let head = common::git_in(&dest, &["rev-parse", "HEAD"]);
    assert_eq!(
        head, *first_sha,
        "materialized clone must be at LOCKED SHA (first_sha), not branch HEAD"
    );

    // ... and ATTACHED to the tracking branch's local counterpart, with the
    // branch itself at the pin. The birth-target rule for an absent clone:
    // the birth happens AT the lock revision, so bootstrapping a weave from a
    // lock behind origin performs no MOVE and needs no consent. A clone that
    // landed on origin's tip and was then aligned would answer `None` here —
    // which is the state that made "detached" the normal resting state of a
    // freshly-cloned weave.
    assert_eq!(
        current_branch(&dest).as_deref(),
        Some("main"),
        "the clone must be born attached to the tracking counterpart, not detached at the pin"
    );
    assert_eq!(
        common::git_in(&dest, &["rev-parse", "main"]),
        *first_sha,
        "the branch itself is born AT the pin, not at origin's tip"
    );
}

// ============================================================================
// Present member ALREADY at the locked SHA is not moved (no HEAD churn, no
// re-clone). Scope note: the fixture leaves the member detached at the pin,
// so this pins the no-op case only. The realignment cases below cover a
// present member whose HEAD differs from the pin.
// ============================================================================

#[test]
fn update_leaves_a_member_that_is_in_its_expected_state_untouched() {
    let s = setup_workspace_with_locked_project(&["github/acme/a", "github/acme/b"]);
    let (repo_a, bare_a, first_a, _) = &s.repos[0];

    let dest_a = materialize_repo_on_branch_at(&s.workspace, repo_a, bare_a, first_a);
    let head_before = common::git_in(&dest_a, &["rev-parse", "HEAD"]);
    let git_config_before = std::fs::metadata(dest_a.join(".git/config"))
        .unwrap()
        .modified()
        .unwrap();

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "HEAD"]),
        head_before,
        "a member on its declared branch at its pin must not move"
    );
    assert_eq!(current_branch(&dest_a).as_deref(), Some("main"));
    let git_config_after = std::fs::metadata(dest_a.join(".git/config"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        git_config_before, git_config_after,
        "present member's .git/config must not be touched by update"
    );
}

#[test]
fn update_returns_a_detached_member_to_its_declared_branch() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, second_a) = &s.repos[0];

    materialize_repo_at(&s.workspace, repo_a, bare_a, first_a);
    let dest_a = s.workspace.join(repo_a);
    assert_eq!(current_branch(&dest_a), None, "precondition: detached");

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(
        current_branch(&dest_a).as_deref(),
        Some("main"),
        "a detached member holding nothing unpushed returns to its declared branch"
    );
    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "HEAD"]),
        *second_a,
        "the declared branch is ahead of the pin, and the checkout sits at its tip"
    );
}

// ============================================================================
// Present member whose HEAD differs from the pin IS realigned — as a MOVE of
// the tracking branch's local counterpart. The checkout stays on the branch;
// the branch advances under it.
// ============================================================================

#[test]
fn update_fast_forwards_the_counterpart_and_stays_attached() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, second_a) = &s.repos[0];

    // Present, clean, on main at the FIRST commit; the lock pins the SECOND,
    // so the pin is a strict descendant of the branch tip.
    let dest_a = materialize_repo_on_branch_at(&s.workspace, repo_a, bare_a, first_a);
    // The pin has to be the second commit for this to be a fast-forward; the
    // shared fixture locks the first, so re-pin here.
    let url_a = common::file_url(bare_a);
    common::fixture_lock(
        &s.workspace.join("projects/my-app"),
        &[(repo_a.as_str(), url_a.as_str(), second_a.as_str())],
    );

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "HEAD"]),
        second_a.as_str(),
        "present member must be realigned to the locked SHA"
    );
    assert_eq!(
        current_branch(&dest_a).as_deref(),
        Some("main"),
        "realignment is a MOVE of the counterpart, so the checkout stays on it"
    );
    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "main"]),
        second_a.as_str(),
        "the branch ref is what moved — HEAD did not leave it behind"
    );
}

// ============================================================================
// Precondition (R1): realignment refuses when it would change what HEAD is
// attached to without consent. Not "when work would be stranded" — a clean,
// published, fully-pushed branch refuses too, because the pin it is being
// asked to take is not a fast-forward.
// ============================================================================

#[test]
fn update_leaves_the_declared_branch_where_it_is_when_it_is_ahead_of_the_pin() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, second_a) = &s.repos[0];

    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    assert_eq!(current_branch(&dest_a).as_deref(), Some("main"));

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(
        current_branch(&dest_a).as_deref(),
        Some("main"),
        "a pin behind the branch's tip rewinds nothing and detaches nothing"
    );
    assert_eq!(common::git_in(&dest_a, &["rev-parse", "HEAD"]), *second_a);
    assert_ne!(common::git_in(&dest_a, &["rev-parse", "HEAD"]), *first_a);
}

#[test]
fn update_leaves_a_dirty_member_on_its_declared_branch_alone() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, _first_a, second_a) = &s.repos[0];

    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    std::fs::write(dest_a.join("scratch.txt"), "work in progress\n").unwrap();

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(current_branch(&dest_a).as_deref(), Some("main"));
    assert_eq!(common::git_in(&dest_a, &["rev-parse", "HEAD"]), *second_a);
    assert!(dest_a.join("scratch.txt").exists());
}

#[test]
fn update_keeps_commits_origin_does_not_have_on_the_declared_branch() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, _first_a, _second_a) = &s.repos[0];

    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    std::fs::write(dest_a.join("README"), "third\n").unwrap();
    common::git_in(&dest_a, &["commit", "-am", "third"]);
    let unpushed = common::git_in(&dest_a, &["rev-parse", "HEAD"]);

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(current_branch(&dest_a).as_deref(), Some("main"));
    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "HEAD"]),
        unpushed,
        "the commit origin has never seen is still checked out"
    );
    assert_eq!(common::git_in(&dest_a, &["rev-parse", "main"]), unpushed);
}

#[test]
fn update_moves_a_member_off_a_branch_whose_commits_other_refs_hold_and_reports_it() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, second_a) = &s.repos[0];

    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    common::git_in(&dest_a, &["checkout", "-b", "feature", first_a]);
    let url_a = common::file_url(bare_a);
    common::fixture_lock(
        &s.workspace.join("projects/my-app"),
        &[(repo_a.as_str(), url_a.as_str(), second_a.as_str())],
    );

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "github/acme/a: moved from feature",
        ));

    assert_eq!(current_branch(&dest_a).as_deref(), Some("main"));
    assert_eq!(common::git_in(&dest_a, &["rev-parse", "HEAD"]), *second_a);
    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "feature"]),
        *first_a,
        "the branch it left is where it was"
    );
}

#[test]
fn update_reports_and_leaves_a_member_on_a_branch_holding_commits_nothing_else_does() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, _first_a, _second_a) = &s.repos[0];

    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    common::git_in(&dest_a, &["checkout", "-b", "wip"]);
    std::fs::write(dest_a.join("README"), "wip\n").unwrap();
    common::git_in(&dest_a, &["commit", "-am", "wip work"]);
    let wip = common::git_in(&dest_a, &["rev-parse", "HEAD"]);

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "github/acme/a: alignment on branch with stray commits (wip)",
        ))
        .stdout(predicate::str::contains("wip work"))
        .stdout(predicate::str::contains("rwv explain alignment"));

    assert_eq!(current_branch(&dest_a).as_deref(), Some("wip"));
    assert_eq!(common::git_in(&dest_a, &["rev-parse", "HEAD"]), wip);
}

#[test]
fn lock_refuses_a_member_holding_stray_commits_naming_the_branch_they_are_copied_onto() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, _first_a, _second_a) = &s.repos[0];

    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    common::git_in(&dest_a, &["checkout", "-b", "wip"]);
    std::fs::write(dest_a.join("README"), "wip\n").unwrap();
    common::git_in(&dest_a, &["commit", "-am", "wip work"]);

    let out = rwv()
        .arg("lock")
        .current_dir(&s.workspace)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "{text}");
    assert!(
        text.contains("github/acme/a: alignment on branch with stray commits (wip)"),
        "{text}"
    );
    assert!(text.contains("copy these commits onto `main`"), "{text}");
    assert!(!text.contains("<b>"), "{text}");
}

#[test]
fn the_flags_alignment_replaced_no_longer_parse() {
    for (verb, flag) in [
        ("update", "--detach-checkouts"),
        ("pull", "--detach-checkouts"),
        ("doctor", "--adopt-detached-checkouts"),
        ("doctor", "--reattach-checkouts"),
    ] {
        rwv()
            .args([verb, flag])
            .assert()
            .failure()
            .stderr(predicate::str::contains(format!(
                "unexpected argument '{flag}'"
            )));
    }
}

// ============================================================================
// An already-detached member stays detached: HEAD moves, its symbolic-ness
// does not, so this is a MOVE and needs no consent — but the mid-operation
// precondition applies to it.
// ============================================================================

#[test]
fn update_returns_a_detached_member_ahead_of_the_pin_to_its_declared_branch() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, _first_a, second_a) = &s.repos[0];

    materialize_repo_at(&s.workspace, repo_a, bare_a, second_a);
    let dest_a = s.workspace.join(repo_a);
    assert_eq!(current_branch(&dest_a), None, "precondition: detached");

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(current_branch(&dest_a).as_deref(), Some("main"));
    assert_eq!(common::git_in(&dest_a, &["rev-parse", "HEAD"]), *second_a);
}

#[test]
fn update_reports_and_leaves_a_detached_member_that_is_mid_operation() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, second_a) = &s.repos[0];

    materialize_repo_at(&s.workspace, repo_a, bare_a, second_a);
    let dest_a = s.workspace.join(repo_a);
    common::git_in(&dest_a, &["bisect", "start"]);

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success()
        .stdout(predicate::str::contains("edits mid-operation"))
        .stdout(predicate::str::contains("bisect"));

    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "HEAD"]),
        *second_a,
        "the bisect's HEAD must not be yanked out from under it"
    );
    assert_ne!(common::git_in(&dest_a, &["rev-parse", "HEAD"]), *first_a);
    assert_eq!(current_branch(&dest_a), None);
}

// ============================================================================
// The lock is read, never written: a member it lacks is cloned at its declared
// branch's tip when missing and left where it is when present
// ============================================================================

const MEMBER_A: &str = "github/acme/a";
const MEMBER_B: &str = "github/acme/b";
const B_BRANCH: &str = "release";

/// A project whose members are `a`, declaring `main`, and `b`, declaring
/// `release` — a branch whose tip is neither `main`'s nor the remote's HEAD.
/// The lock file is written only when `lock_a` is set, and then pins `a` at
/// its first commit and nothing else.
struct Pair {
    workspace: std::path::PathBuf,
    project_dir: std::path::PathBuf,
    bare_a: std::path::PathBuf,
    bare_b: std::path::PathBuf,
    first_a: String,
    second_a: String,
    main_b: String,
    release_b: String,
    _tmp: tempfile::TempDir,
}

impl Pair {
    fn new(lock_a: bool) -> Pair {
        let tmp = common::tempdir().unwrap();
        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();

        let bare_a = tmp.path().join("a.git");
        let bare_b = tmp.path().join("b.git");
        let (first_a, second_a) = init_bare_repo_with_two_commits(&bare_a);
        let (_, main_b) = init_bare_repo_with_two_commits(&bare_b);
        let release_b = push_new_branch(&bare_b, B_BRANCH);

        let project_dir = workspace.join("projects").join("my-app");
        std::fs::create_dir_all(&project_dir).unwrap();
        let url_a = common::file_url(&bare_a);
        let url_b = common::file_url(&bare_b);
        std::fs::write(
            project_dir.join("rwv.toml"),
            format!(
                "[repositories.\"{MEMBER_A}\"]\ntype = \"git\"\nurl = \"{url_a}\"\nbranch = \"main\"\nrole = \"owned\"\n\n\
                 [repositories.\"{MEMBER_B}\"]\ntype = \"git\"\nurl = \"{url_b}\"\nbranch = \"{B_BRANCH}\"\nrole = \"owned\"\n"
            ),
        )
        .unwrap();
        if lock_a {
            common::fixture_lock(&project_dir, &[(MEMBER_A, &url_a, &first_a)]);
        }
        std::fs::write(workspace.join(".rwv-active"), "my-app\n").unwrap();

        Pair {
            workspace,
            project_dir,
            bare_a,
            bare_b,
            first_a,
            second_a,
            main_b,
            release_b,
            _tmp: tmp,
        }
    }

    fn lock_bytes(&self) -> Option<Vec<u8>> {
        std::fs::read(self.project_dir.join("rwv.lock")).ok()
    }

    fn member(&self, repo: &str) -> std::path::PathBuf {
        self.workspace.join(repo)
    }

    /// `b` present on its declared branch, one commit behind the remote's tip.
    fn place_b_behind_its_tip(&self) -> std::path::PathBuf {
        let dest = materialize_repo_on_branch(&self.workspace, MEMBER_B, &self.bare_b);
        common::git_in(&dest, &["checkout", B_BRANCH]);
        common::git_in(&dest, &["reset", "--hard", &self.main_b]);
        dest
    }
}

/// Create `branch` on the bare repo with one commit beyond `main`, returning
/// its tip.
fn push_new_branch(bare: &Path, branch: &str) -> String {
    let tmp = common::tempdir().expect("tempdir for working clone");
    let work = tmp.path().join("work");
    common::git_in(
        tmp.path(),
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
    );
    common::git_in(&work, &["config", "user.email", "test@test.com"]);
    common::git_in(&work, &["config", "user.name", "Test"]);
    common::git_in(&work, &["checkout", "-b", branch]);
    std::fs::write(work.join("README"), "release\n").unwrap();
    common::git_in(&work, &["commit", "-am", "release"]);
    common::git_in(&work, &["push", "origin", branch]);
    common::git_in(&work, &["rev-parse", "HEAD"])
}

#[test]
fn update_clones_a_member_the_lock_lacks_on_its_declared_branch_and_leaves_the_lock_bytes_alone() {
    let p = Pair::new(true);
    let lock_before = p.lock_bytes();
    assert!(
        lock_before.is_some(),
        "precondition: the fixture wrote a lock"
    );

    rwv()
        .arg("update")
        .current_dir(&p.workspace)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "cloned github/acme/b onto release at the branch tip: the lock has no entry for it",
        ));

    let (dest_a, dest_b) = (p.member(MEMBER_A), p.member(MEMBER_B));
    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "HEAD"]),
        p.first_a,
        "the member the lock covers is born at its pin, behind the remote's tip"
    );
    assert_ne!(p.first_a, p.second_a, "fixture: the pin is not the tip");
    assert_eq!(
        current_branch(&dest_b).as_deref(),
        Some(B_BRANCH),
        "the member the lock lacks is born on its declared branch, not the remote's HEAD"
    );
    assert_eq!(
        common::git_in(&dest_b, &["rev-parse", "HEAD"]),
        p.release_b,
        "the member the lock lacks is born at the remote tip of its declared branch"
    );
    assert_ne!(
        p.release_b, p.main_b,
        "fixture: the branch tip is not main's"
    );
    assert_eq!(
        p.lock_bytes(),
        lock_before,
        "update writes no lock: the member it cloned is not added"
    );
}

#[test]
fn update_with_no_lock_clones_every_member_at_its_branch_tip_and_creates_no_lock() {
    let p = Pair::new(false);
    assert_eq!(p.lock_bytes(), None, "precondition: there is no lock");

    rwv()
        .arg("update")
        .current_dir(&p.workspace)
        .assert()
        .success();

    let (dest_a, dest_b) = (p.member(MEMBER_A), p.member(MEMBER_B));
    assert_eq!(current_branch(&dest_a).as_deref(), Some("main"));
    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "HEAD"]),
        p.second_a,
        "with no lock a member is born at its declared branch's tip"
    );
    assert_eq!(current_branch(&dest_b).as_deref(), Some(B_BRANCH));
    assert_eq!(common::git_in(&dest_b, &["rev-parse", "HEAD"]), p.release_b);
    assert_eq!(
        p.lock_bytes(),
        None,
        "update must not create the lock it found missing"
    );
}

#[test]
fn update_leaves_a_present_member_the_lock_lacks_where_it_is() {
    let p = Pair::new(true);
    let dest_b = p.place_b_behind_its_tip();
    assert_ne!(
        p.main_b, p.release_b,
        "fixture: the member is behind its tip"
    );

    rwv()
        .arg("update")
        .current_dir(&p.workspace)
        .assert()
        .success();

    assert_eq!(current_branch(&dest_b).as_deref(), Some(B_BRANCH));
    assert_eq!(common::git_in(&dest_b, &["rev-parse", "HEAD"]), p.main_b);
    assert_eq!(
        common::git_in(&dest_b, &["rev-parse", B_BRANCH]),
        p.main_b,
        "the branch is not advanced to the remote's tip"
    );
}

#[test]
fn update_with_no_lock_sits_every_present_member_on_its_declared_branch_as_found() {
    let p = Pair::new(false);
    let dest_b = p.place_b_behind_its_tip();
    materialize_repo_at(&p.workspace, MEMBER_A, &p.bare_a, &p.first_a);
    let dest_a = p.member(MEMBER_A);
    assert_eq!(current_branch(&dest_a), None, "precondition: a is detached");

    rwv()
        .arg("update")
        .current_dir(&p.workspace)
        .assert()
        .success();

    assert_eq!(
        current_branch(&dest_a).as_deref(),
        Some("main"),
        "a detached member the lock is silent on sits on its declared branch as found"
    );
    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "HEAD"]),
        p.second_a,
        "the declared branch is where it was"
    );
    assert_eq!(current_branch(&dest_b).as_deref(), Some(B_BRANCH));
    assert_eq!(common::git_in(&dest_b, &["rev-parse", "HEAD"]), p.main_b);
    assert_eq!(p.lock_bytes(), None, "update must not create a lock");
}

// ============================================================================
// --repo filter limits materialization
// ============================================================================

#[test]
fn update_repo_filter_limits_materialization() {
    let s = setup_workspace_with_locked_project(&["github/acme/a", "github/acme/b"]);
    let (repo_a, _, first_a, _) = &s.repos[0];
    let (repo_b, _, _, _) = &s.repos[1];

    // Both repos missing. Filter to only repo_a.
    rwv()
        .args(["update", "--repo", repo_a])
        .current_dir(&s.workspace)
        .assert()
        .success();

    let dest_a = s.workspace.join(repo_a);
    let dest_b = s.workspace.join(repo_b);
    assert!(
        dest_a.exists(),
        "repo_a must be materialized (matches --repo)"
    );
    assert!(
        !dest_b.exists(),
        "repo_b must NOT be materialized (excluded by --repo filter)"
    );

    // repo_a is at locked SHA.
    let head_a = common::git_in(&dest_a, &["rev-parse", "HEAD"]);
    assert_eq!(head_a, *first_a, "materialized clone must be at LOCKED SHA");
}

fn setup_workspace_with_locked_roles(members: &[(&str, &str)]) -> Setup {
    let paths: Vec<&str> = members.iter().map(|(path, _)| *path).collect();
    let s = setup_workspace_with_locked_project(&paths);
    let mut manifest = String::new();
    for ((path, bare, _, _), (_, role)) in s.repos.iter().zip(members) {
        let url = common::file_url(bare);
        manifest.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"{role}\"\n"
        ));
    }
    std::fs::write(s.workspace.join("projects/my-app/rwv.toml"), manifest).unwrap();
    s
}

fn materialized(s: &Setup, path: &str) -> bool {
    s.workspace.join(path).join(".git").exists()
}

#[test]
fn update_role_filter_materializes_only_that_role() {
    let s = setup_workspace_with_locked_roles(&[
        ("local/org/p", "owned"),
        ("local/org/d", "dependency"),
    ]);

    rwv()
        .args(["update", "--role", "owned"])
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert!(
        materialized(&s, "local/org/p"),
        "the owned member is cloned"
    );
    assert!(
        !materialized(&s, "local/org/d"),
        "the dependency is not cloned under --role owned"
    );
}

#[test]
fn update_repo_glob_filter_materializes_matching_paths() {
    let s = setup_workspace_with_locked_roles(&[
        ("local/org/a", "owned"),
        ("local/org/b", "owned"),
        ("local/other/c", "owned"),
    ]);

    rwv()
        .args(["update", "--repo", "glob:local/org/*"])
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert!(materialized(&s, "local/org/a"));
    assert!(materialized(&s, "local/org/b"));
    assert!(
        !materialized(&s, "local/other/c"),
        "glob:local/org/* does not match local/other/c"
    );
}

#[test]
fn update_repo_regex_filter_materializes_matching_paths() {
    let s = setup_workspace_with_locked_roles(&[
        ("local/cwalv/a", "owned"),
        ("local/cwalv/b", "owned"),
        ("local/other/c", "owned"),
    ]);

    rwv()
        .args(["update", "--repo", "re:^local/cwalv/"])
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert!(materialized(&s, "local/cwalv/a"));
    assert!(materialized(&s, "local/cwalv/b"));
    assert!(!materialized(&s, "local/other/c"));
}

#[test]
fn update_role_and_repo_selectors_union() {
    let s = setup_workspace_with_locked_roles(&[
        ("local/me/p", "owned"),
        ("local/external/dep", "dependency"),
        ("local/external/other", "dependency"),
    ]);

    rwv()
        .args(["update", "--role", "owned", "--repo", "local/external/dep"])
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert!(materialized(&s, "local/me/p"), "owned, by --role");
    assert!(materialized(&s, "local/external/dep"), "named by --repo");
    assert!(
        !materialized(&s, "local/external/other"),
        "matched by neither selector"
    );
}

#[test]
fn update_filter_aligns_only_the_selected_present_members() {
    let s =
        setup_workspace_with_locked_roles(&[("local/org/a", "owned"), ("local/other/b", "owned")]);
    let mut pins = Vec::new();
    let mut dests = Vec::new();
    for (path, bare, first, second) in &s.repos {
        dests.push(materialize_repo_on_branch_at(
            &s.workspace,
            path,
            bare,
            first,
        ));
        pins.push((path.clone(), common::file_url(bare), second.clone()));
    }
    let entries: Vec<(&str, &str, &str)> = pins
        .iter()
        .map(|(p, u, sha)| (p.as_str(), u.as_str(), sha.as_str()))
        .collect();
    common::fixture_lock(&s.workspace.join("projects/my-app"), &entries);

    rwv()
        .args(["update", "--repo", "glob:local/org/*"])
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&dests[0], &["rev-parse", "HEAD"]),
        s.repos[0].3,
        "the selected member is aligned to its pin"
    );
    assert_eq!(
        common::git_in(&dests[1], &["rev-parse", "HEAD"]),
        s.repos[1].2,
        "the member the selector leaves out stays where it was"
    );
}

// ============================================================================
// End-to-end: doctor dangling → update → doctor clean
// ============================================================================

#[test]
fn dangling_reference_end_to_end_update_repairs_and_doctor_clean() {
    // The audit round found text-only tests let dead advice through. This
    // test exercises the FULL loop: create a dangling reference, doctor
    // reports it, run the named repair verb (`rwv update`), doctor now clean.
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo, _, first_sha, _) = &s.repos[0];

    // 1. doctor must report dangling reference (repo missing on disk).
    let out1 = rwv()
        .arg("doctor")
        .current_dir(&s.workspace)
        .output()
        .expect("doctor spawn failed");
    let combined1 = format!(
        "{}{}",
        String::from_utf8_lossy(&out1.stdout),
        String::from_utf8_lossy(&out1.stderr)
    );
    assert!(
        !out1.status.success(),
        "doctor must exit non-zero when a dangling reference exists; got:\n{combined1}"
    );
    assert!(
        combined1.contains("dangling"),
        "doctor must report dangling reference; got:\n{combined1}"
    );
    assert!(
        combined1.contains("run `rwv update` from the workspace"),
        "doctor's dangling-reference message must name `rwv update` as the repair verb; \
         got:\n{combined1}"
    );

    // 2. Run the named repair verb: `rwv update`.
    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success();

    // 3. Repo now exists at locked SHA.
    let dest = s.workspace.join(repo);
    assert!(dest.exists(), "update must materialize the dangling repo");
    let head = common::git_in(&dest, &["rev-parse", "HEAD"]);
    assert_eq!(head, *first_sha, "repair must land at LOCKED SHA");

    // 4. doctor is now clean.
    let out2 = rwv()
        .arg("doctor")
        .current_dir(&s.workspace)
        .output()
        .expect("doctor spawn failed");
    let combined2 = format!(
        "{}{}",
        String::from_utf8_lossy(&out2.stdout),
        String::from_utf8_lossy(&out2.stderr)
    );
    assert!(
        out2.status.success(),
        "doctor must be clean after update repair; got:\n{combined2}"
    );
    assert!(
        !combined2.contains("dangling"),
        "doctor must no longer report a dangling reference; got:\n{combined2}"
    );
}

// ============================================================================
// Usage: no workspace; arguments that belong to other verbs
// ============================================================================

#[test]
fn update_outside_a_workspace_names_the_missing_weave_root() {
    let tmp = common::tempdir().unwrap();
    rwv()
        .arg("update")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("no repoweave workspace found above")
                .and(predicate::str::contains("rwv explain no-weave-root")),
        );
}

#[test]
fn update_refuses_the_arguments_that_belong_to_other_verbs() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    for flag in ["--frozen", "--no-reference", "--allow-non-empty-dir"] {
        rwv()
            .args(["update", flag])
            .current_dir(&s.workspace)
            .assert()
            .code(2)
            .stderr(predicate::str::contains(format!(
                "unexpected argument '{flag}' found"
            )));
    }
    assert!(
        !s.workspace.join("github/acme/a").exists(),
        "a refused invocation materializes nothing"
    );
}

// ============================================================================
// Workweave invocation: canonical clone lands at PRIMARY, not workweave
// ============================================================================

#[test]
fn update_from_workweave_materializes_at_primary() {
    // Clone-topology I1: the canonical store per manifest repo lives at
    // primary's `<weave>/<repo_path>`. Invoking `rwv update` from a workweave
    // must materialize the missing member at PRIMARY's canonical path, not
    // at the workweave's slot. See docs/explanation/joints/clone-topology.md.
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo, _, first_sha, _) = &s.repos[0];

    // Set up a workweave marker pointing at primary.
    let workweave_dir = s.workspace.join(".workweaves/my-app--dev");
    std::fs::create_dir_all(&workweave_dir).unwrap();
    // A projects/<name>/rwv.toml + rwv.lock is per-workspace state; the
    // workweave gets its own copy (mirrors how rwv workweave create/rwv
    // activate populate this). Copy the primary's project files into the
    // workweave so `require_active_project_on_disk` finds them.
    std::fs::create_dir_all(workweave_dir.join("projects/my-app")).unwrap();
    let primary_project = s.workspace.join("projects/my-app");
    std::fs::copy(
        primary_project.join("rwv.toml"),
        workweave_dir.join("projects/my-app/rwv.toml"),
    )
    .unwrap();
    std::fs::copy(
        primary_project.join("rwv.lock"),
        workweave_dir.join("projects/my-app/rwv.lock"),
    )
    .unwrap();

    // Write the workweave marker. Format: JSON with `primary`, `project`, `parent`.
    let marker_yaml = common::workweave_marker(&s.workspace, "my-app", &s.workspace);
    std::fs::write(workweave_dir.join(".rwv-workweave"), marker_yaml).unwrap();
    common::register_workweave(&s.workspace, "my-app", "dev", &workweave_dir);

    // Run update from INSIDE the workweave.
    let output = rwv()
        .arg("update")
        .current_dir(&workweave_dir)
        .output()
        .expect("update spawn failed");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "update from workweave should succeed (canonical materializes at primary); \
         got:\n{combined}"
    );

    // Canonical clone must exist at PRIMARY's path…
    let primary_dest = s.workspace.join(repo);
    assert!(
        primary_dest.exists(),
        "canonical clone must be materialized at primary's `<weave>/<repo_path>`, \
         not the workweave's slot (clone-topology I1)"
    );
    // …at the locked SHA.
    let head = common::git_in(&primary_dest, &["rev-parse", "HEAD"]);
    assert_eq!(
        head, *first_sha,
        "primary canonical clone must be at LOCKED SHA"
    );

    // The workweave's slot holds a worktree linked into that canonical store
    // — never a second clone, which would be two DAGs for one repo — on the
    // workweave's ephemeral branch at the locked SHA.
    let workweave_dest = workweave_dir.join(repo);
    assert!(
        workweave_dest.join(".git").is_file(),
        "update from a workweave must create the workweave's worktree for the member \
         its lock pins, as a linked worktree (`.git` a file), not a clone; got:\n{combined}"
    );
    let common_dir = common::git_in(&workweave_dest, &["rev-parse", "--git-common-dir"]);
    assert_eq!(
        repoweave::path_spelling::resolve(&workweave_dest.join(common_dir)).unwrap(),
        repoweave::path_spelling::resolve(&primary_dest.join(".git")).unwrap(),
        "the workweave's worktree must share primary's canonical store (clone-topology I1)"
    );
    assert_eq!(
        common::checkout_ref(&workweave_dest),
        None,
        "the project is no repo, so the workspace is declared: the primary holds `main`, \
         so the worktree sits detached at the pin"
    );
    assert_eq!(
        common::git_in(&workweave_dest, &["rev-parse", "HEAD"]),
        *first_sha,
        "the worktree sits at the revision the workweave's lock pins"
    );
    assert!(
        combined.contains("checkout: main is checked out in"),
        "the detach names the checkout holding the branch; got:\n{combined}"
    );
}

// ============================================================================
// Project selection: --project, and the weave's only project
// ============================================================================

/// Add a second project, `other`, whose one member is `github/acme/b`.
fn add_second_project(s: &Setup) -> std::path::PathBuf {
    let bare = s._tmp.path().join("github_acme_b.git");
    let (first, _) = init_bare_repo_with_two_commits(&bare);
    let url = common::file_url(&bare);
    let project_dir = s.workspace.join("projects/other");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories]\n[repositories.\"github/acme/b\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    common::fixture_lock(&project_dir, &[("github/acme/b", &url, &first)]);
    s.workspace.join("github/acme/b")
}

#[test]
fn update_activates_the_only_project_when_none_is_active() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    std::fs::remove_file(s.workspace.join(".rwv-active")).unwrap();

    rwv()
        .arg("update")
        .current_dir(&s.workspace)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "activating project 'my-app', the only project in this weave",
        ));

    assert!(s.workspace.join("github/acme/a/.git").exists());
    assert_eq!(
        std::fs::read_to_string(s.workspace.join(".rwv-active"))
            .unwrap()
            .trim(),
        "my-app",
        "the weave's only project must be active after the update"
    );
}

#[test]
fn update_project_flag_targets_that_project_without_selecting_it() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let other_member = add_second_project(&s);

    rwv()
        .args(["update", "--project", "other"])
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert!(
        other_member.join(".git").exists(),
        "--project names the manifest updated"
    );
    assert!(
        !s.workspace.join("github/acme/a").exists(),
        "the active project's members are not cloned"
    );
    assert_eq!(
        std::fs::read_to_string(s.workspace.join(".rwv-active"))
            .unwrap()
            .trim(),
        "my-app",
        "--project does not change .rwv-active"
    );
}

#[test]
fn update_project_flag_does_not_activate_the_only_project() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    std::fs::remove_file(s.workspace.join(".rwv-active")).unwrap();

    rwv()
        .args(["update", "--project", "my-app"])
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert!(s.workspace.join("github/acme/a/.git").exists());
    assert!(
        !s.workspace.join(".rwv-active").exists(),
        "--project does not change .rwv-active, even with one project"
    );
}
