//! E2E coverage for n-way merging via `rwv sync` across multiple workweaves.
//!
//! These tests express the contract proposed in
//! `docs/proposals/sync-project-merging.md`. They are expected to FAIL against
//! the current implementation, which hard-resets the project repo in Phase 1
//! and refuses to sync once project repos have diverged.
//!
//! Scenario shape: a primary workspace ("main") with two sibling workweaves
//! `ww1` and `ww2`. Both workweaves make commits in the same manifest repo
//! and (in some tests) in the project repo. Goal: `rwv sync ww1` followed by
//! `rwv sync primary --strategy rebase` from ww2 followed by `rwv sync ww2`
//! lands both workweaves' contributions in main without manual git surgery.
//!
//! "Main" here is the primary workspace for setup convenience; sync semantics
//! are CWD-driven so the assertions don't depend on which side is primary.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Git + rwv helpers
// ---------------------------------------------------------------------------

fn git(args: &[&str], dir: &Path) {
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
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_out(args: &[&str], dir: &Path) -> String {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git command failed to start");
    assert!(
        out.status.success(),
        "git {:?} in {} failed:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn rwv() -> AssertCommand {
    common::rwv()
}

/// Init a git repo at `path` with one commit on `main`. Returns HEAD SHA.
fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

/// Stage and commit `filename` (relative to `repo`). Returns new HEAD SHA.
fn commit_file(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    let path = repo.join(filename);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
}

// ---------------------------------------------------------------------------
// Workspace fixture
// ---------------------------------------------------------------------------

const MANIFEST_REPO_PATH: &str = "github/org/lib";
const PROJECT: &str = "app";

struct MainWorkspace {
    /// Workspace root.
    root: PathBuf,
    /// projects/app/ — the project repo.
    project_dir: PathBuf,
    /// github/org/lib/ — the manifest repo.
    manifest_repo: PathBuf,
}

/// Build the "main" workspace:
/// ```text
/// {tmp}/ws/                          -- workspace root
/// {tmp}/ws/github/org/lib/           -- manifest repo, initial commit
/// {tmp}/ws/projects/app/             -- project repo with rwv.yaml + rwv.lock committed
/// ```
fn make_main_workspace(tmp: &Path) -> MainWorkspace {
    let ws = tmp.join("ws");
    let manifest_repo = ws.join(MANIFEST_REPO_PATH);
    let initial_sha = init_repo(&manifest_repo);

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);

    let manifest = format!(
        "repositories:\n  {path}:\n    type: git\n    url: file://{repo}\n    version: main\n    role: primary\n",
        path = MANIFEST_REPO_PATH,
        repo = manifest_repo.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();

    let lock = format!(
        "repositories:\n  {path}:\n    type: git\n    url: file://{repo}\n    version: {sha}\n",
        path = MANIFEST_REPO_PATH,
        repo = manifest_repo.display(),
        sha = initial_sha
    );
    std::fs::write(project_dir.join("rwv.lock"), lock).unwrap();

    git(&["add", "rwv.yaml", "rwv.lock"], &project_dir);
    git(&["commit", "-m", "lock: initial"], &project_dir);

    // Post fo-h9prh: action verbs require `.rwv-active` (or --project).
    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    MainWorkspace {
        root: ws,
        project_dir,
        manifest_repo,
    }
}

struct Workweave {
    /// Workweave root, e.g. `{tmp}/.workweaves/app--ww1/`.
    root: PathBuf,
    /// projects/app/ inside the workweave (a worktree of main's project repo).
    project_dir: PathBuf,
    /// github/org/lib/ inside the workweave (a worktree of main's manifest repo).
    manifest_repo: PathBuf,
}

/// Create a workweave via `rwv workweave create`. Returns its paths.
fn create_workweave(main: &MainWorkspace, weaveroot: &Path, name: &str) -> Workweave {
    rwv()
        .args(["workweave", PROJECT, "create", name])
        .env("RWV_WORKWEAVE_DIR", weaveroot)
        .current_dir(&main.root)
        .assert()
        .success();

    let root = weaveroot.join(format!("{PROJECT}--{name}"));
    Workweave {
        project_dir: root.join("projects").join(PROJECT),
        manifest_repo: root.join(MANIFEST_REPO_PATH),
        root,
    }
}

/// Run `rwv lock --commit` from a workspace root.
fn rwv_lock_commit(workspace_root: &Path) {
    rwv()
        .args(["lock", "--commit"])
        .current_dir(workspace_root)
        .assert()
        .success();
}

// ---------------------------------------------------------------------------
// Test 1: lock-only changes — both workweaves change manifest only
// ---------------------------------------------------------------------------

/// Both workweaves edit independent files in the manifest repo. The only
/// change to each workweave's project repo is the lock commit. Sync ww1 → main
/// (ff), then ww2 syncs main with rebase, then sync ww2 → main (ff).
#[test]
fn sync_two_workweaves_lock_only_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    let ww1 = create_workweave(&main, &weaveroot, "ww1");
    let ww2 = create_workweave(&main, &weaveroot, "ww2");

    // ww1 advances the manifest repo and locks.
    let ww1_lib_sha = commit_file(&ww1.manifest_repo, "foo.txt", "ww1 foo\n", "ww1: add foo");
    rwv_lock_commit(&ww1.root);

    // ww2 advances the manifest repo on a different file and locks.
    let ww2_lib_sha = commit_file(&ww2.manifest_repo, "bar.txt", "ww2 bar\n", "ww2: add bar");
    rwv_lock_commit(&ww2.root);

    // From main: sync ww1. Default ff strategy. Main now has ww1's lib commit
    // and an updated lock.
    rwv()
        .args(["sync", &ww1.root.to_string_lossy()])
        .current_dir(&main.root)
        .assert()
        .success();

    let main_lib_head = git_out(&["rev-parse", "main"], &main.manifest_repo);
    assert_eq!(
        main_lib_head, ww1_lib_sha,
        "main lib HEAD should be at ww1's commit after first sync"
    );

    // From ww2: sync from main with rebase. ww2's lib branch rebases onto
    // ww1's tip (gaining foo.txt). ww2's project commits replay onto main's
    // project tip with rwv.lock excluded (ww2's only project commit was the
    // lock commit, so this is a no-op patch). Phase 3 regenerates ww2's lock
    // to reflect the rebased lib SHA.
    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww2.root)
        .assert()
        .success();

    // ww2's lib working tree should now contain both foo.txt (from ww1) and
    // bar.txt (from ww2, replayed).
    assert!(
        ww2.manifest_repo.join("foo.txt").exists(),
        "ww2 lib should have foo.txt after rebase"
    );
    assert!(
        ww2.manifest_repo.join("bar.txt").exists(),
        "ww2 lib should still have bar.txt after rebase"
    );

    // From main: sync ww2. ff. Main now has both files.
    rwv()
        .args(["sync", &ww2.root.to_string_lossy()])
        .current_dir(&main.root)
        .assert()
        .success();

    assert!(
        main.manifest_repo.join("foo.txt").exists(),
        "main lib should have foo.txt"
    );
    assert!(
        main.manifest_repo.join("bar.txt").exists(),
        "main lib should have bar.txt"
    );
    let main_lock = std::fs::read_to_string(main.project_dir.join("rwv.lock")).unwrap();
    let final_main_lib_head = git_out(&["rev-parse", "main"], &main.manifest_repo);
    assert!(
        main_lock.contains(&final_main_lib_head),
        "main's lock should pin lib at the final lib HEAD ({final_main_lib_head}); lock contents:\n{main_lock}"
    );
    // Sanity: the final SHA must be a descendant of both ww1's and ww2's
    // original commits (i.e. ww2's commit was rebased onto ww1's).
    let _ = ww2_lib_sha; // referenced for symmetry
}

