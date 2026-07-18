//! Integration tests for workweave submodule initialization (R23 GAP fix).
//!
//! After `create_workweave`, repos containing `.gitmodules` should have
//! submodules initialized in the new worktree. When submodule init fails
//! (e.g. unreachable remote), the create still succeeds and emits a named
//! warning with the fix command. Repos without `.gitmodules` must not invoke
//! git submodule at all (verified behaviorally: no empty-paths finding).
//!
//! Also covers the doctor-side scanner: `scan_uninitialized_submodules_in_workweaves`
//! surfaces a `UninitializedSubmodule` warning when a worktree has `.gitmodules`
//! with empty submodule dirs, and is silent for workweaves where submodules are
//! correctly initialized.

use std::path::{Path, PathBuf};
use std::process;

use repoweave::manifest::{ProjectName, WorkweaveName};
use repoweave::workweave::{create_workweave, scan_uninitialized_submodules};

mod common;

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

fn git(args: &[&str], dir: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(dir)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git {args:?} in {} failed", dir.display());
}

/// Initialize a git repo with one commit.
fn init_repo_with_commit(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(&["init", "--initial-branch=main"], path);
    git(&["config", "user.email", "test@test.com"], path);
    git(&["config", "user.name", "Test"], path);
    std::fs::write(path.join("README"), "init").unwrap();
    git(&["add", "."], path);
    git(&["commit", "-m", "initial"], path);
}

/// Create a minimal workspace with one project and one member repo.
///
/// Layout:
///   `tmp/ws/`
///   `tmp/ws/github/org/repo/` — git repo
///   `tmp/ws/projects/{project}/rwv.yaml`
fn make_workspace(tmp: &Path, project: &str) -> PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"repositories:
  github/org/repo:
    type: git
    url: file://{repo}
    version: main
    role: owned
"#,
        repo = repo_path.display()
    );
    std::fs::write(project_dir.join("rwv.yaml"), manifest).unwrap();
    ws
}

