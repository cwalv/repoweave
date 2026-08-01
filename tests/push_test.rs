//! E2E tests for `rwv push`.
//!
//! Each test sets up a workspace with:
//!   - one or more bare "manifest" remotes
//!   - local manifest-repo clones at canonical paths
//!   - a bare "project" remote with rwv.yaml + rwv.lock committed
//!   - a local project-repo clone under `projects/<name>/`
//!   - `.rwv-active` pointing at the project
//!
//! Then exercises `rwv push` via the CLI and asserts the publish-ordering
//! invariant and the precondition refuse-paths from the spec.

use assert_cmd::Command;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
    common::rwv()
}

/// Run `git` with the given args in `cwd`; panic on failure.
fn git_run(cwd: &Path, args: &[&str]) -> String {
    let output = common::git()
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should be available");
    if !output.status.success() {
        panic!(
            "git {:?} in {} failed: {}",
            args,
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// Initialize a bare repo and seed it with one commit on `main` so it can
/// be cloned by `--origin` consumers and act as a push target.
fn init_bare_repo_with_commit(bare: &Path) {
    let parent = bare.parent().expect("bare repo path needs a parent");
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    git_run(
        parent,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    let seed = parent.join(format!("__seed_{stem}"));
    git_run(
        parent,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    git_run(&seed, &["config", "user.email", "test@test.com"]);
    git_run(&seed, &["config", "user.name", "Test"]);
    std::fs::write(seed.join("README"), "seed").unwrap();
    git_run(&seed, &["add", "."]);
    git_run(&seed, &["commit", "-m", "initial"]);
    git_run(&seed, &["push", "origin", "main"]);
}

/// A test workspace ready to be driven by `rwv push`.
///
/// Holds the workspace root and the bare-remote paths so tests can both
/// invoke `rwv push` against it and inspect the bare remotes to verify
/// what was pushed.
struct PushWorkspace {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    project_name: String,
    project_bare: PathBuf,
    manifest_bares: Vec<(String, PathBuf)>,
}

/// Build a workspace with `repos.len()` manifest repos plus a project repo.
///
/// Each manifest repo gets a bare remote, a canonical-path local clone, and
/// is referenced by `rwv.yaml`. The project repo gets a bare remote and a
/// clone under `projects/<project_name>/`. `rwv.lock` is generated to match
/// the manifest repos' local HEAD SHAs. Returns the workspace handle.
fn build_workspace(project_name: &str, repos: &[(&str, &str)]) -> PushWorkspace {
    // repos is &[(canonical_path, role)]
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

    // Build manifest bare remotes and local clones.
    let mut manifest_bares: Vec<(String, PathBuf)> = Vec::new();
    let mut manifest_shas: Vec<(String, String)> = Vec::new();
    let mut manifest_yaml = String::from("repositories:\n");
    for (repo_path, role) in repos {
        let bare = tmp
            .path()
            .join(format!("{}.git", repo_path.replace('/', "_")));
        init_bare_repo_with_commit(&bare);

        let canonical = workspace.join(repo_path);
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        git_run(
            workspace.parent().unwrap(),
            &[
                "clone",
                "--origin",
                "origin",
                bare.to_str().unwrap(),
                canonical.to_str().unwrap(),
            ],
        );
        git_run(&canonical, &["config", "user.email", "test@test.com"]);
        git_run(&canonical, &["config", "user.name", "Test"]);
        let head = git_run(&canonical, &["rev-parse", "HEAD"]);
        manifest_shas.push(((*repo_path).to_string(), head));
        manifest_bares.push(((*repo_path).to_string(), bare.clone()));
        let bare_url = bare.to_str().unwrap();
        manifest_yaml.push_str(&format!(
            "  {repo_path}:\n    type: git\n    url: {bare_url}\n    version: main\n    role: {role}\n"
        ));
    }

    // Build a project bare and a `projects/<name>/` clone, then commit
    // rwv.yaml + rwv.lock and push back to the bare.
    let project_bare = tmp.path().join("project.git");
    init_bare_repo_with_commit(&project_bare);
    let project_dir = workspace.join("projects").join(project_name);
    git_run(
        workspace.parent().unwrap(),
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_dir.to_str().unwrap(),
        ],
    );
    git_run(&project_dir, &["config", "user.email", "test@test.com"]);
    git_run(&project_dir, &["config", "user.name", "Test"]);

    std::fs::write(project_dir.join("rwv.yaml"), &manifest_yaml).unwrap();

    // Write a lock that exactly matches manifest HEAD SHAs. Round-trips
    // through the real parser + `lock::write_lock`: a hand-formatted string
    // that differs only in whitespace from what `rwv lock` itself would
    // emit still diffs against a real relock.
    let mut lock_entries = Vec::new();
    for (rp, sha) in &manifest_shas {
        let (_, bare) = manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let bare_url = bare.to_str().unwrap();
        lock_entries.push(format!(
            "{rp:?}: {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {sha:?}}}"
        ));
    }
    let raw_lock = format!("{{\"repositories\": {{{}}}}}", lock_entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();

    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "manifest + lock"]);

    // Mark this project active.
    std::fs::write(workspace.join(".rwv-active"), format!("{project_name}\n")).unwrap();

    PushWorkspace {
        _tmp: tmp,
        workspace,
        project_name: project_name.to_string(),
        project_bare,
        manifest_bares,
    }
}

/// Get the `main` SHA in a bare repo, or `None` if `main` doesn't exist
/// (e.g. nothing was ever pushed).
fn bare_main_sha(bare: &Path) -> Option<String> {
    let output = common::git()
        .args(["rev-parse", "main"])
        .current_dir(bare)
        .output()
        .expect("git should be available");
    if output.status.success() {
        Some(String::from_utf8(output.stdout).unwrap().trim().to_string())
    } else {
        None
    }
}

// ============================================================================
// Happy path
// ============================================================================

/// Happy path: bare `rwv push` pushes Owned + Fork; Dependency repos are
/// skipped (plan-time default). The project repo is pushed last.
#[test]
fn push_happy_path_pushes_manifest_then_project() {
    let ws = build_workspace(
        "alpha",
        &[
            ("local/org/a", "owned"),
            ("local/org/b", "fork"),
            ("local/org/c", "dependency"),
        ],
    );

    // Advance each manifest repo with a new commit so there's something to
    // push. Re-write the lock to match new HEADs.
    let repos = [
        ("local/org/a", "owned"),
        ("local/org/b", "fork"),
        ("local/org/c", "dependency"),
    ];
    let mut manifest_yaml = String::from("repositories:\n");
    let mut lock_entries = Vec::new();
    let mut expected_shas: Vec<(String, String)> = Vec::new();
    for (rp, role) in &repos {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let local = ws.workspace.join(rp);
        std::fs::write(local.join("changed.txt"), "new").unwrap();
        git_run(&local, &["add", "."]);
        git_run(&local, &["commit", "-m", "advance"]);
        let sha = git_run(&local, &["rev-parse", "HEAD"]);
        let bare_url = bare.to_str().unwrap();
        manifest_yaml.push_str(&format!(
            "  {rp}:\n    type: git\n    url: {bare_url}\n    version: main\n    role: {role}\n"
        ));
        lock_entries.push(format!(
            "{rp:?}: {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {sha:?}}}"
        ));
        expected_shas.push(((*rp).to_string(), sha));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("rwv.yaml"), &manifest_yaml).unwrap();
    // Round-trips through the real parser + `lock::write_lock` (see
    // `build_workspace` above for why).
    let raw_lock = format!("{{\"repositories\": {{{}}}}}", lock_entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "advance lock"]);
    let project_head = git_run(&project_dir, &["rev-parse", "HEAD"]);

    // Record the dependency's baseline SHA before push (it must NOT advance).
    let (_, dep_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/c")
        .unwrap();
    let dep_baseline = bare_main_sha(dep_bare);

    rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    // Owned + Fork repos must advance; Dependency must NOT (default plan skips it).
    let (_, owned_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/a")
        .unwrap();
    let (_, fork_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/b")
        .unwrap();
    let (_, owned_sha) = expected_shas
        .iter()
        .find(|(p, _)| p == "local/org/a")
        .unwrap();
    let (_, fork_sha) = expected_shas
        .iter()
        .find(|(p, _)| p == "local/org/b")
        .unwrap();

    assert_eq!(
        bare_main_sha(owned_bare),
        Some(owned_sha.clone()),
        "owned repo should be pushed"
    );
    assert_eq!(
        bare_main_sha(fork_bare),
        Some(fork_sha.clone()),
        "fork repo should be pushed"
    );
    assert_eq!(
        bare_main_sha(dep_bare),
        dep_baseline,
        "dependency bare must NOT advance under bare rwv push (default plan skips non-writable roles)"
    );
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        Some(project_head),
        "project bare should match local project HEAD"
    );
}

#[test]
fn push_dry_run_prints_plan_and_pushes_nothing() {
    let ws = build_workspace("alpha", &[("local/org/a", "owned")]);

    // Capture the baseline manifest-bare SHA before dry-run.
    let (_, manifest_bare) = &ws.manifest_bares[0];
    let baseline_manifest = bare_main_sha(manifest_bare);
    let baseline_project = bare_main_sha(&ws.project_bare);

    // Make a local advance so a real push would change things.
    let local = ws.workspace.join("local/org/a");
    std::fs::write(local.join("x.txt"), "x").unwrap();
    git_run(&local, &["add", "."]);
    git_run(&local, &["commit", "-m", "advance"]);
    let new_sha = git_run(&local, &["rev-parse", "HEAD"]);

    // Rewrite lock to match the new HEAD so the precondition passes.
    let bare_url = manifest_bare.to_str().unwrap();
    let raw_lock = format!(
        "{{\"repositories\": {{\"local/org/a\": {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {new_sha:?}}}}}}}"
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "relock"]);

    let output = rwv()
        .args(["push", "--dry-run"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push --dry-run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "dry-run should succeed: {stdout}");
    assert!(
        stdout.contains("dry-run"),
        "dry-run output should announce itself; got: {stdout}"
    );
    assert!(
        stdout.contains("local/org/a"),
        "dry-run should list manifest repo; got: {stdout}"
    );
    assert!(
        stdout.contains("projects/alpha"),
        "dry-run should list project repo; got: {stdout}"
    );

    // Nothing should have moved.
    assert_eq!(
        bare_main_sha(manifest_bare),
        baseline_manifest,
        "dry-run must not touch the manifest bare"
    );
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        baseline_project,
        "dry-run must not touch the project bare"
    );
}

// ============================================================================
// Negative: workweave invocation refused
// ============================================================================

#[test]
fn push_refuses_from_workweave() {
    let ws = build_workspace("alpha", &[("local/org/a", "owned")]);

    // Drop a `.rwv-workweave` marker in a sibling dir so resolve sees a
    // Workweave. This sidesteps the need to actually run `rwv workweave
    // create` (which requires a much heavier setup).
    let workweave_dir = ws.workspace.parent().unwrap().join("alpha--feat");
    std::fs::create_dir_all(&workweave_dir).unwrap();
    let primary_path = ws.workspace.display().to_string();
    let marker = format!(
        "{{\"primary\":\"{p}\",\"project\":\"{proj}\",\"parent\":\"{p}\"}}",
        p = primary_path,
        proj = ws.project_name
    );
    std::fs::write(workweave_dir.join(".rwv-workweave"), marker).unwrap();

    let output = rwv()
        .args(["push"])
        .current_dir(&workweave_dir)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "push from workweave must fail; stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("workweave"),
        "error should mention workweave; got: {stderr}"
    );
    // Hint must name sync-to, not the wrong direction `sync primary`.
    assert!(
        stderr.contains("sync-to"),
        "error hint must name `rwv sync-to` (not `rwv sync primary`); got: {stderr}"
    );
    assert!(
        !stderr.contains("sync primary"),
        "error hint must NOT say `sync primary` (wrong direction); got: {stderr}"
    );
}

// ============================================================================
// Negative: lock-state mismatch — bail before touching the network
// ============================================================================

#[test]
fn push_refuses_when_lock_disagrees_with_local_state() {
    let ws = build_workspace("alpha", &[("local/org/a", "owned")]);
    let (_, manifest_bare) = &ws.manifest_bares[0];
    let baseline_manifest = bare_main_sha(manifest_bare);

    // Advance the local repo WITHOUT updating the lock.
    let local = ws.workspace.join("local/org/a");
    std::fs::write(local.join("drift.txt"), "drift").unwrap();
    git_run(&local, &["add", "."]);
    git_run(&local, &["commit", "-m", "drift past lock"]);

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "push must refuse when lock and state disagree"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lock") || stderr.contains("rwv lock") || stderr.contains("git checkout"),
        "error should hint at `rwv lock` or `git checkout`; got: {stderr}"
    );
    // Network must not have been touched.
    assert_eq!(
        bare_main_sha(manifest_bare),
        baseline_manifest,
        "lock-mismatch refuse must happen before any push"
    );
}

// ============================================================================
// Negative: detached HEAD refused
// ============================================================================

#[test]
fn push_refuses_detached_head() {
    let ws = build_workspace("alpha", &[("local/org/a", "owned")]);
    // Detach HEAD in the manifest repo.
    let local = ws.workspace.join("local/org/a");
    let head_sha = git_run(&local, &["rev-parse", "HEAD"]);
    git_run(&local, &["checkout", &head_sha]);

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(!output.status.success(), "detached HEAD must refuse");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("detached"),
        "error should mention detached HEAD; got: {stderr}"
    );
}

