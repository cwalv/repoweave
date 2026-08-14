//! Tests for `rwv doctor`'s state-hygiene checks.
//!
//! Exercises the three check kinds:
//!
//!   1. `stale-worktree-registration` — registered worktree whose path is
//!      gone. `--fix` runs `git worktree prune` (information-preserving:
//!      the only state dropped is a pointer to an already-missing dir).
//!   2. `stale-op-state` — a `.rwv-op` file is present at a workspace
//!      root. **Report-only forever** — never auto-fixed.
//!   3. `orphaned-savepoint` — a `refs/rwv/pre-op/<op-id>` ref whose
//!      op-id is not present in any live `.rwv-op` file. Classified
//!      into:
//!        - `redundant` (savepoint tip reachable from current HEAD) →
//!          `--fix` may drop.
//!        - `live` (savepoint tip not reachable) → `--fix` must NOT
//!          drop; the ref is the last anchor for unreachable commits.
//!
//! Each kind has a fixture test asserting the report shape, and the
//! `--fix` behaviour (where applicable) is exercised plus its
//! idempotence checked.

use assert_cmd::Command;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a minimal workspace directory with `github/` and `projects/`.
/// Returns the workspace root path.
fn make_workspace(parent: &Path, name: &str) -> PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github")).unwrap();
    std::fs::create_dir_all(root.join("projects")).unwrap();
    root
}

/// Initialise a git repo at `path` with a single commit so HEAD exists.
/// Returns the SHA of that commit.
fn init_git_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    let run = |args: &[&str], dir: &Path| {
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
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    run(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    run(&["add", "."], path);
    run(&["commit", "-m", "initial"], path);
    run(&["rev-parse", "HEAD"], path)
}

/// Add an empty commit and return its SHA.
fn add_empty_commit(path: &Path, msg: &str) -> String {
    let out = common::git()
        .args(["commit", "--allow-empty", "-m", msg])
        .current_dir(path)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .output()
        .expect("git commit failed to start");
    assert!(
        out.status.success(),
        "git commit --allow-empty failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = common::git()
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Write a minimal `rwv.toml` listing a single repo.
fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    std::fs::create_dir_all(project_dir).unwrap();
    let mut manifest_toml = String::from("[repositories]\n");
    for (repo_path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
}

/// Add a `refs/rwv/pre-op/<op_id>` savepoint pointing at the given SHA.
fn add_savepoint(repo: &Path, op_id: &str, sha: &str) {
    let out = common::git()
        .args(["update-ref", &format!("refs/rwv/pre-op/{op_id}"), sha])
        .current_dir(repo)
        .output()
        .expect("git update-ref failed to start");
    assert!(
        out.status.success(),
        "git update-ref failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Read whether a savepoint ref currently resolves.
fn savepoint_exists(repo: &Path, op_id: &str) -> bool {
    common::git()
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/rwv/pre-op/{op_id}"),
        ])
        .current_dir(repo)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Register a worktree at `wt_path` and immediately delete its on-disk
/// directory so the registration is `prunable` per
/// `git worktree list --porcelain`.
fn make_stale_worktree(repo: &Path, wt_path: &Path) {
    let wt_str = wt_path.to_str().unwrap();
    let out = common::git()
        .args(["worktree", "add", "-q", wt_str, "-b", "stale-wt-branch"])
        .current_dir(repo)
        .output()
        .expect("git worktree add failed to start");
    assert!(
        out.status.success(),
        "git worktree add failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Hand-delete the directory so the registration is left behind.
    std::fs::remove_dir_all(wt_path).unwrap();
}

/// Build an `rwv` `Command` whose `current_dir` is set so tests never
/// accidentally pick up the real workspace.
fn rwv_cmd() -> Command {
    let mut cmd = common::rwv();
    cmd.current_dir(std::env::temp_dir());
    cmd
}

/// Write a `.rwv-op` v2 owner record by hand (sync.rs writes the same shape,
/// but we don't want to drive a real sync to produce one).
fn write_op_state(workspace_dir: &Path, op_id: &str) {
    write_op_state_for_verb(workspace_dir, op_id, "sync");
}

/// As [`write_op_state`], for an op started by `verb`.
fn write_op_state_for_verb(workspace_dir: &Path, op_id: &str, verb: &str) {
    let json = format!(
        "{{\"id\": \"{op_id}\", \"verb\": \"{verb}\", \"strategy\": \"rebase\", \
         \"source\": \"/tmp/src\", \"target\": \"/tmp/tgt\", \"retire\": false, \
         \"phase\": \"replay\", \"advanced_tips\": {{}}, \"converged_tips\": {{}}, \
         \"overrides\": [], \"started_at\": \"2026-01-01T00:00:00Z\"}}",
    );
    std::fs::write(workspace_dir.join(".rwv-op"), json).unwrap();
}

// ===========================================================================
// 1. stale-worktree-registration
// ===========================================================================

/// Scenario: a manifest repo has a worktree registration whose on-disk
/// directory has been hand-deleted. `rwv doctor` must report
/// stale-worktree-registration; `--fix` must call `git worktree prune`
/// and a re-run must be clean (idempotent).
#[test]
fn stale_worktree_registration_reported_and_fixable() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_rel = "github/acme/server";
    let repo_abs = root.join(repo_rel);
    init_git_repo(&repo_abs);

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_rel, "https://github.com/acme/server.git")],
    );

    // Register a worktree, then delete its directory so the registration
    // is `prunable` per `git worktree list --porcelain`.
    let stale_wt = tmp.path().join("stale-wt");
    make_stale_worktree(&repo_abs, &stale_wt);

    // Without --fix: doctor reports the stale registration.
    let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("stale-worktree-registration"),
        "doctor should report stale-worktree-registration; got:\n{stdout}"
    );

    // With --fix: doctor prunes the registration.
    let out = rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[fixed]") && stdout.contains("stale-worktree-registration"),
        "doctor --fix should announce the prune; got:\n{stdout}"
    );

    // Re-run: clean (idempotent).
    let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("stale-worktree-registration"),
        "after --fix, doctor should not re-report; got:\n{stdout}"
    );
}

