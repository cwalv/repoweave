//! Tests for `rwv sync --json`, and for `rwv sync-to --json` where it shares
//! the multi-repo fixture.
//!
//! Two layers:
//! 1. End-to-end: drive `rwv sync --json` through the binary, parse stdout
//!    as JSON, assert envelope + per-repo shape and exit code.
//! 2. Library-level snapshot tests of `VcsError` / `SyncFailure` /
//!    `RepoSyncOutcome` -> wire-output serialization, so kebab-case tags
//!    and field names are pinned independent of the live sync path.

use assert_cmd::Command as AssertCommand;
use repoweave::sync::{
    Containment, ContainmentOutput, ContainmentVerdictOutput, ReplayBaseline, RepoSyncOutcome,
    SyncFailure, SyncFailureOutput, SyncJsonOutput, SyncOutcomeOutput, SYNC_JSON_SCHEMA_URL,
    SYNC_TO_JSON_SCHEMA_URL,
};
use repoweave::vcs::{ConflictOp, MidOperation, VcsError, VcsErrorKind, VcsErrorOutput};
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Helpers (mirrors of the helpers in e2e_sync_abort_test.rs; kept local so
// this file doesn't pull on that test file's private fixtures)
// ---------------------------------------------------------------------------

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    common::git_in(path, &["init", "-b", "main"]);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    common::git_in(path, &["add", "."]);
    common::git_in(path, &["commit", "-m", "initial"]);
    common::git_in(path, &["rev-parse", "HEAD"])
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    common::git_in(repo, &["add", filename]);
    common::git_in(repo, &["commit", "-m", msg]);
    common::git_in(repo, &["rev-parse", "HEAD"])
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nversion = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.toml"), &manifest_toml).unwrap();
}

fn rwv() -> AssertCommand {
    common::rwv()
}

const SERVER_URL: &str = "https://github.com/chatly/server.git";
const SERVER_PATH: &str = "github/chatly/server";

struct Workspace {
    root: PathBuf,
    project_dir: PathBuf,
    server_dir: PathBuf,
}

fn make_locked_workspace(parent: &Path, name: &str) -> (Workspace, String) {
    let root = parent.join(name);
    std::fs::create_dir_all(root.join("github/chatly")).unwrap();
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
    write_manifest(&project_dir, &[(SERVER_PATH, SERVER_URL)]);
    common::fixture_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &sha)]);
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

fn make_shared_workspaces(parent: &Path) -> (Workspace, Workspace, String) {
    let (primary, c1) = make_locked_workspace(parent, "owned");

    let ww_root = parent.join("ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
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
// End-to-end: drive `rwv sync --json` through the binary
// ---------------------------------------------------------------------------

#[test]
fn sync_json_emits_envelope_and_outcomes() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Workweave: advance + relock.
    let c2 = make_commit(
        &ww.server_dir,
        "change.txt",
        "workweave change\n",
        "ww: add change",
    );
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww change"]);

    let assert = rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--json"])
        .current_dir(&primary.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not parseable as JSON ({e}):\n{stdout}"));
    let obj = parsed.as_object().expect("top level should be object");
    assert_eq!(
        obj.get("$schema").and_then(Value::as_str),
        Some(SYNC_JSON_SCHEMA_URL),
        "envelope must include $schema URL"
    );
    let outcomes = obj
        .get("outcomes")
        .and_then(Value::as_array)
        .expect("outcomes should be an array");
    assert!(!outcomes.is_empty(), "outcomes should not be empty");
    for o in outcomes {
        let entry = o.as_object().expect("each outcome must be an object");
        assert!(entry.contains_key("kind"), "outcome missing `kind`: {o}");
        assert!(entry.contains_key("path"), "outcome missing `path`: {o}");
        assert!(
            entry.contains_key("absolute_path"),
            "outcome missing `absolute_path`: {o}"
        );
    }

    // Server should have actually advanced to C2.
    let primary_head = common::git_in(&primary.server_dir, &["rev-parse", "main"]);
    assert_eq!(primary_head, c2);
}

#[test]
fn sync_json_failed_outcome_yields_nonzero_exit() {
    // Build a divergence that ff cannot resolve, then run `--json` and
    // expect exit code 1 with a `failed` outcome in the JSON.
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    // Primary: advance to C2.
    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: C2"]);

    // Workweave: diverge.
    let c_ww = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: diverged from C1");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: C_ww"]);

    // ff --discard-local-commits: bypasses the Phase 1 ancestor check;
    // Phase 2 ff still fails.
    let assert = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--json",
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not parseable as JSON ({e}):\n{stdout}"));
    let outcomes = parsed
        .get("outcomes")
        .and_then(Value::as_array)
        .expect("outcomes array");
    let any_failed = outcomes
        .iter()
        .any(|o| o.get("kind").and_then(Value::as_str) == Some("failed"));
    assert!(any_failed, "expected at least one failed outcome: {stdout}");

    // Exit code is 1 (non-zero) per spec.
    assert_eq!(
        assert.get_output().status.code(),
        Some(1),
        "expected exit code 1"
    );
}