// ============================================================================
// Negative: project repo off canonical branch refused
// ============================================================================

#[test]
fn push_refuses_when_project_repo_off_canonical_branch() {
    let ws = build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    // Move project repo to a non-canonical branch.
    git_run(&project_dir, &["checkout", "-b", "feat/x"]);

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "off-canonical-branch project must refuse"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("canonical") || stderr.contains("branch"),
        "error should mention canonical branch; got: {stderr}"
    );
}

// ============================================================================
// Negative: project repo's `origin/HEAD` is unset — no "main" fallback
// ============================================================================

/// branch-model.md §4.2/§4.6(2): `RemoteDefaultBranch`'s sole producer
/// returns `None` when `origin/HEAD` is unset, and the gate must refuse
/// rather than fabricate "main". Proves the refusal by construction: the
/// project repo here IS on `main` (the real canonical branch `git clone`
/// would have recorded), so the old fallback-to-"main" behaviour would
/// have let this push through silently — only the typed `None` path
/// catches it.
#[test]
fn push_refuses_when_project_repo_origin_head_unset() {
    let ws = build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    git_run(
        &project_dir,
        &["symbolic-ref", "--delete", "refs/remotes/origin/HEAD"],
    );

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "unset origin/HEAD must refuse rather than fall back to a guessed branch"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("origin/HEAD is unset"),
        "error should name the unset origin/HEAD condition, not a fabricated branch; got: {stderr}"
    );
}

