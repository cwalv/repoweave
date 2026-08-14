//! E2E acceptance tests for `rwv abort --abandon-foreign-tip` on the
//! sync-to TWO-WORKSPACE path (design § 5).
//!
//! `abort_hardening_test.rs` pins the flag's semantics for a sync-shaped op
//! in one workspace. A sync-to op adds a second workspace whose repos are
//! worktree pairs of the first, sharing one refdb — so the target side's
//! savepoint and pre-abort references live under the `<op-id>-target`
//! namespace (`target_savepoint_id` / `restore_id_for` in `src/sync.rs`),
//! and a consent given for a repo covers BOTH copies of that repo
//! (`AbandonForeignTipConsent::covers`). The shared refdb is exactly where
//! a misrouted namespace would make one side's capture stand in for the
//! other side's tip, so every test here asserts against the side-specific
//! reference names, not just outcomes.
//!
//! Fixture shape mirrors `phase_reentry_test.rs` / `crash_matrix_test.rs`:
//! the op-state file, lease, and savepoints are planted by hand so the unit
//! under test is the abort path itself.

use assert_cmd::Command as AssertCommand;
use predicates::prelude::*;
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
    git(&["init", "-q", "-b", "main"], path);
    git(&["config", "user.email", "t@example.com"], path);
    git(&["config", "user.name", "Test"], path);
    git(&["config", "commit.gpgsign", "false"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "README.md"], path);
    git(&["commit", "-m", "init"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
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

/// primary (the sync-to TARGET) plus ww (the owner/source), with ww's repos
/// worktree-linked to primary's — one refdb per repo pair, which is the
/// state the `<op-id>-target` namespace exists for.
fn make_shared_workspaces(parent: &Path) -> (Workspace, Workspace) {
    let (primary, _c1) = make_locked_workspace(parent, "primary");
    let ww_root = parent.join("ww");
    std::fs::create_dir_all(ww_root.join("github/example")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    let ww_server = ww_root.join(SERVER_PATH);
    git(
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "ww/main",
        ],
        &primary.server_dir,
    );

    let ww_project = ww_root.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            &ww_project.to_string_lossy(),
            "-b",
            "ww/project",
        ],
        &primary.project_dir,
    );
    std::fs::write(ww_root.join(".rwv-active"), "web-app\n").unwrap();

    let ww = Workspace {
        root: ww_root,
        project_dir: ww_project,
        server_dir: ww_server,
    };
    (primary, ww)
}

/// Owner record for a sync-to op crashed mid-replay: both sides savepointed
/// (the savepoint phase runs before replay and covers the target for
/// sync-to), converged_tips still empty.
fn plant_sync_to_owner_record(owner: &Path, target: &Path, op_id: &str) {
    let body = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"sync-to\", \"strategy\": \"rebase\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \
         \"phase\": \"replay\", \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \
         \"overrides\": [], \"started_at\": \"2026-06-10T00:00:00Z\"}}",
        src = common::json_escaped(owner),
        tgt = common::json_escaped(target),
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
    let head = git_out(&["rev-parse", "HEAD"], repo);
    git(
        &[
            "update-ref",
            &format!("refs/rwv/pre-op/{restore_id}"),
            &head,
        ],
        repo,
    );
    head
}

/// The target side's restore id: savepoints and pre-abort refs for repos in
/// the op's target workspace are keyed `<op-id>-target` so worktree pairs
/// sharing one refdb do not collide on ref names.
fn target_restore_id(op_id: &str) -> String {
    format!("{op_id}-target")
}

fn pre_abort_ref_name(restore_id: &str) -> String {
    format!("refs/rwv/pre-abort/{restore_id}")
}

