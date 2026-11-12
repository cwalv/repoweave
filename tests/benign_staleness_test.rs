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

mod common;

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
/// (relation `Ahead`) auto-relocks at op START, printing the LOUD per-repo line
/// INCLUDING the commit count, and the op succeeds.
#[test]
fn relation_ahead_sync_to_auto_relocks_at_op_start_with_commit_count() {
    let f = fixture();

    // Advance ww's manifest repo by TWO commits WITHOUT relocking. ww's committed
    // lock is now a strict ancestor of the manifest HEAD → LockRelation::Ahead.
    commit_file(&f.ww.manifest_repo, "a.txt", "a\n", "ww: a");
    commit_file(&f.ww.manifest_repo, "b.txt", "b\n", "ww: b");

    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    // The LOUD op-start auto-relock line, naming the repo, the commit count (2),
    // and the "auto-relocked" verb.
    assert!(
        stderr.contains(MANIFEST_REPO_PATH)
            && stderr.contains("lock behind HEAD by 2 commits")
            && stderr.contains("auto-relocked"),
        "expected op-start auto-relock line with commit count 2; got stderr:\n{stderr}"
    );

    // The op landed ww's manifest tip on main, and main's lock pins it.
    let main_lib = common::git_in(&f.main.manifest_repo, &["rev-parse", "main"]);
    let ww_lib = head(&f.ww.manifest_repo);
    assert_eq!(main_lib, ww_lib, "main's manifest tip should match ww's");
    let main_lock = std::fs::read_to_string(f.main.project_dir.join("rwv.lock")).unwrap();
    assert!(
        main_lock.contains(&ww_lib),
        "main's lock must pin the landed manifest tip; lock:\n{main_lock}"
    );

    // The line names the revision the relock commit pinned, not one the
    // announcement chose for itself.
    assert!(
        stderr.contains(&format!("auto-relocked to {}", &ww_lib[..12])),
        "the auto-relock line must name the tip the commit pinned ({ww_lib}); got \
         stderr:\n{stderr}"
    );
}

/// Refuse every commit in `repo` from now on, by pointing it at a hooks
/// directory whose `pre-commit` exits non-zero. Worktrees share one hooks
/// path with the clone they were added from, which is what makes this reach
/// the relock: it commits in a workweave's project repo.
fn block_commits(repo: &Path, hooks_dir: &Path) {
    std::fs::create_dir_all(hooks_dir).unwrap();
    let hook = hooks_dir.join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    common::git_in(
        repo,
        &["config", "core.hooksPath", &hooks_dir.to_string_lossy()],
    );
}

