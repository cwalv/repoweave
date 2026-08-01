//! E2E coverage for rwv topology + parent exposure.
//!
//! Exercises the Correction-5 + parent-exposure surface:
//!   1. retire/delete ADOPT living children (0/1/N children; grandparent vs
//!      primary fallback) and print the loud per-child line.
//!   2. dangling-parent `rwv doctor --fix` re-points to primary.
//!   3. a bare `rwv sync-to` on a dangling parent emits friendly
//!      doctor-remediation text (no raw `failed to canonicalize … (os error 2)`).
//!   4. `rwv status --json` `.parent` carries the recorded path + parent tip,
//!      correct for a STACKED parent.
//!   5. `rwv workweave log`/`diff` compute unique commits vs the parent, correct
//!      when the parent ADVANCED after the fork (no phantom reversals), across a
//!      stacked parent, with a `--json` shape.

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

const MANIFEST_REPO_PATH: &str = "github/org/lib";
const PROJECT: &str = "app";

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

struct MainWorkspace {
    root: PathBuf,
    manifest_repo: PathBuf,
    weaveroot: PathBuf,
}

/// Build the primary workspace with a manifest repo + project repo, and a
/// dedicated `.workweaves/` dir for workweaves.
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

    // Put the workweave parent OUTSIDE the workspace root (sibling), which is
    // where rwv puts `.workweaves/` by default (parent of ws root).
    let weaveroot = tmp.join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    MainWorkspace {
        root: ws,
        manifest_repo,
        weaveroot,
    }
}

struct Workweave {
    name: String,
    root: PathBuf,
    manifest_repo: PathBuf,
}

/// Create a workweave forked from `from` (a workspace root path), or from
/// primary when `from` is None.
fn create_workweave(main: &MainWorkspace, name: &str, from: Option<&Path>) -> Workweave {
    let mut cmd = rwv();
    cmd.args(["workweave", PROJECT, "create", name]);
    if let Some(f) = from {
        cmd.args(["--from", &f.to_string_lossy()]);
    }
    cmd.current_dir(&main.root).assert().success();

    let root = main.weaveroot.join(format!("{PROJECT}--{name}"));
    Workweave {
        name: name.to_string(),
        manifest_repo: root.join(MANIFEST_REPO_PATH),
        root,
    }
}

/// Read the recorded parent from a workweave's `.rwv-workweave` marker.
fn recorded_parent(ww_root: &Path) -> String {
    let content = std::fs::read_to_string(ww_root.join(".rwv-workweave")).unwrap();
    let marker: serde_json::Value = serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("marker at {} is not JSON: {e}", ww_root.display()));
    marker["parent"]
        .as_str()
        .unwrap_or_else(|| panic!("no parent field in marker at {}", ww_root.display()))
        .to_string()
}

fn canon(p: &Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .to_string()
}

/// Run `rwv lock --commit` from a workspace root.
fn rwv_lock_commit(workspace_root: &Path) {
    rwv()
        .args(["lock", "--commit"])
        .current_dir(workspace_root)
        .assert()
        .success();
}

// ===========================================================================
// 1. Adoption on delete — 0 / 1 / N children
// ===========================================================================

/// Deleting a childless workweave prints no adoption line and succeeds.
#[test]
fn delete_with_zero_children_adopts_nothing() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());
    let ww = create_workweave(&main, "solo", None);
    // Ensure clean (no unmerged commits) so delete without a waiver works.
    let out = rwv()
        .args(["workweave", PROJECT, "delete", &ww.name])
        .current_dir(&main.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "delete failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("adopted child workweave"),
        "no adoption line expected for a childless delete; got:\n{stderr}"
    );
    assert!(!ww.root.exists(), "workweave dir should be gone");
}

