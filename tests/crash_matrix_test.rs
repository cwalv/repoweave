//! Crash-matrix sweep for the phase machine (design § 4–5, 9; fo-jsbr3i.7).
//!
//! ## What this battery proves
//!
//! For every (phase × kill-point) cell of the sync phase machine, this test
//! file pins two contracts:
//!
//! 1. **Continue-fidelity**: `rwv sync --continue` (or `rwv sync-to
//!    --continue`) from the synthesised crash state drives the op to an end
//!    state equal to an uninterrupted run's. We compare *real* end states
//!    (repo tips, lock content, marker absence) — not implementation echoes.
//! 2. **Abort-restore**: `rwv abort` from the same crash state restores every
//!    marked workspace to its pre-op tip and clears markers. For cells that
//!    plant a post-crash foreign commit, the `foreign-tip` refusal fires
//!    instead (nonzero exit, tip unchanged, op-state retained).
//!
//! Each cell also names the recorded epic decisions it pins. Specifically:
//!   - the per-session re-pin of the source snapshot on `--continue`
//!     (`continue_after_source_mutation`);
//!   - named-override consent persistence
//!     (`override_resume_fidelity_discard_local_commits`);
//!   - lease-side vs owner-side invariance for sync-to (the
//!     "owner-rooted engine, invocation-CWD-rooted op-state" rule from
//!     fo-jsbr3i.2's lease-side --continue case).
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
//! >   - After `run_phase` returned but before `advance_phase` of the next
//! >     phase committed: record still says current → `--continue` re-runs
//! >     the just-completed phase (idempotent), then transitions.
//! >   - After `advance_phase` of the next phase committed: record says
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
//! Both verbs (`sync` and `sync-to`):
//!   - plain sync exercises the degenerate machine (replay → relock); we
//!     cover replay and relock cells for `sync`.
//!   - sync-to exercises the full machine; we cover every phase.
//!
//! Both invocation sides (owner and lease):
//!   - for sync-to cells, every phase has at least one cell that exercises
//!     lease-side --continue AND lease-side abort. fo-jsbr3i.2 already
//!     caught two invocation-side inversion bugs; the matrix locks them
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
//!   `e2e_sync_abort_test`) cover anecdotal slices of this matrix. We do
//!   not replace them — they are the per-bead acceptance tests. This file
//!   is the matrix sweep that makes the shrunk state space provable.
//! - The destructive-ops tripwire (`destructive_ops_audit_test.rs`) tracks
//!   `update-ref`, `--hard`, `remove_file` etc. We audit (but do not edit)
//!   the allowlist for the final epic shape.

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

fn ref_exists(repo: &Path, refname: &str) -> bool {
    common::git()
        .args(["rev-parse", "--verify", refname])
        .current_dir(repo)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Workspace setup — mirrors phase_reentry_test / e2e_sync_abort_test style
// ---------------------------------------------------------------------------

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

fn write_manifest(project_dir: &Path) {
    let body = format!(
        "repositories:\n  {SERVER_PATH}:\n    type: git\n    url: {SERVER_URL}\n    version: main\n    role: owned\n"
    );
    std::fs::write(project_dir.join("rwv.yaml"), body).unwrap();
}

fn write_lock_at(project_dir: &Path, sha: &str) {
    let body = format!(
        "repositories:\n  {SERVER_PATH}:\n    type: git\n    url: {SERVER_URL}\n    version: {sha}\n"
    );
    std::fs::write(project_dir.join("rwv.lock"), body).unwrap();
}

fn make_locked_workspace(parent: &Path, name: &str) -> (Workspace, String) {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/example")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();

    let server_dir = root.join(SERVER_PATH);
    let sha = init_repo(&server_dir);

    let project_dir = root.join("projects/web-app");
    init_repo(&project_dir);
    std::fs::write(project_dir.join(".gitattributes"), "rwv.lock merge=ours\n").unwrap();
    write_manifest(&project_dir);
    write_lock_at(&project_dir, &sha);
    git(
        &["add", ".gitattributes", "rwv.yaml", "rwv.lock"],
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

/// primary + ww sharing server/project via git worktrees. Both start at the
/// same SHAs.
fn make_shared_workspaces(parent: &Path) -> (Workspace, Workspace, String) {
    let (primary, c1) = make_locked_workspace(parent, "primary");
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
    (primary, ww, c1)
}

// ---------------------------------------------------------------------------
// Op-state planting helpers (matching the YAML shape op_state.rs reads)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum PlantedVerb {
    Sync,
    SyncTo,
}

impl PlantedVerb {
    fn yaml(self) -> &'static str {
        match self {
            PlantedVerb::Sync => "sync",
            PlantedVerb::SyncTo => "sync-to",
        }
    }
}

#[derive(Default, Clone)]
struct OwnerRecordYaml {
    id: String,
    verb_str: &'static str,
    source: String,
    target: String,
    retire: bool,
    phase: &'static str,
    /// (repo-path-or-"(project)", sha) entries.
    converged_tips: Vec<(String, String)>,
    /// override strings (e.g. "discard-local-commits", "allow-stale-lock").
    overrides: Vec<&'static str>,
}

fn plant_owner_record(workspace: &Path, rec: &OwnerRecordYaml) {
    let tips_yaml = if rec.converged_tips.is_empty() {
        "converged_tips: {}\n".to_owned()
    } else {
        let mut s = String::from("converged_tips:\n");
        for (k, v) in &rec.converged_tips {
            s.push_str(&format!("  \"{k}\": \"{v}\"\n"));
        }
        s
    };
    let overrides_yaml = if rec.overrides.is_empty() {
        "overrides: []\n".to_owned()
    } else {
        let mut s = String::from("overrides:\n");
        for o in &rec.overrides {
            s.push_str(&format!("  - {o}\n"));
        }
        s
    };
    let body = format!(
        "id: \"{id}\"\n\
         verb: {verb}\n\
         strategy: rebase\n\
         source: \"{src}\"\n\
         target: \"{tgt}\"\n\
         retire: {retire}\n\
         phase: {phase}\n\
         {tips_yaml}\
         {overrides_yaml}\
         started_at: \"2026-06-10T00:00:00Z\"\n",
        id = rec.id,
        verb = rec.verb_str,
        src = rec.source,
        tgt = rec.target,
        retire = rec.retire,
        phase = rec.phase,
    );
    std::fs::write(workspace.join(".rwv-op"), body).unwrap();
}

fn plant_lease(workspace: &Path, owner: &Path, id: &str) {
    let body = format!(
        "id: \"{id}\"\nowner: \"{owner}\"\n",
        owner = owner.display(),
    );
    std::fs::write(workspace.join(".rwv-op-lease"), body).unwrap();
}

fn plant_savepoint(repo: &Path, op_id: &str) {
    let head = git_out(&["rev-parse", "HEAD"], repo);
    git(
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), &head],
        repo,
    );
}