// ===========================================================================
// 2. stale-op-state (report-only forever)
// ===========================================================================

/// A `.rwv-op` file at the workspace root is reported. `--fix` does NOT
/// remove it — the file may belong to a sync in flight in another
/// terminal that needs to roll back.
#[test]
fn stale_op_state_reported_untouched_by_fix() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_rel = "github/acme/server";
    init_git_repo(&root.join(repo_rel));
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_rel, "https://github.com/acme/server.git")],
    );

    // Hand-write a `.rwv-op` file with a synthetic op_id.
    write_op_state(&root, "9999999999999999999");

    // Without --fix: doctor reports stale-op-state.
    let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("stale-op-state"),
        "doctor should report stale-op-state; got:\n{stdout}"
    );
    assert!(
        stdout.contains("rwv abort") || stdout.contains("--continue"),
        "report should mention the resolution path; got:\n{stdout}"
    );

    // With --fix: the file is left in place and the report still fires.
    let out = rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("stale-op-state"),
        "doctor --fix must still report stale-op-state (never auto-fixed); got:\n{stdout}"
    );
    assert!(
        root.join(".rwv-op").exists(),
        "doctor --fix must not delete the .rwv-op file"
    );
}

/// Every verb the published doctor schema admits on a stale-op-state
/// finding, read out of the generated artifact rather than restated here.
fn published_op_verbs() -> Vec<String> {
    let schema_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/reference/schemas/doctor.json");
    let schema: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&schema_path).expect("read doctor schema"))
            .expect("parse doctor schema");
    schema["definitions"]["OpVerb"]["oneOf"]
        .as_array()
        .expect("doctor schema must define OpVerb as a oneOf")
        .iter()
        .map(|variant| {
            variant["enum"][0]
                .as_str()
                .expect("each OpVerb variant is a single-token string enum")
                .to_owned()
        })
        .collect()
}

