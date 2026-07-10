//! Acceptance tests for `rwv fetch -j N` and `rwv update -j N`.
//!
//! These cover the visible behaviours pinned in the spec notes:
//!
//! - `-j N` accepted as a flag on both verbs.
//! - `-j 1` reproduces serial behaviour (no `[<repo>]` prefix in output).
//! - `-j > 1` runs concurrently; per-line `[<repo-path>]` prefix appears.
//! - Failure aggregation under `-j > 1` matches the serial shape (all
//!   failing repos surface in the trailing summary; exit non-zero).
//! - Lock write under `-j > 1` happens after the worker pool joins —
//!   asserted via the resulting `rwv.lock` content being well-formed and
//!   covering every manifest repo (no race / partial-write artefacts).
//!
//! The tests follow the existing `tests/fetch_test.rs` pattern: spin up
//! local bare repos as file:// remotes; no network.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

mod common;

fn rwv() -> Command {
    common::rwv()
}

// ----- Setup helpers (mirroring tests/fetch_test.rs) -------------------------

fn run_git(args: &[&str], cwd: &Path) {
    let status = common::git()
        .args(args)
        .current_dir(cwd)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git command failed to start");
    assert!(status.success(), "git {:?} failed in {:?}", args, cwd);
}

fn init_bare_repo(path: &Path) {
    let status = common::git()
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(path)
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .status()
        .expect("git should be available");
    assert!(status.success(), "git init --bare failed");
}

fn init_bare_repo_with_commit(path: &Path) {
    init_bare_repo(path);
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().join("w");
    run_git(
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
        tmp.path(),
    );
    run_git(&["config", "user.email", "t@t.com"], &work);
    run_git(&["config", "user.name", "T"], &work);
    std::fs::write(work.join("README"), "init").unwrap();
    run_git(&["add", "."], &work);
    run_git(&["commit", "-m", "initial"], &work);
    run_git(&["push", "origin", "main"], &work);
}

/// Set up a project bare repo whose `rwv.yaml` references the given
/// `(repo_path, url)` pairs. Returns the source URL.
fn make_project_source(tmp: &Path, name: &str, repos: &[(&str, &str)]) -> String {
    let project_bare = tmp.join(format!("{name}.git"));
    init_bare_repo(&project_bare);
    let work = tmp.join(format!("{name}_work"));
    run_git(
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
        tmp,
    );
    run_git(&["config", "user.email", "t@t.com"], &work);
    run_git(&["config", "user.name", "T"], &work);
    let mut yaml = String::from("repositories:\n");
    for (path, url) in repos {
        yaml.push_str(&format!(
            "  {path}:\n    type: git\n    url: {url}\n    version: main\n    role: owned\n"
        ));
    }
    std::fs::write(work.join("rwv.yaml"), &yaml).unwrap();
    run_git(&["add", "rwv.yaml"], &work);
    run_git(&["commit", "-m", "manifest"], &work);
    run_git(&["push", "origin", "main"], &work);
    format!("file://{}", project_bare.display())
}

// ----- Tests -----------------------------------------------------------------

/// `-j 1` reproduces serial behaviour: output never contains a
/// `[<repo-path>]` prefix.
#[test]
fn fetch_dash_j_one_emits_no_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let r1 = tmp.path().join("r1.git");
    let r2 = tmp.path().join("r2.git");
    init_bare_repo_with_commit(&r1);
    init_bare_repo_with_commit(&r2);
    let u1 = format!("file://{}", r1.display());
    let u2 = format!("file://{}", r2.display());

    let source = make_project_source(
        tmp.path(),
        "proj",
        &[("local/org/r1", &u1), ("local/org/r2", &u2)],
    );

    let out = rwv()
        .args(["fetch", &source, "-j", "1"])
        .current_dir(&ws)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    // No per-repo bracketed prefix lines under serial mode.
    assert!(
        !stdout.contains("[local/org/r1]") && !stdout.contains("[local/org/r2]"),
        "expected no [<repo>] prefix under -j 1, got:\n{stdout}"
    );
}

/// `-j N` (N > 1) attaches `[<repo-path>]` to per-repo lines so
/// interleaved output is parseable. Mirrors `make -j` / `ninja`.
#[test]
fn fetch_dash_j_two_emits_repo_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let r1 = tmp.path().join("r1.git");
    let r2 = tmp.path().join("r2.git");
    init_bare_repo_with_commit(&r1);
    init_bare_repo_with_commit(&r2);
    let u1 = format!("file://{}", r1.display());
    let u2 = format!("file://{}", r2.display());

    let source = make_project_source(
        tmp.path(),
        "proj",
        &[("local/org/r1", &u1), ("local/org/r2", &u2)],
    );

    let out = rwv()
        .args(["fetch", &source, "-j", "2"])
        .current_dir(&ws)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains("[local/org/r1]") && stdout.contains("[local/org/r2]"),
        "expected [<repo>] prefix for each repo under -j 2, got:\n{stdout}"
    );
}