// ============================================================================
// Negative: project repo directory is not a VCS repo at all
// ============================================================================

/// branch-model.md §4.5/§4.6(2): a non-repo `projects/<name>/` must surface
/// as `NotARepo`, not be misreported as a detached HEAD (the shipped bug
/// `current_ref`'s `Ok(None)` collapse produced).
#[test]
fn push_refuses_when_project_repo_is_not_a_repo() {
    let ws = build_workspace("alpha", &[("local/org/a", "owned")]);
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::remove_dir_all(project_dir.join(".git")).unwrap();

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "a project dir with no .git must refuse"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not a vcs repository") || stderr.contains("not a repo"),
        "error should name the not-a-repo condition; got: {stderr}"
    );
    assert!(
        !stderr.contains("detached"),
        "a non-repo directory must not be misreported as a detached HEAD; got: {stderr}"
    );
}

// ============================================================================
// Branch-mismatch warning is non-fatal
// ============================================================================

#[test]
fn push_warns_but_succeeds_when_manifest_repo_on_other_branch() {
    let ws = build_workspace("alpha", &[("local/org/a", "owned")]);

    // Create a new branch in the manifest repo and commit there — the
    // manifest declares `main`, so this should warn.
    let local = ws.workspace.join("local/org/a");
    git_run(&local, &["checkout", "-b", "feat-x"]);
    std::fs::write(local.join("f.txt"), "f").unwrap();
    git_run(&local, &["add", "."]);
    git_run(&local, &["commit", "-m", "feat advance"]);
    let feat_sha = git_run(&local, &["rev-parse", "HEAD"]);

    // Update lock to point at the new SHA (HEAD on feat-x).
    let (_, bare) = &ws.manifest_bares[0];
    let bare_url = bare.to_str().unwrap();
    let raw_lock = format!(
        "{{\"repositories\": {{\"local/org/a\": {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {feat_sha:?}}}}}}}"
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "relock"]);

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        output.status.success(),
        "branch-mismatch is non-fatal warn; stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("warning") && (stderr.contains("feat-x") || stderr.contains("main")),
        "expected branch-mismatch warning; got stderr: {stderr}"
    );

    // The bare's `feat-x` ref should now exist and equal feat_sha.
    let bare_feat = common::git()
        .args(["rev-parse", "feat-x"])
        .current_dir(bare)
        .output()
        .unwrap();
    assert!(bare_feat.status.success(), "feat-x must exist on bare");
    let bare_feat_sha = String::from_utf8(bare_feat.stdout)
        .unwrap()
        .trim()
        .to_string();
    assert_eq!(bare_feat_sha, feat_sha);
}

