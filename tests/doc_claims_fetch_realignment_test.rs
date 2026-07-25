//! Doc-claims coverage for what in-place `rwv fetch` does to a clone that is
//! ALREADY on disk, and for what `--frozen` changes about it.
//!
//! The claim is conditional on lock state, and both flat readings of it are
//! wrong: "present clones are untouched" holds in two of the three lock
//! states, "present clones are realigned" in the third.
//!
//! `tests/fetch_in_place_test.rs` pins the lock-HAS-an-entry arm — the
//! realignment itself, the detach it causes, the refusal when the branch
//! carries unpublished work, and the no-op when HEAD is already at the pin.
//! This file pins the other two lock states, the locality of the resolve,
//! `--frozen`'s coverage-not-freshness semantics, default/frozen equivalence,
//! and the lock-write step a `--repo` filter skips.
//!
//! Vocabulary: STALE is freshness — the lock is behind HEAD. INCOMPLETE is
//! coverage — a manifest repo has no lock entry. `--frozen` errors on
//! incomplete only.
//!
//! `rwv fetch <source>` onto an existing `projects/<name>/` is pinned by
//! `doc_claims_fetch_test.rs::fetch_name_collision_behavior`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
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

fn git_ok(args: &[&str], cwd: &Path) -> bool {
    common::git()
        .args(args)
        .current_dir(cwd)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git command failed to start")
        .success()
}

/// `git symbolic-ref --short HEAD`, or `None` when HEAD is detached.
fn current_branch(repo: &Path) -> Option<String> {
    let out = common::git()
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(repo)
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::null())
        .output()
        .expect("git symbolic-ref failed to spawn");
    out.status
        .success()
        .then(|| String::from_utf8(out.stdout).unwrap().trim().to_string())
}

struct Repo {
    path: String,
    bare: PathBuf,
    first: String,
    second: String,
}

struct Fixture {
    workspace: PathBuf,
    project_dir: PathBuf,
    repos: Vec<Repo>,
    tmp: tempfile::TempDir,
}

fn url_of(bare: &Path) -> String {
    format!("file://{}", bare.display())
}

/// Bare repo with two commits, so a lock pin and a branch HEAD can disagree.
fn init_bare_repo_with_two_commits(path: &Path) -> (String, String) {
    let status = common::git()
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(path)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git init --bare failed");

    let tmp = tempfile::tempdir().expect("tempdir for working clone");
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
    let first = git_capture(&["rev-parse", "HEAD"], &work);
    std::fs::write(work.join("README"), "second\n").unwrap();
    git_run(&["add", "."], &work);
    git_run(&["commit", "-m", "second"], &work);
    let second = git_capture(&["rev-parse", "HEAD"], &work);
    git_run(&["push", "origin", "main"], &work);
    (first, second)
}

/// Workspace with an active project whose manifest lists `repo_paths`. No
/// `rwv.lock` is written — each test writes the lock state it is pinning.
fn setup(repo_paths: &[&str]) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let repos: Vec<Repo> = repo_paths
        .iter()
        .map(|rp| {
            let bare = tmp.path().join(format!("{}.git", rp.replace('/', "_")));
            let (first, second) = init_bare_repo_with_two_commits(&bare);
            Repo {
                path: (*rp).to_owned(),
                bare,
                first,
                second,
            }
        })
        .collect();

    let project_dir = workspace.join("projects").join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();

    let mut manifest = String::from("repositories:\n");
    for repo in &repos {
        let (path, url) = (&repo.path, url_of(&repo.bare));
        manifest.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &manifest).unwrap();
    std::fs::write(workspace.join(".rwv-active"), "my-app\n").unwrap();

    Fixture {
        workspace,
        project_dir,
        repos,
        tmp,
    }
}

impl Fixture {
    fn repo(&self, path: &str) -> &Repo {
        self.repos
            .iter()
            .find(|r| r.path == path)
            .unwrap_or_else(|| panic!("no fixture repo {path}"))
    }

    fn lock_path(&self) -> PathBuf {
        self.project_dir.join("rwv.lock")
    }

    /// The lock verbatim, so equality over it is byte equality.
    fn lock_text(&self) -> Option<String> {
        std::fs::read_to_string(self.lock_path()).ok()
    }

    /// Write an `rwv.lock` covering exactly `entries` — `(repo_path, sha)`.
    fn write_lock(&self, entries: &[(&str, &str)]) {
        let mut lock = String::from("repositories:\n");
        for (path, sha) in entries {
            let url = url_of(&self.repo(path).bare);
            lock.push_str(&format!(
                "  {path}:\n    type: git\n    url: {url}\n    version: {sha}\n"
            ));
        }
        std::fs::write(self.lock_path(), &lock).unwrap();
    }