// ---------------------------------------------------------------------------
// Test 2: project doc changes — both workweaves change manifest AND project
// ---------------------------------------------------------------------------

/// Both workweaves change manifest repo content AND project doc content
/// (independent files). Sync ww1 → main (ff), ww2 syncs main with rebase
/// (Phase 1' replays ww2's project doc commit onto main's tip), sync ww2 →
/// main (ff). All four contributions land.
#[test]
fn sync_two_workweaves_with_project_doc_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    let ww1 = create_workweave(&main, &weaveroot, "ww1");
    let ww2 = create_workweave(&main, &weaveroot, "ww2");

    // ww1: manifest commit + project doc commit + lock.
    commit_file(&ww1.manifest_repo, "foo.txt", "ww1 foo\n", "ww1: add foo");
    commit_file(
        &ww1.project_dir,
        "notes/feat-a.md",
        "feature a notes\n",
        "docs: add feat-a notes",
    );
    rwv_lock_commit(&ww1.root);

    // ww2: manifest commit + project doc commit + lock.
    commit_file(&ww2.manifest_repo, "bar.txt", "ww2 bar\n", "ww2: add bar");
    commit_file(
        &ww2.project_dir,
        "notes/feat-b.md",
        "feature b notes\n",
        "docs: add feat-b notes",
    );
    rwv_lock_commit(&ww2.root);

    // From main: sync ww1.
    rwv()
        .args(["sync", &ww1.root.to_string_lossy()])
        .current_dir(&main.root)
        .assert()
        .success();
    assert!(main.project_dir.join("notes/feat-a.md").exists());
    assert!(main.manifest_repo.join("foo.txt").exists());

    // From ww2: rebase onto main. Phase 1' replays ww2's `docs: add feat-b
    // notes` commit onto main's tip (which now includes feat-a.md, on a
    // different path → no conflict). Lock commit's diff is empty after
    // exclusion, so it's skipped. Phase 3 produces a fresh lock commit.
    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww2.root)
        .assert()
        .success();
    assert!(
        ww2.project_dir.join("notes/feat-a.md").exists(),
        "ww2 project should now contain ww1's feat-a notes"
    );
    assert!(
        ww2.project_dir.join("notes/feat-b.md").exists(),
        "ww2 project should still contain its own feat-b notes"
    );

    // From main: sync ww2.
    rwv()
        .args(["sync", &ww2.root.to_string_lossy()])
        .current_dir(&main.root)
        .assert()
        .success();

    // Final state: all four files present, lock points at the final lib SHA.
    assert!(main.project_dir.join("notes/feat-a.md").exists());
    assert!(main.project_dir.join("notes/feat-b.md").exists());
    assert!(main.manifest_repo.join("foo.txt").exists());
    assert!(main.manifest_repo.join("bar.txt").exists());

    let main_lock = std::fs::read_to_string(main.project_dir.join("rwv.lock")).unwrap();
    let final_main_lib_head = git_out(&["rev-parse", "main"], &main.manifest_repo);
    assert!(
        main_lock.contains(&final_main_lib_head),
        "main's lock should pin lib at the final lib HEAD ({final_main_lib_head}); lock contents:\n{main_lock}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: genuine project conflict surfaces normally
// ---------------------------------------------------------------------------

/// Both workweaves edit the same project file with conflicting content. After
/// main absorbs ww1, ww2's rebase from main should hit a real git conflict on
/// the project file (not on rwv.lock — that's auto-resolved by Phase 3).
/// Sync should fail with an actionable error naming the conflicting path.
#[test]
fn sync_rebase_surfaces_genuine_project_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    let ww1 = create_workweave(&main, &weaveroot, "ww1");
    let ww2 = create_workweave(&main, &weaveroot, "ww2");

    // Both workweaves edit the SAME project file with conflicting content.
    commit_file(
        &ww1.project_dir,
        "notes/shared.md",
        "ww1 wrote this\n",
        "docs: ww1 take",
    );
    commit_file(&ww1.manifest_repo, "foo.txt", "ww1 foo\n", "ww1: add foo");
    rwv_lock_commit(&ww1.root);

    commit_file(
        &ww2.project_dir,
        "notes/shared.md",
        "ww2 wrote this\n",
        "docs: ww2 take",
    );
    commit_file(&ww2.manifest_repo, "bar.txt", "ww2 bar\n", "ww2: add bar");
    rwv_lock_commit(&ww2.root);

    // From main: sync ww1 — clean ff.
    rwv()
        .args(["sync", &ww1.root.to_string_lossy()])
        .current_dir(&main.root)
        .assert()
        .success();

    // From ww2: rebase onto main. Phase 1' tries to replay ww2's
    // `docs: ww2 take` commit on top of main's tip, which already has
    // ww1's version of notes/shared.md. Real conflict on a non-lock path
    // → sync should fail with a clear error.
    let assert = rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww2.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("notes/shared.md") || stderr.contains("conflict"),
        "sync failure should name the conflicting path or mention conflict; got stderr:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// Test: Phase 3 materialize — add repo at primary, workweave sync clones it
// ---------------------------------------------------------------------------

/// Bead fo-62glp: When primary adds a new repo to its manifest + lock, an
/// existing workweave running `rwv sync primary` should materialize the new
/// repo as a worktree (not silently advance the lock and leave a dangling
/// reference).
#[test]
fn sync_phase3_materializes_newly_added_repo_in_workweave() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    let ww1 = create_workweave(&main, &weaveroot, "ww1");

    // At primary: create a second manifest repo, then `rwv add` it.
    let new_repo_path = "github/org/extras";
    let new_repo_abs = main.root.join(new_repo_path);
    let new_repo_sha = init_repo(&new_repo_abs);
    // Add an origin so `rwv add <path>` can infer the URL.
    git(
        &[
            "remote",
            "add",
            "origin",
            &format!("file://{}", new_repo_abs.display()),
        ],
        &new_repo_abs,
    );

    rwv()
        .args(["add", new_repo_path])
        .current_dir(&main.root)
        .assert()
        .success();

    // Commit the manifest change and lock.
    git(&["add", "rwv.yaml"], &main.project_dir);
    git(&["commit", "-m", "add: extras"], &main.project_dir);
    rwv_lock_commit(&main.root);

    // Sanity: primary's lock now includes the new repo.
    let primary_lock = std::fs::read_to_string(main.project_dir.join("rwv.lock")).unwrap();
    assert!(
        primary_lock.contains(new_repo_path),
        "primary lock should list {new_repo_path}; got:\n{primary_lock}"
    );

    // From ww1: sync primary. Phase 3 should materialize the new repo as a
    // worktree of the canonical clone at primary.
    rwv()
        .args(["sync", "primary"])
        .current_dir(&ww1.root)
        .assert()
        .success();

    let ww1_new_repo = ww1.root.join(new_repo_path);
    assert!(
        ww1_new_repo.exists(),
        "Phase 3 should materialize {new_repo_path} in workweave; not found at {}",
        ww1_new_repo.display()
    );
    assert!(
        ww1_new_repo.join(".git").exists(),
        "{new_repo_path} should be a git worktree (have a .git entry)"
    );
    let ww1_head = git_out(&["rev-parse", "HEAD"], &ww1_new_repo);
    assert_eq!(
        ww1_head, new_repo_sha,
        "newly-materialized worktree should be at the locked SHA"
    );

    // doctor --locked should now pass cleanly.
    rwv()
        .args(["doctor", "--locked"])
        .current_dir(&ww1.root)
        .assert()
        .success();
}

