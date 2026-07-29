//! E2E tests for what `rwv update` does to a checkout's ref
//! (branch-model.md §5's two `update` rows, §5.3, §3.6).
//!
//! Before the branch model, `rwv update` resolved the remote branch tip and
//! ran `git checkout <sha>`, which detaches. That made "detached" the normal
//! resting state of every member (§6 item 2), and inside a workweave it
//! detached the ephemeral branch at the *identical* SHA while reporting
//! "advanced 1 repo(s)".
//!
//! The assertion shape the suite was missing is "which ref is this checkout
//! on" (§4.7). Every test here asserts it, because a run that lands on the
//! right SHA by detaching is exactly the outcome being ruled out.

use assert_cmd::Command;
use predicates::prelude::*;
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

struct Fixture {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    repo_path: String,
    bare: PathBuf,
    canonical: PathBuf,
}

impl Fixture {
    /// Advance `main` on the bare remote and return its new tip.
    fn advance_remote(&self) -> String {
        let parent = self.bare.parent().unwrap();
        let work = parent.join("__adv");
        git_run(
            parent,
            &["clone", self.bare.to_str().unwrap(), work.to_str().unwrap()],
        );
        git_run(&work, &["config", "user.email", "test@test.com"]);
        git_run(&work, &["config", "user.name", "Test"]);
        std::fs::write(work.join("advance.txt"), "advance").unwrap();
        git_run(&work, &["add", "."]);
        git_run(&work, &["commit", "-m", "advance"]);
        git_run(&work, &["push", "origin", "main"]);
        git_run(&work, &["rev-parse", "HEAD"])
    }

    fn head(&self) -> String {
        git_run(&self.canonical, &["rev-parse", "HEAD"])
    }
}

/// Workspace with one manifest repo (`version: main`), cloned on `main`, and
/// a lock recording its current tip.
fn build_workspace() -> Fixture {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

    let repo_path = "github/acme/a".to_string();
    let bare = tmp.path().join("a.git");
    init_bare_repo_with_commit(&bare);

    let canonical = workspace.join(&repo_path);
    std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
    git_run(
        tmp.path(),
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

    let project_dir = workspace.join("projects").join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    let bare_url = bare.to_str().unwrap();
    std::fs::write(
        project_dir.join("rwv.yaml"),
        format!(
            "repositories:\n  {repo_path}:\n    type: git\n    url: {bare_url}\n    \
             version: main\n    role: owned\n"
        ),
    )
    .unwrap();
    std::fs::write(
        project_dir.join("rwv.lock"),
        format!(
            "repositories:\n  {repo_path}:\n    type: git\n    url: {bare_url}\n    \
             version: {head}\n"
        ),
    )
    .unwrap();
    std::fs::write(workspace.join(".rwv-active"), "my-app\n").unwrap();

    Fixture {
        _tmp: tmp,
        workspace,
        repo_path,
        bare,
        canonical,
    }
}

// ============================================================================
// Canonical, attached to the tracking declaration's local counterpart:
// a MOVE of that branch, not a detach.
// ============================================================================

#[test]
fn update_fast_forwards_the_counterpart_and_stays_attached() {
    let fx = build_workspace();
    let tip = fx.advance_remote();

    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(fx.head(), tip, "the checkout must land on the remote tip");
    assert_eq!(
        current_branch(&fx.canonical).as_deref(),
        Some("main"),
        "update advances the counterpart under the checkout; it does not \
         abandon the branch the operator's commits hang off"
    );
    assert_eq!(
        git_run(&fx.canonical, &["rev-parse", "main"]),
        tip,
        "the branch ref itself is what moved"
    );
}

#[test]
fn update_is_a_no_op_when_the_counterpart_is_already_at_the_tip() {
    let fx = build_workspace();
    let before = fx.head();

    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(fx.head(), before);
    assert_eq!(current_branch(&fx.canonical).as_deref(), Some("main"));
}

// ============================================================================
// §5.3 — update MOVEs only the tracking declaration's local counterpart.
// ============================================================================

#[test]
fn update_refuses_when_attached_to_a_branch_the_manifest_does_not_declare() {
    let fx = build_workspace();
    let before = fx.head();
    git_run(&fx.canonical, &["checkout", "-b", "feature"]);
    let tip = fx.advance_remote();

    // `feature` is at the merge-base of the remote tip, so advancing it WOULD
    // be a fast-forward. The refusal is therefore provably the relatedness
    // guard and not the fast-forward check: attachment is operator state, and
    // moving a personal bookmark changes what it means even when it strands
    // nothing (§8.3).
    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("is on branch 'feature'"))
        .stderr(predicate::str::contains("local counterpart ('main')"))
        .stderr(predicate::str::contains("--detach-checkouts"))
        .stderr(predicate::str::contains("is not a fast-forward").not());

    assert_eq!(current_branch(&fx.canonical).as_deref(), Some("feature"));
    assert_eq!(fx.head(), before, "the refusal moved nothing");
    assert_ne!(fx.head(), tip);
}