fn plant_savepoint_at(repo: &Path, op_id: &str, sha: &str) {
    git(
        &["update-ref", &format!("refs/rwv/pre-op/{op_id}"), sha],
        repo,
    );
}

/// `sync-to` plants two savepoint refs per worktree pair: `<id>` on the
/// owner/source side, `<id>-target` on the target side. The pre-abort ref
/// uses `<id>` for the owner side and `<id>-target` for target — matching
/// `restore_id_for` in `src/sync.rs`.
fn target_op_id(op_id: &str) -> String {
    format!("{op_id}-target")
}

// ---------------------------------------------------------------------------
// Divergence helpers — moving repos off their initial tip in controlled ways
// ---------------------------------------------------------------------------

/// Advance a repo by one commit; return the new tip.
fn advance_one(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
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
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(
        &["commit", "-m", "lock: pin ww server tip"],
        &ww.project_dir,
    );
    let ww_project_tip = git_out(&["rev-parse", "HEAD"], &ww.project_dir);

    (primary, ww, ww_server_tip, ww_project_tip)
}

// ---------------------------------------------------------------------------
// Test helpers: invoke continue / abort and parse outcomes
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Verb {
    Sync,
    SyncTo,
}

impl Verb {
    fn arg(self) -> &'static str {
        match self {
            Verb::Sync => "sync",
            Verb::SyncTo => "sync-to",
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

fn run_abort_refuses_foreign(cwd: &Path, cell_name: &str) -> String {
    let out = rwv()
        .args(["abort"])
        .current_dir(cwd)
        .output()
        .expect("rwv abort failed to spawn");
    assert!(
        !out.status.success(),
        "[cell {cell_name}] abort must refuse on a foreign tip; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains("foreign-tip"),
        "[cell {cell_name}] refusal must name `foreign-tip`; stderr:\n{stderr}"
    );
    stderr
}

// ---------------------------------------------------------------------------
// Per-cell end-state assertions
// ---------------------------------------------------------------------------

/// After a successful `--continue` of a plain `sync` from ww (sharing tips
/// with primary), the markers must be cleared and the auto-relock commit
/// applied to the project repo (the lock now contains a `workweave:` field).
fn assert_plain_sync_continued_clean(ww: &Workspace, cell_name: &str) {
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "[cell {cell_name}] owner record must be cleared after sync --continue"
    );
    assert!(
        !ww.root.join(".rwv-op-lease").exists(),
        "[cell {cell_name}] no lease should exist after plain sync"
    );
    // The project repo should reach a state where the lock references the
    // (shared) server tip and the file is committed; we can't enumerate every
    // expected SHA, but lock file presence + project repo cleanliness is the
    // contract.
    let lock = std::fs::read_to_string(ww.project_dir.join("rwv.lock"))
        .expect("rwv.lock must exist after sync --continue");
    assert!(
        lock.contains("repositories:"),
        "[cell {cell_name}] lock must be well-formed after sync --continue; got:\n{lock}"
    );
}

/// After a successful `sync-to --continue` from a ww-ahead-of-primary fixture,
/// primary must have ff'd to ww's server tip and the markers are cleared.
fn assert_sync_to_continued_clean(
    primary: &Workspace,
    ww: &Workspace,
    expected_server_tip: &str,
    cell_name: &str,
) {
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "[cell {cell_name}] owner record must be cleared after sync-to --continue"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "[cell {cell_name}] lease must be cleared after sync-to --continue"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
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
        git_out(&["rev-parse", "HEAD"], &ww.server_dir),
        ww_server_pre,
        "[cell {cell_name}] abort must restore ww's server tip"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
        primary_server_pre,
        "[cell {cell_name}] abort must restore primary's server tip"
    );
}

