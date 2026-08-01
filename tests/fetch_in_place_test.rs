//! E2E tests for `rwv fetch` in-place mode (no SOURCE argument).
//!
//! Covers the settled repair verb for dangling references:
//! `rwv fetch` with no SOURCE re-materializes missing manifest members of
//! the active project, aligning each clone to `rwv.lock` (or branch HEAD
//! when the lock has no entry).
//!
//! Adversarial coverage:
//! - missing member re-clone is BORN ATTACHED at the LOCKED SHA — not at
//!   branch HEAD, and not detached (branch-model.md §5, `fetch` (absent
//!   clone): R1's birth-target rule)
//! - present member ALREADY at the locked SHA is not moved (mtime/HEAD
//!   unchanged) — which is not the same as "present members are untouched":
//!   a present member whose HEAD differs from the pin IS realigned
//! - realignment of a present member is a MOVE of the tracking branch's local
//!   counterpart, not a detach: it fast-forwards, refuses a non-fast-forward,
//!   and refuses outright when the checkout is on some other branch (§5.3)
//! - `--detach-checkouts` is the named exit from both refusals; `--frozen` is
//!   not a waiver
//! - an already-detached member stays detached (a MOVE), but refuses while
//!   the repo is mid-operation (§3.6)
//! - missing repo with no lock entry → default branch + additive lock write
//! - `--repo` filter limits materialization
//! - end-to-end DanglingReference: doctor reports → fetch → doctor clean
//! - usage error when no SOURCE AND no workspace
//! - `--allow-non-empty-dir` without SOURCE is rejected (bootstrap-only flag)

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn git_run(args: &[&str], cwd: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(cwd)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git command failed to start");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        cwd.display()
    );
}

