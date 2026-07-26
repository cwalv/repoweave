//! Doc claim (docs/reference/cli.md, post fo-wdrl2r.2): each renamed override
//! flag waives exactly the one precondition it is named for — never a
//! neighbor.
//!
//! `workweave delete --force` used to gate two independent refusals (dirty
//! worktree, unmerged commits) behind one name that only described the
//! first. fo-wdrl2r.2 split it into `--discard-uncommitted` and
//! `--discard-unmerged-commits`. A happy-path test per flag would have
//! passed even under the old bug, since each scenario only ever constructed
//! the one precondition its flag was meant to waive. The tests below
//! construct BOTH preconditions at once and pass only one flag, so a flag
//! that quietly waives its neighbor fails here.
//!
//! Coverage for the other renamed flags already exists elsewhere and is not
//! repeated:
//!   - `--replace-existing` (workweave create): refuses on a dirty/unmerged
//!     existing workweave even when passed —
//!     `workweave_idempotent_test.rs::workweave_recreate_with_replace_existing_destroys_and_recreates`.
//!   - `--delete-shared-clone` (remove --delete): both directions —
//!     `doc_claims_fetch_test.rs::remove_delete_does_not_check_other_projects`.
//!   - the op-in-progress guard on `workweave delete`, which no flag here
//!     waives — `workweave_topology_parent_test.rs::delete_waivers_do_not_bypass_op_mutex`.
//!
//! `--allow-non-empty-dir` (fetch) only had unit coverage of the underlying
//! function; the tests here exercise it through the CLI. `push --force` is
//! the deliberate non-waiver in the family (it is git's force-push, not a
//! precondition override) — the tests here pin that it actually force-pushes
//! and that it does not widen into the unrelated from-workweave refusal.

use assert_cmd::Command;
use std::path::Path;
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(
        status.success(),
        "git {:?} in {} failed",
        args,
        dir.display()
    );
}

fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

fn make_workspace(tmp: &Path, project: &str) -> std::path::PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: owned
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    ws
}

fn head_sha(dir: &Path) -> String {
    let output = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git rev-parse should run");
    assert!(output.status.success(), "git rev-parse HEAD failed");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

// ============================================================================
// fetch --allow-non-empty-dir
// ============================================================================

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

/// Push an empty `rwv.yaml` manifest into a bare repo, via a temporary
/// working clone.
fn push_empty_manifest_to_bare(bare: &Path) {
    let tmp = common::tempdir().expect("tempdir for manifest work clone");
    let work = tmp.path().join("mwork");
    git(
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
    git(&["config", "user.email", "test@test.com"], &work);
    git(&["config", "user.name", "Test"], &work);
    std::fs::write(work.join("rwv.yaml"), "repositories: {}\n").unwrap();
    git(&["add", "rwv.yaml"], &work);
    git(&["commit", "-m", "add manifest"], &work);
    git(&["push", "origin", "main"], &work);
}

/// `--allow-non-empty-dir` waives exactly the non-empty-non-workspace
/// refusal: bootstrapping into a directory that already holds unrelated
/// files refuses without it and succeeds with it, leaving the pre-existing
/// content untouched.
#[test]
fn fetch_allow_non_empty_dir_waives_only_the_nonempty_dir_refusal() {
    let tmp = common::tempdir().unwrap();

    let project_bare = tmp.path().join("proj.git");
    init_bare_repo(&project_bare);
    push_empty_manifest_to_bare(&project_bare);
    let project_url = format!("file://{}", project_bare.display());

    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("unrelated.txt"), "pre-existing\n").unwrap();

    // Without the waiver: refuses and names the flag.
    let output = rwv()
        .args(["fetch", &project_url])
        .current_dir(&workspace)
        .output()
        .expect("rwv fetch should run");
    assert!(
        !output.status.success(),
        "fetch into a non-empty non-workspace dir must refuse without the waiver"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--allow-non-empty-dir"),
        "refusal must name the waiver; got: {stderr}"
    );
    assert!(
        !workspace.join("projects").exists(),
        "refused fetch must not bootstrap anything"
    );

    // With the waiver: proceeds, and the pre-existing file survives.
    rwv()
        .args(["fetch", &project_url, "--allow-non-empty-dir"])
        .current_dir(&workspace)
        .assert()
        .success();
    assert!(
        workspace.join("projects/proj").exists(),
        "--allow-non-empty-dir should bootstrap the project"
    );
    assert!(
        workspace.join("unrelated.txt").exists(),
        "pre-existing file must survive the bootstrap"
    );
}

// ============================================================================
// workweave delete --discard-uncommitted / --discard-unmerged-commits
// ============================================================================

/// A workweave holding BOTH an uncommitted edit and a commit on the
/// ephemeral branch that is not merged anywhere. Every test in this section
/// builds this exact dual-precondition state so passing only one discard
/// flag can be checked against the precondition it does NOT name.
fn make_workweave_with_dirty_and_unmerged_commit(
    tmp: &Path,
    project: &str,
    name: &str,
) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let ws = make_workspace(tmp, project);
    let weaveroot = tmp.join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    rwv()
        .args(["workweave", project, "create", name])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();

    let ww_dir = weaveroot.join(format!("{project}--{name}"));
    let weave_repo = ww_dir.join("github/org/repo");

    // Commit work on the ephemeral branch — unmerged anywhere.
    std::fs::write(weave_repo.join("feature.txt"), "committed work\n").unwrap();
    git(&["add", "feature.txt"], &weave_repo);
    git(&["commit", "-m", "ww: feature"], &weave_repo);

    // On top, an uncommitted edit — dirty.
    std::fs::write(weave_repo.join("scratch.txt"), "uncommitted edit\n").unwrap();

    (ws, weaveroot, ww_dir)
}

