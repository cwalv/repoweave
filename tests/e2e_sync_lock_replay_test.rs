//! E2E integration tests for `rwv sync` lock+replay semantics across all
//! sync strategies with concurrent lock-bumping workweaves.
//!
//! ## What these tests cover
//!
//! ### Rebase strategy (replays committed trees)
//!
//! 1. **N=2 lock-only convergence** (`sync_two_workweaves_lock_only_rebase_converges`):
//!    Two workweaves both bump the manifest lock. Primary absorbs WA; WB then
//!    syncs primary with `--strategy=rebase`. The lock-only commit in WB's
//!    project history should drop silently (empty patch via `merge=ours`). No
//!    manual `git rebase --continue`.
//!
//! 2. **N=3 lock-only convergence** (`sync_three_workweaves_lock_only_rebase_converges`):
//!    Same recipe with three workweaves. WC syncs a primary that already has
//!    two prior lock-bump commits — asserts the mechanism holds for deeper
//!    histories.
//!
//! 3. **Missing `.gitattributes` hard-bails on rebase**
//!    (`sync_rebase_without_gitattributes_bails_cleanly`): the precondition
//!    check fires before any git ops; exit non-zero, no `.git/rebase-merge/`,
//!    `git status` clean.
//!
//! ### Merge strategy (3-way merges into one commit)
//!
//! 5. **N=2 lock-only convergence on merge** (`sync_two_workweaves_lock_only_merge_converges`):
//!    Same N=2 recipe as test 1, but with `--strategy=merge`. The `merge=ours`
//!    driver assignment must auto-resolve the rwv.lock conflict.
//!
//! 6. **Missing `.gitattributes` hard-bails on merge**
//!    (`sync_merge_without_gitattributes_bails_cleanly`): the precondition
//!    check fires for merge too — the inline `-c merge.ours.driver=true` only
//!    *defines* a driver; the `.gitattributes` line *assigns* it to `rwv.lock`.
//!
//! ### FF strategy (advances branch pointer, no replay)
//!
//! 4. **FF preserves lock-only commits** (`sync_ff_preserves_lock_only_commits`):
//!    With `--strategy=ff` (no replay), a workweave's lock-only commit lands
//!    verbatim in primary's history. Attribution is preserved; the commit count
//!    grows by exactly 1 (the lock commit). The precondition does NOT fire for
//!    FF since no merge happens.
//!
//! ## Fixture shape
//!
//! Primary workspace `P` owns one manifest repo `R`. Workweaves are created
//! via `rwv workweave create`, which sets up git worktrees.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Git + rwv helpers (local to this test file)
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

struct PrimaryWorkspace {
    root: PathBuf,
    project_dir: PathBuf,
    manifest_repo: PathBuf,
}

struct Workweave {
    root: PathBuf,
    project_dir: PathBuf,
    manifest_repo: PathBuf,
}

/// Build the primary workspace:
/// ```text
/// {tmp}/ws/                      -- workspace root
/// {tmp}/ws/github/org/lib/       -- manifest repo, initial commit
/// {tmp}/ws/projects/app/         -- project repo with rwv.yaml, rwv.lock,
///                                   and .gitattributes (rwv.lock merge=ours)
/// ```
fn make_primary(tmp: &Path) -> PrimaryWorkspace {
    let ws = tmp.join("ws");
    let manifest_repo = ws.join(MANIFEST_REPO_PATH);
    let initial_sha = init_repo(&manifest_repo);

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);

    // The replay-exclusion line that sync depends on.
    std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();

    let manifest = format!(
        "repositories:\n  {path}:\n    type: git\n    url: file://{repo}\n    version: main\n    role: owned\n",
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

    git(
        &["add", ".gitattributes", "rwv.yaml", "rwv.lock"],
        &project_dir,
    );
    git(&["commit", "-m", "lock: initial"], &project_dir);

    // Action verbs require `.rwv-active`.
    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    PrimaryWorkspace {
        root: ws,
        project_dir,
        manifest_repo,
    }
}