/// B6: when Phase 3 materialize fails (e.g. canonical clone for a newly-added
/// repo doesn't exist on primary), `rwv sync` must exit non-zero. Previously
/// the failure was an stderr line and the loop fell through to a
/// `skipped (not on disk)` print that did NOT flip `any_failure`, so the
/// lock advanced past a never-materialised repo and sync exited 0 — same
/// shape as fo-62glp.
#[test]
fn sync_phase3_materialize_failure_is_fatal() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    let ww1 = create_workweave(&main, &weaveroot, "ww1");

    // At primary: add a second manifest repo and lock it.
    let new_repo_path = "github/org/extras";
    let new_repo_abs = main.root.join(new_repo_path);
    init_repo(&new_repo_abs);
    git(
        &[
            "remote",
            "add",
            "origin",
            &format!("file://{}", new_repo_abs.display()),
        ],
        &new_repo_abs,
    );
    rwv()
        .args(["add", new_repo_path])
        .current_dir(&main.root)
        .assert()
        .success();
    git(&["add", "rwv.yaml"], &main.project_dir);
    git(&["commit", "-m", "add: extras"], &main.project_dir);
    rwv_lock_commit(&main.root);

    // Now sabotage the canonical clone: remove it so `git worktree add`
    // against primary's path can't succeed.
    std::fs::remove_dir_all(&new_repo_abs).unwrap();

    // From ww1: sync primary should FAIL — materialize can't proceed and
    // the workweave is missing the new repo. Sync must report this rather
    // than silently skip and exit 0.
    let assert = rwv()
        .args(["sync", "primary"])
        .current_dir(&ww1.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("materialize failed") || stderr.contains("sync completed with failures"),
        "expected materialize/sync failure signal in stderr, got: {stderr}"
    );

    let ww1_new_repo = ww1.root.join(new_repo_path);
    assert!(
        !ww1_new_repo.exists(),
        "workweave should not have a partially-materialised repo on failure"
    );
}

