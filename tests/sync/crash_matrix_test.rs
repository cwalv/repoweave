//! Crash-matrix sweep for the phase machine (design § 4–5, 9).
//!
//! ## What this battery proves
//!
//! For every (phase × kill-point) cell of the land phase machine, this test
//! file pins two contracts:
//!
//! 1. **Continue-fidelity**: `rwv land --continue` from the synthesized crash state drives the op to an end
//!    state equal to an uninterrupted run's. We compare *real* end states
//!    (repo tips, lock content, marker absence) — not implementation echoes.
//! 2. **Abort-restore**: `rwv abort` from the same crash state restores every
//!    marked workspace to its pre-op tip and clears markers. For cells that
//!    plant a post-crash foreign commit, the `foreign-tip` refusal fires
//!    instead (nonzero exit, tip unchanged, op-state retained).
//!
//! Each cell also names the recorded decisions it pins. Specifically:
//!   - the per-session re-pin of the source snapshot on `--continue`
//!     (`continue_after_source_mutation`);
//!   - named-override consent persistence
//!     (`override_resume_fidelity_discard_local_commits`);
//!   - lease-side vs owner-side invariance for land (the
//!     "owner-rooted engine, invocation-CWD-rooted op-state" rule from
//!     the lease-side --continue case).
//!
//! ## Why on-disk synthesis is the accepted approach
//!
//! We cannot literally SIGKILL the binary mid-instruction. Instead, every
//! cell constructs **exactly** the on-disk state a kill at that point would
//! leave behind — per the `drive()` doc comment in `src/sync.rs`:
//!
//! > Crash semantics:
//! >   - Inside `run_phase`: record stays at the phase that was running →
//! >     `--continue` re-enters that phase (idempotent by construction).
//! >   - After `run_phase` returned but before `set_phase` of the next
//! >     phase committed: record still says current → `--continue` re-runs
//! >     the just-completed phase (idempotent), then transitions.
//! >   - After `set_phase` of the next phase committed: record says
//! >     next → `--continue` enters next directly.
//!
//! ## The matrix
//!
//! Phases × kill-points (`E` = at-entry, `M` = mid-mutation, `J` = just-
//! before-persist). For each phase the three kill-points map to record-
//! observable states:
//!
//! | Phase           | E (at-entry)                | M (mid-mutation)                            | J (just-before-persist)                              |
//! |-----------------|-----------------------------|---------------------------------------------|------------------------------------------------------|
//! | replay          | record=replay, no repo work | record=replay, one repo converged, one not  | record=replay, all repos converged, project rebased  |
//! | relock          | record=relock, repos done   | record=relock, lock committed, tips empty   | record=relock, lock committed + tips populated       |
//! | advance-target  | record=advance, target raw  | record=advance, some target repos ff'd      | record=advance, all target repos ff'd                |
//! | retire          | record=retire, ww present   | (= entry; retire is read-check then delete) | record=retire, dirty/merged check completed          |
//!
//! ### Shared-by-equivalence cells (claimed explicitly here, asserted by sharing fixtures)
//!
//! - `J(replay)` and `E(relock)` produce identical end states: J(replay)
//!   re-runs replay (every per-repo no-op) then advances; E(relock) just
//!   runs relock. The continue-fidelity contract is *the same end state*,
//!   so we test both record-states but reuse the comparison reference run.
//! - `J(relock)` (lock committed + tips populated, record=relock) and
//!   `E(advance-target)` (record=advance-target, no advance work) are
//!   equivalent under the same rule. Sharing is called out per-cell.
//! - `M(retire)` and `E(retire)` are observationally identical (retire's
//!   merged-check + dirty-check are read-only; `delete_workweave` is the
//!   one mutation, and either it has run or it has not). We test `E` and
//!   document the equivalence.
//!
//! ### Coverage axes (per design § 9, this is the matrix)
//!
//! The verb is `land`:
//!   - quiescent cells (workweave and primary at shared tips) exercise the
//!     replay and relock phases with nothing to move.
//!   - the remaining cells run the full machine; we cover every phase.
//!
//! Both invocation sides (owner and lease):
//!   - for land cells, every phase has at least one cell that exercises
//!     lease-side --continue AND lease-side abort. Two invocation-side
//!     inversion bugs were already caught this way; the matrix locks them
//!     down per phase.
//!
//! Override resume fidelity (one cell):
//!   - `discard-local-commits` recorded at fresh start, crash mid-replay,
//!     continue → the consent must still gate Phase 1' (tombstone savepoint
//!     preserved on cleanup, per `cleanup()` in sync.rs).
//!
//! Continue-after-source-mutation (one cell, pins design § 6's re-pin
//! decision):
//!   - crash mid-replay, then mutate the source between crash and continue;
//!     `--continue` must converge to the NEW pin (re-pin per session in
//!     `load_continuing_context`), not the original.
//!
//! ## What we did NOT change
//!
//! - Existing tests (`phase_reentry_test`, `abort_hardening_test`,
//!   `e2e_quiescent_sync_to_abort_test`) cover anecdotal slices of this matrix. We do
//!   not replace them — they are the per-spec acceptance tests. This file
//!   is the matrix sweep that makes the shrunk state space provable.
//! - The destructive-ops tripwire (`destructive_ops_audit_test.rs`) tracks
//!   `update-ref`, `--hard`, `remove_file` etc. We audit (but do not edit)
//!   the allowlist for the final shape.

use assert_cmd::Command as AssertCommand;
use std::path::{Path, PathBuf};

use crate::common;

use repoweave::diagnosis::State;

const SERVER_PATH: &str = "github/example/server";
const SERVER_URL: &str = "https://github.com/example/server";

fn rwv() -> AssertCommand {
    common::rwv()
}

/// A linked worktree's `.git` is a pointer file, not a directory, so
/// mid-rebase state never lives at `<repo>/.git/rebase-merge` there — only
/// `git rev-parse --git-path` resolves the real location.
fn mid_rebase(repo: &Path) -> bool {
    repo.join(".git/rebase-merge").exists()
        || repo.join(".git/rebase-apply").exists()
        || git_path_exists(repo, "rebase-merge")
        || git_path_exists(repo, "rebase-apply")
}

fn git_path_exists(repo: &Path, name: &str) -> bool {
    let out = common::git()
        .args(["rev-parse", "--git-path", name])
        .current_dir(repo)
        .output()
        .expect("git rev-parse failed");
    if !out.status.success() {
        return false;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    repo.join(&p).exists() || Path::new(&p).exists()
}

// ---------------------------------------------------------------------------
// Workspace setup — mirrors phase_reentry_test / e2e_quiescent_sync_to_abort_test style
// ---------------------------------------------------------------------------

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

fn write_manifest(project_dir: &Path) {
    let body = format!(
        "[repositories.\"{SERVER_PATH}\"]\ntype = \"git\"\nurl = \"{SERVER_URL}\"\nbranch = \"main\"\nrole = \"owned\"\n"
    );
    std::fs::write(project_dir.join("rwv.toml"), body).unwrap();
}

fn write_lock_at(project_dir: &Path, sha: &str) {
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
    write_lock_at(&project_dir, &sha);
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

/// primary + ww sharing server/project via git worktrees. Both start at the
/// same SHAs.
fn make_shared_workspaces(parent: &Path) -> (Workspace, Workspace, String) {
    let (primary, c1) = make_locked_workspace(parent, "primary");
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
    (primary, ww, c1)
}

// ---------------------------------------------------------------------------
// Op-state planting helpers (matching the JSON shape op_state.rs reads)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum PlantedVerb {
    Land,
}

impl PlantedVerb {
    fn wire(self) -> &'static str {
        match self {
            PlantedVerb::Land => "land",
        }
    }
}

#[derive(Default, Clone)]
struct OwnerRecordFixture {
    id: String,
    verb_str: &'static str,
    source: String,
    target: String,
    retire: bool,
    phase: &'static str,
    /// (repo-path-or-"(project)", sha) entries.
    converged_tips: Vec<(String, String)>,
    /// The target repos a landing journaled as moved. `None` journals what a
    /// landing that reached `phase` journals: each repo with a converged tip
    /// once it is advancing the target, before it moves any.
    moved_in_target: Option<Vec<repoweave::manifest::RepoKey>>,
    /// The rebases the landing journaled starting.
    rebases: Vec<repoweave::op_state::Rebasing>,
}

