//! `rwv status` discloses an in-flight sync/land op (D4b).
//!
//! `.rwv-op` was previously invisible to `rwv status` — a git-clean
//! worktree licensed an operator into re-running with a different flag while
//! an op sat parked, discoverable only by attempting a mutation (the
//! in-flight refusal) or by `rwv doctor`'s `StaleOpState` finding. This file
//! pins the read-only disclosure both ways: an op in progress shows, text and
//! `--json`; a workspace with no op shows nothing.
//!
//! The op here is parked by a REAL interrupted `rwv land` — two repos
//! edit one file differently after a fork, the rebase replay genuinely conflicts, and
//! the acquired op-state is kept on disk per the phase-failure cleanup rule.
//! No `.rwv-op` is hand-planted.

use std::path::{Path, PathBuf};

use crate::common;

fn rwv() -> assert_cmd::Command {
    common::rwv()
}

fn canon(p: &Path) -> String {
    repoweave::path_spelling::resolve(p)
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
        "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"file://{repo}\"\nbranch = \"main\"\nrole = \"owned\"\n",
        path = REPO_PATH,
        repo = common::url_path(&repo)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
    common::set_up_lock_driver(&project_dir);

    let repo_url = common::file_url(&repo);
    common::fixture_lock(&project_dir, &[(REPO_PATH, &repo_url, &initial_sha)]);

    common::git_in(
        &project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
    common::record_trunk(&project_dir);

    std::fs::write(ws.join(".rwv-active"), format!("{PROJECT}\n")).unwrap();

    PrimaryWorkspace {
        root: ws,
        project_dir,
        repo,
    }
}

fn create_workweave(primary: &PrimaryWorkspace, weaveroot: &Path, name: &str) -> Workweave {
    rwv()
        .args(["workweave", "create", "--project", PROJECT, name])
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
/// interrupted land, and stays silent for a workspace that never ran one.
#[test]
fn status_discloses_a_parked_op_from_a_real_interrupted_sync_to() {
    let tmp = common::tempdir().unwrap();
    let weaveroot = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&weaveroot).unwrap();

    let primary = make_primary(tmp.path());
    let ww = create_workweave(&primary, &weaveroot, "wa");
    let bystander = create_workweave(&primary, &weaveroot, "wb");

    // Primary and ww each commit a different version of the same file from
    // the same fork point, so the rebase replay stops on a conflict. Primary
    // relocks so its own lock-freshness precondition passes.
    commit_file(
        &primary.repo,
        "shared.txt",
        "from primary\n",
        "primary: add shared.txt",
    );
    rwv_lock_commit(&primary.root);
    commit_file(&ww.repo, "shared.txt", "from ww\n", "ww: add shared.txt");

    let sync_out = rwv()
        .args(["land", "--history", "linear"])
        .current_dir(&ww.root)
        .output()
        .unwrap();
    assert!(
        !sync_out.status.success(),
        "land should fail on a genuine conflict; stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&sync_out.stdout),
        String::from_utf8_lossy(&sync_out.stderr)
    );
    let sync_stderr = String::from_utf8_lossy(&sync_out.stderr);
    assert!(
        sync_stderr.contains("conflict"),
        "expected a conflict refusal; got:\n{sync_stderr}"
    );

    // Git sees only the conflicted member as dirty; the project repos and
    // the primary stay clean while the op is parked.
    for repo in [&ww.project_dir, &primary.repo, &primary.project_dir] {
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
        text.contains("land in progress") && text.contains("mid `replay`"),
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
    assert_eq!(op["verb"], "land");
    assert_eq!(op["phase"], "replay");
    assert_eq!(
        canon(Path::new(op["owner"].as_str().unwrap())),
        canon(&ww.root)
    );
    assert_eq!(
        canon(Path::new(op["source"].as_str().unwrap())),
        canon(&ww.root)
    );
    assert_eq!(
        canon(Path::new(op["target"].as_str().unwrap())),
        canon(&primary.root)
    );
    assert!(op["overrides"].as_array().unwrap().is_empty());
    assert!(op["id"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(op["started_at"].as_str().is_some_and(|s| !s.is_empty()));

    // Negative: a workweave outside the op — status must not show one. An
    // assertion that only checks the positive passes when the surface is
    // stuck on.
    let bystander_text_out = rwv()
        .args(["status"])
        .current_dir(&bystander.root)
        .output()
        .unwrap();
    assert!(bystander_text_out.status.success());
    let bystander_text = String::from_utf8_lossy(&bystander_text_out.stdout);
    assert!(
        !bystander_text.contains("in progress"),
        "the bystander has no op; status text must not disclose one: {bystander_text}"
    );

    let bystander_json_out = rwv()
        .args(["status", "--json"])
        .current_dir(&bystander.root)
        .output()
        .unwrap();
    assert!(bystander_json_out.status.success());
    let pv: serde_json::Value = serde_json::from_slice(&bystander_json_out.stdout).unwrap();
    assert!(
        pv.get("op").is_none(),
        "the bystander has no op; the `op` key must be absent, not null: {pv}"
    );
}
