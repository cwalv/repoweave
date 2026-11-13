//! Adversarial coverage for Design-B Corrections 2 & 3:
//! source-side cleanliness preflight, benign-staleness classification (reusing
//! `status::LockRelation`), op-start auto-relock with commit-count output, and
//! the lock-freshness gates of landings.
//!
//! TERMINOLOGY (load-bearing): `LockRelation` names the relation from the TIP's
//! vantage point, which is inverted from the spec's prose.
//!
//! - spec term "lock behind HEAD" (new commits since the last lock, the benign
//!   in-progress shape) == `LockRelation::Ahead` (tip ahead of lock).
//! - spec term "ahead" (HEAD is ancestor of lock — a reset) == `LockRelation::Behind`.
//!
//! These tests exercise BOTH the `LockRelation::Ahead` (benign) branch and the
//! `LockRelation::Behind` / `Diverged` / `NoLock` / `Unknown` (refuse) branches
//! on both `sync` and `land`.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

use crate::common;

use repoweave::diagnosis::State;

// ---------------------------------------------------------------------------
// Git + rwv helpers
// ---------------------------------------------------------------------------

fn rwv() -> AssertCommand {
    common::rwv()
}

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
// Fixture: a primary workspace + a real (marker-carrying) workweave.
// ---------------------------------------------------------------------------

const MANIFEST_REPO_PATH: &str = "github/org/lib";
const PROJECT: &str = "app";

struct MainWorkspace {
    root: PathBuf,
    project_dir: PathBuf,
    manifest_repo: PathBuf,
}

struct Workweave {
    root: PathBuf,
    project_dir: PathBuf,
    manifest_repo: PathBuf,
}

/// Build the primary workspace with `rwv.toml` + `rwv.lock` committed and the
/// manifest repo pinned at its initial SHA.
fn make_main_workspace(tmp: &Path) -> MainWorkspace {
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
    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    MainWorkspace {
        root: ws,
        project_dir,
        manifest_repo,
    }
}

