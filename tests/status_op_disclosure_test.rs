//! `rwv status` discloses an in-flight sync/sync-to op (D4b).
//!
//! `.rwv-op` was previously invisible to `rwv status` — a git-clean
//! worktree licensed an operator into re-running with a different flag while
//! an op sat parked, discoverable only by attempting a mutation (the
//! in-flight refusal) or by `rwv doctor`'s `StaleOpState` finding. This file
//! pins the read-only disclosure both ways: an op in progress shows, text and
//! `--json`; a workspace with no op shows nothing.
//!
//! The op here is parked by a REAL interrupted `rwv sync` — two repos
//! diverge after a fork, `--strategy ff` genuinely cannot fast-forward, and
//! the acquired op-state is kept on disk per the phase-failure cleanup rule.
//! No `.rwv-op` is hand-planted.

use std::path::{Path, PathBuf};

mod common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn canon(p: &Path) -> String {
    p.canonicalize()
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .to_string()
}

const REPO_PATH: &str = "github/org/lib";
const PROJECT: &str = "app";

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

struct PrimaryWorkspace {
    root: PathBuf,
    project_dir: PathBuf,
    repo: PathBuf,
}

struct Workweave {
    root: PathBuf,
    project_dir: PathBuf,
    repo: PathBuf,
}

fn make_primary(tmp: &Path) -> PrimaryWorkspace {
    let ws = tmp.join("ws");
    let repo = ws.join(REPO_PATH);
    let initial_sha = init_repo(&repo);

    let project_dir = ws.join("projects").join(PROJECT);
    init_repo(&project_dir);

    let manifest = format!(
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nversion = \"main\"\nrole = \"owned\"\n",
        path = REPO_PATH,
        repo = common::url_path(&repo)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();

    let repo_url = common::file_url(&repo);
    let raw_lock = format!(
        "{{\"repositories\": {{{path:?}: {{\"type\": \"git\", \"url\": {repo_url:?}, \"version\": {sha:?}}}}}}}",
        path = REPO_PATH,
        sha = initial_sha
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();

    common::git_in(&project_dir, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);

    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    PrimaryWorkspace {
        root: ws,
        project_dir,
        repo,
    }
}

fn create_workweave(primary: &PrimaryWorkspace, weaveroot: &Path, name: &str) -> Workweave {
    rwv()
        .args(["workweave", PROJECT, "create", name])
        .current_dir(&primary.root)
        .assert()
        .success();

    let root = weaveroot.join(format!("{PROJECT}--{name}"));
    Workweave {
        project_dir: root.join("projects").join(PROJECT),
        repo: root.join(REPO_PATH),
        root,
    }
}

fn rwv_lock_commit(workspace_root: &Path) {
    rwv()
        .args(["lock", "--commit"])
        .current_dir(workspace_root)
        .assert()
        .success();
}

/// `rwv status` (text + `--json`) discloses an op parked by a real
/// interrupted sync, and stays silent for a workspace that never ran one.
#[test]
fn status_discloses_a_parked_op_from_a_real_interrupted_sync() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, &weaveroot, "wa");

    // Diverge the shared repo: primary and ww each commit a distinct file
    // from the same fork point, so neither side is a fast-forward of the
    // other. `--strategy ff` (the default) then fails for a real reason.
    // Primary relocks so its own lock-freshness precondition passes; ww's
    // divergence is discovered only once replay actually attempts the ff.
    commit_file(
        &primary.repo,
        "primary_only.txt",
        "from primary\n",
        "primary: add primary_only.txt",
    );
    rwv_lock_commit(&primary.root);
    commit_file(&ww.repo, "ww_only.txt", "from ww\n", "ww: add ww_only.txt");

    let sync_out = rwv()
        .args(["sync", "primary"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(
        !sync_out.status.success(),
        "sync should fail on a genuine ff divergence; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&sync_out.stdout),
        String::from_utf8_lossy(&sync_out.stderr)
    );
    let sync_stderr = String::from_utf8_lossy(&sync_out.stderr);
    assert!(
        sync_stderr.contains("fast-forward"),
        "expected a fast-forward refusal; got:\n{sync_stderr}"
    );

    // The field corollary this disclosure exists for: git status is clean
    // in every repo even though an op is parked.
    for repo in [
        &ww.repo,
        &ww.project_dir,
        &primary.repo,
        &primary.project_dir,
    ] {
        let out = common::git()
            .args(["status", "--porcelain"])
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git status failed in {}",
            repo.display()
        );
        assert!(
            out.stdout.is_empty(),
            "git status should be clean in {} while the op is parked: {}",
            repo.display(),
            String::from_utf8_lossy(&out.stdout)
        );
    }

    // Positive: the parked workspace discloses the op.
    let text_out = rwv()
        .args(["status"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(text_out.status.success());
    let text = String::from_utf8_lossy(&text_out.stdout);
    assert!(
        text.contains("sync in progress") && text.contains("mid `replay`"),
        "status text should name the verb and phase: {text}"
    );
    assert!(
        text.contains("--continue") && text.contains("rwv abort"),
        "status text should offer both remedies: {text}"
    );

    let json_out = rwv()
        .args(["status", "--json"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(json_out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&json_out.stdout).unwrap();
    let op = v
        .get("op")
        .expect("status --json should disclose the parked op");
    assert_eq!(op["verb"], "sync");
    assert_eq!(op["phase"], "replay");
    assert_eq!(
        canon(Path::new(op["owner"].as_str().unwrap())),
        canon(&ww.root)
    );
    assert_eq!(
        canon(Path::new(op["source"].as_str().unwrap())),
        canon(&primary.root)
    );
    assert_eq!(
        canon(Path::new(op["target"].as_str().unwrap())),
        canon(&ww.root)
    );
    assert!(op["overrides"].as_array().unwrap().is_empty());
    assert!(op["id"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(op["started_at"].as_str().is_some_and(|s| !s.is_empty()));

    // Negative: primary never ran this op — status must not show one. An
    // assertion that only checks the positive passes when the surface is
    // stuck on.
    let primary_text_out = rwv()
        .args(["status"])
        .current_dir(&primary.root)
        .output()
        .unwrap();
    assert!(primary_text_out.status.success());
    let primary_text = String::from_utf8_lossy(&primary_text_out.stdout);
    assert!(
        !primary_text.contains("in progress"),
        "primary has no op; status text must not disclose one: {primary_text}"
    );

    let primary_json_out = rwv()
        .args(["status", "--json"])
        .current_dir(&primary.root)
        .output()
        .unwrap();
    assert!(primary_json_out.status.success());
    let pv: serde_json::Value = serde_json::from_slice(&primary_json_out.stdout).unwrap();
    assert!(
        pv.get("op").is_none(),
        "primary has no op; the `op` key must be absent, not null: {pv}"
    );
}
