//! End-to-end tests for the slim tutorial path.
//!
//! The docs/tutorial.md walkthrough is the single beginner-facing flow:
//! `fetch` → `update` → edit → `lock`. The
//! tutorial drops sync / workweave / release / multi-machine fan-out
//! to dedicated how-to guides. This test file anchors the tutorial's
//! central promise: a user who copies these commands gets a working
//! workspace with an aligned lock at the end.
//!
//! Scenarios:
//!
//! - `tutorial_step1_fetch_clones_project_and_repos` — `rwv fetch
//!   <project>` from scratch clones the project repo + every manifest
//!   repo to its canonical path.
//! - `tutorial_step1_fetch_auto_activates` — `.rwv-active` lands and
//!   ecosystem symlinks appear at the workspace root (the project
//!   uses a Cargo workspace in this fixture).
//! - `tutorial_step2_update_advances_lock` — `rwv update` after the
//!   remote advances re-snapshots `rwv.lock` to the new HEAD. The
//!   "loud about `rwv update`" tutorial wording is contractual here.
//! - `tutorial_step3_first_edit_lands_on_disk` — a committed edit
//!   to a manifest repo is visible on the local clone (sanity-check
//!   that fetch/update don't clobber local commits).
//! - `tutorial_step4_lock_is_idempotent` — `rwv lock` is no-op-idempotent:
//!   two runs in a row, with no edits in between, produce byte-identical
//!   `rwv.lock` files.
//! - `tutorial_full_path` — end-to-end run of the entire tutorial in
//!   one test, in the same order the doc presents. This is the
//!   integration check; the per-step tests pin the individual
//!   contracts the doc makes.
//!
//! Test isolation: each test owns a fresh tempdir with its own bare
//! remotes; no network, no shared state, parallel-safe via cargo
//! test's per-process tempdir model.

use assert_cmd::Command;
use std::path::Path;
use std::process;

mod common;

// ---------------------------------------------------------------------------
// Helpers (mirrors tests/parallel_test.rs / tests/doc_claims_update_test.rs)
// ---------------------------------------------------------------------------

fn rwv() -> Command {
    common::rwv()
}