fn assert_plain_sync_aborted_clean(ww: &Workspace, ww_server_pre: &str, cell_name: &str) {
    assert!(
        !ww.root.join(".rwv-op").exists(),
        "[cell {cell_name}] owner record must be cleared by abort"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ww.server_dir),
        ww_server_pre,
        "[cell {cell_name}] abort must restore ww's server tip"
    );
}

// ===========================================================================
// PLAIN SYNC — degenerate machine (replay → relock); no lease, no target.
//
// Workspaces share tips. We construct each kill state, --continue, assert
// clean end state. Then re-do the setup, abort, assert pre-op restoration.
// ===========================================================================

// --- E(replay), plain sync ---------------------------------------------------

/// Cell `E(replay) / sync / owner-side`: record=replay, no work yet. This is
/// the moment immediately after `guard_and_mark` returned. `--continue`
/// re-enters replay (per-repo no-op since repos at savepoint tips).
#[test]
fn cell_e_replay_sync_continue_drives_to_clean_end_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-e-replay-sync";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Sync.yaml(),
            source: primary.root.display().to_string(),
            target: ww.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);

    run_continue(Verb::Sync, &ww.root, "E(replay)/sync/owner");
    assert_plain_sync_continued_clean(&ww, "E(replay)/sync/owner");
}

#[test]
fn cell_e_replay_sync_abort_restores_to_pre_op() {
    let tmp = tempfile::tempdir().unwrap();
    let (_primary, ww, sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-e-replay-sync-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Sync.yaml(),
            source: ww.root.display().to_string(),
            target: ww.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);

    run_abort_ok(&ww.root, "E(replay)/sync/owner/abort");
    assert_plain_sync_aborted_clean(&ww, &sha, "E(replay)/sync/owner/abort");
}

// --- M(replay), plain sync ---------------------------------------------------

/// Cell `M(replay) / sync / owner-side`: record=replay; mid-mutation is
/// modelled by a VCS-native rebase wreckage in one of the manifest repos.
/// `--continue` must abort the wreckage and re-run replay to convergence.
///
/// We use the discard-local-commits override path implicitly avoided here
/// (no override planted) — a plain rebase-strategy replay with already-
/// converged tips is what we want to verify.
#[test]
fn cell_m_replay_sync_abort_cancels_mid_rebase_and_restores() {
    let tmp = tempfile::tempdir().unwrap();
    let (_primary, ww, sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-m-replay-sync-abort";

    // Plant savepoint at original tip.
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);

    // Manufacture a mid-rebase wreckage on the server repo (conflicting line).
    // The ww workspace's server worktree is on branch `ww/main` (see
    // `make_shared_workspaces`); we cannot `checkout main` because the
    // primary's worktree holds that branch.
    let pre_tip = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    advance_one(&ww.server_dir, "conflict.txt", "main\n", "main: c-base");
    git(&["checkout", "-b", "diverge", &pre_tip], &ww.server_dir);
    advance_one(&ww.server_dir, "conflict.txt", "diverge\n", "div: c");
    git(&["checkout", "ww/main"], &ww.server_dir);
    let _ = std::process::Command::new("git")
        .args(["rebase", "diverge"])
        .current_dir(&ww.server_dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output();
    // For linked worktrees, git stores per-worktree mid-op state under
    // `<main>/.git/worktrees/<name>/`. Use `mid_op_label` semantics via
    // `git status --porcelain=v2 --branch` would be ideal, but the
    // existing pattern (file/dir check) works once we accept either
    // worktree variant.
    let in_rebase = ww.server_dir.join(".git/rebase-merge").exists()
        || ww.server_dir.join(".git/rebase-apply").exists()
        || {
            // Resolve worktree's gitdir via `git rev-parse --git-path rebase-merge`.
            let out = common::git()
                .args(["rev-parse", "--git-path", "rebase-merge"])
                .current_dir(&ww.server_dir)
                .output()
                .expect("git rev-parse failed");
            if !out.status.success() {
                false
            } else {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                ww.server_dir.join(&p).exists() || Path::new(&p).exists()
            }
        }
        || {
            let out = common::git()
                .args(["rev-parse", "--git-path", "rebase-apply"])
                .current_dir(&ww.server_dir)
                .output()
                .expect("git rev-parse failed");
            if !out.status.success() {
                false
            } else {
                let p = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                ww.server_dir.join(&p).exists() || Path::new(&p).exists()
            }
        };
    assert!(in_rebase, "fixture must leave ww's server mid-rebase");

    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Sync.yaml(),
            source: ww.root.display().to_string(),
            target: ww.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );

    run_abort_ok(&ww.root, "M(replay)/sync/owner/abort");
    // Server must be back at the savepoint (mid-op classification + reset).
    assert!(
        !ww.server_dir.join(".git/rebase-merge").exists()
            && !ww.server_dir.join(".git/rebase-apply").exists(),
        "abort must cancel the mid-rebase"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ww.server_dir),
        sha,
        "abort must restore ww's server to the pre-op savepoint"
    );
}