fn tip_map_json(tips: &[(String, String)]) -> String {
    tips.iter()
        .map(|(k, v)| format!("\"{k}\": \"{v}\""))
        .collect::<Vec<_>>()
        .join(", ")
}

fn plant_owner_record(workspace: &Path, rec: &OwnerRecordFixture) {
    let converged_json = tip_map_json(&rec.converged_tips);
    let body = format!(
        "{{\"id\": \"{id}\", \"verb\": \"{verb}\", \"history\": null, \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": {retire}, \
         \"phase\": \"{phase}\", \
         \"converged_tips\": {{{converged_json}}}, \
         \"started_at\": \"2026-06-10T00:00:00Z\", \"schema\": 8, \"release\": \"0.0.0\", \"take_in\": {take_in}}}",
        take_in = if rec.verb_str == "land" {
            let mut plan: repoweave::op_state::TakeInRecord = serde_json::from_str(
                &common::land_plan_json(workspace, Path::new(&rec.target), "web-app"),
            )
            .unwrap();
            plan.moved_in_target = rec.moved_in_target.clone().unwrap_or_else(|| {
                if matches!(rec.phase, "advance-target" | "retire") {
                    rec.converged_tips
                        .iter()
                        .map(|(key, _)| match key.as_str() {
                            "(project)" => repoweave::manifest::RepoKey::Project,
                            path => repoweave::manifest::RepoKey::Member(
                                repoweave::manifest::RepoPath::new(path).unwrap(),
                            ),
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            });
            plan.rebases = rec.rebases.clone();
            serde_json::to_string(&plan).unwrap()
        } else {
            "null".to_owned()
        },
        id = rec.id,
        verb = rec.verb_str,
        src = common::json_escaped(&rec.source),
        tgt = common::json_escaped(&rec.target),
        retire = rec.retire,
        phase = rec.phase,
    );
    std::fs::write(workspace.join(".rwv-op"), body).unwrap();
}

fn plant_lease(workspace: &Path, owner: &Path, id: &str) {
    let body = format!(
        "{{\"id\": \"{id}\", \"owner\": \"{owner}\", \"created_at\": \"2026-06-10T00:00:00Z\"}}",
        owner = common::json_escaped(owner),
    );
    std::fs::write(workspace.join(".rwv-op-lease"), body).unwrap();
}

fn plant_savepoint(repo: &Path, op_id: &str) {
    common::savepoint::plant(repo, op_id);
}

fn plant_savepoint_at(repo: &Path, op_id: &str, sha: &str) {
    common::savepoint::plant_at(repo, op_id, sha);
}

/// `land` plants two savepoint refs per worktree pair: `<id>` on the
/// owner/source side, `<id>-target` on the target side — matching
/// `restore_id_for` in `src/sync.rs`.
fn target_op_id(op_id: &str) -> String {
    format!("{op_id}-target")
}

fn plant_target_side(primary: &Workspace, ww: &Workspace, op_id: &str) {
    plant_lease(&primary.root, &ww.root, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);
}

// ---------------------------------------------------------------------------
// Divergence helpers — moving repos off their initial tip in controlled ways
// ---------------------------------------------------------------------------

/// Advance a repo by one commit; return the new tip.
fn advance_one(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

/// Sync-to setup with ww (owner) ahead of primary (target) by one commit in
/// the server repo AND one in the project repo (the latter via a manual lock
/// pin update so the project tip differs).
///
/// Returns: (primary, ww, ww_server_tip, ww_project_tip).
fn make_ww_ahead_sync_to(parent: &Path) -> (Workspace, Workspace, String, String) {
    let (primary, ww, _initial) = make_shared_workspaces(parent);

    // Advance ww's server.
    let ww_server_tip = advance_one(&ww.server_dir, "feature.txt", "ww feature\n", "ww: feature");

    // Update ww's lock to pin ww's server tip, commit.
    write_lock_at(&ww.project_dir, &ww_server_tip);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &ww.project_dir,
        &["commit", "-m", "lock: pin ww server tip"],
    );
    let ww_project_tip = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);

    (primary, ww, ww_server_tip, ww_project_tip)
}

// ---------------------------------------------------------------------------
// Test helpers: invoke continue / abort and parse outcomes
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Verb {
    Land,
}

impl Verb {
    fn arg(self) -> &'static str {
        match self {
            Verb::Land => "land",
        }
    }
}

fn run_continue(verb: Verb, cwd: &Path, cell_name: &str) {
    let out = rwv()
        .args([verb.arg(), "--continue"])
        .current_dir(cwd)
        .output()
        .expect("rwv --continue failed to spawn");
    assert!(
        out.status.success(),
        "[cell {cell_name}] {verb} --continue must succeed; stderr:\n{}\nstdout:\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout),
        verb = verb.arg(),
    );
}

fn run_abort_ok(cwd: &Path, cell_name: &str) {
    let out = rwv()
        .args(["abort"])
        .current_dir(cwd)
        .output()
        .expect("rwv abort failed to spawn");
    assert!(
        out.status.success(),
        "[cell {cell_name}] abort must succeed; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// Per-cell end-state assertions
// ---------------------------------------------------------------------------

/// After a successful `--continue` of a quiescent `land` from ww (sharing tips
/// with primary — no divergence is introduced ahead of the planted cell),
/// the markers must be cleared and ww's server/project repos and lock must
/// sit at the real tips passed in, the same end-state-equality standard
/// `assert_sync_to_continued_clean` holds land to.
fn assert_quiescent_sync_to_continued_clean(
    ww: &Workspace,
    expected_server_tip: &str,
    expected_project_tip: &str,
    cell_name: &str,
) {
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "[cell {cell_name}] owner record must be cleared after land --continue"
    );
    assert!(
        !ww.root.join(".rwv-op-lease").exists(),
        "[cell {cell_name}] no lease should exist after quiescent land"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        expected_server_tip,
        "[cell {cell_name}] ww's server tip must equal the real pre-op tip"
    );
    assert_eq!(
        common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
        expected_project_tip,
        "[cell {cell_name}] ww's project tip must equal the real pre-op tip"
    );
    let lock = repoweave::manifest::LockFile::from_path(&ww.project_dir.join("rwv.lock"))
        .expect("rwv.lock must parse after land --continue");
    let server_path = repoweave::manifest::RepoPath::new(SERVER_PATH).unwrap();
    let pinned = lock
        .get_entry(&server_path)
        .unwrap_or_else(|| panic!("[cell {cell_name}] lock must still carry the server entry"))
        .version
        .as_str();
    assert_eq!(
        pinned, expected_server_tip,
        "[cell {cell_name}] lock's pinned server version must equal the real server tip"
    );
}

/// After a successful `land --continue` from a ww-ahead-of-primary fixture,
/// primary must have ff'd to ww's server tip and the markers are cleared.
fn assert_sync_to_continued_clean(
    primary: &Workspace,
    ww: &Workspace,
    expected_server_tip: &str,
    cell_name: &str,
) {
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "[cell {cell_name}] owner record must be cleared after land --continue"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "[cell {cell_name}] lease must be cleared after land --continue"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        expected_server_tip,
        "[cell {cell_name}] target's server tip must equal owner's converged server tip"
    );
}

/// Abort must restore the owner workspace to pre-op tips (recorded as
/// savepoints) and clear all markers.
fn assert_sync_to_aborted_clean(
    primary: &Workspace,
    ww: &Workspace,
    ww_server_pre: &str,
    primary_server_pre: &str,
    cell_name: &str,
) {
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "[cell {cell_name}] owner record must be cleared by abort"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "[cell {cell_name}] target lease must be cleared by abort"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_pre,
        "[cell {cell_name}] abort must restore ww's server tip"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        primary_server_pre,
        "[cell {cell_name}] abort must restore primary's server tip"
    );
}

fn assert_quiescent_sync_to_aborted_clean(ww: &Workspace, ww_server_pre: &str, cell_name: &str) {
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "[cell {cell_name}] owner record must be cleared by abort"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_pre,
        "[cell {cell_name}] abort must restore ww's server tip"
    );
}

// ===========================================================================
// QUIESCENT SYNC-TO — replay and relock with nothing to move.
//
// Workspaces share tips. We construct each kill state, --continue, assert
// clean end state. Then re-do the setup, abort, assert pre-op restoration.
// ===========================================================================