/// Cross-verb mutex (Correction 1 COVERAGE): `workweave delete`
/// refuses while an op involves the target workweave, naming the in-flight op.
/// The workweave must NOT be destroyed — `rwv abort` (not delete) clears a
/// stale record.
#[test]
fn delete_refuses_while_workweave_is_mid_op() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());
    let ww = create_workweave(&main, "busy", None);

    // Plant a v2 owner record in the workweave root (simulate an in-flight op).
    let op_json = format!(
        "{{\"id\": \"planted-delete-op\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"replay\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        src = ww.root.display(),
        tgt = main.root.display(),
    );
    std::fs::write(ww.root.join(".rwv-op"), &op_json).unwrap();

    let out = rwv()
        .args(["workweave", PROJECT, "delete", &ww.name])
        .current_dir(&main.root)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "delete must refuse while the workweave is mid-op"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("sync-to in progress") && stderr.contains("in progress (started"),
        "refusal must name the in-flight op with its age; got:\n{stderr}"
    );
    assert!(
        stderr.contains("--continue") && stderr.contains("rwv abort"),
        "refusal must offer `--continue` and `rwv abort`; got:\n{stderr}"
    );
    assert!(
        ww.root.exists(),
        "the mid-op workweave must NOT be destroyed by the refused delete"
    );
}

/// The discard waivers do NOT bypass the op mutex on delete: the hazard is to
/// the op's recovery, and the waivers are for dirty/unmerged work, not stale
/// op-state.
#[test]
fn delete_waivers_do_not_bypass_op_mutex() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());
    let ww = create_workweave(&main, "busyforce", None);

    let op_json = format!(
        "{{\"id\": \"planted-delete-op-2\", \"verb\": \"sync\", \"strategy\": \"rebase\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \"phase\": \"relock\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-05-27T10:00:00Z\"}}",
        src = main.root.display(),
        tgt = ww.root.display(),
    );
    std::fs::write(ww.root.join(".rwv-op"), &op_json).unwrap();

    let out = rwv()
        .args([
            "workweave",
            PROJECT,
            "delete",
            &ww.name,
            "--discard-uncommitted",
            "--discard-unmerged-commits",
        ])
        .current_dir(&main.root)
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "delete must still refuse while the workweave is mid-op, waivers or not"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("sync in progress"),
        "refusal must name the in-flight op even under the waivers; got:\n{stderr}"
    );
    assert!(
        ww.root.exists(),
        "workweave must survive the refused delete"
    );
}

/// Deleting a parent with ONE child re-points the child to the parent's own
/// parent (here: primary, since the parent was forked from primary), and
/// prints the loud line.
#[test]
fn delete_with_one_child_adopts_to_grandparent_primary() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    // wwa forked from primary; wwb forked from wwa (stacked).
    let wwa = create_workweave(&main, "wwa", None);
    let wwb = create_workweave(&main, "wwb", Some(&wwa.root));

    // Sanity: wwb's recorded parent is wwa.
    assert_eq!(recorded_parent(&wwb.root), canon(&wwa.root));

    // Delete wwa. wwb should be adopted by wwa's parent = primary.
    let out = rwv()
        .args(["workweave", PROJECT, "delete", &wwa.name])
        .current_dir(&main.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "delete wwa failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("adopted child workweave wwb: parent now"),
        "expected loud adoption line for wwb; got:\n{stderr}"
    );

    // wwb's marker now records primary as parent.
    assert_eq!(
        recorded_parent(&wwb.root),
        canon(&main.root),
        "wwb should be adopted by primary (wwa's own parent)"
    );
    assert!(!wwa.root.exists(), "wwa should be gone");
}

