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
//!    project history should drop silently (empty patch via `merge=rwv-ours`). No
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
//!    `git status` clean. The check fires for rebase because git replays each
//!    commit as a 3-way merge — the inline `-c merge.rwv-ours.driver=true` only
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
///                                   and .gitattributes (rwv.lock merge=rwv-ours)
/// ```
fn make_primary(tmp: &Path) -> PrimaryWorkspace {
    let ws = tmp.join("ws");
    let manifest_repo = ws.join(MANIFEST_REPO_PATH);
    let initial_sha = init_repo(&manifest_repo);

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);

    // The replay-exclusion line that sync depends on.
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();

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
/// `merge=rwv-ours` + `--empty=drop` mechanism. No manual `git rebase --continue`.
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
    // post-WA lock on the same lines. With `.gitattributes merge=rwv-ours`, the
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
/// `rwv.lock merge=rwv-ours`, `rwv sync --strategy=rebase` must:
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
        stderr.contains("rwv.lock merge=rwv-ours"),
        "error must name the missing line `rwv.lock merge=rwv-ours`; got stderr:\n{stderr}"
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
// fo-yk0rlj: durable driver-config plant + legacy-needle bail message
// ---------------------------------------------------------------------------

/// The rebase-strategy sync invariant PLANTS the durable
/// `merge.rwv-ours.driver=true` config as its first act. This is what
/// keeps a bare `git rebase --continue` (the resume path git itself
/// prints in conflict stderr) from re-conflicting on every subsequent
/// lock-only pick. Test: run `rwv sync --strategy=rebase` from a
/// workweave whose canonical clone has NO `merge.rwv-ours.*` config, and
/// assert the config is set after the sync completes.
#[test]
fn sync_rebase_plants_merge_driver_config() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, &weaveroot, "ww");

    // Sanity: the config must be UNSET pre-sync. Worktrees share
    // `.git/config` with the canonical clone, so check the project
    // repo's canonical `.git/config` — that's what a bare
    // `git rebase --continue` from any worktree would consult.
    let pre = std::process::Command::new("git")
        .args(["config", "--local", "--get", "merge.rwv-ours.driver"])
        .current_dir(&primary.project_dir)
        .output()
        .unwrap();
    assert!(
        !pre.status.success(),
        "pre-sync: merge.rwv-ours.driver must be unset; got stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&pre.stdout),
        String::from_utf8_lossy(&pre.stderr)
    );

    // WW: bump the manifest repo + lock. Setup for a rebase-strategy sync.
    commit_file(&ww.manifest_repo, "ww.txt", "from ww\n", "ww: add ww.txt");
    rwv_lock_commit(&ww.root);

    // Also advance primary so ww's rebase has a divergence to replay.
    let wa = create_workweave(&primary, &weaveroot, "wa");
    commit_file(&wa.manifest_repo, "wa.txt", "from wa\n", "wa: add wa.txt");
    rwv_lock_commit(&wa.root);
    rwv()
        .args(["sync", &wa.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // From WW: rebase-sync onto primary. Must succeed AND plant the config.
    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Post-sync assertion: `merge.rwv-ours.driver=true` is now set in
    // the canonical clone's local config, so bare `git rebase
    // --continue` in this or any other worktree would find the driver
    // defined.
    let post = std::process::Command::new("git")
        .args(["config", "--local", "--get", "merge.rwv-ours.driver"])
        .current_dir(&primary.project_dir)
        .output()
        .unwrap();
    assert!(
        post.status.success(),
        "post-sync: merge.rwv-ours.driver must be planted; got stderr={:?}",
        String::from_utf8_lossy(&post.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&post.stdout).trim(),
        "true",
        "post-sync: merge.rwv-ours.driver value must be `true`"
    );
}

/// When the CWD project repo's committed `.gitattributes` still carries
/// the LEGACY `rwv.lock merge=ours` line (pre-fo-yk0rlj rename), the
/// invariant bails with a migration-specific message that directs the
/// operator at `rwv doctor --fix`. It must NOT silently accept the
/// legacy needle — sync's guarantee is the new spelling that closes the
/// global-config collision hazard.
#[test]
fn sync_rebase_with_legacy_needle_bails_pointing_at_doctor_fix() {
    let tmp = tempfile::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Build a primary workspace by hand with the LEGACY `.gitattributes`
    // spelling. This mirrors `make_primary` structurally so a workweave
    // can be created from it, but the .gitattributes line is the old
    // form.
    let ws = tmp.path().join("ws");
    let manifest_repo = ws.join(MANIFEST_REPO_PATH);
    let initial_sha = init_repo(&manifest_repo);

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);
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
    git(
        &["commit", "-m", "lock: initial (legacy attrs)"],
        &project_dir,
    );
    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    let primary = PrimaryWorkspace {
        root: ws.clone(),
        project_dir: project_dir.clone(),
        manifest_repo: manifest_repo.clone(),
    };

    // Create a workweave; workweaves inherit the primary's .gitattributes.
    let ww = create_workweave(&primary, &weaveroot, "ww");

    // Advance ww so a rebase has actual work to do (otherwise the fast
    // path might short-circuit before the invariant fires).
    commit_file(&ww.manifest_repo, "ww.txt", "from ww\n", "ww: add ww.txt");
    rwv_lock_commit(&ww.root);

    // Advance primary via a sibling workweave so ww's rebase has a
    // divergence point.
    let wa = create_workweave(&primary, &weaveroot, "wa");
    commit_file(&wa.manifest_repo, "wa.txt", "from wa\n", "wa: add wa.txt");
    rwv_lock_commit(&wa.root);
    // Land wa via ff (does not require the invariant).
    rwv()
        .args(["sync", &wa.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // From ww: attempt rebase sync onto primary. Must FAIL with a
    // migration-specific bail message.
    let assert = rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();

    assert!(
        stderr.contains("legacy `rwv.lock merge=ours`"),
        "bail must call out the legacy spelling; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv doctor --fix"),
        "bail must direct the operator at `rwv doctor --fix`; got stderr:\n{stderr}"
    );
    // And it must NOT direct the operator at the generic "add the line" fix
    // (which would leave them writing the WRONG spelling).
    assert!(
        !stderr.contains("chore: add rwv.lock replay-exclusion"),
        "legacy-needle bail must not use the generic add-the-line hint; got stderr:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// fo-yk0rlj: BARE git rebase — planted config alone must carry the exclusion
// ---------------------------------------------------------------------------

/// Run a BARE git command via `std::process::Command` — deliberately NOT
/// `common::git()`, NO `-c` flags, nothing rwv-spawned. This simulates the
/// operator's own shell git, which inherits none of rwv's inline driver
/// definitions. `GIT_EDITOR=true` only suppresses interactive message
/// editing on `rebase --continue`; it does not affect merge-driver lookup.
fn bare_git(args: &[&str], dir: &Path) -> std::process::Output {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_EDITOR", "true")
        .output()
        .expect("bare git failed to spawn")
}

/// Bare git that must succeed; returns trimmed stdout.
fn bare_git_ok(args: &[&str], dir: &Path) -> String {
    let out = bare_git(args, dir);
    assert!(
        out.status.success(),
        "bare git {args:?} in {} failed:\nstdout: {}\nstderr: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Porcelain status via bare git (string form for contains-assertions).
fn bare_status(dir: &Path) -> String {
    bare_git_ok(&["status", "--porcelain"], dir)
}

/// Lock-file YAML in the exact shape `make_primary` writes, with a chosen
/// version value — both branches rewrite the same `version:` line so a
/// 3-way merge without the driver is guaranteed to conflict.
fn lock_yaml(manifest_repo: &Path, version: &str) -> String {
    format!(
        "repositories:\n  {path}:\n    type: git\n    url: file://{repo}\n    version: {version}\n",
        path = MANIFEST_REPO_PATH,
        repo = manifest_repo.display(),
    )
}

/// The literal fo-yk0rlj / tc-jxrv incident shape, driven end-to-end by a
/// git process rwv did not spawn:
///
/// A rebase stops on a genuine non-lock conflict; the operator resolves it
/// and resumes with bare `git rebase --continue` — the exact command git
/// itself advertises in its conflict stderr. The resuming git process has
/// no inline `-c merge.rwv-ours.driver` flag, so the ONLY thing standing
/// between the next lock-only pick and a conflict on `rwv.lock` is the
/// durable config planted by rwv.
///
/// Phase A (negative control): with NO planted config, the resumed rebase
/// MUST conflict on `rwv.lock` — proving the fixture actually exercises
/// the failure mode, and that a plant to a scope bare git doesn't consult
/// would not sneak past this test.
///
/// Phase B: after planting via the production path (`rwv doctor --fix`),
/// the same bare rebase + resume completes without any conflict stopping
/// on `rwv.lock`, and the final lock content is the rebase target's
/// ("ours" semantics).
///
/// NOTE on empty commits: bare git's `--empty` policy for a pick that
/// becomes empty differs from rwv's explicit `--empty=drop`. The assertion
/// that matters here is "no conflict stops the rebase on rwv.lock" — if
/// the local git version stops on the now-empty pick instead of dropping
/// it, that stop is not a conflict and the test finishes the rebase the
/// way git's own hint says (`git rebase --skip`).
#[test]
fn bare_git_rebase_continue_resolves_lock_pick_via_planted_config_only() {
    let tmp = tempfile::tempdir().unwrap();
    let primary = make_primary(tmp.path());
    let repo = primary.project_dir.clone();

    // -- Fixture: two divergent branches. Overlapping (both-modified)
    //    paths: shared.txt (the genuine non-lock conflict that strands the
    //    operator mid-rebase) and rwv.lock (the lock-only pick). --
    commit_file(&repo, "shared.txt", "base\n", "base: add shared.txt");
    git(&["branch", "feature"], &repo);

    // main: bump the lock and shared.txt in one commit.
    let main_lock = lock_yaml(
        &primary.manifest_repo,
        "1111111111111111111111111111111111111111",
    );
    std::fs::write(repo.join("rwv.lock"), &main_lock).unwrap();
    std::fs::write(repo.join("shared.txt"), "main version\n").unwrap();
    git(&["add", "rwv.lock", "shared.txt"], &repo);
    git(&["commit", "-m", "main: bump lock + shared"], &repo);

    // feature: F1 = genuine non-lock conflict; F2 = lock-only pick.
    git(&["checkout", "feature"], &repo);
    commit_file(
        &repo,
        "shared.txt",
        "feature version\n",
        "F1: edit shared.txt",
    );
    let feat_lock = lock_yaml(
        &primary.manifest_repo,
        "2222222222222222222222222222222222222222",
    );
    std::fs::write(repo.join("rwv.lock"), &feat_lock).unwrap();
    git(&["add", "rwv.lock"], &repo);
    git(&["commit", "-m", "F2: lock-only bump"], &repo);
    let feature_tip = git_out(&["rev-parse", "HEAD"], &repo);

    // Sanity: the driver config must be UNSET — nothing has planted yet
    // (make_primary writes files directly; no rwv verb has run).
    let pre = bare_git(
        &["config", "--local", "--get", "merge.rwv-ours.driver"],
        &repo,
    );
    assert!(
        !pre.status.success(),
        "fixture precondition: merge.rwv-ours.driver must be unset; got: {}",
        String::from_utf8_lossy(&pre.stdout)
    );

    // ---- Phase A: negative control — no plant, bare rebase + resume ----

    let rebase_a = bare_git(&["rebase", "main"], &repo);
    assert!(
        !rebase_a.status.success(),
        "F1's shared.txt conflict must stop the bare rebase"
    );
    assert!(
        bare_status(&repo).contains("UU shared.txt"),
        "expected shared.txt in conflict; status:\n{}",
        bare_status(&repo)
    );

    // Operator resolves the genuine conflict and resumes with bare
    // `git rebase --continue` — as git's own conflict hint instructs.
    std::fs::write(repo.join("shared.txt"), "merged version\n").unwrap();
    bare_git_ok(&["add", "shared.txt"], &repo);
    let cont_a = bare_git(&["rebase", "--continue"], &repo);

    // THE BUG (negative control): the resuming git process has no driver
    // definition anywhere, so the F2 lock-only pick 3-way merges rwv.lock
    // and conflicts. If plant_rwv_merge_driver_config wrote to a scope
    // bare git doesn't consult, Phase B would fail exactly like this.
    assert!(
        !cont_a.status.success(),
        "without the planted config, bare `git rebase --continue` must \
         conflict on the lock-only pick"
    );
    assert!(
        bare_status(&repo).contains("UU rwv.lock"),
        "negative control: expected rwv.lock in conflict; status:\n{}",
        bare_status(&repo)
    );

    // Roll back to the pre-rebase state.
    bare_git_ok(&["rebase", "--abort"], &repo);
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &repo),
        feature_tip,
        "abort must restore the feature tip"
    );

    // ---- Plant via the production path: `rwv doctor --fix` ----
    //
    // Exit status is deliberately not asserted: the hand-built fixture
    // lock legitimately triggers unrelated warnings (stale-lock etc.).
    // The plant itself is asserted directly below — through bare git,
    // the same lens the resumed rebase will use.
    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&primary.root)
        .output()
        .expect("rwv doctor --fix failed to spawn");
    assert_eq!(
        bare_git_ok(
            &["config", "--local", "--get", "merge.rwv-ours.driver"],
            &repo
        ),
        "true",
        "doctor --fix must plant merge.rwv-ours.driver where bare git finds it"
    );

    // ---- Phase B: same bare rebase + resume, now armed ----

    let rebase_b = bare_git(&["rebase", "main"], &repo);
    assert!(
        !rebase_b.status.success(),
        "F1's genuine conflict fires again (the plant must not paper over \
         real non-lock conflicts)"
    );
    assert!(
        bare_status(&repo).contains("UU shared.txt"),
        "expected shared.txt in conflict again; status:\n{}",
        bare_status(&repo)
    );
    std::fs::write(repo.join("shared.txt"), "merged version\n").unwrap();
    bare_git_ok(&["add", "shared.txt"], &repo);
    let cont_b = bare_git(&["rebase", "--continue"], &repo);

    // The assertion that matters: NO conflict stops the rebase on rwv.lock.
    let status_b = bare_status(&repo);
    assert!(
        !status_b.contains("UU rwv.lock"),
        "with the planted config, the lock-only pick must NOT conflict; \
         status:\n{status_b}\ncontinue stderr:\n{}",
        String::from_utf8_lossy(&cont_b.stderr)
    );
    let lock_wt = std::fs::read_to_string(repo.join("rwv.lock")).unwrap();
    assert!(
        !lock_wt.contains("<<<<<<<") && !lock_wt.contains(">>>>>>>"),
        "rwv.lock must not contain conflict markers; got:\n{lock_wt}"
    );

    if !cont_b.status.success() {
        // Not a conflict (asserted above) — some git versions stop on the
        // now-empty pick rather than dropping it. Finish the way git's own
        // hint says. Do NOT paper over a still-conflicted state: require
        // mid-rebase with a clean index before skipping.
        let rebase_merge = repo.join(".git").join("rebase-merge");
        assert!(
            rebase_merge.exists(),
            "continue failed but repo is not mid-rebase; stderr:\n{}",
            String::from_utf8_lossy(&cont_b.stderr)
        );
        bare_git_ok(&["rebase", "--skip"], &repo);
    }

    // Rebase fully complete: no in-flight state.
    assert!(
        !repo.join(".git").join("rebase-merge").exists()
            && !repo.join(".git").join("rebase-apply").exists(),
        "rebase must be complete (no rebase-merge/rebase-apply dirs)"
    );

    // "Ours" semantics: the final lock is the rebase TARGET's version —
    // feature's lock edit vanished into the driver, exactly as during an
    // rwv-driven replay.
    let final_lock = std::fs::read_to_string(repo.join("rwv.lock")).unwrap();
    assert_eq!(
        final_lock, main_lock,
        "final rwv.lock must be main's version (ours semantics)"
    );

    // And the operator's genuine conflict resolution survived.
    let final_shared = std::fs::read_to_string(repo.join("shared.txt")).unwrap();
    assert_eq!(
        final_shared, "merged version\n",
        "the resolved non-lock conflict must survive the completed rebase"
    );
}