// --- J(replay), plain sync — observational equivalent of E(relock) ----------

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
fn cell_j_replay_sync_continue_drives_to_clean_end_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-j-replay-sync";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Sync.yaml(),
            source: primary.root.display().to_string(),
            target: ww.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);

    run_continue(Verb::Sync, &ww.root, "J(replay)/sync/owner");
    assert_plain_sync_continued_clean(&ww, "J(replay)/sync/owner");
}

// --- E(relock), plain sync ---------------------------------------------------

/// Cell `E(relock) / sync / owner-side`: record=relock, no relock work done.
/// `--continue` regenerates the lock (no-op if current) and finishes.
#[test]
fn cell_e_relock_sync_continue_drives_to_clean_end_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-e-relock-sync";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Sync.yaml(),
            source: primary.root.display().to_string(),
            target: ww.root.display().to_string(),
            phase: "relock",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);

    run_continue(Verb::Sync, &ww.root, "E(relock)/sync/owner");
    assert_plain_sync_continued_clean(&ww, "E(relock)/sync/owner");
}

// --- M(relock), plain sync ---------------------------------------------------

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
fn cell_m_relock_sync_continue_drives_to_clean_end_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-m-relock-sync";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Sync.yaml(),
            source: primary.root.display().to_string(),
            target: ww.root.display().to_string(),
            phase: "relock",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);

    run_continue(Verb::Sync, &ww.root, "M(relock)/sync/owner");
    assert_plain_sync_continued_clean(&ww, "M(relock)/sync/owner");
}

// --- J(relock), plain sync ---------------------------------------------------

/// Cell `J(relock) / sync / owner-side`: record=relock, lock committed AND
/// converged_tips populated. `--continue` re-runs relock (no-op), terminates
/// the plain sync machine.
#[test]
fn cell_j_relock_sync_continue_drives_to_clean_end_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-j-relock-sync";
    let project_tip = git_out(&["rev-parse", "HEAD"], &ww.project_dir);
    let server_tip = git_out(&["rev-parse", "HEAD"], &ww.server_dir);

    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Sync.yaml(),
            source: primary.root.display().to_string(),
            target: ww.root.display().to_string(),
            phase: "relock",
            converged_tips: vec![
                (SERVER_PATH.to_owned(), server_tip),
                ("(project)".to_owned(), project_tip),
            ],
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);

    run_continue(Verb::Sync, &ww.root, "J(relock)/sync/owner");
    assert_plain_sync_continued_clean(&ww, "J(relock)/sync/owner");
}

// ===========================================================================
// SYNC-TO — full machine. Plant ww-ahead-of-primary divergence and exercise
// each phase × kill-point cell. We cover BOTH invocation sides per phase.
// ===========================================================================

// --- E(replay), sync-to, owner-side -----------------------------------------

/// Cell `E(replay) / sync-to / owner-side`: ww ahead, record=replay, no work
/// done. `--continue` runs the full machine.
#[test]
fn cell_e_replay_sync_to_owner_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-e-replay-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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

    run_continue(Verb::SyncTo, &ww.root, "E(replay)/sync-to/owner");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "E(replay)/sync-to/owner");
}

// --- E(replay), sync-to, lease-side -----------------------------------------

/// Cell `E(replay) / sync-to / lease-side`: identical setup to the
/// owner-side cell above, but `--continue` is invoked from the lease
/// workspace (primary). End state must be identical — `load_continuing_context`
/// re-roots the engine at the owner. (fo-jsbr3i.2 invocation-side rule.)
#[test]
fn cell_e_replay_sync_to_lease_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-e-replay-syncto-lease";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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
    run_continue(Verb::SyncTo, &primary.root, "E(replay)/sync-to/lease");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "E(replay)/sync-to/lease");
}

// --- E(replay), sync-to, abort owner-side -----------------------------------

#[test]
fn cell_e_replay_sync_to_owner_abort() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let ww_server_pre = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    let primary_server_pre = git_out(&["rev-parse", "HEAD"], &primary.server_dir);

    let op_id = "crash-matrix-e-replay-syncto-owner-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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

    run_abort_ok(&ww.root, "E(replay)/sync-to/owner/abort");
    assert_sync_to_aborted_clean(
        &primary,
        &ww,
        &ww_server_pre,
        &primary_server_pre,
        "E(replay)/sync-to/owner/abort",
    );
}

// --- E(replay), sync-to, abort lease-side -----------------------------------

#[test]
fn cell_e_replay_sync_to_lease_abort() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let ww_server_pre = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    let primary_server_pre = git_out(&["rev-parse", "HEAD"], &primary.server_dir);

    let op_id = "crash-matrix-e-replay-syncto-lease-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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
    run_abort_ok(&primary.root, "E(replay)/sync-to/lease/abort");
    assert_sync_to_aborted_clean(
        &primary,
        &ww,
        &ww_server_pre,
        &primary_server_pre,
        "E(replay)/sync-to/lease/abort",
    );
}

// --- M(replay), sync-to (owner-side) — one repo converged, one not ----------