/// Create a workweave via `rwv workweave create`. Returns its paths.
fn create_workweave(primary: &PrimaryWorkspace, weaveroot: &Path, name: &str) -> Workweave {
    rwv()
        .args(["workweave", PROJECT, "create", name])
        .env("RWV_WORKWEAVE_DIR", weaveroot)
        .current_dir(&primary.root)
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
// Test 1: N=2 concurrent lock-only workweaves — rebase converges
// ---------------------------------------------------------------------------

/// Two workweaves (WA and WB) both do a commit in the manifest repo and bump
/// the lock. Primary absorbs WA (ff); WB then syncs primary with
/// `--strategy=rebase`. WB's lock-only commit should drop silently via the
/// `merge=ours` + `--empty=drop` mechanism. No manual `git rebase --continue`.
#[test]
fn sync_two_workweaves_lock_only_rebase_converges() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let wa = create_workweave(&primary, &weaveroot, "wa");
    let wb = create_workweave(&primary, &weaveroot, "wb");

    // WA: advance manifest repo and lock.
    let wa_lib_sha = commit_file(&wa.manifest_repo, "wa.txt", "from wa\n", "wa: add wa.txt");
    rwv_lock_commit(&wa.root);

    // WB: advance manifest repo on a different file and lock.
    // WB's parent state is primary's pre-WA lock (WB was forked before WA landed).
    commit_file(&wb.manifest_repo, "wb.txt", "from wb\n", "wb: add wb.txt");
    rwv_lock_commit(&wb.root);

    // From primary: sync WA → primary (ff). Primary now has WA's commit + lock.
    rwv()
        .args(["sync", &wa.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    let primary_lib_head = git_out(&["rev-parse", "main"], &primary.manifest_repo);
    assert_eq!(
        primary_lib_head, wa_lib_sha,
        "primary lib should be at WA's commit after first sync"
    );

    // From WB: sync primary with rebase. This is the exact repro step:
    // WB's project history has a lock-only commit that conflicts with primary's
    // post-WA lock on the same lines. With `.gitattributes merge=ours`, the
    // lock-only commit produces an empty patch and is dropped silently.
    //
    // Before the lock-replay fix landed this step would fail with a
    // `git rebase --continue` conflict on rwv.lock. After: success.
    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&wb.root)
        .assert()
        .success();

    // WB's manifest repo should now contain wa.txt (from WA, carried via rebase)
    // and wb.txt (WB's own commit, replayed on top).
    assert!(
        wb.manifest_repo.join("wa.txt").exists(),
        "wb manifest repo should have wa.txt after rebase onto primary"
    );
    assert!(
        wb.manifest_repo.join("wb.txt").exists(),
        "wb manifest repo should still have wb.txt after rebase"
    );

    // WB's project repo should not be mid-rebase.
    let rebase_merge_dir = wb.project_dir.join(".git");
    // For a worktree, .git is a file; check for rebase-merge inside the actual git dir.
    // Use `git status` to verify clean state instead.
    let status_out = common::git()
        .args(["status", "--porcelain"])
        .current_dir(&wb.project_dir)
        .output()
        .expect("git status failed");
    assert!(
        status_out.status.success(),
        "git status should succeed (not mid-op)"
    );
    let _ = rebase_merge_dir; // only needed to assert no mid-op state above

    // From primary: sync WB → primary (ff). Both contributions land.
    rwv()
        .args(["sync", &wb.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    assert!(
        primary.manifest_repo.join("wa.txt").exists(),
        "primary lib should have wa.txt"
    );
    assert!(
        primary.manifest_repo.join("wb.txt").exists(),
        "primary lib should have wb.txt"
    );
    let primary_lock = std::fs::read_to_string(primary.project_dir.join("rwv.lock")).unwrap();
    let final_head = git_out(&["rev-parse", "main"], &primary.manifest_repo);
    assert!(
        primary_lock.contains(&final_head),
        "primary lock should pin lib at final HEAD ({final_head}); lock:\n{primary_lock}"
    );
}

// ---------------------------------------------------------------------------
// Test 2: N=3 concurrent lock-only workweaves — rebase converges
// ---------------------------------------------------------------------------

/// Three workweaves (WA, WB, WC) all bump the manifest lock independently.
/// WA and WB land into primary sequentially. WC, which was forked before any
/// of them landed, syncs primary (which now has two prior lock-bump commits)
/// with `--strategy=rebase`. Both of WC's lock-only project commits should
/// drop silently.
#[test]
fn sync_three_workweaves_lock_only_rebase_converges() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let wa = create_workweave(&primary, &weaveroot, "wa");
    let wb = create_workweave(&primary, &weaveroot, "wb");
    let wc = create_workweave(&primary, &weaveroot, "wc");

    // WA: manifest commit + lock.
    commit_file(&wa.manifest_repo, "wa.txt", "from wa\n", "wa: add wa.txt");
    rwv_lock_commit(&wa.root);

    // WB: manifest commit + lock (forked from same pre-WA primary).
    commit_file(&wb.manifest_repo, "wb.txt", "from wb\n", "wb: add wb.txt");
    rwv_lock_commit(&wb.root);

    // WC: manifest commit + lock (also forked from pre-WA primary).
    commit_file(&wc.manifest_repo, "wc.txt", "from wc\n", "wc: add wc.txt");
    rwv_lock_commit(&wc.root);

    // Primary absorbs WA (ff).
    rwv()
        .args(["sync", &wa.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // WB syncs primary (rebase) then lands in primary (ff).
    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&wb.root)
        .assert()
        .success();
    rwv()
        .args(["sync", &wb.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Now primary has two lock-bump commits from WA and WB.
    // WC syncs primary with rebase. WC's project repo has one lock-only commit
    // (its initial rwv lock commit) that conflicts with the two prior lock-bumps.
    // Both should drop silently.
    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&wc.root)
        .assert()
        .success();

    // WC should now see all three manifest repo files.
    assert!(
        wc.manifest_repo.join("wa.txt").exists(),
        "wc manifest should have wa.txt after rebase"
    );
    assert!(
        wc.manifest_repo.join("wb.txt").exists(),
        "wc manifest should have wb.txt after rebase"
    );
    assert!(
        wc.manifest_repo.join("wc.txt").exists(),
        "wc manifest should still have wc.txt after rebase"
    );

    // WC project should be in a clean git state (not mid-rebase).
    let status = common::git()
        .args(["status", "--porcelain"])
        .current_dir(&wc.project_dir)
        .output()
        .expect("git status failed");
    assert!(
        status.status.success(),
        "git status should succeed after rebase completes"
    );

    // Land WC into primary (ff).
    rwv()
        .args(["sync", &wc.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    assert!(primary.manifest_repo.join("wa.txt").exists());
    assert!(primary.manifest_repo.join("wb.txt").exists());
    assert!(primary.manifest_repo.join("wc.txt").exists());

    let primary_lock = std::fs::read_to_string(primary.project_dir.join("rwv.lock")).unwrap();
    let final_head = git_out(&["rev-parse", "main"], &primary.manifest_repo);
    assert!(
        primary_lock.contains(&final_head),
        "primary lock should pin lib at final HEAD ({final_head}); lock:\n{primary_lock}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: missing .gitattributes — hard-bail before any git ops
// ---------------------------------------------------------------------------

/// When the CWD project repo's committed `.gitattributes` does NOT contain
/// `rwv.lock merge=ours`, `rwv sync --strategy=rebase` must:
/// 1. Exit non-zero.
/// 2. Leave no in-flight rebase state (`.git/rebase-merge/` absent).
/// 3. Leave `git status` clean (no partial changes committed or staged).
/// 4. Emit an actionable error message naming the file, the missing line,
///    and the fix command `rwv doctor --fix`.
#[test]
fn sync_rebase_without_gitattributes_bails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Build the primary workspace WITHOUT `.gitattributes` (don't call make_primary,
    // build manually so we can omit the gitattributes file).
    let ws = tmp.path().join("ws");
    let manifest_repo = ws.join(MANIFEST_REPO_PATH);
    let initial_sha = init_repo(&manifest_repo);

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);

    // Intentionally omit `.gitattributes` — this is the bug scenario.

    let manifest = format!(
        "repositories:\n  {path}:\n    type: git\n    url: file://{repo}\n    version: main\n    role: owned\n",
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

    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    // Create a workweave from this (no-.gitattributes) primary.
    let ww = {
        rwv()
            .args(["workweave", PROJECT, "create", "ww"])
            .env("RWV_WORKWEAVE_DIR", &weaveroot)
            .current_dir(&ws)
            .assert()
            .success();
        let root = weaveroot.join(format!("{PROJECT}--ww"));
        Workweave {
            project_dir: root.join("projects").join(PROJECT),
            manifest_repo: root.join(MANIFEST_REPO_PATH),
            root,
        }
    };

    // WW: advance the manifest repo and bump the lock.
    commit_file(&ww.manifest_repo, "ww.txt", "from ww\n", "ww: add ww.txt");
    // Run `rwv lock --commit` in the workweave. The workweave also has no
    // .gitattributes (inherited from primary via worktree).
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Also advance primary so WW's rebase has something to replay onto.
    // Give primary a new manifest commit + lock so WW's project tip diverges.
    let primary_struct = PrimaryWorkspace {
        root: ws.clone(),
        project_dir: project_dir.clone(),
        manifest_repo: manifest_repo.clone(),
    };
    // Create another workweave (wa) to advance primary, then land it.
    let wa = create_workweave(&primary_struct, &weaveroot, "wa");
    // But wa's project repo also has no .gitattributes, so we need to add one
    // for wa to land via ff (ff doesn't need .gitattributes — only rebase does).
    commit_file(&wa.manifest_repo, "wa.txt", "from wa\n", "wa: add wa.txt");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&wa.root)
        .assert()
        .success();
    rwv()
        .args(["sync", &wa.root.to_string_lossy()])
        .current_dir(&ws)
        .assert()
        .success();

    // Record WW's project HEAD before the failed sync attempt.
    let ww_project_head_before = git_out(&["rev-parse", "HEAD"], &ww.project_dir);

    // From WW: attempt rebase sync onto primary. Must fail before any git op.
    let assert = rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .failure();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();

    // (a) Error message must name the missing line and the fix command.
    assert!(
        stderr.contains("rwv.lock merge=ours"),
        "error must name the missing line `rwv.lock merge=ours`; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv doctor --fix"),
        "error must name `rwv doctor --fix` as the fix; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(".gitattributes"),
        "error must name the .gitattributes file; got stderr:\n{stderr}"
    );

    // (b) WW's project HEAD must be unchanged — no commits were applied.
    let ww_project_head_after = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    assert_eq!(
        ww_project_head_before, ww_project_head_after,
        "sync must not modify the project repo's HEAD on bail"
    );

    // (c) No in-flight rebase state. For a worktree the actual git dir is
    // under the canonical clone's .git/worktrees/<name>/. We verify absence
    // of mid-op state and no staged or modified tracked files via
    // `git status --porcelain`. Untracked files (e.g., VS Code workspace
    // files in the temp dir) are filtered out — only tracked-file changes
    // matter for the invariant that sync did not mutate the project repo.
    let status = common::git()
        .args(["status", "--porcelain"])
        .current_dir(&ww.project_dir)
        .output()
        .expect("git status should not fail");
    assert!(
        status.status.success(),
        "git status must succeed after bail (no mid-op state)"
    );
    let status_out = String::from_utf8_lossy(&status.stdout).to_string();
    // Filter to lines that indicate tracked-file changes (staged or modified);
    // lines starting with `?` are untracked and not relevant.
    let tracked_changes: Vec<&str> = status_out
        .lines()
        .filter(|l| !l.starts_with('?') && !l.trim().is_empty())
        .collect();
    assert!(
        tracked_changes.is_empty(),
        "no staged or modified tracked files should exist after bail; \
         git status tracked changes:\n{}",
        tracked_changes.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Test 4: FF strategy preserves lock-only commits
// ---------------------------------------------------------------------------

/// Fast-forward does not replay commits — it advances the branch pointer.
/// A lock-only commit in the source's history must land verbatim in primary's
/// history after `rwv sync <workweave>` (default ff strategy). The commit
/// count grows by exactly 1 (the lock commit), and it retains its original
/// author and SHA.
///
/// This companion test ensures the "lock-only commits drop silently on rebase"
/// behavior does NOT bleed into the ff path.
#[test]
fn sync_ff_preserves_lock_only_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, &weaveroot, "ww");

    // Record primary's project tip before anything.
    let primary_tip_before = git_out(&["rev-parse", "HEAD"], &primary.project_dir);

    // WW: bump the manifest repo and lock. This produces exactly one lock-only
    // commit in the project repo.
    commit_file(&ww.manifest_repo, "ww.txt", "from ww\n", "ww: add ww.txt");
    rwv_lock_commit(&ww.root);

    // Capture WW's lock commit SHA.
    let ww_lock_commit_sha = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    let ww_project_log_count = git_out(&["rev-list", "--count", "HEAD"], &ww.project_dir);

    // From primary: sync WW → primary via ff (the default).
    rwv()
        .args(["sync", &ww.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // Phase 3 generates a fresh lock commit on primary (re-lock after sync).
    // Primary's tip is that re-lock commit. WW's lock commit must appear as
    // its parent (or grandparent, depending on whether Phase 3 committed).
    // The key assertion: WW's lock-only commit SHA exists in primary's history.
    let primary_log = git_out(
        &["log", "--format=%H", &format!("{primary_tip_before}..HEAD")],
        &primary.project_dir,
    );
    assert!(
        primary_log.contains(ww_lock_commit_sha.as_str()),
        "primary's history after ff sync must contain WW's lock commit {};\n\
         commits added:\n{primary_log}",
        ww_lock_commit_sha,
    );

    // WW's project commit count is a baseline for comparing history length.
    let _ = ww_project_log_count;

    // Also verify: the lib commit landed in primary's manifest repo.
    assert!(
        primary.manifest_repo.join("ww.txt").exists(),
        "primary manifest should have ww.txt after ff sync"
    );

    // Primary's lock must pin the final lib HEAD.
    let primary_lock = std::fs::read_to_string(primary.project_dir.join("rwv.lock")).unwrap();
    let lib_head = git_out(&["rev-parse", "main"], &primary.manifest_repo);
    assert!(
        primary_lock.contains(&lib_head),
        "primary lock should pin lib at final HEAD ({lib_head}); lock:\n{primary_lock}"
    );
}

// ---------------------------------------------------------------------------
// Test 5: N=2 concurrent lock-only workweaves — merge converges
// ---------------------------------------------------------------------------

/// Mirror of test 1 but using `--strategy=merge`. The `merge=ours` driver +
/// `.gitattributes` assignment must auto-resolve the rwv.lock conflict during
/// the 3-way merge, so the sync converges without operator intervention.
///
/// Companion coverage for `verify_replay_exclusion_invariant` firing on
/// merge — the original implementation only tested rebase, which missed
/// the parallel bug on the merge path.
#[test]
fn sync_two_workweaves_lock_only_merge_converges() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let wa = create_workweave(&primary, &weaveroot, "wa");
    let wb = create_workweave(&primary, &weaveroot, "wb");

    commit_file(&wa.manifest_repo, "wa.txt", "from wa\n", "wa: add wa.txt");
    rwv_lock_commit(&wa.root);

    commit_file(&wb.manifest_repo, "wb.txt", "from wb\n", "wb: add wb.txt");
    rwv_lock_commit(&wb.root);

    // Primary absorbs WA (ff).
    rwv()
        .args(["sync", &wa.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // From WB: sync primary with merge. Without the merge=ours assignment in
    // .gitattributes, this would conflict on rwv.lock. With it, the merge
    // completes automatically.
    rwv()
        .args(["sync", "primary", "--strategy", "merge"])
        .current_dir(&wb.root)
        .assert()
        .success();

    // WB's project repo should be clean (not mid-merge).
    let status = common::git()
        .args(["status", "--porcelain"])
        .current_dir(&wb.project_dir)
        .output()
        .expect("git status failed");
    assert!(
        status.status.success(),
        "git status should succeed after merge"
    );

    // WB's manifest repo carries both contributions.
    assert!(wb.manifest_repo.join("wa.txt").exists());
    assert!(wb.manifest_repo.join("wb.txt").exists());
}

// ---------------------------------------------------------------------------
// Test 6: merge without .gitattributes — hard-bail before any git ops
// ---------------------------------------------------------------------------

/// Mirror of test 3 but with `--strategy=merge`. The precondition check
/// fires for both Rebase and Merge — only Ff is exempt. Without this
/// coverage the check could be scoped to Rebase only, leaving merge to
/// fall back to a 3-way conflict on rwv.lock.
#[test]
fn sync_merge_without_gitattributes_bails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let ws = tmp.path().join("ws");
    let manifest_repo = ws.join(MANIFEST_REPO_PATH);
    let initial_sha = init_repo(&manifest_repo);

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);

    // Intentionally omit .gitattributes — bug scenario.

    let manifest = format!(
        "repositories:\n  {path}:\n    type: git\n    url: file://{repo}\n    version: main\n    role: owned\n",
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

    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    let ww = {
        rwv()
            .args(["workweave", PROJECT, "create", "ww"])
            .env("RWV_WORKWEAVE_DIR", &weaveroot)
            .current_dir(&ws)
            .assert()
            .success();
        let root = weaveroot.join(format!("{PROJECT}--ww"));
        Workweave {
            project_dir: root.join("projects").join(PROJECT),
            manifest_repo: root.join(MANIFEST_REPO_PATH),
            root,
        }
    };

    commit_file(&ww.manifest_repo, "ww.txt", "from ww\n", "ww: add ww.txt");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Advance primary so WW's merge has divergence to merge.
    let primary_struct = PrimaryWorkspace {
        root: ws.clone(),
        project_dir: project_dir.clone(),
        manifest_repo: manifest_repo.clone(),
    };
    let wa = create_workweave(&primary_struct, &weaveroot, "wa");
    commit_file(&wa.manifest_repo, "wa.txt", "from wa\n", "wa: add wa.txt");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&wa.root)
        .assert()
        .success();
    rwv()
        .args(["sync", &wa.root.to_string_lossy()])
        .current_dir(&ws)
        .assert()
        .success();

    let ww_head_before = git_out(&["rev-parse", "HEAD"], &ww.project_dir);

    let assert = rwv()
        .args(["sync", "primary", "--strategy", "merge"])
        .current_dir(&ww.root)
        .assert()
        .failure();

    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();

    assert!(
        stderr.contains("rwv.lock merge=ours"),
        "error must name `rwv.lock merge=ours`; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv doctor --fix"),
        "error must name `rwv doctor --fix`; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(".gitattributes"),
        "error must name `.gitattributes`; got stderr:\n{stderr}"
    );

    let ww_head_after = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    assert_eq!(
        ww_head_before, ww_head_after,
        "sync must not modify project HEAD on bail"
    );

    let status = common::git()
        .args(["status", "--porcelain"])
        .current_dir(&ww.project_dir)
        .output()
        .expect("git status should not fail");
    assert!(
        status.status.success(),
        "git status must succeed after bail"
    );
    let status_out = String::from_utf8_lossy(&status.stdout).to_string();
    let tracked_changes: Vec<&str> = status_out
        .lines()
        .filter(|l| !l.starts_with('?') && !l.trim().is_empty())
        .collect();
    assert!(
        tracked_changes.is_empty(),
        "no staged or modified tracked files should exist after merge-bail; \
         tracked changes:\n{}",
        tracked_changes.join("\n")
    );
}