/// The op-start auto-relock announces the relock AFTER the commit returns, so a
/// relock that could not commit leaves no past-tense claim standing.
///
/// The claim and the outcome used to derive from two different things: the
/// LOUD "auto-relocked" line printed from the precondition classification
/// before the commit was attempted, and the soft warning underneath it was the
/// only correction. An operator reading the loud line has been told the lock
/// pins its tips; the lock the op then lands still pins the older ones.
#[test]
fn a_relock_that_cannot_commit_leaves_no_auto_relocked_claim() {
    let f = fixture();
    let hooks = f.main.root.parent().unwrap().join("blocked-hooks");

    commit_file(&f.ww.manifest_repo, "a.txt", "a\n", "ww: a");
    commit_file(&f.ww.manifest_repo, "b.txt", "b\n", "ww: b");
    let ww_lock_before = common::read_normalized(f.ww.project_dir.join("rwv.lock"));
    let ww_project_before = head(&f.ww.project_dir);
    block_commits(&f.ww.project_dir, &hooks);

    // The op refuses at op start. That refusal is also this fixture's proof
    // that the hook fired at all — without it, every check below holds against
    // a run where the hook never ran and the relock succeeded.
    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    // The outcome the claim would have described: no relock commit landed, so
    // the committed lock still pins the tips it pinned before the op.
    assert_eq!(
        head(&f.ww.project_dir),
        ww_project_before,
        "the blocked hook must leave the project repo without a relock commit; \
         stderr:\n{stderr}"
    );
    let ww_lock_committed = common::git_in(
        &f.ww.project_dir,
        &["show", &format!("{ww_project_before}:rwv.lock")],
    );
    assert_eq!(
        ww_lock_committed.trim(),
        ww_lock_before.trim(),
        "the committed lock must be unchanged when no relock commit landed"
    );

    assert!(
        !stderr.contains("auto-relocked"),
        "a relock that never committed must not print a past-tense relock claim; got \
         stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("could not commit, so the op did not start"),
        "the operator must be told the op-start relock failed; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(MANIFEST_REPO_PATH) && stderr.contains("not relocked"),
        "the per-repo line must name the repo whose lock is still behind HEAD; got \
         stderr:\n{stderr}"
    );

    // The residue "Clear what refused the commit, then re-run." implies is
    // clear is left before that line is reached: no owner record, no lease,
    // no savepoint survives the refusal.
    assert!(
        !f.ww.root.join(".rwv-op").exists(),
        "a refused op-start relock must release the owner's op-state; \
         stderr:\n{stderr}"
    );
    assert!(
        !f.main.root.join(".rwv-op-lease").exists(),
        "a refused op-start relock must release the target's lease; \
         stderr:\n{stderr}"
    );
    assert_eq!(
        savepoint_refs(&f.ww.project_dir),
        Vec::<String>::new(),
        "a refused op-start relock must drop the project-repo savepoint it \
         created; stderr:\n{stderr}"
    );
    assert_eq!(
        savepoint_refs(&f.ww.manifest_repo),
        Vec::<String>::new(),
        "a refused op-start relock must drop the manifest-repo savepoint it \
         created; stderr:\n{stderr}"
    );

    // "Clear what refused the commit, then re-run." — follow it, and the op
    // that was blocked must now go through.
    common::git_in(&f.ww.project_dir, &["config", "--unset", "core.hooksPath"]);
    rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
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

    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("lock-freshness precondition failed")
            && stderr.contains("target workspace")
            && stderr.contains(MANIFEST_REPO_PATH)
            && stderr.contains("HEAD ahead of lock by 2 commits")
            && stderr.contains("rwv lock --commit"),
        "target lock-behind-HEAD must refuse at op start naming the repo, the commit \
         count and the remedy; got stderr:\n{stderr}"
    );
    // The refusal must not sanction the flag that skips it without converging.
    assert!(
        !stderr.contains("--allow-stale-lock"),
        "the target lock-behind refusal must not offer `--allow-stale-lock`; it skips \
         this check without making the op converge. got stderr:\n{stderr}"
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
    let again = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr_again = String::from_utf8_lossy(&again.get_output().stderr).into_owned();
    assert!(
        stderr_again.contains("lock-freshness precondition failed")
            && !stderr_again.contains("in-flight")
            && !stderr_again.contains("--continue"),
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
    let child = create_workweave(&f.main, &weaveroot, "child");

    // The parent workweave (ww) picks up a commit without relocking.
    commit_file(&f.ww.manifest_repo, "p1.txt", "p1\n", "ww: parent commit");
    commit_file(&child.project_dir, "note.txt", "n\n", "child: note");

    let ww_project_before = head(&f.ww.project_dir);
    let assert = rwv()
        .args(["land"])
        .current_dir(&child.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("lock-freshness precondition failed")
            && stderr.contains("target workspace 'ww'")
            && stderr.contains("HEAD ahead of lock by 1 commits")
            && stderr.contains("rwv lock --commit"),
        "a workweave target's lock-behind state must refuse the same way; got:\n{stderr}"
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
        .args(["land"])
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
        .args(["land"])
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

/// `--allow-stale-lock` skips the op-start refusal without making the op
/// converge, so the landing still dies at step 3's fast-forward. That is the one
/// path where an operator still reads the late failure, and it must not blame a
/// concurrent modification that did not happen: nothing touched either workspace
/// between op start and step 3 here.
#[test]
fn allow_stale_lock_target_ahead_still_fails_at_step_3_without_blaming_concurrency() {
    let f = fixture();

    commit_file(&f.main.manifest_repo, "d1.txt", "d1\n", "main: docs 1");
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");

    let assert = rwv()
        .args([
            "land",
            "--allow-stale-lock",
        ])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("cannot be fast-forwarded"),
        "the consented path must still refuse the fast-forward; got stderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("This indicates concurrent modification"),
        "step 3 must not assert a concurrent modification it cannot know happened — \
         nothing modified either workspace during this op. got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("The target holds commits CWD's tip does not") && stderr.contains("lock"),
        "step 3 must state what it observed and name the lock-behind cause; got \
         stderr:\n{stderr}"
    );
}

/// Every revision the target's lock names must be one the target actually
/// holds. A manifest repo that fails to land must therefore stop the project
/// repo — the one that carries the lock — from being advanced past it. The
/// consented path is where this is reachable, so it is where it is pinned.
#[test]
fn allow_stale_lock_step_3_failure_leaves_the_target_lock_true_about_its_members() {
    let f = fixture();

    commit_file(&f.main.manifest_repo, "d1.txt", "d1\n", "main: docs 1");
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");
    commit_file(&f.ww.manifest_repo, "feature.txt", "f\n", "ww: feature");

    let target_project_before = head(&f.main.project_dir);
    let target_lock_before = std::fs::read(f.main.project_dir.join("rwv.lock")).unwrap();

    let assert = rwv()
        .args([
            "land",
            "--allow-stale-lock",
        ])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("(project): not advanced"),
        "the skipped project advance must be named where the per-repo lines are; got \
         stderr:\n{stderr}"
    );
    assert_eq!(
        head(&f.main.project_dir),
        target_project_before,
        "a manifest repo that did not land must leave the target's project repo where it was"
    );
    let target_lock_after = std::fs::read(f.main.project_dir.join("rwv.lock")).unwrap();
    assert_eq!(
        target_lock_after, target_lock_before,
        "the target's lock must be byte-identical after a failed landing"
    );

    // The invariant itself, not a proxy for it: every revision the lock the
    // target is LEFT WITH pins is reachable from the branch its repo is on.
    // Read back from disk rather than reusing the pre-op bytes, so this holds
    // independently of the equality assertion above.
    let lock = repoweave::manifest::LockFile::from_json_str(
        &String::from_utf8(target_lock_after).unwrap(),
    )
    .unwrap();
    assert_eq!(
        lock.len(),
        1,
        "the sweep below is vacuous unless the lock has the fixture's one entry"
    );
    let target_lib_tip = common::git_in(&f.main.manifest_repo, &["rev-parse", "main"]);
    for (_repo_path, entry) in lock.iter_entries() {
        common::git_in(
            &f.main.manifest_repo,
            &[
                "merge-base",
                "--is-ancestor",
                entry.version.as_str(),
                &target_lib_tip,
            ],
        );
    }
}

/// The full stranded-then-resumed shape, end to end: the consented path strands
/// the op at advance-target, the operator rebases CWD onto the target's live tip
/// by hand, and resumes. The resume reports success — so what it delivered has to
/// be right. The lock the target ends up with must pin the manifest tip the same
/// resume just gave it, not the revision CWD held before the operator's fix.
#[test]
fn stranded_advance_target_resumed_after_a_manual_fix_lands_a_lock_that_pins_the_tip() {
    let f = fixture();

    commit_file(&f.main.manifest_repo, "d1.txt", "d1\n", "main: docs 1");
    let target_live = common::git_in(&f.main.manifest_repo, &["rev-parse", "main"]);
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");
    commit_file(&f.ww.manifest_repo, "feature.txt", "f\n", "ww: feature");

    rwv()
        .args([
            "land",
            "--allow-stale-lock",
        ])
        .current_dir(&f.ww.root)
        .assert()
        .failure();

    // The operator's fix: put CWD's manifest repo on top of the target's live
    // tip so step 3's fast-forward can proceed. Nothing relocks this.
    common::git_in(&f.ww.manifest_repo, &["rebase", &target_live]);
    let cwd_lib_after_fix = head(&f.ww.manifest_repo);

    rwv()
        .args(["land", "--continue"])
        .current_dir(&f.ww.root)
        .assert()
        .success();

    let target_lib = common::git_in(&f.main.manifest_repo, &["rev-parse", "main"]);
    assert_eq!(
        target_lib, cwd_lib_after_fix,
        "the resume must land the manifest tip CWD held after the operator's fix"
    );
    let target_lock = std::fs::read_to_string(f.main.project_dir.join("rwv.lock")).unwrap();
    assert!(
        target_lock.contains(&target_lib),
        "the lock the resume published must pin the tip the same resume delivered \
         ({target_lib}); lock:\n{target_lock}"
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
/// reset/`update`-without-FF case; it must REFUSE, naming the relation, and
/// retain `--allow-stale-lock`.
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

    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("lock-freshness precondition failed")
            && stderr.contains("behind")
            && stderr.contains("--allow-stale-lock"),
        "Behind CWD must refuse naming the relation + flag; got:\n{stderr}"
    );
    assert_eq!(
        head(&f.main.project_dir),
        target_project_before,
        "a refused land must leave the target's project repo untouched"
    );

    // Escape hatch still works: --allow-stale-lock bypasses the gate. Use
    // --strategy=ff (ww is at C1, which is where main also is — an equal-tip
    // no-op is fine) to confirm the flag opens the door.
    rwv()
        .args([
            "land",
            "--allow-stale-lock",
            "--strategy=ff",
        ])
        .current_dir(&f.ww.root)
        .assert()
        .success();
}

// ===========================================================================
// LockRelation::Diverged — refuse, additionally hint `rwv lock --commit`
// ===========================================================================

/// land: CWD's lock and HEAD have diverged past a shared base (out-of-band
/// rewrite). Refuse, name `diverged`, and hint `rwv lock --commit` to bless HEAD.
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

    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("lock-freshness precondition failed")
            && stderr.contains("diverged")
            && stderr.contains("rwv lock --commit")
            && stderr.contains("--allow-stale-lock"),
        "Diverged must refuse, name the relation, and hint `rwv lock --commit`; got:\n{stderr}"
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
    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("unknown revision") && stderr.contains("v9.9.9-nope"),
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

    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("lock-freshness precondition failed") && stderr.contains("no-lock"),
        "a project with no committed lock must refuse as no-lock; got:\n{stderr}"
    );
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