#[test]
fn sync_json_no_op_when_already_at_lock() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, c1) = make_shared_workspaces(tmp.path());
    let _ = ww; // unused

    // Primary already at C1 (the lock SHA): running sync ff from primary
    // (using primary as source) should produce no-op for the server repo.
    let assert = rwv()
        .args(["sync", &primary.root.to_string_lossy(), "--json"])
        .current_dir(&primary.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    let parsed: Value = serde_json::from_str(&stdout).expect("parseable");
    let outcomes = parsed
        .get("outcomes")
        .and_then(Value::as_array)
        .expect("outcomes");
    // Expect at least one `no-op` outcome (server already at C1).
    let any_no_op = outcomes.iter().any(|o| {
        o.get("kind").and_then(Value::as_str) == Some("no-op")
            && o.get("path").and_then(Value::as_str) == Some(SERVER_PATH)
    });
    assert!(any_no_op, "expected no-op for server: {stdout}");

    // Sanity: c1 actually was the initial SHA.
    let head = common::git_in(&primary.server_dir, &["rev-parse", "HEAD"]);
    assert_eq!(head, c1);
}

// ---------------------------------------------------------------------------
// Library-level snapshot tests of wire-output serialization
// ---------------------------------------------------------------------------

fn serialize_outcome(path: &str, abs: &str, outcome: &RepoSyncOutcome) -> Value {
    serialize_outcome_decided_from(path, abs, outcome, None)
}

fn serialize_outcome_decided_from(
    path: &str,
    abs: &str,
    outcome: &RepoSyncOutcome,
    containment: Option<ContainmentOutput>,
) -> Value {
    let out = SyncOutcomeOutput::from_outcome(
        path.to_owned(),
        repoweave::path_spelling::WirePath::of(std::path::Path::new(abs)),
        outcome,
        containment,
    );
    serde_json::to_value(&out).unwrap()
}

#[test]
fn outcome_converged_serializes() {
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::Converged {
            derived_content_dropped: Vec::new(),
        },
    );
    assert_eq!(v["kind"], "converged");
    assert_eq!(v["path"], "github/cwalv/foo");
    assert_eq!(v["absolute_path"], "/abs/foo");
}

#[test]
fn outcome_already_ahead_serializes_with_commits_ahead() {
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::AlreadyAhead {
            commits_ahead: 3,
            baseline: ReplayBaseline::SourceLockEntry,
            source_position: Some(Containment::Ahead(5)),
        },
    );
    assert_eq!(v["kind"], "already-ahead");
    assert_eq!(v["commits_ahead"], 3);
}

/// A `--json` consumer branching on `commits_ahead` alone learns that this
/// checkout holds the baseline and nothing about how far the source has moved
/// past it. `source_position` is the field that separates "contains the
/// baseline" from "at the source's tip", and it carries its own count — a
/// consumer reading only the relation would call a 5-commit gap the same as a
/// 1-commit one.
#[test]
fn the_already_ahead_record_carries_the_source_own_position() {
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::AlreadyAhead {
            commits_ahead: 3,
            baseline: ReplayBaseline::SourceLockEntry,
            source_position: Some(Containment::Ahead(5)),
        },
    );
    assert_eq!(v["source_position"]["relation"], "ahead");
    assert_eq!(v["source_position"]["commits"], 5);
    assert_ne!(
        v["source_position"]["commits"], v["commits_ahead"],
        "the two relations are separate counts; a record echoing one into the \
         other reports the gap it exists to disclose as zero"
    );
}

/// An unmeasurable source position is absent, not zero: a consumer that reads
/// a missing field as `Equal` is told the source is at the baseline by a run
/// that could not look.
#[test]
fn an_unmeasured_source_position_is_absent_from_the_record() {
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::AlreadyAhead {
            commits_ahead: 3,
            baseline: ReplayBaseline::SourceLockEntry,
            source_position: None,
        },
    );
    assert!(
        v.get("source_position").is_none(),
        "an unmeasured position must be omitted; got: {v}"
    );
}

