//! Integration tests anchoring documented behavior of `rwv push` (fo-r982a).
//!
//! Doc claims pinned here (one #[test] per claim; if a single test exercises
//! multiple claims it lists them all):
//!
//!   - push: Role::Fork repos are skipped at the loop level
//!   - push: manifest repos pushed before the project repo (publish order)
//!   - push: lock precondition refuses if any repo HEAD != recorded lock SHA
//!     (refuse happens before any network call)
//!   - push --dry-run: prints a plan line per manifest repo + the project
//!     repo, never touches a bare
//!   - push --role / push --repo: filter the push loop with union semantics;
//!     `Exact`, `re:`, and `glob:` selectors all flow through
//!   - push -j N: lines per repo carry the `[<repo>]` prefix (Reporter::Parallel)
//!   - push: PushOutcome::{Pushed, Skipped, Failed(...)} surface in the
//!     user-facing output
//!
//! Style note: these tests reproduce the bare-remote-with-seed setup pattern
//! used by `push_test.rs` / `push_parallel_test.rs` rather than `common`'s
//! single-repo helpers — `rwv push` needs both manifest bares and a project
//! bare, plus a committed lock that matches local HEAD, which the
//! doc_claims_activate fixture pattern can't model. The helpers below are
//! deliberately structured the same as the two push test files so a
//! reviewer can verify they exercise the same shape.

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

/// Initialize a bare repo and seed it with one commit on `main`.
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

/// Build a workspace with `repos.len()` manifest repos + a project repo,
/// each backed by its own bare remote. The committed lock matches every
/// manifest repo's local HEAD so the precondition passes by default.
fn build_workspace(project_name: &str, repos: &[(&str, &str)]) -> PushWorkspace {
    let tmp = tempfile::tempdir().unwrap();
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
        let remote_name = if *role == "fork" {
            "upstream"
        } else {
            "origin"
        };
        git_run(
            workspace.parent().unwrap(),
            &[
                "clone",
                "--origin",
                remote_name,
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

/// Advance every manifest repo with one new commit and rewrite the lock
/// to match. Returns the (repo_path, new SHA) pairs.
fn advance_all_and_relock(
    ws: &PushWorkspace,
    repos: &[(&str, &str)],
) -> Vec<(String, String)> {
    let mut manifest_yaml = String::from("repositories:\n");
    let mut lock_yaml = String::from("repositories:\n");
    let mut expected_shas: Vec<(String, String)> = Vec::new();
    for (rp, role) in repos {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let local = ws.workspace.join(rp);
        std::fs::write(local.join(format!("ch_{}.txt", rp.replace('/', "_"))), "x").unwrap();
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
    expected_shas
}

// ===========================================================================
// 1. Role::Fork repos are skipped at the loop level (fo-r982a / fo-nxba7)
//
// Doc claim: even when a selector matches a fork-role repo, `rwv push` does
// not push it — instead it emits an info line referencing the skip. The
// fork bare must not advance.
// ===========================================================================

#[test]
fn push_skips_role_fork_at_loop_level() {
    let repos = [
        ("local/org/fork-repo", "fork"),
        ("local/org/main-repo", "primary"),
    ];
    let ws = build_workspace("alpha", &repos);
    let fork_baseline = bare_main_sha(&ws.manifest_bares[0].1);
    let _expected_shas = advance_all_and_relock(&ws, &repos);

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    assert!(
        output.status.success(),
        "push should succeed even with a fork repo present; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Doc claim: fork repos are surfaced with a "Role::Fork" / "skipping"
    // info line, mapping to PushOutcome::Skipped.
    assert!(
        stdout.contains("skipping") && stdout.contains("fork-repo"),
        "push should announce that the fork repo was skipped; got: {stdout}"
    );

    // Fork bare must not have moved.
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[0].1),
        fork_baseline,
        "fork bare must not be advanced even when manifest selects it"
    );
}

// ===========================================================================
// 2. Project repo pushed last (fo-r982a / fo-nxba7)
//
// Doc claim: the lock-carrying project repo is pushed only after every
// manifest repo's push succeeds. Verified via a failure path: if a
// manifest push fails, the project bare must NOT have moved.
// ===========================================================================

#[test]
fn push_project_repo_pushed_after_manifest_repos() {
    let repos = [("local/org/a", "primary"), ("local/org/b", "primary")];
    let ws = build_workspace("alpha", &repos);
    let baseline_project = bare_main_sha(&ws.project_bare);
    let _expected_shas = advance_all_and_relock(&ws, &repos);

    // Sabotage repo B so its push fails; repo A succeeds.
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

    // Project bare baseline preserved — proves manifest gate fired before
    // the project-repo push step.
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        baseline_project,
        "project bare must NOT advance when a manifest push fails"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("project repo not pushed")
            || stderr.contains("manifest-side partial state"),
        "error should surface project-not-pushed / partial state; got: {stderr}"
    );
}

// ===========================================================================
// 3. Lock-precondition: refuse before any push when HEAD != lock (fo-r982a)
//
// Doc claim: if any manifest repo's HEAD differs from its recorded lock SHA,
// `rwv push` refuses *before* touching the network. No bare advances.
// ===========================================================================

#[test]
fn push_refuses_on_lock_precondition_before_network() {
    let repos = [("local/org/a", "primary")];
    let ws = build_workspace("alpha", &repos);
    let (_, manifest_bare) = &ws.manifest_bares[0];
    let baseline_manifest = bare_main_sha(manifest_bare);
    let baseline_project = bare_main_sha(&ws.project_bare);

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
        "push must refuse when lock and HEAD disagree"
    );

    // Neither bare should have moved.
    assert_eq!(
        bare_main_sha(manifest_bare),
        baseline_manifest,
        "lock-precondition refusal must happen before any push"
    );
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        baseline_project,
        "project bare must not advance on lock-precondition refusal"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("lock") || stderr.contains("rwv lock") || stderr.contains("git checkout"),
        "error should hint at lock / checkout remediation; got: {stderr}"
    );
}

