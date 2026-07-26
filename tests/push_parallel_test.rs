//! Acceptance tests for `rwv push -j N`.
//!
//! Mirrors the shape of `tests/parallel_test.rs` (fetch/update
//! coverage) but for push. The fixtures duplicate `tests/push_test.rs`'s
//! setup helpers (bare-remote + local-clone + project-repo + lock) so
//! these tests stand alone — the same pattern parallel_test.rs uses
//! against fetch_test.rs.
//!
//! Coverage:
//!
//! - Happy path with `-j > 1`: every manifest bare advances; project bare
//!   advances last.
//! - Order invariant: project repo push happens AFTER all manifest pushes
//!   complete (proved by failure path — a manifest failure stops the
//!   project bare from being touched).
//! - Mid-batch failure under `-j > 1`: other pushes still complete; project
//!   bare NOT touched; failed repo surfaces in the aggregated summary.
//! - `[<repo>]` prefix appears under `-j > 1`.
//! - `-j 1` reproduces serial output (no `[<repo>]` prefix).

use assert_cmd::Command;
use std::path::{Path, PathBuf};

mod common;

fn rwv() -> Command {
    common::rwv()
}

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

struct PushWorkspace {
    _tmp: tempfile::TempDir,
    workspace: PathBuf,
    project_name: String,
    project_bare: PathBuf,
    manifest_bares: Vec<(String, PathBuf)>,
}

fn build_workspace(project_name: &str, repos: &[(&str, &str)]) -> PushWorkspace {
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

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

    let mut lock_yaml = String::from("repositories:\n");
    for (rp, sha) in &manifest_shas {
        let (_, bare) = manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let bare_url = bare.to_str().unwrap();
        lock_yaml.push_str(&format!(
            "  {rp}:\n    type: git\n    url: {bare_url}\n    version: {sha}\n"
        ));
    }
    std::fs::write(project_dir.join("rwv.lock"), lock_yaml).unwrap();

    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "manifest + lock"]);

    std::fs::write(workspace.join(".rwv-active"), format!("{project_name}\n")).unwrap();

    PushWorkspace {
        _tmp: tmp,
        workspace,
        project_name: project_name.to_string(),
        project_bare,
        manifest_bares,
    }
}

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

/// Advance every manifest repo with one new commit and rewrite the lock to
/// match. Returns the (repo_path, new SHA) pairs and the project repo's
/// HEAD SHA after committing the lock update.
fn advance_all_and_relock(
    ws: &PushWorkspace,
    repos: &[(&str, &str)],
) -> (Vec<(String, String)>, String) {
    let mut manifest_yaml = String::from("repositories:\n");
    let mut lock_yaml = String::from("repositories:\n");
    let mut expected_shas: Vec<(String, String)> = Vec::new();
    for (rp, role) in repos {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let local = ws.workspace.join(rp);
        std::fs::write(
            local.join(format!("changed_{}.txt", rp.replace('/', "_"))),
            "new",
        )
        .unwrap();
        git_run(&local, &["add", "."]);
        git_run(&local, &["commit", "-m", "advance"]);
        let sha = git_run(&local, &["rev-parse", "HEAD"]);
        let bare_url = bare.to_str().unwrap();
        manifest_yaml.push_str(&format!(
            "  {rp}:\n    type: git\n    url: {bare_url}\n    version: main\n    role: {role}\n"
        ));
        lock_yaml.push_str(&format!(
            "  {rp}:\n    type: git\n    url: {bare_url}\n    version: {sha}\n"
        ));
        expected_shas.push(((*rp).to_string(), sha));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("rwv.yaml"), &manifest_yaml).unwrap();
    std::fs::write(project_dir.join("rwv.lock"), &lock_yaml).unwrap();
    git_run(&project_dir, &["add", "."]);
    git_run(&project_dir, &["commit", "-m", "advance lock"]);
    let project_head = git_run(&project_dir, &["rev-parse", "HEAD"]);
    (expected_shas, project_head)
}

// ----- Tests -----------------------------------------------------------------