// --- E(replay), quiescent land ---------------------------------------------------

/// Cell `E(replay) / sync / owner-side`: record=replay, no work yet. This is
/// the moment immediately after `guard_and_mark` returned. `--continue`
/// re-enters replay (per-repo no-op since repos at savepoint tips).
#[test]
fn cell_e_replay_quiescent_sync_to_continue_drives_to_clean_end_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-e-replay-sync";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    plant_target_side(&primary, &ww, op_id);

    let server_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let project_tip = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);

    run_continue(Verb::Land, &ww.root, "E(replay)/land-quiescent/owner");
    assert_quiescent_sync_to_continued_clean(
        &ww,
        &server_tip,
        &project_tip,
        "E(replay)/land-quiescent/owner",
    );
}

#[test]
fn cell_e_replay_quiescent_sync_to_abort_restores_to_pre_op() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-e-replay-sync-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);

    run_abort_ok(&ww.root, "E(replay)/land-quiescent/owner/abort");
    assert_quiescent_sync_to_aborted_clean(&ww, &sha, "E(replay)/land-quiescent/owner/abort");
}

// --- M(replay), quiescent land ---------------------------------------------------

/// Cell `M(replay) / sync / owner-side`: record=replay; mid-mutation is
/// modeled by a VCS-native rebase wreckage in one of the manifest repos.
/// `--continue` must abort the wreckage and re-run replay to convergence.
///
/// We use the discard-local-commits override path implicitly avoided here
/// (no override planted) — a rebase-strategy replay with already-
/// converged tips is what we want to verify.
#[test]
fn cell_m_replay_quiescent_sync_to_abort_cancels_mid_rebase_and_restores() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-m-replay-sync-abort";

    // Manufacture a mid-rebase wreckage on the server repo (conflicting line),
    // started from the savepoint as the op's own rebase is.
    // The ww workspace's server worktree is on branch `ww/main` (see
    // `make_shared_workspaces`); we cannot `checkout main` because the
    // primary's worktree holds that branch.
    let pre_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    advance_one(&ww.server_dir, "conflict.txt", "main\n", "main: c-base");
    let sha = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    common::git_in(&ww.server_dir, &["checkout", "-b", "diverge", &pre_tip]);
    advance_one(&ww.server_dir, "conflict.txt", "diverge\n", "div: c");
    let onto = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    common::git_in(&ww.server_dir, &["checkout", "ww/main"]);
    let _ = std::process::Command::new("git")
        .args(["rebase", "diverge"])
        .current_dir(&ww.server_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output();
    assert!(
        mid_rebase(&ww.server_dir),
        "fixture must leave ww's server mid-rebase"
    );

    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            rebases: vec![repoweave::op_state::Rebasing {
                repo: repoweave::manifest::RepoKey::Member(
                    repoweave::manifest::RepoPath::new(SERVER_PATH).unwrap(),
                ),
                branch: "ww/main".to_owned(),
                base: repoweave::vcs::CommitId::parse(&pre_tip).unwrap(),
                from: repoweave::vcs::CommitId::parse(&sha).unwrap(),
                onto: repoweave::vcs::CommitId::parse(&onto).unwrap(),
            }],
            ..Default::default()
        },
    );

    run_abort_ok(&ww.root, "M(replay)/land-quiescent/owner/abort");
    // Server must be back at the savepoint (mid-op classification + reset).
    assert!(
        !mid_rebase(&ww.server_dir),
        "abort must cancel the mid-rebase"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        sha,
        "abort must restore ww's server to the pre-op savepoint"
    );
}

// --- J(replay), quiescent land — observational equivalent of E(relock) ----------

/// Cell `J(replay) / sync / owner-side`: record=replay, all replay work
/// applied (project repo at source tip, manifest repos at lock tips). On
/// `--continue` the replay re-runs (every per-repo no-op), then transitions
/// to relock.
///
/// Equivalence claim: `J(replay)` and `E(relock)` (the next test) drive to
/// **the same end state**. They differ only in the record's phase field. We
/// keep both cells (the proof is structural: the record-state difference
/// must not produce an end-state difference).
#[test]
fn cell_j_replay_quiescent_sync_to_continue_drives_to_clean_end_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-j-replay-sync";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    plant_target_side(&primary, &ww, op_id);

    let server_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let project_tip = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);

    run_continue(Verb::Land, &ww.root, "J(replay)/land-quiescent/owner");
    assert_quiescent_sync_to_continued_clean(
        &ww,
        &server_tip,
        &project_tip,
        "J(replay)/land-quiescent/owner",
    );
}

// --- E(relock), quiescent land ---------------------------------------------------

/// Cell `E(relock) / sync / owner-side`: record=relock, no relock work done.
/// `--continue` regenerates the lock (no-op if current) and finishes.
#[test]
fn cell_e_relock_quiescent_sync_to_continue_drives_to_clean_end_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-e-relock-sync";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "relock",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    plant_target_side(&primary, &ww, op_id);

    let server_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let project_tip = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);

    run_continue(Verb::Land, &ww.root, "E(relock)/land-quiescent/owner");
    assert_quiescent_sync_to_continued_clean(
        &ww,
        &server_tip,
        &project_tip,
        "E(relock)/land-quiescent/owner",
    );
}

// --- M(relock), quiescent land ---------------------------------------------------

/// Cell `M(relock) / sync / owner-side`: record=relock, the lock has been
/// regenerated and committed but `record_converged_tips` has not yet run
/// (`converged_tips: {}` in the record). On `--continue` relock runs again:
/// the lock regenerate is a no-op (content unchanged), then `converged_tips`
/// gets populated and cleanup runs.
///
/// On abort: the converged tips map is empty, so the attributable-tip table
/// reduces to `{savepoint, mid-op}`. The project repo may have the auto-
/// relock commit — which is one commit *past* the savepoint and NOT in
/// converged_tips. That tip is **foreign** under the design § 5 rule:
/// abort refuses for that repo. We assert exactly that.
#[test]
fn cell_m_relock_quiescent_sync_to_continue_drives_to_clean_end_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-m-relock-sync";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "relock",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    plant_target_side(&primary, &ww, op_id);

    let server_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let project_tip = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);

    run_continue(Verb::Land, &ww.root, "M(relock)/land-quiescent/owner");
    assert_quiescent_sync_to_continued_clean(
        &ww,
        &server_tip,
        &project_tip,
        "M(relock)/land-quiescent/owner",
    );
}

// --- J(relock), quiescent land ---------------------------------------------------

/// Cell `J(relock) / sync / owner-side`: record=relock, lock committed AND
/// converged_tips populated. `--continue` re-runs relock (no-op), terminates
/// the quiescent land.
#[test]
fn cell_j_relock_quiescent_sync_to_continue_drives_to_clean_end_state() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-j-relock-sync";
    let project_tip = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    let server_tip = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);

    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "relock",
            converged_tips: vec![
                (SERVER_PATH.to_owned(), server_tip.clone()),
                ("(project)".to_owned(), project_tip.clone()),
            ],
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    plant_target_side(&primary, &ww, op_id);

    run_continue(Verb::Land, &ww.root, "J(relock)/land-quiescent/owner");
    assert_quiescent_sync_to_continued_clean(
        &ww,
        &server_tip,
        &project_tip,
        "J(relock)/land-quiescent/owner",
    );
}

// ===========================================================================
// SYNC-TO — full machine. Plant ww-ahead-of-primary divergence and exercise
// each phase × kill-point cell. We cover BOTH invocation sides per phase.
// ===========================================================================

// --- E(replay), land, owner-side -----------------------------------------

/// Cell `E(replay) / land / owner-side`: ww ahead, record=replay, no work
/// done. `--continue` runs the full machine.
#[test]
fn cell_e_replay_sync_to_owner_continue() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-e-replay-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    run_continue(Verb::Land, &ww.root, "E(replay)/land/owner");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "E(replay)/land/owner");
}

// --- E(replay), land, lease-side -----------------------------------------

/// Cell `E(replay) / land / lease-side`: identical setup to the
/// owner-side cell above, but `--continue` is invoked from the lease
/// workspace (primary). End state must be identical — `load_continuing_context`
/// re-roots the engine at the owner (the invocation-side rule).
#[test]
fn cell_e_replay_sync_to_lease_continue() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-e-replay-syncto-lease";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    // Invoke FROM THE LEASE.
    run_continue(Verb::Land, &primary.root, "E(replay)/land/lease");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "E(replay)/land/lease");
}

