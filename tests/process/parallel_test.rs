//! Acceptance tests for `-j N` on `rwv clone`, `rwv update` and
//! `rwv update --remote`.
//!
//! These cover the visible behaviors pinned in the spec notes:
//!
//! - `-j N` accepted as a flag on all three verbs.
//! - `-j 1` reproduces serial behavior (no `[<repo>]` prefix in output).
//! - `-j > 1` runs concurrently; per-line `[<repo-path>]` prefix appears.
//! - Failure aggregation under `-j > 1` matches the serial shape (all
//!   failing repos surface in the trailing summary; exit non-zero).
//! - Every member lands at the commit the lock pins under `-j > 1`, and
//!   `rwv update --remote` writes the lock after the worker pool joins —
//!   asserted via the resulting `rwv.lock` covering every advanced SHA (no
//!   race / partial-write artifacts).
//!
//! The tests follow the existing `tests/clone_test.rs` pattern: spin up
//! local bare repos as file:// remotes; no network.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::process;

use crate::common;

fn rwv() -> Command {
    common::rwv()
}

// ----- Setup helpers (mirroring tests/clone_test.rs) -------------------------

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
    let tmp = common::tempdir().expect("tempdir");
    let work = tmp.path().join("w");
    common::git_in(
        tmp.path(),
        &["clone", &path.to_string_lossy(), &work.to_string_lossy()],
    );
    common::git_in(&work, &["config", "user.email", "t@t.com"]);
    common::git_in(&work, &["config", "user.name", "T"]);
    std::fs::write(work.join("README"), "init").unwrap();
    common::git_in(&work, &["add", "."]);
    common::git_in(&work, &["commit", "-m", "initial"]);
    common::git_in(&work, &["push", "origin", "main"]);
}

/// Set up a project bare repo whose `rwv.toml` references the given
/// `(repo_path, url)` pairs and whose `rwv.lock` pins each at the tip of its
/// remote's `main`. Returns the source URL.
fn make_project_source(tmp: &Path, name: &str, repos: &[(&str, &str)]) -> String {
    make_project_source_locked(tmp, name, repos, |work| {
        common::fixture_lock_at_remote_tips(work, repos);
    })
}

/// [`make_project_source`], with `write_lock` writing the project's `rwv.lock`.
fn make_project_source_locked(
    tmp: &Path,
    name: &str,
    repos: &[(&str, &str)],
    write_lock: impl FnOnce(&Path),
) -> String {
    let project_bare = tmp.join(format!("{name}.git"));
    init_bare_repo(&project_bare);
    let work = tmp.join(format!("{name}_work"));
    common::git_in(
        tmp,
        &[
            "clone",
            &project_bare.to_string_lossy(),
            &work.to_string_lossy(),
        ],
    );
    common::git_in(&work, &["config", "user.email", "t@t.com"]);
    common::git_in(&work, &["config", "user.name", "T"]);
    let mut manifest_toml = String::from("[repositories]\n");
    for (path, url) in repos {
        manifest_toml.push_str(&format!(
            "[repositories.\"{path}\"]\ntype = \"git\"\nurl = \"{url}\"\nbranch = \"main\"\nrole = \"owned\"\n"
        ));
    }
    std::fs::write(work.join("rwv.toml"), &manifest_toml).unwrap();
    common::set_up_lock_driver(&work);
    write_lock(&work);
    common::git_in(&work, &["add", "rwv.toml", "rwv.lock"]);
    common::git_in(&work, &["commit", "-m", "manifest"]);
    common::git_in(&work, &["push", "origin", "main"]);
    common::file_url(&project_bare)
}

// ----- Tests -----------------------------------------------------------------