/// branch-model.md §4.6(2): the warning is built from two typed refs — the
/// checkout's `AttachedRef` witness and the manifest's declared
/// `TrackingRef` — routed through named projections instead of a raw
/// string compare. Assert both names appear in the *same* warning line
/// (not just "either", as the looser check above allows), pinning that the
/// typed refactor didn't drop or garble either side.
#[test]
fn push_branch_mismatch_warning_names_both_observed_and_declared_branch() {
    let ws = build_workspace("alpha", &[("local/org/a", "owned")]);

    let local = ws.workspace.join("local/org/a");
    git_run(&local, &["checkout", "-b", "feat-x"]);
    std::fs::write(local.join("f.txt"), "f").unwrap();
    git_run(&local, &["add", "."]);
    git_run(&local, &["commit", "-m", "feat advance"]);
    let feat_sha = git_run(&local, &["rev-parse", "HEAD"]);

    let (_, bare) = &ws.manifest_bares[0];
    let bare_url = bare.to_str().unwrap();
    let raw_lock = format!(
        "{{\"repositories\": {{\"local/org/a\": {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {feat_sha:?}}}}}}}"
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "relock"]);

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(output.status.success(), "branch-mismatch is non-fatal warn");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let expected = "rwv push: warning: local/org/a is on branch 'feat-x', manifest declares 'main'";
    assert!(
        stderr.contains(expected),
        "expected warning naming both the observed branch ('feat-x') and the \
         manifest's declared branch ('main') in one line; got stderr: {stderr}"
    );
}