fn pre_abort_sha(repo: &Path, restore_id: &str) -> Option<String> {
    let out = common::git()
        .args(["rev-parse", "--verify", &pre_abort_ref_name(restore_id)])
        .current_dir(repo)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

/// Plant the full crashed-mid-replay sync-to state: owner record at ww,
/// lease at primary, savepoints on both sides under their side-specific
/// restore ids. The two sides' server branches are first advanced to
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
    plant_sync_to_owner_record(&ww.root, &primary.root, op_id);
    plant_lease(&primary.root, &ww.root, op_id);
    let ww_server_sp = plant_savepoint(&ww.server_dir, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    let tid = target_restore_id(op_id);
    let primary_server_sp = plant_savepoint(&primary.server_dir, &tid);
    plant_savepoint(&primary.project_dir, &tid);
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
/// exactly as the one-workspace shape does — and the pre-abort reference
/// the refusal names is the target-side one (`<op-id>-target`), written
/// despite the refusal (rail 1 runs before rail 2 on both sides).
#[test]
fn sync_to_abort_refuses_target_side_foreign_tip_without_the_flag() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_workspaces(tmp.path());
    let op_id = "sync-to-abandon-refuse";

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
    let tid = target_restore_id(op_id);
    assert!(
        stderr.contains(&pre_abort_ref_name(&tid)),
        "the refusal must name the target-side pre-abort reference.\nstderr:\n{stderr}"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
        foreign_tip,
        "a refusal must not move the target-side branch"
    );
    assert_eq!(
        pre_abort_sha(&primary.server_dir, &tid).as_deref(),
        Some(foreign_tip.as_str()),
        "rail 1 must capture the target-side tip under the -target id despite the refusal"
    );
    assert!(
        pre_abort_sha(&ww.server_dir, op_id).is_some(),
        "the owner side gets its own capture under the base op id"
    );
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
/// abandoned, and each side's abandoned tip is captured under that side's
/// reference — owner under `<op-id>`, target under `<op-id>-target` — in the
/// one shared refdb. If the namespaces were collapsed, the second side's
/// capture would be first-write-wins'd into the first side's, and the
/// abandon would move a branch off a tip its own reference never held.
#[test]
fn sync_to_abandon_covers_both_copies_under_side_specific_refs() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_workspaces(tmp.path());
    let op_id = "sync-to-abandon-both";

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
        git_out(&["rev-parse", "HEAD"], &ww.server_dir),
        ww_server_sp,
        "owner-side branch must be restored to the owner-side savepoint"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
        primary_server_sp,
        "target-side branch must be restored to the target-side savepoint"
    );
    assert_eq!(
        pre_abort_sha(&ww.server_dir, op_id).as_deref(),
        Some(src_foreign.as_str()),
        "the owner-side abandoned tip must be captured under the base op id"
    );
    assert_eq!(
        pre_abort_sha(&primary.server_dir, &target_restore_id(op_id)).as_deref(),
        Some(tgt_foreign.as_str()),
        "the target-side abandoned tip must be captured under the -target id"
    );
    for sha in [&src_foreign, &tgt_foreign] {
        assert!(
            try_git(
                &["cat-file", "-e", &format!("{sha}^{{commit}}")],
                &primary.server_dir,
            ),
            "abandoned commit {sha} must survive the abort"
        );
    }
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
    let op_id = "sync-to-abandon-scope";

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
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
        primary_server_sp,
        "the named repo's target-side branch must be restored"
    );
    assert!(
        stderr.contains("[target] (project): foreign-tip violation"),
        "the unnamed repo's target-side refusal must stand.\nstderr:\n{stderr}"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.project_dir),
        project_foreign,
        "consent must not move a branch it did not name"
    );
    assert!(
        ww.root.join(".rwv-op").exists(),
        "owner record must be retained while any repo refused"
    );
}

// ---------------------------------------------------------------------------
// Capture-advance along ancestry, target side
// ---------------------------------------------------------------------------

