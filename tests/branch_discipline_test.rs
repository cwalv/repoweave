//! Tests for branch-discipline checks (`rwv doctor`, fo-hycb06.2).
//!
//! Enforces the I3 invariant from `docs/explanation/joints/clone-topology.md`
//! (every workweave repo checkout sits on its owned
//! `<project>--<workweave>/<segment>` ephemeral branch; canonicals sit on a
//! non-ephemeral branch) plus the safe/live doctrine from
//! `docs/explanation/joints/shared-refs-drift.md` applied to refs in (c).
//!
//! Three checks, five sub-kinds:
//!
//!   (a) workweave-branch — `shared-branch`, `foreign-ephemeral`, `detached`
//!   (b) ephemeral-at-primary
//!   (c) stale-ephemeral-branches — `safe` (auto-fixable) / `live` (never)
//!
//! Healthy fixtures (workweave on its own ephemeral branch, canonical on
//! `main`, ephemeral branch whose workweave still exists) must stay clean.
//!
//! Fixture rationale: branch-discipline operates on real git repos, so the
//! workspaces here include actual git checkouts (not just directory shells
//! like the tree-integrity tests).

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

// ---------------------------------------------------------------------------
// Helpers: build a primary + workweave with real git checkouts on demand.
// ---------------------------------------------------------------------------

/// Create a minimal primary workspace with a `github/` registry dir and a
/// `projects/` directory. Returns the workspace root.
fn make_primary(parent: &Path) -> PathBuf {
    let ws = parent.join("ws");
    std::fs::create_dir_all(ws.join("github")).unwrap();
    std::fs::create_dir_all(ws.join("projects")).unwrap();
    ws
}

/// Return the `.workweaves/` parent directory for `ws_root`.
fn workweaves_dir(ws_root: &Path) -> PathBuf {
    ws_root
        .parent()
        .expect("ws_root has a parent")
        .join(".workweaves")
}

/// Write a well-formed `.rwv-workweave` marker file into `ww_dir`.
fn write_marker(ww_dir: &Path, primary: &Path, project: &str, parent: &Path) {
    std::fs::create_dir_all(ww_dir).unwrap();
    let primary_str = primary
        .canonicalize()
        .unwrap_or_else(|_| primary.to_path_buf());
    let parent_str = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    let content = format!(
        "primary: {}\nproject: {}\nparent: {}\n",
        primary_str.display(),
        project,
        parent_str.display()
    );
    std::fs::write(ww_dir.join(".rwv-workweave"), content).unwrap();
}

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn git() -> Command {
    common::git()
}

/// Run a git subcommand in `cwd` and assert success. Strip inherited `GIT_*`
/// env (see `tests/common/mod.rs` for context).
fn git_in(cwd: &Path, args: &[&str]) {
    let out = git()
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?} in {} failed to spawn: {e}", cwd.display()));
    assert!(
        out.status.success(),
        "git {args:?} in {} failed: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Initialize a git repo at `path` with a single commit on `main`. Returns
/// the path so the caller can chain.
fn init_repo_with_commit(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).unwrap();
    git_in(path, &["init", "--initial-branch=main", "-q"]);
    git_in(path, &["config", "user.email", "test@test"]);
    git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git_in(path, &["add", "README.md"]);
    git_in(path, &["commit", "-q", "-m", "init"]);
    path.to_path_buf()
}

/// Add a worktree from canonical `repo` at `worktree_path`, on a new branch
/// `branch_name` starting from the canonical's current HEAD. Returns the
/// worktree path.
fn worktree_add(repo: &Path, worktree_path: &Path, branch_name: &str) -> PathBuf {
    git_in(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch_name,
            worktree_path.to_str().unwrap(),
        ],
    );
    worktree_path.to_path_buf()
}

/// Add a worktree from canonical `repo` at `worktree_path`, on the *existing*
/// branch `branch_name` (no `-b`). Used to fixture shared-branch and
/// foreign-ephemeral cases.
fn worktree_add_existing(repo: &Path, worktree_path: &Path, branch_name: &str) {
    git_in(
        repo,
        &[
            "worktree",
            "add",
            worktree_path.to_str().unwrap(),
            branch_name,
        ],
    );
}

/// Add a detached worktree from canonical `repo` at `worktree_path` pointing
/// at HEAD. Used to fixture the detached case.
fn worktree_add_detached(repo: &Path, worktree_path: &Path) {
    git_in(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            worktree_path.to_str().unwrap(),
            "HEAD",
        ],
    );
}

/// Append a commit on the currently checked-out branch in `repo`.
fn add_commit(repo: &Path, fname: &str, msg: &str) {
    std::fs::write(repo.join(fname), format!("{msg}\n")).unwrap();
    git_in(repo, &["add", fname]);
    git_in(repo, &["commit", "-q", "-m", msg]);
}

