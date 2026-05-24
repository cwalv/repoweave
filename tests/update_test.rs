//! E2E tests for the `--role` / `--repo` selector grammar on `rwv update`
//! (fo-9kweo).
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
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

    let mut manifest_bares: Vec<(String, PathBuf)> = Vec::new();
    let mut manifest_shas: Vec<(String, String)> = Vec::new();
    let mut manifest_yaml = String::from("repositories:\n");
    for (repo_path, role) in repos {
        let bare = tmp
            .path()
            .join(format!("{}.git", repo_path.replace('/', "_")));
        init_bare_repo_with_commit(&bare);
        let canonical = workspace.join(repo_path);
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        let remote_name = if *role == "fork" {
            "upstream"
        } else {
            "origin"
        };
        git_run(
            workspace.parent().unwrap(),
            &[
                "clone",
                "--origin",
                remote_name,
                bare.to_str().unwrap(),
                canonical.to_str().unwrap(),
            ],
        );
        git_run(&canonical, &["config", "user.email", "test@test.com"]);
        git_run(&canonical, &["config", "user.name", "Test"]);
        let head = git_run(&canonical, &["rev-parse", "HEAD"]);
        manifest_shas.push(((*repo_path).to_string(), head));
        manifest_bares.push(((*repo_path).to_string(), bare.clone()));
        let bare_url = bare.to_str().unwrap();
        manifest_yaml.push_str(&format!(
            "  {repo_path}:\n    type: git\n    url: {bare_url}\n    version: main\n    role: {role}\n"
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

    std::fs::write(project_dir.join("rwv.yaml"), &manifest_yaml).unwrap();
    let mut lock_yaml = String::from("repositories:\n");
    for (rp, sha) in &manifest_shas {
        let (_, bare) = manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        lock_yaml.push_str(&format!(
            "  {rp}:\n    type: git\n    url: {}\n    version: {sha}\n",
            bare.to_str().unwrap()
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), lock_yaml).unwrap();
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