fn run_git(args: &[&str], cwd: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(cwd)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git command failed to start");
    assert!(status.success(), "git {:?} failed in {:?}", args, cwd);
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

/// Bare repo with a single Cargo.toml-bearing commit so the
/// downstream cargo-workspace integration recognises the clone.
fn init_bare_cargo_lib(path: &Path, crate_name: &str) {
    init_bare_repo(path);
    let tmp = common::tempdir().expect("tempdir");
    let work = tmp.path().join("w");
    run_git(
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
    run_git(&["config", "user.email", "t@t.com"], &work);
    run_git(&["config", "user.name", "T"], &work);
    std::fs::write(
        work.join("Cargo.toml"),
        format!("[package]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    )
    .unwrap();
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(work.join("src/lib.rs"), "// initial\n").unwrap();
    run_git(&["add", "."], &work);
    run_git(&["commit", "-m", "initial"], &work);
    run_git(&["push", "origin", "main"], &work);
}

/// Project source: bare repo whose default-branch tip carries an
/// `rwv.toml` pointing at the given `(repo_path, url)` manifest
/// entries. Returns the project-source URL `file:///...`.
///
/// **Trigger-model note:** under the new spec, `rwv fetch` is
/// a context verb — it surfaces existing managed/generated content but does
/// not author. So the project source must already contain the
/// integration-authored files (e.g. `Cargo.toml` workspace root) that the
/// downstream `rwv activate` will surface via symlinks. We pre-author them
/// here by spinning up a side workspace that clones the manifest repos
/// (so `cargo-workspace`'s detection cache fires), drives intent-mode
/// activation against a copy, then commits the generated outputs (Cargo.toml,
/// rwv.lock) into the project source. This mirrors the real-world flow:
/// the project author ran `rwv add` locally, committed both the manifest and
/// the integration outputs, and pushed.
fn make_project_source(tmp: &Path, name: &str, repos: &[(&str, &str)]) -> String {
    let project_bare = tmp.join(format!("{name}.git"));
    init_bare_repo(&project_bare);
    let work = tmp.join(format!("{name}_work"));
    run_git(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp,
    );
    run_git(&["config", "user.email", "t@t.com"], &work);
    run_git(&["config", "user.name", "T"], &work);
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(work.join("rwv.toml"), &manifest_toml).unwrap();

    // Stage a side workspace that mirrors what the project's first-time
    // author would have on their machine: clone the manifest repos, run
    // intent-mode activation, then bring the generated files back into the
    // project source repo (work).
    {
        let staging = tmp.join(format!("{name}_staging"));
        let staging_proj_dir = staging.join("projects").join(name);
        std::fs::create_dir_all(&staging_proj_dir).unwrap();
        std::fs::write(staging_proj_dir.join("rwv.toml"), &manifest_toml).unwrap();
        // Clone each manifest repo into the staging workspace so the
        // ecosystem integrations' detection cache picks them up.
        for (path, url) in repos {
            let dest = staging.join(path);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            run_git(
                &["clone", url, &dest.to_string_lossy()],
                staging.as_path().parent().unwrap_or(tmp),
            );
        }
        // Drive intent-mode activation against the staging workspace. We
        // skip install hooks (no `cargo generate-lockfile` etc.) because
        // staging is throwaway and the activate hook needs network/tooling
        // we're not assuming.
        let ctx = repoweave::workspace::WorkspaceContext::resolve(&staging, None).unwrap();
        repoweave::activate::activate_intent_with_options(
            name,
            &ctx,
            repoweave::activate::ActivateOptions {
                no_materialize: true,
            },
        )
        .expect("staging intent-mode activation should author integration outputs");
        // Copy generated outputs from staging back into the project source
        // repo. The set we care about today is whatever's in
        // `projects/<name>/`, minus rwv.toml (already committed).
        let staging_outputs = staging.join("projects").join(name);
        for entry in std::fs::read_dir(&staging_outputs).unwrap().flatten() {
            let fname = entry.file_name();
            if fname == "rwv.toml" {
                continue;
            }
            let src = entry.path();
            let dst = work.join(&fname);
            if src.is_dir() {
                copy_dir_recursive(&src, &dst);
            } else {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }

    run_git(&["add", "-A"], &work);
    run_git(
        &["commit", "-m", "manifest + intent-authored content"],
        &work,
    );
    run_git(&["push", "origin", "main"], &work);
    common::file_url(&project_bare)
}

/// Recursively copy `src` to `dst` (test-only helper).
fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap().flatten() {
        let path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dst_path);
        } else {
            std::fs::copy(&path, &dst_path).unwrap();
        }
    }
}

/// Advance a bare repo's `main` by one commit. Returns the new tip SHA.
fn advance_bare(tmp: &Path, bare: &Path, label: &str) -> String {
    let work = tmp.join(format!("{label}-work"));
    run_git(
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        tmp,
    );
    run_git(&["config", "user.email", "t@t.com"], &work);
    run_git(&["config", "user.name", "T"], &work);
    std::fs::write(work.join("advance.txt"), label).unwrap();
    run_git(&["add", "."], &work);
    run_git(&["commit", "-m", label], &work);
    run_git(&["push", "origin", "main"], &work);
    let out = common::git()
        .args(["rev-parse", "main"])
        .current_dir(bare)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// One-call fixture builder: project + two manifest repos as bare
/// remotes; project_url is what the tutorial's `rwv fetch
/// <project>` consumes.
struct Fixture {
    _tmp: tempfile::TempDir,
    workspace: std::path::PathBuf,
    project_url: String,
    bare_a: std::path::PathBuf,
    bare_b: std::path::PathBuf,
    repo_a_path: &'static str,
    repo_b_path: &'static str,
}

fn build_fixture() -> Fixture {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    let bare_a = tmp.path().join("a.git");
    let bare_b = tmp.path().join("b.git");
    init_bare_cargo_lib(&bare_a, "alpha");
    init_bare_cargo_lib(&bare_b, "beta");
    let url_a = common::file_url(&bare_a);
    let url_b = common::file_url(&bare_b);
    let repo_a_path = "github/tutorial/alpha";
    let repo_b_path = "github/tutorial/beta";
    let project_url = make_project_source(
        tmp.path(),
        "tutorial-project",
        &[(repo_a_path, &url_a), (repo_b_path, &url_b)],
    );
    Fixture {
        _tmp: tmp,
        workspace,
        project_url,
        bare_a,
        bare_b,
        repo_a_path,
        repo_b_path,
    }
}

fn read_lock(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join("projects/tutorial-project/rwv.lock"))
        .expect("rwv.lock should exist after fetch")
}

fn head_sha(clone: &Path) -> String {
    let out = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(clone)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

// ---------------------------------------------------------------------------
// Step 1: `rwv fetch <project>` from scratch
// ---------------------------------------------------------------------------

/// Tutorial §1: `rwv fetch <project>` clones the project repo into
/// `projects/<name>/` AND every manifest repo into its canonical
/// path. Pin both halves so a regression that only clones one or the
/// other is caught.
#[test]
fn tutorial_step1_fetch_clones_project_and_repos() {
    let fx = build_fixture();
    rwv()
        .args(["fetch", &fx.project_url])
        .current_dir(&fx.workspace)
        .assert()
        .success();
    assert!(
        fx.workspace.join("projects/tutorial-project").is_dir(),
        "project repo should be cloned to projects/<name>/"
    );
    assert!(
        fx.workspace
            .join("projects/tutorial-project/rwv.toml")
            .is_file(),
        "fetched project must contain rwv.toml"
    );
    assert!(
        fx.workspace.join(fx.repo_a_path).is_dir(),
        "manifest repo A should be cloned at {}",
        fx.repo_a_path
    );
    assert!(
        fx.workspace.join(fx.repo_b_path).is_dir(),
        "manifest repo B should be cloned at {}",
        fx.repo_b_path
    );
    // The lock lands at fetch-time too (default fetch is non-frozen
    // and bootstraps the lock when absent).
    assert!(
        fx.workspace
            .join("projects/tutorial-project/rwv.lock")
            .is_file(),
        "rwv.lock should be written by fetch (bootstrap mode)"
    );
}

// ---------------------------------------------------------------------------
// Step 1 (continued): auto-activate sets .rwv-active + ecosystem symlinks
// ---------------------------------------------------------------------------

/// Tutorial §1 (auto-activate): the first fetch sets `.rwv-active`
/// to the project name and lays down ecosystem workspace files at
/// the weave root via symlink. With Cargo manifests in both repos,
/// the cargo-workspace integration synthesises a workspace
/// `Cargo.toml` in the project dir and symlinks it to the root.
#[test]
fn tutorial_step1_fetch_auto_activates() {
    let fx = build_fixture();
    rwv()
        .args(["fetch", &fx.project_url])
        .current_dir(&fx.workspace)
        .assert()
        .success();

    let active = fx.workspace.join(".rwv-active");
    assert!(
        active.is_file(),
        ".rwv-active should be written by auto-activate"
    );
    let body = std::fs::read_to_string(&active).unwrap();
    assert_eq!(
        body.trim(),
        "tutorial-project",
        ".rwv-active must name the freshly-fetched project"
    );

    // Ecosystem workspace file: generated Cargo.toml in project dir,
    // symlinked at the weave root.
    let root_cargo = fx.workspace.join("Cargo.toml");
    assert!(
        root_cargo.symlink_metadata().is_ok(),
        "Cargo.toml should be present at the weave root after activate"
    );
    assert!(
        root_cargo
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink(),
        "weave-root Cargo.toml must be a symlink into the project dir"
    );
    let target = std::fs::read_link(&root_cargo).unwrap();
    assert!(
        target
            .ancestors()
            .any(|a| a.ends_with("projects/tutorial-project")),
        "Cargo.toml should symlink into projects/tutorial-project, got: {}",
        target.display()
    );
}

// ---------------------------------------------------------------------------
// Step 2: `rwv update` is the verb that gets the latest
// ---------------------------------------------------------------------------

/// Tutorial §2: `rwv update` advances each manifest repo to the
/// branch HEAD on the remote AND re-snapshots `rwv.lock`. This is
/// the verb the slim tutorial is loud about; the doc-side test pins
/// the slogan-to-behaviour mapping ("the verb that gets the
/// latest").
#[test]
fn tutorial_step2_update_advances_lock() {
    let fx = build_fixture();
    rwv()
        .args(["fetch", &fx.project_url])
        .current_dir(&fx.workspace)
        .assert()
        .success();

    let lock_before = read_lock(&fx.workspace);
    let initial_a = head_sha(&fx.workspace.join(fx.repo_a_path));

    // Advance remote A. Update should bump local clone A and lock.
    let new_a_remote_sha = advance_bare(fx._tmp.path(), &fx.bare_a, "tutorial-step2-a");
    assert_ne!(new_a_remote_sha, initial_a);

    rwv()
        .args(["update"])
        .current_dir(&fx.workspace)
        .assert()
        .success();

    let head_after = head_sha(&fx.workspace.join(fx.repo_a_path));
    assert_eq!(
        head_after, new_a_remote_sha,
        "update should advance local HEAD to remote tip"
    );
    let lock_after = read_lock(&fx.workspace);
    assert!(
        lock_after.contains(&new_a_remote_sha),
        "lock should be re-snapshotted to the new SHA after update; lock:\n{lock_after}"
    );
    assert_ne!(
        lock_before, lock_after,
        "lock must change after update advances a repo"
    );
}

// ---------------------------------------------------------------------------
// Step 3: First edit
// ---------------------------------------------------------------------------

/// Tutorial §3: any manifest repo is a regular git clone — editing,
/// committing, and pushing works the same as raw git, and `rwv`
/// neither clobbers nor hides the new commit. This pins the
/// "manifest repos are real git repos, not vendored copies" claim.
#[test]
fn tutorial_step3_first_edit_lands_on_disk() {
    let fx = build_fixture();
    rwv()
        .args(["fetch", &fx.project_url])
        .current_dir(&fx.workspace)
        .assert()
        .success();

    let repo_dir = fx.workspace.join(fx.repo_a_path);
    let pre_sha = head_sha(&repo_dir);

    // Make a real commit in the local clone.
    run_git(&["config", "user.email", "t@t.com"], &repo_dir);
    run_git(&["config", "user.name", "T"], &repo_dir);
    std::fs::write(repo_dir.join("EDIT.md"), "tutorial step 3\n").unwrap();
    run_git(&["add", "EDIT.md"], &repo_dir);
    run_git(&["commit", "-m", "feat: add EDIT.md"], &repo_dir);

    let post_sha = head_sha(&repo_dir);
    assert_ne!(post_sha, pre_sha, "commit should have advanced HEAD");
    assert!(
        repo_dir.join("EDIT.md").is_file(),
        "edit file should be on disk under the manifest-repo clone"
    );
}

// ---------------------------------------------------------------------------
// Step 4: `rwv lock` is idempotent
// ---------------------------------------------------------------------------

/// Tutorial §4: `rwv lock` is the no-network snapshot. Running it
/// twice in a row with no intervening edits must be a no-op for the
/// lock file — byte-identical. This pins the contract the tutorial
/// implies ("rwv lock reads HEAD from each repo and writes
/// projects/<project>/rwv.lock"): same inputs → same output.
#[test]
fn tutorial_step4_lock_is_idempotent() {
    let fx = build_fixture();
    rwv()
        .args(["fetch", &fx.project_url])
        .current_dir(&fx.workspace)
        .assert()
        .success();

    // First lock — captures current state.
    rwv()
        .args(["lock"])
        .current_dir(&fx.workspace)
        .assert()
        .success();
    let lock_first = read_lock(&fx.workspace);

    // Second lock — no inputs changed.
    rwv()
        .args(["lock"])
        .current_dir(&fx.workspace)
        .assert()
        .success();
    let lock_second = read_lock(&fx.workspace);
    assert_eq!(
        lock_first, lock_second,
        "rwv lock should be a no-op-idempotent when nothing changed"
    );
}

// ---------------------------------------------------------------------------
// End-to-end: full tutorial run in order
// ---------------------------------------------------------------------------

/// End-to-end exercise of the slim tutorial in the order the doc
/// presents:
///   1. `rwv fetch <project>`        (bootstrap + auto-activate)
///   2. `rwv update`                  (after the upstream advances)
///   3. edit + commit in a manifest repo
///   4. `rwv lock`                    (snapshot post-edit; idempotent re-run)
///
/// One large test instead of cramming the same shape into the per-
/// step tests above; it's the integration check that the whole path
/// composes without surprises.
#[test]
fn tutorial_full_path() {
    let fx = build_fixture();

    // 1. fetch
    rwv()
        .args(["fetch", &fx.project_url])
        .current_dir(&fx.workspace)
        .assert()
        .success();
    assert!(
        fx.workspace.join(".rwv-active").is_file(),
        "step 1 should leave .rwv-active"
    );

    // 2. update — after remote B advances
    let new_b = advance_bare(fx._tmp.path(), &fx.bare_b, "full-step2");
    rwv()
        .args(["update"])
        .current_dir(&fx.workspace)
        .assert()
        .success();
    let head_b = head_sha(&fx.workspace.join(fx.repo_b_path));
    assert_eq!(head_b, new_b, "step 2 should advance B's local clone");

    // 3. edit
    let repo_a = fx.workspace.join(fx.repo_a_path);
    run_git(&["config", "user.email", "t@t.com"], &repo_a);
    run_git(&["config", "user.name", "T"], &repo_a);
    std::fs::write(repo_a.join("README.tutorial"), "hi\n").unwrap();
    run_git(&["add", "README.tutorial"], &repo_a);
    run_git(&["commit", "-m", "step 3: edit"], &repo_a);
    let post_edit_a = head_sha(&repo_a);

    // 4. lock — snapshot, then re-run for idempotency
    rwv()
        .args(["lock"])
        .current_dir(&fx.workspace)
        .assert()
        .success();
    let lock_first = read_lock(&fx.workspace);
    assert!(
        lock_first.contains(&post_edit_a),
        "lock should reflect the edit-bearing HEAD of repo A"
    );
    assert!(
        lock_first.contains(&new_b),
        "lock should still hold the updated HEAD of repo B"
    );

    rwv()
        .args(["lock"])
        .current_dir(&fx.workspace)
        .assert()
        .success();
    let lock_second = read_lock(&fx.workspace);
    assert_eq!(
        lock_first, lock_second,
        "idempotent re-lock must produce a byte-identical lock"
    );
}
