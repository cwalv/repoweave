//! Integration tests anchoring documented behavior of `rwv update` (fo-r982a,
//! verb-vocabulary split landed in fo-zvxff).
//!
//! Doc claims pinned here:
//!
//!   - update re-snapshots `rwv.lock` from each manifest repo's branch HEAD
//!     (not from the previous lock SHA)
//!   - update is the network-bumping counterpart to `rwv fetch` (which post
//!     fo-zvxff is lock-aligning only); the two verbs are distinct
//!   - update advances disk state to the freshly-fetched branch HEAD before
//!     re-snapshotting the lock
//!   - update -j N runs the per-repo advance loop in parallel and still
//!     writes a single coherent lock at the end
//!
//! Style note: this fixture is the bare-remote-plus-clone-plus-project
//! shape from `update_test.rs`; we keep it local rather than forking
//! helpers (the constraint from the bead) since `update_test.rs` doesn't
//! expose its helpers and the doc_claims_* convention is to be
//! self-contained per file. The verb-vs-fetch contrast claim is the
//! reason this file exists at all.

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

/// Push a new commit on `main` to a bare repo via a working clone. Returns
/// the new HEAD SHA on the bare. The bare's name is embedded in the commit
/// content to keep distinct bares' SHAs distinct.
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

struct UpdateWorkspace {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    project_name: String,
    manifest_bares: Vec<(String, PathBuf)>,
}

/// Build a workspace with manifest repos at the given roles. The lock
/// initially matches local HEAD (which itself matches the bare's HEAD).
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

fn read_lock_sha(workspace: &Path, project_name: &str, repo_path: &str) -> String {
    let lock = std::fs::read_to_string(
        workspace
            .join("projects")
            .join(project_name)
            .join("rwv.lock"),
    )
    .expect("rwv.lock should exist after update");

    // Parse the per-repo block. The lock format is:
    //   <path>:
    //     type: git
    //     url: ...
    //     version: <sha>
    let mut in_block = false;
    for line in lock.lines() {
        let trimmed = line.trim_end();
        if trimmed == format!("  {repo_path}:") {
            in_block = true;
            continue;
        }
        if in_block {
            if let Some(rest) = trimmed.strip_prefix("    version: ") {
                return rest.to_string();
            }
            // A new repo block starts at this indentation.
            if trimmed.starts_with("  ") && !trimmed.starts_with("    ") {
                break;
            }
        }
    }
    panic!("could not find version for {repo_path} in lock:\n{lock}");
}

// ===========================================================================
// 1. update re-snapshots the lock from branch HEAD (fo-r982a / fo-zvxff)
//
// Doc claim: after `rwv update`, the rwv.lock entry for each updated repo
// equals the new branch-HEAD SHA on the remote (not the prior lock SHA).
// ===========================================================================