/// An op is only resumable under the verb that started it — running
/// `rwv sync --continue` against a stalled `sync-to` earns the verb-mismatch
/// refusal. So the finding has to name the record's own verb, and must not
/// name any other.
#[test]
fn stale_op_state_hint_names_the_verb_that_started_the_op() {
    let verbs = published_op_verbs();
    assert!(
        verbs.len() >= 2,
        "read {} verbs out of the doctor schema — with fewer than two the \
         cross-verb assertion below is vacuous; got {verbs:?}",
        verbs.len()
    );

    for verb in &verbs {
        let tmp = common::tempdir().unwrap();
        let root = make_workspace(tmp.path(), "ws");
        let repo_rel = "github/acme/server";
        init_git_repo(&root.join(repo_rel));
        write_manifest(
            &root.join("projects").join("my-app"),
            &[(repo_rel, "https://github.com/acme/server.git")],
        );
        write_op_state_for_verb(&root, "9999999999999999999", verb);

        let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);

        assert!(
            stdout.contains(&format!("`rwv {verb} --continue`")),
            "a stalled `{verb}` op must be reported with its own resume \
             command; got:\n{stdout}"
        );
        for other in verbs.iter().filter(|o| *o != verb) {
            assert!(
                !stdout.contains(&format!("`rwv {other} --continue`")),
                "a stalled `{verb}` op must not be reported with `{other}`'s \
                 resume command; got:\n{stdout}"
            );
        }
    }
}

// ===========================================================================
// 3. orphaned-savepoint — Redundant (safe class)
// ===========================================================================

/// A savepoint ref pointing at the current HEAD (so the tip is reachable
/// from the current branch — trivially redundant). `--fix` drops the
/// ref; a re-run is clean.
#[test]
fn orphaned_savepoint_redundant_fixable() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_rel = "github/acme/server";
    let repo_abs = root.join(repo_rel);
    let head_sha = init_git_repo(&repo_abs);

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_rel, "https://github.com/acme/server.git")],
    );

    // Savepoint at the current HEAD → reachable from HEAD → Redundant.
    let op_id = "111111111111111111";
    add_savepoint(&repo_abs, op_id, &head_sha);

    // Without --fix: reported as redundant.
    let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("orphaned-savepoint"),
        "doctor should report orphaned-savepoint; got:\n{stdout}"
    );
    assert!(
        stdout.contains("redundant") || stdout.contains("safe to --fix"),
        "redundant orphaned-savepoint should be marked safe to --fix; got:\n{stdout}"
    );
    assert!(
        savepoint_exists(&repo_abs, op_id),
        "savepoint must still exist before --fix"
    );

    // With --fix: dropped.
    let out = rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("[fixed]") && stdout.contains("orphaned-savepoint"),
        "doctor --fix should drop the redundant savepoint; got:\n{stdout}"
    );
    assert!(
        !savepoint_exists(&repo_abs, op_id),
        "savepoint must be gone after --fix"
    );

    // Re-run: clean (idempotent).
    let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("orphaned-savepoint"),
        "after --fix, no orphaned-savepoint should remain; got:\n{stdout}"
    );
}

// ===========================================================================
// 4. orphaned-savepoint — Live (unreachable, MUST be kept)
// ===========================================================================