#[test]
fn outcome_no_op_serializes() {
    let v = serialize_outcome("github/cwalv/foo", "/abs/foo", &RepoSyncOutcome::NoOp);
    assert_eq!(v["kind"], "no-op");
}

#[test]
fn outcome_failed_head_unreadable_serializes() {
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
            error: "boom".into(),
            cause: None,
        }),
    );
    assert_eq!(v["kind"], "failed");
    assert_eq!(v["failure"]["kind"], "head-unreadable");
    assert_eq!(v["failure"]["message"], "boom");
}

#[test]
fn outcome_failed_ff_impossible_uses_ff_impossible_tag() {
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::Failed(SyncFailure::FastForwardImpossible {
            error: "diverged".into(),
            cause: VcsError::NotARepo(PathBuf::from("/abs/foo")),
        }),
    );
    // Critical: must be `ff-impossible`, not `fast-forward-impossible`.
    assert_eq!(v["failure"]["kind"], "ff-impossible");
}

#[test]
fn outcome_failed_rebase_failed_serializes() {
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::Failed(SyncFailure::RebaseFailed {
            error: "rebase conflict".into(),
            cause: VcsError::NotARepo(PathBuf::from("/abs/foo")),
        }),
    );
    assert_eq!(v["failure"]["kind"], "rebase-failed");
}

#[test]
fn outcome_failed_with_rebase_conflict_cause_surfaces() {
    let cause = VcsError::RebaseConflict {
        repo: PathBuf::from("/abs/foo"),
        op: ConflictOp::Rebase,
    };
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::Failed(SyncFailure::RebaseFailed {
            error: cause.to_string(),
            cause,
        }),
    );
    assert_eq!(v["failure"]["kind"], "rebase-failed");
    let inner = &v["failure"]["cause"];
    assert_eq!(inner["kind"], "rebase-conflict");
    assert_eq!(inner["op"], "rebase");
    assert_eq!(inner["repo"], "/abs/foo");
}

#[test]
fn strategy_failures_always_carry_cause_on_the_wire() {
    for failure in [
        SyncFailure::FastForwardImpossible {
            error: "diverged".into(),
            cause: VcsError::NotARepo(PathBuf::from("/abs/foo")),
        },
        SyncFailure::RebaseFailed {
            error: "rebase stopped".into(),
            cause: VcsError::NotARepo(PathBuf::from("/abs/foo")),
        },
    ] {
        let v = serialize_outcome(
            "github/cwalv/foo",
            "/abs/foo",
            &RepoSyncOutcome::Failed(failure),
        );
        assert_eq!(
            v["failure"]["cause"]["kind"], "not-a-repo",
            "a strategy failure must serialize its cause: {v}"
        );
    }
}

#[test]
fn schema_requires_cause_exactly_on_strategy_failures() {
    let schema = serde_json::to_value(schemars::schema_for!(SyncFailureOutput)).unwrap();
    let variants = schema["oneOf"]
        .as_array()
        .expect("SyncFailureOutput is a oneOf");
    assert_eq!(variants.len(), 3, "{schema:#}");
    for variant in variants {
        let kind = variant["properties"]["kind"]["enum"][0]
            .as_str()
            .expect("each variant pins its kind");
        let requires_cause = variant["required"]
            .as_array()
            .expect("each variant lists required keys")
            .iter()
            .any(|key| key == "cause");
        assert_eq!(
            requires_cause,
            kind != "head-unreadable",
            "{kind}: cause required = {requires_cause}\n{variant:#}"
        );
    }
}

#[test]
fn head_unreadable_without_cause_omits_cause_field() {
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
            error: "no underlying VcsError".into(),
            cause: None,
        }),
    );
    let failure = v["failure"].as_object().unwrap();
    assert!(
        !failure.contains_key("cause"),
        "cause should be omitted when None: {v}"
    );
}

// ---------------------------------------------------------------------------
// VcsError -> VcsErrorOutput: the tag a failure's kind spells is the tag its
// wire mirror carries, for every variant.
// ---------------------------------------------------------------------------

fn vcs_kind_via_serde(err: &VcsError) -> String {
    let wire = VcsErrorOutput::from(err);
    let v = serde_json::to_value(&wire).unwrap();
    v.get("kind").and_then(Value::as_str).unwrap().to_owned()
}