/// Deleting a MIDDLE workweave in a 3-deep stack re-points the grandchild to
/// the grandparent (a workweave, NOT primary) — the grandparent-fallback path.
#[test]
fn delete_middle_adopts_to_grandparent_workweave() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    // wwa (from primary) → wwb (from wwa) → wwc (from wwb).
    let wwa = create_workweave(&main, "wwa", None);
    let wwb = create_workweave(&main, "wwb", Some(&wwa.root));
    let wwc = create_workweave(&main, "wwc", Some(&wwb.root));

    assert_eq!(recorded_parent(&wwc.root), canon(&wwb.root));

    // Delete wwb (middle). wwc should be adopted by wwb's parent = wwa (a
    // workweave, NOT primary — the grandparent path, not the primary fallback).
    let out = rwv()
        .args(["workweave", PROJECT, "delete", &wwb.name])
        .current_dir(&main.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "delete wwb failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("adopted child workweave wwc: parent now"),
        "expected adoption line for wwc; got:\n{stderr}"
    );

    assert_eq!(
        recorded_parent(&wwc.root),
        canon(&wwa.root),
        "wwc should be adopted by wwa (grandparent workweave), not primary"
    );
}

/// Deleting a parent with N children adopts every one of them and prints a
/// line per child.
#[test]
fn delete_with_n_children_adopts_all() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let wwa = create_workweave(&main, "wwa", None);
    let c1 = create_workweave(&main, "child1", Some(&wwa.root));
    let c2 = create_workweave(&main, "child2", Some(&wwa.root));
    let c3 = create_workweave(&main, "child3", Some(&wwa.root));

    let out = rwv()
        .args(["workweave", PROJECT, "delete", &wwa.name])
        .current_dir(&main.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "delete wwa failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for c in ["child1", "child2", "child3"] {
        assert!(
            stderr.contains(&format!("adopted child workweave {c}: parent now")),
            "expected adoption line for {c}; got:\n{stderr}"
        );
    }
    for c in [&c1, &c2, &c3] {
        assert_eq!(
            recorded_parent(&c.root),
            canon(&main.root),
            "{} should be adopted by primary",
            c.name
        );
    }
}

/// Retire (`sync-to --retire`) adopts children the same way delete does: the
/// shared child-enumeration + adopt step.
#[test]
fn retire_adopts_children() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    // wwa from primary; wwb from wwa. wwa makes no unique commits, so a bare
    // sync-to --retire lands nothing and cleanly retires.
    let wwa = create_workweave(&main, "wwa", None);
    let wwb = create_workweave(&main, "wwb", Some(&wwa.root));

    // Retire wwa to primary (bare sync-to reads parent = primary).
    let out = rwv()
        .args(["sync-to", "--retire"])
        .current_dir(&wwa.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "sync-to --retire failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("adopted child workweave wwb: parent now"),
        "retire should adopt wwb; got:\n{stderr}"
    );
    assert_eq!(
        recorded_parent(&wwb.root),
        canon(&main.root),
        "wwb should be adopted by primary after wwa retire"
    );
    assert!(!wwa.root.exists(), "wwa retired");
}

// ===========================================================================
// 2. dangling-parent doctor --fix
// ===========================================================================

/// A workweave whose parent dir was removed out-of-band is a dangling-parent;
/// `rwv doctor --fix` re-points the marker to primary.
#[test]
fn doctor_fix_repoints_dangling_parent_to_primary() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let wwa = create_workweave(&main, "wwa", None);
    let wwb = create_workweave(&main, "wwb", Some(&wwa.root));

    // Simulate an out-of-band parent loss: blow away wwa's directory WITHOUT
    // running the adopting delete. wwb is now dangling. Capture wwa's canonical
    // path BEFORE removal: the marker stored the canonicalized form at create
    // time, but `canon()` on a since-deleted path falls back to the raw form,
    // which differs on macOS (TMPDIR `/var/...` symlinks to `/private/var/...`).
    let wwa_canonical = canon(&wwa.root);
    std::fs::remove_dir_all(&wwa.root).unwrap();
    assert_eq!(recorded_parent(&wwb.root), wwa_canonical);

    // doctor (no --fix) reports the dangling parent.
    let report = rwv()
        .args(["doctor"])
        .current_dir(&main.root)
        .output()
        .unwrap();
    let report_out = String::from_utf8_lossy(&report.stdout);
    assert!(
        report_out.contains("dangling-parent") || report_out.contains("does not exist"),
        "doctor should report dangling-parent; got:\n{report_out}"
    );

    // doctor --fix re-points wwb to primary and reports [fixed].
    let fixed = rwv()
        .args(["doctor", "--fix"])
        .current_dir(&main.root)
        .output()
        .unwrap();
    let fixed_out = String::from_utf8_lossy(&fixed.stdout);
    assert!(
        fixed_out.contains("re-pointed dangling parent"),
        "doctor --fix should report re-pointing; got:\n{fixed_out}"
    );
    assert_eq!(
        recorded_parent(&wwb.root),
        canon(&main.root),
        "wwb's parent should now be primary after --fix"
    );

    // A follow-up doctor is clean of the dangling-parent violation.
    let after = rwv()
        .args(["doctor"])
        .current_dir(&main.root)
        .output()
        .unwrap();
    let after_out = String::from_utf8_lossy(&after.stdout);
    assert!(
        !after_out.contains("dangling-parent"),
        "dangling-parent should be gone after --fix; got:\n{after_out}"
    );
}

