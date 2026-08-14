//! Idempotency regression test for `rwv workweave <project> create <name>`.
//!
//! Gas City's pool-worker resume contract (`gc runtime request-restart`)
//! relies on re-invoking `rwv workweave` leaving non-git state in the
//! workweave untouched. If a future refactor makes workweave creation
//! destructive on the second call, `.runtime/`, `.claude/`, and similar
//! non-git scratch state get wiped — silently breaking session resume.
//!
//! This test locks the contract: create a workweave, drop sentinel files
//! into it (mimicking agent runtime state), re-invoke `rwv workweave ...
//! create ...`, and assert that the workweave directory, the sentinel
//! files, the `.rwv-workweave` marker, and the per-repo worktree branches
//! are all preserved.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

fn make_workspace(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"
"#,
        repo = common::url_path(&repo_path)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    ws
}

/// The ref this checkout is on. Panics when HEAD is detached — every caller
/// here is asserting an attachment, so a detach is a failure, not a value.
///
/// Delegates to the shared §4.7 primitive (`Vcs::head_attachment`) rather
/// than `git symbolic-ref --short HEAD`; see `tests/common/mod.rs`.
fn current_branch(dir: &Path) -> String {
    common::checkout_ref(dir).unwrap_or_else(|| {
        panic!(
            "{} should be on a branch but HEAD is detached",
            dir.display()
        )
    })
}

