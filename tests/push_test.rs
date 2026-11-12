//! E2E tests for `rwv push`.
//!
//! Each test sets up a workspace with:
//!   - one or more bare "manifest" remotes
//!   - local manifest-repo clones at canonical paths
//!   - a bare "project" remote with rwv.toml + rwv.lock committed
//!   - a local project-repo clone under `projects/<name>/`
//!   - `.rwv-active` pointing at the project
//!
//! Then exercises `rwv push` via the CLI and asserts the publish-ordering
//! invariant and the precondition refuse-paths from the spec.

use assert_cmd::Command;
use std::path::Path;

mod common;

fn rwv() -> Command {
    common::rwv()
}

/// Get the `main` SHA in a bare repo, or `None` if `main` doesn't exist
/// (e.g. nothing was ever pushed).
fn bare_main_sha(bare: &Path) -> Option<String> {
    let output = common::git()
        .args(["rev-parse", "main"])
        .current_dir(bare)
        .output()
        .expect("git should be available");
    if output.status.success() {
        Some(String::from_utf8(output.stdout).unwrap().trim().to_string())
    } else {
        None
    }
}

// ============================================================================
// Happy path
// ============================================================================

/// Happy path: bare `rwv push` pushes Owned + Fork; Dependency repos are
/// skipped (plan-time default). The project repo is pushed last.
#[test]
fn push_happy_path_pushes_manifest_then_project() {
    let ws = common::build_workspace(
        "alpha",
        &[
            ("local/org/a", "owned"),
            ("local/org/b", "fork"),
            ("local/org/c", "dependency"),
        ],
    );

    // Advance each manifest repo with a new commit so there's something to
    // push. Re-write the lock to match the new HEADs of the repos the push
    // acts on; the dependency is advanced too, but its pin stays where it
    // was, a commit its remote holds.
    let repos = [
        ("local/org/a", "owned"),
        ("local/org/b", "fork"),
        ("local/org/c", "dependency"),
    ];
    let mut manifest_yaml = String::from("[repositories]\n");
    let mut lock_entries: Vec<(String, String, String)> = Vec::new();
    let mut expected_shas: Vec<(String, String)> = Vec::new();
    for (rp, role) in &repos {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let local = ws.workspace.join(rp);
        std::fs::write(local.join("changed.txt"), "new").unwrap();
        common::git_in(&local, &["add", "."]);
        common::git_in(&local, &["commit", "-m", "advance"]);
        let sha = common::git_in(&local, &["rev-parse", "HEAD"]);
        let pinned = if *role == "dependency" {
            common::git_in(&local, &["rev-parse", "HEAD~1"])
        } else {
            sha.clone()
        };
        let bare_url = common::file_url(bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{rp}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nbranch = \"main\"\nrole = \"{role}\"\n"
        ));
        lock_entries.push(((*rp).to_string(), bare_url, pinned));
        expected_shas.push(((*rp).to_string(), sha));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();
    let entries: Vec<(&str, &str, &str)> = lock_entries
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &entries);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "advance lock"]);
    let project_head = common::git_in(&project_dir, &["rev-parse", "HEAD"]);

    // Record the dependency's baseline SHA before push (it must NOT advance).
    let (_, dep_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/c")
        .unwrap();
    let dep_baseline = bare_main_sha(dep_bare);

    rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    // Owned + Fork repos must advance; Dependency must NOT (default plan skips it).
    let (_, owned_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/a")
        .unwrap();
    let (_, fork_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/b")
        .unwrap();
    let (_, owned_sha) = expected_shas
        .iter()
        .find(|(p, _)| p == "local/org/a")
        .unwrap();
    let (_, fork_sha) = expected_shas
        .iter()
        .find(|(p, _)| p == "local/org/b")
        .unwrap();

    assert_eq!(
        bare_main_sha(owned_bare),
        Some(owned_sha.clone()),
        "owned repo should be pushed"
    );
    assert_eq!(
        bare_main_sha(fork_bare),
        Some(fork_sha.clone()),
        "fork repo should be pushed"
    );
    assert_eq!(
        bare_main_sha(dep_bare),
        dep_baseline,
        "dependency bare must NOT advance under bare rwv push (default plan skips non-writable roles)"
    );
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        Some(project_head),
        "project bare should match local project HEAD"
    );
}

#[test]
fn push_dry_run_prints_plan_and_pushes_nothing() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);

    // Capture the baseline manifest-bare SHA before dry-run.
    let (_, manifest_bare) = &ws.manifest_bares[0];
    let baseline_manifest = bare_main_sha(manifest_bare);
    let baseline_project = bare_main_sha(&ws.project_bare);

    // Make a local advance so a real push would change things.
    let local = ws.workspace.join("local/org/a");
    std::fs::write(local.join("x.txt"), "x").unwrap();
    common::git_in(&local, &["add", "."]);
    common::git_in(&local, &["commit", "-m", "advance"]);
    let new_sha = common::git_in(&local, &["rev-parse", "HEAD"]);

    // Rewrite lock to match the new HEAD so the precondition passes.
    let bare_url = common::file_url(manifest_bare);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    common::fixture_lock(&project_dir, &[("local/org/a", &bare_url, &new_sha)]);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "relock"]);

    let output = rwv()
        .args(["push", "--dry-run"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push --dry-run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "dry-run should succeed: {stdout}");
    assert!(
        stdout.contains("dry-run"),
        "dry-run output should announce itself; got: {stdout}"
    );
    assert!(
        stdout.contains("local/org/a"),
        "dry-run should list manifest repo; got: {stdout}"
    );
    assert!(
        stdout.contains("projects/alpha"),
        "dry-run should list project repo; got: {stdout}"
    );

    // Nothing should have moved.
    assert_eq!(
        bare_main_sha(manifest_bare),
        baseline_manifest,
        "dry-run must not touch the manifest bare"
    );
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        baseline_project,
        "dry-run must not touch the project bare"
    );
}