#[test]
fn update_detach_checkouts_detaches_rather_than_relocating_the_branch() {
    let fx = build_workspace();
    let before = fx.head();
    git_run(&fx.canonical, &["checkout", "-b", "feature"]);
    let tip = fx.advance_remote();

    rwv()
        .args(["update", "--detach-checkouts"])
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(fx.head(), tip, "the consent materializes the tip");
    assert_eq!(current_branch(&fx.canonical), None);
    assert_eq!(
        git_run(&fx.canonical, &["rev-parse", "feature"]),
        before,
        "the flag names a detach, so the personal branch is left where it was"
    );
}

// ============================================================================
// A non-fast-forward refuses, naming the two exits §5 states.
// ============================================================================

#[test]
fn update_refuses_a_non_fast_forward_naming_both_exits() {
    let fx = build_workspace();
    std::fs::write(fx.canonical.join("local.txt"), "local work").unwrap();
    git_run(&fx.canonical, &["add", "."]);
    git_run(&fx.canonical, &["commit", "-m", "local"]);
    let local = fx.head();
    let tip = fx.advance_remote();
    assert_ne!(local, tip);

    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a fast-forward"))
        // Exit (1): reconcile it yourself with ordinary git and re-run.
        .stderr(predicate::str::contains("git rebase"))
        // Exit (2): materialize the tip without moving your branch.
        .stderr(predicate::str::contains("--detach-checkouts"));

    assert_eq!(current_branch(&fx.canonical).as_deref(), Some("main"));
    assert_eq!(
        fx.head(),
        local,
        "the diverged commit is still checked out and still on the branch"
    );
    assert_eq!(git_run(&fx.canonical, &["rev-parse", "main"]), local);
}

// ============================================================================
// Detached members: a MOVE of HEAD, subject to §3.6.
// ============================================================================

#[test]
fn update_moves_an_already_detached_member_and_leaves_it_detached() {
    let fx = build_workspace();
    git_run(&fx.canonical, &["checkout", "--detach", "HEAD"]);
    let tip = fx.advance_remote();

    // Nothing is attached, so nothing can be abandoned: no consent required.
    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .success();

    assert_eq!(fx.head(), tip);
    assert_eq!(
        current_branch(&fx.canonical),
        None,
        "update does not reattach a detached member — that is --reattach-checkouts' job"
    );
}

#[test]
fn update_refuses_to_move_a_detached_member_that_is_mid_operation() {
    let fx = build_workspace();
    git_run(&fx.canonical, &["checkout", "--detach", "HEAD"]);
    let before = fx.head();
    // `Detached` collapses "rwv left this at a lock SHA" and "the operator is
    // stopped mid-bisect". Only the first is rwv's to move (§3.6).
    git_run(&fx.canonical, &["bisect", "start"]);
    let tip = fx.advance_remote();

    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .failure()
        .stderr(predicate::str::contains("mid-bisect"));

    assert_eq!(fx.head(), before, "the bisect's HEAD must not be yanked");
    assert_ne!(fx.head(), tip);
}

// ============================================================================
// §6.2 — "advanced N repo(s)" counts SHA deltas, not repos visited.
// ============================================================================

#[test]
fn update_counts_sha_deltas_not_repos_visited() {
    let fx = build_workspace();
    fx.advance_remote();

    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .success()
        .stdout(predicate::str::contains("advanced 1 repo(s)"));

    // Second run: nothing on the remote has moved, so nothing advances. The
    // old count reported every non-`Err` outcome, so this said "advanced 1".
    rwv()
        .arg("update")
        .current_dir(&fx.workspace)
        .assert()
        .success()
        .stdout(predicate::str::contains("advanced 0 repo(s)"));
}

// ============================================================================
// Q8 — update inside a workweave advances the ephemeral ref, and points at
// `rwv sync` when it cannot.
// ============================================================================

