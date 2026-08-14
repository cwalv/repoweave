//! E2E tests for the `--role` / `--repo` selector grammar on `rwv update`.
//!
//! `rwv update` advances each manifest repo to its remote branch HEAD and
//! re-snapshots the lock. The filter narrows the *advance* loop but the
//! post-advance lock still walks the full manifest. These tests verify the
//! selection step: only filtered repos advance; others stay at their
//! existing HEAD.

use assert_cmd::Command;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn git_run(cwd: &Path, args: &[&str]) -> String {
    let output = common::git()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should be available");
    if !output.status.success() {
        panic!(
            "git {:?} in {} failed: {}",
            args,
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn init_bare_repo_with_commit(bare: &Path) {
    let parent = bare.parent().expect("bare repo path needs a parent");
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    git_run(
        parent,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    let seed = parent.join(format!("__seed_{stem}"));
    git_run(
        parent,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    git_run(&seed, &["config", "user.email", "test@test.com"]);
    git_run(&seed, &["config", "user.name", "Test"]);
    std::fs::write(seed.join("README"), "seed").unwrap();
    git_run(&seed, &["add", "."]);
    git_run(&seed, &["commit", "-m", "initial"]);
    git_run(&seed, &["push", "origin", "main"]);
}

/// Workspace + active project ready to be driven by `rwv update`.
struct UpdateWorkspace {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    #[allow(dead_code)] // kept for parity with push_test fixture
    project_name: String,
    /// (canonical_path, bare_remote_path) for each manifest repo.
    manifest_bares: Vec<(String, PathBuf)>,
}

/// Build a workspace with manifest repos at the given roles. Each gets its
/// own bare remote (advances pushed to the bare appear when `rwv update`
/// `git fetch`'s the remote). rwv.lock is generated to match local HEAD.
fn build_workspace(project_name: &str, repos: &[(&str, &str)]) -> UpdateWorkspace {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

    let mut manifest_bares: Vec<(String, PathBuf)> = Vec::new();
    let mut manifest_shas: Vec<(String, String)> = Vec::new();
    let mut manifest_yaml = String::from("[repositories]\n");
    for (repo_path, role) in repos {
        let bare = tmp
            .path()
            .join(format!("{}.git", repo_path.replace('/', "_")));
        init_bare_repo_with_commit(&bare);
        let canonical = workspace.join(repo_path);
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        git_run(
            workspace.parent().unwrap(),
            &[
                "clone",
                "--origin",
                "origin",
                bare.to_str().unwrap(),
                canonical.to_str().unwrap(),
            ],
        );
        git_run(&canonical, &["config", "user.email", "test@test.com"]);
        git_run(&canonical, &["config", "user.name", "Test"]);
        let head = git_run(&canonical, &["rev-parse", "HEAD"]);
        manifest_shas.push(((*repo_path).to_string(), head));
        manifest_bares.push(((*repo_path).to_string(), bare.clone()));
        let bare_url = common::file_url(&bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
        ));
    }

    // Project repo carrying the manifest + lock.
    let project_bare = tmp.path().join("project.git");
    init_bare_repo_with_commit(&project_bare);
    let project_dir = workspace.join("projects").join(project_name);
    git_run(
        workspace.parent().unwrap(),
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_dir.to_str().unwrap(),
        ],
    );
    git_run(&project_dir, &["config", "user.email", "test@test.com"]);
    git_run(&project_dir, &["config", "user.name", "Test"]);

    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();
    // Round-trips through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let mut lock_entries = Vec::new();
    for (rp, sha) in &manifest_shas {
        let (_, bare) = manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let bare_url = common::file_url(bare);
        lock_entries.push(format!(
            "{rp:?}: {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {sha:?}}}"
        ));
    }
    let raw_lock = format!("{{\"repositories\": {{{}}}}}", lock_entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "manifest + lock"]);

    std::fs::write(workspace.join(".rwv-active"), format!("{project_name}\n")).unwrap();

    UpdateWorkspace {
        _tmp: tmp,
        workspace,
        project_name: project_name.to_string(),
        manifest_bares,
    }
}

/// Advance the `main` branch on a bare remote by cloning, committing, and
/// pushing back. Returns the new HEAD SHA on the bare. The bare's name is
/// embedded in the commit content so each advance produces a distinct SHA
/// across bares (avoids `Test`-user + same-content collisions in the test).
fn advance_bare_main(bare: &Path) -> String {
    let parent = bare.parent().unwrap();
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    let work = parent.join(format!("__adv_{stem}"));
    git_run(
        parent,
        &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
    );
    git_run(&work, &["config", "user.email", "test@test.com"]);
    git_run(&work, &["config", "user.name", "Test"]);
    std::fs::write(work.join("advance.txt"), format!("advance-{stem}")).unwrap();
    git_run(&work, &["add", "."]);
    git_run(&work, &["commit", "-m", &format!("advance {stem}")]);
    git_run(&work, &["push", "origin", "main"]);
    git_run(&work, &["rev-parse", "HEAD"])
}

// ============================================================================
// CLI plumbing
// ============================================================================

#[test]
fn update_help_lists_role_and_repo_flags() {
    let output = rwv()
        .args(["update", "--help"])
        .output()
        .expect("rwv update --help");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("--role"),
        "update --help should list --role; got: {combined}"
    );
    assert!(
        combined.contains("--repo"),
        "update --help should list --repo; got: {combined}"
    );
}

// ============================================================================
// --role: only matching-role repos advance
// ============================================================================

#[test]
fn update_role_filter_only_advances_matching_role() {
    let ws = build_workspace(
        "alpha",
        &[("local/org/p", "owned"), ("local/org/d", "dependency")],
    );

    // Advance both bares on `main` — only the primary local clone should
    // pick the new tip up.
    let (_, p_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/p")
        .unwrap();
    let (_, d_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/d")
        .unwrap();
    let new_p_sha = advance_bare_main(p_bare);
    let new_d_sha = advance_bare_main(d_bare);
    assert_ne!(new_p_sha, new_d_sha);

    rwv()
        .args(["update", "--role", "owned", "--dirty"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let p_local = ws.workspace.join("local/org/p");
    let d_local = ws.workspace.join("local/org/d");
    assert_eq!(
        git_run(&p_local, &["rev-parse", "HEAD"]),
        new_p_sha,
        "primary repo should advance"
    );
    assert_ne!(
        git_run(&d_local, &["rev-parse", "HEAD"]),
        new_d_sha,
        "dependency repo should NOT advance"
    );
}

// ============================================================================
// --repo exact: only that path advances
// ============================================================================

#[test]
fn update_repo_exact_filter_advances_only_that_path() {
    let ws = build_workspace(
        "alpha",
        &[("local/org/a", "owned"), ("local/org/b", "owned")],
    );
    let (_, a_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/a")
        .unwrap();
    let (_, b_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/b")
        .unwrap();
    let new_a_sha = advance_bare_main(a_bare);
    let new_b_sha = advance_bare_main(b_bare);

    rwv()
        .args(["update", "--repo", "local/org/a", "--dirty"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let a_local = ws.workspace.join("local/org/a");
    let b_local = ws.workspace.join("local/org/b");
    assert_eq!(git_run(&a_local, &["rev-parse", "HEAD"]), new_a_sha);
    assert_ne!(git_run(&b_local, &["rev-parse", "HEAD"]), new_b_sha);
}

// ============================================================================
// --repo glob: matching paths advance
// ============================================================================

#[test]
fn update_repo_glob_filter_advances_matching() {
    let ws = build_workspace(
        "alpha",
        &[
            ("local/org/a", "owned"),
            ("local/org/b", "owned"),
            ("local/other/c", "owned"),
        ],
    );
    let new_shas: Vec<(String, String)> = ws
        .manifest_bares
        .iter()
        .map(|(p, bare)| (p.clone(), advance_bare_main(bare)))
        .collect();

    rwv()
        .args(["update", "--repo", "glob:local/org/*", "--dirty"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    for (rp, new_sha) in &new_shas {
        let local = ws.workspace.join(rp);
        let head = git_run(&local, &["rev-parse", "HEAD"]);
        if rp.starts_with("local/org/") {
            assert_eq!(&head, new_sha, "{rp} should have advanced");
        } else {
            assert_ne!(&head, new_sha, "{rp} should NOT have advanced");
        }
    }
}

// ============================================================================
// --repo re: regex match
// ============================================================================

#[test]
fn update_repo_regex_filter_advances_matching() {
    let ws = build_workspace(
        "alpha",
        &[
            ("local/cwalv/a", "owned"),
            ("local/cwalv/b", "owned"),
            ("local/other/c", "owned"),
        ],
    );
    let new_shas: Vec<(String, String)> = ws
        .manifest_bares
        .iter()
        .map(|(p, bare)| (p.clone(), advance_bare_main(bare)))
        .collect();

    rwv()
        .args(["update", "--repo", "re:^local/cwalv/", "--dirty"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    for (rp, new_sha) in &new_shas {
        let head = git_run(&ws.workspace.join(rp), &["rev-parse", "HEAD"]);
        if rp.starts_with("local/cwalv/") {
            assert_eq!(&head, new_sha);
        } else {
            assert_ne!(&head, new_sha);
        }
    }
}

// ============================================================================
// Union: --role + --repo
// ============================================================================

#[test]
fn update_union_role_and_repo_selectors() {
    let ws = build_workspace(
        "alpha",
        &[
            ("local/me/p", "owned"),
            ("local/external/dep", "dependency"),
            ("local/external/other", "dependency"),
        ],
    );
    let new_shas: Vec<(String, String)> = ws
        .manifest_bares
        .iter()
        .map(|(p, bare)| (p.clone(), advance_bare_main(bare)))
        .collect();

    rwv()
        .args([
            "update",
            "--role",
            "owned",
            "--repo",
            "local/external/dep",
            "--dirty",
        ])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    for (rp, new_sha) in &new_shas {
        let head = git_run(&ws.workspace.join(rp), &["rev-parse", "HEAD"]);
        let expected_advance = rp == "local/me/p" || rp == "local/external/dep";
        if expected_advance {
            assert_eq!(&head, new_sha, "{rp} should advance");
        } else {
            assert_ne!(&head, new_sha, "{rp} should NOT advance");
        }
    }
}

// ============================================================================
// Missing clone: honest repair advice (repair-verb audit)
// ============================================================================

/// A manifest entry whose clone is absent on disk must produce an error that
/// names `rwv fetch` (in-place mode) as the repair verb, followed by re-running
/// `rwv update`. Stale manual-git-clone advice must NOT appear — the settled
/// repair verb replaces the manual repair (see fetch::run_fetch_in_place).
#[test]
fn update_missing_clone_names_rwv_fetch_repair_verb() {
    let ws = build_workspace("proj-missing", &[("local/acme/present", "owned")]);

    // Append a manifest entry for a repo that was never cloned locally.
    // (Constructing the missing state directly — no deletions needed.)
    let project_dir = ws.workspace.join("projects").join("proj-missing");
    let bare_url = common::file_url(&ws.manifest_bares[0].1);
    let mut manifest = std::fs::read_to_string(project_dir.join("rwv.toml")).unwrap();
    manifest.push_str(&format!(
        "[repositories.\"local/acme/absent\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"owned\"\n"
    ));
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    let output = rwv()
        .args(["update", "--dirty"])
        .current_dir(&ws.workspace)
        .output()
        .expect("failed to spawn rwv update");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        !output.status.success(),
        "update must fail when a manifest clone is missing on disk; got:\n{combined}"
    );
    assert!(
        combined.contains("clone missing"),
        "error must name the state (clone missing); got:\n{combined}"
    );
    // Repair verb: `rwv fetch` (in-place mode re-materializes the member).
    assert!(
        combined.contains("rwv fetch"),
        "error must name `rwv fetch` as the repair verb; got:\n{combined}"
    );
    assert!(
        combined.contains("rwv update"),
        "error must name re-running `rwv update` after the repair; \
         got:\n{combined}"
    );
    // Stale honest-manual advice must be gone.
    assert!(
        !combined.contains("git clone"),
        "error must NOT advise manual `git clone` — the repair verb `rwv fetch` \
         performs the repair; got:\n{combined}"
    );
}

// ============================================================================
// R18 regression: prune + ghost-ref detection
// ============================================================================

/// Helper: rename the main branch on a bare remote to a new name. This
/// simulates the upstream "rename branch" case — the old branch is deleted
/// and a new one is created under a different name.
///
/// Bare repos refuse to delete their current branch (HEAD). We work around
/// this by first pointing HEAD at the new branch name in the bare, then
/// deleting the old branch. The sequence:
///   1. Clone the bare, create the new branch, push it.
///   2. In the bare repo, change HEAD to point at the new branch.
///   3. Delete the old branch from the bare.
fn rename_branch_on_bare(bare: &Path, old_name: &str, new_name: &str) {
    let parent = bare.parent().unwrap();
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    let work = parent.join(format!("__rename_{stem}"));
    git_run(
        parent,
        &["clone", bare.to_str().unwrap(), work.to_str().unwrap()],
    );
    git_run(&work, &["config", "user.email", "test@test.com"]);
    git_run(&work, &["config", "user.name", "Test"]);
    // Create the new branch and push it to the bare.
    git_run(&work, &["checkout", "-b", new_name]);
    git_run(
        &work,
        &["push", "origin", &format!("{new_name}:{new_name}")],
    );
    // Update the bare's HEAD to point at the new branch so the old one is
    // no longer the "current branch" and can be deleted.
    git_run(
        bare,
        &["symbolic-ref", "HEAD", &format!("refs/heads/{new_name}")],
    );
    // Now delete the old branch from the bare.
    git_run(bare, &["branch", "-D", old_name]);
}

/// R18 regression test: upstream renames a branch (delete + create under new
/// name). Before this fix, `rwv update` would resolve against the stale
/// `origin/<old-name>` remote-tracking ref indefinitely (ghost-ref bug).
/// After this fix, `--prune` removes the stale ref during fetch, and the
/// subsequent resolution fails with a house-pattern error naming:
/// - the repo path,
/// - the branch name that is gone,
/// - the state ("does not resolve on the remote — renamed or deleted upstream"),
/// - the one exit (update rwv.toml's `version:` to the current branch name);
///   the message must not recommend pinning `version:` to a SHA or tag,
///   since `version:` is branch-only and that pin can never resolve.
#[test]
fn update_prune_detects_deleted_branch_with_actionable_message() {
    let ws = build_workspace("r18-test", &[("local/org/renamed", "owned")]);

    let (_, bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/renamed")
        .unwrap();

    // Rename the tracked branch on the bare remote: "main" -> "main-v2".
    rename_branch_on_bare(bare, "main", "main-v2");

    // rwv update must fail and produce a house-pattern error.
    let output = rwv()
        .args(["update", "--dirty"])
        .current_dir(&ws.workspace)
        .output()
        .expect("failed to spawn rwv update");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        !output.status.success(),
        "update must fail when the tracked branch no longer exists upstream; \
         got:\n{combined}"
    );
    assert!(
        combined.contains("local/org/renamed"),
        "error must name the repo path; got:\n{combined}"
    );
    assert!(
        combined.contains("main"),
        "error must name the branch that is gone; got:\n{combined}"
    );
    assert!(
        combined.contains("does not resolve on the remote")
            || combined.contains("renamed or deleted upstream"),
        "error must state the upstream-deletion/rename condition; got:\n{combined}"
    );
    // Must name the one supported exit.
    assert!(
        combined.contains("version:") || combined.contains("current branch name"),
        "error must name the rwv.toml version update exit; got:\n{combined}"
    );
    // Must not steer the operator toward pinning `version:` to a SHA/tag —
    // that pin is unsupported and can never resolve.
    assert!(
        !combined.contains("pin `version:`"),
        "error must not recommend the unsupported version: pin; got:\n{combined}"
    );
}

/// Post-prune normal update still succeeds. Verifies that `--prune` does not
/// break the happy path: when upstream hasn't renamed or deleted any branch,
/// `rwv update` continues to work correctly.
#[test]
fn update_prune_does_not_break_normal_update() {
    let ws = build_workspace("prune-happy", &[("local/org/repo", "owned")]);

    let (_, bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/repo")
        .unwrap();

    let new_sha = advance_bare_main(bare);

    rwv()
        .args(["update", "--dirty"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let local = ws.workspace.join("local/org/repo");
    assert_eq!(
        git_run(&local, &["rev-parse", "HEAD"]),
        new_sha,
        "repo should advance to the new upstream SHA after prune-enabled update"
    );
}

/// Adversarial: the lock pins a bare SHA. Even if the corresponding remote-
/// tracking ref is pruned (e.g. the branch that originally contained that
/// commit was deleted upstream), the object itself is still in the local
/// clone's object store and is reachable. This test verifies that `rwv lock`
/// (which reads HEAD, not remote refs) and the object store are the correct
/// primitives — a pruned tracking ref must NOT prevent the lock SHA from
/// being available.
///
/// Note: `rwv update` advances to the *current* remote branch HEAD and then
/// snapshots the lock. This test verifies that fetching with `--prune` does
/// not remove the commit objects that the lock references — only the stale
/// remote-tracking *refs* are removed, not the objects they pointed to.
#[test]
fn update_prune_does_not_lose_lock_pinned_sha() {
    let ws = build_workspace("sha-pin-test", &[("local/org/obj", "owned")]);

    let (_, bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/obj")
        .unwrap();

    // Capture the SHA that the local clone currently has checked out.
    let local = ws.workspace.join("local/org/obj");
    let pinned_sha = git_run(&local, &["rev-parse", "HEAD"]);

    // Advance the bare (so main moves forward) but DO NOT advance the local
    // clone — the local is now "behind" but holds the old SHA in its object
    // store. The old SHA is in the local object store but no longer at any
    // remote-tracking ref tip after we prune (main has moved on).
    advance_bare_main(bare);

    // Verify the pinned SHA is still reachable in the object store.
    let cat_file_status = common::git()
        .args(["cat-file", "-e", &format!("{pinned_sha}^{{commit}}")])
        .current_dir(&local)
        .status()
        .expect("git cat-file should be available");
    assert!(
        cat_file_status.success(),
        "pinned SHA {pinned_sha} must still exist in local object store"
    );

    // Now run update — this fetches with --prune and moves the local clone
    // to the new upstream HEAD. The old SHA object remains in the store
    // (git prune/gc would be needed to actually remove it, which rwv never
    // calls). Confirm update succeeds.
    rwv()
        .args(["update", "--dirty"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    // After update, the repo is at the new HEAD, not the old pinned SHA.
    let post_sha = git_run(&local, &["rev-parse", "HEAD"]);
    assert_ne!(
        post_sha, pinned_sha,
        "repo should have advanced past the originally-pinned SHA"
    );

    // The old commit object is still reachable in the object store (not pruned
    // by --prune, which only removes remote-tracking *refs*, not objects).
    let cat_file_after = common::git()
        .args(["cat-file", "-e", &format!("{pinned_sha}^{{commit}}")])
        .current_dir(&local)
        .status()
        .expect("git cat-file should be available");
    assert!(
        cat_file_after.success(),
        "pinned SHA {pinned_sha} must remain in object store after prune-enabled fetch \
         (--prune only removes stale tracking refs, not commit objects)"
    );
}