// ============================================================================
// Negative: lock-state mismatch — bail before touching the network
// ============================================================================

#[test]
fn push_refuses_when_lock_disagrees_with_local_state() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let (_, manifest_bare) = &ws.manifest_bares[0];
    let baseline_manifest = bare_main_sha(manifest_bare);
    let short_lock = &baseline_manifest
        .clone()
        .expect("bare should have a main ref")[..12];

    // Advance the local repo WITHOUT updating the lock.
    let local = ws.workspace.join("local/org/a");
    std::fs::write(local.join("drift.txt"), "drift").unwrap();
    common::git_in(&local, &["add", "."]);
    common::git_in(&local, &["commit", "-m", "drift past lock"]);
    let drifted_head = common::git_in(&local, &["rev-parse", "HEAD"]);
    let short_head = &drifted_head[..12];

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "push must refuse when lock and state disagree"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rwv lock --commit"),
        "error should name `rwv lock --commit`; got: {stderr}"
    );
    // The per-repo line must attach each abbreviated SHA to its own label —
    // swap them and an operator resets the wrong side.
    assert!(
        stderr.contains(&format!(
            "local/org/a: pin unpinned (HEAD {short_head} is 1 commit beyond the pin {short_lock})\n"
        )),
        "expected each label attached to its own 12-character id; got: {stderr}"
    );
    // Network must not have been touched.
    assert_eq!(
        bare_main_sha(manifest_bare),
        baseline_manifest,
        "lock-mismatch refuse must happen before any push"
    );
}

// ============================================================================
// Negative: uncommitted project state — push publishes the last commit, so
// what the precondition reads on disk must be what that commit holds
// ============================================================================

/// Run `rwv push`, require the uncommitted-project-state refusal naming
/// `files`, and require that no remote moved. `dry_run` additionally adds
/// `--dry-run` to the invocation — the precondition must still fire, and
/// unmoved bares are then the only evidence a subprocess test can see that
/// dry-run's own announcement never got the chance to skip it.
fn assert_push_refuses_uncommitted(ws: &common::PushWorkspace, files: &str, dry_run: bool) {
    let manifest_before: Vec<Option<String>> = ws
        .manifest_bares
        .iter()
        .map(|(_, bare)| bare_main_sha(bare))
        .collect();
    let project_before = bare_main_sha(&ws.project_bare);

    let mut args = vec!["push"];
    if dry_run {
        args.push("--dry-run");
    }
    let output = rwv()
        .args(args)
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(1),
        "push must refuse uncommitted project state; stderr: {stderr}"
    );
    let project = &ws.project_name;
    let project_dir = ws.workspace.join("projects").join(project);
    let lock_files_only = files == "rwv.lock" || files == "rwv.toml";
    let (state, choice) = if lock_files_only {
        (
            "rwv.toml or rwv.lock uncommitted",
            "    rwv lock --commit\n".to_owned(),
        )
    } else {
        (
            "dirty",
            format!("    git -C {} commit\n", project_dir.display()),
        )
    };
    for needle in [
        format!("rwv push: projects/{project}: edits {state}\n    {files}\n"),
        choice,
        "\n\nrwv explain edits\n".to_string(),
    ] {
        assert!(
            stderr.contains(&needle),
            "expected {needle:?} in stderr: {stderr}"
        );
    }

    let manifest_after: Vec<Option<String>> = ws
        .manifest_bares
        .iter()
        .map(|(_, bare)| bare_main_sha(bare))
        .collect();
    assert_eq!(
        manifest_after, manifest_before,
        "the refusal must come before any member push"
    );
    assert_eq!(bare_main_sha(&ws.project_bare), project_before);
}