/// `-j 1` reproduces serial behavior: output never contains a
/// `[<repo-path>]` prefix.
#[test]
fn clone_dash_j_one_emits_no_prefix() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let r1 = tmp.path().join("r1.git");
    let r2 = tmp.path().join("r2.git");
    init_bare_repo_with_commit(&r1);
    init_bare_repo_with_commit(&r2);
    let u1 = common::file_url(&r1);
    let u2 = common::file_url(&r2);

    let source = make_project_source(
        tmp.path(),
        "proj",
        &[("local/org/r1", &u1), ("local/org/r2", &u2)],
    );

    let out = rwv()
        .args(["clone", &source, "-j", "1"])
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
fn clone_dash_j_two_emits_repo_prefix() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let r1 = tmp.path().join("r1.git");
    let r2 = tmp.path().join("r2.git");
    init_bare_repo_with_commit(&r1);
    init_bare_repo_with_commit(&r2);
    let u1 = common::file_url(&r1);
    let u2 = common::file_url(&r2);

    let source = make_project_source(
        tmp.path(),
        "proj",
        &[("local/org/r1", &u1), ("local/org/r2", &u2)],
    );

    let out = rwv()
        .args(["clone", &source, "-j", "2"])
        .current_dir(&ws)
        .assert()
        .success();
    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains("[local/org/r1]") && stdout.contains("[local/org/r2]"),
        "expected [<repo>] prefix for each repo under -j 2, got:\n{stdout}"
    );
}

/// Under `-j N` every member lands at the commit the lock pins, not at its
/// remote's tip, and the clone leaves the lock the project carried untouched.
/// The remotes move past the pins after the lock is written, so a worker that
/// placed a member at the tip would differ from its pin.
#[test]
fn clone_dash_j_places_every_member_at_its_pin_and_writes_no_lock() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    // 5 repos — enough to exercise concurrency without being slow.
    let mut bares = Vec::new();
    let mut urls = Vec::new();
    let mut paths = Vec::new();
    for i in 0..5 {
        let bare = tmp.path().join(format!("r{i}.git"));
        init_bare_repo_with_commit(&bare);
        urls.push(common::file_url(&bare));
        paths.push(format!("local/org/r{i}"));
        bares.push(bare);
    }
    let pairs: Vec<(&str, &str)> = paths
        .iter()
        .zip(urls.iter())
        .map(|(p, u)| (p.as_str(), u.as_str()))
        .collect();
    let source = make_project_source(tmp.path(), "proj", &pairs);
    let pins: Vec<String> = bares.iter().map(|bare| tip(bare)).collect();
    for (i, bare) in bares.iter().enumerate() {
        let advanced = advance_bare_repo(tmp.path(), bare, &format!("r{i}-v2"));
        assert_ne!(advanced, pins[i]);
    }

    rwv()
        .args(["clone", &source, "-j", "4"])
        .current_dir(&ws)
        .assert()
        .success();

    for (path, pin) in paths.iter().zip(&pins) {
        let head = common::git_in(ws.join(path), &["rev-parse", "HEAD"]);
        assert_eq!(&head, pin, "{path} should sit at its pin under -j 4");
    }

    let project_dir = ws.join("projects/proj");
    let on_disk = common::read_normalized(project_dir.join("rwv.lock"));
    let committed = common::git_in(&project_dir, &["show", "HEAD:rwv.lock"]);
    assert_eq!(
        on_disk.trim(),
        committed,
        "clone must not rewrite the lock the project carried"
    );
}