fn git_capture(args: &[&str], cwd: &Path) -> String {
    let out = common::git()
        .args(args)
        .current_dir(cwd)
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::null())
        .output()
        .expect("git command failed to spawn");
    assert!(
        out.status.success(),
        "git {:?} in {} failed",
        args,
        cwd.display()
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
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

    git_run(
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
    git_run(&["config", "user.email", "test@test.com"], &work);
    git_run(&["config", "user.name", "Test"], &work);
    std::fs::write(work.join("README"), "initial\n").unwrap();
    git_run(&["add", "."], &work);
    git_run(&["commit", "-m", "initial"], &work);
    let first_sha = git_capture(&["rev-parse", "HEAD"], &work);
    std::fs::write(work.join("README"), "second\n").unwrap();
    git_run(&["add", "."], &work);
    git_run(&["commit", "-m", "second"], &work);
    let second_sha = git_capture(&["rev-parse", "HEAD"], &work);
    git_run(&["push", "origin", "main"], &work);
    (first_sha, second_sha)
}

/// Set up a workspace with an active project. Returns the workspace root.
/// The project has two manifest entries; both bare repos are pre-materialized
/// on the caller (via clone from bare). The rwv.lock pins each repo to its
/// FIRST commit; the bare repo's HEAD is at the SECOND commit — so aligning
/// to the lock (fetch) vs branch HEAD (update) is observably different.
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

    let mut manifest = String::from("repositories:\n");
    for (rp, bare, _, _) in &repos {
        let url = format!("file://{}", bare.display());
        manifest.push_str(&format!(
            "  {rp}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &manifest).unwrap();

    // Write a lock that pins each repo to its FIRST commit.
    let mut lock = String::from("repositories:\n");
    for (rp, bare, first, _) in &repos {
        let url = format!("file://{}", bare.display());
        lock.push_str(&format!(
            "  {rp}:\n    type: git\n    url: {url}\n    version: {first}\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), &lock).unwrap();

    // Activate the project.
    std::fs::write(workspace.join(".rwv-active"), "my-app\n").unwrap();

    Setup {
        workspace,
        repos,
        _tmp: tmp,
    }
}

/// Materialize a manifest repo on disk at the locked SHA (mimics what a
/// previous fetch would have done).
fn materialize_repo_at(workspace: &Path, repo_path: &str, bare: &Path, sha: &str) {
    let dest = workspace.join(repo_path);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    git_run(
        &["clone", &bare.to_string_lossy(), &dest.to_string_lossy()],
        workspace.parent().unwrap_or(workspace),
    );
    git_run(&["checkout", sha], &dest);
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
    git_run(
        &["clone", &bare.to_string_lossy(), &dest.to_string_lossy()],
        workspace.parent().unwrap_or(workspace),
    );
    git_run(&["config", "user.email", "test@test.com"], &dest);
    git_run(&["config", "user.name", "Test"], &dest);
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
    git_run(&["reset", "--hard", sha], &dest);
    dest
}

/// The ref this checkout is on, or `None` when HEAD is detached.
///
/// Delegates to the shared §4.7 primitive, which asks `Vcs::head_attachment`
/// — the production classifier — rather than `git symbolic-ref --short HEAD`.
/// `--short` returns the shortest *unambiguous* name, so a same-named tag
/// makes it answer `heads/main`; and its failure exit collapses detached,
/// unborn, and not-a-repo into one `None` (§4.5).
fn current_branch(repo: &Path) -> Option<String> {
    common::checkout_ref(repo)
}

// ============================================================================
// In-place mode: missing member re-cloned at the LOCKED SHA
// ============================================================================

#[test]
fn in_place_fetch_materializes_missing_member_at_locked_sha() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo, _bare, first_sha, _second_sha) = &s.repos[0];

    // No repo directory yet — this is the dangling-reference state.
    let dest = s.workspace.join(repo);
    assert!(!dest.exists(), "precondition: repo dir must not exist");

    // Run in-place fetch (no SOURCE).
    rwv()
        .arg("fetch")
        .current_dir(&s.workspace)
        .assert()
        .success();

    // Clone materialized...
    assert!(
        dest.exists(),
        "in-place fetch must materialize the missing clone"
    );
    assert!(dest.join(".git").exists(), "must be a real git clone");

    // ... AT THE LOCKED SHA (not branch HEAD). This is the load-bearing
    // assertion: `rwv fetch` aligns to the lock, unlike `rwv update` which
    // would advance to the second commit.
    let head = git_capture(&["rev-parse", "HEAD"], &dest);
    assert_eq!(
        head, *first_sha,
        "materialized clone must be at LOCKED SHA (first_sha), not branch HEAD"
    );

    // ... and ATTACHED to the tracking branch's local counterpart, with the
    // branch itself at the pin. R1's birth-target rule (§5, `fetch` (absent
    // clone)): the birth happens AT the lock revision, so bootstrapping a
    // weave from a lock behind origin performs no MOVE and needs no consent.
    // A clone that landed on origin's tip and was then aligned would answer
    // `None` here — which is the state that made "detached" the normal
    // resting state of a freshly-fetched weave (§6 item 2).
    assert_eq!(
        current_branch(&dest).as_deref(),
        Some("main"),
        "the clone must be born attached to the tracking counterpart, not detached at the pin"
    );
    assert_eq!(
        git_capture(&["rev-parse", "main"], &dest),
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
fn in_place_fetch_leaves_present_member_at_locked_sha_unmoved() {
    let s = setup_workspace_with_locked_project(&["github/acme/a", "github/acme/b"]);
    let (repo_a, bare_a, first_a, _) = &s.repos[0];
    let (_repo_b, _bare_b, _, _) = &s.repos[1];

    // Pre-materialize repo_a at first_sha (matches the lock).
    materialize_repo_at(&s.workspace, repo_a, bare_a, first_a);
    let dest_a = s.workspace.join(repo_a);
    let head_before = git_capture(&["rev-parse", "HEAD"], &dest_a);
    // Take a directory-inode marker: record the .git/config mtime.
    let git_config_before = std::fs::metadata(dest_a.join(".git/config"))
        .unwrap()
        .modified()
        .unwrap();

    // Now run in-place fetch — it should re-materialize repo_b but leave
    // repo_a alone.
    rwv()
        .arg("fetch")
        .current_dir(&s.workspace)
        .assert()
        .success();

    let head_after = git_capture(&["rev-parse", "HEAD"], &dest_a);
    assert_eq!(
        head_before, head_after,
        "present member's HEAD must be unchanged by in-place fetch"
    );
    let git_config_after = std::fs::metadata(dest_a.join(".git/config"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        git_config_before, git_config_after,
        "present member's .git/config must not be touched by in-place fetch"
    );
}

// ============================================================================
// Present member whose HEAD differs from the pin IS realigned — as a MOVE of
// the tracking branch's local counterpart (§5, `fetch` (present clone)). The
// checkout stays on the branch; the branch advances under it.
// ============================================================================

#[test]
fn in_place_fetch_fast_forwards_the_counterpart_and_stays_attached() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, _second_a) = &s.repos[0];

    // Present, clean, on main at the FIRST commit; the lock pins the SECOND,
    // so the pin is a strict descendant of the branch tip.
    let dest_a = materialize_repo_on_branch_at(&s.workspace, repo_a, bare_a, first_a);
    // The pin has to be the second commit for this to be a fast-forward; the
    // shared fixture locks the first, so rewrite the lock here.
    let lock_path = s.workspace.join("projects/my-app/rwv.lock");
    let lock = std::fs::read_to_string(&lock_path).unwrap();
    let second_a = s.repos[0].3.clone();
    std::fs::write(&lock_path, lock.replace(first_a, &second_a)).unwrap();

    rwv()
        .arg("fetch")
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(
        git_capture(&["rev-parse", "HEAD"], &dest_a),
        second_a,
        "present member must be realigned to the locked SHA"
    );
    assert_eq!(
        current_branch(&dest_a).as_deref(),
        Some("main"),
        "realignment is a MOVE of the counterpart, so the checkout stays on it"
    );
    assert_eq!(
        git_capture(&["rev-parse", "main"], &dest_a),
        second_a,
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
fn in_place_fetch_refuses_to_rewind_the_counterpart_even_when_the_member_is_clean() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, second_a) = &s.repos[0];

    // Clean, on a published branch, nothing in flight — and the lock pins an
    // ANCESTOR of the branch tip. Taking the pin would either rewind `main`
    // (a MOVE needing a discard warrant) or leave `main` behind (a detach
    // needing consent). rwv holds neither, so it refuses.
    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    assert_eq!(current_branch(&dest_a).as_deref(), Some("main"));

    rwv()
        .arg("fetch")
        .current_dir(&s.workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a fast-forward"))
        .stderr(predicate::str::contains("--detach-checkouts"));

    assert_eq!(
        current_branch(&dest_a).as_deref(),
        Some("main"),
        "the refusal must leave the member on its branch"
    );
    assert_eq!(git_capture(&["rev-parse", "HEAD"], &dest_a), *second_a);
    assert_ne!(git_capture(&["rev-parse", "HEAD"], &dest_a), *first_a);
}

#[test]
fn in_place_fetch_refuses_to_rewind_a_counterpart_carrying_uncommitted_changes() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, second_a) = &s.repos[0];

    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    // Dirt in a path the checkout does NOT touch, so git itself would happily
    // carry it onto a detached HEAD. The refusal has to be rwv's.
    std::fs::write(dest_a.join("scratch.txt"), "work in progress\n").unwrap();

    rwv()
        .arg("fetch")
        .current_dir(&s.workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a fast-forward"))
        .stderr(predicate::str::contains("--detach-checkouts"));

    assert_eq!(
        current_branch(&dest_a).as_deref(),
        Some("main"),
        "the refusal must leave the member on its branch"
    );
    assert_eq!(git_capture(&["rev-parse", "HEAD"], &dest_a), *second_a);
    assert_ne!(git_capture(&["rev-parse", "HEAD"], &dest_a), *first_a);
    assert!(
        dest_a.join("scratch.txt").exists(),
        "a refusal touches nothing, including the working tree"
    );
}

#[test]
fn in_place_fetch_refuses_when_the_branch_carries_commits_origin_does_not_have() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, _first_a, _second_a) = &s.repos[0];

    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    std::fs::write(dest_a.join("README"), "third\n").unwrap();
    git_run(&["commit", "-am", "third"], &dest_a);
    let unpushed = git_capture(&["rev-parse", "HEAD"], &dest_a);

    // A branch ahead of the pin can never fast-forward TO the pin, so the
    // unpublished commit is protected by the same predicate that protects a
    // clean rewind — the guard no longer has to ask origin what it has.
    rwv()
        .arg("fetch")
        .current_dir(&s.workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a fast-forward"));

    assert_eq!(current_branch(&dest_a).as_deref(), Some("main"));
    assert_eq!(
        git_capture(&["rev-parse", "HEAD"], &dest_a),
        unpushed,
        "the commit origin has never seen is still checked out"
    );
    assert_eq!(git_capture(&["rev-parse", "main"], &dest_a), unpushed);
}

#[test]
fn in_place_fetch_refuses_when_attached_to_a_branch_the_manifest_does_not_declare() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, _second_a) = &s.repos[0];

    // On a personal branch positioned so that taking the pin WOULD be a
    // fast-forward. The refusal therefore cannot be the fast-forward check:
    // it is §5.3's version-relatedness guard, which refuses to relate an
    // operator's bookmark to the lock at all. Advancing it would strand no
    // commits and still silently change what the bookmark means (§8.3).
    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    git_run(&["checkout", "-b", "feature", first_a], &dest_a);
    let lock_path = s.workspace.join("projects/my-app/rwv.lock");
    let lock = std::fs::read_to_string(&lock_path).unwrap();
    let second_a = s.repos[0].3.clone();
    std::fs::write(&lock_path, lock.replace(first_a, &second_a)).unwrap();

    rwv()
        .arg("fetch")
        .current_dir(&s.workspace)
        .assert()
        .failure()
        // Names BOTH refs: the one it is on and the one it expected.
        .stderr(predicate::str::contains("is on branch 'feature'"))
        .stderr(predicate::str::contains("local counterpart ('main')"))
        .stderr(predicate::str::contains("--detach-checkouts"))
        .stderr(predicate::str::contains("is not a fast-forward").not());

    assert_eq!(current_branch(&dest_a).as_deref(), Some("feature"));
    assert_eq!(git_capture(&["rev-parse", "HEAD"], &dest_a), *first_a);
}

#[test]
fn in_place_fetch_detach_checkouts_waives_the_refusal() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, _second_a) = &s.repos[0];

    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    std::fs::write(dest_a.join("README"), "third\n").unwrap();
    git_run(&["commit", "-am", "third"], &dest_a);
    let unpushed = git_capture(&["rev-parse", "HEAD"], &dest_a);
    std::fs::write(dest_a.join("scratch.txt"), "work in progress\n").unwrap();

    rwv()
        .args(["fetch", "--detach-checkouts"])
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(git_capture(&["rev-parse", "HEAD"], &dest_a), *first_a);
    assert_eq!(current_branch(&dest_a), None);
    // The waiver is not a discard: the unpushed commits are still on the
    // branch ref, and the uncommitted file came along.
    assert_eq!(git_capture(&["rev-parse", "main"], &dest_a), unpushed);
    assert!(dest_a.join("scratch.txt").exists());
}