/// `rwv lock` (or `rwv update`) without `--commit`, then `rwv push`: the lock
/// on disk matches every member HEAD, but the commit push would publish
/// still carries the old one.
#[test]
fn push_refuses_an_uncommitted_relock() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let local = ws.workspace.join("local/org/a");
    std::fs::write(local.join("advance.txt"), "advance").unwrap();
    common::git_in(&local, &["add", "."]);
    common::git_in(&local, &["commit", "-m", "advance past lock"]);

    rwv()
        .args(["lock"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    assert_push_refuses_uncommitted(&ws, "rwv.lock", false);
}

/// B1: preconditions run "before any network call, under `--dry-run` too"
/// (cli.md:315, docs/how-to/push-cross-repo-feature.md:11) — the dry-run branch
/// (src/push.rs's `if dry_run` print-the-plan step) sits after every
/// `refuse!`, so passing `--dry-run` must not let this one through.
/// Revert to watch it go red: wrap the `refuse_uncommitted_project_state`
/// call in `run_push` (src/push.rs) in `if !dry_run { ... }`.
#[test]
fn push_refuses_an_uncommitted_relock_under_dry_run() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let local = ws.workspace.join("local/org/a");
    std::fs::write(local.join("advance.txt"), "advance").unwrap();
    common::git_in(&local, &["add", "."]);
    common::git_in(&local, &["commit", "-m", "advance past lock"]);

    rwv()
        .args(["lock"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    assert_push_refuses_uncommitted(&ws, "rwv.lock", true);
}

/// A manifest or lock that was never committed is read by the precondition
/// and never published by the push.
#[test]
fn push_refuses_an_untracked_manifest_or_lock() {
    for file in ["rwv.toml", "rwv.lock"] {
        let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
        let project_dir = ws.workspace.join("projects").join(&ws.project_name);
        common::git_in(&project_dir, &["rm", "--cached", "--quiet", file]);
        common::git_in(&project_dir, &["commit", "-m", &format!("untrack {file}")]);

        assert_push_refuses_uncommitted(&ws, file, false);
    }
}

#[test]
fn push_refuses_any_tracked_dirt_in_the_project_repo() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("notes.md"), "draft\n").unwrap();
    common::git_in(&project_dir, &["add", "notes.md"]);
    common::git_in(&project_dir, &["commit", "-m", "notes"]);
    std::fs::write(project_dir.join("notes.md"), "draft, edited\n").unwrap();

    assert_push_refuses_uncommitted(&ws, "notes.md", false);
}

/// Untracked scratch is not part of anything push reads or publishes.
#[test]
fn push_proceeds_past_untracked_scratch_in_the_project_repo() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("scratch.txt"), "scratch\n").unwrap();

    rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .assert()
        .success();
}

// ============================================================================
// Negative: detached HEAD refused
// ============================================================================

#[test]
fn push_refuses_detached_head() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    // Detach HEAD in the manifest repo.
    let local = ws.workspace.join("local/org/a");
    let head_sha = common::git_in(&local, &["rev-parse", "HEAD"]);
    common::git_in(&local, &["checkout", &head_sha]);

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(!output.status.success(), "detached HEAD must refuse");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("detached"),
        "error should mention detached HEAD; got: {stderr}"
    );
}

// ============================================================================
// A recorded feature branch publishes from the primary
// ============================================================================

/// A feature's member branch is published when it holds commits the target's
/// pin lacks, even when some other remote branch already carries them.
#[test]
fn a_feature_member_branch_holding_commits_beyond_the_targets_pin_is_published() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    common::git_in(&project_dir, &["checkout", "-b", "feat/y"]);
    common::record_feature(&project_dir, "feat/y", "main");
    rwv()
        .args(["update"])
        .current_dir(&ws.workspace)
        .assert()
        .success();
    let member = ws.workspace.join("local/org/a");
    std::fs::write(member.join("y.txt"), "y").unwrap();
    common::git_in(&member, &["add", "."]);
    common::git_in(&member, &["commit", "-m", "feature work"]);
    common::git_in(&member, &["push", "origin", "HEAD:refs/heads/elsewhere"]);
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&ws.manifest_bares[0].1, &["rev-parse", "refs/heads/feat/y"]),
        common::git_in(&member, &["rev-parse", "HEAD"]),
        "the branch holds commits the target's pin lacks, so it is published"
    );
}