/// One `VcsError` of every variant. The match with no default arm stops this
/// compiling when a variant is added.
fn every_vcs_error() -> Vec<VcsError> {
    let repo = || PathBuf::from("/x");
    let all = vec![
        VcsError::NotARepo(repo()),
        VcsError::RevisionNotFound {
            repo: repo(),
            rev: "v1".into(),
        },
        VcsError::RebaseConflict {
            repo: repo(),
            op: ConflictOp::Merge,
        },
        VcsError::StaleRefWitness {
            repo: repo(),
            expected: "a".into(),
            observed: "b".into(),
        },
        VcsError::MidOperation {
            repo: repo(),
            operation: MidOperation::Bisect,
        },
        VcsError::HookRejected {
            repo: repo(),
            stderr: "no".into(),
        },
        VcsError::UntrackedCollision {
            repo: repo(),
            paths: vec!["f".into()],
        },
        VcsError::Io {
            ctx: "spawn".into(),
            source: std::io::Error::other("boom"),
        },
        VcsError::CommandFailed {
            args: vec!["status".into()],
            repo: repo(),
            stderr: "oops".into(),
        },
    ];
    for err in &all {
        match err {
            VcsError::NotARepo(_)
            | VcsError::RevisionNotFound { .. }
            | VcsError::RebaseConflict { .. }
            | VcsError::StaleRefWitness { .. }
            | VcsError::MidOperation { .. }
            | VcsError::HookRejected { .. }
            | VcsError::UntrackedCollision { .. }
            | VcsError::Io { .. }
            | VcsError::CommandFailed { .. } => {}
        }
    }
    all
}

#[test]
fn every_vcs_error_kind_is_the_tag_its_wire_mirror_carries() {
    let errors = every_vcs_error();
    for err in &errors {
        assert_eq!(
            err.kind().to_string(),
            vcs_kind_via_serde(err),
            "{err:?}: the kind and the wire tag differ"
        );
    }

    let listed: std::collections::BTreeSet<String> =
        errors.iter().map(|e| e.kind().to_string()).collect();
    let all: std::collections::BTreeSet<String> =
        VcsErrorKind::ALL.iter().map(ToString::to_string).collect();
    assert_eq!(
        listed, all,
        "VcsErrorKind::ALL omits a kind, or lists one no error has"
    );
}

#[test]
fn conflict_op_serializes_kebab_case() {
    assert_eq!(
        serde_json::to_value(ConflictOp::Rebase).unwrap(),
        Value::String("rebase".into())
    );
    assert_eq!(
        serde_json::to_value(ConflictOp::Merge).unwrap(),
        Value::String("merge".into())
    );
    assert_eq!(
        serde_json::to_value(ConflictOp::CherryPick).unwrap(),
        Value::String("cherry-pick".into())
    );
}

// ---------------------------------------------------------------------------
// The worker count does not choose the shape or the order
// ---------------------------------------------------------------------------

const REPO_PATHS: &[&str] = &[
    "github/chatly/alpha",
    "github/chatly/beta",
    "github/chatly/gamma",
    "github/chatly/delta",
];

fn url_for(repo_path: &str) -> String {
    format!("https://github.com/{repo_path}.git")
}

