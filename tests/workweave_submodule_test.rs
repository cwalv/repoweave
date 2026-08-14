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
///   `tmp/ws/projects/{project}/rwv.toml`
fn make_workspace(tmp: &Path, project: &str) -> PathBuf {
    let ws = tmp.join("ws");
    let repo_path = ws.join("github/org/repo");
    init_repo_with_commit(&repo_path);

    let project_dir = ws.join("projects").join(project);
    std::fs::create_dir_all(&project_dir).unwrap();

    let manifest = format!(
        r#"[repositories."github/org/repo"]
type = "git"
url = "file://{repo}"
version = "main"
role = "owned"
"#,
        repo = common::url_path(&repo_path)
    );
    std::fs::write(project_dir.join("rwv.toml"), manifest).unwrap();
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
            &common::file_url(submodule_remote),
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
///
/// Production no longer injects `protocol.file.allow=always` (CVE-2022-39253
/// class mitigation — see `init_submodules_in_worktree`), so this test runs
/// the create through the CLI binary and sets the allowance on ITS OWN
/// spawned command. The rwv process's child `git submodule update` inherits
/// the env; nothing is widened in the production code path.
#[test]
fn create_initializes_submodules_when_gitmodules_present() {
    let tmp = common::tempdir().unwrap();
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

    // Create the workweave via the CLI. The file-protocol allowance is set
    // on the spawned rwv process only (common::rwv() already sets
    // GIT_CONFIG_COUNT=1 for init.defaultBranch; stack entry 1 on top).
    let ww_dir = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir).unwrap();

    common::rwv()
        .args(["workweave", "proj", "create", "feat"])
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_1", "protocol.file.allow")
        .env("GIT_CONFIG_VALUE_1", "always")
        .current_dir(&ws)
        .assert()
        .success();

    // The submodule directory in the workweave should be non-empty.
    let workweave_path = ww_dir.join("proj--feat");
    assert!(
        workweave_path.is_dir(),
        "workweave should exist at {}",
        workweave_path.display()
    );
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
/// create still succeeds but emits a warning naming the repo, the state
/// ("submodules not initialized"), and the fix command. The worktree exists;
/// the submodule directory is empty (or absent). Doctor can later flag it.
///
/// We commit BOTH halves of a real submodule pointer — the `.gitmodules`
/// entry (with a URL that will never resolve) AND a gitlink index entry
/// (mode 160000, planted via `git update-index --cacheinfo`) — without ever
/// running `git submodule add`, so no cached submodule objects exist in
/// `.git/modules/`. The gitlink matters: `git submodule update` iterates
/// gitlink entries in the index, not `.gitmodules` lines, so a fixture with
/// only `.gitmodules` would make init a successful no-op and never reach
/// the failure arm.
///
/// Run through the CLI so the per-repo warning and the partial-
/// materialization summary can be asserted on stderr BY TEXT — the exact
/// git failure underneath (protocol blocked vs. path not found) is not
/// asserted; both take the same warn-and-continue arm.
#[test]
fn create_succeeds_with_warning_when_submodule_remote_unreachable() {
    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    let repo = ws.join("github/org/repo");

    // Write a .gitmodules pointing at a non-existent path.
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
    git(&["add", ".gitmodules"], &repo);
    // Plant the gitlink entry itself (mode 160000). The recorded commit sha
    // does not need to exist anywhere — gitlink checkout only creates the
    // placeholder dir, and submodule update will fail at clone time (which
    // is the point of this test).
    git(
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa,libs/sub",
        ],
        &repo,
    );
    git(&["commit", "-m", "record submodule with bad url"], &repo);

    let ww_dir = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir).unwrap();

    // Create MUST succeed (exit 0) even though submodule init fails, and
    // the warning must name the state and the fix.
    let assert = common::rwv()
        .args(["workweave", "proj", "create", "feat"])
        .current_dir(&ws)
        .assert()
        .success();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(
        stderr.contains("submodules not initialized"),
        "stderr should name the state 'submodules not initialized'; got:\n{stderr}"
    );
    assert!(
        stderr.contains("submodule update --init --recursive"),
        "stderr should name the fix command; got:\n{stderr}"
    );
    assert!(
        stderr.contains("github/org/repo"),
        "stderr should name the affected repo; got:\n{stderr}"
    );

    // The worktree exists and .gitmodules is there (submodule declared).
    let workweave_path = ww_dir.join("proj--feat");
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
    let tmp = common::tempdir().unwrap();
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
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false,
        false,
        false,
        None,
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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
    let tmp = common::tempdir().unwrap();
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

    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    // Create a submodule remote and add it to the member repo.
    let sub_remote = tmp.path().join("sub-remote");
    init_repo_with_commit(&sub_remote);

    let repo = ws.join("github/org/repo");
    add_submodule(&repo, "libs/sub", &sub_remote);

    // Create the workweave. Under git's default `protocol.file.allow=user`
    // posture the create-time submodule init is blocked (production does not
    // inject an allowance — CVE-2022-39253 class), so the create succeeds
    // with a warning and the workweave lands with the submodule dir empty:
    // exactly the state doctor must detect.
    let ww_dir = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir).unwrap();

    let workweave_path = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false,
        false,
        false,
        None,
    )
    .expect("create should succeed");

    // Defense-in-depth: if the environment's git config DID allow the init
    // (e.g. an operator global `protocol.file.allow=always`), empty the
    // submodule dir so the uninitialized state holds either way.
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

    let tmp = common::tempdir().unwrap();
    let ws = make_workspace(tmp.path(), "proj");

    // No submodule added.
    let ww_dir = tmp.path().join(".workweaves");
    std::fs::create_dir_all(&ww_dir).unwrap();

    let _workweave_path = create_workweave(
        &ws,
        &ws,
        &ProjectName::new("proj").unwrap(),
        &WorkweaveName::new("feat").unwrap(),
        false,
        false,
        false,
        None,
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
