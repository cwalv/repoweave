//! A weave whose repos are not on `main`.
//!
//! [`common::git`] pins `init.defaultBranch=main` into every subprocess git
//! call the suite makes, and [`common::rwv`] pins it into rwv's own. That is
//! load-bearing — CI runners ship no user-level default and `git init` would
//! otherwise answer `master` — but it means every fixture in the suite hands
//! rwv repositories sitting on a branch named `main`. Production guarantees no
//! such thing: `Vcs::init_repo` spells `--initial-branch=main` for the repos
//! rwv creates, and says nothing about the ones an operator brings.
//!
//! So the whole "the weave's own repos are on some other branch" plane is
//! invisible to the suite, and a resolution that reached for `main` instead of
//! reading the manifest would arrive tested.
//!
//! The fixture diverges the constant rather than merely removing it: `main`
//! exists here and points at different content than `trunk`. A path that
//! assumed `main` on a repo that has none would fail loudly and be found by
//! any test; one that assumes `main` on a repo that has both silently serves
//! the wrong tree, which is the defect worth a fixture.
//!
//! Scope: this drives manifest-declared branch resolution through
//! `workweave create` and `lock`. It says nothing about a repo whose *remote*
//! default diverges — `add_url_resolves_a_non_main_remote_default_branch` in
//! `tests/add_remove_test.rs` covers that direction.

use std::path::{Path, PathBuf};

mod common;

use common::git_in;

const TRUNK_ONLY: &str = "trunk-only.txt";

/// A repo whose checked-out branch is `trunk`, with a `main` that exists and
/// carries different content.
fn init_repo_on_trunk(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git_in(path, &["init", "--initial-branch=main"]);
    git_in(path, &["config", "user.email", "test@test.com"]);
    git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README"), "stale-main\n").unwrap();
    git_in(path, &["add", "."]);
    git_in(path, &["commit", "-m", "main: stale"]);

    git_in(path, &["checkout", "-b", "trunk"]);
    std::fs::write(path.join("README"), "trunk-tip\n").unwrap();
    std::fs::write(path.join(TRUNK_ONLY), "reachable only from trunk\n").unwrap();
    git_in(path, &["add", "."]);
    git_in(path, &["commit", "-m", "trunk: advance past main"]);
}

fn make_workspace(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_on_trunk(&repo_path);

    let project_dir = ws.join("projects").join("web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "trunk"
role = "owned"
"#,
            repo = common::url_path(&repo_path)
        ),
    )
    .unwrap();

    ws
}

#[test]
fn a_workweave_forks_from_the_declared_branch_not_from_main() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    common::rwv()
        .args(["workweave", "web-app", "create", "hotfix"])
        .current_dir(&ws)
        .assert()
        .success();

    let weave_repo = weaveroot.join("web-app--hotfix/github/org/repo");
    assert!(
        weave_repo.is_dir(),
        "the workweave should hold a worktree at github/org/repo"
    );
    assert_eq!(
        common::read_normalized(weave_repo.join("README")),
        "trunk-tip\n",
        "the worktree must be forked from `trunk`, the branch the manifest \
         declares. `main` exists in this repo and carries `stale-main`, so a \
         resolution that reached for it lands here silently"
    );
    assert!(
        weave_repo.join(TRUNK_ONLY).is_file(),
        "`{TRUNK_ONLY}` is reachable only from `trunk`; its absence means the \
         fork point was some other branch"
    );
}

#[test]
fn the_primary_checkout_stays_on_the_declared_branch() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    common::rwv()
        .args(["workweave", "web-app", "create", "hotfix"])
        .current_dir(&ws)
        .assert()
        .success();

    common::assert_on_branch(&ws.join("github/org/repo"), "trunk");
}

#[test]
fn lock_resolves_the_declared_branch_not_main() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let repo = ws.join("github/org/repo");
    let trunk_tip = git_in(&repo, &["rev-parse", "trunk"]);
    let main_tip = git_in(&repo, &["rev-parse", "main"]);
    assert_ne!(
        trunk_tip, main_tip,
        "the fixture must diverge the two branches, or this pin cannot tell \
         them apart"
    );

    common::rwv()
        .args(["lock", "--project", "web-app"])
        .current_dir(&ws)
        .assert()
        .success();

    let raw = common::read_normalized(ws.join("projects/web-app/rwv.lock"));
    let lock =
        repoweave::manifest::LockFile::from_json_str(&raw).expect("rwv lock writes valid JSON");
    let entry = lock
        .get_entry(&repoweave::manifest::RepoPath::new("github/org/repo").unwrap())
        .expect("the locked repo must be recorded");
    assert_eq!(
        entry.version.as_str(),
        trunk_tip,
        "the lock must pin the tip of `trunk`, the branch the manifest \
         declares. `main` is at {main_tip} and carries different content, so a \
         resolution that reached for it would be recorded here silently"
    );
}