// ===========================================================================
// 3. Friendly error replaces raw canonicalize on bare sync-to
// ===========================================================================

/// A bare `rwv sync-to` from a workweave whose parent vanished must emit
/// friendly doctor-remediation text — NOT the raw `failed to canonicalize …
/// (os error 2)`.
#[test]
fn bare_sync_to_dangling_parent_is_friendly() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let wwa = create_workweave(&main, "wwa", None);
    let wwb = create_workweave(&main, "wwb", Some(&wwa.root));

    // Remove wwa out-of-band; wwb's parent is now dangling.
    std::fs::remove_dir_all(&wwa.root).unwrap();

    let out = rwv()
        .args(["sync-to"])
        .current_dir(&wwb.root)
        .output()
        .unwrap();
    assert!(!out.status.success(), "bare sync-to should refuse");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("failed to canonicalize") && !stderr.contains("os error 2"),
        "should NOT surface the raw canonicalize IO error; got:\n{stderr}"
    );
    assert!(
        stderr.contains("recorded parent workspace does not exist")
            && stderr.contains("rwv doctor --fix"),
        "should surface friendly doctor-remediation text; got:\n{stderr}"
    );
}

// ===========================================================================
// 4. status --json .parent — correct for a STACKED parent
// ===========================================================================

/// `rwv status --json` `.parent` records the marker path (a STACKED parent —
/// a workweave, not primary) and a resolvable per-repo parent tip.
#[test]
fn status_json_parent_correct_for_stacked_parent() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let wwa = create_workweave(&main, "wwa", None);
    let wwb = create_workweave(&main, "wwb", Some(&wwa.root));

    let out = rwv()
        .args(["status", "--json"])
        .current_dir(&wwb.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "status --json failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let repos = v["repos"].as_array().expect("repos array");
    let lib = repos
        .iter()
        .find(|r| r["path"] == MANIFEST_REPO_PATH)
        .expect("lib repo entry");

    let parent = &lib["parent"];
    assert!(!parent.is_null(), "parent should be present in a workweave");

    // The recorded path is the STACKED parent (wwa), NOT primary.
    let parent_path = parent["path"].as_str().unwrap();
    assert_eq!(
        canon(Path::new(parent_path)),
        canon(&wwa.root),
        "parent path should be the stacked parent wwa, not primary or a reconstructed branch"
    );

    // The parent tip resolves to wwa's lib HEAD.
    let expected_tip = git_out(&["rev-parse", "HEAD"], &wwa.manifest_repo);
    assert_eq!(
        parent["tip"].as_str().unwrap(),
        expected_tip,
        "parent tip should be wwa's lib HEAD"
    );
}

// ===========================================================================
// 5. workweave log / diff — parent advanced after fork (no phantom reversals)
// ===========================================================================