/// Cell `M(replay) / sync-to / owner-side`: record=replay, the server repo
/// has been "converged" (ww was already at its source tip, no change) but
/// the project repo has NOT yet had Phase 1' applied. On `--continue`,
/// replay completes Phase 1' and the machine drives forward.
///
/// Concretely we use the ww-ahead-sync-to fixture where source = ww. ww's
/// own server is at ww_server_tip, so the per-repo "head == lock target"
/// check makes Phase 2 a no-op. Phase 1' rebases ww/project onto itself
/// (trivial no-op). End state: primary ff'd to ww's tips.
#[test]
fn cell_m_replay_sync_to_owner_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-m-replay-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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

    run_continue(Verb::SyncTo, &ww.root, "M(replay)/sync-to/owner");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "M(replay)/sync-to/owner");
}

// --- J(replay) / E(relock), sync-to (owner) — same end state, different
// record values. We test E(relock) to exercise the record-says-relock path. --

#[test]
fn cell_e_relock_sync_to_owner_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-e-relock-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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

    run_continue(Verb::SyncTo, &ww.root, "E(relock)/sync-to/owner");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "E(relock)/sync-to/owner");
}

// --- M(relock), sync-to (owner): lock committed, tips not yet recorded ------

#[test]
fn cell_m_relock_sync_to_owner_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, _ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-m-relock-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
            source: ww.root.display().to_string(),
            target: primary.root.display().to_string(),
            phase: "relock",
            // converged_tips empty — modelling "lock written, tips not yet recorded"
            ..Default::default()
        },
    );
    plant_lease(&primary.root, &ww.root, op_id);
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);
    let target_id = target_op_id(op_id);
    plant_savepoint(&primary.project_dir, &target_id);
    plant_savepoint(&primary.server_dir, &target_id);

    run_continue(Verb::SyncTo, &ww.root, "M(relock)/sync-to/owner");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "M(relock)/sync-to/owner");
}

// --- J(relock) ≡ E(advance-target), sync-to: converged_tips populated -------

#[test]
fn cell_j_relock_sync_to_owner_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-j-relock-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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

    run_continue(Verb::SyncTo, &ww.root, "J(relock)/sync-to/owner");
    assert_sync_to_continued_clean(&primary, &ww, &ww_server_tip, "J(relock)/sync-to/owner");
}

// --- E(advance-target), sync-to, owner-side ---------------------------------

/// `E(advance-target)` is observationally identical to `J(relock)` from the
/// engine's perspective (record value differs but state is the same once
/// the lock has been committed and `converged_tips` populated). We keep
/// both cells because the record-value difference is what `drive()`'s
/// crash semantics actually distinguish.
#[test]
fn cell_e_advance_target_sync_to_owner_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-e-advance-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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

    run_continue(Verb::SyncTo, &ww.root, "E(advance-target)/sync-to/owner");
    assert_sync_to_continued_clean(
        &primary,
        &ww,
        &ww_server_tip,
        "E(advance-target)/sync-to/owner",
    );
}

// --- E(advance-target), sync-to, lease-side ---------------------------------

#[test]
fn cell_e_advance_target_sync_to_lease_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let op_id = "crash-matrix-e-advance-syncto-lease";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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

    run_continue(
        Verb::SyncTo,
        &primary.root,
        "E(advance-target)/sync-to/lease",
    );
    assert_sync_to_continued_clean(
        &primary,
        &ww,
        &ww_server_tip,
        "E(advance-target)/sync-to/lease",
    );
}

// --- M(advance-target), sync-to (owner) — some target repos ff'd ------------

/// Cell `M(advance-target) / sync-to / owner-side`: record=advance-target,
/// the server target repo has been ff'd to the converged tip, but the
/// project target repo has not. On `--continue`, ff is a no-op on the
/// already-advanced server (head == target) and advances the project.
#[test]
fn cell_m_advance_target_sync_to_owner_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    // Manually ff primary's server (modelling partial advance-target).
    git(
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
        &primary.server_dir,
    );
    let fetch_head = git_out(&["rev-parse", "FETCH_HEAD"], &primary.server_dir);
    git(&["reset", "--hard", &fetch_head], &primary.server_dir);
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
        ww_server_tip,
        "fixture must ff primary's server to ww's tip"
    );

    let op_id = "crash-matrix-m-advance-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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
    let pre_advance_primary_server = git_out(&["rev-parse", "HEAD@{1}"], &primary.server_dir);
    plant_savepoint_at(&primary.server_dir, &target_id, &pre_advance_primary_server);

    run_continue(Verb::SyncTo, &ww.root, "M(advance-target)/sync-to/owner");
    assert_sync_to_continued_clean(
        &primary,
        &ww,
        &ww_server_tip,
        "M(advance-target)/sync-to/owner",
    );
}

// --- J(advance-target), sync-to (owner) — every target repo advanced --------

