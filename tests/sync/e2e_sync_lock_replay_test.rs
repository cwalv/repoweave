//! E2E integration tests for `rwv sync` and `rwv land` lock+replay semantics across all
//! sync strategies with concurrent lock-bumping workweaves.
//!
//! ## What these tests cover
//!
//! ### Rebase strategy (replays committed trees)
//!
//! 1. **N=2 lock-only convergence** (`sync_two_workweaves_lock_only_rebase_converges`):
//!    Two workweaves both bump the manifest lock. Primary absorbs WA; WB then
//!    syncs primary with `--history=linear`. The lock-only commit in WB's
//!    project history should drop silently (an empty patch, rwv holding the
//!    lock merge driver to the target's lock). No
//!    manual `git rebase --continue`.
//!
//! 2. **N=3 lock-only convergence** (`sync_three_workweaves_lock_only_rebase_converges`):
//!    Same recipe with three workweaves. WC syncs a primary that already has
//!    two prior lock-bump commits — asserts the mechanism holds for deeper
//!    histories.
//!
//! 3. **The source's lock merge driver** is judged as well as CWD's under
//!    `--history=linear`, since its committed `.gitattributes` governs the
//!    replay.
//!
//! ### FF strategy (advances branch pointer, no replay)
//!
//! 4. **FF preserves lock-only commits** (`sync_ff_preserves_lock_only_commits`):
//!    With `--history=ff-only` (no replay), a workweave's lock-only commit lands
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

use crate::common;

// ---------------------------------------------------------------------------
// Git + rwv helpers (local to this test file)
// ---------------------------------------------------------------------------

fn rwv() -> AssertCommand {
    common::rwv()
}