/// Under `-j > 1` a failing repo (bad URL) does not prevent the other
/// repos from being attempted; the failure surfaces in the trailing
/// aggregated summary and the command exits non-zero — matching the
/// existing serial aggregation shape.
#[test]
fn clone_dash_j_aggregates_failures() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    // r_ok is a real bare repo; r_bad points at a nonexistent path so
    // git clone will fail. The pre-existing aggregation pattern
    // (collect Vec<String>, bail at end) should still report r_bad's
    // failure while letting r_ok succeed.
    let r_ok = tmp.path().join("r_ok.git");
    init_bare_repo_with_commit(&r_ok);
    let u_ok = common::file_url(&r_ok);
    let u_bad = format!("{}/does_not_exist.git", common::file_url(tmp.path()));
    let pin = tip(&r_ok);

    let source = make_project_source_locked(
        tmp.path(),
        "proj",
        &[("local/org/r_ok", &u_ok), ("local/org/r_bad", &u_bad)],
        |work| {
            common::fixture_lock(
                work,
                &[
                    ("local/org/r_ok", &u_ok, &pin),
                    ("local/org/r_bad", &u_bad, &pin),
                ],
            );
        },
    );

    rwv()
        .args(["clone", &source, "-j", "2"])
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
fn clone_accepts_long_jobs_flag() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let r1 = tmp.path().join("r1.git");
    init_bare_repo_with_commit(&r1);
    let u1 = common::file_url(&r1);
    let source = make_project_source(tmp.path(), "proj", &[("local/org/r1", &u1)]);

    rwv()
        .args(["clone", &source, "--jobs", "1"])
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
    let tmp = common::tempdir().unwrap();
    rwv()
        .args(["update", "-j", "2"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument").not());
}

/// `rwv update -j 2` clones the members missing on disk concurrently, each
/// line carrying its `[<repo-path>]` prefix, and puts each at its pin.
#[test]
fn update_dash_j_two_clones_missing_members_with_repo_prefix() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let bare_a = tmp.path().join("a.git");
    let bare_b = tmp.path().join("b.git");
    init_bare_repo_with_commit(&bare_a);
    init_bare_repo_with_commit(&bare_b);
    let url_a = common::file_url(&bare_a);
    let url_b = common::file_url(&bare_b);
    let source = make_project_source(
        tmp.path(),
        "proj",
        &[("local/team/a", &url_a), ("local/team/b", &url_b)],
    );
    let (pin_a, pin_b) = (tip(&bare_a), tip(&bare_b));
    rwv()
        .args(["clone", &source])
        .current_dir(&ws)
        .assert()
        .success();
    std::fs::remove_dir_all(ws.join("local/team/a")).unwrap();
    std::fs::remove_dir_all(ws.join("local/team/b")).unwrap();

    let out = rwv()
        .args(["update", "-j", "2"])
        .current_dir(&ws)
        .assert()
        .success();

    let stdout = String::from_utf8_lossy(&out.get_output().stdout).to_string();
    assert!(
        stdout.contains("[local/team/a]") && stdout.contains("[local/team/b]"),
        "expected [<repo>] prefix for each repo under -j 2, got:\n{stdout}"
    );
    for (path, pin) in [("local/team/a", &pin_a), ("local/team/b", &pin_b)] {
        let head = common::git_in(ws.join(path), &["rev-parse", "HEAD"]);
        assert_eq!(&head, pin, "{path} should be cloned at its pin");
    }
}