    /// Materialize a manifest repo and leave it ON `main` at the second
    /// commit — the state an operator's clone is normally in.
    fn clone_on_branch(&self, path: &str) -> PathBuf {
        let repo = self.repo(path);
        let dest = self.workspace.join(&repo.path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        git_run(
            &[
                "clone",
                &repo.bare.to_string_lossy(),
                &dest.to_string_lossy(),
            ],
            self.tmp.path(),
        );
        git_run(&["config", "user.email", "test@test.com"], &dest);
        git_run(&["config", "user.name", "Test"], &dest);
        dest
    }

    /// Push a third commit to `path`'s origin WITHOUT the workspace clone
    /// ever fetching it. Returns its SHA.
    fn push_third_commit(&self, path: &str) -> String {
        let repo = self.repo(path);
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        git_run(
            &[
                "clone",
                &repo.bare.to_string_lossy(),
                &work.to_string_lossy(),
            ],
            tmp.path(),
        );
        git_run(&["config", "user.email", "test@test.com"], &work);
        git_run(&["config", "user.name", "Test"], &work);
        std::fs::write(work.join("README"), "third\n").unwrap();
        git_run(&["commit", "-am", "third"], &work);
        let sha = git_capture(&["rev-parse", "HEAD"], &work);
        git_run(&["push", "origin", "main"], &work);
        sha
    }
}

// ============================================================================
// Lock state 2 of 3 — an existing lock that does not cover the repo
// (INCOMPLETE coverage). The present clone is not realigned; the lock grows.
// ============================================================================

#[test]
fn present_clone_absent_from_the_lock_is_untouched_and_added_at_its_own_head() {
    let fx = setup(&["github/acme/a", "github/acme/b"]);
    let (a_first, b_first, b_second) = {
        let (a, b) = (fx.repo("github/acme/a"), fx.repo("github/acme/b"));
        (a.first.clone(), b.first.clone(), b.second.clone())
    };
    fx.write_lock(&[("github/acme/a", &a_first)]);

    fx.clone_on_branch("github/acme/a");
    let dest_b = fx.clone_on_branch("github/acme/b");
    // Behind origin, so the on-disk HEAD and the branch tip disagree.
    git_run(&["reset", "--hard", &b_first], &dest_b);

    rwv()
        .arg("fetch")
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(
        git_capture(&["rev-parse", "HEAD"], &dest_b),
        b_first,
        "a present clone the lock does not cover must not be moved"
    );
    assert_eq!(
        current_branch(&dest_b).as_deref(),
        Some("main"),
        "no checkout runs for it, so nothing detaches it"
    );

    let lock = std::fs::read_to_string(fx.lock_path()).unwrap();
    assert!(
        lock.contains(&b_first),
        "the additive entry records the clone's own HEAD; got:\n{lock}"
    );
    assert!(
        !lock.contains(&b_second),
        "not the tip origin is on — nothing is fetched to write it; got:\n{lock}"
    );
    assert!(
        lock.contains(&a_first),
        "the pre-existing entry must not be advanced; got:\n{lock}"
    );
}

// ============================================================================
// Lock state 3 of 3 — no lock at all (bootstrap). Present clones are the
// source of truth, not the target of a realignment.
// ============================================================================

#[test]
fn present_clone_with_no_lock_at_all_is_untouched_and_the_lock_records_its_head() {
    let fx = setup(&["github/acme/a"]);
    let a = fx.repo("github/acme/a");
    let dest = fx.clone_on_branch("github/acme/a");
    assert!(fx.lock_text().is_none(), "precondition: no lock on disk");

    rwv()
        .arg("fetch")
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(
        git_capture(&["rev-parse", "HEAD"], &dest),
        a.second,
        "bootstrap must not move a present clone"
    );
    assert_eq!(current_branch(&dest).as_deref(), Some("main"));

    let lock = std::fs::read_to_string(fx.lock_path()).unwrap();
    assert!(
        lock.contains(&a.second),
        "the bootstrapped lock is snapshotted from the on-disk HEAD; got:\n{lock}"
    );
    assert!(
        !lock.contains(&a.first),
        "nothing pins the older commit here; got:\n{lock}"
    );
}

// ============================================================================
// Realignment is LOCAL: the pin is resolved in the clone's own object store
// and is never fetched from origin.
// ============================================================================

#[test]
fn realignment_resolves_the_pin_locally_and_does_not_fetch_a_missing_one() {
    let fx = setup(&["github/acme/a"]);
    let a_second = fx.repo("github/acme/a").second.clone();
    let dest = fx.clone_on_branch("github/acme/a");
    // Reachable from origin, absent from this clone: only a network fetch
    // could supply it.
    let third = fx.push_third_commit("github/acme/a");
    fx.write_lock(&[("github/acme/a", &third)]);

    rwv()
        .arg("fetch")
        .current_dir(&fx.workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to resolve"))
        .stderr(predicate::str::contains("not found"));

    assert!(
        !git_ok(&["cat-file", "-e", &format!("{third}^{{commit}}")], &dest),
        "fetch must not have pulled the pin from origin"
    );
    assert_eq!(git_capture(&["rev-parse", "HEAD"], &dest), a_second);
    assert_eq!(current_branch(&dest).as_deref(), Some("main"));
}

// ============================================================================
// `--frozen` validates COVERAGE, not freshness.
// ============================================================================

#[test]
fn frozen_errors_on_an_incomplete_lock_without_touching_any_clone() {
    let fx = setup(&["github/acme/a", "github/acme/b"]);
    let (a_first, a_second) = {
        let a = fx.repo("github/acme/a");
        (a.first.clone(), a.second.clone())
    };
    fx.write_lock(&[("github/acme/a", &a_first)]);
    let dest_a = fx.clone_on_branch("github/acme/a");
    fx.clone_on_branch("github/acme/b");
    let lock_before = fx.lock_text().unwrap();

    rwv()
        .args(["fetch", "--frozen"])
        .current_dir(&fx.workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("not covered by lock"))
        .stderr(predicate::str::contains("github/acme/b"));

    assert_eq!(
        git_capture(&["rev-parse", "HEAD"], &dest_a),
        a_second,
        "the coverage check bails before any repo is realigned"
    );
    assert_eq!(
        fx.lock_text().unwrap(),
        lock_before,
        "--frozen never writes"
    );
}

#[test]
fn frozen_checks_coverage_not_freshness_and_realigns_identically_to_default() {
    let fx = setup(&["github/acme/a"]);
    let (a_first, a_second) = {
        let a = fx.repo("github/acme/a");
        (a.first.clone(), a.second.clone())
    };
    // Complete coverage, STALE freshness: the lock pins the first commit
    // while the clone sits on main at the second.
    fx.write_lock(&[("github/acme/a", &a_first)]);
    let dest = fx.clone_on_branch("github/acme/a");
    let lock_before = fx.lock_text().unwrap();

    let observe = || {
        (
            git_capture(&["rev-parse", "HEAD"], &dest),
            current_branch(&dest),
            git_capture(&["rev-parse", "main"], &dest),
            fx.lock_text().unwrap(),
        )
    };

    rwv()
        .arg("fetch")
        .current_dir(&fx.workspace)
        .assert()
        .success();
    let after_default = observe();

    // Back to the start state: the realignment left the branch ref alone.
    git_run(&["checkout", "main"], &dest);
    assert_eq!(git_capture(&["rev-parse", "HEAD"], &dest), a_second);

    rwv()
        .args(["fetch", "--frozen"])
        .current_dir(&fx.workspace)
        .assert()
        .success();
    let after_frozen = observe();

    assert_eq!(
        after_default, after_frozen,
        "--frozen changes lock validation only; the realignment it performs \
         is indistinguishable from the default mode's"
    );
    assert_eq!(after_default.0, a_first, "both modes land on the pin");
    assert_eq!(
        after_default.3, lock_before,
        "a complete lock needs neither a bootstrap nor an additive write, so \
         neither mode writes it"
    );
}

// ============================================================================
// A `--role` / `--repo` filter realigns the same way but skips the whole
// lock-write step — both halves of it.
// ============================================================================

#[test]
fn filtered_fetch_skips_the_bootstrap_lock_write() {
    let fx = setup(&["github/acme/a", "github/acme/b"]);
    fx.clone_on_branch("github/acme/a");
    fx.clone_on_branch("github/acme/b");
    assert!(fx.lock_text().is_none(), "precondition: no lock on disk");

    rwv()
        .args(["fetch", "--repo", "github/acme/a"])
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert!(
        fx.lock_text().is_none(),
        "a filtered fetch has not seen every manifest repo, so it writes no \
         bootstrap lock — even with every clone on disk"
    );
}

#[test]
fn filtered_fetch_skips_the_additive_coverage_write_but_still_realigns() {
    let fx = setup(&["github/acme/a", "github/acme/b"]);
    let (a_first, b_second) = (
        fx.repo("github/acme/a").first.clone(),
        fx.repo("github/acme/b").second.clone(),
    );
    fx.write_lock(&[("github/acme/a", &a_first)]);
    let dest_a = fx.clone_on_branch("github/acme/a");
    let dest_b = fx.clone_on_branch("github/acme/b");
    let lock_before = fx.lock_text().unwrap();

    // Both filtered repos are processed: b is the uncovered one, so the
    // additive entry is produced and then dropped by the filter.
    rwv()
        .args([
            "fetch",
            "--repo",
            "github/acme/a",
            "--repo",
            "github/acme/b",
        ])
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(
        git_capture(&["rev-parse", "HEAD"], &dest_a),
        a_first,
        "the filter narrows which repos are visited, not what happens to them"
    );
    assert_eq!(git_capture(&["rev-parse", "HEAD"], &dest_b), b_second);
    assert_eq!(
        fx.lock_text().unwrap(),
        lock_before,
        "the additive entry for the uncovered repo is not written under a filter"
    );
}