/// Create a fresh local branch in `repo` pointing at `start_point` (a SHA
/// or another branch name) without switching to it.
fn create_branch(repo: &Path, name: &str, start_point: &str) {
    git_in(repo, &["branch", name, start_point]);
}

// ===========================================================================
// (a) workweave-branch
// ===========================================================================

/// Healthy workweave: each repo checkout sits on its
/// `<project>--<workweave>/<segment>` ephemeral branch. Doctor should not
/// report any branch-discipline finding for this directory.
#[test]
fn healthy_workweave_ephemeral_branch_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    worktree_add(&canonical, &ww_checkout, "myproj--feat-a/main");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("workweave checkout is on")
            && !stdout.contains("detached-HEAD")
            && !stdout.contains("foreign-ephemeral"),
        "healthy workweave on its ephemeral branch should be clean; got:\n{stdout}"
    );
}

/// shared-branch sub-kind: workweave repo checkout on `main` (the canonical's
/// tracking branch). This is the bare-main-in-workweave case from the
/// acceptance criteria — must flag from creation, before any commit lands.
#[test]
fn shared_branch_main_in_workweave_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Move the canonical off `main` so the workweave can check it out.
    git_in(&canonical, &["checkout", "-b", "rwv-primary-tip", "-q"]);

    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    // The bead's bare-main case: workweave checkout sits on `main`, no
    // commits beyond the canonical's first commit.
    worktree_add_existing(&canonical, &ww_checkout, "main");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("shared-branch")
            || stdout.contains("workweave checkout is on shared-branch"),
        "doctor should report shared-branch sub-kind for bare-main-in-workweave; got:\n{stdout}"
    );
    assert!(
        stdout.contains("main"),
        "report should name the offending branch (main); got:\n{stdout}"
    );
}

/// foreign-ephemeral sub-kind: workweave checkout on `<project>--<other>/...`,
/// naming a different workweave's branch.
#[test]
fn foreign_ephemeral_branch_in_workweave_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    // Check out on a foreign workweave's ephemeral branch.
    worktree_add(&canonical, &ww_checkout, "myproj--feat-b/main");

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("foreign-ephemeral") || stdout.contains("names a different workweave"),
        "doctor should report foreign-ephemeral sub-kind; got:\n{stdout}"
    );
    assert!(
        stdout.contains("myproj--feat-b/main"),
        "report should name the offending branch; got:\n{stdout}"
    );
}

/// detached sub-kind: workweave checkout in detached-HEAD state.
#[test]
fn detached_head_in_workweave_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);
    let ww_checkout = ww_dir.join("github").join("acme").join("repo");
    std::fs::create_dir_all(ww_checkout.parent().unwrap()).unwrap();
    worktree_add_detached(&canonical, &ww_checkout);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("detached-HEAD") || stdout.contains("detached"),
        "doctor should report detached-HEAD sub-kind; got:\n{stdout}"
    );
}

// ===========================================================================
// (b) ephemeral-at-primary
// ===========================================================================

/// Healthy canonical: checked out on a non-ephemeral branch (`main`).
/// No branch-discipline finding expected.
#[test]
fn healthy_canonical_on_main_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("canonical clone is checked out on ephemeral"),
        "canonical on main should not be flagged as ephemeral-at-primary; got:\n{stdout}"
    );
}

/// ephemeral-at-primary: canonical checked out on a `<project>--<name>/...`
/// branch — the inverse of (a).
#[test]
fn ephemeral_at_primary_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Switch the canonical onto an ephemeral-named branch. (The workweave
    // directory may or may not exist; the violation is about the canonical
    // holding the branch.)
    git_in(&canonical, &["checkout", "-b", "myproj--feat-a/main", "-q"]);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("canonical clone is checked out on ephemeral")
            || stdout.contains("ephemeral-at-primary"),
        "doctor should report ephemeral-at-primary; got:\n{stdout}"
    );
    assert!(
        stdout.contains("myproj--feat-a/main"),
        "report should name the offending branch; got:\n{stdout}"
    );
}

// ===========================================================================
// (c) stale-ephemeral-branches: safe class
// ===========================================================================