/// A feature's member branch at the target's pin holds nothing the pin lacks
/// and is not published.
#[test]
fn a_feature_member_branch_at_the_targets_pin_is_not_published() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    common::git_in(&project_dir, &["checkout", "-b", "feat/z"]);
    common::record_feature(&project_dir, "feat/z", "main");
    rwv()
        .args(["update"])
        .current_dir(&ws.workspace)
        .assert()
        .success();
    rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .assert()
        .success();
    let none = common::git()
        .args(["rev-parse", "--verify", "--quiet", "refs/heads/feat/z"])
        .current_dir(&ws.manifest_bares[0].1)
        .output()
        .unwrap();
    assert!(
        none.stdout.is_empty(),
        "nothing beyond the target's pin to publish"
    );
}

/// On a declared branch, a branch some commit in the pushed range declared is
/// published with the branch the tip declares.
#[test]
fn a_branch_a_commit_in_the_pushed_range_declared_is_published() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    let member = ws.workspace.join("local/org/a");
    common::git_in(&member, &["branch", "side"]);
    let manifest = std::fs::read_to_string(project_dir.join("rwv.toml")).unwrap();
    std::fs::write(
        project_dir.join("rwv.toml"),
        manifest.replace("branch = \"main\"", "branch = \"side\""),
    )
    .unwrap();
    common::git_in(&project_dir, &["commit", "-qam", "declare side"]);
    std::fs::write(project_dir.join("rwv.toml"), &manifest).unwrap();
    common::git_in(&project_dir, &["commit", "-qam", "declare main again"]);

    rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    assert_eq!(
        common::git_in(&ws.manifest_bares[0].1, &["rev-parse", "refs/heads/side"]),
        common::git_in(&member, &["rev-parse", "side"]),
        "the branch the range declared is published"
    );
}

/// A dry run says what it judged from, and how old that is.
#[test]
fn push_dry_run_says_it_judged_refs_as_last_fetched() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let out = rwv()
        .args(["push", "--dry-run"])
        .current_dir(&ws.workspace)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("judged on refs as last fetched (last fetch")
            || stdout.contains("judged on refs as last fetched (no fetch on record)"),
        "{stdout}"
    );
}

/// A publishing push leaves `rwv.lock` as the committed lock holds it: it
/// judges the pins and never writes them.
#[test]
fn push_leaves_the_lock_byte_for_byte_as_committed() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    let lock_before = std::fs::read(project_dir.join("rwv.lock")).unwrap();
    let head_before = common::git_in(&project_dir, &["rev-parse", "HEAD"]);

    rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    assert_eq!(
        std::fs::read(project_dir.join("rwv.lock")).unwrap(),
        lock_before
    );
    assert_eq!(
        common::git_in(&project_dir, &["rev-parse", "HEAD"]),
        head_before,
        "and no commit was made"
    );
}

/// `--branch` names a weave branch that exists; a name nothing has refuses
/// before any remote is touched.
#[test]
fn push_branch_refuses_a_name_the_project_repo_lacks() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let output = rwv()
        .args(["push", "--branch", "nosuch"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("projects/alpha: name no-such-branch (nosuch)"),
        "got: {stderr}"
    );
}

/// The project repo need not be on the remote's default branch: a push
/// publishes the recorded branch it is on, and the record before it.
#[test]
fn push_publishes_a_recorded_feature_branch_from_the_primary() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    common::git_in(&project_dir, &["checkout", "-b", "feat/x"]);
    common::record_feature(&project_dir, "feat/x", "main");
    rwv()
        .args(["update"])
        .current_dir(&ws.workspace)
        .assert()
        .success();
    let member = ws.workspace.join("local/org/a");
    std::fs::write(member.join("x.txt"), "x").unwrap();
    common::git_in(&member, &["add", "."]);
    common::git_in(&member, &["commit", "-m", "feature work"]);
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let head = common::git_in(&project_dir, &["rev-parse", "HEAD"]);
    let member_head = common::git_in(&member, &["rev-parse", "HEAD"]);
    assert_eq!(
        common::git_in(&ws.manifest_bares[0].1, &["rev-parse", "refs/heads/feat/x"]),
        member_head,
        "the member's branch is published"
    );
    let on_bare = common::git_in(&ws.project_bare, &["rev-parse", "refs/heads/feat/x"]);
    assert_eq!(
        on_bare, head,
        "the recorded branch is on the project remote"
    );
    let record = common::git_in(&ws.project_bare, &["show", "rwv-records:feat/x"]);
    assert!(
        record.contains("feature") && record.contains("main"),
        "the remote holds the branch's record beside it; got: {record}"
    );
}

