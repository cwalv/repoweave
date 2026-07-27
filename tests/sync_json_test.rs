//! Tests for `rwv sync --json`.
//!
//! Two layers:
//! 1. End-to-end: drive `rwv sync --json` through the binary, parse stdout
//!    as JSON, assert envelope + per-repo shape and exit code.
//! 2. Library-level snapshot tests of `VcsError` / `SyncFailure` /
//!    `RepoSyncOutcome` -> wire-output serialization, so kebab-case tags
//!    and field names are pinned independent of the live sync path.

use assert_cmd::Command as AssertCommand;
use repoweave::sync::{
    RepoSyncOutcome, SyncFailure, SyncJsonOutput, SyncOutcomeOutput, SYNC_JSON_SCHEMA_URL,
};
use repoweave::vcs::{ConflictOp, VcsError, VcsErrorOutput};
use serde_json::Value;
use std::path::{Path, PathBuf};

mod common;

// ---------------------------------------------------------------------------
// Helpers (mirrors of the helpers in e2e_sync_abort_test.rs; kept local so
// this file doesn't pull on that test file's private fixtures)
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
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn init_repo(path: &Path) -> String {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "-b", "main"], path);
    std::fs::write(path.join("README.md"), "init\n").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
    git_out(&["rev-parse", "HEAD"], path)
}

fn make_commit(repo: &Path, filename: &str, content: &str, msg: &str) -> String {
    std::fs::write(repo.join(filename), content).unwrap();
    git(&["add", filename], repo);
    git(&["commit", "-m", msg], repo);
    git_out(&["rev-parse", "HEAD"], repo)
}

fn write_manifest(project_dir: &Path, repos: &[(&str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.yaml"), &yaml).unwrap();
}

fn write_lock(project_dir: &Path, repos: &[(&str, &str, &str)]) {
    let mut yaml = String::from("repositories:\n");
    for (path, url, sha) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: {sha}\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), &yaml).unwrap();
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
    write_lock(&project_dir, &[(SERVER_PATH, SERVER_URL, &sha)]);
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

fn make_shared_workspaces(parent: &Path) -> (Workspace, Workspace, String) {
    let (primary, c1) = make_locked_workspace(parent, "owned");

    let ww_root = parent.join("ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
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
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: ww change"], &ww.project_dir);

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
    let primary_head = git_out(&["rev-parse", "main"], &primary.server_dir);
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
    write_lock(&primary.project_dir, &[(SERVER_PATH, SERVER_URL, &c2)]);
    git(&["add", "rwv.lock"], &primary.project_dir);
    git(&["commit", "-m", "lock: C2"], &primary.project_dir);

    // Workweave: diverge.
    let c_ww = make_commit(&ww.server_dir, "ww.txt", "ww\n", "ww: diverged from C1");
    write_lock(&ww.project_dir, &[(SERVER_PATH, SERVER_URL, &c_ww)]);
    git(&["add", "rwv.lock"], &ww.project_dir);
    git(&["commit", "-m", "lock: C_ww"], &ww.project_dir);

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
    let head = git_out(&["rev-parse", "HEAD"], &primary.server_dir);
    assert_eq!(head, c1);
}

// ---------------------------------------------------------------------------
// Library-level snapshot tests of wire-output serialization
// ---------------------------------------------------------------------------

fn serialize_outcome(path: &str, abs: &str, outcome: &RepoSyncOutcome) -> Value {
    let out = SyncOutcomeOutput::from_outcome(path.to_owned(), abs.to_owned(), outcome);
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
        &RepoSyncOutcome::AlreadyAhead { commits_ahead: 3 },
    );
    assert_eq!(v["kind"], "already-ahead");
    assert_eq!(v["commits_ahead"], 3);
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
            cause: None,
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
            cause: None,
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
            cause: Some(cause),
        }),
    );
    assert_eq!(v["failure"]["kind"], "rebase-failed");
    let inner = &v["failure"]["cause"];
    assert_eq!(inner["kind"], "rebase-conflict");
    assert_eq!(inner["op"], "rebase");
    assert_eq!(inner["repo"], "/abs/foo");
}