/// A savepoint pointing at a commit no longer reachable from the current
/// branch (the branch was reset back, leaving the savepoint as the only
/// pointer to the discarded work). `--fix` must NOT drop it.
#[test]
fn orphaned_savepoint_live_is_kept_under_fix() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_rel = "github/acme/server";
    let repo_abs = root.join(repo_rel);
    let first_sha = init_git_repo(&repo_abs);

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_rel, "https://github.com/acme/server.git")],
    );

    // Advance HEAD past `first_sha`, then create a savepoint at the new
    // commit and reset HEAD back so the savepoint tip is no longer
    // reachable from the current branch.
    let later_sha = add_empty_commit(&repo_abs, "later");
    let op_id = "222222222222222222";
    add_savepoint(&repo_abs, op_id, &later_sha);
    // Move main back to first_sha, leaving the savepoint pointing at an
    // unreachable commit.
    let out = common::git()
        .args(["update-ref", "refs/heads/main", &first_sha])
        .current_dir(&repo_abs)
        .output()
        .unwrap();
    assert!(out.status.success());

    // Sanity: savepoint exists and tip is NOT an ancestor of HEAD.
    assert!(savepoint_exists(&repo_abs, op_id));
    let is_anc = common::git()
        .args(["merge-base", "--is-ancestor", &later_sha, &first_sha])
        .current_dir(&repo_abs)
        .status()
        .map(|s| s.success())
        .unwrap_or(true);
    assert!(
        !is_anc,
        "test setup invariant: savepoint tip must NOT be reachable from HEAD"
    );

    // Without --fix: reported as live (not safe-to-fix).
    let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("orphaned-savepoint"),
        "live orphaned-savepoint should still be reported; got:\n{stdout}"
    );
    assert!(
        stdout.contains("no live ref anchors") && stdout.contains("report-only"),
        "live finding should warn the operator that the ref is load-bearing \
         and that `--fix` will not touch it; got:\n{stdout}"
    );

    // With --fix: the savepoint MUST NOT be dropped (it's the last pointer
    // to the unreachable commit).
    let _ = rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        savepoint_exists(&repo_abs, op_id),
        "doctor --fix must NOT drop a live (unreachable-tip) savepoint"
    );

    // Re-run reports the same finding (no false-positive escalation).
    let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("orphaned-savepoint"),
        "live orphaned-savepoint must continue to be reported after --fix; got:\n{stdout}"
    );
}

// ===========================================================================
// 5. Combined --fix idempotence across all three kinds
// ===========================================================================

/// All three check kinds firing at once: a stale worktree registration, a
/// live `.rwv-op` file, a redundant savepoint, and a live savepoint. A
/// single `--fix` pass must:
///   - Prune the stale worktree.
///   - Drop the redundant savepoint.
///   - Leave the `.rwv-op` and the live savepoint untouched.
///
/// A second `--fix` pass must be a no-op (idempotent).
#[test]
fn fix_is_idempotent_across_all_three_kinds() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_rel = "github/acme/server";
    let repo_abs = root.join(repo_rel);
    let head_sha = init_git_repo(&repo_abs);

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_rel, "https://github.com/acme/server.git")],
    );

    // (a) Stale worktree registration.
    let stale_wt = tmp.path().join("stale-wt");
    make_stale_worktree(&repo_abs, &stale_wt);

    // (b) Live .rwv-op file.
    write_op_state(&root, "9999999999999999999");

    // (c) Redundant savepoint (tip == HEAD).
    let redundant_op = "111111111111111111";
    add_savepoint(&repo_abs, redundant_op, &head_sha);

    // (d) Live (unreachable-tip) savepoint.
    let later = add_empty_commit(&repo_abs, "later");
    let live_op = "222222222222222222";
    add_savepoint(&repo_abs, live_op, &later);
    // Reset main back to head_sha so `later` is unreachable.
    let out = common::git()
        .args(["update-ref", "refs/heads/main", &head_sha])
        .current_dir(&repo_abs)
        .output()
        .unwrap();
    assert!(out.status.success());

    // First --fix pass.
    let _ = rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .unwrap();

    // Stale worktree: pruned. The `prunable` line should be gone.
    let out = common::git()
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&repo_abs)
        .output()
        .unwrap();
    let porc = String::from_utf8_lossy(&out.stdout);
    assert!(
        !porc.contains("prunable"),
        "first --fix must prune the stale registration; porcelain:\n{porc}"
    );
    // .rwv-op untouched.
    assert!(
        root.join(".rwv-op").exists(),
        "first --fix must NOT delete .rwv-op"
    );
    // Redundant savepoint gone.
    assert!(
        !savepoint_exists(&repo_abs, redundant_op),
        "first --fix must drop the redundant savepoint"
    );
    // Live savepoint kept.
    assert!(
        savepoint_exists(&repo_abs, live_op),
        "first --fix must keep the live savepoint"
    );

    // Second --fix pass — must be a no-op. The only finding that should
    // surface is the stale-op-state report (still present) and the live
    // orphaned-savepoint (still present); nothing should change on disk.
    let out = rwv_cmd()
        .args(["doctor", "--fix"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("stale-worktree-registration"),
        "stale-worktree-registration should be gone after first --fix; got:\n{stdout}"
    );
    // Live op-state and live savepoint must still surface.
    assert!(
        stdout.contains("stale-op-state"),
        "live op-state must continue to be reported; got:\n{stdout}"
    );
    assert!(
        stdout.contains("orphaned-savepoint"),
        "live orphaned-savepoint must continue to be reported; got:\n{stdout}"
    );
    // And neither was disturbed.
    assert!(root.join(".rwv-op").exists());
    assert!(savepoint_exists(&repo_abs, live_op));
    assert!(!savepoint_exists(&repo_abs, redundant_op));
}