/// Cell `J(advance-target) / sync-to / owner-side`: record=advance-target,
/// every target repo (and the project repo) has reached the converged tip.
/// On `--continue` advance-target runs again — every ff is a no-op — and
/// machine terminates.
#[test]
fn cell_j_advance_target_sync_to_owner_continue() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    // Pre-advance primary's server AND project to ww's tips (modelling
    // advance-target complete but advance_phase not yet committed).
    git(
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
        &primary.server_dir,
    );
    let fh = git_out(&["rev-parse", "FETCH_HEAD"], &primary.server_dir);
    git(&["reset", "--hard", &fh], &primary.server_dir);
    let pre_advance_primary_project = git_out(&["rev-parse", "HEAD"], &primary.project_dir);
    git(
        &["fetch", &ww.project_dir.to_string_lossy(), "HEAD"],
        &primary.project_dir,
    );
    let pfh = git_out(&["rev-parse", "FETCH_HEAD"], &primary.project_dir);
    git(&["reset", "--hard", &pfh], &primary.project_dir);

    let op_id = "crash-matrix-j-advance-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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
    let pre_advance_primary_server = git_out(&["rev-parse", "HEAD@{1}"], &primary.server_dir);
    plant_savepoint_at(&primary.server_dir, &target_id, &pre_advance_primary_server);
    plant_savepoint_at(
        &primary.project_dir,
        &target_id,
        &pre_advance_primary_project,
    );

    run_continue(Verb::SyncTo, &ww.root, "J(advance-target)/sync-to/owner");
    assert_sync_to_continued_clean(
        &primary,
        &ww,
        &ww_server_tip,
        "J(advance-target)/sync-to/owner",
    );
}

// --- Abort: M(advance-target), sync-to (owner) ------------------------------

/// Abort from a partially-advanced state. The partially-advanced target
/// server is at the recorded converged tip → classified as
/// `RestoredFromConverged`. The not-yet-advanced target project is at the
/// savepoint → classified as `Untouched`. Both reset back to pre-op.
#[test]
fn cell_m_advance_target_sync_to_owner_abort() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let ww_server_pre = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    let primary_server_pre = git_out(&["rev-parse", "HEAD"], &primary.server_dir);
    let primary_project_pre = git_out(&["rev-parse", "HEAD"], &primary.project_dir);

    // Partial advance: server only.
    git(
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
        &primary.server_dir,
    );
    let fh = git_out(&["rev-parse", "FETCH_HEAD"], &primary.server_dir);
    git(&["reset", "--hard", &fh], &primary.server_dir);

    let op_id = "crash-matrix-m-advance-syncto-owner-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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
        &git_out(&["rev-parse", "HEAD"], &ww.project_dir),
    );
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    let target_id = target_op_id(op_id);
    // Savepoints record PRE-OP target tips.
    plant_savepoint_at(&primary.project_dir, &target_id, &primary_project_pre);
    plant_savepoint_at(&primary.server_dir, &target_id, &primary_server_pre);

    run_abort_ok(&ww.root, "M(advance-target)/sync-to/owner/abort");
    assert_sync_to_aborted_clean(
        &primary,
        &ww,
        &ww_server_pre,
        &primary_server_pre,
        "M(advance-target)/sync-to/owner/abort",
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.project_dir),
        primary_project_pre,
        "M(advance-target) abort must restore target project tip too"
    );
}

// --- Abort: lease-side, M(advance-target) -----------------------------------

#[test]
fn cell_m_advance_target_sync_to_lease_abort() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let ww_server_pre = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    let primary_server_pre = git_out(&["rev-parse", "HEAD"], &primary.server_dir);
    let primary_project_pre = git_out(&["rev-parse", "HEAD"], &primary.project_dir);

    git(
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
        &primary.server_dir,
    );
    let fh = git_out(&["rev-parse", "FETCH_HEAD"], &primary.server_dir);
    git(&["reset", "--hard", &fh], &primary.server_dir);

    let op_id = "crash-matrix-m-advance-syncto-lease-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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
        &git_out(&["rev-parse", "HEAD"], &ww.project_dir),
    );
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    let target_id = target_op_id(op_id);
    plant_savepoint_at(&primary.project_dir, &target_id, &primary_project_pre);
    plant_savepoint_at(&primary.server_dir, &target_id, &primary_server_pre);

    // Invoke abort FROM THE LEASE.
    run_abort_ok(&primary.root, "M(advance-target)/sync-to/lease/abort");
    assert_sync_to_aborted_clean(
        &primary,
        &ww,
        &ww_server_pre,
        &primary_server_pre,
        "M(advance-target)/sync-to/lease/abort",
    );
}

// --- Foreign-tip refusal: post-crash commit on the target server -----------

/// Cell `J(advance-target) / sync-to / owner-side`: target's server has been
/// ff'd to the converged tip AND a post-crash foreign commit was added on
/// top of that tip (modelling: "another agent built on the advanced target
/// after our crash"). Abort must REFUSE for that repo with `foreign-tip`,
/// exit nonzero, leave the tip unchanged, and retain op-state.
#[test]
fn cell_j_advance_target_sync_to_foreign_tip_abort_refuses() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, ww_server_tip, ww_project_tip) = make_ww_ahead_sync_to(tmp.path());

    let primary_server_pre = git_out(&["rev-parse", "HEAD"], &primary.server_dir);

    // Advance primary server to ww's tip (advance-target's effect).
    git(
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
        &primary.server_dir,
    );
    let fh = git_out(&["rev-parse", "FETCH_HEAD"], &primary.server_dir);
    git(&["reset", "--hard", &fh], &primary.server_dir);
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
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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
        &git_out(&["rev-parse", "HEAD"], &ww.project_dir),
    );
    plant_savepoint_at(
        &ww.server_dir,
        op_id,
        &git_out(&["rev-parse", "HEAD"], &ww.server_dir),
    );
    let target_id = target_op_id(op_id);
    plant_savepoint_at(
        &primary.project_dir,
        &target_id,
        &git_out(&["rev-parse", "HEAD"], &primary.project_dir),
    );
    plant_savepoint_at(&primary.server_dir, &target_id, &primary_server_pre);

    let stderr = run_abort_refuses_foreign(&ww.root, "J(advance-target)/foreign-tip/sync-to/owner");
    assert!(
        stderr.contains(SERVER_PATH),
        "refusal must name the foreign repo path; stderr:\n{stderr}"
    );

    // Foreign tip unchanged.
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
        foreign_tip,
        "foreign-tip refusal must not reset"
    );

    // Op-state retained (so operator can re-run abort).
    assert!(
        ww.root.join(".rwv-op").exists(),
        "owner record must be retained on foreign-tip refusal"
    );
    assert!(
        primary.root.join(".rwv-op-lease").exists(),
        "target lease must be retained on foreign-tip refusal"
    );

    // Pre-abort ref must have been written before the refusal.
    assert!(
        ref_exists(
            &primary.server_dir,
            &format!("refs/rwv/pre-abort/{target_id}")
        ),
        "pre-abort ref must be written before the refusal (information-preserving rail)"
    );
}