// ===========================================================================
// 4. --dry-run prints plan but pushes nothing (fo-r982a / fo-nxba7)
//
// Doc claim: under `--dry-run`, `rwv push` prints one plan line per filtered
// manifest repo + a trailing project-repo line; the "(dry-run)" preamble is
// present; no bare repo moves.
// ===========================================================================

#[test]
fn push_dry_run_prints_plan_and_does_not_push() {
    let repos = [
        ("local/org/lib", "primary"),
        ("local/org/forklib", "fork"),
    ];
    let ws = build_workspace("alpha", &repos);
    let baseline_primary = bare_main_sha(&ws.manifest_bares[0].1);
    let baseline_fork = bare_main_sha(&ws.manifest_bares[1].1);
    let baseline_project = bare_main_sha(&ws.project_bare);
    let _expected_shas = advance_all_and_relock(&ws, &repos);

    let output = rwv()
        .args(["push", "--dry-run"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push --dry-run");

    assert!(
        output.status.success(),
        "dry-run should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // PushPlanItem rendering: announces dry-run, then one line per repo.
    assert!(
        stdout.contains("dry-run"),
        "dry-run output should announce itself; got: {stdout}"
    );
    assert!(
        stdout.contains("local/org/lib")
            && (stdout.contains("would push") || stdout.contains("origin")),
        "dry-run should describe the would-push for the primary repo; got: {stdout}"
    );
    assert!(
        stdout.contains("local/org/forklib")
            && (stdout.contains("Role::Fork") || stdout.contains("skip")),
        "dry-run should describe the fork skip; got: {stdout}"
    );
    assert!(
        stdout.contains("projects/alpha"),
        "dry-run should include a line for the project repo; got: {stdout}"
    );

    // No bare moved.
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[0].1),
        baseline_primary,
        "dry-run must not touch the primary bare"
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[1].1),
        baseline_fork,
        "dry-run must not touch the fork bare"
    );
    assert_eq!(
        bare_main_sha(&ws.project_bare),
        baseline_project,
        "dry-run must not touch the project bare"
    );
}