/// `rwv workweave log` lists only the workweave's UNIQUE commits vs the parent,
/// and stays correct when the parent ADVANCED after the fork. `diff` uses
/// merge-base so an advanced-parent's other changes never appear as reversals.
#[test]
fn workweave_log_and_diff_correct_when_parent_advanced() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    // Fork wwb from a first-level workweave wwa (STACKED parent).
    let wwa = create_workweave(&main, "wwa", None);
    let wwb = create_workweave(&main, "wwb", Some(&wwa.root));

    // wwb makes a unique commit.
    commit_file(
        &wwb.manifest_repo,
        "wwb.txt",
        "wwb work\n",
        "wwb: unique commit",
    );

    // The PARENT (wwa) ADVANCES after the fork with an unrelated commit.
    commit_file(
        &wwa.manifest_repo,
        "wwa_advance.txt",
        "wwa later\n",
        "wwa: advance after fork",
    );

    // `rwv workweave log` from wwb: unique commits are wwb's only. wwa's
    // post-fork advance must NOT appear (it's not reachable from wwb's HEAD),
    // and neither should it cause wwb's commit to be hidden.
    let out = rwv()
        .args(["workweave", PROJECT, "log", "--json"])
        .current_dir(&wwb.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "workweave log failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["diff"], false);
    let repos = v["repos"].as_array().unwrap();
    let lib = repos
        .iter()
        .find(|r| r["path"] == MANIFEST_REPO_PATH)
        .expect("lib repo");
    let commits = lib["unique_commits"].as_array().unwrap();
    let subjects: Vec<&str> = commits
        .iter()
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert!(
        subjects.iter().any(|s| s.contains("wwb: unique commit")),
        "wwb's unique commit should be listed; got: {subjects:?}"
    );
    assert!(
        !subjects
            .iter()
            .any(|s| s.contains("wwa: advance after fork")),
        "parent's post-fork advance must NOT appear as a unique commit; got: {subjects:?}"
    );

    // `rwv workweave log --diff --json`: the diff base is the common ancestor
    // of the parent tip and HEAD, NOT the parent tip. So the diff shows ONLY
    // wwb's change and never a phantom reversal of wwa_advance.txt (which wwb's
    // HEAD doesn't have).
    let dout = rwv()
        .args(["workweave", PROJECT, "log", "--diff", "--json"])
        .current_dir(&wwb.root)
        .output()
        .unwrap();
    assert!(
        dout.status.success(),
        "workweave log --diff failed:\n{}",
        String::from_utf8_lossy(&dout.stderr)
    );
    let dv: serde_json::Value = serde_json::from_slice(&dout.stdout).unwrap();
    assert_eq!(dv["diff"], true);
    let dlib = dv["repos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"] == MANIFEST_REPO_PATH)
        .expect("lib repo in diff");
    let diff_text = dlib["diff"].as_str().unwrap_or("");
    assert!(
        diff_text.contains("wwb.txt"),
        "diff should include wwb's change; got:\n{diff_text}"
    );
    // Phantom-reversal guard: the diff must NOT show wwa_advance.txt being
    // removed (which is exactly what diffing against the advanced parent tip
    // instead of the merge-base would produce).
    assert!(
        !diff_text.contains("wwa_advance.txt"),
        "diff must not show a phantom reversal of the parent's post-fork commit; got:\n{diff_text}"
    );

    // diff_base must be the merge-base, i.e. wwa's lib tip AT FORK TIME (before
    // the advance), which equals the primary lib HEAD wwa forked from.
    let diff_base = dlib["diff_base"].as_str().unwrap();
    let fork_point = git_out(&["rev-parse", "HEAD"], &main.manifest_repo);
    assert_eq!(
        diff_base, fork_point,
        "diff base should be the merge-base (fork point), not the advanced parent tip"
    );
}