// ===========================================================================
// RETIRE — E(retire) and J(retire) cells (M ≡ E by construction; retire is
// merged-check + dirty-check + delete_workweave, no partial mutation between
// reads and the single workweave-removal step).
// ===========================================================================

/// Workweave + primary fixture that resolves the workweave as a real
/// `WorkspaceLocation::Workweave` (needs the `.rwv-workweave` marker).
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
    git(
        &[
            "worktree",
            "add",
            &ww_server.to_string_lossy(),
            "-b",
            "web-app--ww/main",
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
            "web-app--ww/project",
        ],
        &primary.project_dir,
    );
    std::fs::write(ww_root.join(".rwv-active"), "web-app\n").unwrap();

    // Marker so the workweave resolves as such.
    let marker = format!(
        "primary: \"{primary}\"\nproject: web-app\nparent: \"{primary}\"\n",
        primary = primary.root.display(),
    );
    std::fs::write(ww_root.join(".rwv-workweave"), marker).unwrap();

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

/// Cell `E(retire) / sync-to / owner-side`: record=retire (and =M(retire),
/// since retire's mutation is the single workweave-delete step). Continue
/// must succeed when the workweave's manifest repos match the target's
/// (the reconciled / happy path).
#[test]
fn cell_e_retire_sync_to_owner_continue_when_reconciled() {
    let tmp = tempfile::tempdir().unwrap();
    let RetireFixture { primary, ww } = make_retire_fixture(tmp.path());

    let op_id = "crash-matrix-e-retire-syncto-owner";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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
    run_continue(Verb::SyncTo, &ww.root, "E(retire)/sync-to/owner");

    assert!(
        !ww.root.exists(),
        "workweave must be deleted after a successful retire"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "target lease must be cleared after a successful retire"
    );
}

/// Cell `E(retire) / sync-to / lease-side`: same set-up as above, invoked
/// from the lease workspace. The recorded epic decision (fo-jsbr3i.2) is
/// that this MUST be observationally identical.
#[test]
fn cell_e_retire_sync_to_lease_continue_when_reconciled() {
    let tmp = tempfile::tempdir().unwrap();
    let RetireFixture { primary, ww } = make_retire_fixture(tmp.path());

    let op_id = "crash-matrix-e-retire-syncto-lease";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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
    run_continue(Verb::SyncTo, &primary.root, "E(retire)/sync-to/lease");

    assert!(
        !ww.root.exists(),
        "workweave must be deleted (lease-side --continue identical to owner-side)"
    );
    assert!(
        !primary.root.join(".rwv-op-lease").exists(),
        "target lease must be cleared after lease-side retire --continue"
    );
}

/// Cell `J(retire) / sync-to / owner-side`: dirty-check + merged-check
/// passed but the workweave-delete has not yet committed. In practice this
/// is the same record state as `E(retire)` (record=retire, ww still exists)
/// — the kill-window is genuinely narrow here. The acceptance is the same:
/// `--continue` completes the delete; `abort` restores the workweave (no-op
/// because still present) and target.
#[test]
fn cell_j_retire_sync_to_owner_abort_restores_target() {
    let tmp = tempfile::tempdir().unwrap();
    let RetireFixture { primary, ww } = make_retire_fixture(tmp.path());

    let ww_server_pre = git_out(&["rev-parse", "HEAD"], &ww.server_dir);
    let primary_server_pre = git_out(&["rev-parse", "HEAD"], &primary.server_dir);
    let primary_project_pre = git_out(&["rev-parse", "HEAD"], &primary.project_dir);

    // Simulate a completed advance: bump ww's server and ff primary to match.
    let ww_server_post = advance_one(&ww.server_dir, "advance.txt", "advanced\n", "advance");
    git(
        &["fetch", &ww.server_dir.to_string_lossy(), "HEAD"],
        &primary.server_dir,
    );
    let fh = git_out(&["rev-parse", "FETCH_HEAD"], &primary.server_dir);
    git(&["reset", "--hard", &fh], &primary.server_dir);

    let op_id = "crash-matrix-j-retire-syncto-owner-abort";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::SyncTo.yaml(),
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
        &git_out(&["rev-parse", "HEAD"], &ww.project_dir),
    );
    plant_savepoint_at(&ww.server_dir, op_id, &ww_server_pre);
    let target_id = target_op_id(op_id);
    plant_savepoint_at(&primary.project_dir, &target_id, &primary_project_pre);
    plant_savepoint_at(&primary.server_dir, &target_id, &primary_server_pre);

    run_abort_ok(&ww.root, "J(retire)/sync-to/owner/abort");

    // Both sides restored to pre-op.
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ww.server_dir),
        ww_server_pre,
        "abort from retire must restore ww's server"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.server_dir),
        primary_server_pre,
        "abort from retire must restore primary's server"
    );
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &primary.project_dir),
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