/// `--discard-uncommitted` alone must still refuse on the unmerged commit:
/// it waives the dirty-worktree refusal, not the diverged-history one.
#[test]
fn workweave_delete_discard_uncommitted_does_not_waive_unmerged_commits() {
    let tmp = common::tempdir().unwrap();
    let (ws, weaveroot, ww_dir) =
        make_workweave_with_dirty_and_unmerged_commit(tmp.path(), "web-app", "both-a");
    let weave_repo = ww_dir.join("github/org/repo");
    let head_before = head_sha(&weave_repo);

    let output = rwv()
        .args([
            "workweave",
            "web-app",
            "delete",
            "both-a",
            "--discard-uncommitted",
        ])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .output()
        .expect("rwv workweave delete should run");
    assert!(
        !output.status.success(),
        "--discard-uncommitted alone must not clear an unmerged-commit refusal"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not merged") && stderr.contains("--discard-unmerged-commits"),
        "refusal must still name the unmerged-commits waiver; got: {stderr}"
    );
    assert!(
        ww_dir.exists(),
        "refused delete must leave the workweave intact"
    );
    assert_eq!(
        head_sha(&weave_repo),
        head_before,
        "refused delete must not touch the worktree"
    );
    assert!(
        weave_repo.join("scratch.txt").exists(),
        "the uncommitted file --discard-uncommitted was meant to cover must survive too"
    );
}

/// `--discard-unmerged-commits` alone must still refuse on the dirty
/// worktree: it waives the diverged-history refusal, not the uncommitted one.
#[test]
fn workweave_delete_discard_unmerged_commits_does_not_waive_uncommitted() {
    let tmp = common::tempdir().unwrap();
    let (ws, weaveroot, ww_dir) =
        make_workweave_with_dirty_and_unmerged_commit(tmp.path(), "web-app", "both-b");
    let weave_repo = ww_dir.join("github/org/repo");
    let head_before = head_sha(&weave_repo);

    let output = rwv()
        .args([
            "workweave",
            "web-app",
            "delete",
            "both-b",
            "--discard-unmerged-commits",
        ])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .output()
        .expect("rwv workweave delete should run");
    assert!(
        !output.status.success(),
        "--discard-unmerged-commits alone must not clear an uncommitted-changes refusal"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("uncommitted changes") && stderr.contains("--discard-uncommitted"),
        "refusal must still name the uncommitted waiver; got: {stderr}"
    );
    assert!(
        ww_dir.exists(),
        "refused delete must leave the workweave intact"
    );
    assert_eq!(
        head_sha(&weave_repo),
        head_before,
        "refused delete must not touch the worktree"
    );
    assert!(
        weave_repo.join("scratch.txt").exists(),
        "the dirty file the refusal is about must survive"
    );
}