/// Happy path under `-j > 1`: writable manifest bares (Owned + Fork) advance
/// and the project bare advances at the end. Dependency repos are skipped by
/// the default plan. Five manifest repos with `-j 4` exercises concurrency
/// without being slow.
#[test]
fn push_dash_j_pushes_all_manifest_then_project() {
    let repos = [
        ("local/org/a", "owned"),
        ("local/org/b", "owned"),
        ("local/org/c", "fork"),
        ("local/org/d", "owned"),
        ("local/org/e", "owned"),
    ];
    let ws = build_workspace("alpha", &repos);
    let (expected_shas, project_head) = advance_all_and_relock(&ws, &repos);

    rwv()
        .args(["push", "-j", "4"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    for (rp, expected_sha) in &expected_shas {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let bare_sha = bare_main_sha(bare).expect("bare main must exist after parallel push");
        assert_eq!(
            &bare_sha, expected_sha,
            "{rp} bare should match local HEAD under -j 4"
        );
    }
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        Some(project_head),
        "project bare should match local project HEAD after parallel manifest pushes"
    );
}

/// Under `-j > 1` per-repo lines carry the `[<repo-path>]` prefix so
/// interleaved output is parseable. Mirrors `make -j` / `ninja`.
#[test]
fn push_dash_j_emits_repo_prefix() {
    let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
    let ws = build_workspace("alpha", &repos);
    let (_, _) = advance_all_and_relock(&ws, &repos);

    let output = rwv()
        .args(["push", "-j", "2"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push -j 2");
    assert!(
        output.status.success(),
        "push -j 2 should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("[local/org/a]") && stdout.contains("[local/org/b]"),
        "expected [<repo>] prefix for each repo under -j 2, got:\n{stdout}"
    );
}

/// `-j 1` reproduces serial behaviour: output never contains a
/// `[<repo-path>]` prefix. The per-repo "rwv push: pushing X" lines must
/// still be present, just unprefixed.
#[test]
fn push_dash_j_one_emits_no_prefix() {
    let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
    let ws = build_workspace("alpha", &repos);
    let (_, _) = advance_all_and_relock(&ws, &repos);

    let output = rwv()
        .args(["push", "-j", "1"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push -j 1");
    assert!(
        output.status.success(),
        "push -j 1 should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("[local/org/a]") && !stdout.contains("[local/org/b]"),
        "expected no [<repo>] prefix under -j 1, got:\n{stdout}"
    );
    // Sanity: the unprefixed per-repo lines still appear.
    assert!(
        stdout.contains("rwv push: pushing local/org/a")
            && stdout.contains("rwv push: pushing local/org/b"),
        "expected unprefixed per-repo lines under -j 1, got:\n{stdout}"
    );
}

/// Under `-j > 1` a failing manifest push doesn't prevent the other
/// manifest pushes from being attempted; the failure surfaces in the
/// trailing aggregated summary; the project bare is NOT touched (the
/// publish-ordering invariant holds under parallelism).
#[test]
fn push_dash_j_mid_batch_failure_skips_project_repo() {
    let repos = [
        ("local/org/ok1", "owned"),
        ("local/org/ok2", "owned"),
        ("local/org/bad", "owned"),
    ];
    let ws = build_workspace("alpha", &repos);
    let baseline_project = bare_main_sha(&ws.project_bare);
    let (expected_shas, _) = advance_all_and_relock(&ws, &repos);

    // Sabotage one repo's remote so its push fails. The other two should
    // still complete; project repo must NOT be pushed.
    let local_bad = ws.workspace.join("local/org/bad");
    let bad_url = ws.workspace.join("nonexistent-remote.git");
    git_run(
        &local_bad,
        &["remote", "set-url", "origin", bad_url.to_str().unwrap()],
    );

    let output = rwv()
        .args(["push", "-j", "3"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push -j 3");
    assert!(
        !output.status.success(),
        "push must fail when any manifest push fails under -j"
    );

    // The two healthy repos still advanced their bares.
    for rp in ["local/org/ok1", "local/org/ok2"] {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let (_, expected_sha) = expected_shas.iter().find(|(p, _)| p == rp).unwrap();
        let bare_sha = bare_main_sha(bare).expect("healthy bare must hold pushed sha");
        assert_eq!(
            &bare_sha, expected_sha,
            "{rp} should still be pushed even though sibling failed under -j"
        );
    }

    // The project bare must NOT have moved — order invariant under parallel.
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        baseline_project,
        "project bare must NOT advance when any manifest push fails (parallel)"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bad"),
        "aggregated error report should name the failing repo; got: {stderr}"
    );
    assert!(
        stderr.contains("project repo not pushed")
            || stderr.contains("manifest-side partial state"),
        "error should surface project-not-pushed / partial state; got: {stderr}"
    );
}

/// Order invariant under parallel: even when manifest pushes run
/// concurrently, the project-repo push happens AFTER all of them. We
/// observe this indirectly: sabotage the project repo's origin so its
/// push fails AFTER manifest pushes succeed; every manifest bare must
/// already hold its new SHA at that point.
#[test]
fn push_dash_j_project_repo_runs_after_manifest_pushes() {
    let repos = [
        ("local/org/a", "owned"),
        ("local/org/b", "owned"),
        ("local/org/c", "owned"),
    ];
    let ws = build_workspace("alpha", &repos);
    let (expected_shas, _) = advance_all_and_relock(&ws, &repos);

    // Break the project repo's origin so its push fails. Manifest pushes
    // must all complete first (proving they ran before the project push).
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    let bad_url = ws.workspace.join("nonexistent-project.git");
    git_run(
        &project_dir,
        &["remote", "set-url", "origin", bad_url.to_str().unwrap()],
    );

    let output = rwv()
        .args(["push", "-j", "3"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push -j 3");
    assert!(
        !output.status.success(),
        "push must fail when project-repo push fails (parallel manifest path)"
    );

    // Every manifest bare advanced — proving the project-repo push was
    // attempted only after the parallel manifest pool joined.
    for (rp, expected_sha) in &expected_shas {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let bare_sha = bare_main_sha(bare).expect("manifest bare must hold pushed sha");
        assert_eq!(
            &bare_sha, expected_sha,
            "{rp} must be pushed before project-repo push is attempted, under -j"
        );
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("project-repo push") || stderr.contains("lock carrier is not"),
        "error should surface project-side failure clearly under -j; got: {stderr}"
    );
}