/// Build a multi-repo workspace pair (primary + workweave) where the
/// workweave has advanced each manifest repo by one commit and re-locked.
/// Returns `(primary, workweave, advanced_shas)` keyed in `REPO_PATHS`
/// order.
fn make_multi_repo_workspaces(parent: &Path) -> (Workspace, Workspace, Vec<String>) {
    // Primary setup: initialize each manifest repo, write manifest + lock.
    let primary_root = parent.join("primary");
    std::fs::create_dir_all(primary_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(primary_root.join("projects")).unwrap();

    let mut initial_shas = Vec::new();
    for repo_path in REPO_PATHS {
        let dir = primary_root.join(repo_path);
        let sha = init_repo(&dir);
        initial_shas.push(sha);
    }

    let primary_project_dir = primary_root.join("projects/web-app");
    init_repo(&primary_project_dir);
    std::fs::write(
        primary_project_dir.join(".gitattributes"),
        "rwv.lock merge=rwv-ours\n",
    )
    .unwrap();
    let manifest_pairs: Vec<(&str, String)> = REPO_PATHS.iter().map(|p| (*p, url_for(p))).collect();
    let manifest_refs: Vec<(&str, &str)> = manifest_pairs
        .iter()
        .map(|(p, u)| (*p, u.as_str()))
        .collect();
    write_manifest(&primary_project_dir, &manifest_refs);
    let lock_triples: Vec<(&str, String, &str)> = REPO_PATHS
        .iter()
        .zip(&initial_shas)
        .map(|(p, sha)| (*p, url_for(p), sha.as_str()))
        .collect();
    let lock_refs: Vec<(&str, &str, &str)> = lock_triples
        .iter()
        .map(|(p, u, s)| (*p, u.as_str(), *s))
        .collect();
    common::fixture_lock(&primary_project_dir, &lock_refs);
    common::git_in(
        &primary_project_dir,
        &["add", ".gitattributes", "rwv.toml", "rwv.lock"],
    );
    common::git_in(&primary_project_dir, &["commit", "-m", "lock: initial"]);
    std::fs::write(primary_root.join(".rwv-active"), "web-app\n").unwrap();

    // Workweave: worktree-add each repo + the project repo.
    let ww_root = parent.join("ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    for repo_path in REPO_PATHS.iter() {
        let dest = ww_root.join(repo_path);
        let canonical = primary_root.join(repo_path);
        common::git_in(
            &canonical,
            &[
                "worktree",
                "add",
                &dest.to_string_lossy(),
                "-b",
                "web-app--ww",
            ],
        );
    }
    let ww_project_dir = ww_root.join("projects/web-app");
    common::git_in(
        &primary_project_dir,
        &[
            "worktree",
            "add",
            &ww_project_dir.to_string_lossy(),
            "-b",
            "ww/project",
        ],
    );
    std::fs::write(ww_root.join(".rwv-active"), "web-app\n").unwrap();

    // Advance each ww repo by one commit; collect new SHAs.
    let mut advanced_shas = Vec::new();
    for (i, repo_path) in REPO_PATHS.iter().enumerate() {
        let dir = ww_root.join(repo_path);
        let sha = make_commit(
            &dir,
            &format!("change-{i}.txt"),
            &format!("ww change {i}\n"),
            &format!("ww: advance {i}"),
        );
        advanced_shas.push(sha);
    }

    // Rewrite ww lock + commit.
    let ww_lock_triples: Vec<(&str, String, &str)> = REPO_PATHS
        .iter()
        .zip(&advanced_shas)
        .map(|(p, sha)| (*p, url_for(p), sha.as_str()))
        .collect();
    let ww_lock_refs: Vec<(&str, &str, &str)> = ww_lock_triples
        .iter()
        .map(|(p, u, s)| (*p, u.as_str(), *s))
        .collect();
    common::fixture_lock(&ww_project_dir, &ww_lock_refs);
    common::git_in(&ww_project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww_project_dir, &["commit", "-m", "lock: ww advance"]);

    let primary = Workspace {
        root: primary_root,
        project_dir: primary_project_dir,
        server_dir: PathBuf::new(), // unused for multi-repo
    };
    let ww = Workspace {
        root: ww_root,
        project_dir: ww_project_dir,
        server_dir: PathBuf::new(),
    };
    (primary, ww, advanced_shas)
}

fn manifest_order() -> Vec<&'static str> {
    let mut paths = REPO_PATHS.to_vec();
    paths.sort();
    paths
}

/// Run `verb` from the fixture's primary against its workweave, which is ahead
/// in every repo, so each repo moves forward in the primary in its own worker:
/// `sync` fast-forwards it, `sync-to`'s first step rebases it, which for a repo
/// with nothing of its own is a fast-forward too.
fn sync_from_primary(primary: &Workspace, ww: &Workspace, verb: &str, jobs: &str) -> Vec<u8> {
    let target = ww.root.to_string_lossy();
    rwv()
        .args([verb, &target, "--json", "-j", jobs])
        .current_dir(&primary.root)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone()
}