/// Another machine's entry that reaches this clone's view of the remote while
/// a push is publishing a record, by a fetch run beside it, is still on the
/// remote after the push: the push never drops an entry its own merge of the
/// records did not see.
#[cfg(feature = "test-pause")]
#[test]
fn a_record_another_machine_published_survives_a_fetch_during_the_publish() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    common::git_in(&project_dir, &["checkout", "-b", "feat/x"]);
    common::record_feature(&project_dir, "feat/x", "main");
    rwv()
        .args(["update"])
        .current_dir(&ws.workspace)
        .assert()
        .success();
    let member = ws.workspace.join("local/org/a");
    std::fs::write(member.join("x.txt"), "x").unwrap();
    common::git_in(&member, &["add", "."]);
    common::git_in(&member, &["commit", "-m", "feature work"]);
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let on_remote = common::git()
        .args(["rev-parse", "-q", "--verify", "refs/heads/rwv-records"])
        .current_dir(&ws.project_bare)
        .output()
        .unwrap();
    let parent = String::from_utf8(on_remote.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let theirs = common::records::commit(
        &project_dir,
        (!parent.is_empty()).then_some(parent.as_str()),
        &[("colleague", "mode = \"declared\"\n")],
        &[],
        "another machine's record",
    );

    let mut push = common::rwv_std();
    push.args(["push"]).current_dir(&ws.workspace);
    let held = common::pause::Paused::spawn(
        push,
        "records-publish-settled",
        &ws._tmp.path().join("pause"),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    common::git_in(
        &ws.project_bare,
        &["fetch", "-q", &common::file_url(&project_dir), &theirs],
    );
    common::git_in(
        &ws.project_bare,
        &["update-ref", "refs/heads/rwv-records", &theirs],
    );
    common::git_in(&project_dir, &["fetch", "-q", "origin"]);
    let out = held.release();
    assert!(
        out.status.success(),
        "push failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let listing = common::git_in(
        &ws.project_bare,
        &["ls-tree", "-r", "--name-only", "rwv-records"],
    );
    assert!(
        listing.lines().any(|name| name == "colleague"),
        "the other machine's entry was dropped from the remote; it holds:\n{listing}\npush said:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        listing.lines().any(|name| name == "feat/x"),
        "the pushed branch's record is on the remote; it holds:\n{listing}"
    );
}

// ============================================================================
// Negative: project repo detached or unborn — `head-not-on-branch`
// ============================================================================

/// A project repo on a detached HEAD is a detached workspace, which runs no
/// push: it refuses on the `alignment` axis, project detached — distinct from
/// `push_refuses_detached_head` above, which detaches a *member* repo.
#[test]
fn push_refuses_project_repo_detached_head_by_route_token() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    let head_sha = common::git_in(&project_dir, &["rev-parse", "HEAD"]);
    common::git_in(&project_dir, &["checkout", &head_sha]);

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "a detached project repo must refuse"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("alignment project detached")
            && stderr.contains("\nrwv explain alignment\n"),
        "expected the `alignment` project detached refusal; got: {stderr}"
    );
}

/// B2's other case: an unborn project repo (HEAD symbolic, no commits yet)
/// also refuses with `head-not-on-branch`. Revert to watch it go red: in
/// `run_push` (src/push.rs), change the project-repo
/// `HeadAttachment::Unborn` arm's `RefusalKind::HeadNotOnBranch` to
/// `RefusalKind::ProjectRepoOffCanonicalBranch`.
#[test]
fn push_refuses_project_repo_unborn_head_by_route_token() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    // `checkout --orphan` leaves HEAD symbolic but pointing at a branch with
    // no commits — `rwv.toml`/`rwv.lock` stay on disk (still tracked from
    // the prior HEAD), but nothing is committed on the new branch yet.
    common::git_in(&project_dir, &["checkout", "--orphan", "newborn"]);
    common::record_feature(&project_dir, "newborn", "main");

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "an unborn project repo must refuse"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("\nrwv explain head-not-on-branch\n"),
        "expected the `head-not-on-branch` route token; got: {stderr}"
    );
}

// ============================================================================
// The remote's default branch is not read to gate a push
// ============================================================================

/// No gate reads the remote's default branch, so a project repo whose
/// `origin/HEAD` is unset publishes.
#[test]
fn push_does_not_read_the_remotes_default_branch_to_gate() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    common::git_in(
        &project_dir,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );

    rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .assert()
        .success();
}