/// Build a git command with `protocol.file.allow=always` (appended to the
/// existing GIT_CONFIG_* chain from `common::git()`).
///
/// Recent git versions default `protocol.file.allow=user`, which blocks
/// `git submodule add` with a `file://` URL in test contexts where the
/// caller is not an interactive user (i.e. CI and `cargo test`). Injecting
/// `always` via the GIT_CONFIG env-var mechanism avoids touching any disk
/// config and stacks on top of whatever common::git() already sets.
fn git_with_file_protocol(args: &[&str], dir: &Path) {
    // common::git() already sets GIT_CONFIG_COUNT=1 for init.defaultBranch.
    // We stack one more entry by bumping the count and adding the next key.
    let output = common::git()
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_1", "protocol.file.allow")
        .env("GIT_CONFIG_VALUE_1", "always")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git should be available");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed:\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Add a real submodule to `repo_dir`, pointing at `submodule_remote`.
/// The submodule is committed so `git worktree add` inherits the `.gitmodules`.
fn add_submodule(repo_dir: &Path, sub_path: &str, submodule_remote: &Path) {
    // git submodule add creates .gitmodules and clones the submodule.
    // We need protocol.file.allow=always so that file:// URLs are accepted
    // in test/CI contexts (recent git defaults to `user` which blocks it).
    git_with_file_protocol(
        &[
            "submodule",
            "add",
            &format!("file://{}", submodule_remote.display()),
            sub_path,
        ],
        repo_dir,
    );
    git(&["add", ".gitmodules", sub_path], repo_dir);
    git(&["commit", "-m", "add submodule"], repo_dir);
}

// ---------------------------------------------------------------------------
// Test 1: workweave create initializes submodules when .gitmodules exists
// ---------------------------------------------------------------------------

/// Workweave create: submodule content is present after create when the
/// member repo has a submodule backed by a reachable (local) remote.
#[test]
fn create_initializes_submodules_when_gitmodules_present() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    // Create a small repo that will serve as the submodule remote.
    let sub_remote = tmp.path().join("sub-remote");
    init_repo_with_commit(&sub_remote);
    std::fs::write(sub_remote.join("lib.txt"), "submodule content").unwrap();
    git(&["add", "."], &sub_remote);
    git(&["commit", "-m", "submodule initial"], &sub_remote);

    // Add the submodule to the member repo and commit it.
    let repo = ws.join("github/org/repo");
    add_submodule(&repo, "libs/sub", &sub_remote);

    // Create the workweave. Should succeed and init the submodule.
    let ww_dir = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir).unwrap();

    let result = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj"),
        &WorkweaveName::new("feat"),
        false,
        false,
        false,
    );

    // Create must succeed.
    assert!(
        result.is_ok(),
        "create_workweave should succeed when submodule remote is reachable: {:?}",
        result.err()
    );

    // The submodule directory in the workweave should be non-empty.
    let workweave_path = result.unwrap();
    let sub_dir = workweave_path.join("github/org/repo/libs/sub");
    assert!(
        sub_dir.is_dir(),
        "submodule directory should exist at {}",
        sub_dir.display()
    );
    let sub_file = sub_dir.join("lib.txt");
    assert!(
        sub_file.exists(),
        "submodule content (lib.txt) should be present after init at {}",
        sub_file.display()
    );

    // scan_uninitialized_submodules sees no empty paths.
    let empty = scan_uninitialized_submodules(&workweave_path.join("github/org/repo"));
    assert!(
        empty.is_empty(),
        "scan_uninitialized_submodules should find no empty paths after successful init, \
         got: {empty:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 2: create succeeds with warning when submodule remote is unreachable
// ---------------------------------------------------------------------------

/// Workweave create: when the submodule remote is unreachable (bad URL),
/// create still succeeds but emits a warning. The worktree exists; the
/// submodule directory is empty (or absent). Doctor can later flag it.
///
/// We set up `.gitmodules` with a URL that will never resolve (points at a
/// non-existent path) without going through `git submodule add` so that
/// no cached submodule objects exist in `.git/modules/`. The commit with
/// `.gitmodules` but no submodule content is the exact state that results
/// when a repo records a submodule pointer but the content is not in scope
/// — for example, a shallow clone or a repo-with-committed-gitmodules where
/// the submodule init was never run.
#[test]
fn create_succeeds_with_warning_when_submodule_remote_unreachable() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    let repo = ws.join("github/org/repo");

    // Write a .gitmodules pointing at a non-existent path, then commit it.
    // This mimics a repo that declares a submodule but whose remote is
    // unreachable (or has never been fetched). There is no .git/modules/
    // entry because we never ran `git submodule add`.
    std::fs::write(
        repo.join(".gitmodules"),
        "[submodule \"libs/sub\"]\n\
         \tpath = libs/sub\n\
         \turl = file:///nonexistent/rwv-test/sub-that-does-not-exist\n",
    )
    .unwrap();
    // Create the empty placeholder directory that git submodule would have
    // made — it only appears in the worktree if the submodule was ever
    // initialized; leaving it absent simulates the uninitialized state.
    git(&["add", ".gitmodules"], &repo);
    git(&["commit", "-m", "record submodule with bad url"], &repo);

    let ww_dir = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir).unwrap();

    let result = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj"),
        &WorkweaveName::new("feat"),
        false,
        false,
        false,
    );

    // Create MUST succeed even though submodule init failed.
    assert!(
        result.is_ok(),
        "create_workweave must succeed even when submodule remote is unreachable: {:?}",
        result.err()
    );

    // The worktree exists and .gitmodules is there (submodule declared).
    let workweave_path = result.unwrap();
    let worktree_repo = workweave_path.join("github/org/repo");
    assert!(worktree_repo.is_dir(), "worktree should exist");
    assert!(
        worktree_repo.join(".gitmodules").exists(),
        ".gitmodules should be present in worktree"
    );

    // The submodule directory should be empty / absent (init failed).
    let sub_dir = worktree_repo.join("libs/sub");
    let is_empty = if sub_dir.is_dir() {
        std::fs::read_dir(&sub_dir)
            .map(|mut rd| rd.next().is_none())
            .unwrap_or(true)
    } else {
        true
    };
    assert!(
        is_empty,
        "submodule directory should be empty when remote is unreachable"
    );

    // scan_uninitialized_submodules should detect the empty path.
    let empty = scan_uninitialized_submodules(&worktree_repo);
    assert!(
        !empty.is_empty(),
        "scan_uninitialized_submodules should find empty paths when init failed"
    );
    assert!(
        empty.iter().any(|p| p.contains("sub")),
        "expected 'sub' in empty paths, got: {empty:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: repo without .gitmodules — no submodule invocation
// ---------------------------------------------------------------------------

/// Workweave create: a repo with no `.gitmodules` should not trigger any
/// submodule logic. The worktree is created cleanly and
/// `scan_uninitialized_submodules` finds nothing.
#[test]
fn create_no_submodule_invocation_for_repo_without_gitmodules() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    // No submodule added — the repo has no .gitmodules.
    let repo = ws.join("github/org/repo");
    assert!(
        !repo.join(".gitmodules").exists(),
        "test precondition: repo must not have .gitmodules"
    );

    let ww_dir = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir).unwrap();

    let result = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj"),
        &WorkweaveName::new("feat"),
        false,
        false,
        false,
    );

    assert!(
        result.is_ok(),
        "create_workweave should succeed for repo without .gitmodules: {:?}",
        result.err()
    );

    let workweave_path = result.unwrap();
    let worktree_repo = workweave_path.join("github/org/repo");

    // No .gitmodules in the worktree either.
    assert!(
        !worktree_repo.join(".gitmodules").exists(),
        ".gitmodules should not appear in worktree when not in source"
    );

    // scan_uninitialized_submodules sees nothing.
    let empty = scan_uninitialized_submodules(&worktree_repo);
    assert!(
        empty.is_empty(),
        "scan_uninitialized_submodules should find nothing for repo without .gitmodules, \
         got: {empty:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: scan_uninitialized_submodules — standalone unit tests
// ---------------------------------------------------------------------------

/// `scan_uninitialized_submodules` returns empty when no `.gitmodules` exists.
#[test]
fn scan_no_gitmodules_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    // No .gitmodules written.
    let empty = scan_uninitialized_submodules(&repo);
    assert!(empty.is_empty(), "expected empty, got {empty:?}");
}

