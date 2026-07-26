//! Integration tests anchoring documented behavior of `rwv lock` as described
//! in `docs/explanation/joints/lock-as-derived.md`.
//!
//! Doc claims pinned here:
//!
//!   - `rwv lock` is a pure git SHA snapshot — it does not run integration
//!     hooks (i.e., ecosystem install hooks such as `cargo generate-lockfile`
//!     do not fire). Source anchor: `src/lock.rs` — no integration-runner
//!     call in the lock codepath.
//!   - `rwv lock` does not read or honor the previous `rwv.lock`. It
//!     overwrites with the current HEAD SHA of each manifest repo.
//!   - `rwv lock` does not fetch from the network.  Running it when a remote
//!     has advanced beyond the local clone leaves the lock at the local HEAD,
//!     not at the remote tip.
//!
//! These tests are self-contained: they set up minimal git fixtures rather
//! than reusing helpers from `lock_test.rs` (which are not exported), in
//! keeping with the `doc_claims_*` file convention.

use assert_cmd::Command;
use std::path::Path;

mod common;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn rwv() -> Command {
    common::rwv()
}

fn git_run(args: &[&str], dir: &Path) -> String {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Create a minimal workspace: one manifest repo with a single commit.
/// Returns (workspace_root, project_dir, initial_head_sha).
fn make_workspace_with_repo(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let root = tmp.join("ws");
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let repo_path = root.join("github/acme/server");
    std::fs::create_dir_all(&repo_path).unwrap();
    git_run(&["init", "-b", "main"], &repo_path);
    std::fs::write(repo_path.join("README"), "init\n").unwrap();
    git_run(&["add", "."], &repo_path);
    git_run(&["commit", "-m", "initial"], &repo_path);
    let sha = git_run(&["rev-parse", "HEAD"], &repo_path);

    let project_dir = root.join("projects/my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.yaml"),
        "repositories:\n  github/acme/server:\n    type: git\n    url: https://github.com/acme/server.git\n    version: main\n    role: owned\n",
    )
    .unwrap();
    std::fs::write(root.join(".rwv-active"), "my-app\n").unwrap();

    (root, project_dir, sha)
}

// ===========================================================================
// 1. rwv lock does not run integration hooks
//
// Doc claim (lock-as-derived.md §"rwv lock is a pure git snapshot"):
//   `rwv lock` does not run integration installs.
//
// We place a Cargo.toml in the manifest repo (which would trigger the
// cargo integration's `cargo generate-lockfile` install hook on `rwv
// activate`) and verify that `rwv lock` succeeds without generating a
// workspace-root Cargo.toml. If integration hooks fired, `cargo
// generate-lockfile` would either fail (no workspace manifest) or produce
// artifacts that lock does not produce.
// ===========================================================================

