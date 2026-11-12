//! Integration tests anchoring documented behavior of `rwv lock` as described
//! in `docs/explanation/joints/lock-as-derived.md`.
//!
//! Doc claims pinned here:
//!
//!   - `rwv lock` is a pure git SHA snapshot — it does not run integration
//!     hooks (i.e., ecosystem install hooks such as `cargo generate-lockfile`
//!     do not fire). Source anchor: `src/lock.rs` — no integration-runner
//!     call in the lock codepath.
//!   - `rwv lock` reads the previous `rwv.lock` as the pins it moves: it
//!     records the current HEAD of each member it acts on when that is at or
//!     beyond the pin, and refuses a pin it cannot show the new commit
//!     descends from unless `--repo` names the member.
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

/// Create a minimal workspace: one manifest repo with a single commit.
/// Returns (workspace_root, project_dir, initial_head_sha).
fn make_workspace_with_repo(tmp: &Path) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let root = tmp.join("ws");
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let repo_path = root.join("github/acme/server");
    std::fs::create_dir_all(&repo_path).unwrap();
    common::git_in(&repo_path, &["init", "-b", "main"]);
    std::fs::write(repo_path.join("README"), "init\n").unwrap();
    common::git_in(&repo_path, &["add", "."]);
    common::git_in(&repo_path, &["commit", "-m", "initial"]);
    let sha = common::git_in(&repo_path, &["rev-parse", "HEAD"]);

    let project_dir = root.join("projects/my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        "[repositories.\"github/acme/server\"]\ntype = \"git\"\nurl = \"https://github.com/acme/server.git\"\nbranch = \"main\"\nrole = \"owned\"\n",
    )
    .unwrap();
    std::fs::write(root.join(".rwv-active"), "my-app\n").unwrap();

    (root, project_dir, sha)
}

// ===========================================================================
// 1. rwv lock does not run integration hooks
//
// Doc claim, from docs/explanation/joints/lock-as-derived.md:
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
    common::git_in(&repo_dir, &["add", "."]);
    common::git_in(&repo_dir, &["commit", "-m", "add Cargo.toml"]);

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
// 2. rwv lock moves a pin forward, and a pin it cannot show it descends from
//    only by name
//
// Doc claim, from docs/explanation/joints/lock-as-derived.md:
//   `rwv lock` records where each member is, moving a pin forward only. A
//   pin the new commit does not descend from is refused, `--repo`
//   included; `rwv member pin` moves it by name.
//
// We hand-write an rwv.lock with a fabricated SHA no ref reaches. `rwv
// lock`, with or without `--repo`, refuses and writes nothing; `rwv member
// pin <m> HEAD` overwrites it with the real HEAD SHA.
// ===========================================================================

#[test]
fn lock_refuses_a_pin_it_cannot_move_forward_from_and_member_pin_names_it() {
    let tmp = common::tempdir().unwrap();
    let (_root, project_dir, real_sha) = make_workspace_with_repo(tmp.path());

    let fake_sha = "0000000000000000000000000000000000000000";
    assert_ne!(real_sha, fake_sha);
    common::fixture_lock(
        &project_dir,
        &[(
            "github/acme/server",
            "https://github.com/acme/server.git",
            fake_sha,
        )],
    );
    let before = std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap();

    for args in [&["lock"][..], &["lock", "--repo", "github/acme/server"]] {
        let out = common::cli::run(args, &project_dir);
        common::cli::refused(&out, repoweave::diagnosis::State::PinBackwards);
        assert_eq!(
            std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap(),
            before,
            "{args:?}: a refused lock writes nothing"
        );
    }

    rwv()
        .args(["member", "pin", "github/acme/server", "HEAD"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stderr(predicates::str::contains("pinned at"));

    let lock_content = std::fs::read_to_string(project_dir.join("rwv.lock")).unwrap();
    assert!(
        lock_content.contains(&real_sha),
        "`rwv member pin` must overwrite with the real HEAD SHA {real_sha}; got:\n{lock_content}"
    );
    assert!(
        !lock_content.contains(fake_sha),
        "`rwv member pin` must not preserve the fabricated SHA {fake_sha}; got:\n{lock_content}"
    );
}

// ===========================================================================
// 3. rwv lock does not fetch from the network
//
// Doc claim, from docs/explanation/joints/lock-as-derived.md:
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
    common::git_in(
        tmp.path(),
        &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
    );

    // Seed the bare via a temp clone.
    let seed = tmp.path().join("seed");
    common::git_in(
        tmp.path(),
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    std::fs::write(seed.join("README"), "seed\n").unwrap();
    common::git_in(&seed, &["add", "."]);
    common::git_in(&seed, &["commit", "-m", "initial"]);
    common::git_in(&seed, &["push", "origin", "main"]);

    // Clone into the workspace (this is the local copy rwv lock will read).
    let local_clone = root.join("github/acme/server");
    common::git_in(
        tmp.path(),
        &[
            "clone",
            "--origin",
            "origin",
            bare.to_str().unwrap(),
            local_clone.to_str().unwrap(),
        ],
    );

    // Advance the bare past the local clone (simulate a collaborator pushing).
    std::fs::write(seed.join("advance.txt"), "remote-advance\n").unwrap();
    common::git_in(&seed, &["add", "."]);
    common::git_in(&seed, &["commit", "-m", "remote advance"]);
    common::git_in(&seed, &["push", "origin", "main"]);
    let remote_sha = common::git_in(&seed, &["rev-parse", "HEAD"]);
    assert_ne!(
        common::git_in(&local_clone, &["rev-parse", "HEAD"]),
        remote_sha,
        "remote must have advanced past local"
    );

    // Set up the project, the member pinned at the local clone's HEAD, and the
    // local clone one commit past the pin.
    let project_dir = root.join("projects/my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let bare_url = common::file_url(&bare);
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"github/acme/server\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ),
    )
    .unwrap();
    std::fs::write(root.join(".rwv-active"), "my-app\n").unwrap();
    common::pin_every_member(&root);
    std::fs::write(local_clone.join("local.txt"), "local\n").unwrap();
    common::git_in(&local_clone, &["add", "."]);
    common::git_in(&local_clone, &["commit", "-m", "local work"]);
    let local_sha = common::git_in(&local_clone, &["rev-parse", "HEAD"]);

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
