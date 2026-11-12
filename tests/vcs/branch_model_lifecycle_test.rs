//! The branch model's create / delete / remove lifecycle.
//!
//! What these tests are for, stated once:
//!
//!   1. **What is deleted is what belongs to the deleted branch, never what
//!      merely looks like it.** The shipped code decided "is this branch
//!      rwv's?" by parsing the name, so a hand-made `my--feature/wip` was rwv's
//!      property. The tests below fix the inverse: a branch that is not the
//!      workweave's own survives every verb that used to glob for it, and the
//!      workweave's own, once landed, is gone.
//!   2. **Break-the-guard, not happy-path.** Each test's fixture contains
//!      the thing that used to be destroyed, at a name and tip that make the
//!      destruction observable. Restore the prefix glob, or the create-retry
//!      force-delete, and these fail.
//!   3. **Refuse rather than destroy.** Where no proof is constructible the
//!      verb must stop or leave the branch and say so; "left it alone and
//!      reported" is the assertion, not "succeeded".

use assert_cmd::Command;
use std::path::{Path, PathBuf};

use crate::common;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn rwv() -> Command {
    common::rwv()
}

/// Every local branch name in `repo`.
fn branch_names(repo: &Path) -> Vec<String> {
    common::git_in(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/heads/",
        ],
    )
    .lines()
    .map(|l| l.trim().to_string())
    .filter(|s| !s.is_empty())
    .collect()
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "--initial-branch=main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README"), "init").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
}

/// A workspace with one project (`web-app`) and one manifest repo.
fn make_workspace(tmp: &Path) -> PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects/web-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        format!(
            "[repositories.\"github/org/repo\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nbranch = \"main\"\nrole = \"owned\"\n",
            repo = common::url_path(&repo_path)
        ),
    )
    .unwrap();
    common::make_project_repo(&project_dir);
    ws
}

/// `make_workspace`, with the project directory a repo whose trunk is
/// recorded, so a workweave has a branch whose record the deletion rule reads.
fn make_workspace_with_project_repo(tmp: &Path) -> PathBuf {
    make_workspace(tmp)
}

/// Create a branch at HEAD carrying a commit reachable from nowhere else,
/// and return that commit's SHA. This is what makes a destroy *observable*:
/// deleting the branch strands the commit.
fn hand_made_branch_with_unique_commit(repo: &Path, name: &str, file: &str) -> String {
    let on_entry = common::git_in(repo, &["symbolic-ref", "--short", "HEAD"]);
    common::git_in(repo, &["checkout", "-b", name]);
    std::fs::write(repo.join(file), "operator work").unwrap();
    common::git_in(repo, &["add", "-A"]);
    common::git_in(repo, &["commit", "-m", "work only this branch can reach"]);
    let sha = common::git_in(repo, &["rev-parse", name]);
    common::git_in(repo, &["checkout", &on_entry]);
    sha
}

// ---------------------------------------------------------------------------
// R2 — a branch that merely LOOKS like the workweave's is not the workweave's
// ---------------------------------------------------------------------------

/// `workweave delete` deletes the branch the workweave was aligned for and what
/// belongs to it, and nothing that merely resembles it.
///
/// The shipped delete globbed the prefix `{project}--{workweave}` and
/// force-deleted everything it returned. A prefix is not a namespace: for
/// workweave `real`, the glob also claims `web-app--really-mine`, an
/// unrelated branch of the operator's. That is the case with teeth here —
/// restore the glob-and-destroy and this test fails on it, with the commit
/// only that branch reached left dangling.
///
/// `my--feature/wip` and `dependabot--npm/lodash` sit outside this workweave's
/// prefix, so delete never had a route to them.
#[test]
fn delete_destroys_only_what_belongs_to_the_workweaves_branch() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    let repo = ws.join("github/org/repo");

    let sibling_sha =
        hand_made_branch_with_unique_commit(&repo, "web-app--really-mine", "sibling.txt");
    let feature_sha = hand_made_branch_with_unique_commit(&repo, "my--feature/wip", "wip.txt");
    let bot_sha = hand_made_branch_with_unique_commit(&repo, "dependabot--npm/lodash", "bump.txt");

    rwv()
        .args(["workweave", "create", "--project", "web-app", "real"])
        .current_dir(&ws)
        .assert()
        .success();
    assert!(
        branch_names(&repo).iter().any(|b| b == "web-app--real"),
        "precondition: the create must have written its own branch"
    );

    rwv()
        .args([
            "workweave",
            "delete",
            "--project",
            "web-app",
            "real",
            "--discard-uncommitted",
        ])
        .current_dir(&ws)
        .assert()
        .success();

    let after = branch_names(&repo);

    assert!(
        !after.iter().any(|b| b == "web-app--real"),
        "the workweave's own branch should have been deleted; branches: {after:?}"
    );

    for (name, sha) in [
        ("web-app--really-mine", &sibling_sha),
        ("my--feature/wip", &feature_sha),
        ("dependabot--npm/lodash", &bot_sha),
    ] {
        assert!(
            after.iter().any(|b| b == name),
            "branch {name} is not the workweave's and must survive; branches: {after:?}"
        );
        assert_eq!(
            &common::git_in(&repo, &["rev-parse", name]),
            sha,
            "{name} must still point at the operator's commit"
        );
    }
}

