//! Integration tests anchoring documented behavior of `rwv push`.
//!
//! Doc claims pinned here (one #[test] per claim; if a single test exercises
//! multiple claims it lists them all):
//!
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
//!   - push --json: emits envelope with $schema and outcomes array
//!   - push --json: project-repo record is the last outcome and uses kind
//!     `project-repo-pushed` (distinguishable from manifest-repo records)
//!   - push --json -j N (N > 1): emits NDJSON (one record per line)
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

/// Initialize a bare repo and seed it with one commit on `main`.
fn init_bare_repo_with_commit(bare: &Path) {
    let parent = bare.parent().expect("bare repo path needs a parent");
    let stem = bare.file_stem().unwrap().to_string_lossy().into_owned();
    common::git_in(
        parent,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    let seed = parent.join(format!("__seed_{stem}"));
    common::git_in(
        parent,
        &["clone", bare.to_str().unwrap(), seed.to_str().unwrap()],
    );
    common::git_in(&seed, &["config", "user.email", "test@test.com"]);
    common::git_in(&seed, &["config", "user.name", "Test"]);
    std::fs::write(seed.join("README"), "seed").unwrap();
    common::git_in(&seed, &["add", "."]);
    common::git_in(&seed, &["commit", "-m", "initial"]);
    common::git_in(&seed, &["push", "origin", "main"]);
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
    let tmp = common::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(workspace.join("projects")).unwrap();

    let mut manifest_bares: Vec<(String, PathBuf)> = Vec::new();
    let mut manifest_shas: Vec<(String, String)> = Vec::new();
    let mut manifest_yaml = String::from("[repositories]\n");
    for (repo_path, role) in repos {
        let bare = tmp
            .path()
            .join(format!("{}.git", repo_path.replace('/', "_")));
        init_bare_repo_with_commit(&bare);

        let canonical = workspace.join(repo_path);
        std::fs::create_dir_all(canonical.parent().unwrap()).unwrap();
        common::git_in(
            workspace.parent().unwrap(),
            &[
                "clone",
                "--origin",
                "origin",
                bare.to_str().unwrap(),
                canonical.to_str().unwrap(),
            ],
        );
        common::git_in(&canonical, &["config", "user.email", "test@test.com"]);
        common::git_in(&canonical, &["config", "user.name", "Test"]);
        let head = common::git_in(&canonical, &["rev-parse", "HEAD"]);
        manifest_shas.push(((*repo_path).to_string(), head));
        manifest_bares.push(((*repo_path).to_string(), bare.clone()));
        let bare_url = common::file_url(&bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{repo_path}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
        ));
    }

    let project_bare = tmp.path().join("project.git");
    init_bare_repo_with_commit(&project_bare);
    let project_dir = workspace.join("projects").join(project_name);
    common::git_in(
        workspace.parent().unwrap(),
        &[
            "clone",
            project_bare.to_str().unwrap(),
            project_dir.to_str().unwrap(),
        ],
    );
    common::git_in(&project_dir, &["config", "user.email", "test@test.com"]);
    common::git_in(&project_dir, &["config", "user.name", "Test"]);

    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();

    let mut lock_entries: Vec<(String, String, String)> = Vec::new();
    for (rp, sha) in &manifest_shas {
        let (_, bare) = manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let bare_url = common::file_url(bare);
        lock_entries.push((rp.clone(), bare_url, sha.clone()));
    }
    let entries: Vec<(&str, &str, &str)> = lock_entries
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &entries);

    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "manifest + lock"]);

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
fn advance_all_and_relock(ws: &PushWorkspace, repos: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut manifest_yaml = String::from("[repositories]\n");
    let mut lock_entries: Vec<(String, String, String)> = Vec::new();
    let mut expected_shas: Vec<(String, String)> = Vec::new();
    for (rp, role) in repos {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let local = ws.workspace.join(rp);
        std::fs::write(local.join(format!("ch_{}.txt", rp.replace('/', "_"))), "x").unwrap();
        common::git_in(&local, &["add", "."]);
        common::git_in(&local, &["commit", "-m", "advance"]);
        let sha = common::git_in(&local, &["rev-parse", "HEAD"]);
        let bare_url = common::file_url(bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{rp}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
        ));
        lock_entries.push(((*rp).to_string(), bare_url, sha.clone()));
        expected_shas.push(((*rp).to_string(), sha));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();
    let entries: Vec<(&str, &str, &str)> = lock_entries
        .iter()
        .map(|(p, u, s)| (p.as_str(), u.as_str(), s.as_str()))
        .collect();
    common::fixture_lock(&project_dir, &entries);
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "advance lock"]);
    expected_shas
}

