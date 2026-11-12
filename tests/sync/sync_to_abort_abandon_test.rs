//! E2E acceptance tests for `rwv abort --abandon-foreign-tip` on the
//! land TWO-WORKSPACE path.
//!
//! `abort_hardening_test.rs` pins the flag's semantics for a sync-shaped op
//! in one workspace. A land op adds a second workspace whose repos are
//! worktree pairs of the first, sharing one refdb, and a consent given for a
//! repo covers BOTH copies of that repo (`AbandonForeignTipConsent::covers`).
//! The shared refdb is exactly where a misrouted backup would let one side's
//! abandoned tip stand in for the other's, so every test here asserts that
//! each side's backup is named for its own branch and holds its own tip.
//!
//! Fixture shape mirrors `phase_reentry_test.rs` / `crash_matrix_test.rs`:
//! the op-state file, lease, and savepoints are planted by hand so the unit
//! under test is the abort path itself.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
use std::path::{Path, PathBuf};

use crate::common;

const SERVER_PATH: &str = "github/example/server";
const SERVER_URL: &str = "https://github.com/example/server";

fn rwv() -> AssertCommand {
    common::rwv()
}

fn try_git(args: &[&str], dir: &Path) -> bool {
    common::git()
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-q", "-b", "main"]);
    common::git_in(path, &["config", "user.email", "t@example.com"]);
    common::git_in(path, &["config", "user.name", "Test"]);
    common::git_in(path, &["config", "commit.gpgsign", "false"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "README.md"]);
    common::git_in(path, &["commit", "-m", "init"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn write_manifest(project_dir: &Path) {
    let body = format!(
        "[repositories.\"{SERVER_PATH}\"]\ntype = \"git\"\nurl = \"{SERVER_URL}\"\nbranch = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_dir.join("rwv.toml"), body).unwrap();
}

fn write_lock(project_dir: &Path, sha: &str) {
    common::fixture_lock(project_dir, &[(SERVER_PATH, SERVER_URL, sha)]);
}

fn make_locked_workspace(parent: &Path, name: &str) -> (Workspace, String) {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/example")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server_dir = root.join(SERVER_PATH);
    let sha = init_repo(&server_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    common::set_up_lock_driver(&project_dir);
    write_manifest(&project_dir);
    write_lock(&project_dir, &sha);
    common::git_in(
        &project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&project_dir, &["commit", "-m", "lock: initial"]);
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

/// primary (the land TARGET) plus ww (the owner/source), with ww's repos
/// worktree-linked to primary's — one refdb per repo pair, which is the
/// state the `<op-id>-target` namespace exists for.
fn make_shared_workspaces(parent: &Path) -> (Workspace, Workspace) {
    let (primary, _c1) = make_locked_workspace(parent, "primary");
    let ww_root = parent.join("ww");
    std::fs::create_dir_all(ww_root.join("github/example")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let ww_server = ww_root.join(SERVER_PATH);
    common::git_in(
        &primary.server_dir,
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/main",
        ],
    );

    let ww_project = ww_root.join("projects/web-app");
    common::git_in(
        &primary.project_dir,
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );
    std::fs::write(ww_root.join(".rwv-active"), "web-app\n").unwrap();

    let ww = Workspace {
        root: ww_root,
        project_dir: ww_project,
        server_dir: ww_server,
    };
    (primary, ww)
}

/// Owner record for a land op stopped in advance-target after it
/// fast-forwarded the target's `server` to `landed_server` and its project
/// repo to `landed_project`: both journaled as fast-forwarded, both tips its
/// converged tips.
fn plant_sync_to_owner_record(
    owner: &Path,
    target: &Path,
    op_id: &str,
    landed_server: &str,
    landed_project: &str,
) {
    let mut plan: repoweave::op_state::TakeInRecord =
        serde_json::from_str(&common::land_plan_json(owner, target, "web-app")).unwrap();
    plan.moved_in_target = vec![
        repoweave::manifest::RepoKey::Member(
            repoweave::manifest::RepoPath::new(SERVER_PATH).unwrap(),
        ),
        repoweave::manifest::RepoKey::Project,
    ];
    let body = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"land\", \"history\": null, \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \
         \"phase\": \"advance-target\", \"advanced_tips\": {{}}, \
         \"converged_tips\": {{\"{SERVER_PATH}\": \"{landed_server}\", \"(project)\": \"{landed_project}\"}}, \
         \"overrides\": [], \"started_at\": \"2026-06-10T00:00:00Z\", \"schema\": 6, \"release\": \"0.0.0\", \"take_in\": {plan}}}",
        src = common::json_escaped(owner),
        tgt = common::json_escaped(target),
        plan = serde_json::to_string(&plan).unwrap(),
    );
    std::fs::write(owner.join(".rwv-op"), body).unwrap();
}

fn plant_lease(workspace: &Path, owner: &Path, op_id: &str) {
    let body = format!(
        "{{\"id\": \"{op_id}\", \"owner\": \"{owner}\", \"created_at\": \"2026-06-10T00:00:00Z\"}}",
        owner = common::json_escaped(owner),
    );
    std::fs::write(workspace.join(".rwv-op-lease"), body).unwrap();
}

fn plant_savepoint(repo: &Path, restore_id: &str) -> String {
    common::savepoint::plant(repo, restore_id);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// The target side's restore id: savepoints for repos in the op's target
/// workspace are keyed `<op-id>-target` so worktree pairs sharing one refdb
/// do not collide on ref names.
fn target_restore_id(op_id: &str) -> String {
    format!("{op_id}-target")
}

const BACKUP: &str = "refs/rwv/backup/";
const PRE_ABORT: &str = "refs/rwv/pre-abort/";

/// Every ref under `prefix` in `repo`, as `(refname, sha)`.
fn refs_under(repo: &Path, prefix: &str) -> Vec<(String, String)> {
    let out = common::git_in(
        repo,
        &["for-each-ref", "--format=%(refname) %(objectname)", prefix],
    );
    out.lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(r, o)| (r.to_owned(), o.to_owned()))
        .collect()
}

/// The backup under `branch` holding `tip`, if one is there.
fn backup_of(repo: &Path, branch: &str, tip: &str) -> Option<String> {
    refs_under(repo, &format!("{BACKUP}{branch}/"))
        .into_iter()
        .find(|(_, sha)| sha == tip)
        .map(|(name, _)| name)
}

/// Plant the state of a land stopped after it fast-forwarded the target:
/// owner record at ww, lease at primary, savepoints on both sides under
/// their side-specific restore ids, and one commit on the target's server
/// and project repo standing for what the landing moved them to, so a
/// commit on top of either is a tip the landing moved and someone moved
/// again. The two sides' server branches are first advanced to
/// DISTINCT commits so their savepoints are distinguishable SHAs — a
/// restore that lands on the other side's savepoint fails the assertion
/// rather than passing by coincidence of shared history.
/// Returns (ww server savepoint, primary server savepoint).
fn plant_crashed_sync_to(primary: &Workspace, ww: &Workspace, op_id: &str) -> (String, String) {
    make_commit(&ww.server_dir, "owner-base.txt", "owner\n", "base: owner");
    make_commit(
        &primary.server_dir,
        "target-base.txt",
        "target\n",
        "base: target",
    );
    plant_lease(&primary.root, &ww.root, op_id);
    let ww_server_sp = plant_savepoint(&ww.server_dir, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    let tid = target_restore_id(op_id);
    let primary_server_sp = plant_savepoint(&primary.server_dir, &tid);
    plant_savepoint(&primary.project_dir, &tid);
    let landed_server = make_commit(&primary.server_dir, "landed.txt", "landed\n", "landed");
    let landed_project = make_commit(&primary.project_dir, "landed.txt", "landed\n", "landed");
    plant_sync_to_owner_record(
        &ww.root,
        &primary.root,
        op_id,
        &landed_server,
        &landed_project,
    );
    assert_ne!(
        ww_server_sp, primary_server_sp,
        "fixture invariant: the two sides' savepoints must be distinct commits"
    );
    (ww_server_sp, primary_server_sp)
}

// ---------------------------------------------------------------------------
// Refusal without the flag, and rail 1 on the target side
// ---------------------------------------------------------------------------

/// A foreign tip on the TARGET workspace's copy refuses without the flag,
/// exactly as the one-workspace shape does, and the refusal writes nothing.
#[test]
fn sync_to_abort_refuses_target_side_foreign_tip_without_the_flag() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_workspaces(tmp.path());
    let op_id = "land-abandon-refuse";

    plant_crashed_sync_to(&primary, &ww, op_id);
    let foreign_tip = make_commit(&primary.server_dir, "f.txt", "f\n", "foreign: on target");

    let output = rwv()
        .arg("abort")
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains(&format!("[target] {SERVER_PATH}: foreign-tip violation")),
        "the refusal must name the target-side copy.\nstderr:\n{stderr}"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        foreign_tip,
        "a refusal must not move the target-side branch"
    );
    assert_eq!(refs_under(&primary.server_dir, BACKUP), vec![]);
    assert_eq!(refs_under(&primary.server_dir, PRE_ABORT), vec![]);
    assert!(
        ww.root.join(".rwv-op").exists(),
        "owner record must be retained on a foreign-tip refusal"
    );
    assert!(
        primary.root.join(".rwv-op-lease").exists(),
        "target lease must be retained on a foreign-tip refusal"
    );
}

// ---------------------------------------------------------------------------
// Consent covers both copies, each under its own side's reference
// ---------------------------------------------------------------------------

/// One consent, foreign tips on BOTH copies of the named repo: both are
/// abandoned, and each side's abandoned tip is kept as a backup named for that
/// side's own branch — the owner's `ww/main`, the target's `main` — in the one
/// shared refdb. If the sides were confused, one side's branch would be moved
/// off a tip no backup of its own held.
#[test]
fn sync_to_abandon_covers_both_copies_with_a_backup_each() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_workspaces(tmp.path());
    let op_id = "land-abandon-both";

    let (ww_server_sp, primary_server_sp) = plant_crashed_sync_to(&primary, &ww, op_id);
    let src_foreign = make_commit(&ww.server_dir, "src.txt", "src\n", "foreign: on owner");
    let tgt_foreign = make_commit(
        &primary.server_dir,
        "tgt.txt",
        "tgt\n",
        "foreign: on target",
    );
    assert_ne!(src_foreign, tgt_foreign, "fixture: copies diverge");

    let output = rwv()
        .arg("abort")
        .arg(format!("--abandon-foreign-tip={SERVER_PATH}"))
        .current_dir(&ww.root)
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains(&format!("{SERVER_PATH}: restored (abandoned foreign tip")),
        "the owner-side copy must be abandoned.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "[target] {SERVER_PATH}: restored (abandoned foreign tip"
        )),
        "the target-side copy must be abandoned.\nstdout:\n{stdout}"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_sp,
        "owner-side branch must be restored to the owner-side savepoint"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        primary_server_sp,
        "target-side branch must be restored to the target-side savepoint"
    );
    let owner_backup = backup_of(&ww.server_dir, "ww/main", &src_foreign)
        .expect("the owner-side tip is kept under the owner's branch");
    let target_backup = backup_of(&primary.server_dir, "main", &tgt_foreign)
        .expect("the target-side tip is kept under the target's branch");
    assert_ne!(owner_backup, target_backup);
    assert_eq!(
        refs_under(&primary.server_dir, BACKUP).len(),
        2,
        "one backup per side, and nothing else"
    );
    assert_eq!(refs_under(&primary.server_dir, PRE_ABORT), vec![]);
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "owner record must be cleared by a clean abort"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "target lease must be cleared by a clean abort"
    );
}