fn install_hook(repo: &Path, name: &str, body: &str) {
    let hooks = repo.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join(name);
    std::fs::write(&hook, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The hook that fires in a repo as `verb` moves it forward.
fn landing_hook(verb: &str) -> &'static str {
    match verb {
        "sync" => "post-merge",
        _ => "post-checkout",
    }
}

/// Hold the first repo's landing until the second repo's has landed, and then
/// a second longer, so a parallel run's workers finish out of manifest order.
/// Each hook appends its repo to `log` the first time it fires. The wait is
/// bounded: a git that runs no hooks leaves an empty log, not a hang.
fn land_first_repo_last(primary: &Workspace, verb: &str, log: &Path) {
    let order = manifest_order();
    let (first, second) = (order[0], order[1]);
    let log = log.to_string_lossy().replace('\\', "/");
    let once = |repo: &str| format!("#!/bin/sh\ngrep -q '{repo}' '{log}' 2>/dev/null && exit 0\n");
    install_hook(
        &primary.root.join(second),
        landing_hook(verb),
        &format!("{}echo {second} >> '{log}'\n", once(second)),
    );
    install_hook(
        &primary.root.join(first),
        landing_hook(verb),
        &format!(
            "{}i=0\n\
             while ! grep -q '{second}' '{log}' 2>/dev/null && [ $i -lt 300 ]; do sleep 0.1; i=$((i+1)); done\n\
             echo {first} >> '{log}'\nsleep 1\n",
            once(first)
        ),
    );
}

fn assert_landed_first_repo_last(log: &Path) {
    let order = manifest_order();
    assert_eq!(
        std::fs::read_to_string(log).unwrap_or_default(),
        format!("{}\n{}\n", order[1], order[0]),
        "the first repo must land after the second, or this run proves nothing"
    );
}

#[test]
fn sync_json_is_one_envelope_whatever_the_worker_count() {
    let tmp = common::tempdir().unwrap();
    let shape_under = |jobs: &str| {
        let (primary, ww, _shas) = make_multi_repo_workspaces(&tmp.path().join(jobs));
        let stdout = sync_from_primary(&primary, &ww, "sync", jobs);
        common::envelope_shape(&stdout, SYNC_JSON_SCHEMA_URL, "outcomes")
    };
    let serial = shape_under("1");
    assert_eq!(serial.1, manifest_order(), "outcomes follow the manifest");
    assert_eq!(shape_under("4"), serial);
}

#[test]
fn sync_to_json_is_one_envelope_whatever_the_worker_count() {
    let tmp = common::tempdir().unwrap();
    let shape_under = |jobs: &str| {
        let (primary, ww, _shas) = make_multi_repo_workspaces(&tmp.path().join(jobs));
        let stdout = sync_from_primary(&primary, &ww, "sync-to", jobs);
        common::envelope_shape(&stdout, SYNC_TO_JSON_SCHEMA_URL, "outcomes")
    };
    let serial = shape_under("1");
    assert_eq!(serial.1, manifest_order(), "outcomes follow the manifest");
    assert_eq!(shape_under("4"), serial);
}

#[test]
fn sync_json_lists_outcomes_in_manifest_order_when_workers_finish_out_of_it() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _shas) = make_multi_repo_workspaces(tmp.path());
    let log = tmp.path().join("landed");
    land_first_repo_last(&primary, "sync", &log);

    let stdout = sync_from_primary(&primary, &ww, "sync", "4");

    assert_landed_first_repo_last(&log);
    let (_, paths) = common::envelope_shape(&stdout, SYNC_JSON_SCHEMA_URL, "outcomes");
    assert_eq!(paths, manifest_order());
}

#[test]
fn sync_to_json_lists_outcomes_in_manifest_order_when_workers_finish_out_of_it() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _shas) = make_multi_repo_workspaces(tmp.path());
    let log = tmp.path().join("landed");
    land_first_repo_last(&primary, "sync-to", &log);

    let stdout = sync_from_primary(&primary, &ww, "sync-to", "4");

    assert_landed_first_repo_last(&log);
    let (_, paths) = common::envelope_shape(&stdout, SYNC_TO_JSON_SCHEMA_URL, "outcomes");
    assert_eq!(paths, manifest_order());
}

#[test]
fn sync_json_envelope_round_trips() {
    // Build a synthetic envelope and round-trip it through serde so we don't
    // depend on the live sync path to exercise the envelope shape.
    let envelope = SyncJsonOutput {
        schema: SYNC_JSON_SCHEMA_URL.to_owned(),
        outcomes: vec![
            SyncOutcomeOutput::from_outcome(
                "p1".into(),
                repoweave::path_spelling::WirePath::of(std::path::Path::new("/abs/p1")),
                &RepoSyncOutcome::Converged {
                    derived_content_dropped: Vec::new(),
                },
                None,
            ),
            SyncOutcomeOutput::from_outcome(
                "p2".into(),
                repoweave::path_spelling::WirePath::of(std::path::Path::new("/abs/p2")),
                &RepoSyncOutcome::AlreadyAhead {
                    commits_ahead: 2,
                    baseline: ReplayBaseline::SourceLockEntry,
                    source_position: Some(Containment::Equal),
                },
                Some(ContainmentOutput {
                    verdict: ContainmentVerdictOutput::Ahead { commits: 2 },
                    baseline: ReplayBaseline::SourceLockEntry,
                }),
            ),
        ],
        placements: Vec::new(),
        members: Vec::new(),
        complete: true,
        advisories: Vec::new(),
        resolution: None,
    };
    let v = serde_json::to_value(&envelope).unwrap();
    assert_eq!(v["$schema"], SYNC_JSON_SCHEMA_URL);
    let outcomes = v["outcomes"].as_array().unwrap();
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0]["kind"], "converged");
    assert_eq!(outcomes[1]["kind"], "already-ahead");
}