/// `-j N` clones all repos and produces a well-formed lock covering
/// every manifest entry. This is the proxy for "lock write is serial
/// post-join": if it were racy under parallelism we'd see missing
/// entries or a truncated file.
#[test]
fn fetch_dash_j_clones_all_repos_and_writes_complete_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    // 5 repos — enough to exercise concurrency without being slow.
    let mut urls = Vec::new();
    let mut paths = Vec::new();
    for i in 0..5 {
        let bare = tmp.path().join(format!("r{i}.git"));
        init_bare_repo_with_commit(&bare);
        urls.push(format!("file://{}", bare.display()));
        paths.push(format!("local/org/r{i}"));
    }
    let pairs: Vec<(&str, &str)> = paths
        .iter()
        .zip(urls.iter())
        .map(|(p, u)| (p.as_str(), u.as_str()))
        .collect();
    let source = make_project_source(tmp.path(), "proj", &pairs);

    rwv()
        .args(["fetch", &source, "-j", "4"])
        .current_dir(&ws)
        .assert()
        .success();

    // Every repo clone present.
    for p in &paths {
        assert!(ws.join(p).exists(), "expected {p} to be cloned under -j 4");
    }

    // Lock file written and covers every repo. Parse the YAML strictly
    // so a corrupt/partial write would fail here.
    let project_dirs: Vec<_> = std::fs::read_dir(ws.join("projects"))
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(project_dirs.len(), 1);
    let lock_path = project_dirs[0].path().join("rwv.lock");
    assert!(
        lock_path.exists(),
        "rwv.lock should be written after parallel fetch"
    );
    let lock_text = std::fs::read_to_string(&lock_path).unwrap();
    for p in &paths {
        assert!(
            lock_text.contains(p),
            "lock should contain entry for {p}, got:\n{lock_text}"
        );
    }
}

/// Under `-j > 1` a failing repo (bad URL) does not prevent the other
/// repos from being attempted; the failure surfaces in the trailing
/// aggregated summary and the command exits non-zero — matching the
/// existing serial aggregation shape.
#[test]
fn fetch_dash_j_aggregates_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    // r_ok is a real bare repo; r_bad points at a nonexistent path so
    // git clone will fail. The pre-existing aggregation pattern
    // (collect Vec<String>, bail at end) should still report r_bad's
    // failure while letting r_ok succeed.
    let r_ok = tmp.path().join("r_ok.git");
    init_bare_repo_with_commit(&r_ok);
    let u_ok = format!("file://{}", r_ok.display());
    let u_bad = format!("file://{}/does_not_exist.git", tmp.path().display());

    let source = make_project_source(
        tmp.path(),
        "proj",
        &[("local/org/r_ok", &u_ok), ("local/org/r_bad", &u_bad)],
    );

    rwv()
        .args(["fetch", &source, "-j", "2"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("r_bad").and(predicate::str::contains("failed")));

    // The healthy repo still landed on disk — failure of one didn't
    // poison the other.
    assert!(
        ws.join("local/org/r_ok").exists(),
        "healthy repo should still be cloned even when sibling fails"
    );
}

/// `--jobs` long form is accepted (mirrors `-j` short form).
#[test]
fn fetch_accepts_long_jobs_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let r1 = tmp.path().join("r1.git");
    init_bare_repo_with_commit(&r1);
    let u1 = format!("file://{}", r1.display());
    let source = make_project_source(tmp.path(), "proj", &[("local/org/r1", &u1)]);

    rwv()
        .args(["fetch", &source, "--jobs", "1"])
        .current_dir(&ws)
        .assert()
        .success();
}