/// Re-invoking `rwv workweave PROJECT create NAME` on an already-created
/// workweave must not destroy non-git state inside the workweave.
///
/// Rationale: Gas City's `gc runtime request-restart` flow re-creates the
/// session inside the same workweave path. The pool-worker contract
/// assumes non-git files written by agents (sentinel state under
/// `.runtime/`, agent scratch under `.claude/`) survive a re-invocation
/// of the same `rwv workweave ... create ...` command.
#[test]
fn workweave_recreate_preserves_non_git_state() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // ---- First invocation: create the workweave fresh. ----
    rwv()
        .args(["workweave", "web-app", "create", "resume"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--resume");
    assert!(ww_dir.exists(), "workweave should exist after first create");

    // ---- Drop non-git state into the workweave. ----
    //
    // Mirrors what Gas City pool-workers actually write: a sentinel under
    // .runtime/ and an agent-scratch file under .claude/.
    let runtime_dir = ww_dir.join(".runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let sentinel_path = runtime_dir.join("sentinel.txt");
    let sentinel_content = "pool-worker session state\n";
    std::fs::write(&sentinel_path, sentinel_content).unwrap();

    let claude_dir = ww_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let claude_state_path = claude_dir.join("agent-scratch.txt");
    let claude_state_content = "claude agent ephemeral state\n";
    std::fs::write(&claude_state_path, claude_state_content).unwrap();

    // Snapshot the marker and the repo's worktree branch so we can
    // assert they are unchanged after re-invocation.
    let marker_path = ww_dir.join(".rwv-workweave");
    assert!(
        marker_path.exists(),
        ".rwv-workweave should exist after create"
    );
    let marker_before = std::fs::read_to_string(&marker_path).unwrap();

    let weave_repo = ww_dir.join("github/org/repo");
    let branch_before = current_branch(&weave_repo);
    assert_eq!(
        branch_before, "web-app--resume",
        "worktree should be on flat ephemeral branch web-app--resume before re-invocation"
    );

    // ---- Second invocation: re-create the same workweave. ----
    //
    // The assertion is that this succeeds AND leaves non-git state
    // intact. If this fails, it confirms the original premise: rwv
    // workweave create is not idempotent on re-invocation and needs a
    // fix to support the pool-worker resume contract.
    rwv()
        .args(["workweave", "web-app", "create", "resume"])
        .current_dir(&ws)
        .assert()
        .success();

    // ---- Assert: workweave directory still at the same path. ----
    assert!(
        ww_dir.exists(),
        "workweave directory should still exist after re-invocation at {}",
        ww_dir.display()
    );

    // ---- Assert: sentinel files survived unchanged. ----
    assert!(
        sentinel_path.exists(),
        ".runtime/sentinel.txt should survive re-invocation"
    );
    let sentinel_after = std::fs::read_to_string(&sentinel_path).unwrap();
    assert_eq!(
        sentinel_after, sentinel_content,
        ".runtime/sentinel.txt content should be unchanged after re-invocation"
    );

    assert!(
        claude_state_path.exists(),
        ".claude/agent-scratch.txt should survive re-invocation"
    );
    let claude_state_after = std::fs::read_to_string(&claude_state_path).unwrap();
    assert_eq!(
        claude_state_after, claude_state_content,
        ".claude/agent-scratch.txt content should be unchanged after re-invocation"
    );

    // ---- Assert: marker still points at the same primary + project. ----
    //
    // We compare the content directly: the marker is derived from the
    // workspace root and project name, both of which are unchanged, so
    // a byte-identical result is the strongest assertion we can make.
    assert!(
        marker_path.exists(),
        ".rwv-workweave marker should still exist"
    );
    let marker_after = std::fs::read_to_string(&marker_path).unwrap();
    assert_eq!(
        marker_after, marker_before,
        ".rwv-workweave marker should be unchanged after re-invocation"
    );

    // ---- Assert: worktree still on the same ephemeral branch. ----
    assert!(
        weave_repo.exists(),
        "per-repo worktree should still exist after re-invocation"
    );
    let branch_after = current_branch(&weave_repo);
    assert_eq!(
        branch_after, branch_before,
        "worktree ephemeral branch should be unchanged after re-invocation"
    );
}

/// Re-invoking `rwv workweave PROJECT create NAME` on a workweave that has
/// local modifications (uncommitted changes OR commits on the ephemeral
/// branch) must refuse without `--replace-existing`, preserving the user's work.
///
/// This protects against silent loss when a user has done work inside a
/// workweave and then accidentally (or a tool) re-issues the create
/// command — a failed idempotency check here would clobber that work.
#[test]
fn workweave_recreate_refuses_on_local_modifications() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // ---- Case A: uncommitted changes in the worktree. ----
    rwv()
        .args(["workweave", "web-app", "create", "dirty"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--dirty");
    let weave_repo = ww_dir.join("github/org/repo");
    let head_before_dirty = head_sha(&weave_repo);

    // Introduce an uncommitted change (new file).
    std::fs::write(weave_repo.join("scratch.txt"), "untracked edit\n").unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "dirty"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("uncommitted changes"));

    // Dirty file must still be there.
    assert!(
        weave_repo.join("scratch.txt").exists(),
        "scratch.txt should survive a refused re-invocation"
    );
    assert_eq!(
        head_sha(&weave_repo),
        head_before_dirty,
        "worktree HEAD should be unchanged after refused re-invocation"
    );

    // ---- Case B: a new commit on the ephemeral branch. ----
    rwv()
        .args(["workweave", "web-app", "create", "advanced"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww2_dir = weaveroot.join("web-app--advanced");
    let weave2_repo = ww2_dir.join("github/org/repo");

    std::fs::write(weave2_repo.join("new-file.txt"), "content\n").unwrap();
    git(&["add", "."], &weave2_repo);
    git(&["commit", "-m", "work in progress"], &weave2_repo);
    let advanced_head = head_sha(&weave2_repo);

    rwv()
        .args(["workweave", "web-app", "create", "advanced"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("diverged from source"));

    // Commit must still be there.
    assert_eq!(
        head_sha(&weave2_repo),
        advanced_head,
        "ephemeral-branch commit should survive a refused re-invocation"
    );
    assert!(
        weave2_repo.join("new-file.txt").exists(),
        "committed file should still be on disk"
    );
}

/// Workweaves with the same name across different projects coexist under
/// the `<project>--<name>` directory convention. The old layout used
/// `<primary>--<name>`, which made this scenario a collision; under the new
/// convention it's a simple peer-creation.
#[test]
fn workweave_same_name_different_projects_coexist() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "project-a");
    // Add a second project manifest pointing at the same repo.
    let project_b_dir = ws.join("projects/project-b");
    std::fs::create_dir_all(&project_b_dir).unwrap();
    let repo_path = ws.join("github/org/repo");
    let manifest_b = format!(
        r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"
"#,
        repo = common::url_path(&repo_path)
    );
    std::fs::write(project_b_dir.join("rwv.toml"), manifest_b).unwrap();

    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Create workweave "shared" for project-a.
    rwv()
        .args(["workweave", "project-a", "create", "shared"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_a = weaveroot.join("project-a--shared");
    assert!(ww_a.exists());
    let marker_a_before = std::fs::read_to_string(ww_a.join(".rwv-workweave")).unwrap();

    // Create the same-named workweave for project-b — must succeed in its
    // own directory, leaving project-a's untouched.
    rwv()
        .args(["workweave", "project-b", "create", "shared"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_b = weaveroot.join("project-b--shared");
    assert!(ww_b.exists(), "project-b's workweave should exist");

    // Project-a's marker is unchanged: peer coexistence, no overwrite.
    let marker_a_after = std::fs::read_to_string(ww_a.join(".rwv-workweave")).unwrap();
    assert_eq!(
        marker_a_after, marker_a_before,
        ".rwv-workweave marker for project-a should be untouched"
    );
}

/// `--replace-existing` recreates the workweave, but only when nothing unsaved
/// or unmerged would be lost: a dirty workweave is refused (the operator
/// never saw what it holds), and once clean, the replace destroys non-git
/// state and rebuilds. This is the explicit rebuild path (corruption
/// recovery, reusing a slot for a new purpose).
#[test]
fn workweave_recreate_with_replace_existing_destroys_and_recreates() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "reset"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--reset");
    let weave_repo = ww_dir.join("github/org/repo");

    // Dirty it, plus non-git state that --replace-existing may legitimately wipe.
    std::fs::write(weave_repo.join("scratch.txt"), "local work\n").unwrap();
    let sentinel = ww_dir.join(".runtime/sentinel.txt");
    std::fs::create_dir_all(sentinel.parent().unwrap()).unwrap();
    std::fs::write(&sentinel, "will be wiped\n").unwrap();
    let head_before = head_sha(&weave_repo);

    // While the workweave is dirty, the replace must refuse and name the work
    // at risk — blind replacement is not consented destruction.
    let err_output = rwv()
        .args([
            "workweave",
            "web-app",
            "create",
            "reset",
            "--replace-existing",
        ])
        .current_dir(&ws)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&err_output.stderr);
    assert!(
        stderr.contains("refusing to replace"),
        "create --replace-existing over a dirty workweave must refuse; got:\n{stderr}"
    );
    assert!(
        stderr.contains("github/org/repo") && stderr.contains("delete"),
        "refusal must list the dirty repo and point at the explicit delete verb; got:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(weave_repo.join("scratch.txt")).unwrap(),
        "local work\n",
        "uncommitted content must survive a refused create --replace-existing"
    );

    // Clean the workweave; the replace now proceeds and wipes the
    // non-git state.
    std::fs::remove_file(weave_repo.join("scratch.txt")).unwrap();
    rwv()
        .args([
            "workweave",
            "web-app",
            "create",
            "reset",
            "--replace-existing",
        ])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        !sentinel.exists(),
        "sentinel file should be wiped by the --replace-existing recreate"
    );

    // Workweave is rebuilt: marker present, worktree on expected branch,
    // HEAD matching primary's current branch.
    assert!(ww_dir.join(".rwv-workweave").exists());
    assert_eq!(current_branch(&weave_repo), "web-app--reset");
    assert_eq!(
        head_sha(&weave_repo),
        head_before,
        "rebuilt worktree HEAD should match primary's current-branch HEAD"
    );
}

fn head_sha(dir: &Path) -> String {
    let output = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git rev-parse should run");
    assert!(
        output.status.success(),
        "git rev-parse HEAD in {} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("sha should be valid UTF-8")
        .trim()
        .to_string()
}

/// With the `.rwv-workweave` marker missing (or foreign), `create
/// --replace-existing`
/// cannot trust any manifest to enumerate the workweave — it must still
/// find dirty repos by scanning, refuse while work is at risk, and only
/// replace once clean.
#[test]
fn workweave_recreate_replace_existing_refuses_dirty_when_marker_missing() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "reset"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--reset");
    let weave_repo = ww_dir.join("github/org/repo");

    // Strip the marker (simulating corruption) and dirty the repo worktree.
    std::fs::remove_file(ww_dir.join(".rwv-workweave")).unwrap();
    std::fs::write(weave_repo.join("scratch.txt"), "local work\n").unwrap();

    let err_output = rwv()
        .args([
            "workweave",
            "web-app",
            "create",
            "reset",
            "--replace-existing",
        ])
        .current_dir(&ws)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&err_output.stderr);
    assert!(
        stderr.contains("refusing to replace") && stderr.contains("github/org/repo"),
        "marker-less create --replace-existing must refuse via the repo scan; \
         got:\n{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(weave_repo.join("scratch.txt")).unwrap(),
        "local work\n",
        "uncommitted content must survive the refusal"
    );

    // Clean → the raw-replace path proceeds.
    std::fs::remove_file(weave_repo.join("scratch.txt")).unwrap();
    rwv()
        .args([
            "workweave",
            "web-app",
            "create",
            "reset",
            "--replace-existing",
        ])
        .current_dir(&ws)
        .assert()
        .success();
    assert!(ww_dir.join(".rwv-workweave").exists());
}

/// After the workweave directory is removed (e.g. `rm -rf`) but the
/// `.git/worktrees/<name>` registration survives in the canonical repo,
/// `rwv workweave PROJECT create NAME` must succeed without requiring a
/// manual `git worktree prune`.
///
/// Repro for the "missing but already registered worktree" failure:
///   fatal: '<path>' is a missing but already registered worktree;
///          use 'add -f' to override, or 'prune'/'remove' to clear
///
/// Safety assertion: a *live* peer workweave's worktree registration must
/// not be touched by this create.
#[test]
fn workweave_create_succeeds_after_rm_rf_leaves_stale_git_registration() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // ---- Create the target workweave and a peer (live) workweave. ----
    rwv()
        .args(["workweave", "web-app", "create", "target"])
        .current_dir(&ws)
        .assert()
        .success();

    rwv()
        .args(["workweave", "web-app", "create", "live-peer"])
        .current_dir(&ws)
        .assert()
        .success();

    let target_ww_dir = weaveroot.join("web-app--target");
    let peer_ww_dir = weaveroot.join("web-app--live-peer");
    assert!(target_ww_dir.exists(), "target workweave should exist");
    assert!(peer_ww_dir.exists(), "peer workweave should exist");

    // Confirm the stale registration we're about to simulate is currently
    // present (i.e., git knows about this worktree).
    let repo_abs = ws.join("github/org/repo");
    let worktree_listing_before = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo_abs)
        .output()
        .expect("git worktree list should work");
    let listing_before = String::from_utf8_lossy(&worktree_listing_before.stdout);
    assert!(
        listing_before.contains("web-app--target"),
        "target worktree should appear in git worktree list before rm -rf; got:\n{listing_before}"
    );
    assert!(
        listing_before.contains("web-app--live-peer"),
        "peer worktree should appear in git worktree list before rm -rf; got:\n{listing_before}"
    );

    // Record the peer worktree's HEAD so we can assert it is unchanged later.
    let peer_repo = peer_ww_dir.join("github/org/repo");
    let peer_branch_before = current_branch(&peer_repo);
    let peer_head_before = head_sha(&peer_repo);

    // ---- Simulate the failure scenario: remove the workweave directory but
    //      leave the .git/worktrees/<name> registration intact. ----
    std::fs::remove_dir_all(&target_ww_dir).expect("rm -rf of target workweave dir should succeed");
    assert!(
        !target_ww_dir.exists(),
        "target workweave dir should be gone after rm -rf"
    );

    // Confirm the stale registration is still present (git has NOT pruned it
    // automatically — this is the broken state we need to self-heal).
    let worktree_listing_stale = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo_abs)
        .output()
        .expect("git worktree list should work");
    let listing_stale = String::from_utf8_lossy(&worktree_listing_stale.stdout);
    assert!(
        listing_stale.contains("web-app--target"),
        "stale registration should still be present after rm -rf (no auto-prune); got:\n{listing_stale}"
    );

    // ---- Re-create: must succeed WITHOUT any manual git worktree prune. ----
    rwv()
        .args(["workweave", "web-app", "create", "target"])
        .current_dir(&ws)
        .assert()
        .success();

    assert!(
        target_ww_dir.exists(),
        "target workweave directory should exist after re-create"
    );
    assert!(
        target_ww_dir.join("github/org/repo").exists(),
        "worktree repo should exist inside re-created workweave"
    );
    assert!(
        target_ww_dir.join(".rwv-workweave").exists(),
        ".rwv-workweave marker should be present after re-create"
    );

    // ---- Safety: peer workweave's registration and files are untouched. ----
    assert!(
        peer_ww_dir.exists(),
        "live peer workweave directory should still exist"
    );
    assert!(
        peer_repo.exists(),
        "peer worktree repo should still exist on disk"
    );
    assert_eq!(
        current_branch(&peer_repo),
        peer_branch_before,
        "peer worktree should still be on the same ephemeral branch"
    );
    assert_eq!(
        head_sha(&peer_repo),
        peer_head_before,
        "peer worktree HEAD should be unchanged"
    );

    // The peer registration should still appear in git worktree list.
    let worktree_listing_after = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo_abs)
        .output()
        .expect("git worktree list should work");
    let listing_after = String::from_utf8_lossy(&worktree_listing_after.stdout);
    assert!(
        listing_after.contains("web-app--live-peer"),
        "live peer registration must remain in git worktree list; got:\n{listing_after}"
    );
}