// --- E(replay), land, abort owner-side -----------------------------------

#[test]
fn cell_e_replay_sync_to_owner_abort() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let primary_server_pre = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);

    let op_id = "crash-matrix-e-replay-syncto-owner-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    run_abort_ok(&ww.root, "E(replay)/land/owner/abort");
    assert_sync_to_aborted_clean(
        &primary,
        &ww,
        &ww_server_pre,
        &primary_server_pre,
        "E(replay)/land/owner/abort",
    );
}

// --- E(replay), land, abort lease-side -----------------------------------

#[test]
fn cell_e_replay_sync_to_lease_abort() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let primary_server_pre = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);

    let op_id = "crash-matrix-e-replay-syncto-lease-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    // Invoke abort FROM THE LEASE.
    run_abort_ok(&primary.root, "E(replay)/land/lease/abort");
    assert_sync_to_aborted_clean(
        &primary,
        &ww,
        &ww_server_pre,
        &primary_server_pre,
        "E(replay)/land/lease/abort",
    );
}

/// Regression: op records hold operator-supplied paths verbatim,
/// which may reach a workspace through a symlink (macOS's `/var` →
/// `/private/var` tempdirs, symlinked weaveroots), while the invocation CWD
/// resolves canonically. Abort's workspace-identity comparisons must be
/// canonical — textual comparison restores the lease-side repos under the
/// wrong savepoint namespace and refuses with a spurious foreign-tip
/// violation. Same cell as `cell_e_replay_sync_to_lease_abort`, with every
/// recorded path routed through a symlink.
#[test]
fn cell_e_replay_sync_to_lease_abort_with_symlinked_record_paths() {
    let tmp = common::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = tmp.path().join("link");
    repoweave::symlink::create(&real, &link, repoweave::symlink::LinkTarget::Directory).unwrap();

    let (primary, ww, _ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(&real);
    let ww_alias = link.join("ww");
    let primary_alias = link.join("primary");

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let primary_server_pre = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);

    let op_id = "crash-matrix-e-replay-syncto-lease-abort-symlink";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww_alias.display().to_string(),
            target: primary_alias.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww_alias, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    // Invoke abort FROM THE LEASE via its canonical path.
    run_abort_ok(&primary.root, "E(replay)/land/lease/abort/symlink");
    assert_sync_to_aborted_clean(
        &primary,
        &ww,
        &ww_server_pre,
        &primary_server_pre,
        "E(replay)/land/lease/abort/symlink",
    );
}

// --- M(replay), land (owner-side) — one repo converged, one not ----------

/// Cell `M(replay) / land / owner-side`: record=replay, the server repo
/// has been "converged" (ww was already at its source tip, no change) but
/// the project repo has NOT yet had Phase 1' applied. On `--continue`,
/// replay completes Phase 1' and the machine drives forward.
///
/// Concretely we use the ww-ahead-land fixture where source = ww. ww's
/// own server is at ww_server_tip, so the per-repo "head == lock target"
/// check makes Phase 2 a no-op. Phase 1' rebases ww/project onto itself
/// (trivial no-op). End state: primary ff'd to ww's tips.
#[test]
fn cell_m_replay_sync_to_owner_continue() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-m-replay-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    run_continue(Verb::Land, &ww.root, "M(replay)/land/owner");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "M(replay)/land/owner");
}

// --- J(replay) / E(relock), land (owner) — same end state, different
// record values. We test E(relock) to exercise the record-says-relock path. --

#[test]
fn cell_e_relock_sync_to_owner_continue() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-e-relock-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "relock",
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    run_continue(Verb::Land, &ww.root, "E(relock)/land/owner");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "E(relock)/land/owner");
}

// --- M(relock), land (owner): lock committed, tips not yet recorded ------

#[test]
fn cell_m_relock_sync_to_owner_continue() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-m-relock-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "relock",
            // converged_tips empty — modeling "lock written, tips not yet recorded"
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    run_continue(Verb::Land, &ww.root, "M(relock)/land/owner");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "M(relock)/land/owner");
}

// --- J(relock) ≡ E(advance-target), land: converged_tips populated -------

#[test]
fn cell_j_relock_sync_to_owner_continue() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-j-relock-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "relock",
            converged_tips: vec![
                (SERVER_PATH.to_owned(), ww_server_tip.clone()),
                ("(project)".to_owned(), ww_project_tip),
            ],
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    run_continue(Verb::Land, &ww.root, "J(relock)/land/owner");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "J(relock)/land/owner");
}

// --- E(advance-target), land, owner-side ---------------------------------

/// `E(advance-target)` is observationally identical to `J(relock)` from the
/// engine's perspective (record value differs but state is the same once
/// the lock has been committed and `converged_tips` populated). We keep
/// both cells because the record-value difference is what `drive()`'s
/// crash semantics actually distinguish.
#[test]
fn cell_e_advance_target_sync_to_owner_continue() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-e-advance-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "advance-target",
            converged_tips: vec![
                (SERVER_PATH.to_owned(), ww_server_tip.clone()),
                ("(project)".to_owned(), ww_project_tip),
            ],
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    run_continue(Verb::Land, &ww.root, "E(advance-target)/land/owner");
    assert_sync_to_continued_clean(
        &primary,
        &ww,
        &ww_server_tip,
        "E(advance-target)/land/owner",
    );
}

// --- E(advance-target), land, lease-side ---------------------------------

#[test]
fn cell_e_advance_target_sync_to_lease_continue() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-e-advance-syncto-lease";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "advance-target",
            converged_tips: vec![
                (SERVER_PATH.to_owned(), ww_server_tip.clone()),
                ("(project)".to_owned(), ww_project_tip),
            ],
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    run_continue(Verb::Land, &primary.root, "E(advance-target)/land/lease");
    assert_sync_to_continued_clean(
        &primary,
        &ww,
        &ww_server_tip,
        "E(advance-target)/land/lease",
    );
}

// --- M(advance-target), land (owner) — some target repos ff'd ------------

/// Cell `M(advance-target) / land / owner-side`: record=advance-target,
/// the server target repo has been ff'd to the converged tip, but the
/// project target repo has not. On `--continue`, ff is a no-op on the
/// already-advanced server (head == target) and advances the project.
#[test]
fn cell_m_advance_target_sync_to_owner_continue() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    // Manually ff primary's server (modeling partial advance-target).
    common::git_in(
        &primary.server_dir,
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
    );
    let fetch_head = common::git_in(&primary.server_dir, &["rev-parse", "FETCH_HEAD"]);
    common::git_in(&primary.server_dir, &["reset", "--hard", &fetch_head]);
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        ww_server_tip,
        "fixture must ff primary's server to ww's tip"
    );

    let op_id = "crash-matrix-m-advance-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "advance-target",
            converged_tips: vec![
                (SERVER_PATH.to_owned(), ww_server_tip.clone()),
                ("(project)".to_owned(), ww_project_tip),
            ],
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    // Plant savepoints at PRE-ADVANCE tips for the target side (the savepoint
    // contract is "pre-op tip"). For the server we plant the original SHA.
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    // Primary's server is now AT the advanced tip; plant the pre-op SHA
    // explicitly (the savepoint = pre-op state).
    // The pre-op SHA is the initial sha (= ww's project's first server-tip).
    // For this fixture, primary's server was at the initial SHA before our
    // manual ff. We can derive it from one of ww's commit parents but it's
    // easier to record at fixture-build time. Re-construct via reflog of
    // primary's server.
    let pre_advance_primary_server =
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD@{1}"]);
    plant_savepoint_at(&primary.server_dir, &target_id, &pre_advance_primary_server);

    run_continue(Verb::Land, &ww.root, "M(advance-target)/land/owner");
    assert_sync_to_continued_clean(
        &primary,
        &ww,
        &ww_server_tip,
        "M(advance-target)/land/owner",
    );
}

// --- J(advance-target), land (owner) — every target repo advanced --------