#[test]
fn in_place_fetch_detach_checkouts_waives_the_relatedness_refusal_too() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, second_a) = &s.repos[0];

    // The other refusal §5.3 defines. The consent detaches; it does not
    // relocate the personal branch, which is the whole point of naming the
    // flag after the consequence rather than after the precondition.
    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    git_run(&["checkout", "-b", "feature"], &dest_a);

    rwv()
        .args(["fetch", "--detach-checkouts"])
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(git_capture(&["rev-parse", "HEAD"], &dest_a), *first_a);
    assert_eq!(current_branch(&dest_a), None);
    assert_eq!(
        git_capture(&["rev-parse", "feature"], &dest_a),
        *second_a,
        "the personal branch is left exactly where it was"
    );
}

#[test]
fn in_place_fetch_frozen_does_not_waive_the_refusal() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, _first_a, second_a) = &s.repos[0];

    let dest_a = materialize_repo_on_branch(&s.workspace, repo_a, bare_a);
    std::fs::write(dest_a.join("scratch.txt"), "work in progress\n").unwrap();

    // --frozen is a lock-validation mode, not a waiver.
    rwv()
        .args(["fetch", "--frozen"])
        .current_dir(&s.workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a fast-forward"));

    assert_eq!(git_capture(&["rev-parse", "HEAD"], &dest_a), *second_a);
    assert_eq!(current_branch(&dest_a).as_deref(), Some("main"));
}