/// `rwv workweave log` text output lists unique commits and names the parent.
#[test]
fn workweave_log_text_output() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let ww = create_workweave(&main, "feat", None);
    commit_file(&ww.manifest_repo, "f.txt", "x\n", "feat: my change");

    let out = rwv()
        .args(["workweave", PROJECT, "log"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "workweave log failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("feat: my change"),
        "log text should list the unique commit; got:\n{stdout}"
    );
    assert!(
        stdout.contains("vs parent"),
        "log text should name the parent relation; got:\n{stdout}"
    );
}

/// `rwv workweave log` from the primary weave refuses (it's not a workweave).
#[test]
fn workweave_log_refuses_in_primary_weave() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let out = rwv()
        .args(["workweave", PROJECT, "log"])
        .current_dir(&main.root)
        .output()
        .unwrap();
    assert!(!out.status.success(), "should refuse from primary weave");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a workweave") || stderr.contains("primary weave"),
        "should explain CWD is not a workweave; got:\n{stderr}"
    );
}

// ===========================================================================
// 6. workweave log — project repo included
// ===========================================================================

/// `rwv workweave log --json` includes a `project_repo` field whose commits
/// reflect real per-workweave project-repo work (e.g. a doc or lock commit).
#[test]
fn workweave_log_json_includes_project_repo_unique_commits() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let ww = create_workweave(&main, "feat", None);
    // Commit something in the workweave's project repo.
    let ww_project = ww.root.join("projects").join(PROJECT);
    commit_file(&ww_project, "notes.md", "work notes\n", "docs: add notes");

    let out = rwv()
        .args(["workweave", PROJECT, "log", "--json"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "workweave log failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    // project_repo field must be present and carry the unique commit.
    let pr = &v["project_repo"];
    assert!(
        !pr.is_null(),
        "project_repo field must be present in JSON output"
    );
    assert_eq!(
        pr["path"].as_str().unwrap(),
        "(project)",
        "project_repo.path must be the '(project)' sentinel"
    );
    let commits = pr["unique_commits"].as_array().unwrap();
    let subjects: Vec<&str> = commits
        .iter()
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert!(
        subjects.iter().any(|s| s.contains("docs: add notes")),
        "project repo unique commit must be listed; got: {subjects:?}"
    );
}

/// `rwv workweave log` text output includes an `=== (project) ===` section.
#[test]
fn workweave_log_text_includes_project_repo_section() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let ww = create_workweave(&main, "feat2", None);
    // Commit something in the workweave's project repo.
    let ww_project = ww.root.join("projects").join(PROJECT);
    commit_file(
        &ww_project,
        "notes.md",
        "work notes\n",
        "docs: project notes",
    );

    let out = rwv()
        .args(["workweave", PROJECT, "log"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "workweave log failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("=== (project) ==="),
        "text output must include a (project) section; got:\n{stdout}"
    );
    assert!(
        stdout.contains("docs: project notes"),
        "text output must list the project repo commit; got:\n{stdout}"
    );
}

/// `rwv workweave log --json` has `project_repo` with empty `unique_commits`
/// when the project repo has NO commits unique vs the parent.
#[test]
fn workweave_log_json_project_repo_no_unique_commits_when_clean() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let ww = create_workweave(&main, "clean", None);
    // No project-repo commits in the workweave.

    let out = rwv()
        .args(["workweave", PROJECT, "log", "--json"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "workweave log failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();

    let pr = &v["project_repo"];
    assert!(!pr.is_null(), "project_repo must always be present");
    let commits = pr["unique_commits"].as_array().unwrap();
    assert!(
        commits.is_empty(),
        "project_repo unique_commits must be empty when there are no unique commits; got: {commits:?}"
    );
}

/// `rwv workweave log` text output shows `(no unique commits vs parent)` for
/// the `(project)` section when the project repo has no unique commits.
#[test]
fn workweave_log_text_project_repo_no_unique_commits_label() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let ww = create_workweave(&main, "clean2", None);
    // No project-repo commits in the workweave.

    let out = rwv()
        .args(["workweave", PROJECT, "log"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "workweave log failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The (project) section must appear and report "no unique commits".
    let project_section_pos = stdout
        .find("=== (project) ===")
        .expect("(project) section must appear in text output");
    let after = &stdout[project_section_pos..];
    assert!(
        after.contains("no unique commits vs parent"),
        "project section must say '(no unique commits vs parent)' when clean; got:\n{after}"
    );
}

/// `rwv workweave log --diff --json` includes a `project_repo` with diff
/// content when the project repo has unique work.
#[test]
fn workweave_log_diff_json_includes_project_repo() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let ww = create_workweave(&main, "difftest", None);
    let ww_project = ww.root.join("projects").join(PROJECT);
    commit_file(
        &ww_project,
        "feature.md",
        "feature description\n",
        "docs: feature",
    );

    let out = rwv()
        .args(["workweave", PROJECT, "log", "--diff", "--json"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "workweave log --diff failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let pr = &v["project_repo"];
    assert!(!pr.is_null(), "project_repo must be present in diff output");
    let diff_text = pr["diff"].as_str().unwrap_or("");
    assert!(
        diff_text.contains("feature.md") || diff_text.contains("feature description"),
        "project_repo diff must include the unique change; got:\n{diff_text}"
    );
}

// ===========================================================================
// 6. sync / sync-to use the recorded parent, not primary — stacked case
// ===========================================================================

/// Bare `rwv sync-to` (no target) from a workweave forked from ANOTHER
/// workweave lands upward onto that STACKED parent, not primary. This is
/// the case that distinguishes the marker's `parent` field from `primary` —
/// a resolver that fell back to primary would pass the primary-forked
/// equivalent of this test while landing the work in the wrong place here.
#[test]
fn bare_sync_to_follows_recorded_stacked_parent() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let wwa = create_workweave(&main, "wwa", None);
    let wwb = create_workweave(&main, "wwb", Some(&wwa.root));

    let wwb_sha = commit_file(
        &wwb.manifest_repo,
        "wwb.txt",
        "from wwb\n",
        "wwb: add wwb.txt",
    );
    rwv_lock_commit(&wwb.root);

    rwv()
        .args(["sync-to", "--strategy=ff"])
        .current_dir(&wwb.root)
        .assert()
        .success();

    let wwa_lib_head = git_out(&["rev-parse", "HEAD"], &wwa.manifest_repo);
    assert_eq!(
        wwa_lib_head, wwb_sha,
        "bare sync-to must land on the recorded parent wwa, not primary"
    );
    let primary_lib_head = git_out(&["rev-parse", "HEAD"], &main.manifest_repo);
    assert_ne!(
        primary_lib_head, wwb_sha,
        "primary must not advance — bare sync-to targets only the immediate recorded parent"
    );
}

/// `rwv sync <source>` from a workweave whose recorded parent is ANOTHER
/// workweave (not primary) must NOT warn about crossing siblings when the
/// explicit source IS that recorded parent — the case that would misfire if
/// the sibling-sync check compared the source against primary instead of the
/// marker's stacked parent.
#[test]
fn sibling_sync_no_warning_when_source_is_recorded_stacked_parent() {
    let tmp = common::tempdir().unwrap();
    let main = make_main_workspace(tmp.path());

    let wwa = create_workweave(&main, "wwa", None);
    let wwb = create_workweave(&main, "wwb", Some(&wwa.root));

    let output = rwv()
        .args(["sync", &wwa.root.to_string_lossy()])
        .current_dir(&wwb.root)
        .output()
        .expect("rwv sync should run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("siblings") && !stderr.contains("skips the recorded parent"),
        "syncing from the recorded (stacked) parent must not warn about crossing \
         siblings; got: {stderr}"
    );
    assert!(
        output.status.success(),
        "sync from the recorded parent should succeed; stderr: {stderr}"
    );
}