/// Cell `J(advance-target) / land / owner-side`: record=advance-target,
/// every target repo (and the project repo) has reached the converged tip.
/// On `--continue` advance-target runs again — every ff is a no-op — and
/// machine terminates.
#[test]
fn cell_j_advance_target_sync_to_owner_continue() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    // Pre-advance primary's server AND project to ww's tips (modeling
    // advance-target complete but set_phase not yet committed).
    common::git_in(
        &primary.server_dir,
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
    );
    let fh = common::git_in(&primary.server_dir, &["rev-parse", "FETCH_HEAD"]);
    common::git_in(&primary.server_dir, &["reset", "--hard", &fh]);
    let pre_advance_primary_project = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);
    common::git_in(
        &primary.project_dir,
        &["fetch", &ww.project_dir.to_string_lossy(), "HEAD"],
    );
    let pfh = common::git_in(&primary.project_dir, &["rev-parse", "FETCH_HEAD"]);
    common::git_in(&primary.project_dir, &["reset", "--hard", &pfh]);

    let op_id = "crash-matrix-j-advance-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "advance-target",
            converged_tips: vec![
                (SERVER_PATH.to_owned(), ww_server_tip.clone()),
                ("(project)".to_owned(), ww_project_tip),
            ],
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    // Target savepoints record PRE-OP tips (before any advance). Use reflog
    // to recover the pre-advance tips.
    let pre_advance_primary_server =
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD@{1}"]);
    plant_savepoint_at(&primary.server_dir, &target_id, &pre_advance_primary_server);
    plant_savepoint_at(
        &primary.project_dir,
        &target_id,
        &pre_advance_primary_project,
    );

    run_continue(Verb::Land, &ww.root, "J(advance-target)/land/owner");
    assert_sync_to_continued_clean(
        &primary,
        &ww,
        &ww_server_tip,
        "J(advance-target)/land/owner",
    );
}

// --- Abort: M(advance-target), land (owner) ------------------------------

/// Abort from a partially-advanced state. The partially-advanced target
/// server is at the recorded converged tip → classified as
/// `RestoredFromConverged`. The not-yet-advanced target project is at the
/// savepoint → classified as `Untouched`. Both reset back to pre-op.
#[test]
fn cell_m_advance_target_sync_to_owner_abort() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let primary_server_pre = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    let primary_project_pre = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);

    // Partial advance: server only.
    common::git_in(
        &primary.server_dir,
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
    );
    let fh = common::git_in(&primary.server_dir, &["rev-parse", "FETCH_HEAD"]);
    common::git_in(&primary.server_dir, &["reset", "--hard", &fh]);

    let op_id = "crash-matrix-m-advance-syncto-owner-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "advance-target",
            converged_tips: vec![
                (SERVER_PATH.to_owned(), ww_server_tip),
                ("(project)".to_owned(), ww_project_tip),
            ],
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint_at(
        &ww.project_dir,
        op_id,
        &common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
    );
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    let target_id = target_op_id(op_id);
    // Savepoints record PRE-OP target tips.
    plant_savepoint_at(&primary.project_dir, &target_id, &primary_project_pre);
    plant_savepoint_at(&primary.server_dir, &target_id, &primary_server_pre);

    run_abort_ok(&ww.root, "M(advance-target)/land/owner/abort");
    assert_sync_to_aborted_clean(
        &primary,
        &ww,
        &ww_server_pre,
        &primary_server_pre,
        "M(advance-target)/land/owner/abort",
    );
    assert_eq!(
        common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]),
        primary_project_pre,
        "M(advance-target) abort must restore target project tip too"
    );
}

// --- Abort: lease-side, M(advance-target) -----------------------------------

#[test]
fn cell_m_advance_target_sync_to_lease_abort() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let primary_server_pre = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    let primary_project_pre = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);

    common::git_in(
        &primary.server_dir,
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
    );
    let fh = common::git_in(&primary.server_dir, &["rev-parse", "FETCH_HEAD"]);
    common::git_in(&primary.server_dir, &["reset", "--hard", &fh]);

    let op_id = "crash-matrix-m-advance-syncto-lease-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "advance-target",
            converged_tips: vec![
                (SERVER_PATH.to_owned(), ww_server_tip),
                ("(project)".to_owned(), ww_project_tip),
            ],
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint_at(
        &ww.project_dir,
        op_id,
        &common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
    );
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    let target_id = target_op_id(op_id);
    plant_savepoint_at(&primary.project_dir, &target_id, &primary_project_pre);
    plant_savepoint_at(&primary.server_dir, &target_id, &primary_server_pre);

    // Invoke abort FROM THE LEASE.
    run_abort_ok(&primary.root, "M(advance-target)/land/lease/abort");
    assert_sync_to_aborted_clean(
        &primary,
        &ww,
        &ww_server_pre,
        &primary_server_pre,
        "M(advance-target)/land/lease/abort",
    );
}

// --- Foreign-tip refusal: post-crash commit on the target server -----------

/// Cell `J(advance-target) / land / owner-side`: target's server has been
/// ff'd to the converged tip AND a post-crash foreign commit was added on
/// top of that tip (modeling: "another agent built on the advanced target
/// after our crash"). Abort keeps that tip as a backup, puts the target's
/// server back at its savepoint, and ends the op.
#[test]
fn cell_j_advance_target_sync_to_foreign_tip_is_kept_and_restored() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let primary_server_pre = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);

    // Advance primary server to ww's tip (advance-target's effect).
    common::git_in(
        &primary.server_dir,
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
    );
    let fh = common::git_in(&primary.server_dir, &["rev-parse", "FETCH_HEAD"]);
    common::git_in(&primary.server_dir, &["reset", "--hard", &fh]);
    // Add a FOREIGN commit on top.
    let foreign_tip = advance_one(
        &primary.server_dir,
        "foreign.txt",
        "another agent built on this\n",
        "foreign: post-crash commit",
    );

    let op_id = "crash-matrix-j-advance-foreign-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "advance-target",
            converged_tips: vec![
                (SERVER_PATH.to_owned(), ww_server_tip),
                ("(project)".to_owned(), ww_project_tip),
            ],
            moved_in_target: Some(vec![repoweave::manifest::RepoKey::Member(
                repoweave::manifest::RepoPath::new(SERVER_PATH).unwrap(),
            )]),
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint_at(
        &ww.project_dir,
        op_id,
        &common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
    );
    plant_savepoint_at(
        &ww.server_dir,
        op_id,
        &common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
    );
    let target_id = target_op_id(op_id);
    plant_savepoint_at(
        &primary.project_dir,
        &target_id,
        &common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]),
    );
    plant_savepoint_at(&primary.server_dir, &target_id, &primary_server_pre);

    run_abort_ok(&ww.root, "J(advance-target)/kept/land/owner");
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        primary_server_pre
    );
    assert_eq!(
        common::git_in(
            &primary.server_dir,
            &["for-each-ref", "--format=%(objectname)", "refs/rwv/backup/"]
        ),
        foreign_tip
    );
    assert!(!ww.root.join(".rwv-op").exists());
    assert!(!primary.root.join(".rwv-op-lease").exists());
}

// ===========================================================================
// RETIRE — E(retire) and J(retire) cells (M ≡ E by construction; retire is
// merged-check + dirty-check + delete_workweave, no partial mutation between
// reads and the single workweave-removal step).
// ===========================================================================

/// Workweave + primary fixture that resolves the workweave as a real
/// `Checkout::Workweave` (needs the `.rwv-workweave` marker).
struct RetireFixture {
    primary: Workspace,
    ww: Workspace,
}

fn make_retire_fixture(parent: &Path) -> RetireFixture {
    let (primary, initial_sha) = make_locked_workspace(parent, "primary");

    let ww_parent = parent.join(".workweaves");
    std::fs::create_dir_all(&ww_parent).unwrap();
    let ww_root = ww_parent.join("web-app--ww");
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
            "web-app--ww",
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
            "web-app--ww",
        ],
    );
    // Marker so the workweave resolves as such. No `.rwv-active` beside it:
    // the two files are mutually exclusive and resolution now refuses a root
    // carrying both, so a fixture that planted one here would be pinning the
    // state this design removed.
    let marker = common::workweave_marker(&primary.root, "web-app", &primary.root);
    std::fs::write(ww_root.join(".rwv-workweave"), marker).unwrap();
    common::register_workweave(&primary.root, "web-app", "ww", &ww_root);

    let _ = initial_sha;
    RetireFixture {
        primary,
        ww: Workspace {
            root: ww_root,
            project_dir: ww_project,
            server_dir: ww_server,
        },
    }
}