/// Create a real workweave via `rwv workweave create` (writes the
/// `.rwv-workweave` marker, so it resolves as a workweave-typed source).
fn create_workweave(main: &MainWorkspace, weaveroot: &Path, name: &str) -> Workweave {
    rwv()
        .args(["workweave", "create", "--project", PROJECT, name])
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

struct Fixture {
    _tmp: tempfile::TempDir,
    main: MainWorkspace,
    ww: Workweave,
}

/// Primary + one workweave `ww`, both at the initial (fresh, `ok`) lock state.
fn fixture() -> Fixture {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();
    let main = make_main_workspace(tmp.path());
    let ww = create_workweave(&main, &weaveroot, "ww");
    Fixture {
        _tmp: tmp,
        main,
        ww,
    }
}

fn head(repo: &Path) -> String {
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// Savepoints under `refs/rwv/pre-op/` in `repo`. Enough to tell "a savepoint
/// exists" from "none does" without needing the op-id; a red assertion
/// prints which refs survived.
fn savepoint_refs(repo: &Path) -> Vec<String> {
    common::git_in(
        repo,
        &["for-each-ref", "--format=%(refname)", "refs/rwv/pre-op"],
    )
    .lines()
    .filter(|l| !l.is_empty())
    .map(str::to_owned)
    .collect()
}

// ===========================================================================
// LockRelation::Ok — both verbs proceed on a fresh lock
// ===========================================================================

#[test]
fn relation_ok_sync_to_succeeds() {
    let f = fixture();
    // ww makes a project-only (non-lock) commit so land has something to land
    // while the manifest repo stays at the ok lock relation.
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");
    rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    // The landing DELIVERED: main's project repo advanced to ww's tip.
    assert_eq!(
        head(&f.main.project_dir),
        head(&f.ww.project_dir),
        "land must land ww's project commit on main"
    );
}

#[test]
fn relation_ok_sync_succeeds() {
    let f = fixture();
    // main advances + relocks so a `rwv sync` from ww has content to take in,
    // with both sides at a fresh (ok) lock relation.
    commit_file(&f.main.manifest_repo, "m.txt", "m\n", "main: advance");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&f.main.root)
        .assert()
        .success();
    rwv()
        .args(["sync"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    // The pull DELIVERED: ww's manifest repo advanced to main's tip.
    assert_eq!(
        head(&f.ww.manifest_repo),
        head(&f.main.manifest_repo),
        "pull must deliver main's locked advance to ww"
    );
}

// ===========================================================================
// LockRelation::Ahead (spec term "lock behind HEAD") — benign
// ===========================================================================

/// land: a workweave whose manifest repo advanced past its committed lock
/// (relation `Ahead`) lands, printing the per-repo line with the commit count
/// and the tip the landing records; the source writes no relock commit of its
/// own, and the target's lock pins that tip.
#[test]
fn relation_ahead_land_records_the_tips_without_a_relock_commit_of_its_own() {
    let f = fixture();

    commit_file(&f.ww.manifest_repo, "a.txt", "a\n", "ww: a");
    commit_file(&f.ww.manifest_repo, "b.txt", "b\n", "ww: b");
    let ww_project_before = head(&f.ww.project_dir);

    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    let main_lib = common::git_in(&f.main.manifest_repo, &["rev-parse", "main"]);
    let ww_lib = head(&f.ww.manifest_repo);
    assert_eq!(main_lib, ww_lib, "main's manifest tip should match ww's");
    let main_lock = std::fs::read_to_string(f.main.project_dir.join("rwv.lock")).unwrap();
    assert!(
        main_lock.contains(&ww_lib),
        "main's lock must pin the landed manifest tip; lock:\n{main_lock}"
    );
    assert!(
        stderr.contains(MANIFEST_REPO_PATH)
            && stderr.contains(&format!(
                "lock behind HEAD by 2 commits — the landing records {}",
                &ww_lib[..12]
            )),
        "expected the lock-behind line naming the tip the landing records; got \
         stderr:\n{stderr}"
    );
    let main_project = common::git_in(&f.main.project_dir, &["rev-parse", "main"]);
    assert_eq!(
        common::git_in(&f.main.project_dir, &["rev-parse", "main^"]),
        ww_project_before,
        "the landing's one project commit sits directly on the source's own tip"
    );
    assert_eq!(head(&f.ww.project_dir), main_project);
}

/// land: the TARGET's manifest repo advanced past the target's committed
/// lock (relation `Ahead` on the target side). Replay takes its targets from the
/// target's lock, so the target's unlocked commits would be missing from the tip
/// CWD replays onto and step 3's fast-forward could not proceed. Refuse at op
/// start — parity with the CWD side's auto-relock — naming the target repo, the
/// commit count, and the `rwv lock --commit` that fixes it.
///
/// The outcome is what this pins: an op-start refusal leaves BOTH workspaces
/// exactly as they were. Before this refusal existed the same state ran to step
/// 3, failed the fast-forward there, and left the target's project repo already
/// advanced onto a lock behind its own manifest tip.
#[test]
fn relation_ahead_sync_to_target_refuses_at_op_start_leaving_both_untouched() {
    let f = fixture();

    // The TARGET (main) advances its manifest repo TWICE without relocking.
    commit_file(&f.main.manifest_repo, "d1.txt", "d1\n", "main: docs 1");
    commit_file(&f.main.manifest_repo, "d2.txt", "d2\n", "main: docs 2");
    // CWD (ww) has a project commit to land.
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");

    let target_lib_before = head(&f.main.manifest_repo);
    let target_project_before = head(&f.main.project_dir);
    let target_lock_before = std::fs::read_to_string(f.main.project_dir.join("rwv.lock")).unwrap();
    let cwd_project_before = head(&f.ww.project_dir);

    let out = common::cli::run(&["land"], &f.ww.root);
    let stderr = common::cli::refused(&out, State::PinUnpinned);
    assert!(
        stderr.contains(MANIFEST_REPO_PATH)
            && stderr.contains("2 commits beyond the pin")
            && stderr.contains("rwv lock --commit"),
        "commits on the target's receiving branch nothing records must refuse up front, \
         naming the repo, the commits and the remedy; got stderr:\n{stderr}"
    );

    // NOTHING moved on either side — this is the whole point of refusing at op
    // start rather than at step 3.
    assert_eq!(
        head(&f.main.manifest_repo),
        target_lib_before,
        "a refused land must leave the target's manifest repo untouched"
    );
    assert_eq!(
        head(&f.main.project_dir),
        target_project_before,
        "a refused land must NOT advance the target's project repo"
    );
    assert_eq!(
        std::fs::read_to_string(f.main.project_dir.join("rwv.lock")).unwrap(),
        target_lock_before,
        "a refused land must leave the target's lock file untouched"
    );
    assert_eq!(
        head(&f.ww.project_dir),
        cwd_project_before,
        "a refused land must leave CWD's project repo untouched"
    );

    // No op-state leaked: a re-run repeats the SAME refusal rather than
    // reporting an in-flight op.
    let again = common::cli::run(&["land"], &f.ww.root);
    let stderr_again = common::cli::refused(&again, State::PinUnpinned);
    assert!(
        !stderr_again.contains("--continue"),
        "the op-start refusal must leave no op-state; got stderr:\n{stderr_again}"
    );
}

/// The same refusal on the topology this was first hit in: a child workweave
/// landing into its parent workweave, where the parent picked up a commit
/// without relocking. The target being workweave-typed rather than the primary
/// changes nothing — the classification reads the target workspace either way —
/// and the remedy still converges and delivers.
#[test]
fn relation_ahead_sync_to_workweave_target_refuses_then_remedy_delivers() {
    let f = fixture();
    let weaveroot = f.main.root.parent().unwrap().join(".workweaves");
    rwv()
        .args(["workweave", "create", "--project", PROJECT, "child"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    let child_root = weaveroot.join(format!("{PROJECT}--child"));
    let child = Workweave {
        project_dir: child_root.join("projects").join(PROJECT),
        manifest_repo: child_root.join(MANIFEST_REPO_PATH),
        root: child_root,
    };

    // The parent workweave (ww) picks up a commit without relocking.
    commit_file(&f.ww.manifest_repo, "p1.txt", "p1\n", "ww: parent commit");
    commit_file(&child.project_dir, "note.txt", "n\n", "child: note");

    let ww_project_before = head(&f.ww.project_dir);
    let out = common::cli::run(&["land"], &child.root);
    let stderr = common::cli::refused(&out, State::PinUnpinned);
    assert!(
        stderr.contains("1 commit beyond the pin") && stderr.contains("rwv lock --commit"),
        "a workweave target's unrecorded commits must refuse the same way; got:\n{stderr}"
    );
    assert_eq!(
        head(&f.ww.project_dir),
        ww_project_before,
        "a refused land must leave the workweave target's project repo untouched"
    );

    rwv()
        .args(["lock", "--commit", "--project", PROJECT])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    rwv()
        .args(["land", "--history", "linear"])
        .current_dir(&child.root)
        .assert()
        .success();

    assert_eq!(
        head(&f.ww.project_dir),
        head(&child.project_dir),
        "after the remedy the child's project commit must land in the parent workweave"
    );
    assert_eq!(
        common::git_in(&f.ww.manifest_repo, &["rev-parse", "HEAD"]),
        head(&child.manifest_repo),
        "after the remedy both workweaves must agree on the manifest tip"
    );
}

/// The remedy the refusal names has to actually work. Run exactly what the
/// message says — `rwv lock --commit --project <p>` in the TARGET workspace —
/// and the landing converges and DELIVERS: the target's manifest tip and
/// project tip both reach CWD's, and the target's lock pins the manifest tip it
/// just landed rather than the revision it was stuck on.
#[test]
fn relation_ahead_sync_to_target_named_remedy_converges_and_delivers() {
    let f = fixture();

    let target_docs = commit_file(&f.main.manifest_repo, "d1.txt", "d1\n", "main: docs 1");
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");

    rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();

    // The remedy, verbatim from the refusal, run in the target workspace.
    rwv()
        .args(["lock", "--commit", "--project", PROJECT])
        .current_dir(&f.main.root)
        .assert()
        .success();

    rwv()
        .args(["land", "--history", "linear"])
        .current_dir(&f.ww.root)
        .assert()
        .success();

    // DELIVERED: the target's manifest repo still holds the commit that was
    // stranding the op, CWD's replay picked it up, and both sides converged.
    let target_lib = common::git_in(&f.main.manifest_repo, &["rev-parse", "main"]);
    assert_eq!(
        target_lib,
        head(&f.ww.manifest_repo),
        "after the remedy, CWD and the target must agree on the manifest tip"
    );
    // The target's own unlocked commit survived the landing (git exits non-zero
    // when it is not an ancestor, which `git` turns into a failure here).
    common::git_in(
        &f.main.manifest_repo,
        &["merge-base", "--is-ancestor", &target_docs, &target_lib],
    );
    assert_eq!(
        head(&f.main.project_dir),
        head(&f.ww.project_dir),
        "after the remedy, land must land CWD's project commit on the target"
    );
    // The lock the target ends up with pins the tip it holds — not the stale
    // revision the refusal was about.
    let target_lock = std::fs::read_to_string(f.main.project_dir.join("rwv.lock")).unwrap();
    assert!(
        target_lock.contains(&target_lib),
        "the target's lock must pin the manifest tip it now holds; lock:\n{target_lock}"
    );
}

// ===========================================================================
// LockRelation::Ahead on the pull DESTINATION — benign
// ===========================================================================

/// A workweave commits member work, so its own lock is behind its HEAD. A sync
/// that takes in new project work proceeds anyway: the work survives, and the
/// lock the sync leaves is the three-way merge of the pins, which neither side
/// moved.
#[test]
fn sync_with_lock_behind_head_proceeds_and_keeps_member_work() {
    let f = fixture();
    let base_pin = head(&f.ww.manifest_repo);
    let ww_tip = commit_file(&f.ww.manifest_repo, "f.txt", "f\n", "ww: feature");
    commit_file(
        &f.main.project_dir,
        "notes.txt",
        "n\n",
        "main: project note",
    );

    rwv()
        .args(["sync"])
        .current_dir(&f.ww.root)
        .assert()
        .success();

    let lock = std::fs::read_to_string(f.ww.project_dir.join("rwv.lock")).unwrap();
    assert!(
        lock.contains(&base_pin),
        "neither side moved the pin, so the merged lock keeps it; lock:\n{lock}"
    );
    assert_eq!(
        head(&f.ww.manifest_repo),
        ww_tip,
        "a sync must not rewind the workweave's own committed member work"
    );
    assert!(
        f.ww.project_dir.join("notes.txt").exists(),
        "the sync must deliver the target's new project work"
    );
}

// ===========================================================================
// LockRelation::Behind (spec term "ahead" — HEAD reset below lock) — refuse
// ===========================================================================

/// land: CWD's committed lock records a manifest SHA that HEAD does not have
/// (HEAD is a strict ancestor of the lock — relation `Behind`). This is the
/// reset/`update`-without-FF case; its relock would move the pin backward, so
/// it refuses, naming the relation.
#[test]
fn relation_behind_sync_to_refuses_and_names_relation() {
    let f = fixture();
    // Advance ww's manifest repo to C2 and relock (lock now pins C2), then reset
    // the manifest worktree back to C1. lock=C2, HEAD=C1 → Behind.
    let c1 = head(&f.ww.manifest_repo);
    commit_file(&f.ww.manifest_repo, "c2.txt", "c2\n", "ww: c2");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    common::git_in(&f.ww.manifest_repo, &["reset", "--hard", &c1]);
    let target_project_before = head(&f.main.project_dir);

    let out = common::cli::run(&["land"], &f.ww.root);
    let stderr = common::cli::refused(&out, State::PinBackwards);
    assert!(
        stderr.contains("behind"),
        "a checkout behind its pin must refuse naming the relation; got:\n{stderr}"
    );
    assert_eq!(
        head(&f.main.project_dir),
        target_project_before,
        "a refused land must leave the target's project repo untouched"
    );
}

// ===========================================================================
// LockRelation::Diverged — refuse, additionally hint `rwv lock --commit`
// ===========================================================================

/// land: CWD's lock and HEAD have diverged past a shared base (out-of-band
/// rewrite). Its relock would move the pin backward, so it refuses.
#[test]
fn relation_diverged_sync_to_refuses_and_hints_lock_commit() {
    let f = fixture();
    // Build divergence: lock pins a commit on one branch; HEAD is a DIFFERENT
    // commit on a sibling branch (neither an ancestor of the other).
    let c1 = head(&f.ww.manifest_repo);
    // Branch A: commit c2a, relock so the lock pins c2a.
    let c2a = commit_file(&f.ww.manifest_repo, "a.txt", "a\n", "ww: c2a");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    // Reset back to c1 and make a sibling commit c2b so HEAD (c2b) and lock (c2a)
    // diverge.
    common::git_in(&f.ww.manifest_repo, &["reset", "--hard", &c1]);
    commit_file(&f.ww.manifest_repo, "b.txt", "b\n", "ww: c2b");
    assert_ne!(head(&f.ww.manifest_repo), c2a);
    let target_project_before = head(&f.main.project_dir);
    let target_lib_before = head(&f.main.manifest_repo);

    let out = common::cli::run(&["land"], &f.ww.root);
    let stderr = common::cli::refused(&out, State::PinBackwards);
    assert!(
        stderr.contains("diverged") && stderr.contains("rwv member pin"),
        "a checkout diverged from its pin must refuse naming the relation and the \
         by-name pin; got:\n{stderr}"
    );
    assert_eq!(
        head(&f.main.project_dir),
        target_project_before,
        "a refused land must leave the target's project repo untouched"
    );
    assert_eq!(
        head(&f.main.manifest_repo),
        target_lib_before,
        "a refused land must leave the target's manifest repo untouched"
    );
}

// ===========================================================================
// LockRelation::NoLock / Unknown — refuse
// ===========================================================================

/// land: a target lock that pins a tag/branch that does not resolve on
/// disk is a corrupt-lock error — refuse, naming the unknown revision.
#[test]
fn unresolvable_target_lock_refuses_naming_unknown_revision() {
    let f = fixture();
    // Rewrite main's committed lock to pin a nonexistent tag.
    let manifest_repo = f.main.manifest_repo.clone();
    let repo_url = common::file_url(&manifest_repo);
    common::fixture_lock(
        &f.main.project_dir,
        &[(MANIFEST_REPO_PATH, &repo_url, "v9.9.9-nope")],
    );
    common::git_in(&f.main.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &f.main.project_dir,
        &["commit", "-m", "lock: nonexistent tag"],
    );

    let dest_lib_before = head(&f.ww.manifest_repo);
    let out = common::cli::run(&["land"], &f.ww.root);
    let stderr = common::cli::refused(&out, State::PinUnreachable);
    assert!(
        stderr.contains("v9.9.9-nope"),
        "unresolvable target lock must name the unknown revision; got:\n{stderr}"
    );
    assert_eq!(
        head(&f.ww.manifest_repo),
        dest_lib_before,
        "a refused pull must leave the destination untouched"
    );
}

/// land: a CWD project that carries NO committed lock at all classifies as
/// `no-lock` and refuses (define behavior explicitly — not a silent proceed).
#[test]
fn no_lock_sync_to_refuses() {
    let f = fixture();
    // Remove the committed rwv.lock from ww's project repo entirely.
    common::git_in(&f.ww.project_dir, &["rm", "rwv.lock"]);
    common::git_in(&f.ww.project_dir, &["commit", "-m", "drop lock"]);
    let target_project_before = head(&f.main.project_dir);

    let out = common::cli::run(&["land"], &f.ww.root);
    common::cli::refused(&out, State::PinNone);
    assert_eq!(
        head(&f.main.project_dir),
        target_project_before,
        "a refused land must leave the target's project repo untouched"
    );
}

// ===========================================================================
// Source-side cleanliness preflight (§1)
// ===========================================================================

/// land refuses UP FRONT on a tracked-dirty manifest repo, naming the dirty
/// repo, before any op-state write or rebase.
#[test]
fn dirty_source_tracked_change_refuses_and_names_repo() {
    let f = fixture();
    // Modify a TRACKED file in ww's manifest repo (README.md exists from init).
    std::fs::write(f.ww.manifest_repo.join("README.md"), "dirtied\n").unwrap();

    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains(&format!(
            "rwv land: {MANIFEST_REPO_PATH}: edits dirty\n    README.md\n"
        )),
        "tracked dirt must refuse up front naming the repo; got:\n{stderr}"
    );
}

/// land IGNORES untracked files in the source (they survive the replay). An
/// untracked scratch file must not block a landing.
#[test]
fn dirty_source_untracked_file_is_ignored() {
    let f = fixture();
    // Give land something to land (a project-only commit), plus an UNTRACKED
    // scratch file in the manifest repo that must not refuse.
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");
    std::fs::write(f.ww.manifest_repo.join("scratch.tmp"), "scratch\n").unwrap();

    rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .success();

    // The untracked scratch file is still present (never touched).
    assert!(
        f.ww.manifest_repo.join("scratch.tmp").exists(),
        "untracked scratch file must survive the op"
    );
}

fn assert_source_refusal_changed_nothing(f: &Fixture, out: &std::process::Output, state: State) {
    common::cli::refused(out, state);
    assert!(savepoint_refs(&f.main.project_dir).is_empty());
    assert!(savepoint_refs(&f.main.manifest_repo).is_empty());
    assert!(savepoint_refs(&f.ww.project_dir).is_empty());
    assert!(savepoint_refs(&f.ww.manifest_repo).is_empty());
    assert!(!f.ww.root.join(".rwv-op").exists());
    assert!(!f.main.root.join(".rwv-op").exists());
    assert!(!f.main.root.join(".rwv-op-lease").exists());
}

fn unborn(repo: &Path) {
    common::git_in(repo, &["checkout", "-q", "--orphan", "unborn-src"]);
    common::git_in(repo, &["rm", "-rfq", "."]);
}

fn land_refused(f: &Fixture) -> std::process::Output {
    let out = common::cli::run(&["land"], &f.ww.root);
    assert!(!out.status.success(), "{}", common::cli::describe(&out));
    out
}

#[test]
fn detached_source_member_refuses_before_any_side_effect() {
    let f = fixture();
    common::git_in(&f.ww.manifest_repo, &["checkout", "-q", "--detach"]);
    let tips = (
        head(&f.main.project_dir),
        head(&f.main.manifest_repo),
        head(&f.ww.manifest_repo),
    );
    let out = land_refused(&f);
    assert_source_refusal_changed_nothing(&f, &out, State::AlignmentDetachedElsewhere);
    assert_eq!(tips.0, head(&f.main.project_dir));
    assert_eq!(tips.1, head(&f.main.manifest_repo));
    assert_eq!(tips.2, head(&f.ww.manifest_repo));
}

#[test]
fn unborn_source_member_refuses_before_any_side_effect() {
    let f = fixture();
    unborn(&f.ww.manifest_repo);
    let tips = (head(&f.main.project_dir), head(&f.main.manifest_repo));
    let out = land_refused(&f);
    assert_source_refusal_changed_nothing(&f, &out, State::AlignmentOnBranch);
    assert_eq!(tips.0, head(&f.main.project_dir));
    assert_eq!(tips.1, head(&f.main.manifest_repo));
}

#[test]
fn detached_source_project_repo_refuses_before_any_side_effect() {
    let f = fixture();
    common::git_in(&f.ww.project_dir, &["checkout", "-q", "--detach"]);
    let tips = (
        head(&f.main.project_dir),
        head(&f.main.manifest_repo),
        head(&f.ww.project_dir),
    );
    let out = land_refused(&f);
    assert_source_refusal_changed_nothing(&f, &out, State::AlignmentProjectDetached);
    assert_eq!(tips.0, head(&f.main.project_dir));
    assert_eq!(tips.1, head(&f.main.manifest_repo));
    assert_eq!(tips.2, head(&f.ww.project_dir));
}

fn lands(f: &Fixture, commits: &[&str]) {
    rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    for sha in commits {
        let contained = std::process::Command::new("git")
            .args(["merge-base", "--is-ancestor", sha, "HEAD"])
            .current_dir(&f.main.manifest_repo)
            .status()
            .unwrap();
        assert!(
            contained.success(),
            "commit {sha} did not land in the target's manifest repo"
        );
    }
}

fn ephemeral_branch(f: &Fixture) -> String {
    common::git_in(&f.ww.manifest_repo, &["branch", "--show-current"])
}

/// A member detached with nothing of its own is aligned by the printed
/// choice, `rwv update`, and then lands.
#[test]
fn detached_source_equal_to_its_branch_lands_after_the_printed_update() {
    let f = fixture();
    let tip = commit_file(&f.ww.manifest_repo, "a.txt", "a\n", "ww: a");
    common::git_in(&f.ww.manifest_repo, &["checkout", "-q", "--detach"]);
    let out = land_refused(&f);
    let stderr = common::cli::refused(&out, State::AlignmentDetachedElsewhere);
    assert!(stderr.contains("rwv update"), "{stderr}");
    common::cli::succeed(&["update"], &f.ww.root);
    lands(&f, &[&tip]);
}

#[test]
fn detached_source_behind_its_branch_lands_after_the_printed_update() {
    let f = fixture();
    commit_file(&f.ww.manifest_repo, "a.txt", "a\n", "ww: a");
    let tip = commit_file(&f.ww.manifest_repo, "b.txt", "b\n", "ww: b");
    common::git_in(
        &f.ww.manifest_repo,
        &["checkout", "-q", "--detach", "HEAD~1"],
    );
    let out = land_refused(&f);
    common::cli::refused(&out, State::AlignmentDetachedElsewhere);
    common::cli::succeed(&["update"], &f.ww.root);
    lands(&f, &[&tip]);
}

/// Detached commits of its own are never moved off by a bare switch: the
/// refusal keeps them first (C14) or copies them onto the branch (C15).
#[test]
fn detached_source_ahead_of_its_branch_lands_after_copying_onto_its_branch() {
    let f = fixture();
    let branch = ephemeral_branch(&f);
    common::git_in(&f.ww.manifest_repo, &["checkout", "-q", "--detach"]);
    let work = commit_file(&f.ww.manifest_repo, "d.txt", "d\n", "ww: detached work");
    let out = land_refused(&f);
    let stderr = common::cli::refused(&out, State::AlignmentDetachedUnpushed);
    assert!(stderr.contains(&work[..12]), "{stderr}");
    assert!(!stderr.contains("rwv switch"), "{stderr}");
    common::git_in(&f.ww.manifest_repo, &["switch", "-q", &branch]);
    common::git_in(&f.ww.manifest_repo, &["merge", "-q", "--ff-only", &work]);
    lands(&f, &[&work]);
    assert_eq!(ephemeral_branch(&f), branch);
}

#[test]
fn detached_source_whose_branch_is_gone_lands_after_keeping_it_on_its_branch() {
    let f = fixture();
    let branch = ephemeral_branch(&f);
    common::git_in(&f.ww.manifest_repo, &["checkout", "-q", "--detach"]);
    let work = commit_file(&f.ww.manifest_repo, "d.txt", "d\n", "ww: detached work");
    common::git_in(&f.ww.manifest_repo, &["branch", "-D", &branch]);
    let out = land_refused(&f);
    common::cli::refused(&out, State::AlignmentDetachedUnpushed);
    common::git_in(&f.ww.manifest_repo, &["switch", "-q", "-c", &branch]);
    lands(&f, &[&work]);
}

/// Carve-out: a project repo dirty ONLY in `rwv.lock` is NOT dirt (it is the
/// auto-relock's own input). land proceeds and commits the regenerated lock.
#[test]
fn dirty_source_rwv_lock_only_is_carved_out() {
    let f = fixture();
    // Advance ww's manifest repo (so there is a real landing) and leave the lock
    // file MODIFIED-BUT-UNCOMMITTED in the project repo — the only tracked dirt
    // in the project repo is rwv.lock, which the carve-out permits.
    commit_file(&f.ww.manifest_repo, "adv.txt", "adv\n", "ww: adv");
    // raw lock bytes: an append onto the committed lock, so it reads as a
    // tracked modification. The shared builder writes a whole file for given
    // content and cannot make an existing one dirty. Trailing whitespace is
    // the only append JSON tolerates without becoming unparseable — a comment
    // line (the YAML-era trick) is trailing *content* and fails to parse.
    let lock_path = f.ww.project_dir.join("rwv.lock");
    let mut contents = std::fs::read_to_string(&lock_path).unwrap();
    contents.push('\n');
    std::fs::write(&lock_path, contents).unwrap();
    // Confirm the carve-out target really is dirty-tracked before we assert.
    let porcelain = common::git_in(&f.ww.project_dir, &["status", "--porcelain"]);
    assert!(
        porcelain.contains("rwv.lock"),
        "test precondition: rwv.lock must be tracked-dirty; got porcelain:\n{porcelain}"
    );

    rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
}

/// A NON-lock tracked change in the project repo still refuses (the carve-out is
/// narrow: only `rwv.lock` alone is exempt), and names the offending files.
#[test]
fn dirty_source_non_lock_project_change_refuses() {
    let f = fixture();
    // Modify a tracked NON-lock file in the project repo (rwv.toml exists).
    let yaml_path = f.ww.project_dir.join("rwv.toml");
    let mut y = std::fs::read_to_string(&yaml_path).unwrap();
    y.push_str("# scratch\n");
    std::fs::write(&yaml_path, y).unwrap();

    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("rwv land: projects/app: edits dirty\n    rwv.toml\n"),
        "a tracked non-lock project change must refuse and name the file; got:\n{stderr}"
    );
}

// ===========================================================================
// The already-ahead line names BOTH relations against the baseline
// ===========================================================================

/// The `already-ahead` outcome line for `repo`, whole, from `stdout`.
fn already_ahead_line(stdout: &str) -> String {
    stdout
        .lines()
        .find(|l| l.contains("already-ahead"))
        .unwrap_or_else(|| panic!("no already-ahead line in stdout:\n{stdout}"))
        .trim()
        .to_owned()
}

/// The source clause is a measurement, not a fixed sentence: a source sitting
/// AT its lock entry is reported as equal to it.
///
#[test]
fn the_already_ahead_line_reports_a_source_that_sits_at_its_lock_entry() {
    let f = fixture();
    // The source's lock is fresh; only CWD has moved.
    commit_file(&f.ww.manifest_repo, "c1.txt", "c1\n", "ww: c1");
    commit_file(&f.ww.manifest_repo, "c2.txt", "c2\n", "ww: c2");

    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    assert_eq!(
        already_ahead_line(&stdout),
        format!(
            "{MANIFEST_REPO_PATH}: already-ahead — HEAD is strictly ahead of the source lock \
             entry by 2 commits (contains it); the source's own HEAD is equal to the source lock \
             entry; nothing to replay"
        ),
    );
}

/// The landing refusals that once named a stale lock, an unresolvable lock
/// entry and a target lock behind its checkouts are each judged as the `pin`
/// state that replaced them: the source behind its pin as `pin` backwards, a
/// target lock pin no clone holds as `pin` unreachable, and target commits no
/// lock records as `pin` unpinned. No retired token is printed, has a page, or
/// is explained.
#[test]
fn the_landing_refusals_that_named_a_stale_lock_are_judged_as_pin_states() {
    let refusals = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/refusals.md"),
    )
    .unwrap();
    let retired = |token: &str, out: &std::process::Output| {
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains(token)
                && !refusals.contains(&format!("### `{token}`")),
            "`{token}` is neither printed nor listed"
        );
        let explained = common::cli::run(&["explain", token], Path::new("."));
        assert!(!explained.status.success(), "`{token}` names no condition");
    };

    let f = fixture();
    let c1 = head(&f.ww.manifest_repo);
    commit_file(&f.ww.manifest_repo, "c2.txt", "c2\n", "ww: c2");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    common::git_in(&f.ww.manifest_repo, &["reset", "--hard", &c1]);
    let out = common::cli::run(&["land"], &f.ww.root);
    common::cli::refused(&out, State::PinBackwards);
    retired("stale-lock", &out);

    let f = fixture();
    common::fixture_lock(
        &f.main.project_dir,
        &[(
            MANIFEST_REPO_PATH,
            &common::file_url(&f.main.manifest_repo),
            "v9.9.9-nope",
        )],
    );
    common::git_in(&f.main.project_dir, &["add", "rwv.lock"]);
    common::git_in(&f.main.project_dir, &["commit", "-m", "lock: no such tag"]);
    let out = common::cli::run(&["land"], &f.ww.root);
    common::cli::refused(&out, State::PinUnreachable);
    retired("unresolvable-lock-entry", &out);

    let f = fixture();
    commit_file(&f.main.manifest_repo, "d1.txt", "d1\n", "main: d1");
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");
    let out = common::cli::run(&["land"], &f.ww.root);
    common::cli::refused(&out, State::PinUnpinned);
    retired("target-lock-behind", &out);
}

/// The lines of the printed choice whose phrase starts with `phrase`, and
/// where it says they run.
fn printed_choice(stderr: &str, phrase: &str) -> (Option<PathBuf>, Vec<String>) {
    let mut lines = stderr
        .lines()
        .skip_while(|l| !l.starts_with(&format!("  {phrase}")));
    let head = lines
        .next()
        .unwrap_or_else(|| panic!("no choice `{phrase}` in:\n{stderr}"));
    let at = head.split_once(", in ").map(|(_, at)| PathBuf::from(at));
    let run = lines
        .take_while(|l| l.starts_with("    "))
        .map(|l| l.trim().to_owned())
        .collect();
    (at, run)
}

/// Runs one printed line as the operator would, in `dir`.
fn run_printed(line: &str, dir: &Path) {
    let words: Vec<&str> = line.split_whitespace().collect();
    let out = match words[0] {
        "rwv" => common::cli::run(&words[1..], dir),
        program => std::process::Command::new(program)
            .args(&words[1..])
            .current_dir(dir)
            .output()
            .unwrap(),
    };
    assert!(
        out.status.success(),
        "{line}: {}",
        common::cli::describe(&out)
    );
}

/// Commits on another branch that no other branch holds, with the expected
/// branch checked out nowhere: no copy is printed, since it would run only
/// after switching there; keeping the work on a branch, as printed, and
/// aligning lands the expected branch, the work kept on both branches.
#[test]
fn stray_commits_with_the_expected_branch_checked_out_nowhere_are_kept_and_the_branch_lands() {
    let f = fixture();
    let branch = ephemeral_branch(&f);
    common::git_in(&f.ww.manifest_repo, &["switch", "-q", "-c", "wip"]);
    let work = commit_file(&f.ww.manifest_repo, "w.txt", "w\n", "ww: wip work");
    let out = land_refused(&f);
    let stderr = common::cli::refused(&out, State::AlignmentOnBranchWithStrayCommits);
    assert!(!stderr.contains("cherry-pick"), "{stderr}");
    assert!(!stderr.contains(&format!("switch {branch}")), "{stderr}");
    let (_, run) = printed_choice(&stderr, "keep this work on a branch");
    let keep = run[0].replace("<name>", "kept").replace("<sha>", &work);
    run_printed(&keep, &f.ww.root);
    run_printed(&run[1], &f.ww.root);
    lands(&f, &[]);
    assert_eq!(ephemeral_branch(&f), branch);
    for kept in ["kept", "wip"] {
        assert_eq!(
            common::git_in(&f.ww.manifest_repo, &["rev-parse", kept]),
            work
        );
    }
}

/// Where the expected branch is checked out in another checkout, the copy
/// runs there, and running it as printed puts the commits on that branch.
/// Where the branch has moved on, the copies are new commits; each records
/// the commit it copies, so the stray set empties and the landing refuses
/// the checkout on its own branch instead, naming `rwv update`.
#[test]
fn stray_commits_are_copied_where_the_expected_branch_is_checked_out() {
    let f = fixture();
    let branch = ephemeral_branch(&f);
    common::git_in(&f.ww.manifest_repo, &["switch", "-q", "-c", "wip"]);
    commit_file(&f.ww.manifest_repo, "w.txt", "w\n", "ww: wip work");
    let other = f._tmp.path().join("other");
    common::git_in(
        &f.ww.manifest_repo,
        &["worktree", "add", "-q", other.to_str().unwrap(), &branch],
    );
    commit_file(&other, "o.txt", "o\n", "other: moved on");
    let out = land_refused(&f);
    let stderr = common::cli::refused(&out, State::AlignmentOnBranchWithStrayCommits);
    let (at, run) = printed_choice(&stderr, "copy these commits onto");
    assert_eq!(at.as_deref(), Some(other.as_path()), "{stderr}");
    assert!(!stderr.contains(&format!("switch {branch}")), "{stderr}");
    run_printed(&run[0], &f.ww.root);
    assert_eq!(
        common::git_in(&other, &["log", "--format=%s", "-1"]),
        "ww: wip work"
    );
    let again = land_refused(&f);
    let stderr = common::cli::refused(&again, State::AlignmentOnBranch);
    assert!(stderr.contains("rwv update"), "{stderr}");
}

/// A source member detached at work diverged from its branch: no copy is
/// printed, since none could run where its branch is checked out without
/// leaving the work on no branch; keeping the work on a branch, as printed,
/// and aligning lands the branch and leaves the work kept, ending on the
/// branch.
#[test]
fn detached_source_diverged_from_its_branch_keeps_its_work_and_lands_its_branch() {
    let f = fixture();
    let branch = ephemeral_branch(&f);
    let on_branch = commit_file(&f.ww.manifest_repo, "c.txt", "c\n", "ww: on branch");
    common::git_in(
        &f.ww.manifest_repo,
        &["checkout", "-q", "--detach", "HEAD~1"],
    );
    let work = commit_file(&f.ww.manifest_repo, "d.txt", "d\n", "ww: detached work");
    let out = land_refused(&f);
    let stderr = common::cli::refused(&out, State::AlignmentDetachedUnpushed);
    assert!(!stderr.contains("cherry-pick"), "{stderr}");
    let (_, run) = printed_choice(&stderr, "keep this work on a branch");
    let keep = run[0].replace("<name>", "kept").replace("<sha>", &work);
    run_printed(&keep, &f.ww.root);
    run_printed(&run[1], &f.ww.root);
    lands(&f, &[&on_branch]);
    assert_eq!(ephemeral_branch(&f), branch);
    assert_eq!(
        common::git_in(&f.ww.manifest_repo, &["rev-parse", "kept"]),
        work
    );
}