/// The first-write-wins-along-divergence advance works on the target side's
/// `-target` reference: refusal captures f1, the foreign agent commits f2 on
/// top, and the consented re-run advances the capture to f2 and abandons —
/// with f1 still reachable through the advanced target-side reference.
#[test]
fn sync_to_abandon_advances_target_side_capture_along_ancestry() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_workspaces(tmp.path());
    let op_id = "sync-to-abandon-advance";

    let (_, primary_server_sp) = plant_crashed_sync_to(&primary, &ww, op_id);
    let first_foreign = make_commit(&primary.server_dir, "f1.txt", "f1\n", "foreign: first");

    rwv()
        .arg("abort")
        .current_dir(&ww.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("foreign-tip"));
    let tid = target_restore_id(op_id);
    assert_eq!(
        pre_abort_sha(&primary.server_dir, &tid).as_deref(),
        Some(first_foreign.as_str()),
        "run 1 must capture the target-side foreign tip under the -target id"
    );

    let second_foreign = make_commit(&primary.server_dir, "f2.txt", "f2\n", "foreign: second");

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
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
        primary_server_sp,
        "the target-side branch must be restored to its savepoint"
    );
    assert_eq!(
        pre_abort_sha(&primary.server_dir, &tid).as_deref(),
        Some(second_foreign.as_str()),
        "the target-side capture must have advanced to the observed tip"
    );
    assert!(
        try_git(
            &[
                "merge-base",
                "--is-ancestor",
                &first_foreign,
                &pre_abort_ref_name(&tid),
            ],
            &primary.server_dir,
        ),
        "the original capture must remain reachable through the advanced target-side ref"
    );
    assert!(
        !ww.root.join(".rwv-op").exists() && !primary.root.join(".rwv-op-lease").exists(),
        "op-state must be cleared on both sides after the clean abort"
    );
}

// ---------------------------------------------------------------------------
// Diverged capture still refuses, target side
// ---------------------------------------------------------------------------

/// A diverged target-side capture (foreign reset off the captured tip, then
/// a different line of work) refuses under consent, naming the DIVERGENCE
/// and the reconcile-by-hand path — same refusal the one-workspace shape
/// gives, keyed on the `-target` reference.
#[test]
fn sync_to_abandon_refuses_diverged_target_side_capture() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww) = make_shared_workspaces(tmp.path());
    let op_id = "sync-to-abandon-diverge";

    let (_, primary_server_sp) = plant_crashed_sync_to(&primary, &ww, op_id);
    let first_foreign = make_commit(&primary.server_dir, "f1.txt", "f1\n", "foreign: first");

    rwv()
        .arg("abort")
        .current_dir(&ww.root)
        .assert()
        .failure()
        .stderr(predicate::str::contains("foreign-tip"));
    let tid = target_restore_id(op_id);
    assert_eq!(
        pre_abort_sha(&primary.server_dir, &tid).as_deref(),
        Some(first_foreign.as_str()),
    );

    git(
        &["reset", "--hard", &primary_server_sp],
        &primary.server_dir,
    );
    let diverged_foreign = make_commit(&primary.server_dir, "d1.txt", "d1\n", "foreign: diverged");
    assert!(
        !try_git(
            &[
                "merge-base",
                "--is-ancestor",
                &first_foreign,
                &diverged_foreign,
            ],
            &primary.server_dir,
        ),
        "fixture invariant: the captured tip must not be an ancestor of the diverged tip"
    );

    let output = rwv()
        .arg("abort")
        .arg(format!("--abandon-foreign-tip={SERVER_PATH}"))
        .current_dir(&ww.root)
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
        diverged_foreign,
        "a diverged capture must not become consent to move the target-side branch"
    );
    assert_eq!(
        pre_abort_sha(&primary.server_dir, &tid).as_deref(),
        Some(first_foreign.as_str()),
        "the diverged target-side capture must not be advanced"
    );
    let consent_line = stderr
        .lines()
        .find(|line| line.contains("--abandon-foreign-tip named this repo"))
        .unwrap_or_else(|| {
            panic!("a refusal that arrives despite consent must say why.\nstderr:\n{stderr}")
        });
    assert!(
        consent_line.contains("DIVERGED"),
        "the refusal must name the divergence.\nconsent line: {consent_line}"
    );
    assert!(
        consent_line.contains(&first_foreign),
        "the refusal must name the diverged capture so the operator can locate it.\n\
         consent line: {consent_line}"
    );
    assert!(
        stderr.contains("to proceed:") && stderr.contains("reconcile"),
        "the refusal must name the reconcile-by-hand path.\nstderr:\n{stderr}"
    );
    assert!(
        ww.root.join(".rwv-op").exists(),
        "owner record must be retained on this refusal"
    );
}