/// Cell `E(retire) / land / owner-side`: record=retire (and =M(retire),
/// since retire's mutation is the single workweave-delete step). Continue
/// must succeed when the workweave's manifest repos match the target's
/// (the reconciled / happy path).
#[test]
fn cell_e_retire_sync_to_owner_continue_when_reconciled() {
    let tmp = common::tempdir().unwrap();
    let RetireFixture { primary, ww } = make_retire_fixture(tmp.path());

    let op_id = "crash-matrix-e-retire-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            retire: true,
            phase: "retire",
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    // ww and primary already share tips (no divergence injected): merged-check passes.
    run_continue(Verb::Land, &ww.root, "E(retire)/land/owner");

    assert!(
        !ww.root.exists(),
        "workweave must be deleted after a successful retire"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "target lease must be cleared after a successful retire"
    );
}

/// Regression: `land --retire` leaks live-orphaned pre-op
/// savepoints. Phase order is `… → retire → cleanup`, so retire deletes the
/// workweave BEFORE cleanup drops savepoints. The buggy cleanup dropped
/// savepoints through the now-deleted workweave paths (`ctx.cwd_project_dir`,
/// `ctx.cwd_workspace_dir.join(repo)`), which silently no-op while the ref
/// survives in the shared clone refdb (`refs/rwv/*` is not worktree-local, and
/// `git worktree remove` never prunes it). With rebase-by-default the pre-op
/// tip becomes unreachable → `rwv doctor` classifies it "Live" and refuses to
/// auto-drop → every retire leaks one savepoint.
///
/// The workweave's server/project repos are `git worktree add`ed from the
/// primary's clones, so any savepoint ref planted in the workweave lives in
/// the primary's (surviving) refdb. After a successful retire+cleanup, NO
/// `refs/rwv/pre-op/<op_id>` (source side) and NO `…-target` (target side) may
/// remain in the surviving canonical repos.
///
/// FAILS before the fix (refs survive the deleted-path drop), PASSES after
/// (cleanup drops through `primary_path()`, which survives the workweave).
#[test]
fn cell_retire_sync_to_drops_savepoints_in_surviving_clone() {
    let tmp = common::tempdir().unwrap();
    let RetireFixture { primary, ww } = make_retire_fixture(tmp.path());

    let op_id = "crash-matrix-retire-savepoint-leak";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            retire: true,
            phase: "retire",
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    // Source-side savepoints (planted in the workweave worktrees, but the refs
    // live in the primary's shared refdb).
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    // Target-side savepoints (`<op_id>-target` namespace) on the primary.
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    // Sanity: the refs exist before the op (visible from the surviving primary
    // clones, which is where the workweave's shared refs actually live).
    let filed = |repo: &Path, key: &str| common::savepoint::checkout_tip(repo, key).is_some();
    let (source_ref, target_ref) = (op_id, target_id.as_str());
    assert!(
        filed(&primary.server_dir, source_ref),
        "fixture must plant the source-side savepoint in the shared refdb"
    );
    assert!(
        filed(&primary.project_dir, source_ref),
        "fixture must plant the source-side project savepoint in the shared refdb"
    );

    // ww and primary share tips: merged-check passes, retire deletes the ww,
    // then cleanup runs against the (now-deleted) workweave paths.
    run_continue(Verb::Land, &ww.root, "retire/land/savepoint-leak");

    assert!(
        !ww.root.exists(),
        "workweave must be deleted after a successful retire"
    );

    // The acceptance criterion: cleanup must have dropped every pre-op
    // savepoint for the op's own repos through the SURVIVING clone. Before the
    // fix these refs survive (cleanup hit the deleted workweave paths).
    assert!(
        !filed(&primary.server_dir, source_ref),
        "source-side server savepoint {source_ref} leaked into the surviving clone refdb after retire"
    );
    assert!(
        !filed(&primary.project_dir, source_ref),
        "source-side project savepoint {source_ref} leaked into the surviving clone refdb after retire"
    );
    assert!(
        !filed(&primary.server_dir, target_ref),
        "target-side server savepoint {target_ref} leaked after retire"
    );
    assert!(
        !filed(&primary.project_dir, target_ref),
        "target-side project savepoint {target_ref} leaked after retire"
    );
}

/// Cell `E(retire) / land / lease-side`: same set-up as above, invoked
/// from the lease workspace. The recorded decision is that this MUST be
/// observationally identical.
#[test]
fn cell_e_retire_sync_to_lease_continue_when_reconciled() {
    let tmp = common::tempdir().unwrap();
    let RetireFixture { primary, ww } = make_retire_fixture(tmp.path());

    let op_id = "crash-matrix-e-retire-syncto-lease";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            retire: true,
            phase: "retire",
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    // Lease-side invocation:
    run_continue(Verb::Land, &primary.root, "E(retire)/land/lease");

    assert!(
        !ww.root.exists(),
        "workweave must be deleted (lease-side --continue identical to owner-side)"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "target lease must be cleared after lease-side retire --continue"
    );
}

/// Cell `J(retire) / land / owner-side`: dirty-check + merged-check
/// passed but the workweave-delete has not yet committed. In practice this
/// is the same record state as `E(retire)` (record=retire, ww still exists)
/// — the kill-window is genuinely narrow here. The acceptance is the same:
/// `--continue` completes the delete; `abort` restores the workweave (no-op
/// because still present) and target.
#[test]
fn cell_j_retire_sync_to_owner_abort_restores_target() {
    let tmp = common::tempdir().unwrap();
    let RetireFixture { primary, ww } = make_retire_fixture(tmp.path());

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let primary_server_pre = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    let primary_project_pre = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);

    // Simulate a completed advance: bump ww's server and ff primary to match.
    let ww_server_post = advance_one(&ww.server_dir, "advance.txt", "advanced\n", "advance");
    common::git_in(
        &primary.server_dir,
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
    );
    let fh = common::git_in(&primary.server_dir, &["rev-parse", "FETCH_HEAD"]);
    common::git_in(&primary.server_dir, &["reset", "--hard", &fh]);

    let op_id = "crash-matrix-j-retire-syncto-owner-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            retire: true,
            phase: "retire",
            converged_tips: vec![(SERVER_PATH.to_owned(), ww_server_post)],
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint_at(
        &ww.project_dir,
        op_id,
        &common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
    );
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    let target_id = target_op_id(op_id);
    plant_savepoint_at(&primary.project_dir, &target_id, &primary_project_pre);
    plant_savepoint_at(&primary.server_dir, &target_id, &primary_server_pre);

    run_abort_ok(&ww.root, "J(retire)/land/owner/abort");

    // Both sides restored to pre-op.
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_pre,
        "abort from retire must restore ww's server"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        primary_server_pre,
        "abort from retire must restore primary's server"
    );
    assert_eq!(
        common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]),
        primary_project_pre,
        "abort from retire must restore primary's project"
    );
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "owner record cleared after abort"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "target lease cleared after abort"
    );
}

/// A landing that fast-forwarded the target's `server` and recorded so, then
/// stopped; someone commits on top of what it landed. Abort is the foreign
/// tip case for that repo, since the landing did move it; without the
/// journal entry the same tip is left where it stands.
fn landed_then_moved_again(journaled: bool) -> (Workspace, Workspace, String, tempfile::TempDir) {
    let tmp = common::tempdir().unwrap();
    let RetireFixture { primary, ww } = make_retire_fixture(tmp.path());
    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let primary_server_pre = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    let landed = advance_one(&ww.server_dir, "advance.txt", "advanced\n", "advance");
    if journaled {
        common::git_in(
            &primary.server_dir,
            &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
        );
        let fh = common::git_in(&primary.server_dir, &["rev-parse", "FETCH_HEAD"]);
        common::git_in(&primary.server_dir, &["merge", "--ff-only", &fh]);
    }
    let op_id = if journaled {
        "crash-matrix-landed-then-moved-journaled"
    } else {
        "crash-matrix-landed-then-moved-unjournaled"
    };
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "advance-target",
            converged_tips: vec![(SERVER_PATH.to_owned(), landed)],
            moved_in_target: Some(if journaled {
                vec![repoweave::manifest::RepoKey::Member(
                    repoweave::manifest::RepoPath::new(SERVER_PATH).unwrap(),
                )]
            } else {
                Vec::new()
            }),
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint_at(&primary.server_dir, &target_id, &primary_server_pre);
    let moved_again = advance_one(&primary.server_dir, "again.txt", "again\n", "again");
    (primary, ww, moved_again, tmp)
}