// ===========================================================================
// 6. Savepoint whose op_id matches a live .rwv-op is NOT orphaned
// ===========================================================================

/// A savepoint whose op_id is present in a live `.rwv-op` belongs to a
/// sync that is in flight or paused; doctor must NOT report it as
/// orphaned regardless of the savepoint tip's reachability.
#[test]
fn live_op_state_protects_matching_savepoint() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_rel = "github/acme/server";
    let repo_abs = root.join(repo_rel);
    let head_sha = init_git_repo(&repo_abs);
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_rel, "https://github.com/acme/server.git")],
    );

    let op_id = "555555555555555555";
    write_op_state(&root, op_id);
    add_savepoint(&repo_abs, op_id, &head_sha);

    let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    // stale-op-state is reported (the .rwv-op file is present)…
    assert!(
        stdout.contains("stale-op-state"),
        "stale-op-state must be reported; got:\n{stdout}"
    );
    // …but the matching savepoint must NOT be tagged as orphaned.
    assert!(
        !stdout.contains("orphaned-savepoint"),
        "savepoint whose op_id matches a live .rwv-op must NOT be orphaned; got:\n{stdout}"
    );
}

// ===========================================================================
// 7. JSON channel surfaces the new variants
// ===========================================================================

#[test]
fn json_output_includes_state_hygiene_kinds() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_rel = "github/acme/server";
    let repo_abs = root.join(repo_rel);
    let head_sha = init_git_repo(&repo_abs);
    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_rel, "https://github.com/acme/server.git")],
    );

    // Trip all three kinds.
    let stale_wt = tmp.path().join("stale-wt");
    make_stale_worktree(&repo_abs, &stale_wt);
    write_op_state(&root, "9999999999999999999");
    add_savepoint(&repo_abs, "111111111111111111", &head_sha);

    let out = rwv_cmd()
        .args(["doctor", "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json produced invalid JSON: {e}\noutput: {stdout}"));

    let violations = json["violations"].as_array().expect("violations is array");
    let kinds: Vec<&str> = violations
        .iter()
        .filter_map(|v| v["kind"].as_str())
        .collect();
    assert!(
        kinds.contains(&"stale-worktree-registration"),
        "JSON output must include stale-worktree-registration; kinds: {kinds:?}"
    );
    assert!(
        kinds.contains(&"stale-op-state"),
        "JSON output must include stale-op-state; kinds: {kinds:?}"
    );
    assert!(
        kinds.contains(&"orphaned-savepoint"),
        "JSON output must include orphaned-savepoint; kinds: {kinds:?}"
    );
}

// ===========================================================================
// 6. The per-class count collapse: reclamation/frozen classes render as one
//    text line each; `--json` keeps the full records; everything else stays
//    itemized.
// ===========================================================================