// ---------------------------------------------------------------------------
// Consent does not reach an unnamed repo on the target side
// ---------------------------------------------------------------------------

/// Consent for one repo abandons that repo's target-side copy and does
/// nothing for a different repo in the same target workspace: the unnamed
/// repo's refusal stands and the abort still fails overall.
#[test]
fn sync_to_abandon_consent_does_not_reach_an_unnamed_repo_on_the_target_side() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_workspaces(tmp.path());
    let op_id = "land-abandon-scope";

    let (_, primary_server_sp) = plant_crashed_sync_to(&primary, &ww, op_id);
    make_commit(&primary.server_dir, "s.txt", "s\n", "foreign: server");
    let project_foreign = make_commit(&primary.project_dir, "p.txt", "p\n", "foreign: project");

    let output = rwv()
        .arg("abort")
        .arg(format!("--abandon-foreign-tip={SERVER_PATH}"))
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains(&format!(
            "[target] {SERVER_PATH}: restored (abandoned foreign tip"
        )),
        "the named repo's target-side copy must be abandoned.\nstdout:\n{stdout}"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        primary_server_sp,
        "the named repo's target-side branch must be restored"
    );
    assert!(
        stderr.contains("[target] projects/web-app: foreign-tip violation"),
        "the unnamed repo's target-side refusal must stand.\nstderr:\n{stderr}"
    );
    assert_eq!(
        common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]),
        project_foreign,
        "consent must not move a branch it did not name"
    );
    assert!(
        ww.root.join(".rwv-op").exists(),
        "owner record must be retained while any repo refused"
    );
    assert_eq!(
        refs_under(&primary.project_dir, BACKUP),
        vec![],
        "the unnamed repo's tip was not abandoned, so nothing was kept for it"
    );
}