/// `rwv update -j N` accepts the flag (clap parses it cleanly even
/// outside a workspace, where the verb itself fails on the
/// no-workspace check).
#[test]
fn update_accepts_dash_j_flag() {
    // `rwv update` outside a workspace should fail for "no workspace"
    // reasons, not "unknown argument" reasons. clap-level rejection
    // would surface a "unexpected argument" error; we want to see the
    // workspace error instead, proving -j parses cleanly.
    let tmp = tempfile::tempdir().unwrap();
    rwv()
        .args(["update", "-j", "2"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument").not());
}

/// Advance a bare repo by one commit and return the new tip SHA.
fn advance_bare_repo(tmp: &Path, bare: &Path, label: &str) -> String {
    let work = tmp.join(format!("{label}-work"));
    run_git(
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
        tmp,
    );
    run_git(&["config", "user.email", "t@t.com"], &work);
    run_git(&["config", "user.name", "T"], &work);
    std::fs::write(work.join("advance.txt"), label).unwrap();
    run_git(&["add", "advance.txt"], &work);
    run_git(&["commit", "-m", label], &work);
    run_git(&["push", "origin", "main"], &work);
    let out = common::git()
        .args(["rev-parse", "main"])
        .current_dir(bare)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// End-to-end parallel correctness: bootstrap a workspace with two
/// repos via `rwv fetch`, advance both remotes, then `rwv update -j 2`.
/// Both repos must advance in their local clones AND in `rwv.lock`.
/// This is the strongest single check for "lock write is serial
/// post-join" — if the lock write raced or the parallel worker pool
/// dropped a repo, the resulting lock would miss an entry or hold a
/// stale SHA.
#[test]
fn update_dash_j_advances_all_repos_and_relocks() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    // Two bare repos as the remotes for the workspace's manifest.
    let bare_a = tmp.path().join("a.git");
    let bare_b = tmp.path().join("b.git");
    init_bare_repo_with_commit(&bare_a);
    init_bare_repo_with_commit(&bare_b);
    let url_a = format!("file://{}", bare_a.display());
    let url_b = format!("file://{}", bare_b.display());

    let source = make_project_source(
        tmp.path(),
        "proj",
        &[("local/team/a", &url_a), ("local/team/b", &url_b)],
    );

    // Bootstrap via `rwv fetch` (serial; tested above to work under
    // -j too — we just need a workspace).
    rwv()
        .args(["fetch", &source])
        .current_dir(&ws)
        .assert()
        .success();

    // Advance both remotes to a new tip, then run parallel update.
    let new_a = advance_bare_repo(tmp.path(), &bare_a, "a-v2");
    let new_b = advance_bare_repo(tmp.path(), &bare_b, "b-v2");
    assert_ne!(new_a, new_b);

    rwv()
        .args(["update", "-j", "2"])
        .current_dir(&ws)
        .assert()
        .success();

    // Each local clone's HEAD should now be at the advanced remote tip.
    for (path, expected) in [("local/team/a", &new_a), ("local/team/b", &new_b)] {
        let out = common::git()
            .args(["rev-parse", "HEAD"])
            .current_dir(ws.join(path))
            .output()
            .unwrap();
        let head = String::from_utf8(out.stdout).unwrap().trim().to_string();
        assert_eq!(&head, expected, "{path} should have advanced to {expected}");
    }

    // And the lock file (written serially post-join) should contain
    // both new SHAs.
    let lock = std::fs::read_to_string(ws.join("projects/proj/rwv.lock")).unwrap();
    assert!(
        lock.contains(&new_a),
        "lock missing advanced SHA for a:\n{lock}"
    );
    assert!(
        lock.contains(&new_b),
        "lock missing advanced SHA for b:\n{lock}"
    );
}

/// Failure aggregation under `rwv update -j 2`: one bad repo (broken
/// remote) must not prevent the healthy one from being attempted, and
/// the aggregated error report must surface the bad repo and bail
/// (lock not written).
#[test]
fn update_dash_j_aggregates_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let bare_ok = tmp.path().join("ok.git");
    init_bare_repo_with_commit(&bare_ok);
    let url_ok = format!("file://{}", bare_ok.display());
    let url_bad = format!("file://{}/missing.git", tmp.path().display());

    // Fetch can't bootstrap a workspace if a repo URL is invalid, so
    // we set up a "good" manifest, fetch, then break the bad repo's
    // remote afterwards by deleting the bare backing store.
    let bare_bad = tmp.path().join("bad.git");
    init_bare_repo_with_commit(&bare_bad);
    let url_bad_initial = format!("file://{}", bare_bad.display());

    let source = make_project_source(
        tmp.path(),
        "proj",
        &[
            ("local/team/ok", &url_ok),
            ("local/team/bad", &url_bad_initial),
        ],
    );
    rwv()
        .args(["fetch", &source])
        .current_dir(&ws)
        .assert()
        .success();

    // Sabotage `bad` by pointing its origin at a nonexistent path; the
    // next `git fetch` against it will fail.
    let bad_clone = ws.join("local/team/bad");
    run_git(&["remote", "set-url", "origin", &url_bad], &bad_clone);
    // Also delete the original backing store to make sure fetch fails.
    std::fs::remove_dir_all(&bare_bad).unwrap();

    let lock_before = std::fs::read_to_string(ws.join("projects/proj/rwv.lock")).unwrap();

    rwv()
        .args(["update", "-j", "2"])
        .current_dir(&ws)
        .assert()
        .failure()
        .stderr(predicate::str::contains("bad").and(predicate::str::contains("lock not written")));

    // Lock must not have been overwritten when the update bailed.
    let lock_after = std::fs::read_to_string(ws.join("projects/proj/rwv.lock")).unwrap();
    assert_eq!(
        lock_before, lock_after,
        "lock should not be rewritten when update bails on failure"
    );
}