// ---------------------------------------------------------------------------
// Structured payload / renamed-field tests
// ---------------------------------------------------------------------------

/// The `error` field on SyncFailureOutput is now `message`. Verify the
/// wire key is `message`, not `error`, so the renamed field is pinned.
#[test]
fn sync_failure_output_uses_message_not_error() {
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
            error: "couldn't read HEAD".into(),
            cause: None,
        }),
    );
    let failure = v["failure"].as_object().unwrap();
    assert!(
        failure.contains_key("message"),
        "failure should have `message` field, got: {v}"
    );
    assert!(
        !failure.contains_key("error"),
        "failure must NOT have legacy `error` field, got: {v}"
    );
    assert_eq!(failure["message"], "couldn't read HEAD");
}

/// When a SyncFailure carries a VcsError cause, the wire output carries
/// the structured `cause` object — not just a string. Verify `cause.kind`
/// is a structured discriminant field, not a free-form string blob.
#[test]
fn sync_failure_cause_is_structured_not_stringly() {
    let cause = VcsError::RebaseConflict {
        repo: PathBuf::from("/abs/foo"),
        op: ConflictOp::Rebase,
    };
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::Failed(SyncFailure::RebaseFailed {
            error: cause.to_string(),
            cause,
        }),
    );
    let failure = &v["failure"];
    assert_eq!(failure["kind"], "rebase-failed");
    // cause must be a structured object with a `kind` discriminant
    let cause_obj = failure["cause"]
        .as_object()
        .expect("cause should be a structured object");
    assert!(
        cause_obj.contains_key("kind"),
        "cause object must have `kind` discriminant: {failure}"
    );
    assert_eq!(cause_obj["kind"], "rebase-conflict");
    // cause must NOT be a raw string
    assert!(
        !failure["cause"].is_string(),
        "cause must be structured, not a raw string: {failure}"
    );
}

/// An io cause must survive the whole way onto the `sync --json` payload,
/// not merely into the mirror type.
///
/// `VcsErrorOutput` builds its `message` from the io source independently of
/// `VcsError`'s `Display`, so the two renders can regress apart: a caller that
/// dropped `cause`, or a mirror that stopped carrying the source, leaves the
/// operator with rwv's own context sentence and no statement of what actually
/// failed. The mirror is pinned alone above, and the failure envelope's `cause`
/// is pinned with a variant that carries no source at all — neither notices
/// this composition breaking.
#[test]
fn an_io_cause_reaches_the_sync_json_payload() {
    let cause = VcsError::Io {
        ctx: "failed to spawn git rev-parse".into(),
        source: std::io::Error::other("permission denied"),
    };
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::Failed(SyncFailure::HeadUnreadable {
            error: cause.to_string(),
            cause: Some(cause),
        }),
    );
    let wire_cause = &v["failure"]["cause"];
    assert_eq!(
        wire_cause["kind"], "io",
        "an io cause must keep its discriminant on the wire: {v}"
    );
    let message = wire_cause["message"]
        .as_str()
        .unwrap_or_else(|| panic!("io cause must carry a `message` on the wire: {v}"));
    assert!(
        message.contains("permission denied"),
        "the operator reading `sync --json` must be told what actually \
         failed, not only rwv's context sentence; got: {message:?}"
    );
}

/// VcsErrorOutput::Io carries `message` (renamed from `error`) to signal
/// it is a free-form display string from io::Error, not a typed discriminant.
#[test]
fn vcs_error_io_output_uses_message_not_error() {
    let err = VcsError::Io {
        ctx: "spawn git".into(),
        source: std::io::Error::other("permission denied"),
    };
    let wire = VcsErrorOutput::from(&err);
    let v = serde_json::to_value(&wire).unwrap();
    assert_eq!(v["kind"], "io");
    assert!(
        v.get("message").is_some(),
        "VcsErrorOutput::Io must have `message` field: {v}"
    );
    assert!(
        v.get("error").is_none(),
        "VcsErrorOutput::Io must NOT have legacy `error` field: {v}"
    );
    assert!(
        v["message"].as_str().unwrap().contains("permission denied"),
        "message should contain io::Error display: {v}"
    );
}