/// `workweave delete` without `--discard-unmerged-commits` must refuse when the
/// workweave's
/// worktrees hold commits not merged into the primary repos — the
/// ephemeral-branch cleanup would force-delete the only ref to them.
#[test]
fn workweave_delete_refuses_on_unmerged_commits() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "web-app");
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "web-app", "create", "committed"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--committed");
    let weave_repo = ww_dir.join("github/org/repo");

    // Commit work on the ephemeral branch: the worktree is clean, but the
    // commit is reachable only from the ephemeral branch.
    std::fs::write(weave_repo.join("feature.txt"), "committed work\n").unwrap();
    git(&["add", "feature.txt"], &weave_repo);
    git(&["commit", "-m", "ww: feature"], &weave_repo);
    let feature_sha = head_sha(&weave_repo);

    let err_output = rwv()
        .args(["workweave", "web-app", "delete", "committed"])
        .current_dir(&ws)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&err_output.stderr);
    assert!(
        stderr.contains("not merged") && stderr.contains("github/org/repo"),
        "delete must refuse on unmerged commits and list the repo; got:\n{stderr}"
    );
    assert_eq!(
        head_sha(&weave_repo),
        feature_sha,
        "workweave must be untouched after the refusal"
    );

    // --discard-unmerged-commits is the explicit consent path.
    rwv()
        .args([
            "workweave",
            "web-app",
            "delete",
            "committed",
            "--discard-unmerged-commits",
        ])
        .current_dir(&ws)
        .assert()
        .success();
    assert!(
        !ww_dir.exists(),
        "--discard-unmerged-commits delete removes the workweave"
    );
}