// ============================================================================
// Negative: manifest-repo push failure — project repo NOT pushed
// ============================================================================

#[test]
fn push_aborts_before_project_when_manifest_push_fails() {
    // Build a workspace; then break a manifest bare so its push fails. The
    // project bare must remain untouched.
    let ws = build_workspace(
        "alpha",
        &[("local/org/a", "owned"), ("local/org/b", "owned")],
    );

    let baseline_project = bare_main_sha(&ws.project_bare);

    // Advance both local repos.
    let mut lock_entries = Vec::new();
    let mut expected_shas: Vec<String> = Vec::new();
    for (rp, bare) in &ws.manifest_bares {
        let local = ws.workspace.join(rp);
        std::fs::write(local.join("x.txt"), "x").unwrap();
        git_run(&local, &["add", "."]);
        git_run(&local, &["commit", "-m", "advance"]);
        let sha = git_run(&local, &["rev-parse", "HEAD"]);
        expected_shas.push(sha.clone());
        let bare_url = bare.to_str().unwrap();
        lock_entries.push(format!(
            "{rp:?}: {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {sha:?}}}"
        ));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    let raw_lock = format!("{{\"repositories\": {{{}}}}}", lock_entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "relock"]);

    // Sabotage repo B's remote URL so push fails (point at a nonexistent
    // location). Repo A pushes succeed; B fails; project must not be pushed.
    let local_b = ws.workspace.join("local/org/b");
    let bad_url = ws.workspace.join("nonexistent-remote.git");
    git_run(
        &local_b,
        &["remote", "set-url", "origin", bad_url.to_str().unwrap()],
    );

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "push must fail when any manifest push fails"
    );
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        baseline_project,
        "project bare must NOT advance when a manifest push fails"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("project repo not pushed")
            || stderr.contains("manifest-side partial state"),
        "error should mention partial state / project-not-pushed; got: {stderr}"
    );
}