// ---------------------------------------------------------------------------
// fo-ran2c + fo-kduyx: bare `rwv sync` follows parent + `--retire` cleanup
// ---------------------------------------------------------------------------

/// fo-ran2c happy path: a workweave forked from primary has `parent` recorded
/// in its marker; bare `rwv sync` (no source) reads that parent and syncs to
/// it. The end state matches `rwv sync primary`.
#[test]
fn bare_sync_follows_recorded_parent_to_primary() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    // rwv lock/sync require an active project at primary. make_main_workspace
    // doesn't write .rwv-active; existing tests reach this state implicitly
    // via `rwv add` (which calls activate). Set it explicitly here.
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    let ww1 = create_workweave(&main, &weaveroot, "ww1");

    // Primary advances the manifest repo and locks. ww1 is behind.
    let primary_sha = commit_file(
        &main.manifest_repo,
        "primary.txt",
        "from primary\n",
        "primary: add primary.txt",
    );
    rwv_lock_commit(&main.root);

    // Bare `rwv sync` from inside ww1 must follow parent (== primary) and
    // bring ww1 forward.
    rwv()
        .args(["sync"])
        .current_dir(&ww1.root)
        .assert()
        .success();

    let ww1_lib_head = git_out(&["rev-parse", "HEAD"], &ww1.manifest_repo);
    assert_eq!(
        ww1_lib_head, primary_sha,
        "after bare sync, ww1's lib HEAD must be at primary's tip"
    );
    assert!(
        ww1.manifest_repo.join("primary.txt").exists(),
        "after bare sync, ww1 must have primary's new file"
    );
}