/// A branch under the workweave's name, in a member the workweave never
/// materialized, is not the workweave's branch: the delete neither touches it
/// nor reports it.
///
/// The member is declared after the create, which is how a store ends up
/// inside a workweave's scope with no branch of the workweave's in it. Git
/// cannot hold `refs/heads/p--ww` and `refs/heads/p--ww/x` at the same time, so
/// in a store where the workweave's branch stands, a lookalike under it cannot
/// exist to be left alone.
#[test]
fn delete_leaves_a_lookalike_under_the_workweaves_name_alone() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "create", "--project", "web-app", "reported"])
        .current_dir(&ws)
        .assert()
        .success();

    let repo2 = ws.join("github/org/other");
    init_repo_with_commit(&repo2);
    let manifest = ws.join("projects/web-app/rwv.toml");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text.push_str(&format!(
        "[repositories.\"github/org/other\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nbranch = \"main\"\nrole = \"owned\"\n",
        repo = common::url_path(&repo2)
    ));
    std::fs::write(&manifest, text).unwrap();
    let leftover_sha =
        hand_made_branch_with_unique_commit(&repo2, "web-app--reported/mine", "mine.txt");

    let assert = rwv()
        .args([
            "workweave",
            "delete",
            "--project",
            "web-app",
            "reported",
            "--discard-uncommitted",
        ])
        .current_dir(&ws)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();

    assert!(
        !stderr.contains("web-app--reported/mine"),
        "a branch that is not the workweave's is not reported; got:\n{stderr}"
    );
    assert_eq!(
        common::git_in(&repo2, &["rev-parse", "web-app--reported/mine"]),
        leftover_sha,
        "the lookalike must still point at the operator's commit"
    );
}

#[test]
fn create_refuses_a_name_the_index_already_records() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "create", "--project", "web-app", "dup"])
        .current_dir(&ws)
        .assert()
        .success();

    let elsewhere = tmp.path().join("elsewhere");
    let assert = rwv()
        .args([
            "workweave",
            "create",
            "--project",
            "web-app",
            "dup",
            "--dir",
            elsewhere.to_str().unwrap(),
        ])
        .current_dir(&ws)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("rwv workweave create: projects/web-app: name taken (dup)\n")
            && stderr.contains("recorded at "),
        "the refusal must name the recorded duplicate; got:\n{stderr}"
    );
    assert!(
        stderr.contains("web-app--dup"),
        "the refusal must name the branch both would mint; got:\n{stderr}"
    );
    assert!(
        !elsewhere.exists(),
        "the duplicate must not be materialized at {}",
        elsewhere.display()
    );

    // The first workweave is untouched.
    assert!(weaveroot.join("web-app--dup").exists());
}

// ---------------------------------------------------------------------------
// R4 — a DESTROY-STORE requires the store to be unclaimed
// ---------------------------------------------------------------------------