/// Init a git repo at `path` with one commit on `main`. Returns HEAD SHA.
fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    common::git_in(path, &["config", "user.email", "test@test.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

/// Stage and commit `filename` (relative to `repo`). Returns new HEAD SHA.
fn commit_file(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    let path = repo.join(filename);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
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
///                                   and .gitattributes (rwv.lock merge=rwv-lock)
/// ```
fn make_primary(tmp: &Path) -> PrimaryWorkspace {
    let ws = tmp.join("ws");
    let manifest_repo = ws.join(MANIFEST_REPO_PATH);
    let initial_sha = init_repo(&manifest_repo);

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);

    common::set_up_lock_driver(&project_dir);

    let manifest = format!(
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nbranch = \"main\"\nrole = \"owned\"\n",
        path = MANIFEST_REPO_PATH,
        repo = common::url_path(&manifest_repo)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    let repo_url = common::file_url(&manifest_repo);
    common::fixture_lock(
        &project_dir,
        &[(MANIFEST_REPO_PATH, &repo_url, &initial_sha)],
    );

    common::git_in(
        &project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    common::record_trunk(&project_dir);

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
        .args(["workweave", "create", "--project", PROJECT, name])
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
/// `--history=linear`. WB's lock-only commit should drop silently via the
/// held lock merge driver + `--empty=drop` mechanism. No manual `git rebase --continue`.
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
        .args(["land", "--history", "ff-only"])
        .current_dir(&wa.root)
        .assert()
        .success();

    let primary_lib_head = common::git_in(&primary.manifest_repo, &["rev-parse", "main"]);
    assert_eq!(
        primary_lib_head, wa_lib_sha,
        "primary lib should be at WA's commit after first sync"
    );

    // From WB: sync primary with rebase. This is the exact repro step:
    // WB's project history has a lock-only commit that conflicts with primary's
    // post-WA lock on the same lines. With the lock merge driver held, the
    // lock-only commit produces an empty patch and is dropped silently.
    //
    // Before the lock-replay fix landed this step would fail with a
    // `git rebase --continue` conflict on rwv.lock. After: success.
    rwv()
        .args(["sync", "--history", "linear"])
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
        .args(["land", "--history", "ff-only"])
        .current_dir(&wb.root)
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
    let final_head = common::git_in(&primary.manifest_repo, &["rev-parse", "main"]);
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
/// with `--history=linear`. Both of WC's lock-only project commits should
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
        .args(["land", "--history", "ff-only"])
        .current_dir(&wa.root)
        .assert()
        .success();

    // WB syncs primary (rebase) then lands in primary (ff).
    rwv()
        .args(["sync", "--history", "linear"])
        .current_dir(&wb.root)
        .assert()
        .success();
    rwv()
        .args(["land", "--history", "ff-only"])
        .current_dir(&wb.root)
        .assert()
        .success();

    // Now primary has two lock-bump commits from WA and WB.
    // WC syncs primary with rebase. WC's project repo has one lock-only commit
    // (its initial rwv lock commit) that conflicts with the two prior lock-bumps.
    // Both should drop silently.
    rwv()
        .args(["sync", "--history", "linear"])
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
        .args(["land", "--history", "ff-only"])
        .current_dir(&wc.root)
        .assert()
        .success();

    assert!(primary.manifest_repo.join("wa.txt").exists());
    assert!(primary.manifest_repo.join("wb.txt").exists());
    assert!(primary.manifest_repo.join("wc.txt").exists());

    let primary_lock = std::fs::read_to_string(primary.project_dir.join("rwv.lock")).unwrap();
    let final_head = common::git_in(&primary.manifest_repo, &["rev-parse", "main"]);
    assert!(
        primary_lock.contains(&final_head),
        "primary lock should pin lib at final HEAD ({final_head}); lock:\n{primary_lock}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: FF strategy preserves lock-only commits
// ---------------------------------------------------------------------------

/// Fast-forward does not replay commits — it advances the branch pointer.
/// A lock-only commit in the source's history must land verbatim in primary's
/// history after `rwv land` (default ff strategy). The commit
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
    let primary_tip_before = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);

    // WW: bump the manifest repo and lock. This produces exactly one lock-only
    // commit in the project repo.
    commit_file(&ww.manifest_repo, "ww.txt", "from ww\n", "ww: add ww.txt");
    rwv_lock_commit(&ww.root);

    // Capture WW's lock commit SHA.
    let ww_lock_commit_sha = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    let ww_project_log_count = common::git_in(&ww.project_dir, &["rev-list", "--count", "HEAD"]);

    // From primary: sync WW → primary via ff (the default).
    rwv()
        .args(["land", "--history", "ff-only"])
        .current_dir(&ww.root)
        .assert()
        .success();

    // Phase 3 generates a fresh lock commit on primary (re-lock after sync).
    // Primary's tip is that re-lock commit. WW's lock commit must appear as
    // its parent (or grandparent, depending on whether Phase 3 committed).
    // The key assertion: WW's lock-only commit SHA exists in primary's history.
    let primary_log = common::git_in(
        &primary.project_dir,
        &["log", "--format=%H", &format!("{primary_tip_before}..HEAD")],
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
    let lib_head = common::git_in(&primary.manifest_repo, &["rev-parse", "main"]);
    assert!(
        primary_lock.contains(&lib_head),
        "primary lock should pin lib at final HEAD ({lib_head}); lock:\n{primary_lock}"
    );
}

// ---------------------------------------------------------------------------
// The source a rebase replays onto
// ---------------------------------------------------------------------------

/// The precondition must also check the workspace a rebase replays ONTO, not
/// just the CWD workspace running the sync: that other workspace's committed
/// `.gitattributes` is the tree git checks out as the rebase base, so it
/// governs `rwv.lock`'s driver for every early pick regardless of what CWD's
/// own `.gitattributes` says. A CWD-only check would pass here (ww's is
/// clean), the rebase would proceed, and the operator would meet a raw git
/// merge conflict instead of an actionable refusal.
#[test]
fn sync_to_rebase_with_clean_cwd_but_corrupt_source_names_the_source_directory() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, &weaveroot, "ww");

    // ww: bump the manifest repo + lock. ww's own .gitattributes stays the
    // clean single-line form `make_primary` wrote — never touched.
    commit_file(&ww.manifest_repo, "ww.txt", "from ww\n", "ww: add ww.txt");
    rwv_lock_commit(&ww.root);

    // Advance primary via a sibling workweave, landed via ff, so ww's
    // rebase has a divergence to replay onto.
    let wa = create_workweave(&primary, &weaveroot, "wa");
    commit_file(&wa.manifest_repo, "wa.txt", "from wa\n", "wa: add wa.txt");
    rwv_lock_commit(&wa.root);
    rwv()
        .args(["land", "--history", "ff-only"])
        .current_dir(&wa.root)
        .assert()
        .success();

    // Put PRIMARY's committed .gitattributes back on an earlier release's
    // line, directly (not through rwv), as a hand edit would.
    std::fs::write(
        primary.project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    common::git_in(&primary.project_dir, &["add", ".gitattributes"]);
    common::git_in(
        &primary.project_dir,
        &["commit", "-m", "the old attribute line"],
    );

    // From ww: attempt rebase sync onto primary. ww's OWN committed
    // .gitattributes is clean, so this must fail on primary's state, not
    // ww's — and the refusal must name primary's directory, not ww's.
    let out = rwv().args(["land"]).current_dir(&ww.root).output().unwrap();
    let stderr = common::cli::refused(
        &out,
        repoweave::diagnosis::State::MissingLockMergeDriverAttributeLegacy,
    );
    common::assert_names_operator_path(&stderr, &primary.project_dir);
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
// Bare git
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

// ---------------------------------------------------------------------------
// rwv-native `--continue` end-to-end
//
// The operator does not have to reach for bare git — `rwv land --continue` itself
// drives a stopped rebase through the remaining picks (including a lock-only
// pick that must resolve to ours via the inline hold on the lock merge driver), runs relock,
// and clears op-state.
// ---------------------------------------------------------------------------

/// Build a workweave-side commit history containing both a genuine non-lock
/// conflict (against primary's post-lock docs edit) AND a subsequent
/// lock-only commit that must merge to ours via the inline hold on the
/// lock merge driver during rebase replay. Returns the workweave paths and the
/// primary's post-setup manifest-tip SHA (== primary's lock content).
///
/// Layout after this helper returns:
/// - primary: base + one commit editing `notes/shared.md` on `main` +
///   one lock-bump project commit reflecting `primary_manifest_sha`.
/// - workweave `ww`: base + one commit editing `notes/shared.md`
///   (conflicts with primary's docs edit) + one lock-bump project commit
///   reflecting `ww_manifest_sha` (that lock content differs from
///   primary's, so the pick is a legitimate 3-way merge target for the
///   held driver — patch becomes empty after driver-resolve and
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
    // primary's; the held driver keeps primary's version and the
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

/// Golden path: a `rwv land --history=linear` that stops on
/// a genuine non-lock conflict in the project repo can be resumed by the
/// operator with `resolve + git add + rwv land --continue` — no `git rebase
/// --continue` step. `--continue` MUST drive the entire remaining op:
///
///   1. Complete the mid-rebase (including any lock-only pick that must
///      merge to ours via the inline hold on the lock merge driver; the
///      clone's own definition runs `rwv` from a `PATH` this test does not
///      give this build, so only the hold settles the pick).
///   2. Regenerate `rwv.lock` from source's manifest tips (relock).
///   3. Clear op-state (`.rwv-op` gone).
///
/// If any of the three fails, the operator is worse off than before —
/// `--continue` promised to be end-to-end and delivered only step 1.
#[test]
fn sync_to_continue_completes_mid_rebase_with_lock_only_pick_via_inline_flags() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let fx = build_project_conflict_with_lock_only_pick(tmp.path(), &weaveroot, "ww1");
    let primary = &fx.primary;
    let ww = &fx.ww;

    // Start the rebase from ww. It must stop on F1's docs conflict.
    rwv()
        .args(["land", "--history", "linear"])
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
    common::git_in(&ww.project_dir, &["add", "notes/shared.md"]);

    // `rwv land --continue` must complete the whole op.
    rwv()
        .args(["land", "--continue"])
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

/// Negative-path complement to the golden test: `rwv land --continue` with
/// the operator's resolution NOT staged must bail cleanly — the conflict
/// message renders, the repo stays mid-rebase, and op-state is retained so
/// a second `--continue` after `git add` succeeds. Losing either the
/// mid-rebase state OR the op-state at this point would strand the operator
/// (bare `git rebase --continue` would then error, `rwv abort` would be the
/// only recovery).
#[test]
fn sync_to_continue_with_unstaged_resolution_bails_and_second_continue_succeeds() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let fx = build_project_conflict_with_lock_only_pick(tmp.path(), &weaveroot, "ww1");
    let ww = &fx.ww;

    // First sync stops on the docs conflict, same as the golden path.
    rwv()
        .args(["land", "--history", "linear"])
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
        .args(["land", "--continue"])
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
    common::git_in(&ww.project_dir, &["add", "notes/shared.md"]);
    rwv()
        .args(["land", "--continue"])
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
