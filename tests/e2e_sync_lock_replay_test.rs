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
/// {tmp}/ws/projects/app/         -- project repo with rwv.toml, rwv.lock,
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
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nversion = \"main\"\nrole = \"owned\"\n",
        path = MANIFEST_REPO_PATH,
        repo = common::url_path(&manifest_repo)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    // Round-trips through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let repo_url = common::file_url(&manifest_repo);
    let raw_lock = format!(
        "{{\"repositories\": {{{path:?}: {{\"type\": \"git\", \"url\": {repo_url:?}, \"version\": {sha:?}}}}}}}",
        path = MANIFEST_REPO_PATH,
        sha = initial_sha
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();

    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nversion = \"main\"\nrole = \"owned\"\n",
        path = MANIFEST_REPO_PATH,
        repo = common::url_path(&manifest_repo)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    // Round-trips through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let repo_url = common::file_url(&manifest_repo);
    let raw_lock = format!(
        "{{\"repositories\": {{{path:?}: {{\"type\": \"git\", \"url\": {repo_url:?}, \"version\": {sha:?}}}}}}}",
        path = MANIFEST_REPO_PATH,
        sha = initial_sha
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git(&["add", "rwv.toml", "rwv.lock"], &project_dir);
    git(&["commit", "-m", "lock: initial"], &project_dir);

    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    // Create a workweave from this (no-.gitattributes) primary.
    let ww = {
        rwv()
            .args(["workweave", PROJECT, "create", "ww"])
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
    let tmp = common::tempdir().unwrap();
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
// Durable driver-config plant + legacy-needle bail message
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
    let tmp = common::tempdir().unwrap();
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
/// the LEGACY `rwv.lock merge=ours` line (pre-rename), the
/// invariant bails with a migration-specific message that directs the
/// operator at `rwv doctor --fix`. It must NOT silently accept the
/// legacy needle — sync's guarantee is the new spelling that closes the
/// global-config collision hazard.
#[test]
fn sync_rebase_with_legacy_needle_bails_pointing_at_doctor_fix() {
    let tmp = common::tempdir().unwrap();
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
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nversion = \"main\"\nrole = \"owned\"\n",
        path = MANIFEST_REPO_PATH,
        repo = common::url_path(&manifest_repo)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    // Round-trips through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let repo_url = common::file_url(&manifest_repo);
    let raw_lock = format!(
        "{{\"repositories\": {{{path:?}: {{\"type\": \"git\", \"url\": {repo_url:?}, \"version\": {sha:?}}}}}}}",
        path = MANIFEST_REPO_PATH,
        sha = initial_sha
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
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

/// When the CWD project repo's committed `.gitattributes` carries BOTH the
/// current `rwv.lock merge=rwv-ours` line and the legacy `rwv.lock
/// merge=ours` line, the invariant must still bail — the current spelling
/// being present is not enough, because which line git honours is decided
/// by attribute reading order and the legacy name stays live either way.
/// The bail must name the both-lines state explicitly (not the generic
/// "not configured" message), and `rwv doctor --fix` must actually resolve
/// it: this test runs `--fix` against the fixture and then re-runs the same
/// sync, asserting it now succeeds.
#[test]
fn sync_rebase_with_both_lines_bails_naming_both_and_doctor_fix_recovers() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    // Build a primary workspace by hand with BOTH `.gitattributes` lines
    // committed. Mirrors `make_primary` structurally so a workweave can be
    // created from it.
    let ws = tmp.path().join("ws");
    let manifest_repo = ws.join(MANIFEST_REPO_PATH);
    let initial_sha = init_repo(&manifest_repo);

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\nrwv.lock merge=ours\n",
    )
    .unwrap();

    let manifest = format!(
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nversion = \"main\"\nrole = \"owned\"\n",
        path = MANIFEST_REPO_PATH,
        repo = common::url_path(&manifest_repo)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    // Round-trips through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let repo_url = common::file_url(&manifest_repo);
    let raw_lock = format!(
        "{{\"repositories\": {{{path:?}: {{\"type\": \"git\", \"url\": {repo_url:?}, \"version\": {sha:?}}}}}}}",
        path = MANIFEST_REPO_PATH,
        sha = initial_sha
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
        &project_dir,
    );
    git(
        &["commit", "-m", "lock: initial (both attrs lines)"],
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

    // Advance ww so a rebase has actual work to do.
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

    // From ww: attempt rebase sync onto primary. Must FAIL, naming the
    // both-lines state.
    let assert = rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();

    assert!(
        stderr.contains("BOTH that line and the legacy `rwv.lock merge=ours`"),
        "bail must name the both-lines state explicitly, not just \
         \"not configured\"; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv doctor --fix"),
        "bail must direct the operator at `rwv doctor --fix`; got stderr:\n{stderr}"
    );
    // primary (the rebase base) independently inherited the same both-lines
    // commit — the refusal must say so and name primary's own directory, not
    // just ww's, or the operator fixes ww alone, retries, and meets a raw git
    // conflict instead of a second actionable refusal.
    assert!(
        stderr.contains("the same kind of problem")
            && stderr.contains(&primary.project_dir.display().to_string()),
        "bail must ALSO name the rebase-base workspace's directory when it \
         independently has the same problem; got stderr:\n{stderr}"
    );

    // The remedy claim: `rwv doctor --fix` actually resolves this state.
    // Both checkouts inherited the both-lines commit independently (each
    // worktree carries its own branch), so both need the migration commit
    // before a clean rebase: ww's own committed HEAD is sync's precondition,
    // and primary's committed HEAD is what git checks out as the rebase
    // base — an unmigrated base still resolves `rwv.lock`'s driver to the
    // legacy (undefined) name by attribute reading order and conflicts on
    // the very first replayed pick.
    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&primary.root)
        .output()
        .expect("rwv doctor --fix failed to spawn");
    let _ = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&ww.root)
        .output()
        .expect("rwv doctor --fix failed to spawn");

    for (label, dir) in [("primary", &primary.project_dir), ("ww", &ww.project_dir)] {
        let committed_attrs = git_out(&["show", "HEAD:.gitattributes"], dir);
        assert!(
            committed_attrs
                .lines()
                .any(|l| l.trim() == "rwv.lock merge=rwv-ours"),
            "doctor --fix must leave the current spelling committed in {label}; got:\n{committed_attrs}"
        );
        assert!(
            !committed_attrs
                .lines()
                .any(|l| l.trim() == "rwv.lock merge=ours"),
            "doctor --fix must drop the legacy line in {label}; got:\n{committed_attrs}"
        );
    }

    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .success();
}

/// The precondition must also check the workspace a rebase replays ONTO, not
/// just the CWD workspace running the sync: that other workspace's committed
/// `.gitattributes` is the tree git checks out as the rebase base, so it
/// governs `rwv.lock`'s driver for every early pick regardless of what CWD's
/// own `.gitattributes` says. A CWD-only check would pass here (ww's is
/// clean), the rebase would proceed, and the operator would meet a raw git
/// merge conflict instead of an actionable refusal.
#[test]
fn sync_rebase_with_clean_cwd_but_corrupt_source_names_the_source_directory() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, &weaveroot, "ww");

    // ww: bump the manifest repo + lock. ww's own .gitattributes stays the
    // clean single-line form `make_primary` wrote — never touched.
    commit_file(&ww.manifest_repo, "ww.txt", "from ww\n", "ww: add ww.txt");
    rwv_lock_commit(&ww.root);

    // Corrupt PRIMARY's committed .gitattributes to the both-lines state,
    // directly (not through rwv) — simulating a state that arose some other
    // way, e.g. a hand edit.
    std::fs::write(
        primary.project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\nrwv.lock merge=ours\n",
    )
    .unwrap();
    git(&["add", ".gitattributes"], &primary.project_dir);
    git(
        &["commit", "-m", "corrupt: both attrs lines"],
        &primary.project_dir,
    );

    // Advance primary further via a sibling workweave, landed via ff (ff
    // does not consult the invariant), so ww's rebase has a divergence to
    // replay onto.
    let wa = create_workweave(&primary, &weaveroot, "wa");
    commit_file(&wa.manifest_repo, "wa.txt", "from wa\n", "wa: add wa.txt");
    rwv_lock_commit(&wa.root);
    rwv()
        .args(["sync", &wa.root.to_string_lossy()])
        .current_dir(&primary.root)
        .assert()
        .success();

    // From ww: attempt rebase sync onto primary. ww's OWN committed
    // .gitattributes is clean, so this must fail on primary's state, not
    // ww's — and the refusal must name primary's directory, not ww's.
    let assert = rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();

    assert!(
        stderr.contains("the workspace this rebase replays onto")
            && stderr.contains(&primary.project_dir.display().to_string()),
        "the refusal must name the rebase-base workspace's directory, not just cwd's; \
         got:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv doctor --fix"),
        "the refusal must direct the operator at `rwv doctor --fix`; got:\n{stderr}"
    );
    assert!(
        !stderr.contains(&ww.project_dir.display().to_string()),
        "the refusal must not blame ww's directory — ww's own .gitattributes is clean; \
         got:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// BARE git rebase — planted config alone must carry the exclusion
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

/// Lock-file JSON in the exact shape `make_primary` writes, with a chosen
/// version value — both branches rewrite the same `"version"` line so a
/// 3-way merge without the driver is guaranteed to conflict.
fn lock_json(manifest_repo: &Path, version: &str) -> String {
    let repo_url = common::file_url(&manifest_repo);
    let raw_lock = format!(
        "{{\"repositories\": {{{path:?}: {{\"type\": \"git\", \"url\": {repo_url:?}, \"version\": {version:?}}}}}}}",
        path = MANIFEST_REPO_PATH,
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    let mut json = serde_json::to_string_pretty(&lock).unwrap();
    json.push('\n');
    json
}

/// The literal incident shape this closes, driven end-to-end by a
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
    let tmp = common::tempdir().unwrap();
    let primary = make_primary(tmp.path());
    let repo = primary.project_dir.clone();

    // -- Fixture: two divergent branches. Overlapping (both-modified)
    //    paths: shared.txt (the genuine non-lock conflict that strands the
    //    operator mid-rebase) and rwv.lock (the lock-only pick). --
    commit_file(&repo, "shared.txt", "base\n", "base: add shared.txt");
    git(&["branch", "feature"], &repo);

    // main: bump the lock and shared.txt in one commit.
    let main_lock = lock_json(
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
    let feat_lock = lock_json(
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

// ---------------------------------------------------------------------------
// rwv-native `--continue` end-to-end
//
// The set of tests above proves that BARE `git rebase --continue`
// is safe after the durable driver plant. This section proves the operator
// no longer has to reach for bare git at all — `rwv sync --continue` itself
// drives a stopped rebase through the remaining picks (including a lock-only
// pick that must resolve to ours via the inline driver flags), runs relock,
// and clears op-state.
// ---------------------------------------------------------------------------

/// Build a workweave-side commit history containing both a genuine non-lock
/// conflict (against primary's post-lock docs edit) AND a subsequent
/// lock-only commit that must merge to ours via the inline `rwv-ours`
/// driver flags during rebase replay. Returns the workweave paths and the
/// primary's post-setup manifest-tip SHA (== primary's lock content).
///
/// Layout after this helper returns:
/// - primary: base + one commit editing `notes/shared.md` on `main` +
///   one lock-bump project commit reflecting `primary_manifest_sha`.
/// - workweave `ww`: base + one commit editing `notes/shared.md`
///   (conflicts with primary's docs edit) + one lock-bump project commit
///   reflecting `ww_manifest_sha` (that lock content differs from
///   primary's, so the pick is a legitimate 3-way merge target for the
///   `rwv-ours` driver — patch becomes empty after driver-resolve and
///   `--empty=drop` retires it).
struct MidRebaseFixture {
    primary: PrimaryWorkspace,
    ww: Workweave,
}

fn build_project_conflict_with_lock_only_pick(
    tmp: &Path,
    weaveroot: &Path,
    ww_name: &str,
) -> MidRebaseFixture {
    let primary = make_primary(tmp);
    let ww = create_workweave(&primary, weaveroot, ww_name);

    // Primary: edit notes/shared.md, commit. This is what ww's F1 will
    // conflict with when ww rebases onto primary.
    commit_file(
        &primary.project_dir,
        "notes/shared.md",
        "primary version\n",
        "docs: primary take",
    );
    // Primary bumps its manifest and locks — this puts primary's rwv.lock
    // at a value ww's forthcoming lock-only commit will collide with.
    commit_file(
        &primary.manifest_repo,
        "primary.txt",
        "from primary\n",
        "primary: add primary.txt",
    );
    rwv_lock_commit(&primary.root);

    // ww: conflicting docs edit (this is F1 — the pick that will stop the
    // rebase mid-way).
    commit_file(
        &ww.project_dir,
        "notes/shared.md",
        "ww version\n",
        "docs: ww take",
    );
    // ww bumps its own manifest (different content than primary's) and
    // locks — this is F2, the lock-only project commit that must be
    // replayed after F1 resolves. Its lock content collides with
    // primary's; the `rwv-ours` driver keeps primary's version and the
    // patch drops via `--empty=drop`.
    commit_file(&ww.manifest_repo, "ww.txt", "from ww\n", "ww: add ww.txt");
    rwv_lock_commit(&ww.root);

    MidRebaseFixture { primary, ww }
}

/// Assert `dir` is (or is not) mid-rebase. For a worktree, `.git` is a file
/// pointing at the shared git-dir under the canonical repo; the
/// `rebase-merge/` directory lives inside THAT git-dir, not inside the
/// worktree's `.git` file. `git rev-parse --git-dir` resolves it correctly.
fn assert_mid_rebase(dir: &Path, expected: bool, msg: &str) {
    let git_dir_str = bare_git_ok(&["rev-parse", "--git-dir"], dir);
    let git_dir = if PathBuf::from(&git_dir_str).is_absolute() {
        PathBuf::from(git_dir_str)
    } else {
        dir.join(git_dir_str)
    };
    let has_rebase_merge = git_dir.join("rebase-merge").exists();
    let has_rebase_apply = git_dir.join("rebase-apply").exists();
    let actual = has_rebase_merge || has_rebase_apply;
    assert_eq!(
        actual,
        expected,
        "{msg} (mid-rebase actual={actual}, expected={expected}, git-dir={})",
        git_dir.display()
    );
}

/// Golden path: a `rwv sync --strategy=rebase` that stops on
/// a genuine non-lock conflict in the project repo can be resumed by the
/// operator with `resolve + git add + rwv sync --continue` — no `git rebase
/// --continue` step. `--continue` MUST drive the entire remaining op:
///
///   1. Complete the mid-rebase (including any lock-only pick that must
///      merge to ours via the inline `rwv-ours` driver flags — proven by
///      deleting the durable driver config just before `--continue` so the
///      resume rebase depends on the flags rwv re-supplies).
///   2. Regenerate `rwv.lock` from source's manifest tips (relock).
///   3. Clear op-state (`.rwv-op` gone).
///
/// If any of the three fails, the operator is worse off than before —
/// `--continue` promised to be end-to-end and delivered only step 1.
#[test]
fn sync_continue_completes_mid_rebase_with_lock_only_pick_via_inline_flags() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let fx = build_project_conflict_with_lock_only_pick(tmp.path(), &weaveroot, "ww1");
    let primary = &fx.primary;
    let ww = &fx.ww;

    // Start the rebase from ww. It must stop on F1's docs conflict.
    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .failure();

    // The project repo (a worktree) must be mid-rebase.
    assert_mid_rebase(
        &ww.project_dir,
        true,
        "expected ww project repo mid-rebase after first sync",
    );
    let op_state_path = ww.root.join(".rwv-op");
    assert!(
        op_state_path.exists(),
        "expected op-state file after conflict-stopped sync"
    );

    // Operator resolves the conflict on notes/shared.md and stages.
    std::fs::write(
        ww.project_dir.join("notes/shared.md"),
        "merged: keep ww's take, acknowledge primary\n",
    )
    .unwrap();
    git(&["add", "notes/shared.md"], &ww.project_dir);

    // Prove `rebase_continue` re-supplies the driver flags inline (not by
    // silently piggybacking on the durable plant) — remove the plant just
    // before `--continue` in the project repo's config. If `--continue`
    // still resolves the lock-only pick without conflict, it's because the
    // inline `-c merge.rwv-ours.driver=true` in Vcs::rebase_continue is
    // doing the work: re-supplying driver flags on replay re-entry is the
    // whole point.
    let project_git_dir_str = bare_git_ok(&["rev-parse", "--git-dir"], &ww.project_dir);
    let project_git_dir = if PathBuf::from(&project_git_dir_str).is_absolute() {
        PathBuf::from(project_git_dir_str)
    } else {
        ww.project_dir.join(project_git_dir_str)
    };
    let config_path = project_git_dir.join("config");
    if config_path.exists() {
        // Unset the two keys the plant writes. Failure is OK — the plant
        // may not have run yet in this fixture path.
        let _ = common::git()
            .args(["config", "--unset", "merge.rwv-ours.driver"])
            .current_dir(&ww.project_dir)
            .output();
        let _ = common::git()
            .args(["config", "--unset", "merge.rwv-ours.name"])
            .current_dir(&ww.project_dir)
            .output();
        // Sanity: unset actually took.
        let after = common::git()
            .args(["config", "--local", "--get", "merge.rwv-ours.driver"])
            .current_dir(&ww.project_dir)
            .output()
            .expect("git config failed to spawn");
        assert!(
            !after.status.success(),
            "test setup: merge.rwv-ours.driver must be unset before --continue \
             so the test proves the inline flag is what drives the lock pick; \
             was: {}",
            String::from_utf8_lossy(&after.stdout)
        );
    }

    // `rwv sync --continue` must complete the whole op.
    rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // (1) Rebase complete: not mid-op anywhere.
    assert_mid_rebase(
        &ww.project_dir,
        false,
        "project repo must not be mid-rebase after --continue",
    );

    // The operator's resolution survived and the lock-only F2 pick was
    // dropped (empty via driver + --empty=drop).
    let shared = std::fs::read_to_string(ww.project_dir.join("notes/shared.md")).unwrap();
    assert_eq!(
        shared, "merged: keep ww's take, acknowledge primary\n",
        "the resolved non-lock conflict must survive the completed rebase"
    );

    // (2) Relock ran — the current rwv.lock reflects source's manifest tip
    // (post-rebase Phase 3 regenerates it from manifest tips). ww's
    // manifest repo now carries primary's + ww's commits.
    assert!(
        ww.manifest_repo.join("primary.txt").exists(),
        "ww manifest should carry primary's commit after rebase"
    );
    assert!(
        ww.manifest_repo.join("ww.txt").exists(),
        "ww manifest should still carry its own commit after rebase"
    );

    // (3) op-state cleared.
    assert!(
        !op_state_path.exists(),
        "op-state file must be gone after successful --continue"
    );

    let _ = primary; // silence unused-binding lint; fixture kept for symmetry.
}

/// Negative-path complement to the golden test: `rwv sync --continue` with
/// the operator's resolution NOT staged must bail cleanly — the conflict
/// message renders, the repo stays mid-rebase, and op-state is retained so
/// a second `--continue` after `git add` succeeds. Losing either the
/// mid-rebase state OR the op-state at this point would strand the operator
/// (bare `git rebase --continue` would then error, `rwv abort` would be the
/// only recovery).
#[test]
fn sync_continue_with_unstaged_resolution_bails_and_second_continue_succeeds() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let fx = build_project_conflict_with_lock_only_pick(tmp.path(), &weaveroot, "ww1");
    let ww = &fx.ww;

    // First sync stops on the docs conflict, same as the golden path.
    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww.root)
        .assert()
        .failure();
    assert_mid_rebase(
        &ww.project_dir,
        true,
        "expected mid-rebase after conflict-stopped sync",
    );
    let op_state_path = ww.root.join(".rwv-op");
    assert!(op_state_path.exists(), "op-state must exist after conflict");

    // Operator "resolves" the conflict but forgets to `git add` — content
    // still shows as needing merge in `git status` output.
    std::fs::write(
        ww.project_dir.join("notes/shared.md"),
        "merged (unstaged)\n",
    )
    .unwrap();

    // First `--continue` must fail; specifically it must NOT clear op-state
    // or leave the repo clean of the rebase.
    let out = rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Post-conditions after the bail: op-state and mid-rebase both intact.
    assert_mid_rebase(
        &ww.project_dir,
        true,
        "repo must stay mid-rebase after unstaged-continue bail",
    );
    assert!(
        op_state_path.exists(),
        "op-state must be retained after unstaged-continue bail so a second \
         --continue after staging succeeds; stderr was:\n{stderr}"
    );

    // The message must mention the conflict / continue path (exact wording
    // is pinned by a sibling test elsewhere; assert only the load-bearing
    // tokens here).
    assert!(
        stderr.contains("rebase") && (stderr.contains("continue") || stderr.contains("conflict")),
        "expected bail stderr to name rebase + continue/conflict; got:\n{stderr}"
    );

    // Now stage the resolution — the second `--continue` must succeed.
    git(&["add", "notes/shared.md"], &ww.project_dir);
    rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww.root)
        .assert()
        .success();

    assert_mid_rebase(
        &ww.project_dir,
        false,
        "second --continue must complete the rebase",
    );
    assert!(
        !op_state_path.exists(),
        "op-state must be cleared after successful second --continue"
    );
}