/// A weave with N reclaimable savepoints renders ONE text line carrying the
/// count, while `--json` carries all N records — the wire shape the
/// per-class baseline capture reads (`violations[]` with per-item records)
/// must stay intact.
///
/// **Mutation evidence**: remove `OrphanedSavepointKind::Redundant` from the
/// collapse set (`CollapsedClass::of` in check.rs) and both halves of the
/// first assertion redden — the count line disappears and three itemized
/// savepoint lines reappear.
#[test]
fn reclaimable_savepoints_render_as_one_count_line_with_full_json_records() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_rel = "github/acme/server";
    let repo_abs = root.join(repo_rel);
    let head_sha = init_git_repo(&repo_abs);

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_rel, "https://github.com/acme/server.git")],
    );

    // Three savepoints at HEAD → all redundant (trivially reachable).
    for op_id in ["311111111111111111", "322222222222222222", "333333333333333333"] {
        add_savepoint(&repo_abs, op_id, &head_sha);
    }

    let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("3 redundant orphaned-savepoint findings"),
        "three reclaimable savepoints must render as one count line; got:\n{stdout}"
    );
    let savepoint_lines = stdout
        .lines()
        .filter(|l| l.contains("orphaned-savepoint"))
        .count();
    assert_eq!(
        savepoint_lines, 1,
        "the class renders as exactly one text line, not per-item; got:\n{stdout}"
    );

    // `--json` still carries every record, with per-item identity.
    let json_out = rwv_cmd()
        .args(["doctor", "--json"])
        .current_dir(&root)
        .output()
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&json_out.stdout))
            .expect("doctor --json parses");
    let savepoints: Vec<&serde_json::Value> = json["violations"]
        .as_array()
        .expect("violations is an array")
        .iter()
        .filter(|v| v["kind"] == "orphaned-savepoint")
        .collect();
    assert_eq!(
        savepoints.len(),
        3,
        "--json must carry all three records; got: {json}"
    );
    for record in &savepoints {
        assert!(
            record["op_id"].is_string(),
            "each record keeps its per-item identity; got: {record}"
        );
    }
}

/// A genuinely new finding outside the reclamation/frozen classes stays
/// itemized in the text report, beside the collapsed count lines, with no
/// filter needed — collapsing is for backlog the operator has already
/// judged, never for a fact they have not seen.
#[test]
fn a_new_non_reclamation_finding_stays_itemized_beside_the_count_lines() {
    let tmp = common::tempdir().unwrap();
    let root = make_workspace(tmp.path(), "ws");

    let repo_rel = "github/acme/server";
    let repo_abs = root.join(repo_rel);
    let head_sha = init_git_repo(&repo_abs);

    let project_dir = root.join("projects").join("my-app");
    write_manifest(
        &project_dir,
        &[(repo_rel, "https://github.com/acme/server.git")],
    );

    // A reclaimable backlog that collapses…
    add_savepoint(&repo_abs, "411111111111111111", &head_sha);

    // …and a misnamed workweave directory — a finding class outside the
    // collapse set. The marker is valid and names this primary; the
    // basename's project half disagrees with it.
    let ww_dir = root
        .parent()
        .expect("workspace root has a parent")
        .join(".workweaves")
        .join("other--feat-x");
    std::fs::create_dir_all(&ww_dir).unwrap();
    let primary = root.canonicalize().unwrap();
    std::fs::write(
        ww_dir.join(".rwv-workweave"),
        format!(
            "{{\"primary\":\"{}\",\"project\":\"my-app\",\"parent\":\"{}\"}}",
            primary.display(),
            primary.display()
        ),
    )
    .unwrap();

    let out = rwv_cmd().arg("doctor").current_dir(&root).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("orphaned-savepoint finding"),
        "the reclaimable class collapses to its count line; got:\n{stdout}"
    );
    assert!(
        stdout.contains("disagrees with its records"),
        "the misnamed-dir finding must stay itemized — full message, no \
         filter; got:\n{stdout}"
    );
}
