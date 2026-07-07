//! Adversarial coverage for `fo-4rpnkm.1` (Design-B Corrections 2 & 3):
//! source-side cleanliness preflight, benign-staleness classification (reusing
//! `status::LockRelation`), op-start auto-relock with commit-count output, and
//! tips-as-truth pulls from a workweave source.
//!
//! TERMINOLOGY (load-bearing): `LockRelation` names the relation from the TIP's
//! vantage point, which is inverted from the bead's prose.
//!
//! - bead "lock behind HEAD" (new commits since the last lock, the benign
//!   in-progress shape) == `LockRelation::Ahead` (tip ahead of lock).
//! - bead "ahead" (HEAD is ancestor of lock — a reset) == `LockRelation::Behind`.
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

/// Build the primary workspace with `rwv.yaml` + `rwv.lock` committed and the
/// manifest repo pinned at its initial SHA.
fn make_main_workspace(tmp: &Path) -> MainWorkspace {
    let ws = tmp.join("ws");
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

struct Fixture {
    _tmp: tempfile::TempDir,
    main: MainWorkspace,
    ww: Workweave,
}

/// Primary + one workweave `ww`, both at the initial (fresh, `ok`) lock state.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
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
}

#[test]
fn relation_ok_sync_succeeds() {
    let f = fixture();
    // main advances + relocks so a bare `rwv sync` from ww has content to pull,
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
}

// ===========================================================================
// LockRelation::Ahead (bead "lock behind HEAD") — benign
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
}

/// sync (pull) from a PRIMARY-weave source whose lock is behind HEAD (`Ahead`):
/// tips-as-truth is scoped to workweave sources, so a primary source keeps the
/// REFUSAL (naming the relation + `--allow-stale-lock`).
#[test]
fn relation_ahead_sync_from_primary_source_refuses() {
    let f = fixture();
    // main (primary) advances its manifest repo WITHOUT relocking → lock behind
    // HEAD on the primary source.
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
}

// ===========================================================================
// LockRelation::Behind (bead "ahead" — HEAD reset below lock) — refuse
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
    let bad_lock = format!(
        "repositories:\n  {path}:\n    type: git\n    url: file://{repo}\n    version: v9.9.9-nope\n",
        path = MANIFEST_REPO_PATH,
        repo = manifest_repo.display(),
    );
    std::fs::write(f.main.project_dir.join("rwv.lock"), bad_lock).unwrap();
    git(&["add", "rwv.lock"], &f.main.project_dir);
    git(
        &["commit", "-m", "lock: nonexistent tag"],
        &f.main.project_dir,
    );

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
}

/// sync-to: a CWD project that carries NO committed lock at all classifies as
/// `no-lock` and refuses (define behavior explicitly — not a silent proceed).
#[test]
fn no_lock_sync_to_refuses() {
    let f = fixture();
    // Remove the committed rwv.lock from ww's project repo entirely.
    git(&["rm", "rwv.lock"], &f.ww.project_dir);
    git(&["commit", "-m", "drop lock"], &f.ww.project_dir);

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
    // Hand-edit the committed lock file so it shows as a tracked modification.
    let lock_path = f.ww.project_dir.join("rwv.lock");
    let mut contents = std::fs::read_to_string(&lock_path).unwrap();
    contents.push_str("# scratch edit to dirty the lock\n");
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
    // Modify a tracked NON-lock file in the project repo (rwv.yaml exists).
    let yaml_path = f.ww.project_dir.join("rwv.yaml");
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
            && stderr.contains("rwv.yaml"),
        "a tracked non-lock project change must refuse and name the file; got:\n{stderr}"
    );
}