fn assert_source_refusal_changed_nothing(f: &Fixture, stderr: &str, state: &str) {
    assert!(
        stderr.contains("\nrwv explain head-not-on-branch\n"),
        "expected the head-not-on-branch route token; got:\n{stderr}"
    );
    assert!(
        stderr.contains("source workweave is not on a branch in:")
            && stderr.contains(state)
            && stderr.contains(&format!(
                "Run the command under each repo in the source ({}), then re-run.",
                f.ww.root.display()
            )),
        "the refusal must name the repo, its state and the fix; got:\n{stderr}"
    );
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

fn sync_to_main_refusal(f: &Fixture) -> String {
    let assert = rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    String::from_utf8_lossy(&assert.get_output().stderr).into_owned()
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
    let stderr = sync_to_main_refusal(&f);
    assert_source_refusal_changed_nothing(
        &f,
        &stderr,
        &format!("{MANIFEST_REPO_PATH} (detached HEAD at "),
    );
    assert_eq!(tips.0, head(&f.main.project_dir));
    assert_eq!(tips.1, head(&f.main.manifest_repo));
    assert_eq!(tips.2, head(&f.ww.manifest_repo));
}

#[test]
fn unborn_source_member_refuses_before_any_side_effect() {
    let f = fixture();
    unborn(&f.ww.manifest_repo);
    let tips = (head(&f.main.project_dir), head(&f.main.manifest_repo));
    let stderr = sync_to_main_refusal(&f);
    assert_source_refusal_changed_nothing(
        &f,
        &stderr,
        &format!("{MANIFEST_REPO_PATH} (unborn branch 'unborn-src', no commits yet)"),
    );
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
    let stderr = sync_to_main_refusal(&f);
    assert_source_refusal_changed_nothing(&f, &stderr, "(detached HEAD at ");
    assert!(
        stderr.contains("  (project) (detached HEAD at "),
        "names the project repo; got:\n{stderr}"
    );
    assert_eq!(tips.0, head(&f.main.project_dir));
    assert_eq!(tips.1, head(&f.main.manifest_repo));
    assert_eq!(tips.2, head(&f.ww.project_dir));
}

/// Runs every `git -C ...` command the refusal advises, in order, with
/// `<new-branch>` spelled as `landing`.
fn follow_advised_git(stderr: &str) {
    let steps: Vec<String> = stderr
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("git -C "))
        .map(|span| span.replace("<new-branch>", "landing"))
        .collect();
    assert!(!steps.is_empty(), "no git command advised in:\n{stderr}");
    for step in steps {
        let out = common::run_advised(&step, std::env::current_dir().unwrap());
        assert!(
            out.status.success(),
            "advised `{step}` failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn lands_after_following_advice(f: &Fixture, stderr: &str, commits: &[&str]) {
    follow_advised_git(stderr);
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

#[test]
fn detached_source_equal_to_its_branch_lands_after_the_advised_switch() {
    let f = fixture();
    let branch = ephemeral_branch(&f);
    let tip = commit_file(&f.ww.manifest_repo, "a.txt", "a\n", "ww: a");
    common::git_in(&f.ww.manifest_repo, &["checkout", "-q", "--detach"]);
    let stderr = sync_to_main_refusal(&f);
    assert!(stderr.contains(&format!("switch {branch}`")), "{stderr}");
    lands_after_following_advice(&f, &stderr, &[&tip]);
}

#[test]
fn detached_source_behind_its_branch_lands_after_the_advised_switch() {
    let f = fixture();
    let branch = ephemeral_branch(&f);
    commit_file(&f.ww.manifest_repo, "a.txt", "a\n", "ww: a");
    let tip = commit_file(&f.ww.manifest_repo, "b.txt", "b\n", "ww: b");
    common::git_in(
        &f.ww.manifest_repo,
        &["checkout", "-q", "--detach", "HEAD~1"],
    );
    let stderr = sync_to_main_refusal(&f);
    assert!(
        stderr.contains(&format!("switch {branch}`")) && !stderr.contains("merge --ff-only"),
        "{stderr}"
    );
    lands_after_following_advice(&f, &stderr, &[&tip]);
}

#[test]
fn detached_source_ahead_of_its_branch_lands_after_the_advised_switch_and_merge() {
    let f = fixture();
    let branch = ephemeral_branch(&f);
    common::git_in(&f.ww.manifest_repo, &["checkout", "-q", "--detach"]);
    let work = commit_file(&f.ww.manifest_repo, "d.txt", "d\n", "ww: detached work");
    let stderr = sync_to_main_refusal(&f);
    assert!(
        stderr.contains(&format!("switch {branch}`"))
            && stderr.contains(&format!("merge --ff-only {work}`")),
        "{stderr}"
    );
    lands_after_following_advice(&f, &stderr, &[&work]);
    assert_eq!(ephemeral_branch(&f), branch);
}

#[test]
fn detached_source_diverged_from_its_branch_lands_both_sides_and_ends_on_its_branch() {
    let f = fixture();
    let branch = ephemeral_branch(&f);
    let on_branch = commit_file(&f.ww.manifest_repo, "c.txt", "c\n", "ww: on branch");
    common::git_in(
        &f.ww.manifest_repo,
        &["checkout", "-q", "--detach", "HEAD~1"],
    );
    let work = commit_file(&f.ww.manifest_repo, "d.txt", "d\n", "ww: detached work");
    let stderr = sync_to_main_refusal(&f);
    assert!(stderr.contains("switch -c <new-branch>`"), "{stderr}");
    follow_advised_git(&stderr);
    rwv()
        .args(["land"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    assert_eq!(ephemeral_branch(&f), branch);
    let landed = |name: &str| {
        std::process::Command::new("git")
            .args(["cat-file", "-e", &format!("HEAD:{name}")])
            .current_dir(&f.main.manifest_repo)
            .status()
            .unwrap()
            .success()
    };
    assert!(
        landed("c.txt"),
        "the branch's commit {on_branch} did not land"
    );
    assert!(landed("d.txt"), "the detached commit {work} did not land");
}

#[test]
fn detached_source_whose_branch_is_gone_lands_after_the_advised_switch_c() {
    let f = fixture();
    let branch = ephemeral_branch(&f);
    common::git_in(&f.ww.manifest_repo, &["checkout", "-q", "--detach"]);
    let work = commit_file(&f.ww.manifest_repo, "d.txt", "d\n", "ww: detached work");
    common::git_in(&f.ww.manifest_repo, &["branch", "-D", &branch]);
    let stderr = sync_to_main_refusal(&f);
    assert!(stderr.contains(&format!("switch -c {branch}`")), "{stderr}");
    lands_after_following_advice(&f, &stderr, &[&work]);
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

/// A CWD that contains the source lock entry is not thereby at the source's
/// tip, and the line has to say which.
///
/// Reading "strictly ahead of the source lock entry … (contains it); nothing to
/// replay" as "nothing outstanding" is the failure: the source here sits three
/// commits past that same entry, and a pull that stops at the entry delivers
/// none of them. The two counts are deliberately different (CWD 2, source 3)
/// so a line that echoes one relation into the other reads as a failure rather
/// than as agreement.
#[test]
fn the_already_ahead_line_states_how_far_the_source_moved_past_its_lock() {
    let f = fixture();
    // The source advances three commits past its own committed lock entry.
    commit_file(&f.main.manifest_repo, "s1.txt", "s1\n", "main: s1");
    commit_file(&f.main.manifest_repo, "s2.txt", "s2\n", "main: s2");
    commit_file(&f.main.manifest_repo, "s3.txt", "s3\n", "main: s3");
    // CWD carries two commits of its own on top of that same entry.
    commit_file(&f.ww.manifest_repo, "c1.txt", "c1\n", "ww: c1");
    commit_file(&f.ww.manifest_repo, "c2.txt", "c2\n", "ww: c2");

    // `--allow-stale-lock` is what reaches this shape: the freshness gate
    // otherwise answers a source whose tip has left its lock behind, either by
    // refusing or by pulling the tips, and neither leaves a lock entry the
    // source has moved past.
    let assert = rwv()
        .args([
            "land",
            "--allow-stale-lock",
        ])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    assert_eq!(
        already_ahead_line(&stdout),
        format!(
            "{MANIFEST_REPO_PATH}: already-ahead — HEAD is strictly ahead of the source lock \
             entry by 2 commits (contains it); the source's own HEAD is strictly ahead of the \
             source lock entry by 3 commits (contains it); nothing to replay"
        ),
    );
}

/// The source clause is a measurement, not a fixed sentence: a source sitting
/// AT its lock entry is reported as equal to it.
///
/// Without this the previous test passes against a line that says "the source
/// has moved past it" unconditionally — the reassuring case is the one an
/// operator acts on, so it is the one a hardcoded clause would get wrong.
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