// ===========================================================================
// 5. --role / --repo selectors with union semantics (fo-r982a / fo-9kweo)
//
// Doc claim: `--role <r>` and `--repo <selector>` narrow the push loop;
// combined they union (a repo matched by EITHER flag pushes). `--repo`
// accepts bare-string Exact, `re:` regex, and `glob:` glob selectors.
// ===========================================================================

#[test]
fn push_role_filter_advances_only_matching_role() {
    let repos = [("local/org/p", "primary"), ("local/org/d", "dependency")];
    let ws = build_workspace("alpha", &repos);
    let baseline_d = bare_main_sha(&ws.manifest_bares[1].1);
    let expected = advance_all_and_relock(&ws, &repos);

    rwv()
        .args(["push", "--role", "primary"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let (_, p_bare) = &ws.manifest_bares[0];
    assert_eq!(
        bare_main_sha(p_bare),
        Some(expected[0].1.clone()),
        "primary repo should advance under --role primary"
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[1].1),
        baseline_d,
        "dependency repo should NOT advance under --role primary"
    );
}

#[test]
fn push_repo_selector_supports_exact_regex_and_glob() {
    // Exact match
    {
        let repos = [("local/org/a", "primary"), ("local/org/b", "primary")];
        let ws = build_workspace("alpha", &repos);
        let baseline_b = bare_main_sha(&ws.manifest_bares[1].1);
        let expected = advance_all_and_relock(&ws, &repos);

        rwv()
            .args(["push", "--repo", "local/org/a"])
            .current_dir(&ws.workspace)
            .assert()
            .success();

        assert_eq!(
            bare_main_sha(&ws.manifest_bares[0].1),
            Some(expected[0].1.clone()),
            "exact selector should advance the matching repo"
        );
        assert_eq!(
            bare_main_sha(&ws.manifest_bares[1].1),
            baseline_b,
            "non-matching repo must not advance"
        );
    }

    // Regex match
    {
        let repos = [
            ("local/cwalv/a", "primary"),
            ("local/cwalv/b", "primary"),
            ("local/other/c", "primary"),
        ];
        let ws = build_workspace("alpha", &repos);
        let baseline_c = bare_main_sha(&ws.manifest_bares[2].1);
        let expected = advance_all_and_relock(&ws, &repos);

        rwv()
            .args(["push", "--repo", "re:^local/cwalv/"])
            .current_dir(&ws.workspace)
            .assert()
            .success();

        for i in 0..2 {
            assert_eq!(
                bare_main_sha(&ws.manifest_bares[i].1),
                Some(expected[i].1.clone()),
                "regex selector should advance local/cwalv/* repos"
            );
        }
        assert_eq!(
            bare_main_sha(&ws.manifest_bares[2].1),
            baseline_c,
            "regex must not match local/other/c"
        );
    }

    // Glob match
    {
        let repos = [
            ("local/org/a", "primary"),
            ("local/org/b", "primary"),
            ("local/other/c", "primary"),
        ];
        let ws = build_workspace("alpha", &repos);
        let baseline_c = bare_main_sha(&ws.manifest_bares[2].1);
        let expected = advance_all_and_relock(&ws, &repos);

        rwv()
            .args(["push", "--repo", "glob:local/org/*"])
            .current_dir(&ws.workspace)
            .assert()
            .success();

        for i in 0..2 {
            assert_eq!(
                bare_main_sha(&ws.manifest_bares[i].1),
                Some(expected[i].1.clone()),
                "glob selector should advance local/org/* repos"
            );
        }
        assert_eq!(
            bare_main_sha(&ws.manifest_bares[2].1),
            baseline_c,
            "glob must not match local/other/c"
        );
    }
}

#[test]
fn push_role_and_repo_filters_union() {
    let repos = [
        ("local/me/p", "primary"),
        ("local/external/dep", "dependency"),
        ("local/external/other", "dependency"),
    ];
    let ws = build_workspace("alpha", &repos);
    let baseline_other = bare_main_sha(&ws.manifest_bares[2].1);
    let expected = advance_all_and_relock(&ws, &repos);

    rwv()
        .args(["push", "--role", "primary", "--repo", "local/external/dep"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    // Primary advances via --role; external/dep via --repo. Other dep
    // matches neither flag and must stay put.
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[0].1),
        Some(expected[0].1.clone()),
        "primary should advance via --role"
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[1].1),
        Some(expected[1].1.clone()),
        "exact-named dep should advance via --repo"
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[2].1),
        baseline_other,
        "unmatched dep must NOT advance — proves union, not cartesian"
    );
}

