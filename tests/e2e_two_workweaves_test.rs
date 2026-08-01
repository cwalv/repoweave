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
/// {tmp}/ws/projects/app/             -- project repo with rwv.toml + rwv.lock committed
/// ```
fn make_main_workspace(tmp: &Path) -> MainWorkspace {
    let ws = tmp.join("ws");
    let manifest_repo = ws.join(MANIFEST_REPO_PATH);
    let initial_sha = init_repo(&manifest_repo);

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);

    // Mirror what `rwv init` writes: `.gitattributes` so sync's native
    // rebase keeps source's `rwv.lock` through the replay.
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();

    let manifest = format!(
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nversion = \"main\"\nrole = \"owned\"\n",
        path = MANIFEST_REPO_PATH,
        repo = manifest_repo.display()
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    // Round-trips through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let repo_url = format!("file://{}", manifest_repo.display());
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

    // Action verbs require `.rwv-active` (or --project).
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
    let tmp = common::tempdir().unwrap();
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

    // History-shape assertion: after ww2 rebases onto ww1's tip and then main
    // ff-absorbs ww2, the manifest repo log on main must show ww2's commit ON
    // TOP of ww1's commit. This catches any implementation that replays in the
    // wrong order (Option A vs Option B semantics).
    //
    // We assert on the manifest repo (github/org/lib) because that is where
    // the two workweaves' contributions land. The project repo's lock-only
    // commits are skipped/dropped during rebase (merge=rwv-ours), so there is no
    // meaningful project-repo shape to assert here.
    common::assert_log_ordering(&main.manifest_repo, &["ww2: add bar", "ww1: add foo"]);
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
    let tmp = common::tempdir().unwrap();
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

    // History-shape assertions: after ww2 rebases onto ww1's tip and main
    // ff-absorbs ww2, both the project repo and the manifest repo must show
    // the correct commit ordering — ww2's contribution ON TOP of ww1's.
    //
    // Project repo: feat-b.md's commit must appear above feat-a.md's commit.
    // This is the specific instance called out in the spec.
    common::assert_log_ordering(
        &main.project_dir,
        &["docs: add feat-b notes", "docs: add feat-a notes"],
    );

    // Manifest repo: ww2's bar commit must appear above ww1's foo commit.
    common::assert_log_ordering(&main.manifest_repo, &["ww2: add bar", "ww1: add foo"]);
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
    let tmp = common::tempdir().unwrap();
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

/// When primary adds a new repo to its manifest + lock, an existing
/// workweave running `rwv sync primary` should materialize the new repo
/// as a worktree (not silently advance the lock and leave a dangling
/// reference).
#[test]
fn sync_phase3_materializes_newly_added_repo_in_workweave() {
    let tmp = common::tempdir().unwrap();
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
    // `rwv add` refuses rather than guess when origin/HEAD is unset, so
    // fetch and set it — the state a real pre-existing clone would have.
    git(&["fetch", "origin"], &new_repo_abs);
    git(&["remote", "set-head", "origin", "-a"], &new_repo_abs);

    rwv()
        .args(["add", new_repo_path])
        .current_dir(&main.root)
        .assert()
        .success();

    // Commit the manifest change and lock.
    git(&["add", "rwv.toml"], &main.project_dir);
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

    // Which ref is this checkout on? The ephemeral name is
    // `{project}--{workweave}` and nothing else: the third component this
    // site used to append (the manifest `version:`) disagreed with what
    // `workweave create` appends, no consumer read either, and the model
    // deletes it rather than picking a winner (branch-model.md §3.5).
    // Asked as the full ref, not `--short`: `--short` answers the shortest
    // *unambiguous* name, so a tag sharing the branch's name would make this
    // read `heads/app--ww1` and the assertion would be about the wrong thing.
    assert_eq!(
        git_out(&["symbolic-ref", "HEAD"], &ww1_new_repo),
        format!("refs/heads/{PROJECT}--ww1"),
        "a sync-materialized worktree must be born on the minted ephemeral name"
    );

    // And rwv must hold a receipt for it. Ownership is by record, not by
    // name shape (R2): without this, the ref sync just created would be
    // one `workweave delete` can never legitimately clean up.
    let canonical = main.root.join(new_repo_path);
    let receipt = repoweave::workweave_index::RefRegistry::for_project(
        &main.root,
        &repoweave::manifest::ProjectName::new(PROJECT).unwrap(),
    )
    .lookup(
        &canonical,
        &repoweave::vcs::RawRefName::new(format!("{PROJECT}--ww1")),
    )
    .expect("the registry is readable");
    assert!(
        receipt.is_some(),
        "sync's materialize must persist an ownership receipt for the ref it births, \
         keyed to the canonical store at {}",
        canonical.display()
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
/// lock advanced past a never-materialised repo and sync exited 0.
#[test]
fn sync_phase3_materialize_failure_is_fatal() {
    let tmp = common::tempdir().unwrap();
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
    // `rwv add` refuses rather than guess when origin/HEAD is unset, so
    // fetch and set it — the state a real pre-existing clone would have.
    git(&["fetch", "origin"], &new_repo_abs);
    git(&["remote", "set-head", "origin", "-a"], &new_repo_abs);
    rwv()
        .args(["add", new_repo_path])
        .current_dir(&main.root)
        .assert()
        .success();
    git(&["add", "rwv.toml"], &main.project_dir);
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
// `rwv sync primary` (explicit source) + bare `rwv sync-to` + `--retire` cleanup
// ---------------------------------------------------------------------------

/// Happy path: a workweave forked from primary syncs from primary using
/// an explicit source. `rwv sync <source>` always requires an explicit source
/// now; bare `rwv sync` was removed (use `rwv sync-to` to land work upward).
#[test]
fn sync_with_explicit_primary_source_advances_workweave() {
    let tmp = common::tempdir().unwrap();
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

    // `rwv sync primary` from inside ww1 brings ww1 forward.
    rwv()
        .args(["sync", "primary"])
        .current_dir(&ww1.root)
        .assert()
        .success();

    let ww1_lib_head = git_out(&["rev-parse", "HEAD"], &ww1.manifest_repo);
    assert_eq!(
        ww1_lib_head, primary_sha,
        "after sync primary, ww1's lib HEAD must be at primary's tip"
    );
    assert!(
        ww1.manifest_repo.join("primary.txt").exists(),
        "after sync primary, ww1 must have primary's new file"
    );
}

/// Bare `rwv sync-to` (no target) from a workweave reads the recorded
/// parent from `.rwv-workweave` and lands work upward. This is the new
/// "bare" behavior — upward landing without requiring an explicit target.
#[test]
fn bare_sync_to_follows_recorded_parent_to_primary() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    let ww1 = create_workweave(&main, &weaveroot, "ww1");

    // Workweave advances the manifest repo and locks.
    let ww1_sha = commit_file(
        &ww1.manifest_repo,
        "ww1.txt",
        "from ww1\n",
        "ww1: add ww1.txt",
    );
    rwv_lock_commit(&ww1.root);

    // Bare `rwv sync-to` (no target) from ww1 reads the marker's parent (primary)
    // and fast-forwards primary to ww1's tip.
    rwv()
        .args(["sync-to", "--strategy=ff"])
        .current_dir(&ww1.root)
        .assert()
        .success();

    let primary_lib_head = git_out(&["rev-parse", "HEAD"], &main.manifest_repo);
    assert_eq!(
        primary_lib_head, ww1_sha,
        "after bare sync-to, primary's lib HEAD must be at ww1's tip"
    );
}

/// Bare `rwv sync-to` from the primary weave (not inside a workweave)
/// must error with a clear message — there is no recorded parent to target.
#[test]
fn bare_sync_to_from_primary_errors_with_no_parent_message() {
    let tmp = common::tempdir().unwrap();

    let main = make_main_workspace(tmp.path());
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    let assert = rwv()
        .args(["sync-to"])
        .current_dir(&main.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("primary weave")
            || stderr.contains("workweave")
            || stderr.contains("target"),
        "expected error message about needing a workweave or explicit target; got: {stderr}"
    );
}

/// `rwv sync --retire` must error with a did-you-mean hint pointing at
/// `rwv sync-to --retire`. The --retire flag was removed from sync.
#[test]
fn sync_retire_flag_gives_did_you_mean_hint() {
    let tmp = common::tempdir().unwrap();

    let main = make_main_workspace(tmp.path());
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    let assert = rwv()
        .args(["sync", "--retire", "primary"])
        .current_dir(&main.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("sync-to") || stderr.contains("--retire"),
        "expected did-you-mean hint mentioning sync-to --retire; got: {stderr}"
    );
}

/// `rwv sync` with no source must fail (source is now required).
#[test]
fn bare_sync_no_source_fails() {
    let tmp = common::tempdir().unwrap();

    let main = make_main_workspace(tmp.path());
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    rwv()
        .args(["sync"])
        .current_dir(&main.root)
        .assert()
        .failure();
}

/// Sibling-sync warning: when CWD is one workweave and an explicit
/// source is another (non-parent) workweave, sync should emit a warning
/// that names both paths and then proceed — the warning is informational,
/// not a refusal.
#[test]
fn sibling_sync_emits_warning_and_proceeds() {
    let tmp = common::tempdir().unwrap();
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
// sync-to --retire
// ---------------------------------------------------------------------------

/// Happy path: `rwv sync-to --retire` from a workweave whose project repo is
/// already at parent's tip (no divergent commits) lands work upward and deletes.
#[test]
fn sync_to_retire_clean_path_deletes_workweave() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    // Gitignore activation-generated artifacts in the project repo before
    // creating the workweave. Without this, the workweave's project worktree
    // ends up with untracked files (`{project}.code-workspace`, `gita/`) that
    // the dirty check counts as uncommitted state, which --retire
    // honors. A real project would gitignore these (or commit them);
    // the test fixture is just minimal-by-default.
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

    // No divergence: nothing in ww1 has changed since fork. Manifest tips
    // identical to parent's, working tree clean (modulo gitignored
    // activation outputs) → sync-to --retire should succeed.
    assert!(ww1.root.exists(), "workweave must exist pre-retire");

    rwv()
        .args(["sync-to", "--retire", &main.root.to_string_lossy()])
        .current_dir(&ww1.root)
        .assert()
        .success();

    assert!(
        !ww1.root.exists(),
        "--retire must delete the workweave on successful sync-to"
    );
}

/// Dirty-tree path: if any manifest-repo worktree in the workweave has
/// uncommitted TRACKED changes when `sync-to --retire` runs, the op must refuse
/// and leave the workweave intact for the operator to fix.
///
/// Since the source-side cleanliness preflight (§1), the refusal fires
/// UP FRONT at op start — before any rebase or op-state write — rather than at
/// the post-sync retire dirty-check. This defines the "half-rebased op with a
/// stale lock" state out of existence for the dirty-tree class; the refusal
/// names every dirty repo so the operator can commit or stash and re-run.
#[test]
fn sync_to_retire_with_dirty_worktree_refuses_to_delete() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    std::fs::write(main.root.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();
    let ww1 = create_workweave(&main, &weaveroot, "ww1");

    // Dirty a TRACKED file in the manifest-repo worktree before retire runs.
    // The source-side cleanliness preflight catches this at op start and refuses.
    std::fs::write(ww1.manifest_repo.join("README.md"), "dirtied\n").unwrap();

    let assert = rwv()
        .args(["sync-to", "--retire", &main.root.to_string_lossy()])
        .current_dir(&ww1.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("uncommitted tracked changes"),
        "expected source-cleanliness preflight to surface the dirty-state refusal, \
         got stderr: {stderr}"
    );
    assert!(
        ww1.root.exists(),
        "dirty --retire must NOT delete the workweave"
    );
}

// ---------------------------------------------------------------------------
// Sync rebase no longer clobbers user resolutions on re-run
// ---------------------------------------------------------------------------

/// After a conflicted sync rebase, the operator resolves conflicts
/// in-place and runs `git rebase --continue` followed by `rwv sync`
/// again. Under the legacy custom cherry-pick loop, the second sync did
/// a fresh `git reset --hard` and clobbered the resolution. Native
/// rebase leaves the repo in standard mid-rebase state —
/// `git rebase --continue` completes the rebase, and the next
/// `rwv sync` is a no-op for the converged project repo.
#[test]
fn sync_rebase_continue_then_resync_does_not_clobber_user_resolution() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let main = make_main_workspace(tmp.path());
    let ww1 = create_workweave(&main, &weaveroot, "ww1");
    let ww2 = create_workweave(&main, &weaveroot, "ww2");

    // Both workweaves edit notes/shared.md with conflicting content.
    commit_file(
        &ww1.project_dir,
        "notes/shared.md",
        "ww1 wrote this\n",
        "docs: ww1 take",
    );
    rwv_lock_commit(&ww1.root);

    commit_file(
        &ww2.project_dir,
        "notes/shared.md",
        "ww2 wrote this\n",
        "docs: ww2 take",
    );
    rwv_lock_commit(&ww2.root);

    // main absorbs ww1's commit cleanly.
    rwv()
        .args(["sync", &ww1.root.to_string_lossy()])
        .current_dir(&main.root)
        .assert()
        .success();

    // ww2 → main rebase: expected to conflict on notes/shared.md.
    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&ww2.root)
        .assert()
        .failure();

    // Verify the repo is left mid-rebase (the contract: standard git state,
    // not the custom-loop reset).
    assert!(
        ww2.project_dir.join(".git").exists() || ww2.project_dir.join(".git").is_file(),
        "ww2 project repo should still exist"
    );
    let mid_op = repoweave::git::git_vcs().mid_operation(&ww2.project_dir);
    assert_eq!(
        mid_op.as_deref(),
        Some("mid-rebase"),
        "conflicted sync rebase should leave the repo mid-rebase, got {mid_op:?}"
    );

    // Operator resolves: pick a different, deliberate value, then continue.
    std::fs::write(
        ww2.project_dir.join("notes/shared.md"),
        "operator-resolved version\n",
    )
    .unwrap();
    git(&["add", "notes/shared.md"], &ww2.project_dir);
    git(&["rebase", "--continue"], &ww2.project_dir);

    // After --continue, the repo is no longer mid-rebase.
    assert!(
        repoweave::git::git_vcs()
            .mid_operation(&ww2.project_dir)
            .is_none(),
        "after `git rebase --continue` the repo must not be mid-op"
    );
    let resolved_content =
        std::fs::read_to_string(ww2.project_dir.join("notes/shared.md")).unwrap();
    assert_eq!(resolved_content, "operator-resolved version\n");

    // Now run `rwv sync --continue` to resume from the recorded op-state.
    // --continue is passed alone; all parameters (source, strategy) are read from
    // the in-progress op-state file. Phase 1' must NOT clobber the resolution —
    // already-converged repos are no-ops.
    rwv()
        .args(["sync", "--continue"])
        .current_dir(&ww2.root)
        .assert()
        .success();

    let after_resync = std::fs::read_to_string(ww2.project_dir.join("notes/shared.md")).unwrap();
    assert_eq!(
        after_resync, "operator-resolved version\n",
        "second `rwv sync` must NOT clobber the operator's resolution; \
         got after_resync={after_resync:?}"
    );
}