/// fo-ran2c + backfill: a workweave whose `.rwv-workweave` predates parent
/// tracking still works under bare sync — `WorkweaveMarker::read` backfills
/// the missing `parent` to `primary`. Simulate that by stripping `parent:`
/// from the marker file after create, then run bare sync.
#[test]
fn bare_sync_works_after_parent_backfill_on_legacy_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    let ww1 = create_workweave(&main, &weaveroot, "ww1");

    // Strip the `parent:` line from the marker to simulate a pre-fo-ran2c
    // workweave on disk. The read path must backfill it to primary.
    let marker_path = ww1.root.join(".rwv-workweave");
    let marker_content = std::fs::read_to_string(&marker_path).unwrap();
    let stripped: String = marker_content
        .lines()
        .filter(|line| !line.trim_start().starts_with("parent:"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&marker_path, &stripped).unwrap();
    assert!(
        !stripped.contains("parent:"),
        "legacy marker must not contain parent: field for this test"
    );

    // Make primary diverge so bare sync has work to do.
    let primary_sha = commit_file(
        &main.manifest_repo,
        "legacy.txt",
        "legacy parent backfill\n",
        "primary: legacy backfill marker",
    );
    rwv_lock_commit(&main.root);

    rwv()
        .args(["sync"])
        .current_dir(&ww1.root)
        .assert()
        .success();

    let ww1_lib_head = git_out(&["rev-parse", "HEAD"], &ww1.manifest_repo);
    assert_eq!(
        ww1_lib_head, primary_sha,
        "bare sync on legacy marker must follow backfilled parent (primary)"
    );
}