// ============================================================================
// Negative: project repo directory is not a VCS repo at all
// ============================================================================

/// A non-repo `projects/<name>/` must surface as `NotARepo`, not be
/// misreported as a detached HEAD (the shipped bug `current_ref`'s
/// `Ok(None)` collapse produced).
#[test]
fn push_refuses_when_project_repo_is_not_a_repo() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::remove_dir_all(project_dir.join(".git")).unwrap();

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "a project dir with no .git must refuse"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a vcs repository") || stderr.contains("not a repo"),
        "error should name the not-a-repo condition; got: {stderr}"
    );
    assert!(
        !stderr.contains("detached"),
        "a non-repo directory must not be misreported as a detached HEAD; got: {stderr}"
    );
}

// ============================================================================
// A member off the branch the checked-out branch's record expects refuses
// ============================================================================

#[test]
fn push_refuses_a_member_on_another_branch_than_the_record_expects() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);

    let local = ws.workspace.join("local/org/a");
    common::git_in(&local, &["checkout", "-b", "feat-x"]);
    std::fs::write(local.join("f.txt"), "f").unwrap();
    common::git_in(&local, &["add", "."]);
    common::git_in(&local, &["commit", "-m", "feat advance"]);
    let feat_sha = common::git_in(&local, &["rev-parse", "HEAD"]);

    let (_, bare) = &ws.manifest_bares[0];
    let bare_url = common::file_url(bare);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    common::fixture_lock(&project_dir, &[("local/org/a", &bare_url, &feat_sha)]);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "relock"]);

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(!output.status.success(), "an unaligned member refuses");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("local/org/a: alignment on branch (feat-x)")
            && stderr.contains("rwv update"),
        "expected the alignment refusal naming `rwv update`; got: {stderr}"
    );
    assert!(
        common::git()
            .args(["rev-parse", "--verify", "--quiet", "feat-x"])
            .current_dir(bare)
            .output()
            .unwrap()
            .stdout
            .is_empty(),
        "nothing was published before the refusal"
    );
}

// ============================================================================
// Negative: manifest-repo push failure — project repo NOT pushed
// ============================================================================

#[test]
fn push_aborts_before_project_when_manifest_push_fails() {
    // Build a workspace; then break a manifest bare so its push fails. The
    // project bare must remain untouched.
    let ws = common::build_workspace(
        "alpha",
        &[("local/org/a", "owned"), ("local/org/b", "owned")],
    );

    let baseline_project = bare_main_sha(&ws.project_bare);

    // Advance both local repos.
    let mut lock_entries: Vec<(String, String, String)> = Vec::new();
    let mut expected_shas: Vec<String> = Vec::new();
    for (rp, bare) in &ws.manifest_bares {
        let local = ws.workspace.join(rp);
        std::fs::write(local.join("x.txt"), "x").unwrap();
        common::git_in(&local, &["add", "."]);
        common::git_in(&local, &["commit", "-m", "advance"]);
        let sha = common::git_in(&local, &["rev-parse", "HEAD"]);
        expected_shas.push(sha.clone());
        let bare_url = common::file_url(bare);
        lock_entries.push((rp.clone(), bare_url, sha));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    let entries: Vec<(&str, &str, &str)> = lock_entries
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &entries);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "relock"]);

    // Make repo B's remote refuse the push. Repo A pushes succeed; B fails;
    // project must not be pushed.
    common::refuse_pushes_to(&ws.manifest_bares[1].1, "refs/heads/main");

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "push must fail when any manifest push fails"
    );
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        baseline_project,
        "project bare must NOT advance when a manifest push fails"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("project repo not pushed") || stderr.contains("member-side partial state"),
        "error should mention partial state / project-not-pushed; got: {stderr}"
    );
}

// ============================================================================
// Negative: project-repo push failure — manifest repos already pushed
// ============================================================================

#[test]
fn push_surfaces_project_push_failure_after_manifest_pushed() {
    let ws = common::build_workspace("alpha", &[("local/org/a", "owned")]);

    // Advance the manifest repo and the lock so the precondition passes.
    let (_, manifest_bare) = &ws.manifest_bares[0];
    let local = ws.workspace.join("local/org/a");
    std::fs::write(local.join("x.txt"), "x").unwrap();
    common::git_in(&local, &["add", "."]);
    common::git_in(&local, &["commit", "-m", "advance"]);
    let manifest_sha = common::git_in(&local, &["rev-parse", "HEAD"]);

    let bare_url = common::file_url(manifest_bare);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    common::fixture_lock(&project_dir, &[("local/org/a", &bare_url, &manifest_sha)]);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "relock"]);

    // Make the project remote refuse the branch, not the record beside it.
    common::refuse_pushes_to(&ws.project_bare, "refs/heads/main");

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "push must fail when project-repo push fails"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("project-repo push") || stderr.contains("lock carrier is not"),
        "error should surface project-side failure clearly; got: {stderr}"
    );

    // The manifest bare DID move (manifest repos pushed before the project
    // attempt). This is the surface-clearly behavior the spec requires.
    assert_eq!(
        bare_main_sha(manifest_bare),
        Some(manifest_sha),
        "manifest repo should already be pushed before project-repo failure"
    );
}