// ============================================================================
// An already-detached member stays detached: HEAD moves, its symbolic-ness
// does not, so this is a MOVE and needs no consent — but §3.6's
// mid-operation precondition applies to it.
// ============================================================================

#[test]
fn in_place_fetch_moves_an_already_detached_member_without_consent() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, second_a) = &s.repos[0];

    // Detached at the second commit; the lock pins the first. Nothing is
    // attached, so nothing can be abandoned — no flag required, and the
    // rewind-vs-fast-forward question does not arise.
    materialize_repo_at(&s.workspace, repo_a, bare_a, second_a);
    let dest_a = s.workspace.join(repo_a);
    assert_eq!(current_branch(&dest_a), None, "precondition: detached");

    rwv()
        .arg("fetch")
        .current_dir(&s.workspace)
        .assert()
        .success();

    assert_eq!(git_capture(&["rev-parse", "HEAD"], &dest_a), *first_a);
    assert_eq!(
        current_branch(&dest_a),
        None,
        "a detached member stays detached — fetch does not reattach it"
    );
}

#[test]
fn in_place_fetch_refuses_to_move_a_detached_member_that_is_mid_operation() {
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo_a, bare_a, first_a, second_a) = &s.repos[0];

    materialize_repo_at(&s.workspace, repo_a, bare_a, second_a);
    let dest_a = s.workspace.join(repo_a);
    // `Detached` collapses "rwv left this at a lock SHA" and "the operator is
    // mid-bisect". Only the first is rwv's to move (§3.6).
    git_run(&["bisect", "start"], &dest_a);

    rwv()
        .arg("fetch")
        .current_dir(&s.workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("mid-bisect"));

    assert_eq!(
        git_capture(&["rev-parse", "HEAD"], &dest_a),
        *second_a,
        "the bisect's HEAD must not be yanked out from under it"
    );
    assert_ne!(git_capture(&["rev-parse", "HEAD"], &dest_a), *first_a);
}