#[test]
fn update_re_snapshots_lock_from_branch_head() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("alpha", &repos);

    let initial_lock_sha = read_lock_sha(&ws.workspace, &ws.project_name, "local/org/a");

    // Advance the remote so HEAD moves past the initial lock.
    let (_, bare) = &ws.manifest_bares[0];
    let new_remote_head = advance_bare_main(bare);
    assert_ne!(initial_lock_sha, new_remote_head);

    rwv()
        .args(["update", "--dirty"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    // Lock now reflects the new branch HEAD.
    let post_lock_sha = read_lock_sha(&ws.workspace, &ws.project_name, "local/org/a");
    assert_eq!(
        post_lock_sha, new_remote_head,
        "update should re-snapshot the lock from the freshly-fetched branch HEAD"
    );

    // Local clone HEAD also matches the new SHA.
    let local_head = git_run(&ws.workspace.join("local/org/a"), &["rev-parse", "HEAD"]);
    assert_eq!(
        local_head, new_remote_head,
        "update should advance the local clone to the new branch HEAD"
    );
}

// ===========================================================================
// 2. update is distinct from fetch (post fo-zvxff verb-vocabulary split)
//
// Doc claim: `rwv fetch` (default mode) aligns the clone to the existing
// rwv.lock and does not advance to remote HEAD. `rwv update` advances to
// remote HEAD and re-snapshots the lock. These are different verbs with
// different side-effects.
//
// We exercise the distinction directly: starting from the same state, run
// `rwv fetch` and `rwv update` in two separate workspaces and observe that
// only the update path moves the lock SHA.
// ===========================================================================

#[test]
fn update_advances_lock_while_fetch_does_not() {
    // Build two workspaces from the same bare-repo seed. The bare advances
    // past the initial lock SHA in both, then:
    //   - ws_fetch:  run `rwv fetch` from inside the active project dir
    //                (default mode = aligns to lock, does not bump it).
    //   - ws_update: run `rwv update` (bumps lock to branch HEAD).
    //
    // The lock SHA in ws_fetch must be unchanged; the lock SHA in
    // ws_update must equal the new branch HEAD.
    let repos = [("local/org/a", "owned")];

    // --- ws_update --------------------------------------------------------
    let ws_update = build_workspace("alpha", &repos);
    let initial_lock_sha_update =
        read_lock_sha(&ws_update.workspace, &ws_update.project_name, "local/org/a");
    let (_, bare_update) = &ws_update.manifest_bares[0];
    let new_head_update = advance_bare_main(bare_update);
    assert_ne!(initial_lock_sha_update, new_head_update);

    rwv()
        .args(["update", "--dirty"])
        .current_dir(&ws_update.workspace)
        .assert()
        .success();

    let post_lock_update =
        read_lock_sha(&ws_update.workspace, &ws_update.project_name, "local/org/a");
    assert_eq!(
        post_lock_update, new_head_update,
        "rwv update must move the lock to the new branch HEAD"
    );

    // --- ws_fetch ---------------------------------------------------------
    // Default `rwv fetch <source>` is a clone-and-align verb that creates a
    // fresh workspace from a project source. It is NOT the same as
    // `rwv update` (a re-snapshot verb on an existing workspace). Verify
    // that aspect of the split: `rwv fetch <project_source>` reads the
    // committed lock and pins clones at the lock SHA — the bare's new
    // HEAD does NOT leak into the clone.
    //
    // We mirror fetch_test.rs's setup: a bare project repo carrying rwv.yaml
    // + rwv.lock, fetched into an empty workspace. The fetched clone of
    // the manifest repo must be at the LOCK sha, not the new bare HEAD.
    let tmp_fetch = tempfile::tempdir().unwrap();
    let ws_fetch = tmp_fetch.path().join("ws");
    std::fs::create_dir_all(&ws_fetch).unwrap();

    // Set up the manifest-repo bare and capture the initial SHA.
    let manifest_bare = tmp_fetch.path().join("manifest.git");
    init_bare_repo_with_commit(&manifest_bare);
    let manifest_url = format!("file://{}", manifest_bare.display());
    let dep_clone = tmp_fetch.path().join("dep_clone");
    git_run(
        tmp_fetch.path(),
        &[
            "clone",
            manifest_bare.to_str().unwrap(),
            dep_clone.to_str().unwrap(),
        ],
    );
    let initial_sha = git_run(&dep_clone, &["rev-parse", "HEAD"]);

    // Build the project bare with rwv.yaml + rwv.lock pinning to initial_sha.
    let project_bare = tmp_fetch.path().join("project.git");
    init_bare_repo_with_commit(&project_bare);
    let project_work = tmp_fetch.path().join("project_work");
    git_run(
        tmp_fetch.path(),
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_work.to_str().unwrap(),
        ],
    );
    git_run(&project_work, &["config", "user.email", "test@test.com"]);
    git_run(&project_work, &["config", "user.name", "Test"]);
    let yaml = format!(
        "repositories:\n  local/team/dep:\n    type: git\n    url: {manifest_url}\n    version: main\n    role: owned\n"
    );
    std::fs::write(project_work.join("rwv.yaml"), yaml).unwrap();
    let lock = format!(
        "repositories:\n  local/team/dep:\n    type: git\n    url: {manifest_url}\n    version: {initial_sha}\n"
    );
    std::fs::write(project_work.join("rwv.lock"), lock).unwrap();
    git_run(&project_work, &["add", "."]);
    git_run(&project_work, &["commit", "-m", "manifest + lock"]);
    git_run(&project_work, &["push", "origin", "main"]);

    // Advance the bare past the lock — proves fetch reads lock, not HEAD.
    git_run(&dep_clone, &["config", "user.email", "test@test.com"]);
    git_run(&dep_clone, &["config", "user.name", "Test"]);
    std::fs::write(dep_clone.join("after.txt"), "after").unwrap();
    git_run(&dep_clone, &["add", "."]);
    git_run(&dep_clone, &["commit", "-m", "after-lock"]);
    git_run(&dep_clone, &["push", "origin", "main"]);

    let project_source = format!("file://{}", project_bare.display());
    rwv()
        .args(["fetch", &project_source])
        .current_dir(&ws_fetch)
        .assert()
        .success();

    // The cloned dep should sit at initial_sha — proves default `rwv fetch`
    // does not bump.
    let fetched_head = git_run(&ws_fetch.join("local/team/dep"), &["rev-parse", "HEAD"]);
    assert_eq!(
        fetched_head, initial_sha,
        "rwv fetch (default) aligns to the lock; it must NOT advance to branch HEAD"
    );
}

// ===========================================================================
// 3. update -j N parallel mode (fo-r982a / fo-ysnuz)
//
// Doc claim: `rwv update -j N` (N > 1) advances each manifest repo on a
// bounded worker pool; the lock write happens serially after the pool
// joins. The per-repo lines carry the `[<repo>]` prefix and every repo
// ends at its new branch HEAD.
// ===========================================================================

#[test]
fn update_dash_j_parallel_advances_all_and_emits_prefix() {
    let repos = [
        ("local/org/a", "owned"),
        ("local/org/b", "owned"),
        ("local/org/c", "owned"),
    ];
    let ws = build_workspace("alpha", &repos);

    // Advance each bare to a new HEAD.
    let mut new_heads: Vec<(String, String)> = Vec::new();
    for (rp, bare) in &ws.manifest_bares {
        let new = advance_bare_main(bare);
        new_heads.push((rp.clone(), new));
    }

    let output = rwv()
        .args(["update", "--dirty", "-j", "2"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv update -j 2");
    assert!(
        output.status.success(),
        "update -j 2 should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Reporter::Parallel wraps each repo's lines with `[<repo>]`. The exact
    // text varies (git output is captured) but at minimum the per-repo
    // "rwv update: fetching <path>" line is emitted under the prefix.
    let any_prefix = new_heads
        .iter()
        .any(|(rp, _)| stdout.contains(&format!("[{rp}]")));
    assert!(
        any_prefix,
        "update -j N must wrap per-repo lines with `[<repo>]`; got:\n{stdout}"
    );

    // Every local clone now sits at its new branch HEAD.
    for (rp, new) in &new_heads {
        let head = git_run(&ws.workspace.join(rp), &["rev-parse", "HEAD"]);
        assert_eq!(&head, new, "{rp} local should be at the new branch HEAD");
    }

    // Lock re-snapshot covered every repo.
    for (rp, new) in &new_heads {
        let lock_sha = read_lock_sha(&ws.workspace, &ws.project_name, rp);
        assert_eq!(
            &lock_sha, new,
            "lock entry for {rp} should reflect the new branch HEAD after update -j"
        );
    }
}