// ============================================================================
// CLI plumbing
// ============================================================================

#[test]
fn push_command_is_registered() {
    let output = rwv()
        .args(["push", "--help"])
        .output()
        .expect("rwv push --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        combined.contains("--dry-run")
            && combined.contains("--overwrite")
            && combined.contains("--branch")
            && !combined.contains("--force"),
        "push --help should list --dry-run, --overwrite and --branch, and no --force; got: {combined}"
    );
}

#[test]
fn push_requires_a_workspace() {
    let tmp = common::tempdir().unwrap();
    let output = rwv()
        .args(["push"])
        .current_dir(tmp.path())
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "push outside a workspace must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no repoweave workspace"),
        "error should mention no workspace; got: {stderr}"
    );
}

// ============================================================================
// `--role` / `--repo` filter
// ============================================================================

/// Build a workspace + advance every manifest repo + write a matching lock.
/// Common setup for the filter tests so each test can focus on the
/// flag-specific assertions.
fn build_workspace_with_advances(
    project_name: &str,
    repos: &[(&str, &str)],
) -> (common::PushWorkspace, Vec<(String, String)>) {
    let ws = common::build_workspace(project_name, repos);

    // Advance each manifest repo with a distinct commit so SHAs differ.
    let mut manifest_yaml = String::from("[repositories]\n");
    let mut lock_entries: Vec<(String, String, String)> = Vec::new();
    let mut expected_shas: Vec<(String, String)> = Vec::new();
    for ((rp, bare), (_, role)) in ws.manifest_bares.iter().zip(repos.iter()) {
        let local = ws.workspace.join(rp);
        std::fs::write(local.join(format!("{}.txt", rp.replace('/', "_"))), rp).unwrap();
        common::git_in(&local, &["add", "."]);
        common::git_in(&local, &["commit", "-m", &format!("advance {rp}")]);
        let sha = common::git_in(&local, &["rev-parse", "HEAD"]);
        let bare_url = common::file_url(bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{rp}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nbranch = \"main\"\nrole = \"{role}\"\n"
        ));
        lock_entries.push((rp.clone(), bare_url, sha.clone()));
        expected_shas.push((rp.clone(), sha));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();
    let entries: Vec<(&str, &str, &str)> = lock_entries
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &entries);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "advance lock"]);

    (ws, expected_shas)
}