// ============================================================================
// Missing member with NO lock entry → clone at default branch HEAD + message
// ============================================================================

#[test]
fn in_place_fetch_missing_repo_no_lock_entry_clones_at_default_branch() {
    // Set up a project where the manifest has TWO repos but the lock only
    // covers one. The missing repo is the one NOT in the lock — it must
    // clone at default branch HEAD, and the message must say so.
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let bare_a = tmp.path().join("a.git");
    let bare_b = tmp.path().join("b.git");
    let (first_a, _) = init_bare_repo_with_two_commits(&bare_a);
    let (_, second_b) = init_bare_repo_with_two_commits(&bare_b);

    let project_dir = workspace.join("projects").join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let url_a = format!("file://{}", bare_a.display());
    let url_b = format!("file://{}", bare_b.display());
    let manifest = format!(
        "repositories:\n  \
         github/acme/a:\n    type: git\n    url: {url_a}\n    version: main\n    role: owned\n  \
         github/acme/b:\n    type: git\n    url: {url_b}\n    version: main\n    role: owned\n"
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    // Lock only covers repo_a (pinned to first_a).
    let lock = format!(
        "repositories:\n  \
         github/acme/a:\n    type: git\n    url: {url_a}\n    version: {first_a}\n"
    );
    std::fs::write(project_dir.join("rwv.lock"), lock).unwrap();

    std::fs::write(workspace.join(".rwv-active"), "my-app\n").unwrap();

    // Both repos are missing on disk. Run in-place fetch.
    let output = rwv()
        .arg("fetch")
        .current_dir(&workspace)
        .output()
        .expect("failed to spawn rwv fetch");
    assert!(
        output.status.success(),
        "in-place fetch should succeed even with an additive lock entry; stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dest_a = workspace.join("github/acme/a");
    let dest_b = workspace.join("github/acme/b");
    assert!(dest_a.exists(), "repo_a must be materialized");
    assert!(dest_b.exists(), "repo_b must be materialized");

    // repo_a is at first_a (locked); repo_b is at second_b (default branch HEAD).
    let head_a = git_capture(&["rev-parse", "HEAD"], &dest_a);
    let head_b = git_capture(&["rev-parse", "HEAD"], &dest_b);
    assert_eq!(head_a, first_a, "repo_a must be at LOCKED SHA");
    assert_eq!(
        head_b, second_b,
        "repo_b (no lock entry) must be at branch HEAD"
    );

    // Message must say the additive-lock branch was taken for repo_b. The
    // `emit` for the additive path lands on stderr in JSON mode, stdout
    // otherwise; combine to check both. Under -j > 1 the line carries a
    // `[<repo>]` prefix; text-search is fine either way.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("additive")
            || combined.contains("branch HEAD")
            || combined.contains("adding"),
        "message must indicate repo_b was added at branch HEAD (additive); got:\n{combined}"
    );

    // Lock file should now include repo_b at second_b (additive write).
    let lock_content = std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap();
    assert!(
        lock_content.contains("github/acme/b"),
        "lock must have grown to include repo_b; got:\n{lock_content}"
    );
    assert!(
        lock_content.contains(&second_b),
        "lock must record repo_b at second_b (branch HEAD); got:\n{lock_content}"
    );
}

// ============================================================================
// --repo filter limits materialization
// ============================================================================

#[test]
fn in_place_fetch_repo_filter_limits_materialization() {
    let s = setup_workspace_with_locked_project(&["github/acme/a", "github/acme/b"]);
    let (repo_a, _, first_a, _) = &s.repos[0];
    let (repo_b, _, _, _) = &s.repos[1];

    // Both repos missing. Filter to only repo_a.
    rwv()
        .args(["fetch", "--repo", repo_a])
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
    let head_a = git_capture(&["rev-parse", "HEAD"], &dest_a);
    assert_eq!(head_a, *first_a, "materialized clone must be at LOCKED SHA");
}

// ============================================================================
// End-to-end: doctor dangling → in-place fetch → doctor clean
// ============================================================================

#[test]
fn dangling_reference_end_to_end_fetch_repairs_and_doctor_clean() {
    // The audit round found text-only tests let dead advice through. This
    // test exercises the FULL loop: create a dangling reference, doctor
    // reports it, run the named repair verb (`rwv fetch`), doctor now clean.
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
        combined1.contains("rwv fetch"),
        "doctor's dangling-reference message must name `rwv fetch` as the repair verb; \
         got:\n{combined1}"
    );

    // 2. Run the named repair verb: `rwv fetch` (no SOURCE).
    rwv()
        .arg("fetch")
        .current_dir(&s.workspace)
        .assert()
        .success();

    // 3. Repo now exists at locked SHA.
    let dest = s.workspace.join(repo);
    assert!(dest.exists(), "fetch must materialize the dangling repo");
    let head = git_capture(&["rev-parse", "HEAD"], &dest);
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
        "doctor must be clean after fetch repair; got:\n{combined2}"
    );
    assert!(
        !combined2.contains("dangling"),
        "doctor must no longer report a dangling reference; got:\n{combined2}"
    );
}