/// Both flags together clear both preconditions — the `git branch -D`
/// contract fo-wdrl2r.2's doc update describes.
#[test]
fn workweave_delete_both_discard_flags_clear_both_preconditions() {
    let tmp = common::tempdir().unwrap();
    let (ws, weaveroot, ww_dir) =
        make_workweave_with_dirty_and_unmerged_commit(tmp.path(), "web-app", "both-c");

    rwv()
        .args([
            "workweave",
            "web-app",
            "delete",
            "both-c",
            "--discard-uncommitted",
            "--discard-unmerged-commits",
        ])
        .env("RWV_WORKWEAVE_DIR", &weaveroot)
        .current_dir(&ws)
        .assert()
        .success();
    assert!(
        !ww_dir.exists(),
        "both waivers together must remove the workweave"
    );
}

// ============================================================================
// push --force: force-pushes; does not waive the from-workweave refusal
// ============================================================================

/// Initialize a bare repo seeded with one commit on `main`.
fn init_bare_repo_with_commit(bare: &Path) {
    let parent = bare.parent().expect("bare repo path needs a parent");
    init_bare_repo(bare);
    let seed = parent.join(format!(
        "__seed_{}",
        bare.file_stem().unwrap().to_string_lossy()
    ));
    git(
        &["clone", &bare.to_string_lossy(), &seed.to_string_lossy()],
        parent,
    );
    git(&["config", "user.email", "test@test.com"], &seed);
    git(&["config", "user.name", "Test"], &seed);
    std::fs::write(seed.join("README"), "seed").unwrap();
    git(&["add", "."], &seed);
    git(&["commit", "-m", "initial"], &seed);
    git(&["push", "origin", "main"], &seed);
}

fn bare_main_sha(bare: &Path) -> Option<String> {
    let output = common::git()
        .args(["rev-parse", "main"])
        .current_dir(bare)
        .output()
        .expect("git should be available");
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).unwrap().trim().to_string())
}

/// Minimal single-repo push fixture: one manifest repo (owned) plus a
/// project repo, both with bare remotes.
struct PushFixture {
    _tmp: tempfile::TempDir,
    workspace: std::path::PathBuf,
    manifest_local: std::path::PathBuf,
    manifest_bare: std::path::PathBuf,
    project_dir: std::path::PathBuf,
}

fn build_push_fixture() -> PushFixture {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

    let manifest_bare = tmp.path().join("repo.git");
    init_bare_repo_with_commit(&manifest_bare);
    let manifest_local = workspace.join("local/org/repo");
    std::fs::create_dir_all(manifest_local.parent().unwrap()).unwrap();
    git(
        &[
            "clone",
            "--origin",
            "origin",
            &manifest_bare.to_string_lossy(),
            &manifest_local.to_string_lossy(),
        ],
        workspace.parent().unwrap(),
    );
    git(&["config", "user.email", "test@test.com"], &manifest_local);
    git(&["config", "user.name", "Test"], &manifest_local);

    let project_bare = tmp.path().join("project.git");
    init_bare_repo_with_commit(&project_bare);
    let project_dir = workspace.join("projects/alpha");
    git(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &project_dir.to_string_lossy(),
        ],
        workspace.parent().unwrap(),
    );
    git(&["config", "user.email", "test@test.com"], &project_dir);
    git(&["config", "user.name", "Test"], &project_dir);

    let bare_url = manifest_bare.to_string_lossy().into_owned();
    let manifest_head = head_sha(&manifest_local);
    std::fs::write(
        project_dir.join("rwv.yaml"),
        format!(
            "repositories:\n  local/org/repo:\n    type: git\n    url: {bare_url}\n    version: main\n    role: owned\n"
        ),
    )
    .unwrap();
    std::fs::write(
        project_dir.join("rwv.lock"),
        format!(
            "repositories:\n  local/org/repo:\n    type: git\n    url: {bare_url}\n    version: {manifest_head}\n"
        ),
    )
    .unwrap();
    git(&["add", "."], &project_dir);
    git(&["commit", "-m", "manifest + lock"], &project_dir);

    std::fs::write(workspace.join(".rwv-active"), "alpha\n").unwrap();

    PushFixture {
        _tmp: tmp,
        workspace,
        manifest_local,
        manifest_bare,
        project_dir,
    }
}