#[test]
fn push_role_filter_only_pushes_matching_role() {
    let (ws, expected_shas) = build_workspace_with_advances(
        "alpha",
        &[("local/org/p", "owned"), ("local/org/d", "dependency")],
    );
    let baseline_d = bare_main_sha(&ws.manifest_bares[1].1);

    rwv()
        .args(["push", "--role", "owned"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let (p_path, p_expected) = &expected_shas[0];
    let (_, p_bare) = ws.manifest_bares.iter().find(|(p, _)| p == p_path).unwrap();
    assert_eq!(
        bare_main_sha(p_bare),
        Some(p_expected.clone()),
        "primary repo bare must advance"
    );

    assert_eq!(
        bare_main_sha(&ws.manifest_bares[1].1),
        baseline_d,
        "dependency bare must NOT advance under --role owned"
    );
}

#[test]
fn push_repo_exact_filter_pushes_only_that_path() {
    let (ws, expected_shas) = build_workspace_with_advances(
        "alpha",
        &[("local/org/a", "owned"), ("local/org/b", "owned")],
    );
    let baseline_b = bare_main_sha(&ws.manifest_bares[1].1);

    rwv()
        .args(["push", "--repo", "local/org/a"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    assert_eq!(
        bare_main_sha(&ws.manifest_bares[0].1),
        Some(expected_shas[0].1.clone())
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[1].1),
        baseline_b,
        "b bare must NOT advance"
    );
}

#[test]
fn push_repo_glob_filter_pushes_matching() {
    let (ws, expected_shas) = build_workspace_with_advances(
        "alpha",
        &[
            ("local/org/a", "owned"),
            ("local/org/b", "owned"),
            ("local/other/c", "owned"),
        ],
    );
    let baseline_c = bare_main_sha(&ws.manifest_bares[2].1);

    rwv()
        .args(["push", "--repo", "glob:local/org/*"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    for (i, (rp, expected)) in expected_shas.iter().enumerate() {
        let bare = &ws.manifest_bares[i].1;
        if rp.starts_with("local/org/") {
            assert_eq!(bare_main_sha(bare), Some(expected.clone()), "{rp}");
        }
    }
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[2].1),
        baseline_c,
        "other/c must NOT advance"
    );
}

#[test]
fn push_repo_regex_filter_pushes_matching() {
    let (ws, expected_shas) = build_workspace_with_advances(
        "alpha",
        &[
            ("local/cwalv/a", "owned"),
            ("local/cwalv/b", "owned"),
            ("local/other/c", "owned"),
        ],
    );
    let baseline_c = bare_main_sha(&ws.manifest_bares[2].1);

    rwv()
        .args(["push", "--repo", "re:^local/cwalv/"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    for (i, (rp, expected)) in expected_shas.iter().enumerate() {
        if rp.starts_with("local/cwalv/") {
            assert_eq!(
                bare_main_sha(&ws.manifest_bares[i].1),
                Some(expected.clone())
            );
        }
    }
    assert_eq!(bare_main_sha(&ws.manifest_bares[2].1), baseline_c);
}

#[test]
fn push_union_role_and_repo_selectors() {
    let (ws, expected_shas) = build_workspace_with_advances(
        "alpha",
        &[
            ("local/me/p", "owned"),
            ("local/external/dep", "dependency"),
            ("local/external/other", "dependency"),
        ],
    );
    let baseline_other = bare_main_sha(&ws.manifest_bares[2].1);

    rwv()
        .args(["push", "--role", "owned", "--repo", "local/external/dep"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    // Primary advances via --role; external/dep via --repo.
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[0].1),
        Some(expected_shas[0].1.clone()),
        "primary should advance"
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[1].1),
        Some(expected_shas[1].1.clone()),
        "exact-named dep should advance"
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[2].1),
        baseline_other,
        "unmatched dep must NOT advance"
    );
}

// ----------------------------------------------------------------------------
// A selector narrows what the push acts on: the members it does not select are
// not judged, and the project branch is not published unless it is named.
// ----------------------------------------------------------------------------

#[test]
fn push_judges_only_the_members_it_acts_on() {
    // Two owned members, both advanced locally, the lock updated for A only.
    // `--repo a` pushes A's branch and judges nothing of B, whose HEAD has
    // drifted from its pin; the project branch, which carries the lock, is
    // not published without `projects/<name>`.
    let ws = common::build_workspace(
        "alpha",
        &[("local/org/a", "owned"), ("local/org/b", "owned")],
    );
    let baseline_project = bare_main_sha(&ws.project_bare);

    let mut new_shas: Vec<String> = Vec::new();
    for (rp, _) in &ws.manifest_bares {
        let local = ws.workspace.join(rp);
        std::fs::write(local.join(format!("{}.txt", rp.replace('/', "_"))), rp).unwrap();
        common::git_in(&local, &["add", "."]);
        common::git_in(&local, &["commit", "-m", &format!("advance {rp}")]);
        new_shas.push(common::git_in(&local, &["rev-parse", "HEAD"]));
    }
    let (_, a_bare) = &ws.manifest_bares[0];
    let (_, b_bare) = &ws.manifest_bares[1];
    let stale_b_lock_sha =
        common::git_in(ws.workspace.join("local/org/b"), &["rev-parse", "HEAD~1"]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    common::fixture_lock(
        &project_dir,
        &[
            ("local/org/a", a_bare.to_str().unwrap(), &new_shas[0]),
            ("local/org/b", b_bare.to_str().unwrap(), &stale_b_lock_sha),
        ],
    );
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "partial relock"]);

    rwv()
        .args(["push", "--repo", "local/org/a"])
        .current_dir(&ws.workspace)
        .assert()
        .success();
    assert_eq!(bare_main_sha(a_bare), Some(new_shas[0].clone()));
    assert_ne!(
        bare_main_sha(b_bare),
        Some(new_shas[1].clone()),
        "the unselected member is not pushed"
    );
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        baseline_project,
        "the project branch is not published without `projects/alpha`"
    );

    rwv()
        .args(["push", "--repo", "local/org/a", "--repo", "projects/alpha"])
        .current_dir(&ws.workspace)
        .assert()
        .success();
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        Some(common::git_in(&project_dir, &["rev-parse", "HEAD"])),
        "naming `projects/alpha` publishes the project branch"
    );
}