/// Safe-class fixture: the stale ephemeral branch's tip is an ancestor of
/// the canonical's primary tip — no unique commits, safe to delete.
/// Doctor should report it; `--fix` should delete it; a follow-up doctor
/// run should be clean (idempotency).
#[test]
fn stale_ephemeral_branch_safe_is_reported_and_fixable() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Stale ephemeral branch pointing at the same commit as `main` — its
    // tip is trivially an ancestor of `main`'s tip.
    create_branch(&canonical, "myproj--dead/main", "main");

    // Advance main so it strictly dominates the stale branch (still
    // trivially safe — stale branch tip is_ancestor of main tip).
    add_commit(&canonical, "f2.txt", "second");

    // No workweave directory `.workweaves/myproj--dead/` exists.

    // First doctor run: report the safe-class violation.
    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("stale ephemeral branch") && stdout.contains("safe class"),
        "doctor should report safe-class stale ephemeral branch; got:\n{stdout}"
    );
    assert!(
        stdout.contains("myproj--dead/main"),
        "report should name the offending branch; got:\n{stdout}"
    );

    // Branch still exists pre-fix.
    let pre_fix = git()
        .args(["branch", "--list", "myproj--dead/main"])
        .current_dir(&canonical)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&pre_fix.stdout).is_empty(),
        "stale branch should exist before --fix"
    );

    // Apply --fix: doctor should delete the safe-class branch.
    let fix_out = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let fix_stdout = String::from_utf8_lossy(&fix_out.stdout);
    assert!(
        fix_stdout.contains("[fixed]") && fix_stdout.contains("myproj--dead/main"),
        "--fix should announce the delete; got:\n{fix_stdout}"
    );

    // Branch gone post-fix.
    let post_fix = git()
        .args(["branch", "--list", "myproj--dead/main"])
        .current_dir(&canonical)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&post_fix.stdout).trim().is_empty(),
        "stale branch should be deleted after --fix"
    );

    // Idempotency: a second --fix run finds nothing to fix and stays clean
    // of the safe-class warning.
    let again = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let again_stdout = String::from_utf8_lossy(&again.stdout);
    assert!(
        !again_stdout.contains("myproj--dead/main"),
        "second --fix run should be a no-op for the deleted branch; got:\n{again_stdout}"
    );
}

// ===========================================================================
// (c) stale-ephemeral-branches: live class
// ===========================================================================

/// Live-class fixture: the stale ephemeral branch carries commits not
/// reachable from the canonical's primary tip. Doctor reports it as
/// live-class; `--fix` must NOT delete it.
#[test]
fn stale_ephemeral_branch_live_is_reported_and_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Create the stale branch and add a unique commit on it (so it carries
    // work not reachable from main).
    git_in(&canonical, &["checkout", "-b", "myproj--dead/main", "-q"]);
    add_commit(&canonical, "unique.txt", "live work");
    git_in(&canonical, &["checkout", "main", "-q"]);

    // Advance main on a divergent path so the live branch's tip is
    // genuinely not an ancestor of main's tip.
    add_commit(&canonical, "mainwork.txt", "main work");

    // No workweave directory `.workweaves/myproj--dead/` exists.

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("stale ephemeral branch") && stdout.contains("live class"),
        "doctor should report live-class stale ephemeral branch; got:\n{stdout}"
    );
    assert!(
        stdout.contains("myproj--dead/main"),
        "report should name the offending branch; got:\n{stdout}"
    );

    // Branch exists pre-fix.
    let pre_fix = git()
        .args(["branch", "--list", "myproj--dead/main"])
        .current_dir(&canonical)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&pre_fix.stdout).is_empty(),
        "live branch should exist before --fix"
    );

    // Apply --fix. The live branch must survive untouched.
    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ws)
        .output()
        .unwrap();

    let post_fix = git()
        .args(["branch", "--list", "myproj--dead/main"])
        .current_dir(&canonical)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&post_fix.stdout).is_empty(),
        "live branch must NOT be deleted by --fix; got:\n{}",
        String::from_utf8_lossy(&post_fix.stdout)
    );
}

// ===========================================================================
// (c) Healthy: ephemeral branch whose workweave still exists is not flagged.
// ===========================================================================

/// An ephemeral branch in the canonical whose `<project>--<name>` workweave
/// directory still exists on disk is owned, not stale. Doctor must not
/// flag it.
#[test]
fn ephemeral_branch_with_existing_workweave_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Create the ephemeral branch in the canonical (just as a bare ref).
    create_branch(&canonical, "myproj--feat-a/main", "main");

    // And create the matching workweave directory with a marker.
    let ww_dir = workweaves_dir(&ws).join("myproj--feat-a");
    write_marker(&ww_dir, &ws, "myproj", &ws);

    let out = rwv().args(["doctor"]).current_dir(&ws).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        !stdout.contains("stale ephemeral branch"),
        "ephemeral branch with existing workweave dir must not be flagged as stale; got:\n{stdout}"
    );
}

// ===========================================================================
// JSON output exposes the branch-discipline kind.
// ===========================================================================

#[test]
fn json_output_includes_branch_discipline_kind() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_primary(tmp.path());
    let canonical = ws.join("github").join("acme").join("repo");
    init_repo_with_commit(&canonical);

    // Synthesize the simplest violation (ephemeral-at-primary by switching
    // the canonical onto an ephemeral-named branch).
    git_in(&canonical, &["checkout", "-b", "myproj--feat-a/main", "-q"]);

    let out = rwv()
        .args(["doctor", "--json"])
        .current_dir(&ws)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));

    let violations = json["violations"].as_array().expect("violations is array");
    let found = violations.iter().any(|v| v["kind"] == "branch-discipline");
    assert!(
        found,
        "doctor --json must include a branch-discipline violation; violations: {violations:?}"
    );
}