#[test]
fn lock_does_not_run_integration_hooks() {
    let tmp = common::tempdir().unwrap();
    let (root, project_dir, _) = make_workspace_with_repo(tmp.path());

    // Add a Cargo.toml to the manifest repo — enough to trigger the cargo
    // integration's autodetect path if integrations were running.
    let repo_dir = root.join("github/acme/server");
    std::fs::write(
        repo_dir.join("Cargo.toml"),
        "[package]\nname = \"server\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    git_run(&["add", "."], &repo_dir);
    git_run(&["commit", "-m", "add Cargo.toml"], &repo_dir);

    rwv()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    // The workspace-root Cargo.toml is generated only by `rwv activate`
    // (via the cargo integration's activation hook). Its absence proves
    // that `rwv lock` did not run integration hooks.
    assert!(
        !root.join("Cargo.toml").exists(),
        "`rwv lock` must not generate the workspace-root Cargo.toml; \
         integration hooks must not run in the lock codepath"
    );

    // Lock was still written.
    assert!(
        project_dir.join("rwv.lock").exists(),
        "rwv.lock should exist after `rwv lock`"
    );
}

// ===========================================================================
// 2. rwv lock overwrites the previous lock from current HEAD
//
// Doc claim (lock-as-derived.md §"rwv lock is a pure git snapshot"):
//   `rwv lock` does not read or honor the previous `rwv.lock`. It
//   overwrites with whatever HEAD says.
//
// We hand-write an rwv.lock with a fabricated SHA, then run `rwv lock`.
// The resulting lock must contain the real HEAD SHA, not the fabricated one.
// ===========================================================================

#[test]
fn lock_overwrites_previous_lock_with_current_head() {
    let tmp = common::tempdir().unwrap();
    let (_root, project_dir, real_sha) = make_workspace_with_repo(tmp.path());

    // Hand-write a lock containing a fake SHA.
    let fake_sha = "0000000000000000000000000000000000000000";
    assert_ne!(real_sha, fake_sha);
    std::fs::write(
        project_dir.join("rwv.lock"),
        format!(
            "repositories:\n  github/acme/server:\n    type: git\n    url: https://github.com/acme/server.git\n    version: {fake_sha}\n"
        ),
    )
    .unwrap();

    rwv()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    let lock_content = std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap();
    assert!(
        lock_content.contains(&real_sha),
        "`rwv lock` must overwrite with the real HEAD SHA {real_sha}; got:\n{lock_content}"
    );
    assert!(
        !lock_content.contains(fake_sha),
        "`rwv lock` must not preserve the fabricated SHA {fake_sha}; got:\n{lock_content}"
    );
}

// ===========================================================================
// 3. rwv lock does not fetch from the network
//
// Doc claim (lock-as-derived.md §"rwv lock is a pure git snapshot"):
//   `rwv lock` does not fetch anything from the network. After `rwv lock`,
//   the lock entry reflects the *local* clone's HEAD, not the remote tip.
//
// We advance the bare remote past the local clone's HEAD, then run
// `rwv lock`. The lock must still record the local clone's HEAD — proof
// that `rwv lock` did not fetch.
// ===========================================================================

#[test]
fn lock_records_local_head_not_remote_tip() {
    let tmp = common::tempdir().unwrap();
    let root = tmp.path().join("ws");
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    // Set up a bare remote and clone it locally.
    let bare = tmp.path().join("server.git");
    std::fs::create_dir_all(&bare).unwrap();
    git_run(
        &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
        tmp.path(),
    );

    // Seed the bare via a temp clone.
    let seed = tmp.path().join("seed");
    git_run(
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
        tmp.path(),
    );
    std::fs::write(seed.join("README"), "seed\n").unwrap();
    git_run(&["add", "."], &seed);
    git_run(&["commit", "-m", "initial"], &seed);
    git_run(&["push", "origin", "main"], &seed);

    // Clone into the workspace (this is the local copy rwv lock will read).
    let local_clone = root.join("github/acme/server");
    git_run(
        &[
            "clone",
            "--origin",
            "origin",
            bare.to_str().unwrap(),
            local_clone.to_str().unwrap(),
        ],
        tmp.path(),
    );
    let local_sha = git_run(&["rev-parse", "HEAD"], &local_clone);

    // Advance the bare past the local clone (simulate a collaborator pushing).
    std::fs::write(seed.join("advance.txt"), "remote-advance\n").unwrap();
    git_run(&["add", "."], &seed);
    git_run(&["commit", "-m", "remote advance"], &seed);
    git_run(&["push", "origin", "main"], &seed);
    // Verify the bare is ahead of the local clone.
    let remote_sha = git_run(&["rev-parse", "HEAD"], &seed);
    assert_ne!(
        local_sha, remote_sha,
        "remote must have advanced past local"
    );

    // Set up the project.
    let project_dir = root.join("projects/my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let bare_url = format!("file://{}", bare.display());
    std::fs::write(
        project_dir.join("rwv.yaml"),
        format!(
            "repositories:\n  github/acme/server:\n    type: git\n    url: {bare_url}\n    version: main\n    role: owned\n"
        ),
    )
    .unwrap();
    std::fs::write(root.join(".rwv-active"), "my-app\n").unwrap();

    rwv()
        .arg("lock")
        .current_dir(&project_dir)
        .assert()
        .success();

    let lock_content = std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap();
    assert!(
        lock_content.contains(&local_sha),
        "`rwv lock` must record the local clone HEAD {local_sha} (no fetch); got:\n{lock_content}"
    );
    assert!(
        !lock_content.contains(&remote_sha),
        "`rwv lock` must NOT record the remote tip {remote_sha}; got:\n{lock_content}"
    );
}