#[test]
fn outcome_failed_without_cause_omits_cause_field() {
    let v = serialize_outcome(
        "github/cwalv/foo",
        "/abs/foo",
        &RepoSyncOutcome::Failed(SyncFailure::RebaseFailed {
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
// VcsError -> VcsErrorOutput tag snapshot. Pins the kebab-case tags so any
// drift between `VcsError::kind()` and serde's tag renaming is caught.
// ---------------------------------------------------------------------------

fn vcs_kind_via_serde(err: &VcsError) -> String {
    let wire = VcsErrorOutput::from(err);
    let v = serde_json::to_value(&wire).unwrap();
    v.get("kind").and_then(Value::as_str).unwrap().to_owned()
}

#[test]
fn vcs_error_kind_tags_match_kind_method() {
    let cases: Vec<(VcsError, &str)> = vec![
        (VcsError::NotARepo(PathBuf::from("/x")), "not-a-repo"),
        (
            VcsError::RevisionNotFound {
                repo: PathBuf::from("/x"),
                rev: "v1".into(),
            },
            "revision-not-found",
        ),
        (
            VcsError::BranchAlreadyExists {
                repo: PathBuf::from("/x"),
                branch: repoweave::vcs::RefName::new("feat"),
            },
            "branch-already-exists",
        ),
        (
            VcsError::WorktreeExists(PathBuf::from("/x")),
            "worktree-exists",
        ),
        (
            VcsError::UncommittedChanges(PathBuf::from("/x")),
            "uncommitted-changes",
        ),
        (
            VcsError::RebaseConflict {
                repo: PathBuf::from("/x"),
                op: ConflictOp::Merge,
            },
            "rebase-conflict",
        ),
        (
            VcsError::Io {
                ctx: "spawn".into(),
                source: std::io::Error::other("boom"),
            },
            "io",
        ),
        (
            VcsError::CommandFailed {
                args: vec!["status".into()],
                repo: PathBuf::from("/x"),
                stderr: "oops".into(),
            },
            "command-failed",
        ),
    ];
    for (err, expected) in &cases {
        assert_eq!(
            err.kind(),
            *expected,
            "VcsError::kind() drift for {expected}"
        );
        assert_eq!(
            vcs_kind_via_serde(err),
            *expected,
            "serde-tag drift for {expected}"
        );
    }
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
// Parallel / NDJSON mode
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
    write_lock(&primary_project_dir, &lock_refs);
    git(
        &["add", ".gitattributes", "rwv.yaml", "rwv.lock"],
        &primary_project_dir,
    );
    git(&["commit", "-m", "lock: initial"], &primary_project_dir);
    std::fs::write(primary_root.join(".rwv-active"), "web-app\n").unwrap();

    // Workweave: worktree-add each repo + the project repo.
    let ww_root = parent.join("ww");
    std::fs::create_dir_all(ww_root.join("github/chatly")).unwrap();
    std::fs::create_dir_all(ww_root.join("projects")).unwrap();

    for repo_path in REPO_PATHS.iter() {
        let dest = ww_root.join(repo_path);
        let canonical = primary_root.join(repo_path);
        git(
            &[
                "worktree",
                "add",
                &dest.to_string_lossy(),
                "-b",
                "web-app--ww",
            ],
            &canonical,
        );
    }
    let ww_project_dir = ww_root.join("projects/web-app");
    git(
        &[
            "worktree",
            "add",
            &ww_project_dir.to_string_lossy(),
            "-b",
            "ww/project",
        ],
        &primary_project_dir,
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
    write_lock(&ww_project_dir, &ww_lock_refs);
    git(&["add", "rwv.lock"], &ww_project_dir);
    git(&["commit", "-m", "lock: ww advance"], &ww_project_dir);

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

#[test]
fn sync_json_ndjson_emits_one_record_per_line_under_jobs_gt_one() {
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _shas) = make_multi_repo_workspaces(tmp.path());

    // Sync primary -> ww (so ww materializes nothing new; primary just
    // advances 4 repos to ww's tips). `-j 2 --json` should produce NDJSON.
    let assert = rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--json", "-j", "2"])
        .current_dir(&primary.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    // Acceptance: NO envelope wrapper. Every non-empty line parses
    // as a self-describing JSON record with `$schema`, `kind`, and
    // `path` fields.
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.len() >= REPO_PATHS.len(),
        "expected >= {} NDJSON lines, got {} in stdout:\n{stdout}",
        REPO_PATHS.len(),
        lines.len()
    );

    // The whole stdout must NOT parse as one big JSON document
    // (proves it's NDJSON, not an envelope).
    assert!(
        serde_json::from_str::<Value>(&stdout).is_err(),
        "NDJSON stdout must not parse as one document; got:\n{stdout}"
    );

    let mut seen_paths = std::collections::BTreeSet::new();
    for line in &lines {
        let v: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("line not parseable as JSON ({e}): {line}"));
        let obj = v
            .as_object()
            .unwrap_or_else(|| panic!("line not an object: {line}"));
        assert_eq!(
            obj.get("$schema").and_then(Value::as_str),
            Some(SYNC_JSON_SCHEMA_URL),
            "every NDJSON record must embed `$schema`: {line}"
        );
        assert!(obj.contains_key("kind"), "missing `kind`: {line}");
        assert!(obj.contains_key("path"), "missing `path`: {line}");
        assert!(
            obj.contains_key("absolute_path"),
            "missing `absolute_path`: {line}"
        );
        if let Some(path) = obj.get("path").and_then(Value::as_str) {
            seen_paths.insert(path.to_string());
        }
    }

    // All four manifest repos appear in the stream.
    for repo_path in REPO_PATHS {
        assert!(
            seen_paths.contains(*repo_path),
            "expected path {repo_path} in NDJSON stream; got {:?}\nstdout:\n{stdout}",
            seen_paths
        );
    }
}

#[test]
fn sync_json_serial_emits_envelope_with_explicit_jobs_one() {
    // Acceptance: -j 1 with --json emits the envelope (not NDJSON), even
    // though we passed -j explicitly. This pins the "no-j or j=1" contract.
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _shas) = make_multi_repo_workspaces(tmp.path());

    let assert = rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--json", "-j", "1"])
        .current_dir(&primary.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();

    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("-j 1 must emit envelope, not NDJSON ({e}):\n{stdout}"));
    let obj = parsed.as_object().expect("top level should be object");
    assert_eq!(
        obj.get("$schema").and_then(Value::as_str),
        Some(SYNC_JSON_SCHEMA_URL),
        "envelope must include $schema URL"
    );
    let outcomes = obj
        .get("outcomes")
        .and_then(Value::as_array)
        .expect("outcomes array");
    assert_eq!(outcomes.len(), REPO_PATHS.len(), "all repos present");
}

#[test]
fn sync_json_ndjson_no_text_prefix_wrapping() {
    // Acceptance: under `--json -j > 1`, the `[<prefix>] <line>` Reporter
    // wrapper must be bypassed. We verify by checking no line in stdout
    // starts with `[github/...]` (the prefix format).
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _shas) = make_multi_repo_workspaces(tmp.path());

    let assert = rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--json", "-j", "4"])
        .current_dir(&primary.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        assert!(
            !trimmed.starts_with('['),
            "NDJSON line must not start with `[` (Reporter prefix wrapper); line: {line}\nstdout:\n{stdout}"
        );
        // Each non-empty line should start with `{` (a JSON object).
        if !trimmed.is_empty() {
            assert!(
                trimmed.starts_with('{'),
                "NDJSON line must start with `{{`; line: {line}\nstdout:\n{stdout}"
            );
        }
    }
}

#[test]
fn sync_json_ndjson_lines_are_not_interleaved() {
    // Each NDJSON line must be a complete JSON object. The mutex guard in
    // OutputSink::record ensures workers can't tear bytes mid-line. Parse
    // line-by-line — any failure indicates interleaving.
    let tmp = common::tempdir().unwrap();
    let (primary, ww, _shas) = make_multi_repo_workspaces(tmp.path());

    let assert = rwv()
        .args(["sync", &ww.root.to_string_lossy(), "--json", "-j", "4"])
        .current_dir(&primary.root)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).into_owned();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("torn / interleaved line ({e}): {line}\nstdout:\n{stdout}"));
        assert!(parsed.is_object(), "line not an object: {line}");
    }
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
                "/abs/p1".into(),
                &RepoSyncOutcome::Converged {
                    derived_content_dropped: Vec::new(),
                },
            ),
            SyncOutcomeOutput::from_outcome(
                "p2".into(),
                "/abs/p2".into(),
                &RepoSyncOutcome::AlreadyAhead { commits_ahead: 2 },
            ),
        ],
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
            cause: Some(cause),
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