// ============================================================================
// Negative: project-repo push failure — manifest repos already pushed
// ============================================================================

#[test]
fn push_surfaces_project_push_failure_after_manifest_pushed() {
    let ws = build_workspace("alpha", &[("local/org/a", "owned")]);

    // Advance the manifest repo and the lock so the precondition passes.
    let (_, manifest_bare) = &ws.manifest_bares[0];
    let local = ws.workspace.join("local/org/a");
    std::fs::write(local.join("x.txt"), "x").unwrap();
    git_run(&local, &["add", "."]);
    git_run(&local, &["commit", "-m", "advance"]);
    let manifest_sha = git_run(&local, &["rev-parse", "HEAD"]);

    let bare_url = manifest_bare.to_str().unwrap();
    let raw_lock = format!(
        "{{\"repositories\": {{\"local/org/a\": {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {manifest_sha:?}}}}}}}"
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "relock"]);

    // Sabotage the project repo's origin so its push fails.
    let bad_url = ws.workspace.join("nonexistent-project.git");
    git_run(
        &project_dir,
        &["remote", "set-url", "origin", bad_url.to_str().unwrap()],
    );

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "push must fail when project-repo push fails"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("project-repo push") || stderr.contains("lock carrier is not"),
        "error should surface project-side failure clearly; got: {stderr}"
    );

    // The manifest bare DID move (manifest repos pushed before the project
    // attempt). This is the surface-clearly behaviour the spec requires.
    assert_eq!(
        bare_main_sha(manifest_bare),
        Some(manifest_sha),
        "manifest repo should already be pushed before project-repo failure"
    );
}

// ============================================================================
// CLI plumbing
// ============================================================================

#[test]
fn push_command_is_registered() {
    let output = rwv()
        .args(["push", "--help"])
        .output()
        .expect("rwv push --help");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        combined.contains("--dry-run") && combined.contains("--force"),
        "push --help should list --dry-run and --force; got: {combined}"
    );
}

#[test]
fn push_requires_a_workspace() {
    let tmp = common::tempdir().unwrap();
    let output = rwv()
        .args(["push"])
        .current_dir(tmp.path())
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "push outside a workspace must fail"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no repoweave workspace"),
        "error should mention no workspace; got: {stderr}"
    );
}

// ============================================================================
// `--role` / `--repo` filter
// ============================================================================