/// `rwv update --remote -j N` accepts the flag, on the same terms as
/// [`update_accepts_dash_j_flag`].
#[test]
fn update_remote_accepts_dash_j_flag() {
    let tmp = common::tempdir().unwrap();
    rwv()
        .args(["update", "--remote", "-j", "2"])
        .current_dir(tmp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument").not());
}

fn tip(bare: &Path) -> String {
    common::git_in(bare, &["rev-parse", "main"])
}

/// Advance a bare repo by one commit and return the new tip SHA.
fn advance_bare_repo(tmp: &Path, bare: &Path, label: &str) -> String {
    let work = tmp.join(format!("{label}-work"));
    common::git_in(
        tmp,
        &["clone", &bare.to_string_lossy(), &work.to_string_lossy()],
    );
    common::git_in(&work, &["config", "user.email", "t@t.com"]);
    common::git_in(&work, &["config", "user.name", "T"]);
    std::fs::write(work.join("advance.txt"), label).unwrap();
    common::git_in(&work, &["add", "advance.txt"]);
    common::git_in(&work, &["commit", "-m", label]);
    common::git_in(&work, &["push", "origin", "main"]);
    tip(bare)
}

/// End-to-end parallel correctness: bootstrap a workspace with two
/// repos via `rwv clone`, advance both remotes, then
/// `rwv update --remote -j 2`. Both repos must advance in their local clones
/// AND in `rwv.lock`.
/// This is the strongest single check for "lock write is serial
/// post-join" — if the lock write raced or the parallel worker pool
/// dropped a repo, the resulting lock would miss an entry or hold a
/// stale SHA.
#[test]
fn update_remote_dash_j_advances_all_repos_and_relocks() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    // Two bare repos as the remotes for the workspace's manifest.
    let bare_a = tmp.path().join("a.git");
    let bare_b = tmp.path().join("b.git");
    init_bare_repo_with_commit(&bare_a);
    init_bare_repo_with_commit(&bare_b);
    let url_a = common::file_url(&bare_a);
    let url_b = common::file_url(&bare_b);

    let source = make_project_source(
        tmp.path(),
        "proj",
        &[("local/team/a", &url_a), ("local/team/b", &url_b)],
    );

    // Bootstrap via `rwv clone` (serial; tested above to work under
    // -j too — we just need a workspace).
    rwv()
        .args(["clone", &source])
        .current_dir(&ws)
        .assert()
        .success();

    // Advance both remotes to a new tip, then run parallel update.
    let new_a = advance_bare_repo(tmp.path(), &bare_a, "a-v2");
    let new_b = advance_bare_repo(tmp.path(), &bare_b, "b-v2");
    assert_ne!(new_a, new_b);

    rwv()
        .args(["update", "--remote", "-j", "2"])
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

/// A worker's *subprocess* output reaches the user, prefixed.
/// `rwv update --remote` streams `git fetch` through the reporter under
/// `-j > 1`; rwv's own stderr
/// lines on this path are written directly and carry no prefix, so a
/// prefixed stderr line is one git wrote and the forwarding delivered.
///
/// The in-src tests in `src/parallel.rs` cannot pin this: under
/// `Reporter::Parallel` the forwarded lines go to the test process's real
/// stdout/stderr from a spawned thread, outside libtest's capture, so those
/// tests can see the call return but not that a line came out.
#[test]
fn update_remote_dash_j_forwards_child_output_prefixed() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let bare = tmp.path().join("a.git");
    init_bare_repo_with_commit(&bare);
    let url = common::file_url(&bare);
    let source = make_project_source(tmp.path(), "proj", &[("local/team/a", &url)]);

    rwv()
        .args(["clone", &source])
        .current_dir(&ws)
        .assert()
        .success();
    advance_bare_repo(tmp.path(), &bare, "a-v2");

    let out = rwv()
        .args(["update", "--remote", "-j", "2"])
        .current_dir(&ws)
        .assert()
        .success();

    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(
        stderr.lines().any(|l| l.starts_with("[local/team/a] ")),
        "no forwarded child line arrived on stderr under -j 2:\n{stderr}"
    );
}

/// Failure aggregation under `rwv update --remote -j 2`: one bad repo (broken
/// remote) must not prevent the healthy one from being attempted, and
/// the aggregated error report must surface the bad repo and bail
/// (lock not written).
#[test]
fn update_remote_dash_j_aggregates_failures() {
    let tmp = common::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();

    let bare_ok = tmp.path().join("ok.git");
    init_bare_repo_with_commit(&bare_ok);
    let url_ok = common::file_url(&bare_ok);
    let url_bad = format!("{}/missing.git", common::file_url(tmp.path()));

    // Clone can't bootstrap a workspace if a repo URL is invalid, so
    // we set up a "good" manifest, clone, then break the bad repo's
    // remote afterwards by deleting the bare backing store.
    let bare_bad = tmp.path().join("bad.git");
    init_bare_repo_with_commit(&bare_bad);
    let url_bad_initial = common::file_url(&bare_bad);

    let source = make_project_source(
        tmp.path(),
        "proj",
        &[
            ("local/team/ok", &url_ok),
            ("local/team/bad", &url_bad_initial),
        ],
    );
    rwv()
        .args(["clone", &source])
        .current_dir(&ws)
        .assert()
        .success();

    // Sabotage `bad` by pointing its origin at a nonexistent path; the
    // next `git fetch` against it will fail.
    let bad_clone = ws.join("local/team/bad");
    common::git_in(&bad_clone, &["remote", "set-url", "origin", &url_bad]);
    // Also delete the original backing store to make sure fetch fails.
    std::fs::remove_dir_all(&bare_bad).unwrap();

    let lock_before = std::fs::read_to_string(ws.join("projects/proj/rwv.lock")).unwrap();

    rwv()
        .args(["update", "--remote", "-j", "2"])
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