fn write_lock_at(fixture: &PushFixture, sha: &str) {
    let bare_url = fixture.manifest_bare.to_string_lossy();
    std::fs::write(
        fixture.project_dir.join("rwv.lock"),
        format!(
            "repositories:\n  local/org/repo:\n    type: git\n    url: {bare_url}\n    version: {sha}\n"
        ),
    )
    .unwrap();
    git(&["add", "rwv.lock"], &fixture.project_dir);
    git(&["commit", "-m", "relock"], &fixture.project_dir);
}

/// `push --force` is not a precondition waiver like the other four flags —
/// it is git's force-push. Pin what it actually claims to do: overwrite a
/// diverged remote that a plain push is rejected by git for touching.
#[test]
fn push_force_actually_force_pushes() {
    let fixture = build_push_fixture();

    // Advance and push normally: bare moves to commit B.
    std::fs::write(fixture.manifest_local.join("b.txt"), "b").unwrap();
    git(&["add", "."], &fixture.manifest_local);
    git(&["commit", "-m", "b"], &fixture.manifest_local);
    let sha_b = head_sha(&fixture.manifest_local);
    write_lock_at(&fixture, &sha_b);
    rwv()
        .args(["push"])
        .current_dir(&fixture.workspace)
        .assert()
        .success();
    assert_eq!(bare_main_sha(&fixture.manifest_bare), Some(sha_b.clone()));

    // Rewrite local history from the same parent: commit C diverges from B.
    git(&["reset", "--hard", "HEAD~1"], &fixture.manifest_local);
    std::fs::write(fixture.manifest_local.join("c.txt"), "c").unwrap();
    git(&["add", "."], &fixture.manifest_local);
    git(&["commit", "-m", "c"], &fixture.manifest_local);
    let sha_c = head_sha(&fixture.manifest_local);
    write_lock_at(&fixture, &sha_c);

    // A plain push is a non-fast-forward: git rejects it, bare stays at B.
    rwv()
        .args(["push"])
        .current_dir(&fixture.workspace)
        .assert()
        .failure();
    assert_eq!(
        bare_main_sha(&fixture.manifest_bare),
        Some(sha_b),
        "a plain push must not move the diverged remote"
    );

    // --force pushes it through: bare now holds the rewritten history.
    rwv()
        .args(["push", "--force"])
        .current_dir(&fixture.workspace)
        .assert()
        .success();
    assert_eq!(
        bare_main_sha(&fixture.manifest_bare),
        Some(sha_c),
        "--force must overwrite the remote with the diverged local history"
    );
}

/// `--force` does not widen into refusals it was never meant to touch: the
/// from-workweave refusal has no waiver at all, with or without `--force`.
#[test]
fn push_force_does_not_waive_workweave_refusal() {
    let fixture = build_push_fixture();

    let workweave_dir = fixture.workspace.parent().unwrap().join("alpha--feat");
    std::fs::create_dir_all(&workweave_dir).unwrap();
    let marker = format!(
        "primary: {p}\nproject: alpha\nparent: {p}\n",
        p = fixture.workspace.display()
    );
    std::fs::write(workweave_dir.join(".rwv-workweave"), marker).unwrap();

    let output = rwv()
        .args(["push", "--force"])
        .current_dir(&workweave_dir)
        .output()
        .expect("rwv push should run");
    assert!(
        !output.status.success(),
        "push --force from a workweave must still refuse"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("workweave"),
        "refusal must still name the workweave hazard; got: {stderr}"
    );
}
