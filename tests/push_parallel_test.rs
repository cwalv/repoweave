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
use std::path::Path;

mod common;

fn rwv() -> Command {
    common::rwv()
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
    ws: &common::PushWorkspace,
    repos: &[(&str, &str)],
) -> (Vec<(String, String)>, String) {
    let mut manifest_yaml = String::from("[repositories]\n");
    let mut lock_entries = Vec::new();
    let mut expected_shas: Vec<(String, String)> = Vec::new();
    for (rp, role) in repos {
        let (_, bare) = ws.manifest_bares.iter().find(|(p, _)| p == rp).unwrap();
        let local = ws.workspace.join(rp);
        std::fs::write(
            local.join(format!("changed_{}.txt", rp.replace('/', "_"))),
            "new",
        )
        .unwrap();
        common::git_in(&local, &["add", "."]);
        common::git_in(&local, &["commit", "-m", "advance"]);
        let sha = common::git_in(&local, &["rev-parse", "HEAD"]);
        let bare_url = common::file_url(bare);
        manifest_yaml.push_str(&format!(
            "[repositories.\"{rp}\"]\ntype = \"git\"\nurl = \"{bare_url}\"\nversion = \"main\"\nrole = \"{role}\"\n"
        ));
        lock_entries.push(format!(
            "{rp:?}: {{\"type\": \"git\", \"url\": {bare_url:?}, \"version\": {sha:?}}}"
        ));
        expected_shas.push(((*rp).to_string(), sha));
    }
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    std::fs::write(project_dir.join("rwv.toml"), &manifest_yaml).unwrap();
    // Round-trips through the real parser + `lock::write_lock` (see
    // `build_workspace` above for why).
    let raw_lock = format!("{{\"repositories\": {{{}}}}}", lock_entries.join(","));
    let lock = repoweave::manifest::LockFile::from_json_str(&raw_lock).unwrap();
    repoweave::lock::write_lock(&lock, &project_dir.join("rwv.lock")).unwrap();
    common::git_in(&project_dir, &["add", "."]);
    common::git_in(&project_dir, &["commit", "-m", "advance lock"]);
    let project_head = common::git_in(&project_dir, &["rev-parse", "HEAD"]);
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
    let ws = common::build_workspace("alpha", &repos);
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
    let ws = common::build_workspace("alpha", &repos);
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
    let ws = common::build_workspace("alpha", &repos);
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
    let ws = common::build_workspace("alpha", &repos);
    let baseline_project = bare_main_sha(&ws.project_bare);
    let (expected_shas, _) = advance_all_and_relock(&ws, &repos);

    // Sabotage one repo's remote so its push fails. The other two should
    // still complete; project repo must NOT be pushed.
    let local_bad = ws.workspace.join("local/org/bad");
    let bad_url = ws.workspace.join("nonexistent-remote.git");
    common::git_in(
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
    let ws = common::build_workspace("alpha", &repos);
    let (expected_shas, _) = advance_all_and_relock(&ws, &repos);

    // Break the project repo's origin so its push fails. Manifest pushes
    // must all complete first (proving they ran before the project push).
    let project_dir = ws.workspace.join("projects").join(&ws.project_name);
    let bad_url = ws.workspace.join("nonexistent-project.git");
    common::git_in(
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
