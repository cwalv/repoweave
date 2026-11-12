//! Doc-claims coverage for what `rwv update` does to a clone that is ALREADY on
//! disk, and for the lock it reads and never writes.
//!
//! The claim is conditional on lock state, and both flat readings of it are
//! wrong: "present clones are untouched" holds in two of the three lock
//! states, "present clones are realigned" in the third.
//!
//! `tests/update_test.rs` pins the lock-HAS-an-entry arm — the realignment
//! itself (a fast-forward of the tracking branch's local counterpart), the
//! refusals when it would not be one, and the no-op when HEAD is already at the
//! pin.
//! This file pins the other two lock states, the locality of the resolve, and
//! that no lock state makes update write the lock.
//!
//! `rwv clone <source>` onto an existing `projects/<name>/` is pinned by
//! `tests/doc_claims_clone_test.rs`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::{Path, PathBuf};
use std::process;

use crate::common;

fn rwv() -> Command {
    common::rwv()
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
    common::file_url(bare)
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

    let tmp = common::tempdir().expect("tempdir for working clone");
    let work = tmp.path().join("work");
    common::git_in(
        tmp.path(),
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
    );
    common::git_in(&work, &["config", "user.email", "test@test.com"]);
    common::git_in(&work, &["config", "user.name", "Test"]);
    std::fs::write(work.join("README"), "initial\n").unwrap();
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "initial"]);
    let first = common::git_in(&work, &["rev-parse", "HEAD"]);
    std::fs::write(work.join("README"), "second\n").unwrap();
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "second"]);
    let second = common::git_in(&work, &["rev-parse", "HEAD"]);
    common::git_in(&work, &["push", "origin", "main"]);
    (first, second)
}

/// Workspace with an active project whose manifest lists `repo_paths`. No
/// `rwv.lock` is written — each test writes the lock state it is pinning.
fn setup(repo_paths: &[&str]) -> Fixture {
    let tmp = common::tempdir().unwrap();
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

    let mut manifest = String::from("[repositories]\n");
    for repo in &repos {
        let (path, url) = (&repo.path, url_of(&repo.bare));
        manifest.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest).unwrap();
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
    ///
    /// Round-trips through the real parser + `lock::write_lock_in`: a
    /// hand-formatted string that differs only in whitespace from what
    /// `rwv lock` itself would emit still diffs against a real relock.
    fn write_lock(&self, entries: &[(&str, &str)]) {
        let mut parts = Vec::new();
        for (path, sha) in entries {
            let url = url_of(&self.repo(path).bare);
            parts.push(format!(
                "{path:?}: {{\"type\": \"git\", \"url\": {url:?}, \"version\": {sha:?}}}"
            ));
        }
        let raw = format!("{{\"repositories\": {{{}}}}}", parts.join(","));
        let lock = repoweave::manifest::LockFile::from_json_str(&raw).unwrap();
        repoweave::lock::write_lock_in(&lock, &self.project_dir).unwrap();
    }

    /// Materialize a manifest repo and leave it ON `main` at the second
    /// commit — the state an operator's clone is normally in.
    fn clone_on_branch(&self, path: &str) -> PathBuf {
        let repo = self.repo(path);
        let dest = self.workspace.join(&repo.path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        common::git_in(
            self.tmp.path(),
            &[
                "clone",
                &repo.bare.to_string_lossy(),
                &dest.to_string_lossy(),
            ],
        );
        common::git_in(&dest, &["config", "user.email", "test@test.com"]);
        common::git_in(&dest, &["config", "user.name", "Test"]);
        dest
    }

    /// Push a third commit to `path`'s origin WITHOUT the workspace clone
    /// ever fetching it. Returns its SHA.
    fn push_third_commit(&self, path: &str) -> String {
        let repo = self.repo(path);
        let tmp = common::tempdir().unwrap();
        let work = tmp.path().join("work");
        common::git_in(
            tmp.path(),
            &[
                "clone",
                &repo.bare.to_string_lossy(),
                &work.to_string_lossy(),
            ],
        );
        common::git_in(&work, &["config", "user.email", "test@test.com"]);
        common::git_in(&work, &["config", "user.name", "Test"]);
        std::fs::write(work.join("README"), "third\n").unwrap();
        common::git_in(&work, &["commit", "-am", "third"]);
        let sha = common::git_in(&work, &["rev-parse", "HEAD"]);
        common::git_in(&work, &["push", "origin", "main"]);
        sha
    }
}

// ============================================================================
// Lock state 2 of 3 — an existing lock that does not cover the repo. The
// present clone is not realigned; the lock is not grown.
// ============================================================================

#[test]
fn present_clone_absent_from_the_lock_is_untouched_and_the_lock_is_not_grown() {
    let fx = setup(&["github/acme/a", "github/acme/b"]);
    let (a_first, b_first) = {
        let (a, b) = (fx.repo("github/acme/a"), fx.repo("github/acme/b"));
        (a.first.clone(), b.first.clone())
    };
    fx.write_lock(&[("github/acme/a", &a_first)]);

    // `a` is the covered repo and is left exactly at its pin, so its arm is a
    // no-op and the only thing this test observes is what happens to `b`.
    let dest_a = fx.clone_on_branch("github/acme/a");
    common::git_in(&dest_a, &["reset", "--hard", &a_first]);
    let dest_b = fx.clone_on_branch("github/acme/b");
    // Behind origin, so the on-disk HEAD and the branch tip disagree.
    common::git_in(&dest_b, &["reset", "--hard", &b_first]);
    let lock_before = fx.lock_text().unwrap();

    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&dest_b, &["rev-parse", "HEAD"]),
        b_first,
        "a present clone the lock does not cover must not be moved"
    );
    assert_eq!(
        current_branch(&dest_b).as_deref(),
        Some("main"),
        "no checkout runs for it, so nothing detaches it"
    );
    assert_eq!(
        fx.lock_text().unwrap(),
        lock_before,
        "update never writes the lock: the clone it left is not added to it"
    );
}