// ===========================================================================
// 6. -j N parallel mode emits [<repo>] prefix; -j 1 does not (fo-r982a / fo-ysnuz)
//
// Doc claim: under `-j > 1` per-repo lines carry the `[<repo-path>]` prefix
// (Reporter::Parallel); under `-j 1` the prefix is absent (Reporter::Serial).
// ===========================================================================

#[test]
fn push_dash_j_parallel_emits_repo_prefix() {
    let repos = [("local/org/a", "primary"), ("local/org/b", "primary")];
    let ws = build_workspace("alpha", &repos);
    let _expected = advance_all_and_relock(&ws, &repos);

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
        "parallel push must wrap per-repo lines with `[<repo>]`; got:\n{stdout}"
    );
}

#[test]
fn push_dash_j_one_emits_no_repo_prefix() {
    let repos = [("local/org/a", "primary"), ("local/org/b", "primary")];
    let ws = build_workspace("alpha", &repos);
    let _expected = advance_all_and_relock(&ws, &repos);

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
        "serial push must NOT wrap per-repo lines with `[<repo>]`; got:\n{stdout}"
    );
    // Sanity: the unwrapped per-repo lines (Reporter::Serial passthrough)
    // are still present so the user still sees progress.
    assert!(
        stdout.contains("rwv push: pushing local/org/a")
            && stdout.contains("rwv push: pushing local/org/b"),
        "serial push should still emit per-repo `pushing` lines; got:\n{stdout}"
    );
}

// ===========================================================================
// 7. PushOutcome variants surface in output (fo-r982a)
//
// Doc claim: the three PushOutcome variants reach the user-visible output.
//   - Pushed   -> "rwv push: pushing <path>" pre-message
//   - Skipped  -> "rwv push: skipping <path> (Role::Fork ...)"
//   - Failed   -> aggregated error summary "rwv push: N repo(s) failed:"
// ===========================================================================

#[test]
fn push_outcome_variants_show_in_output() {
    let repos = [
        ("local/org/ok", "primary"),
        ("local/org/forked", "fork"),
        ("local/org/broken", "primary"),
    ];
    let ws = build_workspace("alpha", &repos);
    let _expected = advance_all_and_relock(&ws, &repos);

    // Sabotage the third repo's origin to force a Failed outcome.
    let local_broken = ws.workspace.join("local/org/broken");
    let bad_url = ws.workspace.join("nope.git");
    git_run(
        &local_broken,
        &["remote", "set-url", "origin", bad_url.to_str().unwrap()],
    );

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Pushed variant: the healthy primary surfaces its pre-message.
    assert!(
        stdout.contains("rwv push: pushing local/org/ok"),
        "Pushed outcome must surface a 'pushing <path>' line; got:\n{stdout}"
    );
    // Skipped variant: fork repo surfaces a skipping line.
    assert!(
        stdout.contains("skipping local/org/forked"),
        "Skipped outcome must surface a 'skipping <path>' line; got:\n{stdout}"
    );
    // Failed variant: per-repo summary + non-zero exit.
    assert!(
        !output.status.success(),
        "push with a broken repo must exit non-zero (Failed outcome)"
    );
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("local/org/broken"),
        "Failed outcome must name the failing repo in the aggregated error; \
         got stdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