/// `scan_uninitialized_submodules` returns the path when the submodule dir
/// is empty.
#[test]
fn scan_empty_submodule_dir_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    // Write a minimal .gitmodules with one submodule path.
    std::fs::write(
        repo.join(".gitmodules"),
        "[submodule \"libs/util\"]\n\tpath = libs/util\n\turl = https://example.com/util\n",
    )
    .unwrap();

    // Create the submodule directory but leave it empty (never initialized).
    std::fs::create_dir_all(repo.join("libs/util")).unwrap();

    let empty = scan_uninitialized_submodules(&repo);
    assert_eq!(
        empty,
        vec!["libs/util".to_string()],
        "empty submodule dir should be reported"
    );
}

/// `scan_uninitialized_submodules` returns empty when the submodule dir is
/// populated.
#[test]
fn scan_populated_submodule_dir_is_not_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    std::fs::write(
        repo.join(".gitmodules"),
        "[submodule \"libs/util\"]\n\tpath = libs/util\n\turl = https://example.com/util\n",
    )
    .unwrap();

    // Populate the submodule dir with a file.
    let sub_dir = repo.join("libs/util");
    std::fs::create_dir_all(&sub_dir).unwrap();
    std::fs::write(sub_dir.join("lib.h"), "// content").unwrap();

    let empty = scan_uninitialized_submodules(&repo);
    assert!(
        empty.is_empty(),
        "populated submodule dir should not be reported, got: {empty:?}"
    );
}