/// `rwv member remove --delete` used to `remove_dir_all` the canonical store while a
/// workweave's live worktree was still linked into it: the workweave's `.git`
/// files point *inside* the directory being removed, so the whole workweave
/// is gutted. R4 refuses while any worktree is registered, and the same command
/// succeeds once the workweave is deleted (which removes the worktree and the
/// branch it was on) — the delete-then-prune ordering §5's `prune_dropped_repo`
/// row describes.
#[test]
fn remove_delete_refuses_a_claimed_store_and_succeeds_after_the_claim_is_gone() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace_with_project_repo(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    let repo = ws.join("github/org/repo");

    rwv()
        .args(["workweave", "create", "--project", "web-app", "live"])
        .current_dir(&ws)
        .assert()
        .success();
    let ww_checkout = weaveroot.join("web-app--live/github/org/repo");
    assert!(ww_checkout.exists(), "precondition: workweave checkout");

    let assert = rwv()
        .args([
            "member",
            "remove",
            "github/org/repo",
            "--delete",
            "--project",
            "web-app",
        ])
        .current_dir(&ws)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains(
            "rwv member remove: github/org/repo: checkout in another workspace (web-app--live)\n"
        ),
        "R4 refusal expected, naming the live workweave whose checkout claims the store; \
         got:\n{stderr}"
    );
    assert!(
        stderr.contains(
            "  free the other checkout\n    rwv workweave delete live --project web-app\n"
        ),
        "the refusal must offer the delete that retracts the workweave's claims; got:\n{stderr}"
    );
    assert!(
        repo.exists() && ww_checkout.exists(),
        "nothing may be destroyed by a refused DESTROY-STORE"
    );

    // Retire the claims in the order R4 requires, then the store may go.
    rwv()
        .args([
            "workweave",
            "delete",
            "--project",
            "web-app",
            "live",
            "--discard-uncommitted",
        ])
        .current_dir(&ws)
        .assert()
        .success();
    // Publish the clone's one branch, so the unpushed-work refusal that sits
    // beside R4 has nothing to say and the delete turns on R4 alone.
    let origin = tmp.path().join("origin.git");
    common::git_in(
        tmp.path(),
        &["init", "-q", "--bare", origin.to_str().unwrap()],
    );
    common::git_in(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    common::git_in(&repo, &["push", "-q", "-u", "origin", "HEAD"]);

    rwv()
        .args([
            "member",
            "remove",
            "github/org/repo",
            "--delete",
            "--project",
            "web-app",
        ])
        .current_dir(&ws)
        .assert()
        .success();
    assert!(
        !repo.exists(),
        "an unclaimed store is removable: {} should be gone",
        repo.display()
    );
}

// ---------------------------------------------------------------------------
// §6.1 — the Claude WorktreeRemove hook holds no consent
// ---------------------------------------------------------------------------

/// The hook used to pass both waivers unconditionally, which made it the one
/// path where a dirty *and* diverged workweave was destroyed with no operator
/// confirmation. It is fire-and-forget (always exits 0), so the assertion is
/// that the workweave and its work SURVIVE.
#[test]
fn claude_worktree_remove_hook_does_not_destroy_uncommitted_work() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "create", "--project", "web-app", "hooked"])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join("web-app--hooked");
    let ww_checkout = ww_dir.join("github/org/repo");
    std::fs::write(ww_checkout.join("README"), "edited, never committed").unwrap();

    let payload = serde_json::json!({
        "hook_event_name": "WorktreeRemove",
        "worktree_path": ww_dir.to_str().unwrap(),
    })
    .to_string();

    rwv()
        .args(["workweave", "--claude-hook"])
        .current_dir(&ws)
        .write_stdin(payload)
        .assert()
        .success(); // fire-and-forget: always exits 0

    assert!(
        ww_dir.exists(),
        "the hook must not destroy a workweave holding uncommitted work"
    );
    assert_eq!(
        std::fs::read_to_string(ww_checkout.join("README")).unwrap(),
        "edited, never committed",
        "the operator's edit must survive"
    );
}

/// A basename with no `--` used to be treated as the whole workweave name
/// and handed straight to `delete_workweave`, which resolves its target
/// from the NAME via the registry rather than from `worktree_path` — so a
/// fabricated name that happened to collide with a different registered
/// workweave would delete that one instead. The hook now identifies the
/// workweave by looking `worktree_path` up in the registry; when the
/// registry has no entry for it, it refuses rather than guesses.
#[test]
fn claude_worktree_remove_hook_refuses_a_basename_with_no_separator() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path());
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", "create", "--project", "web-app", "renamed"])
        .current_dir(&ws)
        .assert()
        .success();

    // The registry still names the original path; this directory is not
    // registered under its new name.
    let original = weaveroot.join("web-app--renamed");
    let no_separator = weaveroot.join("norwvseparator");
    std::fs::rename(&original, &no_separator).unwrap();

    let payload = serde_json::json!({
        "hook_event_name": "WorktreeRemove",
        "worktree_path": no_separator.to_str().unwrap(),
    })
    .to_string();

    let assert = rwv()
        .args(["workweave", "--claude-hook"])
        .current_dir(&ws)
        .write_stdin(payload)
        .assert()
        .success(); // fire-and-forget: always exits 0

    assert!(
        no_separator.exists(),
        "a directory the registry cannot identify by path must survive rather \
         than be deleted under a guessed name"
    );
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("refusing to delete"),
        "expected a refusal on stderr, got:\n{stderr}"
    );
}