// ============================================================================
// Lock state 3 of 3 — no lock at all. Present clones are the source of truth,
// not the target of a realignment, and update does not create the lock.
// ============================================================================

#[test]
fn present_clone_with_no_lock_at_all_is_untouched_and_no_lock_is_written() {
    let fx = setup(&["github/acme/a"]);
    let a = fx.repo("github/acme/a");
    let dest = fx.clone_on_branch("github/acme/a");
    assert!(fx.lock_text().is_none(), "precondition: no lock on disk");

    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&dest, &["rev-parse", "HEAD"]),
        a.second,
        "with no lock a present clone is not moved"
    );
    assert_eq!(current_branch(&dest).as_deref(), Some("main"));
    assert!(
        !fx.lock_path().exists(),
        "update must not create the lock it found missing"
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
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .success()
        .stdout(predicate::str::contains("github/acme/a: pin unreachable"));

    assert!(
        !git_ok(&["cat-file", "-e", &format!("{third}^{{commit}}")], &dest),
        "update must not have pulled the pin from origin"
    );
    assert_eq!(common::git_in(&dest, &["rev-parse", "HEAD"]), a_second);
    assert_eq!(current_branch(&dest).as_deref(), Some("main"));
}

// ============================================================================
// Realigning to a complete lock moves the clone and leaves the lock alone.
// ============================================================================

#[test]
fn realigning_to_a_complete_lock_leaves_the_lock_bytes_alone() {
    let fx = setup(&["github/acme/a"]);
    let (a_first, a_second) = {
        let a = fx.repo("github/acme/a");
        (a.first.clone(), a.second.clone())
    };
    // The clone sits on main at the first commit while the lock pins the
    // second: realignment is the legal fast-forward of the tracking
    // counterpart rather than the rewind the branch model refuses.
    fx.write_lock(&[("github/acme/a", &a_second)]);
    let dest = fx.clone_on_branch("github/acme/a");
    common::git_in(&dest, &["reset", "--hard", &a_first]);
    let lock_before = fx.lock_text().unwrap();

    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(common::git_in(&dest, &["rev-parse", "HEAD"]), a_second);
    assert_eq!(
        current_branch(&dest).as_deref(),
        Some("main"),
        "the checkout stays attached to the counterpart it moved"
    );
    assert_eq!(
        common::git_in(&dest, &["rev-parse", "main"]),
        a_second,
        "the branch ref is what advanced"
    );
    assert_eq!(
        fx.lock_text().unwrap(),
        lock_before,
        "a lock that covers every member needs no write, and update makes none"
    );
}

// ============================================================================
// A `--repo` filter narrows which repos are visited, not what happens to them.
// ============================================================================

#[test]
fn a_repo_filter_realigns_the_named_repos_only_and_writes_no_lock() {
    let fx = setup(&["github/acme/a", "github/acme/b", "github/acme/c"]);
    let (a_first, a_second, b_second, c_first, c_second) = (
        fx.repo("github/acme/a").first.clone(),
        fx.repo("github/acme/a").second.clone(),
        fx.repo("github/acme/b").second.clone(),
        fx.repo("github/acme/c").first.clone(),
        fx.repo("github/acme/c").second.clone(),
    );
    fx.write_lock(&[("github/acme/a", &a_second), ("github/acme/c", &c_second)]);
    // Both covered repos are behind their pins, so each is a fast-forward of
    // `main` if visited.
    let dest_a = fx.clone_on_branch("github/acme/a");
    common::git_in(&dest_a, &["reset", "--hard", &a_first]);
    let dest_b = fx.clone_on_branch("github/acme/b");
    let dest_c = fx.clone_on_branch("github/acme/c");
    common::git_in(&dest_c, &["reset", "--hard", &c_first]);
    let lock_before = fx.lock_text().unwrap();

    rwv()
        .args([
            "update",
            "--repo",
            "github/acme/a",
            "--repo",
            "github/acme/b",
        ])
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&dest_a, &["rev-parse", "HEAD"]),
        a_second,
        "a named repo is realigned"
    );
    assert_eq!(
        common::git_in(&dest_b, &["rev-parse", "HEAD"]),
        b_second,
        "a named repo the lock does not cover is left where it is"
    );
    assert_eq!(
        common::git_in(&dest_c, &["rev-parse", "HEAD"]),
        c_first,
        "a repo the filter does not name is not visited"
    );
    assert_eq!(fx.lock_text().unwrap(), lock_before);
}