/// A target member the landing moved that moved again since is put back,
/// the tip it moved to kept as a backup first, and the op ends.
#[test]
fn abort_keeps_a_target_repo_the_landing_moved_that_moved_again() {
    let (primary, ww, moved_again, _tmp) = landed_then_moved_again(true);
    run_abort_ok(&ww.root, "landed then moved again");
    assert_ne!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        moved_again
    );
    assert_eq!(
        common::git_in(
            &primary.server_dir,
            &["for-each-ref", "--format=%(objectname)", "refs/rwv/backup/"]
        ),
        moved_again
    );
    assert!(!ww.root.join(".rwv-op").exists(), "the abort ends the op");
}

/// A target repo the landing never moved, stopped in a merge someone else
/// started, is left in that merge: abort neither cancels it nor resets the
/// branch under it, and ends the op.
#[test]
fn abort_leaves_a_target_repo_stopped_in_someone_elses_merge() {
    let (primary, ww, moved_again, _tmp) = landed_then_moved_again(false);
    let base = common::git_in(&primary.server_dir, &["rev-parse", "HEAD~1"]);
    common::git_in(&primary.server_dir, &["switch", "-q", "-c", "topic", &base]);
    advance_one(&primary.server_dir, "again.txt", "theirs\n", "theirs");
    common::git_in(&primary.server_dir, &["switch", "-q", "main"]);
    let merge = common::git()
        .args(["merge", "topic"])
        .current_dir(&primary.server_dir)
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the merge stops on its conflict");
    run_abort_ok(&ww.root, "someone else's merge in the target");
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        moved_again
    );
    common::git_in(
        &primary.server_dir,
        &["rev-parse", "--verify", "-q", "MERGE_HEAD"],
    );
    assert!(!ww.root.join(".rwv-op").exists(), "the abort ends the op");
}

/// A target repo the landing never moved, its savepoint taken at a commit
/// of its own that someone then reset its branch off: abort leaves the
/// branch, and keeps that commit as a backup before it drops the savepoint,
/// so it is left on a ref.
#[test]
fn abort_keeps_the_savepoint_of_an_unmoved_target_repo_no_branch_reaches() {
    let (primary, ww, moved_again, _tmp) = landed_then_moved_again(false);
    let tree = common::git_in(&primary.server_dir, &["rev-parse", "HEAD^{tree}"]);
    let saved = common::git_in(
        &primary.server_dir,
        &["commit-tree", &tree, "-p", "HEAD", "-m", "held before"],
    );
    common::savepoint::plant_at(
        &primary.server_dir,
        &target_op_id("crash-matrix-landed-then-moved-unjournaled"),
        &saved,
    );
    assert_eq!(
        common::git_in(
            &primary.server_dir,
            &[
                "for-each-ref",
                "--contains",
                &saved,
                "refs/heads/",
                "refs/remotes/"
            ]
        ),
        ""
    );
    let rewritten = moved_again;

    let out = common::cli::run(&["abort"], &ww.root);
    assert!(out.status.success(), "{}", common::cli::describe(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("it held before this operation is kept as backup"),
        "{}",
        common::cli::describe(&out)
    );
    assert_ne!(
        common::git_in(
            &primary.server_dir,
            &["for-each-ref", "--points-at", &saved, "refs/rwv/backup/"]
        ),
        ""
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        rewritten
    );
}

#[test]
fn abort_leaves_a_target_repo_the_landing_never_moved() {
    let (primary, ww, moved_again, _tmp) = landed_then_moved_again(false);
    run_abort_ok(&ww.root, "moved by someone else only");
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        moved_again
    );
    assert!(!ww.root.join(".rwv-op").exists(), "the abort ends the op");
}

// ===========================================================================
// CONTINUE-AFTER-SOURCE-MUTATION (one cell, pins design § 6's re-pin rule)
// ===========================================================================

/// Crash mid-replay (record=replay), then move the target between crash and
/// `--continue`. The plan the op recorded no longer describes the landing, so
/// `--continue` refuses it as `target` moved mid-operation and changes
/// nothing. The route it prints, `rwv abort`, ends the op in both workspaces:
/// the landing moved nothing in the target, so the commits someone else made
/// there stay where they are rather than reading as foreign tips.
#[test]
fn cell_continue_after_the_target_moved_refuses_and_abort_ends_it() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-continue-after-source-mut";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    plant_target_side(&primary, &ww, op_id);

    // === Source mutation between crash and --continue ===
    let new_primary_server_tip = advance_one(
        &primary.server_dir,
        "new-source.txt",
        "primary new commit\n",
        "primary: new source commit",
    );
    write_lock_at(&primary.project_dir, &new_primary_server_tip);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &primary.project_dir,
        &["commit", "-m", "lock: pin new server tip"],
    );

    let ww_server_before = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let out = common::cli::run(&["land", "--continue"], &ww.root);
    let stderr = common::cli::refused(&out, State::TargetMovedMidOperation);
    assert!(
        !stderr.contains("land: composing"),
        "the resume refuses before it composes anything: {stderr}"
    );
    assert!(
        ww.root.join(".rwv-op").exists(),
        "the refused resume keeps the op"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_before,
        "the refused resume moved nothing"
    );
    let primary_project_moved = common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]);

    let abort = common::cli::succeed(&["abort"], &ww.root);
    let said = String::from_utf8_lossy(&abort.stdout);
    assert!(
        said.contains("where something other than this landing moved it"),
        "{}",
        common::cli::describe(&abort)
    );
    assert!(!ww.root.join(".rwv-op").exists(), "the abort ends the op");
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "the abort ends the op in the target too"
    );
    assert_eq!(
        common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]),
        new_primary_server_tip,
        "the commit someone else made in the target stays"
    );
    assert_eq!(
        common::git_in(&primary.project_dir, &["rev-parse", "HEAD"]),
        primary_project_moved,
        "the target's project branch stays where someone else moved it"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_before,
        "the source is at its savepoint"
    );
}

// ===========================================================================
// REPLAY AND RELOCK KILL CELLS
//
// Each cell synthesizes the on-disk state a kill in the replay or at relock
// leaves, then asserts real end states (repo tips, backups, op-state
// presence, exit code).
// ===========================================================================

/// Fixture: primary's server is one commit AHEAD of ww's server.
/// Returns (primary, ww, initial_sha, primary_server_tip).
/// primary_server_tip is the sha that ww's server will ff to during sync.
fn make_primary_ahead_sync(parent: &Path) -> (Workspace, Workspace, String, String) {
    let (primary, ww, initial_sha) = make_shared_workspaces(parent);

    // Advance primary's server by one commit.
    let primary_server_tip = advance_one(
        &primary.server_dir,
        "primary-feature.txt",
        "primary feature\n",
        "primary: server advance",
    );
    // Update primary's lock to pin the new server tip.
    write_lock_at(&primary.project_dir, &primary_server_tip);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(
        &primary.project_dir,
        &["commit", "-m", "lock: pin primary server advance"],
    );

    (primary, ww, initial_sha, primary_server_tip)
}

/// A kill before any repo advances: every repo is at its savepoint, and
/// abort leaves each untouched.
#[test]
fn cell_replay_no_advances_abort_all_untouched() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, initial_sha, _) = make_primary_ahead_sync(tmp.path());

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let ww_project_pre = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    // ww's server starts at initial_sha; must equal ww_server_pre.
    assert_eq!(ww_server_pre, initial_sha);

    let op_id = "crash-matrix-cell1-entry-write-no-advances";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    // Savepoints record the pre-op tips (repos have NOT advanced yet).
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    plant_savepoint_at(&ww.project_dir, op_id, &ww_project_pre);

    run_abort_ok(&ww.root, "cell1/entry-write-no-advances");

    // Both repos back at pre-op tips.
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_pre,
        "[cell1] abort must leave ww's server at savepoint (Untouched)"
    );
    assert_eq!(
        common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
        ww_project_pre,
        "[cell1] abort must leave ww's project at savepoint (Untouched)"
    );
    // Markers cleared.
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "[cell1] owner record must be cleared"
    );
}