// ---------------------------------------------------------------------------
// A foreign agent that keeps going between a refusal and the consented rerun
// ---------------------------------------------------------------------------

/// Consent applies to the target-side tip as it stands when abort runs: after
/// a refusal the foreign agent commits on top, or resets and does something
/// else, and the rerun keeps the tip it finds as a backup under the target's
/// branch.
#[test]
fn sync_to_abandon_keeps_the_target_side_tip_as_it_stands() {
    for (n, diverge) in [false, true].into_iter().enumerate() {
        let tmp = common::tempdir().unwrap();
        let (primary, ww) = make_shared_workspaces(tmp.path());
        let op_id = format!("land-abandon-rerun-{n}");

        let (_, primary_server_sp) = plant_crashed_sync_to(&primary, &ww, &op_id);
        let first = make_commit(&primary.server_dir, "f1.txt", "f1\n", "foreign: first");

        rwv()
            .arg("abort")
            .current_dir(&ww.root)
            .assert()
            .failure()
            .stderr(predicate::str::contains("foreign-tip"));
        assert_eq!(refs_under(&primary.server_dir, BACKUP), vec![]);

        if diverge {
            common::git_in(
                &primary.server_dir,
                &["reset", "--hard", &primary_server_sp],
            );
        }
        let latest = make_commit(&primary.server_dir, "f2.txt", "f2\n", "foreign: second");
        assert_eq!(
            try_git(
                &["merge-base", "--is-ancestor", &first, &latest],
                &primary.server_dir
            ),
            !diverge,
            "fixture: the first foreign tip is an ancestor of the latest only when not diverged"
        );

        rwv()
            .arg("abort")
            .arg(format!("--abandon-foreign-tip={SERVER_PATH}"))
            .current_dir(&ww.root)
            .assert()
            .success()
            .stdout(predicate::str::contains(format!(
                "[target] {SERVER_PATH}: restored (abandoned foreign tip"
            )));

        assert_eq!(
            common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
            primary_server_sp,
            "the target-side branch must be restored to its savepoint"
        );
        assert!(
            backup_of(&primary.server_dir, "main", &latest).is_some(),
            "diverge={diverge}: the tip as it stood is kept"
        );
        assert!(
            !ww.root.join(".rwv-op").exists() && !primary.root.join(".rwv-op-lease").exists(),
            "op-state must be cleared on both sides after the clean abort"
        );
    }
}