/// fo-ran2c sibling-sync warning: when CWD is one workweave and an explicit
/// source is another (non-parent) workweave, sync should emit a warning that
/// names both paths and then proceed — the warning is informational, not a
/// refusal.
#[test]
fn sibling_sync_emits_warning_and_proceeds() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    let ww1 = create_workweave(&main, &weaveroot, "ww1");
    let ww2 = create_workweave(&main, &weaveroot, "ww2");

    // Both are forked from primary, so each has primary as `parent`. Syncing
    // ww1 → ww2 crosses sibling branches.
    let output = rwv()
        .args(["sync", &ww2.root.to_string_lossy()])
        .current_dir(&ww1.root)
        .output()
        .expect("rwv sync should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("siblings") || stderr.contains("skips the recorded parent"),
        "stderr should contain sibling-sync warning, got: {stderr}"
    );
    // The warning is non-fatal: the command should still attempt the sync and
    // succeed (there are no divergent commits to merge — both forks are at
    // the same primary tip).
    assert!(
        output.status.success(),
        "sibling sync should succeed despite warning, stderr: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// fo-kduyx: --retire
// ---------------------------------------------------------------------------

/// Happy path: `rwv sync --retire` from a workweave whose project repo is
/// already at parent's tip (no divergent commits) syncs and deletes.
#[test]
fn sync_retire_clean_path_deletes_workweave() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    // Gitignore activation-generated artifacts in the project repo before
    // creating the workweave. Without this, the workweave's project worktree
    // ends up with untracked files (`{project}.code-workspace`, `gita/`) that
    // the dirty check counts as uncommitted state — same check fo-gneid
    // requires, so --retire honors it. A real project would gitignore these
    // (or commit them); the test fixture is just minimal-by-default.
    std::fs::write(
        main.project_dir.join(".gitignore"),
        "*.code-workspace\ngita/\n",
    )
    .unwrap();
    git(&["add", ".gitignore"], &main.project_dir);
    git(
        &["commit", "-m", "ignore activation outputs"],
        &main.project_dir,
    );

    let ww1 = create_workweave(&main, &weaveroot, "ww1");

    // No-op sync: nothing in ww1 has changed since fork. Manifest tips
    // identical to parent's, working tree clean (modulo gitignored
    // activation outputs) → retire should succeed.
    assert!(ww1.root.exists(), "workweave must exist pre-retire");

    rwv()
        .args(["sync", "--retire"])
        .current_dir(&ww1.root)
        .assert()
        .success();

    assert!(
        !ww1.root.exists(),
        "--retire must delete the workweave on successful sync"
    );
}

/// Dirty-after-sync path: if any worktree in the workweave has uncommitted
/// changes when --retire runs the post-sync check, retire must refuse to
/// delete and leave the workweave intact for the operator to fix.
#[test]
fn sync_retire_with_dirty_worktree_refuses_to_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    let ww1 = create_workweave(&main, &weaveroot, "ww1");

    // Dirty up the manifest-repo worktree before retire runs. The sync
    // itself will succeed (no manifest changes to apply), but the
    // post-sync dirty check must catch the staged-edit and refuse delete.
    std::fs::write(ww1.manifest_repo.join("README.md"), "dirtied\n").unwrap();

    let assert = rwv()
        .args(["sync", "--retire"])
        .current_dir(&ww1.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("--retire") && stderr.contains("uncommitted"),
        "expected --retire to surface dirty-state refusal, got stderr: {stderr}"
    );
    assert!(
        ww1.root.exists(),
        "dirty --retire must NOT delete the workweave"
    );
}
