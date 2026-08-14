//! Adversarial coverage for Design-B Corrections 2 & 3:
//! source-side cleanliness preflight, benign-staleness classification (reusing
//! `status::LockRelation`), op-start auto-relock with commit-count output, and
//! tips-as-truth pulls from a workweave source.
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
//! on both `sync` and `sync-to`.

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
    // Round-trip through the real parser + `lock::write_lock`: a
    // hand-formatted string that differs only in whitespace from what
    // `rwv lock` itself would emit still diffs against a real relock.
    let raw_lock = format!(
        "{{\"repositories\": {{{path:?}: {{\"type\": \"git\", \"url\": \"file://{repo}\", \"version\": {sha:?}}}}}}}",
        path = MANIFEST_REPO_PATH,
        repo = common::url_path(&manifest_repo),
        sha = initial_sha
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
        &project_dir,
    );
    git(&["commit", "-m", "lock: initial"], &project_dir);
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
    git_out(&["rev-parse", "HEAD"], repo)
}

// ===========================================================================
// LockRelation::Ok — both verbs proceed on a fresh lock
// ===========================================================================

#[test]
fn relation_ok_sync_to_succeeds() {
    let f = fixture();
    // ww makes a project-only (non-lock) commit so sync-to has something to land
    // while the manifest repo stays at the ok lock relation.
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");
    rwv()
        .args(["sync-to", &f.main.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    // The landing DELIVERED: main's project repo advanced to ww's tip.
    assert_eq!(
        head(&f.main.project_dir),
        head(&f.ww.project_dir),
        "sync-to must land ww's project commit on main"
    );
}

#[test]
fn relation_ok_sync_succeeds() {
    let f = fixture();
    // main advances + relocks so a `rwv sync primary` from ww has content to pull,
    // with both sides at a fresh (ok) lock relation.
    commit_file(&f.main.manifest_repo, "m.txt", "m\n", "main: advance");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&f.main.root)
        .assert()
        .success();
    rwv()
        .args(["sync", "primary"])
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

/// sync-to: a workweave whose manifest repo advanced past its committed lock
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
        .args(["sync-to", &f.main.root.to_string_lossy()])
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
    let main_lib = git_out(&["rev-parse", "main"], &f.main.manifest_repo);
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
    git(
        &["config", "core.hooksPath", &hooks_dir.to_string_lossy()],
        repo,
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

    // The op fails: phase 3's relock hits the same blocked commit and bails.
    // That failure is also this fixture's proof that the hook fired at all —
    // without it, every check below holds against a run where the hook never
    // ran and the relock succeeded.
    let assert = rwv()
        .args(["sync-to", &f.main.root.to_string_lossy()])
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
    let ww_lock_committed = git_out(
        &["show", &format!("{ww_project_before}:rwv.lock")],
        &f.ww.project_dir,
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
        stderr.contains("op-start relock could not commit"),
        "the operator must be told the op-start relock failed; got stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(MANIFEST_REPO_PATH) && stderr.contains("not relocked"),
        "the per-repo line must name the repo whose lock is still behind HEAD; got \
         stderr:\n{stderr}"
    );
}

/// sync-to: the TARGET's manifest repo advanced past the target's committed
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
        .args(["sync-to", &f.main.root.to_string_lossy()])
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
        "a refused sync-to must leave the target's manifest repo untouched"
    );
    assert_eq!(
        head(&f.main.project_dir),
        target_project_before,
        "a refused sync-to must NOT advance the target's project repo"
    );
    assert_eq!(
        std::fs::read_to_string(f.main.project_dir.join("rwv.lock")).unwrap(),
        target_lock_before,
        "a refused sync-to must leave the target's lock file untouched"
    );
    assert_eq!(
        head(&f.ww.project_dir),
        cwd_project_before,
        "a refused sync-to must leave CWD's project repo untouched"
    );

    // No op-state leaked: a re-run repeats the SAME refusal rather than
    // reporting an in-flight op.
    let again = rwv()
        .args(["sync-to", &f.main.root.to_string_lossy()])
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
        .args(["sync-to", &f.ww.root.to_string_lossy()])
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
        "a refused sync-to must leave the workweave target's project repo untouched"
    );

    rwv()
        .args(["lock", "--commit", "--project", PROJECT])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    rwv()
        .args(["sync-to", &f.ww.root.to_string_lossy()])
        .current_dir(&child.root)
        .assert()
        .success();

    assert_eq!(
        head(&f.ww.project_dir),
        head(&child.project_dir),
        "after the remedy the child's project commit must land in the parent workweave"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &f.ww.manifest_repo),
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
        .args(["sync-to", &f.main.root.to_string_lossy()])
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
        .args(["sync-to", &f.main.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .success();

    // DELIVERED: the target's manifest repo still holds the commit that was
    // stranding the op, CWD's replay picked it up, and both sides converged.
    let target_lib = git_out(&["rev-parse", "main"], &f.main.manifest_repo);
    assert_eq!(
        target_lib,
        head(&f.ww.manifest_repo),
        "after the remedy, CWD and the target must agree on the manifest tip"
    );
    // The target's own unlocked commit survived the landing (git exits non-zero
    // when it is not an ancestor, which `git` turns into a failure here).
    git(
        &["merge-base", "--is-ancestor", &target_docs, &target_lib],
        &f.main.manifest_repo,
    );
    assert_eq!(
        head(&f.main.project_dir),
        head(&f.ww.project_dir),
        "after the remedy, sync-to must land CWD's project commit on the target"
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
            "sync-to",
            &f.main.root.to_string_lossy(),
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
            "sync-to",
            &f.main.root.to_string_lossy(),
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
    let target_lib_tip = git_out(&["rev-parse", "main"], &f.main.manifest_repo);
    for (_repo_path, entry) in lock.iter_entries() {
        git(
            &[
                "merge-base",
                "--is-ancestor",
                entry.version.as_str(),
                &target_lib_tip,
            ],
            &f.main.manifest_repo,
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
    let target_live = git_out(&["rev-parse", "main"], &f.main.manifest_repo);
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");
    commit_file(&f.ww.manifest_repo, "feature.txt", "f\n", "ww: feature");

    rwv()
        .args([
            "sync-to",
            &f.main.root.to_string_lossy(),
            "--allow-stale-lock",
        ])
        .current_dir(&f.ww.root)
        .assert()
        .failure();

    // The operator's fix: put CWD's manifest repo on top of the target's live
    // tip so step 3's fast-forward can proceed. Nothing relocks this.
    git(&["rebase", &target_live], &f.ww.manifest_repo);
    let cwd_lib_after_fix = head(&f.ww.manifest_repo);

    rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&f.ww.root)
        .assert()
        .success();

    let target_lib = git_out(&["rev-parse", "main"], &f.main.manifest_repo);
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

/// sync (pull) from a WORKWEAVE source whose lock is behind HEAD (`Ahead`):
/// tips-as-truth. The pull prints a NOTE naming the source's lag, pulls the
/// source's committed tips, and leaves the source's lock file alone (no
/// cross-workspace mutation on a read verb).
#[test]
fn relation_ahead_sync_from_workweave_source_pulls_tips_with_note() {
    let f = fixture();
    // Second workweave acts as the pull SOURCE; the primary (main) is CWD.
    let weaveroot = f.main.root.parent().unwrap().join(".workweaves");
    let src = create_workweave(&f.main, &weaveroot, "src");

    // src advances its manifest repo by 3 commits WITHOUT relocking → src's
    // committed lock is behind src's HEAD (LockRelation::Ahead on the source).
    commit_file(&src.manifest_repo, "x.txt", "x\n", "src: x");
    commit_file(&src.manifest_repo, "y.txt", "y\n", "src: y");
    commit_file(&src.manifest_repo, "z.txt", "z\n", "src: z");
    let src_lock_before = std::fs::read_to_string(src.project_dir.join("rwv.lock")).unwrap();

    let assert = rwv()
        .args(["sync", &src.root.to_string_lossy()])
        .current_dir(&f.main.root)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    assert!(
        stderr.contains("source lock behind HEAD by 3 commits")
            && stderr.contains("pulling committed tips"),
        "expected tips-as-truth note naming the source lag; got stderr:\n{stderr}"
    );

    // The source's lock file was NOT mutated (read-side verb; source heals itself).
    let src_lock_after = std::fs::read_to_string(src.project_dir.join("rwv.lock")).unwrap();
    assert_eq!(
        src_lock_before, src_lock_after,
        "tips-as-truth must NOT touch the source's lock file"
    );

    // The pull DELIVERED the tips the note announced: main's manifest repo
    // landed on src's committed HEAD (not src's stale lock), and main's
    // relock pinned it.
    let main_lib = head(&f.main.manifest_repo);
    let src_lib = head(&src.manifest_repo);
    assert_eq!(
        main_lib, src_lib,
        "pull must deliver the source's committed tips"
    );
    let main_lock = std::fs::read_to_string(f.main.project_dir.join("rwv.lock")).unwrap();
    assert!(
        main_lock.contains(&src_lib),
        "destination lock must pin the delivered tip; lock:\n{main_lock}"
    );
}

/// A pull from an `Ahead` workweave source, stranded mid-replay and made ready
/// to resume.
///
/// The source relocks once and then commits again, so its lock and its tip are
/// distinct revisions the destination starts at neither of: what the resume
/// leaves behind names which of the two replay targeted.
///
/// The strand is a project-repo conflict. Replay syncs the MEMBER repos first
/// and the project repo second, so this stops the op with the members already
/// landed under the fresh invocation's pin — which is why the source then
/// commits again inside the operator's window. The resume re-pins at its own
/// T0, so that later commit is what tips-as-truth must reach and what the
/// recorded consent must decline to chase. Without it both outcomes would look
/// identical to "the resume did nothing".
struct StrandedPull {
    f: Fixture,
    /// The source's committed lock revision: replay's target under the
    /// recorded `allow-stale-lock` consent.
    locked: String,
    /// The source's member tip at the moment the resume re-pins: replay's
    /// target under tips-as-truth.
    tip_at_resume: String,
    /// Where the fresh invocation left the destination's member repo.
    dest_after_strand: String,
}

fn strand_a_pull_from_an_ahead_workweave(extra: &[&str]) -> StrandedPull {
    let f = fixture();
    let weaveroot = f.main.root.parent().unwrap().join(".workweaves");
    let src = create_workweave(&f.main, &weaveroot, "src");
    let dest_before = head(&f.main.manifest_repo);

    commit_file(&src.manifest_repo, "x.txt", "x\n", "src: x");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&src.root)
        .assert()
        .success();
    let locked = head(&src.manifest_repo);
    commit_file(&src.manifest_repo, "y.txt", "y\n", "src: y");

    commit_file(
        &src.project_dir,
        "notes/shared.md",
        "src take\n",
        "docs: src take",
    );
    commit_file(
        &f.main.project_dir,
        "notes/shared.md",
        "main take\n",
        "docs: main take",
    );

    let mut args: Vec<String> = ["sync", &src.root.to_string_lossy(), "--strategy", "rebase"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.extend(extra.iter().map(|s| s.to_string()));
    rwv()
        .args(&args)
        .current_dir(&f.main.root)
        .assert()
        .failure();

    assert_eq!(
        repoweave::git::git_vcs()
            .mid_operation(&f.main.project_dir)
            .as_deref(),
        Some("mid-rebase"),
        "the pull must strand mid-replay, or there is nothing for `--continue` to resume"
    );
    let dest_after_strand = head(&f.main.manifest_repo);
    assert_ne!(
        dest_after_strand, dest_before,
        "the strand must follow the member sync, or the source's later commit is not \
         what separates the two targets"
    );

    std::fs::write(
        f.main.project_dir.join("notes/shared.md"),
        "operator-resolved\n",
    )
    .unwrap();
    git(&["add", "notes/shared.md"], &f.main.project_dir);
    git(&["rebase", "--continue"], &f.main.project_dir);

    commit_file(&src.manifest_repo, "z.txt", "z\n", "src: z");
    let tip_at_resume = head(&src.manifest_repo);

    StrandedPull {
        f,
        locked,
        tip_at_resume,
        dest_after_strand,
    }
}

/// Read the `overrides` array off the in-progress owner record.
fn recorded_overrides(workspace_root: &Path) -> Vec<String> {
    let raw = std::fs::read_to_string(workspace_root.join(".rwv-op"))
        .expect("a stranded op must leave an owner record");
    let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
    doc["overrides"]
        .as_array()
        .expect("owner record must carry an overrides array")
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

/// A tips-as-truth pull that was interrupted and resumed still targets the
/// source's committed tips.
///
/// The fresh path's coverage above cannot see this one: `--continue` re-pins
/// the snapshot from scratch, so the classification the fresh guard reached is
/// thrown away and reached again from the persisted record. A regression here
/// is a state-reconstruction bug — no signature and no call site changes.
#[test]
fn a_resumed_pull_from_an_ahead_workweave_source_still_targets_the_tips() {
    let sp = strand_a_pull_from_an_ahead_workweave(&[]);
    assert_ne!(
        sp.dest_after_strand, sp.tip_at_resume,
        "setup: the source must have moved in the window, or the resume has nowhere to go"
    );
    assert!(
        recorded_overrides(&sp.f.main.root).is_empty(),
        "setup: the record must carry no consent, or this is the sibling test"
    );

    rwv()
        .args(["sync", "--continue"])
        .current_dir(&sp.f.main.root)
        .assert()
        .success();

    let delivered = head(&sp.f.main.manifest_repo);
    assert_eq!(
        delivered, sp.tip_at_resume,
        "the resumed pull must deliver the source's committed tip, not its lock ({})",
        sp.locked
    );
    let lock = std::fs::read_to_string(sp.f.main.project_dir.join("rwv.lock")).unwrap();
    assert!(
        lock.contains(&sp.tip_at_resume),
        "the destination lock must pin what the resume delivered; lock:\n{lock}"
    );
}

/// The negative half, and the one that pins the read-back specifically: the
/// consent was spelled on the fresh invocation only, so the resume can honour
/// it solely by recovering `allow-stale-lock` from the owner record. Under it
/// the source's later commit must NOT be chased — replay's target stays the
/// lock.
#[test]
fn a_resumed_pull_carrying_allow_stale_lock_still_targets_the_lock() {
    let sp = strand_a_pull_from_an_ahead_workweave(&["--allow-stale-lock"]);
    assert_eq!(
        recorded_overrides(&sp.f.main.root),
        vec!["allow-stale-lock".to_string()],
        "the consent must reach the record, or the resume has nothing to read back"
    );
    assert_eq!(
        sp.dest_after_strand, sp.locked,
        "setup: the consented fresh pull must have targeted the lock"
    );

    rwv()
        .args(["sync", "--continue"])
        .current_dir(&sp.f.main.root)
        .assert()
        .success();

    let delivered = head(&sp.f.main.manifest_repo);
    assert_eq!(
        delivered, sp.locked,
        "under the recorded consent the resumed pull must keep the lock as replay's target"
    );
    assert_ne!(
        delivered, sp.tip_at_resume,
        "the recorded consent must survive the resume; chasing the tip means it did not"
    );
}

/// sync (pull) from a PRIMARY-weave source whose lock is behind HEAD (`Ahead`):
/// tips-as-truth is scoped to workweave sources, so a primary source keeps the
/// REFUSAL (naming the relation + `--allow-stale-lock`).
#[test]
fn relation_ahead_sync_from_primary_source_refuses() {
    let f = fixture();
    // main (primary) advances its manifest repo WITHOUT relocking → lock behind
    // HEAD on the primary source.
    let ww_lib_before = head(&f.ww.manifest_repo);
    commit_file(&f.main.manifest_repo, "p.txt", "p\n", "main: advance");

    let assert = rwv()
        .args(["sync", "primary"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("lock-freshness precondition failed")
            && stderr.contains("--allow-stale-lock"),
        "primary source lock-behind-HEAD must refuse naming the flag; got:\n{stderr}"
    );
    // The refusal delivered NOTHING: ww's manifest repo did not move.
    assert_eq!(
        head(&f.ww.manifest_repo),
        ww_lib_before,
        "a refused pull must leave the destination untouched"
    );
}

// ===========================================================================
// LockRelation::Ahead on the pull DESTINATION — benign
// ===========================================================================

/// The two-step trap, driven whole. A workweave commits member work, so its own
/// lock is behind its HEAD; the destination's lock is not replay's input and
/// Phase 3 regenerates it, so the pull proceeds and performs the relock the old
/// refusal used to demand.
///
/// The refusal this replaces named `rwv lock --commit` as the remedy, and that
/// relock commit then tripped the ancestry gate on the retry — the two
/// preconditions were mutually unsatisfiable for the bare form. Pinning only
/// "step 1 no longer refuses" would pass on a fix that leaves the operator at
/// step 3, so this asserts what the op DELIVERED: the lock pins the tip.
#[test]
fn pull_destination_lock_behind_head_syncs_and_performs_the_relock_itself() {
    let f = fixture();
    // The ordinary thing a workweave does: commit member work, do not relock.
    let ww_tip = commit_file(&f.ww.manifest_repo, "f.txt", "f\n", "ww: feature");

    let assert = rwv()
        .args(["sync", "primary"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();

    // Accepted, not silently: the relation and its commit count are announced.
    assert!(
        stderr.contains("lock behind HEAD by 1 commits"),
        "the accepted destination relation must be announced with its count; got:\n{stderr}"
    );

    // DELIVERED — the op relocked, so the destination's lock now pins the
    // destination's member tip. This is the state the old first refusal was
    // demanding the operator reach by hand.
    let lock = repoweave::manifest::LockFile::from_json_str(
        &std::fs::read_to_string(f.ww.project_dir.join("rwv.lock")).unwrap(),
    )
    .unwrap();
    let pinned = lock
        .iter_entries()
        .find(|(p, _)| p.as_str() == MANIFEST_REPO_PATH)
        .expect("the fixture's one manifest repo must be in the lock")
        .1
        .version
        .as_str()
        .to_owned();
    assert_eq!(
        pinned, ww_tip,
        "after the pull the destination's lock must pin the destination's member tip"
    );
    // The local member work survived the pull.
    assert_eq!(
        head(&f.ww.manifest_repo),
        ww_tip,
        "a pull must not rewind the destination's own committed member work"
    );
}

/// The trap's second act. The relaxed pull's own relock leaves the
/// destination's project repo one commit ahead of the source, so a second bare
/// pull refuses at the ancestry gate — over rwv's own bookkeeping. The gate is
/// correct to refuse (a bare pull fast-forwards, and ff cannot advance past a
/// destination-only commit without discarding it), so what carries the
/// operator is the refusal's evidence: it must quote the relock commit's
/// subject, making the blocking commit recognisable as rwv's own, and the
/// strategy it names must then converge rather than manufacture a third
/// refusal.
#[test]
fn second_bare_pull_refusal_identifies_rwvs_own_relock_and_its_remedy_converges() {
    let f = fixture();
    let ww_tip = commit_file(&f.ww.manifest_repo, "f.txt", "f\n", "ww: feature");
    rwv()
        .args(["sync", "primary"])
        .current_dir(&f.ww.root)
        .assert()
        .success();

    let assert = rwv()
        .args(["sync", "primary"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("strictly ahead of source workspace") && stderr.contains("by 1 commit"),
        "the second pull's refusal must state the destination's relation to the source; \
         got:\n{stderr}"
    );
    assert!(
        stderr.contains("lock: auto-relock after sync from"),
        "the evidence block must quote the relock commit's subject, so the operator can \
         recognise the blocking commit as rwv's own bookkeeping; got:\n{stderr}"
    );
    assert!(
        stderr.contains("--strategy rebase"),
        "the refusal must name the strategy that lands the relock commit; got:\n{stderr}"
    );
    // The refusal must not read as a one-time flag: assertion 5 below proves
    // the remedy does not retire the shape, and this is what tells the
    // operator that ahead of time, plus the terminating condition.
    assert!(
        stderr.contains("recurs on every fast-forward sync") && stderr.contains("sync-to"),
        "the refusal must warn that it recurs until the relock reaches the source, and \
         name `sync-to` as the way to land it there; got:\n{stderr}"
    );

    rwv()
        .args(["sync", "primary", "--strategy", "rebase"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    // DELIVERED — the named remedy converged: the lock still pins the
    // destination's member tip and the member work survived.
    let lock = repoweave::manifest::LockFile::from_json_str(
        &std::fs::read_to_string(f.ww.project_dir.join("rwv.lock")).unwrap(),
    )
    .unwrap();
    let pinned = lock
        .iter_entries()
        .find(|(p, _)| p.as_str() == MANIFEST_REPO_PATH)
        .expect("the fixture's one manifest repo must be in the lock")
        .1
        .version
        .as_str()
        .to_owned();
    assert_eq!(
        pinned, ww_tip,
        "after the remedy the destination's lock must still pin the destination's member tip"
    );
    assert_eq!(
        head(&f.ww.manifest_repo),
        ww_tip,
        "the remedy must not rewind the destination's own committed member work"
    );
    // The remedy cannot retire the shape: the relock survives the replay, so
    // the destination is again exactly one bookkeeping commit ahead and the
    // NEXT bare pull wants the flag again, until the relock lands in the
    // source. The refusal's evidence block is what carries the operator
    // through each round.
    let ahead = git_out(
        &[
            "log",
            "--oneline",
            &format!("{}..HEAD", head(&f.main.project_dir)),
        ],
        &f.ww.project_dir,
    );
    assert!(
        ahead.lines().count() == 1 && ahead.contains("lock: auto-relock after sync from"),
        "after the remedy the destination must be ahead by exactly its own relock; got:\n{ahead}"
    );
}

/// The relaxation is scoped to `Ahead`. A destination whose lock records a
/// commit HEAD lacks is still anomalous and still refuses — and because the
/// remedy it names lands a project-repo commit, the refusal must also name the
/// `--strategy rebase` that commit then requires. Naming only the relock is
/// what made the two gates mutually unsatisfiable.
#[test]
fn pull_destination_anomalous_lock_refuses_and_names_the_follow_on_strategy() {
    let f = fixture();
    // lock pins C2, HEAD reset to C1 → Behind.
    let c1 = head(&f.ww.manifest_repo);
    commit_file(&f.ww.manifest_repo, "c2.txt", "c2\n", "ww: c2");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&f.ww.root)
        .assert()
        .success();
    git(&["reset", "--hard", &c1], &f.ww.manifest_repo);

    let assert = rwv()
        .args(["sync", "primary"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("lock-freshness precondition failed") && stderr.contains("behind"),
        "an anomalous destination relation must still refuse, naming it; got:\n{stderr}"
    );
    assert!(
        stderr.contains("rwv lock --commit"),
        "the refusal must still name the relock remedy; got:\n{stderr}"
    );
    assert!(
        stderr.contains("--strategy rebase"),
        "the refusal must name the strategy its own remedy then requires, or the \
         operator lands at a second refusal caused by the first one's fix; got:\n{stderr}"
    );
    // The landed relock commit is a clean linear addition the replay
    // reapplies rather than merges away, so it survives every subsequent
    // rebase the same way an auto-relock does (see
    // second_bare_pull_refusal_identifies_rwvs_own_relock_and_its_remedy_converges).
    // This remedy must not read as a one-time fix either.
    assert!(
        stderr.contains("recurs on every subsequent fast-forward sync")
            && stderr.contains("sync-to"),
        "the relock remedy must warn that it recurs until the relock reaches the source, \
         and name `sync-to` as the way to land it there; got:\n{stderr}"
    );
}

// ===========================================================================
// LockRelation::Behind (spec term "ahead" — HEAD reset below lock) — refuse
// ===========================================================================

/// sync-to: CWD's committed lock records a manifest SHA that HEAD does not have
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
    git(&["reset", "--hard", &c1], &f.ww.manifest_repo);
    let target_project_before = head(&f.main.project_dir);

    let assert = rwv()
        .args(["sync-to", &f.main.root.to_string_lossy()])
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
        "a refused sync-to must leave the target's project repo untouched"
    );

    // Escape hatch still works: --allow-stale-lock bypasses the gate. Use
    // --strategy=ff (ww is at C1, which is where main also is — an equal-tip
    // no-op is fine) to confirm the flag opens the door.
    rwv()
        .args([
            "sync-to",
            &f.main.root.to_string_lossy(),
            "--allow-stale-lock",
            "--strategy=ff",
        ])
        .current_dir(&f.ww.root)
        .assert()
        .success();
}

/// sync (pull): a `Behind` SOURCE (source's lock records commits its HEAD lacks)
/// refuses on both source-type paths — it is never benign.
#[test]
fn relation_behind_source_sync_refuses() {
    let f = fixture();
    let weaveroot = f.main.root.parent().unwrap().join(".workweaves");
    let src = create_workweave(&f.main, &weaveroot, "src");

    // src advances to C2, relocks (lock pins C2), then resets HEAD back to C1.
    let c1 = head(&src.manifest_repo);
    commit_file(&src.manifest_repo, "c2.txt", "c2\n", "src: c2");
    rwv()
        .args(["lock", "--commit"])
        .current_dir(&src.root)
        .assert()
        .success();
    git(&["reset", "--hard", &c1], &src.manifest_repo);
    let dest_lib_before = head(&f.main.manifest_repo);

    let assert = rwv()
        .args(["sync", &src.root.to_string_lossy()])
        .current_dir(&f.main.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("lock-freshness precondition failed") && stderr.contains("behind"),
        "Behind source must refuse naming the relation; got:\n{stderr}"
    );
    assert_eq!(
        head(&f.main.manifest_repo),
        dest_lib_before,
        "a refused pull must leave the destination untouched"
    );
}

// ===========================================================================
// LockRelation::Diverged — refuse, additionally hint `rwv lock --commit`
// ===========================================================================

/// sync-to: CWD's lock and HEAD have diverged past a shared base (out-of-band
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
    git(&["reset", "--hard", &c1], &f.ww.manifest_repo);
    commit_file(&f.ww.manifest_repo, "b.txt", "b\n", "ww: c2b");
    assert_ne!(head(&f.ww.manifest_repo), c2a);
    let target_project_before = head(&f.main.project_dir);
    let target_lib_before = head(&f.main.manifest_repo);

    let assert = rwv()
        .args(["sync-to", &f.main.root.to_string_lossy()])
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
        "a refused sync-to must leave the target's project repo untouched"
    );
    assert_eq!(
        head(&f.main.manifest_repo),
        target_lib_before,
        "a refused sync-to must leave the target's manifest repo untouched"
    );
}

// ===========================================================================
// LockRelation::NoLock / Unknown — refuse
// ===========================================================================

/// sync (pull): a source lock that pins a tag/branch that does not resolve on
/// disk is a corrupt-lock error — refuse, naming the unknown revision.
#[test]
fn unresolvable_source_lock_refuses_naming_unknown_revision() {
    let f = fixture();
    // Rewrite main's committed lock to pin a nonexistent tag.
    let manifest_repo = f.main.manifest_repo.clone();
    let raw_lock = format!(
        "{{\"repositories\": {{{path:?}: {{\"type\": \"git\", \"url\": \"file://{repo}\", \"version\": \"v9.9.9-nope\"}}}}}}",
        path = MANIFEST_REPO_PATH,
        repo = common::url_path(&manifest_repo),
    );
    let bad_lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&bad_lock, &f.main.project_dir.join("rwv.lock")).unwrap();
    git(&["add", "rwv.lock"], &f.main.project_dir);
    git(
        &["commit", "-m", "lock: nonexistent tag"],
        &f.main.project_dir,
    );

    let dest_lib_before = head(&f.ww.manifest_repo);
    let assert = rwv()
        .args(["sync", "primary"])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("unknown revision") && stderr.contains("v9.9.9-nope"),
        "unresolvable source lock must name the unknown revision; got:\n{stderr}"
    );
    assert_eq!(
        head(&f.ww.manifest_repo),
        dest_lib_before,
        "a refused pull must leave the destination untouched"
    );
}

/// sync-to: a CWD project that carries NO committed lock at all classifies as
/// `no-lock` and refuses (define behavior explicitly — not a silent proceed).
#[test]
fn no_lock_sync_to_refuses() {
    let f = fixture();
    // Remove the committed rwv.lock from ww's project repo entirely.
    git(&["rm", "rwv.lock"], &f.ww.project_dir);
    git(&["commit", "-m", "drop lock"], &f.ww.project_dir);
    let target_project_before = head(&f.main.project_dir);

    let assert = rwv()
        .args(["sync-to", &f.main.root.to_string_lossy()])
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
        "a refused sync-to must leave the target's project repo untouched"
    );
}

// ===========================================================================
// Source-side cleanliness preflight (§1)
// ===========================================================================

/// sync-to refuses UP FRONT on a tracked-dirty manifest repo, naming the dirty
/// repo, before any op-state write or rebase.
#[test]
fn dirty_source_tracked_change_refuses_and_names_repo() {
    let f = fixture();
    // Modify a TRACKED file in ww's manifest repo (README.md exists from init).
    std::fs::write(f.ww.manifest_repo.join("README.md"), "dirtied\n").unwrap();

    let assert = rwv()
        .args(["sync-to", &f.main.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("uncommitted tracked changes") && stderr.contains(MANIFEST_REPO_PATH),
        "tracked dirt must refuse up front naming the repo; got:\n{stderr}"
    );
}

/// sync-to IGNORES untracked files in the source (they survive the replay). An
/// untracked scratch file must not block a landing.
#[test]
fn dirty_source_untracked_file_is_ignored() {
    let f = fixture();
    // Give sync-to something to land (a project-only commit), plus an UNTRACKED
    // scratch file in the manifest repo that must not refuse.
    commit_file(&f.ww.project_dir, "note.txt", "n\n", "ww: note");
    std::fs::write(f.ww.manifest_repo.join("scratch.tmp"), "scratch\n").unwrap();

    rwv()
        .args(["sync-to", &f.main.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .success();

    // The untracked scratch file is still present (never touched).
    assert!(
        f.ww.manifest_repo.join("scratch.tmp").exists(),
        "untracked scratch file must survive the op"
    );
}

/// Carve-out: a project repo dirty ONLY in `rwv.lock` is NOT dirt (it is the
/// auto-relock's own input). sync-to proceeds and commits the regenerated lock.
#[test]
fn dirty_source_rwv_lock_only_is_carved_out() {
    let f = fixture();
    // Advance ww's manifest repo (so there is a real landing) and leave the lock
    // file MODIFIED-BUT-UNCOMMITTED in the project repo — the only tracked dirt
    // in the project repo is rwv.lock, which the carve-out permits.
    commit_file(&f.ww.manifest_repo, "adv.txt", "adv\n", "ww: adv");
    // Hand-edit the committed lock file so it shows as a tracked
    // modification. Trailing whitespace is the only append JSON tolerates
    // without becoming unparseable — a comment line (the YAML-era trick)
    // is trailing *content* and fails to parse.
    let lock_path = f.ww.project_dir.join("rwv.lock");
    let mut contents = std::fs::read_to_string(&lock_path).unwrap();
    contents.push('\n');
    std::fs::write(&lock_path, contents).unwrap();
    // Confirm the carve-out target really is dirty-tracked before we assert.
    let porcelain = git_out(&["status", "--porcelain"], &f.ww.project_dir);
    assert!(
        porcelain.contains("rwv.lock"),
        "test precondition: rwv.lock must be tracked-dirty; got porcelain:\n{porcelain}"
    );

    rwv()
        .args(["sync-to", &f.main.root.to_string_lossy()])
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
        .args(["sync-to", &f.main.root.to_string_lossy()])
        .current_dir(&f.ww.root)
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("uncommitted tracked changes")
            && stderr.contains("(project)")
            && stderr.contains("rwv.toml"),
        "a tracked non-lock project change must refuse and name the file; got:\n{stderr}"
    );
}