// ===========================================================================
// 1. Fork repos push like Owned repos (no longer skipped at loop level)
//
// B2 will add plan-time selector tests; this placeholder keeps section
// numbering stable for future reference.
// ===========================================================================

// ===========================================================================
// 2. Project repo pushed last
//
// Doc claim: the lock-carrying project repo is pushed only after every
// manifest repo's push succeeds. Verified via a failure path: if a
// manifest push fails, the project bare must NOT have moved.
// ===========================================================================

#[test]
fn push_project_repo_pushed_after_manifest_repos() {
    let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
    let ws = build_workspace("alpha", &repos);
    let baseline_project = bare_main_sha(&ws.project_bare);
    let _expected_shas = advance_all_and_relock(&ws, &repos);

    // Sabotage repo B so its push fails; repo A succeeds.
    let local_b = ws.workspace.join("local/org/b");
    let bad_url = ws.workspace.join("nonexistent-remote.git");
    common::git_in(
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
// 3. Lock-precondition: refuse before any push when HEAD != lock
//
// Doc claim: if any manifest repo's HEAD differs from its recorded lock SHA,
// `rwv push` refuses *before* touching the network. No bare advances.
// ===========================================================================

#[test]
fn push_refuses_on_lock_precondition_before_network() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("alpha", &repos);
    let (_, manifest_bare) = &ws.manifest_bares[0];
    let baseline_manifest = bare_main_sha(manifest_bare);
    let baseline_project = bare_main_sha(&ws.project_bare);

    // Advance the local repo WITHOUT updating the lock.
    let local = ws.workspace.join("local/org/a");
    std::fs::write(local.join("drift.txt"), "drift").unwrap();
    common::git_in(&local, &["add", "."]);
    common::git_in(&local, &["commit", "-m", "drift past lock"]);

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
// 4. --dry-run prints plan but pushes nothing
//
// Doc claim: under `--dry-run`, `rwv push` prints one plan line per filtered
// manifest repo + a trailing project-repo line; the "(dry-run)" preamble is
// present; no bare repo moves.
// ===========================================================================

#[test]
fn push_dry_run_prints_plan_and_does_not_push() {
    let repos = [("local/org/lib", "owned"), ("local/org/forklib", "fork")];
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
    // Read the claim off the line that makes it. Asserting the tokens
    // against the whole of stdout lets any line answer for any other, and
    // an `||` between them lets the always-present half carry the pair —
    // which is how the remote name went unread here for as long as it did.
    // Fork is checked on the same terms as Owned, which is the claim about
    // fork this test carries.
    for repo in ["local/org/lib", "local/org/forklib"] {
        let line = stdout
            .lines()
            .find(|l| l.contains(repo))
            .unwrap_or_else(|| panic!("dry-run printed no plan line for {repo}; got: {stdout}"));
        assert!(
            line.contains("would push") && line.trim_end().ends_with("-> origin"),
            "the plan line for {repo} should say what it would push and to which remote; \
             got: {line}"
        );
    }
    let project_line = stdout
        .lines()
        .find(|l| l.contains("projects/alpha"))
        .unwrap_or_else(|| panic!("dry-run printed no project-repo line; got: {stdout}"));
    assert!(
        project_line.contains("would push")
            && project_line.trim_end().ends_with("-> origin (last)"),
        "the project-repo line should name the remote and that it goes last; got: {project_line}"
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
// 5. --role / --repo selectors with union semantics
//
// Doc claim: `--role <r>` and `--repo <selector>` narrow the push loop;
// combined they union (a repo matched by EITHER flag pushes). `--repo`
// accepts bare-string Exact, `re:` regex, and `glob:` glob selectors.
// ===========================================================================

#[test]
fn push_role_filter_advances_only_matching_role() {
    let repos = [("local/org/p", "owned"), ("local/org/d", "dependency")];
    let ws = build_workspace("alpha", &repos);
    let baseline_d = bare_main_sha(&ws.manifest_bares[1].1);
    let expected = advance_all_and_relock(&ws, &repos);

    rwv()
        .args(["push", "--role", "owned"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    let (_, p_bare) = &ws.manifest_bares[0];
    assert_eq!(
        bare_main_sha(p_bare),
        Some(expected[0].1.clone()),
        "owned repo should advance under --role owned"
    );
    assert_eq!(
        bare_main_sha(&ws.manifest_bares[1].1),
        baseline_d,
        "dependency repo should NOT advance under --role owned"
    );
}

#[test]
fn push_repo_selector_supports_exact_regex_and_glob() {
    // Exact match
    {
        let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
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
            ("local/cwalv/a", "owned"),
            ("local/cwalv/b", "owned"),
            ("local/other/c", "owned"),
        ];
        let ws = build_workspace("alpha", &repos);
        let baseline_c = bare_main_sha(&ws.manifest_bares[2].1);
        let expected = advance_all_and_relock(&ws, &repos);

        rwv()
            .args(["push", "--repo", "re:^local/cwalv/"])
            .current_dir(&ws.workspace)
            .assert()
            .success();

        for (i, item) in expected.iter().enumerate().take(2) {
            assert_eq!(
                bare_main_sha(&ws.manifest_bares[i].1),
                Some(item.1.clone()),
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
            ("local/org/a", "owned"),
            ("local/org/b", "owned"),
            ("local/other/c", "owned"),
        ];
        let ws = build_workspace("alpha", &repos);
        let baseline_c = bare_main_sha(&ws.manifest_bares[2].1);
        let expected = advance_all_and_relock(&ws, &repos);

        rwv()
            .args(["push", "--repo", "glob:local/org/*"])
            .current_dir(&ws.workspace)
            .assert()
            .success();

        for (i, item) in expected.iter().enumerate().take(2) {
            assert_eq!(
                bare_main_sha(&ws.manifest_bares[i].1),
                Some(item.1.clone()),
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
        ("local/me/p", "owned"),
        ("local/external/dep", "dependency"),
        ("local/external/other", "dependency"),
    ];
    let ws = build_workspace("alpha", &repos);
    let baseline_other = bare_main_sha(&ws.manifest_bares[2].1);
    let expected = advance_all_and_relock(&ws, &repos);

    rwv()
        .args(["push", "--role", "owned", "--repo", "local/external/dep"])
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
// 6. -j N parallel mode emits [<repo>] prefix; -j 1 does not
//
// Doc claim: under `-j > 1` per-repo lines carry the `[<repo-path>]` prefix
// (Reporter::Parallel); under `-j 1` the prefix is absent (Reporter::Serial).
// ===========================================================================

#[test]
fn push_dash_j_parallel_emits_repo_prefix() {
    let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
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
    let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
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
// 7. PushOutcome variants surface in output
//
// Doc claim: the two PushOutcome variants in normal operation reach the
// user-visible output.
//   - Pushed   -> "rwv push: pushing <path>" pre-message
//   - Failed   -> aggregated error summary "rwv push: N repo(s) failed:"
// Fork is now treated identically to Owned (pushes to origin).
// ===========================================================================

#[test]
fn push_outcome_variants_show_in_output() {
    let repos = [
        ("local/org/ok", "owned"),
        ("local/org/forked", "fork"),
        ("local/org/broken", "owned"),
    ];
    let ws = build_workspace("alpha", &repos);
    let _expected = advance_all_and_relock(&ws, &repos);

    // Sabotage the third repo's origin to force a Failed outcome.
    let local_broken = ws.workspace.join("local/org/broken");
    let bad_url = ws.workspace.join("nope.git");
    common::git_in(
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

    // Pushed variant: the healthy owned repo surfaces its pre-message.
    assert!(
        stdout.contains("rwv push: pushing local/org/ok"),
        "Pushed outcome must surface a 'pushing <path>' line; got:\n{stdout}"
    );
    // Fork is also pushed (same as Owned).
    assert!(
        stdout.contains("rwv push: pushing local/org/forked"),
        "Fork repo must also surface a 'pushing <path>' line; got:\n{stdout}"
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

// ===========================================================================
// 8. --json: envelope with $schema + outcomes; project-repo distinguishable
//
// Doc claim: `rwv push --json` emits a JSON envelope with:
//   - top-level `$schema` URL
//   - `outcomes` array with per-repo records
//   - project-repo record is the last entry with kind `project-repo-pushed`
//     (distinguishable from manifest-repo records by the `kind` field)
// ===========================================================================

#[test]
fn push_json_emits_schema_and_outcomes() {
    let repos = [("local/org/a", "owned")];
    let ws = build_workspace("alpha", &repos);
    let _expected = advance_all_and_relock(&ws, &repos);

    let output = rwv()
        .args(["push", "--json"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push --json");
    assert!(
        output.status.success(),
        "push --json should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not parseable as JSON ({e}):\n{stdout}"));

    assert!(
        parsed.get("$schema").is_some(),
        "envelope must have $schema: {stdout}"
    );
    assert!(
        parsed.get("outcomes").and_then(|v| v.as_array()).is_some(),
        "envelope must have outcomes array: {stdout}"
    );
}

#[test]
fn push_json_project_repo_record_is_last_and_distinguishable() {
    let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
    let ws = build_workspace("alpha", &repos);
    let _expected = advance_all_and_relock(&ws, &repos);

    let output = rwv()
        .args(["push", "--json"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push --json");
    assert!(output.status.success(), "push --json should succeed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("parseable");
    let outcomes = parsed["outcomes"].as_array().expect("outcomes array");

    // Last record must be the project-repo record.
    let last = outcomes.last().expect("non-empty outcomes");
    let last_kind = last["kind"].as_str().expect("kind field");
    assert!(
        last_kind.starts_with("project-repo-"),
        "last outcome must be a project-repo variant; got kind={last_kind}"
    );

    // All preceding records must be manifest-repo records (no project-repo- prefix).
    for o in &outcomes[..outcomes.len() - 1] {
        let kind = o["kind"].as_str().expect("kind field");
        assert!(
            !kind.starts_with("project-repo-"),
            "manifest-repo records must NOT use project-repo- kind; got kind={kind}"
        );
    }
}

// ===========================================================================
// 9. --json -j N: NDJSON streaming under parallel mode
//
// Doc claim: under `--json -j N` with `N > 1`, output switches to NDJSON.
// Each line is a self-describing JSON record. No envelope wrapper.
// ===========================================================================

#[test]
fn push_json_ndjson_under_parallel_mode() {
    let repos = [("local/org/a", "owned"), ("local/org/b", "owned")];
    let ws = build_workspace("alpha", &repos);
    let _expected = advance_all_and_relock(&ws, &repos);

    let output = rwv()
        .args(["push", "--json", "-j", "2"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push --json -j 2");
    assert!(
        output.status.success(),
        "push --json -j 2 should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // NDJSON: must not parse as one big JSON document.
    assert!(
        serde_json::from_str::<serde_json::Value>(&stdout).is_err(),
        "NDJSON stdout must not parse as one envelope: {stdout}"
    );

    // Each non-empty line must be a valid JSON object.
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("NDJSON line not JSON ({e}): {line}\n{stdout}"));
        assert!(v.is_object(), "NDJSON line must be object: {line}");
        assert!(
            v.get("kind").is_some(),
            "NDJSON record missing kind: {line}"
        );
    }
}

// ===========================================================================
// 10. Default plan = Owned + Fork; Dependency + Reference skipped unless
//     selectors override.
//
// Doc claims:
//   - bare `rwv push` pushes Owned + Fork; Dependency + Reference skipped
//   - `rwv push --role dependency` overrides the default and pushes deps
//   - `rwv push --repo <dep-path>` overrides and pushes just that dep
// ===========================================================================

/// Regression test: bare `rwv push` (no selectors) invokes `git push` for
/// Owned + Fork repos and the project repo, but NOT for Dependency or Reference.
/// This is the plan-time default — non-writable roles are excluded before the
/// push loop, not skipped inside it.
#[test]
fn push_default_plan_skips_dependency_and_reference() {
    let repos = [
        ("local/org/owned", "owned"),
        ("local/org/forked", "fork"),
        ("local/org/dep", "dependency"),
        ("local/org/ref", "reference"),
    ];
    let ws = build_workspace("alpha", &repos);

    // Capture baselines for the non-writable repos before advancing anything.
    let (_, dep_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/dep")
        .unwrap();
    let (_, ref_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/ref")
        .unwrap();
    let dep_baseline = bare_main_sha(dep_bare);
    let ref_baseline = bare_main_sha(ref_bare);

    let expected = advance_all_and_relock(&ws, &repos);

    let output = rwv()
        .args(["push"])
        .current_dir(&ws.workspace)
        .output()
        .expect("rwv push");

    assert!(
        output.status.success(),
        "bare rwv push should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Owned + Fork repos must advance.
    let (_, owned_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/owned")
        .unwrap();
    let (_, fork_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/forked")
        .unwrap();
    let (_, owned_sha) = expected
        .iter()
        .find(|(p, _)| p == "local/org/owned")
        .unwrap();
    let (_, fork_sha) = expected
        .iter()
        .find(|(p, _)| p == "local/org/forked")
        .unwrap();

    assert_eq!(
        bare_main_sha(owned_bare),
        Some(owned_sha.clone()),
        "owned repo must be pushed by bare rwv push"
    );
    assert_eq!(
        bare_main_sha(fork_bare),
        Some(fork_sha.clone()),
        "fork repo must be pushed by bare rwv push"
    );

    // Dependency + Reference repos must NOT advance.
    assert_eq!(
        bare_main_sha(dep_bare),
        dep_baseline,
        "dependency bare must NOT advance under bare rwv push (default skips non-writable roles)"
    );
    assert_eq!(
        bare_main_sha(ref_bare),
        ref_baseline,
        "reference bare must NOT advance under bare rwv push (default skips non-writable roles)"
    );

    // Stdout should mention the skipped repos.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("local/org/dep") && stdout.contains("skipped"),
        "dependency skip notice must appear in output; got:\n{stdout}"
    );
    assert!(
        stdout.contains("local/org/ref") && stdout.contains("skipped"),
        "reference skip notice must appear in output; got:\n{stdout}"
    );
}

/// `--role dependency` overrides the default and pushes all dependency repos.
/// This asserts the "selectors override" contract.
#[test]
fn push_role_dependency_overrides_default_and_pushes_deps() {
    let repos = [
        ("local/org/owned", "owned"),
        ("local/org/dep1", "dependency"),
        ("local/org/dep2", "dependency"),
    ];
    let ws = build_workspace("alpha", &repos);

    // Capture owned baseline — it must NOT advance (not in selector).
    let (_, owned_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/owned")
        .unwrap();
    let owned_baseline = bare_main_sha(owned_bare);

    let expected = advance_all_and_relock(&ws, &repos);

    rwv()
        .args(["push", "--role", "dependency"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    // Dependency repos advance.
    let (_, dep1_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/dep1")
        .unwrap();
    let (_, dep2_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/dep2")
        .unwrap();
    let (_, dep1_sha) = expected
        .iter()
        .find(|(p, _)| p == "local/org/dep1")
        .unwrap();
    let (_, dep2_sha) = expected
        .iter()
        .find(|(p, _)| p == "local/org/dep2")
        .unwrap();

    assert_eq!(
        bare_main_sha(dep1_bare),
        Some(dep1_sha.clone()),
        "dep1 should advance under --role dependency"
    );
    assert_eq!(
        bare_main_sha(dep2_bare),
        Some(dep2_sha.clone()),
        "dep2 should advance under --role dependency"
    );

    // Owned repo must NOT advance (not matched by --role dependency).
    assert_eq!(
        bare_main_sha(owned_bare),
        owned_baseline,
        "owned repo must NOT advance when --role dependency overrides default"
    );
}

/// `--repo <dep-path>` overrides the default and pushes just the named
/// dependency. This asserts the exact-path selector override contract.
#[test]
fn push_repo_selector_overrides_default_and_pushes_named_dep() {
    let repos = [
        ("local/org/owned", "owned"),
        ("local/org/dep", "dependency"),
        ("local/org/other-dep", "dependency"),
    ];
    let ws = build_workspace("alpha", &repos);

    // Baselines: owned and other-dep must NOT advance.
    let (_, owned_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/owned")
        .unwrap();
    let (_, other_dep_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/other-dep")
        .unwrap();
    let owned_baseline = bare_main_sha(owned_bare);
    let other_dep_baseline = bare_main_sha(other_dep_bare);

    let expected = advance_all_and_relock(&ws, &repos);

    rwv()
        .args(["push", "--repo", "local/org/dep"])
        .current_dir(&ws.workspace)
        .assert()
        .success();

    // The named dep advances.
    let (_, dep_bare) = ws
        .manifest_bares
        .iter()
        .find(|(p, _)| p == "local/org/dep")
        .unwrap();
    let (_, dep_sha) = expected.iter().find(|(p, _)| p == "local/org/dep").unwrap();
    assert_eq!(
        bare_main_sha(dep_bare),
        Some(dep_sha.clone()),
        "named dep should advance under --repo local/org/dep"
    );

    // Non-matched repos must NOT advance.
    assert_eq!(
        bare_main_sha(owned_bare),
        owned_baseline,
        "owned repo must NOT advance under --repo local/org/dep"
    );
    assert_eq!(
        bare_main_sha(other_dep_bare),
        other_dep_baseline,
        "other-dep must NOT advance under --repo local/org/dep (exact selector)"
    );
}