/// `scan_uninitialized_submodules` returns the path when the submodule dir
/// is absent (was never created by git submodule update).
#[test]
fn scan_absent_submodule_dir_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();

    std::fs::write(
        repo.join(".gitmodules"),
        "[submodule \"vendor/foo\"]\n\tpath = vendor/foo\n\turl = https://example.com/foo\n",
    )
    .unwrap();
    // Do NOT create vendor/foo — the worktree git creates the placeholder
    // dir only for the checked-out commit, but submodule update fills it.

    let empty = scan_uninitialized_submodules(&repo);
    assert_eq!(
        empty,
        vec!["vendor/foo".to_string()],
        "absent submodule dir should be reported"
    );
}

// ---------------------------------------------------------------------------
// Test 5: doctor scan (scan_uninitialized_submodules_in_workweaves)
// ---------------------------------------------------------------------------

/// Doctor-side: scan_uninitialized_submodules_in_workweaves emits a
/// UninitializedSubmodule finding when a workweave repo has an empty
/// submodule dir, and is silent for repos without .gitmodules.
#[test]
fn doctor_scan_reports_uninitialized_submodule_in_workweave() {
    use repoweave::check::{scan_uninitialized_submodules_in_workweaves, CheckViolation};
    use repoweave::manifest::Project;

    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    // Create a submodule remote and add it to the member repo.
    let sub_remote = tmp.path().join("sub-remote");
    init_repo_with_commit(&sub_remote);

    let repo = ws.join("github/org/repo");
    add_submodule(&repo, "libs/sub", &sub_remote);

    // Create the workweave (submodule init will succeed at create time).
    let ww_dir = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir).unwrap();

    let workweave_path = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj"),
        &WorkweaveName::new("feat"),
        false,
        false,
        false,
    )
    .expect("create should succeed");

    // Now simulate the submodule being uninitialized by removing the content.
    let sub_dir = workweave_path.join("github/org/repo/libs/sub");
    if sub_dir.is_dir() {
        for entry in std::fs::read_dir(&sub_dir).unwrap().flatten() {
            let p = entry.path();
            if p.is_file() {
                std::fs::remove_file(&p).unwrap();
            }
        }
    }

    // Now the submodule dir exists but is empty → should be detected.
    let project_dir = ws.join("projects/proj");
    let project = Project::from_dir(&project_dir).expect("project should load");

    let violations =
        scan_uninitialized_submodules_in_workweaves(&ws, std::slice::from_ref(&project));

    let found = violations
        .iter()
        .any(|v| matches!(v, CheckViolation::UninitializedSubmodule { .. }));
    assert!(
        found,
        "expected UninitializedSubmodule finding; got {violations:?}"
    );
}

/// Doctor-side: scan_uninitialized_submodules_in_workweaves is silent for
/// a workweave repo with no .gitmodules.
#[test]
fn doctor_scan_silent_for_repo_without_gitmodules() {
    use repoweave::check::scan_uninitialized_submodules_in_workweaves;
    use repoweave::manifest::Project;

    let tmp = tempfile::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    // No submodule added.
    let ww_dir = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir).unwrap();

    let _workweave_path = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj"),
        &WorkweaveName::new("feat"),
        false,
        false,
        false,
    )
    .expect("create should succeed");

    let project_dir = ws.join("projects/proj");
    let project = Project::from_dir(&project_dir).expect("project should load");

    let violations =
        scan_uninitialized_submodules_in_workweaves(&ws, std::slice::from_ref(&project));

    assert!(
        violations.is_empty(),
        "expected no findings for repo without .gitmodules, got: {violations:?}"
    );
}