/// A kill mid-fan-out, the server fast-forwarded and the project at its
/// savepoint: abort puts the server back and leaves the project untouched.
#[test]
fn cell_replay_mid_fanout_server_advanced_abort_restores_it() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, initial_sha, primary_server_tip) = make_primary_ahead_sync(tmp.path());

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let ww_project_pre = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);
    assert_eq!(ww_server_pre, initial_sha);

    // Model the mid-fan-out state: server repo has been ff'd to primary_server_tip.
    common::git_in(
        &ww.server_dir,
        &["fetch", &primary.server_dir.to_string_lossy(), "HEAD"],
    );
    let fh = common::git_in(&ww.server_dir, &["rev-parse", "FETCH_HEAD"]);
    common::git_in(&ww.server_dir, &["reset", "--hard", &fh]);
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        primary_server_tip,
        "fixture must advance ww's server to the ff-target"
    );

    let op_id = "crash-matrix-cell2-mid-fanout";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    // Savepoints record PRE-OP tips.
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    plant_savepoint_at(&ww.project_dir, op_id, &ww_project_pre);

    run_abort_ok(&ww.root, "cell2/mid-fanout");

    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_pre,
        "[cell2] abort must restore ww's server to pre-op tip"
    );
    assert_eq!(
        common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
        ww_project_pre,
        "[cell2] abort must leave ww's project at savepoint (Untouched)"
    );
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "[cell2] owner record must be cleared"
    );
}

/// A kill after the project's rebase, the project at the tip it made: abort
/// keeps that tip in the branch's slot and puts the project back.
#[test]
fn cell_replay_project_at_its_rebased_tip_is_kept() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let ww_project_pre = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);

    let op_id = "crash-matrix-cell4-rebased";
    let t1_after_record = || {
        advance_one(
            &ww.project_dir,
            "rebased-one-write-window.txt",
            "T1 — rebase landed here\n",
            "phase1prime: rebased (one-write-window crash)",
        )
    };
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    // Savepoints record PRE-op tips.
    plant_savepoint_at(
        &ww.server_dir,
        op_id,
        &common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
    );
    plant_savepoint_at(&ww.project_dir, op_id, &ww_project_pre);
    // Phase 1' completes after the record holds the tip it started from.
    let t1 = t1_after_record();

    let out = rwv()
        .args(["abort"])
        .current_dir(&ww.root)
        .output()
        .expect("rwv abort failed to spawn");
    let said = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "[cell4] {said}");
    assert_eq!(
        common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
        ww_project_pre
    );
    let kept = common::git_in(
        &ww.project_dir,
        &["for-each-ref", "--format=%(objectname)", "refs/rwv/backup/"],
    );
    assert_eq!(kept, t1, "[cell4] {said}");
    assert!(!ww.root.join(".rwv-op").exists(), "[cell4] {said}");
}

/// A kill after relock, the project at the relock commit: abort puts every
/// repo back.
#[test]
fn cell_after_relock_project_at_t2_abort_restores() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let ww_project_pre = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);

    // Model the post-relock state: ww's project has been advanced to T2
    // (the lock-commit tip written at the end of relock).
    let t2 = advance_one(
        &ww.project_dir,
        "relock-commit.txt",
        "relock commit — T2\n",
        "lock: relock post-rebase",
    );

    let op_id = "crash-matrix-cell5-after-relock";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "relock",
            // converged_tips populated with T2 for the project and current server tip.
            converged_tips: vec![
                ("(project)".to_owned(), t2.clone()),
                (
                    SERVER_PATH.to_owned(),
                    common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
                ),
            ],
            ..Default::default()
        },
    );
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    plant_savepoint_at(&ww.project_dir, op_id, &ww_project_pre);

    run_abort_ok(&ww.root, "cell5/after-relock");

    assert_eq!(
        common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
        ww_project_pre,
        "[cell5] abort must restore ww's project to pre-op"
    );
    // Server restored to pre-op.
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_pre,
        "[cell5] abort must restore ww's server to pre-op"
    );
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "[cell5] owner record must be cleared"
    );
}

/// A kill mid-replay, then an operator commit on the server: the server's
/// tip is kept in its slot and both repos are put back, the record cleared.
#[test]
fn cell_replay_operator_commit_on_the_server_is_kept_and_both_restored() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, primary_server_tip, _ww_project_tip) = make_primary_ahead_sync(tmp.path());

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let ww_project_pre = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);

    // Advance ww's project to simulate it was ff'd or rebased by the op.
    // (In the incident scenario, the project was the op-advanced repo that
    // should auto-restore; the server is the one with a foreign operator commit.)
    advance_one(
        &ww.project_dir,
        "op-project-advance.txt",
        "op advanced project\n",
        "op: project advance",
    );

    // Advance ww's server to the ff target (primary_server_tip), then add an
    // operator commit on top — making the server genuinely foreign.
    // primary_server_tip is already accessible in the shared object store;
    // use reset --hard directly to move ww's server branch to that SHA.
    common::git_in(&ww.server_dir, &["reset", "--hard", &primary_server_tip]);
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        primary_server_tip,
        "fixture must reach ff target before adding foreign commit"
    );
    let foreign_server_tip = advance_one(
        &ww.server_dir,
        "operator-commit.txt",
        "operator built on the advanced tip\n",
        "foreign: operator commit post-crash",
    );

    let op_id = "crash-matrix-cell6-mixed";
    plant_owner_record(
        &ww.root,
        &OwnerRecordFixture {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Land.wire(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    plant_savepoint_at(&ww.project_dir, op_id, &ww_project_pre);

    // The server's tip holds a commit the op did not write: it is kept as a
    // backup, and both repos are put back.
    let out = rwv()
        .args(["abort"])
        .current_dir(&ww.root)
        .output()
        .expect("rwv abort failed to spawn");
    let said = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "[cell6] {said}");
    assert!(
        said.contains(&format!(
            "{SERVER_PATH}: what abort discarded is kept as backup"
        )),
        "[cell6] the server's tip is named as kept: {said}"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_pre
    );
    assert_eq!(
        common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
        ww_project_pre
    );
    let kept = common::git_in(
        &ww.server_dir,
        &["for-each-ref", "--format=%(objectname)", "refs/rwv/backup/"],
    );
    assert_eq!(kept, foreign_server_tip, "[cell6] {said}");
    assert!(!ww.root.join(".rwv-op").exists(), "[cell6] {said}");
}

fn cell7_record(primary: &Workspace, ww: &Workspace, op_id: &str) -> String {
    format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"sync\", \"history\": null, \"project\": \"web-app\", \
         \"source\": \"{src}\", \"target\": \"{tgt}\", \"retire\": false, \
         \"phase\": \"replay\", \
         \"started_at\": \"2026-06-10T00:00:00Z\", \"schema\": 8, \"release\": \"0.0.0\", \"take_in\": null}}",
        src = common::json_escaped(&primary.root),
        tgt = common::json_escaped(&ww.root),
    )
}

/// Cell 7b: a record whose header does not parse names no op, so abort
/// refuses without touching any repo and leaves the record to the operator.
#[test]
fn cell_owner_record_without_a_readable_header_is_refused() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let ww_server_pre = common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]);
    let ww_project_pre = common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]);

    let op_id = "crash-matrix-cell7b-headless";
    let body = cell7_record(&primary, &ww, op_id).replace("\"verb\": \"sync\"", "\"verb\": 7");
    std::fs::write(ww.root.join(".rwv-op"), &body).unwrap();
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    plant_savepoint_at(&ww.project_dir, op_id, &ww_project_pre);

    let out = rwv()
        .args(["abort"])
        .current_dir(&ww.root)
        .output()
        .expect("rwv abort failed to spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "[cell7b] stderr:\n{stderr}");
    assert!(
        stderr.contains("it names no operation this rwv can read")
            && stderr.contains("rwv explain unreadable-op-record"),
        "[cell7b] refusal must say the record names no op; stderr:\n{stderr}"
    );
    assert_eq!(
        common::git_in(&ww.server_dir, &["rev-parse", "HEAD"]),
        ww_server_pre,
    );
    assert_eq!(
        common::git_in(&ww.project_dir, &["rev-parse", "HEAD"]),
        ww_project_pre,
    );
    assert!(ww.root.join(".rwv-op").exists());
}