// ---------------------------------------------------------------------------
// The containment disclosure, on the surface a consumer reads
//
// The verdict a repo's outcome was decided from used to reach `--json` only
// as prose: `already-ahead` carried a bare `commits_ahead` with no statement
// of what it was ahead OF, and a failed fast-forward carried its verdict
// inside `failure.message`, which the schema documents as free-form. Both
// tests below drive the binary and read stdout, because a field that
// serializes and never arrives is the failure mode a struct-literal test
// cannot see.
//
// `behind` and `diverged` are reached here. `ahead` and `equal` are pinned
// one layer down — the envelope round-trip above and the conformance corpus —
// because reaching them through the binary costs a fixture per verdict and
// the wire shape they take is the same one these two exercise.
// ---------------------------------------------------------------------------

/// The containment object of the outcome for `path`, or a panic naming what
/// the envelope did carry.
fn containment_of(stdout: &str, path: &str) -> Value {
    let parsed: Value = serde_json::from_str(stdout)
        .unwrap_or_else(|e| panic!("stdout not parseable as JSON ({e}):\n{stdout}"));
    let outcomes = parsed
        .get("outcomes")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("outcomes array missing:\n{stdout}"));
    let outcome = outcomes
        .iter()
        .find(|o| o.get("path").and_then(Value::as_str) == Some(path))
        .unwrap_or_else(|| panic!("no outcome for {path}:\n{stdout}"));
    outcome
        .get("containment")
        .cloned()
        .unwrap_or_else(|| panic!("outcome for {path} carries no containment:\n{outcome}"))
}

#[test]
fn sync_json_discloses_the_verdict_a_converged_repo_was_behind_by() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let c2 = make_commit(
        &ww.server_dir,
        "change.txt",
        "ww change\n",
        "ww: add change",
    );
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: ww change"]);

    let assert = rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--json"])
        .current_dir(&primary.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let containment = containment_of(&stdout, SERVER_PATH);
    assert_eq!(
        containment["verdict"]["relation"], "behind",
        "a repo the source is ahead of converged by fast-forward, and the \
         envelope must say so: {containment}"
    );
    assert_eq!(
        containment["verdict"]["commits"], 1,
        "the count is the one commit this checkout lacked: {containment}"
    );
    assert_eq!(
        containment["baseline"], "source-lock-entry",
        "the source relocked before this sync, so the target came from its \
         lock entry rather than a tip pulled ahead of it: {containment}"
    );
}

#[test]
fn sync_json_discloses_the_verdict_a_failed_fast_forward_ran_against() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _c1) = make_shared_workspaces(tmp.path());

    let c2 = make_commit(
        &primary.server_dir,
        "primary.txt",
        "primary\n",
        "primary: C2",
    );
    common::fixture_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    common::git_in(&primary.project_dir, &["add", "rwv.lock"]);
    common::git_in(&primary.project_dir, &["commit", "-m", "lock: C2"]);

    // Two commits here against the source's one, so the pair of counts cannot
    // be read correctly by a disclosure that transposes them.
    make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: diverged from C1");
    let c_ww = make_commit(&ww.server_dir, "ww2.txt", "ww2\n", "ww: second");
    common::fixture_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    common::git_in(&ww.project_dir, &["add", "rwv.lock"]);
    common::git_in(&ww.project_dir, &["commit", "-m", "lock: C_ww"]);

    let assert = rwv()
        .args([
            "sync",
            &primary.root.to_string_lossy(),
            "--json",
            "--discard-local-commits",
        ])
        .current_dir(&ww.root)
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let containment = containment_of(&stdout, SERVER_PATH);
    assert_eq!(
        containment["verdict"]["relation"], "diverged",
        "the fast-forward failed because both sides hold commits the other \
         lacks, and that is the fact a consumer must not have to parse out of \
         the failure message: {containment}"
    );
    assert_eq!(
        containment["verdict"]["ahead"], 2,
        "two commits here the source lacks: {containment}"
    );
    assert_eq!(
        containment["verdict"]["behind"], 1,
        "one commit there this checkout lacks: {containment}"
    );
    assert_eq!(
        containment["baseline"], "source-lock-entry",
        "the source relocked, so its lock entry is what this was measured \
         against: {containment}"
    );
}