// ============================================================================
// Usage error: no SOURCE AND no workspace
// ============================================================================

#[test]
fn in_place_fetch_outside_workspace_names_both_forms() {
    // Empty tempdir → no workspace above; no SOURCE arg either.
    // The error must mention SOURCE (naming the bootstrap form) and workspace
    // (naming the in-place form) so the user sees both viable exits.
    let tmp = common::tempdir().unwrap();
    rwv()
        .arg("fetch")
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("no SOURCE").and(predicate::str::contains("workspace")));
}

// ============================================================================
// --allow-non-empty-dir is a bootstrap knob; rejected when no SOURCE
// ============================================================================

#[test]
fn in_place_fetch_allow_non_empty_dir_without_source_is_rejected() {
    // --allow-non-empty-dir is only meaningful in bootstrap mode. Passing it
    // with no SOURCE would silently pretend to matter; keep the UX honest by
    // rejecting it.
    let tmp = common::tempdir().unwrap();
    rwv()
        .args(["fetch", "--allow-non-empty-dir"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("--allow-non-empty-dir"));
}

// ============================================================================
// Workweave invocation: canonical clone lands at PRIMARY, not workweave
// ============================================================================

#[test]
fn in_place_fetch_from_workweave_materializes_at_primary() {
    // Clone-topology I1: the canonical store per manifest repo lives at
    // primary's `<weave>/<repo_path>`. Invoking `rwv fetch` from a workweave
    // must materialize the missing member at PRIMARY's canonical path, not
    // at the workweave's slot. See docs/explanation/joints/clone-topology.md.
    let s = setup_workspace_with_locked_project(&["github/acme/a"]);
    let (repo, _, first_sha, _) = &s.repos[0];

    // Set up a workweave marker pointing at primary.
    let workweave_dir = s.workspace.join(".workweaves/my-app--dev");
    std::fs::create_dir_all(&workweave_dir).unwrap();
    // A projects/<name>/rwv.yaml + rwv.lock is per-workspace state; the
    // workweave gets its own copy (mirrors how rwv workweave create/rwv
    // activate populate this). Copy the primary's project files into the
    // workweave so `require_active_project_on_disk` finds them.
    std::fs::create_dir_all(workweave_dir.join("projects/my-app")).unwrap();
    let primary_project = s.workspace.join("projects/my-app");
    std::fs::copy(
        primary_project.join("rwv.yaml"),
        workweave_dir.join("projects/my-app/rwv.yaml"),
    )
    .unwrap();
    std::fs::copy(
        primary_project.join("rwv.lock"),
        workweave_dir.join("projects/my-app/rwv.lock"),
    )
    .unwrap();

    // Write the workweave marker. Format: JSON with `primary`, `project`, `parent`.
    let marker_yaml = format!(
        "{{\"primary\":\"{}\",\"project\":\"my-app\",\"parent\":\"{}\"}}",
        s.workspace.display(),
        s.workspace.display(),
    );
    std::fs::write(workweave_dir.join(".rwv-workweave"), marker_yaml).unwrap();

    // Run in-place fetch from INSIDE the workweave.
    let output = rwv()
        .arg("fetch")
        .current_dir(&workweave_dir)
        .output()
        .expect("fetch spawn failed");
    // We may not care whether this ends up clean end-to-end for the workweave
    // side (worktree add is sync's job), but the canonical materialization at
    // primary IS the load-bearing assertion.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "fetch from workweave should succeed (canonical materializes at primary); \
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
    let head = git_capture(&["rev-parse", "HEAD"], &primary_dest);
    assert_eq!(
        head, *first_sha,
        "primary canonical clone must be at LOCKED SHA"
    );

    // The workweave's slot should NOT have gotten its own separate clone
    // (that would be a clone-topology I1 violation — two DAGs for one repo).
    let workweave_dest = workweave_dir.join(repo);
    assert!(
        !workweave_dest.exists(),
        "workweave slot must NOT have received a separate clone (I1 violation); \
         `rwv sync` adds worktrees, not fetch"
    );
}