/// Build a workspace + advance every manifest repo + write a matching lock.
/// Common setup for the filter tests so each test can focus on the
/// flag-specific assertions.
fn build_workspace_with_advances(
    project_name: &str,
    repos: &[(&str, &str)],
) -> (PushWorkspace, Vec<(String, String)>) {
    let ws = build_workspace(project_name, repos);

    // Advance each manifest repo with a distinct commit so SHAs differ.
    let mut manifest_yaml = String::from("repositories:\n");
    let mut lock_entries = Vec::new();
    let mut expected_shas: Vec<(String, String)> = Vec::new();
    for ((rp, bare), (_, role)) in ws.manifest_bares.iter().zip(repos.iter()) {
        let local = ws.workspace.join(rp);
        std::fs::write(local.join(format!("{}.txt", rp.replace('/', "_"))), rp).unwrap();
        git_run(&local, &["add", "."]);
        git_run(&local, &["commit", "-m", &format!("advance {rp}")]);
        let sha = git_run(&local, &["rev-parse", "HEAD"]);
        let bare_url = bare.to_str().unwrap();
        manifest_yaml.push_str(&format!(
            "  {rp}:\n    type: git\n    url: {bare_url}\n    version: main\n    role: {role}\n"
        ));
        lock_entries.push(format!(
            "{rp:?}: {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {sha:?}}}"
        ));
        expected_shas.push((rp.clone(), sha));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("rwv.yaml"), &manifest_yaml).unwrap();
    let raw_lock = format!("{{\"repositories\": {{{}}}}}", lock_entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "advance lock"]);

    (ws, expected_shas)
}