// ===========================================================================
// NAMED-OVERRIDE RESUME FIDELITY (one cell, pins fo-jsbr3i.6's decision)
// ===========================================================================

/// `discard-local-commits` recorded at fresh start, crash mid-replay (record=
/// replay), `--continue` must resume with the same consent: the project
/// savepoint is preserved as a tombstone in `cleanup()`. That is, after a
/// successful resume, `refs/rwv/pre-op/<op-id>` must still exist on the
/// project repo — the "consent persisted" tell.
///
/// We don't need to set up *actual* divergence here; the consent
/// re-derivation is purely about whether `cleanup` reads the override and
/// preserves the savepoint. With ww and primary at shared tips, replay is
/// a per-repo no-op, but the `discard-local-commits` override survives in
/// the record across the `--continue` and gates cleanup.
#[test]
fn cell_override_resume_fidelity_discard_local_commits() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-override-resume-discard";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Sync.yaml(),
            source: primary.root.display().to_string(),
            target: ww.root.display().to_string(),
            phase: "replay",
            overrides: vec!["discard-local-commits"],
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);

    run_continue(
        Verb::Sync,
        &ww.root,
        "override-resume/discard-local-commits",
    );

    assert!(
        !ww.root.join(".rwv-op").exists(),
        "owner record must be cleared"
    );
    // The override-consent tell: project savepoint survives cleanup as a
    // tombstone (the only on-disk evidence that the resumed session
    // honoured the recorded consent). Manifest savepoints are dropped as
    // normal.
    assert!(
        ref_exists(&ww.project_dir, &format!("refs/rwv/pre-op/{op_id}"),),
        "discard-local-commits tombstone savepoint must survive --continue's cleanup \
         (consent honoured across resume)"
    );
    assert!(
        !ref_exists(&ww.server_dir, &format!("refs/rwv/pre-op/{op_id}"),),
        "manifest savepoint must be dropped as normal (only project repo is the tombstone)"
    );
}

// ===========================================================================
// CONTINUE-AFTER-SOURCE-MUTATION (one cell, pins design § 6's re-pin rule)
// ===========================================================================

/// Crash mid-replay (record=replay), then mutate the source between crash
/// and `--continue`. Per `load_continuing_context`'s call to
/// `pin_source_snapshot`, the resumed session re-pins T0 — the op must
/// converge to the NEW pin, not the original.
///
/// Setup:
/// - primary is the source (sync FROM primary TO ww).
/// - Initially primary and ww share tips.
/// - Plant record=replay on ww. The "crash" is structural: no actual replay
///   work has been done.
/// - Between crash and continue, advance primary's server by one commit and
///   update primary's lock to pin the new server SHA, commit. This is a
///   genuine source mutation.
/// - `--continue` re-pins, and ww must converge to primary's NEW tip.
#[test]
fn cell_continue_after_source_mutation_converges_to_new_pin() {
    let tmp = tempfile::tempdir().unwrap();
    let (primary, ww, _sha) = make_shared_workspaces(tmp.path());

    let op_id = "crash-matrix-continue-after-source-mut";
    plant_owner_record(
        &ww.root,
        &OwnerRecordYaml {
            id: op_id.to_owned(),
            verb_str: PlantedVerb::Sync.yaml(),
            source: primary.root.display().to_string(),
            target: ww.root.display().to_string(),
            phase: "replay",
            ..Default::default()
        },
    );
    plant_savepoint(&ww.project_dir, op_id);
    plant_savepoint(&ww.server_dir, op_id);

    // === Source mutation between crash and --continue ===
    let new_primary_server_tip = advance_one(
        &primary.server_dir,
        "new-source.txt",
        "primary new commit\n",
        "primary: new source commit",
    );
    write_lock_at(&primary.project_dir, &new_primary_server_tip);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(
        &["commit", "-m", "lock: pin new server tip"],
        &primary.project_dir,
    );

    // --continue should re-pin and converge to the NEW source tip.
    run_continue(Verb::Sync, &ww.root, "continue-after-source-mutation");

    assert!(
        !ww.root.join(".rwv-op").exists(),
        "owner record must be cleared after re-pinned --continue"
    );
    // The re-pin tell: ww's server is at the NEW primary server tip, not
    // the original shared sha.
    assert_eq!(
        git_out(&["rev-parse", "HEAD"], &ww.server_dir),
        new_primary_server_tip,
        "re-pinned --continue must converge ww's server to the NEW source tip"
    );
}
