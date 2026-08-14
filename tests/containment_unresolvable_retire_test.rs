//! E2E coverage for `Containment::observe`'s unresolvable-pair degrade at
//! its realistic call site: `retire_workweave_after_sync_to`'s per-repo
//! divergence report, when CWD and target hold no shared object store for
//! the manifest repo being compared.
//!
//! `docs/explanation/joints/clone-topology.md`'s I1 requires one canonical
//! store per manifest repo, worktree-linked into every checkout; its case
//! study is exactly this fixture's shape, built directly rather than by
//! corrupting a worktree-linked pair after the fact: the same `repo_path`
//! slot backed by two independent object DAGs. `Containment::observe` must
//! decline rather than guess when it hits that pair.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

mod common;

const SERVER_PATH: &str = "github/example/server";
const SERVER_URL: &str = "https://github.com/example/server";

fn rwv() -> AssertCommand {
    common::rwv()
}

fn git(args: &[&str], dir: &Path) {
    let out = common::git()
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git command failed");
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
        .expect("git command failed");
    assert!(
        out.status.success(),
        "git {:?} failed in {}:\n{}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-q", "-b", "main"], path);
    git(&["config", "user.email", "t@example.com"], path);
    git(&["config", "user.name", "Test"], path);
    git(&["config", "commit.gpgsign", "false"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "README.md"], path);
    git(&["commit", "-m", "init"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

/// A repo with its own unrelated history. Distinguished from `init_repo`'s
/// output by tree content rather than by commit timing, so two calls inside
/// the same wall-clock second can't hash to the same commit and silently
/// turn "independent clone" into "same tip by coincidence".
fn init_independent_repo(path: &Path, marker: &str) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-q", "-b", "main"], path);
    git(&["config", "user.email", "t@example.com"], path);
    git(&["config", "user.name", "Test"], path);
    git(&["config", "commit.gpgsign", "false"], path);
    std::fs::write(path.join("README.md"), format!("{marker}\n")).unwrap();
    git(&["add", "README.md"], path);
    let msg = format!("init: {marker}");
    git(&["commit", "-m", &msg], path);
    git_out(&["rev-parse", "HEAD"], path)
}

fn write_manifest(project_dir: &Path) {
    let body = format!(
        "[repositories.\"{SERVER_PATH}\"]\ntype = \"git\"\nurl = \"{SERVER_URL}\"\nversion = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_dir.join("rwv.toml"), body).unwrap();
}

fn write_lock(project_dir: &Path, sha: &str) {
    let raw = format!(
        "{{\"repositories\": {{{SERVER_PATH:?}: {{\"type\": \"git\", \"url\": {SERVER_URL:?}, \"version\": {sha:?}}}}}}}"
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
}

fn make_locked_workspace(parent: &Path, name: &str) -> (Workspace, String) {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/example")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server_dir = root.join(SERVER_PATH);
    let sha = init_repo(&server_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    std::fs::write(
        project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    write_manifest(&project_dir);
    write_lock(&project_dir, &sha);
    git(
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
        &project_dir,
    );
    git(&["commit", "-m", "lock: initial"], &project_dir);
    std::fs::write(root.join(".rwv-active"), "web-app\n").unwrap();

    (
        Workspace {
            root,
            project_dir,
            server_dir,
        },
        sha,
    )
}

fn write_workweave_marker(workweave_dir: &Path, primary_root: &Path) {
    let content = common::workweave_marker(primary_root, "web-app", primary_root);
    std::fs::write(workweave_dir.join(".rwv-workweave"), content).unwrap();
}

/// Build primary plus a workweave whose PROJECT repo is worktree-linked (the
/// normal topology) but whose SERVER repo is a second, independently
/// `git init`'d history: same `repo_path` slot, no fetch relationship, no
/// shared object DAG between `primary.server_dir` and `ww.server_dir`.
fn make_disconnected_retire_workspaces(parent: &Path) -> (Workspace, Workspace) {
    let (primary, _initial_sha) = make_locked_workspace(parent, "primary");

    let ww_root = parent.join(".workweaves/web-app--ww");
    std::fs::create_dir_all(ww_root.join("github/example")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let ww_project = ww_root.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "web-app--ww",
        ],
        &primary.project_dir,
    );

    let ww_server = ww_root.join(SERVER_PATH);
    init_independent_repo(&ww_server, "ww-independent-clone");

    write_workweave_marker(&ww_root, &primary.root);

    let ww = Workspace {
        root: ww_root,
        project_dir: ww_project,
        server_dir: ww_server,
    };
    (primary, ww)
}

fn write_sync_to_retire_record(owner: &Path, source: &Path, target: &Path, id: &str) {
    let body = format!(
        "{{\"id\": \"{id}\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": true, \"phase\": \"retire\", \
         \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \"overrides\": [], \
         \"started_at\": \"2026-06-10T00:00:00Z\"}}",
        src = common::json_escaped(source),
        tgt = common::json_escaped(target),
    );
    std::fs::write(owner.join(".rwv-op"), body).unwrap();
}

fn write_lease_with_id(workspace: &Path, owner: &Path, id: &str) {
    let body = format!(
        "{{\"id\": \"{id}\", \"owner\": \"{owner}\", \"created_at\": \"2026-06-10T00:00:00Z\"}}",
        owner = common::json_escaped(owner),
    );
    std::fs::write(workspace.join(".rwv-op-lease"), body).unwrap();
}

fn create_savepoint(repo: &Path, op_id: &str) {
    let head = git_out(&["rev-parse", "HEAD"], repo);
    git(
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), &head],
        repo,
    );
}

fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// The refusal fires, and its per-repo line for the unresolvable server repo
/// carries no containment-verdict clause: `Containment::observe` returning
/// `None` there, not a guessed `Equal` or `Ahead`, is what this test pins.
#[test]
fn retire_names_repos_differ_with_no_verdict_when_the_pair_is_unresolvable() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_disconnected_retire_workspaces(tmp.path());

    let ww_server_tip = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    let primary_server_tip = git_out(&["rev-parse", "HEAD"], &primary.server_dir);
    assert_ne!(
        ww_server_tip, primary_server_tip,
        "fixture sanity: the two independent inits must not collide"
    );

    // The exact query the retire comparison runs -- `rev-list --count
    // target..cwd` inside the CWD (ww) repo -- must itself fail here, or
    // this fixture exercises a resolvable pair rather than the degrade
    // path this test exists to pin.
    let probe = common::git()
        .args([
            "rev-list",
            "--count",
            &format!("{primary_server_tip}..{ww_server_tip}"),
        ])
        .current_dir(&ww.server_dir)
        .output()
        .expect("git rev-list probe failed to run");
    assert!(
        !probe.status.success(),
        "fixture sanity: primary's server tip must be unresolvable inside ww's disconnected \
         server repo; rev-list stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&probe.stdout),
        String::from_utf8_lossy(&probe.stderr),
    );

    let op_id = "retire-unresolvable-pair";
    write_sync_to_retire_record(&ww.root, &ww.root, &primary.root, op_id);
    write_lease_with_id(&primary.root, &ww.root, op_id);

    // CWD-side savepoints use op_id; target-side savepoints use
    // "<op_id>-target" (see target_savepoint_id in sync.rs).
    let target_op_id = format!("{op_id}-target");
    create_savepoint(&ww.project_dir, op_id);
    create_savepoint(&ww.server_dir, op_id);
    create_savepoint(&primary.project_dir, &target_op_id);
    create_savepoint(&primary.server_dir, &target_op_id);

    let out = rwv()
        .args(["sync-to", "--continue"])
        .current_dir(&ww.root)
        .output()
        .expect("rwv command failed to run");
    assert!(
        !out.status.success(),
        "retire must refuse when a manifest repo's pair cannot be resolved; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--retire: workweave's manifest repos differ from target after sync-to"),
        "must name itself as the retire divergence refusal; stderr:\n{stderr}"
    );

    let want_line = format!(
        "{SERVER_PATH}: CWD={} target={}",
        short_sha(&ww_server_tip),
        short_sha(&primary_server_tip),
    );
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with(&format!("{SERVER_PATH}:")))
        .unwrap_or_else(|| {
            panic!("expected a divergence line for {SERVER_PATH}; stderr:\n{stderr}")
        });
    assert_eq!(
        line, want_line,
        "an unresolvable pair must render with no containment-verdict suffix; got: {line:?}"
    );

    assert!(
        ww.root.exists(),
        "a refused retire must leave the workweave in place"
    );
}