#[test]
fn push_role_filter_only_pushes_matching_role() {
    let (ws, expected_shas) = build_workspace_with_advances(
        "alpha",
        &[("local/org/p", "owned"), ("local/org/d", "dependency")],
    );
    let baseline_d = bare_main_sha(&ws.manifest_bares[1].1);

    rwv()
        .args(["push", "--role", "owned"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let (p_path, p_expected) = &expected_shas[0];
    let (_, p_bare) = ws.manifest_bares.iter().find(|(p, _)| p == p_path).unwrap();
    assert_eq!(
        bare_main_sha(p_bare),
        Some(p_expected.clone()),
        "primary repo bare must advance"
    );

    assert_eq!(
        bare_main_sha(&ws.manifest_bares[1].1),
        baseline_d,
        "dependency bare must NOT advance under --role owned"
    );
}

#[test]
fn push_repo_exact_filter_pushes_only_that_path() {
    let (ws, expected_shas) = build_workspace_with_advances(
        "alpha",
        &[("local/org/a", "owned"), ("local/org/b", "owned")],
    );
    let baseline_b = bare_main_sha(&ws.manifest_bares[1].1);

    rwv()
        .args(["push", "--repo", "local/org/a"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    assert_eq!(
        bare_main_sha(&ws.manifest_bares[0].1),
        Some(expected_shas[0].1.clone())
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[1].1),
        baseline_b,
        "b bare must NOT advance"
    );
}

#[test]
fn push_repo_glob_filter_pushes_matching() {
    let (ws, expected_shas) = build_workspace_with_advances(
        "alpha",
        &[
            ("local/org/a", "owned"),
            ("local/org/b", "owned"),
            ("local/other/c", "owned"),
        ],
    );
    let baseline_c = bare_main_sha(&ws.manifest_bares[2].1);

    rwv()
        .args(["push", "--repo", "glob:local/org/*"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    for (i, (rp, expected)) in expected_shas.iter().enumerate() {
        let bare = &ws.manifest_bares[i].1;
        if rp.starts_with("local/org/") {
            assert_eq!(bare_main_sha(bare), Some(expected.clone()), "{rp}");
        }
    }
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[2].1),
        baseline_c,
        "other/c must NOT advance"
    );
}

#[test]
fn push_repo_regex_filter_pushes_matching() {
    let (ws, expected_shas) = build_workspace_with_advances(
        "alpha",
        &[
            ("local/cwalv/a", "owned"),
            ("local/cwalv/b", "owned"),
            ("local/other/c", "owned"),
        ],
    );
    let baseline_c = bare_main_sha(&ws.manifest_bares[2].1);

    rwv()
        .args(["push", "--repo", "re:^local/cwalv/"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    for (i, (rp, expected)) in expected_shas.iter().enumerate() {
        if rp.starts_with("local/cwalv/") {
            assert_eq!(
                bare_main_sha(&ws.manifest_bares[i].1),
                Some(expected.clone())
            );
        }
    }
    assert_eq!(bare_main_sha(&ws.manifest_bares[2].1), baseline_c);
}

#[test]
fn push_union_role_and_repo_selectors() {
    let (ws, expected_shas) = build_workspace_with_advances(
        "alpha",
        &[
            ("local/me/p", "owned"),
            ("local/external/dep", "dependency"),
            ("local/external/other", "dependency"),
        ],
    );
    let baseline_other = bare_main_sha(&ws.manifest_bares[2].1);

    rwv()
        .args(["push", "--role", "owned", "--repo", "local/external/dep"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    // Primary advances via --role; external/dep via --repo.
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[0].1),
        Some(expected_shas[0].1.clone()),
        "primary should advance"
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[1].1),
        Some(expected_shas[1].1.clone()),
        "exact-named dep should advance"
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[2].1),
        baseline_other,
        "unmatched dep must NOT advance"
    );
}

// ----------------------------------------------------------------------------
// Lock-precondition always runs against the FULL manifest, never the
// filter — the filter narrows the push loop, not the precondition.
// ----------------------------------------------------------------------------

#[test]
fn push_filter_still_runs_lock_precondition_against_full_manifest() {
    // Build a 2-repo workspace. Advance BOTH local clones but only update the
    // lock for repo A. Push with --repo local/org/a (the in-sync one). The
    // push must still refuse — repo B's HEAD drifts from the committed lock,
    // and the committed lock is shared with collaborators regardless of
    // which subset the operator pushes today.
    let ws = build_workspace(
        "alpha",
        &[("local/org/a", "owned"), ("local/org/b", "owned")],
    );

    let baseline_a = bare_main_sha(&ws.manifest_bares[0].1);
    let baseline_b = bare_main_sha(&ws.manifest_bares[1].1);

    // Advance both locals.
    let mut new_shas: Vec<String> = Vec::new();
    for (rp, _) in &ws.manifest_bares {
        let local = ws.workspace.join(rp);
        std::fs::write(local.join(format!("{}.txt", rp.replace('/', "_"))), rp).unwrap();
        git_run(&local, &["add", "."]);
        git_run(&local, &["commit", "-m", &format!("advance {rp}")]);
        new_shas.push(git_run(&local, &["rev-parse", "HEAD"]));
    }

    // Update lock for A only — leaving B's lock entry stale.
    let (_, a_bare) = &ws.manifest_bares[0];
    let (_, b_bare) = &ws.manifest_bares[1];
    let stale_b_lock_sha = git_run(
        &ws.workspace.join("local/org/b"),
        &["rev-parse", "HEAD~1"], // the original lock SHA from build_workspace
    );
    let raw_lock = format!(
        "{{\"repositories\": {{\"local/org/a\": {{\"type\": \"git\", \"url\": {a:?}, \"version\": {a_sha:?}}}, \"local/org/b\": {{\"type\": \"git\", \"url\": {b:?}, \"version\": {b_stale:?}}}}}}}",
        a = a_bare.to_str().unwrap(),
        a_sha = new_shas[0],
        b = b_bare.to_str().unwrap(),
        b_stale = stale_b_lock_sha,
    );
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "partial relock"]);

    let output = rwv()
        .args(["push", "--repo", "local/org/a"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        !output.status.success(),
        "filtered push must still refuse when an unfiltered repo's lock disagrees with HEAD; stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("local/org/b"),
        "error should name the unfiltered repo with the lock mismatch; got: {stderr}"
    );

    // Neither bare should have advanced: lock-precondition bails before
    // touching the network.
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[0].1),
        baseline_a,
        "lock-precondition refusal must happen before any network call"
    );
    assert_eq!(bare_main_sha(&ws.manifest_bares[1].1), baseline_b);
}