/// Create a workweave's marker and manifest without materializing a
/// worktree for the manifest repo — the layout `member_checkout_dir` falls
/// back to the canonical clone for.
fn add_workweave_without_slot(fx: &Fixture, name: &str) -> PathBuf {
    let dir = fx.workspace.join(".workweaves").join(name);
    std::fs::create_dir_all(dir.join("projects/my-app")).unwrap();
    for f in ["rwv.yaml", "rwv.lock"] {
        std::fs::copy(
            fx.workspace.join("projects/my-app").join(f),
            dir.join("projects/my-app").join(f),
        )
        .unwrap();
    }
    std::fs::write(
        dir.join(".rwv-workweave"),
        format!(
            "primary: {}\nproject: my-app\nparent: {}\n",
            fx.workspace.display(),
            fx.workspace.display()
        ),
    )
    .unwrap();
    dir
}

/// Add a workweave whose slot for the manifest repo is a real worktree of the
/// canonical store, on its own ephemeral branch.
fn add_workweave(fx: &Fixture, ephemeral: &str) -> PathBuf {
    let dir = add_workweave_without_slot(fx, "my-app--dev");
    let slot = dir.join(&fx.repo_path);
    std::fs::create_dir_all(slot.parent().unwrap()).unwrap();
    git_run(
        &fx.canonical,
        &[
            "worktree",
            "add",
            "-b",
            ephemeral,
            slot.to_str().unwrap(),
            "HEAD",
        ],
    );
    dir
}

#[test]
fn update_inside_a_workweave_advances_the_ephemeral_ref_without_detaching() {
    let fx = build_workspace();
    let ww = add_workweave(&fx, "my-app--dev");
    let slot = ww.join(&fx.repo_path);
    let tip = fx.advance_remote();

    rwv()
        .args(["update", "--repo", &fx.repo_path])
        .current_dir(&ww)
        .assert()
        .success();

    assert_eq!(git_run(&slot, &["rev-parse", "HEAD"]), tip);
    assert_eq!(
        current_branch(&slot).as_deref(),
        Some("my-app--dev"),
        "the ephemeral ref is advanced, not abandoned — the shipped path \
         detached it (at the identical SHA) while claiming to have advanced it"
    );
    assert_eq!(git_run(&slot, &["rev-parse", "my-app--dev"]), tip);
}

#[test]
fn update_inside_a_workweave_points_at_rwv_sync_when_it_is_not_a_fast_forward() {
    let fx = build_workspace();
    let ww = add_workweave(&fx, "my-app--dev");
    let slot = ww.join(&fx.repo_path);
    git_run(&slot, &["config", "user.email", "test@test.com"]);
    git_run(&slot, &["config", "user.name", "Test"]);
    std::fs::write(slot.join("ww.txt"), "workweave work").unwrap();
    git_run(&slot, &["add", "."]);
    git_run(&slot, &["commit", "-m", "workweave work"]);
    let diverged = git_run(&slot, &["rev-parse", "HEAD"]);
    fx.advance_remote();

    rwv()
        .args(["update", "--repo", &fx.repo_path])
        .current_dir(&ww)
        .assert()
        .failure()
        .stderr(predicate::str::contains("is not a fast-forward"))
        .stderr(predicate::str::contains("rwv sync"))
        // The workweave arm offers no detach: `rwv sync` is the verb that
        // reconciles a workweave with its parent.
        .stderr(predicate::str::contains("--detach-checkouts").not());

    assert_eq!(git_run(&slot, &["rev-parse", "HEAD"]), diverged);
    assert_eq!(current_branch(&slot).as_deref(), Some("my-app--dev"));
}

// ============================================================================
// A workweave that has not materialized a given member: the run is inside a
// workweave, but this member's checkout is the canonical clone, not a slot.
// ============================================================================

#[test]
fn update_inside_a_workweave_falls_through_to_the_canonical_clone_when_the_member_has_no_slot() {
    let fx = build_workspace();
    let ww = add_workweave_without_slot(&fx, "my-app--dev");
    let tip = fx.advance_remote();

    rwv().arg("update").current_dir(&ww).assert().success();

    assert_eq!(
        fx.head(),
        tip,
        "no slot exists for this member, so the canonical clone is what advanced"
    );
    assert_eq!(
        current_branch(&fx.canonical).as_deref(),
        Some("main"),
        "the counterpart branch moved — the canonical arm ran, not the \
         workweave arm, which would have moved an ephemeral ref instead"
    );
    assert!(
        !ww.join(&fx.repo_path).exists(),
        "the run must not have materialized a slot as a side effect"
    );
}
